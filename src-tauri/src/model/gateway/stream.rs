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
        if let Some(fr) = jv
            .pointer("/choices/0/finish_reason")
            .and_then(JsonValue::as_str)
        {
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
                if let Some(args) = tc
                    .pointer("/function/arguments")
                    .and_then(JsonValue::as_str)
                {
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
            if let Some(details) = usage
                .get("prompt_tokens_details")
                .and_then(JsonValue::as_object)
            {
                if let Some(h) = details.get("cached_tokens").and_then(JsonValue::as_u64) {
                    self.cache_hit_tokens = h;
                }
                if let Some(w) = details
                    .get("cache_creation_tokens")
                    .and_then(JsonValue::as_u64)
                {
                    self.cache_creation_tokens = w;
                }
            }
            if let Some(details) = usage
                .get("completion_tokens_details")
                .and_then(JsonValue::as_object)
            {
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
        log.cache_creation_tokens =
            (self.cache_creation_tokens > 0).then_some(self.cache_creation_tokens);
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
            ctx.metrics
                .total_reasoning_requests
                .fetch_add(1, Ordering::Relaxed);
        }
        if self.prompt_tokens > 0 {
            ctx.metrics
                .total_prompt_tokens
                .fetch_add(self.prompt_tokens, Ordering::Relaxed);
        }
        if self.completion_tokens > 0 {
            ctx.metrics
                .total_completion_tokens
                .fetch_add(self.completion_tokens, Ordering::Relaxed);
        }
        if self.reasoning_tokens > 0 {
            ctx.metrics
                .total_reasoning_tokens
                .fetch_add(self.reasoning_tokens, Ordering::Relaxed);
        }
        if self.cache_hit_tokens > 0 {
            ctx.metrics
                .total_cache_hit_tokens
                .fetch_add(self.cache_hit_tokens, Ordering::Relaxed);
        }
        if let Some(t) = log.total_tokens {
            ctx.metrics.total_tokens.fetch_add(t, Ordering::Relaxed);
        }
    }
}

/// 跨 chunk 安全的 SSE 行读取器。
///
/// 历史缺陷警示：此前按「整块 from_utf8 失败即丢弃整个 chunk」处理，
/// 多字节字符（中文 3 字节 / emoji 4 字节）被网络分块切断时数据被静默吞掉；
/// 且 String 缓冲无法携带残缺字节到下一片。改为字节级缓冲：
/// UTF-8 多字节序列的续字节均 ≥ 0x80、不可能与 \n(0x0A) 冲突，
/// 因此按 \n 切行天然不会切断多字节字符，残缺尾部安全留存待拼。
pub(crate) struct SseLineReader {
    buf: Vec<u8>,
}

impl SseLineReader {
    pub(crate) fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// 追加一片原始字节，返回其中所有完整行（不含行尾 \n；\r 自动去除）
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut lines = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let drained: Vec<u8> = self.buf.drain(..=pos).collect();
            let end = drained.len() - 1;
            let mut line = String::from_utf8_lossy(&drained[..end]).to_string();
            if line.ends_with('\r') {
                line.pop();
            }
            lines.push(line);
        }
        lines
    }

    /// 流结束时冲刷不带换行的残余尾行
    pub(crate) fn flush(&mut self) -> Option<String> {
        if self.buf.is_empty() {
            return None;
        }
        let rest = std::mem::take(&mut self.buf);
        let mut line = String::from_utf8_lossy(&rest).to_string();
        if line.ends_with('\r') {
            line.pop();
        }
        Some(line)
    }
}

