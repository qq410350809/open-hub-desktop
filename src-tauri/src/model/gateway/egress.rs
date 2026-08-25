//! 出网协议适配层：实现 4×4 协议矩阵的上游半边。
//!
//! 网关内部统一以 OpenAI Chat Completions 格式为中枢：
//! - 客户端侧（adapters.rs）：任意协议入口 → OpenAI 中枢格式
//! - 出网侧（本模块）：OpenAI 中枢格式 → 渠道配置的目标协议请求上游，
//!   并把上游响应（JSON 或 SSE 流）归一化回 OpenAI 中枢格式
//!
//! 两者组合即可覆盖「客户端任意协议 × 上游任意协议」的完整转换矩阵。

use super::types::ChannelConfig;
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value as JsonValue};

/// 渠道上游目标协议
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetProtocol {
    /// OpenAI · Chat Completions（默认，历史遗留值 opencode 也归入此类）
    OpenAiChat,
    /// OpenAI · Responses API
    OpenAiResponses,
    /// Claude · Anthropic Messages
    AnthropicMessages,
    /// Google · Gemini generateContent
    Gemini,
}

impl TargetProtocol {
    /// 从渠道配置解析目标协议；未知/历史遗留值一律回退为 OpenAI 兼容
    pub fn from_channel(channel: &ChannelConfig) -> Self {
        Self::from_str(&channel.protocol)
    }

    pub fn from_str(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            "openai-responses" | "responses" => Self::OpenAiResponses,
            "anthropic" | "claude" => Self::AnthropicMessages,
            "gemini" => Self::Gemini,
            _ => Self::OpenAiChat,
        }
    }

    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAiChat => "OpenAI Chat",
            Self::OpenAiResponses => "OpenAI Responses",
            Self::AnthropicMessages => "Anthropic Messages",
            Self::Gemini => "Gemini",
        }
    }
}

/// 根据渠道目标协议，把内部 OpenAI 中枢请求转换为出网 URL 与请求体。
///
/// 返回 `(upstream_url, egress_body)`；OpenAI 协议为原样透传。
#[allow(dead_code)]
pub fn prepare_egress(
    channel: &ChannelConfig,
    api_key: &str,
    model: &str,
    openai_body: &JsonValue,
    is_stream: bool,
) -> (String, JsonValue) {
    prepare_egress_with(channel, api_key, model, openai_body, is_stream, true)
}

/// 为 OpenAI Chat 流式出网注入 `stream_options.include_usage`。
///
/// 历史缺陷警示：OpenAI 兼容上游仅在收到该参数时才会在流尾回传 usage chunk，
/// 缺失它意味着网关与客户端永远拿不到 token 统计（表现为「0 0」）。
/// 用 entry/or_insert 尊重调用方显式传入的设置。
fn ensure_include_usage(body: &mut JsonValue, is_stream: bool) {
    if !is_stream {
        return;
    }
    if let Some(obj) = body.as_object_mut() {
        obj.entry("stream_options".to_string())
            .or_insert_with(|| json!({ "include_usage": true }));
    }
}

/// `prepare_egress` 的完整版本：`convert = false` 表示 body 已是目标协议原生格式
/// （同协议快速通道），只构建 URL、跳过请求体转换。
pub fn prepare_egress_with(
    channel: &ChannelConfig,
    api_key: &str,
    model: &str,
    body: &JsonValue,
    is_stream: bool,
    convert: bool,
) -> (String, JsonValue) {
    let base = channel.base_url.trim_end_matches('/');
    let target = TargetProtocol::from_channel(channel);

    // Gemini 原生快速通道（同协议透传）保留特殊 URL 格式
    if matches!(target, TargetProtocol::Gemini) && !convert {
        let action = if is_stream {
            "streamGenerateContent?alt=sse"
        } else {
            "generateContent"
        };
        let mut url = format!("{base}/v1beta/models/{model}:{action}");
        if !api_key.trim().is_empty() {
            url.push(if url.contains('?') { '&' } else { '?' });
            url.push_str("key=");
            url.push_str(api_key.trim());
        }
        return (url, body.clone());
    }

    // 所有渠道统一出口：OpenAI Chat 格式 + /v1/chat/completions 路径
    // 不管入口协议（chat/messages/responses/gemini），发往上游的请求统一为 Chat 格式。
    // 仅 Gemini 原生透传（上方分支）保留特殊路径。
    let url = format!("{base}/chat/completions");
    let mut out = body.clone();
    ensure_include_usage(&mut out, is_stream);
    (url, out)
}

