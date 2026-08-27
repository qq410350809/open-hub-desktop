//! OpenCode 官方免费渠道的个性化策略集中地。
//!
//! 该渠道存在大量与通用网关语义无关的特殊行为：CLI 身份伪装、匿名免费模型
//! 白名单、200 空内容缺陷容错等。此前这些逻辑散落在 balancer / dispatcher /
//! router / config 各处，本模块将其归拢为单一入口 —— 修改 OpenCode 行为
//! 只改这里；新增其他特殊渠道时可参照此模式建立同级策略文件。

use super::super::types::ChannelConfig;
use serde_json::Value as JsonValue;

/// 内置固化渠道 ID 与统计 ID（1-100 保留段）
pub const CHANNEL_ID: &str = "opencode";
pub const STATS_ID: u32 = 1;

/// 官方 CLI 身份标识（抹平 CLI 与反代差异，享受官方正常会话配额）
const CLI_USER_AGENT: &str = "opencode/1.18.18/cli";
/// 非 OpenCode 渠道的默认网关身份
pub const GATEWAY_USER_AGENT: &str = "OpenHub-Gateway/0.3.0";

/// 判断渠道是否为 OpenCode 渠道
pub fn is_opencode_channel(channel: &ChannelConfig) -> bool {
    channel.id == CHANNEL_ID
        || channel.protocol.eq_ignore_ascii_case(CHANNEL_ID)
        || channel
            .alias
            .as_deref()
            .map_or(false, |a| a.eq_ignore_ascii_case(CHANNEL_ID))
        || channel.base_url.contains("opencode.ai")
        || channel.name.to_lowercase().contains("opencode")
}

/// 渠道/出网目标是否命中 OpenCode 官方通道（base_url 特征兜底）。
/// dispatcher 出网与 router 模型探测共用同一判定口径。
pub fn matches_channel_or_url(channel: &ChannelConfig, url: &str) -> bool {
    is_opencode_channel(channel) || url.contains("opencode.ai")
}

pub fn strip_opencode_prefix(model: &str) -> &str {
    model.strip_prefix("opencode/").unwrap_or(model)
}

/// 判断是否为 OpenCode 官方免费模型（除 big-pickle 外均包含 free）
pub fn is_free_opencode_model(model: &str) -> bool {
    let lower = model.to_lowercase();
    let name = strip_opencode_prefix(&lower);
    name == "big-pickle" || name.contains("free")
}

/// 校验请求的模型在目标渠道上是否合法可用
/// （例如在未配置 Key 的 OpenCode 免费渠道上拦截付费模型）
pub fn check_model_channel_compatibility(
    channel: &ChannelConfig,
    model_to_send: &str,
    channel_api_key: &str,
) -> Result<(), String> {
    if is_opencode_channel(channel)
        && channel_api_key.is_empty()
        && !is_free_opencode_model(model_to_send)
    {
        return Err(format!(
            "模型 '{model_to_send}' 为 OpenCode 付费模型，当前未配置 API Key。请使用官方免费模型（如 deepseek-v4-flash-free, mimo-v2.5-free, big-pickle 等）或在渠道设置中配置 API Key。"
        ));
    }
    Ok(())
}

/// OpenCode 官方 CLI 身份与会话请求头组（供出网请求注入）
pub(crate) fn cli_identity_header_pairs(
    session_seed: &str,
    attempt_req_id: &str,
) -> Vec<(&'static str, String)> {
    let session_id = format!("sess_{}", session_seed.replace('-', ""));
    vec![
        ("User-Agent", CLI_USER_AGENT.to_string()),
        ("x-opencode-client", "cli".to_string()),
        ("x-opencode-session", session_id),
        ("x-opencode-project", "proj_openhub_gateway".to_string()),
        ("x-opencode-request", attempt_req_id.to_string()),
    ]
}

pub(crate) fn apply_cli_identity_headers(
    mut builder: reqwest::RequestBuilder,
    session_seed: &str,
    attempt_req_id: &str,
) -> reqwest::RequestBuilder {
    for (k, v) in cli_identity_header_pairs(session_seed, attempt_req_id) {
        builder = builder.header(k, v);
    }
    builder
}

/// 模型列表探测请求的身份头：官方渠道模拟 CLI，其余渠道用网关默认 UA
pub(crate) fn apply_models_probe_identity(
    mut builder: reqwest::RequestBuilder,
    channel: &ChannelConfig,
    base_url: &str,
) -> reqwest::RequestBuilder {
    if matches_channel_or_url(channel, base_url) {
        builder = builder
            .header("User-Agent", CLI_USER_AGENT)
            .header("x-opencode-client", "cli");
    } else {
        builder = builder.header("User-Agent", GATEWAY_USER_AGENT);
    }
    builder
}

/// 内置固化：别名固定为 opencode（网关模型前缀依赖它）、协议固定、统计 ID 固化。
/// 由 config sanitize 在每次加载/保存时强制执行。
pub fn pin_channel_config(ch: &mut ChannelConfig) {
    if ch.id == CHANNEL_ID {
        ch.name = "OpenCode 免费".to_string();
        ch.alias = None;
        ch.protocol = "openai".to_string();
        ch.stats_id = Some(STATS_ID);
    }
}

