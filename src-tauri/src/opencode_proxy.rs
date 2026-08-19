use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::StreamExt;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::Manager;
use tokio::sync::RwLock;
use tower_http::cors::{Any, CorsLayer};

pub const DEFAULT_OPENCODE_PROXY_PORT: u16 = 8088;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelConfig {
    pub id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub protocol: String,
    pub upstream_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub use_proxy_pool: bool,
}

fn default_channels() -> Vec<ChannelConfig> {
    vec![ChannelConfig {
        id: "opencode".to_string(),
        name: "OpenCode".to_string(),
        description: "OpenCode 官方 Public 免费直连通道，免 Key 访问在线优质编码与推理模型".to_string(),
        enabled: true,
        protocol: "opencode".to_string(),
        upstream_url: "https://opencode.ai/zen/v1".to_string(),
        api_key: "public".to_string(),
        use_proxy_pool: false,
    }]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpencodeProxyConfig {
    pub enabled: bool,
    pub port: u16,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_channels")]
    pub channels: Vec<ChannelConfig>,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub record_request_body: bool,
    /// 每次请求失败后的重试次数（默认 0 = 报错直接返回）。
    /// 使用代理池轮询的渠道：失败节点作废移至队尾，按节点队列顺序取下一个节点，最多不超过可用节点数。
    #[serde(default)]
    pub max_retries: u32,
}

fn default_timeout_seconds() -> u64 {
    300
}

impl Default for OpencodeProxyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            port: DEFAULT_OPENCODE_PROXY_PORT,
            api_key: String::new(),
            channels: default_channels(),
            timeout_seconds: default_timeout_seconds(),
            record_request_body: false,
            max_retries: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyRequestLog {
    pub id: String,
    pub timestamp: String,
    pub method: String,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpencodeProxyStatus {
    pub running: bool,
    pub port: u16,
    pub url: String,
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub uptime_seconds: u64,
    pub models_count: usize,
    pub channels_count: usize,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_reasoning_tokens: u64,
    pub total_reasoning_requests: u64,
    pub total_cache_hit_tokens: u64,
    pub total_tokens: u64,
    pub today_total_tokens: u64,
}

#[derive(Default)]
pub struct OpencodeProxyMetrics {
    pub total_requests: AtomicU64,
    pub successful_requests: AtomicU64,
    pub failed_requests: AtomicU64,
    pub total_prompt_tokens: AtomicU64,
    pub total_completion_tokens: AtomicU64,
    pub total_reasoning_tokens: AtomicU64,
    pub total_reasoning_requests: AtomicU64,
    pub total_cache_hit_tokens: AtomicU64,
    pub total_tokens: AtomicU64,
}

#[derive(Clone)]
pub struct OpencodeProxyContext {
    pub config: Arc<RwLock<OpencodeProxyConfig>>,
    pub metrics: Arc<OpencodeProxyMetrics>,
    pub started_at: Arc<RwLock<Option<Instant>>>,
    pub cached_models: Arc<RwLock<Vec<String>>>,
    pub cached_models_updated_at: Arc<RwLock<Option<Instant>>>,
    pub default_http_client: reqwest::Client,
    pub app_handle: Arc<RwLock<Option<tauri::AppHandle>>>,
    /// 当前活跃出口节点下标（原子，跨请求粘性保持）；失败时自动移到下一节点
    pub active_egress_idx: Arc<AtomicUsize>,
}

static REQ_ID_SEQ: AtomicU64 = AtomicU64::new(1);

fn generate_req_id() -> String {
    let seq = REQ_ID_SEQ.fetch_add(1, Ordering::Relaxed);
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("req_{:x}_{:x}", now_nanos, seq)
}

impl OpencodeProxyContext {
    pub async fn record_log(&self, log: ProxyRequestLog) {
        if let Some(pt) = log.prompt_tokens {
            self.metrics.total_prompt_tokens.fetch_add(pt, Ordering::Relaxed);
        }
        if let Some(ct) = log.completion_tokens {
            self.metrics.total_completion_tokens.fetch_add(ct, Ordering::Relaxed);
        }
        if let Some(rt) = log.reasoning_tokens {
            self.metrics.total_reasoning_tokens.fetch_add(rt, Ordering::Relaxed);
            if rt > 0 {
                self.metrics.total_reasoning_requests.fetch_add(1, Ordering::Relaxed);
            }
        }
        if let Some(hit) = log.prompt_cache_hit_tokens {
            self.metrics.total_cache_hit_tokens.fetch_add(hit, Ordering::Relaxed);
        }
        let tot = log.total_tokens.unwrap_or_else(|| {
            log.prompt_tokens.unwrap_or(0) + log.completion_tokens.unwrap_or(0)
        });
        if tot > 0 {
            self.metrics.total_tokens.fetch_add(tot, Ordering::Relaxed);
        }

        let app_opt = self.app_handle.read().await.clone();
        if let Some(app) = app_opt {
            let database = app.state::<crate::models::Database>();
            let _ = (|| -> Result<(), rusqlite::Error> {
                let conn = database.0.lock().map_err(|_| rusqlite::Error::InvalidQuery)?;
                let now_ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;
                conn.execute(
                    "INSERT OR REPLACE INTO opencode_proxy_logs (
                        id, timestamp, method, path, channel_id, model, stream,
                        status_code, duration_ms, ttft_ms, prompt_tokens, prompt_cache_hit_tokens,
                        prompt_cache_miss_tokens, completion_tokens, reasoning_tokens, total_tokens,
                        error_message, request_body, response_body, node_name, created_at
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
                    rusqlite::params![
                        log.id,
                        log.timestamp,
                        log.method,
                        log.path,
                        log.channel_id,
                        log.model,
                        if log.stream { 1 } else { 0 },
                        log.status_code as i64,
                        log.duration_ms as i64,
                        log.ttft_ms.map(|v| v as i64),
                        log.prompt_tokens.map(|v| v as i64),
                        log.prompt_cache_hit_tokens.map(|v| v as i64),
                        log.prompt_cache_miss_tokens.map(|v| v as i64),
                        log.completion_tokens.map(|v| v as i64),
                        log.reasoning_tokens.map(|v| v as i64),
                        log.total_tokens.map(|v| v as i64),
                        log.error_message,
                        log.request_body,
                        log.response_body,
                        log.node_name,
                        now_ts,
                    ],
                )?;
                conn.execute(
                    "DELETE FROM opencode_proxy_logs WHERE id NOT IN (
                        SELECT id FROM opencode_proxy_logs ORDER BY created_at DESC, rowid DESC LIMIT 1000
                    )",
                    [],
                )?;
                Ok(())
            })();
        }
    }
}

pub struct OpencodeProxyState {
    pub context: OpencodeProxyContext,
    pub shutdown_tx: Arc<tokio::sync::Mutex<Option<tokio::sync::broadcast::Sender<()>>>>,
    pub current_port: Arc<RwLock<u16>>,
    pub is_running: Arc<RwLock<bool>>,
}

impl Default for OpencodeProxyState {
    fn default() -> Self {
        Self::new()
    }
}

impl OpencodeProxyState {
    pub fn new() -> Self {
        Self::new_with_app(None)
    }

    pub fn new_with_app(app_handle: Option<tauri::AppHandle>) -> Self {
        let default_http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .unwrap_or_default();

        let context = OpencodeProxyContext {
            config: Arc::new(RwLock::new(OpencodeProxyConfig::default())),
            metrics: Arc::new(OpencodeProxyMetrics::default()),
            started_at: Arc::new(RwLock::new(None)),
            cached_models: Arc::new(RwLock::new(Vec::new())),
            cached_models_updated_at: Arc::new(RwLock::new(None)),
            default_http_client,
            app_handle: Arc::new(RwLock::new(app_handle)),
            active_egress_idx: Arc::new(AtomicUsize::new(0)),
        };

        Self {
            context,
            shutdown_tx: Arc::new(tokio::sync::Mutex::new(None)),
            current_port: Arc::new(RwLock::new(DEFAULT_OPENCODE_PROXY_PORT)),
            is_running: Arc::new(RwLock::new(false)),
        }
    }
}

fn current_timestamp() -> String {
    let now = chrono::Local::now();
    now.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn format_upstream_error_message(status: u16, error_body: &str) -> String {
    if let Ok(jv) = serde_json::from_str::<JsonValue>(error_body) {
        if let Some(msg) = jv.pointer("/error/message").and_then(JsonValue::as_str) {
            return format!("HTTP {status} 接口错误: {msg}");
        }
        if let Some(msg) = jv.pointer("/message").and_then(JsonValue::as_str) {
            return format!("HTTP {status} 接口错误: {msg}");
        }
    }

    if status == 429 || error_body.contains("Rate limit exceeded") {
        return "HTTP 429 频次受限: OpenCode 触发了单 IP 请求频次限制。已自动尝试切换下一个健康节点。".to_string();
    }
    if error_body.contains("400 Bad Request") && error_body.contains("cloudflare") {
        return "HTTP 400 Cloudflare 拦截: 上游网关拒绝请求（请检查模型名称是否支持，或尝试开启/关闭代理池轮询）".to_string();
    }
    if error_body.contains("502 Bad Gateway") && error_body.contains("cloudflare") {
        return "HTTP 502 Cloudflare 上游不可达: 当前节点连接 OpenCode 服务器超时".to_string();
    }
    if error_body.contains("503 Service Temporarily Unavailable") {
        return "HTTP 503 上游服务繁忙".to_string();
    }
    if error_body.contains("<html>") {
        return format!("HTTP {status} 上游返回 HTML 错误页面");
    }

    format!("HTTP {status}: {error_body}")
}

/// 记录节点自动切换事件：请求在某节点失败并重试下一节点时，写入一条独立的错误日志，
/// 让用户能从日志列表中看到切换原因（该请求的最终结果仍由成功/失败日志另行记录）。
async fn record_failover_event(
    ctx: &OpencodeProxyContext,
    req_id: &str,
    path: &str,
    model: &str,
    is_stream: bool,
    status_code: u16,
    error_message: String,
    duration_ms: u64,
    req_body_str: Option<String>,
    cand_id: &str,
) {
    ctx.record_log(ProxyRequestLog {
        id: req_id.to_string(),
        timestamp: current_timestamp(),
        method: "POST".to_string(),
        path: path.to_string(),
        channel_id: "opencode".to_string(),
        model: model.to_string(),
        stream: is_stream,
        status_code,
        duration_ms,
        ttft_ms: None,
        prompt_tokens: None,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
        completion_tokens: None,
        reasoning_tokens: None,
        total_tokens: None,
        error_message: Some(error_message),
        request_body: req_body_str,
        response_body: None,
        node_name: Some(get_node_display_name(ctx, cand_id).await),
    })
    .await;
}

fn clean_sse_stream(
    ctx: OpencodeProxyContext,
    req_id: String,
    start_time: Instant,
    path: String,
    model: String,
    req_body_str: Option<String>,
    node_name: Option<String>,
    stream: impl futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin + Send + 'static,
) -> impl futures_util::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static {
    async_stream::stream! {
        let mut stream = stream;
        let mut buffer = String::new();
        let mut finished = false;
        let mut collected_content = String::new();
        let mut collected_reasoning = String::new();
        let mut ttft_ms: Option<u64> = None;

        let mut prompt_tokens: Option<u64> = None;
        let mut prompt_cache_hit: Option<u64> = None;
        let mut prompt_cache_miss: Option<u64> = None;
        let mut completion_tokens: Option<u64> = None;
        let mut reasoning_tokens: Option<u64> = None;
        let mut total_tokens: Option<u64> = None;

        while let Some(item) = stream.next().await {
            if finished {
                break;
            }
            match item {
                Ok(chunk) => {
                    if let Ok(text) = std::str::from_utf8(&chunk) {
                        buffer.push_str(text);

                        while let Some(pos) = buffer.find("\n\n") {
                            let block = buffer[..pos].to_string();
                            buffer = buffer[pos + 2..].to_string();

                            let trimmed = block.trim();
                            if trimmed.is_empty() {
                                continue;
                            }

                            if trimmed == "data: [DONE]" {
                                finished = true;
                                yield Ok(bytes::Bytes::from("data: [DONE]\n\n"));
                                break;
                            }

                            if let Some(json_payload) = trimmed.strip_prefix("data: ") {
                                if let Ok(mut val) = serde_json::from_str::<JsonValue>(json_payload) {
                                    // 提取 usage 统计
                                    if let Some(u) = val.get("usage").and_then(JsonValue::as_object) {
                                        if let Some(pt) = u.get("prompt_tokens").and_then(JsonValue::as_u64) {
                                            prompt_tokens = Some(pt);
                                        }
                                        if let Some(hit) = u.get("prompt_cache_hit_tokens")
                                            .or_else(|| u.get("prompt_tokens_details").and_then(|d| d.get("cached_tokens")))
                                            .or_else(|| u.get("prompt_tokens_details").and_then(|d| d.get("cache_read")))
                                            .or_else(|| u.get("cache_read_input_tokens"))
                                            .or_else(|| u.get("cached_tokens"))
                                            .or_else(|| u.get("cache_hit_tokens"))
                                            .and_then(JsonValue::as_u64)
                                        {
                                            prompt_cache_hit = Some(hit);
                                        }
                                        if let Some(miss) = u.get("prompt_cache_miss_tokens").and_then(JsonValue::as_u64) {
                                            prompt_cache_miss = Some(miss);
                                        }
                                        if let Some(ct) = u.get("completion_tokens").and_then(JsonValue::as_u64) {
                                            completion_tokens = Some(ct);
                                        }
                                        if let Some(rt) = u.get("completion_tokens_details").and_then(|d| d.get("reasoning_tokens"))
                                            .or_else(|| u.get("reasoning_tokens"))
                                            .and_then(JsonValue::as_u64)
                                        {
                                            reasoning_tokens = Some(rt);
                                        }
                                        if let Some(tt) = u.get("total_tokens").and_then(JsonValue::as_u64) {
                                            total_tokens = Some(tt);
                                        }
                                    }

                                    // 提取 content / reasoning_content 分片
                                    if let Some(delta) = val.pointer("/choices/0/delta") {
                                        if ttft_ms.is_none() {
                                            ttft_ms = Some(start_time.elapsed().as_millis() as u64);
                                        }

                                        if let Some(c) = delta.get("content").and_then(JsonValue::as_str) {
                                            collected_content.push_str(c);
                                        }
                                        if let Some(rc) = delta.get("reasoning_content")
                                            .or_else(|| delta.get("reasoning"))
                                            .and_then(JsonValue::as_str)
                                        {
                                            collected_reasoning.push_str(rc);
                                        }
                                    }

                                    // 过滤 DeepSeek-R1 / V3 等 choices 偶发的空 content null 字段
                                    if let Some(choices) = val.get_mut("choices").and_then(JsonValue::as_array_mut) {
                                        for choice in choices {
                                            if let Some(delta) = choice.get_mut("delta").and_then(JsonValue::as_object_mut) {
                                                if delta.get("content").map_or(false, |c| c.is_null()) {
                                                    if delta.get("reasoning_content").is_none() {
                                                        delta["content"] = JsonValue::String(String::new());
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    if let Ok(serialized) = serde_json::to_string(&val) {
                                        yield Ok(bytes::Bytes::from(format!("data: {serialized}\n\n")));
                                        continue;
                                    }
                                }
                            }

                            yield Ok(bytes::Bytes::from(format!("{block}\n\n")));
                        }
                    } else {
                        yield Ok(chunk);
                    }
                }
                Err(e) => {
                    let total_dur = start_time.elapsed().as_millis() as u64;
                    let mut interrupted_preview = String::new();
                    if !collected_reasoning.is_empty() {
                        interrupted_preview.push_str(" thinking\n");
                        interrupted_preview.push_str(&collected_reasoning);
                        interrupted_preview.push_str("\n response\n\n");
                    }
                    interrupted_preview.push_str(&collected_content);
                    ctx.record_log(ProxyRequestLog {
                        id: req_id.clone(),
                        timestamp: current_timestamp(),
                        method: "POST".to_string(),
                        path: path.clone(),
                        channel_id: "opencode".to_string(),
                        model: model.clone(),
                        stream: true,
                        status_code: 500,
                        duration_ms: total_dur,
                        ttft_ms,
                        prompt_tokens,
                        prompt_cache_hit_tokens: prompt_cache_hit,
                        prompt_cache_miss_tokens: prompt_cache_miss,
                        completion_tokens,
                        reasoning_tokens,
                        total_tokens,
                        error_message: Some(format!("流式响应传输异常中断: {e}")),
                        request_body: req_body_str.clone(),
                        response_body: if interrupted_preview.is_empty() { None } else { Some(interrupted_preview) },
                        node_name: node_name.clone(),
                    }).await;

                    yield Err(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()));
                    return;
                }
            }
        }

        if !finished && !buffer.trim().is_empty() {
            let trimmed = buffer.trim();
            if trimmed != "data: [DONE]" && !trimmed.contains("\"choices\":[]") {
                yield Ok(bytes::Bytes::from(format!("{trimmed}\n\n")));
            }
            yield Ok(bytes::Bytes::from("data: [DONE]\n\n"));
        }

        let mut response_preview = String::new();
        if !collected_reasoning.is_empty() {
            response_preview.push_str("<think>\n");
            response_preview.push_str(&collected_reasoning);
            response_preview.push_str("\n</think>\n\n");
        }
        response_preview.push_str(&collected_content);

        let final_out_tokens = completion_tokens.unwrap_or_else(|| (collected_content.len() / 4).max(1) as u64);
        let final_reason_tokens = reasoning_tokens.or_else(|| {
            if collected_reasoning.is_empty() { None } else { Some((collected_reasoning.len() / 4).max(1) as u64) }
        });
        let total_dur = start_time.elapsed().as_millis() as u64;

        // 仅在流式完全完结后，一次性形成最终完整的请求日志记录！
        ctx.record_log(ProxyRequestLog {
            id: req_id,
            timestamp: current_timestamp(),
            method: "POST".to_string(),
            path,
            channel_id: "opencode".to_string(),
            model,
            stream: true,
            status_code: 200,
            duration_ms: total_dur,
            ttft_ms,
            prompt_tokens,
            prompt_cache_hit_tokens: prompt_cache_hit,
            prompt_cache_miss_tokens: prompt_cache_miss,
            completion_tokens: Some(final_out_tokens),
            reasoning_tokens: final_reason_tokens,
            total_tokens,
            error_message: None,
            request_body: req_body_str,
            response_body: if response_preview.is_empty() { None } else { Some(response_preview) },
            node_name,
        }).await;
    }
}

fn openai_to_anthropic_sse_stream(
    ctx: OpencodeProxyContext,
    req_id: String,
    start_time: Instant,
    path: String,
    model_name: String,
    req_body_str: Option<String>,
    node_name: Option<String>,
    stream: impl futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin + Send + 'static,
) -> impl futures_util::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static {
    async_stream::stream! {
        let mut stream = stream;
        let mut buffer = String::new();
        let mut msg_started = false;
        let mut finished = false;
        let mut collected_content = String::new();
        let mut collected_reasoning = String::new();
        let mut ttft_ms: Option<u64> = None;

        let mut prompt_tokens: Option<u64> = None;
        let mut prompt_cache_hit: Option<u64> = None;
        let mut prompt_cache_miss: Option<u64> = None;
        let mut completion_tokens: Option<u64> = None;
        let mut reasoning_tokens: Option<u64> = None;
        let mut total_tokens: Option<u64> = None;
        let mut upstream_finish_reason: Option<String> = None;

        // Block 状态跟踪
        let mut current_block_index: usize = 0;
        let mut active_block: Option<String> = None; // None, Some("text"), Some("tool_use")
        let mut had_tool_use = false;

        let msg_id = format!(
            "msg_{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );

        while let Some(item) = stream.next().await {
            if finished {
                break;
            }
            match item {
                Ok(chunk) => {
                    if let Ok(text) = std::str::from_utf8(&chunk) {
                        buffer.push_str(text);

                        while let Some(pos) = buffer.find("\n\n") {
                            let block = buffer[..pos].to_string();
                            buffer = buffer[pos + 2..].to_string();

                            let trimmed = block.trim();
                            if trimmed.is_empty() {
                                continue;
                            }

                            if trimmed == "data: [DONE]" {
                                finished = true;
                                break;
                            }

                            if let Some(json_payload) = trimmed.strip_prefix("data: ") {
                                if let Ok(val) = serde_json::from_str::<JsonValue>(json_payload) {
                                    // 提取 usage
                                    if let Some(u) = val.get("usage").and_then(JsonValue::as_object) {
                                        if let Some(pt) = u.get("prompt_tokens").and_then(JsonValue::as_u64) {
                                            prompt_tokens = Some(pt);
                                        }
                                        if let Some(hit) = u.get("prompt_cache_hit_tokens")
                                            .or_else(|| u.get("prompt_tokens_details").and_then(|d| d.get("cached_tokens")))
                                            .or_else(|| u.get("prompt_tokens_details").and_then(|d| d.get("cache_read")))
                                            .or_else(|| u.get("cache_read_input_tokens"))
                                            .or_else(|| u.get("cached_tokens"))
                                            .or_else(|| u.get("cache_hit_tokens"))
                                            .and_then(JsonValue::as_u64)
                                        {
                                            prompt_cache_hit = Some(hit);
                                        }
                                        if let Some(miss) = u.get("prompt_cache_miss_tokens").and_then(JsonValue::as_u64) {
                                            prompt_cache_miss = Some(miss);
                                        }
                                        if let Some(ct) = u.get("completion_tokens").and_then(JsonValue::as_u64) {
                                            completion_tokens = Some(ct);
                                        }
                                        if let Some(rt) = u.get("completion_tokens_details").and_then(|d| d.get("reasoning_tokens"))
                                            .or_else(|| u.get("reasoning_tokens"))
                                            .and_then(JsonValue::as_u64)
                                        {
                                            reasoning_tokens = Some(rt);
                                        }
                                        if let Some(tt) = u.get("total_tokens").and_then(JsonValue::as_u64) {
                                            total_tokens = Some(tt);
                                        }
                                    }

                                    // 首个数据包发出 message_start
                                    if !msg_started {
                                        msg_started = true;
                                        if ttft_ms.is_none() {
                                            ttft_ms = Some(start_time.elapsed().as_millis() as u64);
                                        }

                                        let in_tokens = prompt_tokens.unwrap_or(0);
                                        let msg_start = json!({
                                            "type": "message_start",
                                            "message": {
                                                "id": msg_id.clone(),
                                                "type": "message",
                                                "role": "assistant",
                                                "model": format!("opencode/{}", model_name),
                                                "content": [],
                                                "stop_reason": null,
                                                "stop_sequence": null,
                                                "usage": { "input_tokens": in_tokens, "output_tokens": 0 }
                                            }
                                        });
                                        yield Ok(bytes::Bytes::from(format!("event: message_start\ndata: {msg_start}\n\n")));
                                    }

                                    if let Some(finish) = val.pointer("/choices/0/finish_reason").and_then(JsonValue::as_str) {
                                        upstream_finish_reason = Some(finish.to_string());
                                    }

                                    if let Some(delta) = val.pointer("/choices/0/delta") {
                                        if ttft_ms.is_none() {
                                            ttft_ms = Some(start_time.elapsed().as_millis() as u64);
                                        }

                                        // 提取 content 文本流
                                        if let Some(c) = delta.get("content").and_then(JsonValue::as_str) {
                                            if !c.is_empty() {
                                                collected_content.push_str(c);

                                                if active_block.as_deref() != Some("text") {
                                                    if active_block.is_some() {
                                                        let stop = json!({ "type": "content_block_stop", "index": current_block_index });
                                                        yield Ok(bytes::Bytes::from(format!("event: content_block_stop\ndata: {stop}\n\n")));
                                                        current_block_index += 1;
                                                    }
                                                    active_block = Some("text".to_string());
                                                    let start = json!({
                                                        "type": "content_block_start",
                                                        "index": current_block_index,
                                                        "content_block": { "type": "text", "text": "" }
                                                    });
                                                    yield Ok(bytes::Bytes::from(format!("event: content_block_start\ndata: {start}\n\n")));
                                                }

                                                let block_delta = json!({
                                                    "type": "content_block_delta",
                                                    "index": current_block_index,
                                                    "delta": { "type": "text_delta", "text": c }
                                                });
                                                yield Ok(bytes::Bytes::from(format!("event: content_block_delta\ndata: {block_delta}\n\n")));
                                            }
                                        }

                                        // 提取 tool_calls 工具调用流
                                        if let Some(tool_calls) = delta.get("tool_calls").and_then(JsonValue::as_array) {
                                            for tc in tool_calls {
                                                had_tool_use = true;
                                                let tc_id = tc.get("id").and_then(JsonValue::as_str);
                                                let tc_name = tc.pointer("/function/name").and_then(JsonValue::as_str);
                                                let tc_args = tc.pointer("/function/arguments").and_then(JsonValue::as_str);

                                                if tc_id.is_some() || tc_name.is_some() {
                                                    let new_id = tc_id.unwrap_or("call_default").to_string();
                                                    let new_name = tc_name.unwrap_or_default().to_string();

                                                    if active_block.is_some() {
                                                        let stop = json!({ "type": "content_block_stop", "index": current_block_index });
                                                        yield Ok(bytes::Bytes::from(format!("event: content_block_stop\ndata: {stop}\n\n")));
                                                        current_block_index += 1;
                                                    }

                                                    active_block = Some("tool_use".to_string());
                                                    let start = json!({
                                                        "type": "content_block_start",
                                                        "index": current_block_index,
                                                        "content_block": {
                                                            "type": "tool_use",
                                                            "id": new_id,
                                                            "name": new_name,
                                                            "input": {}
                                                        }
                                                    });
                                                    yield Ok(bytes::Bytes::from(format!("event: content_block_start\ndata: {start}\n\n")));
                                                }

                                                if let Some(args_chunk) = tc_args {
                                                    if !args_chunk.is_empty() {
                                                        let block_delta = json!({
                                                            "type": "content_block_delta",
                                                            "index": current_block_index,
                                                            "delta": {
                                                                "type": "input_json_delta",
                                                                "partial_json": args_chunk
                                                            }
                                                        });
                                                        yield Ok(bytes::Bytes::from(format!("event: content_block_delta\ndata: {block_delta}\n\n")));
                                                    }
                                                }
                                            }
                                        }

                                        if let Some(rc) = delta.get("reasoning_content")
                                            .or_else(|| delta.get("reasoning"))
                                            .and_then(JsonValue::as_str)
                                        {
                                            collected_reasoning.push_str(rc);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    let total_dur = start_time.elapsed().as_millis() as u64;
                    let mut interrupted_preview = String::new();
                    if !collected_reasoning.is_empty() {
                        interrupted_preview.push_str("<think>\n");
                        interrupted_preview.push_str(&collected_reasoning);
                        interrupted_preview.push_str("\n</think>\n\n");
                    }
                    interrupted_preview.push_str(&collected_content);
                    ctx.record_log(ProxyRequestLog {
                        id: req_id.clone(),
                        timestamp: current_timestamp(),
                        method: "POST".to_string(),
                        path: path.clone(),
                        channel_id: "opencode".to_string(),
                        model: model_name.clone(),
                        stream: true,
                        status_code: 500,
                        duration_ms: total_dur,
                        ttft_ms,
                        prompt_tokens,
                        prompt_cache_hit_tokens: prompt_cache_hit,
                        prompt_cache_miss_tokens: prompt_cache_miss,
                        completion_tokens,
                        reasoning_tokens,
                        total_tokens,
                        error_message: Some(format!("Anthropic 流式转换传输中断: {e}")),
                        request_body: req_body_str.clone(),
                        response_body: if interrupted_preview.is_empty() { None } else { Some(interrupted_preview) },
                        node_name: node_name.clone(),
                    }).await;

                    yield Err(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()));
                    return;
                }
            }
        }

        let out_tokens = completion_tokens.unwrap_or_else(|| (collected_content.len() / 4).max(1) as u64);

        if msg_started {
            // 如果还处于某个 content block，发送 content_block_stop
            if active_block.is_some() {
                let block_stop = json!({ "type": "content_block_stop", "index": current_block_index });
                yield Ok(bytes::Bytes::from(format!("event: content_block_stop\ndata: {block_stop}\n\n")));
            }

            let stop_reason = match upstream_finish_reason.as_deref() {
                Some("tool_calls") => "tool_use",
                Some("length") => "max_tokens",
                _ => {
                    if had_tool_use {
                        "tool_use"
                    } else {
                        "end_turn"
                    }
                }
            };

            // message_delta
            let msg_delta = json!({
                "type": "message_delta",
                "delta": { "stop_reason": stop_reason, "stop_sequence": null },
                "usage": { "output_tokens": out_tokens }
            });
            yield Ok(bytes::Bytes::from(format!("event: message_delta\ndata: {msg_delta}\n\n")));

            // message_stop
            let msg_stop = json!({ "type": "message_stop" });
            yield Ok(bytes::Bytes::from(format!("event: message_stop\ndata: {msg_stop}\n\n")));
        }

        let mut response_preview = String::new();
        if !collected_reasoning.is_empty() {
            response_preview.push_str("<think>\n");
            response_preview.push_str(&collected_reasoning);
            response_preview.push_str("\n</think>\n\n");
        }
        response_preview.push_str(&collected_content);

        let final_reason_tokens = reasoning_tokens.or_else(|| {
            if collected_reasoning.is_empty() { None } else { Some((collected_reasoning.len() / 4).max(1) as u64) }
        });
        let total_dur = start_time.elapsed().as_millis() as u64;

        ctx.record_log(ProxyRequestLog {
            id: req_id,
            timestamp: current_timestamp(),
            method: "POST".to_string(),
            path,
            channel_id: "opencode".to_string(),
            model: model_name,
            stream: true,
            status_code: 200,
            duration_ms: total_dur,
            ttft_ms,
            prompt_tokens,
            prompt_cache_hit_tokens: prompt_cache_hit,
            prompt_cache_miss_tokens: prompt_cache_miss,
            completion_tokens: Some(out_tokens),
            reasoning_tokens: final_reason_tokens,
            total_tokens,
            error_message: None,
            request_body: req_body_str,
            response_body: if response_preview.is_empty() { None } else { Some(response_preview) },
            node_name,
        }).await;
    }
}

fn normalize_chat_messages(body: &mut JsonValue) {
    if let Some(messages) = body.get_mut("messages").and_then(JsonValue::as_array_mut) {
        for msg in messages {
            // 1. 规范化 content 字段：如果客户端传入的是数组（包含 text, image_url 等复合 block），提取拼接为纯文本字符串
            // 避免 OpenCode 上游反序列化器报错 "unknown variant image_url, expected text"
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

            // 2. Assistant 深度思考思维链提取
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

// ---------------------------------------------------------------------------
// 代理池按延迟升序与直连候选列表构建 (直连在首位，节点按速度排序)
// ---------------------------------------------------------------------------

async fn get_sorted_egress_candidates(
    ctx: &OpencodeProxyContext,
    channel: &ChannelConfig,
) -> Vec<String> {
    if !channel.use_proxy_pool {
        return vec!["__direct__".to_string()];
    }

    let mut candidates = vec!["__direct__".to_string()];

    if let Some(app) = ctx.app_handle.read().await.as_ref() {
        let database = app.state::<crate::models::Database>();
        let nodes: Vec<String> = {
            match database.0.lock() {
                Ok(conn) => {
                    let stmt_res = conn.prepare(
                        "SELECT id FROM proxy_pool_nodes
                         WHERE (latency_ms > 0 AND latency_ms <= 1000)
                            OR (channel_latency_ms > 0 AND channel_latency_ms <= 1000)
                         ORDER BY COALESCE(NULLIF(latency_ms, 0), channel_latency_ms, 999) ASC"
                    );
                    if let Ok(mut stmt) = stmt_res {
                        if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                            rows.filter_map(Result::ok).collect()
                        } else {
                            Vec::new()
                        }
                    } else {
                        Vec::new()
                    }
                }
                Err(_) => Vec::new(),
            }
        };
        candidates.extend(nodes);
    }

    candidates
}


async fn build_client_for_candidate(
    ctx: &OpencodeProxyContext,
    candidate: &str,
) -> reqwest::Client {
    if candidate == "__direct__" {
        return ctx.default_http_client.clone();
    }

    if let Some(app) = ctx.app_handle.read().await.as_ref() {
        let database = app.state::<crate::models::Database>();
        let runtime = app.state::<crate::proxy_pool::ProxyRuntime>();

        let _ = crate::proxy_pool::select_proxy_node_transient(&database, &runtime, candidate).await;
        let proxy_url = crate::proxy_pool::runtime_proxy_url_pub(&runtime);
        if let Ok(proxy) = reqwest::Proxy::all(&proxy_url) {
            if let Ok(client) = reqwest::Client::builder()
                .proxy(proxy)
                .timeout(Duration::from_secs(300))
                .build()
            {
                return client;
            }
        }
    }

    ctx.default_http_client.clone()
}

async fn get_node_display_name(ctx: &OpencodeProxyContext, candidate: &str) -> String {
    if candidate == "__direct__" {
        return "直连通道".to_string();
    }

    if let Some(app) = ctx.app_handle.read().await.as_ref() {
        let database = app.state::<crate::models::Database>();
        let name_opt: Option<String> = {
            match database.0.lock() {
                Ok(conn) => {
                    let res: Result<String, _> = conn.query_row(
                        "SELECT name FROM proxy_pool_nodes WHERE id = ?1",
                        [candidate],
                        |row| row.get(0),
                    );
                    res.ok()
                }
                Err(_) => None,
            }
        };
        if let Some(name) = name_opt {
            if !name.trim().is_empty() {
                return name;
            }
        }
    }

    candidate.to_string()
}

// ---------------------------------------------------------------------------
// 路由与中间件
// ---------------------------------------------------------------------------

pub fn create_opencode_proxy_router(ctx: OpencodeProxyContext) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/healthz", get(handle_healthz))
        .route("/v1/models", get(handle_models))
        .route("/v1/chat/completions", post(handle_chat_completions))
        .route("/v1/responses", post(handle_responses))
        .route("/v1/messages", post(handle_messages))
        .layer(cors)
        .with_state(ctx)
}

/// 鉴权中间件
async fn check_auth(headers: &HeaderMap, config: &OpencodeProxyConfig) -> Result<(), Response> {
    if config.api_key.trim().is_empty() {
        return Ok(());
    }

    let auth_header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .or_else(|| headers.get("x-api-key").and_then(|v| v.to_str().ok()))
        .unwrap_or_default();

    let token = auth_header
        .strip_prefix("Bearer ")
        .or_else(|| auth_header.strip_prefix("bearer "))
        .unwrap_or(auth_header)
        .trim();

    if token == config.api_key.trim() {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": {
                    "message": "Invalid local API Key (本地 Bearer 访问密钥校验未通过)",
                    "type": "invalid_request_error",
                    "code": "invalid_api_key"
                }
            })),
        )
            .into_response())
    }
}

/// GET /healthz
async fn handle_healthz(State(ctx): State<OpencodeProxyContext>) -> Response {
    let config = ctx.config.read().await;
    let models_count = ctx.cached_models.read().await.len();
    let uptime = ctx
        .started_at
        .read()
        .await
        .map(|t| t.elapsed().as_secs())
        .unwrap_or(0);

    let auth_desc = if config.api_key.is_empty() {
        "免密直接访问"
    } else {
        "Bearer Key 校验"
    };

    let opencode_chan = config.channels.iter().find(|c| c.id == "opencode");
    let proxy_pool_desc = match opencode_chan {
        Some(c) if c.use_proxy_pool => "直连优先 + 代理池按速排序故障转移",
        _ => "直接连接 (直连)",
    };

    let checks = json!([
        {
            "name": "本地反代网关 (Gateway)",
            "endpoint": format!("http://127.0.0.1:{}/v1", config.port),
            "status": "ok",
            "message": format!("网关正常运行中，已运行 {} 秒", uptime),
            "auth": auth_desc
        },
        {
            "name": "OpenCode 上游渠道",
            "endpoint": opencode_chan.map(|c| c.upstream_url.clone()).unwrap_or_else(|| "https://opencode.ai/zen/v1".to_string()),
            "status": if opencode_chan.map(|c| c.enabled).unwrap_or(true) { "ok" } else { "warning" },
            "message": format!("Public 免费通道就绪 · 网络模式：{}", proxy_pool_desc),
            "auth": "public"
        },
        {
            "name": "模型列表 (/v1/models)",
            "endpoint": "/v1/models",
            "status": "ok",
            "message": format!("已加载 {} 个在线模型，统一命名空间为 opencode/*", models_count),
            "auth": auth_desc
        },
        {
            "name": "对话补全 (/v1/chat/completions)",
            "endpoint": "/v1/chat/completions",
            "status": "ok",
            "message": "OpenAI 兼容协议，支持流式 SSE 与工具调用",
            "auth": auth_desc
        },
        {
            "name": "响应端点 (/v1/responses)",
            "endpoint": "/v1/responses",
            "status": "ok",
            "message": "OpenAI Responses 协议代理",
            "auth": auth_desc
        },
        {
            "name": "Claude 消息协议 (/v1/messages)",
            "endpoint": "/v1/messages",
            "status": "ok",
            "message": "Anthropic Claude 格式消息适配代理",
            "auth": auth_desc
        },
        {
            "name": "健康检查端点 (/healthz)",
            "endpoint": "/healthz",
            "status": "ok",
            "message": "公开端点，无需鉴权即可访问",
            "auth": "公开免密"
        }
    ]);

    (StatusCode::OK, Json(checks)).into_response()
}

/// 从 OpenCode 上游抓取并刷新模型列表
async fn fetch_upstream_models_inner(
    ctx: &OpencodeProxyContext,
    config: &OpencodeProxyConfig,
) -> Result<Vec<String>, String> {
    let chan = config
        .channels
        .iter()
        .find(|c| c.id == "opencode" && c.enabled)
        .ok_or_else(|| "OpenCode 渠道未启用或未配置".to_string())?;

    let candidates = get_sorted_egress_candidates(ctx, chan).await;
    let candidate = candidates.first().map(|s| s.as_str()).unwrap_or("__direct__");
    let client = build_client_for_candidate(ctx, candidate).await;
    let models_url = format!("{}/models", chan.upstream_url.trim_end_matches('/'));

    let auth_val = if chan.api_key.trim().is_empty() {
        "Bearer public".to_string()
    } else {
        format!("Bearer {}", chan.api_key.trim())
    };

    let res = client
        .get(&models_url)
        .header("Authorization", auth_val)
        .header("User-Agent", "opencode/1.0.0")
        .header("x-opencode-client", "cli")
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("无法连接 OpenCode 上游模型接口: {e}"))?;

    if !res.status().is_success() {
        return Err(format!("OpenCode 上游模型接口返回 HTTP {}", res.status()));
    }

    let val = res
        .json::<JsonValue>()
        .await
        .map_err(|e| format!("解析模型列表 JSON 失败: {e}"))?;

    let mut model_ids = Vec::new();
    if let Some(list) = val.get("data").and_then(JsonValue::as_array) {
        for item in list {
            if let Some(id) = item.get("id").and_then(JsonValue::as_str) {
                if id.contains("free") || id == "big-pickle" {
                    if !model_ids.contains(&id.to_string()) {
                        model_ids.push(id.to_string());
                    }
                }
            }
        }
    }

    if model_ids.is_empty() {
        return Err("OpenCode 上游未返回可用的免费模型".to_string());
    }

    Ok(model_ids)
}

/// GET /v1/models (ID 统一为 opencode/原id)
async fn handle_models(headers: HeaderMap, State(ctx): State<OpencodeProxyContext>) -> Response {
    let config = ctx.config.read().await;
    if let Err(res) = check_auth(&headers, &config).await {
        return res;
    }

    let need_refresh = {
        let updated = ctx.cached_models_updated_at.read().await;
        updated.is_none() || updated.map(|t| t.elapsed() > Duration::from_secs(300)).unwrap_or(true)
    };

    if need_refresh {
        if let Ok(models) = fetch_upstream_models_inner(&ctx, &config).await {
            let mut cached = ctx.cached_models.write().await;
            *cached = models;
            let mut updated = ctx.cached_models_updated_at.write().await;
            *updated = Some(Instant::now());
        }
    }

    let models = ctx.cached_models.read().await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let data: Vec<JsonValue> = models
        .iter()
        .map(|raw_id| {
            json!({
                "id": format!("opencode/{raw_id}"),
                "object": "model",
                "created": now,
                "owned_by": "opencode",
                "permission": [],
                "root": format!("opencode/{raw_id}"),
                "parent": null
            })
        })
        .collect();

    let response = json!({
        "object": "list",
        "data": data
    });

    (StatusCode::OK, Json(response)).into_response()
}

fn strip_opencode_prefix(model: &str) -> &str {
    model.strip_prefix("opencode/").unwrap_or(model)
}

/// POST /v1/chat/completions (直连优先 + 代理池按速排序粘性轮询故障转移，全流完结后记录完整日志)
async fn handle_chat_completions(
    headers: HeaderMap,
    State(ctx): State<OpencodeProxyContext>,
    Json(mut body): Json<JsonValue>,
) -> Response {
    let start_time = Instant::now();
    let req_id = generate_req_id();
    let config = ctx.config.read().await;

    let req_body_str = if config.record_request_body {
        Some(serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string()))
    } else {
        None
    };

    if let Err(res) = check_auth(&headers, &config).await {
        let dur = start_time.elapsed().as_millis() as u64;
        ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);
        ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
        ctx.record_log(ProxyRequestLog {
            id: req_id,
            timestamp: current_timestamp(),
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            channel_id: "opencode".to_string(),
            model: body.get("model").and_then(JsonValue::as_str).unwrap_or("unknown").to_string(),
            stream: body.get("stream").and_then(JsonValue::as_bool).unwrap_or(false),
            status_code: 401,
            duration_ms: dur,
            ttft_ms: None,
            prompt_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
            completion_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            error_message: Some("鉴权未通过：请求未携带有效的 Bearer API Key 或密钥不匹配".to_string()),
            request_body: req_body_str,
            response_body: None,
            node_name: Some("直连通道".to_string()),
        }).await;
        return res;
    }

    ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);

    let raw_model = body
        .get("model")
        .and_then(JsonValue::as_str)
        .unwrap_or("unknown")
        .to_string();
    let model_to_send = strip_opencode_prefix(&raw_model).to_string();

    if let Some(model_val) = body.get_mut("model") {
        *model_val = JsonValue::String(model_to_send.clone());
    }

    normalize_chat_messages(&mut body);

    let is_stream = body
        .get("stream")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);

    let chan = match config.channels.iter().find(|c| c.id == "opencode" && c.enabled) {
        Some(c) => c,
        None => {
            ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
            ctx.record_log(ProxyRequestLog {
                id: req_id,
                timestamp: current_timestamp(),
                method: "POST".to_string(),
                path: "/v1/chat/completions".to_string(),
                channel_id: "opencode".to_string(),
                model: model_to_send,
                stream: is_stream,
                status_code: 503,
                duration_ms: start_time.elapsed().as_millis() as u64,
                ttft_ms: None,
                prompt_tokens: None,
                prompt_cache_hit_tokens: None,
                prompt_cache_miss_tokens: None,
                completion_tokens: None,
                reasoning_tokens: None,
                total_tokens: None,
                error_message: Some("渠道不可用：OpenCode 上游渠道已被手动禁用".to_string()),
                request_body: req_body_str,
                response_body: None,
                node_name: Some("直连通道".to_string()),
            }).await;

            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": {
                        "message": "OpenCode 上游渠道当前已被禁用",
                        "type": "channel_disabled"
                    }
                })),
            )
                .into_response();
        }
    };

    let target_url = format!("{}/chat/completions", chan.upstream_url.trim_end_matches('/'));
    let auth_val = if chan.api_key.trim().is_empty() {
        "Bearer public".to_string()
    } else {
        format!("Bearer {}", chan.api_key.trim())
    };

    let candidates = get_sorted_egress_candidates(&ctx, chan).await;
    let total_candidates = candidates.len().max(1);
    let max_retries = ctx.config.read().await.max_retries as usize;
    let max_attempts = if chan.use_proxy_pool {
        (max_retries + 1).min(total_candidates)
    } else {
        max_retries + 1
    };

    let base_idx = ctx.active_egress_idx.load(Ordering::Relaxed);
    let mut final_res = None;
    let mut last_send_err = None;
    let mut used_cand_id = candidates.get(base_idx % candidates.len()).cloned().unwrap_or_else(|| "__direct__".to_string());

    for attempt in 0..max_attempts {
        let attempt_start = Instant::now();
        let cand_idx = (base_idx + attempt) % candidates.len();
        let cand_id = &candidates[cand_idx];
        used_cand_id = cand_id.clone();
        let client = build_client_for_candidate(&ctx, cand_id).await;

        let session_id = headers
            .get("x-opencode-session")
            .or_else(|| headers.get("session-id"))
            .or_else(|| headers.get("x-session-id"))
            .or_else(|| headers.get("conversation-id"))
            .or_else(|| headers.get("x-conversation-id"))
            .and_then(|v| v.to_str().ok())
            .map(String::from)
            .unwrap_or_else(|| "openhub-session".to_string());

        let mut req = client
            .post(&target_url)
            .header("Authorization", &auth_val)
            .header("Content-Type", "application/json")
            .header("User-Agent", "opencode/1.0.0")
            .header("x-opencode-client", "cli")
            .header("x-opencode-session", session_id)
            .header("Accept", if is_stream { "text/event-stream" } else { "application/json" });

        // 透传来自客户端的 cache_control 标头（如果客户端已标记）
        if let Some(cc) = headers.get("anthropic-beta") {
            req = req.header("anthropic-beta", cc);
        }

        let req = req.json(&body);

        match req.send().await {
            Ok(r) => {
                let status = r.status();
                if !status.is_success() && attempt + 1 < max_attempts {
                    let err_body = r.text().await.unwrap_or_default();
                    record_failover_event(
                        &ctx,
                        &req_id,
                        "/v1/chat/completions",
                        &model_to_send,
                        is_stream,
                        status.as_u16(),
                        format!("节点失败自动切换：{}", format_upstream_error_message(status.as_u16(), &err_body)),
                        attempt_start.elapsed().as_millis() as u64,
                        req_body_str.clone(),
                        cand_id,
                    )
                    .await;
                    let next_idx = (cand_idx + 1) % candidates.len();
                    ctx.active_egress_idx.store(next_idx, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    continue;
                }

                if status.is_success() {
                    ctx.active_egress_idx.store(cand_idx, Ordering::Relaxed);
                }
                final_res = Some(r);
                break;
            }
            Err(e) => {
                let e_str = e.to_string();
                last_send_err = Some(e);
                if attempt + 1 < max_attempts {
                    record_failover_event(
                        &ctx,
                        &req_id,
                        "/v1/chat/completions",
                        &model_to_send,
                        is_stream,
                        502,
                        format!("节点连接失败自动切换：{e_str}"),
                        attempt_start.elapsed().as_millis() as u64,
                        req_body_str.clone(),
                        cand_id,
                    )
                    .await;
                    let next_idx = (cand_idx + 1) % candidates.len();
                    ctx.active_egress_idx.store(next_idx, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    continue;
                }
            }
        }
    }

    let node_name = Some(get_node_display_name(&ctx, &used_cand_id).await);

    let res = match final_res {
        Some(r) => r,
        None => {
            let err = last_send_err.map(|e| e.to_string()).unwrap_or_else(|| "Unknown connection error".to_string());
            let dur = start_time.elapsed().as_millis() as u64;
            ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
            ctx.record_log(ProxyRequestLog {
                id: req_id,
                timestamp: current_timestamp(),
                method: "POST".to_string(),
                path: "/v1/chat/completions".to_string(),
                channel_id: "opencode".to_string(),
                model: model_to_send,
                stream: is_stream,
                status_code: 502,
                duration_ms: dur,
                ttft_ms: None,
                prompt_tokens: None,
                prompt_cache_hit_tokens: None,
                prompt_cache_miss_tokens: None,
                completion_tokens: None,
                reasoning_tokens: None,
                total_tokens: None,
                error_message: Some(format!("上游连接失败 (Bad Gateway)：{err}")),
                request_body: req_body_str,
                response_body: None,
                node_name,
            }).await;

            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": {
                        "message": format!("Failed to connect to upstream: {err}"),
                        "type": "upstream_error",
                        "code": "upstream_connect_failed"
                    }
                })),
            )
                .into_response();
        }
    };

    let status = res.status();
    if !status.is_success() {
        let dur = start_time.elapsed().as_millis() as u64;
        ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
        let error_body = res.text().await.unwrap_or_default();
        let formatted_err = format_upstream_error_message(status.as_u16(), &error_body);

        ctx.record_log(ProxyRequestLog {
            id: req_id,
            timestamp: current_timestamp(),
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            channel_id: "opencode".to_string(),
            model: model_to_send,
            stream: is_stream,
            status_code: status.as_u16(),
            duration_ms: dur,
            ttft_ms: None,
            prompt_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
            completion_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            error_message: Some(formatted_err),
            request_body: req_body_str,
            response_body: Some(error_body.clone()),
            node_name,
        }).await;

        return (status, [(axum::http::header::CONTENT_TYPE, "application/json")], error_body).into_response();
    }

    ctx.metrics.successful_requests.fetch_add(1, Ordering::Relaxed);

    if is_stream {
        // 流式请求：不在此处提前记录空日志，交由 clean_sse_stream 在流式彻底完结后记录完整数据！
        let stream = clean_sse_stream(
            ctx.clone(),
            req_id,
            start_time,
            "/v1/chat/completions".to_string(),
            model_to_send,
            req_body_str,
            node_name,
            res.bytes_stream(),
        );
        (
            StatusCode::OK,
            [
                (axum::http::header::CONTENT_TYPE, "text/event-stream"),
                (axum::http::header::CACHE_CONTROL, "no-cache"),
                (axum::http::header::CONNECTION, "keep-alive"),
            ],
            Body::from_stream(stream),
        )
            .into_response()
    } else {
        let ttft = start_time.elapsed().as_millis() as u64;
        let data = res.bytes().await.unwrap_or_default();
        let dur = start_time.elapsed().as_millis() as u64;

        let (prompt_tokens, prompt_cache_hit_tokens, prompt_cache_miss_tokens, completion_tokens, reasoning_tokens, total_tokens, resp_str) =
            if let Ok(json_val) = serde_json::from_slice::<JsonValue>(&data) {
                let pt = json_val.pointer("/usage/prompt_tokens").and_then(JsonValue::as_u64);
                let hit = json_val.pointer("/usage/prompt_cache_hit_tokens")
                    .or_else(|| json_val.pointer("/usage/prompt_tokens_details/cached_tokens"))
                    .or_else(|| json_val.pointer("/usage/prompt_tokens_details/cache_read"))
                    .or_else(|| json_val.pointer("/usage/cache_read_input_tokens"))
                    .or_else(|| json_val.pointer("/usage/cached_tokens"))
                    .or_else(|| json_val.pointer("/usage/cache_hit_tokens"))
                    .and_then(JsonValue::as_u64);
                let miss = json_val.pointer("/usage/prompt_cache_miss_tokens").and_then(JsonValue::as_u64);
                let ct = json_val.pointer("/usage/completion_tokens").and_then(JsonValue::as_u64);
                let rt = json_val.pointer("/usage/completion_tokens_details/reasoning_tokens")
                    .or_else(|| json_val.pointer("/usage/reasoning_tokens"))
                    .and_then(JsonValue::as_u64);
                let tt = json_val.pointer("/usage/total_tokens").and_then(JsonValue::as_u64);
                let formatted = serde_json::to_string_pretty(&json_val).unwrap_or_else(|_| json_val.to_string());
                (pt, hit, miss, ct, rt, tt, Some(formatted))
            } else {
                (None, None, None, None, None, None, String::from_utf8(data.to_vec()).ok())
            };

        ctx.record_log(ProxyRequestLog {
            id: req_id,
            timestamp: current_timestamp(),
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            channel_id: "opencode".to_string(),
            model: model_to_send,
            stream: false,
            status_code: 200,
            duration_ms: dur,
            ttft_ms: Some(ttft),
            prompt_tokens,
            prompt_cache_hit_tokens,
            prompt_cache_miss_tokens,
            completion_tokens,
            reasoning_tokens,
            total_tokens,
            error_message: None,
            request_body: req_body_str,
            response_body: resp_str,
            node_name,
        }).await;

        (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            data,
        )
            .into_response()
    }
}

