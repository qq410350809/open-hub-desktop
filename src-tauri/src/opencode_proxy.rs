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
use std::collections::HashMap;
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
    /// 站点转换继承的多个原 Key（请求时自动轮换尝试）；为空时回退使用 api_key。
    #[serde(default)]
    pub api_keys: Vec<String>,
    #[serde(default)]
    pub use_proxy_pool: bool,
    /// 英文别名：作为网关模型前缀（如 opencode/*、mysite/*）。
    /// 全部渠道（含 opencode）别名必须唯一；为空时回退为渠道 id。
    #[serde(default)]
    pub alias: String,
    /// 通过「站点转换」创建时关联的站点库站点 id
    #[serde(default)]
    pub site_id: Option<String>,
    /// 代理池固定通道：开启后始终经代理池出口节点转发，不优先直连
    #[serde(default)]
    pub use_fixed_proxy: bool,
    /// 该渠道对外暴露的模型白名单：
    /// - `None`（默认，兼容旧配置）= 全部启用
    /// - `Some(空列表)` = 不对外暴露任何模型
    /// - `Some(非空列表)` = 仅列表中勾选的模型在可用模型中体现
    #[serde(default)]
    pub enabled_models: Option<Vec<String>>,
}

impl ChannelConfig {
    /// 渠道的生效别名：显式配置的别名（小写化）为空时回退为渠道 id。
    pub fn effective_alias(&self) -> String {
        let a = self.alias.trim().to_lowercase();
        if a.is_empty() {
            self.id.trim().to_lowercase()
        } else {
            a
        }
    }

    /// 渠道可用的 Authorization 值列表：优先多 Key（api_keys），其次单 Key（api_key），都没有则 Bearer public。
    pub fn auth_values(&self) -> Vec<String> {
        let keys: Vec<String> = if !self.api_keys.is_empty() {
            self.api_keys
                .iter()
                .map(|k| k.trim().to_string())
                .filter(|k| !k.is_empty())
                .collect()
        } else if !self.api_key.trim().is_empty() {
            vec![self.api_key.trim().to_string()]
        } else {
            Vec::new()
        };
        if keys.is_empty() {
            vec!["Bearer public".to_string()]
        } else {
            keys.into_iter().map(|k| format!("Bearer {k}")).collect()
        }
    }

    /// 上游 OpenAI 兼容 API 根地址：路径已以 /vN（如 /v1、/zen/v1）结尾时原样返回；
    /// 否则（如站点首页地址 https://x666.me/）补全 /v1，保证 /models、/chat/completions
    /// 等子路径可访问。与站点库拉取模型时 join("/v1/models") 的语义保持一致。
    pub fn upstream_api_base(&self) -> String {
        let url = self.upstream_url.trim();
        let trimmed = url.trim_end_matches('/');
        let path = trimmed.split(['?', '#']).next().unwrap_or(trimmed);
        let last_seg = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("");
        let has_version = last_seg.len() > 1
            && last_seg.starts_with('v')
            && last_seg[1..].chars().all(|c| c.is_ascii_digit());
        if has_version {
            trimmed.to_string()
        } else {
            format!("{trimmed}/v1")
        }
    }
}

/// 将空别名补全为渠道 id，并统一小写化；把单 Key 回填进多 Key 列表，保证配置语义一致。
fn normalize_channel_config(config: &mut OpencodeProxyConfig) {
    for ch in config.channels.iter_mut() {
        if ch.alias.trim().is_empty() {
            ch.alias = ch.id.trim().to_lowercase();
        } else {
            ch.alias = ch.alias.trim().to_lowercase();
        }
        if ch.api_keys.is_empty() {
            let single = ch.api_key.trim().to_string();
            if !single.is_empty() {
                ch.api_keys = vec![single];
            }
        }
        // 两种渠道能力区分：站点转换渠道仅支持「代理池固定通道」；官方通道仅支持「内部代理池轮询」
        if ch.site_id.is_some() {
            ch.use_proxy_pool = false;
        } else {
            ch.use_fixed_proxy = false;
        }
    }
}

/// 校验渠道配置合法性：id 唯一、别名唯一（含 opencode）、别名仅含英文与连字符。
fn validate_channel_config(config: &OpencodeProxyConfig) -> Result<(), String> {
    let mut ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut aliases: std::collections::HashSet<String> = std::collections::HashSet::new();
    for ch in &config.channels {
        if ch.id.trim().is_empty() {
            return Err("渠道 id 不能为空".to_string());
        }
        if !ids.insert(ch.id.clone()) {
            return Err(format!("渠道 id「{}」重复，请修正后重试", ch.id));
        }
        let alias = ch.effective_alias();
        if alias.is_empty() {
            return Err(format!("渠道「{}」缺少英文别名", ch.name));
        }
        if !alias
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(format!(
                "渠道「{}」的英文别名「{alias}」只能包含英文字母、数字、- 与 _",
                ch.name
            ));
        }
        if !aliases.insert(alias.clone()) {
            return Err(format!(
                "英文别名「{alias}」已存在，所有渠道（含 opencode）的别名不能重复"
            ));
        }
    }
    Ok(())
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
        api_keys: Vec::new(),
        use_proxy_pool: false,
        alias: "opencode".to_string(),
        site_id: None,
        use_fixed_proxy: false,
        enabled_models: None,
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
    /// 按渠道拆分的累计计数（key = channel_id），随 record_log 逐条更新
    pub channel: std::sync::Mutex<HashMap<String, ChannelMetrics>>,
}

/// 单个渠道的累计使用计数（内存态，随请求实时累加）。
#[derive(Default)]
pub struct ChannelMetrics {
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

/// 单个渠道拉取到的模型列表（含渠道别名，用于网关聚合 /v1/models）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelModelList {
    pub channel_id: String,
    pub alias: String,
    pub models: Vec<String>,
}

/// 单个渠道拉取模型失败的原因（供前端弹窗空态展示）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelModelFetchError {
    pub channel_id: String,
    pub channel_name: String,
    pub error: String,
}

/// `fetch_opencode_models` 的返回：各渠道模型列表 + 拉取失败明细。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct ChannelModelsFetchResult {
    pub channels: Vec<ChannelModelList>,
    pub errors: Vec<ChannelModelFetchError>,
}

#[derive(Clone)]
pub struct OpencodeProxyContext {
    pub config: Arc<RwLock<OpencodeProxyConfig>>,
    pub metrics: Arc<OpencodeProxyMetrics>,
    pub started_at: Arc<RwLock<Option<Instant>>>,
    /// 按渠道缓存的模型列表（渠道 id + 别名 + 裸模型名）
    pub cached_channel_models: Arc<RwLock<Vec<ChannelModelList>>>,
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

        // 按渠道累加使用统计（供渠道卡片展示累计请求/成功率/Token）
        {
            let mut channels = self.metrics.channel.lock().unwrap();
            let cm = channels.entry(log.channel_id.clone()).or_default();
            cm.total_requests.fetch_add(1, Ordering::Relaxed);
            if (200..300).contains(&log.status_code) {
                cm.successful_requests.fetch_add(1, Ordering::Relaxed);
            } else if log.status_code >= 400 {
                cm.failed_requests.fetch_add(1, Ordering::Relaxed);
            }
            if let Some(pt) = log.prompt_tokens {
                cm.total_prompt_tokens.fetch_add(pt, Ordering::Relaxed);
            }
            if let Some(ct) = log.completion_tokens {
                cm.total_completion_tokens.fetch_add(ct, Ordering::Relaxed);
            }
            if let Some(rt) = log.reasoning_tokens {
                cm.total_reasoning_tokens.fetch_add(rt, Ordering::Relaxed);
                if rt > 0 {
                    cm.total_reasoning_requests.fetch_add(1, Ordering::Relaxed);
                }
            }
            if let Some(hit) = log.prompt_cache_hit_tokens {
                cm.total_cache_hit_tokens.fetch_add(hit, Ordering::Relaxed);
            }
            if tot > 0 {
                cm.total_tokens.fetch_add(tot, Ordering::Relaxed);
            }
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
            cached_channel_models: Arc::new(RwLock::new(Vec::new())),
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
    channel_id: &str,
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
        channel_id: channel_id.to_string(),
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

// ---------------------------------------------------------------------------
// SSE 流式帧提取器（支持字节级 UTF-8 拼包、CRLF / LF 混合换行与完整 JSON 校验）
// ---------------------------------------------------------------------------

#[derive(Default)]
struct SseFrameExtractor {
    byte_buffer: Vec<u8>,
}

struct SseEventBlock {
    event_type: Option<String>,
    data_lines: Vec<String>,
    raw_block: String,
    is_done: bool,
}

impl SseFrameExtractor {
    fn new() -> Self {
        Self {
            byte_buffer: Vec::new(),
        }
    }

    fn push_bytes(&mut self, chunk: &[u8]) {
        self.byte_buffer.extend_from_slice(chunk);
    }

    /// 从字节缓冲区中提取所有已就绪的完整 SSE 事件块
    fn extract_blocks(&mut self) -> Vec<SseEventBlock> {
        let mut blocks = Vec::new();

        loop {
            let valid_len = match std::str::from_utf8(&self.byte_buffer) {
                Ok(_) => self.byte_buffer.len(),
                Err(e) => e.valid_up_to(),
            };

            if valid_len == 0 {
                break;
            }

            let valid_str = match std::str::from_utf8(&self.byte_buffer[..valid_len]) {
                Ok(s) => s,
                Err(_) => break,
            };

            // 识别各类标准与非标准 SSE 分隔符：\r\n\r\n, \n\n, \n\r\n
            let block_end = if let Some(pos) = valid_str.find("\r\n\r\n") {
                Some((pos, pos + 4))
            } else if let Some(pos) = valid_str.find("\n\n") {
                Some((pos, pos + 2))
            } else if let Some(pos) = valid_str.find("\n\r\n") {
                Some((pos, pos + 3))
            } else {
                None
            };

            if let Some((content_end, total_end)) = block_end {
                let block_text = valid_str[..content_end].to_string();
                self.byte_buffer.drain(..total_end);

                if let Some(b) = Self::parse_block(&block_text) {
                    blocks.push(b);
                }
            } else {
                break;
            }
        }

        blocks
    }

    /// 流终止时刷新并提取剩余数据块（若为完整格式）
    fn flush_remaining(&mut self) -> Option<SseEventBlock> {
        let valid_len = match std::str::from_utf8(&self.byte_buffer) {
            Ok(_) => self.byte_buffer.len(),
            Err(e) => e.valid_up_to(),
        };
        if valid_len == 0 {
            self.byte_buffer.clear();
            return None;
        }
        let valid_str = std::str::from_utf8(&self.byte_buffer[..valid_len]).unwrap_or("");
        let block_text = valid_str.to_string();
        self.byte_buffer.clear();
        Self::parse_block(&block_text)
    }

    fn parse_block(text: &str) -> Option<SseEventBlock> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }

        let mut event_type = None;
        let mut data_lines = Vec::new();
        let mut is_done = false;

        for raw_line in text.lines() {
            let line = raw_line.trim_end_matches(['\r', '\n']).trim();
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            if let Some(ev) = line.strip_prefix("event:") {
                event_type = Some(ev.trim().to_string());
            } else if let Some(data) = line.strip_prefix("data:") {
                let d = data.trim();
                if d == "[DONE]" {
                    is_done = true;
                } else if !d.is_empty() {
                    data_lines.push(d.to_string());
                }
            }
        }

        if !data_lines.is_empty() || is_done || event_type.is_some() {
            Some(SseEventBlock {
                event_type,
                data_lines,
                raw_block: trimmed.to_string(),
                is_done,
            })
        } else {
            None
        }
    }
}

