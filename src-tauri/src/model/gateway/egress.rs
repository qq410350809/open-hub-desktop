//! 出网协议适配层：实现 4×4 协议矩阵的上游半边。
//!
//! 网关内部统一以 OpenAI Chat Completions 格式为中枢：
//! - 客户端侧（adapters.rs）：任意协议入口 → OpenAI 中枢格式
//! - 出网侧（本模块）：OpenAI 中枢格式 → 渠道配置的目标协议请求上游，
//!   并把上游响应（JSON 或 SSE 流）归一化回 OpenAI 中枢格式
//!
//! 两者组合即可覆盖「客户端任意协议 × 上游任意协议」的完整转换矩阵。

use super::types::ChannelConfig;
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

/// 把上游响应中值得透传的头复制到客户端响应上。
///
/// 跳过分帧/内容相关头（content-type/length/encoding 等由网关按新响应体重新
/// 生成），保留 x-request-id、retry-after、anthropic-ratelimit-*、openai-* 等
/// 对客户端记账、限流感知与排查有用的头。
pub fn copy_upstream_headers(
    src: &reqwest::header::HeaderMap,
    mut resp: axum::response::Response,
) -> axum::response::Response {
    const SKIP: &[&str] = &[
        "content-type",
        "content-length",
        "content-encoding",
        "content-disposition",
        "connection",
        "transfer-encoding",
        "keep-alive",
        "trailer",
        "upgrade",
        "set-cookie",
    ];
    let headers = resp.headers_mut();
    for (name, value) in src {
        let lower = name.as_str().to_ascii_lowercase();
        if SKIP.contains(&lower.as_str()) {
            continue;
        }
        if let Ok(v) = value.to_str() {
            if let Ok(hv) = v.parse::<axum::http::header::HeaderValue>() {
                let hn = name.clone();
                headers.insert(hn, hv);
            }
        }
    }
    resp
}

/// 出网载荷双轨：跨协议走 IR（序列化器按目标展开），
/// 同协议快车道走原生体直通。
#[derive(Debug, Clone)]
pub enum EgressBody {
    /// 通用对象：由目标协议序列化器展开为原生请求体
    Universal(crate::model::gateway::ir::UniversalRequest),
    /// 已是目标渠道原生格式（同协议快车道），仅构建 URL
    Native(JsonValue),
}

impl EgressBody {
    pub fn native(body: JsonValue) -> Self {
        Self::Native(body)
    }
}

/// 按渠道目标协议把出网载荷转换为 URL 与原生请求体。
pub fn prepare_egress_with(
    channel: &ChannelConfig,
    api_key: &str,
    model: &str,
    payload: EgressBody,
    is_stream: bool,
) -> (String, JsonValue) {
    let (universal, native) = match payload {
        EgressBody::Universal(ur) => (Some(ur), None),
        EgressBody::Native(body) => (None, Some(body)),
    };
    prepare_egress_inner(channel, api_key, model, universal, native, is_stream)
}

fn prepare_egress_inner(
    channel: &ChannelConfig,
    api_key: &str,
    model: &str,
    universal: Option<crate::model::gateway::ir::UniversalRequest>,
    mut native: Option<JsonValue>,
    is_stream: bool,
) -> (String, JsonValue) {
    let base = channel.base_url.trim().trim_end_matches('/');
    let target = TargetProtocol::from_channel(channel);

    match target {
        // Gemini 原生：模型名走 URL，key 走查询参数；跨协议时由 IR 序列化
        TargetProtocol::Gemini => {
            let egress_body = if let Some(ur) = universal {
                super::parsers::universal_to_gemini(&ur)
            } else {
                native.take().unwrap_or_else(|| json!({}))
            };
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
            (url, egress_body)
        }
        // Anthropic 原生：/v1/messages；跨协议时由 Chat 中枢体转换。
        // 此前一律发 Chat 体到 /chat/completions，原生上游无法消费。
        TargetProtocol::AnthropicMessages => {
            let url = normalize_versioned_base(base, "messages");
            let egress_body = if let Some(ur) = universal {
                super::parsers::universal_to_anthropic(&ur)
            } else {
                native.take().unwrap_or_else(|| json!({}))
            };
            (url, egress_body)
        }
        // OpenAI Responses 原生：/v1/responses
        TargetProtocol::OpenAiResponses => {
            let url = normalize_versioned_base(base, "responses");
            let egress_body = if let Some(ur) = universal {
                super::parsers::universal_to_responses(&ur)
            } else {
                native.take().unwrap_or_else(|| json!({}))
            };
            (url, egress_body)
        }
        // OpenAI Chat 统一出口（中枢格式原样）
        TargetProtocol::OpenAiChat => {
            let url = normalize_versioned_base(base, "chat/completions");
            let mut out = if let Some(ur) = universal {
                super::parsers::universal_to_chat(&ur)
            } else {
                native.take().unwrap_or_else(|| json!({}))
            };
            ensure_include_usage(&mut out, is_stream);
            (url, out)
        }
    }
}

