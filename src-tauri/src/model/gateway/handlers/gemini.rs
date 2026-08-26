//! POST /v1/gemini/models/* 与 /v1beta/models/* — Google Gemini 原生入口。
//!
//! 常规路径：Gemini → OpenAI 中枢 → 渠道目标协议，响应再转回 Gemini。
//! 同协议快速通道（渠道目标 = Gemini）：请求体原生透传（模型名在 URL 中重写），
//! 非流式响应原样下发，仅旁路提取 usageMetadata 供日志统计。

use super::super::egress::{self, TargetProtocol};
use crate::model::gateway::adapters::GeminiProtocolAdapter;
use super::super::logger::{cap_log_body, client_name_from_headers};
use super::super::pipeline::{
    auth_and_count, dispatch_protocol_egress, resolve_channel_or_404, ClientProtocol,
};
use super::super::types::{generate_req_id, ModelProxyContext};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value as JsonValue;
use std::time::Instant;

pub async fn handle_gemini_generate(
    headers: HeaderMap,
    uri: Uri,
    State(ctx): State<ModelProxyContext>,
    Path(model_action): Path<String>,
    Json(body): Json<JsonValue>,
) -> Response {
    let start_time = Instant::now();
    let req_id = generate_req_id();
    let config = ctx.config.read().await.clone();

    let (raw_model, is_stream) = if let Some((m, action)) = model_action.rsplit_once(':') {
        let stream = action.contains("stream");
        (m.trim_start_matches('/'), stream)
    } else {
        (model_action.trim_start_matches('/'), false)
    };

    let log_path = if uri.path().starts_with("/v1beta") {
        format!("/v1beta/models/{model_action}")
    } else {
        format!("/v1/gemini/models/{model_action}")
    };

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
        &log_path,
        raw_model,
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
        raw_model,
        &log_path,
        &req_id,
        is_stream,
        start_time,
        &req_body_str,
        ClientProtocol::Gemini,
    )
    .await
    {
        Ok(pair) => pair,
        Err(res) => return res,
    };

    // 同协议快速通道：Gemini 客户端 → Gemini 上游，请求体原生透传（模型名走 URL）
    let fast_path = TargetProtocol::from_channel(chan) == TargetProtocol::Gemini;
    let egress_payload = if fast_path {
        crate::model::gateway::egress::EgressBody::Native(body.clone())
    } else {
        // Gemini 协议 body 无 stream 字段（由 URL action 表达），
        // 跨协议时必须显式回填，否则序列化到目标上游会退化为非流式
        let mut ur = crate::model::gateway::parsers::gemini_to_universal(&body, &model_to_send);
        ur.stream = is_stream;
        crate::model::gateway::egress::EgressBody::Universal(ur)
    };

    let outcome = match dispatch_protocol_egress(
        &ctx,
        &config,
        chan,
        &model_to_send,
        raw_model,
        &log_path,
        &req_id,
        is_stream,
        start_time,
        &req_body_str,
        egress_payload,
        ClientProtocol::Gemini,
    )
    .await
    {
        Ok(o) => o,
        Err(res) => return res,
    };

    let mut log = outcome.base_log(&log_path, raw_model, is_stream, req_body_str);
    log.client_name = Some(client_name_from_headers(&headers, &log_path));

    if is_stream {
        if fast_path {
            // 同协议：字节直通 + 旁路统计，保留 thoughtSignature 等原生元素
            let upstream_headers = outcome.success.response.headers().clone();
            let stream_body = crate::model::gateway::stream::passthrough_sse_body(
                outcome.success.response.bytes_stream(),
                outcome.target,
                ctx.clone(),
                log,
                start_time,
                raw_model.to_string(),
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
        let (tool_hints, preferred_tool) = crate::model::gateway::stream::extract_tool_hints(&body);
        let stream_body = crate::model::gateway::stream::proxy_sse_body_with_hints(
            outcome.success.response.bytes_stream(),
            outcome.target,
            crate::model::gateway::stream::SseClientProtocol::Gemini,
            ctx.clone(),
            log,
            start_time,
            raw_model.to_string(),
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
    let resp_body = cap_log_body(String::from_utf8_lossy(&raw_bytes).to_string());

    // 快速通道非流式：上游原生 Gemini 响应直接下发，usageMetadata 旁路提取用于日志
    if fast_path {
        let mut final_log = log;
        final_log.duration_ms = dur;
        final_log.response_body = resp_body;
        if let Ok(jv) = serde_json::from_slice::<JsonValue>(&raw_bytes) {
            let p = jv
                .pointer("/usageMetadata/promptTokenCount")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0);
            let c = jv
                .pointer("/usageMetadata/candidatesTokenCount")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0);
            let thoughts = jv
                .pointer("/usageMetadata/thoughtsTokenCount")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0);
            let cached = jv
                .pointer("/usageMetadata/cachedContentTokenCount")
                .and_then(JsonValue::as_u64);
            final_log.prompt_tokens = Some(p);
            final_log.completion_tokens = Some(c + thoughts);
            final_log.prompt_cache_hit_tokens = cached.filter(|v| *v > 0);
            final_log.reasoning_tokens = (thoughts > 0).then_some(thoughts);
            final_log.total_tokens = jv
                .pointer("/usageMetadata/totalTokenCount")
                .and_then(JsonValue::as_u64)
                .or(Some(p + c + thoughts));
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
    let openai_resp = serde_json::from_slice::<JsonValue>(&resp_bytes).unwrap_or_default();
    let gemini_resp = GeminiProtocolAdapter::openai_response_to_gemini(&openai_resp, raw_model);

    let mut final_log = log;
    final_log.duration_ms = dur;
    final_log.response_body = cap_log_body(String::from_utf8_lossy(&resp_bytes).to_string());
    // 归一化后的 OpenAI usage 已带缓存/推理明细
    final_log.prompt_tokens = openai_resp
        .pointer("/usage/prompt_tokens")
        .and_then(JsonValue::as_u64)
        .filter(|v| *v > 0);
    final_log.completion_tokens = openai_resp
        .pointer("/usage/completion_tokens")
        .and_then(JsonValue::as_u64)
        .filter(|v| *v > 0);
    final_log.prompt_cache_hit_tokens = openai_resp
        .pointer("/usage/prompt_tokens_details/cached_tokens")
        .and_then(JsonValue::as_u64)
        .filter(|v| *v > 0);
    final_log.reasoning_tokens = openai_resp
        .pointer("/usage/completion_tokens_details/reasoning_tokens")
        .and_then(JsonValue::as_u64)
        .filter(|v| *v > 0);
    final_log.total_tokens = openai_resp
        .pointer("/usage/total_tokens")
        .and_then(JsonValue::as_u64)
        .filter(|v| *v > 0);
    ctx.record_log(final_log).await;

    egress::copy_upstream_headers(&upstream_headers, Json(gemini_resp).into_response())
}