// ---------------------------------------------------------------------------
// 请求体转换：OpenAI Chat → 目标协议
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn message_text(content: &JsonValue) -> String {
    match content {
        JsonValue::String(s) => s.clone(),
        JsonValue::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(JsonValue::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// OpenAI Chat 请求体 → Anthropic Messages 请求体
#[allow(dead_code)]
pub fn chat_to_anthropic_body(openai_body: &JsonValue, model: &str, stream: bool) -> JsonValue {
    let mut system_parts: Vec<String> = Vec::new();
    let mut messages: Vec<JsonValue> = Vec::new();

    if let Some(msgs) = openai_body.get("messages").and_then(JsonValue::as_array) {
        for m in msgs {
            let role = m.get("role").and_then(JsonValue::as_str).unwrap_or("user");
            match role {
                "system" | "developer" => {
                    let text = message_text(m.get("content").unwrap_or(&JsonValue::Null));
                    if !text.trim().is_empty() {
                        system_parts.push(text);
                    }
                }
                "tool" => {
                    // 工具结果 → user 消息中的 tool_result 块
                    messages.push(json!({
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": m.get("tool_call_id").cloned().unwrap_or(JsonValue::Null),
                            "content": m.get("content").cloned().unwrap_or(json!("")),
                        }]
                    }));
                }
                "assistant" => {
                    let mut blocks: Vec<JsonValue> = Vec::new();
                    let text = message_text(m.get("content").unwrap_or(&JsonValue::Null));
                    if !text.is_empty() {
                        blocks.push(json!({ "type": "text", "text": text }));
                    }
                    if let Some(tcs) = m.get("tool_calls").and_then(JsonValue::as_array) {
                        for tc in tcs {
                            let args = tc
                                .pointer("/function/arguments")
                                .cloned()
                                .unwrap_or(json!("{}"));
                            let input = if let Some(s) = args.as_str() {
                                serde_json::from_str::<JsonValue>(s).unwrap_or(json!({}))
                            } else {
                                args
                            };
                            blocks.push(json!({
                                "type": "tool_use",
                                "id": tc.get("id").cloned().unwrap_or(json!("toolu_unknown")),
                                "name": tc.pointer("/function/name").cloned().unwrap_or(json!("tool")),
                                "input": input,
                            }));
                        }
                    }
                    if blocks.is_empty() {
                        continue;
                    }
                    messages.push(json!({ "role": "assistant", "content": blocks }));
                }
                _ => {
                    messages.push(json!({
                        "role": "user",
                        "content": m.get("content").cloned().unwrap_or(json!("")),
                    }));
                }
            }
        }
    }

    let mut body = json!({
        "model": model,
        "max_tokens": openai_body.get("max_tokens").and_then(JsonValue::as_u64).filter(|v| *v > 0).unwrap_or(4096),
        "stream": stream,
        "messages": messages,
    });
    if !system_parts.is_empty() {
        body["system"] = json!(system_parts.join("\n\n"));
    }
    for (from, to) in [("temperature", "temperature"), ("top_p", "top_p")] {
        if let Some(v) = openai_body.get(from) {
            if v.is_number() {
                body[to] = v.clone();
            }
        }
    }
    if let Some(stop) = openai_body.get("stop") {
        match stop {
            JsonValue::String(s) => body["stop_sequences"] = json!([s]),
            JsonValue::Array(arr) if !arr.is_empty() => body["stop_sequences"] = stop.clone(),
            _ => {}
        }
    }
    if let Some(tools) = openai_body.get("tools").and_then(JsonValue::as_array) {
        let mapped: Vec<JsonValue> = tools
            .iter()
            .filter_map(|t| {
                let name = t.pointer("/function/name").and_then(JsonValue::as_str)?;
                Some(json!({
                    "name": name,
                    "description": t.pointer("/function/description").cloned().unwrap_or(json!("")),
                    "input_schema": t.pointer("/function/parameters").cloned().unwrap_or(json!({"type": "object"})),
                }))
            })
            .collect();
        if !mapped.is_empty() {
            body["tools"] = json!(mapped);
        }
    }
    body
}

/// OpenAI Chat 请求体 → Gemini generateContent 请求体
#[allow(dead_code)]
pub fn chat_to_gemini_body(openai_body: &JsonValue) -> JsonValue {
    let mut contents: Vec<JsonValue> = Vec::new();
    let mut system_instruction: Option<JsonValue> = None;

    if let Some(msgs) = openai_body.get("messages").and_then(JsonValue::as_array) {
        for m in msgs {
            let role = m.get("role").and_then(JsonValue::as_str).unwrap_or("user");
            let text = message_text(m.get("content").unwrap_or(&JsonValue::Null));
            match role {
                "system" | "developer" => {
                    if !text.trim().is_empty() {
                        system_instruction = Some(json!({ "parts": [{ "text": text }] }));
                    }
                }
                "assistant" => {
                    let mut parts: Vec<JsonValue> = Vec::new();
                    if !text.is_empty() {
                        parts.push(json!({ "text": text }));
                    }
                    if let Some(tcs) = m.get("tool_calls").and_then(JsonValue::as_array) {
                        for tc in tcs {
                            let args = tc
                                .pointer("/function/arguments")
                                .cloned()
                                .unwrap_or(json!("{}"));
                            let args_val = if let Some(s) = args.as_str() {
                                serde_json::from_str::<JsonValue>(s).unwrap_or(json!({}))
                            } else {
                                args
                            };
                            parts.push(json!({
                                "functionCall": {
                                    "name": tc.pointer("/function/name").cloned().unwrap_or(json!("tool")),
                                    "args": args_val,
                                }
                            }));
                        }
                    }
                    if !parts.is_empty() {
                        contents.push(json!({ "role": "model", "parts": parts }));
                    }
                }
                "tool" => {
                    contents.push(json!({
                        "role": "user",
                        "parts": [{
                            "functionResponse": {
                                "name": m.get("tool_call_id").and_then(JsonValue::as_str).unwrap_or("tool"),
                                "response": { "result": m.get("content").cloned().unwrap_or(json!("")) },
                            }
                        }]
                    }));
                }
                _ => {
                    if !text.is_empty() {
                        contents.push(json!({ "role": "user", "parts": [{ "text": text }] }));
                    }
                }
            }
        }
    }

    let mut generation_config = json!({});
    if let Some(v) = openai_body
        .get("max_tokens")
        .and_then(JsonValue::as_u64)
        .filter(|v| *v > 0)
    {
        generation_config["maxOutputTokens"] = json!(v);
    }
    for (from, to) in [("temperature", "temperature"), ("top_p", "topP")] {
        if let Some(v) = openai_body.get(from) {
            if v.is_number() {
                generation_config[to] = v.clone();
            }
        }
    }
    if let Some(stop) = openai_body.get("stop") {
        match stop {
            JsonValue::String(s) => generation_config["stopSequences"] = json!([s]),
            JsonValue::Array(arr) if !arr.is_empty() => {
                generation_config["stopSequences"] = stop.clone()
            }
            _ => {}
        }
    }

    let mut body = json!({ "contents": contents, "generationConfig": generation_config });
    if let Some(si) = system_instruction {
        body["systemInstruction"] = si;
    }
    if let Some(tools) = openai_body.get("tools").and_then(JsonValue::as_array) {
        let decls: Vec<JsonValue> = tools
            .iter()
            .filter_map(|t| {
                let name = t.pointer("/function/name").and_then(JsonValue::as_str)?;
                Some(json!({
                    "name": name,
                    "description": t.pointer("/function/description").cloned().unwrap_or(json!("")),
                    "parameters": t.pointer("/function/parameters").cloned().unwrap_or(json!({"type": "object"})),
                }))
            })
            .collect();
        if !decls.is_empty() {
            body["tools"] = json!([{ "functionDeclarations": decls }]);
        }
    }
    body
}

/// OpenAI Chat 请求体 → OpenAI Responses API 请求体
#[allow(dead_code)]
pub fn chat_to_responses_body(openai_body: &JsonValue, model: &str, stream: bool) -> JsonValue {
    let mut instructions = String::new();
    let mut input: Vec<JsonValue> = Vec::new();

    if let Some(msgs) = openai_body.get("messages").and_then(JsonValue::as_array) {
        for m in msgs {
            let role = m.get("role").and_then(JsonValue::as_str).unwrap_or("user");
            let text = message_text(m.get("content").unwrap_or(&JsonValue::Null));
            match role {
                "system" | "developer" => {
                    if !text.trim().is_empty() {
                        if !instructions.is_empty() {
                            instructions.push('\n');
                        }
                        instructions.push_str(&text);
                    }
                }
                "assistant" => {
                    if !text.is_empty() {
                        input.push(json!({
                            "type": "message",
                            "role": "assistant",
                            "content": [{ "type": "output_text", "text": text }],
                        }));
                    }
                    if let Some(tcs) = m.get("tool_calls").and_then(JsonValue::as_array) {
                        for tc in tcs {
                            let args = tc
                                .pointer("/function/arguments")
                                .cloned()
                                .unwrap_or(json!("{}"));
                            input.push(json!({
                                "type": "function_call",
                                "call_id": tc.get("id").cloned().unwrap_or(json!("call_unknown")),
                                "name": tc.pointer("/function/name").cloned().unwrap_or(json!("tool")),
                                "arguments": if args.is_string() { args } else { json!(args.to_string()) },
                            }));
                        }
                    }
                }
                "tool" => {
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": m.get("tool_call_id").cloned().unwrap_or(json!("call_unknown")),
                        "output": m.get("content").cloned().unwrap_or(json!("")),
                    }));
                }
                _ => {
                    if !text.is_empty() {
                        input.push(json!({
                            "type": "message",
                            "role": "user",
                            "content": [{ "type": "input_text", "text": text }],
                        }));
                    }
                }
            }
        }
    }

    let mut body = json!({ "model": model, "stream": stream, "input": input });
    if !instructions.is_empty() {
        body["instructions"] = json!(instructions);
    }
    if let Some(v) = openai_body
        .get("max_tokens")
        .and_then(JsonValue::as_u64)
        .filter(|v| *v > 0)
    {
        body["max_output_tokens"] = json!(v);
    }
    if let Some(v) = openai_body.get("temperature") {
        if v.is_number() {
            body["temperature"] = v.clone();
        }
    }
    body
}

// ---------------------------------------------------------------------------
// 非流式响应归一化：目标协议 JSON → OpenAI Chat 响应
// ---------------------------------------------------------------------------