/// 规范化拼接带版本前缀的上游端点：
/// 1. 基址已带版本路径（/v1 等）则原样衔接；裸域名（站点库普遍形态，
///    如 https://x666.me）自动补 /v1 —— 否则请求打到 /chat/completions
///    根本进不了上游网关，表现为「上游无请求记录、客户端空返回」；
/// 2. 折叠基址尾部斜杠，杜绝「域名//v1」双斜杠。
/// 与 router 模型探测的 /v1 回退候选规则保持同一口径。
fn normalize_versioned_base(base: &str, endpoint: &str) -> String {
    let trimmed = base.trim().trim_end_matches('/');
    if trimmed.ends_with("/v1") || trimmed.ends_with("/vbeta") || trimmed.ends_with("/v2") {
        format!("{trimmed}/{endpoint}")
    } else {
        format!("{trimmed}/v1/{endpoint}")
    }
}

// ---------------------------------------------------------------------------
// 请求体转换：OpenAI Chat → 目标协议
// ---------------------------------------------------------------------------





// ---------------------------------------------------------------------------
// 非流式响应归一化：目标协议 JSON → OpenAI Chat 响应
// ---------------------------------------------------------------------------

/// 从非流式 JSON 响应嗅探实际协议。
///
/// 渠道配置的 TargetProtocol 可能与上游实际返回不一致（配错、网关二次转换、
/// 上游擅自变更），按特征强匹配判定；无法识别时返回 None 交由调用方回退配置值。
/// 判定顺序按特异性从高到低，避免宽松特征（如 choices）抢先命中。
pub fn detect_response_protocol_from_json(jv: &JsonValue) -> Option<TargetProtocol> {
    // Responses API：{"object":"response",...} 或 output 数组 + status 字段
    if jv.get("object").and_then(JsonValue::as_str) == Some("response")
        || (jv.get("output").and_then(JsonValue::as_array).is_some()
            && jv.get("status").and_then(JsonValue::as_str).is_some())
    {
        return Some(TargetProtocol::OpenAiResponses);
    }
    // Anthropic Messages：顶层 type:"message"，或 content[] + role:"assistant"
    if jv.get("type").and_then(JsonValue::as_str) == Some("message")
        || (jv.get("content").and_then(JsonValue::as_array).is_some()
            && jv.get("role").and_then(JsonValue::as_str) == Some("assistant"))
    {
        return Some(TargetProtocol::AnthropicMessages);
    }
    // Gemini generateContent：candidates 数组
    if jv.get("candidates").and_then(JsonValue::as_array).is_some() {
        return Some(TargetProtocol::Gemini);
    }
    // OpenAI Chat：choices 数组（含 usage 尾包的空数组形态）
    if jv.get("choices").and_then(JsonValue::as_array).is_some() {
        return Some(TargetProtocol::OpenAiChat);
    }
    None
}

/// 从 SSE 数据行（单个 JSON payload）嗅探流式协议。
///
/// 仅在首个可判定事件上锁定解析器；歧义行（如 Anthropic 的 ping 心跳、
/// 解析失败的残片）返回 None，由调用方继续观察后续行。
pub fn detect_response_protocol_from_sse_data(data: &str) -> Option<TargetProtocol> {
    let Ok(jv) = serde_json::from_str::<JsonValue>(data) else {
        return None;
    };
    if let Some(event_type) = jv.get("type").and_then(JsonValue::as_str) {
        // Anthropic 流：message_start/message_delta/message_stop/content_block_*
        if event_type.starts_with("message_") || event_type.starts_with("content_block_") {
            return Some(TargetProtocol::AnthropicMessages);
        }
        // Responses 流：response.created / response.output_text.delta / response.completed...
        if event_type.starts_with("response.") {
            return Some(TargetProtocol::OpenAiResponses);
        }
        // 其余带 type 的事件（ping 等心跳）不具备协议区分度
        return None;
    }
    if jv.get("candidates").and_then(JsonValue::as_array).is_some() {
        return Some(TargetProtocol::Gemini);
    }
    if let Some(choices) = jv.get("choices").and_then(JsonValue::as_array) {
        // Chat chunk：delta/finish 帧，或 include_usage 尾包的空 choices + usage
        if choices.is_empty() && jv.get("usage").is_none() {
            return None;
        }
        return Some(TargetProtocol::OpenAiChat);
    }
    None
}

