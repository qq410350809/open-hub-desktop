//! POST /v1/chat/completions — OpenAI Chat Completions 入口。
//!
//! 客户端协议即 OpenAI 中枢格式：请求侧无需转换（仅清洗规范化），
//! 响应侧按渠道目标协议归一化回 OpenAI 后直接下发。

use super::super::adapters::{normalize_chat_messages, OpenAiProtocolAdapter};
use super::super::egress;
use super::super::logger::{cap_log_body, client_name_from_headers};
use super::super::pipeline::{
    auth_and_count, dispatch_protocol_egress, resolve_channel_or_404, ClientProtocol,
};
use super::super::stream::clean_sse_stream;
use super::super::types::{generate_req_id, ModelProxyContext};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value as JsonValue;
use std::sync::atomic::Ordering;
use std::time::Instant;

pub async fn handle_chat_completions(
    headers: HeaderMap,
    uri: Uri,
    State(ctx): State<ModelProxyContext>,
    Json(mut body): Json<JsonValue>,
) -> Response {
    const PATH: &str = "/v1/chat/completions";
    let start_time = Instant::now();
    let req_id = generate_req_id();
    let config = ctx.config.read().await.clone();

    let raw_model = body
        .get("model")
        .and_then(JsonValue::as_str)
        .unwrap_or("gpt-4o")
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
        ClientProtocol::OpenAi,
    )
    .await
    {
        Ok(pair) => pair,
        Err(res) => return res,
    };

    body["model"] = JsonValue::String(model_to_send.clone());
    OpenAiProtocolAdapter::sanitize_and_normalize(&mut body);
    if let Some(msgs) = body.get_mut("messages") {
        normalize_chat_messages(msgs);
    }

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
        &body,
        true,
        ClientProtocol::OpenAi,
    )
    .await
    {
        Ok(o) => o,
        Err(res) => return res,
    };

    let mut log = outcome.base_log(PATH, &raw_model, is_stream, req_body_str);
    log.client_name = Some(client_name_from_headers(&headers, PATH));

    if is_stream {
        let stream_body = clean_sse_stream(
            egress::normalized_sse_stream(outcome.success.response.bytes_stream(), outcome.target),
            ctx.clone(),
            log,
            start_time,
            raw_model,
        );
        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/event-stream")
            .header("Cache-Control", "no-cache")
            .header("Connection", "keep-alive")
            .body(stream_body)
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    } else {
        let raw_bytes = outcome.success.response.bytes().await.unwrap_or_default();
        let resp_bytes =
            egress::normalize_response_bytes(outcome.target, &outcome.model_to_send, &raw_bytes);
        let dur = outcome.success.cand_start.elapsed().as_millis() as u64;
        let resp_body = cap_log_body(String::from_utf8_lossy(&resp_bytes).to_string());

        let mut prompt_toks = None;
        let mut comp_toks = None;
        let mut reas_toks = None;
        let mut cache_toks = None;
        let mut cache_creation_toks = None;
        let mut total_toks = None;
        let mut has_reasoning = false;

        if let Ok(mut jv) = serde_json::from_slice::<JsonValue>(&resp_bytes) {
            if let Some(usage) = jv.get("usage").and_then(JsonValue::as_object) {
                prompt_toks = usage.get("prompt_tokens").and_then(JsonValue::as_u64);
                comp_toks = usage.get("completion_tokens").and_then(JsonValue::as_u64);
                total_toks = usage.get("total_tokens").and_then(JsonValue::as_u64);
                if let Some(details) = usage
                    .get("prompt_tokens_details")
                    .and_then(JsonValue::as_object)
                {
                    cache_toks = details.get("cached_tokens").and_then(JsonValue::as_u64);
                    cache_creation_toks = details
                        .get("cache_creation_tokens")
                        .and_then(JsonValue::as_u64);
                }
                if let Some(details) = usage
                    .get("completion_tokens_details")
                    .and_then(JsonValue::as_object)
                {
                    reas_toks = details.get("reasoning_tokens").and_then(JsonValue::as_u64);
                    if reas_toks.is_some() {
                        has_reasoning = true;
                    }
                }
            }

            // 内联 <think> 标签剥离为独立 reasoning_content，便于客户端与日志展示
            if let Some(msg) = jv.pointer_mut("/choices/0/message") {
                let mut extracted_reasoning = None;
                if let Some(content) = msg.get_mut("content") {
                    if let Some(s) = content.as_str() {
                        if let (Some(start), Some(end)) = (s.find("<think>"), s.find("</think>")) {
                            if start < end {
                                let reasoning = s[start + 7..end].trim().to_string();
                                let after = s[end + 8..].trim_start().to_string();
                                *content = JsonValue::String(after);
                                extracted_reasoning = Some(reasoning);
                                has_reasoning = true;
                            }
                        }
                    }
                }
                if let Some(reasoning) = extracted_reasoning {
                    msg["reasoning_content"] = JsonValue::String(reasoning);
                }
            }

            let mut final_log = log;
            final_log.duration_ms = dur;
            final_log.response_body = resp_body;
            final_log.prompt_tokens = prompt_toks;
            final_log.completion_tokens = comp_toks;
            final_log.reasoning_tokens = reas_toks;
            final_log.prompt_cache_hit_tokens = cache_toks;
            final_log.cache_creation_tokens = cache_creation_toks;
            final_log.total_tokens = total_toks;
            ctx.record_log(final_log).await;

            if has_reasoning {
                ctx.metrics
                    .total_reasoning_requests
                    .fetch_add(1, Ordering::Relaxed);
            }
            if let Some(p) = prompt_toks {
                ctx.metrics
                    .total_prompt_tokens
                    .fetch_add(p, Ordering::Relaxed);
            }
            if let Some(c) = comp_toks {
                ctx.metrics
                    .total_completion_tokens
                    .fetch_add(c, Ordering::Relaxed);
            }
            if let Some(r) = reas_toks {
                ctx.metrics
                    .total_reasoning_tokens
                    .fetch_add(r, Ordering::Relaxed);
            }
            if let Some(h) = cache_toks {
                ctx.metrics
                    .total_cache_hit_tokens
                    .fetch_add(h, Ordering::Relaxed);
            }
            if let Some(t) = total_toks {
                ctx.metrics.total_tokens.fetch_add(t, Ordering::Relaxed);
            }

            return Json(jv).into_response();
        }

        let mut final_log = log;
        final_log.duration_ms = dur;
        final_log.response_body = resp_body;
        ctx.record_log(final_log).await;
        (StatusCode::OK, resp_bytes).into_response()
    }
}