/// 把上游非流式响应字节归一化为 OpenAI Chat 格式；解析失败时原样透传（便于错误排查）
pub fn normalize_response_bytes(target: TargetProtocol, model: &str, raw: &[u8]) -> Vec<u8> {
    if matches!(target, TargetProtocol::OpenAiChat) {
        return raw.to_vec();
    }
    let Ok(jv) = serde_json::from_slice::<JsonValue>(raw) else {
        return raw.to_vec();
    };
    let converted = match target {
        TargetProtocol::AnthropicMessages => anthropic_response_to_openai(&jv),
        TargetProtocol::Gemini => gemini_response_to_openai(&jv, model),
        TargetProtocol::OpenAiResponses => responses_response_to_openai(&jv),
        TargetProtocol::OpenAiChat => unreachable!(),
    };
    serde_json::to_vec(&converted).unwrap_or_else(|_| raw.to_vec())
}

fn empty_chat_response(model: &str) -> JsonValue {
    json!({
        "choices": [{ "index": 0, "message": { "role": "assistant", "content": "" }, "finish_reason": null }],
        "model": model,
    })
}

/// Anthropic Messages 响应 → OpenAI Chat 响应
pub fn anthropic_response_to_openai(resp: &JsonValue) -> JsonValue {
    let model = resp.get("model").and_then(JsonValue::as_str).unwrap_or("");
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<JsonValue> = Vec::new();

    if let Some(blocks) = resp.get("content").and_then(JsonValue::as_array) {
        for b in blocks {
            match b.get("type").and_then(JsonValue::as_str) {
                Some("text") => {
                    if let Some(t) = b.get("text").and_then(JsonValue::as_str) {
                        content.push_str(t);
                    }
                }
                Some("thinking") => {
                    if let Some(t) = b.get("thinking").and_then(JsonValue::as_str) {
                        reasoning.push_str(t);
                    }
                }
                Some("tool_use") => {
                    tool_calls.push(json!({
                        "id": b.get("id").cloned().unwrap_or(json!("toolu_unknown")),
                        "type": "function",
                        "function": {
                            "name": b.get("name").cloned().unwrap_or(json!("tool")),
                            "arguments": json!(b.get("input").map(|i| i.to_string()).unwrap_or_default()),
                        }
                    }));
                }
                _ => {}
            }
        }
    }

    let finish_reason = match resp.get("stop_reason").and_then(JsonValue::as_str) {
        Some("max_tokens") => "length",
        Some("tool_use") => "tool_calls",
        Some(_) | None => "stop",
    };

    // Anthropic 口径：input_tokens 不含缓存部分；归一化为 OpenAI 口径时
    // prompt_tokens = input + cache_read + cache_creation，缓存明细放 details。
    // cache_creation_tokens 为本网关的扩展键（OpenAI 协议无此概念），供统计层提取。
    let usage = resp.get("usage");
    let input_tokens = usage
        .and_then(|u| u.get("input_tokens"))
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    let cache_read = usage
        .and_then(|u| u.get("cache_read_input_tokens"))
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    let cache_creation = usage
        .and_then(|u| u.get("cache_creation_input_tokens"))
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    let completion_tokens = usage
        .and_then(|u| u.get("output_tokens"))
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    let prompt_tokens = input_tokens + cache_read + cache_creation;

    let mut out = empty_chat_response(model);
    let msg = out
        .pointer_mut("/choices/0/message")
        .expect("message exists");
    msg["content"] = json!(content);
    if !reasoning.is_empty() {
        msg["reasoning_content"] = json!(reasoning);
    }
    if !tool_calls.is_empty() {
        msg["tool_calls"] = json!(tool_calls);
    }
    out["choices"][0]["finish_reason"] = json!(finish_reason);
    out["usage"] = json!({
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": prompt_tokens + completion_tokens,
        "prompt_tokens_details": {
            "cached_tokens": cache_read,
            "cache_creation_tokens": cache_creation,
        },
    });
    out
}

/// Gemini generateContent 响应 → OpenAI Chat 响应
pub fn gemini_response_to_openai(resp: &JsonValue, model: &str) -> JsonValue {
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<JsonValue> = Vec::new();
    let mut finish_reason = "stop";

    if let Some(candidates) = resp.get("candidates").and_then(JsonValue::as_array) {
        if let Some(c0) = candidates.first() {
            if let Some(parts) = c0.pointer("/content/parts").and_then(JsonValue::as_array) {
                for p in parts {
                    let is_thought = p
                        .get("thought")
                        .and_then(JsonValue::as_bool)
                        .unwrap_or(false);
                    if let Some(t) = p.get("text").and_then(JsonValue::as_str) {
                        if is_thought {
                            reasoning.push_str(t);
                        } else {
                            content.push_str(t);
                        }
                    }
                    if let Some(fc) = p.get("functionCall") {
                        tool_calls.push(json!({
                            "id": format!("call_{}", tool_calls.len()),
                            "type": "function",
                            "function": {
                                "name": fc.get("name").cloned().unwrap_or(json!("tool")),
                                "arguments": json!(fc.get("args").map(|a| a.to_string()).unwrap_or_default()),
                            }
                        }));
                    }
                }
            }
            finish_reason = match c0.get("finishReason").and_then(JsonValue::as_str) {
                Some("MAX_TOKENS") => "length",
                Some("SAFETY") | Some("RECITATION") => "content_filter",
                _ => "stop",
            };
        }
    }

    let usage = resp.get("usageMetadata");
    let prompt_tokens = usage
        .and_then(|u| u.get("promptTokenCount"))
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    let thoughts_tokens = usage
        .and_then(|u| u.get("thoughtsTokenCount"))
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    let cached_tokens = usage
        .and_then(|u| u.get("cachedContentTokenCount"))
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    let completion_tokens = usage
        .and_then(|u| u.get("candidatesTokenCount"))
        .and_then(JsonValue::as_u64)
        .unwrap_or(0)
        + thoughts_tokens;

    let mut out = empty_chat_response(model);
    let msg = out
        .pointer_mut("/choices/0/message")
        .expect("message exists");
    msg["content"] = json!(content);
    if !reasoning.is_empty() {
        msg["reasoning_content"] = json!(reasoning);
    }
    if !tool_calls.is_empty() {
        msg["tool_calls"] = json!(tool_calls);
    }
    out["choices"][0]["finish_reason"] = json!(finish_reason);
    out["usage"] = json!({
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": usage.and_then(|u| u.get("totalTokenCount")).and_then(JsonValue::as_u64).unwrap_or(prompt_tokens + completion_tokens),
        "prompt_tokens_details": { "cached_tokens": cached_tokens },
        "completion_tokens_details": { "reasoning_tokens": thoughts_tokens },
    });
    out
}

