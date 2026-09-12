use crate::context::home_dir;
#[allow(unused_imports)]
use crate::context::spawn_blocking;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    ffi::{c_char, c_void},
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use url::Url;

const CHROME_EPOCH_OFFSET_SECONDS: i64 = 11_644_473_600;

/// 带进程级死线的 osascript 调用：AppleEvent 通道拥塞时 `output()` 会无限期
/// 阻塞，阶段自身的轮询预算拦不住，整个同步会被 90 秒总超时连坐强杀。
/// 超时即 kill 子进程并返回 Err（错误文本含"AppleEvent已超时"，归入瞬态
/// 错误，由调用方继续轮询直到阶段预算耗尽）。
#[cfg(target_os = "macos")]
fn run_osascript_with_deadline(
    mut command: Command,
    deadline: Duration,
) -> Result<std::process::Output, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动 osascript：{error}"))?;
    let stdout_reader = child.stdout.take().map(|mut pipe| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });
    let stderr_reader = child.stderr.take().map(|mut pipe| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_reader.map(|handle| handle.join());
                    let _ = stderr_reader.map(|handle| handle.join());
                    return Err("osascript AppleEvent已超时，已终止本次调用".to_string());
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(format!("osascript 状态读取失败：{error}")),
        }
    };
    Ok(std::process::Output {
        status,
        stdout: stdout_reader
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default(),
        stderr: stderr_reader
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default(),
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChromeSessionInfo {
    pub(crate) profile_id: String,
    pub(crate) domain: String,
    pub(crate) cookie_count: usize,
    pub(crate) cookie_names: Vec<String>,
    pub(crate) profile_name: String,
    pub(crate) account_name: String,
    pub(crate) username: String,
    pub(crate) api_key_count: usize,
    pub(crate) api_model_count: usize,
    pub(crate) api_counts_synced: bool,
    pub(crate) api_sync_error: String,
    pub(crate) has_access_token: bool,
    pub(crate) remaining: Option<f64>,
    pub(crate) used: Option<f64>,
    pub(crate) total: Option<f64>,
    pub(crate) unit: String,
    pub(crate) is_valid: bool,
    pub(crate) sync_error: String,
    pub(crate) checkin_enabled: bool,
    pub(crate) checked_in_today: bool,
    pub(crate) checkin_error: String,
    pub(crate) account_updated_at: String,
    /// 浏览器兜底剩余冷却毫秒数（由 failed_at + 连续失败次数算出，读取时计算）。
    /// 前端据此决定自动重试是否跳过浏览器；手动按钮不受限制。
    pub(crate) browser_fallback_cooldown_ms: i64,
    #[serde(skip)]
    pub(crate) newapi_token: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) newapi_user_id: String,
    /// 浏览器兜底冷却的原始持久化值：扫描流程 DELETE+INSERT 重建 site_accounts
    /// 时原样写回，避免一次扫描就把退避状态清零。
    #[serde(skip)]
    pub(crate) browser_fallback_failed_at: i64,
    #[serde(skip)]
    pub(crate) browser_fallback_fail_count: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChromeSiteSessionMatch {
    pub(crate) site_id: String,
    pub(crate) sessions: Vec<ChromeSessionInfo>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChromeSessionValue {
    domain: String,
    cookie: String,
    cookie_count: usize,
    profile_name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedChromeSession {
    pub(crate) profile_id: String,
    pub(crate) profile_name: String,
    pub(crate) account_name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenUrlInChromeSessionsResult {
    pub(crate) opened: usize,
    pub(crate) attempted: usize,
    pub(crate) profiles: Vec<OpenedChromeSession>,
    pub(crate) errors: Vec<String>,
}

impl OpenUrlInChromeSessionsResult {
    fn empty() -> Self {
        Self {
            opened: 0,
            attempted: 0,
            profiles: Vec::new(),
            errors: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct ChromeCookieSession {
    pub(crate) profile_name: String,
    pub(crate) account_name: String,
    pub(crate) cookie_header: String,
}

#[derive(Debug)]
struct ChromeProfile {
    id: String,
    name: String,
    account_name: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ChromeProfileIdentity {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) account_name: String,
}

#[derive(Debug)]
struct ChromeCookie {
    host: String,
    name: String,
    value: String,
    encrypted_value: Vec<u8>,
    path: String,
    expires_utc: i64,
    secure: bool,
}

#[cfg(target_os = "macos")]
struct ChromeContext {
    root: PathBuf,
    url: Url,
    domain: String,
    profiles: Vec<ChromeProfile>,
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn list_chrome_sessions(url: String) -> Result<Vec<ChromeSessionInfo>, String> {
    let home_dir = home_dir().ok_or("无法定位用户目录")?;

    spawn_blocking(move || list_chrome_sessions_from_home(&home_dir, &url))
        .await
        .map_err(|error| format!("读取 Chrome 会话任务失败：{error}"))?
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn read_chrome_session(
    url: String,
    profile_id: String,
) -> Result<ChromeSessionValue, String> {
    let home_dir = home_dir().ok_or("无法定位用户目录")?;

    spawn_blocking(move || read_chrome_session_from_home(&home_dir, &url, &profile_id))
        .await
        .map_err(|error| format!("读取 Chrome 会话任务失败：{error}"))?
}

pub(crate) fn read_chrome_cookie_header_from_home(
    home_dir: &Path,
    target_url: &str,
    profile_id: &str,
) -> Result<String, String> {
    read_chrome_session_from_home(home_dir, target_url, profile_id).map(|value| value.cookie)
}

fn chrome_user_agent_for_version(version: &str) -> String {
    let major = version
        .split('.')
        .next()
        .filter(|value| {
            !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
        })
        .unwrap_or("120");
    format!(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
         (KHTML, like Gecko) Chrome/{major}.0.0.0 Safari/537.36"
    )
}

#[cfg(target_os = "macos")]
pub(crate) fn chrome_user_agent() -> String {
    let version = Command::new("/usr/libexec/PlistBuddy")
        .args([
            "-c",
            "Print :CFBundleShortVersionString",
            "/Applications/Google Chrome.app/Contents/Info.plist",
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .unwrap_or_default();
    chrome_user_agent_for_version(version.trim())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn chrome_user_agent() -> String {
    chrome_user_agent_for_version("")
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn open_url_in_chrome_profile(url: String, profile_id: String) -> Result<(), String> {
    spawn_blocking(move || open_url_in_chrome_profile_blocking(&url, &profile_id))
        .await
        .map_err(|error| format!("启动 Chrome 任务失败：{error}"))?
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn open_url_in_chrome_sessions(
    url: String,
) -> Result<OpenUrlInChromeSessionsResult, String> {
    let home_dir = home_dir().ok_or("无法定位用户目录")?;
    spawn_blocking(move || open_url_in_chrome_sessions_from_home(&home_dir, &url))
        .await
        .map_err(|error| format!("启动 Chrome 任务失败：{error}"))?
}

const CHROME_BRIDGE_TAB_NOT_FOUND: &str = "__OPENHUB_TAB_NOT_FOUND__";
const CHROME_BRIDGE_PENDING: &str = "__OPENHUB_PENDING__";
const CHROME_BRIDGE_TAB_PENDING_PREFIX: &str = "__OPENHUB_TAB_PENDING__:";
const CHROME_BRIDGE_PROFILE_MISMATCH: &str = "__OPENHUB_PROFILE_MISMATCH__";

/// 可见 / 后台同步标签的 URL fragment 前缀，与 sync.rs 生成的 marker 保持一致。
/// 用于识别“本应用此前同步遗留的标签”，复用而不是再开一个新标签。
#[cfg(target_os = "macos")]
const VISIBLE_BRIDGE_MARKER_PREFIX: &str = "openhub-sync-";
#[cfg(target_os = "macos")]
const BACKGROUND_BRIDGE_MARKER_PREFIX: &str = "openhub-background-";

fn chrome_tab_id_from_pending(value: &str) -> Option<&str> {
    value
        .strip_prefix(CHROME_BRIDGE_TAB_PENDING_PREFIX)
        .filter(|tab_id| {
            !tab_id.is_empty() && tab_id.chars().all(|character| character.is_ascii_digit())
        })
}

fn is_transient_chrome_automation_error(error: &str) -> bool {
    error.contains("-609")
        || error.contains("-600")
        || error.contains("-1719")
        || error.contains("-1728")
        // -1712：AppleEvent 超时。页面尚在加载、被内存省电休眠或 Chrome 忙时
        // execute javascript 会超时，属于可重试的瞬态错误；直接判定失败会让
        // 一个慢标签页中断整轮同步，并连锁触发反复打开新标签页兜底。
        || error.contains("-1712")
        || error.contains("AppleEvent已超时")
        || error.contains("连接无效")
        || error.contains("无效的索引")
        || error.contains("Connection is invalid")
        || error.contains("connection is invalid")
        || error.contains("Invalid index")
        || error.contains("invalid index")
        || error.contains("Application isn’t running")
        || error.contains("Application isn't running")
        || error.contains("应用程序没有运行")
}

pub(crate) fn is_blocking_chrome_automation_error(error: &str) -> bool {
    error.contains("Chrome 已关闭 Apple Events JavaScript")
        || error.contains("JavaScript from Apple Events")
        || error.contains("Apple Events 的 JavaScript")
        || error.contains("macOS 未允许 OpenHub 控制 Chrome")
        || error.contains("not authorized to send Apple events")
        || error.contains("不允许发送 Apple 事件")
}

#[cfg(target_os = "macos")]
pub(crate) fn run_javascript_in_existing_chrome_tab(
    target_url: &str,
    javascript: &str,
    timeout: Duration,
) -> Result<Option<String>, String> {
    let target_url = validated_external_url(target_url)?;
    let target_origin = target_url.origin().ascii_serialization();
    if target_origin == "null" {
        return Err("Chrome 静默请求地址缺少有效来源".into());
    }

    const SCRIPT: &str = r#"
on run argv
    set targetOrigin to item 1 of argv
    set sourceCode to item 2 of argv
    set targetTabId to item 3 of argv
    if application "Google Chrome" is not running then return "__OPENHUB_TAB_NOT_FOUND__"
    tell application "Google Chrome"
        set browserWindowCount to count of windows
        repeat with windowIndex from 1 to browserWindowCount
            try
                set browserTabCount to count of tabs of window windowIndex
                repeat with tabIndex from 1 to browserTabCount
                    try
                        set browserTab to tab tabIndex of window windowIndex
                        set currentTabId to (id of browserTab) as text
                        if (targetTabId is "") or (currentTabId is equal to targetTabId) then
                            with timeout of 3 seconds
                                set tabOrigin to execute browserTab javascript "window.location.origin"
                            end timeout
                            if tabOrigin is equal to targetOrigin then
                                with timeout of 3 seconds
                                    set scriptResult to execute browserTab javascript sourceCode
                                end timeout
                                if scriptResult is not "__OPENHUB_PROFILE_MISMATCH__" then
                                    if scriptResult is missing value or scriptResult is "__OPENHUB_PENDING__" then return "__OPENHUB_TAB_PENDING__:" & currentTabId
                                    return scriptResult as text
                                end if
                            end if
                        end if
                    on error errorMessage number errorNumber
                        if errorNumber is not -1719 and errorNumber is not -1728 and errorNumber is not -1712 then
                            error errorMessage number errorNumber
                        end if
                    end try
                end repeat
            on error errorMessage number errorNumber
                if errorNumber is not -1719 and errorNumber is not -1728 and errorNumber is not -1712 then
                    error errorMessage number errorNumber
                end if
            end try
        end repeat
    end tell
    return "__OPENHUB_TAB_NOT_FOUND__"
end run
"#;

    let started = Instant::now();
    let mut target_tab_id = String::new();
    while started.elapsed() < timeout {
        let mut command = Command::new("/usr/bin/osascript");
        command.args([
            "-e",
            SCRIPT,
            "--",
            &target_origin,
            javascript,
            &target_tab_id,
        ]);
        let output = match run_osascript_with_deadline(command, Duration::from_secs(8)) {
            Ok(output) => output,
            Err(deadline_error) => {
                // 死线强杀（AppleEvent 通道超时）与 stderr 里的 -1712 同为
                // 瞬态错误：稍候重试，直到静默阶段预算耗尽再交给上层降级。
                if is_transient_chrome_automation_error(&deadline_error) {
                    thread::sleep(Duration::from_millis(300));
                    continue;
                }
                return Err(format!("无法调用 Chrome 静默自动化：{deadline_error}"));
            }
        };
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if error.contains("JavaScript from Apple Events")
                || error.contains("Apple Events 的 JavaScript")
            {
                return Err(
                    "Chrome 已关闭 Apple Events JavaScript；请在 Chrome 的“视图 → 开发者”菜单中开启后重试"
                        .into(),
                );
            }
            if error.contains("-1743")
                || error.contains("not authorized to send Apple events")
                || error.contains("不允许发送 Apple 事件")
            {
                return Err(
                    "macOS 未允许 OpenHub 控制 Chrome；请在“系统设置 → 隐私与安全性 → 自动化”中授权后重试"
                        .into(),
                );
            }
            // -1712 已归入瞬态错误：不在此处致命返回，由外层轮询重试到整体超时。
            if is_transient_chrome_automation_error(&error) {
                thread::sleep(Duration::from_millis(300));
                continue;
            }
            return Err(if error.is_empty() {
                "Chrome 静默自动化执行失败".into()
            } else {
                format!("Chrome 静默自动化执行失败：{error}")
            });
        }
        let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if result == CHROME_BRIDGE_TAB_NOT_FOUND {
            return Ok(None);
        }
        if let Some(tab_id) = chrome_tab_id_from_pending(&result) {
            target_tab_id = tab_id.to_string();
            thread::sleep(Duration::from_millis(300));
            continue;
        }
        if !matches!(result.as_str(), "" | CHROME_BRIDGE_PENDING)
            && result != CHROME_BRIDGE_PROFILE_MISMATCH
        {
            return Ok(Some(result));
        }
        thread::sleep(Duration::from_millis(300));
    }
    Err("等待已打开的 Chrome 页面返回数据超时".into())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn run_javascript_in_existing_chrome_tab(
    _target_url: &str,
    _javascript: &str,
    _timeout: Duration,
) -> Result<Option<String>, String> {
    Ok(None)
}

#[cfg(target_os = "macos")]
pub(crate) fn run_javascript_in_chrome_profile(
    target_url: &str,
    profile_id: &str,
    marker: &str,
    javascript: &str,
    timeout: Duration,
    proxy_url: Option<&str>,
    allow_tab_reuse: bool,
) -> Result<String, String> {
    validate_chrome_bridge_marker(marker)?;
    // 上一次同步可能已经为该站点开过验证标签（被总超时掐断、或手动同步遗留）。
    // 优先复用，不再新开一个同样的标签，避免 Chrome 里反复堆积站点标签。
    // 仅在 bridge 脚本能用 Local Storage 用户 ID 核对账号时才允许复用，
    // 否则可能把账号请求注入其他 Chrome 账号的标签页。
    if allow_tab_reuse {
        if let Some(tab_id) = find_openhub_bridge_tab(target_url, VISIBLE_BRIDGE_MARKER_PREFIX) {
            // 不新开标签，但仍把 Chrome 带到前台，保留“可见验证”的语义。
            let _ = Command::new("/usr/bin/open")
                .args(["-a", "Google Chrome"])
                .output();
            match run_javascript_in_marked_chrome_tab(marker, javascript, Some(&tab_id), timeout) {
                Ok(value) if value == CHROME_BRIDGE_PROFILE_MISMATCH => {}
                outcome => return outcome,
            }
        }
    }
    let existing_tab_ids = chrome_tab_ids();
    open_url_in_chrome_profile_blocking_with_mode(target_url, profile_id, false, proxy_url, false)?;
    let target_tab_id =
        wait_for_new_chrome_tab(&existing_tab_ids, target_url, Duration::from_secs(8));
    run_javascript_in_marked_chrome_tab(marker, javascript, target_tab_id.as_deref(), timeout)
}

#[cfg(target_os = "macos")]
pub(crate) fn run_javascript_in_background_chrome_profile(
    target_url: &str,
    profile_id: &str,
    marker: &str,
    javascript: &str,
    timeout: Duration,
    proxy_url: Option<&str>,
    allow_tab_reuse: bool,
) -> Result<String, String> {
    validate_chrome_bridge_marker(marker)?;
    if allow_tab_reuse {
        if let Some(tab_id) = find_openhub_bridge_tab(target_url, BACKGROUND_BRIDGE_MARKER_PREFIX) {
            match run_javascript_in_marked_chrome_tab(marker, javascript, Some(&tab_id), timeout) {
                Ok(value) if value == CHROME_BRIDGE_PROFILE_MISMATCH => {}
                outcome => {
                    close_chrome_bridge_tabs(Some(&tab_id), marker);
                    return outcome;
                }
            }
        }
    }
    let existing_tab_ids = chrome_tab_ids();
    open_url_in_chrome_profile_blocking_with_mode(target_url, profile_id, true, proxy_url, false)?;
    let target_tab_id =
        wait_for_new_chrome_tab(&existing_tab_ids, target_url, Duration::from_secs(8));
    let result =
        run_javascript_in_marked_chrome_tab(marker, javascript, target_tab_id.as_deref(), timeout);
    close_chrome_bridge_tabs(target_tab_id.as_deref(), marker);
    result
}

#[cfg(target_os = "macos")]
fn validate_chrome_bridge_marker(marker: &str) -> Result<(), String> {
    if marker.is_empty()
        || !marker
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return Err("Chrome 同步标识无效".into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn run_javascript_in_marked_chrome_tab(
    marker: &str,
    javascript: &str,
    initial_tab_id: Option<&str>,
    timeout: Duration,
) -> Result<String, String> {
    const SCRIPT: &str = r#"
on run argv
    set targetMarker to item 1 of argv
    set sourceCode to item 2 of argv
    set targetTabId to item 3 of argv
    tell application "Google Chrome"
        set browserWindowCount to count of windows
        repeat with windowIndex from 1 to browserWindowCount
            try
                set browserTabCount to count of tabs of window windowIndex
                repeat with tabIndex from 1 to browserTabCount
                    try
                        set browserTab to tab tabIndex of window windowIndex
                        set tabUrl to URL of browserTab
                        set currentTabId to (id of browserTab) as text
                        if ((targetTabId is not "") and (currentTabId is equal to targetTabId)) or (tabUrl contains targetMarker) then
                            with timeout of 3 seconds
                                set scriptResult to execute browserTab javascript sourceCode
                            end timeout
                            if scriptResult is missing value or scriptResult is "__OPENHUB_PENDING__" then return "__OPENHUB_TAB_PENDING__:" & currentTabId
                            return scriptResult as text
                        end if
                    on error errorMessage number errorNumber
                        if errorNumber is not -1719 and errorNumber is not -1728 and errorNumber is not -1712 then
                            error errorMessage number errorNumber
                        end if
                    end try
                end repeat
            on error errorMessage number errorNumber
                if errorNumber is not -1719 and errorNumber is not -1728 and errorNumber is not -1712 then
                    error errorMessage number errorNumber
                end if
            end try
        end repeat
    end tell
    return "__OPENHUB_TAB_NOT_FOUND__"
end run
"#;

    let started = Instant::now();
    let mut target_tab_id = initial_tab_id.unwrap_or_default().to_string();
    while started.elapsed() < timeout {
        let mut command = Command::new("/usr/bin/osascript");
        command.args(["-e", SCRIPT, "--", marker, javascript, &target_tab_id]);
        let output = match run_osascript_with_deadline(command, Duration::from_secs(8)) {
            Ok(output) => output,
            Err(deadline_error) => {
                // 死线强杀（AppleEvent 通道超时）与 -1712 同为瞬态错误：
                // 稍候重试，直到当前阶段预算耗尽。
                if is_transient_chrome_automation_error(&deadline_error) {
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
                return Err(format!("无法调用 Chrome 自动化：{deadline_error}"));
            }
        };
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if error.contains("JavaScript from Apple Events")
                || error.contains("Apple Events 的 JavaScript")
            {
                return Err(
                    "Chrome 已关闭 Apple Events JavaScript；请在 Chrome 的“视图 → 开发者”菜单中开启后重试"
                        .into(),
                );
            }
            if error.contains("-1743")
                || error.contains("not authorized to send Apple events")
                || error.contains("不允许发送 Apple 事件")
            {
                return Err(
                    "macOS 未允许 OpenHub 控制 Chrome；请在“系统设置 → 隐私与安全性 → 自动化”中授权后重试"
                        .into(),
                );
            }
            // -1712 已归入瞬态错误：不在此处致命返回，由外层轮询重试到整体超时。
            if is_transient_chrome_automation_error(&error) {
                thread::sleep(Duration::from_millis(500));
                continue;
            }
            return Err(if error.is_empty() {
                "Chrome 自动化执行失败".into()
            } else {
                format!("Chrome 自动化执行失败：{error}")
            });
        }
        let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if let Some(tab_id) = chrome_tab_id_from_pending(&result) {
            target_tab_id = tab_id.to_string();
            // 轮询间隔 200ms：osascript 本身有百毫秒级开销，过长间隔会让
            // 已完成的桥接脚本多等一轮，直接拖慢账号同步。
            thread::sleep(Duration::from_millis(200));
            continue;
        }
        if !matches!(
            result.as_str(),
            "" | CHROME_BRIDGE_TAB_NOT_FOUND | CHROME_BRIDGE_PENDING
        ) {
            return Ok(result);
        }
        thread::sleep(Duration::from_millis(200));
    }
    Err("等待 Chrome 返回账号数据超时；请完成页面验证后重试".into())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChromeWindowSnapshot {
    window_id: String,
    tabs: Vec<(String, String)>,
}

/// 仅测试覆盖：公益打开已改为 `open_url_in_chrome_profile_blocking`，
/// 不再走 AppleScript 标签复用规划。
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChromeSessionOpenPlan {
    ActivateTab { tab_id: String, window_id: String },
    NewTabInWindow { window_id: String },
    Launch { new_window: bool },
}

fn parse_chrome_window_snapshot_line(line: &str) -> Option<(String, String, String)> {
    let mut parts = line.splitn(3, '\t');
    let window_id = parts.next()?.trim();
    let tab_id = parts.next()?.trim();
    let url = parts.next().unwrap_or("").trim();
    if window_id.is_empty()
        || tab_id.is_empty()
        || !window_id
            .chars()
            .all(|character| character.is_ascii_digit())
        || !tab_id.chars().all(|character| character.is_ascii_digit())
    {
        return None;
    }
    Some((window_id.to_string(), tab_id.to_string(), url.to_string()))
}

fn group_chrome_window_snapshots(
    lines: impl IntoIterator<Item = (String, String, String)>,
) -> Vec<ChromeWindowSnapshot> {
    let mut windows: Vec<ChromeWindowSnapshot> = Vec::new();
    for (window_id, tab_id, url) in lines {
        match windows.last_mut() {
            Some(window) if window.window_id == window_id => {
                window.tabs.push((tab_id, url));
            }
            _ => windows.push(ChromeWindowSnapshot {
                window_id,
                tabs: vec![(tab_id, url)],
            }),
        }
    }
    windows
}

fn normalize_chrome_reuse_url(url: &str) -> String {
    let Ok(parsed) = Url::parse(url.trim()) else {
        return url.trim().trim_end_matches('/').to_string();
    };
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    let mut path = parsed.path().to_string();
    if path.len() > 1 {
        path = path.trim_end_matches('/').to_string();
    }
    format!("{}://{host}{path}", parsed.scheme())
}

#[cfg(test)]
fn chrome_tab_url_matches(existing: &str, target: &str) -> bool {
    !existing.is_empty()
        && normalize_chrome_reuse_url(existing) == normalize_chrome_reuse_url(target)
}

#[cfg(test)]
fn is_linuxdo_tab_url(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url.trim()) else {
        return false;
    };
    parsed.host_str().is_some_and(|host| {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        host == "linux.do" || host == "www.linux.do"
    })
}

#[cfg(test)]
fn preferred_reuse_tab(window: &ChromeWindowSnapshot, target_url: &str) -> Option<(String, bool)> {
    if let Some((tab_id, _)) = window
        .tabs
        .iter()
        .find(|(_, url)| chrome_tab_url_matches(url, target_url))
    {
        return Some((tab_id.clone(), true));
    }
    window
        .tabs
        .iter()
        .find(|(_, url)| is_linuxdo_tab_url(url))
        .map(|(tab_id, _)| (tab_id.clone(), false))
}

fn parse_chrome_singleton_lock_pid(target: &str) -> Option<u32> {
    let name = Path::new(target)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(target);
    let pid = name.rsplit('-').next()?.parse::<u32>().ok()?;
    (pid > 0).then_some(pid)
}

fn extract_http_urls_from_bytes(data: &[u8]) -> HashSet<String> {
    let mut urls = HashSet::new();
    let mut index = 0;
    while index < data.len() {
        let rest = &data[index..];
        let https = rest.starts_with(b"https://");
        let http = rest.starts_with(b"http://");
        if !https && !http {
            index += 1;
            continue;
        }
        let mut end = if https { 8 } else { 7 };
        while index + end < data.len() {
            let byte = data[index + end];
            if byte <= 0x20 || byte >= 0x7f || matches!(byte, b'"' | b'\'' | b'<' | b'>' | b'\\') {
                break;
            }
            end += 1;
            if end > 2048 {
                break;
            }
        }
        if let Ok(url) = std::str::from_utf8(&data[index..index + end]) {
            if Url::parse(url).is_ok() {
                urls.insert(normalize_chrome_reuse_url(url));
            }
        }
        index += end.max(1);
    }
    urls
}

#[cfg(test)]
const SNSS_COMMAND_SET_TAB_WINDOW: u8 = 0;
#[cfg(test)]
const SNSS_COMMAND_TAB_CLOSED: u8 = 3;
#[cfg(test)]
const SNSS_COMMAND_WINDOW_CLOSED: u8 = 4;
#[cfg(test)]
const SNSS_COMMAND_TAB_CLOSED2: u8 = 16;
#[cfg(test)]
const SNSS_COMMAND_WINDOW_CLOSED2: u8 = 17;

#[cfg(test)]
fn snss_i32(payload: &[u8]) -> Option<i32> {
    payload.get(..4)?.try_into().ok().map(i32::from_le_bytes)
}

#[cfg(test)]
struct SnssCommandIter<'a> {
    data: &'a [u8],
    offset: usize,
}

#[cfg(test)]
impl<'a> SnssCommandIter<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }
}

#[cfg(test)]
impl<'a> Iterator for SnssCommandIter<'a> {
    type Item = (u8, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset == 0 {
            if self.data.len() < 8 || self.data.get(..4) != Some(b"SNSS".as_slice()) {
                return None;
            }
            self.offset = 8;
        }
        if self.offset + 3 > self.data.len() {
            return None;
        }
        let size =
            u16::from_le_bytes(self.data[self.offset..self.offset + 2].try_into().ok()?) as usize;
        self.offset += 2;
        if size == 0 || self.offset + size > self.data.len() {
            self.offset = self.data.len();
            return None;
        }
        let command_id = self.data[self.offset];
        let payload = &self.data[self.offset + 1..self.offset + size];
        self.offset += size;
        Some((command_id, payload))
    }
}

#[cfg(test)]
fn parse_chrome_session_open_window_ids(data: &[u8]) -> HashSet<String> {
    let mut windows: HashMap<i32, HashSet<i32>> = HashMap::new();
    for (command_id, payload) in SnssCommandIter::new(data) {
        match command_id {
            SNSS_COMMAND_SET_TAB_WINDOW => {
                let Some(window_id) = snss_i32(payload) else {
                    continue;
                };
                let Some(tab_id) = snss_i32(payload.get(4..).unwrap_or(&[])) else {
                    continue;
                };
                windows.entry(window_id).or_default().insert(tab_id);
            }
            SNSS_COMMAND_WINDOW_CLOSED | SNSS_COMMAND_WINDOW_CLOSED2 => {
                if let Some(window_id) = snss_i32(payload) {
                    windows.remove(&window_id);
                }
            }
            SNSS_COMMAND_TAB_CLOSED | SNSS_COMMAND_TAB_CLOSED2 => {
                if let Some(tab_id) = snss_i32(payload) {
                    for tabs in windows.values_mut() {
                        tabs.remove(&tab_id);
                    }
                }
            }
            _ => {}
        }
    }
    windows
        .into_keys()
        .map(|window_id| window_id.to_string())
        .collect()
}

#[cfg(test)]
fn chrome_session_restore_files(profile_dir: &Path, limit: usize) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for name in ["Current Session", "Last Session"] {
        let path = profile_dir.join(name);
        if path.is_file() {
            files.push(path);
        }
    }
    if let Ok(entries) = fs::read_dir(profile_dir.join("Sessions")) {
        files.extend(entries.flatten().map(|entry| entry.path()).filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("Session_"))
        }));
    }
    files.sort_by_key(|path| {
        std::cmp::Reverse(
            path.metadata()
                .and_then(|meta| meta.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH),
        )
    });
    files.truncate(limit);
    files
}

#[cfg(test)]
fn collect_profile_open_window_ids(profile_dir: &Path) -> HashSet<String> {
    let mut ids = HashSet::new();
    for path in chrome_session_restore_files(profile_dir, 2) {
        let Ok(data) = fs::read(&path) else {
            continue;
        };
        if data.len() > 8 * 1024 * 1024 {
            continue;
        }
        ids.extend(parse_chrome_session_open_window_ids(&data));
    }
    ids
}

#[cfg(test)]
fn assign_chrome_windows_by_session_ids(
    windows: &[ChromeWindowSnapshot],
    profile_window_ids: &[(String, HashSet<String>)],
) -> HashMap<String, Vec<ChromeWindowSnapshot>> {
    let mut assigned: HashMap<String, Vec<ChromeWindowSnapshot>> = HashMap::new();
    for window in windows {
        let mut matches = profile_window_ids
            .iter()
            .filter(|(_, ids)| ids.contains(&window.window_id))
            .map(|(profile_id, _)| profile_id.as_str());
        let Some(profile_id) = matches.next() else {
            continue;
        };
        if matches.next().is_some() {
            continue;
        }
        assigned
            .entry(profile_id.to_string())
            .or_default()
            .push(window.clone());
    }
    assigned
}

#[cfg(test)]
fn assign_chrome_windows_to_profiles(
    windows: &[ChromeWindowSnapshot],
    profile_urls: &[(String, HashSet<String>)],
) -> HashMap<String, Vec<ChromeWindowSnapshot>> {
    let mut assigned: HashMap<String, Vec<ChromeWindowSnapshot>> = HashMap::new();
    for window in windows {
        let window_urls = window
            .tabs
            .iter()
            .map(|(_, url)| normalize_chrome_reuse_url(url))
            .collect::<HashSet<_>>();
        let mut best_profile = None;
        let mut best_score = 0usize;
        let mut second_score = 0usize;
        for (profile_id, urls) in profile_urls {
            let score = urls.intersection(&window_urls).count();
            if score == 0 {
                continue;
            }
            if score > best_score {
                second_score = best_score;
                best_score = score;
                best_profile = Some(profile_id.as_str());
            } else if score == best_score {
                second_score = score;
            } else if score > second_score {
                second_score = score;
            }
        }
        if let Some(profile_id) = best_profile {
            if best_score > second_score {
                assigned
                    .entry(profile_id.to_string())
                    .or_default()
                    .push(window.clone());
            }
        }
    }
    assigned
}

#[cfg(test)]
fn exclusive_profile_urls(
    profile_urls: &[(String, HashSet<String>)],
) -> Vec<(String, HashSet<String>)> {
    profile_urls
        .iter()
        .enumerate()
        .map(|(index, (profile_id, urls))| {
            let exclusive = urls
                .iter()
                .filter(|url| {
                    profile_urls
                        .iter()
                        .enumerate()
                        .all(|(other, (_, other_urls))| {
                            other == index || !other_urls.contains(*url)
                        })
                })
                .cloned()
                .collect::<HashSet<_>>();
            (profile_id.clone(), exclusive)
        })
        .collect()
}

#[cfg(test)]
fn assigned_chrome_window_ids(
    assigned: &HashMap<String, Vec<ChromeWindowSnapshot>>,
) -> HashSet<String> {
    assigned
        .values()
        .flatten()
        .map(|window| window.window_id.clone())
        .collect()
}

#[cfg(test)]
fn merge_assigned_chrome_windows(
    into: &mut HashMap<String, Vec<ChromeWindowSnapshot>>,
    extra: HashMap<String, Vec<ChromeWindowSnapshot>>,
) {
    for (profile_id, windows) in extra {
        let entry = into.entry(profile_id).or_default();
        for window in windows {
            if !entry
                .iter()
                .any(|existing| existing.window_id == window.window_id)
            {
                entry.push(window);
            }
        }
    }
}

#[cfg(test)]
fn apply_cached_chrome_window_profiles(
    windows: &[ChromeWindowSnapshot],
    assigned_window_ids: &HashSet<String>,
    assigned_profiles: &HashSet<String>,
    cache: &HashMap<String, String>,
) -> HashMap<String, Vec<ChromeWindowSnapshot>> {
    let windows_by_id = windows
        .iter()
        .map(|window| (window.window_id.as_str(), window))
        .collect::<HashMap<_, _>>();
    let mut extra: HashMap<String, Vec<ChromeWindowSnapshot>> = HashMap::new();
    for (window_id, profile_id) in cache {
        if assigned_window_ids.contains(window_id) || assigned_profiles.contains(profile_id) {
            continue;
        }
        if let Some(window) = windows_by_id.get(window_id.as_str()) {
            extra
                .entry(profile_id.clone())
                .or_default()
                .push((*window).clone());
        }
    }
    extra
}

#[cfg(test)]
fn remember_chrome_window_profiles(
    assigned: &HashMap<String, Vec<ChromeWindowSnapshot>>,
    live_window_ids: &HashSet<String>,
    allowed_profiles: &HashSet<String>,
    cache: &mut HashMap<String, String>,
) {
    cache.retain(|window_id, profile_id| {
        live_window_ids.contains(window_id) && allowed_profiles.contains(profile_id)
    });
    for (profile_id, windows) in assigned {
        for window in windows {
            cache.insert(window.window_id.clone(), profile_id.clone());
        }
    }
}

#[cfg(test)]
fn resolve_chrome_windows_for_profiles(
    windows: &[ChromeWindowSnapshot],
    profile_urls: &[(String, HashSet<String>)],
    profile_window_ids: &[(String, HashSet<String>)],
    cache: &mut HashMap<String, String>,
) -> HashMap<String, Vec<ChromeWindowSnapshot>> {
    let mut assigned = assign_chrome_windows_by_session_ids(windows, profile_window_ids);
    let assigned_window_ids = assigned_chrome_window_ids(&assigned);
    let remaining = windows
        .iter()
        .filter(|window| !assigned_window_ids.contains(&window.window_id))
        .cloned()
        .collect::<Vec<_>>();
    let exclusive = exclusive_profile_urls(profile_urls);
    merge_assigned_chrome_windows(
        &mut assigned,
        assign_chrome_windows_to_profiles(&remaining, &exclusive),
    );
    let assigned_window_ids = assigned_chrome_window_ids(&assigned);
    let remaining = windows
        .iter()
        .filter(|window| !assigned_window_ids.contains(&window.window_id))
        .cloned()
        .collect::<Vec<_>>();
    merge_assigned_chrome_windows(
        &mut assigned,
        assign_chrome_windows_to_profiles(&remaining, profile_urls),
    );

    let assigned_window_ids = assigned_chrome_window_ids(&assigned);
    let assigned_profiles = assigned.keys().cloned().collect::<HashSet<_>>();
    merge_assigned_chrome_windows(
        &mut assigned,
        apply_cached_chrome_window_profiles(
            windows,
            &assigned_window_ids,
            &assigned_profiles,
            cache,
        ),
    );

    let live_window_ids = windows
        .iter()
        .map(|window| window.window_id.clone())
        .collect::<HashSet<_>>();
    let mut allowed_profiles = profile_urls
        .iter()
        .map(|(profile_id, _)| profile_id.clone())
        .collect::<HashSet<_>>();
    allowed_profiles.extend(
        profile_window_ids
            .iter()
            .map(|(profile_id, _)| profile_id.clone()),
    );
    remember_chrome_window_profiles(&assigned, &live_window_ids, &allowed_profiles, cache);
    assigned
}

#[allow(dead_code)]
fn chrome_window_profile_cache() -> std::sync::MutexGuard<'static, HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
fn plan_chrome_session_open(
    target_url: &str,
    profile_running: bool,
    profile_windows: &[ChromeWindowSnapshot],
) -> ChromeSessionOpenPlan {
    for window in profile_windows {
        if let Some((tab_id, true)) = preferred_reuse_tab(window, target_url) {
            return ChromeSessionOpenPlan::ActivateTab {
                tab_id,
                window_id: window.window_id.clone(),
            };
        }
    }
    if let Some(window) = profile_windows.first() {
        return ChromeSessionOpenPlan::NewTabInWindow {
            window_id: window.window_id.clone(),
        };
    }
    ChromeSessionOpenPlan::Launch {
        new_window: !profile_running,
    }
}

#[cfg(target_os = "macos")]
fn chrome_window_snapshots() -> Vec<ChromeWindowSnapshot> {
    const SCRIPT: &str = r#"
if application "Google Chrome" is not running then return ""
set tabLines to ""
tell application "Google Chrome"
    repeat with windowIndex from 1 to (count of windows)
        try
            set windowId to (id of window windowIndex) as text
            repeat with tabIndex from 1 to (count of tabs of window windowIndex)
                try
                    set browserTab to tab tabIndex of window windowIndex
                    set tabLines to tabLines & windowId & tab & ((id of browserTab) as text) & tab & (URL of browserTab) & linefeed
                end try
            end repeat
        end try
    end repeat
end tell
return tabLines
"#;
    let mut command = Command::new("/usr/bin/osascript");
    command.args(["-e", SCRIPT]);
    let Ok(output) = run_osascript_with_deadline(command, Duration::from_secs(10)) else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    group_chrome_window_snapshots(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(parse_chrome_window_snapshot_line),
    )
}

#[cfg(target_os = "macos")]
fn chrome_tabs() -> Vec<(String, String)> {
    chrome_window_snapshots()
        .into_iter()
        .flat_map(|window| window.tabs)
        .collect()
}

#[cfg(target_os = "macos")]
#[allow(dead_code)]
fn chrome_pid_is_alive(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
#[allow(dead_code)]
fn chrome_profile_is_running(profile_dir: &Path) -> bool {
    let lock = profile_dir.join("SingletonLock");
    let Ok(target) = fs::read_link(&lock) else {
        return false;
    };
    parse_chrome_singleton_lock_pid(&target.to_string_lossy()).is_some_and(chrome_pid_is_alive)
}

#[allow(dead_code)]
fn collect_profile_open_tab_urls(profile_dir: &Path) -> HashSet<String> {
    let mut urls = HashSet::new();
    let mut files = vec![
        profile_dir.join("Current Session"),
        profile_dir.join("Current Tabs"),
        profile_dir.join("Last Session"),
        profile_dir.join("Last Tabs"),
    ];
    if let Ok(entries) = fs::read_dir(profile_dir.join("Sessions")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                files.push(path);
            }
        }
    }
    for path in files {
        let Ok(data) = fs::read(&path) else {
            continue;
        };
        if data.len() > 8 * 1024 * 1024 {
            continue;
        }
        urls.extend(extract_http_urls_from_bytes(&data));
    }
    urls
}

#[cfg(target_os = "macos")]
fn chrome_tab_ids() -> HashSet<String> {
    chrome_tabs().into_iter().map(|(id, _)| id).collect()
}

/// 判断某个 Chrome 标签页是否是本应用此前同步遗留的桥接标签：
/// 站点 origin 相同，且 URL fragment 以对应流程的 marker 前缀开头。
#[cfg(target_os = "macos")]
fn is_openhub_bridge_tab(tab_url: &str, target_origin: &str, fragment_prefix: &str) -> bool {
    let Ok(parsed) = Url::parse(tab_url) else {
        return false;
    };
    if parsed.origin().ascii_serialization() != target_origin {
        return false;
    }
    parsed
        .fragment()
        .is_some_and(|fragment| fragment.starts_with(fragment_prefix))
}

/// 在已打开的 Chrome 标签页里寻找此前同步遗留的 OpenHub 桥接标签。
#[cfg(target_os = "macos")]
fn find_openhub_bridge_tab(target_url: &str, fragment_prefix: &str) -> Option<String> {
    let target_origin = validated_external_url(target_url)
        .ok()?
        .origin()
        .ascii_serialization();
    chrome_tabs().into_iter().find_map(|(id, url)| {
        is_openhub_bridge_tab(&url, &target_origin, fragment_prefix).then_some(id)
    })
}

#[cfg(target_os = "macos")]
fn wait_for_new_chrome_tab(
    existing_tab_ids: &HashSet<String>,
    target_url: &str,
    timeout: Duration,
) -> Option<String> {
    let target_origin = validated_external_url(target_url)
        .ok()?
        .origin()
        .ascii_serialization();
    let started = Instant::now();
    while started.elapsed() < timeout {
        if let Some(id) = chrome_tabs().into_iter().find_map(|(id, url)| {
            if existing_tab_ids.contains(&id) {
                return None;
            }
            Url::parse(&url)
                .ok()
                .is_some_and(|url| url.origin().ascii_serialization() == target_origin)
                .then_some(id)
        }) {
            return Some(id);
        }
        thread::sleep(Duration::from_millis(250));
    }
    None
}

#[cfg(target_os = "macos")]
fn close_chrome_bridge_tabs(target_tab_id: Option<&str>, marker: &str) {
    const SCRIPT: &str = r#"
on run argv
    set targetTabId to item 1 of argv
    set targetMarker to item 2 of argv
    if application "Google Chrome" is not running then return
    tell application "Google Chrome"
        repeat with windowIndex from (count of windows) to 1 by -1
            try
                repeat with tabIndex from (count of tabs of window windowIndex) to 1 by -1
                    try
                        set browserTab to tab tabIndex of window windowIndex
                        set currentTabId to (id of browserTab) as text
                        if ((targetTabId is not "") and (currentTabId is equal to targetTabId)) or ((URL of browserTab) contains targetMarker) then close browserTab
                    end try
                end repeat
            end try
        end repeat
    end tell
end run
"#;
    let mut command = Command::new("/usr/bin/osascript");
    command.args([
        "-e",
        SCRIPT,
        "--",
        target_tab_id.unwrap_or_default(),
        marker,
    ]);
    let _ = run_osascript_with_deadline(command, Duration::from_secs(10));
}

#[cfg(target_os = "macos")]
fn close_openhub_sync_tabs() -> Result<(), String> {
    const SCRIPT: &str = r##"
if application "Google Chrome" is not running then return
set targetMarkers to {"#openhub-sync-", "#openhub-models-", "#openhub-background-", "#openhub-silent-"}
tell application "Google Chrome"
    repeat with windowIndex from (count of windows) to 1 by -1
        try
            repeat with tabIndex from (count of tabs of window windowIndex) to 1 by -1
                try
                    set browserTab to tab tabIndex of window windowIndex
                    set tabUrl to URL of browserTab
                    repeat with targetMarker in targetMarkers
                        if tabUrl contains (targetMarker as text) then
                            close browserTab
                            exit repeat
                        end if
                    end repeat
                end try
            end repeat
        end try
    end repeat
end tell
"##;
    let mut command = Command::new("/usr/bin/osascript");
    command.args(["-e", SCRIPT]);
    let output = run_osascript_with_deadline(command, Duration::from_secs(10))
        .map_err(|error| format!("无法调用 Chrome 标签清理自动化：{error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if error.is_empty() {
            "Chrome 标签清理自动化执行失败".into()
        } else {
            format!("Chrome 标签清理自动化执行失败：{error}")
        })
    }
}

#[cfg(not(target_os = "macos"))]
fn close_openhub_sync_tabs() -> Result<(), String> {
    Ok(())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn close_chrome_sync_tabs() -> Result<(), String> {
    spawn_blocking(close_openhub_sync_tabs)
        .await
        .map_err(|error| format!("清理 Chrome 同步标签任务失败：{error}"))?
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn run_javascript_in_chrome_profile(
    _target_url: &str,
    _profile_id: &str,
    _marker: &str,
    _javascript: &str,
    _timeout: Duration,
    _proxy_url: Option<&str>,
    _allow_tab_reuse: bool,
) -> Result<String, String> {
    Err("当前仅支持在 macOS 上通过 Chrome 同步账号".into())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn run_javascript_in_background_chrome_profile(
    _target_url: &str,
    _profile_id: &str,
    _marker: &str,
    _javascript: &str,
    _timeout: Duration,
    _proxy_url: Option<&str>,
    _allow_tab_reuse: bool,
) -> Result<String, String> {
    Err("当前仅支持在 macOS 上通过 Chrome 同步账号".into())
}

#[cfg(target_os = "macos")]
fn open_url_in_chrome_profile_blocking(url: &str, profile_id: &str) -> Result<(), String> {
    open_url_in_chrome_profile_blocking_with_mode(url, profile_id, false, None, false)
}

#[cfg(target_os = "macos")]
fn open_url_in_chrome_profile_blocking_with_mode(
    url: &str,
    profile_id: &str,
    background: bool,
    proxy_url: Option<&str>,
    new_window: bool,
) -> Result<(), String> {
    if !is_safe_profile_dir(profile_id) {
        return Err("Chrome Profile 标识无效".into());
    }
    let parsed = validated_external_url(url)?;
    let mut command = Command::new("/usr/bin/open");
    if background {
        command.arg("-g");
    }
    command
        .args(["-na", "Google Chrome", "--args"])
        .arg(format!("--profile-directory={profile_id}"));
    if new_window {
        command.arg("--new-window");
    }
    if let Some(proxy) = proxy_url {
        if !proxy.trim().is_empty() {
            command.arg(format!("--proxy-server={proxy}"));
        }
    }
    let status = command
        .arg(parsed.as_str())
        .status()
        .map_err(|error| format!("无法启动 Google Chrome：{error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Google Chrome 启动失败（状态码：{status}）"))
    }
}

#[cfg(not(target_os = "macos"))]
fn open_url_in_chrome_profile_blocking(_url: &str, _profile_id: &str) -> Result<(), String> {
    Err("当前仅支持在 macOS 上打开指定 Chrome 账户".into())
}

fn validated_external_url(value: &str) -> Result<Url, String> {
    let parsed = Url::parse(value).map_err(|_| "链接必须是完整的 http:// 或 https:// 地址")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("仅允许打开 http:// 或 https:// 地址".into());
    }
    Ok(parsed)
}

#[cfg(target_os = "macos")]
const CHROME_MULTI_PROFILE_OPEN_GAP: Duration = Duration::from_millis(400);

#[cfg(target_os = "macos")]
fn opened_chrome_session_from_info(session: &ChromeSessionInfo) -> OpenedChromeSession {
    OpenedChromeSession {
        profile_id: session.profile_id.clone(),
        profile_name: session.profile_name.clone(),
        account_name: session.account_name.clone(),
    }
}

fn format_chrome_session_open_error(session: &OpenedChromeSession, error: &str) -> String {
    let label = if session.account_name.trim().is_empty() {
        session.profile_name.as_str()
    } else {
        session.account_name.as_str()
    };
    format!("{label}：{error}")
}

fn open_url_in_listed_chrome_sessions<F>(
    url: &str,
    sessions: &[OpenedChromeSession],
    mut open: F,
) -> OpenUrlInChromeSessionsResult
where
    F: FnMut(&str, &OpenedChromeSession) -> Result<(), String>,
{
    let parsed = match validated_external_url(url) {
        Ok(parsed) => parsed,
        Err(error) => {
            return OpenUrlInChromeSessionsResult {
                opened: 0,
                attempted: sessions.len(),
                profiles: Vec::new(),
                errors: vec![error],
            };
        }
    };
    let mut profiles = Vec::new();
    let mut errors = Vec::new();
    for session in sessions {
        match open(parsed.as_str(), session) {
            Ok(()) => profiles.push(session.clone()),
            Err(error) => errors.push(format_chrome_session_open_error(session, &error)),
        }
    }
    OpenUrlInChromeSessionsResult {
        opened: profiles.len(),
        attempted: sessions.len(),
        profiles,
        errors,
    }
}

fn open_url_in_chrome_sessions_from_home(
    home_dir: &Path,
    url: &str,
) -> Result<OpenUrlInChromeSessionsResult, String> {
    let parsed = validated_external_url(url)?;
    #[cfg(not(target_os = "macos"))]
    {
        let _ = home_dir;
        let _ = parsed;
        return Ok(OpenUrlInChromeSessionsResult::empty());
    }
    #[cfg(target_os = "macos")]
    {
        let sessions = match collect_chrome_sessions_from_home(home_dir, parsed.as_str()) {
            Ok((_, sessions)) => sessions,
            Err(_) => return Ok(OpenUrlInChromeSessionsResult::empty()),
        };
        let targets = sessions
            .iter()
            .map(opened_chrome_session_from_info)
            .collect::<Vec<_>>();
        if targets.is_empty() {
            return Ok(OpenUrlInChromeSessionsResult::empty());
        }
        // 与站点库 open_url_in_chrome_profile 相同：open -na Chrome --profile-directory
        // 且不加 --new-window，已打开的账号会在原窗口新建标签。
        let mut needs_gap = false;
        Ok(open_url_in_listed_chrome_sessions(
            parsed.as_str(),
            &targets,
            |target_url, session| {
                if needs_gap {
                    thread::sleep(CHROME_MULTI_PROFILE_OPEN_GAP);
                }
                needs_gap = true;
                open_url_in_chrome_profile_blocking(target_url, &session.profile_id)
            },
        ))
    }
}

#[cfg(target_os = "macos")]
fn collect_chrome_sessions_from_home(
    home_dir: &Path,
    target_url: &str,
) -> Result<(String, Vec<ChromeSessionInfo>), String> {
    let context = chrome_context(home_dir, target_url)?;
    let mut sessions = Vec::new();
    let mut first_query_error = None;
    for profile in context.profiles {
        let cookie_path = context.root.join(&profile.id).join("Cookies");
        if !cookie_path.is_file() {
            continue;
        }
        let cookies =
            match query_profile_cookies(&cookie_path, &profile.name, &context.url, &context.domain)
            {
                Ok((_, cookies)) => cookies,
                Err(error) => {
                    first_query_error.get_or_insert(error);
                    continue;
                }
            };
        if cookies.is_empty() {
            continue;
        }
        let mut cookie_names = cookies
            .iter()
            .map(|cookie| cookie.name.clone())
            .collect::<Vec<_>>();
        cookie_names.sort();
        cookie_names.dedup();
        sessions.push(ChromeSessionInfo {
            profile_id: profile.id,
            domain: context.domain.clone(),
            cookie_count: cookies.len(),
            cookie_names,
            profile_name: profile.name,
            account_name: profile.account_name,
            username: String::new(),
            api_key_count: 0,
            api_model_count: 0,
            api_counts_synced: false,
            api_sync_error: String::new(),
            has_access_token: false,
            remaining: None,
            used: None,
            total: None,
            unit: String::new(),
            is_valid: false,
            sync_error: String::new(),
            checkin_enabled: false,
            checked_in_today: false,
            checkin_error: String::new(),
            account_updated_at: String::new(),
            browser_fallback_cooldown_ms: 0,
            newapi_token: String::new(),
            newapi_user_id: String::new(),
            browser_fallback_failed_at: 0,
            browser_fallback_fail_count: 0,
        });
    }
    if sessions.is_empty() {
        if let Some(error) = first_query_error {
            return Err(error);
        }
    }
    Ok((context.domain, sessions))
}

#[cfg(target_os = "macos")]
fn list_chrome_sessions_from_home(
    home_dir: &Path,
    target_url: &str,
) -> Result<Vec<ChromeSessionInfo>, String> {
    let (domain, sessions) = collect_chrome_sessions_from_home(home_dir, target_url)?;
    if sessions.is_empty() {
        return Err(format!(
            "所有 Chrome 账号中都没有适用于 {domain} 的登录 Cookie"
        ));
    }
    Ok(sessions)
}

#[cfg(target_os = "macos")]
fn read_chrome_session_from_home(
    home_dir: &Path,
    target_url: &str,
    profile_id: &str,
) -> Result<ChromeSessionValue, String> {
    if !is_safe_profile_dir(profile_id) {
        return Err("Chrome Profile 标识无效".into());
    }
    let context = chrome_context(home_dir, target_url)?;
    let profile = context
        .profiles
        .into_iter()
        .find(|profile| profile.id == profile_id)
        .ok_or_else(|| "找不到指定的 Chrome Profile".to_string())?;
    let cookie_path = context.root.join(&profile.id).join("Cookies");
    let (database_version, cookies) =
        query_profile_cookies(&cookie_path, &profile.name, &context.url, &context.domain)?;
    if cookies.is_empty() {
        return Err(format!(
            "Chrome Profile「{}」中没有适用于 {} 的登录 Cookie",
            profile.name, context.domain
        ));
    }

    let mut cookie_pairs = Vec::with_capacity(cookies.len());
    let mut derived_key = None;
    for cookie in cookies {
        let value = if cookie.encrypted_value.is_empty() {
            cookie.value
        } else {
            let key = match derived_key {
                Some(key) => key,
                None => {
                    let key = derive_chrome_key()?;
                    derived_key = Some(key);
                    key
                }
            };
            decrypt_cookie_value(
                &cookie.encrypted_value,
                &key,
                &cookie.host,
                database_version,
            )?
        };
        if value.contains(['\r', '\n']) {
            return Err("Chrome Cookie 中包含不安全的换行字符".into());
        }
        cookie_pairs.push(format!("{}={value}", cookie.name));
    }

    Ok(ChromeSessionValue {
        domain: context.domain,
        cookie_count: cookie_pairs.len(),
        cookie: cookie_pairs.join("; "),
        profile_name: profile.name,
    })
}

#[cfg(target_os = "macos")]
pub(crate) fn read_chrome_cookie_sessions_from_home(
    home_dir: &Path,
    target_url: &str,
    cookie_name: &str,
) -> Result<Vec<ChromeCookieSession>, String> {
    if cookie_name.is_empty() || cookie_name.contains(['\r', '\n', ';', '=']) {
        return Err("Cookie 名称无效".into());
    }

    let context = chrome_context(home_dir, target_url)?;
    let mut sessions = Vec::new();
    let mut derived_key = None;

    for profile in context.profiles {
        let cookie_path = context.root.join(&profile.id).join("Cookies");
        if !cookie_path.is_file() {
            continue;
        }
        let (database_version, cookies) =
            query_profile_cookies(&cookie_path, &profile.name, &context.url, &context.domain)?;

        for cookie in cookies
            .into_iter()
            .filter(|cookie| cookie.name == cookie_name)
        {
            let value = if cookie.encrypted_value.is_empty() {
                cookie.value
            } else {
                let key = match derived_key {
                    Some(key) => key,
                    None => {
                        let key = derive_chrome_key()?;
                        derived_key = Some(key);
                        key
                    }
                };
                decrypt_cookie_value(
                    &cookie.encrypted_value,
                    &key,
                    &cookie.host,
                    database_version,
                )?
            };
            if value.contains(['\r', '\n', ';']) {
                return Err("Chrome Cookie 中包含不安全字符".into());
            }
            sessions.push(ChromeCookieSession {
                profile_name: profile.name.clone(),
                account_name: profile.account_name.clone(),
                cookie_header: format!("{cookie_name}={value}"),
            });
        }
    }

    if sessions.is_empty() {
        return Err(format!(
            "所有 Chrome 账号中都没有找到 {cookie_name} 登录会话"
        ));
    }
    Ok(sessions)
}

#[cfg(target_os = "macos")]
pub(crate) fn site_sessions_from_home(
    home_dir: &Path,
    targets: &[(String, Vec<String>)],
) -> Result<Vec<ChromeSiteSessionMatch>, String> {
    if targets.is_empty() {
        return Ok(Vec::new());
    }

    let (root, profiles) = chrome_installation(home_dir)?;
    let mut matched_sites = Vec::new();
    let mut found_cookie_database = false;
    let mut successful_queries = 0_usize;
    let mut first_query_error = None;

    for (site_id, urls) in targets {
        let mut sessions = Vec::new();
        let mut matched_profiles = HashSet::new();
        for target_url in urls {
            let Ok((url, domain)) = chrome_target(target_url) else {
                continue;
            };

            for profile in &profiles {
                if matched_profiles.contains(&profile.id) {
                    continue;
                }
                let cookie_path = root.join(&profile.id).join("Cookies");
                if !cookie_path.is_file() {
                    continue;
                }
                found_cookie_database = true;
                // 待定/会话探测：只要该域名下有 Cookie 就算有浏览器会话。
                // 不按 path / top_frame 过滤，避免漏掉登录态。
                match query_profile_domain_has_cookies(&cookie_path, &profile.name, &domain) {
                    Ok(has_cookies) => {
                        successful_queries += 1;
                        if has_cookies {
                            let cookie_names =
                                query_profile_cookies(&cookie_path, &profile.name, &url, &domain)
                                    .map(|(_, cookies)| {
                                        let mut names = cookies
                                            .into_iter()
                                            .map(|cookie| cookie.name)
                                            .collect::<Vec<_>>();
                                        names.sort();
                                        names.dedup();
                                        names
                                    })
                                    .unwrap_or_default();
                            sessions.push(ChromeSessionInfo {
                                profile_id: profile.id.clone(),
                                domain: domain.clone(),
                                cookie_count: cookie_names.len().max(1),
                                cookie_names,
                                profile_name: profile.name.clone(),
                                account_name: profile.account_name.clone(),
                                username: String::new(),
                                api_key_count: 0,
                                api_model_count: 0,
                                api_counts_synced: false,
                                api_sync_error: String::new(),
                                has_access_token: false,
                                remaining: None,
                                used: None,
                                total: None,
                                unit: String::new(),
                                is_valid: false,
                                sync_error: String::new(),
                                checkin_enabled: false,
                                checked_in_today: false,
                                checkin_error: String::new(),
                                account_updated_at: String::new(),
                                browser_fallback_cooldown_ms: 0,
                                newapi_token: String::new(),
                                newapi_user_id: String::new(),
                                browser_fallback_failed_at: 0,
                                browser_fallback_fail_count: 0,
                            });
                            matched_profiles.insert(profile.id.clone());
                        }
                    }
                    Err(error) => {
                        first_query_error.get_or_insert(error);
                    }
                }
            }
        }
        if !sessions.is_empty() {
            matched_sites.push(ChromeSiteSessionMatch {
                site_id: site_id.clone(),
                sessions,
            });
        }
    }

    if found_cookie_database && successful_queries == 0 {
        return Err(
            first_query_error.unwrap_or_else(|| "无法读取 Chrome Cookies 数据库".to_string())
        );
    }

    Ok(matched_sites)
}

#[cfg(target_os = "macos")]
fn query_profile_domain_has_cookies(
    cookie_path: &Path,
    profile_name: &str,
    domain: &str,
) -> Result<bool, String> {
    let connection = Connection::open_with_flags(
        cookie_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| {
        format!(
            "无法读取 Chrome Profile「{}」的 Cookies：{error}",
            profile_name
        )
    })?;
    let dotted_domain = format!(".{domain}");
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(1)
             FROM cookies
             WHERE host_key = ?1
                OR host_key = ?2
                OR (
                  substr(host_key, 1, 1) = '.'
                  AND (?1 = substr(host_key, 2) OR ?1 LIKE '%.' || substr(host_key, 2))
                )",
            [domain, dotted_domain.as_str()],
            |row| row.get(0),
        )
        .map_err(|error| format!("无法查询 Chrome Cookies：{error}"))?;
    Ok(count > 0)
}

fn query_profile_cookies(
    cookie_path: &Path,
    profile_name: &str,
    url: &Url,
    domain: &str,
) -> Result<(u32, Vec<ChromeCookie>), String> {
    let connection = Connection::open_with_flags(
        cookie_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| {
        format!(
            "无法读取 Chrome Profile「{}」的 Cookies：{error}",
            profile_name
        )
    })?;

    let database_version = connection
        .query_row("SELECT value FROM meta WHERE key = 'version'", [], |row| {
            row.get::<_, String>(0)
        })
        .optional()
        .map_err(|error| format!("无法读取 Chrome Cookies 版本：{error}"))?
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or_default();

    let mut statement = connection
        .prepare(
            "SELECT host_key, name, value, encrypted_value, path, expires_utc, is_secure
             FROM cookies
             WHERE (
                 host_key = ?1
                 OR host_key = ?2
                 OR (
                   substr(host_key, 1, 1) = '.'
                   AND (?1 = substr(host_key, 2) OR ?1 LIKE '%.' || substr(host_key, 2))
                 )
               )
             ORDER BY length(path) DESC, creation_utc ASC",
        )
        .map_err(|error| format!("无法查询 Chrome Cookies：{error}"))?;

    let dotted_domain = format!(".{domain}");
    let rows = statement
        .query_map([domain, dotted_domain.as_str()], |row| {
            Ok(ChromeCookie {
                host: row.get(0)?,
                name: row.get(1)?,
                value: row.get(2)?,
                encrypted_value: row.get(3)?,
                path: row.get(4)?,
                expires_utc: row.get(5)?,
                secure: row.get::<_, i64>(6)? != 0,
            })
        })
        .map_err(|error| format!("无法读取 Chrome Cookies：{error}"))?;

    let now_chrome = chrome_timestamp_now()?;
    let mut cookies = Vec::new();
    for row in rows {
        let cookie = row.map_err(|error| format!("Chrome Cookie 数据无效：{error}"))?;
        if cookie.secure && url.scheme() != "https" {
            continue;
        }
        if cookie.expires_utc > 0 && cookie.expires_utc <= now_chrome {
            continue;
        }
        if !cookie_path_matches(url.path(), &cookie.path) {
            continue;
        }
        if cookie.name.contains(['\r', '\n', ';']) {
            continue;
        }
        cookies.push(cookie);
    }
    Ok((database_version, cookies))
}

#[cfg(not(target_os = "macos"))]
fn list_chrome_sessions_from_home(
    _home_dir: &Path,
    _target_url: &str,
) -> Result<Vec<ChromeSessionInfo>, String> {
    Err("当前仅支持在 macOS 上直接读取 Chrome 会话".into())
}

#[cfg(not(target_os = "macos"))]
fn read_chrome_session_from_home(
    _home_dir: &Path,
    _target_url: &str,
    _profile_id: &str,
) -> Result<ChromeSessionValue, String> {
    Err("当前仅支持在 macOS 上直接读取 Chrome 会话".into())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn read_chrome_cookie_sessions_from_home(
    _home_dir: &Path,
    _target_url: &str,
    _cookie_name: &str,
) -> Result<Vec<ChromeCookieSession>, String> {
    Err("当前仅支持在 macOS 上直接读取 Chrome 会话".into())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn site_sessions_from_home(
    _home_dir: &Path,
    _targets: &[(String, Vec<String>)],
) -> Result<Vec<ChromeSiteSessionMatch>, String> {
    Err("当前仅支持在 macOS 上直接读取 Chrome 会话".into())
}

#[cfg(target_os = "macos")]
fn chrome_context(home_dir: &Path, target_url: &str) -> Result<ChromeContext, String> {
    let (url, domain) = chrome_target(target_url)?;
    let (root, profiles) = chrome_installation(home_dir)?;
    Ok(ChromeContext {
        root,
        url,
        domain,
        profiles,
    })
}

#[cfg(target_os = "macos")]
fn chrome_target(target_url: &str) -> Result<(Url, String), String> {
    let url = Url::parse(target_url).map_err(|_| "站点地址无效".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("只支持读取 http:// 或 https:// 站点的 Chrome 会话".into());
    }
    let domain = url
        .host_str()
        .ok_or_else(|| "站点地址缺少域名".to_string())?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    Ok((url, domain))
}

#[cfg(target_os = "macos")]
fn chrome_installation(home_dir: &Path) -> Result<(PathBuf, Vec<ChromeProfile>), String> {
    let root = home_dir.join("Library/Application Support/Google/Chrome");
    let local_state: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join("Local State"))
            .map_err(|_| "未找到 Google Chrome 配置，请先启动并登录 Chrome".to_string())?,
    )
    .map_err(|error| format!("Chrome 配置文件格式无效：{error}"))?;
    let profiles = chrome_profiles(&local_state);
    if profiles.is_empty() {
        return Err("没有找到可读取的 Chrome Profile".into());
    }
    Ok((root, profiles))
}

#[cfg(target_os = "macos")]
pub(crate) fn profile_identities_from_home(
    home_dir: &Path,
) -> Result<Vec<ChromeProfileIdentity>, String> {
    let (_, profiles) = chrome_installation(home_dir)?;
    Ok(profiles
        .into_iter()
        .map(|profile| ChromeProfileIdentity {
            id: profile.id,
            name: profile.name,
            account_name: profile.account_name,
        })
        .collect())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn profile_identities_from_home(
    _home_dir: &Path,
) -> Result<Vec<ChromeProfileIdentity>, String> {
    Err("当前仅支持在 macOS 上读取 Chrome Profile".into())
}

fn chrome_profiles(local_state: &serde_json::Value) -> Vec<ChromeProfile> {
    let Some(info_cache) = local_state
        .pointer("/profile/info_cache")
        .and_then(serde_json::Value::as_object)
    else {
        return Vec::new();
    };

    let mut ordered_ids = local_state
        .pointer("/profile/profiles_order")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut remaining_ids = info_cache.keys().cloned().collect::<Vec<_>>();
    remaining_ids.sort();
    ordered_ids.extend(remaining_ids);

    let mut seen = HashSet::new();
    ordered_ids
        .into_iter()
        .filter(|id| is_safe_profile_dir(id) && seen.insert(id.clone()))
        .filter_map(|id| {
            let info = info_cache.get(&id)?;
            let raw_name = info
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&id)
                .trim();
            let gaia_name = info
                .get("gaia_name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim();
            let user_name = info
                .get("user_name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim();

            let is_generic = raw_name.is_empty()
                || raw_name == "您的 Chrome"
                || raw_name == "Default"
                || raw_name.starts_with("个人资料")
                || raw_name.starts_with("Person ")
                || raw_name.starts_with("Profile ");

            let name = if is_generic && !gaia_name.is_empty() {
                gaia_name.to_string()
            } else if is_generic && !user_name.is_empty() {
                user_name.to_string()
            } else if !raw_name.is_empty() {
                raw_name.to_string()
            } else if !gaia_name.is_empty() {
                gaia_name.to_string()
            } else if !user_name.is_empty() {
                user_name.to_string()
            } else {
                id.clone()
            };

            Some(ChromeProfile {
                name,
                account_name: user_name.to_string(),
                id,
            })
        })
        .collect()
}

fn is_safe_profile_dir(value: &str) -> bool {
    let mut components = Path::new(value).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn cookie_path_matches(request_path: &str, cookie_path: &str) -> bool {
    if request_path == cookie_path {
        return true;
    }
    if !request_path.starts_with(cookie_path) {
        return false;
    }
    cookie_path.ends_with('/') || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/')
}

fn chrome_timestamp_now() -> Result<i64, String> {
    let unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "系统时间无效".to_string())?
        .as_secs() as i64;
    Ok((unix_seconds + CHROME_EPOCH_OFFSET_SECONDS) * 1_000_000)
}

#[cfg(target_os = "macos")]
fn derive_chrome_key() -> Result<[u8; 16], String> {
    let output = Command::new("/usr/bin/security")
        .args([
            "find-generic-password",
            "-w",
            "-a",
            "Chrome",
            "-s",
            "Chrome Safe Storage",
        ])
        .output()
        .map_err(|error| format!("无法调用 macOS 钥匙串：{error}"))?;
    if !output.status.success() {
        return Err("无法读取钥匙串中的“Chrome Safe Storage”，请在系统提示中允许访问后重试".into());
    }

    let mut password = output.stdout;
    while matches!(password.last(), Some(b'\n' | b'\r')) {
        password.pop();
    }
    if password.is_empty() {
        return Err("钥匙串中的 Chrome 解密密钥为空".into());
    }

    let mut key = [0_u8; 16];
    let status = unsafe {
        CCKeyDerivationPBKDF(
            2,
            password.as_ptr().cast::<c_char>(),
            password.len(),
            b"saltysalt".as_ptr(),
            b"saltysalt".len(),
            1,
            1003,
            key.as_mut_ptr(),
            key.len(),
        )
    };
    if status != 0 {
        return Err(format!("Chrome Cookie 密钥派生失败（错误码 {status}）"));
    }
    Ok(key)
}

#[cfg(target_os = "macos")]
fn decrypt_cookie_value(
    encrypted: &[u8],
    key: &[u8; 16],
    host: &str,
    database_version: u32,
) -> Result<String, String> {
    if encrypted.len() <= 3 || !matches!(&encrypted[..3], b"v10" | b"v11") {
        return Err("Chrome 使用了当前版本不支持的 Cookie 加密格式".into());
    }

    let ciphertext = &encrypted[3..];
    let iv = [b' '; 16];
    let mut output = vec![0_u8; ciphertext.len() + 16];
    let mut output_len = 0_usize;
    let status = unsafe {
        CCCrypt(
            1,
            0,
            1,
            key.as_ptr().cast::<c_void>(),
            key.len(),
            iv.as_ptr().cast::<c_void>(),
            ciphertext.as_ptr().cast::<c_void>(),
            ciphertext.len(),
            output.as_mut_ptr().cast::<c_void>(),
            output.len(),
            &mut output_len,
        )
    };
    if status != 0 {
        return Err("Chrome Cookie 解密失败，请确认已允许访问 Chrome 钥匙串".into());
    }
    output.truncate(output_len);

    if database_version >= 24 {
        if output.len() < 32 {
            return Err("Chrome Cookie 的域名校验数据不完整".into());
        }
        let expected_hash = sha256(host.as_bytes());
        if output[..32] != expected_hash {
            return Err("Chrome Cookie 的域名校验失败".into());
        }
        output.drain(..32);
    }

    String::from_utf8(output).map_err(|_| "Chrome Cookie 不是有效的 UTF-8 文本".into())
}

#[cfg(target_os = "macos")]
fn sha256(value: &[u8]) -> [u8; 32] {
    let mut digest = [0_u8; 32];
    unsafe {
        CC_SHA256(
            value.as_ptr().cast::<c_void>(),
            value.len() as u32,
            digest.as_mut_ptr(),
        );
    }
    digest
}

#[cfg(target_os = "macos")]
#[link(name = "System")]
unsafe extern "C" {
    fn CCKeyDerivationPBKDF(
        algorithm: u32,
        password: *const c_char,
        password_len: usize,
        salt: *const u8,
        salt_len: usize,
        pseudo_random_algorithm: u32,
        rounds: u32,
        derived_key: *mut u8,
        derived_key_len: usize,
    ) -> i32;

    fn CCCrypt(
        operation: u32,
        algorithm: u32,
        options: u32,
        key: *const c_void,
        key_length: usize,
        iv: *const c_void,
        data_in: *const c_void,
        data_in_length: usize,
        data_out: *mut c_void,
        data_out_available: usize,
        data_out_moved: *mut usize,
    ) -> i32;

    fn CC_SHA256(data: *const c_void, len: u32, digest: *mut u8) -> *mut u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_reduced_chrome_user_agent_from_installed_version() {
        let user_agent = chrome_user_agent_for_version("151.0.7922.72");
        assert!(user_agent.contains("Chrome/151.0.0.0"));
        assert!(!user_agent.contains("7922"));
    }

    #[cfg(target_os = "macos")]
    fn encrypt_fixture(value: &[u8], key: &[u8; 16]) -> Vec<u8> {
        let iv = [b' '; 16];
        let mut output = vec![0_u8; value.len() + 16];
        let mut output_len = 0_usize;
        let status = unsafe {
            CCCrypt(
                0,
                0,
                1,
                key.as_ptr().cast::<c_void>(),
                key.len(),
                iv.as_ptr().cast::<c_void>(),
                value.as_ptr().cast::<c_void>(),
                value.len(),
                output.as_mut_ptr().cast::<c_void>(),
                output.len(),
                &mut output_len,
            )
        };
        assert_eq!(status, 0);
        output.truncate(output_len);
        [b"v10".as_slice(), output.as_slice()].concat()
    }

    #[test]
    fn validates_profile_directory_names() {
        assert!(is_safe_profile_dir("Default"));
        assert!(is_safe_profile_dir("Profile 12"));
        assert!(!is_safe_profile_dir("../Default"));
        assert!(!is_safe_profile_dir("Profile 1/Cookies"));
    }

    #[test]
    fn validates_urls_opened_in_chrome_profiles() {
        assert!(validated_external_url("https://example.com/path").is_ok());
        assert!(validated_external_url("http://localhost:3000/").is_ok());
        assert!(validated_external_url("javascript:alert(1)").is_err());
        assert!(validated_external_url("example.com").is_err());
    }

    fn sample_opened_session(id: &str, name: &str, account: &str) -> OpenedChromeSession {
        OpenedChromeSession {
            profile_id: id.to_string(),
            profile_name: name.to_string(),
            account_name: account.to_string(),
        }
    }

    #[test]
    fn opens_all_matching_chrome_sessions_and_keeps_partial_failures() {
        let sessions = vec![
            sample_opened_session("Default", "默认", "alice"),
            sample_opened_session("Profile 1", "工作", "bob"),
            sample_opened_session("Profile 2", "备用", ""),
        ];
        let result = open_url_in_listed_chrome_sessions(
            "https://linux.do/t/topic/1",
            &sessions,
            |_, session| {
                if session.profile_id == "Profile 1" {
                    Err("启动失败".into())
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(result.attempted, 3);
        assert_eq!(result.opened, 2);
        assert_eq!(result.profiles[0].profile_id, "Default");
        assert_eq!(result.profiles[1].profile_id, "Profile 2");
        assert_eq!(result.errors, vec!["bob：启动失败"]);
    }

    #[test]
    fn rejects_unsafe_url_when_opening_listed_chrome_sessions() {
        let sessions = vec![sample_opened_session("Default", "默认", "")];
        let result =
            open_url_in_listed_chrome_sessions("javascript:alert(1)", &sessions, |_, _| Ok(()));
        assert_eq!(result.opened, 0);
        assert_eq!(result.attempted, 1);
        assert!(result.errors.iter().any(|error| error.contains("http")));
    }

    #[test]
    fn treats_empty_chrome_sessions_as_no_attempt() {
        let result =
            open_url_in_listed_chrome_sessions("https://linux.do/t/topic/1", &[], |_, _| {
                panic!("should not open");
            });
        assert_eq!(result.attempted, 0);
        assert_eq!(result.opened, 0);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn serializes_open_url_in_chrome_sessions_result() {
        let result = OpenUrlInChromeSessionsResult {
            opened: 1,
            attempted: 1,
            profiles: vec![sample_opened_session("Profile 1", "工作", "bob")],
            errors: vec![],
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["opened"], 1);
        assert_eq!(value["attempted"], 1);
        assert_eq!(value["profiles"][0]["profileId"], "Profile 1");
        assert_eq!(value["profiles"][0]["accountName"], "bob");
    }

    fn sample_window(window_id: &str, tabs: &[(&str, &str)]) -> ChromeWindowSnapshot {
        ChromeWindowSnapshot {
            window_id: window_id.to_string(),
            tabs: tabs
                .iter()
                .map(|(tab_id, url)| (tab_id.to_string(), url.to_string()))
                .collect(),
        }
    }

    #[test]
    fn parses_chrome_singleton_lock_pid() {
        assert_eq!(
            parse_chrome_singleton_lock_pid("MacBook-Pro-45231"),
            Some(45231)
        );
        assert_eq!(parse_chrome_singleton_lock_pid("127.0.0.1-88"), Some(88));
        assert_eq!(parse_chrome_singleton_lock_pid("0"), None);
        assert_eq!(parse_chrome_singleton_lock_pid("not-a-pid"), None);
    }

    #[test]
    fn extracts_http_urls_from_session_bytes() {
        let data =
            b"xxhttps://linux.do/t/topic/1\0https://mail.google.com/mail\nhttp://localhost:3000/ok";
        let urls = extract_http_urls_from_bytes(data);
        assert!(urls.contains(&normalize_chrome_reuse_url("https://linux.do/t/topic/1")));
        assert!(urls.contains(&normalize_chrome_reuse_url("https://mail.google.com/mail")));
        assert!(urls.contains(&normalize_chrome_reuse_url("http://localhost:3000/ok")));
    }

    #[test]
    fn matches_chrome_tab_urls_without_fragment_or_trailing_slash() {
        assert!(chrome_tab_url_matches(
            "https://linux.do/t/topic/1#openhub",
            "https://linux.do/t/topic/1/"
        ));
        assert!(!chrome_tab_url_matches(
            "https://linux.do/t/topic/2",
            "https://linux.do/t/topic/1"
        ));
    }

    #[test]
    fn assigns_chrome_windows_to_profiles_by_unique_url_overlap() {
        let windows = vec![
            sample_window(
                "11",
                &[
                    ("1", "https://linux.do/t/a"),
                    ("2", "https://mail.google.com/mail"),
                ],
            ),
            sample_window("22", &[("3", "https://linux.do/t/b")]),
        ];
        let profile_urls = vec![
            (
                "Default".to_string(),
                HashSet::from([
                    normalize_chrome_reuse_url("https://linux.do/t/a"),
                    normalize_chrome_reuse_url("https://mail.google.com/mail"),
                ]),
            ),
            (
                "Profile 1".to_string(),
                HashSet::from([normalize_chrome_reuse_url("https://linux.do/t/b")]),
            ),
        ];
        let assigned = assign_chrome_windows_to_profiles(&windows, &profile_urls);
        assert_eq!(assigned["Default"][0].window_id, "11");
        assert_eq!(assigned["Profile 1"][0].window_id, "22");
    }

    #[test]
    fn leaves_tied_chrome_windows_unassigned() {
        let windows = vec![sample_window("11", &[("1", "https://linux.do/t/a")])];
        let profile_urls = vec![
            (
                "Default".to_string(),
                HashSet::from([normalize_chrome_reuse_url("https://linux.do/t/a")]),
            ),
            (
                "Profile 1".to_string(),
                HashSet::from([normalize_chrome_reuse_url("https://linux.do/t/a")]),
            ),
        ];
        let assigned = assign_chrome_windows_to_profiles(&windows, &profile_urls);
        assert!(assigned.is_empty());
    }

    #[test]
    fn reuses_existing_tab_when_profile_window_already_has_url() {
        let windows = vec![sample_window(
            "11",
            &[("99", "https://linux.do/t/topic/1#x")],
        )];
        assert_eq!(
            plan_chrome_session_open("https://linux.do/t/topic/1", true, &windows),
            ChromeSessionOpenPlan::ActivateTab {
                tab_id: "99".into(),
                window_id: "11".into()
            }
        );
    }

    #[test]
    fn opens_new_tab_in_existing_window_when_target_url_is_absent() {
        let windows = vec![sample_window(
            "11",
            &[
                ("88", "https://mail.google.com/mail"),
                ("99", "https://linux.do/t/old"),
            ],
        )];
        assert_eq!(
            plan_chrome_session_open("https://linux.do/t/topic/1", true, &windows),
            ChromeSessionOpenPlan::NewTabInWindow {
                window_id: "11".into()
            }
        );
    }

    #[test]
    fn opens_new_tab_in_existing_window_without_linuxdo_tab() {
        let windows = vec![sample_window(
            "11",
            &[("99", "https://mail.google.com/mail")],
        )];
        assert_eq!(
            plan_chrome_session_open("https://linux.do/t/topic/1", true, &windows),
            ChromeSessionOpenPlan::NewTabInWindow {
                window_id: "11".into()
            }
        );
    }

    #[test]
    fn recognizes_linuxdo_hosts() {
        assert!(is_linuxdo_tab_url("https://linux.do/t/topic/1"));
        assert!(is_linuxdo_tab_url("https://www.linux.do/t/topic/1"));
        assert!(!is_linuxdo_tab_url("https://rate.linux.do/merchant/1"));
        assert!(!is_linuxdo_tab_url("https://mail.google.com/mail"));
        assert!(!is_linuxdo_tab_url("not-a-url"));
    }

    #[test]
    fn assigns_windows_by_exclusive_urls_even_when_linuxdo_overlaps() {
        let windows = vec![
            sample_window(
                "11",
                &[
                    ("1", "https://linux.do/t/shared"),
                    ("2", "https://mail.google.com/a"),
                ],
            ),
            sample_window(
                "22",
                &[
                    ("3", "https://linux.do/t/shared"),
                    ("4", "https://github.com/b"),
                ],
            ),
        ];
        let profile_urls = vec![
            (
                "Default".to_string(),
                HashSet::from([
                    normalize_chrome_reuse_url("https://linux.do/t/shared"),
                    normalize_chrome_reuse_url("https://mail.google.com/a"),
                ]),
            ),
            (
                "Profile 1".to_string(),
                HashSet::from([
                    normalize_chrome_reuse_url("https://linux.do/t/shared"),
                    normalize_chrome_reuse_url("https://github.com/b"),
                ]),
            ),
        ];
        let mut cache = HashMap::new();
        let assigned =
            resolve_chrome_windows_for_profiles(&windows, &profile_urls, &[], &mut cache);
        assert_eq!(assigned["Default"][0].window_id, "11");
        assert_eq!(assigned["Profile 1"][0].window_id, "22");
        assert_eq!(cache.get("11").map(String::as_str), Some("Default"));
    }

    #[test]
    fn reuses_cached_window_profile_when_live_urls_are_identical() {
        let first = vec![
            sample_window("11", &[("1", "https://linux.do/t/a")]),
            sample_window("22", &[("2", "https://linux.do/t/b")]),
        ];
        let profile_urls = vec![
            (
                "Default".to_string(),
                HashSet::from([normalize_chrome_reuse_url("https://linux.do/t/a")]),
            ),
            (
                "Profile 1".to_string(),
                HashSet::from([normalize_chrome_reuse_url("https://linux.do/t/b")]),
            ),
        ];
        let mut cache = HashMap::new();
        resolve_chrome_windows_for_profiles(&first, &profile_urls, &[], &mut cache);

        let second = vec![
            sample_window("11", &[("1", "https://linux.do/t/topic/9")]),
            sample_window("22", &[("2", "https://linux.do/t/topic/9")]),
        ];
        let later_urls = vec![
            (
                "Default".to_string(),
                HashSet::from([normalize_chrome_reuse_url("https://linux.do/t/topic/9")]),
            ),
            (
                "Profile 1".to_string(),
                HashSet::from([normalize_chrome_reuse_url("https://linux.do/t/topic/9")]),
            ),
        ];
        let assigned = resolve_chrome_windows_for_profiles(&second, &later_urls, &[], &mut cache);
        assert_eq!(assigned["Default"][0].window_id, "11");
        assert_eq!(assigned["Profile 1"][0].window_id, "22");
    }

    fn snss_command(command_id: u8, payload: &[u8]) -> Vec<u8> {
        let size = (payload.len() + 1) as u16;
        let mut out = size.to_le_bytes().to_vec();
        out.push(command_id);
        out.extend_from_slice(payload);
        out
    }

    fn snss_file(commands: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut data = b"SNSS".to_vec();
        data.extend_from_slice(&3i32.to_le_bytes());
        for (command_id, payload) in commands {
            data.extend(snss_command(*command_id, payload));
        }
        data
    }

    fn snss_set_tab_window(window: i32, tab: i32) -> (u8, Vec<u8>) {
        let mut payload = window.to_le_bytes().to_vec();
        payload.extend_from_slice(&tab.to_le_bytes());
        (0, payload)
    }

    fn snss_window_closed2(window: i32) -> (u8, Vec<u8>) {
        let mut payload = window.to_le_bytes().to_vec();
        payload.extend_from_slice(&[0_u8; 12]);
        (17, payload)
    }

    #[test]
    fn parses_open_session_window_ids_and_ignores_closed_windows() {
        let data = snss_file(&[
            snss_set_tab_window(1546485871, 11),
            snss_set_tab_window(1546487285, 22),
            snss_window_closed2(1546487285),
        ]);
        let ids = parse_chrome_session_open_window_ids(&data);
        assert_eq!(ids, HashSet::from(["1546485871".to_string()]));
    }

    #[test]
    fn assigns_live_windows_by_session_ids_when_urls_tie() {
        let windows = vec![
            sample_window("1546485871", &[("1", "https://linux.do/t/topic/9")]),
            sample_window("1546487285", &[("2", "https://linux.do/t/topic/9")]),
        ];
        let profile_urls = vec![
            (
                "Profile 15".to_string(),
                HashSet::from([normalize_chrome_reuse_url("https://linux.do/t/topic/9")]),
            ),
            (
                "Profile 11".to_string(),
                HashSet::from([normalize_chrome_reuse_url("https://linux.do/t/topic/9")]),
            ),
        ];
        let profile_window_ids = vec![
            (
                "Profile 15".to_string(),
                HashSet::from(["1546485871".to_string()]),
            ),
            (
                "Profile 11".to_string(),
                HashSet::from(["1546487285".to_string()]),
            ),
        ];
        let mut cache = HashMap::new();
        let assigned = resolve_chrome_windows_for_profiles(
            &windows,
            &profile_urls,
            &profile_window_ids,
            &mut cache,
        );
        assert_eq!(assigned["Profile 15"][0].window_id, "1546485871");
        assert_eq!(assigned["Profile 11"][0].window_id, "1546487285");
        assert_eq!(
            plan_chrome_session_open("https://linux.do/t/topic/1", false, &assigned["Profile 15"]),
            ChromeSessionOpenPlan::NewTabInWindow {
                window_id: "1546485871".into()
            }
        );
        assert_eq!(
            plan_chrome_session_open("https://linux.do/t/topic/9", false, &assigned["Profile 11"]),
            ChromeSessionOpenPlan::ActivateTab {
                tab_id: "2".into(),
                window_id: "1546487285".into()
            }
        );
    }

    #[test]
    fn ignores_stale_session_window_ids_that_are_not_live() {
        let windows = vec![sample_window(
            "1546485871",
            &[("1", "https://linux.do/t/a")],
        )];
        let profile_window_ids = vec![
            (
                "Profile 15".to_string(),
                HashSet::from(["1546485871".to_string()]),
            ),
            (
                "Profile 13".to_string(),
                HashSet::from(["1546488863".to_string()]),
            ),
        ];
        let assigned = assign_chrome_windows_by_session_ids(&windows, &profile_window_ids);
        assert_eq!(assigned["Profile 15"][0].window_id, "1546485871");
        assert!(!assigned.contains_key("Profile 13"));
    }

    #[test]
    fn collects_open_window_ids_from_latest_session_file() {
        let dir = std::env::temp_dir().join(format!(
            "openhub-chrome-session-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let sessions = dir.join("Sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join("Session_1"),
            snss_file(&[
                snss_set_tab_window(11, 1),
                snss_window_closed2(11),
                snss_set_tab_window(22, 2),
            ]),
        )
        .unwrap();
        let ids = collect_profile_open_window_ids(&dir);
        assert_eq!(ids, HashSet::from(["22".to_string()]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn launches_without_new_window_when_profile_is_already_running() {
        assert_eq!(
            plan_chrome_session_open("https://linux.do/t/topic/1", true, &[]),
            ChromeSessionOpenPlan::Launch { new_window: false }
        );
        assert_eq!(
            plan_chrome_session_open("https://linux.do/t/topic/1", false, &[]),
            ChromeSessionOpenPlan::Launch { new_window: true }
        );
    }

    #[test]
    fn groups_chrome_window_snapshot_lines() {
        let windows = group_chrome_window_snapshots([
            parse_chrome_window_snapshot_line("11\t21\thttps://a.example/").unwrap(),
            parse_chrome_window_snapshot_line("11\t22\thttps://b.example/").unwrap(),
            parse_chrome_window_snapshot_line("12\t31\thttps://c.example/").unwrap(),
        ]);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].tabs.len(), 2);
        assert_eq!(windows[1].window_id, "12");
    }

    #[test]
    fn retries_transient_chrome_apple_event_disconnects() {
        assert!(is_transient_chrome_automation_error(
            "execution error: 连接无效。 (-609)"
        ));
        assert!(is_transient_chrome_automation_error(
            "Google Chrome got an error: Connection is invalid. (-609)"
        ));
        assert!(is_transient_chrome_automation_error(
            "Application isn't running. (-600)"
        ));
        assert!(is_transient_chrome_automation_error(
            "不能获得 item 16 of every tab。无效的索引。 (-1719)"
        ));
        assert!(is_transient_chrome_automation_error(
            "不能获得 tab id of window id。 (-1728)"
        ));
        assert!(is_transient_chrome_automation_error(
            "execution error: AppleEvent已超时。 (-1712)"
        ));
        assert!(is_transient_chrome_automation_error(
            "Google Chrome got an error: AppleEvent timed out. (-1712)"
        ));
        assert!(!is_transient_chrome_automation_error(
            "Not authorized to send Apple events. (-1743)"
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn matches_only_same_origin_openhub_bridge_tabs() {
        assert!(is_openhub_bridge_tab(
            "https://example.com/console#openhub-sync-123",
            "https://example.com",
            "openhub-sync-"
        ));
        assert!(!is_openhub_bridge_tab(
            "https://example.com/console#openhub-models-123",
            "https://example.com",
            "openhub-sync-"
        ));
        assert!(!is_openhub_bridge_tab(
            "https://other.com/#openhub-sync-123",
            "https://example.com",
            "openhub-sync-"
        ));
        assert!(!is_openhub_bridge_tab(
            "https://example.com/console",
            "https://example.com",
            "openhub-sync-"
        ));
        assert!(!is_openhub_bridge_tab(
            "not a url",
            "https://example.com",
            "openhub-sync-"
        ));
    }

    #[test]
    fn captures_only_numeric_chrome_tab_ids_from_pending_results() {
        assert_eq!(
            chrome_tab_id_from_pending("__OPENHUB_TAB_PENDING__:1546453789"),
            Some("1546453789")
        );
        assert_eq!(chrome_tab_id_from_pending("__OPENHUB_PENDING__"), None);
        assert_eq!(
            chrome_tab_id_from_pending("__OPENHUB_TAB_PENDING__:1; quit"),
            None
        );
    }

    #[test]
    fn preserves_chrome_profile_order_and_account_names() {
        let state = serde_json::json!({
            "profile": {
                "profiles_order": ["Profile 2", "Default"],
                "info_cache": {
                    "Default": { "name": "Main", "user_name": "main@example.com" },
                    "Profile 2": { "name": "Work", "user_name": "work@example.com" },
                    "Profile 7": { "name": "Spare" }
                }
            }
        });

        let profiles = chrome_profiles(&state);
        assert_eq!(profiles.len(), 3);
        assert_eq!(profiles[0].id, "Profile 2");
        assert_eq!(profiles[0].account_name, "work@example.com");
        assert_eq!(profiles[1].id, "Default");
        assert_eq!(profiles[2].id, "Profile 7");
    }

    #[test]
    fn applies_rfc_cookie_path_matching() {
        assert!(cookie_path_matches("/", "/"));
        assert!(cookie_path_matches("/console", "/"));
        assert!(cookie_path_matches("/console", "/console"));
        assert!(cookie_path_matches("/console/user", "/console"));
        assert!(!cookie_path_matches("/console-old", "/console"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn decrypts_v24_domain_bound_cookie() {
        let host = ".example.com";
        let key = [7_u8; 16];
        let plaintext = [sha256(host.as_bytes()).as_slice(), b"session-value"].concat();
        let encrypted = encrypt_fixture(&plaintext, &key);

        assert_eq!(
            decrypt_cookie_value(&encrypted, &key, host, 24).unwrap(),
            "session-value"
        );
        assert!(decrypt_cookie_value(&encrypted, &key, ".other.test", 24).is_err());
    }
}
