use crate::context::AppContext;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::sync::Arc;
use std::sync::Arc as StdArc;
use std::time::Instant;
use tokio::sync::RwLock;

/// 模型网关默认端口：与内嵌 Web 服务同端口，dev 隔离形态走 17996。
pub fn default_model_proxy_port() -> u16 {
    crate::core::profile::preferred_service_port()
}

/// Key 分组调度模式：组内 Key 轮转做负载均衡。
pub const KEY_GROUP_MODE_ROUND_ROBIN: &str = "round_robin";
/// Key 分组调度模式：黏住组内首个可用 Key，仅在其失败时顺延到组内下一个 Key。
pub const KEY_GROUP_MODE_INDEPENDENT: &str = "independent";

pub fn default_key_group_mode() -> String {
    KEY_GROUP_MODE_ROUND_ROBIN.to_string()
}

/// 渠道内的一个 Key 分组。分组身份直接取自 Key 自身的 groupName（站点同步下发），
/// 同名 groupName 的 Key 天然落在同一组；`id` 即该 groupName。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyGroupItem {
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// round_robin（缺省）= 组内 Key 逐请求轮询；independent = 黏住首个 Key，失败才顺延组内下一个
    #[serde(default = "default_key_group_mode")]
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelKeyRule {
    pub key: String,
    #[serde(default)]
    pub group_id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub supported_models: Option<Vec<String>>,
    /// 渠道 proxy_mode = fixed_channel 时，该 Key 绑定的代理池固定通道 ID；
    /// 空 = 使用渠道级默认固定通道（proxy_fixed_channel）
    #[serde(default)]
    pub fixed_channel_id: Option<String>,
}

fn default_true() -> bool {
    true
}

/// 模型级代理出口覆盖规则：「管理可用模型」中为单个模型独立选择代理策略。
/// 语义与渠道级 proxy_mode 一致，仅作用域缩小到单模型。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProxyRule {
    /// 渠道内模型名（原始大小写保留，匹配时忽略大小写）
    pub model: String,
    /// direct = 强制直连；pool = 走代理池轮询；fixed = 固定单一出口节点（旧值，加载时
    /// 归一为 custom_node）；custom_node = 固定单一出口节点；
    /// fixed_channel = 固定通道（代理池通道出口，可按 Key 绑定覆盖）；
    /// follow = 跟随渠道（等价于无本条规则，前端显式表达默认值）
    pub mode: String,
    /// fixed/custom_node 模式下锁定的代理池节点 ID；缺省时锁定池内首个启用节点
    #[serde(default)]
    pub node_id: Option<String>,
    /// fixed_channel 模式锁定的代理池通道 ID；缺省回退渠道默认固定通道
    #[serde(default)]
    pub channel_id: Option<String>,
    /// 模型级上游协议覆盖：openai / openai-responses / anthropic / gemini。
    /// 缺省 = 跟随渠道级 protocol。仅覆盖出网协议，不影响出口代理策略。
    #[serde(default)]
    pub protocol: Option<String>,
}

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
    /// 渠道级代理设置（合并旧 use_proxy_pool / use_fixed_proxy 两个布尔）：
    /// direct = 强制直连（默认）| pool = 代理池轮询+失败切换（直连优先）|
    /// fixed_channel = 代理池固定通道（Key 可按 KeyRule 绑定不同通道）|
    /// custom_node = 固定单一出口节点（fixed_proxy_node）。
    /// 缺省时按旧布尔字段推导，保持存量配置兼容。
    #[serde(default)]
    pub proxy_mode: Option<String>,
    /// fixed_channel 模式的渠道级默认固定通道 ID（Key 规则可按 Key 覆盖）
    #[serde(default)]
    pub proxy_fixed_channel: Option<String>,
    /// 代理池固定节点：始终经代理池出口节点转发，不优先直连（custom_node 模式生效）
    #[serde(default)]
    pub use_fixed_proxy: bool,
    /// 固定出口节点 ID（仅在 use_fixed_proxy 为 true 时生效）
    #[serde(default)]
    pub fixed_proxy_node: Option<String>,
    /// 该渠道对外暴露的模型白名单
    #[serde(default)]
    pub enabled_models: Option<Vec<String>>,
    /// 模型重定向映射表：例如 {"gpt-4": "gpt-4-turbo"}
    #[serde(default)]
    pub model_redirects: Option<HashMap<String, String>>,
    /// 统计维度稳定数字 ID：内置固化渠道占用 1-100（opencode=1），动态渠道从 101 递增。
    /// 与可修改的英文别名解耦，改名/改编码后历史统计不错位。
    #[serde(default)]
    pub stats_id: Option<u32>,
    /// 渠道内 Key 分组定义与优先级序列（索引越靠前优先级越高）
    #[serde(default)]
    pub key_groups: Option<Vec<KeyGroupItem>>,
    /// 渠道中各个 Key 的详细配置（所属分组、启用/禁用、支持模型白名单）
    #[serde(default)]
    pub key_rules: Option<Vec<ChannelKeyRule>>,
    /// 模型级代理出口覆盖：为单个模型独立指定直连/代理池/固定节点
    #[serde(default)]
    pub model_proxy_rules: Option<Vec<ModelProxyRule>>,
}

