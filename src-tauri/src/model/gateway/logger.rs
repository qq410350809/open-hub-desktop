use super::types::{current_timestamp, ModelProxyContext, ProxyRequestLog};
use std::sync::atomic::Ordering;

/// 日志中保存的单条报文（请求/响应）最大字符数，防止超大响应撑爆数据库与前端渲染
pub const MAX_LOG_BODY_CHARS: usize = 128 * 1024;

/// 截断超长正文并追加省略标记；空文本返回 None
pub fn cap_log_body(text: String) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().count() <= MAX_LOG_BODY_CHARS {
        return Some(trimmed.to_string());
    }
    let truncated: String = trimmed.chars().take(MAX_LOG_BODY_CHARS).collect();
    Some(format!(
        "{truncated}\n\n…[内容过长已截断，原始长度 {} 字符]",
        trimmed.chars().count()
    ))
}

#[derive(Clone, Debug)]
pub struct ProxyLogParams {
    pub id: String,
    pub path: String,
    pub channel_id: String,
    pub model: String,
    pub stream: bool,
    pub status_code: u16,
    pub duration_ms: u64,
    pub ttft_ms: Option<u64>,
    pub prompt_tokens: Option<u64>,
    pub prompt_cache_hit_tokens: Option<u64>,
    pub prompt_cache_miss_tokens: Option<u64>,
    pub cache_creation_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub error_message: Option<String>,
    pub request_body: Option<String>,
    pub response_body: Option<String>,
    pub node_name: Option<String>,
    /// 统计维度稳定数字 ID（字符串形式）；未设置时日统计回退 channel_id
    pub channel_stats_id: Option<String>,
    /// 发起请求的客户端标识（User-Agent / 端点推断）
    pub client_name: Option<String>,
    /// 客户端原始 User-Agent 请求头（截断保存）
    pub user_agent: Option<String>,
    pub upstream_url: Option<String>,
    /// 客户端会话标识（x-session-id 等）
    pub session_id: Option<String>,
}

impl ProxyLogParams {
    pub fn new_failure(
        id: String,
        path: String,
        channel_id: String,
        model: String,
        stream: bool,
        status_code: u16,
        duration_ms: u64,
        error_message: Option<String>,
        request_body: Option<String>,
        node_name: Option<String>,
    ) -> Self {
        Self {
            id,
            path,
            channel_id,
            model,
            stream,
            status_code,
            duration_ms,
            ttft_ms: None,
            prompt_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
            cache_creation_tokens: None,
            completion_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            error_message,
            request_body,
            response_body: None,
            node_name,
            channel_stats_id: None,
            client_name: None,
            user_agent: None,
            upstream_url: None,
            session_id: None,
        }
    }

    pub fn with_response_body(mut self, body: Option<String>) -> Self {
        self.response_body = body;
        self
    }

    pub fn with_channel_stats_id(mut self, stats_id: Option<String>) -> Self {
        self.channel_stats_id = stats_id;
        self
    }

    pub fn with_client_name(mut self, client_name: Option<String>) -> Self {
        self.client_name = client_name;
        self
    }

    pub fn with_user_agent(mut self, user_agent: Option<String>) -> Self {
        self.user_agent = user_agent;
        self
    }

    #[cfg(test)]
    pub fn with_session_id(mut self, session_id: Option<String>) -> Self {
        self.session_id = session_id;
        self
    }

    pub fn with_upstream_url(mut self, upstream_url: Option<String>) -> Self {
        self.upstream_url = upstream_url;
        self
    }

    pub fn into_log(self) -> ProxyRequestLog {
        ProxyRequestLog {
            id: self.id,
            timestamp: current_timestamp(),
            method: "POST".to_string(),
            path: self.path,
            channel_id: self.channel_id,
            model: self.model,
            stream: self.stream,
            status_code: self.status_code,
            duration_ms: self.duration_ms,
            ttft_ms: self.ttft_ms,
            prompt_tokens: self.prompt_tokens,
            prompt_cache_hit_tokens: self.prompt_cache_hit_tokens,
            prompt_cache_miss_tokens: self.prompt_cache_miss_tokens,
            cache_creation_tokens: self.cache_creation_tokens,
            completion_tokens: self.completion_tokens,
            reasoning_tokens: self.reasoning_tokens,
            total_tokens: self.total_tokens,
            error_message: self.error_message,
            request_body: self.request_body,
            response_body: self.response_body,
            node_name: self.node_name,
            channel_stats_id: self.channel_stats_id,
            client_name: self.client_name,
            user_agent: self.user_agent,
            upstream_url: self.upstream_url,
            session_id: self.session_id,
        }
    }
}

