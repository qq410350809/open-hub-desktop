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
    // 新版 Sub2API 的 /v1/usage 返回扁平结构：{"balance":…,"daily_usage":[…],
    // "isValid":…,"planName":…}，既没有 code 也没有 data.status。旧逻辑要求二者
    // 必有一个，于是把所有合法的用量响应都判成“数据无效”，Key 路径彻底失效，
    // 只能退回 auth_token 会话端点；会话一过期整站就报 401——同一站点两个 Chrome
    // Profile 一个成功、一个失败，就是这么来的。
    // 改为先读余额：只有明确失败的信封才判错，读不到余额时才回落到信封判定。
    let explicit_failure = value.get("success").and_then(json_boolish) == Some(false)
        || value.get("code").is_some_and(|code| {
            code.as_i64().is_some_and(|code| code != 0)
                || code.as_str().is_some_and(|code| code != "0")
        });
    if explicit_failure {
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
    let remaining = remaining.ok_or_else(|| {
        let code_valid = value
            .get("code")
            .is_some_and(|code| code.as_i64() == Some(0) || code.as_str() == Some("0"));
        let status_valid = value
            .pointer("/data/status")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|status| status.eq_ignore_ascii_case("active"));
        if code_valid || status_valid {
            "Sub2API 用量响应缺少有效的余额字段".to_string()
        } else {
            api_error_message(value, "Sub2API 返回的用量数据无效")
        }
    })?;
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
    request_json_with_hint(request, "Sub2API 用量接口", SUB2API_API_KEY_FAILURE_HINT)
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

/// 浏览器会话判定：只回答「浏览器里这个站点有没有登录会话」。
///
/// 只有两类证据算数：
///  1. Local Storage 里能解析出该架构的结构化账号数据（`user` / Sub2API 账号 /
///     Sub2API 账号）—— 站点在浏览器里存下了自己的账号信息；
///  2. 有**登录类** Cookie —— 排除 `cf_clearance` / `acw_tc` 这类人机验证与
///     统计 Cookie：它们只证明浏览器访问过该站，不证明存在账号会话。
///
/// 两者皆无就是「没有会话信息」，站点上不该出现这个账号：留一个点任何操作都
/// 只会得到「没有可用凭据」的账号行，是噪声而不是会话。判定用 cookie_names
/// 而不是 cookie_count——后者会与名字不同步（脏数据里 count=1 而 names 为空），
/// 按名字判才准。
pub(crate) fn has_browser_session_evidence(
    system_type: &str,
    values: Option<&HashMap<String, String>>,
    cookie_names: &[String],
) -> bool {
    if values.is_some_and(|values| has_local_account_session(system_type, values)) {
        return true;
    }
    cookie_names.iter().any(|name| is_login_cookie_name(name))
}

/// 只用于判定「算不算登录会话」的噪音 Cookie：人机验证、风控与统计类。
/// 它们由边缘节点下发，任何人访问过站点都会拿到，与账号登录无关。
///
/// 名单里只列**固定名**的；前缀型噪音（Cloudflare 的 `cf_*` / `__cf*`，
/// 如 `cf_clearance`、`__cf_bm`、`__cf_ob`、`cf_chl_*`）由 `is_noise_cookie_name`
/// 按前缀判定，因为 Cloudflare 会随验证方式不断新增名字。
const NON_LOGIN_COOKIE_NAMES: [&str; 6] = [
    "acw_tc",
    "acw_sc__v2",
    "__cfduid",
    "_cfuvid",
    "__stripe_mid",
    "_gcl_au",
];

/// Cookie 名是否属于人机验证 / 风控 / 统计类噪音（前缀 + 固定名单）。
///
/// Cloudflare 的下发物一律忽略：`cf_clearance`、`cf_chl_*`、`__cf_bm`、
/// `__cf_ob`、`__cfwaitingroom` … 它们的共同点是"证明浏览器访问过该站"，
/// 但不证明存在账号会话；把它们当会话会让每个用过 Cloudflare 的站点都凭空
/// 多出一个空账号。
///
/// 判定用**包含**而不是前缀：Cloudflare 除了 `cf_*` / `__cf*` 前缀，还会下发
/// `_cfuvid`、`x-cf-*` 这类夹在中间的名字，只匹配前缀一定会漏。
pub(crate) fn is_noise_cookie_name(name: &str) -> bool {
    let normalized = name.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return true;
    }
    if normalized.contains("cf_") || normalized.contains("__cf") || normalized.contains("-cf-") {
        return true;
    }
    NON_LOGIN_COOKIE_NAMES
        .iter()
        .any(|noise| normalized == *noise)
}

/// Cookie 名是否算登录会话证据（只看名字，不看值）。
pub(crate) fn is_login_cookie_name(name: &str) -> bool {
    !is_noise_cookie_name(name)
}

/// 只用于判定「算不算会话痕迹」的噪音 Local Storage 键。
///
/// 这些键由前端框架 / 组件库写入，和登录无关：`iconify*`（图标缓存）、
/// `theme` / `color-scheme`（主题）、`i18next*`（语言包）等。站点换了 UI 库
/// 就会新增名字，所以按前缀 + 固定名判定。
const NON_SESSION_STORAGE_KEY_PREFIXES: [&str; 6] = [
    "iconify",
    "i18next",
    "vite",
    "webpack",
    "redux",
    "__vite",
];

/// 无论在哪都算噪音的片段：Cloudflare 的标记键也可能落进 Local Storage。
const NON_SESSION_STORAGE_KEY_FRAGMENTS: [&str; 3] = ["cf_", "__cf", "-cf-"];

const NON_SESSION_STORAGE_KEYS: [&str; 6] = [
    "theme",
    "color-scheme",
    "colorscheme",
    "darkmode",
    "language",
    "locale",
];

/// Local Storage 键是否算会话痕迹（前缀 + 片段 + 固定名，忽略大小写）。
pub(crate) fn is_session_storage_key(key: &str) -> bool {
    let normalized = key.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }
    if NON_SESSION_STORAGE_KEY_PREFIXES
        .iter()
        .any(|prefix| normalized.starts_with(prefix))
    {
        return false;
    }
    if NON_SESSION_STORAGE_KEY_FRAGMENTS
        .iter()
        .any(|fragment| normalized.contains(fragment))
    {
        return false;
    }
    !NON_SESSION_STORAGE_KEYS
        .iter()
        .any(|noise| normalized == *noise)
}

/// Local Storage 里是否至少有一个**与登录有关**的键。
///
/// 用于「有任意键就当会话候选」的宽松判定：只有 `iconify*` / `theme` 这种
/// UI 噪音键的站点，不该因为"桶里非空"就凭空多出一个空账号。
pub(crate) fn has_session_storage_keys(values: &HashMap<String, String>) -> bool {
    values.keys().any(|key| is_session_storage_key(key))
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

/// 目标 Chrome Profile 自己的 NewAPI 登录会话 ID。
///
/// `new_api_refresh` Cookie 的值形如 `<sid>.<随机串>`，而站点签发的访问令牌
/// （JWT）里的 `sid` 声明就是同一个值；NewAPI 前端不写 `localStorage.user` 的
/// 站点没有别的可比对象，这是判断"这条令牌是否真的属于这个 Profile"的唯一
/// 可靠依据（同一个 Profile 里会话轮换后 `sid` 保持不变）。
pub(crate) fn newapi_refresh_session_id(cookie_header: &str) -> Option<String> {
    cookie_header
        .split(';')
        .filter_map(|pair| pair.trim().strip_prefix("new_api_refresh="))
        .find_map(|value| {
            let sid = value.split('.').next().unwrap_or("").trim();
            (sid.len() >= 8
                && sid
                    .chars()
                    .all(|character| character.is_ascii_hexdigit() || character == '-'))
            .then(|| sid.to_string())
        })
}

/// 从 NewAPI 访问令牌（JWT）里读出 `sid`（登录会话 ID）。令牌不透明时返回 None。
pub(crate) fn newapi_token_session_id(token: &str) -> Option<String> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    let payload = token.split('.').nth(1)?.trim_end_matches('=');
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let value = serde_json::from_slice::<serde_json::Value>(&decoded).ok()?;
    value
        .get("sid")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|sid| !sid.is_empty())
        .map(str::to_string)
}

/// 桥接返回的令牌是否来自另一个登录会话。
///
/// 返回 `Some(实际会话 ID)` 表示能明确判定串号（复用到了别的 Chrome Profile 的
/// 同站点页面）；返回 `None` 表示无法判定或一致，按可用处理。
fn bridge_session_mismatch(
    result: &ChromeBridgeAccountResult,
    expected: Option<&str>,
) -> Option<String> {
    let expected = expected?;
    let actual = newapi_token_session_id(&result.api_token)?;
    (!actual.eq_ignore_ascii_case(expected)).then_some(actual)
}
/// 读取某个 Chrome Profile 自己的 NewAPI 登录会话 ID（读不到时返回 None）。
async fn profile_newapi_session_id(
    base_url: &Url,
    profile_id: &str,
    home_dir: &std::path::Path,
) -> Option<String> {
    let target_url = base_url.to_string();
    let profile = profile_id.to_string();
    let home = home_dir.to_path_buf();
    spawn_blocking(move || {
        sync::read_chrome_cookie_header_from_home(&home, &target_url, &profile)
    })
    .await
    .ok()
    .and_then(|value| value.ok())
    .as_deref()
    .and_then(newapi_refresh_session_id)
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
    let success_failed = value.get("success").and_then(json_boolish) == Some(false);
    // 部分发行版用字符串 code（"SUCCESS"/"OK"），只有纯数字且非 0 才算失败。
    let code_failed = value.get("code").is_some_and(|code| match code {
        serde_json::Value::Number(code) => code.as_i64().is_some_and(|code| code != 0),
        serde_json::Value::String(code) => code
            .trim()
            .parse::<i64>()
            .map(|code| code != 0)
            .unwrap_or(false),
        _ => false,
    });
    if success_failed || code_failed {
        return Err(api_error_message(value, "Sub2API 签到状态数据无效"));
    }
    let explicit = [
        "/data/checked_in_today",
        "/data/checked_in",
        "/data/is_checked_in",
        "/data/has_checked_in",
        "/data/today_checked",
        "/data/checked",
        "/checked_in_today",
        "/checked_in",
        "/is_checked_in",
        "/has_checked_in",
        "/today_checked",
        "/checked",
        "/data/status",
        "/status",
        "/data",
    ]
    .iter()
    .find_map(|pointer| value.pointer(pointer).and_then(json_boolish));
    if let Some(explicit) = explicit {
        return Ok(explicit);
    }
    // 部分发行版（如呆瓜）没有显式「今日已签到」字段，用 can_checkin
    // （今日是否还能签）与 today_reward（今日已发放的奖励）表达。
    let can_checkin = ["/data/can_checkin", "/can_checkin"]
        .iter()
        .find_map(|pointer| value.pointer(pointer))
        .and_then(json_boolish);
    let today_reward = ["/data/today_reward", "/today_reward"]
        .iter()
        .find_map(|pointer| value.pointer(pointer))
        .filter(|reward| !reward.is_null());
    match (can_checkin, today_reward) {
        (Some(true), _) => Ok(false),
        (Some(false), Some(_)) => Ok(true),
        (Some(false), None) => {
            // 不能签但也没有今日奖励：仅在没有「不可签」信号时才认定为已签到，
            // 否则交由上层保留旧状态，避免把「功能未开放」误写成已签到。
            let unavailable = ["enabled", "unavailable_reason", "unavailable"]
                .iter()
                .any(|key| {
                    value
                        .pointer(&format!("/data/{key}"))
                        .or_else(|| value.pointer(&format!("/{key}")))
                        .is_some_and(|signal| match *key {
                            "enabled" => json_boolish(signal) == Some(false),
                            _ => !signal.is_null(),
                        })
                });
            if unavailable {
                Err("Sub2API 签到状态无法确认今日是否已签到".to_string())
            } else {
                Ok(true)
            }
        }
        (None, Some(_)) => Ok(true),
        (None, None) => Err("Sub2API 签到状态缺少今日签到字段".to_string()),
    }
}

