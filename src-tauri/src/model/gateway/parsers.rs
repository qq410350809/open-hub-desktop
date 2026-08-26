//! 上游 SSE → IR 事件流（[`UniversalStreamEvent`]）的协议解析器。
//!
//! 每个上游协议一个 Parser；由响应嗅探（`egress::detect_response_protocol_from_sse_data`）
//! 选定后逐行喂数据，产出统一事件供出口 Emitter 重建客户端协议。

use super::ir::{
    CacheControl, ContentPart, ImageSource, PartKind, ReasoningConfig, Role, StopReason,
    ToolChoice, ToolDef, UniversalMessage, UniversalRequest, UniversalStreamEvent, UniversalUsage,
};
use serde_json::{json, Value as JsonValue};
use std::collections::{BTreeMap, BTreeSet};

/// Anthropic 孤儿工具参数缓冲：
/// 部分 new-api 系上游 tool_use 场景缺失 content_block_start 帧，
/// 参数增量先缓冲，工具名可靠恢复后才合成元数据一次性下发。
struct PendingOrphanTool {
    index: u64,
    args: String,
    buffered_fragments: Vec<String>,
}

/// OpenAI Chat Completions SSE 解析器
/// 首片缺 name 的工具调用：缓冲 Start 与参数片段，等 name 出现或流结束时收口
struct PendingToolStart {
    index: u64,
    call_id: String,
    args: String,
}

pub struct ChatParser {
    usage: UniversalUsage,
    usage_seen: bool,
    stop_reason: Option<StopReason>,
    started_tools: BTreeSet<u64>,
    /// 无法解析为 JSON 的坏帧计数（供上层告警；坏帧不再静默吞掉）
    dropped_frames: u64,
    tool_hints: Vec<super::stream::ToolHint>,
    preferred_tool: Option<String>,
    pending_start: Option<PendingToolStart>,
}

impl ChatParser {
    pub fn new(tool_hints: Vec<super::stream::ToolHint>, preferred_tool: Option<String>) -> Self {
        Self {
            usage: UniversalUsage::default(),
            usage_seen: false,
            stop_reason: None,
            started_tools: BTreeSet::new(),
            dropped_frames: 0,
            tool_hints,
            preferred_tool,
            pending_start: None,
        }
    }

    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames
    }

    pub fn feed(&mut self, data: &str) -> Vec<UniversalStreamEvent> {
        let Ok(jv) = serde_json::from_str::<JsonValue>(data) else {
            self.dropped_frames += 1;
            return Vec::new();
        };
        let mut out = Vec::new();

        if let Some(usage) = jv.get("usage").and_then(JsonValue::as_object) {
            self.usage_seen = true;
            if let Some(v) = usage.get("prompt_tokens").and_then(JsonValue::as_u64) {
                self.usage.input_tokens = v;
            }
            if let Some(cached) = usage.get("prompt_tokens_details") {
                if let Some(v) = cached.get("cached_tokens").and_then(JsonValue::as_u64) {
                    self.usage.cache_read_tokens = v;
                }
                if let Some(v) = cached.get("cache_creation_tokens").and_then(JsonValue::as_u64) {
                    self.usage.cache_creation_tokens = v;
                }
            }
            if let Some(v) = usage.get("completion_tokens").and_then(JsonValue::as_u64) {
                self.usage.output_tokens = v;
            }
            if let Some(details) = usage.get("completion_tokens_details") {
                if let Some(v) = details.get("reasoning_tokens").and_then(JsonValue::as_u64) {
                    self.usage.reasoning_tokens = v;
                }
            }
        }

        if let Some(fr) = jv
            .pointer("/choices/0/finish_reason")
            .and_then(JsonValue::as_str)
        {
            self.stop_reason = Some(StopReason::from_chat(fr));
        }

        let Some(delta) = jv.pointer("/choices/0/delta") else {
            return out;
        };
        if let Some(s) = delta
            .get("reasoning_content")
            .or_else(|| delta.get("reasoning"))
            .and_then(JsonValue::as_str)
        {
            if !s.is_empty() {
                out.push(UniversalStreamEvent::ReasoningDelta(s.to_string()));
            }
        }
        if let Some(s) = delta.get("content").and_then(JsonValue::as_str) {
            if !s.is_empty() {
                // DeepSeek 方言：<think>...</think> 内嵌于 content，
                // 拆分为推理/正文两个事件（原 clean_sse_stream 能力的 IR 化）。
                // 注意：不得提前 return——同帧可能还携带 tool_calls 增量
                let mut handled_think = false;
                if let Some(start) = s.find("<think>") {
                    if let Some(end) = s.find("</think>") {
                        if start < end {
                            handled_think = true;
                            let reasoning = s[start + 7..end].trim();
                            let after = s[end + 8..].trim_start();
                            if !reasoning.is_empty() {
                                out.push(UniversalStreamEvent::ReasoningDelta(reasoning.to_string()));
                            }
                            if !after.is_empty() {
                                out.push(UniversalStreamEvent::TextDelta(after.to_string()));
                            }
                        }
                    }
                }
                if !handled_think {
                    out.push(UniversalStreamEvent::TextDelta(s.to_string()));
                }
            }
        }
        if let Some(tool_calls) = delta.get("tool_calls").and_then(JsonValue::as_array) {
            for tc in tool_calls {
                let index = tc.get("index").and_then(JsonValue::as_u64).unwrap_or(0);
                let raw_id = tc
                    .get("id")
                    .and_then(JsonValue::as_str)
                    .filter(|s| !s.is_empty());
                let raw_name = tc
                    .pointer("/function/name")
                    .and_then(JsonValue::as_str)
                    .filter(|s| !s.is_empty());
                let fragment: String = tc
                    .pointer("/function/arguments")
                    .and_then(JsonValue::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("")
                    .to_string();

                // 同一工具仍在 pending 缓冲（首片缺 name 的方言）：追加参数；
                // 后续分片补齐 name 时收口补发 Start（携带已缓冲参数）。
                if let Some(p) = &mut self.pending_start {
                    if p.index == index {
                        p.args.push_str(&fragment);
                        if let Some(n) = raw_name {
                            let p = self.pending_start.take().expect("pending 已被借用");
                            self.started_tools.insert(index);
                            out.push(UniversalStreamEvent::ToolCallStart {
                                index,
                                call_id: p.call_id,
                                name: n.to_string(),
                            });
                            if !p.args.is_empty() {
                                out.push(UniversalStreamEvent::ToolCallDelta {
                                    index,
                                    fragment: p.args,
                                });
                            }
                        }
                        continue;
                    }
                    // 其他工具到达：先按 hints/占位收口 pending，再处理当前
                    let p = self.pending_start.take().expect("pending 已被借用");
                    self.flush_pending(&mut out, &p);
                }

                if self.started_tools.contains(&index) {
                    if !fragment.is_empty() {
                        out.push(UniversalStreamEvent::ToolCallDelta { index, fragment });
                    }
                    continue;
                }

                if raw_name.is_some() {
                    // 首片即带完整元数据：直接发 Start（标准客户端校验要求）
                    self.started_tools.insert(index);
                    out.push(UniversalStreamEvent::ToolCallStart {
                        index,
                        call_id: raw_id
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("call_{index}")),
                        name: raw_name.map(str::to_string).unwrap_or_else(|| "tool".to_string()),
                    });
                    if !fragment.is_empty() {
                        out.push(UniversalStreamEvent::ToolCallDelta { index, fragment });
                    }
                } else {
                    // 缺 name：进入 pending，等待迟到元数据或 finish() 收口
                    self.pending_start = Some(PendingToolStart {
                        index,
                        call_id: raw_id
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("call_{index}")),
                        args: fragment,
                    });
                }
            }
        }
        out
    }

    /// 收口 pending 工具：按 preferred_tool → hints 参数键匹配 → 占位名恢复
    fn flush_pending(&mut self, out: &mut Vec<UniversalStreamEvent>, p: &PendingToolStart) {
        self.started_tools.insert(p.index);
        let name = self
            .preferred_tool
            .clone()
            .or_else(|| {
                self.tool_hints
                    .iter()
                    .map(|(n, keys)| {
                        (n.clone(), keys.iter().filter(|k| p.args.contains(k.as_str())).count())
                    })
                    .max_by_key(|(_, sc)| *sc)
                    .filter(|(_, sc)| *sc > 0)
                    .map(|(n, _)| n)
            })
            .unwrap_or_else(|| "tool".to_string());
        out.push(UniversalStreamEvent::ToolCallStart {
            index: p.index,
            call_id: p.call_id.clone(),
            name,
        });
        if !p.args.is_empty() {
            out.push(UniversalStreamEvent::ToolCallDelta {
                index: p.index,
                fragment: p.args.clone(),
            });
        }
    }

    pub fn finish(&mut self) -> Vec<UniversalStreamEvent> {
        let _ = self.usage_seen;
        let mut out = Vec::new();
        if let Some(p) = self.pending_start.take() {
            self.flush_pending(&mut out, &p);
        }
        out.push(UniversalStreamEvent::Finish {
            reason: self.stop_reason.unwrap_or(StopReason::EndTurn),
            usage: self.usage.clone(),
        });
        out
    }
}

/// Anthropic Messages SSE 解析器。
/// 保留孤儿工具恢复逻辑（tool_hints / preferred_tool / 缓冲收口）。
pub struct AnthropicParser {
    usage: UniversalUsage,
    /// Anthropic 上游原始 input_tokens（不含缓存）；IR 口径的总量由此换算
    raw_input_tokens: Option<u64>,
    stop_reason: Option<StopReason>,
    block_kinds: BTreeMap<u64, String>,
    tool_meta: BTreeMap<u64, (String, String)>,
    started_tools: BTreeSet<u64>,
    pending_orphan: Option<PendingOrphanTool>,
    tool_hints: Vec<super::stream::ToolHint>,
    preferred_tool: Option<String>,
    /// 无法解析为 JSON 的坏帧计数（供上层告警）
    dropped_frames: u64,
}