/// POST /v1/responses
async fn handle_responses(
    headers: HeaderMap,
    State(ctx): State<OpencodeProxyContext>,
    Json(mut body): Json<JsonValue>,
) -> Response {
    let start_time = Instant::now();
    let req_id = generate_req_id();
    let config = ctx.config.read().await;

    let req_body_str = if config.record_request_body {
        Some(serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string()))
    } else {
        None
    };

    if let Err(res) = check_auth(&headers, &config).await {
        let dur = start_time.elapsed().as_millis() as u64;
        ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);
        ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
        ctx.record_log(ProxyRequestLog {
            id: req_id,
            timestamp: current_timestamp(),
            method: "POST".to_string(),
            path: "/v1/responses".to_string(),
            channel_id: "opencode".to_string(),
            model: body.get("model").and_then(JsonValue::as_str).unwrap_or("unknown").to_string(),
            stream: false,
            status_code: 401,
            duration_ms: dur,
            ttft_ms: None,
            prompt_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
            completion_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            error_message: Some("鉴权未通过：请求未携带有效的 Bearer API Key".to_string()),
            request_body: req_body_str,
            response_body: None,
            node_name: Some("直连通道".to_string()),
        }).await;
        return res;
    }

    ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);

    let raw_model = body
        .get("model")
        .and_then(JsonValue::as_str)
        .unwrap_or("unknown")
        .to_string();
    let model_to_send = strip_opencode_prefix(&raw_model).to_string();

    if let Some(model_val) = body.get_mut("model") {
        *model_val = JsonValue::String(model_to_send.clone());
    }

    normalize_chat_messages(&mut body);

    let chan = match config.channels.iter().find(|c| c.id == "opencode" && c.enabled) {
        Some(c) => c,
        None => {
            ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": { "message": "OpenCode 上游渠道已禁用", "type": "channel_disabled" } })),
            )
                .into_response();
        }
    };

    let target_url = format!("{}/responses", chan.upstream_url.trim_end_matches('/'));
    let auth_val = if chan.api_key.trim().is_empty() {
        "Bearer public".to_string()
    } else {
        format!("Bearer {}", chan.api_key.trim())
    };

    let candidates = get_sorted_egress_candidates(&ctx, chan).await;
    let total_candidates = candidates.len().max(1);
    let max_retries = ctx.config.read().await.max_retries as usize;
    let max_attempts = if chan.use_proxy_pool {
        (max_retries + 1).min(total_candidates)
    } else {
        max_retries + 1
    };

    let base_idx = ctx.active_egress_idx.load(Ordering::Relaxed);
    let mut final_res = None;
    let mut last_send_err = None;
    let mut used_cand_id = candidates.get(base_idx % candidates.len()).cloned().unwrap_or_else(|| "__direct__".to_string());

    for attempt in 0..max_attempts {
        let attempt_start = Instant::now();
        let cand_idx = (base_idx + attempt) % candidates.len();
        let cand_id = &candidates[cand_idx];
        used_cand_id = cand_id.clone();
        let client = build_client_for_candidate(&ctx, cand_id).await;

        let session_id = headers
            .get("x-opencode-session")
            .or_else(|| headers.get("session-id"))
            .or_else(|| headers.get("x-session-id"))
            .or_else(|| headers.get("conversation-id"))
            .or_else(|| headers.get("x-conversation-id"))
            .and_then(|v| v.to_str().ok())
            .map(String::from)
            .unwrap_or_else(|| "openhub-session".to_string());

        let mut req = client
            .post(&target_url)
            .header("Authorization", &auth_val)
            .header("Content-Type", "application/json")
            .header("User-Agent", "opencode/1.0.0")
            .header("x-opencode-client", "cli")
            .header("x-opencode-session", session_id)
            .header("Accept", "application/json");

        if let Some(cc) = headers.get("anthropic-beta") {
            req = req.header("anthropic-beta", cc);
        }

        let req = req.json(&body);

        match req.send().await {
            Ok(r) => {
                let status = r.status();
                if !status.is_success() && attempt + 1 < max_attempts {
                    let err_body = r.text().await.unwrap_or_default();
                    record_failover_event(
                        &ctx,
                        &req_id,
                        "/v1/responses",
                        &model_to_send,
                        false,
                        status.as_u16(),
                        format!("节点失败自动切换：{}", format_upstream_error_message(status.as_u16(), &err_body)),
                        attempt_start.elapsed().as_millis() as u64,
                        req_body_str.clone(),
                        cand_id,
                    )
                    .await;
                    let next_idx = (cand_idx + 1) % candidates.len();
                    ctx.active_egress_idx.store(next_idx, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    continue;
                }
                if status.is_success() {
                    ctx.active_egress_idx.store(cand_idx, Ordering::Relaxed);
                }
                final_res = Some(r);
                break;
            }
            Err(e) => {
                let e_str = e.to_string();
                last_send_err = Some(e);
                if attempt + 1 < max_attempts {
                    record_failover_event(
                        &ctx,
                        &req_id,
                        "/v1/responses",
                        &model_to_send,
                        false,
                        502,
                        format!("节点连接失败自动切换：{e_str}"),
                        attempt_start.elapsed().as_millis() as u64,
                        req_body_str.clone(),
                        cand_id,
                    )
                    .await;
                    let next_idx = (cand_idx + 1) % candidates.len();
                    ctx.active_egress_idx.store(next_idx, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    continue;
                }
            }
        }
    }

    let node_name = Some(get_node_display_name(&ctx, &used_cand_id).await);

    let res = match final_res {
        Some(r) => r,
        None => {
            let err = last_send_err.map(|e| e.to_string()).unwrap_or_else(|| "Unknown error".to_string());
            let dur = start_time.elapsed().as_millis() as u64;
            ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
            ctx.record_log(ProxyRequestLog {
                id: req_id,
                timestamp: current_timestamp(),
                method: "POST".to_string(),
                path: "/v1/responses".to_string(),
                channel_id: "opencode".to_string(),
                model: model_to_send,
                stream: false,
                status_code: 502,
                duration_ms: dur,
                ttft_ms: None,
                prompt_tokens: None,
                prompt_cache_hit_tokens: None,
                prompt_cache_miss_tokens: None,
                completion_tokens: None,
                reasoning_tokens: None,
                total_tokens: None,
                error_message: Some(format!("上游连接错误: {err}")),
                request_body: req_body_str,
                response_body: None,
                node_name,
            }).await;

            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": {
                        "message": format!("Upstream connection error: {err}"),
                        "type": "upstream_error"
                    }
                })),
            )
                .into_response();
        }
    };

    let status = res.status();
    if !status.is_success() {
        let dur = start_time.elapsed().as_millis() as u64;
        ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
        let error_body = res.text().await.unwrap_or_default();
        let formatted_err = format_upstream_error_message(status.as_u16(), &error_body);

        ctx.record_log(ProxyRequestLog {
            id: req_id,
            timestamp: current_timestamp(),
            method: "POST".to_string(),
            path: "/v1/responses".to_string(),
            channel_id: "opencode".to_string(),
            model: model_to_send,
            stream: false,
            status_code: status.as_u16(),
            duration_ms: dur,
            ttft_ms: None,
            prompt_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
            completion_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            error_message: Some(formatted_err),
            request_body: req_body_str,
            response_body: Some(error_body.clone()),
            node_name,
        }).await;

        return (status, [(axum::http::header::CONTENT_TYPE, "application/json")], error_body).into_response();
    }

    ctx.metrics.successful_requests.fetch_add(1, Ordering::Relaxed);
    let ttft = start_time.elapsed().as_millis() as u64;
    let data = res.bytes().await.unwrap_or_default();
    let dur = start_time.elapsed().as_millis() as u64;

    let (prompt_tokens, prompt_cache_hit_tokens, prompt_cache_miss_tokens, completion_tokens, reasoning_tokens, total_tokens, resp_str) =
        if let Ok(json_val) = serde_json::from_slice::<JsonValue>(&data) {
            let pt = json_val.pointer("/usage/prompt_tokens").and_then(JsonValue::as_u64);
            let hit = json_val.pointer("/usage/prompt_cache_hit_tokens")
                .or_else(|| json_val.pointer("/usage/prompt_tokens_details/cached_tokens"))
                .or_else(|| json_val.pointer("/usage/prompt_tokens_details/cache_read"))
                .or_else(|| json_val.pointer("/usage/cache_read_input_tokens"))
                .or_else(|| json_val.pointer("/usage/cached_tokens"))
                .or_else(|| json_val.pointer("/usage/cache_hit_tokens"))
                .and_then(JsonValue::as_u64);
            let miss = json_val.pointer("/usage/prompt_cache_miss_tokens").and_then(JsonValue::as_u64);
            let ct = json_val.pointer("/usage/completion_tokens").and_then(JsonValue::as_u64);
            let rt = json_val.pointer("/usage/completion_tokens_details/reasoning_tokens")
                .or_else(|| json_val.pointer("/usage/reasoning_tokens"))
                .and_then(JsonValue::as_u64);
            let tt = json_val.pointer("/usage/total_tokens").and_then(JsonValue::as_u64);
            let formatted = serde_json::to_string_pretty(&json_val).unwrap_or_else(|_| json_val.to_string());
            (pt, hit, miss, ct, rt, tt, Some(formatted))
        } else {
            (None, None, None, None, None, None, String::from_utf8(data.to_vec()).ok())
        };

    ctx.record_log(ProxyRequestLog {
        id: req_id,
        timestamp: current_timestamp(),
        method: "POST".to_string(),
        path: "/v1/responses".to_string(),
        channel_id: "opencode".to_string(),
        model: model_to_send,
        stream: false,
        status_code: 200,
        duration_ms: dur,
        ttft_ms: Some(ttft),
        prompt_tokens,
        prompt_cache_hit_tokens,
        prompt_cache_miss_tokens,
        completion_tokens,
        reasoning_tokens,
        total_tokens,
        error_message: None,
        request_body: req_body_str,
        response_body: resp_str,
        node_name,
    }).await;

    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        data,
    )
        .into_response()
}

