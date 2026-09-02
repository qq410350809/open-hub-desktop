use super::ir::{StopReason, UniversalStreamEvent, UniversalUsage};
use super::types::{ModelProxyContext, ProxyRequestLog};
use axum::body::Body;
use bytes::Bytes;
use futures_util::stream::StreamExt;
use serde_json::{json, Value as JsonValue};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::Ordering;
use std::time::Instant;
use tracing::warn;

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
    /// OpenAI tool_calls index → 真实 (id, name)：块被文本/思考插队关闭后
    /// 重开时复用，避免伪造 id/name 导致客户端工具结果关联断裂
    tool_meta: BTreeMap<u64, (String, String)>,
    /// 工具参数流中插队的文本/思考增量（Anthropic 块模型不允许文本插队
    /// 工具参数，缓冲到工具块关闭后按到达顺序冲刷）
    pending_text: Vec<(AnthropicBlockKind, String)>,
    finished: bool,
    tool_hints: Vec<ToolHint>,
    preferred_tool: Option<String>,
}

impl AnthropicSseEmitter {
    fn new(
        msg_id: &str,
        model: &str,
        tool_hints: Vec<ToolHint>,
        preferred_tool: Option<String>,
    ) -> Self {
        Self {
            msg_id: format!("msg_{msg_id}"),
            model: model.to_string(),
            message_started: false,
            next_index: 0,
            open_block: None,
            tool_blocks: BTreeMap::new(),
            tool_meta: BTreeMap::new(),
            pending_text: Vec::new(),
            finished: false,
            tool_hints,
            preferred_tool,
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
        // 离开 Tool 块时先冲刷被缓冲的文本/思考：Anthropic 块模型不允许文本
        // 插队工具参数，先关工具块、再按到达顺序补发文本块，保证内容不丢
        if matches!(self.open_block, Some((_, AnthropicBlockKind::Tool)))
            && !self.pending_text.is_empty()
        {
            events.extend(self.close_open_block());
            events.extend(self.drain_pending_text());
        }
        if had_open {
            events.extend(self.close_open_block());
        } else {
            events.extend(self.start_message());
        }
        let (open_events, new_idx) = match kind {
            AnthropicBlockKind::Text => self.open_block(
                AnthropicBlockKind::Text,
                json!({ "type": "text", "text": "" }),
            ),
            AnthropicBlockKind::Thinking => self.open_block(
                AnthropicBlockKind::Thinking,
                json!({ "type": "thinking", "thinking": "" }),
            ),
            AnthropicBlockKind::Tool => unreachable!("tool block 必须携带 id/name 元数据"),
        };
        events.extend(open_events);
        (events, Some(new_idx))
    }

    /// 冲刷工具块打开期间缓冲的文本/思考增量（按到达顺序）。
    /// 调用前提：当前已无 Tool 块打开（否则会陷入递归）。
    /// 冲刷结束后关闭最后打开的块，保证 message_delta 前无未关闭块。
    fn drain_pending_text(&mut self) -> Vec<String> {
        let pending = std::mem::take(&mut self.pending_text);
        let mut events = Vec::new();
        for (kind, text) in pending {
            if text.is_empty() {
                continue;
            }
            let (mut open_events, idx) = self.switch_block(kind);
            events.append(&mut open_events);
            if let Some(idx) = idx {
                let delta = match kind {
                    AnthropicBlockKind::Text => json!({ "type": "text_delta", "text": text }),
                    AnthropicBlockKind::Thinking => {
                        json!({ "type": "thinking_delta", "thinking": text })
                    }
                    AnthropicBlockKind::Tool => unreachable!("pending 只缓冲文本/思考"),
                };
                events.push(sse_event(
                    "content_block_delta",
                    &json!({ "type": "content_block_delta", "index": idx, "delta": delta }),
                ));
            }
        }
        events.extend(self.close_open_block());
        events
    }

    /// 文本 delta
    fn feed_text(&mut self, text: &str) -> Vec<String> {
        if text.is_empty() {
            return Vec::new();
        }
        // 工具参数流中插队的文本：缓冲到工具块关闭后统一冲刷
        if matches!(self.open_block, Some((_, AnthropicBlockKind::Tool))) {
            self.pending_text
                .push((AnthropicBlockKind::Text, text.to_string()));
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
        // 工具参数流中插队的思考：缓冲到工具块关闭后统一冲刷
        if matches!(self.open_block, Some((_, AnthropicBlockKind::Tool))) {
            self.pending_text
                .push((AnthropicBlockKind::Thinking, text.to_string()));
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
                    // 先冲刷被缓冲的插队文本，再重开工具块（复用真实 id/name）
                    events.extend(self.drain_pending_text());
                    self.open_block = Some((idx, AnthropicBlockKind::Tool));
                    let (real_id, real_name) = self
                        .tool_meta
                        .get(&tc_index)
                        .cloned()
                        .unwrap_or_else(|| (format!("toolu_reopen_{idx}"), "tool".to_string()));
                    events.push(sse_event(
                        "content_block_start",
                        &json!({
                            "type": "content_block_start",
                            "index": idx,
                            "content_block": {
                                "type": "tool_use",
                                "id": real_id,
                                "name": real_name,
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
                    .or_else(|| self.preferred_tool.clone())
                    .unwrap_or_else(|| {
                        // 上游 tool_calls 缺 name 时按参数键匹配恢复
                        let frag = args_fragment;
                        self.tool_hints
                            .iter()
                            .map(|(n, keys)| {
                                (
                                    n.clone(),
                                    keys.iter().filter(|k| frag.contains(k.as_str())).count(),
                                )
                            })
                            .max_by_key(|(_, sc)| *sc)
                            .filter(|(_, sc)| *sc > 0)
                            .map(|(n, _)| n)
                            .unwrap_or_else(|| "tool".to_string())
                    });
                self.tool_meta
                    .insert(tc_index, (tool_id.clone(), tool_name.clone()));
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

    /// 工具调用声明帧：开新块（真实 id/name），不发参数增量
    fn feed_tool_start(&mut self, tc_index: u64, call_id: &str, name: &str) -> Vec<String> {
        if self.tool_blocks.contains_key(&tc_index) {
            return Vec::new();
        }
        let mut events = self.start_message();
        events.extend(self.close_open_block());
        events.extend(self.drain_pending_text());
        self.tool_meta
            .insert(tc_index, (call_id.to_string(), name.to_string()));
        let (open_events, idx) = self.open_block(
            AnthropicBlockKind::Tool,
            json!({
                "type": "tool_use",
                "id": call_id,
                "name": name,
                "input": {},
            }),
        );
        events.extend(open_events);
        self.tool_blocks.insert(tc_index, idx);
        events
    }

    /// 工具调用参数增量；若块被文本/思考插队需切回
    fn feed_tool_args(&mut self, tc_index: u64, args_fragment: &str) -> Vec<String> {
        let Some(&block_idx) = self.tool_blocks.get(&tc_index) else {
            // 兜底：缺失 Start 帧时合成占位元数据
            return self.feed_tool_call(tc_index, None, None, args_fragment);
        };
        let mut events = Vec::new();
        if self.open_block != Some((block_idx, AnthropicBlockKind::Tool)) {
            events.extend(self.close_open_block());
            events.extend(self.drain_pending_text());
            self.open_block = Some((block_idx, AnthropicBlockKind::Tool));
            let (real_id, real_name) = self
                .tool_meta
                .get(&tc_index)
                .cloned()
                .unwrap_or_else(|| (format!("toolu_reopen_{block_idx}"), "tool".to_string()));
            events.push(sse_event(
                "content_block_start",
                &json!({
                    "type": "content_block_start",
                    "index": block_idx,
                    "content_block": {
                        "type": "tool_use",
                        "id": real_id,
                        "name": real_name,
                        "input": {},
                    },
                }),
            ));
        }
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

    /// IR 事件驱动入口（统一代理链路）
    pub(super) fn on_ir_event(&mut self, event: &UniversalStreamEvent) -> Vec<String> {
        match event {
            UniversalStreamEvent::ReasoningDelta(s) => {
                if s.is_empty() {
                    Vec::new()
                } else {
                    self.feed_thinking(s)
                }
            }
            UniversalStreamEvent::TextDelta(s) => {
                if s.is_empty() {
                    Vec::new()
                } else {
                    self.feed_text(s)
                }
            }
            UniversalStreamEvent::ToolCallStart {
                index,
                call_id,
                name,
            } => self.feed_tool_start(*index, call_id, name),
            UniversalStreamEvent::ToolCallDelta { index, fragment } => {
                self.feed_tool_args(*index, fragment)
            }
            UniversalStreamEvent::Finish { reason, usage } => {
                self.finish_with_usage(*reason, usage)
            }
        }
    }

    /// 收尾：关闭打开的块 + message_delta（携带全量 usage，含缓存明细）+ message_stop。幂等。
    ///
    /// 此前 message_delta 仅回传 output_tokens、message_start 的 input_tokens 恒 0，
    /// 导致 Claude 系客户端（opencode/Claude Code）本地记账 input/cache 全为零。
    fn finish_with_usage(&mut self, reason: StopReason, usage: &UniversalUsage) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let mut events = self.start_message();
        events.extend(self.close_open_block());
        // 冲刷工具参数流中缓冲的插队文本，保证内容不丢
        events.extend(self.drain_pending_text());
        // IR 口径 input 为总量；Anthropic 语义中 input_tokens 不含缓存部分
        let anthropic_input = usage
            .input_tokens
            .saturating_sub(usage.cache_read_tokens)
            .saturating_sub(usage.cache_creation_tokens);
        events.push(sse_event(
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": { "stop_reason": reason.to_anthropic(), "stop_sequence": null },
                "usage": {
                    "output_tokens": usage.output_tokens,
                    "input_tokens": anthropic_input,
                    "cache_read_input_tokens": usage.cache_read_tokens,
                    "cache_creation_input_tokens": usage.cache_creation_tokens,
                },
            }),
        ));
        events.push(sse_event(
            "message_stop",
            &json!({ "type": "message_stop" }),
        ));
        events
    }
}

// ---------------------------------------------------------------- Responses SSE 发射器

/// 文本输出项的流式状态
struct ResponsesTextItem {
    item_id: String,
    output_index: u64,
    text: String,
}

/// 推理输出项的流式状态
struct ResponsesReasoningItem {
    item_id: String,
    output_index: u64,
    text: String,
}

/// 函数调用输出项的流式状态
struct ResponsesToolItem {
    item_id: String,
    output_index: u64,
    call_id: String,
    name: String,
    arguments: String,
}

/// OpenAI Chat SSE → Responses SSE 的事件机。
///
/// 完整产出 Responses 协议事件序列（此前仅透传 output_text.delta，
/// 上游返回纯 tool_calls / reasoning 响应时客户端只会收到空结果）：
///   response.created
///   ├─ reasoning: output_item.added → reasoning_summary_text.delta* → done + item.done
///   ├─ message:   output_item.added → content_part.added → output_text.delta*
///   │             → text.done + part.done + item.done
///   └─ function_call: output_item.added → function_call_arguments.delta*
///             → arguments.done + item.done
///   response.completed（携带完整 output 数组与 usage）
struct ResponsesSseEmitter {
    response_id: String,
    model: String,
    next_output_index: u64,
    reasoning_item: Option<ResponsesReasoningItem>,
    text_item: Option<ResponsesTextItem>,
    tool_items: BTreeMap<u64, ResponsesToolItem>,
    started: bool,
    finished: bool,
}

impl ResponsesSseEmitter {
    fn new(req_id: &str, model: &str) -> Self {
        Self {
            response_id: format!("resp_{req_id}"),
            model: model.to_string(),
            next_output_index: 0,
            reasoning_item: None,
            text_item: None,
            tool_items: BTreeMap::new(),
            started: false,
            finished: false,
        }
    }

    fn alloc_index(&mut self) -> u64 {
        let index = self.next_output_index;
        self.next_output_index += 1;
        index
    }

    /// 确保 response.created 已发出（幂等）
    fn ensure_started(&mut self) -> Vec<String> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        vec![sse_event(
            "response.created",
            &json!({
                "type": "response.created",
                "response": {
                    "id": self.response_id,
                    "object": "response",
                    "created_at": std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                    "status": "in_progress",
                    "model": self.model,
                    "output": [],
                }
            }),
        )]
    }

    fn feed_reasoning(&mut self, text: &str) -> Vec<String> {
        let (is_first, item_id, output_index) = {
            if self.reasoning_item.is_none() {
                let output_index = self.alloc_index();
                self.reasoning_item = Some(ResponsesReasoningItem {
                    item_id: format!("rs_{}", self.response_id),
                    output_index,
                    text: String::new(),
                });
            }
            let item = self.reasoning_item.as_mut().expect("reasoning item");
            let is_first = item.text.is_empty();
            item.text.push_str(text);
            (is_first, item.item_id.clone(), item.output_index)
        };
        let mut events = if is_first {
            vec![sse_event(
                "response.output_item.added",
                &json!({
                    "type": "response.output_item.added",
                    "output_index": output_index,
                    "item": {
                        "id": item_id,
                        "type": "reasoning",
                        "summary": [],
                    },
                }),
            )]
        } else {
            Vec::new()
        };
        events.push(sse_event(
            "response.reasoning_summary_text.delta",
            &json!({
                "type": "response.reasoning_summary_text.delta",
                "item_id": item_id,
                "output_index": output_index,
                "summary_index": 0,
                "delta": text,
            }),
        ));
        events
    }

    fn feed_text(&mut self, text: &str) -> Vec<String> {
        let (is_first, item_id, output_index) = {
            if self.text_item.is_none() {
                let output_index = self.alloc_index();
                self.text_item = Some(ResponsesTextItem {
                    item_id: format!("msg_{}", self.response_id),
                    output_index,
                    text: String::new(),
                });
            }
            let item = self.text_item.as_mut().expect("text item");
            let is_first = item.text.is_empty();
            item.text.push_str(text);
            (is_first, item.item_id.clone(), item.output_index)
        };
        let mut events = if is_first {
            vec![
                sse_event(
                    "response.output_item.added",
                    &json!({
                        "type": "response.output_item.added",
                        "output_index": output_index,
                        "item": {
                            "id": item_id,
                            "type": "message",
                            "status": "in_progress",
                            "role": "assistant",
                            "content": [],
                        },
                    }),
                ),
                sse_event(
                    "response.content_part.added",
                    &json!({
                        "type": "response.content_part.added",
                        "item_id": item_id,
                        "output_index": output_index,
                        "content_index": 0,
                        "part": { "type": "output_text", "text": "", "annotations": [] },
                    }),
                ),
            ]
        } else {
            Vec::new()
        };
        events.push(sse_event(
            "response.output_text.delta",
            &json!({
                "type": "response.output_text.delta",
                "item_id": item_id,
                "output_index": output_index,
                "content_index": 0,
                "delta": text,
            }),
        ));
        events
    }

    fn feed_tool_start(&mut self, tc_index: u64, call_id: &str, name: &str) -> Vec<String> {
        if self.tool_items.contains_key(&tc_index) {
            return Vec::new();
        }
        let output_index = self.alloc_index();
        let item_id = format!("fc_{}_{}", self.response_id, tc_index);
        self.tool_items.insert(
            tc_index,
            ResponsesToolItem {
                item_id: item_id.clone(),
                output_index,
                call_id: call_id.to_string(),
                name: name.to_string(),
                arguments: String::new(),
            },
        );
        vec![sse_event(
            "response.output_item.added",
            &json!({
                "type": "response.output_item.added",
                "output_index": output_index,
                "item": {
                    "id": item_id,
                    "type": "function_call",
                    "status": "in_progress",
                    "call_id": call_id,
                    "name": name,
                    "arguments": "",
                },
            }),
        )]
    }

    fn feed_tool_args(&mut self, tc_index: u64, args_fragment: &str) -> Vec<String> {
        let Some(item) = self.tool_items.get_mut(&tc_index) else {
            // 兜底：缺失 Start 帧时合成占位元数据
            let start = self.feed_tool_start(tc_index, &format!("call_{tc_index}"), "tool");
            let mut events = start;
            events.extend(self.feed_tool_args(tc_index, args_fragment));
            return events;
        };
        item.arguments.push_str(args_fragment);
        if args_fragment.is_empty() {
            return Vec::new();
        }
        vec![sse_event(
            "response.function_call_arguments.delta",
            &json!({
                "type": "response.function_call_arguments.delta",
                "item_id": item.item_id,
                "output_index": item.output_index,
                "delta": args_fragment,
            }),
        )]
    }

    /// IR 事件驱动入口（统一代理链路）
    pub(super) fn on_ir_event(&mut self, event: &UniversalStreamEvent) -> Vec<String> {
        // response.created 必须是流首事件：首帧即发出（幂等），
        // 否则元数据晚于 output_item.added，破坏协议顺序
        let mut out = self.ensure_started();
        out.extend(match event {
            UniversalStreamEvent::ReasoningDelta(s) => {
                if s.is_empty() {
                    Vec::new()
                } else {
                    self.feed_reasoning(s)
                }
            }
            UniversalStreamEvent::TextDelta(s) => {
                if s.is_empty() {
                    Vec::new()
                } else {
                    self.feed_text(s)
                }
            }
            UniversalStreamEvent::ToolCallStart {
                index,
                call_id,
                name,
            } => self.feed_tool_start(*index, call_id, name),
            UniversalStreamEvent::ToolCallDelta { index, fragment } => {
                self.feed_tool_args(*index, fragment)
            }
            UniversalStreamEvent::Finish { reason, usage } => {
                self.finish_with_usage(*reason, usage)
            }
        });
        out
    }

    /// 收尾：关闭所有打开项 + response.completed（携带完整 output 与 usage）。幂等。
    fn finish_with_usage(&mut self, reason: StopReason, usage: &UniversalUsage) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let mut events = self.ensure_started();

        // 最终 output 数组按分配顺序收集；BTreeMap 遍历天然有序且与
        // index 分配顺序一致，无需再排序。
        let mut finalized: Vec<(u64, JsonValue)> = Vec::new();

        if let Some(item) = self.reasoning_item.take() {
            events.push(sse_event(
                "response.reasoning_summary_text.done",
                &json!({
                    "type": "response.reasoning_summary_text.done",
                    "item_id": item.item_id,
                    "output_index": item.output_index,
                    "summary_index": 0,
                    "text": item.text,
                }),
            ));
            let out_item = json!({
                "id": item.item_id,
                "type": "reasoning",
                "summary": [{ "type": "summary_text", "text": item.text }],
            });
            // Responses 协议要求每个 output item 以 output_item.done 收尾；
            // 缺终事件时严格客户端（Codex 系）会报「Tool call ended without a
            // terminal event」并中断任务。三类 item 均须补发。
            // 注意字段名必须是 `item`（zcode/Codex 客户端按 ae.item.type 分支），
            // 官方文档示例的 `output` 会让客户端在校验后取 ae.item 抛错。
            events.push(sse_event(
                "response.output_item.done",
                &json!({
                    "type": "response.output_item.done",
                    "output_index": item.output_index,
                    "item": out_item.clone(),
                }),
            ));
            finalized.push((item.output_index, out_item));
        }

        if let Some(item) = self.text_item.take() {
            events.push(sse_event(
                "response.output_text.done",
                &json!({
                    "type": "response.output_text.done",
                    "item_id": item.item_id,
                    "output_index": item.output_index,
                    "content_index": 0,
                    "text": item.text,
                }),
            ));
            events.push(sse_event(
                "response.content_part.done",
                &json!({
                    "type": "response.content_part.done",
                    "item_id": item.item_id,
                    "output_index": item.output_index,
                    "content_index": 0,
                    "part": { "type": "output_text", "text": item.text, "annotations": [] },
                }),
            ));
            let out_item = json!({
                "id": item.item_id,
                "type": "message",
                "status": "completed",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": item.text,
                    "annotations": [],
                }],
            });
            events.push(sse_event(
                "response.output_item.done",
                &json!({
                    "type": "response.output_item.done",
                    "output_index": item.output_index,
                    "item": out_item.clone(),
                }),
            ));
            finalized.push((item.output_index, out_item));
        }

        for (_, item) in self.tool_items.iter() {
            let call_id = if item.call_id.is_empty() {
                format!("call_{}", item.output_index)
            } else {
                item.call_id.clone()
            };
            events.push(sse_event(
                "response.function_call_arguments.done",
                &json!({
                    "type": "response.function_call_arguments.done",
                    "item_id": item.item_id,
                    "output_index": item.output_index,
                    "arguments": item.arguments,
                }),
            ));
            let out_item = json!({
                "id": item.item_id,
                "type": "function_call",
                "status": "completed",
                "call_id": call_id,
                "name": item.name,
                "arguments": item.arguments,
            });
            events.push(sse_event(
                "response.output_item.done",
                &json!({
                    "type": "response.output_item.done",
                    "output_index": item.output_index,
                    "item": out_item.clone(),
                }),
            ));
            finalized.push((item.output_index, out_item));
        }

        finalized.sort_by_key(|(index, _)| *index);
        let usage = json!({
            "input_tokens": usage.input_tokens,
            "input_tokens_details": { "cached_tokens": usage.cache_read_tokens },
            "output_tokens": usage.output_tokens,
            "output_tokens_details": { "reasoning_tokens": usage.reasoning_tokens },
            "total_tokens": usage.total(),
        });
        let status = match reason {
            StopReason::MaxTokens => "incomplete",
            _ => "completed",
        };
        events.push(sse_event(
            "response.completed",
            &json!({
                "type": "response.completed",
                "response": {
                    "id": self.response_id,
                    "object": "response",
                    "created_at": std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                    "status": status,
                    "model": self.model,
                    "output": finalized.into_iter().map(|(_, item)| item).collect::<Vec<_>>(),
                    "usage": usage,
                }
            }),
        ));
        events
    }
}

#[cfg(test)]
mod stream_accumulator_tests {
    use super::*;

    #[test]
    fn responses_emitter_emits_full_tool_call_sequence() {
        let mut emitter = ResponsesSseEmitter::new("req1", "big-pickle");
        let mut events: Vec<String> = Vec::new();
        // 模拟 zcode 实际场景：reasoning + 纯 tool_calls（content 为空）
        events.extend(emitter.on_ir_event(&UniversalStreamEvent::ReasoningDelta(
            "Let me check status.".into(),
        )));
        events.extend(emitter.on_ir_event(&UniversalStreamEvent::ToolCallStart {
            index: 0,
            call_id: "call_a".into(),
            name: "Bash".into(),
        }));
        events.extend(emitter.on_ir_event(&UniversalStreamEvent::ToolCallDelta {
            index: 0,
            fragment: "{\"command\":".into(),
        }));
        events.extend(emitter.on_ir_event(&UniversalStreamEvent::ToolCallDelta {
            index: 0,
            fragment: "\"git status\"}".into(),
        }));
        events.extend(emitter.on_ir_event(&UniversalStreamEvent::ToolCallStart {
            index: 1,
            call_id: "call_b".into(),
            name: "Bash".into(),
        }));
        events.extend(emitter.on_ir_event(&UniversalStreamEvent::ToolCallDelta {
            index: 1,
            fragment: "{}".into(),
        }));

        events.extend(emitter.finish_with_usage(
            StopReason::ToolUse,
            &UniversalUsage {
                input_tokens: 100,
                output_tokens: 50,
                ..Default::default()
            },
        ));

        let all = events.join("\n");
        assert!(all.contains("event: response.created"));
        assert!(all.contains("\"type\":\"reasoning\""));
        assert!(all.contains("event: response.reasoning_summary_text.delta"));
        // 两个 function_call 都必须出现（旧实现直接丢失）
        assert!(all.matches("\"type\":\"function_call\"").count() >= 4);
        assert!(all.contains("event: response.function_call_arguments.delta"));
        assert!(all.contains("\"call_id\":\"call_a\""));
        assert!(all.contains("\"call_id\":\"call_b\""));
        assert!(all.contains("event: response.function_call_arguments.done"));
        assert!(all.contains("git status"));
        // completed 必须携带完整 output 与 usage
        assert!(all.contains("event: response.completed"));
        let completed_payload = &all[all.find("event: response.completed").unwrap()..];
        assert!(completed_payload.contains("\"input_tokens\":100"));
        assert!(completed_payload.contains("\"output_tokens\":50"));
        assert!(completed_payload.contains("\"total_tokens\":150"));
        // output 数组顺序：reasoning(0) → fc(1) → fc(2)
        assert!(completed_payload.contains("rs_resp_req1"));
        // 每个 output item 都必须以 output_item.done 收尾，且早于 response.completed
        //（缺终事件时 zcode/Codex 客户端报 Tool call ended without a terminal event）
        assert_eq!(
            all.matches("event: response.output_item.done").count(),
            3,
            "reasoning + 2 个 function_call 各应有一条 output_item.done"
        );
        let last_done = all.rfind("event: response.output_item.done").unwrap();
        let completed_pos = all.find("event: response.completed").unwrap();
        assert!(
            last_done < completed_pos,
            "output_item.done 必须早于 response.completed"
        );
    }

    #[test]
    fn responses_emitter_tool_only_stream_terminates_each_function_call() {
        // zcode 实测场景：上游返回纯 reasoning + 工具调用（无正文），
        // 若 function_call 项缺 output_item.done，客户端会中断整个任务。
        let mut emitter = ResponsesSseEmitter::new("req9", "big-pickle");
        let mut events: Vec<String> = Vec::new();
        events.extend(emitter.on_ir_event(&UniversalStreamEvent::ReasoningDelta("分析中".into())));
        events.extend(emitter.on_ir_event(&UniversalStreamEvent::ToolCallStart {
            index: 0,
            call_id: "call_0".into(),
            name: "shell".into(),
        }));
        events.extend(emitter.on_ir_event(&UniversalStreamEvent::ToolCallDelta {
            index: 0,
            fragment: "{\"command\":".into(),
        }));
        events.extend(emitter.on_ir_event(&UniversalStreamEvent::ToolCallDelta {
            index: 0,
            fragment: "\"ls\"}".into(),
        }));
        events.extend(emitter.finish_with_usage(
            StopReason::ToolUse,
            &UniversalUsage {
                input_tokens: 10,
                output_tokens: 5,
                ..Default::default()
            },
        ));

        let all = events.join("\n");
        // reasoning 与 function_call 各 1 条 output_item.done
        assert_eq!(all.matches("event: response.output_item.done").count(), 2);
        // 客户端按 `item` 字段分支（而非官方示例的 `output`）；
        // serde_json 默认按键名字典序输出，断言需与键序无关
        assert!(all.contains("\"item\":{\"arguments\":\"{\\\"command\\\":\\\"ls\\\"}\""));
        assert!(all.contains("\"id\":\"fc_resp_req9_0\""));
        assert!(all.contains("\"status\":\"completed\",\"type\":\"function_call\""));
        // function_call 的 done 携带完成的 call_id/name/arguments
        assert!(all.contains("\"call_id\":\"call_0\""));
        assert!(all.contains("\"arguments\":\"{\\\"command\\\":\\\"ls\\\"}\""));
        // 事件顺序：arguments.done → output_item.done → response.completed
        let args_done = all.find("response.function_call_arguments.done").unwrap();
        let item_done = all.rfind("response.output_item.done").unwrap();
        let completed = all.find("response.completed").unwrap();
        assert!(args_done < item_done && item_done < completed);
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
                let data: JsonValue = serde_json::from_str(
                    it.next().unwrap_or_default().trim_start_matches("data: "),
                )
                .expect("事件体必须是合法 JSON");
                (name, data)
            })
            .collect()
    }

    #[test]
    fn anthropic_emitter_emits_complete_tool_use_sequence() {
        let mut em = AnthropicSseEmitter::new("req1", "m", Vec::new(), None);

        // IR 事件：Start 携带元数据，Delta 只带参数增量
        let mut events = Vec::new();
        events.extend(em.on_ir_event(&UniversalStreamEvent::ToolCallStart {
            index: 0,
            call_id: "call_1".into(),
            name: "get_weather".into(),
        }));
        events.extend(em.on_ir_event(&UniversalStreamEvent::ToolCallDelta {
            index: 0,
            fragment: "{\"city\":".into(),
        }));
        events.extend(em.on_ir_event(&UniversalStreamEvent::ToolCallDelta {
            index: 0,
            fragment: "\"北京\"}".into(),
        }));
        events.extend(em.close_open_block());
        events.extend(em.finish_with_usage(
            StopReason::ToolUse,
            &UniversalUsage {
                input_tokens: 100,
                output_tokens: 42,
                ..Default::default()
            },
        ));

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
            "工具调用必须产生完整 tool_use 块序列"
        );

        let (_, start_block) = &parsed[1];
        assert_eq!(
            start_block.pointer("/content_block/type").unwrap(),
            "tool_use"
        );
        assert_eq!(
            start_block.pointer("/content_block/name").unwrap(),
            "get_weather"
        );
        assert_eq!(start_block.pointer("/content_block/id").unwrap(), "call_1");
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

        // message_delta 必须携带真实 output_tokens 与全量 usage（旧实现 input 恒 0）
        let (_, msg_delta) = &parsed[5];
        assert_eq!(msg_delta.pointer("/usage/output_tokens").unwrap(), 42);
        assert_eq!(
            msg_delta.pointer("/usage/input_tokens").unwrap(),
            100,
            "Chat 口径 input 为总量，直接透传"
        );
        assert_eq!(msg_delta.pointer("/delta/stop_reason").unwrap(), "tool_use");
    }

