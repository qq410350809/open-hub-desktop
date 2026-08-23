//! 统一 HTTP 服务层：桌面轻量模式与独立 server 二进制共用同一套路由。
//!
//! 路由：
//! - GET  /*           静态构建产物 + SPA fallback
//! - POST /api/rpc     命令分发（与桌面 IPC 同一套命令名）
//! - GET  /api/events  SSE 事件流（EventBus 广播订阅）
//! - GET  /api/caps    能力协商（本机功能可用性）

use crate::context::{AppContext, Managed};
#[cfg(not(feature = "desktop"))]
use crate::context::LocalRef;
use crate::model::gateway::ModelProxyState;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures_util::Stream;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// 首选端口；被占用时向后顺延。
pub const DEFAULT_PORT: u16 = 17896;
const PORT_TRIES: u16 = 24;
/// app_meta 中轻量模式的持久化键。
pub const LIGHTWEIGHT_META_KEY: &str = "lightweight_mode";

// ---------------------------------------------------------------------------
// 共享服务状态
// ---------------------------------------------------------------------------

/// HTTP 服务共享状态：桌面轻量模式与独立 server 二进制共用。
/// `gateway` 保持裸值（非 Arc），与命令签名 `Managed<'_, ModelProxyState>` 对齐。
pub struct ServerShared {
    pub ctx: Arc<AppContext>,
    /// 独立 server 形态持有网关状态；桌面端由 Tauri TypeMap 注入，不重复持有。
    #[cfg(not(feature = "desktop"))]
    pub gateway: ModelProxyState,
    /// 前端静态资源目录（dist）。
    pub dist_dir: PathBuf,
    /// 访问令牌；为空表示不启用鉴权（仅限本机回环场景）。
    pub token: String,
    pub running: AtomicBool,
    pub port: AtomicU16,
    /// 桌面端持有的应用句柄（窗口管理 / TypeMap 状态注入）。
    #[cfg(feature = "desktop")]
    pub app: tauri::AppHandle,
}

impl ServerShared {
    pub fn new(
        ctx: Arc<AppContext>,
        #[cfg(not(feature = "desktop"))] gateway: ModelProxyState,
        dist_dir: PathBuf,
        token: String,
        #[cfg(feature = "desktop")] app: tauri::AppHandle,
    ) -> Arc<Self> {
        Arc::new(Self {
            ctx,
            #[cfg(not(feature = "desktop"))]
            gateway,
            dist_dir,
            token,
            running: AtomicBool::new(false),
            port: AtomicU16::new(0),
            #[cfg(feature = "desktop")]
            app,
        })
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub fn current_port(&self) -> u16 {
        self.port.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// 托管引用构造：双形态各自的 Managed 来源
// ---------------------------------------------------------------------------

#[cfg(feature = "desktop")]
fn ctx_managed(shared: &ServerShared) -> Managed<'_, Arc<AppContext>> {
    use tauri::Manager;
    shared.app.state::<Arc<AppContext>>()
}

#[cfg(not(feature = "desktop"))]
fn ctx_managed(shared: &ServerShared) -> Managed<'_, Arc<AppContext>> {
    LocalRef(&shared.ctx)
}

#[cfg(feature = "desktop")]
fn gw_managed(shared: &ServerShared) -> Managed<'_, ModelProxyState> {
    use tauri::Manager;
    shared.app.state::<ModelProxyState>()
}

#[cfg(not(feature = "desktop"))]
fn gw_managed(shared: &ServerShared) -> Managed<'_, ModelProxyState> {
    LocalRef(&shared.gateway)
}

/// 轻量模式命令所需的 ServerShared 托管引用（桌面端由 TypeMap 注入）。
#[cfg(feature = "desktop")]
fn st_managed(shared: &ServerShared) -> Managed<'_, Arc<ServerShared>> {
    use tauri::Manager;
    shared.app.state::<Arc<ServerShared>>()
}

// ---------------------------------------------------------------------------
// 鉴权
// ---------------------------------------------------------------------------

/// 从请求头提取访问令牌：X-OpenHub-Token 或 Authorization: Bearer。
fn extract_token(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers.get("x-openhub-token").and_then(|v| v.to_str().ok()) {
        return Some(value.to_string());
    }
    if let Some(value) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(bearer) = value.strip_prefix("Bearer ") {
            return Some(bearer.to_string());
        }
    }
    None
}

/// 统一鉴权判定：服务静态令牌或有效登录会话任一命中即通过；
/// 未配置服务令牌时仍要求会话（登录门禁默认开启）。
fn token_ok(shared: &ServerShared, headers: &HeaderMap) -> bool {
    match extract_token(headers) {
        Some(token) => access_allowed(shared, &token),
        None => false,
    }
}

fn access_allowed(shared: &ServerShared, token: &str) -> bool {
    if !shared.token.is_empty() && token == shared.token {
        return true;
    }
    shared.ctx.login.validate_session(token)
}

// ---------------------------------------------------------------------------
// RPC 分发（阶段 3 逐模块补全命令表）
// ---------------------------------------------------------------------------