/// POST /v1/messages (Anthropic 协议适配与转发)
async fn handle_messages(
    headers: HeaderMap,
    State(ctx): State<OpencodeProxyContext>,
    Json(body): Json<JsonValue>,
) -> Response {
    let start_time = Instant::now();
    let req_id = generate_req_id();
    let config = ctx.config.read().await;

    let req_body_str = if config.record_request_body {
        Some(serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string()))
    } else {
        None
    };

    if let Err(res) = check_auth(&headers, &config).await {
        let dur = start_time.elapsed().as_millis() as u64;
        ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);
        ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
        ctx.record_log(ProxyRequestLog {
            id: req_id,
            timestamp: current_timestamp(),
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            channel_id: "opencode".to_string(),
            model: body.get("model").and_then(JsonValue::as_str).unwrap_or("free-claude-3-5-sonnet").to_string(),
            stream: body.get("stream").and_then(JsonValue::as_bool).unwrap_or(false),
            status_code: 401,
            duration_ms: dur,
            ttft_ms: None,
            prompt_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
            completion_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            error_message: Some("鉴权未通过：请求未携带有效的 Bearer API Key".to_string()),
            request_body: req_body_str,
            response_body: None,
            node_name: Some("直连通道".to_string()),
        }).await;
        return res;
    }

    ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);

    let raw_model = body
        .get("model")
        .and_then(JsonValue::as_str)
        .unwrap_or("deepseek-v4-flash-free");
    let stripped_model = strip_opencode_prefix(raw_model);

    let mut openai_tools = Vec::new();
    if let Some(tools_arr) = body.get("tools").and_then(JsonValue::as_array) {
        for t in tools_arr {
            let name = t.get("name").and_then(JsonValue::as_str).unwrap_or_default();
            let desc = t.get("description").and_then(JsonValue::as_str).unwrap_or_default();
            let schema = t.get("input_schema").cloned().unwrap_or_else(|| json!({"type": "object"}));
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

    let mut tool_choice_val = None;
    if let Some(tc) = body.get("tool_choice") {
        if let Some(tc_str) = tc.as_str() {
            tool_choice_val = Some(json!(tc_str));
        } else if let Some(tc_obj) = tc.as_object() {
            let tc_type = tc_obj.get("type").and_then(JsonValue::as_str).unwrap_or_default();
            match tc_type {
                "auto" => tool_choice_val = Some(json!("auto")),
                "any" => tool_choice_val = Some(json!("required")),
                "tool" => {
                    if let Some(name) = tc_obj.get("name").and_then(JsonValue::as_str) {
                        tool_choice_val = Some(json!({
                            "type": "function",
                            "function": { "name": name }
                        }));
                    }
                }
                _ => {}
            }
        }
    }

    let mut openai_messages = Vec::new();
    if let Some(sys_val) = body.get("system") {
        if let Some(s) = sys_val.as_str() {
            if !s.is_empty() {
                openai_messages.push(json!({
                    "role": "system",
                    "content": s
                }));
            }
        } else if let Some(arr) = sys_val.as_array() {
            let mut text = String::new();
            for block in arr {
                if let Some(t) = block.get("text").and_then(JsonValue::as_str) {
                    text.push_str(t);
                }
            }
            if !text.is_empty() {
                openai_messages.push(json!({
                    "role": "system",
                    "content": text
                }));
            }
        }
    }

    if let Some(anthropic_msgs) = body.get("messages").and_then(JsonValue::as_array) {
        for msg in anthropic_msgs {
            let role = msg.get("role").and_then(JsonValue::as_str).unwrap_or("user");
            let content_val = msg.get("content");

            if let Some(s) = content_val.and_then(JsonValue::as_str) {
                openai_messages.push(json!({
                    "role": role,
                    "content": s
                }));
            } else if let Some(arr) = content_val.and_then(JsonValue::as_array) {
                if role == "assistant" {
                    let mut text = String::new();
                    let mut tool_calls = Vec::new();
                    let mut thinking = String::new();

                    for block in arr {
                        let block_type = block.get("type").and_then(JsonValue::as_str).unwrap_or_default();
                        match block_type {
                            "text" => {
                                if let Some(t) = block.get("text").and_then(JsonValue::as_str) {
                                    text.push_str(t);
                                }
                            }
                            "thinking" | "redacted_thinking" => {
                                if let Some(th) = block.get("thinking").and_then(JsonValue::as_str) {
                                    thinking.push_str(th);
                                }
                            }
                            "tool_use" => {
                                let id = block.get("id").and_then(JsonValue::as_str).unwrap_or("call_default");
                                let name = block.get("name").and_then(JsonValue::as_str).unwrap_or_default();
                                let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                                let args_str = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                                tool_calls.push(json!({
                                    "id": id,
                                    "type": "function",
                                    "function": {
                                        "name": name,
                                        "arguments": args_str
                                    }
                                }));
                            }
                            _ => {}
                        }
                    }

                    let mut asst_msg = json!({
                        "role": "assistant",
                        "content": if text.is_empty() && !tool_calls.is_empty() { JsonValue::Null } else { JsonValue::String(text) }
                    });
                    if !tool_calls.is_empty() {
                        asst_msg["tool_calls"] = json!(tool_calls);
                    }
                    if !thinking.is_empty() {
                        asst_msg["reasoning_content"] = json!(thinking);
                    }
                    openai_messages.push(asst_msg);
                } else {
                    // role == "user"
                    let mut user_text = String::new();
                    let mut user_contents = Vec::new();
                    let mut tool_results = Vec::new();

                    for block in arr {
                        let block_type = block.get("type").and_then(JsonValue::as_str).unwrap_or_default();
                        match block_type {
                            "text" => {
                                if let Some(t) = block.get("text").and_then(JsonValue::as_str) {
                                    user_text.push_str(t);
                                    user_contents.push(json!({ "type": "text", "text": t }));
                                }
                            }
                            "image" => {
                                if let Some(src) = block.get("source") {
                                    let media_type = src.get("media_type").and_then(JsonValue::as_str).unwrap_or("image/png");
                                    let data = src.get("data").and_then(JsonValue::as_str).unwrap_or_default();
                                    user_contents.push(json!({
                                        "type": "image_url",
                                        "image_url": {
                                            "url": format!("data:{media_type};base64,{data}")
                                        }
                                    }));
                                }
                            }
                            "tool_result" => {
                                let tool_use_id = block.get("tool_use_id").and_then(JsonValue::as_str).unwrap_or_default();
                                let content_raw = block.get("content");
                                let result_text = if let Some(s) = content_raw.and_then(JsonValue::as_str) {
                                    s.to_string()
                                } else if let Some(sub_arr) = content_raw.and_then(JsonValue::as_array) {
                                    let mut st = String::new();
                                    for sub in sub_arr {
                                        if let Some(t) = sub.get("text").and_then(JsonValue::as_str) {
                                            st.push_str(t);
                                        }
                                    }
                                    st
                                } else {
                                    content_raw.map(|v| v.to_string()).unwrap_or_default()
                                };
                                tool_results.push((tool_use_id.to_string(), result_text));
                            }
                            _ => {}
                        }
                    }

                    if !user_contents.is_empty() {
                        if user_contents.len() == 1 && user_contents[0].get("type").and_then(JsonValue::as_str) == Some("text") {
                            openai_messages.push(json!({
                                "role": "user",
                                "content": user_text
                            }));
                        } else {
                            openai_messages.push(json!({
                                "role": "user",
                                "content": user_contents
                            }));
                        }
                    }

                    for (tool_call_id, res_content) in tool_results {
                        openai_messages.push(json!({
                            "role": "tool",
                            "tool_call_id": tool_call_id,
                            "content": res_content
                        }));
                    }
                }
            }
        }
    }

    let is_stream = body
        .get("stream")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);

    let mut openai_body = json!({
        "model": stripped_model,
        "messages": openai_messages,
        "stream": is_stream,
    });
    if let Some(t) = body.get("temperature") {
        openai_body["temperature"] = t.clone();
    } else {
        openai_body["temperature"] = json!(0.7);
    }
    if let Some(mt) = body.get("max_tokens") {
        openai_body["max_tokens"] = mt.clone();
    } else {
        openai_body["max_tokens"] = json!(4096);
    }
    if let Some(top_p) = body.get("top_p") {
        openai_body["top_p"] = top_p.clone();
    }
    if let Some(stop) = body.get("stop_sequences") {
        openai_body["stop"] = stop.clone();
    }
    if !openai_tools.is_empty() {
        openai_body["tools"] = json!(openai_tools);
    }
    if let Some(tc) = tool_choice_val {
        openai_body["tool_choice"] = tc;
    }

    let chan = match config.channels.iter().find(|c| c.id == "opencode" && c.enabled) {
        Some(c) => c,
        None => {
            ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": { "type": "api_error", "message": "OpenCode 上游渠道未启用" } })),
            )
                .into_response();
        }
    };

    let target_url = format!("{}/chat/completions", chan.upstream_url.trim_end_matches('/'));
    let auth_val = if chan.api_key.trim().is_empty() {
        "Bearer public".to_string()
    } else {
        format!("Bearer {}", chan.api_key.trim())
    };

    let candidates = get_sorted_egress_candidates(&ctx, chan).await;
    let total_candidates = candidates.len().max(1);
    let max_retries = ctx.config.read().await.max_retries as usize;
    let max_attempts = if chan.use_proxy_pool {
        (max_retries + 1).min(total_candidates)
    } else {
        max_retries + 1
    };

    let base_idx = ctx.active_egress_idx.load(Ordering::Relaxed);
    let mut final_res = None;
    let mut last_send_err = None;
    let mut used_cand_id = candidates.get(base_idx % candidates.len()).cloned().unwrap_or_else(|| "__direct__".to_string());

    for attempt in 0..max_attempts {
        let attempt_start = Instant::now();
        let cand_idx = (base_idx + attempt) % candidates.len();
        let cand_id = &candidates[cand_idx];
        used_cand_id = cand_id.clone();
        let client = build_client_for_candidate(&ctx, cand_id).await;

        let session_id = headers
            .get("x-opencode-session")
            .or_else(|| headers.get("session-id"))
            .or_else(|| headers.get("x-session-id"))
            .or_else(|| headers.get("conversation-id"))
            .or_else(|| headers.get("x-conversation-id"))
            .and_then(|v| v.to_str().ok())
            .map(String::from)
            .unwrap_or_else(|| "openhub-session".to_string());

        let mut req = client
            .post(&target_url)
            .header("Authorization", &auth_val)
            .header("Content-Type", "application/json")
            .header("User-Agent", "opencode/1.0.0")
            .header("x-opencode-client", "cli")
            .header("x-opencode-session", session_id)
            .header("Accept", if is_stream { "text/event-stream" } else { "application/json" });

        if let Some(cc) = headers.get("anthropic-beta") {
            req = req.header("anthropic-beta", cc);
        }

        let req = req.json(&openai_body);

        match req.send().await {
            Ok(r) => {
                let status = r.status();
                if !status.is_success() && attempt + 1 < max_attempts {
                    let err_body = r.text().await.unwrap_or_default();
                    record_failover_event(
                        &ctx,
                        &req_id,
                        "/v1/messages",
                        stripped_model,
                        is_stream,
                        status.as_u16(),
                        format!("节点失败自动切换：{}", format_upstream_error_message(status.as_u16(), &err_body)),
                        attempt_start.elapsed().as_millis() as u64,
                        req_body_str.clone(),
                        cand_id,
                    )
                    .await;
                    let next_idx = (cand_idx + 1) % candidates.len();
                    ctx.active_egress_idx.store(next_idx, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    continue;
                }
                if status.is_success() {
                    ctx.active_egress_idx.store(cand_idx, Ordering::Relaxed);
                }
                final_res = Some(r);
                break;
            }
            Err(e) => {
                let e_str = e.to_string();
                last_send_err = Some(e);
                if attempt + 1 < max_attempts {
                    record_failover_event(
                        &ctx,
                        &req_id,
                        "/v1/messages",
                        stripped_model,
                        is_stream,
                        502,
                        format!("节点连接失败自动切换：{e_str}"),
                        attempt_start.elapsed().as_millis() as u64,
                        req_body_str.clone(),
                        cand_id,
                    )
                    .await;
                    let next_idx = (cand_idx + 1) % candidates.len();
                    ctx.active_egress_idx.store(next_idx, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    continue;
                }
            }
        }
    }

    let node_name = Some(get_node_display_name(&ctx, &used_cand_id).await);

    let res = match final_res {
        Some(r) => r,
        None => {
            let err = last_send_err.map(|e| e.to_string()).unwrap_or_else(|| "Unknown error".to_string());
            let dur = start_time.elapsed().as_millis() as u64;
            ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
            ctx.record_log(ProxyRequestLog {
                id: req_id,
                timestamp: current_timestamp(),
                method: "POST".to_string(),
                path: "/v1/messages".to_string(),
                channel_id: "opencode".to_string(),
                model: stripped_model.to_string(),
                stream: is_stream,
                status_code: 502,
                duration_ms: dur,
                ttft_ms: None,
                prompt_tokens: None,
                prompt_cache_hit_tokens: None,
                prompt_cache_miss_tokens: None,
                completion_tokens: None,
                reasoning_tokens: None,
                total_tokens: None,
                error_message: Some(format!("Anthropic 消息转发上游失败: {err}")),
                request_body: req_body_str,
                response_body: None,
                node_name,
            }).await;

            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": {
                        "type": "api_error",
                        "message": format!("Failed to connect to upstream: {err}")
                    }
                })),
            )
                .into_response();
        }
    };

    if !res.status().is_success() {
        let dur = start_time.elapsed().as_millis() as u64;
        let status = res.status();
        ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
        let error_body = res.text().await.unwrap_or_default();
        let formatted_err = format_upstream_error_message(status.as_u16(), &error_body);

        ctx.record_log(ProxyRequestLog {
            id: req_id,
            timestamp: current_timestamp(),
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            channel_id: "opencode".to_string(),
            model: stripped_model.to_string(),
            stream: is_stream,
            status_code: status.as_u16(),
            duration_ms: dur,
            ttft_ms: None,
            prompt_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
            completion_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            error_message: Some(formatted_err),
            request_body: req_body_str,
            response_body: Some(error_body.clone()),
            node_name,
        }).await;

        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": { "type": "api_error", "message": error_body } })),
        )
            .into_response();
    }

    ctx.metrics.successful_requests.fetch_add(1, Ordering::Relaxed);

    if is_stream {
        let stream = openai_to_anthropic_sse_stream(
            ctx.clone(),
            req_id,
            start_time,
            "/v1/messages".to_string(),
            stripped_model.to_string(),
            req_body_str,
            node_name,
            res.bytes_stream(),
        );
        (
            StatusCode::OK,
            [
                (axum::http::header::CONTENT_TYPE, "text/event-stream"),
                (axum::http::header::CACHE_CONTROL, "no-cache"),
                (axum::http::header::CONNECTION, "keep-alive"),
            ],
            Body::from_stream(stream),
        )
            .into_response()
    } else {
        let ttft = start_time.elapsed().as_millis() as u64;
        let res_json = res.json::<JsonValue>().await;
        let dur = start_time.elapsed().as_millis() as u64;

        match res_json {
            Ok(chat_val) => {
                let text = chat_val
                    .pointer("/choices/0/message/content")
                    .and_then(JsonValue::as_str)
                    .unwrap_or_default();
                let reasoning = chat_val
                    .pointer("/choices/0/message/reasoning_content")
                    .and_then(JsonValue::as_str)
                    .unwrap_or_default();
                let prompt_tokens = chat_val.pointer("/usage/prompt_tokens").and_then(JsonValue::as_u64);
                let hit = chat_val.pointer("/usage/prompt_cache_hit_tokens")
                    .or_else(|| chat_val.pointer("/usage/prompt_tokens_details/cached_tokens"))
                    .and_then(JsonValue::as_u64);
                let miss = chat_val.pointer("/usage/prompt_cache_miss_tokens").and_then(JsonValue::as_u64);
                let completion_tokens = chat_val.pointer("/usage/completion_tokens").and_then(JsonValue::as_u64);
                let reasoning_tokens = chat_val.pointer("/usage/completion_tokens_details/reasoning_tokens")
                    .or_else(|| chat_val.pointer("/usage/reasoning_tokens"))
                    .and_then(JsonValue::as_u64);
                let total_tokens = chat_val.pointer("/usage/total_tokens").and_then(JsonValue::as_u64);

                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos();
                let mut content_items = Vec::new();
                if !reasoning.trim().is_empty() {
                    content_items.push(json!({ "type": "thinking", "thinking": reasoning }));
                }
                if !text.is_empty() {
                    content_items.push(json!({ "type": "text", "text": text }));
                }

                let mut stop_reason = "end_turn";
                if let Some(finish) = chat_val.pointer("/choices/0/finish_reason").and_then(JsonValue::as_str) {
                    match finish {
                        "tool_calls" => stop_reason = "tool_use",
                        "length" => stop_reason = "max_tokens",
                        _ => {}
                    }
                }

                if let Some(tool_calls) = chat_val.pointer("/choices/0/message/tool_calls").and_then(JsonValue::as_array) {
                    stop_reason = "tool_use";
                    for tc in tool_calls {
                        let id = tc.get("id").and_then(JsonValue::as_str).unwrap_or("call_default");
                        let name = tc.pointer("/function/name").and_then(JsonValue::as_str).unwrap_or_default();
                        let args_raw = tc.pointer("/function/arguments").and_then(JsonValue::as_str).unwrap_or("{}");
                        let args_parsed: JsonValue = serde_json::from_str(args_raw).unwrap_or_else(|_| json!({}));
                        content_items.push(json!({
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": args_parsed
                        }));
                    }
                }

                let anthropic_res = json!({
                    "id": format!("msg_{nanos:x}"),
                    "type": "message",
                    "role": "assistant",
                    "model": format!("opencode/{}", stripped_model),
                    "content": content_items,
                    "stop_reason": stop_reason,
                    "usage": {
                        "input_tokens": prompt_tokens.unwrap_or(0),
                        "output_tokens": completion_tokens.unwrap_or(0)
                    }
                });

                let formatted_resp = serde_json::to_string_pretty(&anthropic_res).unwrap_or_else(|_| anthropic_res.to_string());

                ctx.record_log(ProxyRequestLog {
                    id: req_id,
                    timestamp: current_timestamp(),
                    method: "POST".to_string(),
                    path: "/v1/messages".to_string(),
                    channel_id: "opencode".to_string(),
                    model: stripped_model.to_string(),
                    stream: false,
                    status_code: 200,
                    duration_ms: dur,
                    ttft_ms: Some(ttft),
                    prompt_tokens,
                    prompt_cache_hit_tokens: hit,
                    prompt_cache_miss_tokens: miss,
                    completion_tokens,
                    reasoning_tokens,
                    total_tokens,
                    error_message: None,
                    request_body: req_body_str,
                    response_body: Some(formatted_resp),
                    node_name,
                }).await;

                (StatusCode::OK, Json(anthropic_res)).into_response()
            }
            Err(err) => {
                ctx.record_log(ProxyRequestLog {
                    id: req_id,
                    timestamp: current_timestamp(),
                    method: "POST".to_string(),
                    path: "/v1/messages".to_string(),
                    channel_id: "opencode".to_string(),
                    model: stripped_model.to_string(),
                    stream: false,
                    status_code: 502,
                    duration_ms: dur,
                    ttft_ms: Some(ttft),
                    prompt_tokens: None,
                    prompt_cache_hit_tokens: None,
                    prompt_cache_miss_tokens: None,
                    completion_tokens: None,
                    reasoning_tokens: None,
                    total_tokens: None,
                    error_message: Some(format!("JSON 解析异常: {err}")),
                    request_body: req_body_str,
                    response_body: None,
                    node_name,
                }).await;

                (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({
                        "error": {
                            "type": "api_error",
                            "message": format!("Upstream JSON decode error: {err}")
                        }
                    })),
                )
                    .into_response()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 数据库持久化与加载
// ---------------------------------------------------------------------------

pub fn load_opencode_proxy_config(conn: &Connection) -> OpencodeProxyConfig {
    let row = conn.query_row(
        "SELECT value FROM app_meta WHERE key = 'opencode_proxy_config'",
        [],
        |r| r.get::<_, String>(0),
    );
    match row {
        Ok(json_str) => serde_json::from_str(&json_str).unwrap_or_default(),
        Err(_) => OpencodeProxyConfig::default(),
    }
}

pub fn save_opencode_proxy_config(conn: &Connection, config: &OpencodeProxyConfig) -> Result<(), String> {
    let json_str = serde_json::to_string(config).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO app_meta (key, value) VALUES ('opencode_proxy_config', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [&json_str],
    ).map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 服务启停控制器
// ---------------------------------------------------------------------------

pub async fn start_opencode_proxy_server(state: &OpencodeProxyState) -> Result<(), String> {
    let mut is_running = state.is_running.write().await;
    if *is_running {
        return Ok(());
    }

    let config = state.context.config.read().await.clone();
    let port = config.port;

    let router = create_opencode_proxy_router(state.context.clone());
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("无法绑定反代端口 {port}: {e}"))?;

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::broadcast::channel::<()>(1);
    *state.shutdown_tx.lock().await = Some(shutdown_tx);
    *state.current_port.write().await = port;
    *is_running = true;
    *state.context.started_at.write().await = Some(Instant::now());

    let ctx_clone = state.context.clone();
    tokio::spawn(async move {
        let cfg = ctx_clone.config.read().await.clone();
        if let Ok(models) = fetch_upstream_models_inner(&ctx_clone, &cfg).await {
            let mut cached = ctx_clone.cached_models.write().await;
            *cached = models;
            let mut updated = ctx_clone.cached_models_updated_at.write().await;
            *updated = Some(Instant::now());
        }
    });

    tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.recv().await;
            })
            .await
            .ok();
    });

    Ok(())
}

