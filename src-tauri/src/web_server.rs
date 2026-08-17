//! 轻量模式：内嵌本地 HTTP 服务（现统一由 Gateway 在 17896 端口处理 API 路由）。
#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_variables)]
//!
//! 目标：设置页一键关闭 GUI 窗口后，进程继续作为内核常驻，
//! 用户通过浏览器访问同一个资料库。
//!
//! 设计要点：
//! - 仅监听 127.0.0.1，不对外开放；
//! - GET 服务构建产物（dist），带 SPA fallback；
//! - POST /api/rpc 把命令分发到与桌面端完全相同的内核实现；
//! - 无新增第三方依赖（纯 std + 现有 tokio/tauri 运行时）。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::charity_monitor::CharityMonitorRuntime;
use crate::models::{Database, SiteModelCacheAccount, SiteRecord};
use crate::models_fetch::SiteModelsResult;
use crate::proxy_pool::ProxyRuntime;
use crate::{
    auto_sync, charity_monitor, chrome_usage, model_catalog, models_fetch, proxy_pool, site_crud,
    token_stats,
};

/// 首选端口；被占用时向后顺延。
const DEFAULT_PORT: u16 = 17896;
const PORT_TRIES: u16 = 24;
/// app_meta 中轻量模式的持久化键。
const LIGHTWEIGHT_META_KEY: &str = "lightweight_mode";

pub struct WebServerHandle {
    pub running: AtomicBool,
    pub port: AtomicU16,
    token: Mutex<String>,
    dist_dir: Mutex<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LightweightState {
    pub running: bool,
    pub port: u16,
    /// 已开启（下次启动自动隐藏窗口）。
    pub enabled: bool,
    pub url: String,
}

impl WebServerHandle {
    /// 服务不可用时的占位状态（端口 0，未运行），保证 app.state 永远可用。
    pub fn disabled() -> Arc<WebServerHandle> {
        Arc::new(WebServerHandle {
            running: AtomicBool::new(false),
            port: AtomicU16::new(0),
            token: Mutex::new(String::new()),
            dist_dir: Mutex::new(PathBuf::new()),
        })
    }
}

/// 启动轻量模式 HTTP 服务（纯 API 模式下由网关处理）。
pub fn start(_app: AppHandle) -> Result<Arc<WebServerHandle>, String> {
    Ok(WebServerHandle::disabled())
}

fn bind_listener() -> Result<(TcpListener, u16), String> {
    for port in DEFAULT_PORT..DEFAULT_PORT + PORT_TRIES {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => return Ok((listener, port)),
            // 刚杀掉旧实例时端口可能尚未释放：短暂轮询首选端口再放弃，避免无谓顺延。
            Err(_) if port == DEFAULT_PORT => {
                for _ in 0..30 {
                    std::thread::sleep(Duration::from_millis(100));
                    if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
                        return Ok((listener, port));
                    }
                }
            }
            Err(_) => continue,
        }
    }
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| format!("绑定轻量模式端口失败：{error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    Ok((listener, port))
}