/// OpenAI Responses API 响应 → OpenAI Chat 响应
pub fn responses_response_to_openai(resp: &JsonValue) -> JsonValue {
    let model = resp.get("model").and_then(JsonValue::as_str).unwrap_or("");
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<JsonValue> = Vec::new();

    if let Some(output) = resp.get("output").and_then(JsonValue::as_array) {
        for item in output {
            match item.get("type").and_then(JsonValue::as_str) {
                Some("message") => {
                    if let Some(parts) = item.get("content").and_then(JsonValue::as_array) {
                        for p in parts {
                            if let Some(t) = p.get("text").and_then(JsonValue::as_str) {
                                content.push_str(t);
                            }
                        }
                    }
                }
                Some("reasoning") => {
                    if let Some(summaries) = item.get("summary").and_then(JsonValue::as_array) {
                        for s in summaries {
                            if let Some(t) = s.get("text").and_then(JsonValue::as_str) {
                                reasoning.push_str(t);
                            }
                        }
                    }
                }
                Some("function_call") => {
                    tool_calls.push(json!({
                        "id": item.get("call_id").or_else(|| item.get("id")).cloned().unwrap_or(json!("call_unknown")),
                        "type": "function",
                        "function": {
                            "name": item.get("name").cloned().unwrap_or(json!("tool")),
                            "arguments": item.get("arguments").cloned().unwrap_or(json!("{}")),
                        }
                    }));
                }
                _ => {}
            }
        }
    }

    let usage = resp.get("usage");
    let prompt_tokens = usage
        .and_then(|u| u.get("input_tokens"))
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    let completion_tokens = usage
        .and_then(|u| u.get("output_tokens"))
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    let cached_tokens = usage
        .and_then(|u| u.pointer("/input_tokens_details/cached_tokens"))
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    let reasoning_tokens = usage
        .and_then(|u| u.pointer("/output_tokens_details/reasoning_tokens"))
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);

    let mut out = empty_chat_response(model);
    let msg = out
        .pointer_mut("/choices/0/message")
        .expect("message exists");
    msg["content"] = json!(content);
    if !reasoning.is_empty() {
        msg["reasoning_content"] = json!(reasoning);
    }
    if !tool_calls.is_empty() {
        msg["tool_calls"] = json!(tool_calls);
    }
    out["choices"][0]["finish_reason"] = json!("stop");
    out["usage"] = json!({
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": prompt_tokens + completion_tokens,
        "prompt_tokens_details": { "cached_tokens": cached_tokens },
        "completion_tokens_details": { "reasoning_tokens": reasoning_tokens },
    });
    out
}

// ---------------------------------------------------------------------------
// 流式响应归一化：目标协议 SSE → OpenAI Chat SSE
// ---------------------------------------------------------------------------

/// 单个发往客户端的 OpenAI delta 分片（不含 data: 前缀）
fn delta_chunk(delta: JsonValue, finish_reason: Option<&str>, usage: Option<JsonValue>) -> String {
    let mut chunk = json!({
        "id": "chatcmpl-egress",
        "object": "chat.completion.chunk",
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish_reason.map(JsonValue::from),
        }]
    });
    if let Some(u) = usage {
        chunk["usage"] = u;
    }
    format!("data: {}\n\n", chunk)
}

#[derive(Default)]
struct AnthropicSseState {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
    /// 块索引 → ("text" | "thinking" | "tool_use")
    block_kinds: std::collections::HashMap<u64, String>,
    /// 工具块索引 → (id, name)
    tool_meta: std::collections::HashMap<u64, (String, String)>,
    /// 请求 tools 的参数键线索：上游缺失 content_block_start 时按 args 匹配恢复工具名
    tool_hints: Vec<super::stream::ToolHint>,
    /// 已下发过首片的工具块索引：保证 id/name 只注入一次
    emitted_tools: std::collections::HashSet<u64>,
    preferred_tool: Option<String>,
    synth_seq: u64,
    stop_reason: Option<String>,
    /// 孤儿 tool_use 块缓冲：上游缺失 content_block_start 时暂存参数增量，
    /// 直到工具名可被可靠恢复（参数键命中/tool_choice/终态帧）才一次性下发，
    /// 避免首片 args 过短时误判为占位名导致客户端「Tool not found」
    pending_orphan: Option<PendingOrphanTool>,
}

/// 孤儿 tool_use 块的缓冲态
struct PendingOrphanTool {
    idx: u64,
    args: String,
    buffered: Vec<JsonValue>,
}

impl AnthropicSseState {
    /// 孤儿块的最终工具名决策：tool_choice 指定 > 参数键命中；None 表示仍无法识别
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

