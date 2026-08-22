//! POST /v1/gemini/models/* 与 /v1beta/models/* — Google Gemini 原生入口。
//!
//! 常规路径：Gemini → OpenAI 中枢 → 渠道目标协议，响应再转回 Gemini。
//! 同协议快速通道（渠道目标 = Gemini）：请求体原生透传（模型名在 URL 中重写），
//! 非流式响应原样下发，仅旁路提取 usageMetadata 供日志统计。

use super::super::adapters::{GeminiProtocolAdapter, OpenAiProtocolAdapter};
use super::super::egress::{self, TargetProtocol};
use super::super::logger::cap_log_body;
use super::super::pipeline::{
    auth_and_count, dispatch_protocol_egress, resolve_channel_or_404, ClientProtocol,
};
use super::super::stream::openai_to_gemini_sse_stream;
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
    let (egress_body, convert) = if fast_path {
        (body.clone(), false)
    } else {
        let mut openai_body =
            GeminiProtocolAdapter::gemini_request_to_openai(&body, &model_to_send, is_stream);
        OpenAiProtocolAdapter::sanitize_and_normalize(&mut openai_body);
        (openai_body, true)
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
        &egress_body,
        convert,
        ClientProtocol::Gemini,
    )
    .await
    {
        Ok(o) => o,
        Err(res) => return res,
    };

    let log = outcome.base_log(&log_path, raw_model, is_stream, req_body_str);

    if is_stream {
        let stream_body = openai_to_gemini_sse_stream(
            egress::normalized_sse_stream(outcome.success.response.bytes_stream(), outcome.target),
            ctx.clone(),
            log,
            start_time,
            raw_model.to_string(),
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

    // 快速通道非流式：上游原生 Gemini 响应直接下发，usageMetadata 旁路提取用于日志
    if fast_path {
        let mut final_log = log;
        final_log.duration_ms = dur;
        final_log.response_body = resp_body;
        if let Ok(jv) = serde_json::from_slice::<JsonValue>(&raw_bytes) {
            let p = jv.pointer("/usageMetadata/promptTokenCount").and_then(JsonValue::as_u64).unwrap_or(0);
            let c = jv.pointer("/usageMetadata/candidatesTokenCount").and_then(JsonValue::as_u64).unwrap_or(0);
            final_log.prompt_tokens = Some(p);
            final_log.completion_tokens = Some(c);
            final_log.total_tokens = jv.pointer("/usageMetadata/totalTokenCount").and_then(JsonValue::as_u64).or(Some(p + c));
        }
        ctx.record_log(final_log).await;
        return (
            StatusCode::OK,
            [("content-type", "application/json")],
            raw_bytes,
        )
            .into_response();
    }

    let resp_bytes = egress::normalize_response_bytes(outcome.target, &outcome.model_to_send, &raw_bytes);
    let openai_resp = serde_json::from_slice::<JsonValue>(&resp_bytes).unwrap_or_default();
    let gemini_resp = GeminiProtocolAdapter::openai_response_to_gemini(&openai_resp, raw_model);

    let mut final_log = log;
    final_log.duration_ms = dur;
    final_log.response_body = cap_log_body(String::from_utf8_lossy(&resp_bytes).to_string());
    if let Some(usage) = gemini_resp.get("usageMetadata") {
        final_log.prompt_tokens = usage.get("promptTokenCount").and_then(JsonValue::as_u64);
        final_log.completion_tokens = usage.get("candidatesTokenCount").and_then(JsonValue::as_u64);
        final_log.total_tokens = usage.get("totalTokenCount").and_then(JsonValue::as_u64);
    }
    ctx.record_log(final_log).await;

    Json(gemini_resp).into_response()
}
