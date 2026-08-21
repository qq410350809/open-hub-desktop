use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::sync::Arc;
use std::time::Instant;
use tauri::AppHandle;
use tokio::sync::RwLock;

pub const DEFAULT_MODEL_PROXY_PORT: u16 = 8088;
#[allow(dead_code)]
pub const DEFAULT_OPENCODE_PROXY_PORT: u16 = DEFAULT_MODEL_PROXY_PORT;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelConfig {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub enabled: bool,
    #[serde(default = "default_protocol")]
    pub protocol: String,
    #[serde(rename = "upstreamUrl", alias = "base_url", alias = "baseUrl")]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    /// 站点转换继承的多个原 Key，请求时自动轮换尝试；为空时回退 apiKey
    #[serde(default)]
    pub api_keys: Option<Vec<String>>,
    #[serde(default)]
    pub use_proxy_pool: bool,
    /// 英文别名：网关模型前缀（如 opencode/*）；全渠道唯一。为空回退为渠道 id。
    #[serde(default)]
    pub alias: Option<String>,
    /// 通过「站点转换」创建时关联的站点库站点 id
    #[serde(default)]
    pub site_id: Option<String>,
    /// 代理池固定通道：始终经代理池出口节点转发，不优先直连
    #[serde(default)]
    pub use_fixed_proxy: bool,
    /// 固定出口节点 ID（仅在 use_fixed_proxy 为 true 时生效）
    #[serde(default)]
    pub fixed_proxy_node: Option<String>,
    /// 优先级（数值越小越优先）
    #[serde(default)]
    pub priority: Option<u32>,
    /// 权重（1-100）
    #[serde(default)]
    pub weight: Option<u32>,
    /// 该渠道对外暴露的模型白名单
    #[serde(default)]
    pub enabled_models: Option<Vec<String>>,
    /// 模型重定向映射表：例如 {"gpt-4": "gpt-4-turbo"}
    #[serde(default)]
    pub model_redirects: Option<HashMap<String, String>>,
    /// 渠道 RPM 限制
    #[serde(default)]
    pub rate_limit_rpm: Option<u32>,
}

fn default_protocol() -> String {
    "openai".to_string()
}

impl ChannelConfig {
    /// 获取当前渠道的生效英文别名：如果显式配置了 alias 则返回其小写去空格版本，否则回退为 id。
    pub fn effective_alias(&self) -> String {
        self.alias
            .as_deref()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.id.to_lowercase())
    }

    /// 获取有效的 API Keys 列表（若配置了 api_keys 列表则取其非空项，否则取单个 api_key）
    pub fn get_effective_keys(&self) -> Vec<String> {
        if let Some(keys) = &self.api_keys {
            let valid = keys
                .iter()
                .map(|k| k.trim().to_string())
                .filter(|k| !k.is_empty())
                .collect::<Vec<_>>();
            if !valid.is_empty() {
                return valid;
            }
        }
        let key = self.api_key.trim().to_string();
        if !key.is_empty() {
            vec![key]
        } else {
            Vec::new()
        }
    }
}

pub fn default_channels() -> Vec<ChannelConfig> {
    vec![ChannelConfig {
        id: "opencode".to_string(),
        name: "OpenCode 官方免费通道".to_string(),
        description: "由 OpenCode 提供的公益推理加速通道，免 Key 访问多种 Coding/Chat 顶尖模型".to_string(),
        enabled: true,
        protocol: "openai".to_string(),
        base_url: "https://opencode.ai/zen/v1".to_string(),
        api_key: String::new(),
        api_keys: None,
        use_proxy_pool: false,
        alias: Some("opencode".to_string()),
        site_id: None,
        use_fixed_proxy: false,
        fixed_proxy_node: None,
        priority: Some(1),
        weight: Some(100),
        enabled_models: None,
        model_redirects: None,
        rate_limit_rpm: None,
    }]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProxyConfig {
    pub enabled: bool,
    pub port: u16,
    pub api_key: String,
    pub channels: Vec<ChannelConfig>,
    pub timeout_seconds: u64,
    #[serde(default)]
    pub record_request_body: bool,
    #[serde(default)]
    pub max_retries: u32,
}

pub type OpencodeProxyConfig = ModelProxyConfig;

fn default_timeout_seconds() -> u64 {
    300
}

impl Default for ModelProxyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            port: DEFAULT_MODEL_PROXY_PORT,
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
pub struct ModelProxyStatus {
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

pub type OpencodeProxyStatus = ModelProxyStatus;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelUsageStats {
    pub channel_id: String,
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub avg_duration_ms: u64,
    pub avg_ttft_ms: Option<u64>,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_reasoning_tokens: u64,
    pub total_cache_hit_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelModelList {
    pub channel_id: String,
    pub channel_name: String,
    pub alias: String,
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelModelFetchError {
    pub channel_id: String,
    pub channel_name: String,
    pub alias: String,
    pub error: String,
}

#[derive(Default)]
pub struct ProxyMetrics {
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
pub struct ModelProxyContext {
    pub config: Arc<RwLock<ModelProxyConfig>>,
    pub metrics: Arc<ProxyMetrics>,
    pub started_at: Arc<RwLock<Option<Instant>>>,
    pub cached_channel_models: Arc<RwLock<Vec<ChannelModelList>>>,
    pub cached_fetch_errors: Arc<RwLock<Vec<ChannelModelFetchError>>>,
    pub default_http_client: Client,
    pub app_handle: Arc<RwLock<Option<AppHandle>>>,
    pub key_round_robin: Arc<AtomicUsize>,
    pub node_round_robin: Arc<AtomicUsize>,
}

#[allow(dead_code)]
pub type OpencodeProxyContext = ModelProxyContext;

pub struct ModelProxyState {
    pub context: ModelProxyContext,
    pub shutdown_sender: Arc<RwLock<Option<tokio::sync::oneshot::Sender<()>>>>,
    /// 正在运行的服务任务句柄，stop 时等待其退出以确保端口真正释放
    pub server_task: Arc<RwLock<Option<tauri::async_runtime::JoinHandle<()>>>>,
    pub current_port: Arc<RwLock<u16>>,
}

pub type OpencodeProxyState = ModelProxyState;

pub fn generate_req_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("req_{now:x}")
}

pub fn current_timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyLogsResponse {
    pub items: Vec<ProxyRequestLog>,
    pub total: usize,
    pub global_total: usize,
    pub global_success: usize,
    pub global_error: usize,
    pub success_total: usize,
    pub error_total: usize,
}