/// 判定 OpenCode 成功响应（2xx）是否为「空内容」——官方已知缺陷：返回 200 但无任何有效负载。
///
/// 视为空内容：
/// - 响应体为空字节
/// - JSON 无 `choices` 键（如伪装成 200 的错误对象）
/// - `choices` 为空数组
/// - 首个 choice 的 message 既无正文、也无工具调用、也无思考内容。
///   注意：纯思考（reasoning_content 非空、无正文）视为有效负载——纯推理
///   模型可能整段只输出思考，若判空会触发重试并最终 400。
///
/// 不视为空内容：非 JSON 负载（HTML 错误页/纯文本等，交由上层按原样透传排查）。
pub fn is_empty_success_payload(body: &[u8]) -> bool {
    if body.is_empty() {
        return true;
    }
    let Ok(jv) = serde_json::from_slice::<JsonValue>(body) else {
        return false;
    };
    let Some(choices) = jv.get("choices").and_then(JsonValue::as_array) else {
        return true;
    };
    let Some(first) = choices.first() else {
        return true;
    };
    let no_text = first
        .pointer("/message/content")
        .map(|v| v.as_str().map(str::is_empty).unwrap_or(v.is_null()))
        .unwrap_or(true);
    let no_tools = first
        .pointer("/message/tool_calls")
        .and_then(JsonValue::as_array)
        .map(|a| a.is_empty())
        .unwrap_or(true);
    let no_reasoning = first
        .pointer("/message/reasoning_content")
        .or_else(|| first.pointer("/message/reasoning"))
        .map(|v| v.as_str().map(str::is_empty).unwrap_or(v.is_null()))
        .unwrap_or(true);
    no_text && no_tools && no_reasoning
}

#[cfg(test)]
mod opencode_policy_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_payload_detection_covers_reasoning_only_and_blank() {
        // P1-9：纯推理响应（无正文无工具）应视为有效负载，
        // 否则触发重试并最终 400，纯思考模型被误杀
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"","reasoning_content":"思考中"}}],"usage":{"prompt_tokens":1,"completion_tokens":10}}"#.as_bytes();
        assert!(!is_empty_success_payload(body), "仅含思考的响应不是空内容");

        let body = br#"{"choices":[{"message":{"role":"assistant","content":null},"finish_reason":"stop"}]}"#;
        assert!(is_empty_success_payload(body), "真空白响应仍是空内容");

        let body =
            r#"{"choices":[{"message":{"role":"assistant","content":"有正文"}}]}"#.as_bytes();
        assert!(!is_empty_success_payload(body));
    }

    #[test]
    fn cli_identity_headers_shape() {
        let pairs = cli_identity_header_pairs("abc-def-123", "req_9");
        let get = |k: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.as_str())
                .expect(k)
        };
        assert_eq!(get("User-Agent"), "opencode/1.18.18/cli");
        assert_eq!(get("x-opencode-client"), "cli");
        // 会话 ID 必须剥离连字符
        assert_eq!(get("x-opencode-session"), "sess_abcdef123");
        assert_eq!(get("x-opencode-project"), "proj_openhub_gateway");
        assert_eq!(get("x-opencode-request"), "req_9");
    }

    #[test]
    fn free_and_non_free_model_classification() {
        // 免费模型
        assert!(is_free_opencode_model("deepseek-v4-flash-free"));
        assert!(is_free_opencode_model("opencode/deepseek-v4-flash-free"));
        assert!(is_free_opencode_model("big-pickle"));
        assert!(is_free_opencode_model("mimo-v2.5-free"));

        // 付费模型（需携带 Key）
        assert!(!is_free_opencode_model("gpt-4o"));
        assert!(!is_free_opencode_model("claude-sonnet-5"));
    }

    #[test]
    fn paid_model_blocked_only_when_anonymous() {
        let mut ch = serde_json::from_value::<ChannelConfig>(json!({
            "id": "opencode",
            "name": "OpenCode",
            "enabled": true,
            "upstreamUrl": "https://opencode.ai/zen/v1",
        }))
        .expect("渠道可解析");
        assert!(is_opencode_channel(&ch));

        // 匿名模式下付费模型拦截、免费模型放行
        assert!(check_model_channel_compatibility(&ch, "gpt-4o", "").is_err());
        assert!(check_model_channel_compatibility(&ch, "big-pickle", "").is_ok());

        // 配置 Key 后全部放行
        ch.api_key = "sk-x".to_string();
        assert!(check_model_channel_compatibility(&ch, "gpt-4o", "sk-x").is_ok());
    }

    #[test]
    fn pin_config_forces_alias_protocol_and_stats_id() {
        let mut ch = serde_json::from_value::<ChannelConfig>(json!({
            "id": "opencode",
            "name": "任意名称",
            "enabled": true,
            "protocol": "gemini",
            "upstreamUrl": "https://opencode.ai/zen/v1",
            "alias": "custom"
        }))
        .expect("渠道可解析");
        pin_channel_config(&mut ch);
        assert_eq!(ch.alias, None);
        assert_eq!(ch.protocol, "openai");
        assert_eq!(ch.stats_id, Some(STATS_ID));
        // 非 opencode 渠道不受影响
        let mut other = serde_json::from_value::<ChannelConfig>(json!({
            "id": "other",
            "name": "Other",
            "enabled": true,
            "protocol": "gemini",
            "upstreamUrl": "https://x.example/v1",
            "alias": "keep"
        }))
        .expect("渠道可解析");
        pin_channel_config(&mut other);
        assert_eq!(other.alias.as_deref(), Some("keep"));
    }
}
