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
use tracing::warn;

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
                            } else if data.starts_with('{') || data.starts_with('[') {
                                // 残缺 JSON（上游 write 截断缺陷，实测 x666 会发出
                                // `{"id":"msg_...","choices":[{"index":0,` 这类半截 chunk）：
                                // 原样透传会让客户端 JSON.parse 直接报错，必须拦截丢弃。
                                // 截断片段不含任何可用增量语义，丢弃无损。
                                warn!(
                                    "[ModelGateway] 丢弃上游残缺 SSE 分片（{} 字节）: {:?}",
                                    line.len(),
                                    cap_log_body(line.clone())
                                );
                            } else {
                                // 非 JSON 结构的负载（纯文本等）原样透传；补齐事件终止空行，避免客户端粘包
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

/// 工具名候选：(工具名, 参数键列表)。提取自客户端请求体的 tools 数组，
/// 用于在上游省略 content_block_start 帧时启发式恢复工具名。
pub type ToolHint = (String, Vec<String>);

/// Anthropic 快速通道专用：上游原生 Anthropic SSE 字节流**零转换直通**客户端，
/// 同时旁路扫描事件以提取 token 统计与响应全文（供日志与仪表盘使用）。
///
/// 兼容性能力：部分上游（如 new-api 系站点）在 tool_use 场景不发送
/// content_block_start 帧，input_json_delta 直接裸奔。标准客户端必须先收到
/// start 才能建立块并获得工具名。本函数维护块开启状态，检测到孤儿 delta 时
/// 动态注入合成的 content_block_start —— 工具名按请求 tools 的参数键匹配
/// 启发式恢复，保证客户端始终收到规范完整的事件序列。
#[allow(clippy::too_many_arguments)]
pub fn passthrough_anthropic_sse_with_stats<E: std::fmt::Display + Send + 'static>(
    stream: impl futures_util::Stream<Item = Result<Bytes, E>> + Send + 'static,
    ctx: ModelProxyContext,
    mut log: ProxyRequestLog,
    start_time: Instant,
    tool_hints: Vec<ToolHint>,
    preferred_tool: Option<String>,
) -> Body {
    let s = async_stream::stream! {
        let mut reader = SseLineReader::new();
        let mut stats = SseTokenStats::new();
        // 响应全文：逐行忠实记录上游 data 载荷原文，仅以换行分隔，不做任何格式化加工
        let mut raw_body = String::new();
        let mut has_thinking = false;
        // 兼容层状态
        let mut open_blocks: std::collections::HashSet<u64> = std::collections::HashSet::new();
        let mut synth_seq: u64 = 0;
        // 孤儿 tool_use 块缓冲：上游缺失 content_block_start 时暂存参数增量，
        // 直到工具名可被可靠恢复（参数键命中 / tool_choice 指定）或终态帧到达才冲刷下发，
        // 避免以占位名即时下发导致客户端「Tool not found」
        struct PendingTool {
            idx: u64,
            args: String,
            buffered: Vec<String>,
        }
        let mut pending: Option<PendingTool> = None;
        let mut ttft_recorded = false;

        tokio::pin!(stream);

        while let Some(chunk_res) = stream.next().await {
            match chunk_res {
                Ok(bytes) => {
                    // 首字节到达即 TTFT（直通模式首个上游分片就是首字时间）
                    if !ttft_recorded {
                        log.ttft_ms = Some(start_time.elapsed().as_millis() as u64);
                        ttft_recorded = true;
                    }
                    let lines = reader.push(&bytes);
                    // 本 chunk 的重组输出：普通行直通，孤儿块相关行按冲刷时机插入
                    let mut out_bytes: Vec<u8> = Vec::new();
                    let stats_lines = lines.clone();

                    for line in lines {
                        let data_payload = line.strip_prefix("data:").map(|d| d.trim().to_string());
                        let jv = data_payload
                            .as_deref()
                            .and_then(|d| serde_json::from_str::<JsonValue>(d).ok());

                        // ---- 工具名恢复与孤儿块冲刷辅助闭包（以宏内联形式展开）----
                        macro_rules! resolve_name {
                            ($args:expr) => {{
                                let mut best_name: Option<String> = None;
                                let mut best_score: usize = 0;
                                for (name, keys) in &tool_hints {
                                    let score =
                                        keys.iter().filter(|k| $args.contains(k.as_str())).count();
                                    if score > best_score {
                                        best_score = score;
                                        best_name = Some(name.clone());
                                    }
                                }
                                best_name.unwrap_or_else(|| {
                                    synth_seq += 1;
                                    format!("unknown_tool_{synth_seq}")
                                })
                            }};
                        }
                        macro_rules! flush_pending {
                            ($out:expr, $name:expr, $before:expr) => {{
                                if let Some(p) = pending.take() {
                                    let resolved = match &$name {
                                        Some(n) => n.clone(),
                                        None => resolve_name!(p.args),
                                    };
                                    let start_event = json!({
                                        "type": "content_block_start",
                                        "index": p.idx,
                                        "content_block": {
                                            "type": "tool_use",
                                            "id": format!("toolu_synth_{}", p.idx + 1),
                                            "name": resolved,
                                            "input": {},
                                        },
                                    });
                                    $out.extend_from_slice(
                                        format!("event: content_block_start\ndata: {start_event}\n\n")
                                            .as_bytes(),
                                    );
                                    for l in &p.buffered {
                                        $out.extend_from_slice(format!("{l}\n").as_bytes());
                                    }
                                    open_blocks.insert(p.idx);
                                    let note = if $name.is_some() || resolved.starts_with("unknown") {
                                        String::new()
                                    } else {
                                        format!("（工具名按参数键匹配推断为 {resolved}）")
                                    };
                                    warn!(
                                        "[ModelGateway] 兼容修复：上游缺失 content_block_start(index={})，已合成 tool_use 帧并恢复工具名为 \"{}\"{}",
                                        p.idx, resolved, note
                                    );
                                }
                            }};
                        }

                        match (&jv, jv.as_ref().and_then(|j| j.get("type")).and_then(JsonValue::as_str)) {
                            // ---------- 正常 start 帧 ----------
                            (Some(jv), Some("content_block_start")) => {
                                let idx = jv.get("index").and_then(JsonValue::as_u64).unwrap_or(0);
                                if let Some(pt) = &pending {
                                    if pt.idx == idx {
                                        // 缓冲中的孤儿块迎来了迟到但真实的 start：丢弃缓冲直接直通真实帧
                                        pending = None;
                                    }
                                }
                                open_blocks.insert(idx);
                                out_bytes.extend_from_slice(format!("{line}\n").as_bytes());
                            }
                            // ---------- stop 帧 ----------
                            (Some(jv), Some("content_block_stop")) => {
                                let idx = jv.get("index").and_then(JsonValue::as_u64).unwrap_or(0);
                                if let Some(pt) = &pending {
                                    if pt.idx == idx {
                                        // 终态到达仍无名：以全量 args 做最后一次匹配，失败才用占位
                                        let name = preferred_tool.clone().or_else(|| {
                                            tool_hints.iter()
                                                .map(|(n, keys)| {
                                                    (n.clone(), keys.iter().filter(|k| pt.args.contains(k.as_str())).count())
                                                })
                                                .max_by_key(|(_, sc)| *sc)
                                                .filter(|(_, sc)| *sc > 0)
                                                .map(|(n, _)| n)
                                        });
                                        flush_pending!(out_bytes, name, true);
                                    }
                                }
                                open_blocks.remove(&idx);
                                out_bytes.extend_from_slice(format!("{line}\n").as_bytes());
                            }
                            // ---------- delta ----------
                            (Some(jv), Some("content_block_delta")) => {
                                let idx = jv.get("index").and_then(JsonValue::as_u64).unwrap_or(0);
                                let dtype = jv.pointer("/delta/type").and_then(JsonValue::as_str).unwrap_or("text");
                                if dtype == "thinking_delta" {
                                    has_thinking = true;
                                }

                                // 孤儿 input_json_delta：进入缓冲/继续累积
                                if !open_blocks.contains(&idx)
                                    && dtype == "input_json_delta"
                                    && (pending.is_none()
                                        || pending.as_ref().map(|p| p.idx) == Some(idx))
                                {
                                    if let Some(frag) = jv.pointer("/delta/partial_json").and_then(JsonValue::as_str) {
                                        let pt = pending.get_or_insert(PendingTool {
                                            idx,
                                            args: String::new(),
                                            buffered: Vec::new(),
                                        });
                                        pt.args.push_str(frag);
                                    }
                                    pending
                                        .as_mut()
                                        .unwrap()
                                        .buffered
                                        .push(line.to_string());
                                    // 每片都尝试提前恢复名字：命中立即冲刷，恢复流式实时性
                                    let hit = preferred_tool.is_some()
                                        || tool_hints.iter().any(|(_, keys)| {
                                            keys.iter().any(|k| pending.as_ref().unwrap().args.contains(k.as_str()))
                                        });
                                    if hit && pending.is_some() {
                                        let name = preferred_tool.clone().or_else(|| {
                                            let args = &pending.as_ref().unwrap().args;
                                            tool_hints
                                                .iter()
                                                .map(|(n, keys)| {
                                                    (n.clone(), keys.iter().filter(|k| args.contains(k.as_str())).count())
                                                })
                                                .max_by_key(|(_, sc)| *sc)
                                                .filter(|(_, sc)| *sc > 0)
                                                .map(|(n, _)| n)
                                        });
                                        flush_pending!(out_bytes, name, false);
                                    }
                                    continue;
                                }

                                // 其他类型孤儿 delta（text/thinking）：无需工具名，即时合成开块
                                if !open_blocks.contains(&idx) {
                                    let block = if dtype == "thinking_delta" {
                                        json!({ "type": "thinking", "thinking": "" })
                                    } else {
                                        json!({ "type": "text", "text": "" })
                                    };
                                    let start_event = json!({
                                        "type": "content_block_start",
                                        "index": idx,
                                        "content_block": block,
                                    });
                                    out_bytes.extend_from_slice(
                                        format!("event: content_block_start\ndata: {start_event}\n\n").as_bytes(),
                                    );
                                    open_blocks.insert(idx);
                                }
                                out_bytes.extend_from_slice(format!("{line}\n").as_bytes());
                            }
                            // ---------- 终态帧：强制冲刷残余孤儿块 ----------
                            (_, Some("message_delta")) | (_, Some("message_stop")) => {
                                if let Some(pt) = &pending {
                                    let name = preferred_tool.clone().or_else(|| {
                                        tool_hints.iter()
                                            .map(|(n, keys)| {
                                                (n.clone(), keys.iter().filter(|k| pt.args.contains(k.as_str())).count())
                                            })
                                            .max_by_key(|(_, sc)| *sc)
                                            .filter(|(_, sc)| *sc > 0)
                                            .map(|(n, _)| n)
                                    });
                                    flush_pending!(out_bytes, name, true);
                                }
                                out_bytes.extend_from_slice(format!("{line}\n").as_bytes());
                            }
                            // ---------- 其余事件 ----------
                            _ => {
                                // 孤儿块缓冲期间的所有行一并延迟，保持事件原子性
                                if let Some(pt) = pending.as_mut() {
                                    pt.buffered.push(line.to_string());
                                } else {
                                    out_bytes.extend_from_slice(format!("{line}\n").as_bytes());
                                }
                            }
                        }
                    }

                    if !out_bytes.is_empty() {
                        yield Ok::<Bytes, std::io::Error>(Bytes::from(out_bytes));
                    }

                    // ③ 旁路统计扫描（与输出逻辑解耦）
                    for line in stats_lines {
                        let Some(data) = line.strip_prefix("data:") else { continue };
                        raw_body.push_str(data.trim_start());
                        raw_body.push('\n');
                        let Ok(jv) = serde_json::from_str::<JsonValue>(data.trim()) else { continue };
                        match jv.get("type").and_then(JsonValue::as_str) {
                            Some("message_start") => {
                                if let Some(u) = jv.pointer("/message/usage") {
                                    let i = u.get("input_tokens").and_then(JsonValue::as_u64).unwrap_or(0);
                                    let r = u.get("cache_read_input_tokens").and_then(JsonValue::as_u64).unwrap_or(0);
                                    let w = u.get("cache_creation_input_tokens").and_then(JsonValue::as_u64).unwrap_or(0);
                                    stats.cache_hit_tokens = r;
                                    stats.cache_creation_tokens = w;
                                    stats.prompt_tokens = i + r + w;
                                }
                            }
                            Some("content_block_delta") => {
                                if jv.pointer("/delta/type").and_then(JsonValue::as_str) == Some("thinking_delta") {
                                    has_thinking = true;
                                }
                            }
                            Some("message_delta") => {
                                if let Some(o) = jv.pointer("/usage/output_tokens").and_then(JsonValue::as_u64) {
                                    stats.completion_tokens = o;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Err(err) => {
                    log.error_message = Some(format!("Anthropic 直通流中断: {err}"));
                    break;
                }
            }
        }

        // 响应全文 = 上游 data 载荷原文（逐行换行），不做任何格式化加工
        log.response_body = cap_log_body(raw_body);

        if has_thinking {
            stats.has_reasoning = true;
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

        ctx.record_log(log).await;
    };

    Body::from_stream(s)
}

#[cfg(test)]
mod clean_stream_tests {
    use super::*;

    /// 喂入原始 SSE 字节流，收集 clean_sse_stream 的全部输出
    async fn run_clean_stream(raw: Vec<u8>) -> String {
        let state = crate::model::gateway::ModelProxyState::new_with_app(None);
        let ctx = state.context.clone();
        let log: ProxyRequestLog = serde_json::from_value(json!({
            "id": "t1", "timestamp": "", "method": "POST",
            "path": "/v1/chat/completions", "channelId": "opencode",
            "model": "m", "stream": true, "statusCode": 200, "durationMs": 0
        }))
        .unwrap();
        let upstream = futures_util::stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(raw))]);
        let mut out = clean_sse_stream(upstream, ctx, log, Instant::now(), "m".into())
            .into_data_stream();
        let mut collected = Vec::new();
        while let Some(chunk) = out.next().await {
            collected.extend_from_slice(&chunk.unwrap());
        }
        String::from_utf8_lossy(&collected).to_string()
    }

    // ProxyRequestLog 需要 Default —— 若无则手工构造
    #[tokio::test]
    async fn malformed_json_chunks_are_dropped_not_forwarded() {
        let raw = b"\
data: {\"id\":\"msg_abc\",\"choices\":[{\"index\":0,\n\
\n\
data: {\"id\":\"msg_abc\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"\\u6570\"}}]}\n\
\n\
data: [DONE]\n\
\n"
        .to_vec();
        let out = run_clean_stream(raw).await;
        assert!(
            !out.contains("\"index\":0,\n"),
            "残缺 JSON 分片必须被拦截，不得透传给客户端"
        );
        assert!(!out.contains("msg_abc\",\"choices\":[{\"index\":0,\n\n"));
        assert!(out.contains("数") || out.contains("\\u6570"), "完整 chunk 必须正常透传");
        assert!(out.contains("[DONE]"));
    }
}

#[cfg(test)]
mod passthrough_tests {
    use super::*;

    async fn run_passthrough(raw: Vec<u8>, hints: Vec<ToolHint>) -> String {
        let state = crate::model::gateway::ModelProxyState::new_with_app(None);
        let ctx = state.context.clone();
        let log: ProxyRequestLog = serde_json::from_value(json!({
            "id": "t2", "timestamp": "", "method": "POST",
            "path": "/v1/messages", "channelId": "x666",
            "model": "m", "stream": true, "statusCode": 200, "durationMs": 0
        }))
        .unwrap();
        let upstream = futures_util::stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(raw))]);
        let mut out = passthrough_anthropic_sse_with_stats(upstream, ctx, log, Instant::now(), hints, None)
            .into_data_stream();
        let mut collected = Vec::new();
        while let Some(chunk) = out.next().await {
            collected.extend_from_slice(&chunk.unwrap());
        }
        String::from_utf8_lossy(&collected).to_string()
    }

    /// 兼容性回归：上游缺失 content_block_start 时必须自动合成，
    /// 且工具名按请求 tools 参数键启发式恢复
    #[tokio::test]
    async fn orphan_input_json_delta_gets_synthesized_start_with_recovered_name() {
        let raw = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12,\"output_tokens\":0}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"command\\\": \\\"cd /tmp\\\", \"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"description\\\": \\\"go\\\"}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":30}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        )
        .as_bytes()
        .to_vec();

        let hints: Vec<ToolHint> = vec![
            ("bash".into(), vec!["command".into(), "description".into()]),
            ("read_file".into(), vec!["path".into()]),
        ];
        let out = run_passthrough(raw, hints).await;

        // 合成的 content_block_start 必须出现在首个孤儿 delta 之前
        let synth_pos = out.find("toolu_synth_1").expect("应注入合成 start 帧");
        let delta_pos = out.find("input_json_delta").expect("原始 delta 必须保留");
        assert!(synth_pos < delta_pos, "合成 start 必须先于孤儿 delta");

        // 工具名按参数键匹配恢复为 bash（command+description 双命中）
        let start_line = out.lines().find(|l| l.contains("toolu_synth_1")).unwrap();
        assert!(start_line.contains("\"name\":\"bash\""), "工具名应按参数键匹配恢复: {start_line}");

        // 原始事件保真：孤儿 delta 与终止序列原样到达客户端
        assert!(out.contains("stop_reason\":\"tool_use"));
        assert!(out.contains("message_stop"));
    }

    /// 正常流（含完整 start 帧）不得重复注入
    #[tokio::test]
    async fn normal_flow_with_start_frame_is_not_duplicated() {
        let raw = concat!(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
        )
        .as_bytes()
        .to_vec();
        let out = run_passthrough(raw, vec![("t".into(), vec![])]).await;
        assert_eq!(out.matches("content_block_start").count(), 1, "正常帧不应被二次合成");
        assert!(out.contains("\"hi\""));
    }
}
