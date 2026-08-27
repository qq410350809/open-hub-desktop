//! 统一 HTTP 服务层：桌面内嵌 HTTP 与独立 server 二进制共用同一套路由。
//!
//! 路由：
//! - GET  /*           静态构建产物 + SPA fallback
//! - POST /api/rpc     命令分发（与桌面 IPC 同一套命令名）
//! - GET  /api/events  SSE 事件流（EventBus 广播订阅）
//! - GET  /api/caps    能力协商（本机功能可用性）

#[cfg(not(feature = "desktop"))]
use crate::context::LocalRef;
use crate::context::{AppContext, Managed};
use crate::model::gateway::ModelProxyState;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, RawQuery, State};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures_util::Stream;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::net::{IpAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// 首选端口；被占用时向后顺延。
pub const DEFAULT_PORT: u16 = 17896;
const PORT_TRIES: u16 = 24;
/// 共享服务状态：桌面内嵌 HTTP 与独立 server 共用。
/// `gateway` 保持裸值（非 Arc），与命令签名 `Managed<'_, ModelProxyState>` 对齐。
pub struct ServerShared {
    pub ctx: Arc<AppContext>,
    /// 独立 server 形态持有网关状态；桌面端由 Tauri TypeMap 注入，不重复持有。
    #[cfg(not(feature = "desktop"))]
    pub gateway: ModelProxyState,
    /// 前端静态资源目录（dist）。
    pub dist_dir: PathBuf,
    pub running: AtomicBool,
    pub port: AtomicU16,
    /// 桌面端用于读取 Tauri TypeMap 状态。
    #[cfg(feature = "desktop")]
    pub app: tauri::AppHandle,
}

impl ServerShared {
    pub fn new(
        ctx: Arc<AppContext>,
        #[cfg(not(feature = "desktop"))] gateway: ModelProxyState,
        dist_dir: PathBuf,
        #[cfg(feature = "desktop")] app: tauri::AppHandle,
    ) -> Arc<Self> {
        Arc::new(Self {
            ctx,
            #[cfg(not(feature = "desktop"))]
            gateway,
            dist_dir,
            running: AtomicBool::new(false),
            port: AtomicU16::new(0),
            #[cfg(feature = "desktop")]
            app,
        })
    }

    #[allow(dead_code)]
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    #[allow(dead_code)]
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

#[cfg(feature = "desktop")]
fn gateway_context(shared: &ServerShared) -> crate::model::gateway::ModelProxyContext {
    use tauri::Manager;
    shared.app.state::<ModelProxyState>().context.clone()
}

#[cfg(not(feature = "desktop"))]
fn gw_managed(shared: &ServerShared) -> Managed<'_, ModelProxyState> {
    LocalRef(&shared.gateway)
}

/// 统一托管状态引用，桌面端由 Tauri TypeMap 提供。
#[cfg(not(feature = "desktop"))]
fn gateway_context(shared: &ServerShared) -> crate::model::gateway::ModelProxyContext {
    shared.gateway.context.clone()
}

// 鉴权
// ---------------------------------------------------------------------------

/// 从请求头提取登录会话令牌：X-OpenHub-Token 或 Authorization: Bearer。
fn extract_token(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers.get("x-openhub-token").and_then(|v| v.to_str().ok()) {
        return Some(value.to_string());
    }
    if let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(bearer) = value.strip_prefix("Bearer ") {
            return Some(bearer.to_string());
        }
    }
    None
}

/// 统一鉴权判定：只接受有效登录会话，不支持静态令牌或其他免登录方式。
fn token_ok(shared: &ServerShared, headers: &HeaderMap) -> bool {
    let token = extract_token(headers).unwrap_or_default();
    access_allowed(shared, &token)
}

fn access_allowed(shared: &ServerShared, token: &str) -> bool {
    shared.ctx.login.validate_session(token)
}

// ---------------------------------------------------------------------------
// RPC 分发（阶段 3 逐模块补全命令表）
// ---------------------------------------------------------------------------

/// 只能在客户端本地数据平面执行的命令。
///
/// 浏览器访问一体式客户端的轻量 Web 服务时也必须拒绝这些命令：
/// Web 服务不能代替访问者读取本机 AI 日志、Chrome Profile 或桌面资源。
fn is_local_only_command(command: &str) -> bool {
    matches!(
        command,
        "get_token_stats"
            | "sync_token_data"
            | "get_token_usage"
            | "get_token_raw_logs"
            | "get_token_request_health"
            | "get_local_agent_paths"
            | "list_chrome_sessions"
            | "read_chrome_session"
            | "open_url_in_chrome_profile"
            | "close_chrome_sync_tabs"
            | "get_system_fonts"
    )
}

