pub(crate) mod charity;
pub(crate) mod core;
pub(crate) mod kernel;
pub(crate) mod model;
pub(crate) mod proxypool;
pub(crate) mod site;
pub(crate) mod token;

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

pub use core::app_menu;

#[cfg(test)]
mod tests;

use std::fs;
use tauri::Manager;


#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
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
                    eprintln!("[OpenHub] 菜单 file-refresh 触发");
                    let handle = app_handle.clone();
                    tauri::async_runtime::spawn(async move {
                        let database = handle.state::<crate::models::Database>();
                        let monitor =
                            handle.state::<crate::charity::CharityMonitorRuntime>();
                        match crate::charity::refresh_all_charity_feeds(database, monitor)
                            .await
                        {
                            Ok(_) => {
                                eprintln!("[OpenHub] 全量刷新已提交");
                            }
                            Err(err) => {
                                eprintln!("[OpenHub] 全量刷新失败：{err}");
                            }
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
            let database = Database::open(&app_data_dir.join("sites.sqlite3"))
                .map_err(std::io::Error::other)?;
            // 升级阶段先把现有采集缓存迁入 SQLite，页面首次查询即可得到完整快照。
            if let Err(error) = token::stats::seed_token_database_from_caches(&database) {
                eprintln!("[OpenHub] Token 缓存迁移到数据库失败：{error}");
            }
            // 首次启动时若 AppData 尚无文件，先秒级释放安装包自带的内置基础版内核与 GeoIP 数据库
            if let Err(e) = crate::kernel::ensure_bundled_assets_installed(app.handle()) {
                eprintln!("[OpenHub] 释放内置资源提示：{e}");
            }
            let proxy_runtime = proxypool::ProxyRuntime::new(app_data_dir.join("proxy-runtime"));
            let charity_runtime = charity::CharityMonitorRuntime::new();
            let auto_sync_runtime = crate::site::sync::AutoSyncRuntime::default();
            let model_catalog_runtime = crate::model::catalog::ModelCatalogRuntime::new();
            let model_proxy_state =
                crate::model::gateway::ModelProxyState::new_with_app(Some(app.handle().clone()));
            app.manage(database);
            app.manage(proxy_runtime);
            app.manage(charity_runtime);
            app.manage(auto_sync_runtime);
            app.manage(model_catalog_runtime);
            app.manage(model_proxy_state);
            // 启动时清理历史订阅里遗留的测速结果后缀，避免旧库节点名继续显示脏数据。
            if let Err(error) =
                proxypool::repair_stored_node_names(&app.state::<crate::models::Database>())
            {
                eprintln!("[OpenHub] 修复代理节点名称失败：{error}");
            }
            // Token 采集与页面查询完全解耦：后台每 20 秒增量入库。
            token::stats::start_token_collector(app.handle().clone());

            // 轻量模式：常驻本地 HTTP 服务（浏览器访问内核）。
            let web_server = match web_server::start(app.handle().clone()) {
                Ok(handle) => handle,
                Err(error) => {
                    eprintln!("OpenHub 轻量模式服务启动失败：{error}");
                    web_server::WebServerHandle::disabled()
                }
            };
            app.manage(web_server);
            web_server::apply_startup_lightweight_mode(app.handle());

            // 启动阶段禁止阻塞 UI 线程：
            // 1) 恢复代理在后台
            // 2) 检查模型参数当天是否已同步
            // 3) 公益监听延后启动
            // 前端启动后调用 sync_model_catalog(false)：后端以本地日期判断当天是否已同步；
            // 页面保持打开跨过午夜时，前端计时器会再次调用同一命令。

            let restore_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                let result = tauri::async_runtime::spawn_blocking({
                    let restore_handle = restore_handle.clone();
                    move || {
                        let database = restore_handle.state::<crate::models::Database>();
                        let runtime = restore_handle.state::<crate::proxypool::ProxyRuntime>();
                        proxypool::restore_saved_proxy(&database, &runtime);
                    }
                })
                .await;
                if let Err(error) = result {
                    eprintln!("OpenHub 后台恢复代理失败：{error}");
                }
                // 代理恢复后再启动公益监听，避免启动瞬间抢锁/抢内核。
                // 前端 onMounted 会 request_charity_round，循环启动后立刻消费 force。
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                charity::start_charity_monitor(restore_handle.clone());
                // 自动会话同步：账号保活 / 失效恢复 / 模型刷新全程后台化，
                // 与公益监听错开启动（调度器内部还有首轮延迟）。
                crate::site::sync::start_auto_sync(restore_handle.clone());

                // 启动模型网关 (Model Proxy) 独立反代服务
                let database = restore_handle.state::<crate::models::Database>();
                let proxy_state = restore_handle.state::<crate::model::gateway::ModelProxyState>();
                let proxy_cfg = {
                    let conn = database.0.lock().ok();
                    conn.map(|c| crate::model::gateway::load_model_proxy_config(&c)).unwrap_or_default()
                };
                *proxy_state.context.config.write().await = proxy_cfg.clone();
                if proxy_cfg.enabled {
                    if let Err(e) = crate::model::gateway::start_model_proxy_server(&proxy_state).await {
                        eprintln!("[OpenHub] 模型网关服务启动失败: {e}");
                    }
                }
            });

            // 启动时后台异步检测核心组件是否缺失，若缺失则全自动静默下载
            let auto_download_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(600)).await;

                // 1. 检测 Mihomo 内核
                let has_mihomo = crate::kernel::resolve_mihomo_binary(Some(&auto_download_handle)).is_some();
                if !has_mihomo {
                    eprintln!("[OpenHub] 启动组件检测：未检测到 Mihomo 内核，启动后台自动拉取…");
                    match crate::kernel::download_or_update_mihomo_kernel(auto_download_handle.clone(), None).await {
                        Ok(status) => eprintln!("[OpenHub] Mihomo 内核自动安装成功 ({})", status.version),
                        Err(e) => eprintln!("[OpenHub] Mihomo 内核自动安装失败：{e}"),
                    }
                }

                // 2. 检测 GeoIP 数据库
                let has_geoip = crate::kernel::get_app_geoip_path(&auto_download_handle)
                    .map(|p| p.is_file())
                    .unwrap_or(false);
                if !has_geoip {
                    eprintln!("[OpenHub] 启动组件检测：未检测到 GeoIP 数据库，启动后台自动拉取…");
                    match crate::kernel::download_or_update_geoip(auto_download_handle.clone(), None).await {
                        Ok(_) => eprintln!("[OpenHub] GeoIP 数据库自动下载成功并已就绪"),
                        Err(e) => eprintln!("[OpenHub] GeoIP 数据库自动下载失败：{e}"),
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
            crate::site::sync::get_auto_sync_settings,
            crate::site::sync::set_auto_sync_settings,
            crate::site::sync::get_auto_sync_status,
            crate::site::sync::request_auto_sync_round,
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
            crate::model::gateway::get_model_proxy_channel_stats,
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
            // 退出应用时同步停止轻量模式服务，避免端口被常驻进程占用。
            if let tauri::RunEvent::ExitRequested { .. } = event {
                let server = app_handle.state::<std::sync::Arc<web_server::WebServerHandle>>();
                web_server::stop(&server);
            }
            // macOS：点击 Dock 图标时重新显示轻量模式下隐藏的窗口。
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                let _ = web_server::show_main_window(app_handle.clone());
            }
        });
}
