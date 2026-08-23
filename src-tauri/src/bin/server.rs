//! OpenHub 独立服务形态入口：单文件跨平台 HTTP 服务。
//!
//! 与桌面壳共享同一套业务内核（open_hub_desktop_lib），差异仅在于：
//! - 无窗口 / 托盘，进程即服务；
//! - 可选监听 0.0.0.0 对外提供服务（务必配合 --token）；
//! - Chrome 会话同步、Token 本地日志等本机能力按 /api/caps 协商降级。
//!
//! 用法：
//! ```text
//! openhub-server [--data-dir <dir>] [--listen <addr:port|port>]
//!                [--host-all] [--token <token>] [--dist-dir <dir>]
//! ```

use open_hub_desktop_lib::server_api::{
    context, kernel, load_model_proxy_config, proxypool, single_instance, start_charity_monitor,
    start_model_proxy_server, token, web_server, CharityMonitorRuntime, Database,
    ModelCatalogRuntime, ModelProxyState, ServerShared,
};
use context::{spawn, AppContext};
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    // 统一日志（与桌面端一致）
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

    let args = parse_args();
    context::init_runtime_handle(tokio::runtime::Handle::current());

    // —— 数据目录 ——
    if let Err(error) = std::fs::create_dir_all(&args.data_dir) {
        eprintln!("[OpenHub] 创建数据目录失败：{error}");
        std::process::exit(1);
    }
    single_instance::claim(&args.data_dir);

    // —— 登录凭据 ——
    let mut login_manager = context::LoginManager::from_env();
    if let Some(user) = &args.login_user {
        login_manager.username = user.clone();
    }
    if let Some(password) = &args.login_password {
        login_manager.password = password.clone();
    }
    if args.no_auth {
        login_manager.enabled = false;
    }

    // —— 业务上下文 ——
    let database = Arc::new(
        Database::open(&args.data_dir.join("sites.sqlite3"))
            .map_err(|e| e.to_string())
            .unwrap_or_else(|error| {
                eprintln!("[OpenHub] 打开数据库失败：{error}");
                std::process::exit(1);
            }),
    );
    if let Err(error) = token::stats::seed_token_database_from_caches(&database)
    {
        tracing::error!("[OpenHub] Token 缓存迁移到数据库失败：{error}");
    }
    if let Err(error) = proxypool::repair_stored_node_names(&database) {
        tracing::warn!("[OpenHub] 修复代理节点名称失败：{error}");
    }
    let proxy_runtime =
        Arc::new(proxypool::ProxyRuntime::new(args.data_dir.join("proxy-runtime")));
    let charity_runtime =
        Arc::new(CharityMonitorRuntime::new());
    let model_catalog_runtime = Arc::new(
        ModelCatalogRuntime::new(),
    );
    let ctx = Arc::new(AppContext {
        database,
        proxy_runtime,
        charity_runtime,
        model_catalog_runtime,
        event_bus: context::EventBus::new(),
        data_dir: args.data_dir.clone(),
        resource_dir: None,
        capabilities: context::Capabilities::detect(),
        login: login_manager,
    });
    if let Err(e) = kernel::ensure_bundled_assets_installed(&ctx) {
        tracing::warn!("[OpenHub] 释放内置资源提示：{e}");
    }
    tracing::info!(
        "[OpenHub] 能力协商：chrome_sync={} token_local_logs={}",
        ctx.capabilities.chrome_sync,
        ctx.capabilities.token_local_logs
    );

    // —— 模型网关状态 ——
    let gateway = ModelProxyState::new();
    gateway.attach_ctx(ctx.clone()).await;

    // —— 共享服务状态 ——
    let dist_dir = args.dist_dir.unwrap_or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("dist")))
            .filter(|d| d.join("index.html").is_file())
            .or_else(|| std::env::current_dir().ok().map(|c| c.join("dist")))
            .filter(|d| d.join("index.html").is_file())
            .unwrap_or_else(|| PathBuf::from("dist"))
    });
    let shared = ServerShared::new(ctx.clone(), gateway, dist_dir, args.token.clone());

    // —— 后台任务链 ——
    spawn_boot_tasks(ctx, &shared).await;

    // —— 监听并启动 HTTP 服务 ——
    let (preferred_port, host_all) = match &args.listen {
        ListenTarget::Port(port) => (*port, false),
        ListenTarget::Addr(addr, port) => (*port, *addr),
    };
    let listener = web_server::bind_listener(preferred_port, host_all)
        .unwrap_or_else(|error| {
            eprintln!("[OpenHub] 绑定监听端口失败：{error}");
            std::process::exit(1);
        });
    let bound = listener
        .local_addr()
        .map(|a| a.port())
        .unwrap_or(preferred_port);
    let bind_display = if host_all { "0.0.0.0" } else { "127.0.0.1" };
    tracing::info!(
        "[OpenHub] 服务已就绪：http://{bind_display}:{bound}{}",
        if args.token.is_empty() {
            "（未启用访问令牌，仅限本机使用）"
        } else {
            "（已启用令牌鉴权）"
        }
    );

    if let Err(error) = web_server::serve(shared, listener).await {
        tracing::error!("[OpenHub] HTTP 服务异常退出：{error}");
        std::process::exit(1);
    }
}