/// RPC 命令表：与桌面 IPC 同一套命令名。
/// 保持与桌面 invoke 相同的信封协议——命令 Result 直接序列化为 {"Ok":..}/{"Err":..}，
/// 由前端 unwrapResult 统一解包；参数提取错误走 {"error":..} 通道。
macro_rules! rpc_arms {
    ($ctx:expr, $gw:expr, $command:expr, $args:expr) => {
        match $command {
            // —— 站点库 ——
            "list_library" => Ok(json!(crate::site::library::list_library($ctx))),
            "create_site" => {
                let input: crate::models::SiteRecord = take($args, &["input"])?;
                Ok(json!(crate::site::library::create_site($ctx, input)))
            }
            "update_site" => {
                let id: String = take($args, &["id"])?;
                let input: crate::models::SiteRecord = take($args, &["input"])?;
                Ok(json!(crate::site::library::update_site($ctx, id, input)))
            }
            "delete_site" => {
                let id: String = take($args, &["id"])?;
                Ok(json!(crate::site::library::delete_site($ctx, id)))
            }
            "toggle_personal" => {
                let id: String = take($args, &["id"])?;
                Ok(json!(crate::site::library::toggle_personal($ctx, id)))
            }
            "toggle_pending" => {
                let id: String = take($args, &["id"])?;
                Ok(json!(crate::site::library::toggle_pending($ctx, id)))
            }
            "cycle_usage_state" => {
                let id: String = take($args, &["id"])?;
                Ok(json!(crate::site::library::cycle_usage_state($ctx, id)))
            }
            "set_usage_state" => {
                let id: String = take($args, &["id"])?;
                let state: String = take($args, &["state"])?;
                Ok(json!(crate::site::library::set_usage_state($ctx, id, state)))
            }
            "toggle_hidden" => {
                let id: String = take($args, &["id"])?;
                Ok(json!(crate::site::library::toggle_hidden($ctx, id)))
            }
            "toggle_runaway" => {
                let id: String = take($args, &["id"])?;
                Ok(json!(crate::site::library::toggle_runaway($ctx, id)))
            }
            "import_site" => {
                let site_url: String = take($args, &["siteUrl", "site_url"])?;
                let usage_state: Option<String> = take_opt($args, &["usageState", "usage_state"])?;
                Ok(json!(
                    crate::site::library::import_site($ctx, site_url, usage_state).await
                ))
            }

            // —— 站点同步 / 远端 ——
            "mark_sites_with_chrome_sessions" => {
                let site_id: Option<String> = take_opt($args, &["siteId", "site_id"])?;
                let site_ids: Option<Vec<String>> = take_opt($args, &["siteIds", "site_ids"])?;
                let run_id: Option<u64> = take_opt($args, &["runId", "run_id"])?;
                let extract_only: Option<bool> = take_opt($args, &["extractOnly", "extract_only"])?;
                let refresh_pending: Option<bool> =
                    take_opt($args, &["refreshPending", "refresh_pending"])?;
                Ok(json!(crate::site::sync::mark_sites_with_chrome_sessions(
                    $ctx, site_id, site_ids, run_id, extract_only, refresh_pending
                )
                .await))
            }
            "delete_site_account" => {
                let site_id: String = take($args, &["siteId", "site_id"])?;
                let profile_id: String = take($args, &["profileId", "profile_id"])?;
                Ok(json!(crate::site::sync::delete_site_account($ctx, site_id, profile_id)))
            }
            "sync_site_account_via_chrome" => {
                let site_id: String = take($args, &["siteId", "site_id"])?;
                let profile_id: String = take($args, &["profileId", "profile_id"])?;
                let run_id: u64 = take($args, &["runId", "run_id"])?;
                Ok(json!(crate::site::sync::sync_site_account_via_chrome(
                    $ctx, site_id, profile_id, run_id
                )
                .await))
            }
            "get_remote_user" => Ok(json!(crate::site::library::get_remote_user($ctx).await)),
            "sync_remote_sites" => {
                let runaway: Option<bool> = take_opt($args, &["runaway"])?;
                let run_id: u64 = take($args, &["runId", "run_id"])?;
                Ok(json!(crate::site::library::sync_remote_sites($ctx, runaway, run_id).await))
            }
            "detect_site_system_types" => {
                let site_ids: Vec<String> = take($args, &["siteIds", "site_ids"])?;
                let run_id: u64 = take($args, &["runId", "run_id"])?;
                Ok(json!(crate::site::library::detect_site_system_types($ctx, site_ids, run_id).await))
            }

            // —— Chrome 会话（无状态依赖） ——
            "list_chrome_sessions" => {
                let url: String = take($args, &["url"])?;
                Ok(json!(crate::site::sync::list_chrome_sessions(url).await))
            }
            "read_chrome_session" => {
                let url: String = take($args, &["url"])?;
                let profile_id: String = take($args, &["profileId", "profile_id"])?;
                Ok(json!(crate::site::sync::read_chrome_session(url, profile_id).await))
            }
            "open_url_in_chrome_profile" => {
                let url: String = take($args, &["url"])?;
                let profile_id: String = take($args, &["profileId", "profile_id"])?;
                Ok(json!(crate::site::sync::open_url_in_chrome_profile(url, profile_id).await))
            }
            "close_chrome_sync_tabs" => {
                Ok(json!(crate::site::sync::close_chrome_sync_tabs().await))
            }

            // —— 代理池 ——
            "get_proxy_pool_state" => Ok(json!(crate::proxypool::get_proxy_pool_state($ctx))),
            "analyze_proxy_nodes" => Ok(json!(crate::proxypool::analyze_proxy_nodes($ctx))),
            "save_proxy_subscription" => {
                let id: Option<String> = take_opt($args, &["id"])?;
                let name: String = take($args, &["name"])?;
                let url: String = take($args, &["url"])?;
                Ok(json!(crate::proxypool::save_proxy_subscription($ctx, id, name, url)))
            }
            "delete_proxy_subscription" => {
                let id: String = take($args, &["id"])?;
                Ok(json!(crate::proxypool::delete_proxy_subscription($ctx, id)))
            }
            "refresh_proxy_subscription" => {
                let id: String = take($args, &["id"])?;
                Ok(json!(crate::proxypool::refresh_proxy_subscription($ctx, id).await))
            }
            "set_proxy_pool_settings" => {
                let ignore_addresses: String =
                    take($args, &["ignoreAddresses", "ignore_addresses"])?;
                Ok(json!(crate::proxypool::set_proxy_pool_settings($ctx, ignore_addresses)))
            }
            "save_proxy_channel" => {
                let id: Option<String> = take_opt($args, &["id"])?;
                let name: String = take($args, &["name"])?;
                Ok(json!(crate::proxypool::save_proxy_channel($ctx, id, name)))
            }
            "delete_proxy_channel" => {
                let id: String = take($args, &["id"])?;
                Ok(json!(crate::proxypool::delete_proxy_channel($ctx, id)))
            }
            "set_proxy_channel_node" => {
                let channel_id: String = take($args, &["channelId", "channel_id"])?;
                let node_id: String = take($args, &["nodeId", "node_id"])?;
                Ok(json!(crate::proxypool::set_proxy_channel_node($ctx, channel_id, node_id).await))
            }
            "assign_account_proxy_channel" => {
                let profile_id: String = take($args, &["profileId", "profile_id"])?;
                let channel_id: String = take($args, &["channelId", "channel_id"])?;
                Ok(json!(crate::proxypool::assign_account_proxy_channel(
                    $ctx, profile_id, channel_id
                )))
            }
            "unassign_account_proxy_channel" => {
                let profile_id: String = take($args, &["profileId", "profile_id"])?;
                Ok(json!(crate::proxypool::unassign_account_proxy_channel($ctx, profile_id)))
            }
            "test_proxy_channel_nodes" => {
                let channel_id: Option<String> = take_opt($args, &["channelId", "channel_id"]).ok().flatten();
                let node_ids: Option<Vec<String>> =
                    take_opt($args, &["nodeIds", "node_ids"]).ok().flatten();
                Ok(json!(crate::proxypool::test_proxy_channel_nodes(
                    $ctx, channel_id, node_ids
                )
                .await))
            }
            "set_active_proxy_node" => {
                let node_id: String = take($args, &["nodeId", "node_id"])?;
                Ok(json!(crate::proxypool::set_active_proxy_node($ctx, node_id).await))
            }
            "clear_active_proxy_node" => {
                Ok(json!(crate::proxypool::clear_active_proxy_node($ctx)))
            }
            "delete_invalid_proxy_nodes" => {
                Ok(json!(crate::proxypool::delete_invalid_proxy_nodes($ctx)))
            }
            "test_proxy_node" => {
                let node_id: String = take($args, &["nodeId", "node_id"])?;
                Ok(json!(crate::proxypool::test_proxy_node($ctx, node_id).await))
            }
            "test_proxy_nodes" => {
                let node_ids: Vec<String> = take($args, &["nodeIds", "node_ids"])?;
                Ok(json!(crate::proxypool::test_proxy_nodes($ctx, node_ids).await))
            }
            "test_all_proxy_nodes" => {
                Ok(json!(crate::proxypool::test_all_proxy_nodes($ctx).await))
            }
            "cancel_proxy_node_tests" => {
                Ok(json!(crate::proxypool::cancel_proxy_node_tests($ctx)))
            }

            // —— 模型目录 / 模型缓存 ——
            "get_system_fonts" => Ok(json!(crate::model::catalog::get_system_fonts())),
            "get_site_model_cache" => {
                let site_id: String = take($args, &["siteId", "site_id"])?;
                Ok(json!(crate::model::catalog::get_site_model_cache($ctx, site_id)))
            }
            "get_all_site_model_caches" => {
                Ok(json!(crate::model::catalog::get_all_site_model_caches($ctx)))
            }
            "clear_site_model_cache_for_site" => {
                let site_id: String = take($args, &["siteId", "site_id"])?;
                Ok(json!(crate::model::catalog::clear_site_model_cache_for_site($ctx, site_id)))
            }
            "save_site_model_cache_for_account" => {
                let site_id: String = take($args, &["siteId", "site_id"])?;
                let account: crate::models::SiteModelCacheAccount = take($args, &["account"])?;
                let result: Option<crate::model::catalog::SiteModelsResult> =
                    take_opt($args, &["result"])?;
                let preserve_keys: Option<bool> =
                    take_opt($args, &["preserveKeys", "preserve_keys"])?;
                Ok(json!(crate::model::catalog::save_site_model_cache_for_account(
                    $ctx, site_id, account, result, preserve_keys
                )))
            }
            "get_model_catalog" => Ok(json!(crate::model::catalog::get_model_catalog($ctx))),
            "get_model_catalog_detail" => {
                let canonical_key: Option<String> =
                    take_opt($args, &["canonicalKey", "canonical_key"])?;
                let id: Option<String> = take_opt($args, &["id"])?;
                Ok(json!(crate::model::catalog::get_model_catalog_detail(
                    $ctx, canonical_key, id
                )
                .await))
            }
            "sync_model_catalog" => {
                let force: Option<bool> = take_opt($args, &["force"])?;
                Ok(json!(crate::model::catalog::sync_model_catalog($ctx, force).await))
            }
            "fetch_site_models_json" => {
                let url: String = take($args, &["url"])?;
                let site_id: Option<String> = take_opt($args, &["siteId", "site_id"])?;
                let profile_id: Option<String> = take_opt($args, &["profileId", "profile_id"])?;
                Ok(json!(crate::model::catalog::fetch_site_models_json(
                    $ctx, url, site_id, profile_id
                )
                .await))
            }

            // —— 公益监听 ——
            "get_charity_feed" => {
                let feed_id: Option<String> = take_opt($args, &["feedId", "feed_id"])?;
                let offset: Option<usize> = take_opt($args, &["offset"])?;
                let limit: Option<usize> = take_opt($args, &["limit"])?;
                let keyword: Option<String> = take_opt($args, &["keyword"])?;
                Ok(json!(crate::charity::get_charity_feed($ctx, feed_id, offset, limit, keyword).await))
            }
            "mark_charity_feed_read" => {
                let feed_id: Option<String> = take_opt($args, &["feedId", "feed_id"])?;
                Ok(json!(crate::charity::mark_charity_feed_read($ctx, feed_id).await))
            }
            "get_charity_today_count" => {
                Ok(json!(crate::charity::get_charity_today_count($ctx).await))
            }
            "get_charity_unread_total" => {
                Ok(json!(crate::charity::get_charity_unread_total($ctx).await))
            }
            "fetch_charity_feed" => {
                let feed_id: Option<String> = take_opt($args, &["feedId", "feed_id"])?;
                Ok(json!(crate::charity::fetch_charity_feed($ctx, feed_id).await))
            }
            "get_charity_proxy_pool_summary" => {
                Ok(json!(crate::charity::get_charity_proxy_pool_summary($ctx).await))
            }
            "get_charity_sync_logs" => {
                let limit: Option<usize> = take_opt($args, &["limit"])?;
                Ok(json!(crate::charity::get_charity_sync_logs($ctx, limit).await))
            }
            "clear_charity_sync_logs" => {
                Ok(json!(crate::charity::clear_charity_sync_logs($ctx).await))
            }
            "set_charity_monitor_visible" => {
                let visible: bool = take($args, &["visible"])?;
                Ok(json!(crate::charity::set_charity_monitor_visible($ctx, visible)))
            }
            "request_charity_round" => Ok(json!(crate::charity::request_charity_round($ctx))),
            "refresh_all_charity_feeds" => {
                Ok(json!(crate::charity::refresh_all_charity_feeds($ctx).await))
            }
            "list_charity_sources" => {
                Ok(json!(crate::charity::list_charity_sources($ctx).await))
            }
            "add_charity_source" => {
                let id: String = take($args, &["id"])?;
                let name: String = take($args, &["name"])?;
                let json_url: Option<String> = take_opt($args, &["jsonUrl", "json_url"])?;
                Ok(json!(crate::charity::add_charity_source($ctx, id, name, json_url).await))
            }
            "update_charity_source" => {
                let id: String = take($args, &["id"])?;
                let name: Option<String> = take_opt($args, &["name"])?;
                let json_url: Option<String> = take_opt($args, &["jsonUrl", "json_url"])?;
                let enabled: Option<bool> = take_opt($args, &["enabled"])?;
                Ok(json!(crate::charity::update_charity_source($ctx, id, name, json_url, enabled).await))
            }
            "remove_charity_source" => {
                let id: String = take($args, &["id"])?;
                Ok(json!(crate::charity::remove_charity_source($ctx, id).await))
            }

            // —— Token 统计 ——
            "get_token_stats" => {
                let from: Option<String> = take_opt($args, &["from"])?;
                let to: Option<String> = take_opt($args, &["to"])?;
                let _refresh: Option<bool> = take_opt($args, &["refresh"]).ok().flatten();
                Ok(json!(crate::token::stats::get_token_stats($ctx, from, to, None).await))
            }
            "sync_token_data" => {
                let force: Option<bool> = take_opt($args, &["force"])?;
                Ok(json!(crate::token::stats::sync_token_data($ctx, force).await))
            }
            "get_token_usage" => Ok(json!(crate::token::stats::get_token_usage($ctx).await)),
            "get_token_raw_logs" => Ok(json!(crate::token::stats::get_token_raw_logs().await)),
            "get_token_request_health" => {
                let _refresh: Option<bool> = take_opt($args, &["refresh"]).ok().flatten();
                Ok(json!(crate::token::stats::get_token_request_health($ctx, None).await))
            }
            "get_local_agent_paths" => {
                Ok(json!(crate::token::stats::get_local_agent_paths().await))
            }

            // —— 内核管理（Mihomo / GeoIP） ——
            "get_mihomo_kernel_status" => {
                Ok(json!(crate::kernel::get_mihomo_kernel_status($ctx).await))
            }
            "check_mihomo_kernel_update" => {
                let mirror: Option<String> = take_opt($args, &["mirror"]).ok().flatten();
                Ok(json!(crate::kernel::check_mihomo_kernel_update($ctx, mirror).await))
            }
            "download_or_update_mihomo_kernel" => {
                let mirror: Option<String> = take_opt($args, &["mirror"]).ok().flatten();
                Ok(json!(crate::kernel::download_or_update_mihomo_kernel($ctx, mirror).await))
            }
            "get_geoip_status" => Ok(json!(crate::kernel::get_geoip_status($ctx).await)),
            "download_or_update_geoip" => {
                let mirror: Option<String> = take_opt($args, &["mirror"]).ok().flatten();
                Ok(json!(crate::kernel::download_or_update_geoip($ctx, mirror).await))
            }

            // —— 模型网关（Model Gateway） ——
            "get_model_proxy_config" => {
                Ok(json!(crate::model::gateway::get_model_proxy_config($ctx).await))
            }
            "save_model_proxy_config_cmd" => {
                let config: crate::model::gateway::ModelProxyConfig = take($args, &["config"])?;
                Ok(json!(crate::model::gateway::save_model_proxy_config_cmd($ctx, $gw, config).await))
            }
            "get_model_proxy_status" => {
                Ok(json!(crate::model::gateway::get_model_proxy_status($gw).await))
            }
            "start_model_proxy" => Ok(json!(crate::model::gateway::start_model_proxy($gw).await)),
            "stop_model_proxy" => Ok(json!(crate::model::gateway::stop_model_proxy($gw).await)),
            "fetch_model_proxy_models" => {
                Ok(json!(crate::model::gateway::fetch_model_proxy_models($gw).await))
            }
            "get_cached_channel_models" => {
                Ok(json!(crate::model::gateway::get_cached_channel_models($gw).await))
            }
            "get_cached_channel_errors" => {
                Ok(json!(crate::model::gateway::get_cached_channel_errors($gw).await))
            }
            "test_model_proxy_health" => {
                Ok(json!(crate::model::gateway::test_model_proxy_health($gw).await))
            }
            "get_model_proxy_logs" => {
                let page: Option<usize> = take_opt($args, &["page"]).ok().flatten();
                let page_size: Option<usize> =
                    take_opt($args, &["pageSize", "page_size"]).ok().flatten();
                let filter: Option<String> = take_opt($args, &["filter"]).ok().flatten();
                let q: Option<String> = take_opt($args, &["q"]).ok().flatten();
                let sort_by: Option<String> = take_opt($args, &["sortBy", "sort_by"]).ok().flatten();
                let sort_order: Option<String> =
                    take_opt($args, &["sortOrder", "sort_order"]).ok().flatten();
                let from: Option<String> = take_opt($args, &["from"]).ok().flatten();
                let to: Option<String> = take_opt($args, &["to"]).ok().flatten();
                Ok(json!(crate::model::gateway::get_model_proxy_logs(
                    $ctx, page, page_size, filter, q, sort_by, sort_order, from, to
                )
                .await))
            }
            "get_model_proxy_channel_stats" => {
                Ok(json!(crate::model::gateway::get_model_proxy_channel_stats($gw).await))
            }
            "get_proxy_token_usage" => {
                let from: Option<String> = take_opt($args, &["from"]).ok().flatten();
                let to: Option<String> = take_opt($args, &["to"]).ok().flatten();
                Ok(json!(crate::model::gateway::get_proxy_token_usage($gw, from, to).await))
            }
            "get_model_proxy_overview_stats" => {
                let days: Option<u32> = take_opt($args, &["days"]).ok().flatten();
                let from: Option<String> = take_opt($args, &["from"]).ok().flatten();
                let to: Option<String> = take_opt($args, &["to"]).ok().flatten();
                Ok(json!(crate::model::gateway::get_model_proxy_overview_stats($gw, days, from, to).await))
            }
            "clear_model_proxy_logs" => {
                let mode: Option<String> = take_opt($args, &["mode"]).ok().flatten();
                let before: Option<String> = take_opt($args, &["before"]).ok().flatten();
                Ok(json!(crate::model::gateway::clear_model_proxy_logs($ctx, mode, before).await))
            }
            "sync_model_proxy_site_channels" => {
                let site_ids: Option<Vec<String>> =
                    take_opt($args, &["siteIds", "site_ids"]).ok().flatten();
                Ok(json!(crate::model::gateway::sync_model_proxy_site_channels($ctx, $gw, site_ids).await))
            }

            // —— OpenCode 代理别名 ——
            "get_opencode_proxy_config" => {
                Ok(json!(crate::model::gateway::get_opencode_proxy_config($ctx).await))
            }
            "save_opencode_proxy_config_cmd" => {
                let config: crate::model::gateway::OpencodeProxyConfig = take($args, &["config"])?;
                Ok(json!(crate::model::gateway::save_opencode_proxy_config_cmd($ctx, $gw, config).await))
            }
            "get_opencode_proxy_status" => {
                Ok(json!(crate::model::gateway::get_opencode_proxy_status($gw).await))
            }
            "start_opencode_proxy" => {
                Ok(json!(crate::model::gateway::start_opencode_proxy($gw).await))
            }
            "stop_opencode_proxy" => {
                Ok(json!(crate::model::gateway::stop_opencode_proxy($gw).await))
            }
            "fetch_opencode_models" => {
                Ok(json!(crate::model::gateway::fetch_opencode_models($gw).await))
            }
            "get_opencode_cached_channel_models" => {
                Ok(json!(crate::model::gateway::get_opencode_cached_channel_models($gw).await))
            }
            "get_opencode_cached_channel_errors" => {
                Ok(json!(crate::model::gateway::get_opencode_cached_channel_errors($gw).await))
            }
            "test_opencode_proxy_health" => {
                Ok(json!(crate::model::gateway::test_opencode_proxy_health($gw).await))
            }
            "get_opencode_proxy_logs" => {
                let page: Option<usize> = take_opt($args, &["page"]).ok().flatten();
                let page_size: Option<usize> =
                    take_opt($args, &["pageSize", "page_size"]).ok().flatten();
                let filter: Option<String> = take_opt($args, &["filter"]).ok().flatten();
                let q: Option<String> = take_opt($args, &["q"]).ok().flatten();
                let sort_by: Option<String> = take_opt($args, &["sortBy", "sort_by"]).ok().flatten();
                let sort_order: Option<String> =
                    take_opt($args, &["sortOrder", "sort_order"]).ok().flatten();
                let from: Option<String> = take_opt($args, &["from"]).ok().flatten();
                let to: Option<String> = take_opt($args, &["to"]).ok().flatten();
                Ok(json!(crate::model::gateway::get_opencode_proxy_logs(
                    $ctx, page, page_size, filter, q, sort_by, sort_order, from, to
                )
                .await))
            }
            "get_opencode_channel_stats" => {
                Ok(json!(crate::model::gateway::get_opencode_channel_stats($gw).await))
            }
            "clear_opencode_proxy_logs" => {
                let mode: Option<String> = take_opt($args, &["mode"]).ok().flatten();
                let before: Option<String> = take_opt($args, &["before"]).ok().flatten();
                Ok(json!(crate::model::gateway::clear_opencode_proxy_logs($ctx, mode, before).await))
            }
            "sync_opencode_site_channels" => {
                let site_ids: Option<Vec<String>> =
                    take_opt($args, &["siteIds", "site_ids"]).ok().flatten();
                Ok(json!(crate::model::gateway::sync_opencode_site_channels($ctx, $gw, site_ids).await))
            }

            // —— 登录会话 ——
            "get_login_state" => {
                let token: Option<String> = take_opt($args, &["token"]).ok().flatten();
                Ok(json!(get_login_state($ctx, token)))
            }
            "login" => {
                let username: String = take($args, &["username", "user"])?;
                let password: String = take($args, &["password"])?;
                Ok(json!(login($ctx, username, password)))
            }
            "logout" => {
                let token: Option<String> = take_opt($args, &["token"]).ok().flatten();
                Ok(json!(logout($ctx, token)))
            }

            _ => Err(format!("暂不支持的命令：{}", $command)),
        }
    };
}

