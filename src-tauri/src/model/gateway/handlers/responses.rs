//! POST /v1/responses — OpenAI Responses API 入口。
//!
//! 常规路径：Responses → OpenAI 中枢 → 渠道目标协议，响应再转回 Responses。
//! 同协议快速通道（渠道目标 = Responses）：请求体原生透传（仅重写模型名），
//! 非流式响应原样下发，仅旁路提取 usage 供日志统计。

use super::super::adapters::{OpenAiProtocolAdapter, ResponsesProtocolAdapter};
use super::super::egress;
use super::super::logger::{cap_log_body, client_name_from_headers};
use super::super::pipeline::{
    auth_and_count, dispatch_protocol_egress, resolve_channel_or_404, ClientProtocol,
};
use super::super::stream::openai_to_responses_sse_stream;
use super::super::types::{generate_req_id, ModelProxyContext};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value as JsonValue};
use std::time::Instant;

pub async fn handle_responses(
    headers: HeaderMap,
    uri: Uri,
    State(ctx): State<ModelProxyContext>,
    Json(body): Json<JsonValue>,
) -> Response {
    const PATH: &str = "/v1/responses";
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

    // 上游统一走 /v1/chat/completions（OpenAI Chat 格式），取消同协议快速通道
    let (egress_body, convert) = {
        let mut openai_body = body.clone();
        openai_body["model"] = JsonValue::String(model_to_send.clone());
        ResponsesProtocolAdapter::convert_input_to_messages(&mut openai_body);
        OpenAiProtocolAdapter::sanitize_and_normalize(&mut openai_body);
        (openai_body, true)
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
        let stream_body = openai_to_responses_sse_stream(
            {
                let (tool_hints, preferred_tool) =
                    crate::model::gateway::stream::extract_tool_hints(&body);
                egress::normalized_sse_stream(
                    outcome.success.response.bytes_stream(),
                    egress::TargetProtocol::OpenAiChat,
                    tool_hints,
                    preferred_tool,
                )
            },
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

    let resp_bytes =
        egress::normalize_response_bytes(egress::TargetProtocol::OpenAiChat, &outcome.model_to_send, &raw_bytes);
    let resp_body = cap_log_body(String::from_utf8_lossy(&resp_bytes).to_string());

    if let Ok(jv) = serde_json::from_slice::<JsonValue>(&resp_bytes) {
        // 完整转换 OpenAI message → Responses output 数组：
        // 此前仅取 content 文本，纯 tool_calls / reasoning 响应会变成空输出
        let message = jv.pointer("/choices/0/message").cloned().unwrap_or(json!({}));
        let text = message
            .get("content")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        let reasoning = message
            .get("reasoning_content")
            .or_else(|| message.get("reasoning"))
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        let mut output: Vec<JsonValue> = Vec::new();
        if !reasoning.trim().is_empty() {
            output.push(json!({
                "id": format!("rs_{req_id}"),
                "type": "reasoning",
                "summary": [{ "type": "summary_text", "text": reasoning }],
            }));
        }
        if !text.is_empty() {
            output.push(json!({
                "id": format!("msg_{req_id}"),
                "type": "message",
                "status": "completed",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": text,
                    "annotations": [],
                }],
            }));
        }
        if let Some(tool_calls) = message.get("tool_calls").and_then(JsonValue::as_array) {
            for (idx, tc) in tool_calls.iter().enumerate() {
                let call_id = tc
                    .get("id")
                    .and_then(JsonValue::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("call_{idx}"));
                let name = tc
                    .pointer("/function/name")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("tool");
                let arguments = tc
                    .pointer("/function/arguments")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("{}");
                output.push(json!({
                    "id": format!("fc_{req_id}_{idx}"),
                    "type": "function_call",
                    "status": "completed",
                    "call_id": call_id,
                    "name": name,
                    "arguments": arguments,
                }));
            }
        }

        let usage = jv.get("usage").cloned().unwrap_or(json!({}));
        let responses_usage = json!({
            "input_tokens": usage.get("prompt_tokens").and_then(JsonValue::as_u64).unwrap_or(0),
            "input_tokens_details": {
                "cached_tokens": usage
                    .pointer("/prompt_tokens_details/cached_tokens")
                    .and_then(JsonValue::as_u64)
                    .unwrap_or(0),
            },
            "output_tokens": usage.get("completion_tokens").and_then(JsonValue::as_u64).unwrap_or(0),
            "output_tokens_details": {
                "reasoning_tokens": usage
                    .pointer("/completion_tokens_details/reasoning_tokens")
                    .and_then(JsonValue::as_u64)
                    .unwrap_or(0),
            },
            "total_tokens": usage.get("total_tokens").and_then(JsonValue::as_u64).unwrap_or(0),
        });

        let responses_output = json!({
            "id": format!("resp_{req_id}"),
            "object": "response",
            "created_at": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            "status": "completed",
            "model": raw_model,
            "output": output,
            "usage": responses_usage,
        });

        let mut final_log = log;
        final_log.duration_ms = dur;
        final_log.response_body = resp_body;
        final_log.prompt_tokens = jv
            .pointer("/usage/prompt_tokens")
            .and_then(JsonValue::as_u64)
            .filter(|v| *v > 0);
        final_log.completion_tokens = jv
            .pointer("/usage/completion_tokens")
            .and_then(JsonValue::as_u64)
            .filter(|v| *v > 0);
        final_log.prompt_cache_hit_tokens = jv
            .pointer("/usage/prompt_tokens_details/cached_tokens")
            .and_then(JsonValue::as_u64)
            .filter(|v| *v > 0);
        final_log.reasoning_tokens = jv
            .pointer("/usage/completion_tokens_details/reasoning_tokens")
            .and_then(JsonValue::as_u64)
            .filter(|v| *v > 0);
        final_log.total_tokens = jv
            .pointer("/usage/total_tokens")
            .and_then(JsonValue::as_u64)
            .filter(|v| *v > 0);
        ctx.record_log(final_log).await;
        return Json(responses_output).into_response();
    }

    let mut final_log = log;
    final_log.duration_ms = dur;
    final_log.response_body = resp_body;
    ctx.record_log(final_log).await;
    (StatusCode::OK, resp_bytes).into_response()
}