/// 后台任务链：恢复代理 → 公益监听 → Token 采集 → 网关自启 → 内核自动下载。
async fn spawn_boot_tasks(ctx: Arc<AppContext>, shared: &Arc<ServerShared>) {
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    {
        let boot_ctx = ctx.clone();
        spawn(async move {
            let restore_ctx = boot_ctx.clone();
            let result = tokio::task::spawn_blocking(move || {
                proxypool::restore_saved_proxy(&restore_ctx.database, &restore_ctx.proxy_runtime);
            })
            .await;
            if let Err(error) = result {
                tracing::error!("[OpenHub] 后台恢复代理失败：{error}");
            }
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            start_charity_monitor(boot_ctx);
        });
    }

    token::stats::start_token_collector(ctx.clone());

    // 网关自启：读取配置后按 enabled 拉起反代服务
    {
        let ctx = ctx.clone();
        let gateway_shared = shared.clone();
        spawn(async move {
            let proxy_cfg = match ctx.database.lock_conn() {
                Ok(conn) => load_model_proxy_config(&conn),
                Err(_) => Default::default(),
            };
            if proxy_cfg.enabled {
                if let Err(e) = start_model_proxy_server(&gateway_shared.gateway).await {
                    tracing::error!("[OpenHub] 模型网关服务启动失败: {e}");
                }
            }
        });
    }

    // 内核 / GeoIP 自动下载
    {
        let ctx = ctx.clone();
        spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            let has_mihomo =
                kernel::resolve_mihomo_binary(Some(&ctx)).is_some();
            if !has_mihomo {
                tracing::info!("[OpenHub] 未检测到 Mihomo 内核，后台自动拉取…");
                if let Err(e) =
                    kernel::download_or_update_mihomo_kernel_impl(&ctx, None)
                        .await
                {
                    tracing::error!("[OpenHub] Mihomo 内核自动安装失败：{e}");
                }
            }
            if !ctx.geoip_path().is_file() {
                tracing::info!("[OpenHub] 未检测到 GeoIP 数据库，后台自动拉取…");
                if let Err(e) = kernel::download_or_update_geoip_inner(
                    &ctx,
                    Some(&ctx.database),
                    Some(&ctx.proxy_runtime),
                    None,
                )
                .await
                {
                    tracing::error!("[OpenHub] GeoIP 数据库自动下载失败：{e}");
                }
            }
        });
    }
}

enum ListenTarget {
    /// 仅本机回环
    Port(u16),
    /// (是否监听全部网卡, 端口)
    Addr(bool, u16),
}

struct Args {
    listen: ListenTarget,
    data_dir: PathBuf,
    dist_dir: Option<PathBuf>,
    token: String,
    login_user: Option<String>,
    login_password: Option<String>,
    no_auth: bool,
}

fn default_data_dir() -> PathBuf {
    let base = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    #[cfg(target_os = "macos")]
    return base.join("Library/Application Support/OpenHub");
    #[cfg(target_os = "windows")]
    return base.join(r"AppData\Roaming\OpenHub");
    #[cfg(all(unix, not(target_os = "macos")))]
    return base.join(".local/share/OpenHub");
}

fn parse_args() -> Args {
    let mut listen = ListenTarget::Port(web_server::DEFAULT_PORT);
    let mut host_all_flag = false;
    let mut data_dir: Option<PathBuf> = None;
    let mut dist_dir: Option<PathBuf> = None;
    let mut token = String::new();
    let mut login_user: Option<String> = None;
    let mut login_password: Option<String> = None;
    let mut no_auth = false;

    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        let mut value = |name: &str| -> String {
            iter.next().unwrap_or_else(|| {
                eprintln!("[OpenHub] 参数 {name} 缺少取值");
                std::process::exit(2);
            })
        };
        match arg.as_str() {
            "--listen" => {
                let raw = value("--listen");
                if let Some((_, port)) = raw.rsplit_once(':') {
                    if let Ok(p) = port.parse() {
                        listen = ListenTarget::Addr(true, p);
                        continue;
                    }
                }
                match raw.parse::<u16>() {
                    Ok(p) => listen = ListenTarget::Port(p),
                    Err(_) => {
                        eprintln!("[OpenHub] 无效的监听地址：{raw}");
                        std::process::exit(2);
                    }
                }
            }
            "--host-all" => host_all_flag = true,
            "--data-dir" => data_dir = Some(PathBuf::from(value("--data-dir"))),
            "--dist-dir" => dist_dir = Some(PathBuf::from(value("--dist-dir"))),
            "--token" => token = value("--token"),
            "--user" => login_user = Some(value("--user")),
            "--password" => login_password = Some(value("--password")),
            "--no-auth" => no_auth = true,
            "--help" | "-h" => {
                println!("用法：openhub-server [--listen <port|host:port>] [--host-all] [--data-dir <dir>] [--dist-dir <dir>] [--token <token>]");
                println!("  --listen     监听端口或地址:端口（默认 17896，仅回环）");
                println!("  --host-all   监听 0.0.0.0 对外提供服务（远程部署时配合 --token）");
                println!("  --data-dir   数据目录（默认平台应用数据目录）");
                println!("  --dist-dir   前端静态资源目录（默认可执行文件旁的 dist 或 ./dist）");
                println!("  --token      服务访问令牌；与登录会话在鉴权层等价");
                println!("  --user       登录用户名（默认 admin，可用 OPENHUB_LOGIN_USER 覆盖）");
                println!("  --password   登录密码（默认 Admin@2026，可用 OPENHUB_LOGIN_PASSWORD 覆盖）");
                println!("  --no-auth    关闭登录门禁（脚本化场景使用）");
                std::process::exit(0);
            }
            other => {
                eprintln!("[OpenHub] 未知参数：{other}（--help 查看用法）");
                std::process::exit(2);
            }
        }
    }

    // --host-all 与纯端口形式组合
    if host_all_flag {
        listen = match listen {
            ListenTarget::Port(p) => ListenTarget::Addr(true, p),
            other => other,
        };
    }

    Args {
        listen,
        data_dir: data_dir.unwrap_or_else(default_data_dir),
        dist_dir,
        token,
        login_user,
        login_password,
        no_auth,
    }
}
