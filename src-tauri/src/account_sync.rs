use crate::chrome_local_storage;
use crate::chrome_session;
use crate::db::*;
use crate::models::*;
use crate::platform_detect::{is_newapi, is_sub2api, is_zero_v_zero};
use crate::site_ops::*;
use rusqlite::{params, OptionalExtension};
use serde_json;
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{Manager, State};
use url::Url;

pub(crate) fn json_number(value: &serde_json::Value, pointer: &str) -> Option<f64> {
    let value = value.pointer(pointer)?;
    let number = value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse::<f64>().ok())?;
    number.is_finite().then_some(number)
}

pub(crate) fn json_string(value: &serde_json::Value, pointers: &[&str]) -> String {
    pointers
        .iter()
        .find_map(|pointer| value.pointer(pointer))
        .and_then(|value| match value {
            serde_json::Value::String(value) => Some(value.trim().to_string()),
            serde_json::Value::Number(value) => Some(value.to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

pub(crate) fn api_error_message(value: &serde_json::Value, fallback: &str) -> String {
    let message = json_string(
        value,
        &[
            "/message",
            "/msg",
            "/error/message",
            "/error/msg",
            "/error",
            "/detail",
            "/data/message",
            "/data/error/message",
        ],
    );
    if message.is_empty() {
        fallback.to_string()
    } else {
        message
    }
}

pub(crate) fn parse_local_json(value: &str) -> Option<serde_json::Value> {
    let parsed = serde_json::from_str::<serde_json::Value>(value).ok()?;
    if let serde_json::Value::String(nested) = &parsed {
        serde_json::from_str(nested).ok().or(Some(parsed))
    } else {
        Some(parsed)
    }
}

pub(crate) fn local_scalar(value: &str) -> String {
    parse_local_json(value)
        .and_then(|value| match value {
            serde_json::Value::String(value) => Some(value),
            serde_json::Value::Number(value) => Some(value.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| value.trim().to_string())
}

pub(crate) fn parse_newapi_local_account(
    values: &HashMap<String, String>,
) -> Result<SiteAccountSnapshot, String> {
    let user = values
        .get("user")
        .and_then(|value| parse_local_json(value))
        .filter(serde_json::Value::is_object)
        .ok_or_else(|| "Chrome Local Storage 中没有有效的 NewAPI user 数据".to_string())?;
    let status = values
        .get("status")
        .and_then(|value| parse_local_json(value));
    let quota = json_number(&user, "/quota")
        .or_else(|| json_number(&user, "/data/quota"))
        .unwrap_or(0.0);
    let used_quota = json_number(&user, "/used_quota")
        .or_else(|| json_number(&user, "/data/used_quota"))
        .unwrap_or(0.0);
    let quota_per_unit = values
        .get("quota_per_unit")
        .map(|value| local_scalar(value))
        .and_then(|value| value.parse::<f64>().ok())
        .or_else(|| json_number(&user, "/quota_per_unit"))
        .or_else(|| json_number(&user, "/data/quota_per_unit"))
        .or_else(|| {
            status
                .as_ref()
                .and_then(|value| json_number(value, "/quota_per_unit"))
        })
        .or_else(|| {
            status
                .as_ref()
                .and_then(|value| json_number(value, "/data/quota_per_unit"))
        })
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(500_000.0);
    let display_type = values
        .get("quota_display_type")
        .map(|value| local_scalar(value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            json_string(&user, &["/quota_display_type", "/data/quota_display_type"])
        });
    let display_type = if display_type.is_empty() {
        status
            .as_ref()
            .map(|value| json_string(value, &["/quota_display_type", "/data/quota_display_type"]))
            .unwrap_or_default()
    } else {
        display_type
    };
    Ok(SiteAccountSnapshot {
        username: json_string(&user, &["/username", "/data/username"]),
        remaining: Some(quota / quota_per_unit),
        used: Some(used_quota / quota_per_unit),
        total: Some((quota + used_quota) / quota_per_unit),
        unit: if display_type.is_empty() {
            "USD".into()
        } else {
            display_type.to_ascii_uppercase()
        },
    })
}

pub(crate) fn parse_sub2api_account(
    value: &serde_json::Value,
) -> Result<SiteAccountSnapshot, String> {
    let code_valid = value
        .get("code")
        .is_some_and(|code| code.as_i64() == Some(0) || code.as_str() == Some("0"));
    let status_valid = value
        .pointer("/data/status")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|status| status.eq_ignore_ascii_case("active"));
    if !code_valid && !status_valid {
        return Err(api_error_message(value, "Sub2API 返回的账号数据无效"));
    }
    let remaining = ["/data/remaining", "/data/quota/remaining", "/data/balance"]
        .iter()
        .find_map(|pointer| json_number(value, pointer));
    let remaining = remaining.ok_or_else(|| "Sub2API 响应缺少有效的余额字段".to_string())?;
    let unit = json_string(value, &["/data/unit", "/data/quota/unit"]);
    Ok(SiteAccountSnapshot {
        username: json_string(value, &["/data/username"]),
        remaining: Some(remaining),
        used: None,
        total: None,
        unit: if unit.is_empty() { "USD".into() } else { unit },
    })
}

pub(crate) fn parse_sub2api_local_account(
    values: &HashMap<String, String>,
) -> Result<SiteAccountSnapshot, String> {
    let user = values
        .get("auth_user")
        .and_then(|value| parse_local_json(value))
        .filter(serde_json::Value::is_object)
        .ok_or_else(|| "Chrome Local Storage 中没有有效的 Sub2API auth_user 数据".to_string())?;
    let remaining = [
        "/remaining",
        "/quota/remaining",
        "/balance",
        "/data/remaining",
        "/data/quota/remaining",
        "/data/balance",
    ]
    .iter()
    .find_map(|pointer| json_number(&user, pointer))
    .unwrap_or(0.0);
    let unit = json_string(
        &user,
        &["/unit", "/quota/unit", "/data/unit", "/data/quota/unit"],
    );
    Ok(SiteAccountSnapshot {
        username: json_string(&user, &["/username", "/data/username"]),
        remaining: Some(remaining),
        used: None,
        total: None,
        unit: if unit.is_empty() { "USD".into() } else { unit },
    })
}

pub(crate) fn zero_v_zero_token(values: &HashMap<String, String>) -> Option<String> {
    values
        .get("0v0_token")
        .map(|value| local_scalar(value))
        .filter(|value| !value.is_empty())
}

pub(crate) fn parse_zero_v_zero_self(
    value: &serde_json::Value,
) -> Result<SiteAccountSnapshot, String> {
    if value.get("success").and_then(serde_json::Value::as_bool) != Some(true)
        || !value
            .pointer("/data")
            .is_some_and(serde_json::Value::is_object)
    {
        return Err(api_error_message(value, "0v0 返回的账号数据无效"));
    }
    let username = json_string(value, &["/data/username", "/data/display_name"]);
    let has_id = value.pointer("/data/id").is_some_and(|id| {
        id.as_u64().is_some_and(|id| id > 0) || id.as_str().is_some_and(|id| !id.trim().is_empty())
    });
    if username.is_empty() && !has_id {
        return Err("0v0 账号响应缺少用户标识".into());
    }
    let quota = json_number(value, "/data/quota").unwrap_or(0.0);
    let used_quota = json_number(value, "/data/used_quota").unwrap_or(0.0);
    Ok(SiteAccountSnapshot {
        username,
        remaining: Some(quota / 500_000.0),
        used: Some(used_quota / 500_000.0),
        total: Some((quota + used_quota) / 500_000.0),
        unit: "USD".into(),
    })
}

pub(crate) fn apply_zero_v_zero_stats(
    account: &mut SiteAccountSnapshot,
    value: &serde_json::Value,
) -> Result<(), String> {
    if value.get("success").and_then(serde_json::Value::as_bool) != Some(true)
        || !value
            .pointer("/data")
            .is_some_and(serde_json::Value::is_object)
    {
        return Err(api_error_message(value, "0v0 返回的额度统计无效"));
    }
    let remaining = json_number(value, "/data/total_quota")
        .ok_or_else(|| "0v0 额度统计缺少 total_quota".to_string())?;
    let used = json_number(value, "/data/used_quota").unwrap_or(0.0);
    account.remaining = Some(remaining / 500_000.0);
    account.used = Some(used / 500_000.0);
    account.total = Some((remaining + used) / 500_000.0);
    account.unit = "USD".into();
    Ok(())
}

pub(crate) fn has_local_account_session(
    system_type: &str,
    values: &HashMap<String, String>,
) -> bool {
    let has_newapi = parse_newapi_local_account(values).is_ok();
    let has_sub2api = parse_sub2api_local_account(values).is_ok();
    let has_zero_v_zero = zero_v_zero_token(values).is_some();
    if is_newapi(system_type) {
        has_newapi
    } else if is_sub2api(system_type) {
        has_sub2api
    } else if is_zero_v_zero(system_type) {
        has_zero_v_zero
    } else {
        has_newapi || has_sub2api || has_zero_v_zero
    }
}

pub(crate) fn has_account_session_candidate(
    system_type: &str,
    values: &HashMap<String, String>,
    cookie_names: &[String],
) -> bool {
    if has_local_account_session(system_type, values) {
        return true;
    }
    let system_type = system_type.trim().to_ascii_lowercase();
    (system_type.is_empty() || is_newapi(&system_type))
        && has_newapi_refresh_cookie_name(cookie_names.iter().map(String::as_str))
}

pub(crate) fn infer_system_type_from_local_accounts<'a>(
    accounts: impl IntoIterator<Item = &'a HashMap<String, String>>,
) -> &'static str {
    let mut has_newapi = false;
    let mut has_sub2api = false;
    let mut has_zero_v_zero = false;
    for values in accounts {
        has_newapi |= parse_newapi_local_account(values).is_ok();
        has_sub2api |= parse_sub2api_local_account(values).is_ok();
        has_zero_v_zero |= zero_v_zero_token(values).is_some();
    }
    if has_zero_v_zero {
        "0v0"
    } else if has_newapi {
        "new-api"
    } else if has_sub2api {
        "sub2api"
    } else {
        ""
    }
}

pub(crate) fn parse_newapi_account(
    value: &serde_json::Value,
) -> Result<SiteAccountSnapshot, String> {
    if value.get("success").and_then(serde_json::Value::as_bool) != Some(true)
        || !value
            .pointer("/data")
            .is_some_and(serde_json::Value::is_object)
    {
        return Err(api_error_message(value, "NewAPI 返回的账号数据无效"));
    }
    let quota = json_number(value, "/data/quota")
        .ok_or_else(|| "NewAPI 响应缺少有效的 quota".to_string())?;
    let used_quota = json_number(value, "/data/used_quota").unwrap_or(0.0);
    Ok(SiteAccountSnapshot {
        username: json_string(value, &["/data/username"]),
        remaining: Some(quota / 500_000.0),
        used: Some(used_quota / 500_000.0),
        total: Some((quota + used_quota) / 500_000.0),
        unit: "USD".into(),
    })
}

pub(crate) fn newapi_user_id(values: &HashMap<String, String>) -> Option<String> {
    let user = values
        .get("user")
        .and_then(|value| parse_local_json(value))
        .filter(serde_json::Value::is_object)?;
    let id = json_string(&user, &["/id", "/data/id"]);
    (!id.is_empty()).then_some(id)
}

pub(crate) fn has_newapi_refresh_cookie_name<'a>(names: impl IntoIterator<Item = &'a str>) -> bool {
    names
        .into_iter()
        .any(|name| name.trim() == "new_api_refresh")
}

pub(crate) fn cookie_header_has_name(cookie_header: &str, expected_name: &str) -> bool {
    cookie_header.split(';').any(|pair| {
        pair.trim()
            .split_once('=')
            .is_some_and(|(name, _)| name.trim() == expected_name)
    })
}

pub(crate) fn apply_newapi_auth(
    request: reqwest::RequestBuilder,
    auth: &NewApiAuth,
) -> reqwest::RequestBuilder {
    match auth {
        NewApiAuth::Legacy {
            cookie_header,
            user_id,
        } => request
            .header(reqwest::header::COOKIE, cookie_header)
            .header("new-api-user", user_id),
        NewApiAuth::Token {
            access_token,
            user_id,
        } => {
            let request = request.bearer_auth(access_token);
            if user_id.trim().is_empty() {
                request
            } else {
                request.header("new-api-user", user_id)
            }
        }
    }
}

/// 尝试调用 /api/user/token 获取永久 API Token（入库）。
/// 可以用 Cookie+user_id（旧版）或临时 Bearer Token（新版 refresh 后）调用。
/// 成功返回 `Some(token_string)`，遇盾返回 `Err(shield_error)`，其他失败返回 `None`。
pub(crate) async fn try_acquire_newapi_token(
    client: &reqwest::Client,
    base_url: &Url,
    auth: &NewApiAuth,
    user_agent: &str,
) -> Result<Option<String>, String> {
    let endpoint = match base_url.join("/api/user/token") {
        Ok(url) => url,
        Err(_) => return Ok(None),
    };
    let request = apply_newapi_auth(
        chrome_request_headers(client.get(endpoint), base_url.as_str(), user_agent),
        auth,
    );
    match request_json(request, "NewAPI Token 接口").await {
        Ok(value) => {
            let token = value
                .pointer("/data/token")
                .or_else(|| value.pointer("/data/access_token"))
                .or_else(|| value.pointer("/data/accessToken"))
                .or_else(|| value.pointer("/token"))
                .or_else(|| value.pointer("/access_token"))
                .and_then(serde_json::Value::as_str)
                .or_else(|| value.pointer("/data").and_then(serde_json::Value::as_str))
                .unwrap_or("")
                .trim()
                .to_string();
            if token.is_empty() {
                Ok(None)
            } else {
                Ok(Some(token))
            }
        }
        Err(error) => {
            if error.contains("返回 HTML") || error.contains("Cloudflare") {
                Err(error)
            } else {
                Ok(None)
            }
        }
    }
}

pub(crate) fn parse_newapi_checkin_status(
    value: &serde_json::Value,
) -> Result<(bool, bool), String> {
    if value.get("success").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(api_error_message(value, "签到状态数据无效"));
    }
    let enabled = value
        .pointer("/data/enabled")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| "签到状态缺少 enabled 字段".to_string())?;
    let checked_in_today = value
        .pointer("/data/stats/checked_in_today")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| "签到状态缺少 checked_in_today 字段".to_string())?;
    Ok((enabled, checked_in_today))
}

