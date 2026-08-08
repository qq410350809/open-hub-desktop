use crate::chrome_session;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashSet;
use tauri::Emitter;

pub(crate) const SEED_JSON: &str = include_str!("../resources/sites.json");
pub(crate) const NOW_SQL: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";
pub(crate) const REMOTE_ROOT_URL: &str = "https://ldoh.105117.xyz/";
pub(crate) const REMOTE_USER_URL: &str = "https://ldoh.105117.xyz/api/ld/user";
pub(crate) const REMOTE_SITES_URL: &str = "https://ldoh.105117.xyz/api/sites";
pub(crate) const REMOTE_SESSION_COOKIE: &str = "ld_auth_session";
pub(crate) const NETWORK_PROXY_KEY: &str = "network_proxy";
pub(crate) const ACTIVE_PROXY_NODE_KEY: &str = "active_proxy_node";
pub(crate) const PROXY_IGNORE_KEY: &str = "proxy_ignore_addresses";
pub(crate) const PROXY_SPEED_TEST_URL_KEY: &str = "proxy_speed_test_url";
pub(crate) const PROXY_RUNTIME_URL: &str = "http://127.0.0.1:17890";
pub(crate) const DEFAULT_PROXY_IGNORE: &str =
    "localhost,127.0.0.1,::1,.local,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16";
pub(crate) const DEFAULT_PROXY_SPEED_TEST_URL: &str = "http://www.gstatic.com/generate_204";
pub(crate) const ZERO_V_ZERO_CONSOLE_URL: &str = "https://0v0.club/";

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
    pub(crate) error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SiteModelCache {
    pub(crate) models: Vec<SiteModelItem>,
    pub(crate) api_source: String,
    pub(crate) accounts: Vec<SiteModelCacheAccount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SiteModelItem {
    pub(crate) id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) owned_by: Option<String>,
}