impl AnthropicParser {
    pub fn new(tool_hints: Vec<super::stream::ToolHint>, preferred_tool: Option<String>) -> Self {
        Self {
            usage: UniversalUsage::default(),
            raw_input_tokens: None,
            stop_reason: None,
            block_kinds: BTreeMap::new(),
            tool_meta: BTreeMap::new(),
            started_tools: BTreeSet::new(),
            pending_orphan: None,
            tool_hints,
            preferred_tool,
            dropped_frames: 0,
        }
    }

    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames
    }

    fn resolve_orphan_name(&self, args: &str) -> Option<String> {
        if let Some(p) = &self.preferred_tool {
            return Some(p.clone());
        }
        for (name, keys) in &self.tool_hints {
            if keys.iter().any(|k| args.contains(k.as_str())) {
                return Some(name.clone());
            }
        }
        None
    }

    /// IR 口径归一：`input_tokens` = 原始输入 + 缓存命中 + 缓存写入。
    /// Anthropic 上游的 input_tokens 语义不含缓存部分，若原样存入，
    /// 出口侧（Anthropic 扣减缓存）与落库统计会双重扣减/少记。
    fn recompute_input_tokens(&mut self) {
        if let Some(raw) = self.raw_input_tokens {
            self.usage.input_tokens = raw
                .saturating_add(self.usage.cache_read_tokens)
                .saturating_add(self.usage.cache_creation_tokens);
        }
    }

    /// 下发孤儿缓冲：合成 Start + 全部 Delta
    fn flush_orphan(&mut self, events: &mut Vec<UniversalStreamEvent>) {
        let Some(pt) = self.pending_orphan.take() else {
            return;
        };
        let name = self
            .resolve_orphan_name(&pt.args)
            .unwrap_or_else(|| format!("unknown_tool_{}", pt.index + 1));
        let call_id = format!("toolu_synth_{}", pt.index + 1);
        self.tool_meta
            .insert(pt.index, (call_id.clone(), name.clone()));
        self.block_kinds.insert(pt.index, "tool_use".to_string());
        // 标记已启动：否则后续 partial_json 走主匹配 tool_use 臂会重复发 Start
        self.started_tools.insert(pt.index);
        events.push(UniversalStreamEvent::ToolCallStart {
            index: pt.index,
            call_id,
            name,
        });
        for fragment in pt.buffered_fragments {
            events.push(UniversalStreamEvent::ToolCallDelta {
                index: pt.index,
                fragment,
            });
        }
    }

    pub fn feed(&mut self, data: &str) -> Vec<UniversalStreamEvent> {
        let Ok(jv) = serde_json::from_str::<JsonValue>(data) else {
            self.dropped_frames += 1;
            return Vec::new();
        };
        let mut out = Vec::new();

        match jv.get("type").and_then(JsonValue::as_str) {
            Some("message_start") => {
                if let Some(v) = jv
                    .pointer("/message/usage/input_tokens")
                    .and_then(JsonValue::as_u64)
                {
                    self.raw_input_tokens = Some(v);
                }
                self.usage.cache_read_tokens = jv
                    .pointer("/message/usage/cache_read_input_tokens")
                    .and_then(JsonValue::as_u64)
                    .unwrap_or(self.usage.cache_read_tokens);
                self.usage.cache_creation_tokens = jv
                    .pointer("/message/usage/cache_creation_input_tokens")
                    .and_then(JsonValue::as_u64)
                    .unwrap_or(self.usage.cache_creation_tokens);
                self.recompute_input_tokens();
            }
            Some("content_block_start") => {
                let index = jv.get("index").and_then(JsonValue::as_u64).unwrap_or(0);
                let kind = jv
                    .pointer("/content_block/type")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("text")
                    .to_string();
                if kind == "tool_use" {
                    let call_id = jv
                        .pointer("/content_block/id")
                        .and_then(JsonValue::as_str)
                        .unwrap_or("toolu_unknown");
                    let name = jv
                        .pointer("/content_block/name")
                        .and_then(JsonValue::as_str)
                        .unwrap_or("tool");
                    self.tool_meta
                        .insert(index, (call_id.to_string(), name.to_string()));
                }
                self.block_kinds.insert(index, kind);
            }
            Some("content_block_delta") => {
                let index = jv.get("index").and_then(JsonValue::as_u64).unwrap_or(0);
                match self.block_kinds.get(&index).map(String::as_str) {
                    Some("thinking") => {
                        if let Some(t) =
                            jv.pointer("/delta/thinking").and_then(JsonValue::as_str)
                        {
                            if !t.is_empty() {
                                out.push(UniversalStreamEvent::ReasoningDelta(t.to_string()));
                            }
                        }
                    }
                    Some("tool_use") => {
                        if let Some(fragment) = jv
                            .pointer("/delta/partial_json")
                            .and_then(JsonValue::as_str)
                        {
                            if !self.started_tools.contains(&index) {
                                let (call_id, name) = self
                                    .tool_meta
                                    .get(&index)
                                    .cloned()
                                    .unwrap_or(("toolu_unknown".into(), "unknown_tool".into()));
                                out.push(UniversalStreamEvent::ToolCallStart {
                                    index,
                                    call_id,
                                    name,
                                });
                                self.started_tools.insert(index);
                            }
                            if !fragment.is_empty() {
                                out.push(UniversalStreamEvent::ToolCallDelta {
                                    index,
                                    fragment: fragment.to_string(),
                                });
                            }
                        }
                    }
                    _ => {
                        if let Some(t) = jv.pointer("/delta/text").and_then(JsonValue::as_str) {
                            if !t.is_empty() {
                                out.push(UniversalStreamEvent::TextDelta(t.to_string()));
                            }
                        }
                    }
                }
            }
            Some("message_delta") => {
                if let Some(r) = jv.pointer("/delta/stop_reason").and_then(JsonValue::as_str) {
                    self.stop_reason = Some(StopReason::from_anthropic(r));
                }
                if let Some(v) = jv
                    .pointer("/usage/output_tokens")
                    .and_then(JsonValue::as_u64)
                {
                    self.usage.output_tokens = v;
                }
                // 部分上游在 message_delta 里才给出（或更新）缓存计数
                if let Some(v) = jv
                    .pointer("/usage/cache_read_input_tokens")
                    .and_then(JsonValue::as_u64)
                {
                    self.usage.cache_read_tokens = self.usage.cache_read_tokens.max(v);
                }
                if let Some(v) = jv
                    .pointer("/usage/cache_creation_input_tokens")
                    .and_then(JsonValue::as_u64)
                {
                    self.usage.cache_creation_tokens = self.usage.cache_creation_tokens.max(v);
                }
                // 缓存计数可能在 message_delta 才给出或更新，按 IR 口径重算总量
                self.recompute_input_tokens();
            }
            _ => {}
        }

        // 兼容性处理必须在主匹配之后：孤儿 input_json_delta 先于其他帧消费。
        // 终态帧强制收口同样放在主匹配之后——若作为守卫臂会吞掉 message_delta
        // 的 stop_reason / output_tokens / 缓存计数解析（P0：usage 全丢）。
        if self.pending_orphan.is_some()
            && matches!(
                jv.get("type").and_then(JsonValue::as_str),
                Some("content_block_stop") | Some("message_delta") | Some("message_stop")
            )
        {
            self.flush_orphan(&mut out);
        }
        if jv.get("type").and_then(JsonValue::as_str) == Some("content_block_delta")
            && jv.pointer("/delta/type").and_then(JsonValue::as_str) == Some("input_json_delta")
        {
            let index = jv.get("index").and_then(JsonValue::as_u64).unwrap_or(0);
            let orphan = !self.block_kinds.contains_key(&index) && !self.tool_meta.contains_key(&index);
            if orphan || self.pending_orphan.as_ref().map(|p| p.index) == Some(index) {
                let fragment = jv
                    .pointer("/delta/partial_json")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("")
                    .to_string();
                let pt = self.pending_orphan.get_or_insert(PendingOrphanTool {
                    index,
                    args: String::new(),
                    buffered_fragments: Vec::new(),
                });
                pt.args.push_str(&fragment);
                pt.buffered_fragments.push(fragment);

                let hit = self.preferred_tool.is_some()
                    || self.tool_hints.iter().any(|(_, keys)| {
                        keys.iter().any(|k| pt.args.contains(k.as_str()))
                    });
                if hit {
                    self.flush_orphan(&mut out);
                }
                // 未命中：继续缓冲，等待更多参数或终态帧
            }
        }

        out
    }

    pub fn finish(&mut self) -> Vec<UniversalStreamEvent> {
        let mut out = Vec::new();
        if self.pending_orphan.is_some() {
            self.flush_orphan(&mut out);
        }
        out.push(UniversalStreamEvent::Finish {
            reason: self.stop_reason.unwrap_or(StopReason::EndTurn),
            usage: self.usage.clone(),
        });
        out
    }
}

/// Google Gemini generateContent SSE 解析器
#[derive(Default)]
pub struct GeminiParser {
    usage: UniversalUsage,
    total_tokens: u64,
    stop_reason: Option<StopReason>,
    tool_seq: u64,
    emitted_any: bool,
    /// 无法解析为 JSON 的坏帧计数（供上层告警）
    dropped_frames: u64,
}

impl GeminiParser {
    pub fn new() -> Self {
        Self {
            usage: UniversalUsage::default(),
            total_tokens: 0,
            stop_reason: None,
            tool_seq: 0,
            emitted_any: false,
            dropped_frames: 0,
        }
    }

    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames
    }

    pub fn feed(&mut self, data: &str) -> Vec<UniversalStreamEvent> {
        let Ok(jv) = serde_json::from_str::<JsonValue>(data) else {
            self.dropped_frames += 1;
            return Vec::new();
        };
        let mut out = Vec::new();
        if let Some(candidates) = jv.get("candidates").and_then(JsonValue::as_array) {
            if let Some(c0) = candidates.first() {
                if let Some(parts) = c0.pointer("/content/parts").and_then(JsonValue::as_array) {
                    for part in parts {
                        let is_thought = part
                            .get("thought")
                            .and_then(JsonValue::as_bool)
                            .unwrap_or(false);
                        if let Some(t) = part.get("text").and_then(JsonValue::as_str) {
                            if t.is_empty() {
                                continue;
                            }
                            self.emitted_any = true;
                            out.push(if is_thought {
                                UniversalStreamEvent::ReasoningDelta(t.to_string())
                            } else {
                                UniversalStreamEvent::TextDelta(t.to_string())
                            });
                        }
                        if let Some(fc) = part.get("functionCall") {
                            self.emitted_any = true;
                            let index = self.tool_seq;
                            self.tool_seq += 1;
                            let name = fc
                                .get("name")
                                .and_then(JsonValue::as_str)
                                .unwrap_or("tool");
                            let args = fc
                                .get("args")
                                .cloned()
                                .unwrap_or_else(|| json!({}));
                            out.push(UniversalStreamEvent::ToolCallStart {
                                index,
                                call_id: format!("gem_call_{index}"),
                                name: name.to_string(),
                            });
                            out.push(UniversalStreamEvent::ToolCallDelta {
                                index,
                                fragment: args.to_string(),
                            });
                        }
                    }
                }
                if let Some(fr) = c0.get("finishReason").and_then(JsonValue::as_str) {
                    self.stop_reason = Some(StopReason::from_gemini(fr));
                }
            }
        }
        if let Some(u) = jv.get("usageMetadata").and_then(JsonValue::as_object) {
            let get = |key: &str| {
                u.get(key).and_then(JsonValue::as_u64).unwrap_or(0)
            };
            self.usage.input_tokens = get("promptTokenCount");
            self.usage.cache_read_tokens = get("cachedContentTokenCount");
            let thoughts = get("thoughtsTokenCount");
            self.usage.reasoning_tokens = self.usage.reasoning_tokens.max(thoughts);
            // 与既有口径一致：completion = candidates + thoughts
            let candidates = get("candidatesTokenCount");
            self.usage.output_tokens = candidates + thoughts;
            self.total_tokens = get("totalTokenCount");
        }
        out
    }

    pub fn finish(&mut self) -> Vec<UniversalStreamEvent> {
        let _ = self.emitted_any;
        // Gemini 函数调用的 finishReason 恒为 "STOP"（映射为 EndTurn）；
        // 已产出工具调用时必须推断为 ToolUse，否则依赖
        // finish_reason:"tool_calls" 的客户端框架不会执行工具。
        let reason = match self.stop_reason {
            Some(r) if r != StopReason::EndTurn => r,
            _ if self.tool_seq > 0 => StopReason::ToolUse,
            _ => StopReason::EndTurn,
        };
        vec![UniversalStreamEvent::Finish { reason, usage: self.usage.clone() }]
    }
}