pub async fn stop_opencode_proxy_server(state: &OpencodeProxyState) -> Result<(), String> {
    let mut is_running = state.is_running.write().await;
    if !*is_running {
        return Ok(());
    }

    let mut shutdown_tx = state.shutdown_tx.lock().await;
    if let Some(tx) = shutdown_tx.take() {
        let _ = tx.send(());
    }

    *is_running = false;
    *state.context.started_at.write().await = None;
    Ok(())
}

pub async fn get_opencode_proxy_status_summary(state: &OpencodeProxyState) -> OpencodeProxyStatus {
    let is_running = *state.is_running.read().await;
    let port = *state.current_port.read().await;
    let metrics = &state.context.metrics;
    let uptime = if is_running {
        state
            .context
            .started_at
            .read()
            .await
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0)
    } else {
        0
    };
    let models_count = state.context.cached_models.read().await.len();
    let channels_count = state.context.config.read().await.channels.len();

    let (db_tot_req, db_succ_req, db_fail_req, db_prompt, db_completion, db_reasoning, db_reasoning_req, db_cache_hit, db_total) = {
        let app_opt = state.context.app_handle.read().await.clone();
        if let Some(app) = app_opt {
            let database = app.state::<crate::models::Database>();
            let counts = match database.0.lock() {
                Ok(conn) => {
                    let res: Result<(i64, i64, i64, i64, i64, i64, i64, i64, i64), _> = conn.query_row(
                        "SELECT 
                            COUNT(*),
                            COALESCE(SUM(CASE WHEN status_code >= 200 AND status_code < 300 THEN 1 ELSE 0 END), 0),
                            COALESCE(SUM(CASE WHEN status_code >= 400 THEN 1 ELSE 0 END), 0),
                            COALESCE(SUM(prompt_tokens), 0),
                            COALESCE(SUM(completion_tokens), 0),
                            COALESCE(SUM(reasoning_tokens), 0),
                            COALESCE(SUM(CASE WHEN COALESCE(reasoning_tokens, 0) > 0 THEN 1 ELSE 0 END), 0),
                            COALESCE(SUM(prompt_cache_hit_tokens), 0),
                            COALESCE(SUM(COALESCE(total_tokens, COALESCE(prompt_tokens, 0) + COALESCE(completion_tokens, 0))), 0)
                         FROM opencode_proxy_logs",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?)),
                    );
                    if let Ok(vals) = res {
                        (vals.0 as u64, vals.1 as u64, vals.2 as u64, vals.3 as u64, vals.4 as u64, vals.5 as u64, vals.6 as u64, vals.7 as u64, vals.8 as u64)
                    } else {
                        (0, 0, 0, 0, 0, 0, 0, 0, 0)
                    }
                }
                Err(_) => (0, 0, 0, 0, 0, 0, 0, 0, 0),
            };
            counts
        } else {
            (0, 0, 0, 0, 0, 0, 0, 0, 0)
        }
    };

    let total_requests = metrics.total_requests.load(Ordering::Relaxed).max(db_tot_req);
    let successful_requests = metrics.successful_requests.load(Ordering::Relaxed).max(db_succ_req);
    let failed_requests = metrics.failed_requests.load(Ordering::Relaxed).max(db_fail_req);
    let total_prompt_tokens = metrics.total_prompt_tokens.load(Ordering::Relaxed).max(db_prompt);
    let total_completion_tokens = metrics.total_completion_tokens.load(Ordering::Relaxed).max(db_completion);
    let total_reasoning_tokens = metrics.total_reasoning_tokens.load(Ordering::Relaxed).max(db_reasoning);
    let total_reasoning_requests = metrics.total_reasoning_requests.load(Ordering::Relaxed).max(db_reasoning_req);
    let total_cache_hit_tokens = metrics.total_cache_hit_tokens.load(Ordering::Relaxed).max(db_cache_hit);
    let total_tokens = metrics.total_tokens.load(Ordering::Relaxed).max(db_total);

    let today_prefix = chrono::Local::now().format("%Y-%m-%d").to_string();
    let today_total_tokens = {
        let app_opt = state.context.app_handle.read().await.clone();
        if let Some(app) = app_opt {
            let database = app.state::<crate::models::Database>();
            let val = match database.0.lock() {
                Ok(conn) => {
                    let res: Result<i64, _> = conn.query_row(
                        "SELECT COALESCE(SUM(COALESCE(total_tokens, COALESCE(prompt_tokens, 0) + COALESCE(completion_tokens, 0))), 0)
                         FROM opencode_proxy_logs
                         WHERE timestamp LIKE ?1",
                        [format!("{today_prefix}%")],
                        |r| r.get(0),
                    );
                    res.unwrap_or(0) as u64
                }
                Err(_) => 0,
            };
            val
        } else {
            0
        }
    };

    OpencodeProxyStatus {
        running: is_running,
        port,
        url: format!("http://127.0.0.1:{port}/v1"),
        total_requests,
        successful_requests,
        failed_requests,
        uptime_seconds: uptime,
        models_count,
        channels_count,
        total_prompt_tokens,
        total_completion_tokens,
        total_reasoning_tokens,
        total_reasoning_requests,
        total_cache_hit_tokens,
        total_tokens,
        today_total_tokens,
    }
}