    #[test]
    fn anthropic_emitter_alternates_text_thinking_and_defers_usage() {
        let mut em = AnthropicSseEmitter::new("req2", "m", Vec::new(), None);

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

    #[test]
    fn gemini_emitter_aggregates_fragmented_tool_args() {
        // P0-1：跨协议流式工具参数是碎分片，旧实现逐片解析失败 → args:{} 且 name:""
        let mut em = GeminiEmitter::default();
        let mut events = Vec::new();
        events.extend(em.on_event(&UniversalStreamEvent::ToolCallStart {
            index: 0,
            call_id: "call_1".into(),
            name: "search".into(),
        }));
        events.extend(em.on_event(&UniversalStreamEvent::ToolCallDelta {
            index: 0,
            fragment: "{\"q\":".into(),
        }));
        events.extend(em.on_event(&UniversalStreamEvent::ToolCallDelta {
            index: 0,
            fragment: "\"git\"}".into(),
        }));
        events.extend(em.on_event(&UniversalStreamEvent::Finish {
            reason: StopReason::ToolUse,
            usage: UniversalUsage {
                input_tokens: 5,
                output_tokens: 3,
                ..Default::default()
            },
        }));

        let all = events.join("\n");
        assert!(
            all.contains("\"name\":\"search\""),
            "必须携带真实函数名: {all}"
        );
        assert!(
            all.contains("\"q\":\"git\""),
            "碎分片必须聚合为完整 args: {all}"
        );
        assert!(
            !all.contains("\"name\":\"\""),
            "不得出现空名 functionCall: {all}"
        );
        assert!(
            all.contains("finishReason"),
            "终帧必须携带 finishReason: {all}"
        );

        // 终帧 chunk 内必须是完整 functionCall（含解析后的完整 args）
        let last_raw = events.last().expect("最后应为 Finish chunk");
        let last_json: JsonValue = serde_json::from_str(
            last_raw
                .trim()
                .strip_prefix("data: ")
                .expect("Gemini 事件以 data: 开头"),
        )
        .expect("终帧应为合法 JSON");
        let parts = last_json
            .pointer("/candidates/0/content/parts")
            .and_then(JsonValue::as_array)
            .expect("应有 parts");
        assert!(
            parts.iter().any(|p| {
                p.pointer("/functionCall/name").and_then(JsonValue::as_str) == Some("search")
                    && p.pointer("/functionCall/args/q")
                        .and_then(JsonValue::as_str)
                        == Some("git")
            }),
            "Finish chunk 内应有完整 functionCall: {all}"
        );
    }

    #[test]
    fn anthropic_emitter_reopens_tool_with_real_meta_and_buffers_text() {
        // P1-2：文本插队工具参数时不得伪造 toolu_reopen_*/name:"tool"，
        // 工具块复用真实 id/name，插队文本缓冲到工具块关闭后完整出现
        let mut em = AnthropicSseEmitter::new("req3", "m", Vec::new(), None);
        let mut events = Vec::new();
        events.extend(em.on_ir_event(&UniversalStreamEvent::ToolCallStart {
            index: 0,
            call_id: "call_x".into(),
            name: "search".into(),
        }));
        events.extend(em.on_ir_event(&UniversalStreamEvent::ToolCallDelta {
            index: 0,
            fragment: "{\"q\":".into(),
        }));
        events.extend(em.on_ir_event(&UniversalStreamEvent::TextDelta("等等".into())));
        events.extend(em.on_ir_event(&UniversalStreamEvent::ToolCallDelta {
            index: 0,
            fragment: "\"git\"}".into(),
        }));
        events.extend(em.on_ir_event(&UniversalStreamEvent::Finish {
            reason: StopReason::ToolUse,
            usage: UniversalUsage {
                input_tokens: 5,
                output_tokens: 3,
                ..Default::default()
            },
        }));

        let all = events.join("\n");
        let parsed = parse_emitter_events(&events);
        assert!(
            all.contains("\"id\":\"call_x\""),
            "工具块必须复用真实 id: {all}"
        );
        assert!(!all.contains("toolu_reopen"), "不得出现伪造 id: {all}");
        assert!(!all.contains("\"name\":\"tool\""), "不得出现占位名: {all}");
        assert!(all.contains("等等"), "插队文本不得丢失: {all}");

        // 两段参数分片都必须下发（input_json_delta 逐片透传）
        let partial_jsons: Vec<&str> = parsed
            .iter()
            .filter(|(n, d)| {
                n == "content_block_delta"
                    && d.pointer("/delta/type").and_then(JsonValue::as_str)
                        == Some("input_json_delta")
            })
            .filter_map(|(_, d)| d.pointer("/delta/partial_json").and_then(JsonValue::as_str))
            .collect();
        assert_eq!(
            partial_jsons,
            vec!["{\"q\":", "\"git\"}"],
            "参数分片应完整下发"
        );

        // 文本必须出现在工具块之后（独立文本块），message_delta 前无未关闭块
        let types: Vec<&str> = parsed
            .iter()
            .filter(|(n, _)| n == "content_block_start")
            .filter_map(|(_, d)| d.pointer("/content_block/type").and_then(JsonValue::as_str))
            .collect();
        assert_eq!(
            types,
            vec!["tool_use", "text"],
            "工具块与文本块按序独立: {all}"
        );
        let delta_times = parsed
            .iter()
            .filter(|(n, d)| {
                n == "content_block_delta"
                    && d.pointer("/delta/type").and_then(JsonValue::as_str) == Some("text_delta")
            })
            .count();
        assert_eq!(delta_times, 1, "插队文本应完整出现在单个 text_delta 中");
    }
}

// ---------------------------------------------------------------- 统一 IR 出口

use super::egress::delta_chunk;
use super::ir::usage_to_chat_json;
use super::parsers::UniversalParser;
use crate::model::gateway::egress::{self, TargetProtocol};
use crate::model::gateway::pipeline::ClientProtocol;

/// 出口协议（客户端侧）。与 pipeline::ClientProtocol 相比细分了
/// OpenAI Chat 与 Responses 两种出口形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseClientProtocol {
    Chat,
    Anthropic,
    Gemini,
    Responses,
}