/// OpenAI Responses SSE 解析器
#[derive(Default)]
pub struct ResponsesParser {
    usage: UniversalUsage,
    usage_seen: bool,
    stop_reason: Option<StopReason>,
    /// item_id → 工具序号（output_item.added 时分配）
    item_index: BTreeMap<String, u64>,
    started_items: BTreeSet<String>,
    tool_seq: u64,
    /// 无法解析为 JSON 的坏帧计数（供上层告警）
    dropped_frames: u64,
}

impl ResponsesParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames
    }

    pub fn feed(&mut self, data: &str) -> Vec<UniversalStreamEvent> {
        let Ok(jv) = serde_json::from_str::<JsonValue>(data) else {
            self.dropped_frames += 1;
            return Vec::new();
        };
        let mut out = Vec::new();
        match jv.get("type").and_then(JsonValue::as_str) {
            Some("response.output_text.delta") => {
                if let Some(t) = jv.get("delta").and_then(JsonValue::as_str) {
                    if !t.is_empty() {
                        out.push(UniversalStreamEvent::TextDelta(t.to_string()));
                    }
                }
            }
            Some("response.reasoning_summary_text.delta") => {
                if let Some(t) = jv.get("delta").and_then(JsonValue::as_str) {
                    if !t.is_empty() {
                        out.push(UniversalStreamEvent::ReasoningDelta(t.to_string()));
                    }
                }
            }
            Some("response.output_item.added") => {
                if jv.pointer("/item/type").and_then(JsonValue::as_str) == Some("function_call") {
                    let item_id = jv
                        .pointer("/item/id")
                        .and_then(JsonValue::as_str)
                        .unwrap_or("")
                        .to_string();
                    let index = self.tool_seq;
                    self.tool_seq += 1;
                    self.item_index.insert(item_id.clone(), index);
                    let call_id = jv
                        .pointer("/item/call_id")
                        .and_then(JsonValue::as_str)
                        .or_else(|| jv.pointer("/item/id").and_then(JsonValue::as_str))
                        .unwrap_or("call_unknown");
                    let name = jv
                        .pointer("/item/name")
                        .and_then(JsonValue::as_str)
                        .unwrap_or("tool");
                    self.started_items.insert(item_id);
                    out.push(UniversalStreamEvent::ToolCallStart {
                        index,
                        call_id: call_id.to_string(),
                        name: name.to_string(),
                    });
                }
            }
            Some("response.function_call_arguments.delta") => {
                let item_id = jv.get("item_id").and_then(JsonValue::as_str).unwrap_or("");
                let index = *self.item_index.entry(item_id.to_string()).or_insert_with(|| {
                    let index = self.tool_seq;
                    self.tool_seq += 1;
                    index
                });
                if !self.started_items.contains(item_id) {
                    self.started_items.insert(item_id.to_string());
                    out.push(UniversalStreamEvent::ToolCallStart {
                        index,
                        call_id: item_id.to_string(),
                        name: "tool".to_string(),
                    });
                }
                if let Some(frag) = jv.get("delta").and_then(JsonValue::as_str) {
                    if !frag.is_empty() {
                        out.push(UniversalStreamEvent::ToolCallDelta {
                            index,
                            fragment: frag.to_string(),
                        });
                    }
                }
            }
            Some("response.output_item.done") => {
                // 兜底：上游缺失 added/delta 帧时按完整块下发
                if jv.pointer("/item/type").and_then(JsonValue::as_str) == Some("function_call") {
                    let item_id = jv
                        .pointer("/item/id")
                        .and_then(JsonValue::as_str)
                        .unwrap_or("")
                        .to_string();
                    if !self.started_items.contains(&item_id) {
                        let index = *self.item_index.entry(item_id.clone()).or_insert_with(|| {
                            let index = self.tool_seq;
                            self.tool_seq += 1;
                            index
                        });
                        self.started_items.insert(item_id.clone());
                        let call_id = jv
                            .pointer("/item/call_id")
                            .and_then(JsonValue::as_str)
                            .or_else(|| jv.pointer("/item/id").and_then(JsonValue::as_str))
                            .unwrap_or("call_unknown");
                        let name = jv
                            .pointer("/item/name")
                            .and_then(JsonValue::as_str)
                            .unwrap_or("tool");
                        out.push(UniversalStreamEvent::ToolCallStart {
                            index,
                            call_id: call_id.to_string(),
                            name: name.to_string(),
                        });
                        let arguments = jv
                            .pointer("/item/arguments")
                            .cloned()
                            .unwrap_or_else(|| json!("{}"))
                            .to_string();
                        out.push(UniversalStreamEvent::ToolCallDelta { index, fragment: arguments });
                    }
                }
            }
            Some("response.completed") | Some("response.incomplete") => {
                self.usage_seen = true;
                if let Some(v) = jv.pointer("/response/usage/input_tokens").and_then(JsonValue::as_u64) {
                    self.usage.input_tokens = v;
                }
                if let Some(v) = jv
                    .pointer("/response/usage/input_tokens_details/cached_tokens")
                    .and_then(JsonValue::as_u64)
                {
                    self.usage.cache_read_tokens = v;
                }
                if let Some(v) = jv.pointer("/response/usage/output_tokens").and_then(JsonValue::as_u64) {
                    self.usage.output_tokens = v;
                }
                if let Some(v) = jv
                    .pointer("/response/usage/output_tokens_details/reasoning_tokens")
                    .and_then(JsonValue::as_u64)
                {
                    self.usage.reasoning_tokens = v;
                }
                if jv.get("type").and_then(JsonValue::as_str) == Some("response.incomplete") {
                    self.stop_reason = Some(StopReason::MaxTokens);
                } else if self.tool_seq > 0 {
                    self.stop_reason = Some(StopReason::ToolUse);
                }
            }
            _ => {}
        }
        out
    }

    pub fn finish(&mut self) -> Vec<UniversalStreamEvent> {
        let reason = self.stop_reason.unwrap_or({
            if self.tool_seq > 0 {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            }
        });
        let _ = self.usage_seen;
        vec![UniversalStreamEvent::Finish { reason, usage: self.usage.clone() }]
    }
}

/// 嗅探后选定的统一解析器
pub enum UniversalParser {
    Chat(ChatParser),
    Anthropic(AnthropicParser),
    Gemini(GeminiParser),
    Responses(ResponsesParser),
}

impl UniversalParser {
    pub fn new(
        protocol: crate::model::gateway::egress::TargetProtocol,
        tool_hints: Vec<super::stream::ToolHint>,
        preferred_tool: Option<String>,
    ) -> Self {
        use crate::model::gateway::egress::TargetProtocol as T;
        match protocol {
            T::AnthropicMessages => Self::Anthropic(AnthropicParser::new(tool_hints, preferred_tool)),
            T::Gemini => Self::Gemini(GeminiParser::new()),
            T::OpenAiResponses => Self::Responses(ResponsesParser::new()),
            // 网页直连上游的流即 OpenAI Chat 形状（嗅探失败回退时同样按此解析）
            T::WebChat | T::OpenAiChat => Self::Chat(ChatParser::new(tool_hints, preferred_tool)),
        }
    }

    pub fn feed(&mut self, data: &str) -> Vec<UniversalStreamEvent> {
        match self {
            Self::Chat(p) => p.feed(data),
            Self::Anthropic(p) => p.feed(data),
            Self::Gemini(p) => p.feed(data),
            Self::Responses(p) => p.feed(data),
        }
    }

    pub fn finish(&mut self) -> Vec<UniversalStreamEvent> {
        match self {
            Self::Chat(p) => p.finish(),
            Self::Anthropic(p) => p.finish(),
            Self::Gemini(p) => p.finish(),
            Self::Responses(p) => p.finish(),
        }
    }

    /// 无法解析为 JSON 而被丢弃的坏帧数（诊断用）
    pub fn dropped_frames(&self) -> u64 {
        match self {
            Self::Chat(p) => p.dropped_frames(),
            Self::Anthropic(p) => p.dropped_frames(),
            Self::Gemini(p) => p.dropped_frames(),
            Self::Responses(p) => p.dropped_frames(),
        }
    }
}


// ===========================================================================
// 请求方向：协议体 → UniversalRequest
// ===========================================================================


/// 从 OpenAI Chat content 复合数组提取部件（image_url 支持 data URL 与 http URL）
fn chat_content_parts(content: &JsonValue) -> Vec<ContentPart> {
    if let Some(text) = content.as_str() {
        return if text.is_empty() {
            Vec::new()
        } else {
            vec![ContentPart::text(text)]
        };
    }
    let Some(arr) = content.as_array() else { return Vec::new() };
    let mut parts = Vec::new();
    for item in arr {
        if let Some(t) = item.get("text").and_then(JsonValue::as_str) {
            if !t.is_empty() {
                parts.push(ContentPart::text(t));
            }
        } else if let Some(img) = item.get("image_url").and_then(|v| v.get("url")).or_else(|| item.get("url")) {
            if let Some(url) = img.as_str() {
                let source = if let Some(rest) = url.strip_prefix("data:") {
                    let (mime, data) = rest.split_once(";base64,").map(|(m, d)| (m.to_string(), d.to_string())).unwrap_or(("image/png".into(), String::new()));
                    ImageSource::Base64 { media_type: mime, data }
                } else {
                    ImageSource::Url(url.to_string())
                };
                parts.push(ContentPart { kind: PartKind::Image(source), cache_control: None });
            }
        } else if item.get("input_audio").is_some() {
            parts.push(ContentPart { kind: PartKind::Unsupported { hint: "[语音输入]".into() }, cache_control: None });
        }
    }
    parts
}

/// 对文本部件做内联 `<think>...</think>` 拆分（DeepSeek 方言）
fn split_inline_think(parts: Vec<ContentPart>) -> Vec<ContentPart> {
    let mut out = Vec::with_capacity(parts.len());
    for mut part in parts {
        if let PartKind::Text { text } = &part.kind {
            if let (Some(start), Some(end)) = (text.find("<think>"), text.find("</think>")) {
                if start < end {
                    let reasoning = text[start + 7..end].trim().to_string();
                    let after = text[end + 8..].trim_start().to_string();
                    part.kind = PartKind::Text { text: after };
                    out.push(ContentPart {
                        kind: PartKind::Thinking { text: reasoning, signature: None },
                        cache_control: None,
                    });
                }
            }
        }
        if !matches!(&part.kind, PartKind::Text { text } if text.is_empty()) {
            out.push(part);
        }
    }
    out
}