    fn feed(&mut self, jv: &JsonValue) -> Vec<String> {
        let mut out = Vec::new();

        // 兼容性：部分上游（如 new-api 系）tool_use 场景缺失 content_block_start 帧。
        // 孤儿参数增量先缓冲，工具名可被可靠恢复（参数键命中 / tool_choice 指定 /
        // 终态帧强制收口）后才合成元数据并一次性下发全部增量 —— 首片往往只有
        // `{"` 这样的短碎片，立即匹配必然失败并以占位名下发导致「Tool not found」。
        if jv.get("type").and_then(JsonValue::as_str) == Some("content_block_delta")
            && jv.pointer("/delta/type").and_then(JsonValue::as_str) == Some("input_json_delta")
        {
            let idx = jv.get("index").and_then(JsonValue::as_u64).unwrap_or(0);
            let orphan = !self.block_kinds.contains_key(&idx) && !self.tool_meta.contains_key(&idx);
            if orphan || self.pending_orphan.as_ref().map(|p| p.idx) == Some(idx) {
                let pt = self.pending_orphan.get_or_insert(PendingOrphanTool {
                    idx,
                    args: String::new(),
                    buffered: Vec::new(),
                });
                pt.args.push_str(jv.pointer("/delta/partial_json").and_then(JsonValue::as_str).unwrap_or(""));
                pt.buffered.push(jv.clone());

                let hit = self.preferred_tool.is_some()
                    || self.tool_hints.iter().any(|(_, keys)| {
                        keys.iter().any(|k| pt.args.contains(k.as_str()))
                    });
                if !hit {
                    return Vec::new(); // 继续缓冲，等待更多参数或终态帧
                }
                // 命中：冲刷缓冲，按标准增量协议输出（首片带 id/name）
                let args = pt.args.clone();
                let idx = pt.idx;
                let name = self
                    .resolve_orphan_name(&args)
                    .unwrap_or_else(|| format!("unknown_tool_{}", idx + 1));
                let id = format!("toolu_synth_{}", idx + 1);
                self.tool_meta.insert(idx, (id.clone(), name.clone()));
                self.block_kinds.insert(idx, "tool_use".to_string());
                let Some(pt) = self.pending_orphan.as_mut() else {
                    unreachable!("pending 已确认存在");
                };
                for (i, bjv) in pt.buffered.drain(..).enumerate() {
                    let mut tc = json!({ "index": idx });
                    if i == 0 {
                        tc["id"] = json!(id);
                        tc["type"] = json!("function");
                        tc["function"]["name"] = json!(name);
                    }
                    tc["function"]["arguments"] =
                        json!(bjv.pointer("/delta/partial_json").and_then(JsonValue::as_str).unwrap_or(""));
                    out.push(delta_chunk(json!({ "tool_calls": [tc] }), None, None));
                }
                self.pending_orphan = None;
                return out;
            }
        }

        match jv.get("type").and_then(JsonValue::as_str) {
            Some("message_start") => {
                self.input_tokens = jv
                    .pointer("/message/usage/input_tokens")
                    .and_then(JsonValue::as_u64)
                    .unwrap_or(0);
                self.cache_read_tokens = jv
                    .pointer("/message/usage/cache_read_input_tokens")
                    .and_then(JsonValue::as_u64)
                    .unwrap_or(0);
                self.cache_creation_tokens = jv
                    .pointer("/message/usage/cache_creation_input_tokens")
                    .and_then(JsonValue::as_u64)
                    .unwrap_or(0);
            }
            Some("content_block_start") => {
                let idx = jv.get("index").and_then(JsonValue::as_u64).unwrap_or(0);
                let kind = jv
                    .pointer("/content_block/type")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("text")
                    .to_string();
                if kind == "tool_use" {
                    let id = jv
                        .pointer("/content_block/id")
                        .and_then(JsonValue::as_str)
                        .unwrap_or("toolu_unknown");
                    let name = jv
                        .pointer("/content_block/name")
                        .and_then(JsonValue::as_str)
                        .unwrap_or("tool");
                    self.tool_meta
                        .insert(idx, (id.to_string(), name.to_string()));
                }
                self.block_kinds.insert(idx, kind);
            }
            Some("content_block_delta") => {
                let idx = jv.get("index").and_then(JsonValue::as_u64).unwrap_or(0);
                match self.block_kinds.get(&idx).map(String::as_str) {
                    Some("thinking") => {
                        if let Some(t) = jv.pointer("/delta/thinking").and_then(JsonValue::as_str) {
                            out.push(delta_chunk(json!({ "reasoning_content": t }), None, None));
                        }
                    }
                    Some("tool_use") => {
                        if let Some(frag) = jv
                            .pointer("/delta/partial_json")
                            .and_then(JsonValue::as_str)
                        {
                            // 首个可见分片必须携带 id 与 function.name，
                            // 否则标准客户端（opencode/zcode 等）校验直接失败
                            let first_visible = !self.emitted_tools.contains(&idx);
                            let mut tc = json!({ "index": idx });
                            if first_visible {
                                let (id, name) = self
                                    .tool_meta
                                    .get(&idx)
                                    .cloned()
                                    .unwrap_or(("toolu_unknown".into(), "unknown_tool".into()));
                                tc["id"] = json!(id);
                                tc["type"] = json!("function");
                                tc["function"]["name"] = json!(name);
                                self.emitted_tools.insert(idx);
                            }
                            tc["function"]["arguments"] = json!(frag);
                            out.push(delta_chunk(
                                json!({ "tool_calls": [tc] }),
                                None,
                                None,
                            ));
                        }
                    }
                    _ => {
                        if let Some(t) = jv.pointer("/delta/text").and_then(JsonValue::as_str) {
                            out.push(delta_chunk(json!({ "content": t }), None, None));
                        }
                    }
                }
            }
            // 终态帧强制收口：孤儿缓冲中仍无法识别名字时以占位下发（客户端会自愈重试）
            _ if self.pending_orphan.is_some()
                && matches!(
                    jv.get("type").and_then(JsonValue::as_str),
                    Some("content_block_stop") | Some("message_delta") | Some("message_stop")
                ) =>
            {
                let (idx, args, buffered) = {
                    let pt = self.pending_orphan.as_ref().unwrap();
                    (pt.idx, pt.args.clone(), pt.buffered.clone())
                };
                let name = self.resolve_orphan_name(&args).unwrap_or_else(|| {
                    self.synth_seq += 1;
                    format!("unknown_tool_{}", self.synth_seq)
                });
                let id = format!("toolu_synth_{}", idx + 1);
                self.tool_meta.insert(idx, (id.clone(), name.clone()));
                self.block_kinds.insert(idx, "tool_use".to_string());
                for (i, bjv) in buffered.iter().enumerate() {
                    let mut tc = json!({ "index": idx });
                    if i == 0 {
                        tc["id"] = json!(id);
                        tc["type"] = json!("function");
                        tc["function"]["name"] = json!(name);
                    }
                    tc["function"]["arguments"] =
                        json!(bjv.pointer("/delta/partial_json").and_then(JsonValue::as_str).unwrap_or(""));
                    out.push(delta_chunk(json!({ "tool_calls": [tc] }), None, None));
                }
                self.pending_orphan = None;
            }
            Some("message_delta") => {
                if let Some(r) = jv.pointer("/delta/stop_reason").and_then(JsonValue::as_str) {
                    self.stop_reason = Some(r.to_string());
                }
                if let Some(o) = jv
                    .pointer("/usage/output_tokens")
                    .and_then(JsonValue::as_u64)
                {
                    self.output_tokens = o;
                }
                // 部分上游在 message_delta 里才给出（或更新）缓存计数
                if let Some(v) = jv
                    .pointer("/usage/cache_read_input_tokens")
                    .and_then(JsonValue::as_u64)
                {
                    self.cache_read_tokens = self.cache_read_tokens.max(v);
                }
                if let Some(v) = jv
                    .pointer("/usage/cache_creation_input_tokens")
                    .and_then(JsonValue::as_u64)
                {
                    self.cache_creation_tokens = self.cache_creation_tokens.max(v);
                }
            }
            _ => {}
        }
        out
    }

    fn finish(&self) -> Vec<String> {
        let finish_reason = match self.stop_reason.as_deref() {
            Some("max_tokens") => "length",
            Some("tool_use") => "tool_calls",
            _ => "stop",
        };
        // 与非流式归一化同口径：prompt_tokens 含缓存读+写，明细放 details
        let prompt_tokens = self.input_tokens + self.cache_read_tokens + self.cache_creation_tokens;
        vec![
            delta_chunk(
                json!({}),
                Some(finish_reason),
                Some(json!({
                    "prompt_tokens": prompt_tokens,
                    "completion_tokens": self.output_tokens,
                    "total_tokens": prompt_tokens + self.output_tokens,
                    "prompt_tokens_details": {
                        "cached_tokens": self.cache_read_tokens,
                        "cache_creation_tokens": self.cache_creation_tokens,
                    },
                })),
            ),
            "data: [DONE]\n\n".to_string(),
        ]
    }
}

#[derive(Default)]
struct GeminiSseState {
    prompt_tokens: u64,
    completion_tokens: u64,
    reasoning_tokens: u64,
    cached_tokens: u64,
    total_tokens: u64,
    finish_reason: Option<String>,
}

impl GeminiSseState {
    fn feed(&mut self, jv: &JsonValue) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(candidates) = jv.get("candidates").and_then(JsonValue::as_array) {
            if let Some(c0) = candidates.first() {
                if let Some(parts) = c0.pointer("/content/parts").and_then(JsonValue::as_array) {
                    for p in parts {
                        let is_thought = p
                            .get("thought")
                            .and_then(JsonValue::as_bool)
                            .unwrap_or(false);
                        if let Some(t) = p.get("text").and_then(JsonValue::as_str) {
                            let key = if is_thought {
                                "reasoning_content"
                            } else {
                                "content"
                            };
                            out.push(delta_chunk(json!({ key: t }), None, None));
                        }
                        if let Some(fc) = p.get("functionCall") {
                            out.push(delta_chunk(
                                json!({ "tool_calls": [{
                                    "index": 0,
                                    "function": {
                                        "name": fc.get("name").cloned().unwrap_or(json!("tool")),
                                        "arguments": json!(fc.get("args").map(|a| a.to_string()).unwrap_or_default()),
                                    }
                                }] }),
                                None,
                                None,
                            ));
                        }
                    }
                }
                if let Some(fr) = c0.get("finishReason").and_then(JsonValue::as_str) {
                    self.finish_reason = Some(fr.to_string());
                }
            }
        }
        if let Some(u) = jv.get("usageMetadata") {
            self.prompt_tokens = u
                .get("promptTokenCount")
                .and_then(JsonValue::as_u64)
                .unwrap_or(self.prompt_tokens);
            let thoughts = u
                .get("thoughtsTokenCount")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0);
            self.reasoning_tokens = self.reasoning_tokens.max(thoughts);
            self.completion_tokens = u
                .get("candidatesTokenCount")
                .and_then(JsonValue::as_u64)
                .unwrap_or(self.completion_tokens)
                + thoughts;
            self.cached_tokens = u
                .get("cachedContentTokenCount")
                .and_then(JsonValue::as_u64)
                .unwrap_or(self.cached_tokens);
            self.total_tokens = u
                .get("totalTokenCount")
                .and_then(JsonValue::as_u64)
                .unwrap_or(self.total_tokens);
        }
        out
    }

    fn finish(&self) -> Vec<String> {
        let finish_reason = match self.finish_reason.as_deref() {
            Some("MAX_TOKENS") => "length",
            Some("SAFETY") | Some("RECITATION") => "content_filter",
            _ => "stop",
        };
        vec![
            delta_chunk(
                json!({}),
                Some(finish_reason),
                Some(json!({
                    "prompt_tokens": self.prompt_tokens,
                    "completion_tokens": self.completion_tokens,
                    "total_tokens": if self.total_tokens > 0 { self.total_tokens } else { self.prompt_tokens + self.completion_tokens },
                    "prompt_tokens_details": { "cached_tokens": self.cached_tokens },
                    "completion_tokens_details": { "reasoning_tokens": self.reasoning_tokens },
                })),
            ),
            "data: [DONE]\n\n".to_string(),
        ]
    }
}

