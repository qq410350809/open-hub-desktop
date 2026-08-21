use crate::chrome_session;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::{HashMap, HashSet};
use tauri::Emitter;

pub(crate) const SEED_JSON: &str = include_str!("../../resources/sites.json");
pub(crate) const NOW_SQL: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";
pub(crate) const REMOTE_ROOT_URL: &str = "https://ldoh.105117.xyz/";
pub(crate) const REMOTE_USER_URL: &str = "https://ldoh.105117.xyz/api/ld/user";
pub(crate) const REMOTE_SITES_URL: &str = "https://ldoh.105117.xyz/api/sites";
pub(crate) const REMOTE_SESSION_COOKIE: &str = "ld_auth_session";
pub(crate) const NETWORK_PROXY_KEY: &str = "network_proxy";
pub(crate) const ACTIVE_PROXY_NODE_KEY: &str = "active_proxy_node";
pub(crate) const PROXY_IGNORE_KEY: &str = "proxy_ignore_addresses";
pub(crate) const PROXY_SPEED_TEST_URL_KEY: &str = "proxy_speed_test_url";
pub(crate) const DEFAULT_PROXY_IGNORE: &str =
    "localhost,127.0.0.1,::1,.local,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16";
pub(crate) const DEFAULT_PROXY_SPEED_TEST_URL: &str = "http://www.gstatic.com/generate_204";

pub(crate) struct Database(pub(crate) std::sync::Mutex<Connection>);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Maintainer {
    pub(crate) name: String,
    pub(crate) id: String,
    pub(crate) username: String,
    pub(crate) profile_url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct ExtensionLink {
    pub(crate) label: String,
    pub(crate) url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct SiteRecord {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) registration_limit: i64,
    pub(crate) icon: String,
    pub(crate) api_base_url: String,
    #[serde(
        alias = "siteType",
        alias = "site_type",
        alias = "apiType",
        alias = "api_type",
        alias = "platform",
        alias = "system"
    )]
    pub(crate) system_type: String,
    pub(crate) tags: Vec<String>,
    pub(crate) supports_immersive_translation: bool,
    pub(crate) supports_ldc: bool,
    pub(crate) supports_checkin: bool,
    pub(crate) supports_nsfw: bool,
    pub(crate) checkin_url: String,
    pub(crate) checkin_note: String,
    pub(crate) benefit_url: String,
    pub(crate) maintainers: Vec<Maintainer>,
    pub(crate) rate_limit: String,
    pub(crate) status_url: String,
    pub(crate) extension_links: Vec<ExtensionLink>,
    pub(crate) is_only_maintainer_visible: bool,
    pub(crate) requires_invite_code: bool,
    pub(crate) is_runaway: bool,
    pub(crate) is_fake_charity: bool,
    pub(crate) has_pending_report: bool,
    pub(crate) is_personal: bool,
    pub(crate) is_pending: bool,
    #[serde(skip)]
    pub(crate) use_system_proxy: bool,
    pub(crate) use_proxy_pool: bool,

    pub(crate) favorite: bool,
    pub(crate) hidden: bool,
    pub(crate) updated_at: String,
}