/// 本地命令被 HTTP 调用时返回稳定、可识别的能力错误。
fn local_only_error(command: &str) -> String {
    format!(
        "命令 {command} 仅在客户端本地数据平面可用；浏览器 Web 服务不读取本机 Token 日志或 Chrome 数据"
    )
}

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
                Ok(json!(crate::site::library::set_usage_state(
                    $ctx, id, state
                )))
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
                Ok(json!(
                    crate::site::sync::mark_sites_with_chrome_sessions(
                        $ctx,
                        site_id,
                        site_ids,
                        run_id,
                        extract_only,
                        refresh_pending
                    )
                    .await
                ))
            }
            "delete_site_account" => {
                let site_id: String = take($args, &["siteId", "site_id"])?;
                let profile_id: String = take($args, &["profileId", "profile_id"])?;
                Ok(json!(crate::site::sync::delete_site_account(
                    $ctx, site_id, profile_id
                )))
            }
            "sync_site_account_via_chrome" => {
                let site_id: String = take($args, &["siteId", "site_id"])?;
                let profile_id: String = take($args, &["profileId", "profile_id"])?;
                let run_id: u64 = take($args, &["runId", "run_id"])?;
                Ok(json!(
                    crate::site::sync::sync_site_account_via_chrome(
                        $ctx, site_id, profile_id, run_id
                    )
                    .await
                ))
            }
            "get_remote_user" => Ok(json!(crate::site::library::get_remote_user($ctx).await)),
            "sync_remote_sites" => {
                let runaway: Option<bool> = take_opt($args, &["runaway"])?;
                let run_id: u64 = take($args, &["runId", "run_id"])?;
                Ok(json!(
                    crate::site::library::sync_remote_sites($ctx, runaway, run_id).await
                ))
            }
            "detect_site_system_types" => {
                let site_ids: Vec<String> = take($args, &["siteIds", "site_ids"])?;
                let run_id: u64 = take($args, &["runId", "run_id"])?;
                Ok(json!(
                    crate::site::library::detect_site_system_types($ctx, site_ids, run_id).await
                ))
            }

            // —— Chrome 会话（无状态依赖） ——
            "list_chrome_sessions" => {
                let url: String = take($args, &["url"])?;
                Ok(json!(crate::site::sync::list_chrome_sessions(url).await))
            }
            "read_chrome_session" => {
                let url: String = take($args, &["url"])?;
                let profile_id: String = take($args, &["profileId", "profile_id"])?;
                Ok(json!(
                    crate::site::sync::read_chrome_session(url, profile_id).await
                ))
            }
            "open_url_in_chrome_profile" => {
                let url: String = take($args, &["url"])?;
                let profile_id: String = take($args, &["profileId", "profile_id"])?;
                Ok(json!(
                    crate::site::sync::open_url_in_chrome_profile(url, profile_id).await
                ))
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
                Ok(json!(crate::proxypool::save_proxy_subscription(
                    $ctx, id, name, url
                )))
            }
            "delete_proxy_subscription" => {
                let id: String = take($args, &["id"])?;
                Ok(json!(crate::proxypool::delete_proxy_subscription($ctx, id)))
            }
            "refresh_proxy_subscription" => {
                let id: String = take($args, &["id"])?;
                Ok(json!(
                    crate::proxypool::refresh_proxy_subscription($ctx, id).await
                ))
            }
            "set_proxy_pool_settings" => {
                let ignore_addresses: String =
                    take($args, &["ignoreAddresses", "ignore_addresses"])?;
                Ok(json!(crate::proxypool::set_proxy_pool_settings(
                    $ctx,
                    ignore_addresses
                )))
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
                Ok(json!(
                    crate::proxypool::set_proxy_channel_node($ctx, channel_id, node_id).await
                ))
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
                Ok(json!(crate::proxypool::unassign_account_proxy_channel(
                    $ctx, profile_id
                )))
            }
            "test_proxy_channel_nodes" => {
                let channel_id: Option<String> =
                    take_opt($args, &["channelId", "channel_id"]).ok().flatten();
                let node_ids: Option<Vec<String>> =
                    take_opt($args, &["nodeIds", "node_ids"]).ok().flatten();
                Ok(json!(
                    crate::proxypool::test_proxy_channel_nodes($ctx, channel_id, node_ids).await
                ))
            }
            "set_active_proxy_node" => {
                let node_id: String = take($args, &["nodeId", "node_id"])?;
                Ok(json!(
                    crate::proxypool::set_active_proxy_node($ctx, node_id).await
                ))
            }
            "clear_active_proxy_node" => Ok(json!(crate::proxypool::clear_active_proxy_node($ctx))),
            "delete_invalid_proxy_nodes" => {
                Ok(json!(crate::proxypool::delete_invalid_proxy_nodes($ctx)))
            }
            "test_proxy_node" => {
                let node_id: String = take($args, &["nodeId", "node_id"])?;
                Ok(json!(
                    crate::proxypool::test_proxy_node($ctx, node_id).await
                ))
            }
            "test_proxy_nodes" => {
                let node_ids: Vec<String> = take($args, &["nodeIds", "node_ids"])?;
                Ok(json!(
                    crate::proxypool::test_proxy_nodes($ctx, node_ids).await
                ))
            }
            "test_all_proxy_nodes" => Ok(json!(crate::proxypool::test_all_proxy_nodes($ctx).await)),
            "cancel_proxy_node_tests" => Ok(json!(crate::proxypool::cancel_proxy_node_tests($ctx))),

            // —— 模型目录 / 模型缓存 ——
            "get_system_fonts" => Ok(json!(crate::model::catalog::get_system_fonts())),
            "get_site_model_cache" => {
                let site_id: String = take($args, &["siteId", "site_id"])?;
                Ok(json!(crate::model::catalog::get_site_model_cache(
                    $ctx, site_id
                )))
            }
            "get_all_site_model_caches" => Ok(json!(
                crate::model::catalog::get_all_site_model_caches($ctx)
            )),
            "clear_site_model_cache_for_site" => {
                let site_id: String = take($args, &["siteId", "site_id"])?;
                Ok(json!(
                    crate::model::catalog::clear_site_model_cache_for_site($ctx, site_id)
                ))
            }
            "save_site_model_cache_for_account" => {
                let site_id: String = take($args, &["siteId", "site_id"])?;
                let account: crate::models::SiteModelCacheAccount = take($args, &["account"])?;
                let result: Option<crate::model::catalog::SiteModelsResult> =
                    take_opt($args, &["result"])?;
                let preserve_keys: Option<bool> =
                    take_opt($args, &["preserveKeys", "preserve_keys"])?;
                Ok(json!(
                    crate::model::catalog::save_site_model_cache_for_account(
                        $ctx,
                        site_id,
                        account,
                        result,
                        preserve_keys
                    )
                ))
            }
            "get_model_catalog" => Ok(json!(crate::model::catalog::get_model_catalog($ctx))),
            "get_model_catalog_detail" => {
                let canonical_key: Option<String> =
                    take_opt($args, &["canonicalKey", "canonical_key"])?;
                let id: Option<String> = take_opt($args, &["id"])?;
                Ok(json!(
                    crate::model::catalog::get_model_catalog_detail($ctx, canonical_key, id).await
                ))
            }
            "sync_model_catalog" => {
                let force: Option<bool> = take_opt($args, &["force"])?;
                Ok(json!(
                    crate::model::catalog::sync_model_catalog($ctx, force).await
                ))
            }
            "fetch_site_models_json" => {
                let url: String = take($args, &["url"])?;
                let site_id: Option<String> = take_opt($args, &["siteId", "site_id"])?;
                let profile_id: Option<String> = take_opt($args, &["profileId", "profile_id"])?;
                Ok(json!(
                    crate::model::catalog::fetch_site_models_json($ctx, url, site_id, profile_id)
                        .await
                ))
            }

            // —— 公益监听 ——
            "get_charity_feed" => {
                let feed_id: Option<String> = take_opt($args, &["feedId", "feed_id"])?;
                let offset: Option<usize> = take_opt($args, &["offset"])?;
                let limit: Option<usize> = take_opt($args, &["limit"])?;
                let keyword: Option<String> = take_opt($args, &["keyword"])?;
                Ok(json!(
                    crate::charity::get_charity_feed($ctx, feed_id, offset, limit, keyword).await
                ))
            }
            "mark_charity_feed_read" => {
                let feed_id: Option<String> = take_opt($args, &["feedId", "feed_id"])?;
                Ok(json!(
                    crate::charity::mark_charity_feed_read($ctx, feed_id).await
                ))
            }
            "get_charity_today_count" => {
                Ok(json!(crate::charity::get_charity_today_count($ctx).await))
            }
            "get_charity_unread_total" => {
                Ok(json!(crate::charity::get_charity_unread_total($ctx).await))
            }
            "fetch_charity_feed" => {
                let feed_id: Option<String> = take_opt($args, &["feedId", "feed_id"])?;
                Ok(json!(
                    crate::charity::fetch_charity_feed($ctx, feed_id).await
                ))
            }
            "get_charity_proxy_pool_summary" => Ok(json!(
                crate::charity::get_charity_proxy_pool_summary($ctx).await
            )),
            "get_charity_sync_logs" => {
                let limit: Option<usize> = take_opt($args, &["limit"])?;
                Ok(json!(
                    crate::charity::get_charity_sync_logs($ctx, limit).await
                ))
            }
            "clear_charity_sync_logs" => {
                Ok(json!(crate::charity::clear_charity_sync_logs($ctx).await))
            }
            "set_charity_monitor_visible" => {
                let visible: bool = take($args, &["visible"])?;
                Ok(json!(crate::charity::set_charity_monitor_visible(
                    $ctx, visible
                )))
            }
            "request_charity_round" => Ok(json!(crate::charity::request_charity_round($ctx))),
            "refresh_all_charity_feeds" => {
                Ok(json!(crate::charity::refresh_all_charity_feeds($ctx).await))
            }
            "list_charity_sources" => Ok(json!(crate::charity::list_charity_sources($ctx).await)),
            "add_charity_source" => {
                let id: String = take($args, &["id"])?;
                let name: String = take($args, &["name"])?;
                let json_url: Option<String> = take_opt($args, &["jsonUrl", "json_url"])?;
                Ok(json!(
                    crate::charity::add_charity_source($ctx, id, name, json_url).await
                ))
            }
            "update_charity_source" => {
                let id: String = take($args, &["id"])?;
                let name: Option<String> = take_opt($args, &["name"])?;
                let json_url: Option<String> = take_opt($args, &["jsonUrl", "json_url"])?;
                let enabled: Option<bool> = take_opt($args, &["enabled"])?;
                Ok(json!(
                    crate::charity::update_charity_source($ctx, id, name, json_url, enabled).await
                ))
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
                Ok(json!(
                    crate::token::stats::get_token_stats($ctx, from, to, None).await
                ))
            }
            "sync_token_data" => {
                let force: Option<bool> = take_opt($args, &["force"])?;
                Ok(json!(
                    crate::token::stats::sync_token_data($ctx, force).await
                ))
            }
            "get_token_usage" => Ok(json!(crate::token::stats::get_token_usage($ctx).await)),
            "get_token_raw_logs" => Ok(json!(crate::token::stats::get_token_raw_logs().await)),
            "get_token_request_health" => {
                let _refresh: Option<bool> = take_opt($args, &["refresh"]).ok().flatten();
                Ok(json!(
                    crate::token::stats::get_token_request_health($ctx, None).await
                ))
            }
            "get_local_agent_paths" => {
                Ok(json!(crate::token::stats::get_local_agent_paths().await))
            }

            // —— 内核管理（Mihomo / GeoIP） ——
            "get_component_bootstrap_status" => Ok(json!(
                crate::kernel::get_component_bootstrap_status($ctx).await
            )),
            "get_mihomo_kernel_status" => {
                Ok(json!(crate::kernel::get_mihomo_kernel_status($ctx).await))
            }
            "check_mihomo_kernel_update" => {
                let mirror: Option<String> = take_opt($args, &["mirror"]).ok().flatten();
                Ok(json!(
                    crate::kernel::check_mihomo_kernel_update($ctx, mirror).await
                ))
            }
            "download_or_update_mihomo_kernel" => {
                let mirror: Option<String> = take_opt($args, &["mirror"]).ok().flatten();
                Ok(json!(
                    crate::kernel::download_or_update_mihomo_kernel($ctx, mirror).await
                ))
            }
            "get_geoip_status" => Ok(json!(crate::kernel::get_geoip_status($ctx).await)),
            "download_or_update_geoip" => {
                let mirror: Option<String> = take_opt($args, &["mirror"]).ok().flatten();
                Ok(json!(
                    crate::kernel::download_or_update_geoip($ctx, mirror).await
                ))
            }

            // —— 模型网关（Model Gateway） ——
            "get_model_proxy_config" => Ok(json!(
                crate::model::gateway::get_model_proxy_config($ctx).await
            )),
            "save_model_proxy_config_cmd" => {
                let config: crate::model::gateway::ModelProxyConfig = take($args, &["config"])?;
                Ok(json!(
                    crate::model::gateway::save_model_proxy_config_cmd($ctx, $gw, config).await
                ))
            }
            "get_model_proxy_status" => Ok(json!(
                crate::model::gateway::get_model_proxy_status($gw).await
            )),
            "start_model_proxy" => Ok(json!(crate::model::gateway::start_model_proxy($gw).await)),
            "stop_model_proxy" => Ok(json!(crate::model::gateway::stop_model_proxy($gw).await)),
            "fetch_model_proxy_models" => Ok(json!(
                crate::model::gateway::fetch_model_proxy_models($gw).await
            )),
            "get_cached_channel_models" => Ok(json!(
                crate::model::gateway::get_cached_channel_models($gw).await
            )),
            "get_cached_channel_errors" => Ok(json!(
                crate::model::gateway::get_cached_channel_errors($gw).await
            )),
            "get_model_proxy_logs" => {
                let page: Option<usize> = take_opt($args, &["page"]).ok().flatten();
                let page_size: Option<usize> =
                    take_opt($args, &["pageSize", "page_size"]).ok().flatten();
                let filter: Option<String> = take_opt($args, &["filter"]).ok().flatten();
                let q: Option<String> = take_opt($args, &["q"]).ok().flatten();
                let sort_by: Option<String> =
                    take_opt($args, &["sortBy", "sort_by"]).ok().flatten();
                let sort_order: Option<String> =
                    take_opt($args, &["sortOrder", "sort_order"]).ok().flatten();
                let from: Option<String> = take_opt($args, &["from"]).ok().flatten();
                let to: Option<String> = take_opt($args, &["to"]).ok().flatten();
                Ok(json!(
                    crate::model::gateway::get_model_proxy_logs(
                        $ctx, page, page_size, filter, q, sort_by, sort_order, from, to
                    )
                    .await
                ))
            }
            "get_model_proxy_channel_stats" => Ok(json!(
                crate::model::gateway::get_model_proxy_channel_stats($gw).await
            )),
            "get_proxy_token_usage" => {
                let from: Option<String> = take_opt($args, &["from"]).ok().flatten();
                let to: Option<String> = take_opt($args, &["to"]).ok().flatten();
                Ok(json!(
                    crate::model::gateway::get_proxy_token_usage($gw, from, to).await
                ))
            }
            "get_model_proxy_overview_stats" => {
                let days: Option<u32> = take_opt($args, &["days"]).ok().flatten();
                let from: Option<String> = take_opt($args, &["from"]).ok().flatten();
                let to: Option<String> = take_opt($args, &["to"]).ok().flatten();
                Ok(json!(
                    crate::model::gateway::get_model_proxy_overview_stats($gw, days, from, to)
                        .await
                ))
            }
            "clear_model_proxy_logs" => {
                let mode: Option<String> = take_opt($args, &["mode"]).ok().flatten();
                let before: Option<String> = take_opt($args, &["before"]).ok().flatten();
                Ok(json!(
                    crate::model::gateway::clear_model_proxy_logs($ctx, mode, before).await
                ))
            }
            "sync_model_proxy_site_channels" => {
                let site_ids: Option<Vec<String>> =
                    take_opt($args, &["siteIds", "site_ids"]).ok().flatten();
                Ok(json!(
                    crate::model::gateway::sync_model_proxy_site_channels($ctx, $gw, site_ids)
                        .await
                ))
            }

            // —— OpenCode 代理别名 ——
            "get_opencode_proxy_config" => Ok(json!(
                crate::model::gateway::get_opencode_proxy_config($ctx).await
            )),
            "save_opencode_proxy_config_cmd" => {
                let config: crate::model::gateway::OpencodeProxyConfig = take($args, &["config"])?;
                Ok(json!(
                    crate::model::gateway::save_opencode_proxy_config_cmd($ctx, $gw, config).await
                ))
            }
            "get_opencode_proxy_status" => Ok(json!(
                crate::model::gateway::get_opencode_proxy_status($gw).await
            )),
            "start_opencode_proxy" => Ok(json!(
                crate::model::gateway::start_opencode_proxy($gw).await
            )),
            "stop_opencode_proxy" => {
                Ok(json!(crate::model::gateway::stop_opencode_proxy($gw).await))
            }
            "fetch_opencode_models" => {
                let channel_id: Option<String> =
                    take_opt($args, &["channelId", "channel_id"])?;
                Ok(json!(
                    crate::model::gateway::fetch_opencode_models($gw, channel_id).await
                ))
            }
            "get_opencode_cached_channel_models" => Ok(json!(
                crate::model::gateway::get_opencode_cached_channel_models($gw).await
            )),
            "get_opencode_cached_channel_errors" => Ok(json!(
                crate::model::gateway::get_opencode_cached_channel_errors($gw).await
            )),
            "get_opencode_proxy_logs" => {
                let page: Option<usize> = take_opt($args, &["page"]).ok().flatten();
                let page_size: Option<usize> =
                    take_opt($args, &["pageSize", "page_size"]).ok().flatten();
                let filter: Option<String> = take_opt($args, &["filter"]).ok().flatten();
                let q: Option<String> = take_opt($args, &["q"]).ok().flatten();
                let sort_by: Option<String> =
                    take_opt($args, &["sortBy", "sort_by"]).ok().flatten();
                let sort_order: Option<String> =
                    take_opt($args, &["sortOrder", "sort_order"]).ok().flatten();
                let from: Option<String> = take_opt($args, &["from"]).ok().flatten();
                let to: Option<String> = take_opt($args, &["to"]).ok().flatten();
                Ok(json!(
                    crate::model::gateway::get_opencode_proxy_logs(
                        $ctx, page, page_size, filter, q, sort_by, sort_order, from, to
                    )
                    .await
                ))
            }
            "get_opencode_channel_stats" => Ok(json!(
                crate::model::gateway::get_opencode_channel_stats($gw).await
            )),
            "clear_opencode_proxy_logs" => {
                let mode: Option<String> = take_opt($args, &["mode"]).ok().flatten();
                let before: Option<String> = take_opt($args, &["before"]).ok().flatten();
                Ok(json!(
                    crate::model::gateway::clear_opencode_proxy_logs($ctx, mode, before).await
                ))
            }
            "sync_opencode_site_channels" => {
                let site_ids: Option<Vec<String>> =
                    take_opt($args, &["siteIds", "site_ids"]).ok().flatten();
                Ok(json!(
                    crate::model::gateway::sync_opencode_site_channels($ctx, $gw, site_ids).await
                ))
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
    if is_local_only_command(command) {
        return Err(local_only_error(command));
    }

    #[cfg(feature = "desktop")]
    match command {
        "save_export_file" => {
            let payload: crate::core::file_export::SaveFileArgs = take(args, &["args"])?;
            return Ok(json!(
                crate::core::file_export::save_export_file(payload).await
            ));
        }
        _ => {}
    }
    #[cfg(not(feature = "desktop"))]
    if command == "save_export_file" {
        return Err("此命令仅在桌面形态下可用".to_string());
    }

    // —— Clash 订阅（依赖 ServerShared 的服务端口，先于共享命令表处理） ——
    match command {
        "get_clash_subscription_info" => {
            return crate::proxypool::clash_subscription_info(
                &shared.ctx.database,
                shared.current_port(),
            )
            .map(|info| json!(info));
        }
        "regenerate_clash_subscription_token" => {
            return crate::proxypool::regenerate_clash_subscription_info(
                &shared.ctx.database,
                shared.current_port(),
            )
            .map(|info| json!(info));
        }
        _ => {}
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
    // 登录握手命令免令牌：门禁本身依赖它们完成认证闭环。
    const AUTH_FREE_COMMANDS: &[&str] = &["get_login_state", "login", "logout"];
    if !AUTH_FREE_COMMANDS.contains(&command.as_str()) && !token_ok(&shared, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({
                "code": "AUTH_REQUIRED",
                "error": "登录会话无效或已过期，请重新登录"
            })),
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
// Clash 订阅导出
// ---------------------------------------------------------------------------

/// 解析查询串为键值对（token / maxLatency 均为简单 ASCII 值，无需完整 URL 解析）。
fn parse_query_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((key, value)) => (key.to_string(), value.to_string()),
            None => (pair.to_string(), String::new()),
        })
        .collect()
}