impl From<ClientProtocol> for SseClientProtocol {
    fn from(value: ClientProtocol) -> Self {
        match value {
            ClientProtocol::OpenAi => Self::Chat,
            ClientProtocol::Responses => Self::Responses,
            ClientProtocol::Anthropic => Self::Anthropic,
            ClientProtocol::Gemini => Self::Gemini,
        }
    }
}

/// OpenAI Chat 出口：把 IR 事件重建为 Chat chunk SSE
#[derive(Default)]
struct ChatEmitter {
    started_tools: BTreeSet<u64>,
    finished: bool,
}

impl ChatEmitter {
    fn on_event(&mut self, event: &UniversalStreamEvent) -> Vec<String> {
        match event {
            UniversalStreamEvent::ReasoningDelta(s) => {
                vec![delta_chunk(json!({ "reasoning_content": s }), None, None)]
            }
            UniversalStreamEvent::TextDelta(s) => {
                vec![delta_chunk(json!({ "content": s }), None, None)]
            }
            UniversalStreamEvent::ToolCallStart {
                index,
                call_id,
                name,
            } => {
                self.started_tools.insert(*index);
                vec![delta_chunk(
                    json!({ "tool_calls": [{
                        "index": index,
                        "id": call_id,
                        "type": "function",
                        "function": { "name": name, "arguments": "" },
                    }]}),
                    None,
                    None,
                )]
            }
            UniversalStreamEvent::ToolCallDelta { index, fragment } => {
                let mut tc = json!({ "index": index });
                if !self.started_tools.contains(index) {
                    // 兜底：上游缺失元数据帧时合成占位 Start（客户端会自愈）
                    self.started_tools.insert(*index);
                    tc["id"] = json!(format!("call_{index}"));
                    tc["type"] = json!("function");
                    tc["function"]["name"] = json!("tool");
                }
                tc["function"]["arguments"] = json!(fragment);
                vec![delta_chunk(json!({ "tool_calls": [tc] }), None, None)]
            }
            UniversalStreamEvent::Finish { reason, usage } => {
                if self.finished {
                    return Vec::new();
                }
                self.finished = true;
                vec![
                    delta_chunk(
                        json!({}),
                        Some(reason.to_chat()),
                        Some(usage_to_chat_json(usage)),
                    ),
                    "data: [DONE]\n\n".to_string(),
                ]
            }
        }
    }