async fn dispatch(shared: &ServerShared, command: &str, args: &Value) -> Result<Value, String> {
    // —— 桌面专属命令 ——
    #[cfg(feature = "desktop")]
    match command {
        "get_lightweight_mode_state" => return Ok(json!(get_lightweight_mode_state(st_managed(shared)))),
        "enter_lightweight_mode" => return Ok(json!(enter_lightweight_mode(st_managed(shared)))),
        "show_main_window" => return Ok(json!(show_main_window(st_managed(shared)))),
        "save_export_file" => {
            let payload: crate::core::file_export::SaveFileArgs = take(args, &["args"])?;
            return Ok(json!(crate::core::file_export::save_export_file(payload).await));
        }
        _ => {}
    }
    #[cfg(not(feature = "desktop"))]
    if matches!(
        command,
        "get_lightweight_mode_state"
            | "enter_lightweight_mode"
            | "show_main_window"
            | "save_export_file"
    ) {
        return Err("此命令仅在桌面形态下可用".to_string());
    }

    // —— 共享命令表 ——
    let ctx = ctx_managed(shared);
    // 网关状态在阶段 3d 接入后启用。
    #[allow(unused_variables)]
    let gw = gw_managed(shared);
    rpc_arms!(ctx, gw, command, args)
}

async fn rpc_handler(
    State(shared): State<Arc<ServerShared>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({ "error": format!("请求体解析失败：{error}") })),
            )
                .into_response()
        }
    };
    let command = payload
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    // 登录握手命令免预置令牌：门禁本身依赖它们完成认证闭环。
    const AUTH_FREE_COMMANDS: &[&str] = &["get_login_state", "login", "logout"];
    if !AUTH_FREE_COMMANDS.contains(&command.as_str()) && !token_ok(&shared, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({ "error": "访问令牌无效或缺失，请从设置页复制完整地址访问" })),
        )
            .into_response();
    }
    let args = payload.get("args").cloned().unwrap_or(Value::Null);
    let result = dispatch(&shared, &command, &args).await;
    let body = match result {
        Ok(data) => json!({ "data": data }),
        Err(error) => json!({ "error": error }),
    };
    ([(header::CACHE_CONTROL, "no-store")], axum::Json(body)).into_response()
}