fn query_param(pairs: &[(String, String)], key: &str) -> Option<String> {
    pairs
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.clone())
}

/// Clash 客户端订阅端点：支持登录会话头或 `?token=` 订阅令牌两种鉴权。
///
/// Clash 客户端只会发普通 GET（无法携带自定义 Header），因此订阅令牌走查询串；
/// 令牌持久化在 app_meta，可在代理池页面随时重置使旧链接失效。
async fn clash_subscription_handler(
    State(shared): State<Arc<ServerShared>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    let pairs = parse_query_pairs(query.as_deref().unwrap_or(""));
    let token = query_param(&pairs, "token").unwrap_or_default();
    let authorized = token_ok(&shared, &headers)
        || crate::proxypool::verify_subscription_token(&shared.ctx.database, &token);
    if !authorized {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::CACHE_CONTROL, "no-store")],
            "订阅令牌无效或已被重置，请在 OpenHub 代理池页面重新复制订阅链接",
        )
            .into_response();
    }
    // 阈值允许订阅方按需微调（maxLatency 查询参数），限制在 100~10000ms。
    let max_latency = query_param(&pairs, "maxLatency")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(crate::proxypool::DEFAULT_CLASH_SUB_MAX_LATENCY_MS)
        .clamp(100, 10_000);
    match crate::proxypool::build_clash_subscription_yaml(&shared.ctx.database, max_latency) {
        Ok((yaml, count)) => {
            let mut response = (
                [
                    (header::CONTENT_TYPE, "text/yaml; charset=utf-8"),
                    (header::CACHE_CONTROL, "no-store"),
                ],
                yaml,
            )
                .into_response();
            // Clash 客户端识别的订阅元信息：24h 自动更新 + 节点数提示。
            if let Ok(value) = "24".parse() {
                response.headers_mut().insert("profile-update-interval", value);
            }
            if let Ok(value) = count.to_string().parse() {
                response.headers_mut().insert("x-openhub-node-count", value);
            }
            response
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("生成 Clash 订阅失败：{error}"),
        )
            .into_response(),
    }
}