/// Sub2API 没有 NewAPI 的“访问令牌”概念，登录凭据是 Local Storage 里的
/// `auth_token`（可直接当模型 Key 用）。401 失效提示必须说清楚是登录令牌，
/// 并告诉用户去哪个 Chrome 账号里续期，不能套用 NewAPI 的说法。
pub(crate) const SUB2API_AUTH_FAILURE_HINT: &str =
    "（Sub2API 登录令牌（auth_token）已失效或过期：请在对应的 Chrome 账号里打开该站点完成登录续期后重试）";

/// Sub2API 的 `/v1/usage` 只认 `sk-` 密钥：401 说明这条 Key 已被站点删除或禁用，
/// 与会话登录令牌无关，必须与登录失效分开提示，否则用户会被引去重新登录。
pub(crate) const SUB2API_API_KEY_FAILURE_HINT: &str =
    "（该 Sub2API API Key 已失效或被删除：请在站点控制台确认后重新同步该账号的 Key 与模型）";

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
    let value = decode_json_body(status, &content_type, body, label)?;
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

/// 把响应体解析为 JSON；HTML 兜底页与非法 JSON 翻译成可读错误。
fn decode_json_body(
    status: wreq::StatusCode,
    content_type: &str,
    body: &[u8],
    label: &str,
) -> Result<serde_json::Value, String> {
    match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(value) => Ok(value),
        Err(error) => {
            let first = body
                .iter()
                .copied()
                .find(|byte| !byte.is_ascii_whitespace());
            if content_type.contains("text/html") || first == Some(b'<') {
                let reason = if status == wreq::StatusCode::FORBIDDEN {
                    "站点安全验证（Cloudflare / 阿里云 WAF）拦截了直接请求，请先用对应 Chrome 账号打开站点并通过验证"
                } else {
                    "站点返回了网页而不是 API 数据（可能被安全验证拦截），请用对应 Chrome 账号打开站点后重试"
                };
                return Err(format!(
                    "{label} HTTP {} 返回 HTML：{reason}",
                    status.as_u16()
                ));
            }
            Err(format!(
                "{label}返回了无法解析的数据：{}",
                friendly_json_parse_error(&error, body)
            ))
        }
    }
}

/// 端点探测结果：Sub2API 各发行版的签到路径不同，需要区分
/// 「这个候选路径不存在（换下一个）」与「端点存在但请求失败（直接报错）」。
pub(crate) enum JsonEndpointProbe {
    Found(serde_json::Value),
    Missing,
    Failed(String),
}

/// 探测候选端点：404/405 与「2xx 但返回的不是 JSON（被 SPA 前端路由接管）」
/// 视为路径不存在；其余失败（401 盾、网络错误）原样返回，交给调用方报错。
pub(crate) async fn probe_json_endpoint(
    request: wreq::RequestBuilder,
    label: &str,
    auth_failure_hint: &str,
) -> JsonEndpointProbe {
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => return JsonEndpointProbe::Failed(format!("{label}请求失败：{error:#}")),
    };
    let status = response.status();
    if status == wreq::StatusCode::NOT_FOUND || status == wreq::StatusCode::METHOD_NOT_ALLOWED {
        return JsonEndpointProbe::Missing;
    }
    let content_type = response
        .headers()
        .get(wreq::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let body = match response.bytes().await {
        Ok(body) => body,
        Err(error) => {
            return JsonEndpointProbe::Failed(format!("{label}响应读取失败：{error:#}"));
        }
    };
    let body = body
        .strip_prefix(&[0xef, 0xbb, 0xbf])
        .unwrap_or(body.as_ref());
    match decode_json_body(status, &content_type, body, label) {
        Ok(value) if status.is_success() => JsonEndpointProbe::Found(value),
        Ok(value) => {
            let mut message = format!(
                "{label} HTTP {}：{}",
                status.as_u16(),
                api_error_message(&value, "请求失败")
            );
            if status == wreq::StatusCode::UNAUTHORIZED {
                message.push_str(auth_failure_hint);
            }
            JsonEndpointProbe::Failed(message)
        }
        // 2xx 却不是 JSON：路径被站点前端路由接管（SPA 兜底返回 index.html），
        // 说明该 API 路径不存在，按候选不适用处理。
        Err(_) if status.is_success() => JsonEndpointProbe::Missing,
        Err(error) => JsonEndpointProbe::Failed(error),
    }
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
        // newapi2（如 Pomelo）对失效令牌返回的文案
        "access token 无效",
        "invalid access token",
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

/// 判断错误是否属于站点安全盾/网页拦截（Cloudflare 或阿里云 ESA/WAF 等接口返回
/// HTML 页面的情况）。这类失败意味着直接 HTTP 通道被拦截，但 Chrome 同源请求
/// （在浏览器内执行）仍可能正常通过，因此不应把账号/模型同步判定为“无法补救”
/// 而排除 Chrome 兜底。
pub(crate) fn is_cloudflare_shield_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("cloudflare")
        || lower.contains("安全验证")
        || lower.contains("cf-chl")
        || lower.contains("challenge-platform")
        || lower.contains("just a moment")
        || lower.contains("attention required")
        || lower.contains("cf_clearance")
        || lower.contains("acw_sc__v2")
        || lower.contains("acw_tc")
        || lower.contains("cdn_sec_tc")
        || lower.contains("x-tengine")
        || lower.contains("denied by http_custom")
        || lower.contains("var arg1")
        || lower.contains("返回 html")
        || lower.contains("返回了网页")
}

/// 直连失败是否属于网络层不可用（超时 / 连接失败 / 响应体读取失败）。
///
/// 这类失败和遇盾在「直连通道已废」上是同一结论：站点的 Key/账号接口对桌面端
/// 直连超时或被拦（实测某些 NewAPI 站点 /api/token 直连 6~20s 后超时，而浏览器
/// 同源请求正常返回），继续在直连上重试只是白等。遇盾有专门的 HTML 特征，超时却
/// 没有任何响应可判定，必须在错误文本上识别，否则 Chrome 兜底永远不会触发，
/// 表现为一个站点所有账号都报「operation timed out」。
pub(crate) fn is_direct_request_unavailable(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("operation timed out")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("error sending request")
        || lower.contains("connection refused")
        || lower.contains("connection reset")
        || lower.contains("broken pipe")
        || lower.contains("request or response body error")
}

/// 直连失败后是否应移交 Chrome 同源兜底：遇盾（HTML 挑战）或网络层不可用。
pub(crate) fn needs_browser_bridge(error: &str) -> bool {
    is_cloudflare_shield_error(error) || is_direct_request_unavailable(error)
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

/// Sub2API 各发行版的签到端点对（状态查询, 签到动作）。路径随发行版不同：
/// 实测 `/api/v1/redeem/checkin*` 在所有真实站点上均 404，硬编码单一路径会让
/// 签到状态永远查不到。按顺序探测，404/SPA 兜底视为该候选不适用。
pub(crate) const SUB2API_CHECKIN_ENDPOINTS: &[(&str, &str)] = &[
    ("/api/v1/checkin/status", "/api/v1/checkin"),
    ("/api/v1/check-in", "/api/v1/check-in"),
    ("/api/checkin/status", "/api/checkin"),
    ("/api/v1/redeem/checkin/status", "/api/v1/redeem/checkin"),
];

/// 签到状态刷新失败时的回退：
/// - 当天已确认签到过的，保留「已签到」并清空错误 —— 状态查询失败不能把
///   已签到打回未签到（这正是「当天签到过却显示未签到」的成因）；
/// - 否则带上失败原因，让界面显示「无法签到」而不是无信息的默认值。
pub(crate) fn sub2api_checkin_fallback(
    previous: CheckinSnapshot,
    error: String,
) -> CheckinSnapshot {
    if previous.checked_in_today {
        // checked=true 的路径写库时 enabled 必为 true（两者同源写入），
        // 这里显式置 true 防御脏数据。
        CheckinSnapshot {
            enabled: true,
            checked_in_today: true,
            error: String::new(),
        }
    } else {
        CheckinSnapshot {
            enabled: previous.enabled,
            checked_in_today: false,
            error,
        }
    }
}

/// 从签到域（checkin_url 独立于 API 主机时）的 Local Storage 里取登录令牌。
/// 键名随发行版不同：welfalre 系存 `welfare_token`，标准 sub2api 存 `auth_token`。
/// 主域 auth_token 在签到域上会被拒（实测返回 invalid token），必须按域取。
pub(crate) fn checkin_origin_token(values: &HashMap<String, String>) -> Option<String> {
    ["welfare_token", "auth_token"]
        .iter()
        .find_map(|key| values.get(*key))
        .map(|token| token.trim().trim_matches('"'))
        .filter(|token| !token.is_empty())
        .map(str::to_string)
}

/// 候选 (签到基础地址, 令牌) 列表：
/// 1. 跨主机 checkin_url + 该域自己的令牌（welfalre 系的 welfare_token）；
/// 2. 跨主机 checkin_url + 主域 auth_token（同后端的两域名可能互通）；
/// 3. API 基地址 + 主域 auth_token（同主机签到的常规路径）。
/// 同 origin 同令牌去重，无令牌的组合跳过；无任何组合时上层直接回退。
pub(crate) fn sub2api_checkin_probes(
    base_url: &str,
    auth_token: &str,
    checkin_url: &str,
    checkin_token: &str,
) -> Vec<(String, String)> {
    fn push(probes: &mut Vec<(String, String)>, url: &str, token: &str) {
        let url = url.trim();
        let token = token.trim();
        if url.is_empty() || token.is_empty() {
            return;
        }
        let Ok(parsed) = Url::parse(url) else {
            return;
        };
        let origin = parsed.origin().ascii_serialization();
        if origin == "null" {
            return;
        }
        if probes.iter().any(|(existing_url, existing_token)| {
            existing_token == token
                && Url::parse(existing_url)
                    .is_ok_and(|existing| existing.origin().ascii_serialization() == origin)
        }) {
            return;
        }
        probes.push((parsed.to_string(), token.to_string()));
    }
    let mut probes = Vec::new();
    push(&mut probes, checkin_url, checkin_token);
    push(&mut probes, checkin_url, auth_token);
    push(&mut probes, base_url, auth_token);
    probes
}

pub(crate) async fn refresh_sub2api_checkin(
    client: &wreq::Client,
    base_url: &str,
    auth_token: &str,
    checkin_url: &str,
    checkin_token: &str,
    user_agent: &str,
    previous: CheckinSnapshot,
) -> CheckinSnapshot {
    let probes = sub2api_checkin_probes(base_url, auth_token, checkin_url, checkin_token);
    if probes.is_empty() {
        return sub2api_checkin_fallback(
            previous,
            "缺少 Sub2API 登录令牌（auth_token），无法刷新签到状态".to_string(),
        );
    }
    let mut last_error: Option<String> = None;
    'probe: for (probe_base, probe_token) in &probes {
        let probe_base = match Url::parse(probe_base) {
            Ok(url) => url,
            Err(_) => continue,
        };
        let headers = |request: wreq::RequestBuilder| {
            chrome_request_headers(request, probe_base.as_str(), user_agent)
                .bearer_auth(probe_token.as_str())
        };
        for (status_path, action_path) in SUB2API_CHECKIN_ENDPOINTS {
            let status_url = match probe_base.join(status_path) {
                Ok(url) => url,
                Err(_) => continue,
            };
            let status_value = match probe_json_endpoint(
                headers(client.get(status_url)),
                "Sub2API 签到状态接口",
                SUB2API_AUTH_FAILURE_HINT,
            )
            .await
            {
                JsonEndpointProbe::Found(value) => value,
                JsonEndpointProbe::Missing => continue,
                JsonEndpointProbe::Failed(error) => {
                    // 401（令牌失效）对所有候选路径都一样，但换路径探测成本很低，
                    // 继续尝试以免某发行版只有部分路径需要认证。
                    last_error = Some(error);
                    continue;
                }
            };
            let checked_in_today = match parse_sub2api_checkin_status(&status_value) {
                Ok(checked) => checked,
                Err(error) => {
                    last_error = Some(error);
                    continue;
                }
            };
            if checked_in_today {
                return CheckinSnapshot {
                    enabled: true,
                    checked_in_today: true,
                    error: String::new(),
                };
            }
            // 状态接口命中且今日未签到：用配对的签到动作端点执行签到。
            let action_url = match probe_base.join(action_path) {
                Ok(url) => url,
                Err(_) => continue,
            };
            let value = match probe_json_endpoint(
                headers(client.post(action_url)).json(&serde_json::json!({})),
                "Sub2API 签到接口",
                SUB2API_AUTH_FAILURE_HINT,
            )
            .await
            {
                JsonEndpointProbe::Found(value) => value,
                JsonEndpointProbe::Missing => {
                    last_error = Some(format!("签到动作接口不存在（{action_path}）"));
                    continue;
                }
                JsonEndpointProbe::Failed(error) => {
                    // 换一个（基础地址, 令牌）组合再试，最后统一回退。
                    last_error = Some(error);
                    continue 'probe;
                }
            };
            if sub2api_response_succeeded(&value) {
                return CheckinSnapshot {
                    enabled: true,
                    checked_in_today: true,
                    error: String::new(),
                };
            }
            let message = api_error_message(&value, "Sub2API 签到失败");
            // 站点把「重复签到」当失败返回，但语义上就是今日已签到。
            if message.contains("已签到") || message.to_ascii_lowercase().contains("already") {
                return CheckinSnapshot {
                    enabled: true,
                    checked_in_today: true,
                    error: String::new(),
                };
            }
            return sub2api_checkin_fallback(previous, message);
        }
    }
    let error = last_error.unwrap_or_else(|| {
        format!(
            "站点未提供签到接口（{} 条候选路径均不存在）",
            SUB2API_CHECKIN_ENDPOINTS.len()
        )
    });
    sub2api_checkin_fallback(previous, error)
}

