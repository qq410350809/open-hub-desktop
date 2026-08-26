//! POST /v1/messages — Anthropic Messages 入口。
//!
//! 常规路径：Anthropic → OpenAI 中枢 → 渠道目标协议，响应再转回 Anthropic。
//! 同协议快速通道（渠道目标 = Anthropic）：请求体原生透传（仅重写模型名），
//! 非流式响应原样下发，仅旁路提取 usage 供日志统计，最大化字段保真。

use super::super::adapters::AnthropicProtocolAdapter;
use super::super::egress;
use super::super::logger::{cap_log_body, client_name_from_headers};
use super::super::pipeline::{
    auth_and_count, dispatch_protocol_egress, resolve_channel_or_404, ClientProtocol,
};
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

    // 同协议快速通道：Anthropic 客户端 → Anthropic 上游，请求体原生透传。
    // 此前强制转 Chat 导致 cache_control / thinking signature 丢失，
    // Anthropic 上游的 prompt caching 与思考链连续性完全失效，现恢复直通。
    let fast_path = egress::TargetProtocol::from_channel(chan) == egress::TargetProtocol::AnthropicMessages;
    let egress_payload = if fast_path {
        let mut native = body.clone();
        native["model"] = serde_json::Value::String(model_to_send.clone());
        crate::model::gateway::egress::EgressBody::native(native)
    } else {
        crate::model::gateway::egress::EgressBody::Universal(
            crate::model::gateway::parsers::anthropic_to_universal(&body, &model_to_send),
        )
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
        egress_payload,
        ClientProtocol::Anthropic,
    )
    .await
    {
        Ok(o) => o,
        Err(res) => return res,
    };

    let mut log = outcome.base_log(PATH, &raw_model, is_stream, req_body_str);
    log.client_name = Some(client_name_from_headers(&headers, PATH));

    // 流式：快速通道走「原生字节直通 + 兼容修复 + 旁路统计」，杜绝往返转换丢失内容；
    // 跨协议转换路径仍经归一化链路
    if is_stream {
        if fast_path {
            // 同协议：字节直通 + 旁路统计，保住 thinking signature 等原生元素
            let upstream_headers = outcome.success.response.headers().clone();
            let stream_body = crate::model::gateway::stream::passthrough_sse_body(
                outcome.success.response.bytes_stream(),
                outcome.target,
                ctx.clone(),
                log,
                start_time,
                raw_model,
            );
            let resp = Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "text/event-stream")
                .header("Cache-Control", "no-cache")
                .header("Connection", "keep-alive")
                .body(stream_body)
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            return egress::copy_upstream_headers(&upstream_headers, resp);
        }
        // 跨协议 IR 链路：嗅探上游实际协议 → Parser → UniversalStreamEvent →
        // Anthropic Emitter（全量 usage/缓存明细回传，孤儿工具名恢复保留）
        let upstream_headers = outcome.success.response.headers().clone();
        let (tool_hints, preferred_tool) =
            crate::model::gateway::stream::extract_tool_hints(&body);
        let stream_body = crate::model::gateway::stream::proxy_sse_body_with_hints(
            outcome.success.response.bytes_stream(),
            outcome.target,
            crate::model::gateway::stream::SseClientProtocol::Anthropic,
            ctx.clone(),
            log,
            start_time,
            raw_model,
            tool_hints,
            preferred_tool,
        );
        let resp = Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/event-stream")
            .header("Cache-Control", "no-cache")
            .header("Connection", "keep-alive")
            .body(stream_body)
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        return egress::copy_upstream_headers(&upstream_headers, resp);
    }

    let upstream_headers = outcome.success.response.headers().clone();
    let raw_bytes = outcome.success.response.bytes().await.unwrap_or_default();
    let dur = outcome.success.cand_start.elapsed().as_millis() as u64;

    // 同协议非流式：原生响应直接下发，usage 旁路提取落日志
    if fast_path {
        let mut final_log = log;
        final_log.duration_ms = dur;
        final_log.response_body = cap_log_body(String::from_utf8_lossy(&raw_bytes).to_string());
        if let Ok(jv) = serde_json::from_slice::<JsonValue>(&raw_bytes) {
            // Anthropic 口径换算为网关总量口径：prompt = input + cache_read + cache_creation
            let input = jv.pointer("/usage/input_tokens").and_then(JsonValue::as_u64).unwrap_or(0);
            let cache_read = jv
                .pointer("/usage/cache_read_input_tokens")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0);
            let cache_creation = jv
                .pointer("/usage/cache_creation_input_tokens")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0);
            let output = jv.pointer("/usage/output_tokens").and_then(JsonValue::as_u64).unwrap_or(0);
            final_log.prompt_tokens = (input + cache_read + cache_creation > 0)
                .then_some(input + cache_read + cache_creation);
            final_log.completion_tokens = (output > 0).then_some(output);
            final_log.prompt_cache_hit_tokens = (cache_read > 0).then_some(cache_read);
            final_log.cache_creation_tokens = (cache_creation > 0).then_some(cache_creation);
            final_log.total_tokens =
                Some(input + cache_read + cache_creation + output).filter(|v| *v > 0);
        }
        ctx.record_log(final_log).await;
        return egress::copy_upstream_headers(
            &upstream_headers,
            (
                StatusCode::OK,
                [("content-type", "application/json")],
                raw_bytes,
            )
                .into_response(),
        );
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

        return egress::copy_upstream_headers(
            &upstream_headers,
            Json(anthropic_resp).into_response(),
        );
    }

    let mut final_log = log;
    final_log.duration_ms = dur;
    final_log.response_body = resp_body;
    ctx.record_log(final_log).await;
    egress::copy_upstream_headers(
        &upstream_headers,
        (StatusCode::OK, resp_bytes).into_response(),
    )
}