fn clean_sse_stream(
    ctx: OpencodeProxyContext,
    req_id: String,
    start_time: Instant,
    path: String,
    channel_id: String,
    model: String,
    req_body_str: Option<String>,
    node_name: Option<String>,
    stream: impl futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin + Send + 'static,
) -> impl futures_util::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static {
    async_stream::stream! {
        let mut stream = stream;
        let mut extractor = SseFrameExtractor::new();
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

        struct ToolCallAccumulator {
            id: String,
            name: String,
            arguments: String,
        }
        let mut collected_tools: std::collections::BTreeMap<usize, ToolCallAccumulator> = std::collections::BTreeMap::new();

        while let Some(item) = stream.next().await {
            if finished {
                break;
            }
            match item {
                Ok(chunk) => {
                    extractor.push_bytes(&chunk);
                    let blocks = extractor.extract_blocks();

                    for block in blocks {
                        if block.is_done {
                            finished = true;
                            yield Ok(bytes::Bytes::from("data: [DONE]\n\n"));
                            break;
                        }

                        if !block.data_lines.is_empty() {
                            let json_payload = block.data_lines.join("\n");
                            if let Ok(mut val) = serde_json::from_str::<JsonValue>(&json_payload) {
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

                                // 提取 content / reasoning_content / tool_calls 分片
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
                                    if let Some(tool_calls_arr) = delta.get("tool_calls").and_then(JsonValue::as_array) {
                                        for tc in tool_calls_arr {
                                            let index = tc.get("index").and_then(JsonValue::as_u64).unwrap_or(0) as usize;
                                            let id = tc.get("id").and_then(JsonValue::as_str);
                                            let name = tc.pointer("/function/name").and_then(JsonValue::as_str);
                                            let args = tc.pointer("/function/arguments").and_then(JsonValue::as_str).unwrap_or_default();
                                            let entry = collected_tools.entry(index).or_insert_with(|| ToolCallAccumulator {
                                                id: String::new(),
                                                name: String::new(),
                                                arguments: String::new(),
                                            });
                                            if let Some(id_str) = id {
                                                if !id_str.is_empty() { entry.id = id_str.to_string(); }
                                            }
                                            if let Some(n) = name {
                                                if !n.is_empty() { entry.name = n.to_string(); }
                                            }
                                            entry.arguments.push_str(args);
                                        }
                                    }
                                }

                                // 规范化 choices 中的 delta（补全 function.name 字符串，防止客户端 Zod 校验报 Expected 'function.name' to be a string）
                                if let Some(choices) = val.get_mut("choices").and_then(JsonValue::as_array_mut) {
                                    for choice in choices {
                                        if let Some(delta) = choice.get_mut("delta").and_then(JsonValue::as_object_mut) {
                                            // 过滤 DeepSeek-R1 / V3 等 choices 偶发的空 content null 字段
                                            if delta.get("content").map_or(false, |c| c.is_null()) {
                                                if delta.get("reasoning_content").is_none() {
                                                    delta["content"] = JsonValue::String(String::new());
                                                }
                                            }

                                            // 规范化 tool_calls：确保 function 存在且 function.name 始终为合法 string
                                            if let Some(tool_calls_arr) = delta.get_mut("tool_calls").and_then(JsonValue::as_array_mut) {
                                                for tc in tool_calls_arr {
                                                    let index = tc.get("index").and_then(JsonValue::as_u64).unwrap_or(0) as usize;
                                                    let accum_name = collected_tools.get(&index).map(|a| a.name.clone()).unwrap_or_default();
                                                    let accum_id = collected_tools.get(&index).map(|a| a.id.clone()).unwrap_or_default();

                                                    if let Some(tc_obj) = tc.as_object_mut() {
                                                        if !tc_obj.contains_key("index") {
                                                            tc_obj.insert("index".to_string(), json!(index));
                                                        }
                                                        if !tc_obj.contains_key("type") {
                                                            tc_obj.insert("type".to_string(), json!("function"));
                                                        }
                                                        if !accum_id.is_empty() && !tc_obj.contains_key("id") {
                                                            tc_obj.insert("id".to_string(), json!(accum_id));
                                                        }

                                                        if let Some(func) = tc_obj.get_mut("function").and_then(JsonValue::as_object_mut) {
                                                            if func.get("name").map_or(true, |v| v.is_null() || v.as_str().map_or(true, |s| s.is_empty())) {
                                                                func.insert("name".to_string(), json!(accum_name));
                                                            }
                                                            if func.get("arguments").map_or(true, |v| v.is_null()) {
                                                                func.insert("arguments".to_string(), json!(""));
                                                            }
                                                        } else {
                                                            tc_obj.insert("function".to_string(), json!({
                                                                "name": accum_name,
                                                                "arguments": ""
                                                            }));
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }

                                if let Ok(serialized) = serde_json::to_string(&val) {
                                    if let Some(ref ev) = block.event_type {
                                        yield Ok(bytes::Bytes::from(format!("event: {ev}\ndata: {serialized}\n\n")));
                                    } else {
                                        yield Ok(bytes::Bytes::from(format!("data: {serialized}\n\n")));
                                    }
                                    continue;
                                }
                            } else if json_payload.starts_with('{') || json_payload.starts_with('[') {
                                // 截断或残缺 JSON 分片，不向客户端转发残片，避免破坏客户端解析器
                                continue;
                            }
                        }

                        // 如果非 JSON 事件且有 raw_block，保持标准 SSE 格式输出
                        if !block.raw_block.is_empty() {
                            yield Ok(bytes::Bytes::from(format!("{}\n\n", block.raw_block)));
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
                    if !collected_tools.is_empty() {
                        if !interrupted_preview.is_empty() { interrupted_preview.push_str("\n\n"); }
                        for (_idx, tc) in &collected_tools {
                            interrupted_preview.push_str(&format!("[工具调用] {}({})\n", tc.name, tc.arguments));
                        }
                    }
                    ctx.record_log(ProxyRequestLog {
                        id: req_id.clone(),
                        timestamp: current_timestamp(),
                        method: "POST".to_string(),
                        path: path.clone(),
                        channel_id: channel_id.clone(),
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

        if !finished {
            if let Some(rem_block) = extractor.flush_remaining() {
                if !rem_block.data_lines.is_empty() {
                    let payload = rem_block.data_lines.join("\n");
                    if let Ok(val) = serde_json::from_str::<JsonValue>(&payload) {
                        if let Ok(serialized) = serde_json::to_string(&val) {
                            yield Ok(bytes::Bytes::from(format!("data: {serialized}\n\n")));
                        }
                    }
                }
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
        if !collected_tools.is_empty() {
            if !response_preview.is_empty() { response_preview.push_str("\n\n"); }
            for (_idx, tc) in &collected_tools {
                response_preview.push_str(&format!("[工具调用] {}({})\n", tc.name, tc.arguments));
            }
        }

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
            channel_id,
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
    channel_id: String,
    model_name: String,
    req_body_str: Option<String>,
    node_name: Option<String>,
    stream: impl futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin + Send + 'static,
) -> impl futures_util::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static {
    async_stream::stream! {
        let mut stream = stream;
        let mut extractor = SseFrameExtractor::new();
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
                    extractor.push_bytes(&chunk);
                    let blocks = extractor.extract_blocks();

                    for block in blocks {
                        if block.is_done {
                            finished = true;
                            break;
                        }

                        if !block.data_lines.is_empty() {
                            let json_payload = block.data_lines.join("\n");
                            if let Ok(val) = serde_json::from_str::<JsonValue>(&json_payload) {
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

                                    // 提取 reasoning_content 思考流
                                    if let Some(rc) = delta.get("reasoning_content")
                                        .or_else(|| delta.get("reasoning"))
                                        .and_then(JsonValue::as_str)
                                    {
                                        if !rc.is_empty() {
                                            collected_reasoning.push_str(rc);

                                            if active_block.as_deref() != Some("thinking") {
                                                if active_block.is_some() {
                                                    let stop = json!({ "type": "content_block_stop", "index": current_block_index });
                                                    yield Ok(bytes::Bytes::from(format!("event: content_block_stop\ndata: {stop}\n\n")));
                                                    current_block_index += 1;
                                                }
                                                active_block = Some("thinking".to_string());
                                                let start = json!({
                                                    "type": "content_block_start",
                                                    "index": current_block_index,
                                                    "content_block": { "type": "thinking", "thinking": "" }
                                                });
                                                yield Ok(bytes::Bytes::from(format!("event: content_block_start\ndata: {start}\n\n")));
                                            }

                                            let block_delta = json!({
                                                "type": "content_block_delta",
                                                "index": current_block_index,
                                                "delta": { "type": "thinking_delta", "thinking": rc }
                                            });
                                            yield Ok(bytes::Bytes::from(format!("event: content_block_delta\ndata: {block_delta}\n\n")));
                                        }
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
                                                let new_name = tc_name.unwrap_or("tool").to_string();

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
                        channel_id: channel_id.clone(),
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
                        error_message: Some(format!("Anthropic 流式响应传输中断: {e}")),
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
            channel_id,
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

fn openai_to_responses_sse_stream(
    ctx: OpencodeProxyContext,
    req_id: String,
    start_time: Instant,
    path: String,
    channel_id: String,
    model_name: String,
    req_body_str: Option<String>,
    node_name: Option<String>,
    stream: impl futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin + Send + 'static,
) -> impl futures_util::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static {
    async_stream::stream! {
        let mut stream = stream;
        let mut extractor = SseFrameExtractor::new();
        let mut resp_created_sent = false;
        let mut msg_item_added = false;
        let mut content_part_added = false;
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

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let resp_id = format!("resp_{:x}", nanos);
        let msg_item_id = format!("item_msg_{:x}", nanos);

        let mut next_output_index = 0usize;
        let mut msg_output_index = 0usize;

        struct ActiveCall {
            item_id: String,
            call_id: String,
            name: String,
            arguments: String,
            output_index: usize,
        }
        let mut active_tool_calls: std::collections::BTreeMap<usize, ActiveCall> = std::collections::BTreeMap::new();

        while let Some(item) = stream.next().await {
            if finished {
                break;
            }
            match item {
                Ok(chunk) => {
                    extractor.push_bytes(&chunk);
                    let blocks = extractor.extract_blocks();

                    for block in blocks {
                        if block.is_done {
                            finished = true;
                            break;
                        }

                        if !block.data_lines.is_empty() {
                            let json_payload = block.data_lines.join("\n");
                            if let Ok(val) = serde_json::from_str::<JsonValue>(&json_payload) {
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

                                // 首包发出 response.created
                                if !resp_created_sent {
                                    resp_created_sent = true;
                                    if ttft_ms.is_none() {
                                        ttft_ms = Some(start_time.elapsed().as_millis() as u64);
                                    }
                                    let created_ev = json!({
                                        "type": "response.created",
                                        "response": {
                                            "id": resp_id.clone(),
                                            "object": "response",
                                            "status": "in_progress",
                                            "model": format!("opencode/{}", model_name),
                                            "output": [],
                                            "usage": null
                                        }
                                    });
                                    yield Ok(bytes::Bytes::from(format!("event: response.created\ndata: {created_ev}\n\n")));
                                }

                                if let Some(delta) = val.pointer("/choices/0/delta") {
                                    if ttft_ms.is_none() {
                                        ttft_ms = Some(start_time.elapsed().as_millis() as u64);
                                    }

                                    if let Some(rc) = delta.get("reasoning_content")
                                        .or_else(|| delta.get("reasoning"))
                                        .and_then(JsonValue::as_str)
                                    {
                                        collected_reasoning.push_str(rc);
                                    }

                                    // 处理文本增量
                                    if let Some(c) = delta.get("content").and_then(JsonValue::as_str) {
                                        if !c.is_empty() {
                                            collected_content.push_str(c);

                                            if !msg_item_added {
                                                msg_item_added = true;
                                                msg_output_index = next_output_index;
                                                next_output_index += 1;
                                                let item_added_ev = json!({
                                                    "type": "response.output_item.added",
                                                    "output_index": msg_output_index,
                                                    "item": {
                                                        "id": msg_item_id.clone(),
                                                        "type": "message",
                                                        "role": "assistant",
                                                        "content": []
                                                    }
                                                });
                                                yield Ok(bytes::Bytes::from(format!("event: response.output_item.added\ndata: {item_added_ev}\n\n")));
                                            }

                                            if !content_part_added {
                                                content_part_added = true;
                                                let part_added_ev = json!({
                                                    "type": "response.content_part.added",
                                                    "item_id": msg_item_id.clone(),
                                                    "output_index": msg_output_index,
                                                    "content_index": 0,
                                                    "part": {
                                                        "type": "output_text",
                                                        "text": ""
                                                    }
                                                });
                                                yield Ok(bytes::Bytes::from(format!("event: response.content_part.added\ndata: {part_added_ev}\n\n")));
                                            }

                                            let text_delta_ev = json!({
                                                "type": "response.output_text.delta",
                                                "item_id": msg_item_id.clone(),
                                                "output_index": msg_output_index,
                                                "content_index": 0,
                                                "delta": c
                                            });
                                            yield Ok(bytes::Bytes::from(format!("event: response.output_text.delta\ndata: {text_delta_ev}\n\n")));
                                        }
                                    }

                                    // 处理工具调用 tool_calls (Agent 工具流式事件)
                                    if let Some(tool_calls_arr) = delta.get("tool_calls").and_then(JsonValue::as_array) {
                                        for tc in tool_calls_arr {
                                            let index = tc.get("index").and_then(JsonValue::as_u64).unwrap_or(0) as usize;
                                            let call_id = tc.get("id").and_then(JsonValue::as_str);
                                            let name = tc.pointer("/function/name").and_then(JsonValue::as_str);
                                            let args_chunk = tc.pointer("/function/arguments").and_then(JsonValue::as_str).unwrap_or_default();

                                            if !active_tool_calls.contains_key(&index) {
                                                let item_call_id = format!("item_call_{:x}_{index}", nanos);
                                                let c_id = call_id.unwrap_or("call_default").to_string();
                                                let fn_name = name.unwrap_or_default().to_string();
                                                let cur_idx = next_output_index;
                                                next_output_index += 1;

                                                let added_ev = json!({
                                                    "type": "response.output_item.added",
                                                    "output_index": cur_idx,
                                                    "item": {
                                                        "id": item_call_id.clone(),
                                                        "type": "function_call",
                                                        "name": fn_name.clone(),
                                                        "call_id": c_id.clone(),
                                                        "arguments": ""
                                                    }
                                                });
                                                yield Ok(bytes::Bytes::from(format!("event: response.output_item.added\ndata: {added_ev}\n\n")));

                                                active_tool_calls.insert(index, ActiveCall {
                                                    item_id: item_call_id,
                                                    call_id: c_id,
                                                    name: fn_name,
                                                    arguments: String::new(),
                                                    output_index: cur_idx,
                                                });
                                            }

                                            if let Some(active_tc) = active_tool_calls.get_mut(&index) {
                                                if let Some(n) = name {
                                                    if active_tc.name.is_empty() {
                                                        active_tc.name = n.to_string();
                                                    }
                                                }
                                                if let Some(c) = call_id {
                                                    if active_tc.call_id.is_empty() || active_tc.call_id == "call_default" {
                                                        active_tc.call_id = c.to_string();
                                                    }
                                                }
                                                if !args_chunk.is_empty() {
                                                    active_tc.arguments.push_str(args_chunk);
                                                    let delta_ev = json!({
                                                        "type": "response.function_call_arguments.delta",
                                                        "item_id": active_tc.item_id.clone(),
                                                        "output_index": active_tc.output_index,
                                                        "call_id": active_tc.call_id.clone(),
                                                        "delta": args_chunk
                                                    });
                                                    yield Ok(bytes::Bytes::from(format!("event: response.function_call_arguments.delta\ndata: {delta_ev}\n\n")));
                                                }
                                            }
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
                        channel_id: channel_id.clone(),
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
                        error_message: Some(format!("Responses 流式传输中断: {e}")),
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

        if resp_created_sent {
            // 如果 content 为空但 reasoning 不为空，将 reasoning 兜底作为 content，避免客户端拿到空 output
            let fallback_content = if collected_content.is_empty() && !collected_reasoning.is_empty() {
                collected_reasoning.clone()
            } else {
                collected_content.clone()
            };

            // 如果在此之前尚未发送过 message item，且存在文本内容
            if !msg_item_added && !fallback_content.is_empty() {
                msg_item_added = true;
                msg_output_index = next_output_index;
                let item_added_ev = json!({
                    "type": "response.output_item.added",
                    "output_index": msg_output_index,
                    "item": {
                        "id": msg_item_id.clone(),
                        "type": "message",
                        "role": "assistant",
                        "content": []
                    }
                });
                yield Ok(bytes::Bytes::from(format!("event: response.output_item.added\ndata: {item_added_ev}\n\n")));

                let part_added_ev = json!({
                    "type": "response.content_part.added",
                    "item_id": msg_item_id.clone(),
                    "output_index": msg_output_index,
                    "content_index": 0,
                    "part": {
                        "type": "output_text",
                        "text": ""
                    }
                });
                yield Ok(bytes::Bytes::from(format!("event: response.content_part.added\ndata: {part_added_ev}\n\n")));

                let text_delta_ev = json!({
                    "type": "response.output_text.delta",
                    "item_id": msg_item_id.clone(),
                    "output_index": msg_output_index,
                    "content_index": 0,
                    "delta": fallback_content.clone()
                });
                yield Ok(bytes::Bytes::from(format!("event: response.output_text.delta\ndata: {text_delta_ev}\n\n")));
                content_part_added = true;
            }

            if content_part_added {
                let text_done_ev = json!({
                    "type": "response.output_text.done",
                    "item_id": msg_item_id.clone(),
                    "output_index": msg_output_index,
                    "content_index": 0,
                    "text": fallback_content.clone()
                });
                yield Ok(bytes::Bytes::from(format!("event: response.output_text.done\ndata: {text_done_ev}\n\n")));

                let part_done_ev = json!({
                    "type": "response.content_part.done",
                    "item_id": msg_item_id.clone(),
                    "output_index": msg_output_index,
                    "content_index": 0,
                    "part": {
                        "type": "output_text",
                        "text": fallback_content.clone()
                    }
                });
                yield Ok(bytes::Bytes::from(format!("event: response.content_part.done\ndata: {part_done_ev}\n\n")));
            }

            if msg_item_added {
                let item_done_ev = json!({
                    "type": "response.output_item.done",
                    "output_index": msg_output_index,
                    "item": {
                        "id": msg_item_id.clone(),
                        "type": "message",
                        "status": "completed",
                        "role": "assistant",
                        "content": [
                            {
                                "type": "output_text",
                                "text": fallback_content.clone()
                            }
                        ]
                    }
                });
                yield Ok(bytes::Bytes::from(format!("event: response.output_item.done\ndata: {item_done_ev}\n\n")));
            }

            let mut output_items = Vec::new();
            if msg_item_added {
                output_items.push(json!({
                    "id": msg_item_id.clone(),
                    "type": "message",
                    "status": "completed",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "output_text",
                            "text": fallback_content.clone()
                        }
                    ]
                }));
            }

            // 完成所有工具调用 output_item.done
            for (_idx, tc) in active_tool_calls.iter() {
                let call_done_ev = json!({
                    "type": "response.output_item.done",
                    "output_index": tc.output_index,
                    "item": {
                        "id": tc.item_id.clone(),
                        "type": "function_call",
                        "status": "completed",
                        "name": tc.name.clone(),
                        "call_id": tc.call_id.clone(),
                        "arguments": tc.arguments.clone()
                    }
                });
                yield Ok(bytes::Bytes::from(format!("event: response.output_item.done\ndata: {call_done_ev}\n\n")));

                output_items.push(json!({
                    "id": tc.item_id.clone(),
                    "type": "function_call",
                    "status": "completed",
                    "name": tc.name.clone(),
                    "call_id": tc.call_id.clone(),
                    "arguments": tc.arguments.clone()
                }));
            }

            let response_payload = json!({
                "id": resp_id.clone(),
                "object": "response",
                "status": "completed",
                "model": format!("opencode/{}", model_name),
                "output": output_items,
                "usage": {
                    "input_tokens": prompt_tokens.unwrap_or(0),
                    "output_tokens": out_tokens,
                    "total_tokens": total_tokens.unwrap_or_else(|| prompt_tokens.unwrap_or(0) + out_tokens)
                }
            });

            // 1. 标准 Responses API 完成事件 response.done
            let done_ev = json!({
                "type": "response.done",
                "response": response_payload.clone()
            });
            yield Ok(bytes::Bytes::from(format!("event: response.done\ndata: {done_ev}\n\n")));

            // 2. 兼容 response.completed 完成事件
            let completed_ev = json!({
                "type": "response.completed",
                "response": response_payload
            });
            yield Ok(bytes::Bytes::from(format!("event: response.completed\ndata: {completed_ev}\n\n")));
            yield Ok(bytes::Bytes::from("data: [DONE]\n\n"));
        }

        let mut response_preview = String::new();
        if !collected_reasoning.is_empty() {
            response_preview.push_str("<think>\n");
            response_preview.push_str(&collected_reasoning);
            response_preview.push_str("\n</think>\n\n");
        }
        response_preview.push_str(&collected_content);
        if !active_tool_calls.is_empty() {
            if !response_preview.is_empty() { response_preview.push_str("\n\n"); }
            for (_idx, tc) in &active_tool_calls {
                response_preview.push_str(&format!("[工具调用] {}({})\n", tc.name, tc.arguments));
            }
        }

        let final_reason_tokens = reasoning_tokens.or_else(|| {
            if collected_reasoning.is_empty() { None } else { Some((collected_reasoning.len() / 4).max(1) as u64) }
        });
        let total_dur = start_time.elapsed().as_millis() as u64;

        ctx.record_log(ProxyRequestLog {
            id: req_id,
            timestamp: current_timestamp(),
            method: "POST".to_string(),
            path,
            channel_id,
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

// ---------------------------------------------------------------------------
// 统一模型协议适配器体系 (Protocol Adapters)
// 支持 OpenAI Chat Completions, Anthropic Messages, OpenAI Responses API
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

                            // 格式 B: 扁平格式 (Anthropic 格式，name / input_schema 等直接位于顶层)
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

                            // 只有提取到非空名称时才保留（防止上游抛 Expected 'function.name' to be a string）
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
                // 规范化 role：解决上游报 unknown variant `developer` 错误（developer -> system, model -> assistant, function -> tool）
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

                // 规范化 tool_calls，严格过滤掉空 name 的残缺调用
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

                // 规范化 role: "tool"，保证包含非空 tool_call_id
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

#[inline]
fn normalize_chat_messages(body: &mut JsonValue) {
    OpenAiProtocolAdapter::sanitize_and_normalize(body);
}

// ---------------------------------------------------------------------------
// 代理池按延迟升序与直连候选列表构建 (直连在首位，节点按速度排序)
// ---------------------------------------------------------------------------

async fn get_sorted_egress_candidates(
    ctx: &OpencodeProxyContext,
    channel: &ChannelConfig,
) -> Vec<String> {
    if !channel.use_proxy_pool && !channel.use_fixed_proxy {
        return vec!["__direct__".to_string()];
    }

    // 代理池固定通道：只走代理池节点，不包含直连
    let mut candidates = Vec::new();
    if !channel.use_fixed_proxy {
        candidates.push("__direct__".to_string());
    }

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
    let models_count = {
        let models = ctx.cached_channel_models.read().await;
        models
            .iter()
            .map(|entry| {
                let channel = config.channels.iter().find(|c| c.id == entry.channel_id);
                let allowed = channel.and_then(|c| c.enabled_models.as_ref());
                match allowed {
                    None => entry.models.len(),
                    Some(allowed) => entry.models.iter().filter(|m| allowed.contains(m)).count(),
                }
            })
            .sum::<usize>()
    };
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
        Some(c) if c.use_fixed_proxy => "代理池固定通道（不直连）",
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

/// 用渠道的 Key 列表逐个请求上游 /models，合并所有 Key 返回的模型 id（不同 Key 权限不同，合并取并集）。
async fn fetch_channel_models_raw(
    ctx: &OpencodeProxyContext,
    chan: &ChannelConfig,
    extra_headers: &[(&str, &str)],
) -> Result<Vec<String>, String> {
    let candidates = get_sorted_egress_candidates(ctx, chan).await;
    let candidate = candidates.first().map(|s| s.as_str()).unwrap_or("__direct__");
    let client = build_client_for_candidate(ctx, candidate).await;
    let models_url = format!("{}/models", chan.upstream_api_base());
    let auth_vals = chan.auth_values();

    let mut merged: Vec<String> = Vec::new();
    let mut last_err: Option<String> = None;
    for auth_val in &auth_vals {
        let mut req = client
            .get(&models_url)
            .header("Authorization", auth_val)
            .header("Accept", "application/json")
            .timeout(Duration::from_secs(10));
        for (k, v) in extra_headers {
            req = req.header(*k, *v);
        }

        match req.send().await {
            Ok(r) if r.status().is_success() => {
                let val = r
                    .json::<JsonValue>()
                    .await
                    .map_err(|e| format!("解析渠道「{}」模型列表失败: {e}", chan.name))?;
                if let Some(list) = val.get("data").and_then(JsonValue::as_array) {
                    for item in list {
                        if let Some(id) = item.get("id").and_then(JsonValue::as_str) {
                            let id = id.to_string();
                            if !merged.contains(&id) {
                                merged.push(id);
                            }
                        }
                    }
                }
            }
            Ok(r) if r.status() == StatusCode::UNAUTHORIZED || r.status() == StatusCode::FORBIDDEN => {
                // Key 无效：记录后继续尝试下一个 Key
                last_err = Some(format!("渠道「{}」模型接口返回 HTTP {}（Key 无效）", chan.name, r.status()));
            }
            Ok(r) => {
                last_err = Some(format!("渠道「{}」模型接口返回 HTTP {}", chan.name, r.status()));
            }
            Err(e) => {
                last_err = Some(format!("无法连接渠道「{}」模型接口: {e}", chan.name));
            }
        }
    }

    if merged.is_empty() {
        Err(last_err.unwrap_or_else(|| format!("渠道「{}」模型接口请求失败", chan.name)))
    } else {
        Ok(merged)
    }
}

/// 从 OpenCode 上游抓取模型（Public 免费通道：仅保留 free / big-pickle）
async fn fetch_opencode_channel_models(
    ctx: &OpencodeProxyContext,
    chan: &ChannelConfig,
) -> Result<Vec<String>, String> {
    let ids = fetch_channel_models_raw(
        ctx,
        chan,
        &[("User-Agent", "opencode/1.0.0"), ("x-opencode-client", "cli")],
    )
    .await?;
    let model_ids: Vec<String> = ids
        .into_iter()
        .filter(|id| id.contains("free") || id == "big-pickle")
        .collect();
    if model_ids.is_empty() {
        return Err("OpenCode 上游未返回可用的免费模型".to_string());
    }
    Ok(model_ids)
}

/// 从本地数据库读取站点模型缓存（默认优先从库中读取）
async fn read_site_model_cache_for_site(
    ctx: &OpencodeProxyContext,
    site_id: &str,
) -> Option<Vec<String>> {
    let app_opt = ctx.app_handle.read().await.clone();
    let app = app_opt?;
    let database = app.state::<crate::models::Database>();
    let all_models = {
        let conn = database.0.lock().ok()?;
        let mut stmt = conn
            .prepare("SELECT models_json FROM site_model_cache WHERE site_id = ?1")
            .ok()?;
        let mut models = Vec::new();
        let rows = stmt
            .query_map([site_id], |row| {
                let json_str: String = row.get(0)?;
                Ok(json_str)
            })
            .ok()?;
        for r in rows.flatten() {
            if let Ok(items) = serde_json::from_str::<Vec<crate::models::SiteModelItem>>(&r) {
                for it in items {
                    if !it.id.is_empty() && !models.contains(&it.id) {
                        models.push(it.id);
                    }
                }
            }
        }
        models
    };
    if all_models.is_empty() {
        None
    } else {
        Some(all_models)
    }
}

/// 将拉取到的最新上游模型和 Key 落库到本地数据库
async fn save_site_channel_models_to_db(
    ctx: &OpencodeProxyContext,
    site_id: &str,
    channel_name: &str,
    models: &[String],
    keys: &[String],
) {
    let app_opt = ctx.app_handle.read().await.clone();
    if let Some(app) = app_opt {
        let database = app.state::<crate::models::Database>();
        let model_items: Vec<crate::models::SiteModelItem> = models
            .iter()
            .map(|m| crate::models::SiteModelItem {
                id: m.clone(),
                owned_by: None,
            })
            .collect();
        let models_json = serde_json::to_string(&model_items).unwrap_or_else(|_| "[]".to_string());
        let keys_json = serde_json::to_string(keys).unwrap_or_else(|_| "[]".to_string());

        if let Ok(conn) = database.0.lock() {
            let _ = conn.execute(
                "INSERT INTO site_model_cache (site_id, profile_id, profile_name, account_name, username, api_source, keys_json, groups_json, models_json, key_models_json, error, updated_at)
                 VALUES (?1, '', ?2, '', '', 'proxy_fetch', ?3, '{}', ?4, '{}', '', CURRENT_TIMESTAMP)
                 ON CONFLICT(site_id, profile_id) DO UPDATE SET
                    models_json = excluded.models_json,
                    keys_json = CASE WHEN excluded.keys_json != '[]' THEN excluded.keys_json ELSE site_model_cache.keys_json END,
                    error = '',
                    updated_at = CURRENT_TIMESTAMP",
                rusqlite::params![site_id, channel_name, keys_json, models_json],
            );
        };
    }
}

/// 从站点转换渠道抓取模型（OpenAI 兼容 /v1/models，保留全部模型 id）
/// force_refresh = false 时默认优先读取本地库中的模型缓存；
/// force_refresh = true 时（点击刷新上游模型）强制向远端请求并落库结果。
async fn fetch_site_channel_models(
    ctx: &OpencodeProxyContext,
    chan: &ChannelConfig,
    force_refresh: bool,
) -> Result<Vec<String>, String> {
    if !force_refresh {
        if let Some(site_id) = &chan.site_id {
            if let Some(cached_models) = read_site_model_cache_for_site(ctx, site_id).await {
                if !cached_models.is_empty() {
                    return Ok(cached_models);
                }
            }
        }
    }

    let ids = fetch_channel_models_raw(ctx, chan, &[]).await?;
    if ids.is_empty() {
        return Err(format!("渠道「{}」未返回模型列表", chan.name));
    }

    if let Some(site_id) = &chan.site_id {
        save_site_channel_models_to_db(ctx, site_id, &chan.name, &ids, &chan.api_keys).await;
    }

    Ok(ids)
}

/// 拉取全部启用渠道的模型列表（单个渠道失败不影响其他渠道，失败原因随返回值透传）
async fn fetch_upstream_models_inner(
    ctx: &OpencodeProxyContext,
    config: &OpencodeProxyConfig,
    force_refresh: bool,
) -> (Vec<ChannelModelList>, Vec<ChannelModelFetchError>) {
    let mut result = Vec::new();
    let mut errors = Vec::new();
    for chan in config.channels.iter().filter(|c| c.enabled) {
        let fetched = if chan.id == "opencode" {
            fetch_opencode_channel_models(ctx, chan).await
        } else {
            fetch_site_channel_models(ctx, chan, force_refresh).await
        };
        match fetched {
            Ok(models) if !models.is_empty() => {
                result.push(ChannelModelList {
                    channel_id: chan.id.clone(),
                    alias: chan.effective_alias(),
                    models,
                });
            }
            Ok(_) => errors.push(ChannelModelFetchError {
                channel_id: chan.id.clone(),
                channel_name: chan.name.clone(),
                error: format!("渠道「{}」上游未返回任何模型", chan.name),
            }),
            Err(e) => errors.push(ChannelModelFetchError {
                channel_id: chan.id.clone(),
                channel_name: chan.name.clone(),
                error: e,
            }),
        }
    }
    (result, errors)
}

/// GET /v1/models (ID 统一为 {alias}/原id)
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
        let (models, _errors) = fetch_upstream_models_inner(&ctx, &config, false).await;
        let mut cached = ctx.cached_channel_models.write().await;
        *cached = models;
        let mut updated = ctx.cached_models_updated_at.write().await;
        *updated = Some(Instant::now());
    }

    let models = ctx.cached_channel_models.read().await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // 逐渠道输出模型：ID 带别名前缀，并应用该渠道的 enabled_models 白名单
    let data: Vec<JsonValue> = models
        .iter()
        .flat_map(|entry| {
            let channel = config.channels.iter().find(|c| c.id == entry.channel_id);
            let allowed = channel.and_then(|c| c.enabled_models.as_ref());
            entry.models.iter().filter_map(move |raw_id| {
                if let Some(allowed) = allowed {
                    if !allowed.iter().any(|m| m == raw_id) {
                        return None;
                    }
                }
                Some(json!({
                    "id": format!("{}/{}", entry.alias, raw_id),
                    "object": "model",
                    "created": now,
                    "owned_by": entry.alias.clone(),
                    "permission": [],
                    "root": format!("{}/{}", entry.alias, raw_id),
                    "parent": null
                }))
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

/// 根据请求模型名解析目标渠道与发送给上游的裸模型名。
/// 规则：
/// 1. `alias/裸模型` 优先按别名前缀精确匹配启用渠道；
/// 2. 无前缀时，若某个启用渠道的白名单（enabled_models）中包含该模型，则优先分发给该渠道；
/// 3. 若无匹配，回退至启用的默认 opencode 渠道；
/// 4. 若默认 opencode 未启用，回退至首个已启用的自定义渠道。
fn resolve_channel<'a>(
    config: &'a OpencodeProxyConfig,
    raw_model: &str,
) -> Option<(&'a ChannelConfig, String)> {
    // 1. 带前缀别名匹配 (如 x666/claude-sonnet-5)
    if let Some((prefix, rest)) = raw_model.split_once('/') {
        if let Some(ch) = config
            .channels
            .iter()
            .find(|c| c.enabled && c.effective_alias().eq_ignore_ascii_case(prefix))
        {
            return Some((ch, rest.to_string()));
        }
    }

    let stripped = strip_opencode_prefix(raw_model);

    // 2. 检查是否有启用渠道显式在 enabled_models 中勾选/包含了该模型
    if let Some(ch) = config.channels.iter().find(|c| {
        c.enabled && c.enabled_models.as_ref().map_or(false, |models| {
            models.iter().any(|m| m.eq_ignore_ascii_case(stripped) || m.eq_ignore_ascii_case(raw_model))
        })
    }) {
        return Some((ch, stripped.to_string()));
    }

    // 3. 回退默认 opencode 渠道（如果已启用）
    if let Some(ch) = config.channels.iter().find(|c| c.id == "opencode" && c.enabled) {
        return Some((ch, stripped.to_string()));
    }

    // 4. 若 opencode 渠道未启用，回退到首个已启用的自定义渠道
    if let Some(ch) = config.channels.iter().find(|c| c.enabled) {
        return Some((ch, stripped.to_string()));
    }

    None
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

    let is_stream = body
        .get("stream")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);

    let (chan, model_to_send) = match resolve_channel(&config, &raw_model) {
        Some((c, m)) => (c, m),
        None => {
            ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
            ctx.record_log(ProxyRequestLog {
                id: req_id,
                timestamp: current_timestamp(),
                method: "POST".to_string(),
                path: "/v1/chat/completions".to_string(),
                channel_id: "opencode".to_string(),
                model: strip_opencode_prefix(&raw_model).to_string(),
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
                error_message: Some("渠道不可用：未找到匹配的启用渠道（含默认 OpenCode）".to_string()),
                request_body: req_body_str,
                response_body: None,
                node_name: Some("直连通道".to_string()),
            }).await;

            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": {
                        "message": "未找到可用的上游渠道，请检查渠道是否启用",
                        "type": "channel_disabled"
                    }
                })),
            )
                .into_response();
        }
    };

    let chan_alias = chan.effective_alias();

    if let Some(model_val) = body.get_mut("model") {
        *model_val = JsonValue::String(model_to_send.clone());
    }

    normalize_chat_messages(&mut body);

    let target_url = format!("{}/chat/completions", chan.upstream_api_base());
    let auth_vals = chan.auth_values();

    let candidates = get_sorted_egress_candidates(&ctx, chan).await;
    let total_candidates = candidates.len().max(1);
    let max_retries = ctx.config.read().await.max_retries as usize;
    let max_attempts = if chan.use_proxy_pool || chan.use_fixed_proxy {
        (max_retries + 1).min(total_candidates * auth_vals.len()).max(auth_vals.len())
    } else {
        (max_retries + 1).max(auth_vals.len())
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
            .header("Authorization", auth_vals[attempt % auth_vals.len()].as_str())
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
                        &chan_alias,
                        &model_to_send,
                        is_stream,
                        status.as_u16(),
                        format!(
                            "{}自动切换：{}",
                            if status.as_u16() == 401 || status.as_u16() == 403 || status.as_u16() == 429 {
                                "Key/限流"
                            } else {
                                "节点失败"
                            },
                            format_upstream_error_message(status.as_u16(), &err_body)
                        ),
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
                        &chan_alias,
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
                channel_id: chan_alias.clone(),
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
            channel_id: chan_alias.clone(),
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
            chan_alias,
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

        let (prompt_tokens, prompt_cache_hit_tokens, prompt_cache_miss_tokens, completion_tokens, reasoning_tokens, total_tokens, resp_str, final_body_bytes) =
            if let Ok(mut json_val) = serde_json::from_slice::<JsonValue>(&data) {
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

                // 规范化 non-streaming choices[i].message 中的 tool_calls
                if let Some(choices) = json_val.get_mut("choices").and_then(JsonValue::as_array_mut) {
                    for choice in choices {
                        if let Some(msg) = choice.get_mut("message").and_then(JsonValue::as_object_mut) {
                            if let Some(tc_val) = msg.get_mut("tool_calls") {
                                if let Some(tc_arr) = tc_val.as_array_mut() {
                                    for tc in tc_arr {
                                        if let Some(tc_obj) = tc.as_object_mut() {
                                            if !tc_obj.contains_key("type") {
                                                tc_obj.insert("type".to_string(), json!("function"));
                                            }
                                            if !tc_obj.contains_key("id") {
                                                tc_obj.insert("id".to_string(), json!("call_default"));
                                            }
                                            if let Some(func) = tc_obj.get_mut("function").and_then(JsonValue::as_object_mut) {
                                                if func.get("name").map_or(true, |v| v.is_null()) {
                                                    func.insert("name".to_string(), json!("tool"));
                                                }
                                                if func.get("arguments").map_or(true, |v| v.is_null()) {
                                                    func.insert("arguments".to_string(), json!("{}"));
                                                }
                                            } else {
                                                tc_obj.insert("function".to_string(), json!({
                                                    "name": "tool",
                                                    "arguments": "{}"
                                                }));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                let formatted = serde_json::to_string_pretty(&json_val).unwrap_or_else(|_| json_val.to_string());
                let serialized_bytes = serde_json::to_vec(&json_val).unwrap_or_else(|_| data.to_vec());
                (pt, hit, miss, ct, rt, tt, Some(formatted), serialized_bytes)
            } else {
                let d_vec = data.to_vec();
                (None, None, None, None, None, None, String::from_utf8(d_vec.clone()).ok(), d_vec)
            };

        ctx.record_log(ProxyRequestLog {
            id: req_id,
            timestamp: current_timestamp(),
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            channel_id: chan_alias,
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
            final_body_bytes,
        )
            .into_response()
    }
}

/// POST /v1/responses (OpenAI Responses API 兼容与转发)
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
        .unwrap_or("unknown")
        .to_string();

    let is_stream = body
        .get("stream")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);

    // 智能适配：如果 body 包含 input/instructions（Responses 格式），将其转换为标准 messages
    ResponsesProtocolAdapter::convert_input_to_messages(&mut body);

    let (chan, model_to_send) = match resolve_channel(&config, &raw_model) {
        Some((c, m)) => (c, m),
        None => {
            ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": { "message": "未找到可用的上游渠道，请检查渠道是否启用", "type": "channel_disabled" } })),
            )
                .into_response();
        }
    };

    let chan_alias = chan.effective_alias();

    if let Some(model_val) = body.get_mut("model") {
        *model_val = JsonValue::String(model_to_send.clone());
    }

    normalize_chat_messages(&mut body);

    // 默认转发至上游标准的 /chat/completions 端点
    let target_url = format!("{}/chat/completions", chan.upstream_api_base());
    let auth_vals = chan.auth_values();

    let candidates = get_sorted_egress_candidates(&ctx, chan).await;
    let total_candidates = candidates.len().max(1);
    let max_retries = ctx.config.read().await.max_retries as usize;
    let max_attempts = if chan.use_proxy_pool || chan.use_fixed_proxy {
        (max_retries + 1).min(total_candidates * auth_vals.len()).max(auth_vals.len())
    } else {
        (max_retries + 1).max(auth_vals.len())
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
            .header("Authorization", auth_vals[attempt % auth_vals.len()].as_str())
            .header("Content-Type", "application/json")
            .header("User-Agent", "opencode/1.0.0")
            .header("x-opencode-client", "cli")
            .header("x-opencode-session", session_id)
            .header("Accept", if is_stream { "text/event-stream" } else { "application/json" });

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
                        &chan_alias,
                        &model_to_send,
                        is_stream,
                        status.as_u16(),
                        format!(
                            "{}自动切换：{}",
                            if status.as_u16() == 401 || status.as_u16() == 403 || status.as_u16() == 429 {
                                "Key/限流"
                            } else {
                                "节点失败"
                            },
                            format_upstream_error_message(status.as_u16(), &err_body)
                        ),
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
                        &chan_alias,
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
            let err = last_send_err.map(|e| e.to_string()).unwrap_or_else(|| "Unknown error".to_string());
            let dur = start_time.elapsed().as_millis() as u64;
            ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
            ctx.record_log(ProxyRequestLog {
                id: req_id,
                timestamp: current_timestamp(),
                method: "POST".to_string(),
                path: "/v1/responses".to_string(),
                channel_id: chan_alias.clone(),
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
            channel_id: chan_alias.clone(),
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
        let stream = openai_to_responses_sse_stream(
            ctx.clone(),
            req_id,
            start_time,
            "/v1/responses".to_string(),
            chan_alias,
            model_to_send,
            req_body_str,
            node_name,
            res.bytes_stream(),
        );
        (
            StatusCode::OK,
            [
                (axum::http::header::CONTENT_TYPE, "text/event-stream; charset=utf-8"),
                (axum::http::header::CACHE_CONTROL, "no-cache, no-transform"),
                (axum::http::header::CONNECTION, "keep-alive"),
                (axum::http::header::HeaderName::from_static("x-accel-buffering"), "no"),
            ],
            Body::from_stream(stream),
        )
            .into_response()
    } else {
        let ttft = start_time.elapsed().as_millis() as u64;
        let data = res.bytes().await.unwrap_or_default();
        let dur = start_time.elapsed().as_millis() as u64;

        let (prompt_tokens, prompt_cache_hit_tokens, prompt_cache_miss_tokens, completion_tokens, reasoning_tokens, total_tokens, resp_str, final_response_json) =
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

                let content_text = json_val.pointer("/choices/0/message/content").and_then(JsonValue::as_str).unwrap_or_default();
                let reasoning_text = json_val.pointer("/choices/0/message/reasoning_content").and_then(JsonValue::as_str).unwrap_or_default();

                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos();

                let mut output_items = Vec::new();
                if !reasoning_text.is_empty() {
                    output_items.push(json!({
                        "id": format!("item_reason_{:x}", nanos),
                        "type": "reasoning",
                        "summary": [
                            {
                                "type": "summary_text",
                                "text": reasoning_text
                            }
                        ]
                    }));
                }
                if !content_text.is_empty() {
                    output_items.push(json!({
                        "id": format!("item_msg_{:x}", nanos),
                        "type": "message",
                        "role": "assistant",
                        "content": [
                            {
                                "type": "output_text",
                                "text": content_text
                            }
                        ]
                    }));
                }

                // 支持非流式 tool_calls 转换为 function_call items
                if let Some(tool_calls_arr) = json_val.pointer("/choices/0/message/tool_calls").and_then(JsonValue::as_array) {
                    for (i, tc) in tool_calls_arr.iter().enumerate() {
                        let call_id = tc.get("id").and_then(JsonValue::as_str).unwrap_or("call_default");
                        let name = tc.pointer("/function/name").and_then(JsonValue::as_str).unwrap_or_default();
                        let args = tc.pointer("/function/arguments").and_then(JsonValue::as_str).unwrap_or("{}");
                        output_items.push(json!({
                            "id": format!("item_call_{:x}_{i}", nanos),
                            "type": "function_call",
                            "name": name,
                            "call_id": call_id,
                            "arguments": args
                        }));
                    }
                }

                let resp_obj = json!({
                    "id": format!("resp_{:x}", nanos),
                    "object": "response",
                    "status": "completed",
                    "model": format!("opencode/{}", model_to_send),
                    "output": output_items,
                    "usage": {
                        "input_tokens": pt.unwrap_or(0),
                        "output_tokens": ct.unwrap_or(0),
                        "total_tokens": tt.unwrap_or_else(|| pt.unwrap_or(0) + ct.unwrap_or(0))
                    }
                });

                let formatted = serde_json::to_string_pretty(&resp_obj).unwrap_or_else(|_| resp_obj.to_string());
                (pt, hit, miss, ct, rt, tt, Some(formatted), Some(resp_obj))
            } else {
                (None, None, None, None, None, None, String::from_utf8(data.to_vec()).ok(), None)
            };

        ctx.record_log(ProxyRequestLog {
            id: req_id,
            timestamp: current_timestamp(),
            method: "POST".to_string(),
            path: "/v1/responses".to_string(),
            channel_id: chan_alias,
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

        if let Some(jb) = final_response_json {
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                Json(jb),
            )
                .into_response()
        } else {
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                data,
            )
                .into_response()
        }
    }
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
    let (chan, model_to_send) = match resolve_channel(&config, raw_model) {
        Some((c, m)) => (c, m),
        None => {
            ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": { "type": "api_error", "message": "未找到可用的上游渠道，请检查渠道是否启用" } })),
            )
                .into_response();
        }
    };
    let stripped_model = model_to_send.clone();
    let chan_alias = chan.effective_alias();

    let mut openai_tools = Vec::new();
    if let Some(tools_arr) = body.get("tools").and_then(JsonValue::as_array) {
        for t in tools_arr {
            let mut name = String::new();
            let mut desc = String::new();
            let mut schema = json!({"type": "object", "properties": {}});

            // 1. Anthropic 格式: { name: "...", description: "...", input_schema: {...} }
            if let Some(n) = t.get("name").and_then(JsonValue::as_str) {
                name = n.trim().to_string();
            }
            if let Some(d) = t.get("description").and_then(JsonValue::as_str) {
                desc = d.to_string();
            }
            if let Some(s) = t.get("input_schema").or_else(|| t.get("parameters")) {
                schema = s.clone();
            }

            // 2. 兼容嵌套在 function 中的格式: { type: "function", function: { name: "...", ... } }
            if name.is_empty() {
                if let Some(f) = t.get("function").and_then(JsonValue::as_object) {
                    if let Some(n) = f.get("name").and_then(JsonValue::as_str) {
                        name = n.trim().to_string();
                    }
                    if let Some(d) = f.get("description").and_then(JsonValue::as_str) {
                        desc = d.to_string();
                    }
                    if let Some(s) = f.get("parameters").or_else(|| f.get("input_schema")) {
                        schema = s.clone();
                    }
                }
            }

            if !name.is_empty() {
                if !schema.is_object() {
                    schema = json!({"type": "object", "properties": {}});
                }
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

    let mut tool_choice_val = None;
    if !openai_tools.is_empty() {
        if let Some(tc) = body.get("tool_choice") {
            if let Some(tc_str) = tc.as_str() {
                match tc_str {
                    "auto" => tool_choice_val = Some(json!("auto")),
                    "any" => tool_choice_val = Some(json!("required")),
                    "none" => tool_choice_val = Some(json!("none")),
                    _ => {}
                }
            } else if let Some(tc_obj) = tc.as_object() {
                let tc_type = tc_obj.get("type").and_then(JsonValue::as_str).unwrap_or_default();
                match tc_type {
                    "auto" => tool_choice_val = Some(json!("auto")),
                    "any" => tool_choice_val = Some(json!("required")),
                    "none" => tool_choice_val = Some(json!("none")),
                    "tool" => {
                        let name = tc_obj.get("name").and_then(JsonValue::as_str)
                            .or_else(|| tc.pointer("/function/name").and_then(JsonValue::as_str))
                            .unwrap_or_default().trim();
                        if !name.is_empty() {
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
                                let name = block.get("name").and_then(JsonValue::as_str)
                                    .or_else(|| block.pointer("/function/name").and_then(JsonValue::as_str))
                                    .unwrap_or_default().trim();
                                let input = block.get("input").or_else(|| block.get("arguments")).cloned().unwrap_or_else(|| json!({}));
                                let args_str = if let Some(s) = input.as_str() {
                                    s.to_string()
                                } else {
                                    serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string())
                                };
                                if !name.is_empty() {
                                    tool_calls.push(json!({
                                        "id": id,
                                        "type": "function",
                                        "function": {
                                            "name": name,
                                            "arguments": args_str
                                        }
                                    }));
                                }
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

    normalize_chat_messages(&mut openai_body);

    let target_url = format!("{}/chat/completions", chan.upstream_api_base());
    let auth_vals = chan.auth_values();

    let candidates = get_sorted_egress_candidates(&ctx, chan).await;
    let total_candidates = candidates.len().max(1);
    let max_retries = ctx.config.read().await.max_retries as usize;
    let max_attempts = if chan.use_proxy_pool || chan.use_fixed_proxy {
        (max_retries + 1).min(total_candidates * auth_vals.len()).max(auth_vals.len())
    } else {
        (max_retries + 1).max(auth_vals.len())
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
            .header("Authorization", auth_vals[attempt % auth_vals.len()].as_str())
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
                        &chan_alias,
                        &stripped_model,
                        is_stream,
                        status.as_u16(),
                        format!(
                            "{}自动切换：{}",
                            if status.as_u16() == 401 || status.as_u16() == 403 || status.as_u16() == 429 {
                                "Key/限流"
                            } else {
                                "节点失败"
                            },
                            format_upstream_error_message(status.as_u16(), &err_body)
                        ),
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
                        &chan_alias,
                        &stripped_model,
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
                channel_id: chan_alias.clone(),
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
            channel_id: chan_alias.clone(),
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
            chan_alias,
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
                    "model": format!("{chan_alias}/{stripped_model}"),
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
                    channel_id: chan_alias.clone(),
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
                    channel_id: chan_alias,
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
    let mut config = match row {
        Ok(json_str) => serde_json::from_str(&json_str).unwrap_or_default(),
        Err(_) => OpencodeProxyConfig::default(),
    };
    normalize_channel_config(&mut config);
    config
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
        let (models, _errors) = fetch_upstream_models_inner(&ctx_clone, &cfg, false).await;
        let mut cached = ctx_clone.cached_channel_models.write().await;
        *cached = models;
        let mut updated = ctx_clone.cached_models_updated_at.write().await;
        *updated = Some(Instant::now());
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
    let models_count = {
        let models = state.context.cached_channel_models.read().await;
        let config = state.context.config.read().await;
        models
            .iter()
            .map(|entry| {
                let channel = config.channels.iter().find(|c| c.id == entry.channel_id);
                let allowed = channel.and_then(|c| c.enabled_models.as_ref());
                match allowed {
                    None => entry.models.len(),
                    Some(allowed) => entry.models.iter().filter(|m| allowed.contains(m)).count(),
                }
            })
            .sum::<usize>()
    };
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
    mut config: OpencodeProxyConfig,
) -> Result<OpencodeProxyStatus, String> {
    normalize_channel_config(&mut config);
    validate_channel_config(&config)?;
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
) -> Result<Vec<ChannelModelList>, String> {
    let cfg = state.context.config.read().await.clone();
    let (models, _errors) = fetch_upstream_models_inner(&state.context, &cfg, true).await;
    let mut cached = state.context.cached_channel_models.write().await;
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

/// 单个渠道的累计使用统计（供渠道卡片底部展示）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelUsageStats {
    pub channel_id: String,
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_reasoning_tokens: u64,
    pub total_reasoning_requests: u64,
    pub total_cache_hit_tokens: u64,
    pub total_tokens: u64,
    pub today_total_tokens: u64,
}

/// 按渠道聚合使用统计：内存实时计数（自启动以来）与日志库留存记录（最近 1000 条）取最大值，
/// 语义与全局「累计」统计一致。
#[tauri::command]
pub async fn get_opencode_channel_stats(
    _database: tauri::State<'_, crate::models::Database>,
    state: tauri::State<'_, OpencodeProxyState>,
) -> Result<Vec<ChannelUsageStats>, String> {
    let mut map: HashMap<String, ChannelUsageStats> = HashMap::new();
    {
        let channels = state.context.metrics.channel.lock().map_err(|e| e.to_string())?;
        for (id, cm) in channels.iter() {
            map.insert(
                id.clone(),
                ChannelUsageStats {
                    channel_id: id.clone(),
                    total_requests: cm.total_requests.load(Ordering::Relaxed),
                    successful_requests: cm.successful_requests.load(Ordering::Relaxed),
                    failed_requests: cm.failed_requests.load(Ordering::Relaxed),
                    total_prompt_tokens: cm.total_prompt_tokens.load(Ordering::Relaxed),
                    total_completion_tokens: cm.total_completion_tokens.load(Ordering::Relaxed),
                    total_reasoning_tokens: cm.total_reasoning_tokens.load(Ordering::Relaxed),
                    total_reasoning_requests: cm.total_reasoning_requests.load(Ordering::Relaxed),
                    total_cache_hit_tokens: cm.total_cache_hit_tokens.load(Ordering::Relaxed),
                    total_tokens: cm.total_tokens.load(Ordering::Relaxed),
                    today_total_tokens: 0,
                },
            );
        }
    }

    let app_opt = state.context.app_handle.read().await.clone();
    if let Some(app) = app_opt {
        let database = app.state::<crate::models::Database>();
        let db_rows = match database.0.lock() {
            Ok(conn) => query_channel_stats_from_db(&conn),
            Err(_) => Ok(Vec::new()),
        };
        if let Ok(db_rows) = db_rows {
            for (channel_id, total, success, failed, prompt, completion, reasoning, reasoning_req, cache_hit, total_tokens, today_tokens) in db_rows
            {
                let entry = map
                    .entry(channel_id.clone())
                    .or_insert_with(|| ChannelUsageStats {
                        channel_id: channel_id.clone(),
                        ..Default::default()
                    });
                entry.total_requests = entry.total_requests.max(total);
                entry.successful_requests = entry.successful_requests.max(success);
                entry.failed_requests = entry.failed_requests.max(failed);
                entry.total_prompt_tokens = entry.total_prompt_tokens.max(prompt);
                entry.total_completion_tokens = entry.total_completion_tokens.max(completion);
                entry.total_reasoning_tokens = entry.total_reasoning_tokens.max(reasoning);
                entry.total_reasoning_requests = entry.total_reasoning_requests.max(reasoning_req);
                entry.total_cache_hit_tokens = entry.total_cache_hit_tokens.max(cache_hit);
                entry.total_tokens = entry.total_tokens.max(total_tokens);
                entry.today_total_tokens = entry.today_total_tokens.max(today_tokens);
            }
        }
    }

    let mut stats: Vec<ChannelUsageStats> = map.into_values().collect();
    stats.sort_by(|a, b| a.channel_id.cmp(&b.channel_id));
    Ok(stats)
}

/// 从日志库聚合各渠道统计（留存记录范围内）。
fn query_channel_stats_from_db(
    conn: &Connection,
) -> Result<Vec<(String, u64, u64, u64, u64, u64, u64, u64, u64, u64, u64)>, String> {
    let today_prefix = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut stmt = conn
        .prepare(
            "SELECT channel_id,
                COUNT(*),
                COALESCE(SUM(CASE WHEN status_code >= 200 AND status_code < 300 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN status_code >= 400 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(prompt_tokens), 0),
                COALESCE(SUM(completion_tokens), 0),
                COALESCE(SUM(reasoning_tokens), 0),
                COALESCE(SUM(CASE WHEN COALESCE(reasoning_tokens, 0) > 0 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(prompt_cache_hit_tokens), 0),
                COALESCE(SUM(COALESCE(total_tokens, COALESCE(prompt_tokens, 0) + COALESCE(completion_tokens, 0))), 0),
                COALESCE(SUM(CASE WHEN timestamp LIKE ?1 THEN COALESCE(total_tokens, COALESCE(prompt_tokens, 0) + COALESCE(completion_tokens, 0)) ELSE 0 END), 0)
             FROM opencode_proxy_logs GROUP BY channel_id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([format!("{today_prefix}%")], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)? as u64,
                r.get::<_, i64>(2)? as u64,
                r.get::<_, i64>(3)? as u64,
                r.get::<_, i64>(4)? as u64,
                r.get::<_, i64>(5)? as u64,
                r.get::<_, i64>(6)? as u64,
                r.get::<_, i64>(7)? as u64,
                r.get::<_, i64>(8)? as u64,
                r.get::<_, i64>(9)? as u64,
                r.get::<_, i64>(10)? as u64,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sse_frame_extractor_standard_and_crlf() {
        let mut extractor = SseFrameExtractor::new();

        // 1. 测试标准 LF: \n\n
        let chunk1 = b"data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n";
        extractor.push_bytes(chunk1);
        let blocks = extractor.extract_blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].data_lines.len(), 1);
        assert_eq!(blocks[0].data_lines[0], "{\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}");

        // 2. 测试 Windows CRLF: \r\n\r\n
        let chunk2 = b"data: {\"id\":\"2\",\"choices\":[{\"delta\":{\"content\":\"World\"}}]}\r\n\r\n";
        extractor.push_bytes(chunk2);
        let blocks = extractor.extract_blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].data_lines[0], "{\"id\":\"2\",\"choices\":[{\"delta\":{\"content\":\"World\"}}]}");
    }

    #[test]
    fn test_sse_frame_extractor_utf8_multibyte_boundary_split() {
        let mut extractor = SseFrameExtractor::new();

        // 中文字符 "你好" (E4 BD A0, E5 A5 BD)
        let full_text = "data: {\"content\":\"你好世界\"}\n\n";
        let bytes = full_text.as_bytes();

        // 将字节在多字节中文字符中间切片（如在 '好' 的第 1 字节后切割）
        let split_pos = 20; // 落在 UTF-8 多字节序列内部
        let part1 = &bytes[..split_pos];
        let part2 = &bytes[split_pos..];

        extractor.push_bytes(part1);
        let blocks1 = extractor.extract_blocks();
        assert_eq!(blocks1.len(), 0, "分片未完成时不应误解析残缺数据");

        extractor.push_bytes(part2);
        let blocks2 = extractor.extract_blocks();
        assert_eq!(blocks2.len(), 1, "两片拼接后应正确提取完整事件");
        assert_eq!(blocks2[0].data_lines[0], "{\"content\":\"你好世界\"}");
    }

    #[test]
    fn test_sse_frame_extractor_event_done_and_comments() {
        let mut extractor = SseFrameExtractor::new();

        let sse_data = b": ping keep-alive\n\nevent: completion\ndata: {\"id\":\"msg_01\"}\n\ndata: [DONE]\n\n";
        extractor.push_bytes(sse_data);
        let blocks = extractor.extract_blocks();

        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].event_type.as_deref(), Some("completion"));
        assert_eq!(blocks[0].data_lines[0], "{\"id\":\"msg_01\"}");
        assert!(blocks[1].is_done);
    }

    #[test]
    fn test_sse_frame_extractor_flush_remaining() {
        let mut extractor = SseFrameExtractor::new();
        let partial = b"data: {\"id\":\"msg_final\"}";
        extractor.push_bytes(partial);
        assert_eq!(extractor.extract_blocks().len(), 0);

        let rem = extractor.flush_remaining();
        assert!(rem.is_some());
        let block = rem.unwrap();
        assert_eq!(block.data_lines[0], "{\"id\":\"msg_final\"}");
    }

    #[test]
    fn test_resolve_channel_routing() {
        let mut config = OpencodeProxyConfig::default();
        config.channels = vec![
            ChannelConfig {
                id: "opencode".to_string(),
                name: "OpenCode".to_string(),
                description: String::new(),
                enabled: true,
                protocol: "openai".to_string(),
                upstream_url: "https://api.opencode.ai/v1".to_string(),
                api_key: "sk-default".to_string(),
                api_keys: Vec::new(),
                use_proxy_pool: false,
                alias: "opencode".to_string(),
                site_id: None,
                use_fixed_proxy: false,
                enabled_models: Some(vec!["gpt-4o".to_string()]),
            },
            ChannelConfig {
                id: "x666".to_string(),
                name: "薄荷 API".to_string(),
                description: String::new(),
                enabled: true,
                protocol: "openai".to_string(),
                upstream_url: "https://x666.me/v1".to_string(),
                api_key: "sk-x666".to_string(),
                api_keys: Vec::new(),
                use_proxy_pool: false,
                alias: "x666".to_string(),
                site_id: None,
                use_fixed_proxy: false,
                enabled_models: Some(vec!["claude-sonnet-5".to_string(), "gemini-2.5-pro".to_string()]),
            },
        ];

        // 1. 带别名前缀解析
        let (ch, model) = resolve_channel(&config, "x666/claude-sonnet-5").expect("should resolve x666");
        assert_eq!(ch.id, "x666");
        assert_eq!(model, "claude-sonnet-5");

        // 2. 裸模型匹配 enabled_models 白名单
        let (ch2, model2) = resolve_channel(&config, "claude-sonnet-5").expect("should resolve x666 by model whitelist");
        assert_eq!(ch2.id, "x666");
        assert_eq!(model2, "claude-sonnet-5");

        // 3. 裸模型无特殊匹配时回退默认 opencode
        let (ch3, model3) = resolve_channel(&config, "unknown-model").expect("should fallback to opencode");
        assert_eq!(ch3.id, "opencode");
        assert_eq!(model3, "unknown-model");

        // 4. opencode 禁用后回退首个启用渠道
        config.channels[0].enabled = false;
        let (ch4, model4) = resolve_channel(&config, "unknown-model").expect("should fallback to x666");
        assert_eq!(ch4.id, "x666");
        assert_eq!(model4, "unknown-model");
    }

    #[test]
    fn test_sanitize_and_normalize_tools_and_calls() {
        let mut body = json!({
            "model": "claude-sonnet-5",
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "", // 残缺无名称 tool
                        "description": "empty name"
                    }
                },
                {
                    "name": "get_weather",
                    "description": "Get current weather",
                    "input_schema": { "type": "object", "properties": { "city": { "type": "string" } } }
                }
            ],
            "tool_choice": "auto",
            "messages": [
                {
                    "role": "assistant",
                    "content": "Let me check the weather.",
                    "tool_calls": [
                        {
                            "id": "call_1",
                            "type": "function",
                            "function": {
                                "name": "get_weather",
                                "arguments": "{\"city\":\"Beijing\"}"
                            }
                        },
                        {
                            "id": "call_bad",
                            "function": {
                                "name": "" // 空名称 tool call
                            }
                        }
                    ]
                },
                {
                    "role": "tool",
                    "content": "{\"temp\": 25}"
                }
            ]
        });

        OpenAiProtocolAdapter::sanitize_and_normalize(&mut body);

        // 验证 tools 中空名称被剔除，仅保留有效工具
        let tools = body["tools"].as_array().expect("tools should be array");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["function"]["name"], "get_weather");

        // 验证 assistant 消息中 tool_calls 残缺项被剔除
        let tc = body["messages"][0]["tool_calls"].as_array().expect("tool_calls should be array");
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0]["function"]["name"], "get_weather");

        // 验证 tool 消息补充了默认 tool_call_id
        assert_eq!(body["messages"][1]["tool_call_id"], "call_default");
    }

    #[test]
    fn test_stream_chunk_tool_calls_normalization() {
        // 模拟上游返回的第 2 个分片：只有 arguments，没有 name 或 name 为 null
        let mut chunk_val = json!({
            "id": "chatcmpl-123",
            "choices": [
                {
                    "index": 0,
                    "delta": {
                        "tool_calls": [
                            {
                                "index": 0,
                                "function": {
                                    "arguments": "{\"path\":"
                                }
                            }
                        ]
                    }
                }
            ]
        });

        let mut collected_tools = std::collections::BTreeMap::new();
        collected_tools.insert(0, ("call_abc123".to_string(), "read_file".to_string()));

        if let Some(choices) = chunk_val.get_mut("choices").and_then(JsonValue::as_array_mut) {
            for choice in choices {
                if let Some(delta) = choice.get_mut("delta").and_then(JsonValue::as_object_mut) {
                    if let Some(tool_calls_arr) = delta.get_mut("tool_calls").and_then(JsonValue::as_array_mut) {
                        for tc in tool_calls_arr {
                            let index = tc.get("index").and_then(JsonValue::as_u64).unwrap_or(0) as usize;
                            let (accum_id, accum_name) = collected_tools.get(&index).cloned().unwrap_or_default();

                            if let Some(tc_obj) = tc.as_object_mut() {
                                if !tc_obj.contains_key("index") {
                                    tc_obj.insert("index".to_string(), json!(index));
                                }
                                if !tc_obj.contains_key("type") {
                                    tc_obj.insert("type".to_string(), json!("function"));
                                }
                                if !accum_id.is_empty() && !tc_obj.contains_key("id") {
                                    tc_obj.insert("id".to_string(), json!(accum_id));
                                }

                                if let Some(func) = tc_obj.get_mut("function").and_then(JsonValue::as_object_mut) {
                                    if func.get("name").map_or(true, |v| v.is_null() || v.as_str().map_or(true, |s| s.is_empty())) {
                                        func.insert("name".to_string(), json!(accum_name));
                                    }
                                    if func.get("arguments").map_or(true, |v| v.is_null()) {
                                        func.insert("arguments".to_string(), json!(""));
                                    }
                                } else {
                                    tc_obj.insert("function".to_string(), json!({
                                        "name": accum_name,
                                        "arguments": ""
                                    }));
                                }
                            }
                        }
                    }
                }
            }
        }

        let tc = &chunk_val["choices"][0]["delta"]["tool_calls"][0];
        assert_eq!(tc["type"], "function");
        assert_eq!(tc["id"], "call_abc123");
        assert_eq!(tc["function"]["name"], "read_file");
        assert_eq!(tc["function"]["arguments"], "{\"path\":");
    }
}