/// OpenAI Chat 请求体 → UniversalRequest（chat 入口；含 developer/model 角色方言归一）
pub fn chat_to_universal(body: &JsonValue, model: &str) -> UniversalRequest {
    let mut ur = UniversalRequest::new(model);
    ur.source = Some("chat");
    ur.stream = body.get("stream").and_then(JsonValue::as_bool).unwrap_or(false);
    ur.temperature = body.get("temperature").and_then(JsonValue::as_f64);
    ur.top_p = body.get("top_p").and_then(JsonValue::as_f64);
    ur.max_tokens = body
        .get("max_completion_tokens")
        .or_else(|| body.get("max_tokens"))
        .and_then(JsonValue::as_u64);
    ur.response_format = body.get("response_format").filter(|v| v.is_object()).cloned();
    if let Some(effort) = body.get("reasoning_effort").and_then(JsonValue::as_str) {
        ur.reasoning = Some(ReasoningConfig { effort: Some(effort.to_string()), ..Default::default() });
    }
    match body.get("stop") {
        Some(JsonValue::String(s)) => ur.stop_sequences.push(s.clone()),
        Some(JsonValue::Array(a)) => {
            for s in a {
                if let Some(s) = s.as_str() {
                    ur.stop_sequences.push(s.to_string());
                }
            }
        }
        _ => {}
    }

    // tools
    if let Some(tools) = body.get("tools").and_then(JsonValue::as_array) {
        for t in tools {
            let name = t.pointer("/function/name").and_then(JsonValue::as_str).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            ur.tools.push(ToolDef {
                name: name.to_string(),
                description: t
                    .pointer("/function/description")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("")
                    .to_string(),
                input_schema: t
                    .pointer("/function/parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
            });
        }
    }
    ur.tool_choice = parse_chat_tool_choice(body);
    ur.parallel_tool_calls = body.get("parallel_tool_calls").and_then(JsonValue::as_bool);

    // messages
    if let Some(msgs) = body.get("messages").and_then(JsonValue::as_array) {
        for m in msgs {
            let role = m.get("role").and_then(JsonValue::as_str).unwrap_or("user");
            match role {
                "system" | "developer" => {
                    for part in chat_content_parts(m.get("content").unwrap_or(&JsonValue::Null)) {
                        ur.system.push(part);
                    }
                }
                "assistant" => {
                    let mut um = UniversalMessage {
                        role: Role::Assistant,
                        parts: split_inline_think(chat_content_parts(
                            m.get("content").unwrap_or(&JsonValue::Null),
                        )),
                    };
                    if let Some(tcs) = m.get("tool_calls").and_then(JsonValue::as_array) {
                        for tc in tcs {
                            let call_id = tc.get("id").and_then(JsonValue::as_str).unwrap_or("call_default");
                            let name = tc.pointer("/function/name").and_then(JsonValue::as_str).unwrap_or("");
                            let args = tc.pointer("/function/arguments").and_then(JsonValue::as_str).unwrap_or("{}");
                            if !name.is_empty() {
                                um.parts.push(ContentPart {
                                    kind: PartKind::ToolUse {
                                        call_id: call_id.to_string(),
                                        name: name.to_string(),
                                        input: serde_json::from_str(args).unwrap_or_else(|_| json!({})),
                                    },
                                    cache_control: None,
                                });
                            }
                        }
                    }
                    ur.messages.push(um);
                }
                "user" | "human" => {
                    let um = UniversalMessage {
                        role: Role::User,
                        parts: chat_content_parts(m.get("content").unwrap_or(&JsonValue::Null)),
                    };
                    ur.messages.push(um);
                }
                // Gemini 方言："model" 即 assistant
                "model" => {
                    let um = UniversalMessage {
                        role: Role::Assistant,
                        parts: split_inline_think(chat_content_parts(
                            m.get("content").unwrap_or(&JsonValue::Null),
                        )),
                    };
                    ur.messages.push(um);
                }
                // role:"tool" / function → 归入 User 消息携带 ToolResult（与 Anthropic tool_result 语义对齐）
                _ => {
                    let call_id = m.get("tool_call_id").and_then(JsonValue::as_str).unwrap_or("call_default");
                    let mut content = String::new();
                    if let Some(cs) = m.get("content").and_then(JsonValue::as_str) {
                        content = cs.to_string();
                    } else if let Some(arr) = m.get("content").and_then(JsonValue::as_array) {
                        for part in arr {
                            if let Some(t) = part.get("text").and_then(JsonValue::as_str) {
                                if !content.is_empty() {
                                    content.push('\n');
                                }
                                content.push_str(t);
                            }
                        }
                    }
                    let is_error = m.get("is_error").and_then(JsonValue::as_bool).unwrap_or(false);
                    let mut um = UniversalMessage { role: Role::User, parts: Vec::new() };
                    um.parts.push(ContentPart {
                        kind: PartKind::ToolResult {
                            call_id: call_id.to_string(),
                            content,
                            is_error,
                        },
                        cache_control: None,
                    });
                    ur.messages.push(um);
                }
            }
        }
    }

    collect_extra(body, &mut ur, &[
        "model", "messages", "stream", "temperature", "top_p", "max_tokens",
        "max_completion_tokens", "stop", "tools", "tool_choice", "response_format",
        "reasoning_effort",
    ]);
    ur
}

fn parse_chat_tool_choice(body: &JsonValue) -> Option<ToolChoice> {
    let tc = body.get("tool_choice")?;
    match tc {
        JsonValue::String(s) => match s.as_str() {
            "auto" => Some(ToolChoice::Auto),
            "none" => Some(ToolChoice::None_),
            "required" => Some(ToolChoice::Required),
            _ => None,
        },
        JsonValue::Object(_) => {
            let name = tc.pointer("/function/name").and_then(JsonValue::as_str)?;
            Some(ToolChoice::Tool { name: name.to_string() })
        }
        _ => None,
    }
}

/// 未被结构化建模的顶层字段收入 extra 保真通道
fn collect_extra(body: &JsonValue, ur: &mut UniversalRequest, modeled: &[&str]) {
    if let Some(obj) = body.as_object() {
        for (key, value) in obj {
            if !modeled.contains(&key.as_str()) && !value.is_null() {
                ur.extra.insert(key.clone(), value.clone());
            }
        }
    }
}

/// Anthropic Messages 请求体 → UniversalRequest（cache_control / thinking signature 全保真）
pub fn anthropic_to_universal(body: &JsonValue, model: &str) -> UniversalRequest {
    let mut ur = UniversalRequest::new(model);
    ur.source = Some("anthropic");
    ur.stream = body.get("stream").and_then(JsonValue::as_bool).unwrap_or(false);
    ur.max_tokens = body.get("max_tokens").and_then(JsonValue::as_u64);
    ur.temperature = body.get("temperature").and_then(JsonValue::as_f64);
    ur.top_p = body.get("top_p").and_then(JsonValue::as_f64);
    ur.top_k = body.get("top_k").and_then(JsonValue::as_u64).map(|v| v as u32);
    if let Some(ss) = body.get("stop_sequences").and_then(JsonValue::as_array) {
        for s in ss {
            if let Some(s) = s.as_str() {
                ur.stop_sequences.push(s.to_string());
            }
        }
    }
    ur.metadata = body.get("metadata").filter(|v| v.is_object()).cloned();
    if let Some(thinking) = body.get("thinking") {
        if thinking.get("type").and_then(JsonValue::as_str) == Some("enabled") {
            ur.reasoning = Some(ReasoningConfig {
                effort: None,
                budget_tokens: thinking.get("budget_tokens").and_then(JsonValue::as_u64),
            });
        }
    }

    // tools
    if let Some(tools) = body.get("tools").and_then(JsonValue::as_array) {
        for t in tools {
            let name = t.get("name").and_then(JsonValue::as_str).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            ur.tools.push(ToolDef {
                name: name.to_string(),
                description: t.get("description").and_then(JsonValue::as_str).unwrap_or("").to_string(),
                input_schema: t
                    .get("input_schema")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
            });
        }
    }
    if let Some(tc) = body.get("tool_choice").and_then(|v| v.get("type")).and_then(JsonValue::as_str) {
        ur.tool_choice = match tc {
            "any" => Some(ToolChoice::Required),
            "auto" => Some(ToolChoice::Auto),
            "tool" => body
                .pointer("/tool_choice/name")
                .and_then(JsonValue::as_str)
                .map(|n| ToolChoice::Tool { name: n.to_string() }),
            _ => None,
        };
    }

    // system（string 或 blocks，blocks 携带 cache_control）
    parse_anthropic_system_into(body, &mut ur);

    // messages
    let mut gem_seq = 0usize; // 无 id 工具调用的兜底序号
    if let Some(msgs) = body.get("messages").and_then(JsonValue::as_array) {
        for msg in msgs {
            let role = match msg.get("role").and_then(JsonValue::as_str) {
                Some("assistant") => Role::Assistant,
                _ => Role::User,
            };
            let mut um = UniversalMessage { role, parts: Vec::new() };
            match msg.get("content") {
                Some(JsonValue::String(text)) => {
                    if !text.is_empty() {
                        um.parts.push(ContentPart::text(text));
                    }
                }
                Some(JsonValue::Array(blocks)) => {
                    for block in blocks {
                        let b_type = block.get("type").and_then(JsonValue::as_str).unwrap_or("");
                        let cache_control = block.get("cache_control").and_then(|cc| {
                            cc.get("type")
                                .and_then(JsonValue::as_str)
                                .map(|k| CacheControl { kind: k.to_string() })
                        });
                        let kind = match b_type {
                            "text" => block.get("text").and_then(JsonValue::as_str).map(|t| {
                                PartKind::Text { text: t.to_string() }
                            }),
                            "thinking" | "redacted_thinking" => {
                                block.get("thinking").and_then(JsonValue::as_str).map(|t| PartKind::Thinking {
                                    text: t.to_string(),
                                    signature: block
                                        .get("signature")
                                        .and_then(JsonValue::as_str)
                                        .map(str::to_string),
                                })
                            }
                            "tool_use" => {
                                let part = PartKind::ToolUse {
                                    call_id: block
                                        .get("id")
                                        .and_then(JsonValue::as_str)
                                        .unwrap_or("call_default")
                                        .to_string(),
                                    name: block.get("name").and_then(JsonValue::as_str).unwrap_or("").to_string(),
                                    input: block.get("input").cloned().unwrap_or_else(|| json!({})),
                                };
                                Some(part)
                            }
                            "tool_result" => {
                                let content_text = match block.get("content") {
                                    Some(JsonValue::String(cs)) => cs.clone(),
                                    Some(JsonValue::Array(arr)) => arr
                                        .iter()
                                        .filter_map(|p| p.get("text").and_then(JsonValue::as_str))
                                        .collect::<Vec<_>>()
                                        .join("\n"),
                                    _ => String::new(),
                                };
                                let part = PartKind::ToolResult {
                                    call_id: block
                                        .get("tool_use_id")
                                        .and_then(JsonValue::as_str)
                                        .unwrap_or("call_default")
                                        .to_string(),
                                    content: content_text,
                                    is_error: block.get("is_error").and_then(JsonValue::as_bool).unwrap_or(false),
                                };
                                Some(part)
                            }
                            "image" => block.pointer("/source/type").and_then(JsonValue::as_str).map(|st| {
                                let source = if st == "url" {
                                    ImageSource::Url(
                                        block.pointer("/source/url").and_then(JsonValue::as_str).unwrap_or("").to_string(),
                                    )
                                } else {
                                    ImageSource::Base64 {
                                        media_type: block
                                            .pointer("/source/media_type")
                                            .and_then(JsonValue::as_str)
                                            .unwrap_or("image/png")
                                            .to_string(),
                                        data: block.pointer("/source/data").and_then(JsonValue::as_str).unwrap_or("").to_string(),
                                    }
                                };
                                PartKind::Image(source)
                            }),
                            _ => None,
                        };
                        if let Some(kind) = kind {
                            um.parts.push(ContentPart { kind, cache_control });
                        }
                    }
                }
                _ => {}
            }
            if !um.parts.is_empty() {
                ur.messages.push(um);
            }
            let _ = &mut gem_seq;
        }
    }

    collect_extra(body, &mut ur, &[
        "model", "messages", "system", "stream", "temperature", "top_p", "top_k",
        "max_tokens", "stop_sequences", "tools", "tool_choice", "thinking",
        "metadata",
    ]);
    ur
}

fn parse_anthropic_system_into(body: &JsonValue, ur: &mut UniversalRequest) {
    match body.get("system") {
        Some(JsonValue::String(sys)) => {
            if !sys.is_empty() {
                ur.system.push(ContentPart::text(sys));
            }
        }
        Some(JsonValue::Array(blocks)) => {
            for block in blocks {
                if let Some(text) = block.get("text").and_then(JsonValue::as_str) {
                    let cache_control = block.get("cache_control").and_then(|cc| {
                        cc.get("type")
                            .and_then(JsonValue::as_str)
                            .map(|k| CacheControl { kind: k.to_string() })
                    });
                    ur.system.push(ContentPart {
                        kind: PartKind::Text { text: text.to_string() },
                        cache_control,
                    });
                }
            }
        }
        _ => {}
    }
}


/// OpenAI Responses 请求体 → UniversalRequest
pub fn responses_to_universal(body: &JsonValue, model: &str) -> UniversalRequest {
    let mut ur = UniversalRequest::new(model);
    ur.source = Some("responses");
    ur.stream = body.get("stream").and_then(JsonValue::as_bool).unwrap_or(false);
    ur.temperature = body.get("temperature").and_then(JsonValue::as_f64);
    ur.top_p = body.get("top_p").and_then(JsonValue::as_f64);
    // Responses 的 max_output_tokens 含推理部分；Chat 口径亦近似处理
    ur.max_tokens = body.get("max_output_tokens").and_then(JsonValue::as_u64);
    if let Some(reasoning) = body.get("reasoning") {
        ur.reasoning = Some(ReasoningConfig {
            effort: reasoning.get("effort").and_then(JsonValue::as_str).map(str::to_string),
            budget_tokens: None,
        });
    }
    if let Some(instructions) = body.get("instructions").and_then(JsonValue::as_str) {
        if !instructions.is_empty() {
            ur.system.push(ContentPart::text(instructions));
        }
    }

    // tools：{type:function,name,description,parameters}
    if let Some(tools) = body.get("tools").and_then(JsonValue::as_array) {
        for t in tools {
            if t.get("type").and_then(JsonValue::as_str) != Some("function") {
                continue;
            }
            let name = t.get("name").and_then(JsonValue::as_str).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            ur.tools.push(ToolDef {
                name: name.to_string(),
                description: t.get("description").and_then(JsonValue::as_str).unwrap_or("").to_string(),
                input_schema: t
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
            });
        }
    }
    match body.get("tool_choice") {
        Some(JsonValue::String(s)) => match s.as_str() {
            "auto" => ur.tool_choice = Some(ToolChoice::Auto),
            "none" => ur.tool_choice = Some(ToolChoice::None_),
            "required" => ur.tool_choice = Some(ToolChoice::Required),
            _ => {}
        },
        Some(JsonValue::Object(_)) => {
            if let Some(name) = body.pointer("/tool_choice/name").and_then(JsonValue::as_str) {
                ur.tool_choice = Some(ToolChoice::Tool { name: name.to_string() });
            }
        }
        _ => {}
    }

    // input：字符串或 items 数组
    match body.get("input") {
        Some(JsonValue::String(text)) => {
            if !text.is_empty() {
                ur.messages.push(UniversalMessage {
                    role: Role::User,
                    parts: vec![ContentPart::text(text)],
                });
            }
        }
        Some(JsonValue::Array(items)) => {
            let mut gem_seq = 0usize;
            for item in items {
                let item_type = item.get("type").and_then(JsonValue::as_str).unwrap_or("message");
                match item_type {
                    "message" => {
                        let role = match item.get("role").and_then(JsonValue::as_str) {
                            Some("assistant") => Role::Assistant,
                            _ => Role::User,
                        };
                        let mut um = UniversalMessage { role, parts: Vec::new() };
                        match item.get("content") {
                            Some(JsonValue::String(text)) => {
                                if !text.is_empty() {
                                    um.parts.push(ContentPart::text(text));
                                }
                            }
                            Some(JsonValue::Array(parts)) => {
                                for part in parts {
                                    let text = part
                                        .get("text")
                                        .or_else(|| part.get("input_text"))
                                        .or_else(|| part.get("output_text"))
                                        .and_then(JsonValue::as_str);
                                    if let Some(t) = text {
                                        if !t.is_empty() {
                                            um.parts.push(ContentPart::text(t));
                                        }
                                    }
                                    if part.get("type").and_then(JsonValue::as_str) == Some("input_image") {
                                        if let Some(url) =
                                            part.pointer("/image_url").and_then(JsonValue::as_str)
                                        {
                                            let source = if let Some(rest) = url.strip_prefix("data:") {
                                                let (mime, data) = rest
                                                    .split_once(";base64,")
                                                    .map(|(m, d)| (m.to_string(), d.to_string()))
                                                    .unwrap_or(("image/png".into(), String::new()));
                                                ImageSource::Base64 { media_type: mime, data }
                                            } else {
                                                ImageSource::Url(url.to_string())
                                            };
                                            um.parts.push(ContentPart {
                                                kind: PartKind::Image(source),
                                                cache_control: None,
                                            });
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                        if !um.parts.is_empty() {
                            ur.messages.push(um);
                        }
                    }
                    "function_call" => {
                        let index = gem_seq; // 保留递增语义供后续扩展
                        gem_seq += 1;
                        let _ = index;
                        let mut um = UniversalMessage { role: Role::Assistant, parts: Vec::new() };
                        um.parts.push(ContentPart {
                            kind: PartKind::ToolUse {
                                call_id: item
                                    .get("call_id")
                                    .or_else(|| item.get("id"))
                                    .and_then(JsonValue::as_str)
                                    .unwrap_or("call_unknown")
                                    .to_string(),
                                name: item.get("name").and_then(JsonValue::as_str).unwrap_or("").to_string(),
                                input: item
                                    .get("arguments")
                                    .and_then(JsonValue::as_str)
                                    .and_then(|a| serde_json::from_str(a).ok())
                                    .unwrap_or_else(|| json!({})),
                            },
                            cache_control: None,
                        });
                        ur.messages.push(um);
                    }
                    "function_call_output" => {
                        let output = match item.get("output") {
                            Some(JsonValue::String(os)) => os.clone(),
                            Some(other) => other.to_string(),
                            None => String::new(),
                        };
                        let mut um = UniversalMessage { role: Role::User, parts: Vec::new() };
                        um.parts.push(ContentPart {
                            kind: PartKind::ToolResult {
                                call_id: item
                                    .get("call_id")
                                    .and_then(JsonValue::as_str)
                                    .unwrap_or("call_default")
                                    .to_string(),
                                content: output,
                                is_error: false,
                            },
                            cache_control: None,
                        });
                        ur.messages.push(um);
                    }
                    // reasoning 输入项：encrypted_content 仅同协议快车道有意义，
                    // 跨协议转换时无解密可能，按协议语义丢弃
                    _ => {}
                }
            }
        }
        _ => {}
    }

    collect_extra(body, &mut ur, &[
        "model", "input", "instructions", "stream", "temperature", "top_p",
        "max_output_tokens", "tools", "tool_choice", "reasoning",
    ]);
    ur
}

/// Google Gemini generateContent 请求体 → UniversalRequest
pub fn gemini_to_universal(body: &JsonValue, model: &str) -> UniversalRequest {
    let mut ur = UniversalRequest::new(model);
    ur.source = Some("gemini");
    ur.stream = false; // 由 URL action 决定，body 内无 stream 字段
    let mut tool_seq = 0usize;
    // Gemini 以函数名关联 functionCall 与 functionResponse；解析过程中
    // 维护「函数名 → 最近一次 functionCall 的 call_id」映射，让 ToolResult
    // 能关联到合成 call_id（跨协议出站时 linkage 不失真）。
    let mut name_to_call_id = std::collections::HashMap::new();

    if let Some(sys) = body.get("systemInstruction").or_else(|| body.get("system_instruction")) {
        if let Some(parts) = sys.pointer("/parts").and_then(JsonValue::as_array) {
            for part in parts {
                if let Some(t) = part.get("text").and_then(JsonValue::as_str) {
                    if !t.is_empty() {
                        ur.system.push(ContentPart::text(t));
                    }
                }
            }
        }
    }

    if let Some(contents) = body.get("contents").and_then(JsonValue::as_array) {
        for content in contents {
            let role = match content.get("role").and_then(JsonValue::as_str) {
                Some("model") => Role::Assistant,
                _ => Role::User,
            };
            let mut um = UniversalMessage { role, parts: Vec::new() };
            if let Some(parts) = content.pointer("/content/parts").or_else(|| content.get("parts")).and_then(JsonValue::as_array) {
                for part in parts {
                    let is_thought = part.get("thought").and_then(JsonValue::as_bool).unwrap_or(false);
                    if let Some(t) = part.get("text").and_then(JsonValue::as_str) {
                        if t.is_empty() {
                            continue;
                        }
                        if is_thought {
                            um.parts.push(ContentPart {
                                kind: PartKind::Thinking { text: t.to_string(), signature: None },
                                cache_control: None,
                            });
                        } else {
                            um.parts.push(ContentPart::text(t));
                        }
                    }
                    if let Some(fc) = part.get("functionCall") {
                        let index = tool_seq;
                        tool_seq += 1;
                        let call_id = format!("gem_call_{index}");
                        let name = fc.get("name").and_then(JsonValue::as_str).unwrap_or("").to_string();
                        name_to_call_id.insert(name.clone(), call_id.clone());
                        um.parts.push(ContentPart {
                            kind: PartKind::ToolUse {
                                call_id,
                                name,
                                input: fc.get("args").cloned().unwrap_or_else(|| json!({})),
                            },
                            cache_control: None,
                        });
                    }
                    if let Some(fr) = part.get("functionResponse") {
                        let name = fr.get("name").and_then(JsonValue::as_str).unwrap_or("").to_string();
                        // 优先关联到最近一次同名 functionCall 的合成 call_id；
                        // 无匹配时回退函数名（保住 gemini→gemini 的同名关联语义）
                        let call_id = name_to_call_id.get(&name).cloned().unwrap_or_else(|| name.clone());
                        um.parts.push(ContentPart {
                            kind: PartKind::ToolResult {
                                call_id,
                                content: fr.get("response").cloned().map(|v| v.to_string()).unwrap_or_default(),
                                is_error: false,
                            },
                            cache_control: None,
                        });
                    }
                    if let Some(inline) = part.get("inlineData").or_else(|| part.get("inline_data")) {
                        um.parts.push(ContentPart {
                            kind: PartKind::Image(ImageSource::Base64 {
                                media_type: inline
                                    .get("mimeType")
                                    .or_else(|| inline.get("mime_type"))
                                    .and_then(JsonValue::as_str)
                                    .unwrap_or("image/png")
                                    .to_string(),
                                data: inline.get("data").and_then(JsonValue::as_str).unwrap_or("").to_string(),
                            }),
                            cache_control: None,
                        });
                    }
                }
            }
            if !um.parts.is_empty() {
                ur.messages.push(um);
            }
        }
    }

    // tools[0].functionDeclarations
    if let Some(decls) = body
        .pointer("/tools/0/functionDeclarations")
        .or_else(|| body.pointer("/tools/0/function_declarations"))
        .and_then(JsonValue::as_array)
    {
        for d in decls {
            let name = d.get("name").and_then(JsonValue::as_str).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            ur.tools.push(ToolDef {
                name: name.to_string(),
                description: d.get("description").and_then(JsonValue::as_str).unwrap_or("").to_string(),
                input_schema: d.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
            });
        }
    }
    // toolConfig.mode → ToolChoice
    let mode = body
        .pointer("/toolConfig/functionCallingConfig/mode")
        .or_else(|| body.pointer("/tool_config/function_calling_config/mode"))
        .and_then(JsonValue::as_str);
    let allowed = body
        .pointer("/toolConfig/functionCallingConfig/allowedFunctionNames")
        .or_else(|| body.pointer("/tool_config/function_calling_config/allowed_function_names"))
        .and_then(JsonValue::as_array)
        .and_then(|a| a.first())
        .and_then(JsonValue::as_str);
    ur.tool_choice = match mode {
        Some("ANY") => allowed.map(|n| ToolChoice::Tool { name: n.to_string() }).or(Some(ToolChoice::Required)),
        Some("NONE") => Some(ToolChoice::None_),
        Some("AUTO") => Some(ToolChoice::Auto),
        _ => None,
    };

    // generationConfig
    let gc = body.get("generationConfig");
    if let Some(gc) = gc {
        ur.temperature = gc.get("temperature").and_then(JsonValue::as_f64);
        ur.top_p = gc.get("topP").or_else(|| gc.get("top_p")).and_then(JsonValue::as_f64);
        ur.top_k = gc.get("topK").or_else(|| gc.get("top_k")).and_then(JsonValue::as_u64).map(|v| v as u32);
        ur.max_tokens = gc
            .get("maxOutputTokens")
            .or_else(|| gc.get("max_output_tokens"))
            .and_then(JsonValue::as_u64);
        if let Some(ss) = gc.get("stopSequences").or_else(|| gc.get("stop_sequences")).and_then(JsonValue::as_array) {
            for s in ss {
                if let Some(s) = s.as_str() {
                    ur.stop_sequences.push(s.to_string());
                }
            }
        }
        if let Some(budget) = gc
            .pointer("/thinkingConfig/thinkingBudget")
            .or_else(|| gc.pointer("/thinking_config/thinking_budget"))
            .and_then(JsonValue::as_u64)
        {
            ur.reasoning = Some(ReasoningConfig { effort: None, budget_tokens: Some(budget) });
        }
    }

    collect_extra(body, &mut ur, &[
        "contents", "systemInstruction", "system_instruction", "tools",
        "toolConfig", "tool_config", "generationConfig", "generation_config",
    ]);
    ur
}


// ===========================================================================
// 出网方向：UniversalRequest → 目标协议原生请求体
// ===========================================================================

fn parts_to_text(parts: &[ContentPart]) -> String {
    let mut out = String::new();
    for part in parts {
        let text = match &part.kind {
            PartKind::Text { text } => text.clone(),
            PartKind::Image(_) => "[图片输入]".to_string(),
            PartKind::Thinking { text, .. } => text.clone(),
            PartKind::Unsupported { hint } => hint.clone(),
            _ => continue,
        };
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&text);
    }
    out
}

/// UR → OpenAI Chat 请求体
pub fn universal_to_chat(ur: &UniversalRequest) -> JsonValue {
    let mut messages = Vec::<JsonValue>::new();
    if !ur.system.is_empty() {
        messages.push(json!({ "role": "system", "content": parts_to_text(&ur.system) }));
    }
    for msg in &ur.messages {
        // 工具调用与文本可能同属一条 assistant 消息
        let mut tool_calls = Vec::<JsonValue>::new();
        let mut text_parts = Vec::<String>::new();
        let mut tool_results = Vec::<JsonValue>::new();
        for part in &msg.parts {
            match &part.kind {
                PartKind::Text { text } => text_parts.push(text.clone()),
                PartKind::Thinking { .. } => {} // Chat 无思考输入槽位
                PartKind::Image(source) => {
                    let url = match source {
                        ImageSource::Url(u) => u.clone(),
                        ImageSource::Base64 { media_type, data } => {
                            format!("data:{media_type};base64,{data}")
                        }
                    };
                    text_parts.push(format!("[图片输入:{url}]"));
                }
                PartKind::ToolUse { call_id, name, input } => {
                    tool_calls.push(json!({
                        "id": call_id,
                        "type": "function",
                        "function": { "name": name, "arguments": input.to_string() },
                    }));
                }
                PartKind::ToolResult { call_id, content, is_error } => {
                    let mut content_val = json!(content);
                    if *is_error {
                        content_val = json!({ "error": content });
                    }
                    let _ = content_val;
                    tool_results.push(json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": content,
                    }));
                }
                PartKind::Unsupported { hint } => text_parts.push(hint.clone()),
            }
        }
        if !tool_results.is_empty() {
            messages.extend(tool_results);
            if !text_parts.is_empty() || !tool_calls.is_empty() {
                // 同消息内混合场景：工具结果优先，其余内容按 assistant 追加
                if !tool_calls.is_empty() {
                    messages.push(json!({
                        "role": "assistant",
                        "content": text_parts.join("\n"),
                        "tool_calls": tool_calls,
                    }));
                } else if !text_parts.is_empty() {
                    messages.push(json!({ "role": msg.role.as_str(), "content": text_parts.join("\n") }));
                }
            }
        } else if !tool_calls.is_empty() {
            messages.push(json!({
                "role": "assistant",
                "content": if text_parts.is_empty() { JsonValue::Null } else { json!(text_parts.join("\n")) },
                "tool_calls": tool_calls,
            }));
        } else if !text_parts.is_empty() {
            messages.push(json!({ "role": msg.role.as_str(), "content": text_parts.join("\n") }));
        }
    }

    let mut out = json!({
        "model": ur.model,
        "messages": messages,
        "stream": ur.stream,
    });
    if let Some(v) = ur.max_tokens {
        out["max_tokens"] = json!(v);
    }
    if let Some(v) = ur.temperature {
        out["temperature"] = json!(v);
    }
    if let Some(v) = ur.top_p {
        out["top_p"] = json!(v);
    }
    if !ur.stop_sequences.is_empty() {
        out["stop"] = json!(ur.stop_sequences);
    }
    if let Some(rf) = &ur.response_format {
        out["response_format"] = rf.clone();
    }
    if let Some(reasoning) = &ur.reasoning {
        if let Some(effort) = &reasoning.effort {
            out["reasoning_effort"] = json!(effort);
        }
    }
    if !ur.tools.is_empty() {
        let tools: Vec<JsonValue> = ur
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    },
                })
            })
            .collect();
        out["tools"] = json!(tools);
        if let Some(tc) = &ur.tool_choice {
            out["tool_choice"] = chat_tool_choice_json(tc);
        }
        if let Some(parallel) = ur.parallel_tool_calls {
            out["parallel_tool_calls"] = json!(parallel);
        }
    }
    // extra 保真回填：chat→chat 时 extra 全部收集自 chat 请求体，全量回填；
    // 跨协议来源仅回填 Chat 认识的白名单，避免把其他协议的专属字段泄漏进 chat 请求。
    if ur.source == Some("chat") {
        for (key, value) in &ur.extra {
            out[key.as_str()] = value.clone();
        }
    } else {
        for key in CHAT_EXTRA_PASSTHROUGH_KEYS {
            if let Some(value) = ur.extra.get(*key) {
                out[*key] = value.clone();
            }
        }
    }
    out
}