/// 读取 Clash 订阅信息（含基于当前内嵌服务端口的完整订阅 URL）。
#[cfg(feature = "desktop")]
#[tauri::command]
pub fn get_clash_subscription_info(
    shared: tauri::State<'_, Arc<ServerShared>>,
) -> Result<crate::proxypool::ClashSubscriptionInfo, String> {
    crate::proxypool::clash_subscription_info(&shared.ctx.database, shared.current_port())
}

/// 重置订阅令牌并返回新订阅信息：旧订阅链接立即失效。
#[cfg(feature = "desktop")]
#[tauri::command]
pub fn regenerate_clash_subscription_token(
    shared: tauri::State<'_, Arc<ServerShared>>,
) -> Result<crate::proxypool::ClashSubscriptionInfo, String> {
    crate::proxypool::regenerate_clash_subscription_info(
        &shared.ctx.database,
        shared.current_port(),
    )
}

// ---------------------------------------------------------------------------
// SSE 事件流
// ---------------------------------------------------------------------------

async fn events_handler(
    State(shared): State<Arc<ServerShared>>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, StatusCode> {
    if !token_ok(&shared, &headers) {
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

async fn caps_handler(State(shared): State<Arc<ServerShared>>, headers: HeaderMap) -> Response {
    if !token_ok(&shared, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({
                "code": "AUTH_REQUIRED",
                "error": "登录会话无效或已过期，请重新登录"
            })),
        )
            .into_response();
    }
    let body = json!({
        "mode": "server",
        "chromeSync": false,
        "tokenLocalLogs": false,
        "localTokenStats": false,
        "proxyTokenStats": shared.ctx.capabilities.proxy_token_stats,
        "desktopIntegration": false,
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
    let relative = if relative.is_empty() {
        "index.html"
    } else {
        relative
    };
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
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    content_type_for(&file_path).parse().unwrap(),
                );
                response
            }
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response(),
        };
    }
    // SPA fallback：未知路径一律回退到 index.html。
    let index_path = match safe_join(dist_dir, "index.html") {
        Some(path) => path,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "OpenHub 服务已就绪，但前端资源路径无效",
            )
                .into_response();
        }
    };
    match tokio::fs::read(index_path).await {
        Ok(content) => (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (header::CACHE_CONTROL, "no-store"),
            ],
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
    let relative_path = Path::new(relative);
    if relative_path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return None;
    }
    let candidate = base.join(relative_path);
    if candidate.strip_prefix(base).is_err() {
        return None;
    }

    let canonical_base = match std::fs::canonicalize(base) {
        Ok(path) => path,
        Err(_) => return Some(candidate),
    };
    let mut existing_parent = candidate.as_path();
    while !existing_parent.exists() {
        existing_parent = existing_parent.parent()?;
    }
    let canonical_parent = std::fs::canonicalize(existing_parent).ok()?;
    canonical_parent
        .strip_prefix(&canonical_base)
        .is_ok()
        .then_some(candidate)
}

