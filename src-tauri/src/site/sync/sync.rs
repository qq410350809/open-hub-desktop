use crate::context::{spawn_blocking, AppContext, EventBus, Managed};
use crate::db::*;
use crate::models::*;
use crate::proxypool;
use crate::site::library::*;
use crate::site::library::{is_newapi, is_newapi_refresh, is_sub2api};
use crate::site::sync;
use rusqlite::{params, OptionalExtension};
use serde_json;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

/// 浏览器兜底（Chrome 桥接）失败后的冷却总时长：10 分钟起步，每多失败一次翻倍，
/// 上限 2 小时。指数退避避免自动同步在用户未完成 Cloudflare 验证时反复拉起
/// 后台标签页；手动点击“使用 Chrome 同步”不受冷却限制。
pub(crate) fn browser_fallback_total_cooldown_ms(fail_count: i64) -> i64 {
    if fail_count <= 0 {
        return 0;
    }
    const BASE_MS: i64 = 10 * 60 * 1000;
    const CAP_MS: i64 = 2 * 60 * 60 * 1000;
    let shift = (fail_count - 1).min(16) as u32;
    BASE_MS.saturating_mul(1i64 << shift).min(CAP_MS)
}

/// 由持久化的失败时间与连续失败次数算出剩余冷却毫秒（0 表示不在冷却）。
pub(crate) fn browser_fallback_cooldown_remaining_ms(failed_at_ms: i64, fail_count: i64) -> i64 {
    if failed_at_ms <= 0 || fail_count <= 0 {
        return 0;
    }
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0);
    (failed_at_ms + browser_fallback_total_cooldown_ms(fail_count) - now_ms).max(0)
}

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

pub(crate) fn parse_sub2api_usage(
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
        return Err(api_error_message(value, "Sub2API 返回的用量数据无效"));
    }
    let remaining = [
        "/data/remaining",
        "/data/quota/remaining",
        "/data/balance",
        "/remaining",
        "/balance",
    ]
    .iter()
    .find_map(|pointer| json_number(value, pointer));
    let remaining = remaining.ok_or_else(|| "Sub2API 用量响应缺少有效的余额字段".to_string())?;
    let used = ["/data/used", "/data/quota/used", "/used"]
        .iter()
        .find_map(|pointer| json_number(value, pointer));
    let total = ["/data/total", "/data/quota/total", "/total"]
        .iter()
        .find_map(|pointer| json_number(value, pointer))
        .or_else(|| used.map(|used| remaining + used));
    let unit = json_string(value, &["/data/unit", "/data/quota/unit", "/unit"]);
    Ok(SiteAccountSnapshot {
        username: json_string(value, &["/data/username", "/username"]),
        remaining: Some(remaining),
        used,
        total,
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

async fn fetch_sub2api_usage(
    client: &wreq::Client,
    base_url: &str,
    api_key: &str,
    user_agent: &str,
) -> Result<SiteAccountSnapshot, String> {
    let url = Url::parse(base_url)
        .map_err(|_| "站点 API 地址无效".to_string())?
        .join("/v1/usage")
        .map_err(|_| "无法生成 Sub2API 用量接口地址".to_string())?;
    let request =
        chrome_request_headers(client.get(url), base_url, user_agent).bearer_auth(api_key);
    request_json_with_hint(request, "Sub2API 用量接口", SUB2API_AUTH_FAILURE_HINT)
        .await
        .and_then(|value| parse_sub2api_usage(&value))
}

pub(crate) fn has_local_account_session(
    system_type: &str,
    values: &HashMap<String, String>,
) -> bool {
    let has_newapi = parse_newapi_local_account(values).is_ok();
    let has_sub2api = parse_sub2api_local_account(values).is_ok();
    if is_newapi(system_type) {
        has_newapi
    } else if is_sub2api(system_type) {
        has_sub2api
    } else {
        has_newapi || has_sub2api
    }
}

/// 宽松的浏览器会话判定：扫描阶段只回答「浏览器里该站点有没有登录痕迹」，
/// 不再要求痕迹能解析出完整账号结构。任意 Cookie（含站点自定义会话名、
/// cf_clearance 等）或 Local Storage 里任意已知键（哪怕只是 status/auth_token
/// 的残缺数据）都算有会话；真实性由后续账号接口 / Chrome 桥接验证。
/// 旧的强过滤（结构化账号或 new_api_refresh cookie）会把改了键名/Cookie 名
/// 的站点整站误判成“无会话”，导致同步弹窗老是提示未检测到账号。
pub(crate) fn has_browser_session_evidence(
    system_type: &str,
    values: Option<&HashMap<String, String>>,
    cookie_count: usize,
) -> bool {
    if values.is_some_and(|values| has_local_account_session(system_type, values)) {
        return true;
    }
    values.is_some_and(|values| !values.is_empty()) || cookie_count > 0
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

/// 按名判断 Cookie 头中是否含指定项（保留给测试与通用判定使用）
#[allow(dead_code)]
pub(crate) fn cookie_header_has_name(cookie_header: &str, expected_name: &str) -> bool {
    cookie_header.split(';').any(|pair| {
        pair.trim()
            .split_once('=')
            .is_some_and(|(name, _)| name.trim() == expected_name)
    })
}

pub(crate) fn apply_newapi_auth(
    request: wreq::RequestBuilder,
    auth: &NewApiAuth,
) -> wreq::RequestBuilder {
    match auth {
        NewApiAuth::Legacy {
            cookie_header,
            user_id,
        } => request
            .header(wreq::header::COOKIE, cookie_header)
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

/// 刷新令牌模式下尝试调用 /api/user/token 获取可持久化的访问令牌。
/// 传统 Cookie 模式没有访问令牌机制，调用方不得走到这里。
/// 成功返回 `Some(token_string)`，遇盾返回 `Err(shield_error)`，其他失败返回 `None`。
pub(crate) async fn try_acquire_newapi_token(
    client: &wreq::Client,
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

/// 刷新令牌模式下用不轮换会话的方式获取 NewAPI 访问令牌。刻意不在浏览器外
/// 调用 /api/user/auth/refresh——refresh 会轮换
/// HttpOnly 刷新令牌，浏览器里的旧令牌随即作废，用户会被登出；旧会话失效时
/// 返回 Ok(None)，由调用方转 Chrome 桥接在浏览器内完成刷新。
/// 返回 Some(NewApiAuth::Token) 表示拿到了可用的访问令牌；
/// Ok(None) 表示本地无可用令牌；Err 表示遇盾需要浏览器验证。
pub(crate) async fn acquire_newapi_session_token(
    client: &wreq::Client,
    base_url: &Url,
    legacy: &NewApiAuth,
    user_agent: &str,
) -> Result<Option<NewApiAuth>, String> {
    let user_id = match legacy {
        NewApiAuth::Legacy { user_id, .. } | NewApiAuth::Token { user_id, .. } => user_id.clone(),
    };
    match try_acquire_newapi_token(client, base_url, legacy, user_agent).await {
        Ok(Some(token)) => Ok(Some(NewApiAuth::Token {
            access_token: token,
            user_id,
        })),
        Ok(None) => Ok(None),
        Err(shield_error) => Err(shield_error),
    }
}

/// 识别签到"未启用"类提示。部分站点签到功能关闭时状态接口直接返回
/// success:false + 提示语（如"签到功能未启用"），这并非数据异常，应视为
/// 未启用状态，由调用方查当天签到日志兜底确认实际签到情况。
pub(crate) fn is_checkin_disabled_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("未启用")
        || lower.contains("未开启")
        || lower.contains("没有启用")
        || lower.contains("not enabled")
        || lower.contains("not_enabled")
        || lower.contains("checkin disabled")
        || lower.contains("checkin_disabled")
}

#[allow(dead_code)]
pub(crate) fn is_turnstile_checkin_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("turnstile")
}

pub(crate) fn parse_newapi_checkin_status(
    value: &serde_json::Value,
) -> Result<(bool, bool), String> {
    if value.get("success").and_then(serde_json::Value::as_bool) != Some(true) {
        let error = api_error_message(value, "签到状态数据无效");
        if is_checkin_disabled_message(&error) {
            return Ok((false, false));
        }
        return Err(error);
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

/// 当前本地时区相对 UTC 的偏移秒数（如东八区为 28800）。取不到时退化为 0。
pub(crate) fn local_utc_offset_secs() -> i64 {
    unsafe {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_secs() as libc::time_t)
            .unwrap_or(0);
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return 0;
        }
        tm.tm_gmtoff
    }
}

/// 当天（本地时区）的 Unix 秒范围：[00:00:00, 23:59:59]，用于查询签到日志。
pub(crate) fn local_day_unix_range() -> (i64, i64) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0);
    let offset = local_utc_offset_secs();
    let local_now = now + offset;
    let local_start_of_day = local_now - local_now.rem_euclid(86_400);
    let start = local_start_of_day - offset;
    (start, start + 86_399)
}

/// 解析 NewAPI /api/log/self 响应：当天存在签到记录（items 非空）
/// 返回 Ok(true)，无记录返回 Ok(false)。响应异常视为无法确认。
pub(crate) fn parse_newapi_checkin_logs(value: &serde_json::Value) -> Result<bool, String> {
    if value.get("success").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(api_error_message(value, "签到日志数据无效"));
    }
    let items = [
        "/data/items",
        "/data/list",
        "/data/records",
        "/data/data",
        "/items",
        "/data",
    ]
    .iter()
    .find_map(|pointer| value.pointer(pointer).and_then(serde_json::Value::as_array))
    .ok_or_else(|| "签到日志缺少记录列表".to_string())?;
    Ok(!items.is_empty())
}

/// 签到状态接口报 enabled=false 时的日志兜底：查当天（本地时区）签到日志确认
/// 实际签到情况。部分站点状态接口的 enabled 字段不可靠，用户当天可能已在网页
/// 手动签到过。分别查询 type=4（签到日志）和 type=1（充值/系统日志），任一存在
/// 记录即视为今日已签到；返回 Ok(true) 表示今日已签到，Ok(false) 表示无签到记录。
pub(crate) async fn query_newapi_checkin_log(
    client: &wreq::Client,
    base_url: &str,
    auth: &NewApiAuth,
    user_agent: &str,
) -> Result<bool, String> {
    let base_url = Url::parse(base_url).map_err(|_| "站点 API 地址无效".to_string())?;
    let (start_timestamp, end_timestamp) = local_day_unix_range();
    let endpoint = base_url
        .join("/api/log/self")
        .map_err(|_| "无法生成签到日志接口地址".to_string())?;
    let start_str = start_timestamp.to_string();
    let end_str = end_timestamp.to_string();
    // 依次查询 type=4（签到日志）和 type=1（充值/系统日志），任一有记录即视为已签到。
    for log_type in ["4", "1"] {
        let mut query_url = endpoint.clone();
        query_url
            .query_pairs_mut()
            .append_pair("p", "1")
            .append_pair("page_size", "20")
            .append_pair("type", log_type)
            .append_pair("start_timestamp", &start_str)
            .append_pair("end_timestamp", &end_str);
        match request_json(
            apply_newapi_auth(
                chrome_request_headers(client.get(query_url), base_url.as_str(), user_agent),
                auth,
            ),
            "签到日志接口",
        )
        .await
        {
            Ok(value) => {
                if parse_newapi_checkin_logs(&value).unwrap_or(false) {
                    return Ok(true);
                }
            }
            Err(_) => {}
        }
    }
    Ok(false)
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

/// Sub2API 没有 NewAPI 的“访问令牌”概念，登录凭据是 Local Storage 里的
/// `auth_token`（可直接当模型 Key 用）。401 失效提示必须说清楚是登录令牌，
/// 不能套用 NewAPI 的“账号令牌/访问令牌”说法。
pub(crate) const SUB2API_AUTH_FAILURE_HINT: &str =
    "（Sub2API 登录令牌（auth_token）已失效或过期，请重新登录后同步账号）";

/// NewAPI 刷新令牌移交提示：本地会话已失效、浏览器存在 new_api_refresh 时，
/// 必须由 Chrome 同源请求刷新（轮换的 HttpOnly Cookie 只有浏览器内请求能写回）。
/// 这是“移交 Chrome 处理”的中间状态，不是失败；界面层据此以信息提示展示。
pub(crate) const NEWAPI_REFRESH_HANDOFF_MESSAGE: &str =
    "检测到 NewAPI refresh cookie 且本地会话已失效，将通过 Chrome 同源请求刷新并写回浏览器";

pub(crate) async fn request_json(
    request: wreq::RequestBuilder,
    label: &str,
) -> Result<serde_json::Value, String> {
    request_json_with_hint(
        request,
        label,
        "（账号令牌已失效或过期，请重新登录后同步账号）",
    )
    .await
}

pub(crate) async fn request_json_with_hint(
    request: wreq::RequestBuilder,
    label: &str,
    auth_failure_hint: &str,
) -> Result<serde_json::Value, String> {
    let response = request
        .send()
        .await
        .map_err(|error| format!("{label}请求失败：{error:#}"))?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(wreq::header::CONTENT_TYPE)
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
                let reason = if status == wreq::StatusCode::FORBIDDEN {
                    "Cloudflare 安全验证拦截了直接请求，请先用对应 Chrome 账号打开站点并通过验证"
                } else {
                    "站点返回了网页而不是 API 数据"
                };
                return Err(format!(
                    "{label} HTTP {} 返回 HTML：{reason}",
                    status.as_u16()
                ));
            }
            return Err(format!(
                "{label}返回了无法解析的数据：{}",
                friendly_json_parse_error(&error, body)
            ));
        }
    };
    if !status.is_success() {
        let mut message = format!(
            "{label} HTTP {}：{}",
            status.as_u16(),
            api_error_message(&value, "请求失败")
        );
        if status == wreq::StatusCode::UNAUTHORIZED {
            message.push_str(auth_failure_hint);
        }
        return Err(message);
    }
    Ok(value)
}