/// Chat 出口认识的 extra 白名单（客户端显式设置不得因 IR 往返而丢失）
const CHAT_EXTRA_PASSTHROUGH_KEYS: &[&str] = &[
    "stream_options",
    "frequency_penalty",
    "presence_penalty",
    "seed",
    "user",
    "n",
    "logit_bias",
    "logprobs",
    "top_logprobs",
];

fn chat_tool_choice_json(tc: &ToolChoice) -> JsonValue {
    match tc {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None_ => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Tool { name } => json!({ "type": "function", "function": { "name": name } }),
    }
}

/// UR → Anthropic Messages 原生体（cache_control 回填、thinking signature 保真）
pub fn universal_to_anthropic(ur: &UniversalRequest) -> JsonValue {
    fn part_to_block(part: &ContentPart) -> Option<JsonValue> {
        let mut block = match &part.kind {
            PartKind::Text { text } => json!({ "type": "text", "text": text }),
            // 无 signature 的思考块（chat <think> / gemini thought 拆出）在
            // assistant 消息内会被 Anthropic 上游拒绝（thinking 块必须携带有效签名），
            // 直接跳过；带签名（anthropic 源往返）才回写。
            PartKind::Thinking { signature: None, .. } => return None,
            PartKind::Thinking { text, signature: Some(sig) } => {
                json!({ "type": "thinking", "thinking": text, "signature": sig })
            }
            PartKind::Image(ImageSource::Base64 { media_type, data }) => json!({
                "type": "image",
                "source": { "type": "base64", "media_type": media_type, "data": data },
            }),
            PartKind::Image(ImageSource::Url(url)) => json!({
                "type": "image",
                "source": { "type": "url", "url": url },
            }),
            PartKind::ToolUse { call_id, name, input } => json!({
                "type": "tool_use", "id": call_id, "name": name, "input": input,
            }),
            PartKind::ToolResult { call_id, content, is_error } => {
                let mut b = json!({ "type": "tool_result", "tool_use_id": call_id, "content": content });
                if *is_error {
                    b["is_error"] = json!(true);
                }
                b
            }
            PartKind::Unsupported { hint } => json!({ "type": "text", "text": hint }),
        };
        if let Some(cc) = &part.cache_control {
            block["cache_control"] = json!({ "type": cc.kind });
        }
        Some(block)
    }

    let mut messages = Vec::<JsonValue>::new();
    for msg in &ur.messages {
        let blocks: Vec<JsonValue> =
            msg.parts.iter().filter_map(part_to_block).collect();
        if blocks.is_empty() {
            continue;
        }
        messages.push(json!({ "role": msg.role.as_str(), "content": blocks }));
    }

    // Anthropic 官方必填 max_tokens；缺省给保守默认（与旧转换器口径一致）
    let max_tokens = ur.max_tokens.unwrap_or(4096);

    let mut system_blocks = Vec::<JsonValue>::new();
    for part in &ur.system {
        let mut block = match &part.kind {
            PartKind::Text { text } => json!({ "type": "text", "text": text }),
            _ => continue,
        };
        if let Some(cc) = &part.cache_control {
            block["cache_control"] = json!({ "type": cc.kind });
        }
        system_blocks.push(block);
    }

    let mut out = json!({
        "model": ur.model,
        "max_tokens": max_tokens,
        "stream": ur.stream,
        "messages": messages,
    });
    if !system_blocks.is_empty() {
        out["system"] = json!(system_blocks);
    }
    if let Some(v) = ur.temperature {
        out["temperature"] = json!(v);
    }
    if let Some(v) = ur.top_p {
        out["top_p"] = json!(v);
    }
    if let Some(v) = ur.top_k {
        out["top_k"] = json!(v);
    }
    if !ur.stop_sequences.is_empty() {
        out["stop_sequences"] = json!(ur.stop_sequences);
    }
    if let Some(metadata) = &ur.metadata {
        out["metadata"] = metadata.clone();
    }
    if let Some(reasoning) = &ur.reasoning {
        if let Some(budget) = reasoning.budget_tokens {
            out["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
        }
    }
    if !ur.tools.is_empty() {
        let tools: Vec<JsonValue> = ur
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                })
            })
            .collect();
        out["tools"] = json!(tools);
        if let Some(tc) = &ur.tool_choice {
            let choice = match tc {
                ToolChoice::Auto => json!({ "type": "auto" }),
                ToolChoice::Required => json!({ "type": "any" }),
                ToolChoice::None_ => json!({ "type": "auto" }), // Anthropic 无 none，退化为 auto
                ToolChoice::Tool { name } => json!({ "type": "tool", "name": name }),
            };
            out["tool_choice"] = choice;
        }
    }
    out
}

