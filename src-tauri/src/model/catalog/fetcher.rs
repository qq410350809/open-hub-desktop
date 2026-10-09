use crate::context::{home_dir, spawn, spawn_blocking, AppContext, Managed};
use crate::db::*;
use crate::models::*;
use crate::proxypool;
use crate::site::library::{
    is_explicit_unknown, is_newapi, is_newapi_refresh, is_platform, is_sub2api, uses_access_token,
};
use crate::site::sync;
use crate::site::sync::*;
use futures_util::StreamExt;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn get_system_fonts() -> Vec<String> {
    #[cfg(feature = "desktop")]
    {
        let mut fonts = Vec::new();
        let source = font_kit::source::SystemSource::new();
        if let Ok(families) = source.all_families() {
            for family in families {
                fonts.push(family);
            }
        }
        fonts.sort();
        fonts.dedup();
        fonts
    }
    #[cfg(not(feature = "desktop"))]
    {
        Vec::new()
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SiteModelsResult {
    pub(crate) models: Vec<SiteModelItem>,
    pub(crate) source: String,
    pub(crate) keys: Vec<String>,
    #[serde(default)]
    pub(crate) key_groups: HashMap<String, String>,
    /// 每个 Key 对应的模型列表（逐 Key 查询 /v1/models 的结果）。
    /// Key 为去前缀的原始值，与 `keys` 字段一致。
    #[serde(default)]
    pub(crate) key_models: HashMap<String, Vec<SiteModelItem>>,
    /// 逐账号同步过程中收集的失败原因。Key/模型为空时它就是同步失败的
    /// 真实原因，必须随结果带回并落库——此前被丢弃导致界面只显示
    /// “0 个 Key”而没有任何报错。
    #[serde(default)]
    pub(crate) errors: Vec<String>,
    /// 实际产出这批 Key 的账号（Chrome Profile）。不带 profile_id 的站点级
    /// 请求仍会借有效账号的会话抓 Key，调用方落库时按它归属，避免 Key
    /// 被挂到 profile_id='' 的幽灵行、与真实账号脱钩。
    #[serde(default)]
    pub(crate) profile_id: String,
    /// 全站模型健康度（模型 ID → 健康度），来自 NewAPI 的
    /// `/api/perf-metrics/summary` 或「模型状态」增强模块
    /// `/api/enhancements/model-status/status/all`。站点两条路由都没有时为空。
    #[serde(default)]
    pub(crate) model_health: HashMap<String, SiteModelHealth>,
}

pub(crate) fn json_array_at<'a>(
    value: &'a serde_json::Value,
    pointers: &[&str],
) -> Option<&'a Vec<serde_json::Value>> {
    pointers
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(serde_json::Value::as_array))
}

/// Key / 令牌列表数组的候选位置。
///
/// NewAPI 系把列表放在 `data.items`，但「白与黑」等自定义后端常见分页形状
/// （GoFrame 的 `data.list`、DRF 的 `records`、`rows`…）也必须覆盖：形状不认
/// 会被误报成「没有返回可用令牌 ID」，用户与日志都无从判断真实原因。
pub(crate) const API_KEY_ITEM_POINTERS: &[&str] = &[
    "",
    "/data",
    "/data/items",
    "/data/keys",
    "/keys",
    "/items",
    "/result/items",
    "/result/keys",
    // 常见分页/自定义后端的列表位置
    "/data/list",
    "/data/records",
    "/data/rows",
    "/data/data",
    "/list",
    "/records",
    "/rows",
    "/result/list",
    "/result/data",
];

/// 压缩 JSON 原文片段，用于「响应形状无法识别」类错误的自诊断。
pub(crate) fn compact_json_snippet(value: &serde_json::Value, max_chars: usize) -> String {
    let text = serde_json::to_string(value).unwrap_or_default();
    if text.chars().count() <= max_chars {
        return text;
    }
    let mut truncated: String = text.chars().take(max_chars).collect();
    truncated.push('…');
    truncated
}

pub(crate) fn parse_site_models(value: &serde_json::Value) -> Vec<SiteModelItem> {
    let Some(items) = json_array_at(
        value,
        &[
            "",
            "/data",
            "/data/items",
            "/data/models",
            "/models",
            "/items",
            "/result/data",
            "/result/models",
        ],
    ) else {
        return Vec::new();
    };
    let mut models = items
        .iter()
        .filter_map(|item| {
            let (id, owned_by) = match item {
                serde_json::Value::String(id) => (id.trim().to_string(), None),
                serde_json::Value::Object(_) => (
                    json_string(item, &["/model_name", "/id", "/name", "/model", "/slug"]),
                    Some(json_string(
                        item,
                        &["/owner", "/owned_by", "/ownedBy", "/vendor"],
                    ))
                    .filter(|value| !value.is_empty()),
                ),
                _ => return None,
            };
            (!id.is_empty()).then_some(SiteModelItem { id, owned_by })
        })
        .collect::<Vec<_>>();
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.dedup_by(|left, right| left.id == right.id);
    models
}

pub(crate) fn api_key_is_enabled(item: &serde_json::Value) -> bool {
    if item.get("enabled").and_then(json_boolish) == Some(false)
        || item.get("is_active").and_then(json_boolish) == Some(false)
    {
        return false;
    }
    if let Some(status) = item.get("status") {
        match status {
            serde_json::Value::Bool(false) => return false,
            serde_json::Value::Number(number) if number.as_i64() == Some(0) => return false,
            serde_json::Value::String(value)
                if matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "disabled" | "inactive" | "expired" | "revoked" | "0" | "false"
                ) =>
            {
                return false;
            }
            _ => {}
        }
    }
    let expires_at = ["/expired_time", "/expires_at", "/expire_at", "/expiration"]
        .iter()
        .find_map(|pointer| json_number(item, pointer));
    if let Some(expires_at) = expires_at {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        if expires_at > 0.0 && expires_at < now {
            return false;
        }
    }
    true
}

pub(crate) fn normalize_api_key_value(value: &str) -> Option<String> {
    let value = value
        .strip_prefix("Bearer ")
        .unwrap_or(value)
        .trim()
        .to_string();
    (value.len() >= 8
        && !value.chars().any(char::is_whitespace)
        && !value.contains('*')
        && !value.contains("...")
        && !value.contains('…'))
    .then_some(value)
}

pub(crate) fn parse_api_key_entries(value: &serde_json::Value) -> Vec<(String, String)> {
    let Some(items) = json_array_at(value, API_KEY_ITEM_POINTERS) else {
        return Vec::new();
    };
    let mut entries = HashMap::<String, String>::new();
    for item in items.iter().filter(|item| api_key_is_enabled(item)) {
        let (value, prefix, group) = match item {
            serde_json::Value::String(value) => {
                (value.trim().to_string(), String::new(), String::new())
            }
            serde_json::Value::Object(_) => (
                json_string(
                    item,
                    &[
                        "/key",
                        "/api_key",
                        "/apiKey",
                        "/plain_key",
                        "/plainKey",
                        "/secret_key",
                        "/secretKey",
                        "/token",
                        "/secret",
                        "/value",
                    ],
                ),
                json_string(item, &["/key_prefix", "/keyPrefix", "/prefix"]),
                json_string(
                    item,
                    &[
                        // Sub2API 的 group 是对象（如 {"id":..,"name":..}），标识取其中的 name
                        "/group/name",
                        "/group",
                        "/group_name",
                        "/groupName",
                        "/token_group",
                        "/tokenGroup",
                        "/name",
                        "/token_name",
                        "/tokenName",
                    ],
                ),
            ),
            _ => continue,
        };
        let Some(value) = normalize_api_key_value(&value) else {
            continue;
        };
        let insert = |entries: &mut HashMap<String, String>, key: String, group: &str| {
            let current = entries.entry(key).or_default();
            if current.is_empty() && !group.is_empty() {
                *current = group.to_string();
            }
        };
        insert(&mut entries, value.clone(), &group);
        if !prefix.is_empty() && !value.starts_with(&prefix) {
            insert(&mut entries, format!("{prefix}{value}"), &group);
        }
    }
    let mut entries = entries.into_iter().collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    entries
}

pub(crate) fn parse_api_keys(value: &serde_json::Value) -> Vec<String> {
    parse_api_key_entries(value)
        .into_iter()
        .map(|(key, _)| key)
        .collect()
}

pub(crate) fn parse_api_key_groups(value: &serde_json::Value) -> HashMap<String, String> {
    let mut groups = parse_api_key_entries(value)
        .into_iter()
        .filter(|(_, group)| !group.is_empty())
        .collect::<HashMap<_, _>>();
    for pointer in [
        "/keyGroups",
        "/key_groups",
        "/data/keyGroups",
        "/data/key_groups",
    ] {
        let Some(object) = value
            .pointer(pointer)
            .and_then(serde_json::Value::as_object)
        else {
            continue;
        };
        merge_api_key_groups(
            &mut groups,
            object.iter().filter_map(|(key, group)| {
                let group = group.as_str()?.trim();
                (!key.trim().is_empty() && !group.is_empty())
                    .then_some((key.trim().to_string(), group.to_string()))
            }),
        );
    }
    groups
}

pub(crate) fn parse_newapi_token_ids(value: &serde_json::Value) -> Vec<String> {
    let Some(items) = json_array_at(value, API_KEY_ITEM_POINTERS) else {
        return Vec::new();
    };
    let mut ids = items
        .iter()
        .filter(|item| api_key_is_enabled(item))
        .filter_map(|item| {
            let id = json_string(item, &["/id", "/token_id", "/tokenId", "/key_id", "/keyId", "/uuid"]);
            (!id.is_empty()
                && id.len() <= 64
                && id.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
                }))
                .then_some(id)
        })
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

pub(crate) fn parse_newapi_token_groups(value: &serde_json::Value) -> HashMap<String, String> {
    let Some(items) = json_array_at(value, API_KEY_ITEM_POINTERS) else {
        return HashMap::new();
    };
    items
        .iter()
        .filter(|item| api_key_is_enabled(item))
        .filter_map(|item| {
            let id = json_string(item, &["/id", "/token_id", "/tokenId"]);
            let group = json_string(
                item,
                &[
                    "/group",
                    "/group_name",
                    "/groupName",
                    "/token_group",
                    "/tokenGroup",
                    "/name",
                    "/token_name",
                    "/tokenName",
                ],
            );
            (!id.is_empty() && !group.is_empty()).then_some((id, group))
        })
        .collect()
}

pub(crate) fn parse_revealed_api_key(value: &serde_json::Value) -> Option<String> {
    let key = json_string(
        value,
        &[
            "/data/key",
            "/data/api_key",
            "/data/apiKey",
            "/data/secret_key",
            "/data/secretKey",
            "/data",
            "/key",
            "/api_key",
            "/apiKey",
            "/secret_key",
            "/secretKey",
        ],
    );
    normalize_api_key_value(&key)
}

pub(crate) async fn reveal_newapi_keys(
    client: &wreq::Client,
    base_url: &Url,
    auth: &NewApiAuth,
    user_agent: &str,
    token_list: &serde_json::Value,
) -> Result<(Vec<String>, HashMap<String, String>), String> {
    let mut keys = parse_api_keys(token_list);
    let mut key_groups = parse_api_key_groups(token_list);
    if !keys.is_empty() {
        return Ok((keys, key_groups));
    }
    let token_ids = parse_newapi_token_ids(token_list);
    let token_groups = parse_newapi_token_groups(token_list);
    if token_ids.is_empty() {
        // 把「没有可用令牌 ID」拆成三种可诊断的原因，避免站点拒绝、确实没有
        // Key、响应形状不认识全被同一句话吞掉（用户与日志都无从判断下一步）。
        if token_list.get("success").and_then(json_boolish) == Some(false) {
            return Err(api_error_message(token_list, "/api/token 请求被站点拒绝"));
        }
        if let Some(items) = json_array_at(token_list, API_KEY_ITEM_POINTERS) {
            if items.is_empty() {
                return Err("该账号没有可用的 API Key（令牌列表为空）".into());
            }
            return Err(format!(
                "/api/token 返回了 {} 条记录，但没有可用的令牌 ID（可能全部禁用/过期，或缺少 id 字段）",
                items.len()
            ));
        }
        let message = api_error_message(token_list, "");
        let snippet = compact_json_snippet(token_list, 200);
        return Err(if message.is_empty() {
            format!("/api/token 响应形状无法识别：{snippet}")
        } else {
            format!("/api/token 响应形状无法识别（站点返回：{message}）：{snippet}")
        });
    }
    let mut errors = Vec::new();
    for token_id in token_ids {
        let endpoint = base_url
            .join(&format!("/api/token/{token_id}/key"))
            .map_err(|_| "无法生成完整 Key 接口地址".to_string())?;
        let request = apply_newapi_auth(
            chrome_request_headers(client.post(endpoint), base_url.as_str(), user_agent),
            auth,
        );
        match request_json(request, "NewAPI 完整 Key 接口").await {
            Ok(value) => {
                if let Some(key) = parse_revealed_api_key(&value) {
                    if let Some(group) = token_groups
                        .get(&token_id)
                        .filter(|group| !group.is_empty())
                    {
                        key_groups.insert(key.clone(), group.clone());
                    }
                    keys.push(key);
                } else {
                    errors.push(format!("令牌 {token_id} 没有返回完整 Key"));
                }
            }
            Err(error) => errors.push(format!("令牌 {token_id}：{error}")),
        }
    }
    keys.sort();
    keys.dedup();
    if keys.is_empty() {
        Err(errors
            .last()
            .cloned()
            .unwrap_or_else(|| "没有取得可用的完整 Key".into()))
    } else {
        Ok((keys, key_groups))
    }
}

/// Cloudflare 盾站点的 Chrome 同源兜底：在站点页面上下文里拉 Key 列表
/// （必要时揭示完整 Key）与模型列表。仅在直连被盾拦截时调用——
/// 桥接会打开/复用 Chrome 标签页，失败路径与账号同步一致。
///
/// `user_id` 为该账号缓存的 NewAPI 用户 ID，供桥接脚本核对页面身份防串号；
/// 拿不到时传空（脚本读不到 user 键的新版前端同样按 Cookie 罐裁决身份）。
pub(crate) async fn chrome_bridge_fetch_keys_models(
    database: &Database,
    base_url: &Url,
    system_type: &str,
    profile_id: &str,
    user_id: &str,
    site_id: Option<&str>,
) -> Result<SiteModelsResult, String> {
    let should_fetch_models = true;
    // 「白与黑」的 Key 列表参数与 NewAPI 系不同（page/size/keyword/order），
    // 页面内 fetch 必须用它自己的路径，否则站点按非法参数拒绝。
    let token_path = if is_platform(system_type, "baiheibai") {
        "/api/token/?page=1&size=10&keyword=&order=-id"
    } else {
        "/api/token/?p=1&size=20"
    };
    let javascript = sync::chrome_key_models_bridge_script(should_fetch_models, user_id, token_path);
    let marker = format!(
        "openhub-sync-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "系统时间异常")?
            .as_nanos()
    );
    // 桥接页按平台分派（NewAPI → /console/personal，Sub2API → /dashboard），
    // 其余平台退回站点根路径 —— 有的站点根本没有 /console/personal。
    let console_path = crate::site::library::console_page_path(system_type).unwrap_or("/");
    let browser_url = base_url
        .join(console_path)
        .map_err(|_| "无法生成 Chrome 验证地址")?;
    let base_str = base_url.to_string();
    let user_id_owned = user_id.to_string();
    let profile_id_owned = profile_id.to_string();

    // 静默尝试已有标签（10s）→ 可见标签兜底（25s）。Key 同步的脚本不导航、
    // 只 fetch，无需账号同步那样的后台阶段；预算计入 fetch_site_models_json
    // 的 90s 总超时。静默阶段未命中时转入可见标签；静默阶段报错时只有
    // 阻断性配置问题（JS 自动化开关 / macOS 授权）直接失败，其余与账号
    // 同步一致——降级打开可见标签再试，而不是整个兜底立即放弃。
    let bridge_result = {
        let javascript = javascript.clone();
        let browser_url = browser_url.to_string();
        let base_for_silent = base_str.clone();
        let marker_for_silent = marker.clone();
        let user_id_for_silent = user_id_owned.clone();
        let profile_id_for_bridge = profile_id_owned.clone();
        spawn_blocking(move || {
            // 先扫一遍已打开的同源标签（含遗留桥接标签）做静默注入
            let silent = sync::run_javascript_in_existing_chrome_tab(
                &base_for_silent,
                &javascript,
                Duration::from_secs(10),
            );
            let visible = move || {
                sync::run_javascript_in_chrome_profile(
                    &browser_url,
                    &profile_id_for_bridge,
                    &marker_for_silent,
                    &javascript,
                    Duration::from_secs(25),
                    None,
                    !user_id_for_silent.is_empty(),
                )
            };
            match silent {
                Ok(Some(value)) => Ok(value),
                Ok(None) => visible(),
                Err(silent_error) if sync::is_blocking_chrome_automation_error(&silent_error) => {
                    Err(silent_error)
                }
                Err(_) => visible(),
            }
        })
        .await
        .map_err(|error| format!("Chrome 兜底任务失败：{error}"))?
    }?;
    let parsed = sync::parse_chrome_key_models_bridge_result(&bridge_result)?;

    // 解析 Key 列表：明文 Key 直出；否则用 token id 走揭示接口
    // （页面已过盾，但揭示请求仍从 Rust 直连发出——部分站点仅拦首页，
    // 失败则把错误带回给上层）
    let mut keys = parse_api_keys(&parsed.token_list);
    let mut key_groups = parse_api_key_groups(&parsed.token_list);
    let mut errors = Vec::new();
    if keys.is_empty() {
        let client = build_site_http_client(database, Duration::from_secs(10), 3, "站点模型请求")?;
        match reveal_newapi_keys(
            &client,
            base_url,
            &NewApiAuth::Legacy {
                cookie_header: String::new(),
                user_id: user_id_owned.clone(),
            },
            &sync::chrome_user_agent(),
            &parsed.token_list,
        )
        .await
        {
            Ok((revealed, groups)) => {
                keys = revealed;
                key_groups = groups;
            }
            Err(error) => errors.push(format!("Chrome 已取到令牌列表但揭示 Key 失败：{error}")),
        }
    }

    // 模型列表：桥接已带回则直接解析；否则用拿到的 Key 直连重试
    let mut key_models: HashMap<String, Vec<SiteModelItem>> = HashMap::new();
    let mut all_models: Vec<SiteModelItem> = Vec::new();
    if let Some(models_json) = &parsed.models {
        all_models = parse_site_models(models_json);
    }
    if all_models.is_empty() && !keys.is_empty() {
        let client = build_site_http_client(database, Duration::from_secs(10), 3, "站点模型请求")?;
        if let Ok(result) = fetch_models_with_keys(
            &client,
            base_url,
            keys.clone(),
            keys.clone(),
            key_groups.clone(),
            &sync::chrome_user_agent(),
            "newapi-key",
            (!user_id_owned.is_empty()).then_some(user_id_owned.as_str()),
        )
        .await
        {
            key_models = result.key_models;
            all_models = result.models;
        }
    }

    // 全站模型健康度：盾站点直连必被 403，只能靠页面内同源 fetch 带回。
    // 桥接没带回（老站点两条路由都没有）就留空，不算错误。
    let model_health = parsed
        .health
        .as_ref()
        .map(|value| parse_site_model_health(value))
        .unwrap_or_default();

    // 与账号同步的落库口径对齐：把 Chrome 里核对过的 Key/模型写进缓存，
    // 并标记 Key 的真实归属（本次桥接使用的 Chrome Profile）。
    let result = SiteModelsResult {
        models: all_models,
        source: "newapi-key".into(),
        keys: keys.clone(),
        key_groups: key_groups.clone(),
        key_models,
        errors,
        profile_id: profile_id.to_string(),
        model_health,
    };
    if let Some(site_id) = site_id {
        let account = SiteModelCacheAccount {
            profile_id: profile_id.to_string(),
            profile_name: String::new(),
            account_name: String::new(),
            username: String::new(),
            keys: result.keys.clone(),
            key_groups: result.key_groups.clone(),
            key_models: result.key_models.clone(),
            model_health: result.model_health.clone(),
            error: String::new(),
        };
        if let Err(error) = save_site_model_cache(database, site_id, &account, Some(&result), false)
        {
            return Err(error);
        }
    }
    if result.keys.is_empty() && result.models.is_empty() {
        return Err(result
            .errors
            .last()
            .cloned()
            .unwrap_or_else(|| "Chrome 兜底未取得任何 Key 或模型".into()));
    }
    Ok(result)
}

