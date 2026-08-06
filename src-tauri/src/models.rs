use crate::chrome_session;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
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
pub(crate) const DEFAULT_PROXY_SPEED_TEST_URL: &str = "https://cp.cloudflare.com/generate_204";
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