#[derive(Default)]
struct ResponsesSseState {
    prompt_tokens: u64,
    completion_tokens: u64,
    cached_tokens: u64,
    reasoning_tokens: u64,
    tool_count: u64,
}

impl ResponsesSseState {
    fn feed(&mut self, jv: &JsonValue) -> Vec<String> {
        let mut out = Vec::new();
        match jv.get("type").and_then(JsonValue::as_str) {
            Some("response.output_text.delta") => {
                if let Some(t) = jv.get("delta").and_then(JsonValue::as_str) {
                    out.push(delta_chunk(json!({ "content": t }), None, None));
                }
            }
            Some("response.reasoning_summary_text.delta") => {
                if let Some(t) = jv.get("delta").and_then(JsonValue::as_str) {
                    out.push(delta_chunk(json!({ "reasoning_content": t }), None, None));
                }
            }
            Some("response.output_item.done") => {
                if jv.pointer("/item/type").and_then(JsonValue::as_str) == Some("function_call") {
                    let idx = self.tool_count;
                    self.tool_count += 1;
                    out.push(delta_chunk(
                        json!({ "tool_calls": [{
                            "index": idx,
                            "id": jv.pointer("/item/call_id").or_else(|| jv.pointer("/item/id")).cloned().unwrap_or(json!("call_unknown")),
                            "function": {
                                "name": jv.pointer("/item/name").cloned().unwrap_or(json!("tool")),
                                "arguments": jv.pointer("/item/arguments").cloned().unwrap_or(json!("{}")),
                            }
                        }] }),
                        None,
                        None,
                    ));
                }
            }
            Some("response.completed") | Some("response.incomplete") => {
                self.prompt_tokens = jv
                    .pointer("/response/usage/input_tokens")
                    .and_then(JsonValue::as_u64)
                    .unwrap_or(self.prompt_tokens);
                self.completion_tokens = jv
                    .pointer("/response/usage/output_tokens")
                    .and_then(JsonValue::as_u64)
                    .unwrap_or(self.completion_tokens);
                self.cached_tokens = jv
                    .pointer("/response/usage/input_tokens_details/cached_tokens")
                    .and_then(JsonValue::as_u64)
                    .unwrap_or(self.cached_tokens);
                self.reasoning_tokens = jv
                    .pointer("/response/usage/output_tokens_details/reasoning_tokens")
                    .and_then(JsonValue::as_u64)
                    .unwrap_or(self.reasoning_tokens);
            }
            _ => {}
        }
        out
    }

    fn finish(&self) -> Vec<String> {
        vec![
            delta_chunk(
                json!({}),
                Some("stop"),
                Some(json!({
                    "prompt_tokens": self.prompt_tokens,
                    "completion_tokens": self.completion_tokens,
                    "total_tokens": self.prompt_tokens + self.completion_tokens,
                    "prompt_tokens_details": { "cached_tokens": self.cached_tokens },
                    "completion_tokens_details": { "reasoning_tokens": self.reasoning_tokens },
                })),
            ),
            "data: [DONE]\n\n".to_string(),
        ]
    }
}

enum SseNormalizer {
    Anthropic(AnthropicSseState),
    Gemini(GeminiSseState),
    Responses(ResponsesSseState),
}

impl SseNormalizer {
    fn new(
        target: TargetProtocol,
        tool_hints: Vec<super::stream::ToolHint>,
        preferred_tool: Option<String>,
    ) -> Self {
        match target {
            TargetProtocol::AnthropicMessages => Self::Anthropic(AnthropicSseState {
                tool_hints,
                preferred_tool,
                ..AnthropicSseState::default()
            }),
            TargetProtocol::Gemini => Self::Gemini(GeminiSseState::default()),
            _ => Self::Responses(ResponsesSseState::default()),
        }
    }

    fn feed(&mut self, data: &str) -> Vec<String> {
        let Ok(jv) = serde_json::from_str::<JsonValue>(data) else {
            return Vec::new();
        };
        match self {
            Self::Anthropic(s) => s.feed(&jv),
            Self::Gemini(s) => s.feed(&jv),
            Self::Responses(s) => s.feed(&jv),
        }
    }

    fn finish(self) -> Vec<String> {
        match self {
            Self::Anthropic(s) => s.finish(),
            Self::Gemini(s) => s.finish(),
            Self::Responses(s) => s.finish(),
        }
    }
}

/// 把上游目标协议的 SSE 字节流归一化为 OpenAI Chat SSE 字节流。
/// OpenAI Chat 上游本就是中枢格式：零转换透传原始字节，避免误吞 delta 与 usage。
pub fn normalized_sse_stream<E, S>(
    stream: S,
    target: TargetProtocol,
    tool_hints: Vec<super::stream::ToolHint>,
    preferred_tool: Option<String>,
) -> impl futures_util::Stream<Item = Result<Bytes, E>> + Send
where
    S: futures_util::Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: Send + 'static,
{
    type BoxedStream<E> = std::pin::Pin<
        Box<dyn futures_util::Stream<Item = Result<Bytes, E>> + Send>,
    >;

    // 历史缺陷警示：此前 OpenAI Chat 上游也会走 SseNormalizer（回退成 Responses 解析器），
    // 导致所有 delta 因缺少 "type" 字段而被静默丢弃 —— 客户端只见 200 空响应 + 全 0 usage。
    if matches!(target, TargetProtocol::OpenAiChat) {
        let passthrough: BoxedStream<E> = Box::pin(async_stream::stream! {
            tokio::pin!(stream);
            while let Some(item) = stream.next().await {
                yield item;
            }
        });
        return passthrough;
    }

    let mut normalizer = SseNormalizer::new(target, tool_hints, preferred_tool);
    let converted: BoxedStream<E> = Box::pin(async_stream::stream! {
        let mut reader = super::stream::SseLineReader::new();
        tokio::pin!(stream);

        while let Some(chunk_res) = stream.next().await {
            match chunk_res {
                Ok(bytes) => {
                    for line in reader.push(&bytes) {
                        let Some(data) = line.strip_prefix("data:") else { continue };
                        let data = data.trim();
                        if data.is_empty() || data == "[DONE]" {
                            continue;
                        }
                        for payload in normalizer.feed(data) {
                            yield Ok::<Bytes, E>(Bytes::from(payload));
                        }
                    }
                }
                Err(err) => {
                    yield Err(err);
                    return;
                }
            }
        }

        if let Some(line) = reader.flush() {
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if !data.is_empty() && data != "[DONE]" {
                    for payload in normalizer.feed(data) {
                        yield Ok::<Bytes, E>(Bytes::from(payload));
                    }
                }
            }
        }

        for payload in normalizer.finish() {
            yield Ok::<Bytes, E>(Bytes::from(payload));
        }
    });
    converted
}

#[cfg(test)]
mod egress_tests {
    use super::*;