    /// 流中断等异常场景的强制收尾：补终帧与 [DONE]，避免客户端悬挂
    fn abort(&mut self, usage: &UniversalUsage) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        vec![
            delta_chunk(json!({}), Some("stop"), Some(usage_to_chat_json(usage))),
            "data: [DONE]\n\n".to_string(),
        ]
    }
}

/// Google Gemini 出口：把 IR 事件重建为 Gemini streamResponse SSE
#[derive(Default)]
pub(super) struct GeminiEmitter {
    /// 工具序号 → (函数名, 参数片段缓冲)。Gemini 的 functionCall part 必须是
    /// 含完整 args 的独立对象，跨协议流式的参数是碎分片，逐片解析必然失败；
    /// 统一缓冲到 Finish/abort 时聚合为完整 functionCall。
    pending_tools: BTreeMap<u64, (String, String)>,
    finished: bool,
}

impl GeminiEmitter {
    pub(super) fn on_event(&mut self, event: &UniversalStreamEvent) -> Vec<String> {
        let mut parts = Vec::<JsonValue>::new();
        let mut finish = None::<String>;
        let mut usage_json = None::<JsonValue>;
        match event {
            UniversalStreamEvent::ReasoningDelta(s) => {
                parts.push(json!({ "text": s, "thought": true }));
            }
            UniversalStreamEvent::TextDelta(s) => {
                parts.push(json!({ "text": s }));
            }
            UniversalStreamEvent::ToolCallStart { index, name, .. } => {
                self.pending_tools
                    .entry(*index)
                    .or_insert_with(|| (name.clone(), String::new()));
            }
            UniversalStreamEvent::ToolCallDelta { index, fragment } => {
                let entry = self
                    .pending_tools
                    .entry(*index)
                    .or_insert_with(|| (String::new(), String::new()));
                entry.1.push_str(fragment);
            }
            UniversalStreamEvent::Finish { reason, usage } => {
                if self.finished {
                    return Vec::new();
                }
                self.finished = true;
                for (_index, (name, args)) in std::mem::take(&mut self.pending_tools) {
                    let args_val = serde_json::from_str::<JsonValue>(&args)
                        .unwrap_or_else(|_| json!({ "result": args }));
                    parts.push(json!({ "functionCall": { "name": name, "args": args_val } }));
                }
                finish = Some(reason.to_gemini().to_string());
                usage_json = Some(json!({
                    "promptTokenCount": usage.input_tokens,
                    "candidatesTokenCount": usage.output_tokens.saturating_sub(usage.reasoning_tokens),
                    "thoughtsTokenCount": usage.reasoning_tokens,
                    "cachedContentTokenCount": usage.cache_read_tokens,
                    "totalTokenCount": usage.total(),
                }));
            }
        }
        if parts.is_empty() && finish.is_none() {
            return Vec::new();
        }
        let mut chunk = json!({
            "candidates": [{
                "content": { "parts": parts, "role": "model" },
                "index": 0,
            }],
        });
        if let Some(fr) = finish {
            chunk["candidates"][0]["finishReason"] = json!(fr);
        }
        if let Some(u) = usage_json {
            chunk["usageMetadata"] = u;
        }
        vec![format!("data: {}\n\n", chunk)]
    }

