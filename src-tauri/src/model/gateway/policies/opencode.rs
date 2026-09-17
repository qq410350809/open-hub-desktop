//! OpenCode 官方免费渠道的个性化策略集中地。
//!
//! 该渠道存在大量与通用网关语义无关的特殊行为：CLI 身份伪装、匿名免费模型
//! 白名单、200 空内容缺陷容错等。此前这些逻辑散落在 balancer / dispatcher /
//! router / config 各处，本模块将其归拢为单一入口 —— 修改 OpenCode 行为
//! 只改这里；新增其他特殊渠道时可参照此模式建立同级策略文件。
//!
//! 免费层身份校验（上游 2026-09 收紧，实测结论）：
//! - `User-Agent` 必须以 `opencode/<版本>` 开头且版本 ≥ 1.17.0，否则 403
//!   「OpenCode's free tier can only be used from within OpenCode」（低于版本下限为 426）；
//! - `x-opencode-session` 必须是 `ses_` + 12 位小写十六进制 + 14 位 Base62（总长 26），
//!   长度、前缀、大小写、字符集任一不符同样 403；
//! - `x-opencode-client` / `x-opencode-project` / `x-opencode-request` 不参与校验，
//!   但取值保持与真实 CLI 一致，避免后续再加形状校验时二次失配。

use super::super::types::ChannelConfig;
use serde_json::Value as JsonValue;

/// 内置固化渠道 ID 与统计 ID（1-100 保留段）
pub const CHANNEL_ID: &str = "opencode";
pub const STATS_ID: u32 = 1;

/// 官方 CLI 版本：上游免费层解析 User-Agent 中的版本号，低于 1.17.0 直接返回
/// 426 UpgradeRequired；UA 缺失或不是 `opencode/<版本>` 前缀则返回 403。
/// 常量值同步官方 CLI（升级后可随之上调），不得低于上游下限。
const CLI_VERSION: &str = "1.18.30";
/// 官方 CLI UA 的运行时版本段（真实 CLI 由 Vercel AI SDK + Bun 发出）
const CLI_AI_SDK_VERSION: &str = "4.0.23";
const CLI_BUN_VERSION: &str = "1.3.14";
/// 非 OpenCode 渠道的默认网关身份
pub const GATEWAY_USER_AGENT: &str = "OpenHub-Gateway/0.3.0";

/// 官方 CLI User-Agent：`opencode/<版本> ai-sdk/provider-utils/<版本> runtime/bun/<版本>`
pub(crate) fn cli_user_agent() -> String {
    format!(
        "opencode/{CLI_VERSION} ai-sdk/provider-utils/{CLI_AI_SDK_VERSION} runtime/bun/{CLI_BUN_VERSION}"
    )
}

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

/// 判断是否为具备推理能力（思考/思维链）的 OpenCode 模型
pub fn is_opencode_reasoning_model(model: &str) -> bool {
    let lower = model.to_lowercase();
    let name = strip_opencode_prefix(&lower);
    name.contains("muse-spark")
        || name.contains("mimo")
        || name.contains("deepseek")
        || name.contains("reasoning")
        || name.contains("thinking")
}

/// 判断指定 OpenCode 模型是否必须走 OpenAI Responses 协议（如 muse-spark）
pub fn is_opencode_responses_model(model: &str) -> bool {
    let lower = model.to_lowercase();
    let name = strip_opencode_prefix(&lower);
    name.contains("muse-spark") || name.contains("muse")
}

/// OpenCode 模型的默认上游协议覆盖（仅对 OpenCode 渠道生效）
pub fn target_protocol_for_opencode_model(
    model: &str,
) -> Option<crate::model::gateway::egress::TargetProtocol> {
    if is_opencode_responses_model(model) {
        Some(crate::model::gateway::egress::TargetProtocol::OpenAiResponses)
    } else {
        None
    }
}