pub(crate) async fn fetch_models_with_keys(
    client: &wreq::Client,
    base_url: &Url,
    keys: Vec<String>,
    visible_keys: Vec<String>,
    visible_key_groups: HashMap<String, String>,
    user_agent: &str,
    source: &str,
    newapi_user_id: Option<&str>,
) -> Result<SiteModelsResult, String> {
    if keys.is_empty() {
        return Ok(SiteModelsResult {
            models: Vec::new(),
            source: source.into(),
            keys: visible_keys,
            key_groups: visible_key_groups,
            key_models: HashMap::new(),
            errors: Vec::new(),
            profile_id: String::new(),
            model_health: HashMap::new(),
        });
    }
    let models_url = base_url
        .join("/v1/models")
        .map_err(|_| "无法生成 /v1/models 地址".to_string())?;
    let mut errors = Vec::new();
    let mut key_models: HashMap<String, Vec<SiteModelItem>> = HashMap::new();
    for key in &keys {
        let mut candidates = vec![key.clone()];
        if !key.starts_with("sk-") {
            candidates.push(format!("sk-{key}"));
        }
        for candidate in candidates {
            let mut request = chrome_request_headers(
                client.get(models_url.clone()),
                base_url.as_str(),
                user_agent,
            )
            .bearer_auth(&candidate);
            if let Some(user_id) = newapi_user_id.filter(|value| !value.trim().is_empty()) {
                request = request.header("new-api-user", user_id);
            }
            match request_json(request, "模型接口").await {
                Ok(value) => {
                    let models = parse_site_models(&value);
                    if !models.is_empty() {
                        key_models.insert(key.clone(), models);
                        break;
                    }
                    errors.push("模型接口返回空列表".to_string());
                }
                Err(error) => errors.push(error),
            }
        }
    }
    if key_models.is_empty() {
        return Err(errors
            .last()
            .cloned()
            .unwrap_or_else(|| "现有 Key 均无法获取模型".into()));
    }
    // 合并所有 Key 的模型作为整站模型列表（去重），保持向后兼容。
    let mut all_models: Vec<SiteModelItem> = Vec::new();
    for models in key_models.values() {
        for model in models {
            if !all_models.iter().any(|item| item.id == model.id) {
                all_models.push(model.clone());
            }
        }
    }
    all_models.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(SiteModelsResult {
        models: all_models,
        source: source.into(),
        keys: visible_keys,
        key_groups: visible_key_groups,
        key_models,
        errors,
        profile_id: String::new(),
        model_health: HashMap::new(),
    })
}

/// 模型健康度的统计窗口小时数。取 24 与 NewAPI 前端默认一致：窗口越长，
/// 单次聚合越重，而界面只需要「近一天」这一个粒度。
pub(crate) const PERF_METRICS_WINDOW_HOURS: i64 = 24;

/// 拉取健康度的独立超时。刻意远小于站点的 90s 同步总预算：它是补偿信息，
/// 慢站点上宁可这一轮没有标签，也不能把 Key/模型同步一起拖到超时失败。
const PERF_METRICS_TIMEOUT: Duration = Duration::from_secs(8);

/// 解析 NewAPI `/api/perf-metrics/summary` 的响应。
///
/// 响应形如 `{ success, data: { window_start, window_end, models: [
/// { model_name, avg_latency_ms, success_rate, avg_tps, recent_success_series } ] } }`。
///
/// **单位**：`success_rate` 是 0~100 的百分数（上游 `successRate()` 直接返回
/// `successCount / requestCount * 100`），这里统一归一化到 0~1。按 0~1 解读
/// 会让 99.87% 被当成「大于 0.995」显示成 100%，界面就只剩 0 和 100。
///
/// `recent_success_series` 逐整点序列会随健康度一起带回：界面要用它画
/// 「近 24 小时逐时状态条」。它上游已按整点对齐，且**只包含有流量的整点**
/// （`QuerySummaryAll` 侧 requestCount==0 的桶会被跳过），所以这里同样
/// 不补零——界面按 `window_start` 对号入座，缺槽画灰底，避免把「无流量」
/// 显示成「成功率为 0」。
///
/// 解析不出 `data.models` 一律返回空映射而不是报错：老版本与魔改站点没有
/// 这条路由，站点侧也可能没启用性能采集，模型可用性判定仍以 `/v1/models`
/// 为准，不能因为缺健康度就判定同步失败。
pub(crate) fn parse_perf_metrics_health(
    value: &serde_json::Value,
    window_hours: i64,
) -> HashMap<String, SiteModelHealth> {
    let Some(items) = json_array_at(value, &["/data/models", "/models"]) else {
        return HashMap::new();
    };
    let window_start = json_number(value, "/data/window_start")
        .map(|value| value.round() as i64)
        .filter(|value| *value > 0);
    items
        .iter()
        .filter_map(|item| {
            let model_name = json_string(item, &["/model_name", "/id", "/name"]);
            if model_name.is_empty() {
                return None;
            }
            Some((
                model_name,
                SiteModelHealth {
                    success_rate: json_number(item, "/success_rate")
                        .map(|percent| (percent / 100.0).clamp(0.0, 1.0)),
                    avg_latency_ms: json_number(item, "/avg_latency_ms")
                        .map(|value| value.round() as i64),
                    avg_tps: json_number(item, "/avg_tps"),
                    window_hours,
                    requests: read_request_count(item),
                    window_start,
                    series: parse_perf_metrics_series(item),
                },
            ))
        })
        .collect()
}

