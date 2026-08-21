//! 站点平台类型检测 —— 移植自 metapi 的 detectPlatform 流水线。
//!
//! 检测顺序与 metapi 保持一致：
//!   1. URL 提示（host/port/path 高置信识别）
//!   2. 首页 `<title>` 提示（title-first 平台：anyrouter/done-hub/one-hub/veloera/sub2api）
//!   3. 网络端点探测：
//!      - `/v0/management/openai-compatibility` → cliproxyapi（专属响应头）
//!      - `/api/status` → veloera / new-api / one-api（按 data.system_name 区分）
//!      - `/api/v1/auth/me`、`/v1/models` → sub2api（`{code,message}` 信封）
//!   4. `<title>` 兜底（new-api / one-api）
//!   5. URL 低置信兜底（保留旧 OpenHub 行为：域名含 newapi/one-api）

use crate::site_library::{html_title, shield_page_response};
use reqwest::header;
use serde_json::Value;
use std::time::Duration;
use url::Url;

// ---------------------------------------------------------------- 别名归一化

/// 把任意写法（含 metapi 别名）归一化为规范平台名；无法识别返回空串。
pub(crate) fn canonical_platform(raw: &str) -> String {
    let compact = raw.trim().to_ascii_lowercase().replace(['-', '_', ' '], "");
    match compact.as_str() {
        "anyrouter" => "anyrouter".into(),
        "wonggongyi" => "new-api".into(),
        "voapi" | "superapi" | "rixapi" | "neoapi" => "new-api".into(),
        "newapi" => "new-api".into(),
        "newapi2" | "newapi-refresh" | "newapirefresh" => "newapi2".into(),
        "oneapi" => "one-api".into(),
        "onehub" => "one-hub".into(),
        "donehub" => "done-hub".into(),
        "veloera" => "veloera".into(),
        "sub2api" => "sub2api".into(),
        "openai" => "openai".into(),
        "codex" | "chatgptcodex" => "codex".into(),
        "anthropic" | "claude" => "claude".into(),
        "gemini" | "google" => "gemini".into(),
        "geminicli" => "gemini-cli".into(),
        "antigravity" => "antigravity".into(),
        "cliproxyapi" | "cpa" | "cliproxapi" => "cliproxyapi".into(),
        _ => String::new(),
    }
}

pub(crate) fn is_platform(system_type: &str, kind: &str) -> bool {
    canonical_platform(system_type) == kind
}

pub(crate) fn is_newapi(system_type: &str) -> bool {
    // new-api 与 newapi2 是同一套 NewAPI 后端的两种认证形态：
    // new-api 走 Cookie（session/new-api-user），newapi2 走刷新令牌。
    // 同时 anyrouter、one-api、one-hub、done-hub、veloera 等同源/衍生架构
    // 在账号（/api/user/self、/api/user/token、/api/user/checkin）、Key 与模型同步接口上完全兼容。
    is_platform(system_type, "new-api")
        || is_newapi_refresh(system_type)
        || is_platform(system_type, "anyrouter")
        || is_platform(system_type, "one-api")
        || is_platform(system_type, "one-hub")
        || is_platform(system_type, "done-hub")
        || is_platform(system_type, "veloera")
}

pub(crate) fn is_newapi_refresh(system_type: &str) -> bool {
    is_platform(system_type, "newapi2")
}

pub(crate) fn is_sub2api(system_type: &str) -> bool {
    is_platform(system_type, "sub2api")
}

// ---------------------------------------------------------------- URL 提示

fn parse_url_candidates(value: &str) -> Vec<String> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Vec::new();
    }
    if normalized.contains("://") {
        vec![normalized.to_string()]
    } else {
        vec![format!("https://{normalized}")]
    }
}