fn resolve_dist_dir(app: &AppHandle) -> PathBuf {
    let repo_dist = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../dist");
    // 开发调试(debug)：前端随时 npm run build，优先读仓库 dist，保证改动立即生效。
    // 打包(release)：资源拷贝打进 .app/Contents/Resources/dist，优先读它。
    if cfg!(debug_assertions) && repo_dist.join("index.html").is_file() {
        return repo_dist;
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(resource_dir) = app.path().resource_dir() {
        // 打包后的资源目录（tauri.conf.json 中把 ../dist 打进 resources）
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

fn generate_token() -> String {
    use sha2::{Digest, Sha256};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let mut hasher = Sha256::new();
    hasher.update(format!("openhub-{}-{}", nanos, std::process::id()));
    hex::encode(hasher.finalize())
}

fn serve_loop(listener: TcpListener, app: AppHandle, handle: Arc<WebServerHandle>) {
    // 非阻塞 accept + 轮询 running：stop() 后能及时退出循环并释放端口。
    let _ = listener.set_nonblocking(true);
    while handle.running.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let app = app.clone();
                let handle = handle.clone();
                std::thread::spawn(move || handle_connection(&app, &handle, stream));
            }
            Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    handle.running.store(false, Ordering::Relaxed);
}

/// 停止轻量模式 HTTP 服务：置 running=false，后台 accept 循环在下一轮退出并释放端口。
pub fn stop(handle: &Arc<WebServerHandle>) {
    handle.running.store(false, Ordering::Relaxed);
}

// —— 极简 HTTP/1.1 实现（只服务本机，单请求即断连） ——

struct HttpRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<HttpRequest> {
    let mut buffer = Vec::with_capacity(4096);
    let mut chunk = [0u8; 8192];
    let head_end;
    loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "连接提前关闭",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(index) = find_head_end(&buffer) {
            head_end = index;
            break;
        }
        if buffer.len() > 128 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "请求头过大",
            ));
        }
    }
    let head = String::from_utf8_lossy(&buffer[..head_end]);
    let mut lines = head.split("\r\n");
    let mut parts = lines.next().unwrap_or("").split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let content_length: usize = headers
        .get("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    while buffer.len() < head_end + 4 + content_length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    // 限制请求体大小（约 8MB），避免异常请求撑爆内存。
    let body = buffer[head_end + 4..].to_vec();
    if body.len() > 8 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "请求体过大",
        ));
    }
    Ok(HttpRequest {
        method,
        path,
        headers,
        body,
    })
}

fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    extra_headers: &[(&str, &str)],
) {
    let mut response = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n",
        status_text(status),
        body.len()
    );
    for (key, value) in extra_headers {
        response.push_str(&format!("{key}: {value}\r\n"));
    }
    response.push_str("\r\n");
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(body);
}

fn handle_connection(app: &AppHandle, handle: &WebServerHandle, mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(_) => return,
    };
    if request.method == "GET" {
        serve_get(&mut stream, handle, &request.path);
    } else if request.method == "POST" && request.path == "/api/rpc" {
        serve_rpc(app, handle, &mut stream, &request);
    } else {
        write_response(
            &mut stream,
            404,
            "text/plain; charset=utf-8",
            b"Not Found",
            &[],
        );
    }
}

