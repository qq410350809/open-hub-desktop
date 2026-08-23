//! OpenHub 独立服务形态入口：单文件跨平台 HTTP 服务。
//!
//! 与桌面壳共享同一套业务内核（open_hub_desktop_lib），差异仅在于：
//! - 无窗口 / 托盘，进程即服务；
//! - 对外监听仍要求登录会话；
//! - Chrome 会话同步、Token 本地日志等本机能力按 /api/caps 协商降级。
//!
//! 用法：
//! ```text
//! openhub-server [--data-dir <dir>] [--listen <addr:port|port>]
//!                [--host-all] [--dist-dir <dir>]
//! ```

use context::{spawn, AppContext};
use open_hub_desktop_lib::server_api::{
    context, load_model_proxy_config, proxypool, single_instance, start_charity_monitor,
    start_model_proxy_server, web_server, CharityMonitorRuntime, Database, ModelCatalogRuntime,
    ModelProxyState, ServerShared,
};
use std::net::{IpAddr, SocketAddr};
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
    let preferred_port = args.listen.port();
    let bind_ip = args.listen.bind_ip();
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
    let login_manager = login_manager.load_from_data_dir(&args.data_dir);

    // —— 业务上下文 ——
    let database = Arc::new(
        Database::open(&args.data_dir.join("sites.sqlite3"))
            .map_err(|e| e.to_string())
            .unwrap_or_else(|error| {
                eprintln!("[OpenHub] 打开数据库失败：{error}");
                std::process::exit(1);
            }),
    );
    if let Err(error) = proxypool::repair_stored_node_names(&database) {
        tracing::warn!("[OpenHub] 修复代理节点名称失败：{error}");
    }
    let proxy_runtime = Arc::new(proxypool::ProxyRuntime::new(
        args.data_dir.join("proxy-runtime"),
    ));
    let charity_runtime = Arc::new(CharityMonitorRuntime::new());
    let model_catalog_runtime = Arc::new(ModelCatalogRuntime::new());
    let ctx = Arc::new(AppContext {
        database,
        proxy_runtime,
        charity_runtime,
        model_catalog_runtime,
        event_bus: context::EventBus::new(),
        data_dir: args.data_dir.clone(),
        resource_dir: None,
        capabilities: context::Capabilities::server_defaults(),
        login: login_manager,
    });
    // 组件（Mihomo / GeoIP）不再随 server 包释放；首次访问 Web 界面时由初始化引导按需下载。

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
    let shared = ServerShared::new(ctx.clone(), gateway, dist_dir);

    // 代理恢复和网关自启保留；可选组件初始化由 Web 引导显式触发。
    spawn_boot_tasks(ctx, &shared).await;

    let listener = web_server::bind_listener(preferred_port, bind_ip).unwrap_or_else(|error| {
        eprintln!("[OpenHub] 绑定监听端口失败：{error}");
        std::process::exit(1);
    });
    let bound = listener
        .local_addr()
        .unwrap_or_else(|_| SocketAddr::new(bind_ip, preferred_port));
    tracing::info!("[OpenHub] 服务已就绪：http://{bound}（登录会话鉴权已启用）");

    if let Err(error) = web_server::serve(shared, listener).await {
        tracing::error!("[OpenHub] HTTP 服务异常退出：{error}");
        std::process::exit(1);
    }
}

/// 后台任务链：恢复代理 → 公益监听 → 网关自启。
///
/// 本地 Token 日志和可选组件初始化均不在 server 启动阶段静默执行。
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

    // 共享 HTTP 服务已承载 /v1 模型接口；按配置启用共享网关路由，不创建第二个监听器。
    {
        let ctx = ctx.clone();
        let gateway_shared = shared.clone();
        spawn(async move {
            let proxy_cfg = match ctx.database.lock_conn() {
                Ok(conn) => load_model_proxy_config(&conn),
                Err(_) => Default::default(),
            };
            *gateway_shared.gateway.context.config.write().await = proxy_cfg.clone();
            if proxy_cfg.enabled {
                if let Err(e) = start_model_proxy_server(&gateway_shared.gateway).await {
                    tracing::error!("[OpenHub] 共享模型网关路由启动失败: {e}");
                }
            }
        });
    }
}