/// 客户端 User-Agent 前缀 → 本地模式同名的来源标识（sourceNameMap 可直接复用）。
/// 命名刻意避开 "unknown" 字样：前端 isKnownSource 会过滤含 unknown 的来源。
const USER_AGENT_SOURCE_PREFIXES: &[(&str, &str)] = &[
    // 本软件进程内直调（AI 分析 / 模型测试等）必须最先识别：
    // 这些请求同样走 /v1/chat/completions 端点，漏判会兜底成 "openai-api"，
    // 在网关 Token 统计里被错误显示成「OpenAI 协议客户端」。
    ("openhub", "openhub"),
    ("claude", "claude"),
    ("codex", "codex"),
    ("cursor", "cursor"),
    ("gemini", "gemini"),
    ("opencode", "opencode"),
    ("kiro", "kiro"),
    ("copilot", "copilot"),
    ("goose", "goose"),
    ("cline", "cline"),
    ("aider", "aider"),
    ("continue", "continue"),
    ("windsurf", "windsurf"),
    ("zcode", "zcode"),
    ("zed", "zed"),
    ("catpawai", "catpawai"),
    ("antigravity", "antigravity"),
    ("openclaw", "openclaw"),
    // DeepSeek CLI（DSH）底层 harness 的 UA，无 dsh 字样
    ("deepseek-harness", "dsh"),
    // Command Code 的 UA 存在驼峰/连字符两种写法
    ("commandcode", "command-code"),
    ("command-code", "command-code"),
    ("dsh", "dsh"),
];

/// 从请求头 User-Agent 与端点路径推断客户端标识。
/// 优先 User-Agent 前缀匹配（与本地模式来源命名一致），退化为按端点协议推断，
/// 最终兜底 "other"（前端显示为「其他客户端」）。
pub fn client_name_from_headers(headers: &axum::http::HeaderMap, path: &str) -> String {
    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    for (prefix, name) in USER_AGENT_SOURCE_PREFIXES {
        if user_agent.contains(prefix) {
            return name.to_string();
        }
    }
    if user_agent.contains("python") || user_agent.contains("node") || user_agent.contains("axios")
    {
        return "sdk".to_string();
    }
    match path {
        // 后缀匹配而非全等：路由还注册了无 /v1 前缀的别名
        // （/messages、/responses、/chat/completions），同样按端点协议归档
        p if p.ends_with("/messages") => "anthropic-api".to_string(),
        p if p.ends_with("/responses") => "responses-api".to_string(),
        // Gemini 原生入口有 /v1/gemini 与 /v1beta 两种前缀（模型名在路径里）
        p if p.contains("/gemini") || p.starts_with("/v1beta") => "gemini-api".to_string(),
        p if p.ends_with("/chat/completions") => "openai-api".to_string(),
        _ => "other".to_string(),
    }
}

/// 从请求头提取原始 User-Agent（截断到 256 字符防滥用）；空/缺失返回 None
pub fn user_agent_from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.chars().take(256).collect())
}

/// 从请求头提取客户端会话标识，供日志按会话聚合排查。
/// 覆盖各客户端的常见约定头：通用 x-session-id、Anthropic 元数据、
/// OpenAI Assistants、Codex 与 OpenCode 的会话头；长度钳制防滥用。
pub fn session_id_from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
    const SESSION_HEADERS: &[&str] = &[
        "x-session-id",
        "session_id",
        "anthropic-metadata-user-id",
        "x-assistant-session-id",
        "openai-conversation-id",
        "x-opencode-session",
        "x-claude-session-id",
    ];
    for name in SESSION_HEADERS {
        if let Some(value) = headers.get(*name).and_then(|v| v.to_str().ok()) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.chars().take(64).collect());
            }
        }
    }
    None
}

/// 记录单次尝试的失败日志，并原子递增 failed_requests 计数器
pub async fn record_attempt_failure(ctx: &ModelProxyContext, params: ProxyLogParams) {
    ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
    ctx.record_log(params.into_log()).await;
}

/// 记录鉴权失败日志并更新总指标
pub async fn record_auth_failure_log(
    ctx: &ModelProxyContext,
    req_id: &str,
    path: &str,
    model: &str,
    stream: bool,
    dur: u64,
    req_body_str: Option<String>,
    client_name: Option<String>,
    user_agent: Option<String>,
) {
    ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);
    // 鉴权失败发生在渠道解析前，沿用既有惯例计入 opencode 通道（含其统计 ID）
    let opencode_stats_id = ctx
        .config
        .read()
        .await
        .channels
        .iter()
        .find(|c| c.id == "opencode")
        .and_then(|c| c.stats_id)
        .map(|v| v.to_string());
    record_attempt_failure(
        ctx,
        ProxyLogParams::new_failure(
            req_id.to_string(),
            path.to_string(),
            "opencode".to_string(),
            model.to_string(),
            stream,
            401,
            dur,
            Some("本地 API Key 鉴权失败 (Unauthorized)".to_string()),
            req_body_str,
            None,
        )
        .with_channel_stats_id(opencode_stats_id)
        .with_client_name(client_name)
        .with_user_agent(user_agent),
    )
    .await;
}