impl Default for SiteRecord {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            registration_limit: 0,
            icon: String::new(),
            api_base_url: String::new(),
            system_type: String::new(),
            tags: Vec::new(),
            supports_immersive_translation: false,
            supports_ldc: false,
            supports_checkin: false,
            supports_nsfw: false,
            checkin_url: String::new(),
            checkin_note: String::new(),
            benefit_url: String::new(),
            maintainers: Vec::new(),
            rate_limit: String::new(),
            status_url: String::new(),
            extension_links: Vec::new(),
            is_only_maintainer_visible: false,
            requires_invite_code: false,
            is_runaway: false,
            is_fake_charity: false,
            has_pending_report: false,
            is_personal: false,
            is_pending: false,
            use_system_proxy: false,
            use_proxy_pool: false,
            favorite: false,
            hidden: false,
            updated_at: String::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct SeedPayload {
    pub(crate) sites: Vec<SiteRecord>,
    pub(crate) tags: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LibraryData {
    pub(crate) sites: Vec<SiteRecord>,
    pub(crate) suggested_tags: Vec<String>,
    pub(crate) usage_sites: Vec<chrome_session::ChromeSiteSessionMatch>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SyncSitesResult {
    pub(crate) added: usize,
    pub(crate) updated: usize,
    pub(crate) total: usize,
    pub(crate) profile_name: String,
    pub(crate) account_name: String,
    pub(crate) user_name: String,
    pub(crate) runaway: bool,
    pub(crate) site_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SyncSitesProgress {
    pub(crate) run_id: u64,
    pub(crate) stage: String,
    pub(crate) status: String,
    pub(crate) message: String,
}

pub(crate) fn emit_sync_progress(
    app: &tauri::AppHandle,
    run_id: u64,
    stage: &str,
    status: &str,
    message: String,
) {
    let _ = app.emit(
        "sync-sites-progress",
        SyncSitesProgress {
            run_id,
            stage: stage.into(),
            status: status.into(),
            message,
        },
    );
}

pub(crate) fn emit_chrome_account_progress(
    app: &tauri::AppHandle,
    run_id: u64,
    stage: &str,
    status: &str,
    message: impl Into<String>,
) {
    let _ = app.emit(
        "chrome-account-sync-progress",
        SyncSitesProgress {
            run_id,
            stage: stage.into(),
            status: status.into(),
            message: message.into(),
        },
    );
}

pub(crate) fn emit_optional_sync_progress(
    app: &tauri::AppHandle,
    run_id: Option<u64>,
    stage: &str,
    status: &str,
    message: String,
) {
    if let Some(run_id) = run_id {
        emit_sync_progress(app, run_id, stage, status, message);
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemoteUserInfo {
    pub(crate) name: String,
    pub(crate) username: String,
    pub(crate) avatar_url: String,
    pub(crate) profile_name: String,
    pub(crate) account_name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChromeUsageScanResult {
    pub(crate) scanned: usize,
    pub(crate) detected: usize,
    pub(crate) accounts: usize,
    pub(crate) warnings: usize,
    pub(crate) newly_marked: usize,
    pub(crate) sites: Vec<chrome_session::ChromeSiteSessionMatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SiteModelCacheAccount {
    pub(crate) profile_id: String,
    pub(crate) profile_name: String,
    pub(crate) account_name: String,
    pub(crate) username: String,
    pub(crate) keys: Vec<String>,
    #[serde(default)]
    pub(crate) key_groups: HashMap<String, String>,
    /// 每个 Key 对应的模型列表（逐 Key 查询 /v1/models 的结果）。
    #[serde(default)]
    pub(crate) key_models: HashMap<String, Vec<SiteModelItem>>,
    pub(crate) error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SiteModelCache {
    pub(crate) models: Vec<SiteModelItem>,
    pub(crate) api_source: String,
    pub(crate) accounts: Vec<SiteModelCacheAccount>,
}

/// 跨站点聚合用：站点 ID + 该站点的模型缓存。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SiteModelCacheEntry {
    pub(crate) site_id: String,
    pub(crate) cache: SiteModelCache,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SiteModelItem {
    pub(crate) id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) owned_by: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SiteAccountSnapshot {
    pub(crate) username: String,
    pub(crate) remaining: Option<f64>,
    pub(crate) used: Option<f64>,
    pub(crate) total: Option<f64>,
    pub(crate) unit: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CheckinSnapshot {
    pub(crate) enabled: bool,
    pub(crate) checked_in_today: bool,
    pub(crate) error: String,
}

#[derive(Debug)]
pub(crate) struct SiteAccountRefresh {
    pub(crate) account: SiteAccountSnapshot,
    pub(crate) is_valid: bool,
    pub(crate) sync_error: String,
    pub(crate) checkin: CheckinSnapshot,
    pub(crate) newapi_token: String,
    pub(crate) newapi_user_id: String,
}

#[derive(Clone)]
pub(crate) enum NewApiAuth {
    Legacy {
        cookie_header: String,
        user_id: String,
    },
    Token {
        access_token: String,
        user_id: String,
    },
}

pub(crate) fn site_matches_requested_scope(
    site_id: &str,
    requested_site_id: Option<&str>,
    site_id_was_supplied: bool,
    requested_site_ids: &HashSet<String>,
    site_ids_were_supplied: bool,
) -> bool {
    if let Some(requested) = requested_site_id {
        requested == site_id
    } else if site_id_was_supplied {
        false
    } else if site_ids_were_supplied {
        requested_site_ids.contains(site_id)
    } else {
        true
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChromeBridgeAccountResult {
    pub(crate) ok: bool,
    #[serde(default)]
    pub(crate) error: String,
    pub(crate) account: Option<serde_json::Value>,
    #[serde(default)]
    pub(crate) checkin_enabled: bool,
    #[serde(default)]
    pub(crate) checked_in_today: bool,
    #[serde(default)]
    pub(crate) checkin_error: String,
    #[serde(default)]
    pub(crate) api_token: String,
    #[serde(default)]
    pub(crate) user_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct ProxySubscription {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) url: String,
    pub(crate) node_count: i64,
    pub(crate) last_error: String,
    pub(crate) updated_at: String,
    pub(crate) created_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct ProxyNode {
    pub(crate) id: String,
    pub(crate) subscription_names: Vec<String>,
    pub(crate) name: String,
    pub(crate) proxy_type: String,
    pub(crate) server: String,
    pub(crate) port: i64,
    pub(crate) cipher: String,
    pub(crate) udp: bool,
    pub(crate) latency_ms: Option<i64>,
    pub(crate) test_status: String,
    pub(crate) tested_at: String,
    pub(crate) channel_latency_ms: Option<i64>,
    pub(crate) channel_test_status: String,
    pub(crate) country_code: String,
    pub(crate) country_name: String,
    pub(crate) classification: String,
    pub(crate) primary_ip: String,
    pub(crate) updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct ProxyChannelAccount {
    pub(crate) profile_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct ProxyChannel {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) node_id: String,
    pub(crate) node: Option<ProxyNode>,
    pub(crate) port: Option<u16>,
    pub(crate) test_url: String,
    pub(crate) account_count: i64,
    pub(crate) accounts: Vec<ProxyChannelAccount>,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct ProxyPoolState {
    pub(crate) subscriptions: Vec<ProxySubscription>,
    pub(crate) nodes: Vec<ProxyNode>,
    pub(crate) channels: Vec<ProxyChannel>,
    pub(crate) default_channel_id: String,
    pub(crate) active_node_id: String,
    pub(crate) active_node: Option<ProxyNode>,
    pub(crate) enabled: bool,
    pub(crate) ignore_addresses: String,
    pub(crate) speed_test_url: String,
    pub(crate) runtime_available: bool,
    pub(crate) runtime_path: String,
    pub(crate) runtime_error: String,
    pub(crate) node_count: i64,
    pub(crate) subscription_count: i64,
    pub(crate) invalid_node_count: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct ProxyPoolRefreshResult {
    pub(crate) subscription: ProxySubscription,
    pub(crate) added: usize,
    pub(crate) total: usize,
    pub(crate) discarded: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct ProxySourceProgress {
    pub(crate) source_id: String,
    pub(crate) stage: String,
    pub(crate) status: String,
    pub(crate) message: String,
    pub(crate) completed: usize,
    pub(crate) total: usize,
    pub(crate) added: usize,
    pub(crate) discarded: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct ProxyNodeTestProgress {
    pub(crate) node_id: String,
    pub(crate) phase: String,
    pub(crate) latency_ms: Option<i64>,
    pub(crate) status: String,
    pub(crate) completed: usize,
    pub(crate) total: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct ProxyIpNodeAnalysis {
    pub(crate) node_id: String,
    pub(crate) node_name: String,
    pub(crate) server: String,
    pub(crate) resolved_ips: Vec<String>,
    pub(crate) primary_ip: String,
    pub(crate) classification: String,
    pub(crate) country_code: String,
    pub(crate) country_name: String,
    pub(crate) error: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct ProxyIpGroup {
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) classification: String,
    pub(crate) country_code: String,
    pub(crate) country_name: String,
    pub(crate) node_ids: Vec<String>,
    pub(crate) node_count: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct ProxyIpAnalysis {
    pub(crate) analyzed_at: String,
    pub(crate) geoip_available: bool,
    pub(crate) geoip_database_path: String,
    pub(crate) total_nodes: usize,
    pub(crate) resolved_nodes: usize,
    pub(crate) unresolved_nodes: usize,
    pub(crate) unique_ips: usize,
    pub(crate) nodes: Vec<ProxyIpNodeAnalysis>,
    pub(crate) groups: Vec<ProxyIpGroup>,
}

// —— Token 统计（OpenHub 直接读取各工具本地日志）——
// 后端缓存与前端均使用 camelCase，避免自有缓存序列化/反序列化口径分裂。

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct TokenStatsReport {
    pub(crate) available: bool,
    pub(crate) sessions: Vec<TokenSession>,
    #[serde(alias = "session_count")]
    pub(crate) session_count: i64,
    pub(crate) summary: TokenSummary,
    #[serde(alias = "by_model")]
    pub(crate) by_model: Vec<TokenModelStat>,
    pub(crate) subagents: Vec<TokenSubagentStat>,
    pub(crate) provenance: JsonValue,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct TokenSessionTokens {
    #[serde(alias = "input_tokens")]
    pub(crate) input_tokens: i64,
    #[serde(alias = "cached_input_tokens")]
    pub(crate) cached_input_tokens: i64,
    #[serde(alias = "cache_creation_input_tokens")]
    pub(crate) cache_creation_input_tokens: i64,
    #[serde(alias = "output_tokens")]
    pub(crate) output_tokens: i64,
    #[serde(alias = "reasoning_output_tokens")]
    pub(crate) reasoning_output_tokens: i64,
    #[serde(alias = "total_tokens")]
    pub(crate) total_tokens: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct TokenSession {
    pub(crate) version: i64,
    #[serde(alias = "session_hash")]
    pub(crate) session_hash: String,
    pub(crate) source: String,
    #[serde(alias = "project_key")]
    pub(crate) project_key: String,
    pub(crate) model: String,
    #[serde(alias = "started_at")]
    pub(crate) started_at: String,
    #[serde(alias = "ended_at")]
    pub(crate) ended_at: String,
    #[serde(alias = "active_ms")]
    pub(crate) active_ms: i64,
    pub(crate) turns: i64,
    #[serde(alias = "edit_turns")]
    pub(crate) edit_turns: i64,
    #[serde(alias = "retry_turns")]
    pub(crate) retry_turns: i64,
    #[serde(alias = "subagent_calls")]
    pub(crate) subagent_calls: i64,
    #[serde(alias = "subagent_types")]
    pub(crate) subagent_types: JsonValue,
    pub(crate) tokens: TokenSessionTokens,
    pub(crate) provenance: JsonValue,
    #[serde(alias = "duration_ms")]
    pub(crate) duration_ms: i64,
    #[serde(alias = "total_tokens")]
    pub(crate) total_tokens: i64,
    #[serde(alias = "cost_usd")]
    pub(crate) cost_usd: f64,
    pub(crate) productive: bool,
    #[serde(alias = "first_pass")]
    pub(crate) first_pass: bool,
    #[serde(alias = "one_shot")]
    pub(crate) one_shot: bool,
    #[serde(alias = "tokens_per_edit")]
    pub(crate) tokens_per_edit: Option<f64>,
    #[serde(alias = "cost_per_edit")]
    pub(crate) cost_per_edit: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct TokenSummary {
    pub(crate) sessions: i64,
    #[serde(alias = "productive_sessions")]
    pub(crate) productive_sessions: i64,
    #[serde(alias = "one_shot_sessions")]
    pub(crate) one_shot_sessions: i64,
    #[serde(alias = "edit_turns")]
    pub(crate) edit_turns: i64,
    pub(crate) retries: i64,
    #[serde(alias = "total_tokens")]
    pub(crate) total_tokens: i64,
    #[serde(alias = "cost_usd")]
    pub(crate) cost_usd: f64,
    #[serde(alias = "edit_tokens")]
    pub(crate) edit_tokens: i64,
    #[serde(alias = "edit_cost_usd")]
    pub(crate) edit_cost_usd: f64,
    #[serde(alias = "productive_rate")]
    pub(crate) productive_rate: f64,
    #[serde(alias = "one_shot_rate")]
    pub(crate) one_shot_rate: Option<f64>,
    #[serde(alias = "edit_sessions")]
    pub(crate) edit_sessions: i64,
    #[serde(alias = "first_pass_sessions")]
    pub(crate) first_pass_sessions: i64,
    #[serde(alias = "edit_session_rate")]
    pub(crate) edit_session_rate: f64,
    #[serde(alias = "first_pass_rate")]
    pub(crate) first_pass_rate: Option<f64>,
    #[serde(alias = "tokens_per_edit")]
    pub(crate) tokens_per_edit: Option<f64>,
    #[serde(alias = "cost_per_edit")]
    pub(crate) cost_per_edit: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct TokenModelStat {
    pub(crate) model: String,
    pub(crate) sessions: i64,
    #[serde(alias = "productive_sessions")]
    pub(crate) productive_sessions: i64,
    #[serde(alias = "one_shot_sessions")]
    pub(crate) one_shot_sessions: i64,
    #[serde(alias = "edit_turns")]
    pub(crate) edit_turns: i64,
    pub(crate) retries: i64,
    #[serde(alias = "total_tokens")]
    pub(crate) total_tokens: i64,
    #[serde(alias = "cost_usd")]
    pub(crate) cost_usd: f64,
    #[serde(alias = "edit_tokens")]
    pub(crate) edit_tokens: i64,
    #[serde(alias = "edit_cost_usd")]
    pub(crate) edit_cost_usd: f64,
    #[serde(alias = "productive_rate")]
    pub(crate) productive_rate: f64,
    #[serde(alias = "one_shot_rate")]
    pub(crate) one_shot_rate: Option<f64>,
    #[serde(alias = "edit_sessions")]
    pub(crate) edit_sessions: i64,
    #[serde(alias = "first_pass_sessions")]
    pub(crate) first_pass_sessions: i64,
    #[serde(alias = "edit_session_rate")]
    pub(crate) edit_session_rate: f64,
    #[serde(alias = "first_pass_rate")]
    pub(crate) first_pass_rate: Option<f64>,
    #[serde(alias = "tokens_per_edit")]
    pub(crate) tokens_per_edit: Option<f64>,
    #[serde(alias = "cost_per_edit")]
    pub(crate) cost_per_edit: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct TokenSubagentStat {
    pub(crate) name: String,
    pub(crate) calls: i64,
    pub(crate) sessions: i64,
    #[serde(alias = "total_tokens")]
    pub(crate) total_tokens: f64,
    #[serde(alias = "cost_usd")]
    pub(crate) cost_usd: f64,
}

// —— Token 用量小时桶（OpenHub 自有采集器按来源/模型/项目/半小时聚合）——

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct TokenUsageBucket {
    pub(crate) source: String,
    pub(crate) model: String,
    /// 可选项目维度；支持该维度的数据源会由 OpenHub 直接填充。
    #[serde(alias = "project_key")]
    pub(crate) project_key: String,
    pub(crate) timestamp: String,
    #[serde(alias = "total_tokens")]
    pub(crate) total_tokens: i64,
    #[serde(alias = "billable_total_tokens")]
    pub(crate) billable_total_tokens: i64,
    #[serde(alias = "input_tokens")]
    pub(crate) input_tokens: i64,
    #[serde(alias = "cached_input_tokens")]
    pub(crate) cached_input_tokens: i64,
    #[serde(alias = "cache_creation_input_tokens")]
    pub(crate) cache_creation_input_tokens: i64,
    #[serde(alias = "output_tokens")]
    pub(crate) output_tokens: i64,
    #[serde(alias = "reasoning_output_tokens")]
    pub(crate) reasoning_output_tokens: i64,
    #[serde(alias = "conversation_count")]
    pub(crate) conversation_count: i64,
    /// 桶内真实 API 请求数（一次模型调用 = 一次请求，含子代理与工具循环触发）。
    /// 由采集器的用量事件逐条计数；旧快照可能缺失，前端需按 0 兜底。
    #[serde(alias = "request_count")]
    pub(crate) request_count: i64,
    /// 数据源明确上报的成本；未上报时为 0。
    #[serde(alias = "cost_usd")]
    pub(crate) cost_usd: f64,
    /// 当前桶是否包含可信的来源上报成本。
    #[serde(alias = "pricing_available")]
    pub(crate) pricing_available: bool,
    /// 其中有多少 Token 是根据本地可见会话上下文估算，而非来源直接上报。
    #[serde(alias = "estimated_tokens")]
    pub(crate) estimated_tokens: i64,
    /// 输入 Token 中来自“无缓存字段来源”的估算部分；用于区分真实 0% 命中与无缓存数据。
    #[serde(alias = "estimated_input_tokens")]
    pub(crate) estimated_input_tokens: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct TokenUsageReport {
    pub(crate) available: bool,
    pub(crate) buckets: Vec<TokenUsageBucket>,
    #[serde(alias = "start_date")]
    pub(crate) start_date: String,
    #[serde(alias = "end_date")]
    pub(crate) end_date: String,
    /// 成本数据来源（当前为来源上报，未上报则不估算）。
    #[serde(alias = "pricing_source")]
    pub(crate) pricing_source: String,
}

// —— OpenHub 本地 Token 采集状态 ——
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct TokenCollectorSyncReport {
    pub(crate) available: bool,
    pub(crate) changed: bool,
    pub(crate) skipped: bool,
    #[serde(alias = "elapsed_ms")]
    pub(crate) elapsed_ms: i64,
    #[serde(alias = "updated_at")]
    pub(crate) updated_at: String,
    pub(crate) message: String,
}

// —— 请求/对话活动：多工具直读原始日志后的小时桶 ——
// 对话 = 用户发起 turns；请求 = 模型 API 调用；success/failed 仅为可观测样本
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(
    default,
    rename_all(serialize = "camelCase", deserialize = "snake_case")
)]
pub(crate) struct RequestHealthBucket {
    pub(crate) hour: String,
    /// 用户发起的对话 turns（排除 tool_result / 自动触发）
    pub(crate) dialogues: i64,
    /// 提取到的真实 API 请求数（多工具）
    pub(crate) requests: i64,
    /// 可观测成功样本
    pub(crate) success: i64,
    /// 可观测失败样本
    pub(crate) failed: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(
    default,
    rename_all(serialize = "camelCase", deserialize = "snake_case")
)]
pub(crate) struct RequestHealthSourceSummary {
    pub(crate) source: String,
    pub(crate) dialogues: i64,
    pub(crate) requests: i64,
    pub(crate) success: i64,
    pub(crate) failed: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(
    default,
    rename_all(serialize = "camelCase", deserialize = "snake_case")
)]
pub(crate) struct RequestHealthReport {
    pub(crate) available: bool,
    pub(crate) buckets: Vec<RequestHealthBucket>,
    /// 分工具汇总（便于对账；UI 可先不展示）
    pub(crate) by_source: Vec<RequestHealthSourceSummary>,
}

// —— 原始日志解析：会话 / 对话 / 请求 三级 ——
// 会话 = 会话文件（Claude jsonl / Codex rollout）
// 对话 = 每次用户提问轮（一个 user 消息 + 其后 assistant 消息）
// 请求 = 每条消息（user 或 assistant）

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(
    default,
    rename_all(serialize = "camelCase", deserialize = "snake_case")
)]
pub(crate) struct RawSession {
    pub(crate) id: String,
    pub(crate) source: String,
    pub(crate) project: String,
    pub(crate) started_at: String,
    pub(crate) ended_at: String,
    pub(crate) message_count: i64,
    pub(crate) conversation_count: i64,
    pub(crate) model: String,
    pub(crate) total_tokens: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(
    default,
    rename_all(serialize = "camelCase", deserialize = "snake_case")
)]
pub(crate) struct RawConversation {
    pub(crate) id: String,
    pub(crate) session_id: String,
    pub(crate) source: String,
    pub(crate) project: String,
    pub(crate) index: i64,
    pub(crate) started_at: String,
    pub(crate) ended_at: String,
    pub(crate) request_count: i64,
    pub(crate) model: String,
    pub(crate) total_tokens: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(
    default,
    rename_all(serialize = "camelCase", deserialize = "snake_case")
)]
pub(crate) struct RawRequest {
    pub(crate) id: String,
    pub(crate) session_id: String,
    pub(crate) conversation_id: String,
    pub(crate) source: String,
    pub(crate) timestamp: String,
    pub(crate) role: String,
    pub(crate) model: String,
    pub(crate) input_tokens: i64,
    pub(crate) cache_read_tokens: i64,
    pub(crate) cache_creation_tokens: i64,
    pub(crate) output_tokens: i64,
    pub(crate) total_tokens: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(
    default,
    rename_all(serialize = "camelCase", deserialize = "snake_case")
)]
pub(crate) struct RawLogReport {
    pub(crate) available: bool,
    pub(crate) sessions: Vec<RawSession>,
    pub(crate) conversations: Vec<RawConversation>,
    pub(crate) requests: Vec<RawRequest>,
}

// —— 本地 AI Agent 路径诊断：展示各工具配置 / 数据 / 数据库的根目录 ——
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct LocalAgentPathEntry {
    /// 配置 / 数据 / 日志 / 数据库
    pub(crate) kind: String,
    /// 展示用简短说明（如「配置 config.toml」）
    pub(crate) label: String,
    pub(crate) path: String,
    pub(crate) exists: bool,
    /// 附加信息：文件大小（如 38 MB）或目录直属条目数（如 12 项）。
    pub(crate) detail: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct LocalAgentPaths {
    pub(crate) source: String,
    pub(crate) name: String,
    /// 该 Agent 的首选根目录
    pub(crate) root: String,
    /// 是否检测到至少一个路径（或根目录存在）
    pub(crate) detected: bool,
    pub(crate) paths: Vec<LocalAgentPathEntry>,
    /// 最近一次采集中该来源的会话数 / 用量事件数；0 表示尚未采到数据。
    pub(crate) collected_sessions: usize,
    pub(crate) collected_events: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct LocalAgentEnvOverride {
    /// 生效中的重定向环境变量名（如 CLAUDE_CONFIG_DIR）
    pub(crate) key: String,
    pub(crate) value: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct LocalAgentPathsReport {
    pub(crate) available: bool,
    pub(crate) home: String,
    pub(crate) agents: Vec<LocalAgentPaths>,
    /// 当前生效的路径重定向环境变量。
    pub(crate) env_overrides: Vec<LocalAgentEnvOverride>,
    /// 采集缓存的最近更新时间（ISO），空表示尚无采集缓存。
    pub(crate) collected_at: String,
}