enum ListenTarget {
    /// 仅本机回环
    Port(u16),
    /// 指定监听 IP 和端口
    Addr(IpAddr, u16),
}

impl ListenTarget {
    fn port(&self) -> u16 {
        match self {
            Self::Port(port) | Self::Addr(_, port) => *port,
        }
    }

    fn bind_ip(&self) -> IpAddr {
        match self {
            Self::Port(_) => IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            Self::Addr(ip, _) => *ip,
        }
    }
}

struct Args {
    listen: ListenTarget,
    data_dir: PathBuf,
    dist_dir: Option<PathBuf>,
    login_user: Option<String>,
    login_password: Option<String>,
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
    let mut login_user: Option<String> = None;
    let mut login_password: Option<String> = None;

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
                match parse_listen_target(&raw) {
                    Ok(target) => listen = target,
                    Err(error) => {
                        eprintln!("[OpenHub] {error}");
                        std::process::exit(2);
                    }
                }
            }
            "--host-all" => host_all_flag = true,
            "--data-dir" => data_dir = Some(PathBuf::from(value("--data-dir"))),
            "--dist-dir" => dist_dir = Some(PathBuf::from(value("--dist-dir"))),
            "--user" => login_user = Some(value("--user")),
            "--password" => login_password = Some(value("--password")),
            "--help" | "-h" => {
                println!("用法：openhub-server [--listen <port|ip:port|[ipv6]:port>] [--host-all] [--data-dir <dir>] [--dist-dir <dir>]");
                println!("  --listen     监听端口或指定 IP 地址（默认 17896，仅回环）");
                println!(
                    "  --host-all   纯端口形式监听 0.0.0.0 对外提供服务（所有请求需要登录会话）"
                );
                println!("  --data-dir   数据目录（默认平台应用数据目录）");
                println!("  --dist-dir   前端静态资源目录（默认可执行文件旁的 dist 或 ./dist）");
                println!("  --user       登录用户名（默认 admin，可用 OPENHUB_LOGIN_USER 覆盖）");
                println!(
                    "  --password   登录密码（默认 Admin@2026，可用 OPENHUB_LOGIN_PASSWORD 覆盖）"
                );
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
            ListenTarget::Port(p) => {
                ListenTarget::Addr(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), p)
            }
            other => other,
        };
    }

    Args {
        listen,
        data_dir: data_dir.unwrap_or_else(default_data_dir),
        dist_dir,
        login_user,
        login_password,
    }
}

fn parse_listen_target(raw: &str) -> Result<ListenTarget, String> {
    if let Ok(port) = raw.parse::<u16>() {
        return Ok(ListenTarget::Port(port));
    }
    raw.parse::<SocketAddr>()
        .map(|addr| ListenTarget::Addr(addr.ip(), addr.port()))
        .map_err(|_| format!("无效的监听地址：{raw}（使用 port、ip:port 或 [ipv6]:port）"))
}

#[cfg(test)]
mod tests {
    use super::parse_listen_target;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn parses_loopback_and_external_listen_addresses() {
        let loopback = parse_listen_target("127.0.0.1:17896").unwrap();
        assert_eq!(loopback.port(), 17896);
        assert!(loopback.bind_ip().is_loopback());

        let external = parse_listen_target("192.0.2.10:8080").unwrap();
        assert_eq!(external.bind_ip(), IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)));
        assert!(!external.bind_ip().is_loopback());
    }

    #[test]
    fn parses_bracketed_ipv6_and_rejects_unqualified_addresses() {
        let ipv6 = parse_listen_target("[::1]:17896").unwrap();
        assert_eq!(ipv6.bind_ip(), IpAddr::V6(Ipv6Addr::LOCALHOST));
        assert!(parse_listen_target("localhost:17896").is_err());
    }
}
