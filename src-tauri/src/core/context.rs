//! 平台无关的运行时上下文。
//!
//! 目标：让业务内核既能运行在 Tauri 桌面壳里，也能以单文件 HTTP 服务
//! （openhub-server）形态独立部署。桌面端与 server 端共享同一套业务实现，
//! 差异仅体现在：
//! - 事件推送：桌面走 Tauri emit + SSE 广播；server 仅 SSE 广播；
//! - 托管状态注入：桌面由 Tauri TypeMap 提供（`tauri::State`），server 由
//!   `AppContext` 字段直接借用（`LocalRef`）；
//! - 平台目录：桌面有 resource_dir（打包资源），server 为 None。

use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
#[cfg(feature = "desktop")]
use std::sync::Mutex;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// 跨平台事件总线
// ---------------------------------------------------------------------------

/// SSE / 前端监听共用的统一事件信封。
/// `event` 与既有 Tauri emit 事件名保持一致，前端零改动迁移。
#[derive(Debug, Clone, Serialize)]
pub struct EventMessage {
    pub event: String,
    pub payload: serde_json::Value,
}

/// 进程内事件总线：
/// - 所有事件写入 broadcast 通道，供 `/api/events` (SSE) 订阅；
/// - 桌面端额外转发到 Tauri 窗口事件（现有 `listen()` 不变）。
///
/// 克隆廉价（内部 Arc），可自由移入后台任务。
#[derive(Clone)]
pub struct EventBus {
    inner: std::sync::Arc<EventBusInner>,
}

struct EventBusInner {
    tx: tokio::sync::broadcast::Sender<EventMessage>,
    #[cfg(feature = "desktop")]
    app: Mutex<Option<tauri::AppHandle>>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(256);
        Self {
            inner: std::sync::Arc::new(EventBusInner {
                tx,
                #[cfg(feature = "desktop")]
                app: Mutex::new(None),
            }),
        }
    }

    /// 桌面端：绑定 AppHandle 以便同时向窗口转发事件（幂等）。
    #[cfg(feature = "desktop")]
    pub fn attach_app(&self, app: tauri::AppHandle) {
        if let Ok(mut guard) = self.inner.app.lock() {
            *guard = Some(app);
        }
    }

    /// 发布事件：Tauri 窗口（仅桌面）+ SSE 订阅者。无订阅者时静默丢弃。
    pub fn emit(&self, event: &str, payload: impl Serialize) {
        let payload = serde_json::to_value(payload).unwrap_or(serde_json::Value::Null);
        #[cfg(feature = "desktop")]
        if let Ok(guard) = self.inner.app.lock() {
            if let Some(app) = guard.as_ref() {
                let _ = tauri::Emitter::emit(app, event, payload.clone());
            }
        }
        let _ = self.inner.tx.send(EventMessage {
            event: event.to_string(),
            payload,
        });
    }

    /// 订阅事件流（SSE 使用）。迟到的订阅者不会收到历史事件。
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<EventMessage> {
        self.inner.tx.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// 能力协商：本机依赖功能在远程部署时自动降级
// ---------------------------------------------------------------------------

/// 运行环境能力探测结果，通过 `/api/caps` 暴露给前端。
#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// 本机检测到 Chrome 用户数据目录（会话同步可用）
    pub chrome_sync: bool,
    /// 本机检测到至少一个 AI 工具日志根目录（Token 本地统计可用）
    pub token_local_logs: bool,
}

impl Capabilities {
    /// 启动时探测一次。探测逻辑刻意保守：只检查知名目录是否存在。
    pub fn detect() -> Self {
        Self {
            chrome_sync: detect_chrome_profile(),
            token_local_logs: detect_local_agent_logs(),
        }
    }
}

/// 平台无关的用户主目录探测。
pub fn home_dir() -> Option<PathBuf> {
    // 避免引入 dirs 家族依赖：优先环境变量，Windows 兜底 USERPROFILE。
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .ok()
}fn detect_chrome_profile() -> bool {
    let Some(home) = home_dir() else {
        return false;
    };
    let candidates = [
        #[cfg(target_os = "macos")]
        home.join("Library/Application Support/Google/Chrome"),
        #[cfg(target_os = "windows")]
        home.join(r"AppData\Local\Google\Chrome\User Data"),
        #[cfg(all(unix, not(target_os = "macos")))]
        home.join(".config/google-chrome"),
    ];
    candidates.iter().any(|p| p.is_dir())
}

