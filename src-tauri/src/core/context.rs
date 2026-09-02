//! 平台无关的运行时上下文。
//!
//! 目标：让业务内核既能运行在 Tauri 桌面壳里，也能以单文件 HTTP 服务
//! （openhub-server）形态独立部署。桌面端与 server 端共享同一套业务实现，
//! 差异仅体现在：
//! - 事件推送：桌面走 Tauri emit + SSE 广播；server 仅 SSE 广播；
//! - 托管状态注入：桌面由 Tauri TypeMap 提供（`tauri::State`），server 由
//!   `AppContext` 字段直接借用（`LocalRef`）；
//! - 平台目录：桌面有 resource_dir（打包资源），server 为 None。

use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

/// 运行时能力探测结果。
///
/// `token_local_logs` 保留为旧前端字段兼容；新代码应使用 `local_token_stats`。
/// server 形态使用 `server_defaults()`，不会把服务所在主机的本地日志误报给浏览器。
#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// 本机检测到 Chrome 用户数据目录（仅集成式客户端可用）
    pub chrome_sync: bool,
    /// 兼容旧协议：本机检测到 AI 工具日志根目录
    pub token_local_logs: bool,
    /// 当前客户端是否可读取本机 AI 工具日志
    pub local_token_stats: bool,
    /// 当前运行时是否提供 OpenHub 反代统计
    pub proxy_token_stats: bool,
    /// 当前进程是否拥有桌面窗口/菜单/文件对话框能力
    pub desktop_integration: bool,
}

impl Capabilities {
    /// 集成式客户端：检测本机能力，并开放本地与反代两类统计。
    pub fn detect() -> Self {
        let chrome_sync = detect_chrome_profile();
        let token_local_logs = detect_local_agent_logs();
        Self {
            chrome_sync,
            token_local_logs,
            local_token_stats: token_local_logs,
            proxy_token_stats: true,
            desktop_integration: true,
        }
    }