/// 把 serde_json 的语法报错翻译成用户能看懂的提示，并附一小段原文预览，
/// 避免直接暴露 “trailing characters at line 1 column 5” 这类底层错误。
pub(crate) fn friendly_json_parse_error(error: &serde_json::Error, body: &[u8]) -> String {
    let raw = error.to_string();
    let hint = if raw.contains("trailing characters") {
        "接口在合法 JSON 之后还带了多余内容（可能是页面残留、JSONP 包装或接口格式差异）"
    } else if raw.contains("expected value") {
        "接口没有返回 JSON 数据（可能是空响应或纯文本）"
    } else if raw.contains("EOF while parsing") || raw.contains("unexpected end") {
        "接口返回的 JSON 不完整（可能被截断）"
    } else if raw.contains("expected ident") || raw.contains("key must be a string") {
        "接口返回的 JSON 键值格式不符合预期"
    } else {
        "JSON 格式不符合预期"
    };
    let preview = String::from_utf8_lossy(body)
        .chars()
        .take(40)
        .collect::<String>()
        .trim()
        .to_string();
    let mut message = hint.to_string();
    if !preview.is_empty() {
        message.push_str(&format!("（原文：{preview}）"));
    }
    message
}

pub(crate) fn access_token_was_rejected(error: &str) -> bool {
    // 401 一定是令牌失效；部分 NewAPI 站点对过期令牌返回 403 并带“无效令牌”提示，
    // 不能与 Cloudflare 403（HTML/盾）混淆，因此只认带有明确失效语义的文本。
    if error.contains(" HTTP 401") {
        return true;
    }
    if !error.contains(" HTTP 403") {
        return false;
    }
    [
        "无效的令牌",
        "invalid token",
        "token expired",
        "令牌已过期",
        "unauthorized",
        "token is invalid",
    ]
    .iter()
    .any(|marker| error.to_ascii_lowercase().contains(marker))
}

/// 账号接口失败后是否应移交 Chrome 兜底：令牌被服务端拒绝，或直连被安全盾拦截。
/// 两者直接通道都已不可用，只有浏览器同源请求（可过 Cloudflare 验证）能恢复；
/// 网络抖动、解析失败等其他错误不属于此类，保留本地缓存展示即可。
pub(crate) fn requires_chrome_fallback(error: &str) -> bool {
    access_token_was_rejected(error) || is_cloudflare_shield_error(error)
}

