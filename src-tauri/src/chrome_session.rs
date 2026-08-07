use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Serialize;
use std::{
    collections::HashSet,
    ffi::{c_char, c_void},
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::Manager;
use url::Url;

const CHROME_EPOCH_OFFSET_SECONDS: i64 = 11_644_473_600;

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
    #[serde(skip)]
    pub(crate) newapi_token: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) newapi_user_id: String,
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

#[tauri::command]
pub async fn list_chrome_sessions(
    app: tauri::AppHandle,
    url: String,
) -> Result<Vec<ChromeSessionInfo>, String> {
    let home_dir = app
        .path()
        .home_dir()
        .map_err(|error| format!("无法定位用户目录：{error}"))?;

    tauri::async_runtime::spawn_blocking(move || list_chrome_sessions_from_home(&home_dir, &url))
        .await
        .map_err(|error| format!("读取 Chrome 会话任务失败：{error}"))?
}

#[tauri::command]
pub async fn read_chrome_session(
    app: tauri::AppHandle,
    url: String,
    profile_id: String,
) -> Result<ChromeSessionValue, String> {
    let home_dir = app
        .path()
        .home_dir()
        .map_err(|error| format!("无法定位用户目录：{error}"))?;

    tauri::async_runtime::spawn_blocking(move || {
        read_chrome_session_from_home(&home_dir, &url, &profile_id)
    })
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

#[tauri::command]
pub async fn open_url_in_chrome_profile(url: String, profile_id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        open_url_in_chrome_profile_blocking(&url, &profile_id)
    })
    .await
    .map_err(|error| format!("启动 Chrome 任务失败：{error}"))?
}

const CHROME_BRIDGE_TAB_NOT_FOUND: &str = "__OPENHUB_TAB_NOT_FOUND__";
const CHROME_BRIDGE_PENDING: &str = "__OPENHUB_PENDING__";
const CHROME_BRIDGE_TAB_PENDING_PREFIX: &str = "__OPENHUB_TAB_PENDING__:";
const CHROME_BRIDGE_PROFILE_MISMATCH: &str = "__OPENHUB_PROFILE_MISMATCH__";

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
                        if errorNumber is not -1719 and errorNumber is not -1728 then
                            error errorMessage number errorNumber
                        end if
                    end try
                end repeat
            on error errorMessage number errorNumber
                if errorNumber is not -1719 and errorNumber is not -1728 then
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
        let output = Command::new("/usr/bin/osascript")
            .args([
                "-e",
                SCRIPT,
                "--",
                &target_origin,
                javascript,
                &target_tab_id,
            ])
            .output()
            .map_err(|error| format!("无法调用 Chrome 静默自动化：{error}"))?;
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
            if error.contains("-1712") || error.contains("AppleEvent已超时") {
                return Err("Chrome 标签页响应 AppleEvent 超时".into());
            }
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
) -> Result<String, String> {
    validate_chrome_bridge_marker(marker)?;
    let existing_tab_ids = chrome_tab_ids();
    open_url_in_chrome_profile_blocking_with_mode(target_url, profile_id, false)?;
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
) -> Result<String, String> {
    validate_chrome_bridge_marker(marker)?;
    let existing_tab_ids = chrome_tab_ids();
    open_url_in_chrome_profile_blocking_with_mode(target_url, profile_id, true)?;
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
                        if errorNumber is not -1719 and errorNumber is not -1728 then
                            error errorMessage number errorNumber
                        end if
                    end try
                end repeat
            on error errorMessage number errorNumber
                if errorNumber is not -1719 and errorNumber is not -1728 then
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
        let output = Command::new("/usr/bin/osascript")
            .args(["-e", SCRIPT, "--", marker, javascript, &target_tab_id])
            .output()
            .map_err(|error| format!("无法调用 Chrome 自动化：{error}"))?;
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
            if error.contains("-1712") || error.contains("AppleEvent已超时") {
                return Err("Chrome 标签页响应 AppleEvent 超时".into());
            }
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
            thread::sleep(Duration::from_millis(500));
            continue;
        }
        if !matches!(
            result.as_str(),
            "" | CHROME_BRIDGE_TAB_NOT_FOUND | CHROME_BRIDGE_PENDING
        ) {
            return Ok(result);
        }
        thread::sleep(Duration::from_millis(500));
    }
    Err("等待 Chrome 返回账号数据超时；请完成页面验证后重试".into())
}