// 该测试组依赖 server 形态的 ServerShared 构造（desktop 形态需要 Tauri AppHandle，单测无法提供）
#[cfg(all(test, not(feature = "desktop")))]
mod tests {
    use super::safe_join;
    use std::path::Path;

    #[test]
    fn safe_join_rejects_parent_and_absolute_paths() {
        let base = Path::new("/tmp/openhub-dist");
        assert!(safe_join(base, "assets/app.js").is_some());
        assert!(safe_join(base, "../secret.txt").is_none());
        assert!(safe_join(base, "/etc/passwd").is_none());
        assert!(safe_join(base, "%2e%2e/secret.txt").is_some());
    }

    #[test]
    fn safe_join_rejects_symlink_outside_dist() {
        let root = std::env::temp_dir().join(format!("openhub-safe-{}", std::process::id()));
        let dist = root.join("dist");
        let outside = root.join("outside.txt");
        let _ = std::fs::create_dir_all(&dist);
        let _ = std::fs::write(&outside, "secret");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, dist.join("asset.txt")).expect("create symlink");
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&outside, dist.join("asset.txt"))
            .expect("create symlink");
        assert!(safe_join(&dist, "asset.txt").is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    /// 构建最小可用的共享服务状态：临时数据库 + 独立登录管理器 + 已启用 API Key 的网关。
    async fn spawn_shared_service(tag: &str, gateway_api_key: &str) -> (u16, std::path::PathBuf) {
        use super::{bind_listener, serve};
        use crate::context::{AppContext, EventBus, LoginManager};

        let root =
            std::env::temp_dir().join(format!("openhub-shared-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let database = std::sync::Arc::new(
            crate::models::Database::open(&root.join("sites.sqlite3")).unwrap(),
        );
        let ctx = std::sync::Arc::new(AppContext {
            database: database.clone(),
            proxy_runtime: std::sync::Arc::new(crate::proxypool::ProxyRuntime::new(
                root.join("proxy-runtime"),
            )),
            charity_runtime: std::sync::Arc::new(crate::charity::CharityMonitorRuntime::new()),
            model_catalog_runtime: std::sync::Arc::new(
                crate::model::catalog::ModelCatalogRuntime::new(),
            ),
            event_bus: EventBus::new(),
            data_dir: root.clone(),
            resource_dir: None,
            capabilities: crate::context::Capabilities::server_defaults(),
            login: LoginManager::new("admin".into(), "password".into()),
        });

        let gateway = crate::model::gateway::ModelProxyState::new();
        gateway.attach_ctx(ctx.clone()).await;
        gateway.context.config.write().await.api_key = gateway_api_key.to_string();
        gateway
            .context
            .route_enabled
            .store(true, std::sync::atomic::Ordering::Release);

        let shared = super::ServerShared::new(ctx, gateway, root.join("dist"));
        let listener =
            bind_listener(0, std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)).unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = serve(shared, listener).await;
        });
        (port, root)
    }

    #[tokio::test]
    async fn same_port_separates_session_and_gateway_api_key_auth() {
        let client = reqwest::Client::new();
        let (port, root) = spawn_shared_service("auth-matrix", "sk-test").await;
        let base = format!("http://127.0.0.1:{port}");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // /v1：无 Key 拒绝；正确 Bearer Key 放行；OpenHub Session 不能替代 API Key。
        let no_key = client
            .get(format!("{base}/v1/models"))
            .send()
            .await
            .unwrap();
        assert_eq!(no_key.status(), reqwest::StatusCode::UNAUTHORIZED);

        let with_key = client
            .get(format!("{base}/v1/models"))
            .header("Authorization", "Bearer sk-test")
            .send()
            .await
            .unwrap();
        assert_eq!(with_key.status(), reqwest::StatusCode::OK);

        let session_as_key = client
            .get(format!("{base}/v1/models"))
            .header("X-OpenHub-Token", "not-a-gateway-key")
            .send()
            .await
            .unwrap();
        assert_eq!(session_as_key.status(), reqwest::StatusCode::UNAUTHORIZED);

        // /api：无 Session 返回 AUTH_REQUIRED；网关 API Key 不能获得管理权限。
        let rpc_body = serde_json::json!({ "command": "list_library", "args": {} });
        let no_session = client
            .post(format!("{base}/api/rpc"))
            .json(&rpc_body)
            .send()
            .await
            .unwrap();
        assert_eq!(no_session.status(), reqwest::StatusCode::UNAUTHORIZED);
        assert_eq!(
            no_session.json::<serde_json::Value>().await.unwrap()["code"],
            "AUTH_REQUIRED"
        );

        let key_as_session = client
            .post(format!("{base}/api/rpc"))
            .header("Authorization", "Bearer sk-test")
            .json(&rpc_body)
            .send()
            .await
            .unwrap();
        assert_eq!(key_as_session.status(), reqwest::StatusCode::UNAUTHORIZED);

        let _ = std::fs::remove_dir_all(root);
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

/// 绑定监听地址：首选端口被占用时向后顺延，全部失败则回退随机端口。
pub fn bind_listener(preferred: u16, bind_ip: IpAddr) -> Result<TcpListener, String> {
    let make_addr = |port: u16| std::net::SocketAddr::new(bind_ip, port);
    for port in preferred..preferred.saturating_add(PORT_TRIES) {
        let addr = make_addr(port);
        match std::net::TcpListener::bind(addr) {
            Ok(listener) => {
                return listener
                    .set_nonblocking(false)
                    .map(|_| listener)
                    .map_err(|e| e.to_string())
            }
            // 刚杀掉旧实例时端口可能尚未释放：短暂轮询首选端口再放弃。
            Err(_) if port == preferred && bind_ip.is_loopback() => {
                for _ in 0..30 {
                    std::thread::sleep(Duration::from_millis(100));
                    if let Ok(listener) = std::net::TcpListener::bind(addr) {
                        return listener
                            .set_nonblocking(false)
                            .map(|_| listener)
                            .map_err(|e| e.to_string());
                    }
                }
            }
            Err(_) => continue,
        }
    }
    let addr = std::net::SocketAddr::new(bind_ip, 0);
    let listener =
        std::net::TcpListener::bind(addr).map_err(|error| format!("绑定服务端口失败：{error}"))?;
    listener.set_nonblocking(false).map_err(|e| e.to_string())?;
    Ok(listener)
}

/// 在给定监听器上启动 HTTP 服务（阻塞至连接关闭，通常放入后台任务）。
pub async fn serve(
    shared: Arc<ServerShared>,
    listener: std::net::TcpListener,
) -> Result<(), String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let listener = tokio::net::TcpListener::from_std(listener)
        .map_err(|e| format!("迁移监听器到异步运行时失败：{e}"))?;
    let router = build_router(shared.clone());
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    shared.port.store(port, Ordering::Relaxed);
    #[cfg(feature = "desktop")]
    {
        use tauri::Manager;
        let gateway = shared.app.state::<ModelProxyState>();
        *gateway.context.current_port.write().await = port;
    }
    #[cfg(not(feature = "desktop"))]
    {
        *shared.gateway.context.current_port.write().await = port;
    }
    shared.running.store(true, Ordering::Relaxed);
    let result = axum::serve(listener, router).await;
    shared.running.store(false, Ordering::Relaxed);
    result.map_err(|e| e.to_string())
}

pub fn build_router(shared: Arc<ServerShared>) -> Router {
    let gateway = crate::model::gateway::create_shared_model_proxy_router(gateway_context(&shared))
        .with_state(());
    Router::new()
        .route(
            "/api/rpc",
            post(rpc_handler).layer(DefaultBodyLimit::max(16 * 1024 * 1024)),
        )
        .route("/api/events", get(events_handler))
        .route("/api/caps", get(caps_handler))
        .route(
            crate::proxypool::CLASH_SUB_PATH,
            get(clash_subscription_handler),
        )
        .merge(gateway)
        .fallback(static_fallback)
        .with_state(shared)
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
    // 这里只判定登录会话是否有效。
    let authenticated = !login.enabled || login.validate_session(&token);
    Ok(LoginStateInfo {
        required: login.enabled,
        authenticated,
        username: if login.enabled {
            login.username.clone()
        } else {
            String::new()
        },
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
pub fn login(
    ctx: Managed<'_, Arc<AppContext>>,
    username: String,
    password: String,
) -> Result<String, String> {
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
