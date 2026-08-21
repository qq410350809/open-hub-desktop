use serde_json::{json, Value as JsonValue};

// ---------------------------------------------------------------------------
// 统一模型协议适配器体系 (Protocol Adapters)
// 支持 OpenAI Chat Completions, Anthropic Messages, Google Gemini, OpenAI Responses API
// ---------------------------------------------------------------------------

pub struct OpenAiProtocolAdapter;

impl OpenAiProtocolAdapter {
    /// 严格规范化 tools、functions 与 messages，防止上游反序列化失败或 missing field function
    pub fn sanitize_and_normalize(body: &mut JsonValue) {
        if let Some(obj) = body.as_object_mut() {
            // 1. 兼容老版本 functions 转换为 tools
            if let Some(funcs_val) = obj.remove("functions") {
                if let Some(func_arr) = funcs_val.as_array() {
                    if !obj.contains_key("tools") {
                        let mut converted = Vec::new();
                        for f in func_arr {
                            if let Some(f_obj) = f.as_object() {
                                let name = f_obj.get("name").cloned().unwrap_or_else(|| json!(""));
                                let desc = f_obj.get("description").cloned().unwrap_or_else(|| json!(""));
                                let params = f_obj.get("parameters").or_else(|| f_obj.get("input_schema")).cloned()
                                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
                                converted.push(json!({
                                    "type": "function",
                                    "function": {
                                        "name": name,
                                        "description": desc,
                                        "parameters": params
                                    }
                                }));
                            }
                        }
                        if !converted.is_empty() {
                            obj.insert("tools".to_string(), json!(converted));
                        }
                    }
                }
            }

            // 2. 严格规范化 tools
            if let Some(tools_val) = obj.get_mut("tools") {
                if let Some(tools_arr) = tools_val.as_array() {
                    let mut valid_tools = Vec::new();

                    for item in tools_arr {
                        if let Some(item_obj) = item.as_object() {
                            let mut name = String::new();
                            let mut description = String::new();
                            let mut parameters = json!({ "type": "object", "properties": {} });

                            // 格式 A: 嵌套在 function 内部 (OpenAI 格式)
                            if let Some(f_val) = item_obj.get("function") {
                                if let Some(f_obj) = f_val.as_object() {
                                    if let Some(n) = f_obj.get("name").and_then(JsonValue::as_str) {
                                        name = n.trim().to_string();
                                    }
                                    if let Some(d) = f_obj.get("description").and_then(JsonValue::as_str) {
                                        description = d.to_string();
                                    }
                                    if let Some(p) = f_obj.get("parameters").or_else(|| f_obj.get("input_schema")) {
                                        parameters = p.clone();
                                    }
                                }
                            }

                            // 格式 B: 扁平格式 (Anthropic / Gemini 格式，name / input_schema 等直接位于顶层)
                            if name.is_empty() {
                                if let Some(n) = item_obj.get("name").and_then(JsonValue::as_str) {
                                    name = n.trim().to_string();
                                }
                                if let Some(d) = item_obj.get("description").and_then(JsonValue::as_str) {
                                    description = d.to_string();
                                }
                                if let Some(p) = item_obj.get("parameters").or_else(|| item_obj.get("input_schema")) {
                                    parameters = p.clone();
                                }
                            }

                            // 只有提取到非空名称时才保留
                            if !name.is_empty() {
                                if !parameters.is_object() {
                                    parameters = json!({ "type": "object", "properties": {} });
                                }
                                valid_tools.push(json!({
                                    "type": "function",
                                    "function": {
                                        "name": name,
                                        "description": description,
                                        "parameters": parameters
                                    }
                                }));
                            }
                        }
                    }

                    if valid_tools.is_empty() {
                        obj.remove("tools");
                        obj.remove("tool_choice");
                    } else {
                        obj.insert("tools".to_string(), json!(valid_tools));
                    }
                } else {
                    obj.remove("tools");
                    obj.remove("tool_choice");
                }
            } else {
                obj.remove("tool_choice");
            }
        }

        // 3. 规范化 messages
        if let Some(messages) = body.get_mut("messages").and_then(JsonValue::as_array_mut) {
            for msg in messages {
                // 规范化 role
                if let Some(role_val) = msg.get_mut("role") {
                    if let Some(r_str) = role_val.as_str() {
                        match r_str {
                            "developer" => {
                                *role_val = JsonValue::String("system".to_string());
                            }
                            "model" => {
                                *role_val = JsonValue::String("assistant".to_string());
                            }
                            "function" => {
                                *role_val = JsonValue::String("tool".to_string());
                            }
                            _ => {}
                        }
                    }
                }

                // 规范化 content 复合数组 -> 纯文本
                if let Some(content_val) = msg.get_mut("content") {
                    if let Some(arr) = content_val.as_array() {
                        let mut combined_text = String::new();
                        for part in arr {
                            if let Some(t) = part.get("text").and_then(JsonValue::as_str) {
                                if !combined_text.is_empty() {
                                    combined_text.push('\n');
                                }
                                combined_text.push_str(t);
                            } else if part.get("type").and_then(JsonValue::as_str) == Some("image_url")
                                || part.get("image_url").is_some()
                                || part.get("type").and_then(JsonValue::as_str) == Some("image")
                            {
                                if !combined_text.is_empty() {
                                    combined_text.push('\n');
                                }
                                combined_text.push_str("[图片输入]");
                            } else if let Some(_audio) = part.get("input_audio") {
                                if !combined_text.is_empty() {
                                    combined_text.push('\n');
                                }
                                combined_text.push_str("[语音输入]");
                            }
                        }
                        *content_val = JsonValue::String(combined_text);
                    } else if content_val.is_null() {
                        *content_val = JsonValue::String(String::new());
                    }
                }

                // 规范化 tool_calls
                if let Some(tc_val) = msg.get_mut("tool_calls") {
                    if let Some(tool_calls_arr) = tc_val.as_array() {
                        let mut valid_tc = Vec::new();
                        for tc in tool_calls_arr {
                            if let Some(tc_obj) = tc.as_object() {
                                let mut name = String::new();
                                let mut args_str = String::new();
                                let id = tc_obj.get("id").and_then(JsonValue::as_str).unwrap_or("call_default").to_string();

                                if let Some(f_val) = tc_obj.get("function") {
                                    if let Some(f_obj) = f_val.as_object() {
                                        if let Some(n) = f_obj.get("name").and_then(JsonValue::as_str) {
                                            name = n.trim().to_string();
                                        }
                                        if let Some(a) = f_obj.get("arguments") {
                                            args_str = if let Some(s) = a.as_str() { s.to_string() } else { a.to_string() };
                                        }
                                    }
                                }
                                if name.is_empty() {
                                    if let Some(n) = tc_obj.get("name").and_then(JsonValue::as_str) {
                                        name = n.trim().to_string();
                                    }
                                    if let Some(a) = tc_obj.get("arguments") {
                                        args_str = if let Some(s) = a.as_str() { s.to_string() } else { a.to_string() };
                                    }
                                }

                                if !name.is_empty() {
                                    if args_str.trim().is_empty() {
                                        args_str = "{}".to_string();
                                    }
                                    valid_tc.push(json!({
                                        "id": id,
                                        "type": "function",
                                        "function": {
                                            "name": name,
                                            "arguments": args_str
                                        }
                                    }));
                                }
                            }
                        }

                        if valid_tc.is_empty() {
                            msg.as_object_mut().map(|o| o.remove("tool_calls"));
                        } else {
                            *tc_val = json!(valid_tc);
                        }
                    } else {
                        msg.as_object_mut().map(|o| o.remove("tool_calls"));
                    }
                }

                // 规范化 role: "tool"
                if msg.get("role").and_then(JsonValue::as_str) == Some("tool") {
                    if let Some(msg_obj) = msg.as_object_mut() {
                        if !msg_obj.contains_key("tool_call_id") || msg_obj.get("tool_call_id").map_or(true, |v| v.is_null()) {
                            msg_obj.insert("tool_call_id".to_string(), json!("call_default"));
                        }
                    }
                }

                // 提取 Assistant 思考过程
                if msg.get("role").and_then(JsonValue::as_str) == Some("assistant") {
                    let needs_reasoning = msg.get("reasoning_content").map_or(true, |v| v.is_null());
                    if needs_reasoning {
                        let mut extracted_reasoning = String::new();
                        if let Some(content_str) = msg.get("content").and_then(JsonValue::as_str) {
                            if let (Some(start), Some(end)) = (content_str.find("<think>"), content_str.find("</think>")) {
                                if start < end {
                                    extracted_reasoning = content_str[start + 7..end].trim().to_string();
                                    let after_text = &content_str[end + 8..];
                                    msg["content"] = JsonValue::String(after_text.trim_start().to_string());
                                }
                            }
                        }
                        msg["reasoning_content"] = JsonValue::String(extracted_reasoning);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Google Gemini 原生协议适配器 (Gemini ↔ OpenAI 双向转换)
// ---------------------------------------------------------------------------

pub struct GeminiProtocolAdapter;

impl GeminiProtocolAdapter {
    /// 将 Gemini contents 格式转换为标准 OpenAI Chat Completions 请求格式
    pub fn gemini_request_to_openai(gemini_body: &JsonValue, target_model: &str, stream: bool) -> JsonValue {
        let mut messages = Vec::new();

        // 1. 处理 systemInstruction
        if let Some(sys) = gemini_body.get("systemInstruction") {
            let mut sys_text = String::new();
            if let Some(parts) = sys.get("parts").and_then(JsonValue::as_array) {
                for p in parts {
                    if let Some(t) = p.get("text").and_then(JsonValue::as_str) {
                        if !sys_text.is_empty() {
                            sys_text.push('\n');
                        }
                        sys_text.push_str(t);
                    }
                }
            }
            if !sys_text.is_empty() {
                messages.push(json!({
                    "role": "system",
                    "content": sys_text
                }));
            }
        }

        // 2. 处理 contents
        if let Some(contents) = gemini_body.get("contents").and_then(JsonValue::as_array) {
            for item in contents {
                let role = match item.get("role").and_then(JsonValue::as_str) {
                    Some("model") => "assistant",
                    _ => "user",
                };

                let mut text_content = String::new();
                let mut tool_calls = Vec::new();
                let mut tool_responses = Vec::new();

                if let Some(parts) = item.get("parts").and_then(JsonValue::as_array) {
                    for p in parts {
                        if let Some(t) = p.get("text").and_then(JsonValue::as_str) {
                            if !text_content.is_empty() {
                                text_content.push('\n');
                            }
                            text_content.push_str(t);
                        } else if let Some(fc) = p.get("functionCall") {
                            let name = fc.get("name").and_then(JsonValue::as_str).unwrap_or("tool").to_string();
                            let args = fc.get("args").cloned().unwrap_or_else(|| json!({}));
                            tool_calls.push(json!({
                                "id": format!("call_{name}"),
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": args.to_string()
                                }
                            }));
                        } else if let Some(fr) = p.get("functionResponse") {
                            let name = fr.get("name").and_then(JsonValue::as_str).unwrap_or("tool");
                            let resp = fr.get("response").cloned().unwrap_or_else(|| json!({}));
                            tool_responses.push(json!({
                                "role": "tool",
                                "name": name,
                                "tool_call_id": format!("call_{name}"),
                                "content": resp.to_string()
                            }));
                        }
                    }
                }

                if !tool_responses.is_empty() {
                    for tr in tool_responses {
                        messages.push(tr);
                    }
                } else if !tool_calls.is_empty() {
                    messages.push(json!({
                        "role": "assistant",
                        "content": if text_content.is_empty() { JsonValue::Null } else { JsonValue::String(text_content) },
                        "tool_calls": tool_calls
                    }));
                } else {
                    messages.push(json!({
                        "role": role,
                        "content": text_content
                    }));
                }
            }
        }

        // 3. 构建 OpenAI 请求体
        let mut openai_req = json!({
            "model": target_model,
            "messages": messages,
            "stream": stream
        });

        // 4. 解析 generationConfig
        if let Some(cfg) = gemini_body.get("generationConfig").and_then(JsonValue::as_object) {
            if let Some(temp) = cfg.get("temperature").and_then(JsonValue::as_f64) {
                openai_req["temperature"] = json!(temp);
            }
            if let Some(max_tokens) = cfg.get("maxOutputTokens").and_then(JsonValue::as_i64) {
                openai_req["max_tokens"] = json!(max_tokens);
            }
            if let Some(top_p) = cfg.get("topP").and_then(JsonValue::as_f64) {
                openai_req["top_p"] = json!(top_p);
            }
            if let Some(stop) = cfg.get("stopSequences") {
                openai_req["stop"] = stop.clone();
            }
            if let Some(mime) = cfg.get("responseMimeType").and_then(JsonValue::as_str) {
                if mime == "application/json" {
                    openai_req["response_format"] = json!({ "type": "json_object" });
                }
            }
        }

        // 5. 解析 tools (functionDeclarations)
        if let Some(tools_arr) = gemini_body.get("tools").and_then(JsonValue::as_array) {
            let mut openai_tools = Vec::new();
            for t in tools_arr {
                if let Some(decls) = t.get("functionDeclarations").and_then(JsonValue::as_array) {
                    for d in decls {
                        if let Some(name) = d.get("name").and_then(JsonValue::as_str) {
                            let desc = d.get("description").and_then(JsonValue::as_str).unwrap_or("");
                            let params = d.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object", "properties": {}}));
                            openai_tools.push(json!({
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "description": desc,
                                    "parameters": params
                                }
                            }));
                        }
                    }
                }
            }
            if !openai_tools.is_empty() {
                openai_req["tools"] = json!(openai_tools);
            }
        }

        OpenAiProtocolAdapter::sanitize_and_normalize(&mut openai_req);
        openai_req
    }

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
                            let args_val = f.get("arguments")
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
        let prompt_tokens = usage.and_then(|u| u.get("prompt_tokens")).and_then(JsonValue::as_i64).unwrap_or(0);
        let completion_tokens = usage.and_then(|u| u.get("completion_tokens")).and_then(JsonValue::as_i64).unwrap_or(0);
        let total_tokens = usage.and_then(|u| u.get("total_tokens")).and_then(JsonValue::as_i64).unwrap_or(prompt_tokens + completion_tokens);

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

    /// 将 OpenAI 单个流式 SSE delta chunk 转换为 Gemini SSE chunk
    pub fn openai_chunk_to_gemini_chunk(delta_json: &JsonValue, model: &str) -> Option<JsonValue> {
        let choice = delta_json.pointer("/choices/0")?;
        let mut parts = Vec::new();

        if let Some(delta) = choice.get("delta") {
            if let Some(text) = delta.get("content").and_then(JsonValue::as_str) {
                if !text.is_empty() {
                    parts.push(json!({ "text": text }));
                }
            }
            if let Some(tool_calls) = delta.get("tool_calls").and_then(JsonValue::as_array) {
                for tc in tool_calls {
                    if let Some(f) = tc.get("function") {
                        let name = f.get("name").and_then(JsonValue::as_str).unwrap_or("");
                        let args_str = f.get("arguments").and_then(JsonValue::as_str).unwrap_or("");
                        let args_val = serde_json::from_str::<JsonValue>(args_str).unwrap_or_else(|_| json!({}));
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

        let finish_reason = choice.get("finish_reason").and_then(JsonValue::as_str).map(|r| match r {
            "stop" => "STOP",
            "length" => "MAX_TOKENS",
            "tool_calls" => "STOP",
            _ => "STOP",
        });

        if parts.is_empty() && finish_reason.is_none() {
            return None;
        }

        let mut candidate = json!({
            "content": {
                "parts": parts,
                "role": "model"
            },
            "index": 0
        });

        if let Some(fr) = finish_reason {
            candidate["finishReason"] = json!(fr);
        }

        let mut chunk = json!({
            "candidates": [candidate],
            "modelVersion": model
        });

        if let Some(usage) = delta_json.get("usage") {
            let prompt_tokens = usage.get("prompt_tokens").and_then(JsonValue::as_i64).unwrap_or(0);
            let completion_tokens = usage.get("completion_tokens").and_then(JsonValue::as_i64).unwrap_or(0);
            let total_tokens = usage.get("total_tokens").and_then(JsonValue::as_i64).unwrap_or(prompt_tokens + completion_tokens);
            chunk["usageMetadata"] = json!({
                "promptTokenCount": prompt_tokens,
                "candidatesTokenCount": completion_tokens,
                "totalTokenCount": total_tokens
            });
        }

        Some(chunk)
    }
}

pub struct ResponsesProtocolAdapter;

impl ResponsesProtocolAdapter {
    /// 将 Responses API 的 input 与 instructions 转译为标准 OpenAI messages
    pub fn convert_input_to_messages(body: &mut JsonValue) {
        let is_responses_spec = body.get("input").is_some() || body.get("instructions").is_some();
        if is_responses_spec && body.get("messages").is_none() {
            let mut msgs = Vec::new();
            if let Some(instructions) = body.get("instructions").and_then(JsonValue::as_str) {
                if !instructions.is_empty() {
                    msgs.push(json!({
                        "role": "system",
                        "content": instructions
                    }));
                }
            }
            if let Some(input_val) = body.get("input") {
                if let Some(input_str) = input_val.as_str() {
                    msgs.push(json!({
                        "role": "user",
                        "content": input_str
                    }));
                } else if let Some(input_arr) = input_val.as_array() {
                    for item in input_arr {
                        if let Some(item_obj) = item.as_object() {
                            let role = item_obj.get("role").and_then(JsonValue::as_str).unwrap_or("user");
                            let content = item_obj.get("content").cloned().unwrap_or_else(|| json!(""));
                            msgs.push(json!({
                                "role": role,
                                "content": content
                            }));
                        }
                    }
                }
            }
            if !msgs.is_empty() {
                body["messages"] = json!(msgs);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Anthropic Claude Messages 协议适配器 (Anthropic ↔ OpenAI 双向转换)
// ---------------------------------------------------------------------------

pub struct AnthropicProtocolAdapter;

impl AnthropicProtocolAdapter {
    /// 将 Anthropic tools 格式转换为 OpenAI tools 格式
    fn convert_tools(anthropic_body: &JsonValue) -> Vec<JsonValue> {
        let mut openai_tools = Vec::new();
        if let Some(tools_arr) = anthropic_body.get("tools").and_then(JsonValue::as_array) {
            for t in tools_arr {
                let mut name = String::new();
                let mut desc = String::new();
                let mut schema = json!({"type": "object", "properties": {}});

                if let Some(n) = t.get("name").and_then(JsonValue::as_str) {
                    name = n.trim().to_string();
                }
                if let Some(d) = t.get("description").and_then(JsonValue::as_str) {
                    desc = d.to_string();
                }
                if let Some(s) = t.get("input_schema").or_else(|| t.get("parameters")) {
                    schema = s.clone();
                }

                // 兼容 OpenAI 嵌套格式
                if name.is_empty() {
                    if let Some(f) = t.get("function").and_then(JsonValue::as_object) {
                        if let Some(n) = f.get("name").and_then(JsonValue::as_str) {
                            name = n.trim().to_string();
                        }
                        if let Some(d) = f.get("description").and_then(JsonValue::as_str) {
                            desc = d.to_string();
                        }
                        if let Some(p) = f.get("parameters") {
                            schema = p.clone();
                        }
                    }
                }

                if !name.is_empty() {
                    openai_tools.push(json!({
                        "type": "function",
                        "function": {
                            "name": name,
                            "description": desc,
                            "parameters": schema
                        }
                    }));
                }
            }
        }
        openai_tools
    }

    /// 将 Anthropic system 字段提取为 OpenAI system message
    fn convert_system(anthropic_body: &JsonValue) -> Option<JsonValue> {
        if let Some(system_val) = anthropic_body.get("system") {
            if let Some(sys_str) = system_val.as_str() {
                if !sys_str.is_empty() {
                    return Some(json!({ "role": "system", "content": sys_str }));
                }
            } else if let Some(sys_arr) = system_val.as_array() {
                let mut combined = String::new();
                for item in sys_arr {
                    if let Some(t) = item.get("text").and_then(JsonValue::as_str) {
                        if !combined.is_empty() {
                            combined.push('\n');
                        }
                        combined.push_str(t);
                    }
                }
                if !combined.is_empty() {
                    return Some(json!({ "role": "system", "content": combined }));
                }
            }
        }
        None
    }

    /// 将 Anthropic messages 转换为 OpenAI messages
    fn convert_messages(anthropic_body: &JsonValue) -> Vec<JsonValue> {
        let mut messages = Vec::new();
        if let Some(msgs_arr) = anthropic_body.get("messages").and_then(JsonValue::as_array) {
            for msg in msgs_arr {
                let role = msg.get("role").and_then(JsonValue::as_str).unwrap_or("user");
                if let Some(content_val) = msg.get("content") {
                    if let Some(c_str) = content_val.as_str() {
                        messages.push(json!({ "role": role, "content": c_str }));
                    } else if let Some(c_arr) = content_val.as_array() {
                        let mut text_parts = String::new();
                        let mut tool_calls = Vec::new();
                        let mut tool_results = Vec::new();

                        for block in c_arr {
                            let b_type = block.get("type").and_then(JsonValue::as_str).unwrap_or("");
                            match b_type {
                                "text" => {
                                    if let Some(t) = block.get("text").and_then(JsonValue::as_str) {
                                        if !text_parts.is_empty() {
                                            text_parts.push('\n');
                                        }
                                        text_parts.push_str(t);
                                    }
                                }
                                "tool_use" => {
                                    let id = block.get("id").and_then(JsonValue::as_str).unwrap_or("call_default").to_string();
                                    let name = block.get("name").and_then(JsonValue::as_str).unwrap_or("").to_string();
                                    let input_val = block.get("input").cloned().unwrap_or_else(|| json!({}));
                                    if !name.is_empty() {
                                        tool_calls.push(json!({
                                            "id": id,
                                            "type": "function",
                                            "function": {
                                                "name": name,
                                                "arguments": input_val.to_string()
                                            }
                                        }));
                                    }
                                }
                                "tool_result" => {
                                    let tool_use_id = block.get("tool_use_id").and_then(JsonValue::as_str).unwrap_or("call_default");
                                    let mut res_str = String::new();
                                    if let Some(c) = block.get("content") {
                                        if let Some(s) = c.as_str() {
                                            res_str = s.to_string();
                                        } else if let Some(arr) = c.as_array() {
                                            for part in arr {
                                                if let Some(t) = part.get("text").and_then(JsonValue::as_str) {
                                                    res_str.push_str(t);
                                                }
                                            }
                                        }
                                    }
                                    tool_results.push(json!({
                                        "role": "tool",
                                        "tool_call_id": tool_use_id,
                                        "content": res_str
                                    }));
                                }
                                _ => {}
                            }
                        }

                        if !tool_results.is_empty() {
                            for tr in tool_results {
                                messages.push(tr);
                            }
                        } else if !tool_calls.is_empty() {
                            messages.push(json!({
                                "role": "assistant",
                                "content": if text_parts.is_empty() { JsonValue::Null } else { JsonValue::String(text_parts) },
                                "tool_calls": tool_calls
                            }));
                        } else {
                            messages.push(json!({
                                "role": role,
                                "content": text_parts
                            }));
                        }
                    }
                }
            }
        }
        messages
    }

    /// 将 Anthropic Messages API 请求体转换为 OpenAI Chat Completions 请求体
    pub fn anthropic_request_to_openai(anthropic_body: &JsonValue, target_model: &str, stream: bool) -> JsonValue {
        let mut messages = Vec::new();

        // 1. 提取 system
        if let Some(sys_msg) = Self::convert_system(anthropic_body) {
            messages.push(sys_msg);
        }

        // 2. 转换 messages
        messages.extend(Self::convert_messages(anthropic_body));

        // 3. 构建基础 OpenAI 请求
        let mut openai_req = json!({
            "model": target_model,
            "messages": messages,
            "stream": stream
        });

        // 4. 映射参数
        if let Some(temp) = anthropic_body.get("temperature").and_then(JsonValue::as_f64) {
            openai_req["temperature"] = json!(temp);
        }
        if let Some(max_tokens) = anthropic_body.get("max_tokens").and_then(JsonValue::as_i64) {
            openai_req["max_tokens"] = json!(max_tokens);
        }

        // 5. 转换 tools
        let openai_tools = Self::convert_tools(anthropic_body);
        if !openai_tools.is_empty() {
            openai_req["tools"] = json!(openai_tools);
        }

        OpenAiProtocolAdapter::sanitize_and_normalize(&mut openai_req);
        openai_req
    }

    /// 将 OpenAI 非流式响应转换为 Anthropic Messages 响应格式
    pub fn openai_response_to_anthropic(openai_resp: &JsonValue, req_id: &str, model: &str) -> JsonValue {
        let mut content_blocks = Vec::new();

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
                let id = tc.get("id").and_then(JsonValue::as_str).unwrap_or("call_default");
                let name = tc.pointer("/function/name").and_then(JsonValue::as_str).unwrap_or("tool");
                let args_str = tc.pointer("/function/arguments").and_then(JsonValue::as_str).unwrap_or("{}");
                let args_val = serde_json::from_str::<JsonValue>(args_str).unwrap_or_else(|_| json!({}));
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

        // 映射 usage
        let usage = openai_resp.get("usage");
        let p_tok = usage.and_then(|u| u.get("prompt_tokens")).and_then(JsonValue::as_u64).unwrap_or(0);
        let c_tok = usage.and_then(|u| u.get("completion_tokens")).and_then(JsonValue::as_u64).unwrap_or(0);

        json!({
            "id": format!("msg_{req_id}"),
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": content_blocks,
            "stop_reason": stop_reason,
            "stop_sequence": null,
            "usage": {
                "input_tokens": p_tok,
                "output_tokens": c_tok
            }
        })
    }

    /// 提取 OpenAI 响应中的 token 用量信息，供日志记录使用
    pub fn extract_token_usage(openai_resp: &JsonValue) -> (u64, u64) {
        let usage = openai_resp.get("usage");
        let p_tok = usage.and_then(|u| u.get("prompt_tokens")).and_then(JsonValue::as_u64).unwrap_or(0);
        let c_tok = usage.and_then(|u| u.get("completion_tokens")).and_then(JsonValue::as_u64).unwrap_or(0);
        (p_tok, c_tok)
    }
}

#[inline]
pub fn normalize_chat_messages(body: &mut JsonValue) {
    OpenAiProtocolAdapter::sanitize_and_normalize(body);
}