fn detect_local_agent_logs() -> bool {
    let Some(home) = home_dir() else {
        return false;
    };
    let candidates = [
        home.join(".claude/projects"),
        home.join(".claude.json"),
        home.join(".codex/sessions"),
        home.join(".config/opencode/storage"),
        home.join(".gemini/tmp"),
        home.join(".continue sessions"),
        home.join(".continue/sessions"),
        home.join("Library/Application Support/Code/User/globalStorage"),
        home.join(".cursor/chats"),
    ];
    candidates.iter().any(|p| p.exists())
}

// ---------------------------------------------------------------------------
// 登录会话管理
// ---------------------------------------------------------------------------

/// 默认登录凭据（可用环境变量 / CLI 参数覆盖）。
pub const DEFAULT_LOGIN_USER: &str = "admin";
pub const DEFAULT_LOGIN_PASSWORD: &str = "Admin@2026";
/// 会话有效期：24 小时。
const SESSION_TTL_SECS: u64 = 24 * 60 * 60;

/// 内存态登录会话管理：
/// - 凭据支持环境变量 OPENHUB_LOGIN_USER / OPENHUB_LOGIN_PASSWORD 覆盖；
/// - 会话令牌随机生成，进程重启即失效（服务形态可配合重启轮换）；
/// - 服务静态访问令牌与登录会话在鉴权层等价。
pub struct LoginManager {
    pub username: String,
    pub password: String,
    /// 是否启用登录门禁（--no-auth 可关闭，默认开启）。
    pub enabled: bool,
    sessions: std::sync::Mutex<HashMap<String, std::time::Instant>>,
}

impl LoginManager {
    pub fn new(username: String, password: String) -> Self {
        Self {
            username,
            password,
            enabled: true,
            sessions: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// 从环境变量读取覆盖值后的默认凭据。
    pub fn from_env() -> Self {
        let username = std::env::var("OPENHUB_LOGIN_USER")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_LOGIN_USER.to_string());
        let password = std::env::var("OPENHUB_LOGIN_PASSWORD")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_LOGIN_PASSWORD.to_string());
        Self::new(username, password)
    }

    fn now() -> std::time::Instant {
        std::time::Instant::now()
    }

    /// 校验用户名密码；成功返回 true。比较恒定耗时无必要——本地内存比对。
    pub fn verify(&self, username: &str, password: &str) -> bool {
        self.enabled && username == self.username && password == self.password
    }

    /// 创建会话令牌。
    pub fn create_session(&self) -> Result<String, String> {
        let token = {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let mut hasher = sha2::Sha256::new();
            use sha2::Digest;
            hasher.update(format!(
                "openhub-session-{}-{}-{}",
                nanos,
                std::process::id(),
                self.sessions.lock().map(|s| s.len()).unwrap_or(0)
            ));
            hex::encode(hasher.finalize())
        };
        if let Ok(mut sessions) = self.sessions.lock() {
            // 顺手清理过期会话
            sessions.retain(|_, created| created.elapsed().as_secs() < SESSION_TTL_SECS);
            sessions.insert(token.clone(), Self::now());
        }
        Ok(token)
    }

    /// 校验会话令牌是否有效。
    pub fn validate_session(&self, token: &str) -> bool {
        if token.is_empty() {
            return false;
        }
        match self.sessions.lock() {
            Ok(mut sessions) => {
                sessions.retain(|_, created| created.elapsed().as_secs() < SESSION_TTL_SECS);
                sessions.contains_key(token)
            }
            Err(_) => false,
        }
    }

    /// 注销会话。
    pub fn remove_session(&self, token: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(token);
        }
    }
}

// ---------------------------------------------------------------------------
// 应用上下文
// ---------------------------------------------------------------------------

