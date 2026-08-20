use super::adapters::GeminiProtocolAdapter;
use super::types::{ModelProxyContext, ProxyRequestLog};
use axum::body::Body;
use bytes::Bytes;
use futures_util::stream::StreamExt;
use serde_json::{json, Value as JsonValue};
use std::sync::atomic::Ordering;
use std::time::Instant;

/// OpenAI 标准 SSE 流清洗与指标统计
pub fn clean_sse_stream(
    stream: impl futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    ctx: ModelProxyContext,
    mut log: ProxyRequestLog,
    start_time: Instant,
    _raw_model: String,
) -> Body {
    let s = async_stream::stream! {
        let mut buffer = String::new();
        let mut ttft_recorded = false;
        let mut total_prompt_tokens = 0u64;
        let mut total_completion_tokens = 0u64;
        let mut total_reasoning_tokens = 0u64;
        let mut total_cache_hit_tokens = 0u64;
        let mut total_all_tokens = 0u64;
        let mut has_reasoning = false;

        tokio::pin!(stream);

        while let Some(chunk_res) = stream.next().await {
            match chunk_res {
                Ok(bytes) => {
                    if let Ok(text) = std::str::from_utf8(&bytes) {
                        buffer.push_str(text);

                        while let Some(newline_pos) = buffer.find('\n') {
                            let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
                            buffer = buffer[newline_pos + 1..].to_string();

                            if line.starts_with("data: ") {
                                let data = &line["data: ".len()..];
                                if data == "[DONE]" {
                                    yield Ok::<Bytes, std::io::Error>(Bytes::from("data: [DONE]\n\n"));
                                    continue;
                                }

                                if let Ok(mut jv) = serde_json::from_str::<JsonValue>(data) {
                                    if !ttft_recorded {
                                        let ttft = start_time.elapsed().as_millis() as u64;
                                        log.ttft_ms = Some(ttft);
                                        ttft_recorded = true;
                                    }

                                    if let Some(usage) = jv.get("usage").and_then(JsonValue::as_object) {
                                        if let Some(p) = usage.get("prompt_tokens").and_then(JsonValue::as_u64) {
                                            total_prompt_tokens = p;
                                        }
                                        if let Some(c) = usage.get("completion_tokens").and_then(JsonValue::as_u64) {
                                            total_completion_tokens = c;
                                        }
                                        if let Some(t) = usage.get("total_tokens").and_then(JsonValue::as_u64) {
                                            total_all_tokens = t;
                                        }
                                        if let Some(details) = usage.get("prompt_tokens_details").and_then(JsonValue::as_object) {
                                            if let Some(h) = details.get("cached_tokens").and_then(JsonValue::as_u64) {
                                                total_cache_hit_tokens = h;
                                            }
                                        }
                                        if let Some(details) = usage.get("completion_tokens_details").and_then(JsonValue::as_object) {
                                            if let Some(r) = details.get("reasoning_tokens").and_then(JsonValue::as_u64) {
                                                total_reasoning_tokens = r;
                                                has_reasoning = true;
                                            }
                                        }
                                    }

                                    if let Some(delta) = jv.pointer_mut("/choices/0/delta") {
                                        let mut extracted_reasoning = None;
                                        if let Some(content) = delta.get_mut("content") {
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
                                            delta["reasoning_content"] = JsonValue::String(reasoning);
                                        }
                                    }

                                    let cleaned_data = serde_json::to_string(&jv).unwrap_or_else(|_| data.to_string());
                                    yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("data: {cleaned_data}\n\n")));
                                } else {
                                    yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("{line}\n")));
                                }
                            } else if !line.is_empty() {
                                yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("{line}\n")));
                            }
                        }
                    }
                }
                Err(err) => {
                    log.error_message = Some(format!("流式传输中断: {err}"));
                    break;
                }
            }
        }

        let dur = start_time.elapsed().as_millis() as u64;
        log.duration_ms = dur;
        log.status_code = 200;
        log.prompt_tokens = (total_prompt_tokens > 0).then_some(total_prompt_tokens);
        log.completion_tokens = (total_completion_tokens > 0).then_some(total_completion_tokens);
        log.reasoning_tokens = (total_reasoning_tokens > 0).then_some(total_reasoning_tokens);
        log.prompt_cache_hit_tokens = (total_cache_hit_tokens > 0).then_some(total_cache_hit_tokens);
        log.total_tokens = if total_all_tokens > 0 {
            Some(total_all_tokens)
        } else if total_prompt_tokens + total_completion_tokens > 0 {
            Some(total_prompt_tokens + total_completion_tokens)
        } else {
            None
        };

        if has_reasoning {
            ctx.metrics.total_reasoning_requests.fetch_add(1, Ordering::Relaxed);
        }
        if total_prompt_tokens > 0 {
            ctx.metrics.total_prompt_tokens.fetch_add(total_prompt_tokens, Ordering::Relaxed);
        }
        if total_completion_tokens > 0 {
            ctx.metrics.total_completion_tokens.fetch_add(total_completion_tokens, Ordering::Relaxed);
        }
        if total_reasoning_tokens > 0 {
            ctx.metrics.total_reasoning_tokens.fetch_add(total_reasoning_tokens, Ordering::Relaxed);
        }
        if total_cache_hit_tokens > 0 {
            ctx.metrics.total_cache_hit_tokens.fetch_add(total_cache_hit_tokens, Ordering::Relaxed);
        }
        if let Some(t) = log.total_tokens {
            ctx.metrics.total_tokens.fetch_add(t, Ordering::Relaxed);
        }

        ctx.record_log(log).await;
    };

    Body::from_stream(s)
}