    /// 流中断：冲刷未完成的工具参数并给 STOP 终帧，避免客户端悬挂
    fn abort(&mut self) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let mut parts = Vec::new();
        for (_index, (name, args)) in std::mem::take(&mut self.pending_tools) {
            let args_val = serde_json::from_str::<JsonValue>(&args)
                .unwrap_or_else(|_| json!({ "result": args }));
            parts.push(json!({ "functionCall": { "name": name, "args": args_val } }));
        }
        vec![format!(
            "data: {}\n\n",
            json!({
                "candidates": [{
                    "content": { "parts": parts, "role": "model" },
                    "index": 0,
                    "finishReason": "STOP",
                }],
            })
        )]
    }
}

/// 客户端协议出口分发器
enum ClientSseEmitter {
    Chat(ChatEmitter),
    Anthropic(AnthropicSseEmitter),
    Gemini(GeminiEmitter),
    Responses(ResponsesSseEmitter),
}

impl ClientSseEmitter {
    fn new(
        client: SseClientProtocol,
        req_id: &str,
        model: &str,
        tool_hints: Vec<ToolHint>,
        preferred_tool: Option<String>,
    ) -> Self {
        match client {
            SseClientProtocol::Chat => Self::Chat(ChatEmitter::default()),
            SseClientProtocol::Gemini => Self::Gemini(GeminiEmitter::default()),
            SseClientProtocol::Anthropic => Self::Anthropic(AnthropicSseEmitter::new(
                req_id,
                model,
                tool_hints,
                preferred_tool,
            )),
            SseClientProtocol::Responses => {
                Self::Responses(ResponsesSseEmitter::new(req_id, model))
            }
        }
    }

