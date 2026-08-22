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
    Some(format!("{truncated}\n\n…[内容过长已截断，原始长度 {} 字符]", trimmed.chars().count()))
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
    pub completion_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub error_message: Option<String>,
    pub request_body: Option<String>,
    pub response_body: Option<String>,
    pub node_name: Option<String>,
    /// 统计维度稳定数字 ID（字符串形式）；未设置时日统计回退 channel_id
    pub channel_stats_id: Option<String>,
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
            completion_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            error_message,
            request_body,
            response_body: None,
            node_name,
            channel_stats_id: None,
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
            completion_tokens: self.completion_tokens,
            reasoning_tokens: self.reasoning_tokens,
            total_tokens: self.total_tokens,
            error_message: self.error_message,
            request_body: self.request_body,
            response_body: self.response_body,
            node_name: self.node_name,
            channel_stats_id: self.channel_stats_id,
        }
    }
}

/// 记录单次尝试的失败日志，并原子递增 failed_requests 计数器
pub async fn record_attempt_failure(
    ctx: &ModelProxyContext,
    params: ProxyLogParams,
) {
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
        .with_channel_stats_id(opencode_stats_id),
    ).await;
}

#[cfg(test)]
mod logger_tests {
    use super::*;

    #[test]
    fn cap_log_body_trims_and_truncates() {
        assert!(cap_log_body("  ".to_string()).is_none());
        assert_eq!(cap_log_body(" hello ".to_string()).as_deref(), Some("hello"));

        let long = "a".repeat(MAX_LOG_BODY_CHARS + 10);
        let capped = cap_log_body(long).unwrap();
        assert!(capped.chars().count() < MAX_LOG_BODY_CHARS + 60);
        assert!(capped.contains("内容过长已截断"));
    }
}