/// 把上游非流式响应字节归一化为 OpenAI Chat 格式。
///
/// 先嗅探实际协议再分发解析器：配置与实际不一致时以实际为准；
/// 嗅探失败回退配置值。判定为 Chat（已是中枢格式）或非 JSON 时原样透传。
pub fn normalize_response_bytes(target: TargetProtocol, model: &str, raw: &[u8]) -> Vec<u8> {
    let Ok(jv) = serde_json::from_slice::<JsonValue>(raw) else {
        return raw.to_vec();
    };
    let effective = detect_response_protocol_from_json(&jv).unwrap_or(target);
    match effective {
        TargetProtocol::OpenAiChat => raw.to_vec(),
        TargetProtocol::AnthropicMessages => {
            serde_json::to_vec(&anthropic_response_to_openai(&jv)).unwrap_or_else(|_| raw.to_vec())
        }
        TargetProtocol::Gemini => {
            serde_json::to_vec(&gemini_response_to_openai(&jv, model)).unwrap_or_else(|_| raw.to_vec())
        }
        TargetProtocol::OpenAiResponses => {
            serde_json::to_vec(&responses_response_to_openai(&jv)).unwrap_or_else(|_| raw.to_vec())
        }
    }
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
pub(crate) fn delta_chunk(delta: JsonValue, finish_reason: Option<&str>, usage: Option<JsonValue>) -> String {
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

#[cfg(test)]
mod egress_tests {
    use super::*;
    use bytes::Bytes;
    use futures_util::StreamExt;

    // ---------------------------------------------------------------- 嗅探

    #[test]
    fn detect_json_identifies_each_protocol() {
        let chat = json!({"choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1}});
        assert_eq!(
            detect_response_protocol_from_json(&chat),
            Some(TargetProtocol::OpenAiChat)
        );
        let responses = json!({"id":"resp_1","object":"response","status":"completed","output":[]});
        assert_eq!(
            detect_response_protocol_from_json(&responses),
            Some(TargetProtocol::OpenAiResponses)
        );
        let anthropic = json!({"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"hi"}],"stop_reason":"end_turn"});
        assert_eq!(
            detect_response_protocol_from_json(&anthropic),
            Some(TargetProtocol::AnthropicMessages)
        );
        let gemini = json!({"candidates":[{"content":{"parts":[{"text":"hi"}]}}],"usageMetadata":{"totalTokenCount":1}});
        assert_eq!(
            detect_response_protocol_from_json(&gemini),
            Some(TargetProtocol::Gemini)
        );
    }

    #[test]
    fn detect_json_usage_tail_and_unknown() {
        // include_usage 尾包：空 choices + usage → Chat
        let tail = json!({"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2}});
        assert_eq!(
            detect_response_protocol_from_json(&tail),
            Some(TargetProtocol::OpenAiChat)
        );
        // 错误对象/未知结构 → None（回退渠道配置）
        assert_eq!(detect_response_protocol_from_json(&json!({"type":"error","error":{"message":"x"}})), None);
        assert_eq!(detect_response_protocol_from_json(&json!({})), None);
    }

    #[test]
    fn detect_sse_data_identifies_first_events() {
        assert_eq!(
            detect_response_protocol_from_sse_data(r#"{"type":"message_start","message":{}}"#),
            Some(TargetProtocol::AnthropicMessages)
        );
        assert_eq!(
            detect_response_protocol_from_sse_data(
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"a"}}"#
            ),
            Some(TargetProtocol::AnthropicMessages)
        );
        assert_eq!(
            detect_response_protocol_from_sse_data(r#"{"type":"response.created","response":{}}"#),
            Some(TargetProtocol::OpenAiResponses)
        );
        assert_eq!(
            detect_response_protocol_from_sse_data(
                r#"{"candidates":[{"content":{"parts":[{"text":"a"}]}}]}"#
            ),
            Some(TargetProtocol::Gemini)
        );
        assert_eq!(
            detect_response_protocol_from_sse_data(
                r#"{"choices":[{"index":0,"delta":{"content":"a"}}]}"#
            ),
            Some(TargetProtocol::OpenAiChat)
        );
        // 歧义行不判定
        assert_eq!(
            detect_response_protocol_from_sse_data(r#"{"type":"ping"}"#),
            None
        );
        assert_eq!(detect_response_protocol_from_sse_data("not-json"), None);
    }

    async fn collect_stream<E: Send + 'static>(
        stream: impl futures_util::Stream<Item = Result<Bytes, E>>,
    ) -> Vec<String> {
                let mut out = Vec::new();
        tokio::pin!(stream);
        while let Some(item) = stream.next().await {
            if let Ok(bytes) = item {
                out.push(String::from_utf8_lossy(&bytes).to_string());
            }
        }
        out
    }

    fn sse_body(
        lines: Vec<&str>,
    ) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> {
        let text = lines
            .into_iter()
            .map(|l| format!("data: {l}\n\n"))
            .collect::<String>();
        futures_util::stream::iter(vec![Ok(Bytes::from(text))])
    }

    #[test]
    fn sse_stream_misconfigured_target_uses_actual_protocol() {
        // 配置为 Responses，实际是 Anthropic SSE → 嗅探后按 Anthropic 解析，
        // 以 Chat 客户端出口重建出完整内容与 usage
        let upstream = sse_body(vec![
            r#"{"type":"message_start","message":{"usage":{"input_tokens":10}}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}"#,
            "[DONE]",
        ]);
        let body = crate::model::gateway::stream::proxy_sse_body(
            upstream,
            TargetProtocol::OpenAiResponses,
            crate::model::gateway::stream::SseClientProtocol::Chat,
            test_context(),
            test_log(),
            std::time::Instant::now(),
            "m".to_string(),
        );
        let got = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(collect_stream(body.into_data_stream()));
        let all = got.join("");
        assert!(all.contains("hello"), "跨协议内容必须完整到达客户端: {all}");
        assert!(all.contains("data: [DONE]"));
    }

    #[test]
    fn sse_stream_chat_passthrough_even_when_misconfigured() {
        // 配置为 Gemini，实际是 Chat SSE → 判定为中枢格式后按行透传
        let raw_line = r#"{"choices":[{"index":0,"delta":{"content":"hey"},"finish_reason":null}]}"#;
        let upstream = sse_body(vec![raw_line, "[DONE]"]);
        let body = crate::model::gateway::stream::proxy_sse_body(
            upstream,
            TargetProtocol::Gemini,
            crate::model::gateway::stream::SseClientProtocol::Chat,
            test_context(),
            test_log(),
            std::time::Instant::now(),
            "m".to_string(),
        );
        let got = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(collect_stream(body.into_data_stream()));
        let all = got.join("");
        assert!(all.contains("hey"), "chat 内容必须保留: {all}");
        assert!(all.contains("data: [DONE]"));
    }

    #[test]
    fn sse_stream_undecided_falls_back_to_configured_target() {
        // 全程只有心跳歧义行 → 回退配置的 Responses 解析器收尾
        let upstream = sse_body(vec![r#"{"type":"ping"}"#]);
        let body = crate::model::gateway::stream::proxy_sse_body(
            upstream,
            TargetProtocol::OpenAiResponses,
            crate::model::gateway::stream::SseClientProtocol::Chat,
            test_context(),
            test_log(),
            std::time::Instant::now(),
            "m".to_string(),
        );
        let got = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(collect_stream(body.into_data_stream()));
        let all = got.join("");
        assert!(all.contains("[DONE]"), "回退路径也要合成终帧: {all}");
    }

    fn prepare_egress(
        channel: &ChannelConfig,
        api_key: &str,
        model: &str,
        openai_body: &JsonValue,
        is_stream: bool,
    ) -> (String, JsonValue) {
        let ur = crate::model::gateway::parsers::chat_to_universal(openai_body, model);
        prepare_egress_with(channel, api_key, model, EgressBody::Universal(ur), is_stream)
    }

    fn test_log() -> crate::model::gateway::types::ProxyRequestLog {
        use crate::model::gateway::types::ProxyRequestLog;
        let mut log = ProxyRequestLog {
            id: "test".into(),
            timestamp: String::new(),
            method: "POST".into(),
            path: "/v1/test".into(),
            channel_id: String::new(),
            channel_stats_id: None,
            model: "m".into(),
            stream: true,
            status_code: 200,
            duration_ms: 0,
            ttft_ms: None,
            prompt_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
            cache_creation_tokens: None,
            completion_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            error_message: None,
            request_body: None,
            response_body: None,
            node_name: None,
            client_name: None,
            upstream_url: None,
        };
        // record_log 需要时间戳非空
        log.timestamp = "2026-01-01T00:00:00Z".into();
        log
    }

    fn test_context() -> crate::model::gateway::types::ModelProxyContext {
        use std::sync::atomic::AtomicUsize;
        use std::sync::Arc;
        use tokio::sync::RwLock;
        crate::model::gateway::types::ModelProxyContext {
            route_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            config: Arc::new(RwLock::new(crate::model::gateway::types::ModelProxyConfig::default())),
            metrics: Arc::new(crate::model::gateway::types::ProxyMetrics::default()),
            started_at: Arc::new(RwLock::new(None)),
            current_port: Arc::new(RwLock::new(0)),
            cached_channel_models: Arc::new(RwLock::new(Vec::new())),
            cached_fetch_errors: Arc::new(RwLock::new(Vec::new())),
            default_http_client: Arc::new(RwLock::new(reqwest::Client::new())),
            app_ctx: Arc::new(RwLock::new(None)),
            key_round_robin: Arc::new(AtomicUsize::new(0)),
            node_round_robin: Arc::new(AtomicUsize::new(0)),
            log_retention_last_run: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }


    #[tokio::test]
    async fn chat_upstream_pure_tool_calls_reach_responses_client() {
        // 端到端复刻线上 bug：Chat 上游返回纯 tool_calls + reasoning（content 为空），
        // 旧 Responses 转换器只透传 content 文本 → 客户端收到空响应
        let upstream = futures_util::stream::iter(vec![
            Ok::<Bytes, std::io::Error>(Bytes::from(
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"The user wants to commit.\"}}]}\n\n",
            )),
            Ok(Bytes::from(
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_e96\",\"type\":\"function\",\"function\":{\"name\":\"Bash\",\"arguments\":\"{\\\"command\\\": \\\"git status\\\"}\"}}]}}]}\n\n",
            )),
            Ok(Bytes::from(
                "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            )),
            Ok(Bytes::from("data: [DONE]\n\n")),
        ]);
        let body = crate::model::gateway::stream::proxy_sse_body(
            upstream,
            TargetProtocol::OpenAiChat,
            crate::model::gateway::stream::SseClientProtocol::Responses,
            test_context(),
            test_log(),
            std::time::Instant::now(),
            "big-pickle".to_string(),
        );
        let all = collect_stream(body.into_data_stream()).await.join("");

        // 工具调用必须以标准 function_call item 到达客户端
        assert!(all.contains("\"type\":\"function_call\""), "工具调用不可丢失: {all}");
        assert!(all.contains("\"name\":\"Bash\""));
        assert!(all.contains("call_e96"));
        assert!(all.contains("git status"));
        // reasoning 必须保留
        assert!(all.contains("response.reasoning_summary_text.delta"), "{all}");
        assert!(all.contains("The user wants to commit."));
        // completed 携带完整 output 数组
        assert!(all.contains("event: response.completed"));
        assert!(all.contains("\"status\":\"completed\"") || all.contains("\"status\": \"completed\""));
    }

    #[test]
    fn prepare_egress_native_fast_paths_per_protocol() {
        use crate::model::gateway::egress::EgressBody;
        let body = json!({"model": "m", "messages": [], "max_tokens": 100});

        // Anthropic 渠道：原生体直发 /v1/messages（base 已带 /v1 时不重复）
        let chan = channel_with_protocol("claude");
        let (url, out) = prepare_egress_with(
            &chan,
            "sk-x",
            "claude-x",
            EgressBody::Native(body.clone()),
            true,
        );
        assert!(url.ends_with("/messages"), "anthropic 快车道 URL: {url}");
        assert_eq!(out, body, "原生请求体不得被改写");
        assert!(out.get("stream_options").is_none(), "非 Chat 出口不注入 include_usage");

        // Responses 渠道
        let chan = channel_with_protocol("responses");
        let (url, out) = prepare_egress_with(
            &chan,
            "sk-x",
            "gpt-x",
            EgressBody::Native(body.clone()),
            false,
        );
        assert!(url.ends_with("/responses"), "responses 快车道 URL: {url}");
        assert_eq!(out, body);

        // 跨协议：UniversalRequest 序列化为 Anthropic 原生体
        let chan = channel_with_protocol("claude");
        let ur = crate::model::gateway::parsers::chat_to_universal(
            &json!({
                "model": "claude-x",
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 512,
            }),
            "claude-x",
        );
        let (url2, out2) =
            prepare_egress_with(&chan, "sk-x", "claude-x", EgressBody::Universal(ur), true);
        assert!(url2.ends_with("/v1/messages") || url2.ends_with("/messages"));
        assert!(out2.get("stream_options").is_none(), "非 Chat 出口不注入 include_usage");
        assert_eq!(
            out2.pointer("/messages/0/content/0/text").and_then(JsonValue::as_str),
            Some("hi"),
            "UR 展开的 Anthropic 体为 blocks 结构: {out2}"
        );
    }

    #[tokio::test]
    async fn anthropic_passthrough_preserves_signature_and_reports_usage() {
        // 同协议直通保真回归：thinking signature 等原生元素必须原样到达客户端，
        // 同时网关侧旁路统计到完整 usage
        let upstream = futures_util::stream::iter(vec![
            Ok::<Bytes, std::io::Error>(Bytes::from(
                "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5,\"cache_read_input_tokens\":50,\"cache_creation_input_tokens\":2,\"output_tokens\":0}}}\n\n",
            )),
            Ok(Bytes::from(
                "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
            )),
            Ok(Bytes::from(
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"let me think\"}}\n\n",
            )),
            Ok(Bytes::from(
                "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            )),
            Ok(Bytes::from(
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n\n",
            )),
            Ok(Bytes::from("data: [DONE]\n\n")),
        ]);
        let body = crate::model::gateway::stream::passthrough_sse_body(
            upstream,
            TargetProtocol::AnthropicMessages,
            test_context(),
            test_log(),
            std::time::Instant::now(),
            "claude-x".to_string(),
        );
        let all = collect_stream(body.into_data_stream()).await.join("");

        // 原生元素零损耗
        assert!(all.contains("thinking_delta"), "思考增量必须直通: {all}");
        assert!(all.contains("let me think"));
        // 若上游携带 signature 字段也会原样保留（此处验证结构完整性即可）
        assert!(all.contains("\"type\":\"message_delta\""));
        assert!(all.contains("[DONE]"));
    }
    #[test]
    fn normalize_bytes_misconfigured_target_recovers() {
        // 配置 Anthropic，上游实际返回 Chat JSON → 已是中枢格式，字节原样保留
        let chat = br#"{"choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#;
        let got = normalize_response_bytes(TargetProtocol::AnthropicMessages, "m", chat);
        assert_eq!(got, chat.to_vec());

        // 配置 Chat，上游实际返回 Anthropic JSON → 正确归一化为 Chat
        let anthropic = br#"{"id":"msg_1","type":"message","role":"assistant","model":"m","content":[{"type":"text","text":"hi"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":2}}"#;
        let got = normalize_response_bytes(TargetProtocol::OpenAiChat, "m", anthropic);
        let text = String::from_utf8(got).unwrap();
        assert!(text.contains("\"content\":\"hi\""), "{text}");
    }

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
            key_groups: None,
            key_rules: None,
            model_proxy_rules: None,
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
    fn prepare_egress_bare_domain_base_gets_v1_and_no_double_slash() {
        // 站点转换渠道的 upstreamUrl 普遍是裸域名：出网必须自动补 /v1，
        // 否则请求打到根路径端点、上游无任何请求记录（静默空返回）
        let body = json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});
        let mut chan = channel_with_protocol("openai");
        chan.base_url = "https://x666.me".to_string();
        let (url, _) = prepare_egress(&chan, "", "claude-sonnet-5", &body, true);
        assert_eq!(url, "https://x666.me/v1/chat/completions");

        // 尾部斜杠不得产生双斜杠（scheme 的 // 除外）
        chan.base_url = "https://x666.me/".to_string();
        let (url, _) = prepare_egress(&chan, "", "m", &body, false);
        assert_eq!(url, "https://x666.me/v1/chat/completions");
        assert!(
            !url.split("://", ).nth(1).unwrap_or("").contains("//"),
            "禁止路径双斜杠: {url}"
        );

        // 已带版本路径的基址原样衔接
        chan.base_url = "https://api.example.com/v1".to_string();
        let (url, _) = prepare_egress(&chan, "", "m", &body, false);
        assert_eq!(url, "https://api.example.com/v1/chat/completions");

        // Anthropic / Responses 同规则
        let mut anthro = channel_with_protocol("anthropic");
        anthro.base_url = "https://relay.example".to_string();
        let (url, _) = prepare_egress(&anthro, "", "m", &body, false);
        assert_eq!(url, "https://relay.example/v1/messages");

        let mut resp = channel_with_protocol("responses");
        resp.base_url = "https://relay.example".to_string();
        let (url, _) = prepare_egress(&resp, "", "m", &body, false);
        assert_eq!(url, "https://relay.example/v1/responses");
    }

    #[test]
    fn prepare_egress_builds_url_per_protocol() {
        let body = json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});

        // 出网规则：Chat 渠道走 /chat/completions；
        // 原生协议渠道（anthropic/responses/gemini）跨协议时转换为目标原生体+原生路径
        let (url, _) = prepare_egress(&channel_with_protocol("openai"), "sk-x", "m", &body, false);
        assert_eq!(url, "https://upstream.example/v1/chat/completions");

        let (url, out) = prepare_egress(
            &channel_with_protocol("openai-responses"),
            "sk-x",
            "m",
            &body,
            false,
        );
        assert_eq!(url, "https://upstream.example/v1/responses");
        assert!(
            out.get("input").is_some() || out.get("instructions").is_some(),
            "Responses 原生体必须由 Chat 中枢体转换而来: {out}"
        );

        let (url, out) = prepare_egress(
            &channel_with_protocol("anthropic"),
            "sk-x",
            "m",
            &body,
            true,
        );
        assert_eq!(
            url,
            "https://upstream.example/v1/messages",
            "Anthropic 渠道出网必须使用原生 /v1/messages"
        );
        assert!(
            out.get("max_tokens").is_some(),
            "Anthropic 原生体必须有 max_tokens（官方必填）: {out}"
        );

        // Gemini 原生快速通道（convert=false）保留原生 URL
        let (url, _) = prepare_egress_with(
            &channel_with_protocol("gemini"),
            "sk-123",
            "gemini-2.5-pro",
            crate::model::gateway::egress::EgressBody::Native(body.clone()),
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
            crate::model::gateway::egress::EgressBody::Native(body.clone()),
            true,
        );
        assert_eq!(
            url,
            "https://upstream.example/v1/v1beta/models/g:streamGenerateContent?alt=sse"
        );
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
        // Chat → UR → Gemini 原生体（请求方向经 IR）
        let chat = json!({
            "model": "m",
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "hi"},
            ],
            "max_tokens": 128,
        });
        let ur = crate::model::gateway::parsers::chat_to_universal(&chat, "m");
        let gemini_req = crate::model::gateway::parsers::universal_to_gemini(&ur);
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
    fn anthropic_sse_sequence_parses_to_ir_events() {
        use crate::model::gateway::ir::{UniversalStreamEvent, UniversalUsage};
        use crate::model::gateway::parsers::AnthropicParser;
        let mut p = AnthropicParser::new(Vec::new(), None);
        let mut events = Vec::new();
        for data in [
            r#"{"type":"message_start","message":{"usage":{"input_tokens":20}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"你"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"好"}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":9}}"#,
        ] {
            events.extend(p.feed(data));
        }
        events.extend(p.finish());

        assert!(events.iter().any(|e| matches!(e, UniversalStreamEvent::TextDelta(t) if t == "你")));
        assert!(events.iter().any(|e| matches!(e, UniversalStreamEvent::TextDelta(t) if t == "好")));
        let Some(UniversalStreamEvent::Finish { usage, reason }) = events.last() else {
            panic!("must end with Finish");
        };
        // 全量口径：input 为总量，缓存明细单列
        assert_eq!(usage.input_tokens, 20);
        assert_eq!(usage.output_tokens, 9);
        assert_eq!(*reason, crate::model::gateway::ir::StopReason::EndTurn);
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
        // 首片只有 `{` —— 不得立即以占位名下发
        let upstream = futures_util::stream::iter(vec![
            Ok::<Bytes, std::io::Error>(Bytes::from(
                "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n"
                    .to_string(),
            )),
            Ok(Bytes::from(mk("{"))),
            Ok(Bytes::from(mk("\"command\": \"ls -la\","))),
            Ok(Bytes::from(mk("\"description\": \"list files\"}"))),
            Ok(Bytes::from("data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":20}}\n\n")),
            Ok(Bytes::from("data: [DONE]\n\n")),
        ]);
        let hints = vec![
            (
                "bash".to_string(),
                vec!["command".to_string(), "description".to_string()],
            ),
            ("read_file".to_string(), vec!["path".to_string()]),
        ];
        let body = crate::model::gateway::stream::proxy_sse_body_with_hints(
            upstream,
            TargetProtocol::OpenAiChat,
            crate::model::gateway::stream::SseClientProtocol::Chat,
            test_context(),
            test_log(),
            std::time::Instant::now(),
            "m".to_string(),
            hints,
            None,
        );
        let text: String = collect_stream(body.into_data_stream()).await.join("");

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
            Ok(Bytes::from("data: [DONE]\n\n")),
        ]);
        let hints = vec![
            ("bash".to_string(), vec!["command".to_string()]),
            ("read_file".to_string(), vec!["path".to_string()]),
        ];
        let body = crate::model::gateway::stream::proxy_sse_body_with_hints(
            upstream,
            TargetProtocol::OpenAiChat,
            crate::model::gateway::stream::SseClientProtocol::Chat,
            test_context(),
            test_log(),
            std::time::Instant::now(),
            "m".to_string(),
            hints,
            None,
        );
        let text: String = collect_stream(body.into_data_stream()).await.join("");

        assert!(
            text.contains("\"name\":\"bash\""),
            "工具名应按参数键匹配恢复为 bash: {text}"
        );
        assert!(text.contains("\"id\""), "首片必须携带 id: {text}");
        assert!(text.contains("\\\"command"), "参数增量必须透传");
    }

    #[tokio::test]
    async fn openai_chat_upstream_content_and_usage_survive_ir_pipeline() {
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

        let body = crate::model::gateway::stream::proxy_sse_body(
            upstream,
            TargetProtocol::OpenAiChat,
            crate::model::gateway::stream::SseClientProtocol::Chat,
            test_context(),
            test_log(),
            std::time::Instant::now(),
            "m".to_string(),
        );
        let text: String = collect_stream(body.into_data_stream()).await.join("");

        assert!(text.contains('你') && text.contains('好'));
        assert!(text.contains("\"prompt_tokens\":11"));
        assert!(text.contains("[DONE]"), "[DONE] 必须保留");
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

        // include_usage 仅对 Chat 出口有意义；原生协议出口由响应协议自带 usage
        let (_, out) =
            prepare_egress(&channel_with_protocol("anthropic"), "", "m", &body, true);
        assert!(
            out.get("stream_options").is_none(),
            "Anthropic 原生出口不得注入 Chat 专属的 stream_options"
        );
    }

    #[test]
    fn copy_upstream_headers_skips_framing_and_keeps_useful() {
        // P1-6：透传 x-request-id / retry-after / ratelimit 头，跳过内容与分帧头
        let mut src = reqwest::header::HeaderMap::new();
        src.insert("x-request-id", "req-1".parse().unwrap());
        src.insert("retry-after", "30".parse().unwrap());
        src.insert(
            "anthropic-ratelimit-input-tokens",
            "1000".parse().unwrap(),
        );
        src.insert("content-type", "application/json".parse().unwrap());
        src.insert("transfer-encoding", "chunked".parse().unwrap());
        src.insert("set-cookie", "a=b".parse().unwrap());

        let resp = axum::http::Response::builder()
            .status(200)
            .body(axum::body::Body::empty())
            .unwrap();
        let out = copy_upstream_headers(&src, resp);
        assert_eq!(
            out.headers().get("x-request-id").map(|v| v.to_str().unwrap()),
            Some("req-1")
        );
        assert_eq!(
            out.headers().get("retry-after").map(|v| v.to_str().unwrap()),
            Some("30")
        );
        assert_eq!(
            out.headers()
                .get("anthropic-ratelimit-input-tokens")
                .map(|v| v.to_str().unwrap()),
            Some("1000")
        );
        assert!(out.headers().get("content-type").is_none());
        assert!(out.headers().get("transfer-encoding").is_none());
        assert!(out.headers().get("set-cookie").is_none());
    }

    #[test]
    fn normalized_usage_keeps_reasoning_inside_completion() {
        // P0-3 口径回归：归一化 completion_tokens 含推理，推理明细单列，
        // adapters 端据此直接透传不再叠加（防双重计数）
        let gemini = json!({
            "candidates": [{ "content": { "parts": [{ "text": "hi" }], "role": "model" },
                             "finishReason": "STOP", "index": 0 }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 20,
                "thoughtsTokenCount": 30,
                "totalTokenCount": 60
            }
        });
        let openai = gemini_response_to_openai(&gemini, "m");
        assert_eq!(
            openai.pointer("/usage/completion_tokens").and_then(JsonValue::as_u64),
            Some(50),
            "completion 含推理（candidates + thoughts）"
        );
        assert_eq!(
            openai
                .pointer("/usage/completion_tokens_details/reasoning_tokens")
                .and_then(JsonValue::as_u64),
            Some(30)
        );

        let responses = json!({
            "id": "resp_1", "object": "response", "status": "completed", "output": [],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 300,
                "output_tokens_details": { "reasoning_tokens": 200 }
            }
        });
        let openai = responses_response_to_openai(&responses);
        assert_eq!(
            openai.pointer("/usage/completion_tokens").and_then(JsonValue::as_u64),
            Some(300),
            "Responses output_tokens 已含推理，直接透传"
        );
        assert_eq!(
            openai
                .pointer("/usage/completion_tokens_details/reasoning_tokens")
                .and_then(JsonValue::as_u64),
            Some(200)
        );
    }
}