pub(crate) fn json_boolish(value: &serde_json::Value) -> Option<bool> {
    match value {
        serde_json::Value::Bool(value) => Some(*value),
        serde_json::Value::Number(value) => value.as_i64().map(|value| value != 0),
        serde_json::Value::String(value) => match value.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "checked" | "checked_in" | "success" => Some(true),
            "false" | "0" | "no" | "unchecked" | "not_checked" | "not_checked_in" | "pending" => {
                Some(false)
            }
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn sub2api_response_succeeded(value: &serde_json::Value) -> bool {
    value.get("success").and_then(json_boolish) == Some(true)
        || value
            .get("code")
            .is_some_and(|code| code.as_i64() == Some(0) || code.as_str() == Some("0"))
}

pub(crate) fn parse_sub2api_checkin_status(value: &serde_json::Value) -> Result<bool, String> {
    if value.get("success").and_then(json_boolish) == Some(false)
        || value.get("code").is_some_and(|code| {
            code.as_i64().is_some_and(|code| code != 0)
                || code.as_str().is_some_and(|code| code != "0")
        })
    {
        return Err(api_error_message(value, "Sub2API 签到状态数据无效"));
    }
    [
        "/data/checked_in_today",
        "/data/checked_in",
        "/data/is_checked_in",
        "/data/has_checked_in",
        "/data/today_checked",
        "/checked_in_today",
        "/checked_in",
        "/is_checked_in",
        "/has_checked_in",
        "/today_checked",
        "/data/status",
        "/status",
        "/data",
    ]
    .iter()
    .find_map(|pointer| value.pointer(pointer).and_then(json_boolish))
    .ok_or_else(|| "Sub2API 签到状态缺少今日签到字段".to_string())
}

pub(crate) async fn request_json(
    request: reqwest::RequestBuilder,
    label: &str,
) -> Result<serde_json::Value, String> {
    let response = request
        .send()
        .await
        .map_err(|error| format!("{label}请求失败：{error:#}"))?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let body = response
        .bytes()
        .await
        .map_err(|error| format!("{label}响应读取失败：{error:#}"))?;
    let body = body
        .strip_prefix(&[0xef, 0xbb, 0xbf])
        .unwrap_or(body.as_ref());
    let value = match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(value) => value,
        Err(error) => {
            let first = body
                .iter()
                .copied()
                .find(|byte| !byte.is_ascii_whitespace());
            if content_type.contains("text/html") || first == Some(b'<') {
                let reason = if status == reqwest::StatusCode::FORBIDDEN {
                    "Cloudflare 安全验证拦截了直接请求，请先用对应 Chrome 账号打开站点并通过验证"
                } else {
                    "站点返回了网页而不是 API 数据"
                };
                return Err(format!(
                    "{label} HTTP {} 返回 HTML：{reason}",
                    status.as_u16()
                ));
            }
            return Err(format!("{label}返回的 JSON 无法解析：{error}"));
        }
    };
    if !status.is_success() {
        return Err(format!(
            "{label} HTTP {}：{}",
            status.as_u16(),
            api_error_message(&value, "请求失败")
        ));
    }
    Ok(value)
}

pub(crate) fn access_token_was_rejected(error: &str) -> bool {
    error.contains(" HTTP 401")
}

pub(crate) fn chrome_request_headers(
    request: reqwest::RequestBuilder,
    base_url: &str,
    user_agent: &str,
) -> reqwest::RequestBuilder {
    let major = user_agent
        .split("Chrome/")
        .nth(1)
        .and_then(|value| value.split('.').next())
        .unwrap_or("120");
    request
        .header(reqwest::header::ACCEPT, "application/json, text/plain, */*")
        .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
        .header(reqwest::header::REFERER, base_url)
        .header(reqwest::header::USER_AGENT, user_agent)
        .header(
            "sec-ch-ua",
            format!(
                "\"Not_A Brand\";v=\"99\", \"Chromium\";v=\"{major}\", \"Google Chrome\";v=\"{major}\""
            ),
        )
        .header("sec-ch-ua-mobile", "?0")
        .header("sec-ch-ua-platform", "\"macOS\"")
        .header("sec-fetch-dest", "empty")
        .header("sec-fetch-mode", "cors")
        .header("sec-fetch-site", "same-origin")
}

pub(crate) async fn refresh_newapi_checkin(
    client: &reqwest::Client,
    base_url: &str,
    auth: &NewApiAuth,
    user_agent: &str,
    current_month: &str,
    previous: CheckinSnapshot,
) -> CheckinSnapshot {
    let endpoint = match Url::parse(base_url).and_then(|url| url.join("/api/user/checkin")) {
        Ok(url) => url,
        Err(_) => {
            return CheckinSnapshot {
                error: "无法生成签到接口地址".into(),
                ..previous
            }
        }
    };
    let mut query_url = endpoint.clone();
    query_url
        .query_pairs_mut()
        .append_pair("month", current_month);
    let headers = |request: reqwest::RequestBuilder| {
        apply_newapi_auth(chrome_request_headers(request, base_url, user_agent), auth)
    };
    let value = match request_json(headers(client.get(query_url)), "签到状态接口").await {
        Ok(value) => value,
        Err(error) => return CheckinSnapshot { error, ..previous },
    };
    let (enabled, checked_in_today) = match parse_newapi_checkin_status(&value) {
        Ok(status) => status,
        Err(error) => return CheckinSnapshot { error, ..previous },
    };
    if !enabled || checked_in_today {
        return CheckinSnapshot {
            enabled,
            checked_in_today,
            error: String::new(),
        };
    }
    let value = match request_json(headers(client.post(endpoint)), "签到接口").await {
        Ok(value) => value,
        Err(error) => {
            return CheckinSnapshot {
                enabled,
                checked_in_today: false,
                error,
            }
        }
    };
    if value.get("success").and_then(serde_json::Value::as_bool) == Some(true) {
        CheckinSnapshot {
            enabled: true,
            checked_in_today: true,
            error: String::new(),
        }
    } else {
        CheckinSnapshot {
            enabled: true,
            checked_in_today: false,
            error: api_error_message(&value, "签到失败"),
        }
    }
}

pub(crate) async fn refresh_sub2api_checkin(
    client: &reqwest::Client,
    base_url: &str,
    auth_token: &str,
    user_agent: &str,
    previous: CheckinSnapshot,
) -> CheckinSnapshot {
    let base_url = match Url::parse(base_url) {
        Ok(url) => url,
        Err(_) => {
            return CheckinSnapshot {
                error: "站点 API 地址无效".into(),
                ..previous
            }
        }
    };
    let status_url = match base_url.join("/api/v1/redeem/checkin/status") {
        Ok(url) => url,
        Err(_) => {
            return CheckinSnapshot {
                error: "无法生成 Sub2API 签到状态接口地址".into(),
                ..previous
            }
        }
    };
    let checkin_url = match base_url.join("/api/v1/redeem/checkin") {
        Ok(url) => url,
        Err(_) => {
            return CheckinSnapshot {
                error: "无法生成 Sub2API 签到接口地址".into(),
                ..previous
            }
        }
    };
    let headers = |request: reqwest::RequestBuilder| {
        chrome_request_headers(request, base_url.as_str(), user_agent).bearer_auth(auth_token)
    };
    let value = match request_json(headers(client.get(status_url)), "Sub2API 签到状态接口").await
    {
        Ok(value) => value,
        Err(error) => return CheckinSnapshot { error, ..previous },
    };
    let checked_in_today = match parse_sub2api_checkin_status(&value) {
        Ok(value) => value,
        Err(error) => return CheckinSnapshot { error, ..previous },
    };
    if checked_in_today {
        return CheckinSnapshot {
            enabled: true,
            checked_in_today: true,
            error: String::new(),
        };
    }
    let value = match request_json(headers(client.post(checkin_url)), "Sub2API 签到接口").await
    {
        Ok(value) => value,
        Err(error) => {
            return CheckinSnapshot {
                enabled: true,
                checked_in_today: false,
                error,
            }
        }
    };
    if sub2api_response_succeeded(&value) {
        CheckinSnapshot {
            enabled: true,
            checked_in_today: true,
            error: String::new(),
        }
    } else {
        CheckinSnapshot {
            enabled: true,
            checked_in_today: false,
            error: api_error_message(&value, "Sub2API 签到失败"),
        }
    }
}

pub(crate) async fn fetch_site_account(
    client: &reqwest::Client,
    base_url: &str,
    system_type: &str,
    local_values: &HashMap<String, String>,
    local_error: &str,
    cookie_header: Result<String, String>,
    user_agent: &str,
    current_month: &str,
    should_checkin: bool,
    previous_checkin: CheckinSnapshot,
    cached_newapi_token: Option<String>,
    cached_newapi_user_id: Option<String>,
) -> Result<SiteAccountRefresh, String> {
    let previous_checkin = if should_checkin {
        previous_checkin
    } else {
        CheckinSnapshot::default()
    };
    let inferred_type;
    let system_type =
        if is_newapi(system_type) || is_sub2api(system_type) || is_zero_v_zero(system_type) {
            system_type
        } else if zero_v_zero_token(local_values).is_some() {
            inferred_type = "0v0".to_string();
            &inferred_type
        } else if parse_newapi_local_account(local_values).is_ok() {
            inferred_type = "new-api".to_string();
            &inferred_type
        } else if parse_sub2api_local_account(local_values).is_ok() {
            inferred_type = "sub2api".to_string();
            &inferred_type
        } else {
            inferred_type = probe_site_system_type(client, base_url)
                .await
                .unwrap_or_default();
            &inferred_type
        };
    if is_zero_v_zero(system_type) {
        if !local_error.is_empty() {
            return Err(local_error.to_string());
        }
        let token = zero_v_zero_token(local_values)
            .ok_or_else(|| "Chrome Local Storage 中没有 0v0_token".to_string())?;
        let base_url =
            Url::parse(ZERO_V_ZERO_CONSOLE_URL).map_err(|_| "0v0 控制台地址无效".to_string())?;
        let self_url = base_url
            .join("/api/user/self")
            .map_err(|_| "无法生成 0v0 账号接口地址".to_string())?;
        let stats_url = base_url
            .join("/api/user/stats")
            .map_err(|_| "无法生成 0v0 额度接口地址".to_string())?;
        let self_job = tauri::async_runtime::spawn({
            let client = client.clone();
            let token = token.clone();
            let user_agent = user_agent.to_string();
            async move {
                request_json(
                    chrome_request_headers(
                        client.get(self_url),
                        ZERO_V_ZERO_CONSOLE_URL,
                        &user_agent,
                    )
                    .bearer_auth(token),
                    "0v0 账号接口",
                )
                .await
            }
        });
        let stats_job = tauri::async_runtime::spawn({
            let client = client.clone();
            let user_agent = user_agent.to_string();
            async move {
                request_json(
                    chrome_request_headers(
                        client.get(stats_url),
                        ZERO_V_ZERO_CONSOLE_URL,
                        &user_agent,
                    )
                    .bearer_auth(token),
                    "0v0 额度接口",
                )
                .await
            }
        });
        let self_value = self_job
            .await
            .map_err(|error| format!("0v0 账号同步任务失败：{error}"))??;
        let mut account = parse_zero_v_zero_self(&self_value)?;
        let sync_error = match stats_job.await {
            Ok(Ok(value)) => apply_zero_v_zero_stats(&mut account, &value).err(),
            Ok(Err(error)) => Some(error),
            Err(error) => Some(format!("0v0 额度同步任务失败：{error}")),
        }
        .unwrap_or_default();
        return Ok(SiteAccountRefresh {
            account,
            is_valid: true,
            sync_error,
            checkin: CheckinSnapshot::default(),
            newapi_token: String::new(),
            newapi_user_id: String::new(),
        });
    }
    if is_newapi(system_type) {
        let local_account = parse_newapi_local_account(local_values).ok();
        let base_url_parsed = Url::parse(base_url).map_err(|_| "站点 API 地址无效".to_string())?;

        // ── Step 1: 先查本地 DB 缓存的 user_id + api_token ──
        if let Some(cached_token) = &cached_newapi_token {
            if !cached_token.is_empty() {
                let cached_auth = NewApiAuth::Token {
                    access_token: cached_token.clone(),
                    user_id: cached_newapi_user_id.clone().unwrap_or_default(),
                };
                let checkin = if should_checkin {
                    refresh_newapi_checkin(
                        client,
                        base_url,
                        &cached_auth,
                        user_agent,
                        current_month,
                        previous_checkin,
                    )
                    .await
                } else {
                    CheckinSnapshot::default()
                };
                let endpoint = base_url_parsed
                    .join("/api/user/self")
                    .map_err(|_| "无法生成账号接口地址".to_string())?;
                let cached_result = request_json(
                    apply_newapi_auth(
                        chrome_request_headers(client.get(endpoint), base_url, user_agent),
                        &cached_auth,
                    ),
                    "账号接口",
                )
                .await;

                match cached_result {
                    Ok(value) => match parse_newapi_account(&value) {
                        Ok(remote) => {
                            return Ok(SiteAccountRefresh {
                                account: remote,
                                is_valid: true,
                                sync_error: String::new(),
                                checkin,
                                newapi_token: cached_token.clone(),
                                newapi_user_id: cached_newapi_user_id.clone().unwrap_or_default(),
                            });
                        }
                        Err(error) => {
                            return match local_account {
                                Some(account) => Ok(SiteAccountRefresh {
                                    account,
                                    is_valid: true,
                                    sync_error: error,
                                    checkin: checkin.clone(),
                                    newapi_token: cached_token.clone(),
                                    newapi_user_id: cached_newapi_user_id
                                        .clone()
                                        .unwrap_or_default(),
                                }),
                                None => Err(error),
                            };
                        }
                    },
                    Err(error) if access_token_was_rejected(&error) => {
                        let message =
                            "缓存访问令牌已失效，将通过 Chrome 同源 refresh 更新浏览器会话"
                                .to_string();
                        return match local_account {
                            Some(account) => Ok(SiteAccountRefresh {
                                account,
                                is_valid: false,
                                sync_error: message,
                                checkin: checkin.clone(),
                                newapi_token: cached_token.clone(),
                                newapi_user_id: cached_newapi_user_id.clone().unwrap_or_default(),
                            }),
                            None => Err(message),
                        };
                    }
                    Err(error) => {
                        return match local_account {
                            Some(account) => Ok(SiteAccountRefresh {
                                account,
                                is_valid: true,
                                sync_error: error,
                                checkin: checkin.clone(),
                                newapi_token: cached_token.clone(),
                                newapi_user_id: cached_newapi_user_id.clone().unwrap_or_default(),
                            }),
                            None => Err(error),
                        };
                    }
                }
            }
        }

        // ── Step 2: 获取新 token（新版 refresh 或旧版 Cookie）──
        let cookie_header = match cookie_header {
            Ok(value) => value,
            Err(error) => {
                return match local_account {
                    Some(account) => Ok(SiteAccountRefresh {
                        account,
                        is_valid: true,
                        sync_error: error,
                        checkin: previous_checkin,
                        newapi_token: String::new(),
                        newapi_user_id: String::new(),
                    }),
                    None => Err(error),
                }
            }
        };
        let has_refresh_cookie = cookie_header_has_name(&cookie_header, "new_api_refresh");
        let user_id = newapi_user_id(local_values);
        if !has_refresh_cookie && user_id.is_none() {
            return match local_account {
                Some(account) => Ok(SiteAccountRefresh {
                    account,
                    is_valid: true,
                    sync_error: "NewAPI 本地 user 数据缺少用户 ID".into(),
                    checkin: previous_checkin,
                    newapi_token: String::new(),
                    newapi_user_id: String::new(),
                }),
                None => Err(if local_error.is_empty() {
                    "没有找到可用的 NewAPI 登录凭据".into()
                } else {
                    local_error.to_string()
                }),
            };
        }
        let user_id = user_id.unwrap_or_default();

        // 新版 refresh 必须由 Chrome 同源 fetch 执行，才能让浏览器接收轮换后的 HttpOnly Cookie。
        let temp_auth = if has_refresh_cookie {
            let message =
                "检测到 NewAPI refresh cookie，将通过 Chrome 同源请求刷新并写回浏览器".to_string();
            return match local_account {
                Some(account) => Ok(SiteAccountRefresh {
                    account,
                    is_valid: false,
                    sync_error: message,
                    checkin: previous_checkin,
                    newapi_token: String::new(),
                    newapi_user_id: user_id,
                }),
                None => Err(message),
            };
        } else {
            // 旧版流程：Cookie + user_id
            NewApiAuth::Legacy {
                cookie_header: cookie_header.clone(),
                user_id: user_id.clone(),
            }
        };

        // 用临时 token 或 Cookie 调 /api/user/token 获取永久 API Token
        let mut api_token = match try_acquire_newapi_token(
            client,
            &base_url_parsed,
            &temp_auth,
            user_agent,
        )
        .await
        {
            Ok(Some(token)) => token,
            Ok(None) => String::new(),
            Err(shield_error) => {
                // 遇盾 → 需要 Chrome 浏览器验证
                return match local_account {
                    Some(account) => Ok(SiteAccountRefresh {
                        account,
                        is_valid: false,
                        sync_error: format!(
                            "NewAPI Token 接口遇到安全验证，需通过 Chrome 同步：{shield_error}"
                        ),
                        checkin: previous_checkin,
                        newapi_token: String::new(),
                        newapi_user_id: String::new(),
                    }),
                    None => Err(shield_error),
                };
            }
        };

        if api_token.is_empty() {
            if let NewApiAuth::Token { access_token, .. } = &temp_auth {
                api_token = access_token.clone();
            }
        }

        // ── Step 4: 后续签到、self 与 Key 管理接口只允许使用访问令牌 ──
        let access_token = if !api_token.is_empty() {
            api_token
        } else if let NewApiAuth::Token { access_token, .. } = &temp_auth {
            access_token.clone()
        } else {
            let message = "未取得 NewAPI 访问令牌，停止账号接口同步".to_string();
            return match local_account {
                Some(account) => Ok(SiteAccountRefresh {
                    account,
                    is_valid: false,
                    sync_error: message,
                    checkin: previous_checkin,
                    newapi_token: String::new(),
                    newapi_user_id: user_id,
                }),
                None => Err(message),
            };
        };
        let auth = NewApiAuth::Token {
            access_token,
            user_id: user_id.clone(),
        };
        let (newapi_token, newapi_user_id) = match &auth {
            NewApiAuth::Token {
                access_token,
                user_id,
            } => (access_token.clone(), user_id.clone()),
            _ => (String::new(), String::new()),
        };

        let checkin = if should_checkin {
            refresh_newapi_checkin(
                client,
                base_url,
                &auth,
                user_agent,
                current_month,
                previous_checkin,
            )
            .await
        } else {
            CheckinSnapshot::default()
        };

        let endpoint = base_url_parsed
            .join("/api/user/self")
            .map_err(|_| "无法生成账号接口地址".to_string())?;
        let remote_result = request_json(
            apply_newapi_auth(
                chrome_request_headers(client.get(endpoint), base_url, user_agent),
                &auth,
            ),
            "账号接口",
        )
        .await
        .and_then(|value| {
            let account = parse_newapi_account(&value)?;
            let response_user_id =
                json_string(&value, &["/data/id", "/data/userId", "/id", "/userId"]);
            Ok((account, response_user_id))
        });

        let (remote, response_user_id) = match remote_result {
            Ok(result) => result,
            Err(error) => {
                return match local_account {
                    Some(account) => Ok(SiteAccountRefresh {
                        account,
                        is_valid: !access_token_was_rejected(&error),
                        sync_error: error,
                        checkin,
                        newapi_token: newapi_token.clone(),
                        newapi_user_id: newapi_user_id.clone(),
                    }),
                    None => Err(format!("账号接口失败：{error}")),
                }
            }
        };

        let newapi_user_id = if newapi_user_id.is_empty() {
            response_user_id
        } else {
            newapi_user_id
        };
        return Ok(SiteAccountRefresh {
            account: remote,
            is_valid: true,
            sync_error: String::new(),
            checkin,
            newapi_token,
            newapi_user_id,
        });
    }
    if !local_error.is_empty() {
        return Err(local_error.to_string());
    }
    let local_account = parse_sub2api_local_account(local_values)?;
    let auth_token = local_values
        .get("auth_token")
        .map(|value| local_scalar(value))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Sub2API 本地数据中没有 auth_token".to_string());
    let auth_token = match auth_token {
        Ok(value) => value,
        Err(error) => {
            return Ok(SiteAccountRefresh {
                account: local_account,
                is_valid: true,
                sync_error: error,
                checkin: previous_checkin,
                newapi_token: String::new(),
                newapi_user_id: String::new(),
            })
        }
    };
    let endpoint = if is_sub2api(system_type) {
        "/api/v1/auth/me"
    } else {
        return Ok(SiteAccountRefresh {
            account: local_account,
            is_valid: true,
            sync_error: "站点类型未识别，未请求账号接口".into(),
            checkin: previous_checkin,
            newapi_token: String::new(),
            newapi_user_id: String::new(),
        });
    };
    let url = Url::parse(base_url)
        .map_err(|_| "站点 API 地址无效".to_string())?
        .join(endpoint)
        .map_err(|_| "无法生成账号接口地址".to_string())?;
    let checkin = if should_checkin {
        refresh_sub2api_checkin(client, base_url, &auth_token, user_agent, previous_checkin).await
    } else {
        CheckinSnapshot::default()
    };
    let request =
        chrome_request_headers(client.get(url), base_url, user_agent).bearer_auth(&auth_token);
    let account = request_json(request, "账号接口")
        .await
        .and_then(|value| parse_sub2api_account(&value));
    match account {
        Ok(account) => Ok(SiteAccountRefresh {
            account,
            is_valid: true,
            sync_error: String::new(),
            checkin,
            newapi_token: String::new(),
            newapi_user_id: String::new(),
        }),
        Err(error) => Ok(SiteAccountRefresh {
            account: local_account,
            is_valid: true,
            sync_error: error,
            checkin,
            newapi_token: String::new(),
            newapi_user_id: String::new(),
        }),
    }
}

pub(crate) fn chrome_account_bridge_script(
    user_id: Option<&str>,
    current_month: &str,
    marker: &str,
    use_refresh_auth: bool,
    should_checkin: bool,
    allow_challenge_navigation: bool,
) -> String {
    let user_id =
        serde_json::to_string(user_id.unwrap_or_default()).unwrap_or_else(|_| "\"\"".into());
    let current_month = serde_json::to_string(current_month).unwrap_or_else(|_| "\"\"".into());
    let marker = serde_json::to_string(marker).unwrap_or_else(|_| "\"\"".into());
    r#"(() => {
  const token = __OPENHUB_MARKER__;
  const legacyUserId = __OPENHUB_USER_ID__;
  const useRefreshAuth = __OPENHUB_USE_REFRESH_AUTH__;
  const shouldCheckin = __OPENHUB_SHOULD_CHECKIN__;
  const allowChallengeNavigation = __OPENHUB_ALLOW_CHALLENGE_NAVIGATION__;
  const requestTimeout = 30000;
  const pending = "__OPENHUB_PENDING__";
  if (window.location.protocol !== "http:" && window.location.protocol !== "https:") {
    return pending;
  }
  if (legacyUserId) {
    try {
      let storedUser = localStorage.getItem("user") || "null";
      for (let depth = 0; depth < 2 && typeof storedUser === "string"; depth += 1) {
        storedUser = JSON.parse(storedUser);
      }
      const storedUserId = storedUser?.id ?? storedUser?.data?.id ?? "";
      if (String(storedUserId) !== String(legacyUserId)) {
        return "__OPENHUB_PROFILE_MISMATCH__";
      }
    } catch (_) {
      return "__OPENHUB_PROFILE_MISMATCH__";
    }
  }
  const previous = window.__openHubAccountSync;
  if (previous && previous.token === token) {
    if (previous.result) return JSON.stringify(previous.result);
    if (previous.state !== "challenge" || Date.now() - previous.started < 3000) return pending;
  }
  const bridge = { token, started: Date.now(), state: "running", result: null };
  window.__openHubAccountSync = bridge;
  const readResponse = async (response) => {
    const contentType = (response.headers.get("content-type") || "").toLowerCase();
    const text = await response.text();
    const lower = text.slice(0, 100000).toLowerCase();
    const isHtml = contentType.includes("text/html") || /^\s*<!doctype html|^\s*<html/i.test(text);
    const isChallenge = isHtml && (
      [403, 429, 503].includes(response.status) ||
      lower.includes("cf-chl-") || lower.includes("challenge-platform") ||
      lower.includes("just a moment") || lower.includes("attention required") ||
      lower.includes("cloudflare ray id")
    );
    if (isChallenge) {
      return { challenge: true, status: response.status };
    }
    if (isHtml) {
      return { status: response.status, error: "账号接口返回 HTML，站点 API 地址或系统类型可能不正确" };
    }
    try {
      return { status: response.status, data: JSON.parse(text) };
    } catch (_) {
      return { status: response.status, error: "接口没有返回 JSON" };
    }
  };
  const messageOf = (value, fallback) =>
    value && (value.message || value.msg || value.error) || fallback;
  (async () => {
    const headers = { "Accept": "application/json" };
    let activeAccessToken = "";
    if (useRefreshAuth) {
      const refreshResponse = await readResponse(await fetch("/api/user/auth/refresh", {
        method: "POST", credentials: "include", cache: "no-store", headers,
        signal: AbortSignal.timeout(requestTimeout)
      }));
      if (refreshResponse.challenge) {
        if (!allowChallengeNavigation) {
          bridge.result = { ok: false, error: "Cloudflare 验证仍需要浏览器交互" };
          return;
        }
        bridge.state = "challenge";
        bridge.started = Date.now();
        window.location.assign(`/#${token}`);
        return;
      }
      let accessToken = refreshResponse.data?.data?.access_token ||
        refreshResponse.data?.data?.accessToken || refreshResponse.data?.data?.token ||
        refreshResponse.data?.access_token || refreshResponse.data?.accessToken ||
        refreshResponse.data?.token || "";
      if (!accessToken && typeof refreshResponse.data?.data === "string") {
        accessToken = refreshResponse.data.data;
      }
      if (!accessToken) {
        bridge.result = {
          ok: false,
          error: messageOf(
            refreshResponse.data,
            refreshResponse.error || `刷新认证接口 HTTP ${refreshResponse.status}`
          )
        };
        return;
      }
      activeAccessToken = accessToken;
      headers.Authorization = `Bearer ${accessToken}`;
    } else if (legacyUserId) {
      headers["New-Api-User"] = legacyUserId;
    } else {
      bridge.result = {
        ok: false,
        error: "传统 NewAPI 会话缺少用户 ID"
      };
      return;
    }
    let apiToken = activeAccessToken;
    let userId = legacyUserId || "";
    try {
      const tokenResponse = await readResponse(await fetch("/api/user/token", {
        method: "GET", credentials: "include", cache: "no-store", headers,
        signal: AbortSignal.timeout(requestTimeout)
      }));
      if (!tokenResponse.challenge && !tokenResponse.error && tokenResponse.status >= 200 && tokenResponse.status < 300) {
        const permanentToken = tokenResponse.data?.data?.token || tokenResponse.data?.data?.access_token ||
          tokenResponse.data?.data?.accessToken || tokenResponse.data?.token ||
          tokenResponse.data?.access_token || tokenResponse.data?.accessToken ||
          (typeof tokenResponse.data?.data === "string" ? tokenResponse.data.data : "");
        if (permanentToken) apiToken = permanentToken;
      }
    } catch (_) {}
    if (!apiToken) {
      bridge.result = { ok: false, error: "未取得 NewAPI 访问令牌，停止账号接口同步" };
      return;
    }
    headers.Authorization = `Bearer ${apiToken}`;
    let checkinEnabled = false;
    let checkedInToday = false;
    let checkinError = "";
    if (shouldCheckin) {
      try {
        const checkinUrl = `/api/user/checkin?month=${encodeURIComponent(__OPENHUB_MONTH__)}`;
        const checkinResponse = await readResponse(await fetch(checkinUrl, {
          method: "GET", credentials: "omit", cache: "no-store", headers,
          signal: AbortSignal.timeout(requestTimeout)
        }));
        if (checkinResponse.challenge) {
          checkinError = "Cloudflare 拦截了签到状态请求";
        } else if (checkinResponse.error || checkinResponse.status < 200 || checkinResponse.status >= 300) {
          checkinError = messageOf(
            checkinResponse.data,
            checkinResponse.error || `签到状态接口 HTTP ${checkinResponse.status}`
          );
        } else if (checkinResponse.data && checkinResponse.data.success === true) {
          checkinEnabled = checkinResponse.data.data?.enabled === true;
          checkedInToday = checkinResponse.data.data?.stats?.checked_in_today === true;
          if (checkinEnabled && !checkedInToday) {
            const postResponse = await readResponse(await fetch("/api/user/checkin", {
              method: "POST", credentials: "omit", cache: "no-store", headers,
              signal: AbortSignal.timeout(requestTimeout)
            }));
            if (postResponse.challenge) {
              checkinError = "Cloudflare 拦截了签到请求";
            } else if (
              postResponse.error || postResponse.status < 200 || postResponse.status >= 300 ||
              !postResponse.data || postResponse.data.success !== true
            ) {
              checkinError = messageOf(
                postResponse.data,
                postResponse.error || `签到接口 HTTP ${postResponse.status}`
              );
            } else {
              checkedInToday = true;
            }
          }
        } else {
          checkinError = messageOf(checkinResponse.data, "签到状态数据无效");
        }
      } catch (error) {
        checkinError = String(error && error.message || error);
      }
    }
    const selfResponse = await readResponse(await fetch("/api/user/self", {
      method: "GET", credentials: "omit", cache: "no-store", headers,
      signal: AbortSignal.timeout(requestTimeout)
    }));
    if (selfResponse.challenge) {
      if (!allowChallengeNavigation) {
        bridge.result = { ok: false, error: "Cloudflare 验证仍需要浏览器交互" };
        return;
      }
      bridge.state = "challenge";
      bridge.started = Date.now();
      if (window.location.pathname !== "/api/user/self") {
        window.location.assign(`/api/user/self#${token}`);
      }
      return;
    }
    if (selfResponse.error || selfResponse.status < 200 || selfResponse.status >= 300) {
      bridge.result = {
        ok: false,
        error: messageOf(selfResponse.data, selfResponse.error || `账号接口 HTTP ${selfResponse.status}`)
      };
      return;
    }
    const responseUserId = selfResponse.data?.data?.id || selfResponse.data?.data?.userId || "";
    if (responseUserId) userId = String(responseUserId);
    bridge.result = {
      ok: true,
      account: selfResponse.data,
      checkinEnabled,
      checkedInToday,
      checkinError,
      apiToken,
      userId
    };
  })().catch((error) => {
    const message = String(error && error.message || error);
    if (message.includes("Failed to parse URL")) {
      bridge.started = 0;
      return;
    }
    bridge.result = { ok: false, error: message };
  });
  return pending;
})()"#
        .replace("__OPENHUB_USER_ID__", &user_id)
        .replace("__OPENHUB_MONTH__", &current_month)
        .replace(
            "__OPENHUB_USE_REFRESH_AUTH__",
            if use_refresh_auth { "true" } else { "false" },
        )
        .replace("__OPENHUB_SHOULD_CHECKIN__", if should_checkin { "true" } else { "false" })
        .replace(
            "__OPENHUB_ALLOW_CHALLENGE_NAVIGATION__",
            if allow_challenge_navigation {
                "true"
            } else {
                "false"
            },
        )
        .replace("__OPENHUB_MARKER__", &marker)
}

