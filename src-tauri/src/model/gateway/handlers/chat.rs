//! POST /v1/chat/completions — OpenAI Chat Completions 入口。
//!
//! 请求侧 IR：入参解析为 UniversalRequest，出网由目标序列化器展开；
//! 响应侧嗅探上游实际协议后经 IR 回传 Chat。

use super::super::egress;
use super::super::logger::{cap_log_body, client_name_from_headers};
use super::super::pipeline::{
    auth_and_count, dispatch_protocol_egress, resolve_channel_or_404, ClientProtocol,
};
use super::super::types::{generate_req_id, ModelProxyConfig, ModelProxyContext};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value as JsonValue};
use std::sync::atomic::Ordering;
use std::time::Instant;

const CHAT_PATH: &str = "/v1/chat/completions";

/// 鉴权与出网共用的一次性请求摘要。
struct PreparedChat {
    config: ModelProxyConfig,
    raw_model: String,
    is_stream: bool,
    req_id: String,
    start_time: Instant,
    req_body_str: Option<String>,
}

async fn prepare_chat_request(ctx: &ModelProxyContext, body: &JsonValue) -> PreparedChat {
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
    PreparedChat {
        config,
        raw_model,
        is_stream,
        req_id: generate_req_id(),
        start_time: Instant::now(),
        req_body_str,
    }
}

pub async fn handle_chat_completions(
    headers: HeaderMap,
    uri: Uri,
    State(ctx): State<ModelProxyContext>,
    Json(body): Json<JsonValue>,
) -> Response {
    let prep = prepare_chat_request(&ctx, &body).await;
    if let Err(res) = auth_and_count(
        &ctx,
        &headers,
        &uri,
        &prep.config,
        &prep.req_id,
        CHAT_PATH,
        &prep.raw_model,
        prep.is_stream,
        prep.start_time,
        &prep.req_body_str,
    )
    .await
    {
        return res;
    }
    dispatch_chat_request(&ctx, &headers, &body, prep).await
}

/// 进程内直调 Chat 入口：跳过 HTTP 鉴权与网关路由开关，
/// 渠道解析、协议转换、出网重试、日志与用量统计与 HTTP 请求完全一致。
/// 供 Token 模型映射等内部功能复用渠道与日志链路，免回环端口与 Key。
pub async fn internal_chat_completion(
    ctx: &ModelProxyContext,
    model: &str,
    prompt: &str,
) -> Result<JsonValue, String> {
    let body = json!({
        "model": model,
        "temperature": 0,
        "messages": [{ "role": "user", "content": prompt }],
    });
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str("OpenHub-TokenMapping") {
        headers.insert(axum::http::header::USER_AGENT, value);
    }
    let prep = prepare_chat_request(ctx, &body).await;
    ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);
    let response = dispatch_chat_request(ctx, &headers, &body, prep).await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 32 * 1024 * 1024)
        .await
        .map_err(|error| format!("读取网关响应失败：{error}"))?;
    if !status.is_success() {
        return Err(format!(
            "网关返回 {status}：{}",
            String::from_utf8_lossy(&bytes).trim()
        ));
    }
    serde_json::from_slice(&bytes).map_err(|error| format!("网关响应不是有效 JSON：{error}"))
}

/// 鉴权后的公共主体：渠道解析 → IR 转换 → 出网 → 响应回转与日志落库。
async fn dispatch_chat_request(
    ctx: &ModelProxyContext,
    headers: &HeaderMap,
    body: &JsonValue,
    prep: PreparedChat,
) -> Response {
    let PreparedChat {
        config,
        raw_model,
        is_stream,
        req_id,
        start_time,
        req_body_str,
    } = prep;

    let (chan, model_to_send) = match resolve_channel_or_404(
        &ctx,
        &config,
        &raw_model,
        CHAT_PATH,
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

    // 请求侧 IR：Chat 入口同样解析为通用对象，出网由目标序列化器展开
    let egress_payload = crate::model::gateway::egress::EgressBody::Universal(
        crate::model::gateway::parsers::chat_to_universal(body, &model_to_send),
    );

    let outcome = match dispatch_protocol_egress(
        &ctx,
        &config,
        chan,
        &model_to_send,
        &raw_model,
        CHAT_PATH,
        &req_id,
        is_stream,
        start_time,
        &req_body_str,
        egress_payload,
        ClientProtocol::OpenAi,
    )
    .await
    {
        Ok(o) => o,
        Err(res) => return res,
    };

    let mut log = outcome.base_log(CHAT_PATH, &raw_model, is_stream, req_body_str);
    log.client_name = Some(client_name_from_headers(headers, CHAT_PATH));

    if is_stream {
        // 出网已按渠道目标原生化，响应协议即 outcome.target（嗅探失败时的正确回退）
        let (tool_hints, preferred_tool) = crate::model::gateway::stream::extract_tool_hints(body);
        let upstream_headers = outcome.success.response.headers().clone();
        let stream_body = crate::model::gateway::stream::proxy_sse_body_with_hints(
            outcome.success.response.bytes_stream(),
            outcome.target,
            crate::model::gateway::stream::SseClientProtocol::Chat,
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
        egress::copy_upstream_headers(&upstream_headers, resp)
    } else {
        let upstream_headers = outcome.success.response.headers().clone();
        let raw_bytes = outcome.success.response.bytes().await.unwrap_or_default();
        // 日志记录上游响应原文（未归一化），便于排查协议/内容问题
        let resp_body = cap_log_body(String::from_utf8_lossy(&raw_bytes).to_string());
        let dur = outcome.success.cand_start.elapsed().as_millis() as u64;

        let mut prompt_toks = None;
        let mut comp_toks = None;
        let mut reas_toks = None;
        let mut cache_toks = None;
        let mut cache_creation_toks = None;
        let mut total_toks = None;
        let mut has_reasoning = false;

        let resp_bytes =
            egress::normalize_response_bytes(outcome.target, &outcome.model_to_send, &raw_bytes);
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

            return egress::copy_upstream_headers(&upstream_headers, Json(jv).into_response());
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
}