/// OpenAI SSE -> Anthropic Messages SSE
pub fn openai_to_anthropic_sse_stream(
    stream: impl futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    ctx: ModelProxyContext,
    mut log: ProxyRequestLog,
    start_time: Instant,
    model_name: String,
) -> Body {
    let s = async_stream::stream! {
        let mut buffer = String::new();
        let mut message_started = false;
        let mut content_block_started = false;
        let mut ttft_recorded = false;
        let mut total_prompt_tokens = 0u64;
        let mut total_completion_tokens = 0u64;

        tokio::pin!(stream);

        while let Some(chunk_res) = stream.next().await {
            match chunk_res {
                Ok(bytes) => {
                    if let Ok(text) = std::str::from_utf8(&bytes) {
                        buffer.push_str(text);

                        while let Some(newline_pos) = buffer.find('\n') {
                            let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
                            buffer = buffer[newline_pos + 1..].to_string();

                            if line.starts_with("data: ") {
                                let data = &line["data: ".len()..];
                                if data == "[DONE]" {
                                    let stop_event = json!({
                                        "type": "message_stop"
                                    });
                                    yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("event: message_stop\ndata: {stop_event}\n\n")));
                                    continue;
                                }

                                if let Ok(jv) = serde_json::from_str::<JsonValue>(data) {
                                    if !ttft_recorded {
                                        let ttft = start_time.elapsed().as_millis() as u64;
                                        log.ttft_ms = Some(ttft);
                                        ttft_recorded = true;
                                    }

                                    if !message_started {
                                        let start_event = json!({
                                            "type": "message_start",
                                            "message": {
                                                "id": format!("msg_{}", log.id),
                                                "type": "message",
                                                "role": "assistant",
                                                "model": model_name,
                                                "content": [],
                                                "stop_reason": null,
                                                "stop_sequence": null,
                                                "usage": { "input_tokens": 0, "output_tokens": 0 }
                                            }
                                        });
                                        yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("event: message_start\ndata: {start_event}\n\n")));
                                        message_started = true;
                                    }

                                    if let Some(usage) = jv.get("usage").and_then(JsonValue::as_object) {
                                        if let Some(p) = usage.get("prompt_tokens").and_then(JsonValue::as_u64) {
                                            total_prompt_tokens = p;
                                        }
                                        if let Some(c) = usage.get("completion_tokens").and_then(JsonValue::as_u64) {
                                            total_completion_tokens = c;
                                        }
                                    }

                                    if let Some(delta) = jv.pointer("/choices/0/delta") {
                                        if let Some(content) = delta.get("content").and_then(JsonValue::as_str) {
                                            if !content.is_empty() {
                                                if !content_block_started {
                                                    let block_start = json!({
                                                        "type": "content_block_start",
                                                        "index": 0,
                                                        "content_block": { "type": "text", "text": "" }
                                                    });
                                                    yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("event: content_block_start\ndata: {block_start}\n\n")));
                                                    content_block_started = true;
                                                }

                                                let delta_event = json!({
                                                    "type": "content_block_delta",
                                                    "index": 0,
                                                    "delta": { "type": "text_delta", "text": content }
                                                });
                                                yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("event: content_block_delta\ndata: {delta_event}\n\n")));
                                            }
                                        }
                                    }

                                    if let Some(finish_reason) = jv.pointer("/choices/0/finish_reason").and_then(JsonValue::as_str) {
                                        let stop_reason = match finish_reason {
                                            "stop" => "end_turn",
                                            "length" => "max_tokens",
                                            "tool_calls" => "tool_use",
                                            _ => "end_turn",
                                        };
                                        let msg_delta = json!({
                                            "type": "message_delta",
                                            "delta": {
                                                "stop_reason": stop_reason,
                                                "stop_sequence": null
                                            },
                                            "usage": {
                                                "output_tokens": total_completion_tokens
                                            }
                                        });
                                        yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("event: message_delta\ndata: {msg_delta}\n\n")));
                                    }
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    log.error_message = Some(format!("Anthropic 流式响应传输中断: {err}"));
                    break;
                }
            }
        }

        let dur = start_time.elapsed().as_millis() as u64;
        log.duration_ms = dur;
        log.status_code = 200;
        log.prompt_tokens = (total_prompt_tokens > 0).then_some(total_prompt_tokens);
        log.completion_tokens = (total_completion_tokens > 0).then_some(total_completion_tokens);
        log.total_tokens = (total_prompt_tokens + total_completion_tokens > 0).then_some(total_prompt_tokens + total_completion_tokens);

        ctx.record_log(log).await;
    };

    Body::from_stream(s)
}