/// 「白与黑」账号额度：调 `/api/user/profile`。
///
/// **这个接口认的是浏览器会话，不是访问令牌**：带 Chrome 的登录 Cookie 请求返回
/// `data.quota` / `data.used_quota`（单位 1/500000 美元，站点 `/api/status` 的
/// `quota_per_unit` 也是 500000），而只带 `Authorization: Bearer <访问令牌>` 会被
/// 站点以 403 `auth_required`「无权进行此操作」拒绝——访问令牌是给
/// `/api/user/dashboard` 那类管理接口用的，不是这条。两者同时带上时以 Cookie 为准，
/// 所以这里优先发 Cookie、有令牌时一并带上（对方按会话处理）。
/// 响应形状与 NewAPI 的 `/api/user/self` 相同，因此复用 `parse_newapi_account`。
///
/// 令牌是用户手动保存的（`access_token_is_user_owned`），这里只读不改。
/// 该架构没有签到集成：checkin 快照恒为默认值，不携带旧状态。
///
/// 两条凭据（Cookie 与站点令牌）都没有时没有任何可请求的通道：这不是同步
/// 失败，返回空结果且不写任何错误——调用方在进入前已做同样的跳过判定，
/// 这里只是兜底，保证「没有可用凭据」类提示永远不会出现在同步链路里。
async fn fetch_baiheibai_account(
    client: &wreq::Client,
    base_url: &str,
    cookie_header: &str,
    cached_token: Option<&str>,
    cached_user_id: Option<&str>,
    user_agent: &str,
) -> Result<SiteAccountRefresh, String> {
    let user_id = cached_user_id.unwrap_or_default().to_string();
    let token = cached_token
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let cookie = cookie_header.trim();
    if cookie.is_empty() && token.is_none() {
        return Ok(SiteAccountRefresh {
            account: SiteAccountSnapshot::default(),
            is_valid: false,
            sync_error: String::new(),
            checkin: CheckinSnapshot::default(),
            newapi_token: String::new(),
            newapi_user_id: user_id,
            refreshed: false,
        });
    }
    let endpoint = Url::parse(base_url)
        .map_err(|_| "站点 API 地址无效".to_string())?
        .join("/api/user/profile")
        .map_err(|_| "无法生成账号接口地址".to_string())?;
    // Cookie 是主凭据（站点前端走的就是这条）；有令牌时一并带上，对方按会话处理。
    let mut request = chrome_request_headers(client.get(endpoint), base_url, user_agent);
    if !cookie.is_empty() {
        request = request.header(wreq::header::COOKIE, cookie);
    }
    if let Some(token) = token.as_deref() {
        request = request.bearer_auth(token);
    }
    let response = request_json_with_hint(
        request,
        "账号接口",
        "（登录会话已失效：请用该 Chrome 账号重新登录站点后重试）",
    )
    .await;
    let value = match response {
        Ok(value) => value,
        Err(error) => {
            return Ok(SiteAccountRefresh {
                account: SiteAccountSnapshot::default(),
                is_valid: false,
                sync_error: error,
                checkin: CheckinSnapshot::default(),
                newapi_token: token.clone().unwrap_or_default(),
                newapi_user_id: user_id,
                refreshed: false,
            })
        }
    };
    Ok(match parse_newapi_account(&value) {
        Ok(account) => SiteAccountRefresh {
            account,
            is_valid: true,
            sync_error: String::new(),
            checkin: CheckinSnapshot::default(),
            newapi_token: token.clone().unwrap_or_default(),
            newapi_user_id: user_id,
            refreshed: true,
        },
        Err(error) => SiteAccountRefresh {
            account: SiteAccountSnapshot::default(),
            is_valid: false,
            sync_error: error,
            checkin: CheckinSnapshot::default(),
            newapi_token: token.clone().unwrap_or_default(),
            newapi_user_id: user_id,
            refreshed: false,
        },
    })
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
    // 跨主机签到地址（如 Fengwind 的签到在 api-welfalre.fengwind.com）及其
    // 域下的 Local Storage：签到接口不在 API 主域上时靠它定位与鉴权。
    checkin_url: &str,
    checkin_local_values: &HashMap<String, String>,
) -> Result<SiteAccountRefresh, String> {
    let previous_checkin = if should_checkin {
        previous_checkin
    } else {
        CheckinSnapshot::default()
    };
    // —— 白与黑：浏览器会话 + 访问令牌，额度接口是 /api/user/profile ——
    // 放在架构推断之前：它是已适配的已知架构，类型以库里存的为准，不能再按浏览器里
    // 的 NewAPI 残留被反推成 new-api —— 那会调错接口，报错文案也牛头不对马嘴。
    if is_platform(system_type, "baiheibai") {
        return fetch_baiheibai_account(
            client,
            base_url,
            &cookie_header.clone().unwrap_or_default(),
            cached_newapi_token.as_deref(),
            cached_newapi_user_id.as_deref(),
            user_agent,
        )
        .await;
    }
    let inferred_type;
    let system_type =
        if is_explicit_unknown(system_type) {
            // 用户显式选了「未知类型」：不猜、不探测。浏览器里残留的
            // NewAPI/Sub2API 痕迹不代表这个站点该按那些架构处理，
            // 照着猜就会弹出与设置相矛盾的 NewAPI 报错。
            inferred_type = String::new();
            &inferred_type
        } else if is_newapi(system_type) || is_sub2api(system_type) {
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
            // 空 Cookie = 该 profile 下这个域确实没有登录 Cookie。Cookie 形态
            // 下它就是唯一的登录凭据，继续走只会换来一个 401；刷新令牌形态还有
            // 缓存令牌可试，不在这里拦。
            Ok(value) if !uses_refresh_auth && value.trim().is_empty() => {
                let error = "Chrome 里没有该站点的登录 Cookie".to_string();
                return match local_account {
                    Some(account) => Ok(SiteAccountRefresh {
                        account,
                        is_valid: true,
                        sync_error: error,
                        checkin: previous_checkin,
                        newapi_token: cached_newapi_token.unwrap_or_default(),
                        newapi_user_id: cached_newapi_user_id.unwrap_or_default(),
                        refreshed: false,
                    }),
                    None => Err(error),
                };
            }
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
                        refreshed: false,
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
                    refreshed: false,
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
        // 缓存令牌必须属于这个 Chrome Profile 自己的登录会话：串号的缓存
        // （例如从别的 Profile 页面复用写进来的令牌）会让整站账号数据长期
        // 认成别人，这里按会话 ID 直接丢弃，改走本 Profile 的 Cookie 路径。
        let expected_session = newapi_refresh_session_id(&cookie_header);
        let mut api_token = cached_newapi_token
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .filter(|token| {
                match (newapi_token_session_id(token), expected_session.as_deref()) {
                    (Some(actual), Some(expected)) => actual.eq_ignore_ascii_case(expected),
                    _ => true,
                }
            })
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
                        refreshed: false,
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
            refreshed: true,
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
            // 没有账号接口实现的架构（白与黑等）：这里不探测、不写库，把库里已有的
            // 令牌原样带回，避免调用方整行重写时把手填的令牌清成空串。
            newapi_token: cached_newapi_token.unwrap_or_default(),
            newapi_user_id: cached_newapi_user_id.unwrap_or_default(),
            refreshed: false,
        });
    }

    let checkin = if should_checkin {
        let auth_token = auth_token.as_deref().unwrap_or_default();
        let checkin_token = checkin_origin_token(checkin_local_values).unwrap_or_default();
        refresh_sub2api_checkin(
            client,
            base_url,
            auth_token,
            checkin_url,
            &checkin_token,
            user_agent,
            previous_checkin,
        )
        .await
    } else {
        previous_checkin
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
                    refreshed: true,
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
        // 401 在这里只说明 Local Storage 里的 auth_token 过期（浏览器侧可用
        // refresh_token 续期），必须给 Sub2API 自己的提示，不能套用 NewAPI 的
        // “账号令牌/访问令牌”说法误导用户。
        let account = request_json_with_hint(request, "账号接口", SUB2API_AUTH_FAILURE_HINT)
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
                refreshed: true,
            }),
            Err(error) => Ok(SiteAccountRefresh {
                account: local_account.clone().unwrap_or_default(),
                // 遇盾（403 HTML）时直连通道已不可用，只有 Chrome 同源请求能恢复，
                // 置 is_valid=false 让前端进入浏览器兜底。缺 Key/令牌过期等其它错误
                // 保留缓存展示，不误触发浏览器。
                is_valid: local_account.is_some() && !requires_chrome_fallback(&error),
                sync_error: if candidate_keys.is_empty() && !requires_chrome_fallback(&error) {
                    format!("{error}（该账号暂无可用 API Key，可先同步该账号的 Key 与模型，之后就不再依赖浏览器会话）")
                } else {
                    error
                },
                checkin,
                newapi_token: String::new(),
                newapi_user_id: String::new(),
                refreshed: false,
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
        refreshed: false,
    })
}