/// 平台无关的统一运行时上下文。
///
/// 持有全部共享业务状态（Arc 包装）+ 跨切面服务：
/// - 桌面端：整体作为 `Arc<AppContext>` 注入 Tauri TypeMap；
/// - server 端：由 RPC 分发器直接持有并借用。
pub struct AppContext {
    pub database: std::sync::Arc<crate::models::Database>,
    pub proxy_runtime: std::sync::Arc<crate::proxypool::ProxyRuntime>,
    pub charity_runtime: std::sync::Arc<crate::charity::CharityMonitorRuntime>,
    pub model_catalog_runtime: std::sync::Arc<crate::model::catalog::ModelCatalogRuntime>,
    pub event_bus: EventBus,
    /// 应用数据目录（桌面为 app_data_dir；server 为 --data-dir 或默认路径）。
    pub data_dir: PathBuf,
    /// 打包资源目录；server 形态恒为 None（内核等资源改为自动下载）。
    pub resource_dir: Option<PathBuf>,
    pub capabilities: Capabilities,
    pub login: LoginManager,
}

impl AppContext {
    pub fn bin_dir(&self) -> PathBuf {
        let dir = self.data_dir.join("bin");
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    pub fn geoip_path(&self) -> PathBuf {
        self.data_dir.join("Country.mmdb")
    }
}

// ---------------------------------------------------------------------------
// 异步运行时垫片：替代 tauri::async_runtime，双形态共用
// ---------------------------------------------------------------------------

static RUNTIME_HANDLE: OnceLock<tokio::runtime::Handle> = OnceLock::new();

/// 启动早期注入运行时句柄（桌面取 tauri 全局运行时；server 取自建运行时）。
pub fn init_runtime_handle(handle: tokio::runtime::Handle) {
    let _ = RUNTIME_HANDLE.set(handle);
}

/// 获取全局运行时句柄；未初始化时桌面回退到 tauri 运行时，否则懒建独立运行时。
pub fn runtime_handle() -> tokio::runtime::Handle {
    if let Some(handle) = RUNTIME_HANDLE.get() {
        return handle.clone();
    }
    #[cfg(feature = "desktop")]
    {
        let handle = tauri::async_runtime::handle();
        init_runtime_handle(handle.inner().clone());
        return runtime_handle();
    }
    #[cfg(not(feature = "desktop"))]
    {
        let handle = tokio::runtime::Handle::try_current().ok().unwrap_or_else(|| {
            tracing::warn!("未初始化的异步运行时：临时创建单线程运行时兜底");
            let rt = std::sync::Arc::new(
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("创建 tokio 运行时失败"),
            );
            let handle = rt.handle().clone();
            std::mem::forget(rt);
            handle
        });
        init_runtime_handle(handle.clone());
        handle
    }
}

/// 业务代码统一从这里 spawn 后台任务（替代 tauri::async_runtime::spawn）。
pub fn spawn<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    runtime_handle().spawn(future)
}

/// 阻塞任务调度（替代 tauri::async_runtime::spawn_blocking）。
pub fn spawn_blocking<F, R>(f: F) -> tokio::task::JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    runtime_handle().spawn_blocking(f)
}

/// 在同步上下文中执行异步任务直至完成（替代 tauri::async_runtime::block_on）。
pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        // 已在异步上下文内：直接驱动，避免嵌套 block_on panic。
        tokio::task::block_in_place(|| handle.block_on(future))
    } else {
        runtime_handle().block_on(future)
    }
}

// ---------------------------------------------------------------------------
// 托管状态注入别名：同一命令签名在双形态下编译
// ---------------------------------------------------------------------------

/// 命令参数中的托管状态引用。
/// - desktop：即 `tauri::State<'a, T>`，由 Tauri 注入；
/// - server：`LocalRef<'a, T>`，由 RPC 分发器从 ServerState 借用构造。
#[cfg(feature = "desktop")]
pub type Managed<'a, T> = tauri::State<'a, T>;

#[cfg(not(feature = "desktop"))]
pub type Managed<'a, T> = LocalRef<'a, T>;

/// server 形态下的轻量托管引用：语义对齐 `tauri::State`（Deref 到内部值）。
#[cfg(not(feature = "desktop"))]
#[derive(Clone, Copy, Debug)]
pub struct LocalRef<'a, T>(pub &'a T);

#[cfg(not(feature = "desktop"))]
impl<'a, T> LocalRef<'a, T> {
    pub fn inner(&self) -> &'a T {
        self.0
    }
}

#[cfg(not(feature = "desktop"))]
impl<'a, T> std::ops::Deref for LocalRef<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.0
    }
}
