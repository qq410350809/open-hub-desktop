use serde_json::{json, Value as JsonValue};

// ---------------------------------------------------------------------------
// 统一模型协议适配器体系 (Protocol Adapters)
// 支持 OpenAI Chat Completions, Anthropic Messages, Google Gemini, OpenAI Responses API
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Google Gemini 原生协议适配器 (Gemini ↔ OpenAI 双向转换)
// ---------------------------------------------------------------------------

pub struct GeminiProtocolAdapter;

impl GeminiProtocolAdapter {
    /// 将 Gemini contents 格式转换为标准 OpenAI Chat Completions 请求格式
    /// 将 OpenAI 完整非流式响应转译为 Gemini generateContent 响应格式
    pub fn openai_response_to_gemini(openai_resp: &JsonValue, model: &str) -> JsonValue {
        let mut parts = Vec::new();
        let mut finish_reason = "STOP";

        if let Some(choice) = openai_resp.pointer("/choices/0") {
            if let Some(msg) = choice.get("message") {
                if let Some(content) = msg.get("content").and_then(JsonValue::as_str) {
                    if !content.is_empty() {
                        parts.push(json!({ "text": content }));
                    }
                }
                if let Some(tool_calls) = msg.get("tool_calls").and_then(JsonValue::as_array) {
                    for tc in tool_calls {
                        if let Some(f) = tc.get("function") {
                            let name = f.get("name").and_then(JsonValue::as_str).unwrap_or("tool");
                            let args_val = f
                                .get("arguments")
                                .and_then(|a| {
                                    if let Some(s) = a.as_str() {
                                        serde_json::from_str::<JsonValue>(s).ok()
                                    } else {
                                        Some(a.clone())
                                    }
                                })
                                .unwrap_or_else(|| json!({}));
                            parts.push(json!({
                                "functionCall": {
                                    "name": name,
                                    "args": args_val
                                }
                            }));
                        }
                    }
                }
            }
            if let Some(reason) = choice.get("finish_reason").and_then(JsonValue::as_str) {
                finish_reason = match reason {
                    "stop" => "STOP",
                    "length" => "MAX_TOKENS",
                    "tool_calls" => "STOP",
                    _ => "STOP",
                };
            }
        }

        let usage = openai_resp.get("usage");
        let prompt_tokens = usage
            .and_then(|u| u.get("prompt_tokens"))
            .and_then(JsonValue::as_i64)
            .unwrap_or(0);
        let completion_tokens = usage
            .and_then(|u| u.get("completion_tokens"))
            .and_then(JsonValue::as_i64)
            .unwrap_or(0);
        let total_tokens = usage
            .and_then(|u| u.get("total_tokens"))
            .and_then(JsonValue::as_i64)
            .unwrap_or(prompt_tokens + completion_tokens);

        json!({
            "candidates": [
                {
                    "content": {
                        "parts": parts,
                        "role": "model"
                    },
                    "finishReason": finish_reason,
                    "index": 0
                }
            ],
            "usageMetadata": {
                "promptTokenCount": prompt_tokens,
                "candidatesTokenCount": completion_tokens,
                "totalTokenCount": total_tokens
            },
            "modelVersion": model
        })
    }
}

// ---------------------------------------------------------------------------
// Anthropic Claude Messages 协议适配器 (Anthropic ↔ OpenAI 双向转换)
// ---------------------------------------------------------------------------

pub struct AnthropicProtocolAdapter;