fn default_protocol() -> String {
    "openai".to_string()
}

impl ChannelConfig {
    /// 日统计/汇总表的渠道维度键：优先稳定数字 ID，未分配时回退别名
    pub fn stats_key(&self) -> String {
        self.stats_id
            .map(|v| v.to_string())
            .unwrap_or_else(|| self.effective_alias())
    }

    /// 获取当前渠道的生效英文别名：如果显式配置了 alias 则返回其小写去空格版本，否则回退为 id。
    pub fn effective_alias(&self) -> String {
        self.alias
            .as_deref()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.id.to_lowercase())
    }

    /// 渠道级生效代理模式：proxy_mode 显式配置优先；缺省按旧布尔字段推导
    /// （use_fixed_proxy → custom_node，use_proxy_pool → pool，否则 direct），
    /// 保持存量配置行为不变。
    pub fn effective_proxy_mode(&self) -> String {
        if let Some(mode) = self.proxy_mode.as_deref() {
            let mode = mode.trim().to_lowercase();
            if !mode.is_empty() {
                return mode;
            }
        }
        if self.use_fixed_proxy {
            "custom_node".to_string()
        } else if self.use_proxy_pool {
            "pool".to_string()
        } else {
            "direct".to_string()
        }
    }

    /// fixed_channel 模式下某 API Key 绑定的代理池固定通道 ID（KeyRule 覆盖）
    pub fn key_fixed_channel(&self, api_key: &str) -> Option<String> {
        let needle = api_key.trim();
        if needle.is_empty() {
            return None;
        }
        self.key_rules
            .as_ref()?
            .iter()
            .find(|r| r.key.trim() == needle)?
            .fixed_channel_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }

    /// fixed_channel 模式的渠道级默认固定通道 ID
    pub fn default_fixed_channel(&self) -> Option<String> {
        self.proxy_fixed_channel
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }

    /// 查找指定模型的代理出口覆盖规则（忽略大小写匹配）
    pub fn model_proxy_rule(&self, model: &str) -> Option<&ModelProxyRule> {
        let needle = model.trim().to_lowercase();
        self.model_proxy_rules
            .as_ref()?
            .iter()
            .find(|r| r.model.trim().to_lowercase() == needle)
    }

    /// 出网目标协议：模型级规则覆盖优先，未覆盖或值非法时回退渠道级 protocol。
    pub fn target_protocol_for(&self, model: &str) -> crate::model::gateway::egress::TargetProtocol {
        if let Some(rule) = self.model_proxy_rule(model) {
            if let Some(protocol) = rule.protocol.as_deref().map(str::trim) {
                if !protocol.is_empty() {
                    return crate::model::gateway::egress::TargetProtocol::from_str(protocol);
                }
            }
        }
        crate::model::gateway::egress::TargetProtocol::from_channel(self)
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
        name: "OpenCode 免费".to_string(),
        description: "由 OpenCode 提供的公益推理加速通道，免 Key 访问多种 Coding/Chat 顶尖模型"
            .to_string(),
        enabled: true,
        protocol: "openai".to_string(),
        base_url: "https://opencode.ai/zen/v1".to_string(),
        api_key: String::new(),
        api_keys: None,
        use_proxy_pool: false,
        alias: Some("opencode".to_string()),
        site_id: None,
        proxy_mode: None,
        proxy_fixed_channel: None,
        use_fixed_proxy: false,
        fixed_proxy_node: None,
        enabled_models: None,
        model_redirects: None,
        stats_id: Some(1),
        key_groups: None,
        key_rules: None,
        model_proxy_rules: None,
    }]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProxyConfig {
    pub enabled: bool,
    /// 监听地址。默认仅回环；需要局域网/远程访问时必须显式配置 API key。
    #[serde(default = "default_listen_host")]
    pub listen_host: String,
    pub port: u16,
    pub api_key: String,
    pub channels: Vec<ChannelConfig>,
    /// 出网超时秒数；serde default 兜底，防止存量 JSON 缺字段时整体解析失败
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    /// 全文记录总开关：开启时同时记录客户端请求原文与上游响应原文，
    /// 关闭则两者都不记（在 record_log 落库出口统一拦截）
    #[serde(default)]
    pub record_request_body: bool,
    #[serde(default)]
    pub max_retries: u32,
    /// 动态渠道统计 ID 分配计数器（从 101 起，1-100 预留给内置固化渠道）
    #[serde(default = "default_next_channel_stats_id")]
    pub next_channel_stats_id: u64,
    /// 请求明细日志保留天数：超期日志由网关自动清理（统计聚合表不受影响）。
    /// None 或 0 表示永久保留，由用户手动通过范围清理管理。
    #[serde(default)]
    pub log_retention_days: Option<u32>,
    /// 多渠道共同提供的模型路由顺序：key=模型名（小写），value=候选渠道 ID 有序列表，
    /// 排前的优先承接该模型的无前缀调用；未配置的模型沿用渠道数组顺序。
    #[serde(default)]
    pub model_channel_order: Option<HashMap<String, Vec<String>>>,
}