/// Chrome 账号桥接打开的页面必须是站点同源控制台。
/// AnyRouter 等站点常把 checkinUrl 写成根路径，打开后首页一跳就把 hash marker
/// 丢掉，AppleScript 随后一直找不到标签，最终报「等待 Chrome 返回账号数据超时」。
///
/// 兜底路径按平台分派（NewAPI 系 → `/console/personal`，Sub2API → `/dashboard`）；
/// 未知平台退回站点根路径而不是硬塞 NewAPI 路径 —— 有的站点根本没有
/// `/console/personal`，打开只会得到 404。
pub(crate) fn chrome_account_bridge_url(
    base_url: &Url,
    checkin_url: &str,
    system_type: &str,
    marker: &str,
) -> Result<Url, String> {
    let default_console_url = || {
        let path = crate::site::library::console_page_path(system_type).unwrap_or("/");
        base_url
            .join(path)
            .map_err(|_| "无法生成 Chrome 验证地址".to_string())
    };
    let console_url = || {
        base_url
            .join("/console")
            .map_err(|_| "无法生成 Chrome 验证地址".to_string())
    };
    let mut browser_url = if checkin_url.trim().is_empty() {
        default_console_url()?
    } else {
        Url::parse(checkin_url).unwrap_or_else(|_| base_url.clone())
    };
    if browser_url.origin() != base_url.origin() {
        browser_url = default_console_url()?;
    }
    if browser_url.path().is_empty() || browser_url.path() == "/" {
        browser_url = console_url()?;
    }
    browser_url.set_fragment(Some(marker));
    Ok(browser_url)
}

pub(crate) fn chrome_account_bridge_script(
    user_id: Option<&str>,
    current_month: &str,
    marker: &str,
    use_refresh_auth: bool,
    should_checkin: bool,
    allow_challenge_navigation: bool,
    expected_session: Option<&str>,
) -> String {
    chrome_account_bridge_script_with_mode(
        user_id,
        current_month,
        marker,
        use_refresh_auth,
        false,
        should_checkin,
        allow_challenge_navigation,
        expected_session,
    )
}

/// Sub2API 账号同步的 Chrome 同源桥接脚本。
///
/// Sub2API 没有 NewAPI 的 `/api/user/self` 会话端点，登录凭据是 Local Storage 里的
/// `auth_token`（Bearer）与 `auth_user`。直连 HTTP 会被 Cloudflare / 阿里云 WAF 以
/// 403 HTML 拦截，而浏览器同源 fetch 带着已通过的挑战 Cookie 可以正常返回，因此
/// 这里复用同一条桥接标签页通道，在页面上下文里调 `/api/v1/auth/me` 取余额。
pub(crate) fn chrome_sub2api_account_bridge_script(
    user_id: Option<&str>,
    current_month: &str,
    marker: &str,
    should_checkin: bool,
    allow_challenge_navigation: bool,
    expected_session: Option<&str>,
) -> String {
    chrome_account_bridge_script_with_mode(
        user_id,
        current_month,
        marker,
        false,
        true,
        should_checkin,
        allow_challenge_navigation,
        expected_session,
    )
}