fn serve_get(stream: &mut TcpStream, handle: &WebServerHandle, path: &str) {
    if path == "/api/rpc" || path.starts_with("/api/") {
        write_response(stream, 404, "text/plain; charset=utf-8", b"Not Found", &[]);
        return;
    }
    let dist_dir = handle
        .dist_dir
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    let relative = path.trim_start_matches('/');
    let relative = if relative.is_empty() {
        "index.html"
    } else {
        relative
    };
    let decoded = percent_decode(relative);
    let file_path = match safe_join(&dist_dir, &decoded) {
        Some(path) => path,
        None => {
            write_response(stream, 404, "text/plain; charset=utf-8", b"Not Found", &[]);
            return;
        }
    };
    if file_path.is_file() {
        match std::fs::read(&file_path) {
            Ok(content) => {
                write_response(stream, 200, content_type_for(&file_path), &content, &[]);
            }
            Err(_) => {
                write_response(
                    stream,
                    500,
                    "text/plain; charset=utf-8",
                    b"Internal Server Error",
                    &[],
                );
            }
        }
        return;
    }
    // SPA fallback：未知路径一律回退到 index.html。
    match std::fs::read(dist_dir.join("index.html")) {
        Ok(content) => write_response(
            stream,
            200,
            "text/html; charset=utf-8",
            &content,
            &[("Cache-Control", "no-store")],
        ),
        Err(_) => write_response(
            stream,
            503,
            "text/plain; charset=utf-8",
            "OpenHub 轻量模式：前端资源未就绪，请先执行 npm run build。".as_bytes(),
            &[],
        ),
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

// —— RPC 分发 ——

fn serve_rpc(
    app: &AppHandle,
    handle: &WebServerHandle,
    stream: &mut TcpStream,
    request: &HttpRequest,
) {
    let token_ok = {
        let expected = handle
            .token
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let provided = request
            .headers
            .get("x-openhub-token")
            .cloned()
            .unwrap_or_default();
        // 兼容 Authorization: Bearer <token>
        let provided = if provided.is_empty() {
            request
                .headers
                .get("authorization")
                .cloned()
                .unwrap_or_default()
                .strip_prefix("Bearer ")
                .map(|value| value.to_string())
                .unwrap_or_default()
        } else {
            provided
        };
        !expected.is_empty() && provided == expected
    };
    if !token_ok {
        let body =
            json!({ "error": "轻量模式令牌无效或缺失，请从设置页复制完整地址访问" }).to_string();
        write_response(
            stream,
            401,
            "application/json; charset=utf-8",
            body.as_bytes(),
            &[],
        );
        return;
    }
    let payload: Value = match serde_json::from_slice(&request.body) {
        Ok(value) => value,
        Err(error) => {
            let body = json!({ "error": format!("请求体解析失败：{error}") }).to_string();
            write_response(
                stream,
                400,
                "application/json; charset=utf-8",
                body.as_bytes(),
                &[],
            );
            return;
        }
    };
    let command = payload
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let args = payload.get("args").cloned().unwrap_or(Value::Null);
    let response = match dispatch(app, &command, &args) {
        Ok(data) => json!({ "data": data }),
        Err(error) => json!({ "error": error }),
    };
    let body = response.to_string();
    write_response(
        stream,
        200,
        "application/json; charset=utf-8",
        body.as_bytes(),
        &[],
    );
}

fn take<T: DeserializeOwned>(args: &Value, names: &[&str]) -> Result<T, String> {
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

fn take_opt<T: DeserializeOwned>(args: &Value, names: &[&str]) -> Result<Option<T>, String> {
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

fn take_string(args: &Value, names: &[&str]) -> Result<String, String> {
    take(args, names)
}

/// 轻量模式命令分发：与桌面端 IPC 调用同一套内核实现。
fn dispatch(app: &AppHandle, command: &str, args: &Value) -> Result<Value, String> {
    match command {
        // —— 站点库 ——
        "list_library" => Ok(json!(site_crud::list_library(app.state::<Database>()))),
        "create_site" => {
            let input: SiteRecord = take(args, &["input"])?;
            Ok(json!(site_crud::create_site(
                app.state::<Database>(),
                input
            )))
        }
        "update_site" => {
            let id = take_string(args, &["id"])?;
            let input: SiteRecord = take(args, &["input"])?;
            Ok(json!(site_crud::update_site(
                app.state::<Database>(),
                id,
                input
            )))
        }
        "delete_site" => {
            let id = take_string(args, &["id"])?;
            Ok(json!(site_crud::delete_site(app.state::<Database>(), id)))
        }
        "toggle_personal" => {
            let id = take_string(args, &["id"])?;
            Ok(json!(site_crud::toggle_personal(
                app.state::<Database>(),
                id
            )))
        }
        "toggle_pending" => {
            let id = take_string(args, &["id"])?;
            Ok(json!(site_crud::toggle_pending(
                app.state::<Database>(),
                id
            )))
        }
        "cycle_usage_state" => {
            let id = take_string(args, &["id"])?;
            Ok(json!(site_crud::cycle_usage_state(
                app.state::<Database>(),
                id
            )))
        }
        "set_usage_state" => {
            let id = take_string(args, &["id"])?;
            let state = take_string(args, &["state"])?;
            Ok(json!(site_crud::set_usage_state(
                app.state::<Database>(),
                id,
                state
            )))
        }
        "toggle_hidden" => {
            let id = take_string(args, &["id"])?;
            Ok(json!(site_crud::toggle_hidden(app.state::<Database>(), id)))
        }
        "toggle_runaway" => {
            let id = take_string(args, &["id"])?;
            Ok(json!(site_crud::toggle_runaway(
                app.state::<Database>(),
                id
            )))
        }
        "import_site" => {
            let site_url = take_string(args, &["siteUrl", "site_url"])?;
            let usage_state: Option<String> = take_opt(args, &["usageState", "usage_state"])?;
            let app = app.clone();
            Ok(json!(tauri::async_runtime::block_on(async move {
                site_crud::import_site(app.state::<Database>(), site_url, usage_state).await
            })))
        }
        "mark_sites_with_chrome_sessions" => {
            let site_id: Option<String> = take_opt(args, &["siteId", "site_id"])?;
            let site_ids: Option<Vec<String>> = take_opt(args, &["siteIds", "site_ids"])?;
            let run_id: Option<u64> = take_opt(args, &["runId", "run_id"])?;
            let extract_only: Option<bool> = take_opt(args, &["extractOnly", "extract_only"])?;
            let refresh_pending: Option<bool> =
                take_opt(args, &["refreshPending", "refresh_pending"])?;
            let app = app.clone();
            Ok(json!(tauri::async_runtime::block_on(async move {
                chrome_usage::mark_sites_with_chrome_sessions(
                    app.clone(),
                    app.state::<Database>(),
                    site_id,
                    site_ids,
                    run_id,
                    extract_only,
                    refresh_pending,
                )
                .await
            })))
        }

        // —— 代理池 ——
        "get_proxy_pool_state" => Ok(json!(proxy_pool::get_proxy_pool_state(
            app.state::<Database>(),
            app.state::<ProxyRuntime>(),
        ))),
        "analyze_proxy_nodes" => Ok(json!(proxy_pool::analyze_proxy_nodes(
            app.state::<Database>(),
            app.state::<ProxyRuntime>(),
        ))),
        "save_proxy_subscription" => {
            let id: Option<String> = take_opt(args, &["id"])?;
            let name = take_string(args, &["name"])?;
            let url = take_string(args, &["url"])?;
            Ok(json!(proxy_pool::save_proxy_subscription(
                app.state::<Database>(),
                id,
                name,
                url,
            )))
        }
        "delete_proxy_subscription" => {
            let id = take_string(args, &["id"])?;
            Ok(json!(proxy_pool::delete_proxy_subscription(
                app.state::<Database>(),
                app.state::<ProxyRuntime>(),
                id,
            )))
        }
        "refresh_proxy_subscription" => {
            let id = take_string(args, &["id"])?;
            let app = app.clone();
            Ok(json!(tauri::async_runtime::block_on(async move {
                proxy_pool::refresh_proxy_subscription(
                    app.clone(),
                    app.state::<Database>(),
                    app.state::<ProxyRuntime>(),
                    id,
                )
                .await
            })))
        }
        "set_proxy_pool_settings" => {
            let ignore_addresses = take_string(args, &["ignoreAddresses", "ignore_addresses"])?;
            Ok(json!(proxy_pool::set_proxy_pool_settings(
                app.state::<Database>(),
                app.state::<ProxyRuntime>(),
                ignore_addresses,
            )))
        }
        "save_proxy_channel" => {
            let id: Option<String> = take_opt(args, &["id"])?;
            let name = take_string(args, &["name"])?;
            Ok(json!(proxy_pool::save_proxy_channel(
                app.state::<Database>(),
                app.state::<ProxyRuntime>(),
                id,
                name,
            )))
        }
        "delete_proxy_channel" => {
            let id = take_string(args, &["id"])?;
            Ok(json!(proxy_pool::delete_proxy_channel(
                app.state::<Database>(),
                app.state::<ProxyRuntime>(),
                id,
            )))
        }
        "set_proxy_channel_node" => {
            let channel_id = take_string(args, &["channelId", "channel_id"])?;
            let node_id = take_string(args, &["nodeId", "node_id"])?;
            Ok(json!(proxy_pool::set_proxy_channel_node(
                app.state::<Database>(),
                app.state::<ProxyRuntime>(),
                channel_id,
                node_id,
            )))
        }
        "assign_account_proxy_channel" => {
            let profile_id = take_string(args, &["profileId", "profile_id"])?;
            let channel_id = take_string(args, &["channelId", "channel_id"])?;
            Ok(json!(proxy_pool::assign_account_proxy_channel(
                app.state::<Database>(),
                app.state::<ProxyRuntime>(),
                profile_id,
                channel_id,
            )))
        }
        "unassign_account_proxy_channel" => {
            let profile_id = take_string(args, &["profileId", "profile_id"])?;
            Ok(json!(proxy_pool::unassign_account_proxy_channel(
                app.state::<Database>(),
                app.state::<ProxyRuntime>(),
                profile_id,
            )))
        }
        "test_proxy_channel_nodes" => {
            let channel_id = take_string(args, &["channelId", "channel_id"])?;
            let app = app.clone();
            Ok(json!(tauri::async_runtime::block_on(async move {
                proxy_pool::test_proxy_channel_nodes(
                    app.clone(),
                    app.state::<Database>(),
                    app.state::<ProxyRuntime>(),
                    channel_id,
                )
                .await
            })))
        }
        "set_active_proxy_node" => {
            let node_id = take_string(args, &["nodeId", "node_id"])?;
            let app = app.clone();
            Ok(json!(tauri::async_runtime::block_on(async move {
                proxy_pool::set_active_proxy_node(
                    app.state::<Database>(),
                    app.state::<ProxyRuntime>(),
                    node_id,
                )
                .await
            })))
        }
        "clear_active_proxy_node" => Ok(json!(proxy_pool::clear_active_proxy_node(
            app.state::<Database>(),
            app.state::<ProxyRuntime>(),
        ))),
        "delete_invalid_proxy_nodes" => Ok(json!(proxy_pool::delete_invalid_proxy_nodes(
            app.state::<Database>(),
            app.state::<ProxyRuntime>(),
        ))),
        "test_proxy_node" => {
            let node_id = take_string(args, &["nodeId", "node_id"])?;
            let app = app.clone();
            Ok(json!(tauri::async_runtime::block_on(async move {
                proxy_pool::test_proxy_node(
                    app.state::<Database>(),
                    app.state::<ProxyRuntime>(),
                    node_id,
                )
                .await
            })))
        }
        "test_proxy_nodes" => {
            let node_ids: Vec<String> = take(args, &["nodeIds", "node_ids"])?;
            let app = app.clone();
            Ok(json!(tauri::async_runtime::block_on(async move {
                proxy_pool::test_proxy_nodes(
                    app.clone(),
                    app.state::<Database>(),
                    app.state::<ProxyRuntime>(),
                    node_ids,
                )
                .await
            })))
        }
        "test_all_proxy_nodes" => {
            let app = app.clone();
            Ok(json!(tauri::async_runtime::block_on(async move {
                proxy_pool::test_all_proxy_nodes(
                    app.clone(),
                    app.state::<Database>(),
                    app.state::<ProxyRuntime>(),
                )
                .await
            })))
        }
        "cancel_proxy_node_tests" => Ok(json!(proxy_pool::cancel_proxy_node_tests(
            app.state::<ProxyRuntime>(),
        ))),

        // —— Token 统计 ——
        "get_token_stats" => {
            let from: Option<String> = take_opt(args, &["from"])?;
            let to: Option<String> = take_opt(args, &["to"])?;
            let refresh: Option<bool> = take_opt(args, &["refresh"])?;
            let _ = refresh;
            Ok(json!(token_stats::query_token_stats(
                &*app.state::<Database>(),
                from,
                to,
            )))
        }
        "sync_token_data" => {
            let force: Option<bool> = take_opt(args, &["force"])?;
            Ok(json!(tauri::async_runtime::block_on(
                token_stats::sync_token_data(app.clone(), force)
            )))
        }
        "get_token_usage" => Ok(json!(token_stats::query_token_usage(
            &*app.state::<Database>()
        ))),
        "get_token_raw_logs" => Ok(json!(tauri::async_runtime::block_on(
            token_stats::get_token_raw_logs()
        ))),
        "get_token_request_health" => {
            let refresh: Option<bool> = take_opt(args, &["refresh"])?;
            let _ = refresh;
            Ok(json!(token_stats::query_token_health(
                &*app.state::<Database>()
            )))
        }
        "get_local_agent_paths" => Ok(json!(tauri::async_runtime::block_on(
            token_stats::get_local_agent_paths()
        ))),

        // —— 模型缓存 ——
        "get_system_fonts" => Ok(json!(models_fetch::get_system_fonts())),
        "get_site_model_cache" => {
            let site_id = take_string(args, &["siteId", "site_id"])?;
            Ok(json!(models_fetch::get_site_model_cache(
                app.state::<Database>(),
                site_id,
            )))
        }
        "get_all_site_model_caches" => Ok(json!(models_fetch::get_all_site_model_caches(
            app.state::<Database>(),
        ))),
        "clear_site_model_cache_for_site" => {
            let site_id = take_string(args, &["siteId", "site_id"])?;
            Ok(json!(models_fetch::clear_site_model_cache_for_site(
                app.state::<Database>(),
                site_id,
            )))
        }
        "save_site_model_cache_for_account" => {
            let site_id = take_string(args, &["siteId", "site_id"])?;
            let account: SiteModelCacheAccount = take(args, &["account"])?;
            let result: Option<SiteModelsResult> = take_opt(args, &["result"])?;
            let preserve_keys: Option<bool> = take_opt(args, &["preserveKeys", "preserve_keys"])?;
            Ok(json!(models_fetch::save_site_model_cache_for_account(
                app.state::<Database>(),
                site_id,
                account,
                result,
                preserve_keys,
            )))
        }
        "get_model_catalog" => Ok(json!(model_catalog::get_model_catalog_inner(
            &*app.state::<Database>()
        ))),
        "get_model_catalog_detail" => {
            let key = take_string(args, &["id", "canonicalKey", "canonical_key"])?;
            let db = app.state::<Database>();
            Ok(json!(tauri::async_runtime::block_on(async move {
                model_catalog::get_model_catalog_detail_inner(&*db, &key).await
            })?))
        }
        "sync_model_catalog" => {
            let force: Option<bool> = take_opt(args, &["force"])?;
            let app = app.clone();
            Ok(json!(tauri::async_runtime::block_on(async move {
                model_catalog::sync_model_catalog_inner(
                    &app,
                    &app.state::<Database>(),
                    &app.state::<model_catalog::ModelCatalogRuntime>(),
                    force.unwrap_or(false),
                )
                .await
            })))
        }
        "fetch_site_models_json" => {
            let url = take_string(args, &["url"])?;
            let site_id: Option<String> = take_opt(args, &["siteId", "site_id"])?;
            let profile_id: Option<String> = take_opt(args, &["profileId", "profile_id"])?;
            let app = app.clone();
            Ok(json!(tauri::async_runtime::block_on(async move {
                models_fetch::fetch_site_models_json(
                    app.clone(),
                    app.state::<Database>(),
                    url,
                    site_id,
                    profile_id,
                )
                .await
            })))
        }

        // —— 自动会话同步 ——
        "get_auto_sync_settings" => Ok(json!(auto_sync::get_auto_sync_settings(
            app.state::<Database>(),
        ))),
        "get_auto_sync_status" => Ok(json!(auto_sync::get_auto_sync_status(
            app.state::<Database>(),
        ))),
        "set_auto_sync_settings" => {
            let enabled: Option<bool> = take_opt(args, &["enabled"])?;
            let interval_minutes: Option<u64> =
                take_opt(args, &["intervalMinutes", "interval_minutes"])?;
            let app = app.clone();
            Ok(json!(auto_sync::set_auto_sync_settings(
                app.clone(),
                app.state::<Database>(),
                enabled,
                interval_minutes,
            )))
        }
        "request_auto_sync_round" => Ok(json!(auto_sync::request_auto_sync_round(app.clone(),))),

        // —— 公益监听 ——
        "get_charity_today_count" => Ok(json!(tauri::async_runtime::block_on(
            charity_monitor::get_charity_today_count(app.state::<Database>())
        ))),
        "get_charity_unread_total" => Ok(json!(tauri::async_runtime::block_on(
            charity_monitor::get_charity_unread_total(app.state::<Database>())
        ))),
        "clear_charity_sync_logs" => Ok(json!(tauri::async_runtime::block_on(
            charity_monitor::clear_charity_sync_logs(app.state::<Database>())
        ))),
        "list_charity_sources" => Ok(json!(tauri::async_runtime::block_on(
            charity_monitor::list_charity_sources(app.state::<Database>())
        ))),
        "add_charity_source" => {
            let id = take_string(args, &["id"])?;
            let name = take_string(args, &["name"])?;
            let json_url: Option<String> = take_opt(args, &["jsonUrl", "json_url"])?;
            Ok(json!(tauri::async_runtime::block_on(
                charity_monitor::add_charity_source(app.state::<Database>(), id, name, json_url)
            )))
        }
        "update_charity_source" => {
            let id = take_string(args, &["id"])?;
            let name: Option<String> = take_opt(args, &["name"])?;
            let json_url: Option<String> = take_opt(args, &["jsonUrl", "json_url"])?;
            let enabled: Option<bool> = take_opt(args, &["enabled"])?;
            Ok(json!(tauri::async_runtime::block_on(
                charity_monitor::update_charity_source(
                    app.state::<Database>(),
                    id,
                    name,
                    json_url,
                    enabled
                )
            )))
        }
        "remove_charity_source" => {
            let id = take_string(args, &["id"])?;
            Ok(json!(tauri::async_runtime::block_on(
                charity_monitor::remove_charity_source(app.state::<Database>(), id)
            )))
        }
        "mark_charity_feed_read" => {
            let feed_id: Option<String> = take_opt(args, &["feedId", "feed_id"])?;
            Ok(json!(tauri::async_runtime::block_on(
                charity_monitor::mark_charity_feed_read(app.state::<Database>(), feed_id)
            )))
        }
        "get_charity_feed" => {
            let feed_id: Option<String> = take_opt(args, &["feedId", "feed_id"])?;
            let offset: Option<usize> = take_opt(args, &["offset"])?;
            let limit: Option<usize> = take_opt(args, &["limit"])?;
            let keyword: Option<String> = take_opt(args, &["keyword"])?;
            let app = app.clone();
            Ok(json!(tauri::async_runtime::block_on(async move {
                charity_monitor::get_charity_feed(
                    app.state::<Database>(),
                    app.state::<CharityMonitorRuntime>(),
                    feed_id,
                    offset,
                    limit,
                    keyword,
                )
                .await
            })))
        }
        "get_charity_proxy_pool_summary" => Ok(json!(tauri::async_runtime::block_on(
            charity_monitor::get_charity_proxy_pool_summary(
                app.state::<Database>(),
                app.state::<CharityMonitorRuntime>(),
            )
        ))),
        "get_charity_sync_logs" => {
            let limit: Option<usize> = take_opt(args, &["limit"])?;
            Ok(json!(tauri::async_runtime::block_on(
                charity_monitor::get_charity_sync_logs(app.state::<Database>(), limit)
            )))
        }
        "refresh_all_charity_feeds" => Ok(json!(tauri::async_runtime::block_on(
            charity_monitor::refresh_all_charity_feeds(
                app.state::<Database>(),
                app.state::<CharityMonitorRuntime>(),
            )
        ))),
        "request_charity_round" => Ok(json!(charity_monitor::request_charity_round(
            app.state::<CharityMonitorRuntime>(),
        ))),
        "set_charity_monitor_visible" => {
            let visible: bool = take(args, &["visible"])?;
            Ok(json!(charity_monitor::set_charity_monitor_visible(
                app.state::<CharityMonitorRuntime>(),
                visible,
            )))
        }
        "fetch_charity_feed" => {
            let feed_id: Option<String> = take_opt(args, &["feedId", "feed_id"])?;
            let app = app.clone();
            Ok(json!(tauri::async_runtime::block_on(async move {
                charity_monitor::fetch_charity_feed(
                    app.clone(),
                    app.state::<Database>(),
                    app.state::<ProxyRuntime>(),
                    app.state::<CharityMonitorRuntime>(),
                    feed_id,
                )
                .await
            })))
        }

        // —— 轻量模式自身 ——
        "get_lightweight_mode_state" => Ok(json!(lightweight_state(&app)?)),
        "enter_lightweight_mode" => Ok(json!(enter_lightweight_mode(app.clone())?)),
        "show_main_window" => Ok(json!(show_main_window(app.clone())?)),

        _ => Err(format!("轻量模式暂不支持命令：{command}")),
    }
}

// —— 轻量模式命令（同时作为 Tauri IPC 命令注册） ——

#[tauri::command]
pub fn get_lightweight_mode_state(app: AppHandle) -> Result<LightweightState, String> {
    lightweight_state(&app)
}

fn lightweight_state(app: &AppHandle) -> Result<LightweightState, String> {
    let server = app.state::<Arc<WebServerHandle>>();
    let running = server.running.load(Ordering::Relaxed);
    let port = server.port.load(Ordering::Relaxed);
    let token = server
        .token
        .lock()
        .map_err(|_| "轻量模式服务令牌读取失败".to_string())?
        .clone();
    let enabled = meta_get(&app.state::<Database>(), LIGHTWEIGHT_META_KEY)?
        .map(|value| value == "1")
        .unwrap_or(false);
    let url = if running {
        format!("http://127.0.0.1:{port}/?token={token}")
    } else {
        String::new()
    };
    Ok(LightweightState {
        running,
        port,
        enabled,
        url,
    })
}

/// 一键轻量模式：隐藏 GUI 窗口（进程不退出），打开浏览器访问。
#[tauri::command]
pub fn enter_lightweight_mode(app: AppHandle) -> Result<LightweightState, String> {
    let state = lightweight_state(&app)?;
    if !state.running {
        return Err("轻量模式服务未运行，请重启应用后重试".to_string());
    }
    meta_set(&app.state::<Database>(), LIGHTWEIGHT_META_KEY, "1")?;
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
    open_in_browser(&app, &state.url);
    // state 在写入持久化之前计算，enabled 字段手动纠正为 true。
    Ok(LightweightState {
        enabled: true,
        ..state
    })
}

/// 从浏览器侧唤出桌面窗口，等同退出轻量模式。
#[tauri::command]
pub fn show_main_window(app: AppHandle) -> Result<(), String> {
    let _ = meta_set(&app.state::<Database>(), LIGHTWEIGHT_META_KEY, "0");
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    Ok(())
}

/// 启动时若上次处于轻量模式，自动隐藏窗口并打开浏览器。
pub fn apply_startup_lightweight_mode(app: &AppHandle) {
    let enabled = meta_get(&app.state::<Database>(), LIGHTWEIGHT_META_KEY)
        .map(|value| value.map(|v| v == "1").unwrap_or(false))
        .unwrap_or(false);
    if !enabled {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(600));
        let state = match lightweight_state(&app) {
            Ok(state) => state,
            Err(_) => return,
        };
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.hide();
        }
        open_in_browser(&app, &state.url);
    });
}

fn open_in_browser(app: &AppHandle, url: &str) {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    if let Err(error) = result {
        eprintln!("OpenHub 打开浏览器失败：{error}");
        let _ = app;
    }
}

// —— 持久化（app_meta 表） ——

fn meta_get(database: &Database, key: &str) -> Result<Option<String>, String> {
    use rusqlite::OptionalExtension;
    let connection = database
        .0
        .lock()
        .map_err(|_| "本地数据库锁定失败".to_string())?;
    connection
        .query_row("SELECT value FROM app_meta WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()
        .map_err(|error| error.to_string())
}

fn meta_set(database: &Database, key: &str, value: &str) -> Result<(), String> {
    let connection = database
        .0
        .lock()
        .map_err(|_| "本地数据库锁定失败".to_string())?;
    connection
        .execute(
            "INSERT OR REPLACE INTO app_meta (key, value) VALUES (?1, ?2)",
            [key, value],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}
