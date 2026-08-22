use super::adapters::GeminiProtocolAdapter;
use super::logger::cap_log_body;
use super::types::{ModelProxyContext, ProxyRequestLog};
use axum::body::Body;
use bytes::Bytes;
use futures_util::stream::StreamExt;
use serde_json::{json, Value as JsonValue};
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::time::Instant;

/// 单个工具调用参数的流式累积
#[derive(Default)]
struct ToolCallAccum {
    id: String,
    name: String,
    arguments: String,
}

/// 流式响应内容累积器：从 OpenAI SSE delta 中重建一份完整响应全文，供日志详情展示
#[derive(Default)]
struct StreamResponseAccumulator {
    content: String,
    reasoning: String,
    finish_reason: Option<String>,
    tool_calls: BTreeMap<u64, ToolCallAccum>,
}

impl StreamResponseAccumulator {
    fn observe_chunk(&mut self, jv: &JsonValue) {
        if let Some(fr) = jv.pointer("/choices/0/finish_reason").and_then(JsonValue::as_str) {
            self.finish_reason = Some(fr.to_string());
        }
        let Some(delta) = jv.pointer("/choices/0/delta") else {
            return;
        };
        if let Some(s) = delta.get("content").and_then(JsonValue::as_str) {
            self.content.push_str(s);
        }
        if let Some(s) = delta
            .get("reasoning_content")
            .or_else(|| delta.get("reasoning"))
            .and_then(JsonValue::as_str)
        {
            self.reasoning.push_str(s);
        }
        if let Some(tcs) = delta.get("tool_calls").and_then(JsonValue::as_array) {
            for tc in tcs {
                let idx = tc.get("index").and_then(JsonValue::as_u64).unwrap_or(0);
                let entry = self.tool_calls.entry(idx).or_default();
                if let Some(id) = tc.get("id").and_then(JsonValue::as_str) {
                    entry.id.push_str(id);
                }
                if let Some(name) = tc.pointer("/function/name").and_then(JsonValue::as_str) {
                    entry.name.push_str(name);
                }
                if let Some(args) = tc.pointer("/function/arguments").and_then(JsonValue::as_str) {
                    entry.arguments.push_str(args);
                }
            }
        }
    }

    /// 重建 OpenAI 格式响应全文；流式期间未产生任何内容时返回 None
    fn build_response_body(&self) -> Option<String> {
        if self.content.is_empty() && self.reasoning.is_empty() && self.tool_calls.is_empty() {
            return None;
        }
        let mut message = json!({ "role": "assistant", "content": self.content });
        if !self.reasoning.is_empty() {
            message["reasoning_content"] = JsonValue::String(self.reasoning.clone());
        }
        if !self.tool_calls.is_empty() {
            let calls: Vec<JsonValue> = self
                .tool_calls
                .iter()
                .map(|(idx, tc)| {
                    json!({
                        "id": tc.id,
                        "type": "function",
                        "index": idx,
                        "function": { "name": tc.name, "arguments": tc.arguments }
                    })
                })
                .collect();
            message["tool_calls"] = JsonValue::Array(calls);
        }
        let body = json!({
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": self.finish_reason,
            }]
        });
        cap_log_body(body.to_string())
    }
}


/// SSE 流解析过程中的 token 统计
struct SseTokenStats {
    prompt_tokens: u64,
    completion_tokens: u64,
    reasoning_tokens: u64,
    cache_hit_tokens: u64,
    cache_creation_tokens: u64,
    total_tokens: u64,
    has_reasoning: bool,
}

impl SseTokenStats {
    fn new() -> Self {
        Self {
            prompt_tokens: 0,
            completion_tokens: 0,
            reasoning_tokens: 0,
            cache_hit_tokens: 0,
            cache_creation_tokens: 0,
            total_tokens: 0,
            has_reasoning: false,
        }
    }

