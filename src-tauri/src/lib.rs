pub(crate) mod charity;
pub(crate) mod core;
pub mod kernel;
pub(crate) mod model;
pub mod proxypool;
pub(crate) mod site;
pub mod token;

// context 经由 core 全局再导出；此处显式提升为公开，供 server 二进制使用。
#[cfg(not(feature = "desktop"))]
pub use core::context;
#[cfg(feature = "desktop")]
pub(crate) use core::context;
pub(crate) use core::*;
#[allow(unused_imports)]
pub(crate) use kernel::*;
#[allow(unused_imports)]
pub(crate) use crate::model::catalog::*;
#[allow(unused_imports)]
pub(crate) use crate::model::gateway::*;
#[allow(unused_imports)]
pub(crate) use crate::proxypool::*;
#[allow(unused_imports)]
pub(crate) use crate::site::library::*;
#[allow(unused_imports)]
pub(crate) use crate::site::sync::*;

#[cfg(feature = "desktop")]
pub use core::app_menu;

#[cfg(test)]
mod tests;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(feature = "desktop")]
use tauri::Manager;

#[cfg(feature = "desktop")]
use tracing::{error, info, warn};

#[cfg(feature = "desktop")]
use crate::context::{AppContext, EventBus};

/// 解析前端静态资源目录：开发调试优先仓库 dist；打包后读安装包资源目录。
#[cfg(feature = "desktop")]
fn resolve_dist_dir(app: &tauri::AppHandle) -> PathBuf {
    let repo_dist = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../dist");
    if cfg!(debug_assertions) && repo_dist.join("index.html").is_file() {
        return repo_dist;
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(resource_dir) = app.path().resource_dir() {
        candidates.push(resource_dir.join("dist"));
        candidates.push(resource_dir);
    }
    candidates.push(repo_dist.clone());
    for candidate in candidates {
        if candidate.is_dir() && candidate.join("index.html").is_file() {
            return candidate;
        }
    }
    repo_dist
}

#[cfg(feature = "desktop")]
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 统一日志：本地时间 + 级别 + 模块定位，支持 RUST_LOG 环境变量过滤（默认 info）
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .compact()
        .with_target(true)
        .with_timer(tracing_subscriber::fmt::time::ChronoLocal::new(
            "%Y-%m-%d %H:%M:%S%.3f".into(),
        ))
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            #[cfg(target_os = "macos")]
            {
                // 让系统/WebKit 优先走简体中文资源（对部分系统菜单项生效）。
                let _ = std::process::Command::new("defaults")
                    .args([
                        "write",
                        "com.dfeer.openhub.desktop",
                        "AppleLanguages",
                        "-array",
                        "zh-Hans",
                        "en",
                    ])
                    .status();
            }
            app_menu::install_chinese_menu(app)?;

            // 菜单刷新：文件 → 刷新 → 后端直接全量刷新 + 通知前端刷新 UI。
            app.on_menu_event(move |app_handle, event| {
                if event.id() == "file-refresh" {
                    info!("[OpenHub] 菜单 file-refresh 触发");
                    let handle = app_handle.clone();
                    tauri::async_runtime::spawn(async move {
                        let ctx = handle.state::<Arc<AppContext>>();
                        match crate::charity::commands::refresh_all_charity_feeds_impl(&ctx).await {
                            Ok(_) => info!("[OpenHub] 全量刷新已提交"),
                            Err(err) => error!("[OpenHub] 全量刷新失败：{err}"),
                        }
                        let _ = tauri::Emitter::emit(&handle, "menu-refresh-requested", ());
                    });
                }
            });

            let app_data_dir = app
                .path()
                .app_data_dir()
                .map_err(|error| error.to_string())?;
            fs::create_dir_all(&app_data_dir)?;
            // 先关掉旧实例，再开数据库/绑端口，避免端口顺延导致浏览器指向旧实例。
            single_instance::claim(&app_data_dir);

            // —— 构建统一运行时上下文 ——
            let database =
                Arc::new(crate::models::Database::open(&app_data_dir.join("sites.sqlite3"))
                    .map_err(std::io::Error::other)?);
            // 升级阶段先把现有采集缓存迁入 SQLite，页面首次查询即可得到完整快照。
            if let Err(error) = token::stats::seed_token_database_from_caches(&database) {
                error!("[OpenHub] Token 缓存迁移到数据库失败：{error}");
            }
            let proxy_runtime =
                Arc::new(proxypool::ProxyRuntime::new(app_data_dir.join("proxy-runtime")));
            let charity_runtime = Arc::new(charity::CharityMonitorRuntime::new());
            let model_catalog_runtime =
                Arc::new(crate::model::catalog::ModelCatalogRuntime::new());
            let event_bus = EventBus::new();
            event_bus.attach_app(app.handle().clone());
            let resource_dir = app.path().resource_dir().ok();
            let ctx = Arc::new(AppContext {
                database,
                proxy_runtime,
                charity_runtime,
                model_catalog_runtime,
                event_bus,
                data_dir: app_data_dir.clone(),
                resource_dir,
                capabilities: crate::context::Capabilities::detect(),
                login: crate::context::LoginManager::from_env(),
            });

            // —— 模型网关状态：独立管理（命令注入），启动时挂接上下文 ——
            let gateway_state = crate::model::gateway::ModelProxyState::new();
            crate::context::block_on(gateway_state.attach_ctx(ctx.clone()));

            app.manage(ctx.clone());
            app.manage(gateway_state);

            // 启动时清理历史订阅里遗留的测速结果后缀，避免旧库节点名继续显示脏数据。
            if let Err(error) = proxypool::repair_stored_node_names(&ctx.database) {
                warn!("[OpenHub] 修复代理节点名称失败：{error}");
            }
            // 首次启动时若 AppData 尚无文件，先秒级释放安装包自带的内置基础版内核与 GeoIP 数据库
            if let Err(e) = crate::kernel::ensure_bundled_assets_installed(&ctx) {
                warn!("[OpenHub] 释放内置资源提示：{e}");
            }
            // Token 采集与页面查询完全解耦：后台每 20 秒增量入库。
            token::stats::start_token_collector(ctx.clone());

            // —— 内嵌 HTTP 服务：浏览器访问同一内核（轻量模式 / 套壳数据源）——
            let shared = web_server::ServerShared::new(
                ctx.clone(),
                resolve_dist_dir(app.handle()),
                web_server::generate_token(),
                app.handle().clone(),
            );
            app.manage(shared.clone());
            match web_server::bind_listener(web_server::DEFAULT_PORT, false) {
                Ok(listener) => {
                    let shared_for_serve = shared.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(error) = web_server::serve(shared_for_serve, listener).await {
                            error!("[OpenHub] 内嵌 HTTP 服务退出：{error}");
                        }
                    });
                }
                Err(error) => error!("[OpenHub] 内嵌 HTTP 服务启动失败：{error}"),
            }
            web_server::apply_startup_lightweight_mode(&shared);

            // 启动阶段禁止阻塞 UI 线程：
            // 1) 恢复代理在后台
            // 2) 检查模型参数当天是否已同步
            // 3) 公益监听延后启动
            // 前端启动后调用 sync_model_catalog(false)：后端以本地日期判断当天是否已同步；
            // 页面保持打开跨过午夜时，前端计时器会再次调用同一命令。

            let restore_ctx = ctx.clone();
            let restore_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                let result = tauri::async_runtime::spawn_blocking({
                    let restore_ctx = restore_ctx.clone();
                    move || {
                        proxypool::restore_saved_proxy(&restore_ctx.database, &restore_ctx.proxy_runtime);
                    }
                })
                .await;
                if let Err(error) = result {
                    error!("OpenHub 后台恢复代理失败：{error}");
                }
                // 代理恢复后再启动公益监听，避免启动瞬间抢锁/抢内核。
                // 前端 onMounted 会 request_charity_round，循环启动后立刻消费 force。
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                charity::start_charity_monitor(restore_ctx.clone());

                // 启动模型网关 (Model Proxy) 独立反代服务
                let gw = restore_handle.state::<crate::model::gateway::ModelProxyState>();
                let proxy_cfg = {
                    let conn = restore_ctx.database.0.lock().ok();
                    conn.map(|c| crate::model::gateway::load_model_proxy_config(&c))
                        .unwrap_or_default()
                };
                *gw.context.config.write().await = proxy_cfg.clone();
                if proxy_cfg.enabled {
                    if let Err(e) =
                        crate::model::gateway::start_model_proxy_server(&gw).await
                    {
                        error!("[OpenHub] 模型网关服务启动失败: {e}");
                    }
                }
            });

            // 启动时后台异步检测核心组件是否缺失，若缺失则全自动静默下载
            let auto_download_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(600)).await;

                // 1. 检测 Mihomo 内核
                {
                    let ctx = auto_download_handle.state::<Arc<AppContext>>();
                    let has_mihomo = crate::kernel::resolve_mihomo_binary(Some(&ctx)).is_some();
                    if !has_mihomo {
                        info!("[OpenHub] 启动组件检测：未检测到 Mihomo 内核，启动后台自动拉取…");
                        match crate::kernel::download_or_update_mihomo_kernel_impl(&ctx, None).await {
                            Ok(status) => info!("[OpenHub] Mihomo 内核自动安装成功 ({})", status.version),
                            Err(e) => error!("[OpenHub] Mihomo 内核自动安装失败：{e}"),
                        }
                    }

                    // 2. 检测 GeoIP 数据库
                    let has_geoip = ctx.geoip_path().is_file();
                    if !has_geoip {
                        info!("[OpenHub] 启动组件检测：未检测到 GeoIP 数据库，启动后台自动拉取…");
                        match crate::kernel::download_or_update_geoip_inner(
                            &ctx,
                            Some(&ctx.database),
                            Some(&ctx.proxy_runtime),
                            None,
                        )
                        .await
                        {
                            Ok(_) => info!("[OpenHub] GeoIP 数据库自动下载成功并已就绪"),
                            Err(e) => error!("[OpenHub] GeoIP 数据库自动下载失败：{e}"),
                        }
                    }
                }
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            // 关闭主窗口视为退出整个应用：macOS 默认关窗不退出，
            // 若进程常驻，轻量模式服务会一直占用端口。
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { .. } = event {
                    window.app_handle().exit(0);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            crate::site::library::list_library,
            crate::site::library::create_site,
            crate::site::library::import_site,
            crate::site::library::update_site,
            crate::site::library::delete_site,
            crate::site::library::toggle_personal,
            crate::site::library::toggle_pending,
            crate::site::library::cycle_usage_state,
            crate::site::library::set_usage_state,
            crate::site::library::toggle_hidden,
            crate::site::library::toggle_runaway,
            proxypool::get_proxy_pool_state,
            proxypool::analyze_proxy_nodes,
            proxypool::save_proxy_subscription,
            proxypool::delete_proxy_subscription,
            proxypool::refresh_proxy_subscription,
            proxypool::set_proxy_pool_settings,
            proxypool::save_proxy_channel,
            proxypool::delete_proxy_channel,
            proxypool::set_proxy_channel_node,
            proxypool::assign_account_proxy_channel,
            proxypool::unassign_account_proxy_channel,
            proxypool::test_proxy_channel_nodes,
            proxypool::set_active_proxy_node,
            proxypool::clear_active_proxy_node,
            proxypool::delete_invalid_proxy_nodes,
            proxypool::test_proxy_node,
            proxypool::test_proxy_nodes,
            proxypool::test_all_proxy_nodes,
            proxypool::cancel_proxy_node_tests,
            crate::site::library::get_remote_user,
            crate::site::sync::mark_sites_with_chrome_sessions,
            crate::site::sync::delete_site_account,
            crate::site::sync::sync_site_account_via_chrome,
            crate::site::library::sync_remote_sites,
            crate::site::library::detect_site_system_types,
            crate::model::catalog::get_system_fonts,
            crate::model::catalog::fetch_site_models_json,
            crate::model::catalog::get_site_model_cache,
            crate::model::catalog::get_all_site_model_caches,
            crate::model::catalog::clear_site_model_cache_for_site,
            crate::model::catalog::save_site_model_cache_for_account,
            crate::model::catalog::get_model_catalog,
            crate::model::catalog::get_model_catalog_detail,
            crate::model::catalog::sync_model_catalog,
            crate::site::sync::list_chrome_sessions,
            crate::site::sync::read_chrome_session,
            crate::site::sync::open_url_in_chrome_profile,
            crate::site::sync::close_chrome_sync_tabs,
            charity::get_charity_feed,
            charity::fetch_charity_feed,
            charity::mark_charity_feed_read,
            charity::get_charity_unread_total,
            charity::get_charity_today_count,
            charity::get_charity_proxy_pool_summary,
            charity::get_charity_sync_logs,
            charity::clear_charity_sync_logs,
            charity::set_charity_monitor_visible,
            charity::request_charity_round,
            charity::list_charity_sources,
            charity::add_charity_source,
            charity::update_charity_source,
            charity::remove_charity_source,
            charity::refresh_all_charity_feeds,
            token::stats::get_token_stats,
            token::stats::sync_token_data,
            token::stats::get_token_usage,
            token::stats::get_token_raw_logs,
            token::stats::get_token_request_health,
            token::stats::get_local_agent_paths,
            web_server::get_lightweight_mode_state,
            web_server::get_login_state,
            web_server::login,
            web_server::logout,
            web_server::enter_lightweight_mode,
            web_server::show_main_window,
            crate::model::gateway::get_model_proxy_config,
            crate::model::gateway::save_model_proxy_config_cmd,
            crate::model::gateway::get_model_proxy_status,
            crate::model::gateway::start_model_proxy,
            crate::model::gateway::stop_model_proxy,
            crate::model::gateway::fetch_model_proxy_models,
            crate::model::gateway::test_model_proxy_health,
            crate::model::gateway::get_model_proxy_logs,
            crate::model::gateway::get_proxy_token_usage,
            crate::model::gateway::get_model_proxy_channel_stats,
            crate::model::gateway::get_model_proxy_overview_stats,
            crate::model::gateway::clear_model_proxy_logs,
            crate::model::gateway::sync_model_proxy_site_channels,
            crate::model::gateway::get_opencode_proxy_config,
            crate::model::gateway::save_opencode_proxy_config_cmd,
            crate::model::gateway::get_opencode_proxy_status,
            crate::model::gateway::start_opencode_proxy,
            crate::model::gateway::stop_opencode_proxy,
            crate::model::gateway::fetch_opencode_models,
            crate::model::gateway::get_opencode_cached_channel_models,
            crate::model::gateway::get_opencode_cached_channel_errors,
            crate::model::gateway::test_opencode_proxy_health,
            crate::model::gateway::get_opencode_proxy_logs,
            crate::model::gateway::get_opencode_channel_stats,
            crate::model::gateway::clear_opencode_proxy_logs,
            crate::model::gateway::sync_opencode_site_channels,
            file_export::save_export_file,
            kernel::get_mihomo_kernel_status,
            kernel::check_mihomo_kernel_update,
            kernel::download_or_update_mihomo_kernel,
            kernel::get_geoip_status,
            kernel::download_or_update_geoip
        ])
        .build(tauri::generate_context!())
        .expect("error while building Tauri application")
        .run(|app_handle, event| {
            // macOS：点击 Dock 图标时重新显示轻量模式下隐藏的窗口。
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                let _ = web_server::show_main_window(
                    app_handle.state::<std::sync::Arc<web_server::ServerShared>>(),
                );
            }
            #[cfg(not(target_os = "macos"))]
            let _ = (app_handle, event);
        });
}

/// 供 openhub-server 二进制调用的公共装配接口。
/// 业务内核与桌面壳完全同源，仅暴露双形态所需的装配面。
#[cfg(not(feature = "desktop"))]
pub mod server_api {
    pub use crate::charity::{start_charity_monitor, CharityMonitorRuntime};
    pub use crate::context;
    pub use crate::core::single_instance;
    pub use crate::core::models::Database;
    pub use crate::core::web_server::{self, ServerShared};
    pub use crate::kernel;
    pub use crate::model::catalog::ModelCatalogRuntime;
    pub use crate::model::gateway::{
        load_model_proxy_config, start_model_proxy_server, ModelProxyState,
    };
    pub use crate::proxypool;
    pub use crate::token;
}