    fn on_event(&mut self, event: &UniversalStreamEvent) -> Vec<String> {
        match (self, event) {
            (Self::Chat(e), _) => e.on_event(event),
            (Self::Gemini(e), _) => e.on_event(event),
            (Self::Anthropic(e), ev) => e.on_ir_event(ev),
            (Self::Responses(e), ev) => e.on_ir_event(ev),
        }
    }

    fn abort(&mut self, usage: &UniversalUsage) -> Vec<String> {
        match self {
            Self::Chat(e) => e.abort(usage),
            Self::Gemini(e) => e.abort(),
            // 中断场景仍须发出协议终帧，避免客户端无限等待；
            // 已累积的用量一并带上，客户端本地记账不归零
            Self::Anthropic(e) => e.finish_with_usage(StopReason::EndTurn, usage),
            Self::Responses(e) => e.finish_with_usage(StopReason::EndTurn, usage),
        }
    }
}

/// 统一用量口径落库 + 网关指标累加（input 为总量，缓存明细单列）
fn apply_usage_to_log(log: &mut ProxyRequestLog, usage: &UniversalUsage, ctx: &ModelProxyContext) {
    if usage.input_tokens == 0 && usage.output_tokens == 0 {
        return;
    }
    log.prompt_tokens = Some(usage.input_tokens).filter(|v| *v > 0);
    log.completion_tokens = Some(usage.output_tokens).filter(|v| *v > 0);
    log.prompt_cache_hit_tokens = Some(usage.cache_read_tokens).filter(|v| *v > 0);
    log.cache_creation_tokens = Some(usage.cache_creation_tokens).filter(|v| *v > 0);
    log.reasoning_tokens = Some(usage.reasoning_tokens).filter(|v| *v > 0);
    log.total_tokens = Some(usage.total()).filter(|v| *v > 0);
    ctx.metrics
        .total_prompt_tokens
        .fetch_add(usage.input_tokens, Ordering::Relaxed);
    ctx.metrics
        .total_completion_tokens
        .fetch_add(usage.output_tokens, Ordering::Relaxed);
    if usage.reasoning_tokens > 0 {
        ctx.metrics
            .total_reasoning_requests
            .fetch_add(1, Ordering::Relaxed);
        ctx.metrics
            .total_reasoning_tokens
            .fetch_add(usage.reasoning_tokens, Ordering::Relaxed);
    }
    if usage.cache_read_tokens > 0 {
        ctx.metrics
            .total_cache_hit_tokens
            .fetch_add(usage.cache_read_tokens, Ordering::Relaxed);
    }
    if usage.total() > 0 {
        ctx.metrics
            .total_tokens
            .fetch_add(usage.total(), Ordering::Relaxed);
    }
}