/// OpenCode 推理模型缺省思考档位与 Token 预算
pub fn default_reasoning_for_model(
    model: &str,
) -> Option<crate::model::gateway::ir::ReasoningConfig> {
    if is_opencode_reasoning_model(model) {
        Some(crate::model::gateway::ir::ReasoningConfig {
            effort: Some("high".to_string()),
            budget_tokens: Some(16384),
        })
    } else {
        None
    }
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

/// 官方 CLI 标识主体长度：12 位时间戳段 + 14 位随机段
const ID_TS_HEX_LEN: usize = 12;
const ID_RAND_LEN: usize = 14;
/// Base62 字母表（数字 + 大写 + 小写），与官方 CLI 随机段字符集一致
const BASE62: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// 生成官方 CLI 形状的标识主体：12 位小写十六进制（毫秒时间戳）+ 14 位 Base62。
/// 时间戳段取当前毫秒，保持单调、贴近官方 ID 的可读前缀；随机段由 seed 派生，
/// 同一 seed 的随机段稳定（跨毫秒生成时仅时间戳段前进）。
/// 上游只校验形状，不校验时间戳真实性与随机段内容。
fn ascending_id_body(seed: &str) -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut body = format!(
        "{:0width$x}",
        millis & 0x0000_FFFF_FFFF_FFFF,
        width = ID_TS_HEX_LEN
    );
    let mut state = fnv1a_64(seed.as_bytes());
    for _ in 0..ID_RAND_LEN {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        body.push(BASE62[(state >> 33) as usize % BASE62.len()] as char);
    }
    body
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 会话 ID：`ses_` + 26 位标识主体。前缀必须是 `ses_`（`sess_` 等变体一律被拒），
/// 主体长度、字符集任一不符都会触发 403 免费层拦截。
pub(crate) fn cli_session_id(seed: &str) -> String {
    format!("ses_{}", ascending_id_body(seed))
}

/// 消息 ID：`msg_` + 26 位标识主体，对应真实 CLI 的 x-opencode-request 取值
pub(crate) fn cli_message_id(seed: &str) -> String {
    format!("msg_{}", ascending_id_body(seed))
}

/// OpenCode 官方 CLI 身份与会话请求头组（供出网请求注入）
pub(crate) fn cli_identity_header_pairs(
    session_seed: &str,
    attempt_req_id: &str,
) -> Vec<(&'static str, String)> {
    vec![
        ("User-Agent", cli_user_agent()),
        ("x-opencode-client", "cli".to_string()),
        ("x-opencode-session", cli_session_id(session_seed)),
        // 真实 CLI 在非项目目录下发送 global（项目内为项目哈希）
        ("x-opencode-project", "global".to_string()),
        ("x-opencode-request", cli_message_id(attempt_req_id)),
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
            .header("User-Agent", cli_user_agent())
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

/// 上游免费层对会话 ID 的形状要求（实测 2026-09）：
/// `ses_` + 12 位小写十六进制 + 14 位 Base62，总长固定 26。
/// 形状校验器同时供出网链路测试（tests/mod.rs）断言真实发出的请求头。
#[cfg(test)]
pub(crate) fn upstream_accepts_session_id(id: &str) -> bool {
    let Some(body) = id.strip_prefix("ses_") else {
        return false;
    };
    if body.len() != ID_TS_HEX_LEN + ID_RAND_LEN {
        return false;
    }
    let (ts, rand) = body.split_at(ID_TS_HEX_LEN);
    ts.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && rand.bytes().all(|b| b.is_ascii_alphanumeric())
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
        // UA 版本段是硬性下限：低于 1.17.0 上游直接 426
        let ua = get("User-Agent");
        assert!(
            ua.starts_with("opencode/"),
            "UA 必须带 opencode/ 前缀: {ua}"
        );
        let version = ua
            .trim_start_matches("opencode/")
            .split_whitespace()
            .next()
            .expect("UA 含版本段");
        let segments: Vec<u32> = version.split('.').filter_map(|s| s.parse().ok()).collect();
        assert!(
            segments.len() >= 3 && (segments[0], segments[1]) >= (1, 17),
            "UA 版本需 ≥ 1.17.0: {version}"
        );
        assert_eq!(get("x-opencode-client"), "cli");
        assert_eq!(get("x-opencode-project"), "global");
        // 会话 ID 是免费层的实际闸门：形状不符（如旧的 sess_ 前缀）会被 403 拦截
        let session = get("x-opencode-session");
        assert!(
            upstream_accepts_session_id(session),
            "会话 ID 形状不被上游接受: {session}"
        );
        assert!(get("x-opencode-request").starts_with("msg_"));
    }

    #[test]
    fn session_and_message_ids_follow_cli_shape_and_derive_random_segment_from_seed() {
        let session = cli_session_id("req_18c1f0a9b");
        assert!(
            upstream_accepts_session_id(&session),
            "会话 ID 形状: {session}"
        );

        // 时间戳段随毫秒前进，随机段由 seed 派生：同 seed 稳定、不同 seed 不撞车
        let rand_segment = |id: &str| id[4 + ID_TS_HEX_LEN..].to_string();
        let again = cli_session_id("req_18c1f0a9b");
        assert_eq!(rand_segment(&session), rand_segment(&again));
        assert_ne!(
            rand_segment(&session),
            rand_segment(&cli_session_id("req_18c1f0a9c"))
        );

        let message = cli_message_id("req_18c1f0a9b");
        assert!(message.starts_with("msg_"));
        assert_eq!(
            message.trim_start_matches("msg_").len(),
            ID_TS_HEX_LEN + ID_RAND_LEN
        );
        // 会话与消息共用同一主体生成器：同 seed 随机段一致，靠前缀区分身份类型
        assert_eq!(rand_segment(&session), rand_segment(&message));
        assert_ne!(session, message);
    }

    #[test]
    fn upstream_rejection_cases_are_covered_by_shape_check() {
        // 旧实现产物：ses_ 前缀写成 sess_、超长十六进制主体 —— 上游实测 403 的形态，
        // 形状校验必须把它们判为不合法，避免回归
        assert!(!upstream_accepts_session_id("sess_abcdef123"));
        assert!(!upstream_accepts_session_id(&format!(
            "ses_{}",
            "a".repeat(32)
        )));
        assert!(!upstream_accepts_session_id(
            "ses_D841B79557AAE959A4C2A3A4DB"
        ));
        assert!(!upstream_accepts_session_id(
            "ses_d841b79557aae959a4c2a3a4d"
        ));
    }

    /// 真实上游身份校验（**需联网**，默认忽略）：把策略层产出的身份头直发 zen 免费层，
    /// 确认不再触发 403「free tier can only be used from within OpenCode」/426 版本过低。
    /// 模型可用性随上游波动，故只断言身份闸门本身，不断言业务成功。
    ///
    /// ```sh
    /// cargo test --lib opencode_identity_passes_upstream_free_tier_gate -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore]
    async fn opencode_identity_passes_upstream_free_tier_gate() {
        let pairs = cli_identity_header_pairs("req_e2e_probe", "req_e2e_probe");
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("构建 HTTP 客户端");
        let mut request = client
            .post("https://opencode.ai/zen/v1/chat/completions")
            .header("Content-Type", "application/json");
        for (name, value) in &pairs {
            request = request.header(*name, value.clone());
        }
        let resp = request
            .json(&serde_json::json!({
                "model": "big-pickle",
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 8,
            }))
            .send()
            .await
            .expect("请求上游失败");
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        eprintln!("上游响应 {status}: {}", &body[..body.len().min(200)]);

        assert!(
            !body.contains("free tier can only be used from within OpenCode"),
            "身份头未通过免费层校验: {body}"
        );
        assert_ne!(
            status,
            reqwest::StatusCode::UPGRADE_REQUIRED,
            "UA 版本低于上游下限: {body}"
        );
        assert_ne!(
            status,
            reqwest::StatusCode::UNAUTHORIZED,
            "身份被拒: {body}"
        );
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

    #[test]
    fn opencode_muse_spark_routes_to_responses_protocol_while_others_stay_chat() {
        assert!(is_opencode_responses_model(
            "muse-spark-1.3-contributor-free"
        ));
        assert!(is_opencode_responses_model("opencode/muse-spark-1.2"));
        assert!(!is_opencode_responses_model("mimo-v2.5-free"));
        assert!(!is_opencode_responses_model("deepseek-v4-flash-free"));

        let opencode_ch = serde_json::from_value::<ChannelConfig>(json!({
            "id": "opencode",
            "name": "OpenCode",
            "enabled": true,
            "protocol": "openai",
            "upstreamUrl": "https://opencode.ai/zen/v1",
        }))
        .unwrap();

        assert_eq!(
            opencode_ch.target_protocol_for("muse-spark-1.3-contributor-free"),
            crate::model::gateway::egress::TargetProtocol::OpenAiResponses
        );
        assert_eq!(
            opencode_ch.target_protocol_for("mimo-v2.5-free"),
            crate::model::gateway::egress::TargetProtocol::OpenAiChat
        );

        // 严格隔离：非 OpenCode 渠道即便模型名称带有 muse-spark，也不受 OpenCode 规则影响
        let other_ch = serde_json::from_value::<ChannelConfig>(json!({
            "id": "custom",
            "name": "Custom Channel",
            "enabled": true,
            "protocol": "openai",
            "upstreamUrl": "https://custom.ai/v1",
        }))
        .unwrap();

        assert_eq!(
            other_ch.target_protocol_for("muse-spark-1.3-contributor-free"),
            crate::model::gateway::egress::TargetProtocol::OpenAiChat
        );
    }
}