    /// 从 OpenAI usage 对象中提取 token 统计（含网关扩展的 cache_creation_tokens）
    fn extract_from_usage(&mut self, jv: &JsonValue) {
        if let Some(usage) = jv.get("usage").and_then(JsonValue::as_object) {
            if let Some(p) = usage.get("prompt_tokens").and_then(JsonValue::as_u64) {
                self.prompt_tokens = p;
            }
            if let Some(c) = usage.get("completion_tokens").and_then(JsonValue::as_u64) {
                self.completion_tokens = c;
            }
            if let Some(t) = usage.get("total_tokens").and_then(JsonValue::as_u64) {
                self.total_tokens = t;
            }
            if let Some(details) = usage.get("prompt_tokens_details").and_then(JsonValue::as_object) {
                if let Some(h) = details.get("cached_tokens").and_then(JsonValue::as_u64) {
                    self.cache_hit_tokens = h;
                }
                if let Some(w) = details.get("cache_creation_tokens").and_then(JsonValue::as_u64) {
                    self.cache_creation_tokens = w;
                }
            }
            if let Some(details) = usage.get("completion_tokens_details").and_then(JsonValue::as_object) {
                if let Some(r) = details.get("reasoning_tokens").and_then(JsonValue::as_u64) {
                    self.reasoning_tokens = r;
                    self.has_reasoning = true;
                }
            }
        }
    }

    /// 将统计写入日志字段（不触碰内存指标）
    fn apply_to_log(&self, log: &mut ProxyRequestLog) {
        log.prompt_tokens = (self.prompt_tokens > 0).then_some(self.prompt_tokens);
        log.completion_tokens = (self.completion_tokens > 0).then_some(self.completion_tokens);
        log.reasoning_tokens = (self.reasoning_tokens > 0).then_some(self.reasoning_tokens);
        log.prompt_cache_hit_tokens = (self.cache_hit_tokens > 0).then_some(self.cache_hit_tokens);
        log.cache_creation_tokens = (self.cache_creation_tokens > 0).then_some(self.cache_creation_tokens);
        log.total_tokens = if self.total_tokens > 0 {
            Some(self.total_tokens)
        } else if self.prompt_tokens + self.completion_tokens > 0 {
            Some(self.prompt_tokens + self.completion_tokens)
        } else {
            None
        };
    }

    /// 将统计写入日志并更新指标
    async fn finalize(&self, ctx: &ModelProxyContext, log: &mut ProxyRequestLog) {
        self.apply_to_log(log);

        if self.has_reasoning {
            ctx.metrics.total_reasoning_requests.fetch_add(1, Ordering::Relaxed);
        }
        if self.prompt_tokens > 0 {
            ctx.metrics.total_prompt_tokens.fetch_add(self.prompt_tokens, Ordering::Relaxed);
        }
        if self.completion_tokens > 0 {
            ctx.metrics.total_completion_tokens.fetch_add(self.completion_tokens, Ordering::Relaxed);
        }
        if self.reasoning_tokens > 0 {
            ctx.metrics.total_reasoning_tokens.fetch_add(self.reasoning_tokens, Ordering::Relaxed);
        }
        if self.cache_hit_tokens > 0 {
            ctx.metrics.total_cache_hit_tokens.fetch_add(self.cache_hit_tokens, Ordering::Relaxed);
        }
        if let Some(t) = log.total_tokens {
            ctx.metrics.total_tokens.fetch_add(t, Ordering::Relaxed);
        }
    }
}

/// 从 SSE 行中提取 data 内容，返回 (data_str, is_done)
/// 如果不是 data 行，返回 None
fn parse_sse_data_line(line: &str) -> Option<(&str, bool)> {
    if !line.starts_with("data: ") {
        return None;
    }
    let data = &line["data: ".len()..];
    let is_done = data == "[DONE]";
    Some((data, is_done))
}

