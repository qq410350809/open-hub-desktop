use super::types::{current_timestamp, ModelProxyContext, ProxyRequestLog};
use std::sync::atomic::Ordering;

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
        }
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
        ),
    ).await;
}