/// 解析单个模型的时段序列。
///
/// 两种来源，按顺序尝试：
/// 1. `recent_success_series`（新版 NewAPI）：带 `ts` 的对象数组，逐整点成功率。
///    同顶层字段一样是 0~100 的百分数，逐点归一到 0~1；时间戳按升序排列，
///    顺带挡掉非整数或非正的时间戳（脏点会让界面槽位整体错位）。
/// 2. `recent_success_rates`（旧版/魔改 NewAPI，实测「南梁 API」）：一串 0~100
///    的成功率数字，**没有时间戳**。上游语义是「最近最多 3 个有流量的时间桶、
///    按时间升序，最后一个最新」，桶宽站点可配（默认 1 小时）但响应里不带。
///    这里按 1 小时间距贴到当前整点往前铺：界面按最后一个数据点倒推窗口，
///    条带右端显示最近几小时的明细——没有这层回退，这类站点就只有汇总值、
///    明细条带整条全灰。
fn parse_perf_metrics_series(item: &serde_json::Value) -> Vec<SiteModelHealthPoint> {
    let mut series: Vec<SiteModelHealthPoint> = json_array_at(item, &["/recent_success_series"])
        .map(|points| {
            points
                .iter()
                .filter_map(|point| {
                    let ts = json_number(point, "/ts")?;
                    if !(ts > 0.0) {
                        return None;
                    }
                    Some(SiteModelHealthPoint {
                        ts: ts.round() as i64,
                        success_rate: json_number(point, "/success_rate")
                            .map(|percent| (percent / 100.0).clamp(0.0, 1.0)),
                        requests: read_request_count(point),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if !series.is_empty() {
        series.sort_by_key(|point| point.ts);
        return series;
    }

    let Some(rates) = json_array_at(item, &["/recent_success_rates", "/recentSuccessRates"]) else {
        return Vec::new();
    };
    let count = rates.len() as i64;
    if count == 0 {
        return Vec::new();
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0);
    let current_bucket = now - now % 3600;
    rates
        .iter()
        .enumerate()
        .filter_map(|(index, value)| {
            let rate = json_number(value, "")?;
            Some(SiteModelHealthPoint {
                ts: current_bucket - (count - 1 - index as i64) * 3600,
                success_rate: Some((rate / 100.0).clamp(0.0, 1.0)),
                requests: None,
            })
        })
        .collect()
}

/// 读取「近窗口/该时段请求总数」。不同来源字段名不一致（perf-metrics 上游
/// 是 `request_count`，「模型状态」增强模块是 `total_requests`），没有就留空——
/// 请求量只用于界面提示「成功率的分母是多少」，缺失不降级健康度本身。
fn read_request_count(value: &serde_json::Value) -> Option<u64> {
    json_number(value, "/total_requests")
        .or_else(|| json_number(value, "/request_count"))
        .or_else(|| json_number(value, "/requestCount"))
        .or_else(|| json_number(value, "/requests"))
        .map(|value| value.max(0.0).round() as u64)
}

/// 界面状态条的固定槽数。解析侧与界面共用这个数：站点的时间格粒度是
/// 管理员可配的（30 分钟、1 小时…），把窗口等分成这么多格重新聚合，
/// 「站点给半小时格」和「站点给整点」落到界面上才是同一根条带。
pub(crate) const MODEL_STATUS_SLOT_COUNT: i64 = 24;

/// 「模型状态」增强模块的数据接口，按优先级排列。
///
/// 部分魔改 NewAPI 没有 perf-metrics（x666 返回 `Invalid URL`），模型可用性
/// 只从这条路由下发。第一条要登录态：**普通用户会被拒**（x666 回
/// 「无权进行此操作」，该接口按管理员角色放行）；第二条是站点开放给所有人的
/// 公开嵌入页同款接口，管理员在后台打开「公开嵌入」后普通账号也能拿到。
/// 两条都拿不到就是本站没有健康度，与「没有这条功能」在界面上同样是空。
pub(crate) const MODEL_STATUS_ENHANCEMENT_PATHS: [&str; 2] = [
    "/api/enhancements/model-status/status/all",
    "/api/enhancements/model-status/embed/status/all",
];

/// 「模型状态」的一个时间格：站点按 `slot_minutes` 切好的一段。
struct ModelStatusSlot {
    start: i64,
    end: i64,
    /// 成功率，0~100 百分数已归一到 0~1。
    success_rate: Option<f64>,
    /// 请求总数；站点没下发该字段时为 None（与显式的 0 区分开）。
    requests: Option<u64>,
}

impl ModelStatusSlot {
    /// 显式 0 请求 = 站点标记的「该时段无流量」，界面必须留灰而不是画 0%。
    fn is_idle(&self) -> bool {
        self.requests == Some(0)
    }
}

/// 读取一条模型状态里的 `slot_data` 时间格序列。
fn read_model_status_slots(item: &serde_json::Value) -> Vec<ModelStatusSlot> {
    let Some(points) = json_array_at(item, &["/slot_data", "/slots", "/series"]) else {
        return Vec::new();
    };
    points
        .iter()
        .filter_map(|point| {
            let start = json_number(point, "/start_time")?;
            if !(start > 0.0) {
                return None;
            }
            let start = start.round() as i64;
            let end = json_number(point, "/end_time")
                .map(|value| value.round() as i64)
                .filter(|value| *value > start)
                .unwrap_or(start);
            Some(ModelStatusSlot {
                start,
                end,
                success_rate: json_number(point, "/success_rate")
                    .map(|percent| (percent / 100.0).clamp(0.0, 1.0)),
                requests: json_number(point, "/total_requests")
                    .or_else(|| json_number(point, "/requests"))
                    .map(|value| value.max(0.0).round() as u64),
            })
        })
        .collect()
}

/// 解析 NewAPI「模型状态」增强模块 `/api/enhancements/model-status/status/all`
/// 的响应（x666 等没有 perf-metrics 的魔改站点的唯一健康度来源）。
///
/// 响应形如 `{ success, generated_at, ready, refresh_failed, data: [ 模型状态 ] }`，
/// 每条含 `success_rate`（0~100 百分数）、`total_requests`、
/// `recent_avg_first_response_time`（毫秒）、`recent_avg_output_token_speed`
/// （tok/s）与 `slot_data` 时间格。
///
/// 与 perf-metrics 的两点关键差异，解析时必须抹平，否则界面画不出来：
/// 1. **格宽不固定**：管理员可配 1~1440 分钟（x666 默认 30 分钟），窗口也可以
///    是 24h/7d 等。所以按全部时间格覆盖的跨度算出窗口，再把窗口等分成
///    `MODEL_STATUS_SLOT_COUNT` 格聚合——段内按请求数加权平均成功率。
/// 2. **显式空格**：`total_requests == 0` 是站点画灰的「无请求」，必须跳过，
///    不能当成成功率 0% 写进序列。
///
/// 与 perf-metrics 一样软失败：解析不出 `data` 数组就返回空映射。
pub(crate) fn parse_model_status_health(
    value: &serde_json::Value,
) -> HashMap<String, SiteModelHealth> {
    let Some(items) = json_array_at(
        value,
        &["/data", "/data/list", "/data/models", "/list", "/models"],
    ) else {
        return HashMap::new();
    };

    // 窗口跨度跨所有模型取并集：单个模型缺时间格不影响别的模型对齐到同一
    // 条时间轴（界面要用同站点任一窗口起点兜底全灰条带）。
    let mut span_start = i64::MAX;
    let mut span_end = i64::MIN;
    for item in items {
        for slot in read_model_status_slots(item) {
            span_start = span_start.min(slot.start);
            span_end = span_end.max(slot.end);
        }
    }

    // 窗口 = 全部时间格覆盖的跨度，按小时**向上取整**（不能四舍五入：
    // 取整不足会把最近的那格挤出窗口，而它恰恰是用户最关心的「现在」）。
    // 起点直接用最早时间格的起点、不做整点对齐——界面按「起点 + 等分格」
    // 相对定位，对齐反而会把末尾的格子推出窗口。站点一个时间格都没给时
    // 退化成 24h 窗口：序列为空、条带全灰，但徽标仍在。
    let (window_start, window_hours, step) = if span_end >= span_start {
        let span = (span_end - span_start).max(3600);
        let hours = ((span as f64 / 3600.0).ceil() as i64).max(1);
        let step = (hours * 3600 / MODEL_STATUS_SLOT_COUNT).max(60);
        (Some(span_start), hours, step)
    } else {
        let hours = PERF_METRICS_WINDOW_HOURS;
        (None, hours, hours * 3600 / MODEL_STATUS_SLOT_COUNT)
    };

    let mut health = HashMap::new();
    for item in items {
        let model_name = json_string(item, &["/model_name", "/id", "/name"]);
        if model_name.is_empty() {
            continue;
        }
        let slots = read_model_status_slots(item);
        let requests = read_request_count(item);
        // 站点可能因为「隐藏低请求模型」阈值之外的原因回传空壳条目：既没有
        // 请求也没有时间格流量时，界面上没有可讲的状态，直接丢弃。
        let has_traffic = requests.unwrap_or(0) > 0 || slots.iter().any(|slot| !slot.is_idle());
        if !has_traffic {
            continue;
        }

        // 时间格 → 固定槽数序列；槽内多格按请求数加权，整格无流量就不出数据点。
        // 第三项累计段内请求量，站点没给过请求数时保持 false、整段留空。
        let mut buckets: Vec<Option<(f64, f64, u64, bool)>> =
            vec![None; MODEL_STATUS_SLOT_COUNT as usize];
        let origin = window_start.unwrap_or(0);
        for slot in &slots {
            if slot.is_idle() {
                continue;
            }
            let Some(rate) = slot.success_rate else {
                continue;
            };
            if step <= 0 {
                continue;
            }
            let offset = slot.start - origin;
            if !(0..MODEL_STATUS_SLOT_COUNT * step).contains(&offset) {
                continue;
            }
            let index = (offset / step) as usize;
            let weight = slot.requests.unwrap_or(1).max(1) as f64;
            let bucket = buckets[index].get_or_insert((0.0, 0.0, 0, false));
            bucket.0 += rate * weight;
            bucket.1 += weight;
            if let Some(requests) = slot.requests {
                bucket.2 = bucket.2.saturating_add(requests);
                bucket.3 = true;
            }
        }
        let series = window_start
            .zip((step > 0).then_some(step))
            .map(|(window_start, step)| {
                buckets
                    .iter()
                    .enumerate()
                    .filter_map(|(index, bucket)| {
                        let (weighted, weight, requests, counted) = (*bucket)?;
                        Some(SiteModelHealthPoint {
                            ts: window_start + index as i64 * step,
                            success_rate: Some((weighted / weight).clamp(0.0, 1.0)),
                            requests: counted.then_some(requests),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let success_rate = json_number(item, "/success_rate")
            .map(|percent| (percent / 100.0).clamp(0.0, 1.0))
            .or_else(|| {
                // 站点只给时间格不给汇总时，用加权平均补出窗口成功率，
                // 否则徽标会退化成「成功率未知」。
                let (mut weighted, mut weight) = (0.0, 0.0);
                for point in &series {
                    let rate = point.success_rate?;
                    let w = point.requests.unwrap_or(1).max(1) as f64;
                    weighted += rate * w;
                    weight += w;
                }
                (weight > 0.0).then(|| (weighted / weight).clamp(0.0, 1.0))
            });

        health.insert(
            model_name,
            SiteModelHealth {
                success_rate,
                avg_latency_ms: json_number(item, "/recent_avg_first_response_time")
                    .map(|value| value.round() as i64),
                avg_tps: json_number(item, "/recent_avg_output_token_speed"),
                window_hours,
                requests,
                window_start,
                series,
            },
        );
    }
    health
}

/// 拉取 `/api/perf-metrics/summary?hours=24`，返回全站模型健康度。
///
/// 全程软失败，且都收敛成「空映射」这一个出口：路由不存在（404）、未登录、
/// 站点未启用采集、超时都只是没有标签，**不写 errors、不影响 Key 与模型同步
/// 主流程**。鉴权复用 `apply_newapi_auth`——刷新令牌模式下是 Bearer 访问
/// 令牌，传统 Cookie 模式下是 Cookie + New-Api-User，两者该接口都接受
/// （它只要 user 级鉴权）。
pub(crate) async fn fetch_perf_metrics_health(
    client: &wreq::Client,
    base_url: &Url,
    auth: &NewApiAuth,
    user_agent: &str,
) -> HashMap<String, SiteModelHealth> {
    let health = fetch_health_endpoint(
        client,
        base_url,
        auth,
        user_agent,
        &format!("/api/perf-metrics/summary?hours={PERF_METRICS_WINDOW_HOURS}"),
        "模型性能指标接口",
        |value| parse_perf_metrics_health(value, PERF_METRICS_WINDOW_HOURS),
    )
    .await;
    enrich_series_from_model_detail(client, base_url, auth, user_agent, health).await
}

/// 逐模型明细请求的并发度与整体预算。
///
/// 明细是逐模型的 N 次请求（CUN.AI 这类站点 50+ 个模型），必须限并发、
/// 并给整批一个总超时：补不上明细只是条带留灰，绝不能把 Key/模型同步的
/// 90s 总预算拖垮。
const MODEL_DETAIL_CONCURRENCY: usize = 6;
const MODEL_DETAIL_BUDGET: Duration = Duration::from_secs(20);
const MODEL_DETAIL_TIMEOUT: Duration = Duration::from_secs(5);

/// 汇总接口不带逐时段序列时，逐模型补明细（旧版/魔改 NewAPI，实测 CUN.AI）。
///
/// 只在**整批汇总都没有序列**时触发：新版 NewAPI 的汇总自带
/// `recent_success_series`，有序列的站点不产生任何额外请求。
/// 每个模型打 `/api/perf-metrics?model=<name>&hours=24`，响应里的
/// `data.groups[].series` 就是该模型的分组逐时段序列；补序列**不改汇总数字**
/// （成功率/延迟/TPS 仍以汇总口径为准），条带与徽标因此同源可信。
/// 失败/超时的模型保持原样（只有汇总值、条带留灰），不影响已拿到的汇总数据。
async fn enrich_series_from_model_detail(
    client: &wreq::Client,
    base_url: &Url,
    auth: &NewApiAuth,
    user_agent: &str,
    health: HashMap<String, SiteModelHealth>,
) -> HashMap<String, SiteModelHealth> {
    if health.is_empty() || health.values().any(|item| !item.series.is_empty()) {
        return health;
    }
    let models: Vec<String> = health.keys().cloned().collect();
    let details = tokio::time::timeout(
        MODEL_DETAIL_BUDGET,
        futures_util::stream::iter(models)
            .map(|model| {
                let client = client.clone();
                let base_url = base_url.clone();
                let auth = auth.clone();
                let user_agent = user_agent.to_string();
                async move {
                    let points = fetch_perf_metrics_model_detail(
                        &client, &base_url, &auth, &user_agent, &model,
                    )
                    .await;
                    (model, points)
                }
            })
            .buffer_unordered(MODEL_DETAIL_CONCURRENCY)
            .collect::<Vec<(String, Vec<SiteModelHealthPoint>)>>(),
    )
    .await;
    let Ok(details) = details else {
        // 预算内没跑完：放弃补明细，返回已拿到的汇总数据（条带留灰）。
        return health;
    };
    let mut health = health;
    for (model, points) in details {
        if points.is_empty() {
            continue;
        }
        if let Some(item) = health.get_mut(&model) {
            if item.series.is_empty() {
                item.series = points;
            }
        }
    }
    health
}

async fn fetch_perf_metrics_model_detail(
    client: &wreq::Client,
    base_url: &Url,
    auth: &NewApiAuth,
    user_agent: &str,
    model: &str,
) -> Vec<SiteModelHealthPoint> {
    let Ok(mut url) = base_url.join("/api/perf-metrics") else {
        return Vec::new();
    };
    url.query_pairs_mut()
        .append_pair("model", model)
        .append_pair("hours", &PERF_METRICS_WINDOW_HOURS.to_string());
    let request = apply_newapi_auth(
        chrome_request_headers(client.get(url), base_url.as_str(), user_agent),
        auth,
    );
    match tokio::time::timeout(
        MODEL_DETAIL_TIMEOUT,
        request_json(request, "模型性能明细接口"),
    )
    .await
    {
        Ok(Ok(value)) => parse_perf_metrics_model_detail(&value),
        Ok(Err(_)) | Err(_) => Vec::new(),
    }
}

/// 解析逐模型明细 `/api/perf-metrics?model=X` 的响应，取一个分组的逐时段序列。
///
/// 响应形如 `{ data: { model_name, groups: [ { group, success_rate,
/// avg_latency_ms, avg_tps, series: [ { ts, success_rate, ... } ] } ] } }`。
/// 站点按分组返回；这里取**序列最长**的分组代表该模型（单分组站点无歧义，
/// 多分组时覆盖最完整的那条），成功率 0~100 归一化到 0~1。
pub(crate) fn parse_perf_metrics_model_detail(value: &serde_json::Value) -> Vec<SiteModelHealthPoint> {
    let Some(groups) = value
        .pointer("/data/groups")
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    let mut best: Vec<SiteModelHealthPoint> = Vec::new();
    for group in groups {
        let Some(points) = json_array_at(group, &["/series"]) else {
            continue;
        };
        let mut series: Vec<SiteModelHealthPoint> = points
            .iter()
            .filter_map(|point| {
                let ts = json_number(point, "/ts")?;
                if !(ts > 0.0) {
                    return None;
                }
                Some(SiteModelHealthPoint {
                    ts: ts.round() as i64,
                    success_rate: json_number(point, "/success_rate")
                        .map(|percent| (percent / 100.0).clamp(0.0, 1.0)),
                    requests: read_request_count(point),
                })
            })
            .collect();
        if series.len() > best.len() {
            series.sort_by_key(|point| point.ts);
            best = series;
        }
    }
    best
}

/// 拉取「模型状态」增强模块的数据（见 `MODEL_STATUS_ENHANCEMENT_PATHS`：
/// 登录态全量接口 → 站点开放的公开嵌入接口）。语义与 perf-metrics 完全一致：
/// 拿不到就是没有标签，不算失败。
pub(crate) async fn fetch_model_status_health(
    client: &wreq::Client,
    base_url: &Url,
    auth: &NewApiAuth,
    user_agent: &str,
) -> HashMap<String, SiteModelHealth> {
    for path in MODEL_STATUS_ENHANCEMENT_PATHS {
        let health = fetch_health_endpoint(
            client,
            base_url,
            auth,
            user_agent,
            path,
            "模型状态接口",
            parse_model_status_health,
        )
        .await;
        if !health.is_empty() {
            return health;
        }
    }
    HashMap::new()
}

/// 模型健康度的统一入口：先要原生 perf-metrics（逐整点序列），站点没有这条
/// 路由时依次回退「模型状态」增强模块、站点状态接口 `/api/user/model-status`
/// （Agent Router 等），最后是站点自建监控站的公开嵌入接口（x666 等，见
/// `PUBLIC_EMBED_STATUS_URLS`）。任一级都只是软失败，全空即「本站没有健康度」。
pub(crate) async fn fetch_site_model_health(
    client: &wreq::Client,
    base_url: &Url,
    auth: &NewApiAuth,
    user_agent: &str,
) -> HashMap<String, SiteModelHealth> {
    let health = fetch_perf_metrics_health(client, base_url, auth, user_agent).await;
    if !health.is_empty() {
        return health;
    }
    let health = fetch_model_status_health(client, base_url, auth, user_agent).await;
    if !health.is_empty() {
        return health;
    }
    let health = fetch_user_model_status_health(client, base_url, auth, user_agent).await;
    if !health.is_empty() {
        return health;
    }
    match public_embed_health_url(base_url) {
        Some(url) => {
            fetch_public_health_endpoint(client, url, user_agent, parse_model_status_health).await
        }
        None => HashMap::new(),
    }
}

/// 站点自建监控站的公开嵌入健康度接口：`站点 API 主机的注册域` → `接口地址`。
///
/// 这类站点的「模型状态」路由只在主域且要求登录（未登录回 401），逐模型 ×
/// 逐时段状态由独立工具子域公开下发（实测 x666：主域 `/api/enhancements/
/// model-status/status/all` 401，`tool.x666.me/api/model-status/embed/status/all`
/// 匿名可读、11 个模型各带 24 格 slot_data）。响应形状与「模型状态」增强模块
/// 一致，因此复用 `parse_model_status_health`。用 `all` 而不是 `batch`：
/// batch 只回一个名为 "batch" 的聚合条目，没有逐模型数据。
const PUBLIC_EMBED_STATUS_URLS: [(&str, &str); 1] = [(
    "x666.me",
    "https://tool.x666.me/api/model-status/embed/status/all?window=24h",
)];

/// 站点 API 主机（忽略 `www.`）对应的公开嵌入健康度接口。
pub(crate) fn public_embed_health_url(base_url: &Url) -> Option<&'static str> {
    let host = base_url.host_str()?.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    PUBLIC_EMBED_STATUS_URLS
        .iter()
        .find(|(domain, _)| host == *domain || host.ends_with(&format!(".{domain}")))
        .map(|(_, url)| *url)
}

/// 拉取公开嵌入接口（跨域子域，无需登录）。
///
/// 刻意不注入站点鉴权：请求发往另一个主机，主域的 Cookie / 访问令牌不该外带。
async fn fetch_public_health_endpoint(
    client: &wreq::Client,
    url: &str,
    user_agent: &str,
    parse: impl FnOnce(&serde_json::Value) -> HashMap<String, SiteModelHealth>,
) -> HashMap<String, SiteModelHealth> {
    let Ok(url) = Url::parse(url) else {
        return HashMap::new();
    };
    let request = chrome_request_headers(client.get(url.clone()), url.as_str(), user_agent);
    match tokio::time::timeout(
        PERF_METRICS_TIMEOUT,
        request_json(request, "站点公开状态接口"),
    )
    .await
    {
        Ok(Ok(value)) => parse(&value),
        Ok(Err(_)) | Err(_) => HashMap::new(),
    }
}

/// Sub2API「模型市场」健康度接口（fengwind 等站点）。
///
/// 与 NewAPI 的 perf-metrics 不同：它按 (模型 × 渠道) 展开逐时段健康度，
/// `data.items[].health.buckets` 是固定格宽的时间格，`bucket_seconds` 给出格宽。
/// 鉴权用浏览器会话令牌 `auth_token`（Chrome Local Storage 里的登录令牌）——
/// 既不是 API Key 也不是 Cookie：用 Key 请求会回 `INVALID_TOKEN`。
///
/// **必须分页取全**：`page_size=100` 是服务端硬上限（传 500 回空），模型总数
/// 超过 100 时只取第 1 页会安静漏掉后面的模型。分页由
/// `fetch_sub2api_model_market_health` 负责；`page` 在请求时动态拼接。
pub(crate) const SUB2API_MODEL_MARKET_PATH: &str = "/api/v1/model-market";
pub(crate) const SUB2API_MODEL_MARKET_QUERY: &str =
    "group_by=model&sort_by=model&sort_order=asc&page_size=100&timezone=Asia%2FShanghai";
/// 分页上限：仅作异常站点的兜底护栏，正常由 `total` / 无新增提前终止。
const SUB2API_MODEL_MARKET_MAX_PAGES: u32 = 20;

/// 解析 `2026-10-05T11:45:00Z` 这类 RFC3339 时间串为 Unix 秒。
fn parse_iso_timestamp(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.timestamp())
}

/// 解析 Sub2API「模型市场」健康度。
///
/// 响应形如 `{ data: { bucket_seconds, range, window_start, window_end,
/// items: [ { model, channel, health: { buckets: [ { bucket_start,
/// success_count, failure_count, sample_count, success_rate } ] },
/// performance: { avg_ttft_ms, generation_tps } } ] } }`。
///
/// `items` 是 (模型 × 渠道) 组合，同一模型可能落在多个渠道：这里按模型把
/// 时间格对齐求和，得到「该模型在整站的表现」——界面按模型展示健康度，分渠道
/// 没有落点。无样本（`sample_count` 为 0）的格子不出数据点（界面留灰），与
/// 其它通道「无流量 ≠ 0% 失败」的语义一致。
pub(crate) fn parse_sub2api_model_market_health(
    value: &serde_json::Value,
) -> HashMap<String, SiteModelHealth> {
    let Some(items) = json_array_at(value, &["/data/items", "/data/list", "/items"]) else {
        return HashMap::new();
    };
    // 模型名 → 逐时间格累计 (成功数, 样本数)；延迟/TPS 另存候选。
    let mut slots: HashMap<String, HashMap<i64, (u64, u64)>> = HashMap::new();
    let mut latency: HashMap<String, (u64, i64)> = HashMap::new();
    let mut tps: HashMap<String, (u64, f64)> = HashMap::new();
    for item in items {
        let model = json_string(item, &["/model", "/model_name", "/id"]);
        if model.is_empty() {
            continue;
        }
        // 延迟/TPS 取样本最多的那条渠道：同模型不同渠道差异大，简单平均没有意义。
        let weight = json_number(item, "/performance/ttft_sample_count")
            .unwrap_or(0.0)
            .max(json_number(item, "/performance/generation_sample_count").unwrap_or(0.0))
            .max(0.0) as u64;
        if weight > 0 {
            if let Some(ttft) = json_number(item, "/performance/avg_ttft_ms") {
                if latency.get(&model).is_none_or(|(current, _)| weight > *current) {
                    latency.insert(model.clone(), (weight, ttft.round() as i64));
                }
            }
            if let Some(speed) = json_number(item, "/performance/generation_tps") {
                if tps.get(&model).is_none_or(|(current, _)| weight > *current) {
                    tps.insert(model.clone(), (weight, speed));
                }
            }
        }
        let Some(buckets) = json_array_at(item, &["/health/buckets", "/buckets"]) else {
            continue;
        };
        let entry = slots.entry(model).or_default();
        for bucket in buckets {
            let Some(ts) = parse_iso_timestamp(&json_string(bucket, &["/bucket_start", "/start"]))
            else {
                continue;
            };
            let sample = json_number(bucket, "/sample_count").unwrap_or(0.0).max(0.0) as u64;
            if sample == 0 {
                continue;
            }
            let success = json_number(bucket, "/success_count").unwrap_or(0.0).max(0.0) as u64;
            let slot = entry.entry(ts).or_insert((0, 0));
            slot.0 = slot.0.saturating_add(success);
            slot.1 = slot.1.saturating_add(sample);
        }
    }

    let window_start = parse_iso_timestamp(&json_string(value, &["/data/window_start"]));
    let window_end = parse_iso_timestamp(&json_string(value, &["/data/window_end"]));
    let mut health = HashMap::new();
    for (model, buckets) in slots {
        if buckets.is_empty() {
            continue;
        }
        let mut times: Vec<i64> = buckets.keys().copied().collect();
        times.sort_unstable();
        let mut series = Vec::with_capacity(times.len());
        let mut total_success = 0u64;
        let mut total_sample = 0u64;
        for ts in &times {
            let (success, sample) = buckets[ts];
            total_success = total_success.saturating_add(success);
            total_sample = total_sample.saturating_add(sample);
            series.push(SiteModelHealthPoint {
                ts: *ts,
                success_rate: (sample > 0)
                    .then(|| (success as f64 / sample as f64).clamp(0.0, 1.0)),
                requests: Some(sample),
            });
        }
        // 窗口：优先用站点给的起止；缺了就用时间格覆盖的跨度，按小时向上取整。
        let first = *times.first().unwrap_or(&0);
        let last = *times.last().unwrap_or(&first);
        let start = window_start.unwrap_or(first);
        let end = window_end.filter(|end| *end > start).unwrap_or(last);
        let hours = (((end - start).max(3600) as f64) / 3600.0).ceil() as i64;
        health.insert(
            model.clone(),
            SiteModelHealth {
                success_rate: (total_sample > 0)
                    .then(|| (total_success as f64 / total_sample as f64).clamp(0.0, 1.0)),
                avg_latency_ms: latency.get(&model).map(|(_, value)| *value),
                avg_tps: tps.get(&model).map(|(_, value)| *value),
                window_hours: hours.max(1),
                requests: Some(total_sample),
                window_start: Some(start),
                series,
            },
        );
    }
    health
}

/// 用浏览器会话令牌分页拉取 Sub2API「模型市场」健康度（fengwind 等）。
///
/// 逐页累加 `data.items`，再把合并后的形状交回纯解析器。终止条件：
/// 页面为空、已收模型数达到站点上报的 `total`、或本页没有新增模型
/// （服务端 `page>=3` 会重复返回末页，靠这一条跳出）。
pub(crate) async fn fetch_sub2api_model_market_health(
    client: &wreq::Client,
    base_url: &Url,
    auth_token: &str,
    user_agent: &str,
) -> HashMap<String, SiteModelHealth> {
    if auth_token.trim().is_empty() {
        return HashMap::new();
    }
    let mut all_items: Vec<serde_json::Value> = Vec::new();
    // 保留首屏的窗口字段：分页只切 items，窗口口径每页一致。
    let mut window_start = serde_json::Value::Null;
    let mut window_end = serde_json::Value::Null;
    let mut seen_models: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut total: Option<u64> = None;
    for page in 1..=SUB2API_MODEL_MARKET_MAX_PAGES {
        let path = format!(
            "{SUB2API_MODEL_MARKET_PATH}?{SUB2API_MODEL_MARKET_QUERY}&page={page}"
        );
        let Ok(url) = base_url.join(&path) else {
            break;
        };
        let request = chrome_request_headers(client.get(url), base_url.as_str(), user_agent)
            .bearer_auth(auth_token);
        let Ok(Ok(value)) = tokio::time::timeout(
            PERF_METRICS_TIMEOUT,
            request_json_with_hint(request, "Sub2API 模型市场", SUB2API_AUTH_FAILURE_HINT),
        )
        .await
        else {
            break;
        };
        if page == 1 {
            total = json_number(&value, "/data/total")
                .map(|value| value.max(0.0).round() as u64)
                .filter(|value| *value > 0);
            window_start = value
                .pointer("/data/window_start")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            window_end = value
                .pointer("/data/window_end")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
        }
        let Some(items) = json_array_at(&value, &["/data/items", "/data/list", "/items"]) else {
            break;
        };
        if items.is_empty() {
            break;
        }
        let before = seen_models.len();
        for item in items {
            let model = json_string(item, &["/model", "/model_name", "/id"]);
            if !model.is_empty() {
                seen_models.insert(model);
            }
            all_items.push(item.clone());
        }
        // 本页没有带来任何新模型：服务端在末页之后重复返回同一页，停。
        if page > 1 && seen_models.len() == before {
            break;
        }
        if let Some(total) = total {
            if seen_models.len() as u64 >= total {
                break;
            }
        }
    }
    if all_items.is_empty() {
        return HashMap::new();
    }
    let merged = serde_json::json!({
        "data": {
            "window_start": window_start,
            "window_end": window_end,
            "items": all_items,
        }
    });
    parse_sub2api_model_market_health(&merged)
}

/// 读 Chrome Local Storage 里 Sub2API 的会话令牌 `auth_token`（模型市场接口凭据）。
fn sub2api_auth_token_from_home(
    home: &std::path::Path,
    base_url: &Url,
    profile_id: &str,
) -> String {
    let Some(origin) = Some(base_url.origin().ascii_serialization())
        .filter(|origin| origin != "null" && !origin.is_empty())
    else {
        return String::new();
    };
    sync::read_local_storage_from_home(
        home,
        &[sync::LocalStorageTarget {
            site_id: String::new(),
            profile_id: profile_id.to_string(),
            origin,
        }],
    )
    .into_iter()
    .find_map(|item| item.values.get("auth_token").cloned())
    .map(|value| local_scalar(&value))
    .filter(|value| !value.is_empty())
    .unwrap_or_default()
}

/// 站点状态接口路径（Agent Router 等站点的模型状态页数据源）。
pub(crate) const USER_MODEL_STATUS_PATH: &str = "/api/user/model-status";

/// 站点状态接口的健康度：`/api/user/model-status`。
///
/// 响应形如 `{ data: { window_hours, bucket_seconds, models: [ { name,
/// status, current_tier, heartbeat: ["ok"|"warn"|"none", …], heartbeat_start,
/// success_rate_24h, avg_latency_ms } ] } }`。
/// 与 perf-metrics 的差异：**没有逐桶成功率**，只有心跳等级；因此把心跳按
/// 等级映射成槽位颜色（ok → 100% 绿、warn → 60% 橙、none → 该时段无数据留灰），
/// 窗口汇总成功率仍取站点的 `success_rate_24h`（徽标悬浮提示里的「窗口平均」）。
/// 窗口内完全无流量的模型（全 none 且无汇总值）不产生条目，与其它健康度
/// 通道「无流量不出条目」的语义一致。
pub(crate) fn parse_user_model_status_health(
    value: &serde_json::Value,
) -> HashMap<String, SiteModelHealth> {
    let Some(data) = value.get("data") else {
        return HashMap::new();
    };
    let Some(items) = data.get("models").and_then(serde_json::Value::as_array) else {
        return HashMap::new();
    };
    let window_hours = json_number(data, "/window_hours")
        .map(|value| value.round() as i64)
        .filter(|value| *value > 0)
        .unwrap_or(PERF_METRICS_WINDOW_HOURS);
    let bucket_seconds = json_number(data, "/bucket_seconds")
        .map(|value| value.round() as i64)
        .filter(|value| *value > 0)
        .unwrap_or(3600);
    let mut health = HashMap::new();
    for item in items {
        let name = json_string(item, &["/name"]);
        if name.is_empty() {
            continue;
        }
        let heartbeat_start = json_number(item, "/heartbeat_start")
            .map(|value| value.round() as i64)
            .filter(|value| *value > 0);
        let mut series: Vec<SiteModelHealthPoint> = Vec::new();
        if let (Some(start), Some(beats)) = (
            heartbeat_start,
            item.get("heartbeat").and_then(serde_json::Value::as_array),
        ) {
            for (index, beat) in beats.iter().enumerate() {
                let Some(label) = beat.as_str() else {
                    continue;
                };
                // 心跳只有等级、没有百分数：按等级映射槽位颜色。
                // "none" = 该时段无流量，不出数据点（界面留灰）。
                let success_rate = match label.trim().to_ascii_lowercase().as_str() {
                    "ok" => 1.0,
                    "warn" => 0.6,
                    _ => continue,
                };
                series.push(SiteModelHealthPoint {
                    ts: start + index as i64 * bucket_seconds,
                    success_rate: Some(success_rate),
                    requests: None,
                });
            }
        }
        let success_rate = json_number(item, "/success_rate_24h")
            .map(|percent| (percent / 100.0).clamp(0.0, 1.0));
        if series.is_empty() && success_rate.is_none() {
            // 窗口内无流量：与其它健康度通道一致，不产生条目。
            continue;
        }
        health.insert(
            name,
            SiteModelHealth {
                success_rate,
                avg_latency_ms: json_number(item, "/avg_latency_ms")
                    .map(|value| value.round() as i64),
                avg_tps: None,
                window_hours,
                requests: None,
                window_start: heartbeat_start,
                series,
            },
        );
    }
    health
}

pub(crate) async fn fetch_user_model_status_health(
    client: &wreq::Client,
    base_url: &Url,
    auth: &NewApiAuth,
    user_agent: &str,
) -> HashMap<String, SiteModelHealth> {
    fetch_health_endpoint(
        client,
        base_url,
        auth,
        user_agent,
        USER_MODEL_STATUS_PATH,
        "站点模型状态接口",
        parse_user_model_status_health,
    )
    .await
}

/// 站点状态接口里的模型清单：`data.models[].name`。
///
/// 部分站点（Agent Router）对 `/v1/models` 一律返回 `unauthorized client`，
/// 模型清单只从状态接口下发——它同时也是健康度的来源，两者同源可信。
pub(crate) fn parse_user_model_status_models(value: &serde_json::Value) -> Vec<SiteModelItem> {
    let Some(items) = value
        .pointer("/data/models")
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    let mut models: Vec<SiteModelItem> = items
        .iter()
        .filter_map(|item| {
            let id = json_string(item, &["/name", "/id", "/model_name"]);
            (!id.is_empty()).then_some(SiteModelItem {
                id,
                owned_by: None,
            })
        })
        .collect();
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.dedup_by(|left, right| left.id == right.id);
    models
}

pub(crate) async fn fetch_user_model_status_models(
    client: &wreq::Client,
    base_url: &Url,
    auth: &NewApiAuth,
    user_agent: &str,
) -> Vec<SiteModelItem> {
    let Ok(url) = base_url.join(USER_MODEL_STATUS_PATH) else {
        return Vec::new();
    };
    let request = apply_newapi_auth(
        chrome_request_headers(client.get(url), base_url.as_str(), user_agent),
        auth,
    );
    match tokio::time::timeout(
        PERF_METRICS_TIMEOUT,
        request_json(request, "站点模型状态接口"),
    )
    .await
    {
        Ok(Ok(value)) => parse_user_model_status_models(&value),
        Ok(Err(_)) | Err(_) => Vec::new(),
    }
}

/// 「白与黑」模型健康度：按模型分组查询 `/api/model_health/slot_status`。
///
/// 响应形状与 NewAPI「模型状态」增强模块一致（`data.models[].slot_data`，
/// `success_rate` 为 0~100 百分数、`total_requests` 为请求数），因此复用
/// `parse_model_status_health`。与 NewAPI 的差异：该接口**必须**带 `group`
/// 参数——不带会返回 `{"success":false,"message":"group is not accessible"}`，
/// 所以按本账号 Key 覆盖的分组逐个查询再合并；同一模型出现在多个分组时保留
/// 先到的条目（分组名先排序，结果稳定）。软失败语义与其它健康度通道一致：
/// 超时、报错、形状不符都只丢健康度，不影响 Key 与模型同步。
async fn fetch_baiheibai_model_health(
    client: &wreq::Client,
    base_url: &Url,
    auth: &NewApiAuth,
    user_agent: &str,
    key_groups: &HashMap<String, String>,
) -> HashMap<String, SiteModelHealth> {
    let mut groups: Vec<String> = key_groups
        .values()
        .map(|group| group.trim().to_string())
        .filter(|group| !group.is_empty())
        .collect();
    groups.sort();
    groups.dedup();
    let mut health: HashMap<String, SiteModelHealth> = HashMap::new();
    for group in groups {
        let Ok(mut url) = base_url.join("/api/model_health/slot_status") else {
            continue;
        };
        // 分组名可能含空格/中文，统一交给 query_pairs_mut 做转义。
        url.query_pairs_mut()
            .append_pair("group", &group)
            .append_pair("window", "24h");
        let request = apply_newapi_auth(
            chrome_request_headers(client.get(url), base_url.as_str(), user_agent),
            auth,
        );
        let value =
            match tokio::time::timeout(PERF_METRICS_TIMEOUT, request_json(request, "白与黑健康度接口"))
                .await
            {
                Ok(Ok(value)) => value,
                Ok(Err(_)) | Err(_) => continue,
            };
        for (model, item) in parse_model_status_health(&value) {
            health.entry(model).or_insert(item);
        }
    }
    health
}

/// 按给定路径拉一次健康度并交给 `parse` 解析。
///
/// 与主流程解耦的三条不变量：路径拼不出（相对地址非法）返回空、整体只给
/// `PERF_METRICS_TIMEOUT`（健康度慢不能把 Key 同步拖到 90s 总预算之外）、
/// 任何错误（404 / 401 / 超时 / 非 JSON）都收敛成空映射。
async fn fetch_health_endpoint(
    client: &wreq::Client,
    base_url: &Url,
    auth: &NewApiAuth,
    user_agent: &str,
    path: &str,
    label: &str,
    parse: impl FnOnce(&serde_json::Value) -> HashMap<String, SiteModelHealth>,
) -> HashMap<String, SiteModelHealth> {
    let Ok(url) = base_url.join(path) else {
        return HashMap::new();
    };
    let request = apply_newapi_auth(
        chrome_request_headers(client.get(url), base_url.as_str(), user_agent),
        auth,
    );
    match tokio::time::timeout(PERF_METRICS_TIMEOUT, request_json(request, label)).await {
        Ok(Ok(value)) => parse(&value),
        Ok(Err(_)) | Err(_) => HashMap::new(),
    }
}

/// 健康度响应的统一解析：先按 perf-metrics 形状解析，解析不出再试「模型状态」
/// 增强模块的形状。两条路都空才认定站点没有健康度数据。
pub(crate) fn parse_site_model_health(
    value: &serde_json::Value,
) -> HashMap<String, SiteModelHealth> {
    let perf = parse_perf_metrics_health(value, PERF_METRICS_WINDOW_HOURS);
    if !perf.is_empty() {
        return perf;
    }
    parse_model_status_health(value)
}

pub(crate) fn merge_api_keys(target: &mut Vec<String>, keys: impl IntoIterator<Item = String>) {
    target.extend(keys);
    target.sort();
    target.dedup();
}

pub(crate) fn merge_api_key_groups(
    target: &mut HashMap<String, String>,
    groups: impl IntoIterator<Item = (String, String)>,
) {
    for (key, group) in groups {
        if group.is_empty() {
            continue;
        }
        let current = target.entry(key).or_default();
        if current.is_empty() {
            *current = group;
        }
    }
}

pub(crate) fn cache_profile_api_counts(
    database: &Database,
    site_id: Option<&str>,
    profile_id: Option<&str>,
    result: SiteModelsResult,
) -> Result<SiteModelsResult, String> {
    let should_cache_keys =
        !result.keys.is_empty() || matches!(result.source.as_str(), "newapi-key" | "sub2api-key");
    if let (Some(site_id), Some(profile_id)) = (site_id, profile_id) {
        let connection = database.lock_conn()?;
        if should_cache_keys {
            connection
                .execute(
                    "UPDATE site_accounts
                     SET api_key_count = ?1, api_model_count = ?2
                     WHERE site_id = ?3 AND profile_id = ?4",
                    params![
                        result.keys.len() as i64,
                        result.models.len() as i64,
                        site_id,
                        profile_id
                    ],
                )
                .map_err(|error| error.to_string())?;
        } else {
            connection
                .execute(
                    "UPDATE site_accounts
                     SET api_model_count = ?1
                     WHERE site_id = ?2 AND profile_id = ?3",
                    params![result.models.len() as i64, site_id, profile_id],
                )
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(result)
}

pub(crate) fn clear_site_model_cache(database: &Database, site_id: &str) -> Result<(), String> {
    let connection = database.lock_conn()?;
    connection
        .execute("DELETE FROM site_model_cache WHERE site_id = ?1", [site_id])
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn save_site_model_cache(
    database: &Database,
    site_id: &str,
    account: &SiteModelCacheAccount,
    result: Option<&SiteModelsResult>,
    preserve_keys: bool,
) -> Result<(), String> {
    let connection = database.lock_conn()?;
    // 调用方（站点级同步/手动添加）可能只带 profile_id 而账号名为空：
    // 从 site_accounts 回填该账号的展示名，避免缓存行与账号表脱节。
    let (profile_name, account_name, username) = {
        let cached_name = (
            account.profile_name.clone(),
            account.account_name.clone(),
            account.username.clone(),
        );
        let has_any_name = !account.profile_name.is_empty()
            || !account.account_name.is_empty()
            || !account.username.is_empty();
        if has_any_name || account.profile_id.is_empty() {
            cached_name
        } else {
            connection
                .query_row(
                    "SELECT profile_name, account_name, username
                     FROM site_accounts WHERE site_id = ?1 AND profile_id = ?2",
                    params![site_id, account.profile_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(|error| error.to_string())?
                .unwrap_or(cached_name)
        }
    };
    // 同步模型（preserve_keys）时：保留库中已有 Key/分组；拉取失败时模型数据也一并保留，
    // 只更新错误信息，避免把左侧 Key 树或右侧模型列表清空。
    let existing = if preserve_keys {
        connection
            .query_row(
                "SELECT keys_json, groups_json, models_json, key_models_json, health_json, api_source
                 FROM site_model_cache WHERE site_id = ?1 AND profile_id = ?2",
                params![site_id, account.profile_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .ok()
    } else {
        None
    };
    let keys = if preserve_keys {
        existing
            .as_ref()
            .and_then(|(keys_json, ..)| serde_json::from_str(keys_json).ok())
            .unwrap_or_else(|| account.keys.clone())
    } else {
        result
            .map(|item| item.keys.clone())
            .unwrap_or_else(|| account.keys.clone())
    };
    let mut key_groups = if preserve_keys {
        existing
            .as_ref()
            .and_then(|(_, groups_json, ..)| serde_json::from_str(groups_json).ok())
            .unwrap_or_else(|| account.key_groups.clone())
    } else {
        result
            .map(|item| item.key_groups.clone())
            .filter(|groups| !groups.is_empty())
            .unwrap_or_else(|| account.key_groups.clone())
    };
    let models = result
        .map(|item| item.models.clone())
        .or_else(|| {
            existing
                .as_ref()
                .and_then(|(_, _, models_json, ..)| serde_json::from_str(models_json).ok())
        })
        .unwrap_or_default();
    let mut key_models = result
        .map(|item| item.key_models.clone())
        .filter(|map| !map.is_empty())
        .or_else(|| {
            existing
                .as_ref()
                .and_then(|(_, _, _, key_models_json, ..)| {
                    serde_json::from_str(key_models_json).ok()
                })
        })
        .unwrap_or_default();

    // 关键清理：移除已经不在 keys 中的旧 Key（保证删除的 Key 彻底从分组与模型映射中移除）
    let key_set: HashSet<&str> = keys.iter().map(String::as_str).collect();
    key_groups.retain(|k, _| key_set.contains(k.as_str()));
    key_models.retain(|k, _| key_set.contains(k.as_str()));

    // 全站模型健康度：只在本次真的取到时覆盖，否则保留旧值。
    // 「同步模型」（preserve_keys）不回带健康度，若一并清空会把上一次
    // 「同步 Key」拉到的标签抹掉，界面上模型健康度就变成时有时无。
    let model_health = [
        result.map(|item| item.model_health.clone()),
        Some(account.model_health.clone()),
        existing
            .as_ref()
            .and_then(|(_, _, _, _, health_json, _)| serde_json::from_str(health_json).ok()),
    ]
    .into_iter()
    .flatten()
    .find(|health| !health.is_empty())
    .unwrap_or_default();

    let api_source = result
        .map(|item| item.source.clone())
        .or_else(|| {
            existing
                .as_ref()
                .and_then(|(_, _, _, _, _, source)| (!source.is_empty()).then(|| source.clone()))
        })
        .unwrap_or_default();
    // 同步失败原因落库：调用方传入的 account.error 常为空，而后端收集的
    // result.errors 是唯一能说明“为什么 0 个 Key”的信息，不能丢弃。
    let persisted_error = if !keys.is_empty() || !models.is_empty() {
        String::new()
    } else if !account.error.is_empty() {
        account.error.clone()
    } else {
        result
            .map(|item| item.errors.join("\n"))
            .unwrap_or_default()
    };
    connection
        .execute(
            "INSERT INTO site_model_cache
             (site_id, profile_id, profile_name, account_name, username, api_source, keys_json, groups_json, models_json, key_models_json, error, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, CURRENT_TIMESTAMP)
             ON CONFLICT(site_id, profile_id) DO UPDATE SET
               profile_name = excluded.profile_name,
               account_name = excluded.account_name,
               username = excluded.username,
               api_source = excluded.api_source,
               keys_json = excluded.keys_json,
               groups_json = excluded.groups_json,
               models_json = excluded.models_json,
               key_models_json = excluded.key_models_json,
               error = excluded.error,
               updated_at = CURRENT_TIMESTAMP",
            params![
                site_id,
                account.profile_id,
                profile_name,
                account_name,
                username,
                api_source,
                serde_json::to_string(&keys).map_err(|error| error.to_string())?,
                serde_json::to_string(&key_groups).map_err(|error| error.to_string())?,
                serde_json::to_string(&models).map_err(|error| error.to_string())?,
                serde_json::to_string(&key_models).map_err(|error| error.to_string())?,
                persisted_error,
            ],
        )
        .map_err(|error| error.to_string())?;

    // 健康度单独写、失败只丢标签：它进主 UPSERT 的话，序列化异常或列缺失
    // 会让整条缓存写入报错，Key/模型同步结果被连带吞掉——补偿信息不能
    // 绑架主流程。解析来源已保证是有限数（json_number 过滤 NaN/Inf），
    // 这里的兜底只是不让任何意外波及上面那条写入。
    let health_json = serde_json::to_string(&model_health).unwrap_or_else(|_| "{}".to_string());
    let _ = connection.execute(
        "UPDATE site_model_cache SET health_json = ?3
         WHERE site_id = ?1 AND profile_id = ?2",
        params![site_id, account.profile_id, health_json],
    );

    // 本次真的从站点拿到了 Key/模型（result 非空且调用方没报错）：
    // 上一次同步遗留的 sync_error 只描述旧数据，继续展示会让卡片在恢复后
    // 仍显示「账号信息同步失败」。这里不再按关键词挑着清，「账号同步超过
    // 90 秒」「未扫到登录会话」等历史错误同样应被清掉。
    // 卡片上的 api_sync_error 展示值来自 site_model_cache.error 的 JOIN
    // 计算（见 read_cached_usage_sites），已在上方 persisted_error='' 清掉；
    // site_accounts 表没有 api_sync_error 列，这里写它会整条 UPDATE 报
    // no such column 被静默吞掉，sync_error 反而永远清不掉。
    if result.is_some() && account.error.is_empty() {
        let _ = connection.execute(
            "UPDATE site_accounts
             SET is_valid = 1,
                 sync_error = ''
             WHERE site_id = ?1 AND profile_id = ?2",
            params![site_id, account.profile_id],
        );
    }

    Ok(())
}

/// 站点级同步一个 Key 都没拿到时，把每个账号的失败原因写回它自己的缓存行。
///
/// 错误文本形如 `Profile 11：旧版 NewAPI 本地 user 缺少用户 ID`，冒号前是该账号的
/// profile_id。只写 error，不动 Key/模型/归属：界面在账号下方显示「读取失败」即可，
/// 顶层不再弹一条全局诊断，也不必伪造一个无归属的账号行来挂错误。
/// 没有对应缓存行时按账号表补一行（带展示名），保证账号节点仍能显示失败原因。
pub(crate) fn record_site_model_cache_errors(
    database: &Database,
    site_id: &str,
    errors: &[String],
) -> Result<(), String> {
    let connection = database.lock_conn()?;
    for message in errors {
        let Some((profile_id, reason)) = message.split_once('：') else {
            // 不属于任何账号的站点级错误（如请求超时）没有账号可挂，跳过。
            continue;
        };
        let profile_id = profile_id.trim();
        let reason = reason.trim();
        if profile_id.is_empty() || reason.is_empty() {
            continue;
        }
        let account = connection
            .query_row(
                "SELECT profile_name, account_name, username FROM site_accounts
                  WHERE site_id = ?1 AND profile_id = ?2",
                params![site_id, profile_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| error.to_string())?;
        let updated = connection
            .execute(
                "UPDATE site_model_cache SET error = ?3
                 WHERE site_id = ?1 AND profile_id = ?2",
                params![site_id, profile_id, reason],
            )
            .map_err(|error| error.to_string())?;
        if updated == 0 {
            let (profile_name, account_name, username) = account.unwrap_or_default();
            connection
                .execute(
                    "INSERT OR IGNORE INTO site_model_cache
                         (site_id, profile_id, profile_name, account_name, username, error, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, CURRENT_TIMESTAMP)",
                    params![
                        site_id,
                        profile_id,
                        profile_name,
                        account_name,
                        username,
                        reason
                    ],
                )
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn clear_site_model_cache_for_site(
    ctx: Managed<'_, Arc<AppContext>>,
    site_id: String,
) -> Result<(), String> {
    let database = &*ctx.database;
    clear_site_model_cache(&database, &site_id)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn save_site_model_cache_for_account(
    ctx: Managed<'_, Arc<AppContext>>,
    site_id: String,
    account: SiteModelCacheAccount,
    result: Option<SiteModelsResult>,
    preserve_keys: Option<bool>,
) -> Result<(), String> {
    let database = &*ctx.database;
    save_site_model_cache(
        &database,
        &site_id,
        &account,
        result.as_ref(),
        preserve_keys.unwrap_or(false),
    )
}

/// 按缓存 Key 逐个拉取模型：读取 site_model_cache 中该站点全部账号行的
/// keys_json，逐 Key 请求 /v1/models（不额外向站点要 Key 列表），把结果
/// 写回各自账号行的 key_models_json / models_json。与「同步 Key」（拉站点
/// Key 列表重建）不同，本命令完全以用户手动维护的 Key 集合为准，手动添加
/// 的 Key 也参与拉取；某 Key 拉取失败只记 errors，不清空其旧模型映射。
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn sync_models_for_cached_keys(
    ctx: Managed<'_, Arc<AppContext>>,
    site_id: String,
) -> Result<SiteModelsResult, String> {
    let database = &*ctx.database;
    // 读出全部账号行的 Key 与分组（站点级 profile_id='' 行也参与）。
    let rows: Vec<(String, Vec<String>, HashMap<String, String>)> = {
        let connection = database.lock_conn()?;
        let mut statement = connection
            .prepare(
                "SELECT profile_id, keys_json, groups_json
                 FROM site_model_cache WHERE site_id = ?1",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([&site_id], |row| {
                let keys: Vec<String> =
                    serde_json::from_str(&row.get::<_, String>(1)?).unwrap_or_default();
                let groups: HashMap<String, String> =
                    serde_json::from_str(&row.get::<_, String>(2)?).unwrap_or_default();
                Ok((row.get::<_, String>(0)?, keys, groups))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        rows
    };
    // 该站点的 API 地址（从 directory_sites 读 api_base_url）。
    let base_raw: String = {
        let connection = database.lock_conn()?;
        connection
            .query_row(
                "SELECT api_base_url FROM directory_sites WHERE id = ?1",
                [&site_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "站点不存在".to_string())?
    };

    let mut all_keys: Vec<String> = Vec::new();
    let mut all_groups: HashMap<String, String> = HashMap::new();
    for (_, keys, groups) in &rows {
        merge_api_keys(&mut all_keys, keys.iter().cloned());
        merge_api_key_groups(
            &mut all_groups,
            groups.iter().map(|(k, v)| (k.clone(), v.clone())),
        );
    }
    if all_keys.is_empty() {
        return Err("该站点没有可用的 Key，请先添加 Key".to_string());
    }

    let client = build_site_http_client(database, SITE_PROBE_TIMEOUT, 3, "站点模型同步")?;
    let base_url = normalize_site_base_url(&base_raw)?;
    let profile_ids: Vec<String> = rows.iter().map(|(id, _, _)| id.clone()).collect();
    let system_type: String = {
        let connection = database.lock_conn()?;
        connection
            .query_row(
                "SELECT system_type FROM directory_sites WHERE id = ?1",
                [site_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .unwrap_or_default()
    };
    // 模型与健康度并发跑，且互不依赖：健康度接口（perf-metrics / 模型状态）
    // 与逐 Key 的 `/v1/models` 是两条独立通道，任一方失败都不该拖垮另一方——
    // 模型全 401 时健康度照样出得来，反过来也一样。并行还省掉一轮串行等待。
    let (models_outcome, health_outcome) = tokio::join!(
        // 逐 Key 拉取：复用 fetch_models_with_keys 的候选与解析逻辑。
        // 不带站点会话 Cookie，仅 Bearer Key，与手动管理的语义一致。
        fetch_models_with_keys(
            &client,
            &base_url,
            all_keys.clone(),
            all_keys,
            all_groups,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/133.0.0.0 Safari/537.36",
            "models",
            None,
        ),
        collect_site_model_health(
            database,
            &client,
            &base_url,
            &site_id,
            &system_type,
            &profile_ids,
        )
    );
    // 健康度抓不到只是没有这份辅助信息，不进 errors，也不影响模型结果。
    let model_health = health_outcome.unwrap_or_default();
    // 模型整体失败时降级成「带错误的结果」而不是 Err：同一轮并行抓到的健康度
    // 才回得到界面上（下面取数据那段本来就按 Err 分支兜过），否则前端一次
    // throw 就把健康度一起丢了——那正是「一个失败拖垮另一个」。
    let mut result = match models_outcome {
        Ok(result) => result,
        Err(error) => {
            // Agent Router 这类站点：/v1/models 对所有 Key 返回 unauthorized
            // client，模型清单只从站点状态接口 `/api/user/model-status` 下发
            // （与健康度同源）。逐账号读凭据兜底，拿到清单就按站点级列表
            // 挂到每个 Key 上，走下方正常写回路径持久化；再失败才保留原错误。
            let fallback_models = collect_user_model_status_models(
                database,
                &client,
                &base_url,
                &site_id,
                &system_type,
                &profile_ids,
            )
            .await;
            if fallback_models.is_empty() {
                SiteModelsResult {
                    models: Vec::new(),
                    source: "models".into(),
                    keys: Vec::new(),
                    key_groups: HashMap::new(),
                    key_models: HashMap::new(),
                    errors: vec![error],
                    profile_id: String::new(),
                    model_health: model_health.clone(),
                }
            } else {
                let mut fallback_key_models: HashMap<String, Vec<SiteModelItem>> = HashMap::new();
                for (_, keys, _) in &rows {
                    for key in keys {
                        fallback_key_models.insert(key.clone(), fallback_models.clone());
                    }
                }
                SiteModelsResult {
                    models: fallback_models,
                    source: "models".into(),
                    keys: Vec::new(),
                    key_groups: HashMap::new(),
                    key_models: fallback_key_models,
                    errors: Vec::new(),
                    profile_id: String::new(),
                    model_health: model_health.clone(),
                }
            }
        }
    };

    // 写回每个账号行：本行 Key 中拉到模型的写入 key_models_json；整站模型列表
    // = 各 Key 结果合并。拉取整体失败（如全部 401）也保留已有数据，只把错误
    // 记到站点级行。
    let key_models = result.key_models.clone();
    let errors = result.errors.clone();
    // 整体成功（拿到了完整模型列表）说明站点接口已恢复：清掉该站点账号
    // 行的历史 sync_error，避免卡片在模型已同步成功时仍挂着「账号信息同步失败」。
    // site_accounts 没有 api_sync_error 列（卡片展示值由 site_model_cache.error
    // JOIN 计算得出，成功时在下方 error = '' 清掉），写它会整条 UPDATE 失败。
    let sync_succeeded = result.errors.is_empty() && !result.models.is_empty();
    {
        let connection = database.lock_conn()?;
        if sync_succeeded {
            let _ = connection.execute(
                "UPDATE site_accounts
                 SET sync_error = ''
                 WHERE site_id = ?1",
                [site_id.as_str()],
            );
        }
        for (profile_id, keys, _) in &rows {
            let mut row_models: HashMap<String, Vec<SiteModelItem>> = HashMap::new();
            for key in keys {
                if let Some(models) = key_models.get(key) {
                    row_models.insert(key.clone(), models.clone());
                }
            }
            let merged: Vec<SiteModelItem> = {
                let mut items: Vec<SiteModelItem> = Vec::new();
                for models in row_models.values() {
                    for model in models {
                        if !items.iter().any(|item| item.id == model.id) {
                            items.push(model.clone());
                        }
                    }
                }
                items.sort_by(|left, right| left.id.cmp(&right.id));
                items
            };
            connection
                .execute(
                    "UPDATE site_model_cache
                     SET key_models_json = ?3,
                         models_json = CASE WHEN ?3 != '{}' THEN ?4 ELSE models_json END,
                         error = CASE WHEN ?3 = '{}' AND ?5 != '' THEN ?5 ELSE '' END,
                         updated_at = CURRENT_TIMESTAMP
                     WHERE site_id = ?1 AND profile_id = ?2",
                    params![
                        site_id,
                        profile_id,
                        serde_json::to_string(&row_models).map_err(|error| error.to_string())?,
                        serde_json::to_string(&merged).map_err(|error| error.to_string())?,
                        errors.join("\n"),
                    ],
                )
                .map_err(|error| error.to_string())?;
        }
    }
    // 并发那轮抓到的健康度挂到结果上：它是站点全站口径，不依赖任何 Key，
    // 模型成败都带着。不并进 errors——健康度缺失只是少一份辅助信息。
    result.model_health = model_health;
    // 整体失败通过 result.errors 报给前端（库里已尽量保留旧数据），而不是
    // 抛 Err：同一轮并行抓到的健康度要跟着回到界面上，不能被一次失败吃掉。
    Ok(result)
}

/// 用各账号保存的访问令牌抓某站点的模型健康度，并写回各账号行的 `health_json`。
///
/// 健康度是**站点全站口径**的辅助信息（perf-metrics / 「模型状态」增强模块），
/// 与具体 Key 无关，但接口要求鉴权。这段鉴权与逐 Key 拉 `/v1/models` 是两条独立
/// 通道，所以「同步模型」在拿到模型列表后也要走一次这里，界面才不会是旧数据。
///
/// 取不到令牌、或该架构根本不用令牌时返回空映射——「这个站点没有健康度数据」
/// 是常态，不算同步失败，界面按无数据处理。抓不到也绝不写站点/账号的 error：
/// 健康度只是补偿信息，不该把一次成功的同步变成报错。
/// 读取某账号缓存的分组表（Key → 分组），供按分组查询健康度使用。
fn read_cached_key_groups(
    database: &Database,
    site_id: &str,
    profile_id: &str,
) -> HashMap<String, String> {
    let Ok(connection) = database.lock_conn() else {
        return HashMap::new();
    };
    connection
        .query_row(
            "SELECT groups_json FROM site_model_cache WHERE site_id = ?1 AND profile_id = ?2",
            params![site_id, profile_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
        .and_then(|json| serde_json::from_str::<HashMap<String, String>>(&json).ok())
        .unwrap_or_default()
}

/// 按平台抓一次健康度：NewAPI 系走通用通道，「白与黑」走专属的
/// `/api/model_health/slot_status`（分组表随账号缓存传入）。
async fn fetch_platform_model_health(
    client: &wreq::Client,
    base_url: &Url,
    system_type: &str,
    auth: &NewApiAuth,
    user_agent: &str,
    groups: &HashMap<String, String>,
) -> HashMap<String, SiteModelHealth> {
    if is_platform(system_type, "baiheibai") {
        fetch_baiheibai_model_health(client, base_url, auth, user_agent, groups).await
    } else {
        fetch_site_model_health(client, base_url, auth, user_agent).await
    }
}

pub(crate) async fn collect_site_model_health(
    database: &Database,
    client: &wreq::Client,
    base_url: &Url,
    site_id: &str,
    system_type: &str,
    profile_ids: &[String],
) -> Result<HashMap<String, SiteModelHealth>, String> {
    // 只抓「有访问令牌」的架构：健康度接口按令牌放行，令牌能从库里直接取。
    //
    // Cookie 形态（new-api / done-hub / one-hub 等）此前完全不抓：它得读
    // Chrome Cookie + Local Storage 才能凑出鉴权，慢且经常拿不到（浏览器没开、
    // 已退出登录、站点改了键名），最后多半是一条空结果，却让每次同步模型都
    // 白等一轮。但 Agent Router 这类站点的健康度只从会话接口
    // `/api/user/model-status` 下发、且只认浏览器会话——不放开 cookie 形态，
    // 「同步模型 / 刷新健康度」这两条路径就永远没有健康度。因此对 NewAPI 系
    // 放开 cookie 收集：代价只在 cookie 读取与一次软失败请求上，抓不到照旧
    // 按无数据处理。Sub2API 没有这些路由，不参与。
    //
    // 令牌形态（newapi2 / 白与黑）优先用缓存令牌；但令牌会过期，而部分站点
    // 的续期接口（/api/user/token）对浏览器会话也拒绝（实测 Ultra Router 返回
    // 401），令牌一旦过期健康度就会永远停在旧快照上——所以令牌路径拿不到数据
    // 时回退读 Chrome Cookie 走会话鉴权（实测健康度接口认会话），代价只在
    // 失败路径上多读一次浏览器 Cookie。
    // 站点自建监控站的公开嵌入接口（x666 等）匿名可读，不依赖令牌或会话，
    // 任意架构的站点只要在 `PUBLIC_EMBED_STATUS_URLS` 里就能抓。
    //
    // Sub2API（fengwind 等）的健康度走它自己的「模型市场」接口，凭据是
    // Chrome Local Storage 里的会话令牌 auth_token（Key 会回 INVALID_TOKEN，
    // Cookie 也不是它的鉴权方式），所以按 sub2api 单独开一条通道。
    let public_embed = public_embed_health_url(base_url).is_some();
    let sub2api = is_sub2api(system_type);
    let token_form = uses_access_token(system_type);
    let cookie_form = !token_form && is_newapi(system_type);
    if (!token_form && !cookie_form && !public_embed && !sub2api) || profile_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let home = home_dir().ok_or("无法定位用户目录")?;
    let user_agent = sync::chrome_user_agent();
    let mut merged: HashMap<String, SiteModelHealth> = HashMap::new();
    for profile_id in profile_ids {
        if sub2api {
            // Sub2API：读 Chrome Local Storage 的会话令牌 → 模型市场接口。
            let auth_token = {
                let home = home.clone();
                let base_url = base_url.clone();
                let profile_id = profile_id.clone();
                spawn_blocking(move || {
                    sub2api_auth_token_from_home(&home, &base_url, &profile_id)
                })
                .await
                .unwrap_or_default()
            };
            if !auth_token.is_empty() {
                let health =
                    fetch_sub2api_model_market_health(client, base_url, &auth_token, &user_agent)
                        .await;
                if !health.is_empty() {
                    let health_json =
                        serde_json::to_string(&health).unwrap_or_else(|_| "{}".to_string());
                    let connection = database.lock_conn()?;
                    let _ = connection.execute(
                        "UPDATE site_model_cache SET health_json = ?3, updated_at = CURRENT_TIMESTAMP
                          WHERE site_id = ?1 AND profile_id = ?2",
                        params![site_id, profile_id, health_json],
                    );
                    drop(connection);
                    if merged.is_empty() {
                        merged = health;
                    }
                }
            }
            continue;
        }
        let (token, user_id) = {
            let connection = database.lock_conn()?;
            connection
                .query_row(
                    "SELECT newapi_token, newapi_user_id FROM site_accounts
                      WHERE site_id = ?1 AND profile_id = ?2",
                    params![site_id, profile_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .unwrap_or_default()
        };
        let token = token.trim().to_string();
        let groups = if is_platform(system_type, "baiheibai") {
            // 「白与黑」的分组表随账号缓存（Key 同步时已落库）。
            read_cached_key_groups(database, site_id, profile_id)
        } else {
            HashMap::new()
        };
        let mut health = HashMap::new();
        if !token.is_empty() {
            let auth = NewApiAuth::Token {
                access_token: token,
                user_id: user_id.clone(),
            };
            health =
                fetch_platform_model_health(client, base_url, system_type, &auth, &user_agent, &groups)
                    .await;
        }
        if health.is_empty() {
            // 回退浏览器登录会话：读该 Chrome Profile 在站点 API 域下的 Cookie。
            let cookie_home = home.clone();
            let cookie_target = base_url.to_string();
            let cookie_profile = profile_id.clone();
            let cookie_header = spawn_blocking(move || {
                sync::read_chrome_cookie_header_from_home(
                    &cookie_home,
                    &cookie_target,
                    &cookie_profile,
                )
            })
            .await
            .ok()
            .and_then(|result| result.ok())
            .unwrap_or_default();
            let cookie_header = cookie_header.trim().to_string();
            // 没有 Cookie 也继续：站点自建监控站的公开嵌入接口（x666 等）匿名
            // 可读，不需要任何登录态；有 Cookie 时一并带上，让主域的鉴权路由
            // （enhancements / user/model-status）也有机会命中。
            if !cookie_header.is_empty() || public_embed_health_url(base_url).is_some() {
                let auth = NewApiAuth::Legacy {
                    cookie_header,
                    user_id,
                };
                health = fetch_platform_model_health(
                    client,
                    base_url,
                    system_type,
                    &auth,
                    &user_agent,
                    &groups,
                )
                .await;
            }
        }
        if health.is_empty() {
            continue;
        }
        // 健康度是站点全站口径：同站点各账号一致，逐行写一份，界面按账号行读。
        let health_json = serde_json::to_string(&health).unwrap_or_else(|_| "{}".to_string());
        let connection = database.lock_conn()?;
        let _ = connection.execute(
            "UPDATE site_model_cache SET health_json = ?3, updated_at = CURRENT_TIMESTAMP
              WHERE site_id = ?1 AND profile_id = ?2",
            params![site_id, profile_id, health_json],
        );
        drop(connection);
        // 多账号时保留先到的那份：口径相同，重复拉没有意义。
        if merged.is_empty() {
            merged = health;
        }
    }
    Ok(merged)
}

/// 用站点状态接口 `/api/user/model-status` 兜底收集模型清单。
///
/// 只在 `/v1/models` 对所有 Key 失败时调用（Agent Router 等站点对该接口
/// 一律返回 unauthorized client）。逐账号按「缓存令牌 → 浏览器 Cookie」
/// 取凭据；任一账号拿到清单即返回（该接口是站点级口径，账号间一致）。
/// 失败/超时/无该路由都收敛成空列表，由调用方保留原错误。
async fn collect_user_model_status_models(
    database: &Database,
    client: &wreq::Client,
    base_url: &Url,
    site_id: &str,
    system_type: &str,
    profile_ids: &[String],
) -> Vec<SiteModelItem> {
    if !is_newapi(system_type) || profile_ids.is_empty() {
        return Vec::new();
    }
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let user_agent = sync::chrome_user_agent();
    for profile_id in profile_ids {
        let (token, user_id) = {
            let Ok(connection) = database.lock_conn() else {
                return Vec::new();
            };
            connection
                .query_row(
                    "SELECT newapi_token, newapi_user_id FROM site_accounts
                      WHERE site_id = ?1 AND profile_id = ?2",
                    params![site_id, profile_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .unwrap_or_default()
        };
        let token = token.trim().to_string();
        let mut models = Vec::new();
        if !token.is_empty() {
            let auth = NewApiAuth::Token {
                access_token: token,
                user_id: user_id.clone(),
            };
            models = fetch_user_model_status_models(client, base_url, &auth, &user_agent).await;
        }
        if models.is_empty() {
            let cookie_home = home.clone();
            let cookie_target = base_url.to_string();
            let cookie_profile = profile_id.clone();
            let cookie_header = spawn_blocking(move || {
                sync::read_chrome_cookie_header_from_home(
                    &cookie_home,
                    &cookie_target,
                    &cookie_profile,
                )
            })
            .await
            .ok()
            .and_then(|result| result.ok())
            .unwrap_or_default();
            let cookie_header = cookie_header.trim().to_string();
            if !cookie_header.is_empty() {
                let auth = NewApiAuth::Legacy {
                    cookie_header,
                    user_id,
                };
                models = fetch_user_model_status_models(client, base_url, &auth, &user_agent).await;
            }
        }
        if !models.is_empty() {
            return models;
        }
    }
    Vec::new()
}


/// 单独刷新某站点的模型健康度（模型反代页同步模型后调用），返回本次抓到的数据。
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn refresh_site_model_health(
    ctx: Managed<'_, Arc<AppContext>>,
    site_id: String,
) -> Result<HashMap<String, SiteModelHealth>, String> {
    let database = &*ctx.database;
    let (system_type, base_raw, profile_ids) = {
        let connection = database.lock_conn()?;
        let (system_type, base_raw) = connection
            .query_row(
                "SELECT system_type, api_base_url FROM directory_sites WHERE id = ?1",
                [&site_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "站点不存在".to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT profile_id FROM site_accounts
                  WHERE site_id = ?1
                  GROUP BY profile_id ORDER BY max(updated_at) DESC",
            )
            .map_err(|error| error.to_string())?;
        let profile_ids = statement
            .query_map([&site_id], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        drop(statement);
        (system_type, base_raw, profile_ids)
    };
    let base_url = normalize_site_base_url(&base_raw)?;
    let client = build_site_http_client(database, SITE_PROBE_TIMEOUT, 3, "站点健康度")?;
    collect_site_model_health(database, &client, &base_url, &site_id, &system_type, &profile_ids)
        .await
}

/// 手动管理 Key：向指定账号行追加一个 Key（去重）。账号行不存在时按
/// site_id + profile_id 新建一行。返回是否新增（重复添加返回 false）。
/// `group_name` 与自动拉取的 keyGroups 对齐：写入 groups_json，空值落「默认分组」。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn add_site_model_cache_key(
    ctx: Managed<'_, Arc<AppContext>>,
    site_id: String,
    profile_id: String,
    key: String,
    group_name: Option<String>,
    profile_name: Option<String>,
    username: Option<String>,
) -> Result<bool, String> {
    let database = &*ctx.database;
    add_site_model_cache_key_inner(
        database,
        &site_id,
        &profile_id,
        &key,
        group_name.as_deref().unwrap_or_default(),
        profile_name.as_deref().unwrap_or_default(),
        username.as_deref().unwrap_or_default(),
    )
}

pub(crate) fn add_site_model_cache_key_inner(
    database: &Database,
    site_id: &str,
    profile_id: &str,
    key: &str,
    group_name: &str,
    profile_name: &str,
    username: &str,
) -> Result<bool, String> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err("Key 不能为空".to_string());
    }
    let raw_group = group_name.trim();
    let group = if raw_group.is_empty() {
        "默认分组".to_string()
    } else {
        raw_group.to_string()
    };
    let connection = database.lock_conn()?;
    let row: Option<(String, String)> = connection
        .query_row(
            "SELECT keys_json, groups_json FROM site_model_cache WHERE site_id = ?1 AND profile_id = ?2",
            params![site_id, profile_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    // 行不存在时只允许两种建行：站点级（profile_id 为空）直接建；带
    // profile_id 的必须在 site_accounts 里有真实账号——凭空造行会被
    // read_cached_usage_sites 的缓存行 UNION 分支当成会话吐给前端，渲染出
    // 不存在的账号。真实账号则照常建行（会话已扫描到但还没同步过 Key 的
    // 账号就落在这里），并从账号表回填展示名，避免缓存行与账号表脱节。
    let mut profile_name = profile_name.to_string();
    let mut username = username.to_string();
    let mut account_name = String::new();
    let (row_exists, keys_json, groups_json) = match row {
        Some((keys_json, groups_json)) => (true, keys_json, groups_json),
        None if profile_id.is_empty() => (false, "[]".to_string(), "{}".to_string()),
        None => {
            let known: Option<(String, String, String)> = connection
                .query_row(
                    "SELECT profile_name, account_name, username FROM site_accounts
                     WHERE site_id = ?1 AND profile_id = ?2",
                    params![site_id, profile_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(|error| error.to_string())?;
            let Some((known_profile, known_account, known_username)) = known else {
                return Err("目标账号不存在：请先同步会话建立账号信息，再手动添加 Key".to_string());
            };
            if profile_name.is_empty() {
                profile_name = known_profile;
            }
            if username.is_empty() {
                username = known_username;
            }
            account_name = known_account;
            (false, "[]".to_string(), "{}".to_string())
        }
    };
    let mut keys: Vec<String> = serde_json::from_str(&keys_json).unwrap_or_default();
    if keys.iter().any(|item| item == trimmed) {
        return Ok(false);
    }
    keys.push(trimmed.to_string());
    let mut key_groups: HashMap<String, String> =
        serde_json::from_str(&groups_json).unwrap_or_default();
    key_groups.insert(keys.last().expect("just pushed").clone(), group);
    if row_exists {
        connection
            .execute(
                "UPDATE site_model_cache
                 SET keys_json = ?3,
                     groups_json = ?4,
                     profile_name = COALESCE(NULLIF(?5, ''), profile_name),
                     username = COALESCE(NULLIF(?6, ''), username),
                     error = '',
                     updated_at = CURRENT_TIMESTAMP
                 WHERE site_id = ?1 AND profile_id = ?2",
                params![
                    site_id,
                    profile_id,
                    serde_json::to_string(&keys).map_err(|error| error.to_string())?,
                    serde_json::to_string(&key_groups).map_err(|error| error.to_string())?,
                    profile_name,
                    username,
                ],
            )
            .map_err(|error| error.to_string())?;
    } else {
        connection
            .execute(
                "INSERT INTO site_model_cache (site_id, profile_id, profile_name, account_name, username, keys_json, groups_json, error)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '')",
                params![
                    site_id,
                    profile_id,
                    profile_name,
                    account_name,
                    username,
                    serde_json::to_string(&keys).map_err(|error| error.to_string())?,
                    serde_json::to_string(&key_groups).map_err(|error| error.to_string())?,
                ],
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(true)
}

/// 手动管理 Key：从指定账号行移除一个 Key，并同步清理分组与逐 Key 模型映射。
/// 返回是否发生删除（Key 不存在返回 false）。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn remove_site_model_cache_key(
    ctx: Managed<'_, Arc<AppContext>>,
    site_id: String,
    profile_id: String,
    key: String,
) -> Result<bool, String> {
    let database = &*ctx.database;
    let connection = database.lock_conn()?;
    let keys_json: String = connection
        .query_row(
            "SELECT keys_json FROM site_model_cache WHERE site_id = ?1 AND profile_id = ?2",
            params![site_id, profile_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "目标账号不存在".to_string())?;
    let mut keys: Vec<String> = serde_json::from_str(&keys_json).unwrap_or_default();
    let original_len = keys.len();
    keys.retain(|item| item != &key);
    if keys.len() == original_len {
        return Ok(false);
    }
    let mut key_groups: HashMap<String, String> = connection
        .query_row(
            "SELECT groups_json FROM site_model_cache WHERE site_id = ?1 AND profile_id = ?2",
            params![site_id, profile_id],
            |row| row.get::<_, String>(0),
        )
        .map_err(|error| error.to_string())
        .and_then(|json| serde_json::from_str(&json).map_err(|error| error.to_string()))
        .unwrap_or_default();
    let mut key_models: HashMap<String, Vec<SiteModelItem>> = connection
        .query_row(
            "SELECT key_models_json FROM site_model_cache WHERE site_id = ?1 AND profile_id = ?2",
            params![site_id, profile_id],
            |row| row.get::<_, String>(0),
        )
        .map_err(|error| error.to_string())
        .and_then(|json| serde_json::from_str(&json).map_err(|error| error.to_string()))
        .unwrap_or_default();
    key_groups.remove(&key);
    key_models.remove(&key);
    connection
        .execute(
            "UPDATE site_model_cache
             SET keys_json = ?3, groups_json = ?4, key_models_json = ?5,
                 error = CASE WHEN ?6 = 0 THEN '' ELSE error END,
                 updated_at = CURRENT_TIMESTAMP
             WHERE site_id = ?1 AND profile_id = ?2",
            params![
                site_id,
                profile_id,
                serde_json::to_string(&keys).map_err(|error| error.to_string())?,
                serde_json::to_string(&key_groups).map_err(|error| error.to_string())?,
                serde_json::to_string(&key_models).map_err(|error| error.to_string())?,
                keys.len() as i64,
            ],
        )
        .map_err(|error| error.to_string())?;
    Ok(true)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn get_site_model_cache(
    ctx: Managed<'_, Arc<AppContext>>,
    site_id: String,
) -> Result<SiteModelCache, String> {
    let database = &*ctx.database;
    let connection = database.lock_conn()?;
    let mut statement = connection
        .prepare(
            "SELECT profile_id, profile_name, account_name, username, api_source, keys_json, groups_json, models_json, key_models_json, health_json, error
             FROM site_model_cache WHERE site_id = ?1 ORDER BY profile_name, account_name, profile_id",
        )
        .map_err(|error| error.to_string())?;
    let mut models = Vec::new();
    let mut api_source = String::new();
    let mut model_health: HashMap<String, SiteModelHealth> = HashMap::new();
    let accounts = statement
        .query_map([site_id.as_str()], |row| {
            let keys_json: String = row.get(5)?;
            let groups_json: String = row.get(6)?;
            let models_json: String = row.get(7)?;
            let key_models_json: String = row.get(8)?;
            let health_json: String = row.get(9)?;
            let account_models: Vec<SiteModelItem> =
                serde_json::from_str(&models_json).unwrap_or_default();
            let keys: Vec<String> = serde_json::from_str(&keys_json).unwrap_or_default();
            let key_groups: HashMap<String, String> =
                serde_json::from_str(&groups_json).unwrap_or_default();
            let key_models: HashMap<String, Vec<SiteModelItem>> =
                serde_json::from_str(&key_models_json).unwrap_or_default();
            let source: String = row.get(4)?;
            if api_source.is_empty() && !source.is_empty() {
                api_source = source;
            }
            models.extend(account_models);
            Ok(SiteModelCacheAccount {
                profile_id: row.get(0)?,
                profile_name: row.get(1)?,
                account_name: row.get(2)?,
                username: row.get(3)?,
                keys,
                key_groups,
                key_models,
                model_health: serde_json::from_str(&health_json).unwrap_or_default(),
                error: row.get(10)?,
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    // 健康度是全站口径，同一站点各账号行拿到的是同一份数据；按「先到先得」
    // 合并即可，不需要比较新旧。带逐时序列的行优先：部分账号可能是旧版本
    // 落库的数据（只有聚合值），别用它把状态条的槽位挤掉。
    for account in &accounts {
        for (model_id, health) in &account.model_health {
            let entry = model_health
                .entry(model_id.clone())
                .or_insert_with(|| health.clone());
            if entry.series.is_empty() && !health.series.is_empty() {
                *entry = health.clone();
            }
        }
    }
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.dedup_by(|left, right| left.id == right.id);
    Ok(SiteModelCache {
        models,
        api_source,
        accounts,
        model_health,
    })
}

/// 一次读出全部站点的模型缓存（模型聚合页数据源）。
/// 与 get_site_model_cache 相同的行解析逻辑，但按 site_id 分组返回所有站点。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn get_all_site_model_caches(
    ctx: Managed<'_, Arc<AppContext>>,
) -> Result<Vec<SiteModelCacheEntry>, String> {
    let database = &*ctx.database;
    let connection = database.lock_conn()?;
    let mut statement = connection
        .prepare(
            "SELECT site_id, profile_id, profile_name, account_name, username, api_source, keys_json, groups_json, models_json, key_models_json, health_json, error
             FROM site_model_cache ORDER BY site_id, profile_name, account_name, profile_id",
        )
        .map_err(|error| error.to_string())?;
    // 每行返回 (site_id, 账号, 该行的账号级模型, api_source)。
    let rows = statement
        .query_map([], |row| {
            let keys_json: String = row.get(6)?;
            let groups_json: String = row.get(7)?;
            let models_json: String = row.get(8)?;
            let key_models_json: String = row.get(9)?;
            let health_json: String = row.get(10)?;
            Ok((
                row.get::<_, String>(0)?,
                SiteModelCacheAccount {
                    profile_id: row.get(1)?,
                    profile_name: row.get(2)?,
                    account_name: row.get(3)?,
                    username: row.get(4)?,
                    keys: serde_json::from_str(&keys_json).unwrap_or_default(),
                    key_groups: serde_json::from_str(&groups_json).unwrap_or_default(),
                    key_models: serde_json::from_str(&key_models_json).unwrap_or_default(),
                    model_health: serde_json::from_str(&health_json).unwrap_or_default(),
                    error: row.get(11)?,
                },
                serde_json::from_str::<Vec<SiteModelItem>>(&models_json).unwrap_or_default(),
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    drop(statement);

    let mut entries: Vec<SiteModelCacheEntry> = Vec::new();
    for (site_id, account, account_models, api_source) in rows {
        // ORDER BY site_id 保证同一站点的行连续，直接追加即可。
        let entry = match entries.last_mut() {
            Some(entry) if entry.site_id == site_id => entry,
            _ => {
                entries.push(SiteModelCacheEntry {
                    site_id: site_id.clone(),
                    cache: SiteModelCache {
                        models: Vec::new(),
                        api_source: String::new(),
                        accounts: Vec::new(),
                        model_health: HashMap::new(),
                    },
                });
                entries.last_mut().expect("just pushed")
            }
        };
        if entry.cache.api_source.is_empty() && !api_source.is_empty() {
            entry.cache.api_source = api_source;
        }
        for (model_id, health) in &account.model_health {
            entry
                .cache
                .model_health
                .entry(model_id.clone())
                .or_insert_with(|| health.clone());
        }
        entry.cache.models.extend(account_models);
        entry.cache.accounts.push(account);
    }
    for entry in &mut entries {
        entry
            .cache
            .models
            .sort_by(|left, right| left.id.cmp(&right.id));
        entry
            .cache
            .models
            .dedup_by(|left, right| left.id == right.id);
    }
    Ok(entries)
}

/// 单个站点 / 账号同步的硬性总超时：超过即强制失败。Key 同步在直连之外
/// 还有 Chrome 浏览器兜底（页面首载 + Cloudflare challenge 桥接），且探测
/// / 会话读取等前置开销不定，60 秒会把带盾站点连坐掐断，故给到 90 秒。
const SITE_SYNC_TIMEOUT: Duration = Duration::from_secs(90);

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn fetch_site_models_json(
    ctx: Managed<'_, Arc<AppContext>>,
    url: String,
    site_id: Option<String>,
    profile_id: Option<String>,
) -> Result<SiteModelsResult, String> {
    let database = &*ctx.database;
    tokio::time::timeout(
        SITE_SYNC_TIMEOUT,
        fetch_site_models_json_impl(&ctx, database, url, site_id, profile_id),
    )
    .await
    .map_err(|_| {
        format!(
            "站点模型同步超过 {} 秒，已强制终止",
            SITE_SYNC_TIMEOUT.as_secs()
        )
    })?
}

/// 站点接口探测超时：探测只关心端点可达性，无需站点同步那样的长超时。
const SITE_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// 响应体展示片段的最大字符数。
const SITE_PROBE_EXCERPT_CHARS: usize = 400;

/// 站点 /v1/models 无 Key 探测结果。
/// 只要收到 HTTP 响应就代表端点可达：401/403 的 JSON 错误（key 无效/未授权）视为正常返回。
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SiteProbeResult {
    /// 端点是否正常（key 无效也算正常；连接失败/安全盾拦截/5xx 为异常）。
    pub(crate) ok: bool,
    /// HTTP 状态码，0 表示未收到响应。
    pub(crate) status: u16,
    pub(crate) latency_ms: u64,
    pub(crate) content_type: String,
    pub(crate) is_json: bool,
    /// 能从响应中解析出的模型数量（无法解析时为 0）。
    pub(crate) model_count: usize,
    /// 一句话结论，用于标签旁的说明。
    pub(crate) message: String,
    /// 响应体截断片段，供用户自查。
    pub(crate) body_excerpt: String,
}

/// 归一化站点 API 地址：补协议头、补尾斜杠并解析为 Url。
fn normalize_site_base_url(raw: &str) -> Result<Url, String> {
    let mut base = raw.trim().to_string();
    if !base.starts_with("http://") && !base.starts_with("https://") {
        base = format!("https://{base}");
    }
    if !base.ends_with('/') {
        base.push('/');
    }
    Url::parse(&base).map_err(|_| "站点 API 地址无效".to_string())
}

fn excerpt_body(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    text.chars().take(SITE_PROBE_EXCERPT_CHARS).collect()
}

/// 单通道站点探测结果 = 通道信息 + 探测结果（probe 以 flatten 展开为 camelCase 字段）。
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChannelSiteProbe {
    pub(crate) channel_id: String,
    pub(crate) channel_name: String,
    /// 探测实际经过的出口节点名（通道未固定节点时会自动回退写回）。
    pub(crate) node_name: String,
    #[serde(flatten)]
    pub(crate) probe: SiteProbeResult,
}

/// 测试站点：每个通道用各自的固定出口 lane 各请求一次 /v1/models（通道间并发）。
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn test_site_models_per_channel(
    ctx: Managed<'_, Arc<AppContext>>,
    url: String,
) -> Result<Vec<ChannelSiteProbe>, String> {
    let database = &*ctx.database;
    let runtime = &*ctx.proxy_runtime;
    let models_url = normalize_site_base_url(&url)?
        .join("v1/models")
        .map_err(|_| "站点 API 地址无效".to_string())?;

    // 逐通道准备出口 lane 端口与节点名；ensure/查询是阻塞操作，集中放进阻塞线程池
    let exits = tokio::task::block_in_place(|| {
        let channels: Vec<(String, String)> = {
            let connection = database.lock_conn()?;
            let mut statement = connection
                .prepare("SELECT id, name FROM proxy_channels ORDER BY rowid")
                .map_err(|error| error.to_string())?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|error| error.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            rows
        };
        let mut exits = Vec::with_capacity(channels.len());
        for (channel_id, channel_name) in channels {
            let port = proxypool::ensure_channel_instance(database, runtime, &channel_id)?;
            let node_name = {
                let connection = database.lock_conn()?;
                connection
                    .query_row(
                        "SELECT n.name FROM proxy_channels c
                         JOIN proxy_pool_nodes n ON n.id = c.node_id
                         WHERE c.id = ?1",
                        [&channel_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(|error| error.to_string())?
                    .unwrap_or_else(|| "未绑定节点".into())
            };
            exits.push((channel_id, channel_name, port, node_name));
        }
        Ok::<_, String>(exits)
    })?;

    // 每个通道一条请求：各自独立 lane 出口，互不影响，并发执行
    let probes = exits
        .into_iter()
        .map(|(channel_id, channel_name, port, node_name)| {
            let models_url = models_url.clone();
            async move {
                let proxy_url = format!("http://127.0.0.1:{port}");
                let probe = match proxypool::build_proxy_client_with_url(
                    database,
                    &proxy_url,
                    SITE_PROBE_TIMEOUT,
                    3,
                    "站点接口测试",
                ) {
                    Ok(client) => probe_models_endpoint(client, models_url).await,
                    Err(error) => SiteProbeResult {
                        ok: false,
                        status: 0,
                        latency_ms: 0,
                        content_type: String::new(),
                        is_json: false,
                        model_count: 0,
                        message: format!("通道出口不可用：{error}"),
                        body_excerpt: String::new(),
                    },
                };
                ChannelSiteProbe {
                    channel_id,
                    channel_name,
                    node_name,
                    probe,
                }
            }
        });
    Ok(futures_util::future::join_all(probes).await)
}

async fn probe_models_endpoint(client: wreq::Client, models_url: Url) -> SiteProbeResult {
    let started = std::time::Instant::now();
    let response = match client.get(models_url).send().await {
        Ok(response) => response,
        Err(error) => {
            let latency_ms = started.elapsed().as_millis() as u64;
            return SiteProbeResult {
                ok: false,
                status: 0,
                latency_ms,
                content_type: String::new(),
                is_json: false,
                model_count: 0,
                message: format!("请求失败：{error:#}"),
                body_excerpt: String::new(),
            };
        }
    };
    let latency_ms = started.elapsed().as_millis() as u64;

    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(wreq::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let body = response.bytes().await.unwrap_or_default();
    let body = body
        .strip_prefix(&[0xef, 0xbb, 0xbf])
        .unwrap_or(body.as_ref());
    let parsed = serde_json::from_slice::<serde_json::Value>(body).ok();
    let is_json = parsed.is_some();
    let model_count = parsed
        .as_ref()
        .map(parse_site_models)
        .map(|models| models.len())
        .unwrap_or(0);

    let ok = if (200..300).contains(&status) {
        // 2xx：端点公开可用；返回 HTML 也算通，但结论里点名
        true
    } else if status == 401 {
        true
    } else if status == 403 {
        // JSON 403 = key 无效（预期）；HTML 403 = 多为安全盾拦截，不算正常
        is_json
    } else {
        false
    };
    let message = if (200..300).contains(&status) {
        if is_json && model_count > 0 {
            format!("端点正常，返回 {model_count} 个模型")
        } else if is_json {
            "端点正常，未返回模型列表".to_string()
        } else {
            "端点可达，但返回的不是 JSON".to_string()
        }
    } else if status == 401 {
        "端点正常（未带 key 被拒绝，属预期）".to_string()
    } else if status == 403 && is_json {
        "端点正常（key 无效，属预期）".to_string()
    } else if status == 403 {
        "403 且返回非 JSON，疑似安全盾拦截".to_string()
    } else if status == 404 {
        "站点未提供 /v1/models 接口".to_string()
    } else if status >= 500 {
        format!("服务端错误 HTTP {status}")
    } else {
        format!("端点返回异常状态 HTTP {status}")
    };

    SiteProbeResult {
        ok,
        status,
        latency_ms,
        content_type,
        is_json,
        model_count,
        message,
        body_excerpt: excerpt_body(body),
    }
}

async fn fetch_site_models_json_impl<'a>(
    ctx: &'a Arc<AppContext>,
    database: &'a Database,
    url: String,
    site_id: Option<String>,
    profile_id: Option<String>,
) -> Result<SiteModelsResult, String> {
    let Some(site_id) = site_id.clone() else {
        let client = build_site_http_client(database, Duration::from_secs(6), 3, "站点模型请求")?;
        return fetch_site_models_json_inner(ctx, database, url, None, profile_id, client).await;
    };
    let profile_key = profile_id.clone().unwrap_or_default();
    let site_id_for_closure = site_id.clone();
    proxypool::with_account_proxy(
        database,
        &ctx.proxy_runtime,
        &site_id,
        &profile_key,
        Duration::from_secs(6),
        3,
        "站点模型请求",
        move |client| {
            let url = url.clone();
            let site_id = site_id_for_closure.clone();
            let profile_id = profile_id.clone();
            async move {
                fetch_site_models_json_inner(ctx, database, url, Some(site_id), profile_id, client)
                    .await
            }
        },
    )
    .await
}

async fn fetch_site_models_json_inner(
    _ctx: &Arc<AppContext>,
    database: &Database,
    url: String,
    site_id: Option<String>,
    profile_id: Option<String>,
    client: wreq::Client,
) -> Result<SiteModelsResult, String> {
    let mut base = url.trim().to_string();
    if !base.starts_with("http://") && !base.starts_with("https://") {
        base = format!("https://{base}");
    }
    if !base.ends_with('/') {
        base.push('/');
    }
    let base_url = Url::parse(&base).map_err(|_| "站点 API 地址无效".to_string())?;
    let user_agent = sync::chrome_user_agent();
    let requested_profile_id = profile_id.clone();
    let (system_type, mut profile_ids, cached_model_keys) = if let Some(site_id) =
        site_id.as_deref()
    {
        let connection = database.lock_conn()?;
        let system_type = connection
            .query_row(
                "SELECT system_type FROM directory_sites WHERE id = ?1",
                [site_id],
                |row| row.get::<_, String>(0),
            )
            .unwrap_or_default();
        // 请求指定账号时，即使账号额度状态暂时无效，也要允许使用已有缓存 Key 直连模型。
        // 站点级请求（?2 为空串）则取该站点全部账号：is_valid=0 只说明上次刷新没拿到
        // 登录凭据，账号行与 Chrome profile 都还在，缓存的 newapi_token / Key 仍可能
        // 直连成功，整体排除会让站点级同步白白丢掉这些凭据，也拿不到「为什么失败」。
        let requested_profile = requested_profile_id.as_deref().unwrap_or_default();
        let mut statement = connection
            .prepare(
                "SELECT profile_id FROM site_accounts
                     WHERE site_id = ?1 AND (?2 = '' OR is_valid = 1 OR profile_id = ?2)
                     GROUP BY profile_id ORDER BY max(updated_at) DESC",
            )
            .map_err(|error| error.to_string())?;
        let profile_ids = statement
            .query_map(params![site_id, requested_profile], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let mut key_statement = connection
                .prepare("SELECT profile_id, keys_json, groups_json FROM site_model_cache WHERE site_id = ?1")
                .map_err(|error| error.to_string())?;
        let cached_model_keys = key_statement
            .query_map([site_id], |row| {
                let keys_json: String = row.get(1)?;
                let groups_json: String = row.get(2)?;
                let keys = serde_json::from_str::<Vec<String>>(&keys_json).unwrap_or_default();
                let key_groups = serde_json::from_str::<HashMap<String, String>>(&groups_json)
                    .unwrap_or_default();
                Ok((row.get::<_, String>(0)?, (keys, key_groups)))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<HashMap<_, _>, _>>()
            .map_err(|error| error.to_string())?;
        (system_type, profile_ids, cached_model_keys)
    } else {
        (String::new(), Vec::new(), HashMap::new())
    };
    if let Some(requested_profile_id) = requested_profile_id.as_deref() {
        profile_ids.retain(|candidate| candidate == requested_profile_id);
    }
    let home_dir = home_dir().ok_or("无法定位用户目录")?;
    let origin = base_url.origin().ascii_serialization();
    let local_targets = site_id
        .as_ref()
        .map(|site_id| {
            profile_ids
                .iter()
                .map(|profile_id| sync::LocalStorageTarget {
                    site_id: site_id.clone(),
                    profile_id: profile_id.clone(),
                    origin: origin.clone(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let local_matches = if local_targets.is_empty() {
        Vec::new()
    } else {
        let local_home = home_dir.clone();
        spawn_blocking(move || sync::read_local_storage_from_home(&local_home, &local_targets))
            .await
            .map_err(|error| format!("读取 Chrome Local Storage 任务失败：{error}"))?
    };
    let local_values = local_matches
        .into_iter()
        .map(|item| (item.profile_id, item.values))
        .collect::<HashMap<_, _>>();
    let mut errors = Vec::new();
    let mut discovered_keys = Vec::new();
    let mut discovered_key_groups = HashMap::new();
    let mut no_browser_fallback_profiles = HashSet::new();
    // 实际贡献了 Key 的账号：站点级请求也按它归属落库。
    let mut discovered_profile_id = String::new();

    for profile_id in &profile_ids {
        let values = local_values.get(profile_id).cloned().unwrap_or_default();
        // 空串＝未设置，允许按浏览器里残留的痕迹猜架构；显式「未知类型」是用户
        // 的声明，一律不猜，直接落到循环末尾的「架构不提供 Key 提取」。
        let inferred_type = if is_explicit_unknown(&system_type) {
            ""
        } else if system_type.trim().is_empty() {
            if parse_newapi_local_account(&values).is_ok() {
                "new-api"
            } else if parse_sub2api_local_account(&values).is_ok() {
                "sub2api"
            } else {
                ""
            }
        } else {
            system_type.as_str()
        };
        if is_newapi(inferred_type) {
            let use_refresh_auth = is_newapi_refresh(inferred_type);
            let token_url = base_url
                .join("/api/token/?p=1&size=20")
                .map_err(|_| "无法生成 /api/token 地址")?;

            let (cached_token, cached_uid) = if let Some(site_id) = site_id.as_deref() {
                let connection = database.lock_conn()?;
                connection
                    .query_row(
                        "SELECT newapi_token, newapi_user_id FROM site_accounts WHERE site_id = ?1 AND profile_id = ?2",
                        params![site_id, profile_id],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                    )
                    .unwrap_or_default()
            } else {
                (String::new(), String::new())
            };

            let mut used_cached_token = false;
            let mut auth = if use_refresh_auth && !cached_token.is_empty() {
                used_cached_token = true;
                Some(NewApiAuth::Token {
                    access_token: cached_token,
                    user_id: cached_uid.clone(),
                })
            } else {
                None
            };

            let mut model_user_id = String::new();

            macro_rules! require_legacy_auth {
                () => {{
                    let cookie_home = home_dir.clone();
                    let cookie_target = token_url.to_string();
                    let cookie_profile = profile_id.clone();
                    let cookie_header_result = spawn_blocking(move || {
                        sync::read_chrome_cookie_header_from_home(
                            &cookie_home,
                            &cookie_target,
                            &cookie_profile,
                        )
                    })
                    .await
                    .map_err(|error| format!("读取 Chrome Cookie 任务失败：{error}"));

                    let cookie_header_str = match cookie_header_result {
                        // 读到空 Cookie = 该 profile 下这个域确实没有登录 Cookie：
                        // 与「读取失败」一样都不能继续鉴权，但要分开说清是哪一种。
                        Ok(Ok(v)) if v.trim().is_empty() => {
                            errors.push(format!(
                                "{profile_id}：Chrome 里没有该站点的登录 Cookie"
                            ));
                            continue;
                        }
                        Ok(Ok(v)) => v,
                        Ok(Err(e)) => {
                            errors.push(format!("{profile_id}：{e}"));
                            continue;
                        }
                        Err(e) => {
                            errors.push(format!("{profile_id}：{e}"));
                            continue;
                        }
                    };

                    let user_id = newapi_user_id(&values).unwrap_or_default();
                    // refresh 流程用 Bearer 令牌标识用户，不依赖 user_id；
                    // 传统 Cookie 模式必须有 user_id 才能继续。
                    if user_id.is_empty() && !use_refresh_auth {
                        errors.push(format!("{profile_id}：旧版 NewAPI 本地 user 缺少用户 ID"));
                        continue;
                    }
                    model_user_id = user_id.clone();
                    NewApiAuth::Legacy {
                        cookie_header: cookie_header_str,
                        user_id: user_id.clone(),
                    }
                }};
            }

            if auth.is_none() {
                let legacy_auth = require_legacy_auth!();
                if use_refresh_auth {
                    match acquire_newapi_session_token(
                        &client,
                        &base_url,
                        &legacy_auth,
                        &user_agent,
                    )
                    .await
                    {
                        Ok(Some(auth_value)) => auth = Some(auth_value),
                        Ok(None) => auth = Some(legacy_auth),
                        Err(e) => {
                            errors.push(format!("{profile_id}：{e}"));
                            continue;
                        }
                    }
                } else {
                    auth = Some(legacy_auth);
                }
            }

            let mut auth = auth.unwrap();

            // Cookie 模式用 Cookie + New-Api-User 读取 Key；刷新令牌模式才使用访问令牌。
            // 模型接口始终必须使用实际的 NewAPI Key。
            let mut request = apply_newapi_auth(
                chrome_request_headers(
                    client.get(token_url.clone()),
                    base_url.as_str(),
                    &user_agent,
                ),
                &auth,
            );

            let mut remote_result = request_json(request, "NewAPI Key 接口").await;

            if let Err(error) = &remote_result {
                if used_cached_token && !access_token_was_rejected(error) {
                    // 遇盾（Cloudflare/HTML/网络拦截）不是令牌失效：直接请求被挡，
                    // Chrome 同源兜底是唯一可行路径，不能排除在 Chrome 兜底之外；
                    // 只有确证令牌类问题才跳过 Chrome。
                    let is_shield = is_cloudflare_shield_error(error);
                    if !is_shield {
                        no_browser_fallback_profiles.insert(profile_id.clone());
                    }
                    errors.push(format!(
                        "{profile_id}：缓存访问令牌请求失败，{}：{error}",
                        if is_shield {
                            "将转 Chrome 同源兜底获取模型"
                        } else {
                            "不执行 refresh token"
                        }
                    ));
                    continue;
                }
            }

            if remote_result.is_err() && used_cached_token && use_refresh_auth {
                let legacy_auth = require_legacy_auth!();
                match acquire_newapi_session_token(&client, &base_url, &legacy_auth, &user_agent)
                    .await
                {
                    Ok(Some(auth_value)) => auth = auth_value,
                    Ok(None) => auth = legacy_auth,
                    Err(e) => {
                        errors.push(format!("{profile_id}：{e}"));
                        continue;
                    }
                }
                used_cached_token = false;
                request = apply_newapi_auth(
                    chrome_request_headers(client.get(token_url), base_url.as_str(), &user_agent),
                    &auth,
                );
                remote_result = request_json(request, "NewAPI Key 接口").await;
            }

            // Save the newly acquired token to DB
            if let Some(site_id) = site_id.as_deref() {
                if let NewApiAuth::Token {
                    access_token,
                    user_id,
                } = &auth
                {
                    if let Ok(connection) = database.0.lock() {
                        let _ = connection.execute(
                            "UPDATE site_accounts SET newapi_token = ?1, newapi_user_id = ?2 WHERE site_id = ?3 AND profile_id = ?4",
                            params![access_token, user_id, site_id, profile_id],
                        );
                    }
                }
            }

            if model_user_id.is_empty() {
                if let NewApiAuth::Token { user_id, .. } = &auth {
                    model_user_id = user_id.clone();
                }
            }
            match remote_result {
                Ok(value) => {
                    match reveal_newapi_keys(&client, &base_url, &auth, &user_agent, &value).await {
                        Ok((keys, key_groups)) => {
                            if !keys.is_empty() {
                                discovered_profile_id = profile_id.clone();
                            }
                            merge_api_keys(&mut discovered_keys, keys.iter().cloned());
                            merge_api_key_groups(&mut discovered_key_groups, key_groups.clone());
                            // 留一份 Key 与分组给 /v1/models 全失败时的模型状态兜底。
                            let fallback_keys = keys.clone();
                            let fallback_groups = key_groups.clone();
                            // 模型逐 Key 拉取与全站健康度互不依赖，并发发起，
                            // 避免给站点的 90s 同步总预算再加一次串行往返。
                            let (models_outcome, health) = tokio::join!(
                                fetch_models_with_keys(
                                    &client,
                                    &base_url,
                                    keys.clone(),
                                    keys,
                                    key_groups,
                                    &user_agent,
                                    "newapi-key",
                                    (!model_user_id.is_empty()).then_some(model_user_id.as_str()),
                                ),
                                // perf-metrics 缺失（魔改 NewAPI）时自动回退到
                                // 「模型状态」增强模块与站点状态接口，对模型拉取
                                // 仍然只是并发的补偿信息。
                                fetch_site_model_health(&client, &base_url, &auth, &user_agent),
                            );
                            match models_outcome {
                                Ok(mut result) => {
                                    // 健康度缺失（各路由都没有 / 未启用采集）
                                    // 只是没有标签而已，不算同步失败。
                                    result.model_health = health;
                                    return cache_profile_api_counts(
                                        database,
                                        site_id.as_deref(),
                                        requested_profile_id.as_deref(),
                                        result,
                                    )
                                }
                                Err(error) => {
                                    if used_cached_token && !is_cloudflare_shield_error(&error) {
                                        no_browser_fallback_profiles.insert(profile_id.clone());
                                    }
                                    errors.push(format!("{profile_id}：{error}"));
                                    // Agent Router 这类站点：/v1/models 对所有 Key
                                    // 返回 unauthorized client，模型清单只从站点状态
                                    // 接口下发（与健康度同源）。兜底成功即按站点级
                                    // 模型列表落库，不再报 Key 接口失败。
                                    let fallback_models = fetch_user_model_status_models(
                                        &client,
                                        &base_url,
                                        &auth,
                                        &user_agent,
                                    )
                                    .await;
                                    if !fallback_models.is_empty() {
                                        let key_models = fallback_keys
                                            .iter()
                                            .map(|key| (key.clone(), fallback_models.clone()))
                                            .collect::<HashMap<_, _>>();
                                        return cache_profile_api_counts(
                                            database,
                                            site_id.as_deref(),
                                            requested_profile_id.as_deref(),
                                            SiteModelsResult {
                                                models: fallback_models,
                                                source: "newapi-key".into(),
                                                keys: fallback_keys,
                                                key_groups: fallback_groups,
                                                key_models,
                                                errors: Vec::new(),
                                                profile_id: profile_id.clone(),
                                                model_health: health,
                                            },
                                        );
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            if used_cached_token && !is_cloudflare_shield_error(&error) {
                                no_browser_fallback_profiles.insert(profile_id.clone());
                            }
                            errors.push(format!("{profile_id}：{error}"));
                        }
                    }
                }
                Err(error) => {
                    errors.push(format!("{profile_id}：{error}"));
                    // Cloudflare 盾站点直连全部被 403 挑战拦截（Key 接口与
                    // /v1/models 一视同仁），浏览器同源 fetch 是唯一路径：
                    // 复用账号同步的 Chrome 桥接，页面内拉 Key 列表 + 模型。
                    if is_cloudflare_shield_error(&error) {
                        match chrome_bridge_fetch_keys_models(
                            database,
                            &base_url,
                            &system_type,
                            &profile_id,
                            &model_user_id,
                            site_id.as_deref(),
                        )
                        .await
                        {
                            Ok(result) => {
                                return cache_profile_api_counts(
                                    database,
                                    site_id.as_deref(),
                                    requested_profile_id.as_deref(),
                                    result,
                                );
                            }
                            Err(bridge_error) => {
                                errors.push(format!("{profile_id}：Chrome 兜底：{bridge_error}"));
                            }
                        }
                    }
                }
            }
            if let Some((cached_keys, cached_key_groups)) = cached_model_keys
                .get(profile_id)
                .filter(|(keys, _)| !keys.is_empty())
            {
                if let Ok(result) = fetch_models_with_keys(
                    &client,
                    &base_url,
                    cached_keys.clone(),
                    cached_keys.clone(),
                    cached_key_groups.clone(),
                    &user_agent,
                    "newapi-key",
                    (!cached_uid.is_empty()).then_some(cached_uid.as_str()),
                )
                .await
                {
                    return cache_profile_api_counts(
                        database,
                        site_id.as_deref(),
                        requested_profile_id.as_deref(),
                        result,
                    );
                }
            }
        } else if is_platform(inferred_type, "baiheibai") {
            // —— 白与黑：Key 列表走 NewAPI 兼容接口
            // `/api/token/?page=1&size=10&keyword=&order=-id`（站点前端同款参数，
            // 与 NewAPI 系的 `?p=1&size=20` 不同，不能混用）。
            // 鉴权与账号额度一致：浏览器登录 Cookie 是主凭据（站点前端就是这么调的），
            // 用户手填的「站点令牌」在无 Cookie 或会话被拒时兜底；
            // New-Api-User 取 Local Storage / 缓存里的用户 ID，站点若像 NewAPI
            // 一样校验它就靠它，不校验则无副作用。
            let (cached_token, cached_uid) = if let Some(site_id) = site_id.as_deref() {
                let connection = database.lock_conn()?;
                connection
                    .query_row(
                        "SELECT newapi_token, newapi_user_id FROM site_accounts WHERE site_id = ?1 AND profile_id = ?2",
                        params![site_id, profile_id],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                    )
                    .unwrap_or_default()
            } else {
                (String::new(), String::new())
            };
            let model_user_id = newapi_user_id(&values)
                .filter(|value| !value.is_empty())
                .unwrap_or(cached_uid);
            let token = cached_token.trim().to_string();
            let cookie_home = home_dir.clone();
            let cookie_target = base_url.to_string();
            let cookie_profile = profile_id.clone();
            let cookie_header = spawn_blocking(move || {
                sync::read_chrome_cookie_header_from_home(
                    &cookie_home,
                    &cookie_target,
                    &cookie_profile,
                )
            })
            .await
            .map_err(|error| format!("读取 Chrome Cookie 任务失败：{error}"))?
            .unwrap_or_default()
            .trim()
            .to_string();
            if cookie_header.is_empty() && token.is_empty() {
                errors.push(format!(
                    "{profile_id}：Chrome 没有该站点的登录 Cookie 且未配置站点令牌，无法读取 Key"
                ));
                continue;
            }
            let token_url = base_url
                .join("/api/token/?page=1&size=10&keyword=&order=-id")
                .map_err(|_| "无法生成 /api/token 地址")?;
            let mut auth = if !cookie_header.is_empty() {
                NewApiAuth::Legacy {
                    cookie_header: cookie_header.clone(),
                    user_id: model_user_id.clone(),
                }
            } else {
                NewApiAuth::Token {
                    access_token: token.clone(),
                    user_id: model_user_id.clone(),
                }
            };
            let request = |auth: &NewApiAuth| {
                apply_newapi_auth(
                    chrome_request_headers(
                        client.get(token_url.clone()),
                        base_url.as_str(),
                        &user_agent,
                    ),
                    auth,
                )
            };
            let mut remote_result = request_json(request(&auth), "白与黑 Key 接口").await;
            // Cookie 会话被拒（登录态过期）但配置了站点令牌：管理接口认令牌，换它再试一次。
            if let Err(error) = &remote_result {
                let rejected = access_token_was_rejected(error) || error.contains("无权");
                if rejected && !cookie_header.is_empty() && !token.is_empty() {
                    auth = NewApiAuth::Token {
                        access_token: token.clone(),
                        user_id: model_user_id.clone(),
                    };
                    remote_result = request_json(request(&auth), "白与黑 Key 接口").await;
                }
            }
            match remote_result {
                Ok(value) => {
                    match reveal_newapi_keys(&client, &base_url, &auth, &user_agent, &value).await {
                        Ok((keys, key_groups)) => {
                            if !keys.is_empty() {
                                discovered_profile_id = profile_id.clone();
                            }
                            merge_api_keys(&mut discovered_keys, keys.iter().cloned());
                            merge_api_key_groups(&mut discovered_key_groups, key_groups.clone());
                            // 健康度按 Key 覆盖的分组查询：先克隆分组表，key_groups
                            // 本体要移交给逐 Key 模型拉取。
                            let health_groups = key_groups.clone();
                            let (models_outcome, health) = tokio::join!(
                                fetch_models_with_keys(
                                    &client,
                                    &base_url,
                                    keys.clone(),
                                    keys,
                                    key_groups,
                                    &user_agent,
                                    "newapi-key",
                                    (!model_user_id.is_empty())
                                        .then_some(model_user_id.as_str()),
                                ),
                                fetch_baiheibai_model_health(
                                    &client,
                                    &base_url,
                                    &auth,
                                    &user_agent,
                                    &health_groups,
                                ),
                            );
                            match models_outcome {
                                Ok(mut result) => {
                                    result.model_health = health;
                                    return cache_profile_api_counts(
                                        database,
                                        site_id.as_deref(),
                                        requested_profile_id.as_deref(),
                                        result,
                                    );
                                }
                                Err(error) => errors.push(format!("{profile_id}：{error}")),
                            }
                        }
                        Err(error) => errors.push(format!("{profile_id}：{error}")),
                    }
                }
                Err(error) => {
                    errors.push(format!("{profile_id}：{error}"));
                    // 盾站点直连被拦：浏览器同源兜底拉 Key 与模型
                    // （页面内同样用 `page` 参数，见桥接脚本的 token 路径参数）。
                    if is_cloudflare_shield_error(&error) {
                        match chrome_bridge_fetch_keys_models(
                            database,
                            &base_url,
                            &system_type,
                            &profile_id,
                            &model_user_id,
                            site_id.as_deref(),
                        )
                        .await
                        {
                            Ok(result) => {
                                return cache_profile_api_counts(
                                    database,
                                    site_id.as_deref(),
                                    requested_profile_id.as_deref(),
                                    result,
                                );
                            }
                            Err(bridge_error) => {
                                errors.push(format!("{profile_id}：Chrome 兜底：{bridge_error}"));
                            }
                        }
                    }
                }
            }
        } else if is_sub2api(inferred_type) {
            let auth_token = values
                .get("auth_token")
                .map(|value| local_scalar(value))
                .filter(|value| !value.is_empty());
            let Some(auth_token) = auth_token else {
                errors.push(format!("{profile_id}：Sub2API 本地数据中没有 auth_token"));
                continue;
            };
            // 已有登录令牌（auth_token）优先直接使用：用它同步模型列表，
            // 不再通过 /api/v1/keys 获取 Key。只有直接同步失败才回落到 Key 接口。
            // 健康度（模型市场接口）是补偿信息：与模型列表并发拉，失败只留空，
            // 绝不拖垮模型同步主流程。
            let client_health = client.clone();
            let base_url_health = base_url.clone();
            let auth_token_health = auth_token.clone();
            let user_agent_health = user_agent.clone();
            let mut health_task = Some(spawn(async move {
                fetch_sub2api_model_market_health(
                    &client_health,
                    &base_url_health,
                    &auth_token_health,
                    &user_agent_health,
                )
                .await
            }));
            let direct_models_url = base_url
                .join("/v1/models")
                .map_err(|_| "无法生成 /v1/models 地址".to_string())?;
            let mut direct_errors = Vec::new();
            for candidate in [auth_token.clone(), format!("sk-{auth_token}")] {
                let request = chrome_request_headers(
                    client.get(direct_models_url.clone()),
                    base_url.as_str(),
                    &user_agent,
                )
                .bearer_auth(&candidate);
                match request_json_with_hint(request, "Sub2API 模型接口", SUB2API_AUTH_FAILURE_HINT)
                    .await
                {
                    Ok(value) => {
                        let models = parse_site_models(&value);
                        if !models.is_empty() {
                            merge_api_keys(&mut discovered_keys, [auth_token.clone()]);
                            let model_health = match health_task.take() {
                                Some(task) => task.await.unwrap_or_default(),
                                None => HashMap::new(),
                            };
                            return cache_profile_api_counts(
                                database,
                                site_id.as_deref(),
                                requested_profile_id.as_deref(),
                                SiteModelsResult {
                                    models,
                                    source: "sub2api-key".into(),
                                    keys: vec![auth_token.clone()],
                                    key_groups: HashMap::new(),
                                    key_models: HashMap::new(),
                                    errors: Vec::new(),
                                    profile_id: profile_id.clone(),
                                    model_health,
                                },
                            );
                        }
                        direct_errors.push("访问秘钥获取的模型列表为空".to_string());
                    }
                    Err(error) => direct_errors.push(error),
                }
            }
            let mut sub2api_errors = Vec::new();
            if !direct_errors.is_empty() {
                sub2api_errors.push(format!(
                    "直接使用访问秘钥同步失败（{}），回落到 Key 接口",
                    direct_errors.last().cloned().unwrap_or_default()
                ));
            }
            let keys_url = base_url
                .join("/api/v1/keys?page=1")
                .map_err(|_| "无法生成 /api/v1/keys 地址")?;
            let dashboard_token = auth_token.clone();
            let request =
                chrome_request_headers(client.get(keys_url), base_url.as_str(), &user_agent)
                    .bearer_auth(&auth_token);
            match request_json_with_hint(request, "Sub2API Key 接口", SUB2API_AUTH_FAILURE_HINT)
                .await
            {
                Ok(value) => {
                    let visible_keys = parse_api_keys(&value);
                    let visible_key_groups = parse_api_key_groups(&value);
                    if !visible_keys.is_empty() {
                        discovered_profile_id = profile_id.clone();
                    }
                    merge_api_keys(&mut discovered_keys, visible_keys.iter().cloned());
                    merge_api_key_groups(&mut discovered_key_groups, visible_key_groups.clone());
                    let mut keys = visible_keys.clone();
                    keys.push(dashboard_token);
                    match fetch_models_with_keys(
                        &client,
                        &base_url,
                        keys,
                        visible_keys,
                        visible_key_groups,
                        &user_agent,
                        "sub2api-key",
                        None,
                    )
                    .await
                    {
                        Ok(mut result) => {
                            // 健康度（模型市场）与 Key 拉取并发进行，这里取回；
                            // 失败/超时只留空，不影响模型列表。
                            result.model_health = match health_task.take() {
                                Some(task) => task.await.unwrap_or_default(),
                                None => HashMap::new(),
                            };
                            return cache_profile_api_counts(
                                database,
                                site_id.as_deref(),
                                requested_profile_id.as_deref(),
                                result,
                            )
                        }
                        Err(error) => sub2api_errors.push(error),
                    }
                }
                Err(error) => sub2api_errors.push(error),
            }
            if !sub2api_errors.is_empty() {
                // 两条模型路径都没走到取回健康度的那一步：中止后台任务，
                // 避免无人消费的分页拉取白白继续（结果也无处可挂）。
                if let Some(task) = health_task.take() {
                    task.abort();
                }
                if sub2api_errors
                    .iter()
                    .all(|error| access_token_was_rejected(error))
                {
                    no_browser_fallback_profiles.insert(profile_id.clone());
                    errors.push(format!(
                        "{profile_id}：Sub2API 登录令牌（auth_token）已失效或过期，请重新登录后同步账号"
                    ));
                } else {
                    errors.extend(
                        sub2api_errors
                            .into_iter()
                            .map(|error| format!("{profile_id}：{error}")),
                    );
                }
            }
        } else {
            // 架构既不是 NewAPI 系也不是 Sub2API：这个账号没有 Key 提取路径。
            // 必须显式记一条原因——静默跳过会让外层误判成「整个站点都没有 Key」，
            // 报出一句与真实原因无关的站点级诊断。
            let known = if is_explicit_unknown(&system_type) {
                format!("{profile_id}：站点已设置为未知类型，不做架构推断，无法提取 API Key")
            } else if inferred_type.trim().is_empty() {
                format!("{profile_id}：未识别站点架构，无法提取 API Key")
            } else {
                format!(
                    "{profile_id}：站点架构 {inferred_type} 不支持 API Key 提取"
                )
            };
            errors.push(known);
        }
    }

    // 循环走完一个 Key 都没产出：有原因就按账号把原因写回它自己的缓存行，
    // 由界面在该账号下方显示「读取失败」；连原因都没有（站点没有账号可借、
    // 或架构不参与 Key 提取）才是「没有」而不是「同步成功但结果为空」，直接报错，
    // 别让调用方把空结果落库成无归属的空壳行。
    if discovered_keys.is_empty() {
        if errors.is_empty() {
            return Err(format!("{origin} 读取失败：没有可同步的 API Key"));
        }
        if let Some(site_id) = site_id.as_deref() {
            record_site_model_cache_errors(database, site_id, &errors)?;
        }
    }
    let source = if is_sub2api(&system_type) {
        "sub2api-key"
    } else {
        "newapi-key"
    };
    // discovered_keys 来自具体账号的会话/本地存储：把归属带回给调用方，
    // 站点级（不带 profile_id）请求也要按真实账号落库。
    cache_profile_api_counts(
        database,
        site_id.as_deref(),
        requested_profile_id.as_deref(),
        SiteModelsResult {
            models: Vec::new(),
            source: source.into(),
            keys: discovered_keys,
            key_groups: discovered_key_groups,
            key_models: HashMap::new(),
            errors,
            profile_id: requested_profile_id
                .as_deref()
                .map(str::to_string)
                .unwrap_or(discovered_profile_id),
            model_health: HashMap::new(),
        },
    )
}