/// 统一流式代理出口：上游 SSE（任意协议）→ 嗅探 → Parser → IR → Emitter → 客户端协议。
///
/// 取代「normalized_sse_stream 归一化为 Chat JSON + handler 再 parse」的双重转换链路；
/// 嗅探失败时保守回退渠道配置的 `configured_target`。
/// 无 hints 的便捷形态（e2e 测试与无工具请求场景使用）。
#[cfg_attr(not(test), allow(dead_code))]
pub fn proxy_sse_body<E, S>(
    stream: S,
    configured_target: TargetProtocol,
    client: SseClientProtocol,
    ctx: ModelProxyContext,
    log: ProxyRequestLog,
    start_time: Instant,
    model_name: String,
) -> Body
where
    S: futures_util::Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let (tool_hints, preferred_tool) = (Vec::<ToolHint>::new(), None);
    proxy_sse_body_with_hints(
        stream,
        configured_target,
        client,
        ctx,
        log,
        start_time,
        model_name,
        tool_hints,
        preferred_tool,
    )
}

/// [`proxy_sse_body`] 的完整形态：携带请求侧工具提示（用于孤儿工具名恢复）
#[allow(clippy::too_many_arguments)]
pub fn proxy_sse_body_with_hints<E, S>(
    stream: S,
    configured_target: TargetProtocol,
    client: SseClientProtocol,
    ctx: ModelProxyContext,
    mut log: ProxyRequestLog,
    start_time: Instant,
    model_name: String,
    tool_hints: Vec<ToolHint>,
    preferred_tool: Option<String>,
) -> Body
where
    S: futures_util::Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let s = async_stream::stream! {
        let mut reader = SseLineReader::new();
        let mut ttft_recorded = false;
        let mut parser: Option<UniversalParser> = None;
        let mut buffered: Vec<String> = Vec::new();
        let mut final_usage = UniversalUsage::default();
        let mut aborted = false;
        // 上游 SSE 数据行原文（未解析），供日志全文记录与问题排查
        let mut upstream_raw = String::new();
        let mut emitter = ClientSseEmitter::new(client, &log.id, &model_name, tool_hints.clone(), preferred_tool.clone());

        // 单行喂给 parser 并转发产出事件（非 data 行与 [DONE] 静默跳过）
        macro_rules! feed_line {
            ($line:expr, $parser:expr) => {{
                let line: &str = ($line).as_ref();
                if let Some((data, false)) = parse_sse_data_line(line) {
                    if data != "[DONE]" {
                        if !upstream_raw.is_empty() {
                            upstream_raw.push('\n');
                        }
                        upstream_raw.push_str("data: ");
                        upstream_raw.push_str(data);
                    }
                    for event in $parser.feed(data) {
                        if !ttft_recorded {
                            log.ttft_ms = Some(start_time.elapsed().as_millis() as u64);
                            ttft_recorded = true;
                        }
                        if let UniversalStreamEvent::Finish { usage, .. } = &event {
                            final_usage = usage.clone();
                        }
                        for out in emitter.on_event(&event) {
                            yield Ok::<Bytes, std::io::Error>(Bytes::from(out));
                        }
                    }
                }
            }};
        }

        tokio::pin!(stream);

        'outer: while let Some(chunk_res) = stream.next().await {
            match chunk_res {
                Ok(bytes) => {
                    for line in reader.push(&bytes) {
                        if parser.is_none() {
                            let sniffed = line
                                .strip_prefix("data:")
                                .map(str::trim)
                                .filter(|data| !data.is_empty() && *data != "[DONE]")
                                .and_then(egress::detect_response_protocol_from_sse_data);
                            match sniffed {
                                Some(actual) => {
                                    let mut p = UniversalParser::new(
                                        actual,
                                        tool_hints.clone(),
                                        preferred_tool.clone(),
                                    );
                                    for buffered_line in buffered.drain(..) {
                                        feed_line!(buffered_line.as_str(), p);
                                    }
                                    parser = Some(p);
                                    // 本行重新走一遍已判定的常规路径
                                    if let Some(p) = parser.as_mut() {
                                        feed_line!(line, p);
                                    }
                                }
                                None => {
                                    buffered.push(line.to_string());
                                }
                            }
                            continue;
                        }
                        if let Some(p) = parser.as_mut() {
                            feed_line!(line, p);
                        }
                    }
                }
                Err(err) => {
                    log.error_message = Some(format!("流式响应传输中断: {err}"));
                    aborted = true;
                    break 'outer;
                }
            }
        }

        // 冲刷残余尾行（上游未以换行结尾的最后一片）
        if !aborted {
            if let Some(line) = reader.flush() {
                if parser.is_none() {
                    let sniffed = line
                        .strip_prefix("data:")
                        .map(str::trim)
                        .filter(|data| !data.is_empty() && *data != "[DONE]")
                        .and_then(egress::detect_response_protocol_from_sse_data);
                    match sniffed {
                        Some(actual) => {
                            parser = Some(UniversalParser::new(
                                actual,
                                tool_hints.clone(),
                                preferred_tool.clone(),
                            ));
                        }
                        None => buffered.push(line.clone()),
                    }
                }
                if let Some(p) = parser.as_mut() {
                    feed_line!(line.as_str(), p);
                }
            }
        }

        // 收尾：已判定用实际协议 parser；始终未判定则保守回退渠道配置
        let events = match parser.as_mut() {
            Some(p) => p.finish(),
            None => {
                let mut p = UniversalParser::new(
                    configured_target,
                    tool_hints.clone(),
                    preferred_tool.clone(),
                );
                for buffered_line in buffered.drain(..) {
                    feed_line!(buffered_line.as_str(), p);
                }
                p.finish()
            }
        };
        for event in events {
            if let UniversalStreamEvent::Finish { usage, .. } = &event {
                final_usage = usage.clone();
            }
            for out in emitter.on_event(&event) {
                yield Ok::<Bytes, std::io::Error>(Bytes::from(out));
            }
        }
        if let Some(p) = parser.as_ref() {
            let dropped = p.dropped_frames();
            if dropped > 0 {
                warn!(
                    "[ModelGateway] {} 流式响应丢弃 {dropped} 个坏帧，客户端可能收到不完整内容",
                    log.path
                );
            }
        }
        if aborted {
            for out in emitter.abort(&final_usage) {
                yield Ok(Bytes::from(out));
            }
        }

        let dur = start_time.elapsed().as_millis() as u64;
        log.duration_ms = dur;
        if aborted {
            log.status_code = 502;
            ctx.metrics.successful_requests.fetch_sub(1, Ordering::Relaxed);
            ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
        } else {
            log.status_code = 200;
        }
        // 统一用量口径落库（input 为总量，缓存明细单列，与既有表结构一致）
        apply_usage_to_log(&mut log, &final_usage, &ctx);
        // 日志记录上游 SSE 原文（未解析），便于排查协议/内容问题
        log.response_body = super::logger::cap_log_body(upstream_raw);

        ctx.record_log(log).await;
    };

    Body::from_stream(s)
}