/// OpenAI SSE -> Google Gemini SSE
pub fn openai_to_gemini_sse_stream(
    stream: impl futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    ctx: ModelProxyContext,
    mut log: ProxyRequestLog,
    start_time: Instant,
    model_name: String,
) -> Body {
    let s = async_stream::stream! {
        let mut buffer = String::new();
        let mut ttft_recorded = false;
        let mut total_prompt_tokens = 0u64;
        let mut total_completion_tokens = 0u64;

        tokio::pin!(stream);

        while let Some(chunk_res) = stream.next().await {
            match chunk_res {
                Ok(bytes) => {
                    if let Ok(text) = std::str::from_utf8(&bytes) {
                        buffer.push_str(text);

                        while let Some(newline_pos) = buffer.find('\n') {
                            let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
                            buffer = buffer[newline_pos + 1..].to_string();

                            if line.starts_with("data: ") {
                                let data = &line["data: ".len()..];
                                if data == "[DONE]" {
                                    continue;
                                }

                                if let Ok(jv) = serde_json::from_str::<JsonValue>(data) {
                                    if !ttft_recorded {
                                        let ttft = start_time.elapsed().as_millis() as u64;
                                        log.ttft_ms = Some(ttft);
                                        ttft_recorded = true;
                                    }

                                    if let Some(usage) = jv.get("usage").and_then(JsonValue::as_object) {
                                        if let Some(p) = usage.get("prompt_tokens").and_then(JsonValue::as_u64) {
                                            total_prompt_tokens = p;
                                        }
                                        if let Some(c) = usage.get("completion_tokens").and_then(JsonValue::as_u64) {
                                            total_completion_tokens = c;
                                        }
                                    }

                                    if let Some(gemini_chunk) = GeminiProtocolAdapter::openai_chunk_to_gemini_chunk(&jv, &model_name) {
                                        let out_str = serde_json::to_string(&gemini_chunk).unwrap_or_default();
                                        yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("data: {out_str}\n\n")));
                                    }
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    log.error_message = Some(format!("Gemini 流式响应传输中断: {err}"));
                    break;
                }
            }
        }

        let dur = start_time.elapsed().as_millis() as u64;
        log.duration_ms = dur;
        log.status_code = 200;
        log.prompt_tokens = (total_prompt_tokens > 0).then_some(total_prompt_tokens);
        log.completion_tokens = (total_completion_tokens > 0).then_some(total_completion_tokens);
        log.total_tokens = (total_prompt_tokens + total_completion_tokens > 0).then_some(total_prompt_tokens + total_completion_tokens);

        ctx.record_log(log).await;
    };

    Body::from_stream(s)
}

/// OpenAI SSE -> Responses SSE
pub fn openai_to_responses_sse_stream(
    stream: impl futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    ctx: ModelProxyContext,
    mut log: ProxyRequestLog,
    start_time: Instant,
    model_name: String,
) -> Body {
    let s = async_stream::stream! {
        let mut buffer = String::new();
        let mut response_started = false;
        let mut ttft_recorded = false;
        let mut total_prompt_tokens = 0u64;
        let mut total_completion_tokens = 0u64;

        tokio::pin!(stream);

        while let Some(chunk_res) = stream.next().await {
            match chunk_res {
                Ok(bytes) => {
                    if let Ok(text) = std::str::from_utf8(&bytes) {
                        buffer.push_str(text);

                        while let Some(newline_pos) = buffer.find('\n') {
                            let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
                            buffer = buffer[newline_pos + 1..].to_string();

                            if line.starts_with("data: ") {
                                let data = &line["data: ".len()..];
                                if data == "[DONE]" {
                                    yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("event: response.done\ndata: {}\n\n", json!({"type": "response.done"}))));
                                    continue;
                                }

                                if let Ok(jv) = serde_json::from_str::<JsonValue>(data) {
                                    if !ttft_recorded {
                                        let ttft = start_time.elapsed().as_millis() as u64;
                                        log.ttft_ms = Some(ttft);
                                        ttft_recorded = true;
                                    }

                                    if !response_started {
                                        let created = json!({
                                            "type": "response.created",
                                            "response": {
                                                "id": format!("resp_{}", log.id),
                                                "model": model_name,
                                                "status": "in_progress"
                                            }
                                        });
                                        yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("event: response.created\ndata: {created}\n\n")));
                                        response_started = true;
                                    }

                                    if let Some(usage) = jv.get("usage").and_then(JsonValue::as_object) {
                                        if let Some(p) = usage.get("prompt_tokens").and_then(JsonValue::as_u64) {
                                            total_prompt_tokens = p;
                                        }
                                        if let Some(c) = usage.get("completion_tokens").and_then(JsonValue::as_u64) {
                                            total_completion_tokens = c;
                                        }
                                    }

                                    if let Some(delta) = jv.pointer("/choices/0/delta") {
                                        if let Some(content) = delta.get("content").and_then(JsonValue::as_str) {
                                            if !content.is_empty() {
                                                let delta_event = json!({
                                                    "type": "response.output_text.delta",
                                                    "delta": content
                                                });
                                                yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("event: response.output_text.delta\ndata: {delta_event}\n\n")));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    log.error_message = Some(format!("Responses 流式响应传输中断: {err}"));
                    break;
                }
            }
        }

        let dur = start_time.elapsed().as_millis() as u64;
        log.duration_ms = dur;
        log.status_code = 200;
        log.prompt_tokens = (total_prompt_tokens > 0).then_some(total_prompt_tokens);
        log.completion_tokens = (total_completion_tokens > 0).then_some(total_completion_tokens);
        log.total_tokens = (total_prompt_tokens + total_completion_tokens > 0).then_some(total_prompt_tokens + total_completion_tokens);

        ctx.record_log(log).await;
    };

    Body::from_stream(s)
}