    fn channel_with_protocol(protocol: &str) -> ChannelConfig {
        ChannelConfig {
            id: "test".to_string(),
            name: "Test".to_string(),
            description: String::new(),
            enabled: true,
            protocol: protocol.to_string(),
            base_url: "https://upstream.example/v1".to_string(),
            api_key: String::new(),
            api_keys: None,
            use_proxy_pool: false,
            alias: None,
            site_id: None,
            use_fixed_proxy: false,
            fixed_proxy_node: None,
            priority: None,
            weight: None,
            enabled_models: None,
            model_redirects: None,
            rate_limit_rpm: None,
            stats_id: None,
        }
    }

    #[test]
    fn target_protocol_from_channel_maps_legacy_values() {
        assert_eq!(
            TargetProtocol::from_channel(&channel_with_protocol("openai")),
            TargetProtocol::OpenAiChat
        );
        assert_eq!(
            TargetProtocol::from_channel(&channel_with_protocol("opencode")),
            TargetProtocol::OpenAiChat
        );
        assert_eq!(
            TargetProtocol::from_channel(&channel_with_protocol("responses")),
            TargetProtocol::OpenAiResponses
        );
        assert_eq!(
            TargetProtocol::from_channel(&channel_with_protocol("claude")),
            TargetProtocol::AnthropicMessages
        );
        assert_eq!(
            TargetProtocol::from_channel(&channel_with_protocol("gemini")),
            TargetProtocol::Gemini
        );
        assert_eq!(
            TargetProtocol::from_channel(&channel_with_protocol("unknown")),
            TargetProtocol::OpenAiChat
        );
    }