// ---------------------------------------------------------------------------
// SSE 事件流
// ---------------------------------------------------------------------------

async fn events_handler(
    State(shared): State<Arc<ServerShared>>,
    headers: HeaderMap,
    query: axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, StatusCode> {
    // EventSource 无法携带自定义请求头：允许 ?token= 查询参数兜底。
    let header_ok = token_ok(&shared, &headers);
    let query_ok = query
        .get("token")
        .map(|value| access_allowed(&shared, value))
        .unwrap_or(false);
    if !header_ok && !query_ok {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let rx = shared.ctx.event_bus.subscribe();
    let stream = async_stream::stream! {
        let mut rx = rx;
        loop {
            match rx.recv().await {
                Ok(message) => {
                    let data = serde_json::to_string(&message).unwrap_or_default();
                    yield Ok(SseEvent::default().data(data));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

// ---------------------------------------------------------------------------
// 能力协商
// ---------------------------------------------------------------------------

async fn caps_handler(State(shared): State<Arc<ServerShared>>) -> Response {
    let caps = &shared.ctx.capabilities;
    let body = json!({
        "mode": "server",
        "chromeSync": caps.chrome_sync,
        "tokenLocalLogs": caps.token_local_logs,
    });
    ([(header::CACHE_CONTROL, "no-store")], axum::Json(body)).into_response()
}

// ---------------------------------------------------------------------------
// 静态资源 + SPA fallback
// ---------------------------------------------------------------------------

async fn static_fallback(State(shared): State<Arc<ServerShared>>, req: Request<Body>) -> Response {
    let path = req.uri().path();
    if path.starts_with("/api/") {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    }
    serve_static(&shared.dist_dir, path).await
}

async fn serve_static(dist_dir: &Path, request_path: &str) -> Response {
    let relative = request_path.trim_start_matches('/');
    let relative = if relative.is_empty() { "index.html" } else { relative };
    let decoded = percent_decode(relative);
    let file_path = match safe_join(dist_dir, &decoded) {
        Some(path) => path,
        None => return (StatusCode::NOT_FOUND, "Not Found").into_response(),
    };
    if file_path.is_file() {
        return match tokio::fs::read(&file_path).await {
            Ok(content) => {
                let mut response = Response::new(Body::from(content));
                *response.status_mut() = StatusCode::OK;
                response
                    .headers_mut()
                    .insert(header::CONTENT_TYPE, content_type_for(&file_path).parse().unwrap());
                response
            }
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response(),
        };
    }
    // SPA fallback：未知路径一律回退到 index.html。
    match tokio::fs::read(dist_dir.join("index.html")).await {
        Ok(content) => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")],
            content,
        )
            .into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "OpenHub 服务已就绪，但前端资源未部署（缺少 dist/index.html）",
        )
            .into_response(),
    }
}

fn safe_join(base: &Path, relative: &str) -> Option<PathBuf> {
    let candidate = base.join(relative);
    if candidate.starts_with(base) {
        Some(candidate)
    } else {
        None
    }
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 3 <= bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&value[index + 1..index + 3], 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn content_type_for(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "txt" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

// ---------------------------------------------------------------------------
// RPC 参数提取助手（camelCase / snake_case 双别名）
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub(crate) fn take<T: serde::de::DeserializeOwned>(
    args: &Value,
    names: &[&str],
) -> Result<T, String> {
    for name in names {
        if let Some(value) = args.get(*name) {
            if value.is_null() {
                continue;
            }
            return serde_json::from_value(value.clone())
                .map_err(|error| format!("参数 {name} 格式错误：{error}"));
        }
    }
    Err(format!("缺少参数：{}", names[0]))
}

#[allow(dead_code)]
pub(crate) fn take_opt<T: serde::de::DeserializeOwned>(
    args: &Value,
    names: &[&str],
) -> Result<Option<T>, String> {
    for name in names {
        if let Some(value) = args.get(*name) {
            if value.is_null() {
                return Ok(None);
            }
            return serde_json::from_value(value.clone())
                .map(Some)
                .map_err(|error| format!("参数 {name} 格式错误：{error}"));
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// 启动辅助
// ---------------------------------------------------------------------------

pub fn generate_token() -> String {
    use sha2::{Digest, Sha256};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let mut hasher = Sha256::new();
    hasher.update(format!("openhub-{}-{}", nanos, std::process::id()));
    hex::encode(hasher.finalize())
}

/// 绑定监听地址：首选端口被占用时向后顺延，全部失败则回退随机端口。
pub fn bind_listener(preferred: u16, host_all_interfaces: bool) -> Result<TcpListener, String> {
    let make_addr = |port: u16| {
        if host_all_interfaces {
            std::net::SocketAddr::from(([0, 0, 0, 0], port))
        } else {
            std::net::SocketAddr::from(([127, 0, 0, 1], port))
        }
    };
    for port in preferred..preferred.saturating_add(PORT_TRIES) {
        let addr = make_addr(port);
        match std::net::TcpListener::bind(addr) {
            Ok(listener) => return listener.set_nonblocking(false).map(|_| listener).map_err(|e| e.to_string()),
            // 刚杀掉旧实例时端口可能尚未释放：短暂轮询首选端口再放弃。
            Err(_) if port == preferred && !host_all_interfaces => {
                for _ in 0..30 {
                    std::thread::sleep(Duration::from_millis(100));
                    if let Ok(listener) = std::net::TcpListener::bind(addr) {
                        return listener.set_nonblocking(false).map(|_| listener).map_err(|e| e.to_string());
                    }
                }
            }
            Err(_) => continue,
        }
    }
    let addr = if host_all_interfaces {
        std::net::SocketAddr::from(([0, 0, 0, 0], 0))
    } else {
        std::net::SocketAddr::from(([127, 0, 0, 1], 0))
    };
    let listener = std::net::TcpListener::bind(addr).map_err(|error| format!("绑定服务端口失败：{error}"))?;
    listener.set_nonblocking(false).map_err(|e| e.to_string())?;
    Ok(listener)
}

/// 在给定监听器上启动 HTTP 服务（阻塞至连接关闭，通常放入后台任务）。
pub async fn serve(shared: Arc<ServerShared>, listener: std::net::TcpListener) -> Result<(), String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let listener = tokio::net::TcpListener::from_std(listener)
        .map_err(|e| format!("迁移监听器到异步运行时失败：{e}"))?;
    let router = build_router(shared.clone());
    let port = listener
        .local_addr()
        .map_err(|e| e.to_string())?
        .port();
    shared.port.store(port, Ordering::Relaxed);
    shared.running.store(true, Ordering::Relaxed);
    let result = axum::serve(listener, router).await;
    shared.running.store(false, Ordering::Relaxed);
    result.map_err(|e| e.to_string())
}

pub fn build_router(shared: Arc<ServerShared>) -> Router {
    Router::new()
        .route("/api/rpc", post(rpc_handler).layer(DefaultBodyLimit::max(16 * 1024 * 1024)))
        .route("/api/events", get(events_handler))
        .route("/api/caps", get(caps_handler))
        .fallback(static_fallback)
        .with_state(shared)
}

// ---------------------------------------------------------------------------
// 轻量模式（桌面专属）：一键隐藏 GUI 窗口、浏览器访问同一内核
// ---------------------------------------------------------------------------

/// 轻量模式状态快照。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LightweightState {
    pub running: bool,
    pub port: u16,
    /// 已开启（下次启动自动隐藏窗口）。
    pub enabled: bool,
    pub url: String,
}

#[allow(dead_code)]
fn meta_get(ctx: &AppContext, key: &str) -> Result<Option<String>, String> {
    let value = crate::db::read_meta(&ctx.database, key)?;
    if value.is_empty() { Ok(None) } else { Ok(Some(value)) }
}

fn meta_set(ctx: &AppContext, key: &str, value: &str) -> Result<(), String> {
    crate::db::write_meta_database(&ctx.database, key, value)
}

fn lightweight_state(shared: &ServerShared) -> Result<LightweightState, String> {
    let enabled = meta_get(&shared.ctx, LIGHTWEIGHT_META_KEY)?
        .map(|value| value == "1")
        .unwrap_or(false);
    let (running, port) = (shared.is_running(), shared.current_port());
    let token = shared.token.clone();
    let url = if running {
        format!("http://127.0.0.1:{port}/?token={token}")
    } else {
        String::new()
    };
    Ok(LightweightState { running, port, enabled, url })
}

#[cfg_attr(feature = "desktop", tauri::command)]
#[cfg(feature = "desktop")]
pub fn get_lightweight_mode_state(
    shared: Managed<'_, Arc<ServerShared>>,
) -> Result<LightweightState, String> {
    lightweight_state(&shared)
}

/// 一键轻量模式：隐藏 GUI 窗口（进程不退出），打开浏览器访问。
#[cfg_attr(feature = "desktop", tauri::command)]
#[cfg(feature = "desktop")]
pub fn enter_lightweight_mode(
    shared: Managed<'_, Arc<ServerShared>>,
) -> Result<LightweightState, String> {
    use tauri::Manager;
    if !shared.is_running() {
        return Err("轻量模式服务未运行，请重启应用后重试".to_string());
    }
    meta_set(&shared.ctx, LIGHTWEIGHT_META_KEY, "1")?;
    if let Some(window) = shared.app.get_webview_window("main") {
        let _ = window.hide();
    }
    let state = lightweight_state(&shared)?;
    open_in_browser(&state.url);
    Ok(LightweightState { enabled: true, ..state })
}

/// 从浏览器侧唤出桌面窗口，等同退出轻量模式。
#[cfg_attr(feature = "desktop", tauri::command)]
#[cfg(feature = "desktop")]
pub fn show_main_window(shared: Managed<'_, Arc<ServerShared>>) -> Result<(), String> {
    use tauri::Manager;
    let _ = meta_set(&shared.ctx, LIGHTWEIGHT_META_KEY, "0");
    if let Some(window) = shared.app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    Ok(())
}

/// 启动时若上次处于轻量模式，自动隐藏窗口并打开浏览器。
#[cfg(feature = "desktop")]
pub fn apply_startup_lightweight_mode(shared: &Arc<ServerShared>) {
    let enabled = meta_get(&shared.ctx, LIGHTWEIGHT_META_KEY)
        .ok()
        .flatten()
        .map(|value| value == "1")
        .unwrap_or(false);
    if !enabled {
        return;
    }
    let shared = shared.clone();
    crate::context::spawn(async move {
        tokio::time::sleep(Duration::from_millis(600)).await;
        if !shared.is_running() {
            return;
        }
        use tauri::Manager;
        if let Some(window) = shared.app.get_webview_window("main") {
            let _ = window.hide();
        }
        let state = lightweight_state(&shared).unwrap_or_else(|_| LightweightState {
            running: true,
            port: shared.current_port(),
            enabled: true,
            url: format!("http://127.0.0.1:{}/?token={}", shared.current_port(), shared.token),
        });
        open_in_browser(&state.url);
    });
}

#[cfg(feature = "desktop")]
fn open_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    if let Err(error) = result {
        tracing::warn!("OpenHub 打开浏览器失败：{error}");
    }
}

// ---------------------------------------------------------------------------
// 登录 / 会话命令（双形态共用；HTTP 层与 IPC 层同一套会话池）
// ---------------------------------------------------------------------------

/// 查询登录门禁状态：required 表示是否启用，authenticated 表示所带令牌是否有效。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn get_login_state(
    ctx: Managed<'_, Arc<AppContext>>,
    token: Option<String>,
) -> Result<LoginStateInfo, String> {
    let login = &ctx.login;
    let token = token.unwrap_or_default();
    // 持有服务静态令牌的访问方在 HTTP 层已直接放行；此处只判定登录会话。
    let authenticated = !login.enabled || login.validate_session(&token);
    Ok(LoginStateInfo {
        required: login.enabled,
        authenticated,
        username: if login.enabled { login.username.clone() } else { String::new() },
    })
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginStateInfo {
    pub required: bool,
    pub authenticated: bool,
    /// 当前配置的用户名（用于登录框预填提示）。
    pub username: String,
}

/// 校验凭据并创建会话令牌。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn login(ctx: Managed<'_, Arc<AppContext>>, username: String, password: String) -> Result<String, String> {
    if !ctx.login.verify(&username, &password) {
        return Err("用户名或密码错误".to_string());
    }
    tracing::info!("[OpenHub] 用户 {} 登录成功", username);
    ctx.login.create_session()
}

/// 注销会话。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn logout(ctx: Managed<'_, Arc<AppContext>>, token: Option<String>) -> Result<(), String> {
    if let Some(token) = token {
        ctx.login.remove_session(&token);
    }
    Ok(())
}