#[cfg(test)]
mod logger_tests {
    use super::*;

    #[test]
    fn cap_log_body_trims_and_truncates() {
        assert!(cap_log_body("  ".to_string()).is_none());
        assert_eq!(
            cap_log_body(" hello ".to_string()).as_deref(),
            Some("hello")
        );

        let long = "a".repeat(MAX_LOG_BODY_CHARS + 10);
        let capped = cap_log_body(long).unwrap();
        assert!(capped.chars().count() < MAX_LOG_BODY_CHARS + 60);
        assert!(capped.contains("内容过长已截断"));
    }

    fn ua(value: &str) -> axum::http::HeaderMap {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::USER_AGENT,
            axum::http::HeaderValue::from_str(value).unwrap(),
        );
        headers
    }

    #[test]
    fn internal_self_calls_are_labeled_openhub() {
        // 进程内直调的调用方（AI 分析）走同一 Chat 端点，
        // 但都是本软件自主请求，必须识别为 "openhub" 而非端点兜底的 "openai-api"。
        for agent in ["OpenHub-TokenMapping"] {
            assert_eq!(
                client_name_from_headers(&ua(agent), "/v1/chat/completions"),
                "openhub",
                "User-Agent={agent}"
            );
        }
    }

    #[test]
    fn failure_params_keep_session_id() {
        let log = ProxyLogParams::new_failure(
            "req-1".into(),
            "/v1/chat/completions".into(),
            "opencode".into(),
            "gpt-4".into(),
            false,
            401,
            12,
            Some("unauthorized".into()),
            None,
            None,
        )
        .with_session_id(Some("sess_abc".into()))
        .into_log();
        assert_eq!(log.session_id.as_deref(), Some("sess_abc"));
    }

    #[test]
    fn user_agent_header_captured_and_capped() {
        assert_eq!(user_agent_from_headers(&axum::http::HeaderMap::new()), None);
        assert_eq!(
            user_agent_from_headers(&ua("  claude-cli/2.1.0 ")).as_deref(),
            Some("claude-cli/2.1.0")
        );
        let long_ua = "a".repeat(400);
        assert_eq!(
            user_agent_from_headers(&ua(&long_ua))
                .map(|v| v.chars().count())
                .unwrap_or(0),
            256
        );
    }

    #[test]
    fn endpoint_fallback_covers_alias_and_v1beta_paths() {
        // 无 /v1 前缀的别名路径与 /v1beta Gemini 原生路径，兜底标签必须归到正确协议
        assert_eq!(
            client_name_from_headers(&axum::http::HeaderMap::new(), "/chat/completions"),
            "openai-api"
        );
        assert_eq!(
            client_name_from_headers(&axum::http::HeaderMap::new(), "/messages"),
            "anthropic-api"
        );
        assert_eq!(
            client_name_from_headers(
                &axum::http::HeaderMap::new(),
                "/v1beta/models/gemini-2.5-flash:generateContent"
            ),
            "gemini-api"
        );
        assert_eq!(
            client_name_from_headers(
                &axum::http::HeaderMap::new(),
                "/v1/gemini/models/gemini-2.5-flash:streamGenerateContent"
            ),
            "gemini-api"
        );
    }

    #[test]
    fn locally_supported_clients_are_recognized() {
        for (agent, expected) in [
            (
                "deepseek-harness/0.1.5-rc.2 (+https://github.com/deepseek-ai/deepseek-harness)",
                "dsh",
            ),
            ("dsh/0.4.2", "dsh"),
            ("CommandCode/1.0", "command-code"),
            ("command-code/1.0", "command-code"),
            ("openclaw/0.9", "openclaw"),
        ] {
            assert_eq!(
                client_name_from_headers(&ua(agent), "/v1/chat/completions"),
                expected,
                "User-Agent={agent}"
            );
        }
    }

    #[test]
    fn external_clients_keep_protocol_or_agent_labels() {
        // 已知 agent 的 User-Agent 仍按前缀识别
        assert_eq!(
            client_name_from_headers(&ua("claude-cli/2.1.0"), "/v1/messages"),
            "claude"
        );
        assert_eq!(
            client_name_from_headers(&ua("codex/0.42"), "/v1/chat/completions"),
            "codex"
        );
        // 未知客户端按端点协议兜底（真实外部 OpenAI 协议客户端不受影响）
        assert_eq!(
            client_name_from_headers(&ua("some-sdk/1.0"), "/v1/chat/completions"),
            "openai-api"
        );
        assert_eq!(
            client_name_from_headers(&ua("some-sdk/1.0"), "/v1/responses"),
            "responses-api"
        );
        // 无 User-Agent 的匿名请求仍按端点兜底
        assert_eq!(
            client_name_from_headers(&axum::http::HeaderMap::new(), "/v1/messages"),
            "anthropic-api"
        );
    }
}