/// 判断错误是否属于站点安全盾/网页拦截（Cloudflare 或接口返回 HTML 页面）。
/// 这类失败意味着直接 HTTP 通道被拦截，但 Chrome 同源请求（在浏览器内执行）
/// 仍可能正常通过，因此不应把账号/模型同步判定为“无法补救”而排除 Chrome 兜底。
pub(crate) fn is_cloudflare_shield_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("cloudflare")
        || lower.contains("安全验证")
        || lower.contains("cf-chl")
        || lower.contains("challenge-platform")
        || lower.contains("just a moment")
        || lower.contains("attention required")
        || lower.contains("cf_clearance")
        || lower.contains("返回 html")
        || lower.contains("返回了网页")
}

pub(crate) fn chrome_request_headers(
    request: wreq::RequestBuilder,
    base_url: &str,
    user_agent: &str,
) -> wreq::RequestBuilder {
    let major = user_agent
        .split("Chrome/")
        .nth(1)
        .and_then(|value| value.split('.').next())
        .unwrap_or("120");
    request
        .header(wreq::header::ACCEPT, "application/json, text/plain, */*")
        .header(wreq::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
        .header(wreq::header::REFERER, base_url)
        .header(wreq::header::USER_AGENT, user_agent)
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
    client: &wreq::Client,
    base_url: &str,
    auth: &NewApiAuth,
    user_agent: &str,
    current_month: &str,
    _previous: CheckinSnapshot,
) -> CheckinSnapshot {
    let endpoint = match Url::parse(base_url).and_then(|url| url.join("/api/user/checkin")) {
        Ok(url) => url,
        Err(_) => {
            return CheckinSnapshot {
                enabled: false,
                checked_in_today: false,
                error: String::new(),
            };
        }
    };
    let mut query_url = endpoint.clone();
    query_url
        .query_pairs_mut()
        .append_pair("month", current_month);
    let headers = |request: wreq::RequestBuilder| {
        apply_newapi_auth(chrome_request_headers(request, base_url, user_agent), auth)
    };
    let value = match request_json(headers(client.get(query_url.clone())), "签到状态接口").await
    {
        Ok(value) => value,
        Err(_) => match request_json(headers(client.get(query_url)), "签到状态接口").await {
            Ok(value) => value,
            Err(_) => {
                return CheckinSnapshot {
                    enabled: false,
                    checked_in_today: false,
                    error: String::new(),
                };
            }
        },
    };
    let (enabled, checked_in_today) = match parse_newapi_checkin_status(&value) {
        Ok(status) => status,
        Err(_) => {
            return CheckinSnapshot {
                enabled: false,
                checked_in_today: false,
                error: String::new(),
            };
        }
    };
    if !enabled {
        return match query_newapi_checkin_log(client, base_url, auth, user_agent).await {
            Ok(true) => CheckinSnapshot {
                enabled: true,
                checked_in_today: true,
                error: String::new(),
            },
            _ => CheckinSnapshot {
                enabled: false,
                checked_in_today: false,
                error: String::new(),
            },
        };
    }
    if checked_in_today {
        return CheckinSnapshot {
            enabled: true,
            checked_in_today: true,
            error: String::new(),
        };
    }
    let value = match request_json(headers(client.post(endpoint)), "签到接口").await {
        Ok(value) => value,
        Err(_) => {
            return CheckinSnapshot {
                enabled: false,
                checked_in_today: false,
                error: String::new(),
            };
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
            enabled: false,
            checked_in_today: false,
            error: String::new(),
        }
    }
}

pub(crate) async fn refresh_sub2api_checkin(
    client: &wreq::Client,
    base_url: &str,
    auth_token: &str,
    user_agent: &str,
    _previous: CheckinSnapshot,
) -> CheckinSnapshot {
    let base_url = match Url::parse(base_url) {
        Ok(url) => url,
        Err(_) => {
            return CheckinSnapshot {
                enabled: false,
                checked_in_today: false,
                error: String::new(),
            };
        }
    };
    let status_url = match base_url.join("/api/v1/redeem/checkin/status") {
        Ok(url) => url,
        Err(_) => {
            return CheckinSnapshot {
                enabled: false,
                checked_in_today: false,
                error: String::new(),
            };
        }
    };
    let checkin_url = match base_url.join("/api/v1/redeem/checkin") {
        Ok(url) => url,
        Err(_) => {
            return CheckinSnapshot {
                enabled: false,
                checked_in_today: false,
                error: String::new(),
            };
        }
    };
    let headers = |request: wreq::RequestBuilder| {
        chrome_request_headers(request, base_url.as_str(), user_agent).bearer_auth(auth_token)
    };
    let value = match request_json_with_hint(
        headers(client.get(status_url)),
        "Sub2API 签到状态接口",
        SUB2API_AUTH_FAILURE_HINT,
    )
    .await
    {
        Ok(value) => value,
        Err(_) => {
            return CheckinSnapshot {
                enabled: false,
                checked_in_today: false,
                error: String::new(),
            };
        }
    };
    let checked_in_today = match parse_sub2api_checkin_status(&value) {
        Ok(value) => value,
        Err(_) => {
            return CheckinSnapshot {
                enabled: false,
                checked_in_today: false,
                error: String::new(),
            };
        }
    };
    if checked_in_today {
        return CheckinSnapshot {
            enabled: true,
            checked_in_today: true,
            error: String::new(),
        };
    }
    let value = match request_json_with_hint(
        headers(client.post(checkin_url)),
        "Sub2API 签到接口",
        SUB2API_AUTH_FAILURE_HINT,
    )
    .await
    {
        Ok(value) => value,
        Err(_) => {
            return CheckinSnapshot {
                enabled: false,
                checked_in_today: false,
                error: String::new(),
            };
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
            enabled: false,
            checked_in_today: false,
            error: String::new(),
        }
    }
}

pub(crate) async fn fetch_site_account(
    client: &wreq::Client,
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
    cached_sub2api_keys: &[String],
) -> Result<SiteAccountRefresh, String> {
    let previous_checkin = if should_checkin {
        previous_checkin
    } else {
        CheckinSnapshot::default()
    };
    let inferred_type;
    let system_type = if is_newapi(system_type) || is_sub2api(system_type) {
        system_type
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
    if is_newapi(system_type) {
        let local_account = parse_newapi_local_account(local_values).ok();
        let base_url_parsed = Url::parse(base_url).map_err(|_| "站点 API 地址无效".to_string())?;
        let uses_refresh_auth = is_newapi_refresh(system_type);

        // 读取当前模式所需的浏览器 Cookie。
        let cookie_header = match cookie_header {
            Ok(value) => value,
            Err(error) => {
                return match local_account {
                    Some(account) => Ok(SiteAccountRefresh {
                        account,
                        is_valid: true,
                        sync_error: error,
                        checkin: previous_checkin,
                        newapi_token: cached_newapi_token.unwrap_or_default(),
                        newapi_user_id: cached_newapi_user_id.unwrap_or_default(),
                    }),
                    None => Err(error),
                }
            }
        };
        // user id 优先取 Local Storage 实时数据，取不到时回退数据库缓存
        let user_id = newapi_user_id(local_values).or_else(|| {
            cached_newapi_user_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        });
        if !uses_refresh_auth && user_id.is_none() {
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

        let temp_auth = NewApiAuth::Legacy {
            cookie_header: cookie_header.clone(),
            user_id: user_id.clone(),
        };

        // 1. 优先尝试已缓存的访问令牌（长效凭据）
        let mut api_token = cached_newapi_token
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("")
            .to_string();

        let endpoint = base_url_parsed
            .join("/api/user/self")
            .map_err(|_| "无法生成账号接口地址".to_string())?;

        let mut auth = if !api_token.is_empty() {
            NewApiAuth::Token {
                access_token: api_token.clone(),
                user_id: user_id.clone(),
            }
        } else {
            temp_auth.clone()
        };

        let mut self_response = request_json(
            apply_newapi_auth(
                chrome_request_headers(client.get(endpoint.clone()), base_url, user_agent),
                &auth,
            ),
            "账号接口",
        )
        .await;

        // 2. 如果请求失败（令牌过期、401、权限不足等），且不是盾拦截，进行自愈刷新
        let mut self_heal_failed = false;
        if self_response.is_err()
            && !self_response
                .as_ref()
                .err()
                .is_some_and(|e| is_cloudflare_shield_error(e))
        {
            // 自愈只走不轮换路径：缓存访问令牌 / 现有 Cookie 换取访问令牌。
            // 刻意不在浏览器外调用 /api/user/auth/refresh —— refresh 会轮换
            // HttpOnly new_api_refresh，而本应用没有把新 Cookie 写回浏览器的
            // 通道，轮换后浏览器里的旧令牌随即作废、用户被登出。会话彻底失效
            // 时交由错误提示引导用户在浏览器打开一次站点完成自动续期。
            if uses_refresh_auth {
                if let Ok(Some(new_token)) =
                    try_acquire_newapi_token(client, &base_url_parsed, &temp_auth, user_agent).await
                {
                    api_token = new_token;
                    auth = NewApiAuth::Token {
                        access_token: api_token.clone(),
                        user_id: user_id.clone(),
                    };
                } else {
                    self_heal_failed = true;
                }
            }
            // 使用现有凭证重试 /api/user/self
            self_response = request_json(
                apply_newapi_auth(
                    chrome_request_headers(client.get(endpoint), base_url, user_agent),
                    &auth,
                ),
                "账号接口",
            )
            .await;
        }

        let (newapi_token, newapi_user_id) = match &auth {
            NewApiAuth::Token {
                access_token,
                user_id,
            } => (access_token.clone(), user_id.clone()),
            _ => (String::new(), user_id.clone()),
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

        let (remote, response_user_id) = match self_response.and_then(|value| {
            let account = parse_newapi_account(&value)?;
            let response_user_id =
                json_string(&value, &["/data/id", "/data/userId", "/id", "/userId"]);
            Ok((account, response_user_id))
        }) {
            Ok(result) => result,
            Err(mut error) => {
                // 非盾失败且自愈未取得新令牌：本地会话与访问令牌均已失效。
                // 引导浏览器内自动续期，绝不代调轮换接口（会导致浏览器登出）。
                if self_heal_failed && !requires_chrome_fallback(&error) {
                    error = format!(
                        "{error}；本地会话与访问令牌均已失效。请在浏览器中打开一次该站点（会自动静默续期、不会登出），完成后重新同步"
                    );
                }
                return match local_account {
                    Some(account) => Ok(SiteAccountRefresh {
                        account,
                        // 遇盾时直接通道已不可用，只有 Chrome 同源请求能恢复，
                        // 置 is_valid=false 让前端进入浏览器兜底；新取得的令牌已在
                        // 下方字段返回保留，过盾后可直接复用。
                        is_valid: !requires_chrome_fallback(&error),
                        sync_error: error,
                        checkin,
                        newapi_token: newapi_token.clone(),
                        newapi_user_id: newapi_user_id.clone(),
                    }),
                    None => Err(format!("账号接口失败：{error}")),
                };
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
    // —— Sub2API ——
    // 优先用已有 apiKey 走 /v1/usage 获取余额，不再强依赖 Chrome 会话（auth_user/auth_token）。
    let local_account = parse_sub2api_local_account(local_values).ok();
    let auth_token = local_values
        .get("auth_token")
        .map(|value| local_scalar(value))
        .filter(|value| !value.is_empty());

    if !is_sub2api(system_type) {
        return Ok(SiteAccountRefresh {
            account: local_account.clone().unwrap_or_default(),
            is_valid: local_account.is_some(),
            sync_error: if local_account.is_some() {
                "站点类型未识别，未请求账号接口".into()
            } else if !local_error.is_empty() {
                local_error.to_string()
            } else {
                "站点类型未识别且没有本地账号数据".into()
            },
            checkin: previous_checkin,
            newapi_token: String::new(),
            newapi_user_id: String::new(),
        });
    }

    let checkin = if should_checkin {
        match &auth_token {
            Some(token) => {
                refresh_sub2api_checkin(client, base_url, token, user_agent, previous_checkin).await
            }
            None => CheckinSnapshot::default(),
        }
    } else {
        CheckinSnapshot::default()
    };

    // 候选 apiKey：仅使用已缓存的 Sub2API API Key。/v1/usage 只认 sk-... 密钥，
    // 不接受会话 auth_token，因此不能把 auth_token 混入候选。
    let candidate_keys: Vec<String> = cached_sub2api_keys
        .iter()
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
        .collect();

    // 1) /v1/usage：Bearer apiKey 直接取余额。
    for key in &candidate_keys {
        match fetch_sub2api_usage(client, base_url, key, user_agent).await {
            Ok(account) => {
                return Ok(SiteAccountRefresh {
                    account,
                    is_valid: true,
                    sync_error: String::new(),
                    checkin,
                    newapi_token: String::new(),
                    newapi_user_id: String::new(),
                });
            }
            Err(_) => continue,
        }
    }

    // 2) 回退：/api/v1/auth/me（会话端点）。
    if let Some(token) = &auth_token {
        let url = Url::parse(base_url)
            .map_err(|_| "站点 API 地址无效".to_string())?
            .join("/api/v1/auth/me")
            .map_err(|_| "无法生成账号接口地址".to_string())?;
        let request =
            chrome_request_headers(client.get(url), base_url, user_agent).bearer_auth(token);
        let account = request_json(request, "账号接口")
            .await
            .and_then(|value| parse_sub2api_account(&value));
        return match account {
            Ok(account) => Ok(SiteAccountRefresh {
                account,
                is_valid: true,
                sync_error: String::new(),
                checkin,
                newapi_token: String::new(),
                newapi_user_id: String::new(),
            }),
            Err(error) => Ok(SiteAccountRefresh {
                account: local_account.clone().unwrap_or_default(),
                is_valid: local_account.is_some(),
                sync_error: error,
                checkin,
                newapi_token: String::new(),
                newapi_user_id: String::new(),
            }),
        };
    }

    // 3) 无可用 apiKey/auth_token：回退本地缓存或报错。
    let is_valid = local_account.is_some();
    let sync_error = if is_valid {
        if !local_error.is_empty() {
            local_error.to_string()
        } else {
            "Sub2API 没有可用的 API Key".to_string()
        }
    } else if !local_error.is_empty() {
        local_error.to_string()
    } else {
        "Sub2API 没有可用的 API Key 与会话数据".to_string()
    };
    Ok(SiteAccountRefresh {
        account: local_account.unwrap_or_default(),
        is_valid,
        sync_error,
        checkin,
        newapi_token: String::new(),
        newapi_user_id: String::new(),
    })
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
    if (useRefreshAuth) {
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
    }
    // 传统 Cookie 模式不获取访问令牌，直接带 New-Api-User 与 session Cookie 请求。
    const useSessionCookies = !apiToken;
    if (useSessionCookies) {
      if (!legacyUserId) {
        bridge.result = { ok: false, error: "未取得 NewAPI 访问令牌，停止账号接口同步" };
        return;
      }
    } else {
      headers.Authorization = `Bearer ${apiToken}`;
    }
    let checkinEnabled = false;
    let checkedInToday = false;
    let checkinError = "";
    let checkinResolvedViaLog = false;
    if (shouldCheckin) {
      try {
        const checkinUrl = `/api/user/checkin?month=${encodeURIComponent(__OPENHUB_MONTH__)}`;
        const checkinResponse = await readResponse(await fetch(checkinUrl, {
          method: "GET", credentials: "include", cache: "no-store", headers,
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
        } else {
          // 部分站点签到未启用时状态接口直接返回 success:false + 提示语（如
          // "签到功能未启用"），并非数据异常；识别后交给下方日志兜底确认。
          const statusMessage = messageOf(checkinResponse.data, "");
          if (!/未启用|未开启|没有启用|not enabled|not_enabled|disabled/i.test(statusMessage)) {
            checkinError = messageOf(checkinResponse.data, "签到状态数据无效");
          }
        }
        if (!checkinEnabled && !checkinError) {
          // 状态接口报"功能未启用"或缺少启用状态：查当天签到日志兜底，
          // 依次查 type=4（签到日志）和 type=1（充值/系统日志），任一有记录即已签到；
          // 日志兜底结果绝不触发自动代签。
          const dayStart = new Date();
          dayStart.setHours(0, 0, 0, 0);
          const dayEnd = new Date();
          dayEnd.setHours(23, 59, 59, 999);
          const startTs = Math.floor(dayStart.getTime() / 1000);
          const endTs = Math.floor(dayEnd.getTime() / 1000);
          let logResolved = false;
          for (const logType of [4, 1]) {
            const logResponse = await readResponse(await fetch(
              `/api/log/self?p=1&page_size=20&type=${logType}&start_timestamp=${startTs}&end_timestamp=${endTs}`,
              { method: "GET", credentials: "include", cache: "no-store", headers,
                signal: AbortSignal.timeout(requestTimeout) }
            ));
            if (!logResponse.challenge && !logResponse.error &&
                logResponse.status >= 200 && logResponse.status < 300 &&
                logResponse.data && logResponse.data.success === true) {
              const items = logResponse.data.data?.items || logResponse.data.data?.list ||
                logResponse.data.data?.data || logResponse.data.data?.records;
              if (Array.isArray(items) && items.length > 0) {
                checkinResolvedViaLog = true;
                checkinEnabled = true;
                checkedInToday = true;
                checkinError = "";
                logResolved = true;
                break;
              }
            }
          }
          if (!logResolved && !checkinResolvedViaLog) {
            // 两种日志类型均无记录或请求失败时：如果至少有一种日志接口
            // 成功返回了空列表，仍标记为"功能已启用但未签到"。
            checkinError = "签到功能未启用";
          }
        }
        // 仅当状态接口确认 enabled=true 且未签到、且未经过日志兜底时自动代签。
        if (checkinEnabled && !checkedInToday && !checkinResolvedViaLog) {
            const postResponse = await readResponse(await fetch("/api/user/checkin", {
              method: "POST", credentials: "include", cache: "no-store", headers,
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
        } catch (error) {
        checkinError = String(error && error.message || error);
      }
    }
    const selfResponse = await readResponse(await fetch("/api/user/self", {
      method: "GET", credentials: "include", cache: "no-store", headers,
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

/// 单个站点 / 账号同步的硬性总超时：全过程（可达性探测 + 直连 + 静默/后台/
/// 可见三层兜底）合计超过 60 秒即强制失败并释放界面，避免整个弹窗卡住
/// 什么都干不了。下方三个阶段预算已压缩到该上限之内，宁可早失败
/// （失败会计入浏览器兜底冷却），也不长时间占住同步流程。
const SITE_SYNC_TIMEOUT: Duration = Duration::from_secs(60);

/// 被手动强制停止的账号同步 run_id 集合。取消后在下一个阶段边界
/// （静默/后台/可见）立即失败返回，不再打开新的 Chrome 标签页；
/// 已打开的桥接标签由前端调用 close_chrome_sync_tabs 清理。
fn cancelled_sync_runs() -> &'static Mutex<HashSet<u64>> {
    static CANCELLED: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
    CANCELLED.get_or_init(|| Mutex::new(HashSet::new()))
}

fn is_site_account_sync_cancelled(run_id: u64) -> bool {
    cancelled_sync_runs()
        .lock()
        .map(|runs| runs.contains(&run_id))
        .unwrap_or(false)
}

fn clear_site_account_sync_cancelled(run_id: u64) {
    if let Ok(mut runs) = cancelled_sync_runs().lock() {
        runs.remove(&run_id);
    }
}

/// 强制停止指定 run_id 的账号同步。同步结束时（成功/失败/超时）登记自动清除。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn cancel_site_account_sync(run_id: u64) -> bool {
    cancelled_sync_runs()
        .lock()
        .map(|mut runs| runs.insert(run_id))
        .unwrap_or(false)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn sync_site_account_via_chrome(
    ctx: Managed<'_, Arc<AppContext>>,
    site_id: String,
    profile_id: String,
    run_id: u64,
) -> Result<sync::ChromeSessionInfo, String> {
    sync_site_account_via_chrome_command(&ctx, site_id, profile_id, run_id).await
}

/// 手动 Chrome 账号同步入口：统一 60 秒总超时、
/// 失败原因与浏览器兜底冷却计数落库。
pub(crate) async fn sync_site_account_via_chrome_command(
    ctx: &Arc<AppContext>,
    site_id: String,
    profile_id: String,
    run_id: u64,
) -> Result<sync::ChromeSessionInfo, String> {
    let database = &*ctx.database;
    let outcome = match tokio::time::timeout(
        SITE_SYNC_TIMEOUT,
        sync_site_account_via_chrome_inner(ctx, site_id.clone(), profile_id.clone(), run_id),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err("账号同步超过 60 秒，已强制终止".to_string()),
    };
    if let Err(error) = &outcome {
        // 浏览器兜底的失败原因落库：失败详情原本只出现在当次弹窗日志里，过后无从追溯；
        // 写入 sync_error 后界面和后续诊断都能看到最后一次尝试究竟错在哪。
        // 同时推进持久化冷却计数，避免短时间内反复拉起浏览器。
        if let Ok(connection) = database.0.lock() {
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|value| value.as_millis() as i64)
                .unwrap_or(0);
            let _ = connection.execute(
                "UPDATE site_accounts
                 SET sync_error = ?3,
                     browser_fallback_failed_at = ?4,
                     browser_fallback_fail_count = browser_fallback_fail_count + 1,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE site_id = ?1 AND profile_id = ?2",
                params![site_id, profile_id, error, now_ms],
            );
        }
    }
    clear_site_account_sync_cancelled(run_id);
    outcome
}

async fn sync_site_account_via_chrome_inner(
    ctx: &Arc<AppContext>,
    site_id: String,
    profile_id: String,
    run_id: u64,
) -> Result<sync::ChromeSessionInfo, String> {
    let database = &*ctx.database;
    let runtime = &*ctx.proxy_runtime;
    let bus: EventBus = ctx.event_bus.clone();
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
        let connection = database.lock_conn()?;
        let site = connection
            .query_row(
                // 待定（is_pending）站点同样允许 Chrome 账号同步：
                // 会话弹窗和扫描流程都支持待定站点，这里按 id 精确匹配即可。
                "SELECT name, api_base_url, system_type, checkin_url, supports_checkin
                 FROM directory_sites WHERE id = ?1",
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
            .ok_or_else(|| "找不到对应的站点记录".to_string())?;
        #[derive(Debug)]
        struct AccountRow {
            profile_name: String,
            account_name: String,
            cookie_names: Vec<String>,
            cached_token: Option<String>,
            cached_uid: Option<String>,
        }
        let account_row = connection
            .query_row(
                "SELECT profile_name, account_name, cookie_names, newapi_token, newapi_user_id
                 FROM site_accounts WHERE site_id = ?1 AND profile_id = ?2",
                params![site_id, profile_id],
                |row| {
                    let cookie_names_json: String = row.get(2)?;
                    Ok(AccountRow {
                        profile_name: row.get(0)?,
                        account_name: row.get(1)?,
                        cookie_names: serde_json::from_str::<Vec<String>>(&cookie_names_json)
                            .unwrap_or_default(),
                        cached_token: row.get(3)?,
                        cached_uid: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(|error| error.to_string())?;
        let Some(account) = account_row else {
            return Err("该 Chrome Profile 尚未建立本地账号缓存，请先同步会话".into());
        };
        let AccountRow {
            profile_name,
            account_name,
            cookie_names,
            cached_token,
            cached_uid,
        } = account;
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
        &bus,
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

    // 轻量级可达性检测：用短超时 HEAD 请求探测站点是否在线，
    // 如果 DNS 解析失败、连接被拒绝或超时则直接失败，避免拉起 Chrome。
    // 只拦截网络层彻底不可达的情况（connect error / timeout）；
    // HTTP 4xx/5xx（含 Cloudflare 403）视为"站点在线但需要认证"，放行。
    {
        emit_chrome_account_progress(
            &bus,
            run_id,
            "reachability",
            "running",
            format!("正在检测 {account_label} 站点可达性"),
        );
        let uses_proxy = proxypool::read_site_uses_proxy_pool(database, &site_id).unwrap_or(false);
        let probe_url = base_url.to_string();
        let probe_result: Result<(), String> = if uses_proxy {
            let site_id_for_probe = site_id.clone();
            let profile_id_for_probe = profile_id.clone();
            let probe_url_clone = probe_url.clone();
            proxypool::with_account_proxy(
                database,
                runtime,
                &site_id_for_probe,
                &profile_id_for_probe,
                Duration::from_secs(8),
                3,
                "站点可达性探测",
                move |probe_client| {
                    let probe_url = probe_url_clone.clone();
                    async move {
                        let _ = probe_client
                            .head(&probe_url)
                            .send()
                            .await
                            .map_err(|e| e.to_string())?;
                        Ok(())
                    }
                },
            )
            .await
        } else {
            let probe_client =
                build_site_http_client(database, Duration::from_secs(6), 3, "站点可达性探测")
                    .unwrap_or_else(|_| {
                        wreq::Client::builder()
                            .timeout(Duration::from_secs(6))
                            .no_proxy()
                            .build()
                            .expect("fallback client")
                    });
            probe_client
                .head(&probe_url)
                .send()
                .await
                .map(|_| ())
                .map_err(|e| format!("{e:#}"))
        };

        match probe_result {
            Ok(()) => {
                emit_chrome_account_progress(
                    &bus,
                    run_id,
                    "reachability",
                    "success",
                    format!("{account_label} 站点网络可达"),
                );
            }
            Err(error) => {
                let is_unreachable = error.contains("connect error")
                    || error.contains("timed out")
                    || error.contains("dns error")
                    || error.contains("Name or service not known")
                    || error.contains("No address associated")
                    || error.contains("resolve")
                    || error.contains("connection refused");
                if is_unreachable {
                    let reason = format!("站点不可达：{error}");
                    emit_chrome_account_progress(
                        &bus,
                        run_id,
                        "reachability",
                        "error",
                        format!("{account_label} {reason}"),
                    );
                    return Err(reason);
                }
                emit_chrome_account_progress(
                    &bus,
                    run_id,
                    "reachability",
                    "success",
                    format!("{account_label} 可达性检测遇到非致命错误，继续：{error}"),
                );
            }
        }
    }

    let home_dir = crate::context::home_dir().ok_or("无法定位用户目录")?;
    let local_target = sync::LocalStorageTarget {
        site_id: site_id.clone(),
        profile_id: profile_id.clone(),
        origin,
    };
    let local_match = spawn_blocking({
        let home_dir = home_dir.clone();
        move || sync::read_local_storage_from_home(&home_dir, &[local_target])
    })
    .await
    .map_err(|error| format!("读取 Chrome Local Storage 任务失败：{error}"))?
    .into_iter()
    .next();
    let has_refresh_cookie =
        has_newapi_refresh_cookie_name(cookie_names.iter().map(String::as_str));
    let use_refresh_auth = is_newapi_refresh(&system_type);
    let local_values = local_match
        .as_ref()
        .filter(|item| item.error.is_empty())
        .map(|item| &item.values);
    let local_account_valid =
        local_values.is_some_and(|values| parse_newapi_local_account(values).is_ok());
    // 宽松放行门槛（与扫描的 has_browser_session_evidence 对齐）：refresh cookie、
    // 可解析本地账号、数据库缓存的 user id、任意已知 Local Storage 键、任意 Cookie，
    // 五者有其一就走桥接——真实性由页面上下文里的接口响应裁决，而不是在这里预判。
    // 旧门槛会把自定义会话 Cookie 名/非标准 user 结构的站点直接挡在门外，
    // 明明浏览器里登着号却总是提示“没有找到可用的本地账号或刷新会话”。
    let has_cached_user = cached_uid
        .as_deref()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let user_id = local_values
        .and_then(newapi_user_id)
        .or_else(|| has_cached_user.then(|| cached_uid.clone().unwrap_or_default()));
    let has_any_cookie = !cookie_names.is_empty();
    let has_any_local_keys = local_values.is_some_and(|values| !values.is_empty());
    if !has_refresh_cookie
        && !local_account_valid
        && !has_cached_user
        && !has_any_local_keys
        && !has_any_cookie
    {
        return Err(local_match
            .and_then(|item| (!item.error.is_empty()).then_some(item.error))
            .unwrap_or_else(|| "没有找到可用的 NewAPI 本地账号或刷新会话".into()));
    }
    emit_chrome_account_progress(
        &bus,
        run_id,
        "local-account",
        "success",
        format!(
            "{account_label} 认证策略：{}",
            if use_refresh_auth {
                "NewAPI 刷新令牌（new_api_refresh → Bearer Token）"
            } else if user_id.is_some() {
                "传统 NewAPI 会话（session Cookie + New-Api-User）"
            } else {
                "宽松会话证据（仅有 Cookie/本地存储痕迹，缺少用户 ID，页面内验证失败会快速返回）"
            }
        ),
    );

    let mut resolved_account = None;

    // 只有刷新令牌模式才读取访问令牌缓存；Cookie 模式没有该机制。
    if use_refresh_auth {
        if let Some(cached_token) = &cached_token {
            if !cached_token.is_empty() {
                emit_chrome_account_progress(
                    &bus,
                    run_id,
                    "token-cache",
                    "running",
                    format!("正在使用 {account_label} 的缓存凭证验证"),
                );
                let cached_token = cached_token.clone();
                let cached_uid = cached_uid.clone().unwrap_or_default();
                let base_url_for_proxy = base_url.clone();
                let current_month_for_proxy = current_month.clone();
                let account_label_for_proxy = account_label.clone();
                let proxy_result = proxypool::with_account_proxy(
                    database,
                    runtime,
                    &site_id,
                    &profile_id,
                    Duration::from_secs(8),
                    3,
                    "账号接口请求",
                    move |client| {
                        let cached_token = cached_token.clone();
                        let cached_uid = cached_uid.clone();
                        let base_url = base_url_for_proxy.clone();
                        let current_month = current_month_for_proxy.clone();
                        let account_label = account_label_for_proxy.clone();
                        async move {
                            let cached_auth = NewApiAuth::Token {
                                access_token: cached_token.clone(),
                                user_id: cached_uid.clone(),
                            };
                            let endpoint = base_url
                                .join("/api/user/self")
                                .map_err(|_| "无法生成账号接口地址".to_string())?;
                            let user_agent = sync::chrome_user_agent();
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
                                Ok(value) => {
                                    let account =
                                        parse_newapi_account(&value).map_err(|error| {
                                            format!("{account_label} 访问令牌响应无法解析：{error}")
                                        })?;
                                    Ok(Some((
                                        account,
                                        ChromeBridgeAccountResult {
                                            ok: true,
                                            error: String::new(),
                                            api_token: cached_token,
                                            user_id: cached_uid,
                                            checkin_enabled: checkin.enabled,
                                            checked_in_today: checkin.checked_in_today,
                                            checkin_error: checkin.error,
                                            account: None,
                                        },
                                    )))
                                }
                                Err(error) if access_token_was_rejected(&error) => Ok(None),
                                Err(error) => Err(error),
                            }
                        }
                    },
                )
                .await;

                match proxy_result {
                    Ok(Some((account, bridge_result))) => {
                        emit_chrome_account_progress(
                            &bus,
                            run_id,
                            "token-cache",
                            "success",
                            format!("{account_label} 缓存访问令牌有效，跳过浏览器同步"),
                        );
                        resolved_account = Some((account, bridge_result));
                    }
                    Ok(None) => {
                        emit_chrome_account_progress(
                            &bus,
                            run_id,
                            "token-cache",
                            "info",
                            format!(
                                "{account_label} 访问令牌收到 HTTP 401，继续通过 Chrome 浏览器同步"
                            ),
                        );
                    }
                    Err(error) => {
                        emit_chrome_account_progress(
                        &bus,
                        run_id,
                        "token-cache",
                        "info",
                        format!("{account_label} 缓存访问令牌直连未通过（{error}），继续通过 Chrome 浏览器同步"),
                    );
                    }
                }
            }
        }
    }

    // 三阶段预算合计 60 秒（15+15+30 / 20+15+25），与 SITE_SYNC_TIMEOUT 对齐：
    // 静默/后台失败要尽早让位给可见验证，可见验证也只保留有限窗口，
    // 超时就整体失败并进入浏览器兜底冷却，不长时间卡住同步弹窗。
    let silent_timeout = if use_refresh_auth {
        Duration::from_secs(20)
    } else {
        Duration::from_secs(15)
    };
    let background_timeout = Duration::from_secs(15);
    let visible_timeout = if use_refresh_auth {
        Duration::from_secs(25)
    } else {
        Duration::from_secs(30)
    };

    // 每个阶段开始前检查强制停止：取消后立即失败，不再打开新的 Chrome 标签页。
    if is_site_account_sync_cancelled(run_id) {
        return Err("同步已被手动强制停止".into());
    }

    // refresh 模式下即使没有 user_id 也允许静默请求（bridge 脚本不依赖 user_id）
    let can_silent = (user_id.is_some() || use_refresh_auth) && resolved_account.is_none();
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
            use_refresh_auth,
            supports_checkin,
            false,
        );
        emit_chrome_account_progress(
            &bus,
            run_id,
            "browser-bypass",
            "running",
            format!("正在尝试复用已打开的同账号 Chrome 页面（{account_label}），不切换窗口"),
        );
        let silent_attempt = spawn_blocking({
            let base_url = base_url.to_string();
            move || {
                sync::run_javascript_in_existing_chrome_tab(
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
                        &bus,
                        run_id,
                        "browser-bypass",
                        "success",
                        "已通过现有 Chrome 页面静默获取账号数据",
                    );
                    resolved_account = Some(parsed);
                }
                Err(error) => emit_chrome_account_progress(
                    &bus,
                    run_id,
                    "browser-bypass",
                    "success",
                    format!("现有页面静默请求未通过，继续尝试后台 Chrome：{error}"),
                ),
            },
            Ok(Ok(None)) => emit_chrome_account_progress(
                &bus,
                run_id,
                "browser-bypass",
                "success",
                "没有找到已打开的同账号站点页面，继续尝试后台 Chrome",
            ),
            Ok(Err(error)) => {
                if sync::is_blocking_chrome_automation_error(&error) {
                    return Err(error);
                }
                emit_chrome_account_progress(
                    &bus,
                    run_id,
                    "browser-bypass",
                    "success",
                    format!("现有页面静默请求不可用，继续尝试后台 Chrome：{error}"),
                )
            }
            Err(error) => emit_chrome_account_progress(
                &bus,
                run_id,
                "browser-bypass",
                "success",
                format!("现有页面静默任务失败，继续尝试后台 Chrome：{error}"),
            ),
        }
    } else {
        emit_chrome_account_progress(
            &bus,
            run_id,
            "browser-bypass",
            "info",
            format!("{account_label} 缺少可核验的用户 ID 且非刷新模式，跳过静默请求以避免串用 Chrome 账号"),
        );
    }

    if resolved_account.is_none() {
        if is_site_account_sync_cancelled(run_id) {
            return Err("同步已被手动强制停止".into());
        }
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
            use_refresh_auth,
            supports_checkin,
            true,
        );
        emit_chrome_account_progress(
            &bus,
            run_id,
            "browser-background",
            "running",
            format!("正在后台打开 {account_label} 的 Chrome 并尝试自动通过验证"),
        );
        let account_proxy_url =
            proxypool::proxy_url_for_account(database, runtime, &site_id, &profile_id)
                .ok()
                .flatten();
        let background_attempt = spawn_blocking({
            let browser_url = browser_url.to_string();
            let profile_id = profile_id.clone();
            let marker = marker.clone();
            let account_proxy_url = account_proxy_url.clone();
            // 仅在 bridge 脚本能核对 Local Storage 用户 ID 时才允许复用遗留标签，
            // 防止把账号请求注入其他 Chrome 账号的页面。
            let allow_tab_reuse = user_id.is_some();
            move || {
                sync::run_javascript_in_background_chrome_profile(
                    &browser_url,
                    &profile_id,
                    &marker,
                    &javascript,
                    background_timeout,
                    account_proxy_url.as_deref(),
                    allow_tab_reuse,
                )
            }
        })
        .await;
        match background_attempt {
            Ok(Ok(value)) => match parse_chrome_account_bridge_result(&value) {
                Ok(parsed) => {
                    emit_chrome_account_progress(
                        &bus,
                        run_id,
                        "browser-background",
                        "success",
                        "后台 Chrome 已完成账号请求，临时标签已关闭",
                    );
                    resolved_account = Some(parsed);
                }
                Err(error) => emit_chrome_account_progress(
                    &bus,
                    run_id,
                    "browser-background",
                    "success",
                    format!("后台请求仍需人工验证，将显示 Chrome：{error}"),
                ),
            },
            Ok(Err(error)) => {
                if sync::is_blocking_chrome_automation_error(&error) {
                    return Err(error);
                }
                emit_chrome_account_progress(
                    &bus,
                    run_id,
                    "browser-background",
                    "success",
                    format!("后台请求未完成，将显示 Chrome：{error}"),
                )
            }
            Err(error) => emit_chrome_account_progress(
                &bus,
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
            if is_site_account_sync_cancelled(run_id) {
                return Err("同步已被手动强制停止".into());
            }
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
                use_refresh_auth,
                supports_checkin,
                true,
            );
            emit_chrome_account_progress(
                &bus,
                run_id,
                "chrome-request",
                "running",
                format!("{account_label} 静默请求未能完成，正在打开 Chrome；如出现验证，请在浏览器中完成"),
            );
            let account_proxy_url =
                proxypool::proxy_url_for_account(database, runtime, &site_id, &profile_id)
                    .ok()
                    .flatten();
            let bridge_result = spawn_blocking({
                let browser_url = browser_url.to_string();
                let profile_id = profile_id.clone();
                let marker = marker.clone();
                let account_proxy_url = account_proxy_url.clone();
                let allow_tab_reuse = user_id.is_some();
                move || {
                    sync::run_javascript_in_chrome_profile(
                        &browser_url,
                        &profile_id,
                        &marker,
                        &javascript,
                        visible_timeout,
                        account_proxy_url.as_deref(),
                        allow_tab_reuse,
                    )
                }
            })
            .await
            .map_err(|error| format!("Chrome 同步任务失败：{error}"))??;
            let parsed = parse_chrome_account_bridge_result(&bridge_result)?;
            emit_chrome_account_progress(
                &bus,
                run_id,
                "chrome-request",
                "success",
                format!("{account_label} Chrome 已返回账号接口数据"),
            );
            parsed
        }
    };

    emit_chrome_account_progress(
        &bus,
        run_id,
        "account-cache",
        "running",
        format!("正在更新 {account_label} 的 SQLite 账号缓存"),
    );

    let connection = database.lock_conn()?;
    let changed = connection
        .execute(
            "UPDATE site_accounts
             SET username = ?1, remaining = ?2, used = ?3, total = ?4, unit = ?5,
                 is_valid = 1, sync_error = '', checkin_enabled = ?6,
                 checked_in_today = ?7, checkin_error = ?8,
                 checkin_date = date('now', 'localtime'),
                 newapi_token = ?9, newapi_user_id = ?10,
                 browser_fallback_failed_at = 0, browser_fallback_fail_count = 0,
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
        &bus,
        run_id,
        "account-cache",
        "success",
        format!("{account_label} 账号额度与签到状态已保存到 SQLite"),
    );
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sub2api_usage_with_remaining_used_total() {
        let value = serde_json::json!({
            "code": 0,
            "message": "success",
            "data": {
                "username": "alice",
                "remaining": 12.5,
                "used": 3.5,
                "total": 16.0,
                "unit": "USD"
            }
        });
        let account = parse_sub2api_usage(&value).unwrap();
        assert_eq!(account.username, "alice");
        assert_eq!(account.remaining, Some(12.5));
        assert_eq!(account.used, Some(3.5));
        assert_eq!(account.total, Some(16.0));
        assert_eq!(account.unit, "USD");
    }

    #[test]
    fn derives_total_when_sub2api_usage_omits_total() {
        let value = serde_json::json!({
            "code": "0",
            "data": { "remaining": 10.0, "used": 2.0 }
        });
        let account = parse_sub2api_usage(&value).unwrap();
        assert_eq!(account.remaining, Some(10.0));
        assert_eq!(account.used, Some(2.0));
        assert_eq!(account.total, Some(12.0));
        assert_eq!(account.unit, "USD");
    }

    #[test]
    fn rejects_sub2api_usage_without_valid_envelope() {
        let value = serde_json::json!({ "data": { "remaining": 1.0 } });
        assert!(parse_sub2api_usage(&value).is_err());
    }

    #[test]
    fn cloudflare_shield_errors_require_chrome_fallback() {
        // 生产环境实测错误文本（42公益站开启 Cloudflare 人机验证后）。
        let shield = "账号接口 HTTP 403 返回 HTML：Cloudflare 安全验证拦截了直接请求，请先用对应 Chrome 账号打开站点并通过验证";
        assert!(is_cloudflare_shield_error(shield));
        assert!(requires_chrome_fallback(shield));
        let checkin_shield = "签到状态接口 HTTP 403 返回 HTML：Cloudflare 安全验证拦截了直接请求";
        assert!(is_cloudflare_shield_error(checkin_shield));
    }

    #[test]
    fn browser_fallback_cooldown_backs_off_exponentially_with_cap() {
        // 10 分钟起步，逐次翻倍，2 小时封顶；零失败/零时间戳不进入冷却。
        assert_eq!(browser_fallback_total_cooldown_ms(0), 0);
        assert_eq!(browser_fallback_total_cooldown_ms(1), 10 * 60 * 1000);
        assert_eq!(browser_fallback_total_cooldown_ms(2), 20 * 60 * 1000);
        assert_eq!(browser_fallback_total_cooldown_ms(3), 40 * 60 * 1000);
        assert_eq!(browser_fallback_total_cooldown_ms(10), 2 * 60 * 60 * 1000);
        // 没有失败时间戳（旧库行）或计数为 0 时不冷却。
        assert_eq!(browser_fallback_cooldown_remaining_ms(0, 3), 0);
        assert_eq!(browser_fallback_cooldown_remaining_ms(1, 0), 0);
    }

    #[test]
    fn browser_fallback_cooldown_expires_with_time() {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_millis() as i64)
            .unwrap_or(0);
        // 一分钟前失败、冷却 10 分钟：剩余应略小于 9 分钟且大于 8 分钟。
        let remaining = browser_fallback_cooldown_remaining_ms(now_ms - 60_000, 1);
        assert!(remaining > 8 * 60 * 1000 && remaining <= 9 * 60 * 1000);
        // 3 小时前失败：冷却早已结束。
        assert_eq!(
            browser_fallback_cooldown_remaining_ms(now_ms - 3 * 60 * 60 * 1000, 1),
            0
        );
    }

    #[test]
    fn token_rejection_requires_chrome_fallback() {
        assert!(requires_chrome_fallback(
            "账号接口 HTTP 401：未登录或令牌已失效"
        ));
        assert!(requires_chrome_fallback(
            "账号接口 HTTP 403：无效的令牌，请重新登录"
        ));
    }

    #[test]
    fn transient_errors_keep_local_account_without_chrome_fallback() {
        // 网络抖动、服务端 5xx 等瞬时错误不应触发浏览器兜底，
        // 保留本地缓存展示即可（对应 needsChromeAccountFallback 的语义）。
        assert!(!requires_chrome_fallback(
            "账号接口请求失败：error sending request for url (https://example.com/api/user/self)"
        ));
        assert!(!requires_chrome_fallback("账号接口 HTTP 502：Bad Gateway"));
        assert!(!is_cloudflare_shield_error(
            "账号接口 HTTP 502：Bad Gateway"
        ));
    }

    #[test]
    fn turnstile_checkin_errors_switch_to_manual_hint() {
        // 生产实测：Pomelo（api.67.si）开启 turnstile_check 后签到接口返回 200 + success:false。
        // 识别后仅附加手动签到提示，不再继续自动尝试（浏览器内代签耗时过长）。
        assert!(is_turnstile_checkin_error("Turnstile token 为空"));
        assert!(is_turnstile_checkin_error(
            "Turnstile 校验失败，请刷新重试！（站点签到启用了 Turnstile 人机验证，无法自动签到，请打开站点签到页手动完成）"
        ));
        assert!(!is_turnstile_checkin_error("签到失败：今日已签到"));
        assert!(!is_turnstile_checkin_error("签到状态接口 HTTP 500"));
    }

    #[test]
    fn checkin_disabled_messages_route_to_log_fallback() {
        // 部分站点签到关闭时返回 success:false + "签到功能未启用"提示，
        // 需识别为未启用状态（走日志兜底），而不是解析失败。
        assert!(is_checkin_disabled_message("签到功能未启用"));
        assert!(is_checkin_disabled_message("签到功能尚未开启"));
        assert!(is_checkin_disabled_message("checkin not enabled"));
        assert!(!is_checkin_disabled_message("签到状态接口 HTTP 500"));
        assert!(!is_checkin_disabled_message("今日已签到"));
        // parse 层面：success=false + 未启用提示 → Ok((false, false))。
        let value = serde_json::json!({
            "success": false,
            "message": "签到功能未启用",
        });
        assert_eq!(parse_newapi_checkin_status(&value), Ok((false, false)));
        // 其他失败仍按解析失败处理。
        let failed = serde_json::json!({ "success": false, "message": "未登录" });
        assert!(parse_newapi_checkin_status(&failed).is_err());
    }

    #[test]
    fn chrome_account_bridge_script_always_includes_credentials() {
        let script =
            chrome_account_bridge_script(Some("42"), "2026-08", "openhub-test", false, true, true);
        assert!(!script.contains("credentials: \"omit\""));
        assert!(!script.contains("credentials: useSessionCookies"));
        assert!(script.contains("method: \"GET\", credentials: \"include\""));
        assert!(script.contains("method: \"POST\", credentials: \"include\""));
    }

    #[test]
    fn parse_newapi_checkin_logs_accepts_today_records() {
        // 当天有签到记录：items 非空。
        let value = serde_json::json!({
            "success": true,
            "message": "",
            "data": { "items": [{ "id": 1, "type": 4, "created_at": 1786982400 }] }
        });
        assert_eq!(parse_newapi_checkin_logs(&value), Ok(true));
        // 当天无签到记录：items 为空数组。
        let empty = serde_json::json!({
            "success": true,
            "data": { "items": [], "pagination": { "total": 0 } }
        });
        assert_eq!(parse_newapi_checkin_logs(&empty), Ok(false));
    }

    #[test]
    fn parse_newapi_checkin_logs_accepts_loose_shapes() {
        // 部分实现把记录直接放在 data.list / data.data / data 下。
        for data in [
            serde_json::json!([{ "id": 1 }]),
            serde_json::json!({ "list": [{ "id": 1 }] }),
            serde_json::json!({ "records": [{ "id": 1 }] }),
        ] {
            let value = serde_json::json!({ "success": true, "data": data });
            assert_eq!(parse_newapi_checkin_logs(&value), Ok(true), "{data}");
        }
    }

    #[test]
    fn parse_newapi_checkin_logs_rejects_bad_responses() {
        // success=false 或缺少记录列表都视为无法确认。
        let failed = serde_json::json!({ "success": false, "message": "未登录" });
        assert!(parse_newapi_checkin_logs(&failed).is_err());
        let missing = serde_json::json!({ "success": true, "data": { "pagination": {} } });
        assert!(parse_newapi_checkin_logs(&missing).is_err());
    }

    #[test]
    fn local_day_unix_range_spans_exactly_one_day() {
        let (start, end) = local_day_unix_range();
        // 区间长度固定为 86400 秒（23:59:59 - 00:00:00）。
        assert_eq!(end - start, 86_399);
        // start 转回本地时区后应落在本地当天 00:00:00（秒偏移为 0）。
        assert_eq!((start + local_utc_offset_secs()) % 86_400, 0);
        // 区间覆盖当前时刻。
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_secs() as i64)
            .unwrap_or(0);
        assert!(start <= now && now <= end);
    }

    #[test]
    fn chrome_account_bridge_script_includes_checkin_log_fallback() {
        let script =
            chrome_account_bridge_script(Some("42"), "2026-08", "openhub-test", false, true, true);
        // 状态接口报"功能未启用"（含 success:false + 提示语）时应携带当天签到日志查询
        // （type=4 和 type=1），兜底结果标记 checkinResolvedViaLog，绝不触发自动代签。
        assert!(script.contains("/api/log/self"));
        assert!(script.contains("[4, 1]"));
        assert!(script.contains("start_timestamp="));
        assert!(script.contains("end_timestamp="));
        assert!(script.contains("未启用|未开启|没有启用|not enabled|not_enabled|disabled"));
        assert!(script.contains("checkinResolvedViaLog"));
        assert!(script.contains("!checkinResolvedViaLog"));
        assert!(script.contains("checkinEnabled && !checkedInToday"));
    }
}