/// 移植自 metapi `detectPlatformByUrlHint`，外加 cliproxyapi 的 `:8317`/`cliproxy` 适配器 URL 检查。
pub(crate) fn detect_platform_by_url_hint(value: &str) -> Option<&'static str> {
    for candidate in parse_url_candidates(value) {
        let Ok(parsed) = Url::parse(&candidate) else {
            continue;
        };
        let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
        let port = parsed.port().unwrap_or_default();
        let path = parsed.path().to_ascii_lowercase();

        if host == "api.openai.com" {
            return Some("openai");
        }
        if host == "chatgpt.com" && path.starts_with("/backend-api/codex") {
            return Some("codex");
        }
        if host == "api.anthropic.com" || (host == "anthropic.com" && path.starts_with("/v1")) {
            return Some("claude");
        }
        if host == "generativelanguage.googleapis.com"
            || host == "gemini.google.com"
            || ((host == "googleapis.com" || host.ends_with(".googleapis.com"))
                && path.starts_with("/v1beta/openai"))
        {
            return Some("gemini");
        }
        if host == "cloudcode-pa.googleapis.com" {
            return Some("gemini-cli");
        }
        // cliproxyapi：metapi 适配器对任意主机的 :8317 端口与 cliproxy 域名做 URL 判断。
        if port == 8317 {
            return Some("cliproxyapi");
        }
        if host.contains("cliproxy") {
            return Some("cliproxyapi");
        }
        if host.contains("anyrouter") {
            return Some("anyrouter");
        }
        if host.contains("donehub") || host.contains("done-hub") {
            return Some("done-hub");
        }
        if host.contains("onehub") || host.contains("one-hub") {
            return Some("one-hub");
        }
        if host.contains("veloera") {
            return Some("veloera");
        }
        if host.contains("sub2api") {
            return Some("sub2api");
        }
    }
    None
}

/// 低置信 URL 兜底：域名里明确出现 newapi/one-api，保留旧 OpenHub 行为。
pub(crate) fn low_priority_url_hint(value: &str) -> Option<&'static str> {
    let lower = value.to_ascii_lowercase();
    if lower.contains("newapi") || lower.contains("new-api") {
        Some("new-api")
    } else if lower.contains("oneapi") || lower.contains("one-api") {
        Some("one-api")
    } else {
        None
    }
}

// ---------------------------------------------------------------- title 提示

/// 首页 `<title>` 提示，规则顺序与 metapi TITLE_RULES 一致。
/// 空格/连字符/下划线折叠后匹配，简化正则可移植性。
fn title_hint(title: &str) -> Option<&'static str> {
    let compact = title.to_ascii_lowercase().replace(['-', '_', ' '], "");
    if compact.contains("anyrouter") {
        return Some("anyrouter");
    }
    if compact.contains("donehub") {
        return Some("done-hub");
    }
    if compact.contains("onehub") {
        return Some("one-hub");
    }
    if compact.contains("veloera") {
        return Some("veloera");
    }
    if compact.contains("sub2api") {
        return Some("sub2api");
    }
    if compact.contains("newapi")
        || compact.contains("voapi")
        || compact.contains("superapi")
        || compact.contains("rixapi")
        || compact.contains("neoapi")
        || compact.contains("wong公益站")
    {
        return Some("new-api");
    }
    if compact.contains("oneapi") {
        return Some("one-api");
    }
    None
}

const TITLE_FIRST_PLATFORMS: &[&str] = &["anyrouter", "done-hub", "one-hub", "veloera", "sub2api"];

// ---------------------------------------------------------------- 网络探测

pub(crate) struct JsonProbe {
    pub(crate) json: Option<Value>,
    pub(crate) challenge: bool,
}