#[cfg(target_os = "macos")]
fn chrome_tabs() -> Vec<(String, String)> {
    const SCRIPT: &str = r#"
if application "Google Chrome" is not running then return ""
set tabLines to ""
tell application "Google Chrome"
    repeat with windowIndex from 1 to (count of windows)
        try
            repeat with tabIndex from 1 to (count of tabs of window windowIndex)
                try
                    set browserTab to tab tabIndex of window windowIndex
                    set tabLines to tabLines & ((id of browserTab) as text) & tab & (URL of browserTab) & linefeed
                end try
            end repeat
        end try
    end repeat
end tell
return tabLines
"#;
    let Ok(output) = Command::new("/usr/bin/osascript")
        .args(["-e", SCRIPT])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (id, url) = line.split_once('\t')?;
            (!id.is_empty() && id.chars().all(|character| character.is_ascii_digit()))
                .then(|| (id.to_string(), url.to_string()))
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn chrome_tab_ids() -> HashSet<String> {
    chrome_tabs().into_iter().map(|(id, _)| id).collect()
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
    let _ = Command::new("/usr/bin/osascript")
        .args([
            "-e",
            SCRIPT,
            "--",
            target_tab_id.unwrap_or_default(),
            marker,
        ])
        .output();
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn run_javascript_in_chrome_profile(
    _target_url: &str,
    _profile_id: &str,
    _marker: &str,
    _javascript: &str,
    _timeout: Duration,
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
) -> Result<String, String> {
    Err("当前仅支持在 macOS 上通过 Chrome 同步账号".into())
}

#[cfg(target_os = "macos")]
fn open_url_in_chrome_profile_blocking(url: &str, profile_id: &str) -> Result<(), String> {
    open_url_in_chrome_profile_blocking_with_mode(url, profile_id, false)
}

#[cfg(target_os = "macos")]
fn open_url_in_chrome_profile_blocking_with_mode(
    url: &str,
    profile_id: &str,
    background: bool,
) -> Result<(), String> {
    if !is_safe_profile_dir(profile_id) {
        return Err("Chrome Profile 标识无效".into());
    }
    let parsed = validated_external_url(url)?;
    let mut command = Command::new("/usr/bin/open");
    if background {
        command.arg("-g");
    }
    let status = command
        .args(["-na", "Google Chrome", "--args"])
        .arg(format!("--profile-directory={profile_id}"))
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
fn list_chrome_sessions_from_home(
    home_dir: &Path,
    target_url: &str,
) -> Result<Vec<ChromeSessionInfo>, String> {
    let context = chrome_context(home_dir, target_url)?;
    let mut sessions = Vec::new();
    for profile in context.profiles {
        let cookie_path = context.root.join(&profile.id).join("Cookies");
        if !cookie_path.is_file() {
            continue;
        }
        let (_, cookies) =
            query_profile_cookies(&cookie_path, &profile.name, &context.url, &context.domain)?;
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
            newapi_token: String::new(),
            newapi_user_id: String::new(),
        });
    }

    if sessions.is_empty() {
        return Err(format!(
            "所有 Chrome 账号中都没有适用于 {} 的登录 Cookie",
            context.domain
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
                                newapi_token: String::new(),
                                newapi_user_id: String::new(),
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
            Some(ChromeProfile {
                name: info
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&id)
                    .to_string(),
                account_name: info
                    .get("user_name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
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
        assert!(!is_transient_chrome_automation_error(
            "Not authorized to send Apple events. (-1743)"
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