/// 从 SSE 行中提取 data 内容，返回 (data_str, is_done)。
/// 兼容 SSE 规范：`data:` 后空格可选（`data:x` 与 `data: x` 等价）。
fn parse_sse_data_line(line: &str) -> Option<(&str, bool)> {
    let rest = line.strip_prefix("data:")?;
    let data = rest.strip_prefix(' ').unwrap_or(rest);
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
        let mut reader = SseLineReader::new();
        let mut ttft_recorded = false;
        let mut stats = SseTokenStats::new();
        let mut has_reasoning = false;
        let mut accum = StreamResponseAccumulator::default();

        tokio::pin!(stream);

        while let Some(chunk_res) = stream.next().await {
            match chunk_res {
                Ok(bytes) => {
                    for line in reader.push(&bytes) {
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
                                // 无法解析的负载原样透传；补齐事件终止空行，避免客户端粘包
                                yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("{line}\n\n")));
                            }
                        } else if line.is_empty() {
                            // SSE 事件分隔空行：必须保留，否则透传的事件之间丢失边界
                            yield Ok::<Bytes, std::io::Error>(Bytes::from("\n"));
                        } else {
                            yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("{line}\n")));
                        }
                    }
                }
                Err(err) => {
                    log.error_message = Some(format!("流式传输中断: {err}"));
                    break;
                }
            }
        }

        if let Some(line) = reader.flush() {
            if let Some((data, false)) = parse_sse_data_line(&line) {
                if let Ok(jv) = serde_json::from_str::<JsonValue>(data) {
                    stats.extract_from_usage(&jv);
                    accum.observe_chunk(&jv);
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

fn sse_event(event: &str, data: &JsonValue) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

/// Anthropic content block 种类
#[derive(Clone, Copy, PartialEq)]
enum AnthropicBlockKind {
    Text,
    Thinking,
    Tool,
}

/// OpenAI SSE → Anthropic Messages SSE 的事件组装器。
///
/// 历史缺陷警示：旧实现只转发 `delta.content` 文本，`delta.tool_calls` 与
/// `delta.reasoning_content` 被整段丢弃 —— agent 客户端（如 opencode）收不到
/// 任何 tool_use 块，表现为「HTTP 200 但回复为空」。且 message_delta 在
/// usage chunk 到达前就发出，output_tokens 恒为 0。
///
/// 本组装器维护块开闭状态机，保证事件序列符合 Anthropic 规范：
/// message_start → (content_block_start → delta* → content_block_stop)* → message_delta → message_stop
struct AnthropicSseEmitter {
    msg_id: String,
    model: String,
    message_started: bool,
    next_index: u64,
    /// 当前打开的块：(anthropic 块索引, 种类)
    open_block: Option<(u64, AnthropicBlockKind)>,
    /// OpenAI tool_calls index → anthropic 块索引
    tool_blocks: BTreeMap<u64, u64>,
    finished: bool,
}

impl AnthropicSseEmitter {
    fn new(msg_id: &str, model: &str) -> Self {
        Self {
            msg_id: format!("msg_{msg_id}"),
            model: model.to_string(),
            message_started: false,
            next_index: 0,
            open_block: None,
            tool_blocks: BTreeMap::new(),
            finished: false,
        }
    }

    /// 确保消息头已发出（幂等）；input_tokens 无法从 OpenAI 流提前获知，置 0
    fn start_message(&mut self) -> Vec<String> {
        if self.message_started {
            return Vec::new();
        }
        self.message_started = true;
        vec![sse_event(
            "message_start",
            &json!({
                "type": "message_start",
                "message": {
                    "id": self.msg_id,
                    "type": "message",
                    "role": "assistant",
                    "model": self.model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": { "input_tokens": 0, "output_tokens": 0 }
                }
            }),
        )]
    }

    /// 关闭当前打开的块（若有）
    fn close_open_block(&mut self) -> Vec<String> {
        match self.open_block.take() {
            Some((idx, _)) => vec![sse_event(
                "content_block_stop",
                &json!({ "type": "content_block_stop", "index": idx }),
            )],
            None => Vec::new(),
        }
    }

    /// 打开一个新块并发出 content_block_start
    fn open_block(
        &mut self,
        kind: AnthropicBlockKind,
        content_block: JsonValue,
    ) -> (Vec<String>, u64) {
        let idx = self.next_index;
        self.next_index += 1;
        self.open_block = Some((idx, kind));
        (
            vec![sse_event(
                "content_block_start",
                &json!({
                    "type": "content_block_start",
                    "index": idx,
                    "content_block": content_block,
                }),
            )],
            idx,
        )
    }

    /// 切换到指定种类的块（必要时先关旧块再开新块），返回待发事件与新块索引
    fn switch_block(&mut self, kind: AnthropicBlockKind) -> (Vec<String>, Option<u64>) {
        let already_open = matches!(self.open_block, Some((_, cur)) if cur == kind);
        let had_open = self.open_block.is_some();

        let mut events = Vec::new();
        if already_open {
            return (events, self.open_block.map(|(idx, _)| idx));
        }
        if had_open {
            events.extend(self.close_open_block());
        } else {
            events.extend(self.start_message());
        }
        let (open_events, new_idx) = match kind {
            AnthropicBlockKind::Text => {
                self.open_block(AnthropicBlockKind::Text, json!({ "type": "text", "text": "" }))
            }
            AnthropicBlockKind::Thinking => self
                .open_block(AnthropicBlockKind::Thinking, json!({ "type": "thinking", "thinking": "" })),
            AnthropicBlockKind::Tool => unreachable!("tool block 必须携带 id/name 元数据"),
        };
        events.extend(open_events);
        (events, Some(new_idx))
    }

    /// 文本 delta
    fn feed_text(&mut self, text: &str) -> Vec<String> {
        if text.is_empty() {
            return Vec::new();
        }
        let (mut events, idx) = self.switch_block(AnthropicBlockKind::Text);
        let idx = idx.expect("text block opened");
        events.push(sse_event(
            "content_block_delta",
            &json!({
                "type": "content_block_delta",
                "index": idx,
                "delta": { "type": "text_delta", "text": text },
            }),
        ));
        events
    }

    /// 思考 delta
    fn feed_thinking(&mut self, text: &str) -> Vec<String> {
        if text.is_empty() {
            return Vec::new();
        }
        let (mut events, idx) = self.switch_block(AnthropicBlockKind::Thinking);
        let idx = idx.expect("thinking block opened");
        events.push(sse_event(
            "content_block_delta",
            &json!({
                "type": "content_block_delta",
                "index": idx,
                "delta": { "type": "thinking_delta", "thinking": text },
            }),
        ));
        events
    }

    /// 工具调用增量：tc_index 为 OpenAI 侧 tool_calls 数组下标；
    /// 首个分片携带 id/name，后续分片仅追加 arguments 片段
    fn feed_tool_call(
        &mut self,
        tc_index: u64,
        id: Option<&str>,
        name: Option<&str>,
        args_fragment: &str,
    ) -> Vec<String> {
        let mut events = self.start_message();
        let existing_block = self.tool_blocks.get(&tc_index).copied();
        let block_idx = match existing_block {
            Some(idx) => {
                // 同一工具的续片：若中间被文本/思考块插队，需切回工具块
                if self.open_block != Some((idx, AnthropicBlockKind::Tool)) {
                    let close_events = self.close_open_block();
                    events.extend(close_events);
                    self.open_block = Some((idx, AnthropicBlockKind::Tool));
                    events.push(sse_event(
                        "content_block_start",
                        &json!({
                            "type": "content_block_start",
                            "index": idx,
                            "content_block": {
                                "type": "tool_use",
                                "id": format!("toolu_reopen_{idx}"),
                                "name": "tool",
                                "input": {},
                            },
                        }),
                    ));
                }
                idx
            }
            None => {
                let close_events = self.close_open_block();
                events.extend(close_events);
                let tool_id = id
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("toolu_{tc_index}"));
                let tool_name = name
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| "tool".to_string());
                let (open_events, idx) = self.open_block(
                    AnthropicBlockKind::Tool,
                    json!({
                        "type": "tool_use",
                        "id": tool_id,
                        "name": tool_name,
                        "input": {},
                    }),
                );
                events.extend(open_events);
                self.tool_blocks.insert(tc_index, idx);
                idx
            }
        };
        if !args_fragment.is_empty() {
            events.push(sse_event(
                "content_block_delta",
                &json!({
                    "type": "content_block_delta",
                    "index": block_idx,
                    "delta": { "type": "input_json_delta", "partial_json": args_fragment },
                }),
            ));
        }
        events
    }

    /// 收尾：关闭打开的块 + message_delta（携带最终 output_tokens）+ message_stop。幂等。
    fn finish(&mut self, stop_reason: &str, output_tokens: u64) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let mut events = self.start_message();
        events.extend(self.close_open_block());
        events.push(sse_event(
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": { "stop_reason": stop_reason, "stop_sequence": null },
                "usage": { "output_tokens": output_tokens },
            }),
        ));
        events.push(sse_event("message_stop", &json!({ "type": "message_stop" })));
        events
    }
}