/// UR → Google Gemini generateContent 原生体
pub fn universal_to_gemini(ur: &UniversalRequest) -> JsonValue {
    let mut contents = Vec::<JsonValue>::new();
    // Gemini 以函数名关联 functionCall 与 functionResponse：出站前扫描全部消息，
    // 建立 call_id → 函数名 映射，ToolResult 的 functionResponse.name 取其真实函数名。
    // 无匹配（跨协议孤儿结果）时回退 call_id，保住 gemini→gemini 的同名关联语义。
    let mut call_id_to_name = std::collections::HashMap::new();
    for msg in &ur.messages {
        for part in &msg.parts {
            if let PartKind::ToolUse { call_id, name, .. } = &part.kind {
                call_id_to_name.insert(call_id.clone(), name.clone());
            }
        }
    }
    for msg in &ur.messages {
        let role = match msg.role {
            Role::Assistant => "model",
            Role::User => "user",
        };
        let mut parts = Vec::<JsonValue>::new();
        for part in &msg.parts {
            match &part.kind {
                PartKind::Text { text } => parts.push(json!({ "text": text })),
                PartKind::Thinking { text, .. } => {
                    parts.push(json!({ "text": text, "thought": true }));
                }
                PartKind::Image(ImageSource::Base64 { media_type, data }) => {
                    parts.push(json!({ "inlineData": { "mimeType": media_type, "data": data } }));
                }
                PartKind::Image(ImageSource::Url(url)) => {
                    parts.push(json!({ "fileData": { "fileUri": url } }));
                }
                PartKind::ToolUse { call_id, name, input } => {
                    let _ = call_id; // Gemini 以函数名关联
                    parts.push(json!({ "functionCall": { "name": name, "args": input } }));
                }
                PartKind::ToolResult { call_id, content, .. } => {
                    let response =
                        serde_json::from_str::<JsonValue>(content).unwrap_or_else(|_| json!({ "result": content }));
                    let name = call_id_to_name.get(call_id).cloned().unwrap_or_else(|| call_id.clone());
                    parts.push(json!({ "functionResponse": { "name": name, "response": response } }));
                }
                PartKind::Unsupported { hint } => parts.push(json!({ "text": hint })),
            }
        }
        if parts.is_empty() {
            continue;
        }
        contents.push(json!({ "role": role, "parts": parts }));
    }

    let mut generation_config = json!({});
    if let Some(v) = ur.temperature {
        generation_config["temperature"] = json!(v);
    }
    if let Some(v) = ur.top_p {
        generation_config["topP"] = json!(v);
    }
    if let Some(v) = ur.top_k {
        generation_config["topK"] = json!(v);
    }
    if let Some(v) = ur.max_tokens {
        generation_config["maxOutputTokens"] = json!(v);
    }
    if !ur.stop_sequences.is_empty() {
        generation_config["stopSequences"] = json!(ur.stop_sequences);
    }
    if let Some(reasoning) = &ur.reasoning {
        if let Some(budget) = reasoning.budget_tokens {
            generation_config["thinkingConfig"] = json!({ "thinkingBudget": budget });
        }
    }

    let mut out = json!({
        "contents": contents,
        "generationConfig": generation_config,
    });
    if !ur.system.is_empty() {
        let texts: Vec<JsonValue> = ur
            .system
            .iter()
            .filter_map(|p| match &p.kind {
                PartKind::Text { text } => Some(json!({ "text": text })),
                _ => None,
            })
            .collect();
        if !texts.is_empty() {
            out["systemInstruction"] = json!({ "parts": texts });
        }
    }
    if !ur.tools.is_empty() {
        let decls: Vec<JsonValue> = ur
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                })
            })
            .collect();
        out["tools"] = json!([{ "functionDeclarations": decls }]);
        if let Some(tc) = &ur.tool_choice {
            let (mode, allowed): (&str, Option<Vec<String>>) = match tc {
                ToolChoice::Auto => ("AUTO", None),
                ToolChoice::Required => ("ANY", None),
                ToolChoice::None_ => ("NONE", None),
                ToolChoice::Tool { name } => ("ANY", Some(vec![name.clone()])),
            };
            let mut config = json!({ "mode": mode });
            if let Some(names) = allowed {
                config["allowedFunctionNames"] = json!(names);
            }
            out["toolConfig"] = json!({ "functionCallingConfig": config });
        }
    }
    out
}