    /// 独立 Web 服务：只声明服务端能力，绝不暴露服务进程的本地日志能力。
    pub fn server_defaults() -> Self {
        Self {
            chrome_sync: false,
            token_local_logs: false,
            local_token_stats: false,
            proxy_token_stats: true,
            desktop_integration: false,
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
}
fn detect_chrome_profile() -> bool {
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
/// 会话有效期：7 天。
const SESSION_TTL_SECS: u64 = 7 * 24 * 60 * 60;

/// 会话记录只保存摘要和绝对过期时间，不保存明文令牌。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedSession {
    token_hash: String,
    created_at: u64,
    expires_at: u64,
}

fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// 登录会话管理：
/// - 凭据支持环境变量 OPENHUB_LOGIN_USER / OPENHUB_LOGIN_PASSWORD 覆盖；
/// - 会话令牌由操作系统安全随机源生成；
/// - 会话记录持久化到数据目录，默认有效期 7 天；
/// - 所有受保护 HTTP 请求都必须使用有效登录会话。
pub struct LoginManager {
    pub username: String,
    pub password: String,
    /// 登录门禁始终开启。
    pub enabled: bool,
    sessions: std::sync::Mutex<HashMap<String, PersistedSession>>,
    storage_path: std::sync::Mutex<Option<PathBuf>>,
}

impl LoginManager {
    pub fn new(username: String, password: String) -> Self {
        Self {
            username,
            password,
            enabled: true,
            sessions: std::sync::Mutex::new(HashMap::new()),
            storage_path: std::sync::Mutex::new(None),
        }
    }

    /// 绑定数据目录并恢复仍未过期的会话。
    pub fn load_from_data_dir(self, data_dir: &Path) -> Self {
        let path = data_dir.join("auth-sessions.json");
        let now = unix_timestamp();
        let sessions = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Vec<PersistedSession>>(&raw).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|session| session.expires_at > now && session.token_hash.len() == 64)
            .map(|session| (session.token_hash.clone(), session))
            .collect::<HashMap<_, _>>();
        if let Ok(mut guard) = self.sessions.lock() {
            *guard = sessions;
        }
        if let Ok(mut guard) = self.storage_path.lock() {
            *guard = Some(path);
        }
        self
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

    fn hash_token(token: &str) -> String {
        hex::encode(sha2::Sha256::digest(token.as_bytes()))
    }

    fn persist(&self, sessions: &HashMap<String, PersistedSession>) -> Result<(), String> {
        let path = self
            .storage_path
            .lock()
            .map_err(|_| "登录会话存储不可用".to_string())?
            .clone();
        let Some(path) = path else {
            return Ok(());
        };
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建登录会话目录失败：{error}"))?;
        let records = sessions.values().cloned().collect::<Vec<_>>();
        let content = serde_json::to_vec_pretty(&records)
            .map_err(|error| format!("序列化登录会话失败：{error}"))?;
        let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&temp, content).map_err(|error| format!("写入登录会话失败：{error}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600));
        }
        #[cfg(windows)]
        if path.exists() {
            std::fs::remove_file(&path).map_err(|error| format!("替换登录会话失败：{error}"))?;
        }
        std::fs::rename(&temp, &path).map_err(|error| format!("保存登录会话失败：{error}"))
    }

    fn now() -> u64 {
        unix_timestamp()
    }

    /// 校验用户名密码；成功返回 true。比较恒定耗时无必要——本地内存比对。
    pub fn verify(&self, username: &str, password: &str) -> bool {
        self.enabled && username == self.username && password == self.password
    }

    /// 创建会话令牌。
    pub fn create_session(&self) -> Result<String, String> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|error| format!("生成登录令牌失败：{error}"))?;
        let token = hex::encode(bytes);
        let now = Self::now();
        let session = PersistedSession {
            token_hash: Self::hash_token(&token),
            created_at: now,
            expires_at: now.saturating_add(SESSION_TTL_SECS),
        };
        let key = session.token_hash.clone();
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| "登录会话存储不可用".to_string())?;
        sessions.retain(|_, session| session.expires_at > now);
        sessions.insert(key, session);
        if let Err(error) = self.persist(&sessions) {
            sessions.retain(|_, session| {
                session.expires_at > now && session.token_hash != Self::hash_token(&token)
            });
            return Err(error);
        }
        Ok(token)
    }

    /// 校验会话令牌是否有效。
    pub fn validate_session(&self, token: &str) -> bool {
        if token.is_empty() {
            return false;
        }
        let now = Self::now();
        match self.sessions.lock() {
            Ok(mut sessions) => {
                let before = sessions.len();
                sessions.retain(|_, session| session.expires_at > now);
                let changed = before != sessions.len();
                let valid = sessions.contains_key(&Self::hash_token(token));
                if changed {
                    let _ = self.persist(&sessions);
                }
                valid
            }
            Err(_) => false,
        }
    }

    /// 注销会话。
    pub fn remove_session(&self, token: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(&Self::hash_token(token));
            let _ = self.persist(&sessions);
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
    pub model_probe: std::sync::Arc<crate::model::probe::ProbeRuntime>,
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
        let handle = tokio::runtime::Handle::try_current()
            .ok()
            .unwrap_or_else(|| {
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

#[cfg(test)]
mod tests {
    use super::LoginManager;

    #[test]
    fn sessions_are_random_64_hex_chars_and_can_be_revoked() {
        let manager = LoginManager::new("admin".into(), "password".into());
        let first = manager.create_session().unwrap();
        let second = manager.create_session().unwrap();
        assert_eq!(first.len(), 64);
        assert!(first.chars().all(|character| character.is_ascii_hexdigit()));
        assert_ne!(first, second);
        assert!(manager.validate_session(&first));
        manager.remove_session(&first);
        assert!(!manager.validate_session(&first));
        assert!(manager.validate_session(&second));
    }

    #[test]
    fn expired_sessions_are_not_restored() {
        let root =
            std::env::temp_dir().join(format!("openhub-auth-expired-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("auth-sessions.json"),
            r#"[{"token_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","created_at":1,"expires_at":1}]"#,
        )
        .unwrap();
        let manager =
            LoginManager::new("admin".into(), "password".into()).load_from_data_dir(&root);
        assert!(!manager.validate_session("anything"));
        let _ = std::fs::remove_dir_all(root);
    }
    #[test]
    fn sessions_restore_from_data_directory_without_persisting_plaintext() {
        let root = std::env::temp_dir().join(format!("openhub-auth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let manager =
            LoginManager::new("admin".into(), "password".into()).load_from_data_dir(&root);
        let token = manager.create_session().unwrap();
        let raw = std::fs::read_to_string(root.join("auth-sessions.json")).unwrap();
        assert!(!raw.contains(&token));

        let restored =
            LoginManager::new("admin".into(), "password".into()).load_from_data_dir(&root);
        assert!(restored.validate_session(&token));
        let _ = std::fs::remove_dir_all(root);
    }
}