// ---------------------------------------------------------------------------
// Tauri IPC 命令
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_opencode_proxy_config(
    database: tauri::State<'_, crate::models::Database>,
    state: tauri::State<'_, OpencodeProxyState>,
) -> Result<OpencodeProxyConfig, String> {
    let config = {
        let conn = database.0.lock().map_err(|e| e.to_string())?;
        load_opencode_proxy_config(&conn)
    };
    *state.context.config.write().await = config.clone();
    Ok(config)
}

#[tauri::command]
pub async fn save_opencode_proxy_config_cmd(
    database: tauri::State<'_, crate::models::Database>,
    state: tauri::State<'_, OpencodeProxyState>,
    config: OpencodeProxyConfig,
) -> Result<OpencodeProxyStatus, String> {
    {
        let conn = database.0.lock().map_err(|e| e.to_string())?;
        save_opencode_proxy_config(&conn, &config)?;
    }
    *state.context.config.write().await = config.clone();
    if config.enabled {
        let is_running = *state.is_running.read().await;
        let current_port = *state.current_port.read().await;
        if is_running && current_port != config.port {
            stop_opencode_proxy_server(&state).await?;
            start_opencode_proxy_server(&state).await?;
        } else if !is_running {
            start_opencode_proxy_server(&state).await?;
        }
    } else {
        stop_opencode_proxy_server(&state).await?;
    }
    Ok(get_opencode_proxy_status_summary(&state).await)
}