/// UR → OpenAI Responses 原生体
pub fn universal_to_responses(ur: &UniversalRequest) -> JsonValue {
    let mut input = Vec::<JsonValue>::new();
    for msg in &ur.messages {
        // 文本部件合并为 message item；工具事件各自成 item
        let mut texts = Vec::<String>::new();
        for part in &msg.parts {
            match &part.kind {
                PartKind::Text { text } => texts.push(text.clone()),
                PartKind::Image(ImageSource::Url(url)) => {
                    input.push(json!({
                        "type": "message",
                        "role": msg.role.as_str(),
                        "content": [{ "type": "input_image", "image_url": url }],
                    }));
                }
                PartKind::Image(ImageSource::Base64 { media_type, data }) => {
                    input.push(json!({
                        "type": "message",
                        "role": msg.role.as_str(),
                        "content": [{
                            "type": "input_image",
                            "image_url": format!("data:{media_type};base64,{data}"),
                        }],
                    }));
                }
                PartKind::ToolUse { call_id, name, input: args } => {
                    if !texts.is_empty() {
                        input.push(json!({
                            "type": "message",
                            "role": msg.role.as_str(),
                            "content": texts.join("\n"),
                        }));
                        texts.clear();
                    }
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call_id,
                        "name": name,
                        "arguments": args.to_string(),
                    }));
                }
                PartKind::ToolResult { call_id, content, .. } => {
                    if !texts.is_empty() {
                        input.push(json!({
                            "type": "message",
                            "role": msg.role.as_str(),
                            "content": texts.join("\n"),
                        }));
                        texts.clear();
                    }
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": content,
                    }));
                }
                PartKind::Thinking { .. } => {}
                PartKind::Unsupported { hint } => texts.push(hint.clone()),
            }
        }
        if !texts.is_empty() {
            input.push(json!({
                "type": "message",
                "role": msg.role.as_str(),
                "content": texts.join("\n"),
            }));
        }
    }

    let instructions = parts_to_text(&ur.system);
    let mut out = json!({
        "model": ur.model,
        "stream": ur.stream,
        "input": input,
    });
    if !instructions.is_empty() {
        out["instructions"] = json!(instructions);
    }
    if let Some(v) = ur.max_tokens {
        out["max_output_tokens"] = json!(v);
    }
    if let Some(v) = ur.temperature {
        out["temperature"] = json!(v);
    }
    if let Some(v) = ur.top_p {
        out["top_p"] = json!(v);
    }
    if let Some(reasoning) = &ur.reasoning {
        if let Some(effort) = &reasoning.effort {
            out["reasoning"] = json!({ "effort": effort });
        }
    }
    if !ur.tools.is_empty() {
        let tools: Vec<JsonValue> = ur
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                })
            })
            .collect();
        out["tools"] = json!(tools);
        if let Some(tc) = &ur.tool_choice {
            let choice = match tc {
                ToolChoice::Auto => json!("auto"),
                ToolChoice::None_ => json!("none"),
                ToolChoice::Required => json!("required"),
                ToolChoice::Tool { name } => json!({ "type": "function", "name": name }),
            };
            out["tool_choice"] = choice;
        }
    }
    out
}



#[cfg(test)]
mod parser_tests {
    use super::*;

