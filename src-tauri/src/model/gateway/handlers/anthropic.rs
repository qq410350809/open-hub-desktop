//! POST /v1/messages — Anthropic Messages 入口。
//!
//! 常规路径：Anthropic → OpenAI 中枢 → 渠道目标协议，响应再转回 Anthropic。
//! 同协议快速通道（渠道目标 = Anthropic）：请求体原生透传（仅重写模型名），
//! 非流式响应原样下发，仅旁路提取 usage 供日志统计，最大化字段保真。

use super::super::adapters::{AnthropicProtocolAdapter, OpenAiProtocolAdapter};
use super::super::egress::{self, TargetProtocol};
use super::super::logger::{cap_log_body, client_name_from_headers};
use super::super::pipeline::{
    auth_and_count, dispatch_protocol_egress, resolve_channel_or_404, ClientProtocol,
};
use super::super::stream::openai_to_anthropic_sse_stream;
use super::super::types::{generate_req_id, ModelProxyContext};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value as JsonValue;
use std::time::Instant;

pub async fn handle_messages(
    headers: HeaderMap,
    uri: Uri,
    State(ctx): State<ModelProxyContext>,
    Json(body): Json<JsonValue>,
) -> Response {
    const PATH: &str = "/v1/messages";
    let start_time = Instant::now();
    let req_id = generate_req_id();
    let config = ctx.config.read().await.clone();

    let raw_model = body
        .get("model")
        .and_then(JsonValue::as_str)
        .unwrap_or("claude-3-7-sonnet")
        .to_string();
    let is_stream = body
        .get("stream")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let req_body_str = if config.record_request_body {
        Some(serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string()))
    } else {
        None
    };

    if let Err(res) = auth_and_count(
        &ctx,
        &headers,
        &uri,
        &config,
        &req_id,
        PATH,
        &raw_model,
        is_stream,
        start_time,
        &req_body_str,
    )
    .await
    {
        return res;
    }

    let (chan, model_to_send) = match resolve_channel_or_404(
        &ctx,
        &config,
        &raw_model,
        PATH,
        &req_id,
        is_stream,
        start_time,
        &req_body_str,
        ClientProtocol::Anthropic,
    )
    .await
    {
        Ok(pair) => pair,
        Err(res) => return res,
    };

    // 同协议快速通道：Anthropic 客户端 → Anthropic 上游，请求体原生透传
    let fast_path = TargetProtocol::from_channel(chan) == TargetProtocol::AnthropicMessages;
    let (egress_body, convert) = if fast_path {
        let mut native = body.clone();
        native["model"] = JsonValue::String(model_to_send.clone());
        (native, false)
    } else {
        let mut openai_payload =
            AnthropicProtocolAdapter::anthropic_request_to_openai(&body, &model_to_send, is_stream);
        OpenAiProtocolAdapter::sanitize_and_normalize(&mut openai_payload);
        (openai_payload, true)
    };

    let outcome = match dispatch_protocol_egress(
        &ctx,
        &config,
        chan,
        &model_to_send,
        &raw_model,
        PATH,
        &req_id,
        is_stream,
        start_time,
        &req_body_str,
        &egress_body,
        convert,
        ClientProtocol::Anthropic,
    )
    .await
    {
        Ok(o) => o,
        Err(res) => return res,
    };

    let mut log = outcome.base_log(PATH, &raw_model, is_stream, req_body_str);
    log.client_name = Some(client_name_from_headers(&headers, PATH));

    // 流式：即使同协议也经「归一化 → 回转」链路，保住 Token 统计/思考捕获/日志重建
    if is_stream {
        let stream_body = openai_to_anthropic_sse_stream(
            egress::normalized_sse_stream(outcome.success.response.bytes_stream(), outcome.target),
            ctx.clone(),
            log,
            start_time,
            raw_model,
        );
        return Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/event-stream")
            .header("Cache-Control", "no-cache")
            .header("Connection", "keep-alive")
            .body(stream_body)
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    let raw_bytes = outcome.success.response.bytes().await.unwrap_or_default();
    let dur = outcome.success.cand_start.elapsed().as_millis() as u64;
    let resp_body = cap_log_body(String::from_utf8_lossy(&raw_bytes).to_string());

    // 快速通道非流式：上游原生 Anthropic 响应直接下发，usage 旁路提取用于日志
    if fast_path {
        let mut final_log = log;
        final_log.duration_ms = dur;
        final_log.response_body = resp_body;
        if let Ok(jv) = serde_json::from_slice::<JsonValue>(&raw_bytes) {
            let p = jv
                .pointer("/usage/input_tokens")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0);
            let c = jv
                .pointer("/usage/output_tokens")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0);
            let cache_read = jv
                .pointer("/usage/cache_read_input_tokens")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0);
            let cache_creation = jv
                .pointer("/usage/cache_creation_input_tokens")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0);
            // OpenAI 口径：prompt_tokens 含缓存读+写
            let prompt = p + cache_read + cache_creation;
            final_log.prompt_tokens = Some(prompt);
            final_log.completion_tokens = Some(c);
            final_log.prompt_cache_hit_tokens = (cache_read > 0).then_some(cache_read);
            final_log.cache_creation_tokens = (cache_creation > 0).then_some(cache_creation);
            final_log.total_tokens = Some(prompt + c);
        }
        ctx.record_log(final_log).await;
        return (StatusCode::OK, raw_bytes).into_response();
    }

    let resp_bytes =
        egress::normalize_response_bytes(outcome.target, &outcome.model_to_send, &raw_bytes);
    let resp_body = cap_log_body(String::from_utf8_lossy(&resp_bytes).to_string());

    if let Ok(jv) = serde_json::from_slice::<JsonValue>(&resp_bytes) {
        let (p_tok, c_tok) = AnthropicProtocolAdapter::extract_token_usage(&jv);
        // 归一化后的 usage 已带缓存/推理明细（Anthropic 上游时）
        let cache_hit = jv
            .pointer("/usage/prompt_tokens_details/cached_tokens")
            .and_then(JsonValue::as_u64);
        let cache_creation = jv
            .pointer("/usage/prompt_tokens_details/cache_creation_tokens")
            .and_then(JsonValue::as_u64);
        let reasoning = jv
            .pointer("/usage/completion_tokens_details/reasoning_tokens")
            .and_then(JsonValue::as_u64);
        let anthropic_resp =
            AnthropicProtocolAdapter::openai_response_to_anthropic(&jv, &req_id, &raw_model);

        let mut final_log = log;
        final_log.duration_ms = dur;
        final_log.response_body = resp_body;
        final_log.prompt_tokens = Some(p_tok);
        final_log.completion_tokens = Some(c_tok);
        final_log.prompt_cache_hit_tokens = cache_hit.filter(|v| *v > 0);
        final_log.cache_creation_tokens = cache_creation.filter(|v| *v > 0);
        final_log.reasoning_tokens = reasoning.filter(|v| *v > 0);
        final_log.total_tokens = Some(p_tok + c_tok);
        ctx.record_log(final_log).await;

        return Json(anthropic_resp).into_response();
    }

    let mut final_log = log;
    final_log.duration_ms = dur;
    final_log.response_body = resp_body;
    ctx.record_log(final_log).await;
    (StatusCode::OK, resp_bytes).into_response()
}