/// OpenAI finish_reason → Anthropic stop_reason
fn map_stop_reason(finish_reason: &str) -> &'static str {
    match finish_reason {
        "length" => "max_tokens",
        "tool_calls" | "function_call" => "tool_use",
        _ => "end_turn",
    }
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
        let mut reader = SseLineReader::new();
        let mut ttft_recorded = false;
        let mut stats = SseTokenStats::new();
        let mut accum = StreamResponseAccumulator::default();
        let mut emitter = AnthropicSseEmitter::new(&log.id, &model_name);
        let mut stop_reason: Option<&'static str> = None;

        tokio::pin!(stream);

        while let Some(chunk_res) = stream.next().await {
            match chunk_res {
                Ok(bytes) => {
                    for line in reader.push(&bytes) {
                        let Some((data, is_done)) = parse_sse_data_line(&line) else {
                            continue;
                        };
                        if is_done {
                            // usage 已全部就绪（include_usage 尾包或归一化合并包），此刻才回传真实统计
                            for event in emitter.finish(
                                stop_reason.unwrap_or("end_turn"),
                                stats.completion_tokens,
                            ) {
                                yield Ok::<Bytes, std::io::Error>(Bytes::from(event));
                            }
                            continue;
                        }

                        let Ok(jv) = serde_json::from_str::<JsonValue>(data) else {
                            continue;
                        };

                        if !ttft_recorded {
                            log.ttft_ms = Some(start_time.elapsed().as_millis() as u64);
                            ttft_recorded = true;
                        }
                        accum.observe_chunk(&jv);
                        stats.extract_from_usage(&jv);

                        let delta = jv.pointer("/choices/0/delta").cloned().unwrap_or(json!({}));

                        // 思考内容优先于正文（与上游产出顺序一致）
                        if let Some(s) = delta
                            .get("reasoning_content")
                            .or_else(|| delta.get("reasoning"))
                            .and_then(JsonValue::as_str)
                        {
                            for event in emitter.feed_thinking(s) {
                                yield Ok::<Bytes, std::io::Error>(Bytes::from(event));
                            }
                        }
                        if let Some(s) = delta.get("content").and_then(JsonValue::as_str) {
                            for event in emitter.feed_text(s) {
                                yield Ok::<Bytes, std::io::Error>(Bytes::from(event));
                            }
                        }
                        if let Some(tcs) = delta.get("tool_calls").and_then(JsonValue::as_array) {
                            for tc in tcs {
                                let idx = tc.get("index").and_then(JsonValue::as_u64).unwrap_or(0);
                                let id = tc.get("id").and_then(JsonValue::as_str);
                                let name =
                                    tc.pointer("/function/name").and_then(JsonValue::as_str);
                                let args = tc
                                    .pointer("/function/arguments")
                                    .and_then(JsonValue::as_str)
                                    .unwrap_or("");
                                for event in emitter.feed_tool_call(idx, id, name, args) {
                                    yield Ok::<Bytes, std::io::Error>(Bytes::from(event));
                                }
                            }
                        }

                        // 仅记录 stop_reason 并关闭当前块；message_delta 推迟到 [DONE]
                        // （此时 usage 才到达），避免 output_tokens 恒为 0
                        if let Some(fr) = jv
                            .pointer("/choices/0/finish_reason")
                            .and_then(JsonValue::as_str)
                        {
                            stop_reason = Some(map_stop_reason(fr));
                            for event in emitter.close_open_block() {
                                yield Ok::<Bytes, std::io::Error>(Bytes::from(event));
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

        // 冲刷残余尾行（上游未以换行结尾的最后一片）
        if let Some(line) = reader.flush() {
            if let Some((data, false)) = parse_sse_data_line(&line) {
                if let Ok(jv) = serde_json::from_str::<JsonValue>(data) {
                    accum.observe_chunk(&jv);
                    stats.extract_from_usage(&jv);
                }
            }
        }

        // 无论 [DONE] 是否到达都强制收尾，客户端不至于挂死
        for event in emitter.finish(stop_reason.unwrap_or("end_turn"), stats.completion_tokens) {
            yield Ok::<Bytes, std::io::Error>(Bytes::from(event));
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
        let mut reader = SseLineReader::new();
        let mut ttft_recorded = false;
        let mut stats = SseTokenStats::new();
        let mut accum = StreamResponseAccumulator::default();

        tokio::pin!(stream);

        while let Some(chunk_res) = stream.next().await {
            match chunk_res {
                Ok(bytes) => {
                    for line in reader.push(&bytes) {
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
                Err(err) => {
                    log.error_message = Some(format!("Gemini 流式响应传输中断: {err}"));
                    break;
                }
            }
        }

        // 冲刷残余尾行（上游未以换行结尾的最后一片）
        if let Some(line) = reader.flush() {
            if let Some((data, false)) = parse_sse_data_line(&line) {
                if let Ok(jv) = serde_json::from_str::<JsonValue>(data) {
                    accum.observe_chunk(&jv);
                    stats.extract_from_usage(&jv);
                    if let Some(gemini_chunk) =
                        GeminiProtocolAdapter::openai_chunk_to_gemini_chunk(&jv, &model_name)
                    {
                        let out_str = serde_json::to_string(&gemini_chunk).unwrap_or_default();
                        yield Ok::<Bytes, std::io::Error>(Bytes::from(format!(
                            "data: {out_str}\n\n"
                        )));
                    }
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
        let mut reader = SseLineReader::new();
        let mut response_started = false;
        let mut ttft_recorded = false;
        let mut stats = SseTokenStats::new();
        let mut accum = StreamResponseAccumulator::default();

        tokio::pin!(stream);

        while let Some(chunk_res) = stream.next().await {
            match chunk_res {
                Ok(bytes) => {
                    for line in reader.push(&bytes) {
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
                Err(err) => {
                    log.error_message = Some(format!("Responses 流式响应传输中断: {err}"));
                    break;
                }
            }
        }

        // 冲刷残余尾行（上游未以换行结尾的最后一片）
        if let Some(line) = reader.flush() {
            if let Some((data, false)) = parse_sse_data_line(&line) {
                if let Ok(jv) = serde_json::from_str::<JsonValue>(data) {
                    accum.observe_chunk(&jv);
                    stats.extract_from_usage(&jv);
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
        feed(
            &mut acc,
            r#"{"choices":[{"delta":{"reasoning_content":"思考A"}}]}"#,
        );
        feed(&mut acc, r#"{"choices":[{"delta":{"content":"你"}}]}"#);
        feed(&mut acc, r#"{"choices":[{"delta":{"content":"好"}}]}"#);
        feed(
            &mut acc,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_weather","arguments":"{\"city\":"}}]}}]}"#,
        );
        feed(
            &mut acc,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"北京\"}"}}]}}]}"#,
        );
        feed(
            &mut acc,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10}}"#,
        );

        let body = acc.build_response_body().unwrap();
        let jv: JsonValue = serde_json::from_str(&body).unwrap();
        assert_eq!(jv.pointer("/choices/0/message/content").unwrap(), "你好");
        assert_eq!(
            jv.pointer("/choices/0/message/reasoning_content").unwrap(),
            "思考A"
        );
        assert_eq!(
            jv.pointer("/choices/0/finish_reason").unwrap(),
            "tool_calls"
        );
        let tcs = jv
            .pointer("/choices/0/message/tool_calls")
            .unwrap()
            .as_array()
            .unwrap();
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

    #[test]
    fn sse_reader_survives_utf8_split_across_chunks() {
        // "你" = E4 BD A0：在其 3 字节中间切断，残缺部分必须等待拼接而非丢弃
        let mut reader = SseLineReader::new();
        let lines = reader.push(&[0xE4, 0xBD]);
        assert!(lines.is_empty(), "UTF-8 截断时不应产出任何行");
        let lines = reader.push(&[0xA0, b'\n']);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "你");

        // 完整事件跨 chunk（含空行与不带换行的尾行）
        // push 只返回以 \n 结尾的完整行：`data: {"ok":1}` + 空行；尾行 `event: pi` 留给 flush
        let lines = reader.push(b"data: {\"ok\":1}\n\nevent: pi");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1], "");
        assert_eq!(reader.flush().as_deref(), Some("event: pi"));
    }

    #[test]
    fn parse_data_line_accepts_optional_space() {
        // `data:` 后空格可选：`data:{"a":1}` 与 `data: {"a":1}` 等价
        assert_eq!(
            parse_sse_data_line(concat!("data:", "{\"a\":1}")),
            Some(("{\"a\":1}", false))
        );
        assert_eq!(
            parse_sse_data_line(concat!("data: ", "{\"a\":1}")),
            Some(("{\"a\":1}", false))
        );
        assert_eq!(parse_sse_data_line("data: [DONE]"), Some(("[DONE]", true)));
        assert_eq!(
            parse_sse_data_line("data:[DONE]").map(|(_, d)| d),
            Some(true)
        );
        assert_eq!(parse_sse_data_line("event: x"), None);
    }

    /// 把 emitter 产出的事件串解析为 (event 名, data JSON)
    fn parse_emitter_events(events: &[String]) -> Vec<(String, JsonValue)> {
        events
            .iter()
            .map(|e| {
                let trimmed = e.strip_suffix("\n\n").unwrap_or(e);
                let mut it = trimmed.splitn(2, "\ndata: ");
                // 事件名形如 "event: message_start"，剥掉固定前缀得到纯事件名
                let name = it
                    .next()
                    .unwrap_or_default()
                    .strip_prefix("event: ")
                    .unwrap_or_default()
                    .to_string();
                let data: JsonValue =
                    serde_json::from_str(it.next().unwrap_or_default().trim_start_matches("data: "))
                        .expect("事件体必须是合法 JSON");
                (name, data)
            })
            .collect()
    }

    #[test]
    fn anthropic_emitter_emits_complete_tool_use_sequence() {
        let mut em = AnthropicSseEmitter::new("req1", "m");

        // OpenAI 流式 tool_calls：首片带 id/name + 参数前缀，续片只带参数增量
        let mut events = Vec::new();
        events.extend(em.feed_tool_call(0, Some("call_1"), Some("get_weather"), "{\"city\":"));
        events.extend(em.feed_tool_call(0, None, None, "\"北京\"}"));
        events.extend(em.close_open_block());
        events.extend(em.finish("tool_use", 42));

        let parsed = parse_emitter_events(&events);
        let names: Vec<&str> = parsed.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ],
            "工具调用必须产生完整 tool_use 块序列（旧实现会整段丢弃）"
        );

        let (_, start_block) = &parsed[1];
        assert_eq!(start_block.pointer("/content_block/type").unwrap(), "tool_use");
        assert_eq!(start_block.pointer("/content_block/name").unwrap(), "get_weather");
        assert_eq!(start_block.pointer("/index").unwrap(), 0);

        let (_, first_delta) = &parsed[2];
        assert_eq!(
            first_delta.pointer("/delta/type").unwrap(),
            "input_json_delta"
        );
        assert_eq!(
            first_delta.pointer("/delta/partial_json").unwrap(),
            "{\"city\":"
        );

        // message_delta 必须携带真实 output_tokens（旧实现恒为 0）
        let (_, msg_delta) = &parsed[5];
        assert_eq!(msg_delta.pointer("/usage/output_tokens").unwrap(), 42);
        assert_eq!(msg_delta.pointer("/delta/stop_reason").unwrap(), "tool_use");
    }

    #[test]
    fn anthropic_emitter_alternates_text_thinking_and_defers_usage() {
        let mut em = AnthropicSseEmitter::new("req2", "m");

        let mut events = Vec::new();
        events.extend(em.feed_thinking("思考"));
        events.extend(em.feed_text("你好"));
        events.extend(em.feed_text("世界"));

        let parsed = parse_emitter_events(&events);
        let block_types: Vec<&str> = parsed
            .iter()
            .filter(|(n, _)| n == "content_block_start")
            .filter_map(|(_, d)| d.pointer("/content_block/type").and_then(JsonValue::as_str))
            .collect();
        assert_eq!(
            block_types,
            vec!["thinking", "text"],
            "思考块与文本块应各自独立开块并按序切换"
        );
        // 文本连续追加不应重复开块
        assert_eq!(block_types.iter().filter(|t| **t == "text").count(), 1);
    }
}