    #[test]
    fn chat_parser_splits_inline_think_tags() {
        let mut p = crate::model::gateway::parsers::ChatParser::new(Vec::new(), None);
        let events = p.feed(
            r#"{"choices":[{"delta":{"content":"<think>推理中</think>正式回答"}}]}"#,
        );
        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[0],
            UniversalStreamEvent::ReasoningDelta(t) if t == "推理中"
        ));
        assert!(matches!(
            &events[1],
            UniversalStreamEvent::TextDelta(t) if t == "正式回答"
        ));
        // 无 think 标签时保持原样
        let events = p.feed(r#"{"choices":[{"delta":{"content":"普通文本"}}]}"#);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], UniversalStreamEvent::TextDelta(t) if t == "普通文本"));
    }

    // ---------------------------------------------------------- 请求侧 IR

    #[test]
    fn anthropic_entry_preserves_cache_control_and_signature() {
        let body = json!({
            "model": "claude-x",
            "max_tokens": 100,
            "system": [
                { "type": "text", "text": "base context",
                  "cache_control": { "type": "ephemeral" } }
            ],
            "messages": [
                { "role": "assistant", "content": [
                    { "type": "thinking", "thinking": "step by step",
                      "signature": "sig-abc" },
                    { "type": "text", "text": "answer" }
                ]},
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "call_1",
                      "content": "result text", "is_error": true,
                      "cache_control": { "type": "ephemeral" } }
                ]}
            ]
        });
        let ur = crate::model::gateway::parsers::anthropic_to_universal(&body, "claude-x");
        // system cache_control 保真
        assert!(ur.system[0].cache_control.is_some(), "system 的缓存标记不可丢失");
        // thinking signature 保真
        match &ur.messages[0].parts[0].kind {
            crate::model::gateway::ir::PartKind::Thinking { signature, .. } => {
                assert_eq!(signature.as_deref(), Some("sig-abc"), "signature 是思考链连续性凭证");
            }
            other => panic!("expect thinking, got {other:?}"),
        }
        // tool_result is_error 保真
        match &ur.messages[1].parts[0].kind {
            crate::model::gateway::ir::PartKind::ToolResult { is_error, .. } => {
                assert!(*is_error);
            }
            other => panic!("expect tool_result, got {other:?}"),
        }

        // 往返：UR → Anthropic 原生体，独占元素必须原样回归
        let native = crate::model::gateway::parsers::universal_to_anthropic(&ur);
        assert_eq!(
            native.pointer("/system/0/cache_control/type").and_then(JsonValue::as_str),
            Some("ephemeral"),
            "system 缓存标记必须回填: {native}"
        );
        assert_eq!(
            native.pointer("/messages/0/content/0/signature").and_then(JsonValue::as_str),
            Some("sig-abc")
        );
        assert_eq!(
            native.pointer("/messages/1/content/0/is_error").and_then(JsonValue::as_bool),
            Some(true)
        );
    }

    #[test]
    fn chat_entry_cross_protocol_reaches_anthropic_native() {
        // 端到端：Chat 客户端 → Anthropic 渠道（跨协议经 IR）
        let chat_body = json!({
            "model": "any",
            "messages": [
                { "role": "system", "content": "be brief" },
                { "role": "user", "content": "hi" }
            ],
            "max_tokens": 777
        });
        let ur = crate::model::gateway::parsers::chat_to_universal(&chat_body, "target-model");
        assert_eq!(ur.system.len(), 1);
        let native = crate::model::gateway::parsers::universal_to_anthropic(&ur);
        assert_eq!(native["model"], "target-model", "模型别名必须在 UR 中替换");
        assert_eq!(native["max_tokens"], 777);
        assert_eq!(
            native.pointer("/system/0/text").and_then(JsonValue::as_str),
            Some("be brief")
        );
        assert_eq!(
            native.pointer("/messages/0/content/0/text").and_then(JsonValue::as_str),
            Some("hi")
        );
        // Chat 口径 max_tokens 缺省时 Anthropic 必填兜底
        let mut ur2 = ur.clone();
        ur2.max_tokens = None;
        let native2 = crate::model::gateway::parsers::universal_to_anthropic(&ur2);
        assert!(native2.get("max_tokens").and_then(JsonValue::as_u64).unwrap_or(0) > 0);
    }

    #[test]
    fn responses_entry_tool_roundtrip_via_ir() {
        use crate::model::gateway::ir::{PartKind, Role};
        let body = json!({
            "model": "gpt-x",
            "instructions": "use tools",
            "input": [
                { "type": "message", "role": "user",
                  "content": [{ "type": "input_text", "text": "run it" }] },
                { "type": "function_call", "call_id": "c1", "name": "bash",
                  "arguments": "{\"cmd\":\"ls\"}" },
                { "type": "function_call_output", "call_id": "c1", "output": "files..." }
            ]
        });
        let ur = crate::model::gateway::parsers::responses_to_universal(&body, "gpt-x");
        assert_eq!(ur.messages.len(), 3);
        assert!(matches!(&ur.messages[0].parts[0].kind, PartKind::Text { text } if text == "run it"));
        match &ur.messages[1].parts[0].kind {
            PartKind::ToolUse { call_id, name, input } => {
                assert_eq!(call_id, "c1");
                assert_eq!(name, "bash");
                assert_eq!(input.pointer("/cmd").and_then(JsonValue::as_str), Some("ls"));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(&ur.messages[2].parts[0].kind,
            PartKind::ToolResult { content, .. } if content == "files..."));

        // UR → Chat 出口：instructions 成为 system 首条，工具事件转为 role:tool
        let chat = crate::model::gateway::parsers::universal_to_chat(&ur);
        assert_eq!(chat.pointer("/messages/0/role").and_then(JsonValue::as_str), Some("system"));
        assert_eq!(
            chat.pointer("/messages/2/tool_calls/0/id").and_then(JsonValue::as_str),
            Some("c1")
        );
        assert_eq!(chat.pointer("/messages/3/role").and_then(JsonValue::as_str), Some("tool"));
    }

    #[test]
    fn gemini_entry_maps_system_and_thinking_budget() {
        use crate::model::gateway::ir::Role;
        let body = json!({
            "contents": [
                { "role": "user", "parts": [{ "text": "hello" }] },
                { "role": "model", "parts": [{ "text": "hi there" }] }
            ],
            "systemInstruction": { "parts": [{ "text": "gem sys" }] },
            "generationConfig": {
                "temperature": 0.3,
                "maxOutputTokens": 900,
                "thinkingConfig": { "thinkingBudget": 512 }
            }
        });
        let ur = crate::model::gateway::parsers::gemini_to_universal(&body, "gem-2");
        assert_eq!(ur.system.len(), 1);
        assert_eq!(ur.temperature, Some(0.3));
        assert_eq!(ur.max_tokens, Some(900));
        assert_eq!(ur.reasoning.as_ref().unwrap().budget_tokens, Some(512));
        assert_eq!(ur.messages[0].role, Role::User);

        // UR → Chat 出口：thinking budget 映射为 reasoning_effort 不适用，应静默降级
        let chat = crate::model::gateway::parsers::universal_to_chat(&ur);
        assert!(chat.get("reasoning_effort").is_none() || chat["reasoning_effort"].is_string());
        assert_eq!(chat["temperature"], 0.3);
        assert_eq!(chat["max_tokens"], 900);
    }




    #[test]
    fn gemini_entry_stream_flag_survives_cross_protocol_serialization() {
        // 回归：Gemini body 无 stream 字段，跨协议流式时 UR 必须携带
        // handler 显式回填的标志，否则目标上游收到非流式请求
        let body = json!({
            "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }]
        });
        let mut ur = crate::model::gateway::parsers::gemini_to_universal(&body, "m");
        assert!(!ur.stream, "body 内无 stream 字段，解析默认为 false");
        ur.stream = true; // handler 回填 is_stream
        let chat_body = crate::model::gateway::parsers::universal_to_chat(&ur);
        assert_eq!(chat_body["stream"], true, "跨协议序列化不得丢失流式标志");
        let anthropic_body = crate::model::gateway::parsers::universal_to_anthropic(&ur);
        assert_eq!(anthropic_body["stream"], true);
        let responses_body = crate::model::gateway::parsers::universal_to_responses(&ur);
        assert_eq!(responses_body["stream"], true);
    }

    // ---------------------------------------------------------- 修复回归

    #[test]
    fn chat_round_trip_preserves_parallel_tool_calls_and_extra() {
        // P0-4：chat→chat 不得丢失白名单外字段与 parallel_tool_calls
        let body = json!({
            "model": "gpt-x",
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }],
            "tools": [{ "type": "function", "function": { "name": "f", "parameters": { "type": "object" } } }],
            "parallel_tool_calls": false,
            "service_tier": "flex",
            "modalities": ["text", "audio"]
        });
        let ur = crate::model::gateway::parsers::chat_to_universal(&body, "gpt-x");
        let out = crate::model::gateway::parsers::universal_to_chat(&ur);
        assert_eq!(out["parallel_tool_calls"], false, "parallel_tool_calls 不得被抹掉");
        assert_eq!(out["service_tier"], "flex");
        assert_eq!(out["modalities"][0], "text");
    }

    #[test]
    fn chat_extra_not_leaked_to_other_protocols() {
        // P0-4：跨协议出站只回填白名单，chat 专属 extra 不泄漏到其他协议
        let body = json!({
            "model": "gpt-x",
            "messages": [{ "role": "user", "content": "hi" }],
            "service_tier": "flex",
            "frequency_penalty": 0.5
        });
        let ur = crate::model::gateway::parsers::chat_to_universal(&body, "gpt-x");
        assert!(crate::model::gateway::parsers::universal_to_anthropic(&ur)
            .get("service_tier").is_none());
        assert!(crate::model::gateway::parsers::universal_to_gemini(&ur)
            .get("service_tier").is_none());
        assert!(crate::model::gateway::parsers::universal_to_responses(&ur)
            .get("service_tier").is_none());
        // 白名单键（frequency_penalty）在 chat 出口保留
        let chat_body = crate::model::gateway::parsers::universal_to_chat(&ur);
        assert_eq!(chat_body["frequency_penalty"], 0.5);
    }

    #[test]
    fn gemini_tool_result_links_to_function_call_call_id() {
        // P0-2 入站：functionResponse 的 ToolResult.call_id 关联到同名 functionCall 的合成 call_id
        let body = json!({
            "contents": [
                { "role": "model", "parts": [
                    { "functionCall": { "name": "search", "args": { "q": "x" } } }
                ]},
                { "role": "user", "parts": [
                    { "functionResponse": { "name": "search", "response": { "ok": true } } }
                ]}
            ]
        });
        let ur = crate::model::gateway::parsers::gemini_to_universal(&body, "gemini-x");
        let tool_result = ur
            .messages
            .iter()
            .flat_map(|m| &m.parts)
            .find(|p| matches!(p.kind, PartKind::ToolResult { .. }))
            .expect("应有 ToolResult");
        let call_id = match &tool_result.kind {
            PartKind::ToolResult { call_id, .. } => call_id.clone(),
            _ => unreachable!(),
        };
        assert_eq!(call_id, "gem_call_0", "应关联到同名 functionCall 的合成 call_id");
        // 出站到 chat：ToolUse 与 ToolResult 的 call_id 一致，客户端能对上工具结果
        let chat_body = crate::model::gateway::parsers::universal_to_chat(&ur);
        assert!(chat_body.to_string().contains("gem_call_0"));
    }

    #[test]
    fn chat_tool_result_uses_function_name_for_gemini() {
        // P0-2 出站：chat 的 tool_call_id 不得当 Gemini 函数名，
        // functionResponse.name 必须用真实函数名（Gemini 以函数名关联）
        let body = json!({
            "model": "gpt-x",
            "messages": [
                { "role": "assistant", "content": null,
                  "tool_calls": [{ "id": "call_abc", "type": "function",
                    "function": { "name": "search", "arguments": "{\"q\":\"x\"}" } }] },
                { "role": "tool", "tool_call_id": "call_abc", "content": "ok" }
            ]
        });
        let ur = crate::model::gateway::parsers::chat_to_universal(&body, "gpt-x");
        let gemini_body = crate::model::gateway::parsers::universal_to_gemini(&ur);
        let s = gemini_body.to_string();
        assert!(s.contains("\"name\":\"search\""), "functionResponse.name 应为函数名: {s}");
        assert!(!s.contains("call_abc"), "不得把 tool_call_id 当函数名: {s}");
    }

    #[test]
    fn anthropic_serializer_drops_unsigned_thinking_keeps_signed() {
        // P1-1：无 signature 的 thinking 块（chat <think>/gemini thought 来源）
        // 不得回写 Anthropic 上游；带 signature 的（anthropic 源）必须保留
        let mut ur = UniversalRequest::new("claude-x");
        ur.messages.push(UniversalMessage {
            role: Role::Assistant,
            parts: vec![
                ContentPart {
                    kind: PartKind::Thinking { text: "unsigned".into(), signature: None },
                    cache_control: None,
                },
                ContentPart {
                    kind: PartKind::Thinking {
                        text: "signed".into(),
                        signature: Some("sig-1".into()),
                    },
                    cache_control: None,
                },
            ],
        });
        let body = crate::model::gateway::parsers::universal_to_anthropic(&ur);
        let s = body.to_string();
        assert!(!s.contains("unsigned"), "无签名思考块应被丢弃，避免上游 400");
        assert!(s.contains("sig-1") && s.contains("signed"), "带签名思考块应保留");
    }

    #[test]
    fn chat_parser_recovers_orphan_tool_name_via_hints() {
        // P1-4：首片缺 name 的工具调用缓冲到 finish，经 hints 恢复名字而非永久占位
        let mut p = ChatParser::new(vec![("search".to_string(), vec!["q".to_string()])], None);
        let events = p.feed(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"arguments":"{\"q\":"}}]}}]}"#,
        );
        assert!(events.is_empty(), "缺 name 的首片应缓冲不发 Start");
        let events = p.feed(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"x\"}"}}]}}]}"#,
        );
        assert!(events.is_empty(), "续片仍应缓冲");
        let events = p.finish();
        assert!(events.iter().any(|e| matches!(e, UniversalStreamEvent::ToolCallStart { name, .. } if name == "search")));
        assert!(events.iter().any(|e| matches!(e, UniversalStreamEvent::ToolCallDelta { fragment, .. } if fragment.contains("\"q\""))));
    }

    #[test]
    fn chat_parser_counts_dropped_frames() {
        // P1-3：坏帧不再静默吞掉，计数可观测且 finish 仍产出终帧
        let mut p = ChatParser::new(Vec::new(), None);
        assert!(p.feed("not-json").is_empty());
        assert_eq!(p.feed(r#"{"choices":[{"delta":{"content":"hi"}}]}"#).len(), 1);
        assert_eq!(p.dropped_frames(), 1);
        let events = p.finish();
        assert!(events.iter().any(|e| matches!(e, UniversalStreamEvent::Finish { .. })));
    }
}