impl ModelProxyConfig {
    /// 生效的日志保留天数（0 或未配置 = 永久保留）
    pub fn effective_log_retention_days(&self) -> u32 {
        self.log_retention_days.unwrap_or(0)
    }
}

pub type OpencodeProxyConfig = ModelProxyConfig;

fn default_timeout_seconds() -> u64 {
    300
}

fn default_listen_host() -> String {
    "127.0.0.1".to_string()
}

fn default_next_channel_stats_id() -> u64 {
    101
}

impl Default for ModelProxyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen_host: default_listen_host(),
            port: default_model_proxy_port(),
            api_key: String::new(),
            channels: default_channels(),
            timeout_seconds: default_timeout_seconds(),
            record_request_body: false,
            max_retries: 0,
            next_channel_stats_id: default_next_channel_stats_id(),
            log_retention_days: None,
            model_channel_order: None,
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
    /// 展示用渠道维度：生效别名（请求日志表列，可变、仅短期保留）
    pub channel_id: String,
    /// 统计用渠道维度：稳定数字 ID 字符串；None 时日统计回退用 channel_id
    #[serde(default)]
    pub channel_stats_id: Option<String>,
    pub model: String,
    pub stream: bool,
    pub status_code: u16,
    pub duration_ms: u64,
    pub ttft_ms: Option<u64>,
    pub prompt_tokens: Option<u64>,
    pub prompt_cache_hit_tokens: Option<u64>,
    pub prompt_cache_miss_tokens: Option<u64>,
    /// Prompt 缓存写入量（Anthropic cache_creation_input_tokens 等）
    pub cache_creation_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub error_message: Option<String>,
    pub request_body: Option<String>,
    pub response_body: Option<String>,
    pub node_name: Option<String>,
    /// 发起请求的客户端标识（由 User-Agent / 端点推断，如 claude / codex / cursor）
    pub client_name: Option<String>,
    /// 出网上游地址（完整 URL，含 path），用于日志展示「入->出」双地址
    pub upstream_url: Option<String>,
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
    /// 今日（本地时区）双通道数据
    pub today_requests: u64,
    pub today_successful_requests: u64,
    pub today_failed_requests: u64,
    pub today_avg_duration_ms: u64,
    pub today_avg_ttft_ms: Option<u64>,
    pub today_prompt_tokens: u64,
    pub today_completion_tokens: u64,
    pub today_cache_hit_tokens: u64,
    pub today_total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelModelList {
    pub channel_id: String,
    pub channel_name: String,
    pub alias: String,
    pub models: Vec<String>,
}

/// 「日 × 全渠道」聚合数据点（来自 channel_daily_stats，跨渠道求和）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayDailyPoint {
    /// 本地日期，格式 YYYY-MM-DD
    pub date: String,
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_hit_tokens: u64,
    pub total_tokens: u64,
}

/// 「时 × 全渠道」聚合数据点（≤3 天区间趋势用，来自 channel_hourly_stats 跨渠道求和）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayHourlyPoint {
    /// 本地日期，格式 YYYY-MM-DD（多天区间时用于区分小时桶归属）
    pub date: String,
    /// 0-23（本地时间）
    pub hour: u32,
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_hit_tokens: u64,
    pub total_tokens: u64,
}

/// 全渠道累计汇总（不限于图表窗口，含平均耗时/首 Token 时延）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayOverviewTotals {
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub avg_duration_ms: u64,
    pub avg_ttft_ms: Option<u64>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_hit_tokens: u64,
    pub total_tokens: u64,
}