    #[test]
    fn prepare_egress_builds_url_per_protocol() {
        let body = json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});

        // 统一出网规则：openai / openai-responses / anthropic 默认均转换成 /chat/completions 路径
        let (url, _) = prepare_egress(&channel_with_protocol("openai"), "sk-x", "m", &body, false);
        assert_eq!(url, "https://upstream.example/v1/chat/completions");

        let (url, _) = prepare_egress(
            &channel_with_protocol("openai-responses"),
            "sk-x",
            "m",
            &body,
            false,
        );
        assert_eq!(url, "https://upstream.example/v1/chat/completions");

        let (url, _) = prepare_egress(
            &channel_with_protocol("anthropic"),
            "sk-x",
            "m",
            &body,
            true,
        );
        assert_eq!(
            url,
            "https://upstream.example/v1/chat/completions",
            "上游统一默认转成 /chat/completions 出口"
        );

        // Gemini 原生快速通道（convert=false）保留原生 URL
        let (url, _) = prepare_egress_with(
            &channel_with_protocol("gemini"),
            "sk-123",
            "gemini-2.5-pro",
            &body,
            false,
            false,
        );
        assert_eq!(
            url,
            "https://upstream.example/v1/v1beta/models/gemini-2.5-pro:generateContent?key=sk-123"
        );

        let (url, _) = prepare_egress_with(
            &channel_with_protocol("gemini"),
            "",
            "g",
            &body,
            true,
            false,
        );
        assert_eq!(
            url,
            "https://upstream.example/v1/v1beta/models/g:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn chat_to_anthropic_body_maps_system_tools_and_defaults() {
        let body = json!({
            "model": "m",
            "max_tokens": 0,
            "temperature": 0.7,
            "messages": [
                {"role": "system", "content": "你是助手"},
                {"role": "user", "content": "你好"},
            ],
            "tools": [{"type": "function", "function": {"name": "get_weather", "description": "查天气", "parameters": {"type": "object"}}}],
        });
        let out = chat_to_anthropic_body(&body, "claude-4", true);
        assert_eq!(out["system"], json!("你是助手"));
        assert_eq!(out["max_tokens"], json!(4096));
        assert_eq!(out["model"], json!("claude-4"));
        assert_eq!(out["stream"], json!(true));
        assert_eq!(out["messages"][0]["role"], json!("user"));
        assert_eq!(out["tools"][0]["name"], json!("get_weather"));
        assert!(out["tools"][0]["input_schema"].is_object());
    }

    #[test]
    fn anthropic_response_to_openai_extracts_blocks_and_usage() {
        let resp = json!({
            "model": "claude-4",
            "content": [
                {"type": "thinking", "thinking": "让我想想"},
                {"type": "text", "text": "答案是 42"},
                {"type": "tool_use", "id": "toolu_1", "name": "search", "input": {"q": "hi"}},
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 100, "output_tokens": 50},
        });
        let out = anthropic_response_to_openai(&resp);
        assert_eq!(
            out.pointer("/choices/0/message/content").unwrap(),
            "答案是 42"
        );
        assert_eq!(
            out.pointer("/choices/0/message/reasoning_content").unwrap(),
            "让我想想"
        );
        assert_eq!(
            out.pointer("/choices/0/finish_reason").unwrap(),
            "tool_calls"
        );
        assert_eq!(out.pointer("/usage/prompt_tokens").unwrap(), 100);
        assert_eq!(out.pointer("/usage/completion_tokens").unwrap(), 50);
        let tc = out.pointer("/choices/0/message/tool_calls/0").unwrap();
        assert_eq!(tc["function"]["name"], json!("search"));
        assert_eq!(tc["function"]["arguments"], json!(r#"{"q":"hi"}"#));
    }

    #[test]
    fn gemini_response_roundtrip_keeps_text_and_thought() {
        let chat = json!({
            "model": "m",
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "hi"},
            ],
            "max_tokens": 128,
        });
        let gemini_req = chat_to_gemini_body(&chat);
        assert_eq!(
            gemini_req
                .pointer("/systemInstruction/parts/0/text")
                .unwrap(),
            "sys"
        );
        assert_eq!(gemini_req.pointer("/contents/0/role").unwrap(), "user");
        assert_eq!(
            gemini_req
                .pointer("/generationConfig/maxOutputTokens")
                .unwrap(),
            128
        );

        let gemini_resp = json!({
            "candidates": [{
                "content": {"parts": [
                    {"text": "思考中", "thought": true},
                    {"text": "你好！"},
                    {"functionCall": {"name": "search", "args": {"q": 1}}},
                ]},
                "finishReason": "STOP",
            }],
            "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 5, "totalTokenCount": 15},
        });
        let out = gemini_response_to_openai(&gemini_resp, "gemini-2.5");
        assert_eq!(out.pointer("/choices/0/message/content").unwrap(), "你好！");
        assert_eq!(
            out.pointer("/choices/0/message/reasoning_content").unwrap(),
            "思考中"
        );
        assert_eq!(out.pointer("/choices/0/finish_reason").unwrap(), "stop");
        assert_eq!(out.pointer("/usage/total_tokens").unwrap(), 15);
    }

    #[test]
    fn responses_response_to_openai_extracts_output_items() {
        let resp = json!({
            "model": "gpt-x",
            "output": [
                {"type": "reasoning", "summary": [{"type": "summary_text", "text": "推理过程"}]},
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "最终回答"}]},
                {"type": "function_call", "call_id": "call_1", "name": "calc", "arguments": "{\"a\":1}"},
            ],
            "usage": {"input_tokens": 8, "output_tokens": 6},
        });
        let out = responses_response_to_openai(&resp);
        assert_eq!(
            out.pointer("/choices/0/message/content").unwrap(),
            "最终回答"
        );
        assert_eq!(
            out.pointer("/choices/0/message/reasoning_content").unwrap(),
            "推理过程"
        );
        assert_eq!(
            out.pointer("/choices/0/message/tool_calls/0/function/name")
                .unwrap(),
            "calc"
        );
        assert_eq!(out.pointer("/usage/prompt_tokens").unwrap(), 8);
    }

    #[test]
    fn anthropic_sse_sequence_converts_to_openai_chunks() {
        let mut s = AnthropicSseState::default();
        let mut collected = String::new();
        for data in [
            r#"{"type":"message_start","message":{"usage":{"input_tokens":20}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"你"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"好"}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":9}}"#,
        ] {
            for chunk in s.feed(&serde_json::from_str::<JsonValue>(data).unwrap()) {
                collected.push_str(&chunk);
            }
        }
        for chunk in s.finish() {
            collected.push_str(&chunk);
        }

        assert!(
            collected.contains("你好") || (collected.contains("你") && collected.contains("好"))
        );
        assert!(collected.contains("\"prompt_tokens\":20"));
        assert!(collected.contains("\"completion_tokens\":9"));
        assert!(collected.contains("\"finish_reason\":\"stop\""));
        assert!(collected.ends_with("data: [DONE]\n\n"));
    }

    #[test]
    fn normalize_response_bytes_passthrough_on_invalid_json() {
        let raw = b"<html>Bad Gateway</html>".to_vec();
        let out = normalize_response_bytes(TargetProtocol::AnthropicMessages, "m", &raw);
        assert_eq!(out, raw);
    }

    /// 兼容性回归（分片缓冲）：首片 args 只有 `{"` 时不得立即以占位名下发，
    /// 必须延迟到参数键可识别后，一次性输出带真实名字的完整增量序列
    #[tokio::test]
    async fn anthropic_orphan_short_first_fragment_defers_until_name_recoverable() {
        let mk = |partial: &str| {
            let pj = serde_json::to_string(partial).unwrap();
            format!(
                "data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"input_json_delta\",\"partial_json\":{pj}}}}}\n\n"
            )
        };
        // 首片只有 `{` —— 旧实现会立即定名 unknown_tool_1
        let upstream = futures_util::stream::iter(vec![
            Ok::<Bytes, std::io::Error>(Bytes::from(
                "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n"
                    .to_string(),
            )),
            Ok(Bytes::from(mk("{"))),
            Ok(Bytes::from(mk("\"command\": \"ls -la\","))),
            Ok(Bytes::from(mk("\"description\": \"list files\"}"))),
            Ok(Bytes::from("data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":20}}\n\n")),
        ]);
        let hints = vec![
            (
                "bash".to_string(),
                vec!["command".to_string(), "description".to_string()],
            ),
            ("read_file".to_string(), vec!["path".to_string()]),
        ];
        let collected: Vec<Result<Bytes, std::io::Error>> = {
            use futures_util::StreamExt;
            normalized_sse_stream(upstream, TargetProtocol::AnthropicMessages, hints, None)
                .collect::<Vec<_>>()
                .await
        };
        let text: String = collected
            .into_iter()
            .map(|r| String::from_utf8_lossy(&r.unwrap()).to_string())
            .collect();

        assert!(
            !text.contains("unknown_tool"),
            "短碎片不得触发占位名下发: {text}"
        );
        assert!(
            text.contains("\"name\":\"bash\""),
            "应延迟至键名可识别后恢复为 bash: {text}"
        );
        assert!(text.contains("ls -la") && text.contains("list files"));
        assert!(text.matches("tool_calls").count() >= 3, "缓冲的增量应全部补发");
    }

    /// 兼容性回归：Anthropic 上游缺失 content_block_start 时（x666 实测行为），
    /// 归一化层必须合成工具块元数据，首个可见分片携带 id 与 function.name
    #[tokio::test]
    async fn anthropic_sse_without_start_frame_recovers_tool_name() {
        let upstream = futures_util::stream::iter(vec![
            Ok::<Bytes, std::io::Error>(Bytes::from(
                "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n",
            )),
            Ok(Bytes::from(
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"command\\\": \\\"ls\\\"}\"}}\n\n",
            )),
            Ok(Bytes::from("data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":20}}\n\n")),
        ]);
        let hints = vec![
            ("bash".to_string(), vec!["command".to_string()]),
            ("read_file".to_string(), vec!["path".to_string()]),
        ];
        let collected: Vec<Result<Bytes, std::io::Error>> = {
            use futures_util::StreamExt;
            normalized_sse_stream(upstream, TargetProtocol::AnthropicMessages, hints, None)
                .collect::<Vec<_>>()
                .await
        };
        let text: String = collected
            .into_iter()
            .map(|r| String::from_utf8_lossy(&r.unwrap()).to_string())
            .collect();

        assert!(
            text.contains("\"name\":\"bash\""),
            "工具名应按参数键匹配恢复为 bash: {text}"
        );
        assert!(text.contains("\"id\""), "首片必须携带 id: {text}");
        assert!(text.contains("\\\"command"), "参数增量必须透传");
    }

    #[tokio::test]
    async fn openai_chat_sse_target_passes_through_without_swallowing_deltas() {
        // 回归：OpenAI Chat 上游曾被错误送入 Responses 归一化器，
        // 所有 delta 被吞，客户端只见 200 空响应 + 全 0 usage
        let upstream = futures_util::stream::iter(vec![
            Ok::<Bytes, std::io::Error>(Bytes::from(
                "data: {\"choices\":[{\"delta\":{\"content\":\"你\"}}]}\n\n",
            )),
            Ok(Bytes::from("data: {\"choices\":[{\"delta\":{\"content\":\"好\"}}]}\n\n")),
            Ok(Bytes::from(
                "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            )),
            Ok(Bytes::from(
                "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":7,\"total_tokens\":18}}\n\n",
            )),
            Ok(Bytes::from("data: [DONE]\n\n")),
        ]);

        let collected: Vec<Result<Bytes, std::io::Error>> = {
            use futures_util::StreamExt;
            normalized_sse_stream(upstream, TargetProtocol::OpenAiChat, Vec::new(), None)
                .collect::<Vec<_>>()
                .await
        };
        let text: String = collected
            .into_iter()
            .map(|r| String::from_utf8_lossy(&r.unwrap()).to_string())
            .collect();

        assert!(text.contains("\"你好\"") || (text.contains('你') && text.contains('好')));
        assert!(text.contains("\"prompt_tokens\":11"));
        assert!(text.contains("[DONE]"), "[DONE] 必须原样保留");
    }

    #[test]
    fn prepare_egress_injects_include_usage_only_for_openai_chat_stream() {
        let body = json!({"model": "m", "stream": true, "messages": []});

        let (_, out) = prepare_egress(&channel_with_protocol("openai"), "", "m", &body, true);
        assert_eq!(
            out.pointer("/stream_options/include_usage").and_then(JsonValue::as_bool),
            Some(true),
            "流式出网必须请求 usage，否则 token 统计恒为 0"
        );

        // 非流式不注入
        let (_, out) = prepare_egress(&channel_with_protocol("openai"), "", "m", &body, false);
        assert!(out.get("stream_options").is_none());

        // 客户端显式设置时不覆盖
        let custom = json!({"model": "m", "stream": true, "stream_options": {"include_usage": false}});
        let (_, out) = prepare_egress(&channel_with_protocol("openai"), "", "m", &custom, true);
        assert_eq!(
            out.pointer("/stream_options/include_usage").and_then(JsonValue::as_bool),
            Some(false)
        );

        // 统一出网：所有走 /chat/completions 的流式请求均注入 include_usage（Anthropic/Responses 渠道亦受益）
        let (_, out) =
            prepare_egress(&channel_with_protocol("anthropic"), "", "m", &body, true);
        assert_eq!(
            out.pointer("/stream_options/include_usage").and_then(JsonValue::as_bool),
            Some(true)
        );
    }
}