#[tauri::command]
pub async fn get_opencode_proxy_status(
    state: tauri::State<'_, OpencodeProxyState>,
) -> Result<OpencodeProxyStatus, String> {
    Ok(get_opencode_proxy_status_summary(&state).await)
}

#[tauri::command]
pub async fn start_opencode_proxy(
    state: tauri::State<'_, OpencodeProxyState>,
) -> Result<OpencodeProxyStatus, String> {
    start_opencode_proxy_server(&state).await?;
    Ok(get_opencode_proxy_status_summary(&state).await)
}

#[tauri::command]
pub async fn stop_opencode_proxy(
    state: tauri::State<'_, OpencodeProxyState>,
) -> Result<OpencodeProxyStatus, String> {
    stop_opencode_proxy_server(&state).await?;
    Ok(get_opencode_proxy_status_summary(&state).await)
}

#[tauri::command]
pub async fn fetch_opencode_models(
    state: tauri::State<'_, OpencodeProxyState>,
) -> Result<Vec<String>, String> {
    let cfg = state.context.config.read().await.clone();
    let models = fetch_upstream_models_inner(&state.context, &cfg).await?;
    let mut cached = state.context.cached_models.write().await;
    *cached = models.clone();
    let mut updated = state.context.cached_models_updated_at.write().await;
    *updated = Some(Instant::now());
    Ok(models)
}