#[allow(clippy::too_many_arguments)]
fn chrome_account_bridge_script_with_mode(
    user_id: Option<&str>,
    current_month: &str,
    marker: &str,
    use_refresh_auth: bool,
    use_sub2api: bool,
    should_checkin: bool,
    allow_challenge_navigation: bool,
    expected_session: Option<&str>,
) -> String {
    let user_id =
        serde_json::to_string(user_id.unwrap_or_default()).unwrap_or_else(|_| "\"\"".into());
    let current_month = serde_json::to_string(current_month).unwrap_or_else(|_| "\"\"".into());
    let marker = serde_json::to_string(marker).unwrap_or_else(|_| "\"\"".into());
    let expected_session = serde_json::to_string(expected_session.unwrap_or_default())
        .unwrap_or_else(|_| "\"\"".into());
    r#"(() => {
  const token = __OPENHUB_MARKER__;
  const legacyUserId = __OPENHUB_USER_ID__;
  const useRefreshAuth = __OPENHUB_USE_REFRESH_AUTH__;
  const useSub2Api = __OPENHUB_USE_SUB2API__;
  const shouldCheckin = __OPENHUB_SHOULD_CHECKIN__;
  const allowChallengeNavigation = __OPENHUB_ALLOW_CHALLENGE_NAVIGATION__;
  // 目标 Chrome Profile 自己的登录会话 ID：复用其它窗口里已打开的站点页面时，
  // 页面跑在哪个 Profile 的 Cookie 罐里决定了拿到谁的令牌。NewAPI 前端不写
  // `localStorage.user` 的站点没有别的可比对象，这里用令牌里的 `sid` 兜底，
  // 串号就回 PROFILE_MISMATCH，由调用方改开目标 Profile 的新标签。
  const expectedSession = __OPENHUB_EXPECTED_SESSION__;
  const sessionOf = (value) => {
    if (!value || typeof value !== "string") return "";
    const parts = value.split(".");
    if (parts.length < 2) return "";
    try {
      const base64 = parts[1].replace(/-/g, "+").replace(/_/g, "/");
      const padded = base64 + "=".repeat((4 - (base64.length % 4)) % 4);
      const text = atob(padded);
      const bytes = Uint8Array.from(text, (character) => character.charCodeAt(0));
      const payload = JSON.parse(new TextDecoder().decode(bytes));
      return typeof payload?.sid === "string" ? payload.sid : "";
    } catch (_) {
      return "";
    }
  };
  const requestTimeout = 8000;
  const pending = "__OPENHUB_PENDING__";
  // 站点把 message/msg/error 写成对象（如 {"message":{"error":"..."}}）时，
  // 直接把对象塞进 error 字段会让 Rust 侧 `String` 反序列化失败
  // （invalid type: map, expected a string），整次同步报废。这里统一收敛成字符串：
  // 优先取字段里的字符串值，嵌套对象再取一层 message/msg/error，兜底 stringify。
  const asMessageText = (value) => {
    if (value == null) return "";
    if (typeof value === "string") return value;
    if (typeof value === "number" || typeof value === "boolean") return String(value);
    if (typeof value === "object") {
      const nested = value.message || value.msg || value.error;
      if (typeof nested === "string" && nested) return nested;
      if (nested && typeof nested === "object") return asMessageText(nested);
      try { return JSON.stringify(value); } catch (_) { return ""; }
    }
    return "";
  };
  const messageOf = (value, fallback) => {
    const text = value ? asMessageText(value.message || value.msg || value.error) : "";
    return text || asMessageText(fallback);
  };
  const tryParseDocumentAccount = () => {
    if (window.location.pathname !== "/api/user/self") return null;
    const text = String((document.body && (document.body.innerText || document.body.textContent)) || "").trim();
    if (!text.startsWith("{")) return null;
    try {
      return JSON.parse(text);
    } catch (_) {
      return null;
    }
  };
  const documentLooksLikeChallenge = () => {
    const html = String((document.documentElement && document.documentElement.outerHTML) || "").slice(0, 100000).toLowerCase();
    return /var\s+arg1\s*=/.test(html) || (html.includes("acw_sc__v2") && html.length < 20000);
  };
  const beginChallengeNavigation = () => {
    if (!allowChallengeNavigation) {
      return { ok: false, error: "站点安全验证仍需要浏览器交互（Cloudflare / 阿里云 WAF）" };
    }
    if (window.location.pathname !== "/api/user/self") {
      window.location.assign(`/api/user/self#${token}`);
      return null;
    }
    try {
      const key = `__openHubChallengeReloads:${token}`;
      const count = Number(sessionStorage.getItem(key) || 0);
      if (count >= 2) {
        return { ok: false, error: "站点安全验证仍需要浏览器交互（Cloudflare / 阿里云 WAF）" };
      }
      sessionStorage.setItem(key, String(count + 1));
    } catch (_) {}
    window.location.reload();
    return null;
  };
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
      // 新版前端（如 Pomelo）不写 user 键：无法核对时不得判串号（旧逻辑把
      // 整站 newapi2 桥接错杀成 PROFILE_MISMATCH），身份交由 Profile 隔离的
      // Cookie 罐决定；只有能明确读出不同用户 ID 时才拒绝。
      if (storedUserId && String(storedUserId) !== String(legacyUserId)) {
        return "__OPENHUB_PROFILE_MISMATCH__";
      }
    } catch (_) {}
  }
  const documentAccount = tryParseDocumentAccount();
  if (documentAccount) {
    const responseUserId = String(documentAccount.data?.id || documentAccount.data?.userId || "");
    if (legacyUserId && responseUserId && responseUserId !== String(legacyUserId)) {
      return "__OPENHUB_PROFILE_MISMATCH__";
    }
    if (documentAccount.success === true && documentAccount.data && typeof documentAccount.data === "object") {
      try { sessionStorage.removeItem(`__openHubChallengeReloads:${token}`); } catch (_) {}
      const result = {
        ok: true,
        account: documentAccount,
        checkinEnabled: false,
        checkedInToday: false,
        checkinError: "",
        apiToken: "",
        userId: responseUserId || String(legacyUserId || "")
      };
      window.__openHubAccountSync = { token, started: Date.now(), state: "done", result };
      return JSON.stringify(result);
    }
    if (documentAccount.success === false) {
      const result = { ok: false, error: messageOf(documentAccount, "账号接口失败") };
      window.__openHubAccountSync = { token, started: Date.now(), state: "done", result };
      return JSON.stringify(result);
    }
  }
  const previous = window.__openHubAccountSync;
  if (previous && previous.token === token) {
    // 页面所在 Profile 与目标 Profile 不是同一个登录会话：交给调用方改开新标签。
    if (previous.profileMismatch) return "__OPENHUB_PROFILE_MISMATCH__";
    if (previous.result) return JSON.stringify(previous.result);
    if (previous.state !== "challenge" || Date.now() - previous.started < 3000) return pending;
  }
  if (documentLooksLikeChallenge()) {
    const outcome = beginChallengeNavigation();
    if (outcome) {
      window.__openHubAccountSync = { token, started: Date.now(), state: "done", result: outcome };
      return JSON.stringify(outcome);
    }
    window.__openHubAccountSync = { token, started: Date.now(), state: "challenge", result: null };
    return pending;
  }
  const bridge = { token, started: Date.now(), state: "running", result: null };
  window.__openHubAccountSync = bridge;
  const readResponse = async (response) => {
    const contentType = (response.headers.get("content-type") || "").toLowerCase();
    const text = await response.text();
    const lower = text.slice(0, 100000).toLowerCase();
    const isHtml = contentType.includes("text/html") || /^\s*<!doctype html|^\s*<html/i.test(text);
    // 阿里云 ESA/WAF 的 JS 挑战是 HTTP 200 + text/html，正文含 `var arg1='...'`，
    // 执行后写 acw_sc__v2 Cookie 并 location.reload()。它没有 Cloudflare 特征、
    // 状态码也不是 403/429/503，必须单独识别；否则会被当成普通 HTML 错误直接终止，
    // 挑战页 reload 后桥接不再重跑（AnyRouter 等站点因此取不到余额）。
    const isAlibabaChallenge = isHtml && (
      /var\s+arg1\s*=/.test(lower) ||
      lower.includes("acw_sc__v2") || lower.includes("acw_tc") ||
      lower.includes("cdn_sec_tc") || lower.includes("denied by http_custom") ||
      (response.headers.get("x-tengine-error") || "").length > 0
    );
    const isChallenge = isHtml && (
      [403, 429, 503].includes(response.status) ||
      lower.includes("cf-chl-") || lower.includes("challenge-platform") ||
      lower.includes("just a moment") || lower.includes("attention required") ||
      lower.includes("cloudflare ray id") ||
      isAlibabaChallenge
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
  (async () => {
    const headers = { "Accept": "application/json" };
    let activeAccessToken = "";
    if (useSub2Api) {
      // Sub2API 的登录凭据只存在页面 Local Storage：auth_token（Bearer）与
      // auth_user（账号快照）。直连被 WAF 拦截时，只有页面内同源 fetch 能过盾。
      let storedToken = "";
      try {
        storedToken = String(localStorage.getItem("auth_token") || "").trim().replace(/^"|"$/g, "");
        if (!storedToken) {
          storedToken = String(localStorage.getItem("welfare_token") || "").trim().replace(/^"|"$/g, "");
        }
      } catch (_) {}
      if (!storedToken) {
        bridge.result = {
          ok: false,
          error: "Chrome Local Storage 中没有 Sub2API 登录令牌（auth_token）"
        };
        return;
      }
      headers.Authorization = `Bearer ${storedToken}`;
      // 串号防护：目标 Profile 的令牌是唯一可比对的会话标识，这里不做
      // JWT 解析（Sub2API 令牌形状不固定），交由调用方的 Profile 隔离裁决。
      let meResponse = await readResponse(await fetch("/api/v1/auth/me", {
        method: "GET", credentials: "include", cache: "no-store", headers,
        signal: AbortSignal.timeout(requestTimeout)
      }));
      if (meResponse.challenge) {
        const outcome = beginChallengeNavigation();
        if (outcome) {
          bridge.result = outcome;
          return;
        }
        bridge.state = "challenge";
        bridge.started = Date.now();
        return;
      }
      if (meResponse.error || meResponse.status < 200 || meResponse.status >= 300) {
        bridge.result = {
          ok: false,
          error: messageOf(meResponse.data, meResponse.error || `账号接口 HTTP ${meResponse.status}`)
        };
        return;
      }
      // 余额优先取会话端点；响应里没有余额字段时回退 Local Storage 的 auth_user 快照。
      let accountValue = meResponse.data;
      if (!accountValue || typeof accountValue !== "object") {
        try {
          let storedUser = localStorage.getItem("auth_user") || "";
          for (let depth = 0; depth < 2 && typeof storedUser === "string"; depth += 1) {
            storedUser = JSON.parse(storedUser);
          }
          if (storedUser && typeof storedUser === "object") accountValue = { data: storedUser };
        } catch (_) {}
      }
      bridge.result = {
        ok: true,
        account: accountValue,
        checkinEnabled: false,
        checkedInToday: false,
        checkinError: "",
        apiToken: storedToken,
        userId: String(accountValue?.data?.id || accountValue?.data?.userId || "")
      };
      return;
    }
    if (useRefreshAuth) {
      const refreshResponse = await readResponse(await fetch("/api/user/auth/refresh", {
        method: "POST", credentials: "include", cache: "no-store", headers,
        signal: AbortSignal.timeout(requestTimeout)
      }));
      if (refreshResponse.challenge) {
        const outcome = beginChallengeNavigation();
        if (outcome) {
          bridge.result = outcome;
          return;
        }
        bridge.state = "challenge";
        bridge.started = Date.now();
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
    // 令牌必须属于目标 Profile 自己的登录会话：复用到的页面若开在别的 Chrome
    // 账号窗口里，这里拿到的就是那个账号的令牌，直接判串号，由调用方改开新标签。
    if (expectedSession && apiToken && sessionOf(apiToken) &&
        sessionOf(apiToken) !== expectedSession) {
      bridge.state = "done";
      bridge.profileMismatch = true;
      return;
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
    const selfResponse = await readResponse(await fetch("/api/user/self", {
      method: "GET", credentials: "include", cache: "no-store", headers,
      signal: AbortSignal.timeout(requestTimeout)
    }));
    if (selfResponse.challenge) {
      const outcome = beginChallengeNavigation();
      if (outcome) {
        bridge.result = outcome;
        return;
      }
      bridge.state = "challenge";
      bridge.started = Date.now();
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
          checkinError = "站点安全验证拦截了签到状态请求";
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
              checkinError = "站点安全验证拦截了签到请求";
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
        .replace(
            "__OPENHUB_USE_SUB2API__",
            if use_sub2api { "true" } else { "false" },
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
        .replace("__OPENHUB_EXPECTED_SESSION__", &expected_session)
}

/// NewAPI Key 同步的 Chrome 同源桥接脚本：在站点页面上下文里依次拉取
/// Key 列表（`token_path`，NewAPI 系为 `/api/token/?p=1&size=20`，
/// 「白与黑」为 `/api/token/?page=1&size=10&keyword=&order=-id`）、
/// `/v1/models`（模型列表）与模型健康度（`/api/perf-metrics/summary?hours=24`，
/// 站点没有该路由时回退 `/api/enhancements/model-status/status/all`），一次返回三者原文。
/// Key 明文不在 JSON 里时由调用方再走 `/api/token/{id}/key` 揭示
/// （直连此时通常已可用——页面已通过盾）。
///
/// Cloudflare 盾站点的直连请求全部被 403 挑战拦截，浏览器同源 fetch
/// 带着已通过的 cf_clearance 才是唯一可行路径（与账号同步桥接同一机制）。
pub(crate) fn chrome_key_models_bridge_script(
    should_fetch_models: bool,
    user_id: &str,
    token_path: &str,
) -> String {
    r#"(() => {
  const pending = "__OPENHUB_PENDING__";
  // 传统 new-api 会话除了 Cookie 还必须带 New-Api-User 头；DB 缓存值优先，
  // 页面 localStorage 里能读到 id 时兜底（新版前端可能不写缓存值）。
  const legacyUserId = "__OPENHUB_USER_ID__";
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
      // 与账号桥接同一语义：只在能明确读出不同用户 ID 时判串号，
      // 读不到（新版前端不写 user 键）交由 Profile 隔离的 Cookie 罐裁决。
      if (storedUserId && String(storedUserId) !== String(legacyUserId)) {
        return "__OPENHUB_PROFILE_MISMATCH__";
      }
    } catch (_) {}
  }
  const previous = window.__openHubKeySync;
  if (previous && previous.result) return JSON.stringify(previous.result);
  const bridge = { started: Date.now(), result: null };
  window.__openHubKeySync = bridge;
  const readResponse = async (response) => {
    const contentType = (response.headers.get("content-type") || "").toLowerCase();
    const text = await response.text();
    const lower = text.slice(0, 100000).toLowerCase();
    const isHtml = contentType.includes("text/html") || /^\s*<!doctype html|^\s*<html/i.test(text);
    if (isHtml && ([403, 429, 503].includes(response.status) ||
        lower.includes("cf-chl-") || lower.includes("challenge-platform") ||
        lower.includes("just a moment") || lower.includes("attention required"))) {
      return { challenge: true, status: response.status };
    }
    if (isHtml) {
      return { status: response.status, error: "接口返回 HTML" };
    }
    try {
      return { status: response.status, data: JSON.parse(text) };
    } catch (_) {
      return { status: response.status, error: "接口没有返回 JSON" };
    }
  };
  const requestHeaders = { Accept: "application/json" };
  if (legacyUserId) requestHeaders["New-Api-User"] = legacyUserId;
  (async () => {
    try {
      const tokenResponse = await readResponse(await fetch("__OPENHUB_TOKEN_PATH__", {
        method: "GET", credentials: "include", cache: "no-store", headers: requestHeaders,
        signal: AbortSignal.timeout(30000)
      }));
      if (tokenResponse.challenge) {
        bridge.result = { ok: false, error: "站点安全验证仍需要浏览器交互（Cloudflare / 阿里云 WAF）" };
        return;
      }
      if (tokenResponse.error || tokenResponse.status < 200 || tokenResponse.status >= 300) {
        bridge.result = {
          ok: false,
          error: tokenResponse.error || `Key 接口 HTTP ${tokenResponse.status}`
        };
        return;
      }
      bridge.result = { ok: true, tokenList: tokenResponse.data, models: null, health: null };
      if (!__OPENHUB_FETCH_MODELS__) return;
      // Key 列表成功后再拉模型与健康度：失败不推翻 Key 结果，对应字段
      // 置 null 由调用方直连重试。
      //
      // 两个请求并发发出：桥接整体只有 10s/25s 预算（静默 → 可见标签），
      // 串行多一个往返就可能把 Key 同步本身拖超时——那才是真正的主流程。
      // 各自 catch：任一 fetch 抛错都只丢自己的字段，绝不能冒泡到外层
      // catch 把上面已经拿到的 Key 结果覆盖成失败。
      // 健康度超时给到 6s，远小于桥接预算，对一个汇总接口够用了。
      const [modelsResponse, healthResponse] = await Promise.all([
        readResponse(await fetch("/v1/models", {
          method: "GET", credentials: "include", cache: "no-store", headers: requestHeaders,
          signal: AbortSignal.timeout(30000)
        })).catch(() => ({ status: 0, error: "模型请求超时" })),
        readResponse(await fetch("/api/perf-metrics/summary?hours=24", {
          method: "GET", credentials: "include", cache: "no-store", headers: requestHeaders,
          signal: AbortSignal.timeout(6000)
        })).catch(() => ({ status: 0, error: "健康度请求超时" }))
      ]);
      if (!modelsResponse.challenge && !modelsResponse.error &&
          modelsResponse.status >= 200 && modelsResponse.status < 300) {
        bridge.result.models = modelsResponse.data;
      }
      // 老版本 / 魔改站点没有这条路由：404 直接留 null，绝不影响 Key 与模型。
      if (!healthResponse.challenge && !healthResponse.error &&
          healthResponse.status >= 200 && healthResponse.status < 300) {
        bridge.result.health = healthResponse.data;
      } else if (!healthResponse.challenge) {
        // 魔改 NewAPI（x666 等）没有 perf-metrics，模型可用性改由「模型状态」
        // 增强模块下发：先要登录态的全量接口（普通账号会被站点按角色拒掉，
        // 回 200 + success:false，必须认成失败继续），再试站点开放的公开嵌入
        // 接口。同一套会话、同样软失败，两条都拿不到就保持 null。
        // 刻意用 try/catch 而非 .catch——语义与上面一致，且不额外占用
        // 「模型与健康度并发」那两处 catch 的失败预算。
        const statusPaths = [
          "/api/enhancements/model-status/status/all",
          "/api/enhancements/model-status/embed/status/all"
        ];
        for (const path of statusPaths) {
          if (bridge.result.health) break;
          try {
            const statusResponse = await readResponse(await fetch(path, {
              method: "GET", credentials: "include", cache: "no-store", headers: requestHeaders,
              signal: AbortSignal.timeout(6000)
            }));
            const payload = statusResponse.data;
            const looksLikeStatus = statusResponse.status >= 200 && statusResponse.status < 300 &&
              payload && payload.success !== false &&
              (Array.isArray(payload.data) || Array.isArray(payload.list) || Array.isArray(payload));
            if (looksLikeStatus) bridge.result.health = payload;
          } catch (_) {}
        }
      }
    } catch (error) {
      bridge.result = { ok: false, error: String(error && error.message || error) };
    }
  })();
  return pending;
})()"#
        .replace("__OPENHUB_USER_ID__", user_id)
        .replace("__OPENHUB_TOKEN_PATH__", token_path)
        .replace(
            "__OPENHUB_FETCH_MODELS__",
            if should_fetch_models { "true" } else { "false" },
        )
}

/// 桥接结果解析：按站点架构选择账号解析器。
///
/// Sub2API 的桥接响应结构与 NewAPI（`{success,data}`）不同（`{code,data}` 或扁平
/// 结构），套用 `parse_newapi_account` 会把合法响应判成“数据无效”。系统类型为空时
/// 沿用 NewAPI 语义。
pub(crate) fn parse_chrome_account_bridge_result_for(
    value: &str,
    system_type: &str,
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
    let raw = result
        .account
        .clone()
        .ok_or_else(|| "Chrome 返回结果缺少账号数据".to_string())?;
    let account = if is_sub2api(system_type) {
        // Sub2API 的 /api/v1/auth/me 返回 `{code,data:{...}}`，/v1/usage 返回扁平
        // 结构；两个解析器都试一遍，任一成功即用。
        parse_sub2api_account(&raw).or_else(|_| parse_sub2api_usage(&raw))
    } else {
        parse_newapi_account(&raw)
    }?;
    Ok((account, result))
}

/// Chrome Key 桥接结果：Key 列表、模型列表与（可选）全站模型健康度原文。
#[derive(Debug)]
pub(crate) struct ChromeKeyModelsBridgeResult {
    pub(crate) token_list: serde_json::Value,
    pub(crate) models: Option<serde_json::Value>,
    pub(crate) health: Option<serde_json::Value>,
}

pub(crate) fn parse_chrome_key_models_bridge_result(
    value: &str,
) -> Result<ChromeKeyModelsBridgeResult, String> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Payload {
        ok: bool,
        #[serde(default)]
        error: String,
        #[serde(default)]
        token_list: serde_json::Value,
        #[serde(default)]
        models: Option<serde_json::Value>,
        #[serde(default)]
        health: Option<serde_json::Value>,
    }
    let payload = serde_json::from_str::<Payload>(value)
        .map_err(|error| format!("Chrome 返回的 Key 数据格式无效：{error}"))?;
    if !payload.ok {
        return Err(if payload.error.is_empty() {
            "Chrome Key 请求失败".into()
        } else {
            payload.error
        });
    }
    if payload.token_list.is_null() {
        return Err("Chrome 返回结果缺少 Key 列表".into());
    }
    Ok(ChromeKeyModelsBridgeResult {
        token_list: payload.token_list,
        models: payload.models,
        health: payload.health,
    })
}

/// 单个站点 / 账号同步的硬性总超时：全过程（可达性探测 + 直连 + 静默/后台/
/// 可见三层兜底）合计超过 90 秒即强制失败并释放界面，避免整个弹窗卡住
/// 什么都干不了。此前的 60 秒会把 Cloudflare 盾站点的验证标签页（页面
/// 首载 + challenge 导航 + 桥接轮询）连坐掐断；三层预算合计仍为 45 秒，
/// 余量供前置探测与慢加载页面消耗，宁可早失败（失败计入浏览器兜底冷却），
/// 也不长时间占住同步流程。
const SITE_SYNC_TIMEOUT: Duration = Duration::from_secs(90);

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

/// 手动 Chrome 账号同步入口：统一 90 秒总超时、
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
        Err(_) => Err(format!(
            "账号同步超过 {} 秒，已强制终止",
            SITE_SYNC_TIMEOUT.as_secs()
        )),
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

/// 「白与黑」的手动账号同步：读 Chrome 会话调额度接口并写回账号行，不拉起 Chrome。
///
/// 站点额度接口 `/api/user/profile` 认的是浏览器登录会话（见 `fetch_baiheibai_account`），
/// 所以这里只从对应 Chrome Profile 的 Cookie 库里取登录态，不走桥接标签页。
/// 令牌只读不写：本函数不碰 `newapi_token` / `newapi_user_id` 两列。
async fn refresh_baiheibai_account_row(
    ctx: &Arc<AppContext>,
    site_id: &str,
    profile_id: &str,
    site_name: &str,
    account_label: &str,
    api_base_url: &str,
    cached_token: Option<String>,
    cached_uid: Option<String>,
    run_id: u64,
) -> Result<sync::ChromeSessionInfo, String> {
    let database = &*ctx.database;
    let runtime = &*ctx.proxy_runtime;
    let bus: EventBus = ctx.event_bus.clone();
    emit_chrome_account_progress(
        &bus,
        run_id,
        "account-request",
        "running",
        format!("正在读取 {site_name} · {account_label} 的登录会话与额度"),
    );
    let base_url = api_base_url.trim().to_string();
    if Url::parse(&base_url).is_err() {
        return Err("站点 API 地址无效".into());
    }
    let user_agent = sync::chrome_user_agent();
    // 额度接口认浏览器会话：从该 Profile 的 Cookie 库直接读登录态（不拉起 Chrome）。
    let cookie_header = {
        let home_dir = crate::context::home_dir().ok_or("无法定位用户目录")?;
        let profile_id_for_cookie = profile_id.to_string();
        let cookie_url = base_url.clone();
        spawn_blocking(move || {
            sync::read_chrome_cookie_header_from_home(&home_dir, &cookie_url, &profile_id_for_cookie)
        })
        .await
        .map_err(|error| format!("读取 Chrome Cookie 任务失败：{error}"))?
        .unwrap_or_default()
    };
    // 白与黑没有任何凭据（该 API 域没有 Cookie、也没配置站点令牌）：没有可请求的
    // 通道，这不是同步失败——不写任何凭据类错误，只清掉历史同步遗留的错误并
    // 原样返回当前缓存；引导交给账号行的「站点令牌」入口自己呈现。
    let has_token = cached_token
        .as_deref()
        .map(str::trim)
        .map_or(false, |value| !value.is_empty());
    if cookie_header.trim().is_empty() && !has_token {
        let connection = database.lock_conn()?;
        let _ = connection.execute(
            "UPDATE site_accounts
                SET sync_error = '', updated_at = CURRENT_TIMESTAMP
              WHERE site_id = ?1 AND profile_id = ?2 AND sync_error <> ''",
            params![site_id, profile_id],
        );
        let session = read_cached_usage_sites(&connection)?
            .into_iter()
            .find(|site| site.site_id == site_id)
            .and_then(|site| {
                site.sessions
                    .into_iter()
                    .find(|session| session.profile_id == profile_id)
            });
        emit_chrome_account_progress(
            &bus,
            run_id,
            "account-cache",
            "success",
            format!("{account_label} 没有该站点的登录凭据，已保留当前账号数据"),
        );
        return session.ok_or_else(|| "该账号没有可展示的会话数据".to_string());
    }
    let refresh = proxypool::with_account_proxy(
        database,
        runtime,
        site_id,
        profile_id,
        Duration::from_secs(12),
        3,
        "账号同步请求",
        move |client| {
            let base_url = base_url.clone();
            let token = cached_token.clone();
            let cookie = cookie_header.clone();
            let uid = cached_uid.clone();
            let user_agent = user_agent.clone();
            async move {
                fetch_baiheibai_account(
                    &client,
                    &base_url,
                    &cookie,
                    token.as_deref(),
                    uid.as_deref(),
                    &user_agent,
                )
                .await
            }
        },
    )
    .await?;
    let connection = database.lock_conn()?;
    let changed = connection
        .execute(
            // 只写额度与账号信息：令牌、用户 ID 与签到状态都不归这里管。
            "UPDATE site_accounts
                SET username = ?1, remaining = ?2, used = ?3, total = ?4, unit = ?5,
                    is_valid = ?6, sync_error = ?7, updated_at = CURRENT_TIMESTAMP
              WHERE site_id = ?8 AND profile_id = ?9",
            params![
                refresh.account.username,
                refresh.account.remaining,
                refresh.account.used,
                refresh.account.total,
                refresh.account.unit,
                refresh.is_valid as i64,
                refresh.sync_error,
                site_id,
                profile_id,
            ],
        )
        .map_err(|error| error.to_string())?;
    if changed == 0 {
        return Err(format!("没有更新到 {site_name} · {account_label} 的账号缓存"));
    }
    let session = read_cached_usage_sites(&connection)?
        .into_iter()
        .find(|site| site.site_id == site_id)
        .and_then(|site| {
            site.sessions
                .into_iter()
                .find(|session| session.profile_id == profile_id)
        })
        // 没凭据的账号（既无 Cookie 又无令牌）会被库里的会话视图过滤掉，
        // 这时直接把刷新阶段那句可操作的提示回给界面，不要报成“读取缓存失败”。
        .ok_or_else(|| {
            if refresh.sync_error.is_empty() {
                "账号缓存已更新，但该账号没有可展示的会话数据".to_string()
            } else {
                refresh.sync_error.clone()
            }
        })?;
    emit_chrome_account_progress(
        &bus,
        run_id,
        "account-cache",
        "success",
        format!("{account_label} 的额度已更新"),
    );
    Ok(session)
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
    // —— 白与黑：凭据只有用户手动保存的访问令牌，浏览器会话不是它的鉴权通道 ——
    // 直接走令牌通道取额度，不拉起 Chrome。
    if is_platform(&system_type, "baiheibai") {
        return refresh_baiheibai_account_row(
            ctx,
            &site_id,
            &profile_id,
            &site_name,
            &account_label,
            &api_base_url,
            cached_token.clone(),
            cached_uid.clone(),
            run_id,
        )
        .await;
    }
    // Sub2API 的账号同步同样走 Chrome 同源桥接：WAF 拦截直连时，只有页面上下文里的
    // fetch（带已通过的挑战 Cookie）能取到余额。
    let use_sub2api = is_sub2api(&system_type);
    if !is_newapi(&system_type) && !use_sub2api {
        return Err("当前仅对 NewAPI / Sub2API 账号提供 Chrome 同步".into());
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
    // 桥接会优先复用"任意 Chrome 窗口里已打开的站点页面"，而那条路径只按 URL
    // 匹配、不区分 Chrome Profile：同站点在另一个 Profile 里也登录着时，取回的
    // 是那个账号的令牌、余额与签到状态。NewAPI 前端不写 `localStorage.user` 的
    // 站点（例如 iMeagicAPI）在桥接脚本里无从核对，只能在这里用
    // `new_api_refresh` Cookie 的会话 ID 兜底：只有当返回令牌的 `sid` 与目标
    // Profile 自己的会话对不上时才判定串号。
    let expected_session = if use_refresh_auth {
        profile_newapi_session_id(&base_url, &profile_id, &home_dir).await
    } else {
        None
    };
    let local_values = local_match
        .as_ref()
        .filter(|item| item.error.is_empty())
        .map(|item| &item.values);
    let local_account_valid = local_values.is_some_and(|values| {
        if use_sub2api {
            parse_sub2api_local_account(values).is_ok()
                || checkin_origin_token(values).is_some()
        } else {
            parse_newapi_local_account(values).is_ok()
        }
    });
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
            .unwrap_or_else(|| {
                if use_sub2api {
                    "没有找到可用的 Sub2API 本地账号或登录令牌".into()
                } else {
                    "没有找到可用的 NewAPI 本地账号或刷新会话".into()
                }
            }));
    }
    emit_chrome_account_progress(
        &bus,
        run_id,
        "local-account",
        "success",
        format!(
            "{account_label} 认证策略：{}",
            if use_sub2api {
                "Sub2API 会话（Local Storage auth_token → Bearer）"
            } else if use_refresh_auth {
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
    // 令牌必须与目标 Profile 自己的登录会话一致：串号的缓存（复用到了别的
    // Chrome Profile 页面写进来的）会长期把那个账号的数据写到这一行，
    // 必须先剔除，再交给本 Profile 自己的浏览器路径重新获取。
    let cached_token_is_foreign = expected_session.as_deref().is_some_and(|expected| {
        cached_token
            .as_deref()
            .and_then(newapi_token_session_id)
            .is_some_and(|actual| !actual.eq_ignore_ascii_case(expected))
    });
    if cached_token_is_foreign {
        emit_chrome_account_progress(
            &bus,
            run_id,
            "token-cache",
            "info",
            format!("{account_label} 的缓存凭证属于另一个 Chrome 会话，已跳过并改用本 Profile 重新获取"),
        );
    }
    if use_refresh_auth && !cached_token_is_foreign {
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

    // 三阶段预算与 SITE_SYNC_TIMEOUT（90 秒）的关系：预算合计 45 秒
    // （12+13+20 / 10+13+22），给前置的会话读取、可达性探测、缓存令牌校验
    // （实测 8~15 秒）留出余量。此前总超时恰好 60 秒，任何前置开销都会让最后
    // 的可见验证被总超时连坐掐断——用户还没看到浏览器窗口流程就报失败。
    // 静默/后台失败要尽早让位给可见验证，可见验证也只保留有限窗口。
    let silent_timeout = if use_refresh_auth {
        Duration::from_secs(12)
    } else {
        Duration::from_secs(10)
    };
    let background_timeout = Duration::from_secs(13);
    let visible_timeout = if use_refresh_auth {
        Duration::from_secs(20)
    } else {
        Duration::from_secs(22)
    };

    // 每个阶段开始前检查强制停止：取消后立即失败，不再打开新的 Chrome 标签页。
    if is_site_account_sync_cancelled(run_id) {
        return Err("同步已被手动强制停止".into());
    }

    // 桥接脚本按架构分派：Sub2API 用 auth_token 会话端点，其余走 NewAPI 通道。
    let build_bridge_script = |marker: &str, allow_challenge_navigation: bool| -> String {
        if use_sub2api {
            chrome_sub2api_account_bridge_script(
                user_id.as_deref(),
                &current_month,
                marker,
                supports_checkin,
                allow_challenge_navigation,
                expected_session.as_deref(),
            )
        } else {
            chrome_account_bridge_script(
                user_id.as_deref(),
                &current_month,
                marker,
                use_refresh_auth,
                supports_checkin,
                allow_challenge_navigation,
                expected_session.as_deref(),
            )
        }
    };
    // 桥接结果解析同样按架构分派。
    let parse_bridge_result = |value: &str| {
        parse_chrome_account_bridge_result_for(value, &system_type)
    };

    // refresh 模式下即使没有 user_id 也允许静默请求（bridge 脚本不依赖 user_id）；
    // Sub2API 同理：登录令牌来自页面 Local Storage，不依赖 user_id。
    let can_silent =
        (user_id.is_some() || use_refresh_auth || use_sub2api) && resolved_account.is_none();
    if can_silent {
        let silent_marker = format!(
            "openhub-silent-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "系统时间异常")?
                .as_nanos()
        );
        let silent_javascript = build_bridge_script(&silent_marker, false);
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
            Ok(Ok(Some(value))) => match parse_bridge_result(&value) {
                Ok(parsed) => match bridge_session_mismatch(&parsed.1, expected_session.as_deref()) {
                    // 复用已打开标签页只按 URL 匹配、不区分 Chrome Profile，
                    // 这里跳过别人的会话，继续走 Profile 隔离的后台/可见路径。
                    Some(actual) => emit_chrome_account_progress(
                        &bus,
                        run_id,
                        "browser-bypass",
                        "success",
                        format!("现有 Chrome 页面属于另一个账号的会话（{actual}），已跳过以免串号"),
                    ),
                    None => {
                        emit_chrome_account_progress(
                            &bus,
                            run_id,
                            "browser-bypass",
                            "success",
                            "已通过现有 Chrome 页面静默获取账号数据",
                        );
                        resolved_account = Some(parsed);
                    }
                },
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
        let browser_url =
            chrome_account_bridge_url(&base_url, &checkin_url, &system_type, &marker)?;
        let javascript = build_bridge_script(&marker, true);
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
            Ok(Ok(value)) => match parse_bridge_result(&value) {
                Ok(parsed) => match bridge_session_mismatch(&parsed.1, expected_session.as_deref()) {
                    Some(actual) => emit_chrome_account_progress(
                        &bus,
                        run_id,
                        "browser-background",
                        "success",
                        format!("后台 Chrome 页面属于另一个账号的会话（{actual}），已跳过以免串号"),
                    ),
                    None => {
                        emit_chrome_account_progress(
                            &bus,
                            run_id,
                            "browser-background",
                            "success",
                            "后台 Chrome 已完成账号请求，临时标签已关闭",
                        );
                        resolved_account = Some(parsed);
                    }
                },
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
            let browser_url =
                chrome_account_bridge_url(&base_url, &checkin_url, &system_type, &marker)?;
            let javascript = build_bridge_script(&marker, true);
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
            let parsed = parse_bridge_result(&bridge_result)?;
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
    // 桥接复用了"任意 Chrome 窗口里已打开的站点页面"，同站点在另一个 Chrome
    // Profile 里也登录着时，拿回来的是那个账号的令牌与余额；NewAPI 前端不写
    // `localStorage.user` 的站点在桥接脚本里无法判串号。这里按会话 ID 兜底：
    // 能明确判定串号就拒绝写入，绝不把别人的账号数据落到这一行。
    if let Some(actual) = bridge_session_mismatch(&result, expected_session.as_deref()) {
        if let Ok(connection) = database.lock_conn() {
            // 清掉可能已经写错的缓存凭证，避免下一轮直接复用别人的访问令牌。
            // 手动设置的令牌（白与黑）不属于这条缓存：同步无权动它，串号也不清。
            if !access_token_is_user_owned(&system_type) {
                let _ = connection.execute(
                    "UPDATE site_accounts SET newapi_token = '', newapi_user_id = ''
                     WHERE site_id = ?1 AND profile_id = ?2",
                    params![site_id, profile_id],
                );
            }
        }
        return Err(format!(
            "{account_label} 的 Chrome 会话与本地 Profile 不一致（会话 ID {actual}，期望 {}）：已拒绝写入以免串号，请确认该 Chrome Profile 已登录此站点账号后重试",
            expected_session.as_deref().unwrap_or("")
        ));
    }

    emit_chrome_account_progress(
        &bus,
        run_id,
        "account-cache",
        "running",
        format!("正在更新 {account_label} 的 SQLite 账号缓存"),
    );

    let connection = database.lock_conn()?;
    // 令牌由用户维护的架构（白与黑）：令牌是用户从站点后台手动贴进来的，同步只读不写，
    // 桥接拿回什么都原样写回库里的旧值——这条 UPDATE 写的是整行。
    // Sub2API 同理：它的 auth_token 是浏览器登录会话令牌，不是 NewAPI 访问令牌，
    // 写进 newapi_token 列会被额度直连路径当成 API 凭证误用，因此同样只读不写。
    let read_only_token = access_token_is_user_owned(&system_type) || use_sub2api;
    let api_token = if read_only_token {
        connection
            .query_row(
                "SELECT newapi_token FROM site_accounts WHERE site_id = ?1 AND profile_id = ?2",
                params![site_id, profile_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .unwrap_or_default()
    } else {
        result.api_token.clone()
    };
    let api_user_id = if read_only_token {
        connection
            .query_row(
                "SELECT newapi_user_id FROM site_accounts WHERE site_id = ?1 AND profile_id = ?2",
                params![site_id, profile_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .unwrap_or_default()
    } else {
        result.user_id.clone()
    };
    // Sub2API 桥接只取余额，不查签到（签到由额度同步的 refresh_sub2api_checkin 负责）。
    // 这里不能用桥接返回的空签到覆盖缓存，否则手动同步一次会把「今日已签到」打回未签到。
    let (checkin_enabled, checked_in_today, checkin_error) = if use_sub2api {
        let cached = connection
            .query_row(
                "SELECT checkin_enabled, checked_in_today, checkin_error
                 FROM site_accounts WHERE site_id = ?1 AND profile_id = ?2",
                params![site_id, profile_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)? != 0,
                        row.get::<_, i64>(1)? != 0,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| error.to_string())?
            .unwrap_or((false, false, String::new()));
        // 桥接若真的带回了签到信息（未来的 Sub2API 支持），以桥接为准。
        if result.checkin_enabled || result.checked_in_today {
            (
                result.checkin_enabled,
                result.checked_in_today,
                result.checkin_error.clone(),
            )
        } else {
            cached
        }
    } else {
        (
            result.checkin_enabled,
            result.checked_in_today,
            result.checkin_error.clone(),
        )
    };
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
                checkin_enabled,
                checked_in_today,
                checkin_error,
                api_token,
                api_user_id,
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
    fn baiheibai_profile_response_maps_quota_to_usd() {
        // 白与黑的账号接口（/api/user/profile）响应形状与 NewAPI 的 /api/user/self 相同：
        // data.quota 是剩余额度、data.used_quota 是已用，单位 1/500000 美元
        // （站点 /api/status 的 quota_per_unit 也是 500000），所以两者共用一个解析器。
        let payload = serde_json::json!({
            "success": true,
            "message": "",
            "data": {
                "id": 42,
                "username": "hybw-user",
                "quota": 2_500_000,
                "used_quota": 500_000
            }
        });
        let account = parse_newapi_account(&payload).expect("应能解析白与黑的额度响应");
        assert_eq!(account.username, "hybw-user");
        assert_eq!(account.remaining, Some(5.0));
        assert_eq!(account.used, Some(1.0));
        assert_eq!(account.total, Some(6.0));
        assert_eq!(account.unit, "USD");
    }

    #[tokio::test]
    async fn baiheibai_without_credentials_is_a_silent_no_op() {
        // Cookie 与站点令牌都没有时没有任何可请求的通道：这不是同步失败，
        // 不得写任何错误（该分支不发起网络请求，客户端仅作占位）。
        let client = wreq::Client::builder()
            .timeout(Duration::from_secs(5))
            .no_proxy()
            .build()
            .expect("测试用 HTTP 客户端");
        let refresh = fetch_baiheibai_account(&client, "https://ai.hybgzs.com/", "", None, Some("42"), "ua")
            .await
            .expect("无凭据应返回空结果而非错误");
        assert!(!refresh.is_valid);
        assert!(!refresh.refreshed);
        assert_eq!(refresh.sync_error, "", "无凭据不得写成同步错误");
        assert_eq!(refresh.account.remaining, None);
        assert_eq!(refresh.newapi_user_id, "42", "用户 ID 原样带回");
    }

    #[test]
    fn reads_newapi_refresh_session_id_from_cookie_header() {
        let header = "new_api_has_session=1; new_api_refresh=04818b15-bc60-4707-acba-b9b8271cb91b.9f2c8d";
        assert_eq!(
            newapi_refresh_session_id(header).as_deref(),
            Some("04818b15-bc60-4707-acba-b9b8271cb91b")
        );
        // 缺 Cookie 或格式不符时无法判定，必须返回 None 而不是猜一个
        assert_eq!(newapi_refresh_session_id("new_api_has_session=1"), None);
        assert_eq!(newapi_refresh_session_id("new_api_refresh=abc.def"), None);
    }

    #[test]
    fn detects_cross_profile_account_by_jwt_session() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let sid = "04818b15-bc60-4707-acba-b9b8271cb91b";
        let payload =
            URL_SAFE_NO_PAD.encode(serde_json::json!({ "sub": "971", "sid": sid }).to_string());
        let token = format!("header.{payload}.signature");
        assert_eq!(newapi_token_session_id(&token).as_deref(), Some(sid));
        assert_eq!(newapi_token_session_id("opaque-token"), None);

        let result = ChromeBridgeAccountResult {
            ok: true,
            error: String::new(),
            account: None,
            checkin_enabled: false,
            checked_in_today: false,
            checkin_error: String::new(),
            api_token: token,
            user_id: "971".into(),
        };
        // 与目标 Profile 的会话一致：不判串号
        assert_eq!(bridge_session_mismatch(&result, Some(sid)), None);
        // 目标 Profile 是另一个会话：判定串号并给出实际会话 ID
        assert_eq!(
            bridge_session_mismatch(&result, Some("8c4418b0-8f03-4f6e-9a0d-1b2c3d4e5f60"))
                .as_deref(),
            Some(sid)
        );
        // 无法判定（拿不到目标会话）时不得误判
        assert_eq!(bridge_session_mismatch(&result, None), None);
    }

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
    fn parses_flat_sub2api_usage_shape_without_envelope() {
        // 线上 /v1/usage 的真实形状：余额直接挂顶层，没有 code 也没有 data.status。
        let value = serde_json::json!({
            "balance": 296.3339283,
            "daily_usage": [{ "date": "2026-09-07", "requests": 8 }],
            "isValid": true,
            "mode": "wallet",
            "planName": "Pro"
        });
        let account = parse_sub2api_usage(&value).expect("扁平结构必须能解析出余额");
        assert_eq!(account.remaining, Some(296.3339283));
        assert_eq!(account.unit, "USD");
    }

    #[test]
    fn rejects_sub2api_usage_without_balance_or_envelope() {
        // 既没有余额、也没有可识别的成功信封：仍然必须判错。
        let value = serde_json::json!({ "message": "something went wrong" });
        assert!(parse_sub2api_usage(&value).is_err());
    }

    #[test]
    fn rejects_sub2api_usage_explicit_failure_even_with_balance() {
        // 明确失败的信封不得因为带了余额字段就被当成成功。
        let value = serde_json::json!({ "code": 401, "message": "Token has expired" });
        assert!(parse_sub2api_usage(&value).is_err());
        let value = serde_json::json!({ "success": false, "balance": 1.0 });
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
    fn newapi2_access_token_rejection_is_recognized() {
        // Pomelo（newapi2）实测文案：失效令牌返回 401 + "access token 无效"。
        let error = "账号接口 HTTP 401：无权进行此操作，access token 无效";
        assert!(access_token_was_rejected(error));
        assert!(requires_chrome_fallback(error));
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
            chrome_account_bridge_script(Some("42"), "2026-08", "openhub-test", false, true, true, None);
        assert!(!script.contains("credentials: \"omit\""));
        assert!(!script.contains("credentials: useSessionCookies"));
        assert!(script.contains("method: \"GET\", credentials: \"include\""));
        assert!(script.contains("method: \"POST\", credentials: \"include\""));
    }

    #[test]
    fn chrome_account_bridge_url_rewrites_origin_only_checkin_to_console() {
        let base = Url::parse("https://anyrouter.top/").unwrap();
        let url =
            chrome_account_bridge_url(&base, "https://anyrouter.top/", "new-api", "openhub-sync-1")
                .unwrap();
        assert_eq!(url.as_str(), "https://anyrouter.top/console#openhub-sync-1");
        let slashless = chrome_account_bridge_url(
            &base,
            "https://anyrouter.top",
            "new-api",
            "openhub-background-2",
        )
        .unwrap();
        assert_eq!(
            slashless.as_str(),
            "https://anyrouter.top/console#openhub-background-2"
        );
        let empty = chrome_account_bridge_url(&base, "", "new-api", "openhub-sync-3").unwrap();
        assert_eq!(
            empty.as_str(),
            "https://anyrouter.top/console/personal#openhub-sync-3"
        );
        let personal = chrome_account_bridge_url(
            &base,
            "https://anyrouter.top/console/personal",
            "new-api",
            "openhub-sync-4",
        )
        .unwrap();
        assert_eq!(
            personal.as_str(),
            "https://anyrouter.top/console/personal#openhub-sync-4"
        );
        let foreign = chrome_account_bridge_url(
            &base,
            "https://example.com/path",
            "new-api",
            "openhub-sync-5",
        )
        .unwrap();
        assert_eq!(
            foreign.as_str(),
            "https://anyrouter.top/console/personal#openhub-sync-5"
        );
    }

    /// 未知平台的兜底路径退回站点根（path 为空时再试 /console），
    /// 不再硬塞 NewAPI 的 /console/personal —— 有的站点没有这个地址。
    #[test]
    fn chrome_account_bridge_url_unknown_platform_falls_back_to_root() {
        let base = Url::parse("https://example.com/").unwrap();
        // 空签到地址 → 未知平台退回根路径 → 根路径会被首页跳转丢 marker → 再改试 /console
        let empty = chrome_account_bridge_url(&base, "", "openai", "m-1").unwrap();
        assert_eq!(empty.as_str(), "https://example.com/console#m-1");
        let root =
            chrome_account_bridge_url(&base, "https://example.com", "openai", "m-2").unwrap();
        assert_eq!(root.as_str(), "https://example.com/console#m-2");
        let sub2api = chrome_account_bridge_url(&base, "", "sub2api", "m-3").unwrap();
        assert_eq!(sub2api.as_str(), "https://example.com/dashboard#m-3");
    }

    #[test]
    fn key_models_bridge_script_fetches_token_and_models() {
        let script = chrome_key_models_bridge_script(true, "99", "/api/token/?p=1&size=20");
        // 同源凭证必须带上，cf_clearance 才能生效
        assert!(script.contains("credentials: \"include\""));
        assert!(script.contains("/api/token/?p=1&size=20"));
        assert!(script.contains("/v1/models"));
        assert!(script.contains("__OPENHUB_PENDING__"));
        // 传统 new-api 会话必须带 New-Api-User 头，否则 401「未提供 New-Api-User」
        assert!(script.contains("New-Api-User"));
        // 关闭模型拉取时不得包含 /v1/models 请求
        let script_no_models = chrome_key_models_bridge_script(false, "", "/api/token/?p=1&size=20");
        assert!(script_no_models.contains("if (!false) return;"));
        // 空用户 ID 也要是合法 JS（占位符替换后仍可为空串）
        assert!(script_no_models.contains("const legacyUserId = \"\";"));
        // 有缓存 user_id 时以字面量注入（非空即写入 New-Api-User 头）
        assert!(script.contains("requestHeaders[\"New-Api-User\"] = legacyUserId;"));
        // 「白与黑」用自己的 Key 列表路径（page/size/keyword/order），不得回落到 NewAPI 参数
        let baiheibai_script = chrome_key_models_bridge_script(
            true,
            "99",
            "/api/token/?page=1&size=10&keyword=&order=-id",
        );
        assert!(baiheibai_script.contains("/api/token/?page=1&size=10&keyword=&order=-id"));
        assert!(!baiheibai_script.contains("?p=1&size=20"));
    }

    #[test]
    fn parse_chrome_key_models_bridge_result_rejects_errors() {
        let err = parse_chrome_key_models_bridge_result(
            r#"{"ok":false,"error":"Cloudflare 验证仍需要浏览器交互"}"#,
        )
        .unwrap_err();
        assert!(err.contains("Cloudflare"));
        let missing =
            parse_chrome_key_models_bridge_result(r#"{"ok":true,"tokenList":null,"models":null}"#)
                .unwrap_err();
        assert!(missing.contains("缺少 Key 列表"));
        let ok = parse_chrome_key_models_bridge_result(
            r#"{"ok":true,"tokenList":{"data":[]},"models":{"data":[{"id":"gpt-x"}]}}"#,
        )
        .expect("合法结果应解析成功");
        assert!(ok.models.is_some());
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
    fn bridge_result_tolerates_non_string_error_fields() {
        // 线上实测：站点把 message 写成对象时，严格的 String 反序列化会报
        // invalid type: map, expected a string，整次同步报废。这里必须宽松收敛。
        let raw = serde_json::json!({
            "ok": false,
            "error": { "message": "Unauthorized, not logged in", "code": 401 }
        })
        .to_string();
        let result: ChromeBridgeAccountResult =
            serde_json::from_str(&raw).expect("对象型 error 不应导致解析失败");
        assert!(!result.ok);
        assert_eq!(result.error, "Unauthorized, not logged in");

        // 数字与布尔同样收敛成字符串，而不是报错。
        let numeric = serde_json::json!({ "ok": true, "userId": 12345, "apiToken": true }).to_string();
        let result: ChromeBridgeAccountResult = serde_json::from_str(&numeric).unwrap();
        assert_eq!(result.user_id, "12345");
        assert_eq!(result.api_token, "true");
    }

    #[test]
    fn local_day_unix_range_spans_exactly_one_day() {
        let (start, end) = local_day_unix_range();
        // 区间长度固定为 86400 秒（00:00:00 - 00:00:00）。
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
            chrome_account_bridge_script(Some("42"), "2026-08", "openhub-test", false, true, true, None);
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

    #[test]
    fn sub2api_bridge_script_uses_local_storage_token_and_session_endpoint() {
        let script = chrome_sub2api_account_bridge_script(
            None,
            "2026-08",
            "openhub-test",
            false,
            true,
            None,
        );
        // 登录令牌只来自页面 Local Storage，且必须走同源会话端点。
        assert!(script.contains("const useSub2Api = true"));
        assert!(script.contains("localStorage.getItem(\"auth_token\")"));
        assert!(script.contains("/api/v1/auth/me"));
        // Sub2API 分支必须在 NewAPI 通道之前返回：模板虽共用，但运行期不会走到
        // `/api/user/self`。用分支顺序断言，而不是断言脚本里没有该字符串。
        let sub2api_branch = script.find("if (useSub2Api) {").expect("缺少 Sub2API 分支");
        let refresh_branch = script.find("if (useRefreshAuth) {").expect("缺少刷新分支");
        assert!(
            sub2api_branch < refresh_branch,
            "Sub2API 分支必须先于 NewAPI 刷新分支执行"
        );
    }

    #[test]
    fn newapi_bridge_script_keeps_sub2api_mode_off() {
        let script =
            chrome_account_bridge_script(Some("42"), "2026-08", "openhub-test", false, true, true, None);
        assert!(script.contains("const useSub2Api = false"));
        assert!(script.contains("/api/user/self"));
    }

    #[test]
    fn bridge_result_parser_dispatches_by_system_type() {
        // Sub2API 的 `{code,data}` 信封被 NewAPI 解析器判为无效，必须按架构分派。
        let raw = serde_json::json!({
            "ok": true,
            "account": {
                "code": 0,
                "data": { "username": "alice", "remaining": 7.5, "unit": "USD" }
            }
        })
        .to_string();
        let (account, _) =
            parse_chrome_account_bridge_result_for(&raw, "sub2api").expect("Sub2API 信封应能解析");
        assert_eq!(account.remaining, Some(7.5));
        // 同一份响应交给 NewAPI 语义则解析失败，证明分派确实生效。
        assert!(parse_chrome_account_bridge_result_for(&raw, "").is_err());
    }
}