pub(crate) fn parse_chrome_account_bridge_result(
    value: &str,
) -> Result<(SiteAccountSnapshot, ChromeBridgeAccountResult), String> {
    let result = serde_json::from_str::<ChromeBridgeAccountResult>(value)
        .map_err(|error| format!("Chrome 返回的账号数据格式无效：{error}"))?;
    if !result.ok {
        return Err(if result.error.is_empty() {
            "Chrome 账号请求失败".into()
        } else {
            format!("Chrome 账号请求失败：{}", result.error)
        });
    }
    let account = result
        .account
        .as_ref()
        .ok_or_else(|| "Chrome 返回结果缺少账号数据".to_string())
        .and_then(parse_newapi_account)?;
    Ok((account, result))
}

#[tauri::command]
pub async fn sync_site_account_via_chrome(
    app: tauri::AppHandle,
    database: State<'_, Database>,
    site_id: String,
    profile_id: String,
    run_id: u64,
) -> Result<chrome_session::ChromeSessionInfo, String> {
    let site_id = site_id.trim().to_string();
    let profile_id = profile_id.trim().to_string();
    if site_id.is_empty() || profile_id.is_empty() {
        return Err("站点或 Chrome Profile 标识为空".into());
    }
    let (
        site_name,
        api_base_url,
        system_type,
        checkin_url,
        supports_checkin,
        current_month,
        cookie_names,
        profile_name,
        account_name,
        cached_token,
        cached_uid,
    ) = {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        let site = connection
            .query_row(
                "SELECT name, api_base_url, system_type, checkin_url, supports_checkin
                 FROM directory_sites WHERE id = ?1 AND is_personal = 1",
                [&site_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)? != 0,
                    ))
                },
            )
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "找不到对应的在用站点".to_string())?;
        let account_row: Option<(String, String, Vec<String>, Option<String>, Option<String>)> = connection
            .query_row(
                "SELECT profile_name, account_name, cookie_names, newapi_token, newapi_user_id FROM site_accounts WHERE site_id = ?1 AND profile_id = ?2",
                params![site_id, profile_id],
                |row| {
                    let cookie_names_json: String = row.get(2)?;
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        serde_json::from_str::<Vec<String>>(&cookie_names_json).unwrap_or_default(),
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| error.to_string())?;
        let Some((profile_name, account_name, cookie_names, cached_token, cached_uid)) =
            account_row
        else {
            return Err("该 Chrome Profile 尚未建立本地账号缓存，请先同步会话".into());
        };
        let current_month: String = connection
            .query_row("SELECT strftime('%Y-%m', 'now', 'localtime')", [], |row| {
                row.get(0)
            })
            .map_err(|error| error.to_string())?;
        (
            site.0,
            site.1,
            site.2,
            site.3,
            site.4,
            current_month,
            cookie_names,
            profile_name,
            account_name,
            cached_token,
            cached_uid,
        )
    };
    let account_label = if account_name.is_empty() {
        profile_name.clone()
    } else {
        format!("{profile_name} · {account_name}")
    };
    if !is_newapi(&system_type) {
        return Err("当前仅对 NewAPI 账号提供 Chrome 同步".into());
    }

    emit_chrome_account_progress(
        &app,
        run_id,
        "local-account",
        "running",
        format!("正在读取 {account_label} 的本地账号"),
    );

    let base_url = Url::parse(&api_base_url).map_err(|_| "站点 API 地址无效")?;
    let origin = base_url.origin().ascii_serialization();
    if origin == "null" {
        return Err("站点 API 地址缺少有效来源".into());
    }
    let home_dir = app
        .path()
        .home_dir()
        .map_err(|error| format!("无法定位用户目录：{error}"))?;
    let local_target = chrome_local_storage::LocalStorageTarget {
        site_id: site_id.clone(),
        profile_id: profile_id.clone(),
        origin,
    };
    let local_match = tauri::async_runtime::spawn_blocking({
        let home_dir = home_dir.clone();
        move || chrome_local_storage::read_local_storage_from_home(&home_dir, &[local_target])
    })
    .await
    .map_err(|error| format!("读取 Chrome Local Storage 任务失败：{error}"))?
    .into_iter()
    .next();
    let has_refresh_cookie =
        has_newapi_refresh_cookie_name(cookie_names.iter().map(String::as_str));
    let local_values = local_match
        .as_ref()
        .filter(|item| item.error.is_empty())
        .map(|item| &item.values);
    let local_account_valid =
        local_values.is_some_and(|values| parse_newapi_local_account(values).is_ok());
    let user_id = local_values.and_then(newapi_user_id);
    if !has_refresh_cookie && !local_account_valid {
        return Err(local_match
            .and_then(|item| (!item.error.is_empty()).then_some(item.error))
            .unwrap_or_else(|| "没有找到可用的 NewAPI 本地账号或刷新会话".into()));
    }
    emit_chrome_account_progress(
        &app,
        run_id,
        "local-account",
        "success",
        format!(
            "{account_label} 认证策略：{}",
            if has_refresh_cookie {
                "NewAPI 刷新令牌（new_api_refresh → Bearer Token）"
            } else {
                "传统 NewAPI 会话（session Cookie + New-Api-User）"
            }
        ),
    );

    let mut resolved_account = None;

    // 先查本地 DB 缓存的 user_id + api_token
    if let Some(cached_token) = &cached_token {
        if !cached_token.is_empty() {
            emit_chrome_account_progress(
                &app,
                run_id,
                "token-cache",
                "running",
                format!("正在使用 {account_label} 的缓存凭证验证"),
            );
            if let Ok(client) = crate::db::build_http_client_for_site(
                &database,
                &site_id,
                Duration::from_secs(8),
                3,
                "账号接口请求",
            ) {
                let cached_auth = NewApiAuth::Token {
                    access_token: cached_token.clone(),
                    user_id: cached_uid.clone().unwrap_or_default(),
                };
                if let Ok(endpoint) = base_url.join("/api/user/self") {
                    let user_agent = chrome_session::chrome_user_agent();
                    let checkin = if supports_checkin {
                        refresh_newapi_checkin(
                            &client,
                            base_url.as_str(),
                            &cached_auth,
                            &user_agent,
                            &current_month,
                            CheckinSnapshot::default(),
                        )
                        .await
                    } else {
                        CheckinSnapshot::default()
                    };
                    let cached_result = request_json(
                        apply_newapi_auth(
                            chrome_request_headers(
                                client.get(endpoint),
                                base_url.as_str(),
                                &user_agent,
                            ),
                            &cached_auth,
                        ),
                        "账号接口",
                    )
                    .await;

                    match cached_result {
                        Ok(value) => match parse_newapi_account(&value) {
                            Ok(account) => {
                                emit_chrome_account_progress(
                                    &app,
                                    run_id,
                                    "token-cache",
                                    "success",
                                    format!("{account_label} 缓存访问令牌有效，跳过浏览器同步"),
                                );
                                resolved_account = Some((
                                    account,
                                    ChromeBridgeAccountResult {
                                        ok: true,
                                        error: String::new(),
                                        api_token: cached_token.clone(),
                                        user_id: cached_uid.clone().unwrap_or_default(),
                                        checkin_enabled: checkin.enabled,
                                        checked_in_today: checkin.checked_in_today,
                                        checkin_error: checkin.error,
                                        account: None,
                                    },
                                ));
                            }
                            Err(error) => {
                                emit_chrome_account_progress(
                                    &app,
                                    run_id,
                                    "token-cache",
                                    "error",
                                    format!("{account_label} 访问令牌响应无法解析，不执行 refresh token：{error}"),
                                );
                                return Err(error);
                            }
                        },
                        Err(error) if access_token_was_rejected(&error) => {
                            emit_chrome_account_progress(
                                &app,
                                run_id,
                                "token-cache",
                                "success",
                                format!("{account_label} 访问令牌收到 HTTP 401，开始重新获取"),
                            );
                        }
                        Err(error) => {
                            emit_chrome_account_progress(
                                &app,
                                run_id,
                                "token-cache",
                                "error",
                                format!("{account_label} 访问令牌请求失败，不执行 refresh token：{error}"),
                            );
                            return Err(error);
                        }
                    }
                }
            }
        }
    }

    let silent_timeout = if has_refresh_cookie {
        Duration::from_secs(35)
    } else {
        Duration::from_secs(20)
    };
    let background_timeout = if has_refresh_cookie {
        Duration::from_secs(35)
    } else {
        Duration::from_secs(25)
    };
    let visible_timeout = if has_refresh_cookie {
        Duration::from_secs(120)
    } else {
        Duration::from_secs(60)
    };

    // refresh 模式下即使没有 user_id 也允许静默请求（bridge 脚本不依赖 user_id）
    let can_silent = (user_id.is_some() || has_refresh_cookie) && resolved_account.is_none();
    if can_silent {
        let silent_marker = format!(
            "openhub-silent-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "系统时间异常")?
                .as_nanos()
        );
        let silent_javascript = chrome_account_bridge_script(
            user_id.as_deref(),
            &current_month,
            &silent_marker,
            has_refresh_cookie,
            supports_checkin,
            false,
        );
        emit_chrome_account_progress(
            &app,
            run_id,
            "browser-bypass",
            "running",
            format!("正在尝试复用已打开的同账号 Chrome 页面（{account_label}），不切换窗口"),
        );
        let silent_attempt = tauri::async_runtime::spawn_blocking({
            let base_url = base_url.to_string();
            move || {
                chrome_session::run_javascript_in_existing_chrome_tab(
                    &base_url,
                    &silent_javascript,
                    silent_timeout,
                )
            }
        })
        .await;
        match silent_attempt {
            Ok(Ok(Some(value))) => match parse_chrome_account_bridge_result(&value) {
                Ok(parsed) => {
                    emit_chrome_account_progress(
                        &app,
                        run_id,
                        "browser-bypass",
                        "success",
                        "已通过现有 Chrome 页面静默获取账号数据",
                    );
                    resolved_account = Some(parsed);
                }
                Err(error) => emit_chrome_account_progress(
                    &app,
                    run_id,
                    "browser-bypass",
                    "success",
                    format!("现有页面静默请求未通过，继续尝试后台 Chrome：{error}"),
                ),
            },
            Ok(Ok(None)) => emit_chrome_account_progress(
                &app,
                run_id,
                "browser-bypass",
                "success",
                "没有找到已打开的同账号站点页面，继续尝试后台 Chrome",
            ),
            Ok(Err(error)) => {
                if chrome_session::is_blocking_chrome_automation_error(&error) {
                    return Err(error);
                }
                emit_chrome_account_progress(
                    &app,
                    run_id,
                    "browser-bypass",
                    "success",
                    format!("现有页面静默请求不可用，继续尝试后台 Chrome：{error}"),
                )
            }
            Err(error) => emit_chrome_account_progress(
                &app,
                run_id,
                "browser-bypass",
                "success",
                format!("现有页面静默任务失败，继续尝试后台 Chrome：{error}"),
            ),
        }
    } else {
        emit_chrome_account_progress(
            &app,
            run_id,
            "browser-bypass",
            "info",
            format!("{account_label} 缺少可核验的用户 ID 且非刷新模式，跳过静默请求以避免串用 Chrome 账号"),
        );
    }

    if resolved_account.is_none() {
        let marker = format!(
            "openhub-background-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "系统时间异常")?
                .as_nanos()
        );
        let mut browser_url = if !checkin_url.trim().is_empty() {
            Url::parse(&checkin_url).unwrap_or_else(|_| base_url.clone())
        } else {
            base_url
                .join("/console/personal")
                .map_err(|_| "无法生成 Chrome 验证地址")?
        };
        if browser_url.origin() != base_url.origin() {
            browser_url = base_url
                .join("/console/personal")
                .map_err(|_| "无法生成 Chrome 验证地址")?;
        }
        browser_url.set_fragment(Some(&marker));
        let javascript = chrome_account_bridge_script(
            user_id.as_deref(),
            &current_month,
            &marker,
            has_refresh_cookie,
            supports_checkin,
            true,
        );
        emit_chrome_account_progress(
            &app,
            run_id,
            "browser-background",
            "running",
            format!("正在后台打开 {account_label} 的 Chrome 并尝试自动通过验证"),
        );
        let background_attempt = tauri::async_runtime::spawn_blocking({
            let browser_url = browser_url.to_string();
            let profile_id = profile_id.clone();
            let marker = marker.clone();
            move || {
                chrome_session::run_javascript_in_background_chrome_profile(
                    &browser_url,
                    &profile_id,
                    &marker,
                    &javascript,
                    background_timeout,
                )
            }
        })
        .await;
        match background_attempt {
            Ok(Ok(value)) => match parse_chrome_account_bridge_result(&value) {
                Ok(parsed) => {
                    emit_chrome_account_progress(
                        &app,
                        run_id,
                        "browser-background",
                        "success",
                        "后台 Chrome 已完成账号请求，临时标签已关闭",
                    );
                    resolved_account = Some(parsed);
                }
                Err(error) => emit_chrome_account_progress(
                    &app,
                    run_id,
                    "browser-background",
                    "success",
                    format!("后台请求仍需人工验证，将显示 Chrome：{error}"),
                ),
            },
            Ok(Err(error)) => {
                if chrome_session::is_blocking_chrome_automation_error(&error) {
                    return Err(error);
                }
                emit_chrome_account_progress(
                    &app,
                    run_id,
                    "browser-background",
                    "success",
                    format!("后台请求未完成，将显示 Chrome：{error}"),
                )
            }
            Err(error) => emit_chrome_account_progress(
                &app,
                run_id,
                "browser-background",
                "success",
                format!("后台 Chrome 任务失败，将显示浏览器：{error}"),
            ),
        }
    }

    let (account, result) = match resolved_account {
        Some(parsed) => parsed,
        None => {
            let marker = format!(
                "openhub-sync-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| "系统时间异常")?
                    .as_nanos()
            );
            let mut browser_url = if !checkin_url.trim().is_empty() {
                Url::parse(&checkin_url).unwrap_or_else(|_| base_url.clone())
            } else {
                base_url
                    .join("/console/personal")
                    .map_err(|_| "无法生成 Chrome 验证地址")?
            };
            if browser_url.origin() != base_url.origin() {
                browser_url = base_url
                    .join("/console/personal")
                    .map_err(|_| "无法生成 Chrome 验证地址")?;
            }
            browser_url.set_fragment(Some(&marker));
            let javascript = chrome_account_bridge_script(
                user_id.as_deref(),
                &current_month,
                &marker,
                has_refresh_cookie,
                supports_checkin,
                true,
            );
            emit_chrome_account_progress(
                &app,
                run_id,
                "chrome-request",
                "running",
                format!("{account_label} 静默请求未能完成，正在打开 Chrome；如出现验证，请在浏览器中完成"),
            );
            let bridge_result = tauri::async_runtime::spawn_blocking({
                let browser_url = browser_url.to_string();
                let profile_id = profile_id.clone();
                let marker = marker.clone();
                move || {
                    chrome_session::run_javascript_in_chrome_profile(
                        &browser_url,
                        &profile_id,
                        &marker,
                        &javascript,
                        visible_timeout,
                    )
                }
            })
            .await
            .map_err(|error| format!("Chrome 同步任务失败：{error}"))??;
            let parsed = parse_chrome_account_bridge_result(&bridge_result)?;
            emit_chrome_account_progress(
                &app,
                run_id,
                "chrome-request",
                "success",
                format!("{account_label} Chrome 已返回账号接口数据"),
            );
            parsed
        }
    };

    emit_chrome_account_progress(
        &app,
        run_id,
        "account-cache",
        "running",
        format!("正在更新 {account_label} 的 SQLite 账号缓存"),
    );

    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let changed = connection
        .execute(
            "UPDATE site_accounts
             SET username = ?1, remaining = ?2, used = ?3, total = ?4, unit = ?5,
                 is_valid = 1, sync_error = '', checkin_enabled = ?6,
                 checked_in_today = ?7, checkin_error = ?8,
                 checkin_date = date('now', 'localtime'),
                 newapi_token = ?9, newapi_user_id = ?10,
                 updated_at = CURRENT_TIMESTAMP
             WHERE site_id = ?11 AND profile_id = ?12",
            params![
                account.username,
                account.remaining,
                account.used,
                account.total,
                account.unit,
                result.checkin_enabled,
                result.checked_in_today,
                result.checkin_error,
                result.api_token,
                result.user_id,
                site_id,
                profile_id,
            ],
        )
        .map_err(|error| error.to_string())?;
    if changed == 0 {
        return Err(format!(
            "没有更新到 {site_name} · {account_label} 的账号缓存"
        ));
    }
    let session = read_cached_usage_sites(&connection)?
        .into_iter()
        .find(|site| site.site_id == site_id)
        .and_then(|site| {
            site.sessions
                .into_iter()
                .find(|session| session.profile_id == profile_id)
        })
        .ok_or_else(|| "读取 Chrome 同步后的账号缓存失败".to_string())?;
    emit_chrome_account_progress(
        &app,
        run_id,
        "account-cache",
        "success",
        format!("{account_label} 账号额度与签到状态已保存到 SQLite"),
    );
    Ok(session)
}