/// 控制台「全渠道数据总览」：区间逐日数据（缺日补零）+ 区间累计汇总 + 今日聚合。
/// 日期区间模式（from/to）下 totals/daily 均按区间统计；未提供区间时为近 N 天窗口 + 全量累计。
/// 粒度自动适配：区间 ≤3 天且有小时数据时附带 hourly（每天 24 点缺时补零）；
/// 跨度超过一个季度（92 天）时附带 monthly（缺月补零，date 为 YYYY-MM）；其余按日。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayOverviewStats {
    pub days: u32,
    pub daily: Vec<GatewayDailyPoint>,
    pub totals: GatewayOverviewTotals,
    /// 今日（本地时区）全渠道聚合，与所选区间解耦，供 KPI「今日」角标使用
    pub today: GatewayDailyPoint,
    /// ≤3 天区间的小时级趋势（每天 24 点）；不满足条件或无小时数据时为 None
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hourly: Option<Vec<GatewayHourlyPoint>>,
    /// 长区间（>92 天）的月级趋势；date 为 YYYY-MM；区间内有数据时才返回
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monthly: Option<Vec<GatewayDailyPoint>>,
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
    /// 主 HTTP 服务中网关路由的启用状态。
    pub route_enabled: Arc<AtomicBool>,
    pub config: Arc<RwLock<ModelProxyConfig>>,
    pub metrics: Arc<ProxyMetrics>,
    pub started_at: Arc<RwLock<Option<Instant>>>,
    pub current_port: Arc<RwLock<u16>>,
    pub cached_channel_models: Arc<RwLock<Vec<ChannelModelList>>>,
    pub cached_fetch_errors: Arc<RwLock<Vec<ChannelModelFetchError>>>,
    pub default_http_client: Arc<tokio::sync::RwLock<Client>>,
    /// 平台无关的应用上下文（桌面与 server 共用）；启动后注入。
    pub app_ctx: StdArc<RwLock<Option<StdArc<AppContext>>>>,
    pub key_round_robin: Arc<AtomicUsize>,
    /// 出口节点轮询游标，**按渠道分片**。
    ///
    /// 曾是单个全局计数器：渠道 A 因 429 推进游标会让毫不相关的渠道 B
    /// 下次请求从一个偏移过的节点开始——A 的限流污染了 B 的节点选择。
    /// 现每个渠道各持一份游标，互不干扰；`node_round_robin_for` 按需惰性创建。
    pub node_round_robin: Arc<RwLock<HashMap<String, Arc<AtomicUsize>>>>,
    /// 上次执行明细保留期清理的时刻（epoch 毫秒），用于节流避免每次写入全表扫描
    pub log_retention_last_run: Arc<std::sync::atomic::AtomicU64>,
}

impl ModelProxyContext {
    /// 取指定渠道的出口节点轮询游标（不存在则惰性创建）。
    ///
    /// 返回 `Arc` 而非借用：调用方持有它跨越 await 点（重试循环中推进游标），
    /// 不能让 map 的读锁一直悬着。
    pub async fn node_round_robin_for(&self, channel_id: &str) -> Arc<AtomicUsize> {
        if let Some(counter) = self.node_round_robin.read().await.get(channel_id) {
            return counter.clone();
        }
        // 双检：读锁释放到写锁获取之间可能已被其他请求创建
        self.node_round_robin
            .write()
            .await
            .entry(channel_id.to_string())
            .or_insert_with(|| Arc::new(AtomicUsize::new(0)))
            .clone()
    }
}

#[allow(dead_code)]
pub type OpencodeProxyContext = ModelProxyContext;

pub struct ModelProxyState {
    pub context: ModelProxyContext,
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

/// 反代模式 Token 报表：与本地模式同构的用量桶（TokenUsageReport）+ 请求健康，
/// 由 channel_daily_stats / channel_hourly_stats 聚合表生成，
/// 供 Token 统计中心「反代模式」标签直接复用本地模式的前端聚合层。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyTokenUsageReport {
    pub usage: crate::core::models::TokenUsageReport,
    pub health: crate::core::models::RequestHealthReport,
}

/// 「渠道 × 模型」粒度的累计用量（全量 + 今日），channel_daily_stats 聚合结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelModelUsageStats {
    pub model: String,
    pub total_requests: u64,
    pub failed_requests: u64,
    pub avg_duration_ms: u64,
    pub avg_ttft_ms: Option<u64>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    /// 最近一次调用时间（YYYY-MM-DD HH:MM:SS，无调用时为 None）
    pub last_used_at: Option<String>,
    pub today_requests: u64,
    pub today_tokens: u64,
}