#[tauri::command]
pub async fn test_opencode_proxy_health(
    state: tauri::State<'_, OpencodeProxyState>,
) -> Result<serde_json::Value, String> {
    let port = *state.current_port.read().await;
    let url = format!("http://127.0.0.1:{port}/healthz");
    let res = state
        .context
        .default_http_client
        .get(&url)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| format!("健康检查连接失败: {e}"))?;
    let val = res
        .json::<serde_json::Value>()
        .await
        .map_err(|e| format!("解析健康检查响应失败: {e}"))?;
    Ok(val)
}

/// 分页 + 状态过滤 + 关键词搜索的日志查询结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyLogPage {
    pub items: Vec<ProxyRequestLog>,
    pub total: u64,
    pub success_total: u64,
    pub error_total: u64,
    /// 全库计数（不受当前 filter/搜索影响），供前端标签固定显示
    pub global_total: u64,
    pub global_success_total: u64,
    pub global_error_total: u64,
}

#[tauri::command]
pub async fn get_opencode_proxy_logs(
    database: tauri::State<'_, crate::models::Database>,
    _state: tauri::State<'_, OpencodeProxyState>,
    page: Option<usize>,
    page_size: Option<usize>,
    filter: Option<String>,
    q: Option<String>,
    sort_by: Option<String>,
    sort_order: Option<String>,
) -> Result<ProxyLogPage, String> {
    let page = page.unwrap_or(1).max(1);
    let page_size = page_size.unwrap_or(50).clamp(1, 200);
    let offset = (page - 1) * page_size;

    let mut where_sql = String::from("WHERE 1=1");
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    match filter.as_deref() {
        Some("success") => where_sql.push_str(" AND status_code >= 200 AND status_code < 300"),
        Some("error") => where_sql.push_str(" AND status_code >= 400"),
        _ => {}
    }
    if let Some(kw) = q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        where_sql.push_str(
            " AND (model LIKE ? OR path LIKE ? OR error_message LIKE ? OR CAST(status_code AS TEXT) LIKE ?)",
        );
        let pat = format!("%{kw}%");
        params.push(Box::new(pat.clone()));
        params.push(Box::new(pat.clone()));
        params.push(Box::new(pat.clone()));
        params.push(Box::new(pat));
    }

    let conn = database.0.lock().map_err(|e| e.to_string())?;
    let total: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM opencode_proxy_logs {where_sql}"),
            rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let success_total: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM opencode_proxy_logs {where_sql} AND status_code >= 200 AND status_code < 300"),
            rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let error_total: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM opencode_proxy_logs {where_sql} AND status_code >= 400"),
            rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;

    // 排序：白名单映射列名，杜绝注入；timestamp 用真实时间戳列排序
    let order_expr = match sort_by.as_deref() {
        Some("status") => "status_code",
        Some("model") => "model COLLATE NOCASE",
        Some("tokens") => "COALESCE(total_tokens, COALESCE(prompt_tokens, 0) + COALESCE(completion_tokens, 0))",
        Some("duration") => "duration_ms",
        _ => "created_at",
    };
    let order_dir = if sort_order.as_deref() == Some("asc") { "ASC" } else { "DESC" };

    let mut stmt = conn.prepare(
        &format!(
            "SELECT id, timestamp, method, path, channel_id, model, stream,
                    status_code, duration_ms, ttft_ms, prompt_tokens, prompt_cache_hit_tokens,
                    prompt_cache_miss_tokens, completion_tokens, reasoning_tokens, total_tokens,
                    error_message, request_body, response_body, node_name
             FROM opencode_proxy_logs
             {where_sql}
             ORDER BY {order_expr} {order_dir}, rowid {order_dir} LIMIT ?1 OFFSET ?2"
        )
    )
    .map_err(|e| e.to_string())?;

    let mut rows = Vec::new();
    {
        let mut query_params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        query_params.extend(params.drain(..));
        query_params.push(Box::new(page_size as i64));
        query_params.push(Box::new(offset as i64));
        let mut iter = stmt
            .query_map(rusqlite::params_from_iter(query_params.iter().map(|p| p.as_ref())), |row| {
                let stream_int: i64 = row.get(6)?;
                Ok(ProxyRequestLog {
                    id: row.get(0)?,
                    timestamp: row.get(1)?,
                    method: row.get(2)?,
                    path: row.get(3)?,
                    channel_id: row.get(4)?,
                    model: row.get(5)?,
                    stream: stream_int != 0,
                    status_code: row.get::<_, i64>(7)? as u16,
                    duration_ms: row.get::<_, i64>(8)? as u64,
                    ttft_ms: row.get::<_, Option<i64>>(9)?.map(|v| v as u64),
                    prompt_tokens: row.get::<_, Option<i64>>(10)?.map(|v| v as u64),
                    prompt_cache_hit_tokens: row.get::<_, Option<i64>>(11)?.map(|v| v as u64),
                    prompt_cache_miss_tokens: row.get::<_, Option<i64>>(12)?.map(|v| v as u64),
                    completion_tokens: row.get::<_, Option<i64>>(13)?.map(|v| v as u64),
                    reasoning_tokens: row.get::<_, Option<i64>>(14)?.map(|v| v as u64),
                    total_tokens: row.get::<_, Option<i64>>(15)?.map(|v| v as u64),
                    error_message: row.get(16)?,
                    request_body: row.get(17)?,
                    response_body: row.get(18)?,
                    node_name: row.get(19)?,
                })
            })
            .map_err(|e| e.to_string())?;
        while let Some(l) = iter.next() {
            if let Ok(l) = l {
                rows.push(l);
            }
        }
    }

    let global_total: i64 = conn
        .query_row("SELECT COUNT(*) FROM opencode_proxy_logs", [], |r| r.get(0))
        .unwrap_or(0);
    let global_success_total: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM opencode_proxy_logs WHERE status_code >= 200 AND status_code < 300",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let global_error_total: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM opencode_proxy_logs WHERE status_code >= 400",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    Ok(ProxyLogPage {
        items: rows,
        total: total as u64,
        success_total: success_total as u64,
        error_total: error_total as u64,
        global_total: global_total as u64,
        global_success_total: global_success_total as u64,
        global_error_total: global_error_total as u64,
    })
}