#[derive(Debug, Default)]
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
    pub(crate) country_code: String,
    pub(crate) country_name: String,
    pub(crate) classification: String,
    pub(crate) primary_ip: String,
    pub(crate) updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct ProxyPoolState {
    pub(crate) subscriptions: Vec<ProxySubscription>,
    pub(crate) nodes: Vec<ProxyNode>,
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

// —— Token 统计（数据来源：tokentracker CLI，见 token_stats.rs）——
// CLI `sessions` 输出键为 snake_case，而 app 前端约定 camelCase，
// 因此这里序列化走 camelCase、反序列化走 snake_case 双向映射。

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub(crate) struct TokenStatsReport {
    pub(crate) available: bool,
    pub(crate) sessions: Vec<TokenSession>,
    pub(crate) session_count: i64,
    pub(crate) summary: TokenSummary,
    pub(crate) by_model: Vec<TokenModelStat>,
    pub(crate) subagents: Vec<TokenSubagentStat>,
    pub(crate) provenance: JsonValue,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub(crate) struct TokenSessionTokens {
    pub(crate) input_tokens: i64,
    pub(crate) cached_input_tokens: i64,
    pub(crate) cache_creation_input_tokens: i64,
    pub(crate) output_tokens: i64,
    pub(crate) reasoning_output_tokens: i64,
    pub(crate) total_tokens: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub(crate) struct TokenSession {
    pub(crate) version: i64,
    pub(crate) session_hash: String,
    pub(crate) source: String,
    pub(crate) project_key: String,
    pub(crate) model: String,
    pub(crate) started_at: String,
    pub(crate) ended_at: String,
    pub(crate) active_ms: i64,
    pub(crate) turns: i64,
    pub(crate) edit_turns: i64,
    pub(crate) retry_turns: i64,
    pub(crate) subagent_calls: i64,
    pub(crate) subagent_types: JsonValue,
    pub(crate) tokens: TokenSessionTokens,
    pub(crate) provenance: JsonValue,
    pub(crate) duration_ms: i64,
    pub(crate) total_tokens: i64,
    pub(crate) cost_usd: f64,
    pub(crate) productive: bool,
    pub(crate) first_pass: bool,
    pub(crate) one_shot: bool,
    pub(crate) tokens_per_edit: Option<f64>,
    pub(crate) cost_per_edit: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub(crate) struct TokenSummary {
    pub(crate) sessions: i64,
    pub(crate) productive_sessions: i64,
    pub(crate) one_shot_sessions: i64,
    pub(crate) edit_turns: i64,
    pub(crate) retries: i64,
    pub(crate) total_tokens: i64,
    pub(crate) cost_usd: f64,
    pub(crate) edit_tokens: i64,
    pub(crate) edit_cost_usd: f64,
    pub(crate) productive_rate: f64,
    pub(crate) one_shot_rate: Option<f64>,
    pub(crate) edit_sessions: i64,
    pub(crate) first_pass_sessions: i64,
    pub(crate) edit_session_rate: f64,
    pub(crate) first_pass_rate: Option<f64>,
    pub(crate) tokens_per_edit: Option<f64>,
    pub(crate) cost_per_edit: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub(crate) struct TokenModelStat {
    pub(crate) model: String,
    pub(crate) sessions: i64,
    pub(crate) productive_sessions: i64,
    pub(crate) one_shot_sessions: i64,
    pub(crate) edit_turns: i64,
    pub(crate) retries: i64,
    pub(crate) total_tokens: i64,
    pub(crate) cost_usd: f64,
    pub(crate) edit_tokens: i64,
    pub(crate) edit_cost_usd: f64,
    pub(crate) productive_rate: f64,
    pub(crate) one_shot_rate: Option<f64>,
    pub(crate) edit_sessions: i64,
    pub(crate) first_pass_sessions: i64,
    pub(crate) edit_session_rate: f64,
    pub(crate) first_pass_rate: Option<f64>,
    pub(crate) tokens_per_edit: Option<f64>,
    pub(crate) cost_per_edit: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub(crate) struct TokenSubagentStat {
    pub(crate) name: String,
    pub(crate) calls: i64,
    pub(crate) sessions: i64,
    pub(crate) total_tokens: f64,
    pub(crate) cost_usd: f64,
}

// —— Token 用量小时桶（数据来源：~/.tokentracker/tracker/cursors.json 的 hourly.buckets）——
// tokentracker 仪表盘的汇总数据来自这里（覆盖所有工具的每小时用量），
// 而 `sessions` 子命令只聚合 Claude/Codex 会话日志。

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub(crate) struct TokenUsageBucket {
    pub(crate) source: String,
    pub(crate) model: String,
    pub(crate) timestamp: String,
    pub(crate) total_tokens: i64,
    pub(crate) billable_total_tokens: i64,
    pub(crate) input_tokens: i64,
    pub(crate) cached_input_tokens: i64,
    pub(crate) cache_creation_input_tokens: i64,
    pub(crate) output_tokens: i64,
    pub(crate) reasoning_output_tokens: i64,
    pub(crate) conversation_count: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub(crate) struct TokenUsageReport {
    pub(crate) available: bool,
    pub(crate) buckets: Vec<TokenUsageBucket>,
    pub(crate) start_date: String,
    pub(crate) end_date: String,
}


// —— 请求健康：大模型请求成功/失败计数（来自 Codex rollout 的 task_started/task_complete.error）——
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub(crate) struct RequestHealthBucket {
    pub(crate) hour: String,
    pub(crate) success: i64,
    pub(crate) failed: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub(crate) struct RequestHealthReport {
    pub(crate) available: bool,
    pub(crate) buckets: Vec<RequestHealthBucket>,
}

// —— 原始日志解析：会话 / 对话 / 请求 三级 ——
// 会话 = 会话文件（Claude jsonl / Codex rollout）
// 对话 = 每次用户提问轮（一个 user 消息 + 其后 assistant 消息）
// 请求 = 每条消息（user 或 assistant）

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all(serialize = "camelCase", deserialize = "snake_case"))]
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
#[serde(default, rename_all(serialize = "camelCase", deserialize = "snake_case"))]
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
#[serde(default, rename_all(serialize = "camelCase", deserialize = "snake_case"))]
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
#[serde(default, rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub(crate) struct RawLogReport {
    pub(crate) available: bool,
    pub(crate) sessions: Vec<RawSession>,
    pub(crate) conversations: Vec<RawConversation>,
    pub(crate) requests: Vec<RawRequest>,
}