/// OpenAI 标准 SSE 流清洗与指标统计
pub fn clean_sse_stream<E: std::fmt::Display + Send + 'static>(
    stream: impl futures_util::Stream<Item = Result<Bytes, E>> + Send + 'static,
    ctx: ModelProxyContext,
    mut log: ProxyRequestLog,
    start_time: Instant,
    _raw_model: String,
) -> Body {
    let s = async_stream::stream! {
        let mut buffer = String::new();
        let mut ttft_recorded = false;
        let mut stats = SseTokenStats::new();
        let mut has_reasoning = false;
        let mut accum = StreamResponseAccumulator::default();

        tokio::pin!(stream);

        while let Some(chunk_res) = stream.next().await {
            match chunk_res {
                Ok(bytes) => {
                    if let Ok(text) = std::str::from_utf8(&bytes) {
                        buffer.push_str(text);

                        while let Some(newline_pos) = buffer.find('\n') {
                            let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
                            buffer = buffer[newline_pos + 1..].to_string();

                            if let Some((data, is_done)) = parse_sse_data_line(&line) {
                                if is_done {
                                    yield Ok::<Bytes, std::io::Error>(Bytes::from("data: [DONE]\n\n"));
                                    continue;
                                }

                                if let Ok(mut jv) = serde_json::from_str::<JsonValue>(data) {
                                    if !ttft_recorded {
                                        let ttft = start_time.elapsed().as_millis() as u64;
                                        log.ttft_ms = Some(ttft);
                                        ttft_recorded = true;
                                    }

                                    stats.extract_from_usage(&jv);
                                    accum.observe_chunk(&jv);

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
        if log.error_message.is_some() {
            log.status_code = 502;
            ctx.metrics.successful_requests.fetch_sub(1, Ordering::Relaxed);
            ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
        } else {
            log.status_code = 200;
        }
        if has_reasoning {
            stats.has_reasoning = true;
        }
        stats.finalize(&ctx, &mut log).await;
        log.response_body = accum.build_response_body();

        ctx.record_log(log).await;
    };

    Body::from_stream(s)
}

/// OpenAI SSE -> Anthropic Messages SSE
pub fn openai_to_anthropic_sse_stream<E: std::fmt::Display + Send + 'static>(
    stream: impl futures_util::Stream<Item = Result<Bytes, E>> + Send + 'static,
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
        let mut stats = SseTokenStats::new();
        let mut accum = StreamResponseAccumulator::default();

        tokio::pin!(stream);

        while let Some(chunk_res) = stream.next().await {
            match chunk_res {
                Ok(bytes) => {
                    if let Ok(text) = std::str::from_utf8(&bytes) {
                        buffer.push_str(text);

                        while let Some(newline_pos) = buffer.find('\n') {
                            let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
                            buffer = buffer[newline_pos + 1..].to_string();

                            if let Some((data, is_done)) = parse_sse_data_line(&line) {
                                if is_done {
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
                                    accum.observe_chunk(&jv);
                                    stats.extract_from_usage(&jv);

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
                                                "output_tokens": stats.completion_tokens
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
        if log.error_message.is_some() {
            log.status_code = 502;
            ctx.metrics.successful_requests.fetch_sub(1, Ordering::Relaxed);
            ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
        } else {
            log.status_code = 200;
        }
        stats.apply_to_log(&mut log);
        log.response_body = accum.build_response_body();

        ctx.record_log(log).await;
    };

    Body::from_stream(s)
}

/// OpenAI SSE -> Google Gemini SSE
pub fn openai_to_gemini_sse_stream<E: std::fmt::Display + Send + 'static>(
    stream: impl futures_util::Stream<Item = Result<Bytes, E>> + Send + 'static,
    ctx: ModelProxyContext,
    mut log: ProxyRequestLog,
    start_time: Instant,
    model_name: String,
) -> Body {
    let s = async_stream::stream! {
        let mut buffer = String::new();
        let mut ttft_recorded = false;
        let mut stats = SseTokenStats::new();
        let mut accum = StreamResponseAccumulator::default();

        tokio::pin!(stream);

        while let Some(chunk_res) = stream.next().await {
            match chunk_res {
                Ok(bytes) => {
                    if let Ok(text) = std::str::from_utf8(&bytes) {
                        buffer.push_str(text);

                        while let Some(newline_pos) = buffer.find('\n') {
                            let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
                            buffer = buffer[newline_pos + 1..].to_string();

                            if let Some((data, is_done)) = parse_sse_data_line(&line) {
                                if is_done {
                                    continue;
                                }

                                if let Ok(jv) = serde_json::from_str::<JsonValue>(data) {
                                    if !ttft_recorded {
                                        let ttft = start_time.elapsed().as_millis() as u64;
                                        log.ttft_ms = Some(ttft);
                                        ttft_recorded = true;
                                    }
                                    accum.observe_chunk(&jv);
                                    stats.extract_from_usage(&jv);

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
        if log.error_message.is_some() {
            log.status_code = 502;
            ctx.metrics.successful_requests.fetch_sub(1, Ordering::Relaxed);
            ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
        } else {
            log.status_code = 200;
        }
        stats.apply_to_log(&mut log);
        log.response_body = accum.build_response_body();

        ctx.record_log(log).await;
    };

    Body::from_stream(s)
}

/// OpenAI SSE -> Responses SSE
pub fn openai_to_responses_sse_stream<E: std::fmt::Display + Send + 'static>(
    stream: impl futures_util::Stream<Item = Result<Bytes, E>> + Send + 'static,
    ctx: ModelProxyContext,
    mut log: ProxyRequestLog,
    start_time: Instant,
    model_name: String,
) -> Body {
    let s = async_stream::stream! {
        let mut buffer = String::new();
        let mut response_started = false;
        let mut ttft_recorded = false;
        let mut stats = SseTokenStats::new();
        let mut accum = StreamResponseAccumulator::default();

        tokio::pin!(stream);

        while let Some(chunk_res) = stream.next().await {
            match chunk_res {
                Ok(bytes) => {
                    if let Ok(text) = std::str::from_utf8(&bytes) {
                        buffer.push_str(text);

                        while let Some(newline_pos) = buffer.find('\n') {
                            let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
                            buffer = buffer[newline_pos + 1..].to_string();

                            if let Some((data, is_done)) = parse_sse_data_line(&line) {
                                if is_done {
                                    yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("event: response.done\ndata: {}\n\n", json!({"type": "response.done"}))));
                                    continue;
                                }

                                if let Ok(jv) = serde_json::from_str::<JsonValue>(data) {
                                    if !ttft_recorded {
                                        let ttft = start_time.elapsed().as_millis() as u64;
                                        log.ttft_ms = Some(ttft);
                                        ttft_recorded = true;
                                    }
                                    accum.observe_chunk(&jv);
                                    stats.extract_from_usage(&jv);

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
        if log.error_message.is_some() {
            log.status_code = 502;
            ctx.metrics.successful_requests.fetch_sub(1, Ordering::Relaxed);
            ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
        } else {
            log.status_code = 200;
        }
        stats.apply_to_log(&mut log);
        log.response_body = accum.build_response_body();

        ctx.record_log(log).await;
    };

    Body::from_stream(s)
}

#[cfg(test)]
mod stream_accumulator_tests {
    use super::*;

    fn feed(acc: &mut StreamResponseAccumulator, data: &str) {
        let jv: JsonValue = serde_json::from_str(data).unwrap();
        acc.observe_chunk(&jv);
    }

    #[test]
    fn accumulates_content_reasoning_and_tool_calls() {
        let mut acc = StreamResponseAccumulator::default();
        feed(&mut acc, r#"{"choices":[{"delta":{"reasoning_content":"思考A"}}]}"#);
        feed(&mut acc, r#"{"choices":[{"delta":{"content":"你"}}]}"#);
        feed(&mut acc, r#"{"choices":[{"delta":{"content":"好"}}]}"#);
        feed(&mut acc, r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_weather","arguments":"{\"city\":"}}]}}]}"#);
        feed(&mut acc, r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"北京\"}"}}]}}]}"#);
        feed(&mut acc, r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10}}"#);

        let body = acc.build_response_body().unwrap();
        let jv: JsonValue = serde_json::from_str(&body).unwrap();
        assert_eq!(jv.pointer("/choices/0/message/content").unwrap(), "你好");
        assert_eq!(jv.pointer("/choices/0/message/reasoning_content").unwrap(), "思考A");
        assert_eq!(jv.pointer("/choices/0/finish_reason").unwrap(), "tool_calls");
        let tcs = jv.pointer("/choices/0/message/tool_calls").unwrap().as_array().unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0]["function"]["name"], "get_weather");
        assert_eq!(tcs[0]["function"]["arguments"], r#"{"city":"北京"}"#);
    }

    #[test]
    fn empty_stream_yields_no_response_body() {
        let mut acc = StreamResponseAccumulator::default();
        feed(&mut acc, r#"{"choices":[{"delta":{}}]}"#);
        assert!(acc.build_response_body().is_none());
    }
}