/// 同协议快速通道：上游与客户端协议一致时，字节级直通 + 旁路统计。
///
/// IR 往返无法表达协议全部元素（如 Anthropic thinking 的 signature、
/// Responses reasoning 的 encrypted_content），同协议场景必须原生直通，
/// 否则 Claude 系客户端的思考链连续性会被破坏。
/// usage 由 Parser 旁路累积，仅供网关日志记账，不影响下发内容。
pub fn passthrough_sse_body<E, S>(
    stream: S,
    upstream_protocol: TargetProtocol,
    ctx: ModelProxyContext,
    mut log: ProxyRequestLog,
    start_time: Instant,
    _model_name: String,
) -> Body
where
    S: futures_util::Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let s = async_stream::stream! {
        let mut reader = SseLineReader::new();
        let mut ttft_recorded = false;
        let mut parser = UniversalParser::new(upstream_protocol, Vec::new(), None);
        let mut final_usage = UniversalUsage::default();
        let mut aborted = false;

        tokio::pin!(stream);

        while let Some(chunk_res) = stream.next().await {
            match chunk_res {
                Ok(bytes) => {
                    for line in reader.push(&bytes) {
                        if !ttft_recorded {
                            log.ttft_ms = Some(start_time.elapsed().as_millis() as u64);
                            ttft_recorded = true;
                        }
                        if let Some((data, false)) = parse_sse_data_line(&line) {
                            for event in parser.feed(data) {
                                if let UniversalStreamEvent::Finish { usage, .. } = &event {
                                    final_usage = usage.clone();
                                }
                            }
                        } else if parse_sse_data_line(&line).map(|(_, done)| done) == Some(true) {
                            // [DONE] 原样透传
                        }
                        yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("{line}\n")));
                    }
                }
                Err(err) => {
                    log.error_message = Some(format!("流式响应传输中断: {err}"));
                    aborted = true;
                    break;
                }
            }
        }

        if !aborted {
            if let Some(line) = reader.flush() {
                yield Ok(Bytes::from(format!("{line}\n")));
                if let Some((data, false)) = parse_sse_data_line(&line) {
                    for event in parser.feed(data) {
                        if let UniversalStreamEvent::Finish { usage, .. } = &event {
                            final_usage = usage.clone();
                        }
                    }
                }
            }
            for event in parser.finish() {
                if let UniversalStreamEvent::Finish { usage, .. } = &event {
                    final_usage = usage.clone();
                }
            }
        }

        let dur = start_time.elapsed().as_millis() as u64;
        log.duration_ms = dur;
        if aborted {
            log.status_code = 502;
            ctx.metrics.successful_requests.fetch_sub(1, Ordering::Relaxed);
            ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
        } else {
            log.status_code = 200;
        }
        apply_usage_to_log(&mut log, &final_usage, &ctx);
        ctx.record_log(log).await;
    };

    Body::from_stream(s)
}

/// 从请求体提取工具名恢复线索：兼容 OpenAI（function.*）、Anthropic（input_schema）、
/// Gemini（functionDeclarations 的 parameters）三种格式，同时解析 tool_choice 显式指定（最高优先级）。
pub fn extract_tool_hints(body: &JsonValue) -> (Vec<ToolHint>, Option<String>) {
    let mut hints = Vec::new();
    if let Some(tools) = body.get("tools").and_then(JsonValue::as_array) {
        for t in tools {
            let name = t
                .pointer("/function/name")
                .and_then(JsonValue::as_str)
                .or_else(|| t.get("name").and_then(JsonValue::as_str));
            let Some(name) = name else { continue };
            let props = t
                .pointer("/function/parameters/properties")
                .or_else(|| t.pointer("/input_schema/properties"))
                .or_else(|| t.pointer("/parameters/properties"));
            let keys: Vec<String> = props
                .and_then(JsonValue::as_object)
                .map(|o| o.keys().cloned().collect())
                .unwrap_or_default();
            hints.push((name.to_string(), keys));
        }
    }
    let preferred = body
        .pointer("/tool_choice/function/name")
        .and_then(JsonValue::as_str)
        .or_else(|| {
            body.pointer("/tool_choice/name")
                .and_then(JsonValue::as_str)
        })
        .map(str::to_string);
    (hints, preferred)
}

/// 工具名候选：(工具名, 参数键列表)。提取自客户端请求体的 tools 数组，
/// 用于在上游省略 content_block_start 帧时启发式恢复工具名。
pub type ToolHint = (String, Vec<String>);