#[tauri::command]
pub async fn clear_opencode_proxy_logs(
    database: tauri::State<'_, crate::models::Database>,
    state: tauri::State<'_, OpencodeProxyState>,
    mode: Option<String>,
) -> Result<(), String> {
    let clear_mode = mode.as_deref().unwrap_or("all");

    if clear_mode == "payload_only" || clear_mode == "details_only" {
        // 仅清空请求和响应的详细报文内容（释放大体积存储，保留请求元数据及 Token 统计）
        if let Ok(conn) = database.0.lock() {
            let _ = conn.execute("UPDATE opencode_proxy_logs SET request_body = NULL, response_body = NULL", []);
        }
    } else {
        // 全量清空所有历史记录
        if let Ok(conn) = database.0.lock() {
            let _ = conn.execute("DELETE FROM opencode_proxy_logs", []);
        }

        state.context.metrics.total_requests.store(0, Ordering::Relaxed);
        state.context.metrics.successful_requests.store(0, Ordering::Relaxed);
        state.context.metrics.failed_requests.store(0, Ordering::Relaxed);
        state.context.metrics.total_prompt_tokens.store(0, Ordering::Relaxed);
        state.context.metrics.total_completion_tokens.store(0, Ordering::Relaxed);
        state.context.metrics.total_reasoning_tokens.store(0, Ordering::Relaxed);
        state.context.metrics.total_reasoning_requests.store(0, Ordering::Relaxed);
        state.context.metrics.total_cache_hit_tokens.store(0, Ordering::Relaxed);
        state.context.metrics.total_tokens.store(0, Ordering::Relaxed);
    }

    Ok(())
}