async fn probe_json(client: &reqwest::Client, base_url: &str, path: &str) -> Option<JsonProbe> {
    let url = Url::parse(base_url).ok()?.join(path).ok()?;
    let response = client
        .get(url)
        .header(header::ACCEPT, "application/json")
        .header(header::USER_AGENT, "OpenHub-Desktop/0.3")
        .timeout(Duration::from_secs(6))
        .send()
        .await
        .ok()?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let security_gateway_header = response.headers().contains_key("x-tengine-error")
        || response
            .headers()
            .get(header::SERVER)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("ESA"))
        || response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .any(|value| {
                let lower = value.to_ascii_lowercase();
                lower.starts_with("acw_") || lower.starts_with("cdn_sec_")
            });
    let body = response.bytes().await.ok()?;
    let json = serde_json::from_slice::<serde_json::Value>(&body).ok();
    let challenge = shield_page_response(status, &content_type, security_gateway_header, &body);
    Some(JsonProbe { json, challenge })
}

async fn probe_title(client: &reqwest::Client, base_url: &str) -> Option<String> {
    let url = Url::parse(base_url).ok()?.join("/").ok()?;
    let response = client
        .get(url)
        .header(header::ACCEPT, "text/html,application/xhtml+xml,*/*;q=0.8")
        .header(header::USER_AGENT, "OpenHub-Desktop/0.3")
        .timeout(Duration::from_secs(6))
        .send()
        .await
        .ok()?;
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !content_type.contains("text/html") && !content_type.contains("application/xhtml+xml") {
        return None;
    }
    let text = response.text().await.ok()?;
    let title = html_title(&text);
    if title.trim().is_empty() {
        None
    } else {
        Some(title)
    }
}

/// 探测 cliproxyapi 专属管理端点（响应头 x-cpa-* 或 JSON 含 openai-compatibility）。
async fn probe_cliproxyapi(client: &reqwest::Client, base_url: &str) -> bool {
    let Ok(base) = Url::parse(base_url) else {
        return false;
    };
    let Ok(url) = base.join("/v0/management/openai-compatibility") else {
        return false;
    };
    let Ok(response) = client
        .get(url)
        .header(header::USER_AGENT, "OpenHub-Desktop/0.3")
        .timeout(Duration::from_secs(6))
        .send()
        .await
    else {
        return false;
    };
    let status = response.status();
    let has_cpa_headers = response.headers().contains_key("x-cpa-version")
        || response.headers().contains_key("x-cpa-commit")
        || response.headers().contains_key("x-cpa-build-date");
    if has_cpa_headers {
        return status == reqwest::StatusCode::OK
            || status == reqwest::StatusCode::UNAUTHORIZED
            || status == reqwest::StatusCode::FORBIDDEN;
    }
    if status == reqwest::StatusCode::OK {
        if let Ok(json) = response.json::<Value>().await {
            return json.is_object() && json.get("openai-compatibility").is_some();
        }
    }
    false
}

// ---------------------------------------------------------------- 分类判定