impl AnthropicProtocolAdapter {
    /// 将 Anthropic tools 格式转换为 OpenAI tools 格式
    /// 将 Anthropic system 字段提取为 OpenAI system message
    /// 将 Anthropic messages 转换为 OpenAI messages
    /// 将 Anthropic Messages API 请求体转换为 OpenAI Chat Completions 请求体
    /// 将 OpenAI 非流式响应转换为 Anthropic Messages 响应格式
    pub fn openai_response_to_anthropic(
        openai_resp: &JsonValue,
        req_id: &str,
        model: &str,
    ) -> JsonValue {
        let mut content_blocks = Vec::new();

        // 思考内容 → thinking 块（置于正文前，与 Anthropic 产出顺序一致）；
        // 请求方向对未知块静默忽略，客户端回显安全
        let reasoning = openai_resp
            .pointer("/choices/0/message/reasoning_content")
            .or_else(|| openai_resp.pointer("/choices/0/message/reasoning"))
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        if !reasoning.trim().is_empty() {
            content_blocks.push(json!({ "type": "thinking", "thinking": reasoning }));
        }

        // 提取文本内容
        let text = openai_resp
            .pointer("/choices/0/message/content")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        if !text.is_empty() {
            content_blocks.push(json!({ "type": "text", "text": text }));
        }

        // 提取 tool_calls
        if let Some(tool_calls) = openai_resp
            .pointer("/choices/0/message/tool_calls")
            .and_then(JsonValue::as_array)
        {
            for tc in tool_calls {
                let id = tc
                    .get("id")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("call_default");
                let name = tc
                    .pointer("/function/name")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("tool");
                let args_str = tc
                    .pointer("/function/arguments")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("{}");
                let args_val =
                    serde_json::from_str::<JsonValue>(args_str).unwrap_or_else(|_| json!({}));
                content_blocks.push(json!({
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": args_val
                }));
            }
        }

        // 映射 stop_reason
        let finish_reason = openai_resp
            .pointer("/choices/0/finish_reason")
            .and_then(JsonValue::as_str);
        let stop_reason = match finish_reason {
            Some("stop") => "end_turn",
            Some("length") => "max_tokens",
            Some("tool_calls") => "tool_use",
            _ => "end_turn",
        };

        // 映射 usage：归一化口径中 prompt_tokens 为总量（含缓存命中/写入），
        // Anthropic 语义要求 input_tokens 不含缓存部分，明细单列。
        // 此前仅回传 input/output，客户端本地记账的缓存命中恒为 0。
        let usage = openai_resp.get("usage");
        let p_tok = usage
            .and_then(|u| u.get("prompt_tokens"))
            .and_then(JsonValue::as_u64)
            .unwrap_or(0);
        let c_tok = usage
            .and_then(|u| u.get("completion_tokens"))
            .and_then(JsonValue::as_u64)
            .unwrap_or(0);
        let cache_read = usage
            .and_then(|u| u.pointer("/prompt_tokens_details/cached_tokens"))
            .and_then(JsonValue::as_u64)
            .unwrap_or(0);
        let cache_creation = usage
            .and_then(|u| u.pointer("/prompt_tokens_details/cache_creation_tokens"))
            .and_then(JsonValue::as_u64)
            .unwrap_or(0);
        // 归一化口径中 completion_tokens 已含推理（Gemini 为 candidates+thoughts、
        // Responses 为含 reasoning 的 output_tokens、Chat 上游原生即含），
        // Anthropic 语义的 output_tokens 同样含思考部分，因此直接透传，
        // 不得再加 reasoning_tokens —— 否则推理 token 被计两次。
        let input_only = p_tok
            .saturating_sub(cache_read)
            .saturating_sub(cache_creation);

        json!({
            "id": format!("msg_{req_id}"),
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": content_blocks,
            "stop_reason": stop_reason,
            "stop_sequence": null,
            "usage": {
                "input_tokens": input_only,
                "cache_read_input_tokens": cache_read,
                "cache_creation_input_tokens": cache_creation,
                "output_tokens": c_tok
            }
        })
    }

    /// 提取 OpenAI 响应中的 token 用量信息，供日志记录使用
    pub fn extract_token_usage(openai_resp: &JsonValue) -> (u64, u64) {
        let usage = openai_resp.get("usage");
        let p_tok = usage
            .and_then(|u| u.get("prompt_tokens"))
            .and_then(JsonValue::as_u64)
            .unwrap_or(0);
        let c_tok = usage
            .and_then(|u| u.get("completion_tokens"))
            .and_then(JsonValue::as_u64)
            .unwrap_or(0);
        (p_tok, c_tok)
    }
}

#[cfg(test)]
mod adapter_tests {
    use super::*;

    #[test]
    fn openai_response_to_anthropic_does_not_double_count_reasoning() {
        // P0-3：归一化 completion_tokens 已含推理（Gemini 为 candidates+thoughts、
        // Responses 为含 reasoning 的 output_tokens），Anthropic 语义的 output_tokens
        // 同样含思考部分，直接透传即可，叠加 reasoning 会重复计数
        let openai_resp = json!({
            "choices": [{ "message": { "role": "assistant", "content": "ok" }, "finish_reason": "stop" }],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 300,
                "total_tokens": 400,
                "completion_tokens_details": { "reasoning_tokens": 200 }
            }
        });
        let out = AnthropicProtocolAdapter::openai_response_to_anthropic(&openai_resp, "req1", "m");
        assert_eq!(
            out.pointer("/usage/output_tokens")
                .and_then(JsonValue::as_u64),
            Some(300),
            "推理 token 不得被计两次"
        );
    }
}