/// `/api/status`（success=true）区分 veloera / new-api / one-api，规则与 metapi 一致。
fn classify_api_status(json: &Value) -> Option<&'static str> {
    if json.get("success").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let data = json.get("data").and_then(Value::as_object);
    let system_name = data
        .and_then(|data| data.get("system_name"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let version = data
        .and_then(|data| data.get("version"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    if system_name.contains("veloera") || version.contains("veloera") {
        return Some("veloera");
    }
    let has_string_system_name =
        data.is_some_and(|data| data.get("system_name").and_then(Value::as_str).is_some());
    if has_string_system_name {
        Some("new-api")
    } else {
        Some("one-api")
    }
}

/// sub2api 的 `{code, message}` 错误信封判定（移植自 metapi sub2api adapter）。
fn sub2api_envelope(json: &Value) -> bool {
    let Some(obj) = json.as_object() else {
        return false;
    };
    let raw_code = obj.get("code");
    let code = raw_code
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_uppercase();
    let message = obj
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if code == "UNAUTHORIZED" || code == "API_KEY_REQUIRED" {
        return true;
    }
    if message.contains("authorization header is required")
        || message.contains("api key is required")
    {
        return true;
    }
    if raw_code.and_then(Value::as_i64) == Some(0) && obj.contains_key("data") {
        return true;
    }
    false
}

// ---------------------------------------------------------------- 主流程

pub(crate) struct PlatformDetection {
    pub(crate) platform: Option<String>,
    pub(crate) challenge: bool,
}

/// 完整检测流水线，返回规范平台名与是否命中盾页（供 Chrome 兜底）。
pub(crate) async fn detect_platform(client: &reqwest::Client, base_url: &str) -> PlatformDetection {
    if let Some(platform) = detect_platform_by_url_hint(base_url) {
        return PlatformDetection {
            platform: Some(platform.into()),
            challenge: false,
        };
    }

    let status_job = tauri::async_runtime::spawn({
        let client = client.clone();
        let base_url = base_url.to_string();
        async move { probe_json(&client, &base_url, "/api/status").await }
    });
    let auth_me_job = tauri::async_runtime::spawn({
        let client = client.clone();
        let base_url = base_url.to_string();
        async move { probe_json(&client, &base_url, "/api/v1/auth/me").await }
    });
    let models_job = tauri::async_runtime::spawn({
        let client = client.clone();
        let base_url = base_url.to_string();
        async move { probe_json(&client, &base_url, "/v1/models").await }
    });
    let cpa_job = tauri::async_runtime::spawn({
        let client = client.clone();
        let base_url = base_url.to_string();
        async move { probe_cliproxyapi(&client, &base_url).await }
    });
    let title_job = tauri::async_runtime::spawn({
        let client = client.clone();
        let base_url = base_url.to_string();
        async move { probe_title(&client, &base_url).await }
    });

    let status_probe = status_job.await.ok().flatten();
    let auth_me_probe = auth_me_job.await.ok().flatten();
    let models_probe = models_job.await.ok().flatten();
    let cpa_hit = cpa_job.await.ok().unwrap_or(false);
    let title = title_job.await.ok().flatten();

    // 1) 首页 <title> 高置信提示（title-first 平台）
    if let Some(title) = title.as_deref() {
        if let Some(platform) = title_hint(title) {
            if TITLE_FIRST_PLATFORMS.contains(&platform) {
                return PlatformDetection {
                    platform: Some(platform.into()),
                    challenge: false,
                };
            }
        }
    }

    // 2) cliproxyapi 专属管理端点
    if cpa_hit {
        return PlatformDetection {
            platform: Some("cliproxyapi".into()),
            challenge: false,
        };
    }

    // 3) /api/status 区分 veloera / new-api / one-api
    if let Some(probe) = status_probe.as_ref() {
        if let Some(json) = probe.json.as_ref() {
            if let Some(platform) = classify_api_status(json) {
                return PlatformDetection {
                    platform: Some(platform.into()),
                    challenge: false,
                };
            }
        }
    }

    // 4) sub2api 信封
    for probe in [auth_me_probe.as_ref(), models_probe.as_ref()] {
        if let Some(probe) = probe {
            if let Some(json) = probe.json.as_ref() {
                if sub2api_envelope(json) {
                    return PlatformDetection {
                        platform: Some("sub2api".into()),
                        challenge: false,
                    };
                }
            }
        }
    }

    // 5) <title> 兜底（new-api / one-api）
    if let Some(title) = title.as_deref() {
        if let Some(platform) = title_hint(title) {
            return PlatformDetection {
                platform: Some(platform.into()),
                challenge: false,
            };
        }
    }

    // 6) URL 低置信兜底（保留旧 OpenHub 行为）
    if let Some(platform) = low_priority_url_hint(base_url) {
        return PlatformDetection {
            platform: Some(platform.into()),
            challenge: false,
        };
    }

    let challenge = status_probe.as_ref().is_some_and(|probe| probe.challenge)
        || auth_me_probe.as_ref().is_some_and(|probe| probe.challenge)
        || models_probe.as_ref().is_some_and(|probe| probe.challenge);
    PlatformDetection {
        platform: None,
        challenge,
    }
}
