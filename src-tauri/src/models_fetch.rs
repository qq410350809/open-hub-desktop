use crate::account_sync::*;
use crate::chrome_local_storage;
use crate::chrome_session;
use crate::db::*;
use crate::models::*;
use crate::platform_detect::{is_newapi, is_sub2api};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{Manager, State};
use url::Url;

#[tauri::command]
pub fn get_system_fonts() -> Vec<String> {
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SiteModelsResult {
    pub(crate) models: Vec<SiteModelItem>,
    pub(crate) source: String,
    pub(crate) keys: Vec<String>,
    #[serde(default)]
    pub(crate) key_groups: HashMap<String, String>,
}

pub(crate) fn json_array_at<'a>(
    value: &'a serde_json::Value,
    pointers: &[&str],
) -> Option<&'a Vec<serde_json::Value>> {
    pointers
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(serde_json::Value::as_array))
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
    let Some(items) = json_array_at(
        value,
        &[
            "",
            "/data",
            "/data/items",
            "/data/keys",
            "/keys",
            "/items",
            "/result/items",
            "/result/keys",
        ],
    ) else {
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
                        "/group",
                        "/group_name",
                        "/groupName",
                        "/token_group",
                        "/tokenGroup",
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
    for pointer in ["/keyGroups", "/key_groups", "/data/keyGroups", "/data/key_groups"] {
        let Some(object) = value.pointer(pointer).and_then(serde_json::Value::as_object) else {
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
    let Some(items) = json_array_at(
        value,
        &["", "/data", "/data/items", "/items", "/result/items"],
    ) else {
        return Vec::new();
    };
    let mut ids = items
        .iter()
        .filter(|item| api_key_is_enabled(item))
        .filter_map(|item| {
            let id = json_string(item, &["/id", "/token_id", "/tokenId"]);
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
    let Some(items) = json_array_at(value, &["", "/data", "/data/items", "/items", "/result/items"])
    else {
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
    client: &reqwest::Client,
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
        return Err("/api/token 没有返回可用令牌 ID".into());
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
                    if let Some(group) = token_groups.get(&token_id).filter(|group| !group.is_empty()) {
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

pub(crate) async fn fetch_models_with_keys(
    client: &reqwest::Client,
    base_url: &Url,
    keys: Vec<String>,
    visible_keys: Vec<String>,
    visible_key_groups: HashMap<String, String>,
    user_agent: &str,
    source: &str,
    newapi_user_id: Option<&str>,
) -> Result<SiteModelsResult, String> {
    if keys.is_empty() {
        return Err("Key 接口没有返回可用 Key".into());
    }
    let models_url = base_url
        .join("/v1/models")
        .map_err(|_| "无法生成 /v1/models 地址".to_string())?;
    let mut errors = Vec::new();
    for key in keys {
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
                        return Ok(SiteModelsResult {
                            models,
                            source: source.into(),
                            keys: visible_keys,
                            key_groups: visible_key_groups,
                        });
                    }
                    errors.push("模型接口返回空列表".to_string());
                }
                Err(error) => errors.push(error),
            }
        }
    }
    Err(errors
        .last()
        .cloned()
        .unwrap_or_else(|| "现有 Key 均无法获取模型".into()))
}

pub(crate) fn chrome_models_bridge_script(
    system_type: &str,
    legacy_user_id: Option<&str>,
    marker: &str,
) -> String {
    let system_type = serde_json::to_string(system_type).unwrap_or_else(|_| "\"\"".into());
    let user_id =
        serde_json::to_string(legacy_user_id.unwrap_or_default()).unwrap_or_else(|_| "\"\"".into());
    let marker = serde_json::to_string(marker).unwrap_or_else(|_| "\"\"".into());
    r#"(() => {
  const bridgeToken = __OPENHUB_MARKER__;
  const systemType = __OPENHUB_SYSTEM_TYPE__.toLowerCase();
  const legacyUserId = __OPENHUB_USER_ID__;
  const pending = "__OPENHUB_PENDING__";
  if (!/^https?:$/.test(window.location.protocol)) return pending;
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
  const previous = window.__openHubModelsSync;
  if (previous && previous.token === bridgeToken) {
    if (previous.result) return JSON.stringify(previous.result);
    return pending;
  }
  const bridge = { token: bridgeToken, result: null };
  window.__openHubModelsSync = bridge;
  const scalar = (value) => {
    if (!value) return "";
    try { const parsed = JSON.parse(value); return typeof parsed === "string" ? parsed : value; }
    catch (_) { return value; }
  };
  const readJson = async (path, options) => {
    const response = await fetch(path, { credentials: "include", cache: "no-store", signal: AbortSignal.timeout(30000), ...options });
    const text = await response.text();
    let data = null;
    try { data = JSON.parse(text); } catch (_) {}
    return { ok: response.ok, status: response.status, data };
  };
  const arrays = (value, paths) => {
    for (const path of paths) {
      let current = value;
      for (const part of path) current = current && current[part];
      if (Array.isArray(current)) return current;
    }
    return [];
  };
  const activeKeyItems = (value) => arrays(value, [[], ["data"], ["data","items"], ["data","keys"], ["keys"], ["items"], ["result","items"], ["result","keys"]])
    .filter((item) => item && item.enabled !== false && item.is_active !== false && ![0, "0", "disabled", "inactive", "expired", "revoked"].includes(item.status));
  const keyGroup = (item) => typeof item === "object" && item ? String(item.group || item.group_name || item.groupName || item.token_group || item.tokenGroup || "").trim() : "";
  const extractKeyEntries = (value) => activeKeyItems(value)
    .flatMap((item) => {
      const key = String(typeof item === "string" ? item : item.key || item.api_key || item.apiKey || item.plain_key || item.plainKey || item.secret_key || item.secretKey || item.token || item.secret || item.value || "").replace(/^Bearer\s+/i, "").trim();
      const prefix = typeof item === "object" && item ? String(item.key_prefix || item.keyPrefix || item.prefix || "") : "";
      const group = keyGroup(item);
      return (prefix && !key.startsWith(prefix) ? [key, `${prefix}${key}`] : [key]).map((value) => ({ key: value, group }));
    })
    .filter((item) => item.key.length >= 8 && !/\s|\*|…|\.\.\./.test(item.key));
  const extractKeys = (value) => extractKeyEntries(value).map((item) => item.key);
  const extractKeyGroups = (value) => Object.fromEntries(extractKeyEntries(value).filter((item) => item.group).map((item) => [item.key, item.group]));
  const extractTokenIds = (value) => activeKeyItems(value)
    .map((item) => typeof item === "object" && item ? item.id ?? item.token_id ?? item.tokenId ?? "" : "")
    .map((id) => String(id))
    .filter((id) => id.length > 0 && id.length <= 64 && /^[A-Za-z0-9_-]+$/.test(id));
  const extractTokenGroups = (value) => Object.fromEntries(activeKeyItems(value)
    .map((item) => [String(item && (item.id ?? item.token_id ?? item.tokenId) || ""), keyGroup(item)])
    .filter(([id, group]) => id && group));
  const extractRevealedKey = (value) => {
    const key = String(value?.data?.key || value?.data?.api_key || value?.data?.apiKey || value?.data?.secret_key || value?.data?.secretKey ||
      (typeof value?.data === "string" ? value.data : "") || value?.key || value?.api_key || value?.apiKey || value?.secret_key || value?.secretKey || "")
      .replace(/^Bearer\s+/i, "").trim();
    return key.length >= 8 && !/\s|\*|…|\.\.\./.test(key) ? key : "";
  };
  const extractModels = (value) => arrays(value, [[], ["data"], ["data","items"], ["data","models"], ["models"], ["items"], ["result","data"], ["result","models"]])
    .map((item) => typeof item === "string" ? { id: item } : {
      id: String(item && (item.model_name || item.id || item.name || item.model || item.slug) || ""),
      ownedBy: item && (item.owner || item.owned_by || item.vendor) || undefined
    })
    .filter((item) => item.id);
  let visibleKeys = [];
  let visibleKeyGroups = {};
  (async () => {
    const headers = { Accept: "application/json, text/plain, */*" };
    let keyPath = "/api/token/?p=1&size=20";
    let source = "newapi-key";
    let dashboardAccessToken = "";
    if (systemType === "sub2api") {
      keyPath = "/api/v1/keys?page=1";
      source = "sub2api-key";
      const authToken = scalar(localStorage.getItem("auth_token"));
      if (!authToken) throw new Error("Chrome Local Storage 中没有 auth_token");
      dashboardAccessToken = authToken;
      headers.Authorization = `Bearer ${authToken}`;
    } else if (legacyUserId) {
      headers["New-Api-User"] = legacyUserId;
    }
    let keyResponse = await readJson(keyPath, { method: "GET", headers });
    // 只有认证明确返回 401 才允许 refresh。403、超时、HTML、解析失败或空 Key
    // 都不能证明访问令牌失效，避免有效会话被无谓刷新并触发 Chrome。
    if (systemType !== "sub2api" && keyResponse.status === 401) {
      const refreshResponse = await readJson("/api/user/auth/refresh", { method: "POST", headers: { Accept: "application/json" } });
      const accessToken = refreshResponse.data?.data?.access_token || refreshResponse.data?.data?.accessToken ||
        refreshResponse.data?.data?.token || refreshResponse.data?.access_token || refreshResponse.data?.accessToken || refreshResponse.data?.token || "";
      if (accessToken) {
        dashboardAccessToken = accessToken;
        headers.Authorization = `Bearer ${accessToken}`;
        keyResponse = await readJson(keyPath, { method: "GET", headers });
      }
    }
    const keys = extractKeys(keyResponse.data);
    visibleKeyGroups = extractKeyGroups(keyResponse.data);
    if (systemType !== "sub2api" && !keys.length) {
      const tokenGroups = extractTokenGroups(keyResponse.data);
      for (const tokenId of extractTokenIds(keyResponse.data)) {
        const revealResponse = await readJson(`/api/token/${encodeURIComponent(tokenId)}/key`, { method: "POST", headers });
        const revealedKey = extractRevealedKey(revealResponse.data);
        if (revealResponse.ok && revealedKey) {
          keys.push(revealedKey);
          if (tokenGroups[tokenId]) visibleKeyGroups[revealedKey] = tokenGroups[tokenId];
        }
      }
    }
    visibleKeys = [...new Set(keys)];
    // Sub2API auth_token 可直接用于模型接口；NewAPI 访问令牌只能管理 Key，不能冒充模型 Key。
    if (systemType === "sub2api" && dashboardAccessToken) keys.push(dashboardAccessToken);
    if (!keys.length) throw new Error(`${keyPath} 没有返回可用 Key（HTTP ${keyResponse.status}）`);
    let lastStatus = 0;
    let lastError = "";
    for (const key of keys) {
      const candidates = key.startsWith("sk-") || key.includes(".") ? [key] : [key, `sk-${key}`];
      for (const candidate of candidates) {
        const modelHeaders = { Accept: "application/json", Authorization: `Bearer ${candidate}` };
        if (legacyUserId) modelHeaders["New-Api-User"] = legacyUserId;
        const response = await readJson("/v1/models", { method: "GET", headers: modelHeaders });
        lastStatus = response.status;
        lastError = response.data?.error?.message || response.data?.message || response.data?.msg || response.data?.detail || "";
        const models = extractModels(response.data);
        if (response.ok && models.length) {
          bridge.result = { ok: true, models, source, keys: visibleKeys, keyGroups: visibleKeyGroups };
          return;
        }
      }
    }
    throw new Error(`/v1/models 未返回模型（HTTP ${lastStatus}${lastError ? `：${lastError}` : ""}）`);
  })().catch((error) => {
    bridge.result = { ok: false, error: error && error.message || String(error), keys: visibleKeys, keyGroups: visibleKeyGroups };
  });
  return pending;
})()"#
        .replace("__OPENHUB_SYSTEM_TYPE__", &system_type)
        .replace("__OPENHUB_USER_ID__", &user_id)
        .replace("__OPENHUB_MARKER__", &marker)
}

pub(crate) fn parse_chrome_models_result(value: &str) -> Result<SiteModelsResult, String> {
    let value = serde_json::from_str::<serde_json::Value>(value)
        .map_err(|error| format!("Chrome 模型数据无法解析：{error}"))?;
    if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(api_error_message(&value, "Chrome 没有返回模型"));
    }
    let models = parse_site_models(&value);
    if models.is_empty() {
        return Err("Chrome 返回的模型列表为空".into());
    }
    Ok(SiteModelsResult {
        models,
        source: json_string(&value, &["/source"]),
        keys: parse_api_keys(&value),
        key_groups: parse_api_key_groups(&value),
    })
}

pub(crate) fn parse_chrome_models_keys(value: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(value)
        .map(|value| parse_api_keys(&value))
        .unwrap_or_default()
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
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
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
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
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
) -> Result<(), String> {
    let models = result.map(|item| &item.models).cloned().unwrap_or_default();
    let api_source = result.map(|item| item.source.as_str()).unwrap_or("");
    let keys = result
        .map(|item| item.keys.clone())
        .unwrap_or_else(|| account.keys.clone());
    let key_groups = result
        .map(|item| item.key_groups.clone())
        .filter(|groups| !groups.is_empty())
        .unwrap_or_else(|| account.key_groups.clone());
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    connection
        .execute(
            "INSERT INTO site_model_cache
             (site_id, profile_id, profile_name, account_name, username, api_source, keys_json, groups_json, models_json, error, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, CURRENT_TIMESTAMP)
             ON CONFLICT(site_id, profile_id) DO UPDATE SET
               profile_name = excluded.profile_name,
               account_name = excluded.account_name,
               username = excluded.username,
               api_source = excluded.api_source,
               keys_json = excluded.keys_json,
               groups_json = excluded.groups_json,
               models_json = excluded.models_json,
               error = excluded.error,
               updated_at = CURRENT_TIMESTAMP",
            params![
                site_id,
                account.profile_id,
                account.profile_name,
                account.account_name,
                account.username,
                api_source,
                serde_json::to_string(&keys).map_err(|error| error.to_string())?,
                serde_json::to_string(&key_groups).map_err(|error| error.to_string())?,
                serde_json::to_string(&models).map_err(|error| error.to_string())?,
                account.error,
            ],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn clear_site_model_cache_for_site(
    database: State<'_, Database>,
    site_id: String,
) -> Result<(), String> {
    clear_site_model_cache(&database, &site_id)
}

#[tauri::command]
pub fn save_site_model_cache_for_account(
    database: State<'_, Database>,
    site_id: String,
    account: SiteModelCacheAccount,
    result: Option<SiteModelsResult>,
) -> Result<(), String> {
    save_site_model_cache(&database, &site_id, &account, result.as_ref())
}

#[tauri::command]
pub fn get_site_model_cache(
    database: State<'_, Database>,
    site_id: String,
) -> Result<SiteModelCache, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let mut statement = connection
        .prepare(
            "SELECT profile_id, profile_name, account_name, username, api_source, keys_json, groups_json, models_json, error
             FROM site_model_cache WHERE site_id = ?1 ORDER BY profile_name, account_name, profile_id",
        )
        .map_err(|error| error.to_string())?;
    let mut models = Vec::new();
    let mut api_source = String::new();
    let accounts = statement
        .query_map([site_id.as_str()], |row| {
            let keys_json: String = row.get(5)?;
            let groups_json: String = row.get(6)?;
            let models_json: String = row.get(7)?;
            let account_models: Vec<SiteModelItem> =
                serde_json::from_str(&models_json).unwrap_or_default();
            let keys: Vec<String> = serde_json::from_str(&keys_json).unwrap_or_default();
            let key_groups: HashMap<String, String> =
                serde_json::from_str(&groups_json).unwrap_or_default();
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
                error: row.get(8)?,
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.dedup_by(|left, right| left.id == right.id);
    Ok(SiteModelCache {
        models,
        api_source,
        accounts,
    })
}

#[tauri::command]
pub async fn fetch_site_models_json(
    app: tauri::AppHandle,
    database: State<'_, Database>,
    url: String,
    site_id: Option<String>,
    profile_id: Option<String>,
) -> Result<SiteModelsResult, String> {
    let client = if let Some(site_id) = site_id.as_deref() {
        build_http_client_for_site(
            &database,
            site_id,
            Duration::from_secs(6),
            3,
            "站点模型请求",
        )?
    } else {
        build_http_client(&database, Duration::from_secs(6), 3, "站点模型请求")?
    };
    let mut base = url.trim().to_string();
    if !base.starts_with("http://") && !base.starts_with("https://") {
        base = format!("https://{base}");
    }
    if !base.ends_with('/') {
        base.push('/');
    }
    let base_url = Url::parse(&base).map_err(|_| "站点 API 地址无效".to_string())?;
    let user_agent = chrome_session::chrome_user_agent();
    let requested_profile_id = profile_id.clone();
    let (system_type, mut profile_ids, cached_model_keys) =
        if let Some(site_id) = site_id.as_deref() {
            let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
            let system_type = connection
                .query_row(
                    "SELECT system_type FROM directory_sites WHERE id = ?1",
                    [site_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap_or_default();
            // 请求指定账号时，即使账号额度状态暂时无效，也要允许使用已有缓存 Key 直连模型。
            let requested_profile = requested_profile_id.as_deref().unwrap_or_default();
            let mut statement = connection
                .prepare(
                    "SELECT profile_id FROM site_accounts
                     WHERE site_id = ?1 AND (is_valid = 1 OR profile_id = ?2)
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
                    let key_groups =
                        serde_json::from_str::<HashMap<String, String>>(&groups_json)
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
    let home_dir = app
        .path()
        .home_dir()
        .map_err(|error| format!("无法定位用户目录：{error}"))?;
    let origin = base_url.origin().ascii_serialization();
    let local_targets = site_id
        .as_ref()
        .map(|site_id| {
            profile_ids
                .iter()
                .map(|profile_id| chrome_local_storage::LocalStorageTarget {
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
        tauri::async_runtime::spawn_blocking(move || {
            chrome_local_storage::read_local_storage_from_home(&local_home, &local_targets)
        })
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

    for profile_id in &profile_ids {
        let values = local_values.get(profile_id).cloned().unwrap_or_default();
        let inferred_type = if system_type.trim().is_empty() {
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
            let token_url = base_url
                .join("/api/token/?p=1&size=20")
                .map_err(|_| "无法生成 /api/token 地址")?;

            let (cached_token, cached_uid) = if let Some(site_id) = site_id.as_deref() {
                let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
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

            // 已同步过的 NewAPI 访问秘钥优先直连 /v1/models，避免因为额度/签到状态异常再次弹出浏览器。
            if let Some((cached_keys, cached_key_groups)) = cached_model_keys
                .get(profile_id)
                .filter(|(keys, _)| !keys.is_empty())
            {
                match fetch_models_with_keys(
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
                    Ok(result) => {
                        return cache_profile_api_counts(
                            &database,
                            site_id.as_deref(),
                            requested_profile_id.as_deref(),
                            result,
                        );
                    }
                    Err(error) => errors.push(format!(
                        "{profile_id}：已有访问秘钥无法获取模型，继续检查其他认证方式：{error}"
                    )),
                }
            }

            let mut used_cached_token = false;
            let mut auth = if !cached_token.is_empty() {
                used_cached_token = true;
                Some(NewApiAuth::Token {
                    access_token: cached_token,
                    user_id: cached_uid,
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
                    let cookie_header_result = tauri::async_runtime::spawn_blocking(move || {
                        chrome_session::read_chrome_cookie_header_from_home(
                            &cookie_home,
                            &cookie_target,
                            &cookie_profile,
                        )
                    })
                    .await
                    .map_err(|error| format!("读取 Chrome Cookie 任务失败：{error}"));

                    let cookie_header_str = match cookie_header_result {
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

                    if cookie_header_has_name(&cookie_header_str, "new_api_refresh") {
                        errors.push(format!(
                            "{profile_id}：refresh cookie 必须通过 Chrome 同源请求刷新，以便写回浏览器"
                        ));
                        continue;
                    } else {
                        let user_id = match newapi_user_id(&values) {
                            Some(u) => u,
                            None => {
                                errors.push(format!("{profile_id}：旧版 NewAPI 本地 user 缺少用户 ID"));
                                continue;
                            }
                        };
                        model_user_id = user_id.clone();
                        NewApiAuth::Legacy {
                            cookie_header: cookie_header_str,
                            user_id: user_id.clone(),
                        }
                    }
                }};
            }

            if auth.is_none() {
                let legacy_auth = require_legacy_auth!();
                match try_acquire_newapi_token(&client, &base_url, &legacy_auth, &user_agent).await
                {
                    Ok(Some(token)) => {
                        auth = Some(NewApiAuth::Token {
                            access_token: token,
                            user_id: match &legacy_auth {
                                NewApiAuth::Legacy { user_id, .. }
                                | NewApiAuth::Token { user_id, .. } => user_id.clone(),
                            },
                        });
                    }
                    Ok(None) => {
                        auth = Some(legacy_auth);
                    }
                    Err(e) => {
                        errors.push(format!("{profile_id}：{e}"));
                        continue;
                    }
                }
            }

            let mut auth = auth.unwrap();

            // 访问令牌只用于 NewAPI 账号与 Key 管理接口；模型接口必须使用 NewAPI Key。
            // 已有缓存 Key 已在前面优先验证；没有可用 Key 时，才用访问令牌读取 Key 列表与明文 Key。
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
                    no_browser_fallback_profiles.insert(profile_id.clone());
                    errors.push(format!(
                        "{profile_id}：缓存访问令牌请求失败，不执行 refresh token：{error}"
                    ));
                    continue;
                }
            }

            if remote_result.is_err() && used_cached_token {
                let legacy_auth = require_legacy_auth!();
                match try_acquire_newapi_token(&client, &base_url, &legacy_auth, &user_agent).await
                {
                    Ok(Some(token)) => {
                        auth = NewApiAuth::Token {
                            access_token: token,
                            user_id: match &legacy_auth {
                                NewApiAuth::Legacy { user_id, .. }
                                | NewApiAuth::Token { user_id, .. } => user_id.clone(),
                            },
                        };
                    }
                    Ok(None) => {
                        auth = legacy_auth;
                    }
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
                            merge_api_keys(&mut discovered_keys, keys.iter().cloned());
                            merge_api_key_groups(
                                &mut discovered_key_groups,
                                key_groups.clone(),
                            );
                            match fetch_models_with_keys(
                                &client,
                                &base_url,
                                keys.clone(),
                                keys,
                                key_groups,
                                &user_agent,
                                "newapi-key",
                                (!model_user_id.is_empty()).then_some(model_user_id.as_str()),
                            )
                            .await
                            {
                                Ok(result) => {
                                    return cache_profile_api_counts(
                                        &database,
                                        site_id.as_deref(),
                                        requested_profile_id.as_deref(),
                                        result,
                                    )
                                }
                                Err(error) => {
                                    if used_cached_token {
                                        no_browser_fallback_profiles.insert(profile_id.clone());
                                    }
                                    errors.push(format!("{profile_id}：{error}"));
                                }
                            }
                        }
                        Err(error) => {
                            if used_cached_token {
                                no_browser_fallback_profiles.insert(profile_id.clone());
                            }
                            errors.push(format!("{profile_id}：{error}"));
                        }
                    }
                }
                Err(error) => errors.push(format!("{profile_id}：{error}")),
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
            // 已有访问秘钥（auth_token）优先直接使用：用它同步模型列表，
            // 不再通过 /api/v1/keys 获取 Key。只有直接同步失败才回落到 Key 接口。
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
                match request_json(request, "Sub2API 模型接口").await {
                    Ok(value) => {
                        let models = parse_site_models(&value);
                        if !models.is_empty() {
                            merge_api_keys(&mut discovered_keys, [auth_token.clone()]);
                            return cache_profile_api_counts(
                                &database,
                                site_id.as_deref(),
                                requested_profile_id.as_deref(),
                                SiteModelsResult {
                                    models,
                                    source: "sub2api-key".into(),
                                    keys: vec![auth_token.clone()],
                                    key_groups: HashMap::new(),
                                },
                            );
                        }
                        direct_errors.push("访问秘钥获取的模型列表为空".to_string());
                    }
                    Err(error) => direct_errors.push(error),
                }
            }
            if !direct_errors.is_empty() {
                errors.push(format!(
                    "{profile_id}：直接使用访问秘钥同步失败（{}），回落到 Key 接口",
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
            match request_json(request, "Sub2API Key 接口").await {
                Ok(value) => {
                    let visible_keys = parse_api_keys(&value);
                    let visible_key_groups = parse_api_key_groups(&value);
                    merge_api_keys(&mut discovered_keys, visible_keys.iter().cloned());
                    merge_api_key_groups(
                        &mut discovered_key_groups,
                        visible_key_groups.clone(),
                    );
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
                        Ok(result) => {
                            return cache_profile_api_counts(
                                &database,
                                site_id.as_deref(),
                                requested_profile_id.as_deref(),
                                result,
                            )
                        }
                        Err(error) => errors.push(format!("{profile_id}：{error}")),
                    }
                }
                Err(error) => errors.push(format!("{profile_id}：{error}")),
            }
        }
    }

    for profile_id in &profile_ids {
        if no_browser_fallback_profiles.contains(profile_id) {
            continue;
        }
        let values = local_values.get(profile_id).cloned().unwrap_or_default();
        let inferred_type = if system_type.trim().is_empty() {
            if parse_sub2api_local_account(&values).is_ok() {
                "sub2api"
            } else {
                "new-api"
            }
        } else {
            system_type.as_str()
        };
        // 类型已有明确值（Sub2API/0v0）：不走 Chrome 兜底。
        // Sub2API 靠 auth_token 直连接口，Chrome 兜底脚本同样需要 auth_token；
        // 本地没有就直接报错跳过，避免无意义地弹出浏览器。
        if matches!(
            inferred_type.trim().to_ascii_lowercase().as_str(),
            "sub2api" | "0v0"
        ) {
            errors.push(format!(
                "{profile_id}：{inferred_type} 不通过 Chrome 兜底，已在前面按类型直连"
            ));
            continue;
        }
        let legacy_user_id = newapi_user_id(&values);
        if legacy_user_id.is_some() {
            let marker = format!(
                "openhub-models-silent-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            let script =
                chrome_models_bridge_script(inferred_type, legacy_user_id.as_deref(), &marker);
            let silent_base_url = base_url.to_string();
            match tauri::async_runtime::spawn_blocking(move || {
                chrome_session::run_javascript_in_existing_chrome_tab(
                    &silent_base_url,
                    &script,
                    Duration::from_secs(8),
                )
            })
            .await
            .map_err(|error| format!("Chrome 静默模型同步任务失败：{error}"))?
            {
                Ok(Some(value)) => {
                    merge_api_keys(&mut discovered_keys, parse_chrome_models_keys(&value));
                    match parse_chrome_models_result(&value) {
                        Ok(result) => {
                            return cache_profile_api_counts(
                                &database,
                                site_id.as_deref(),
                                requested_profile_id.as_deref(),
                                result,
                            )
                        }
                        Err(error) => errors.push(format!("{profile_id} 静默请求：{error}")),
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    if chrome_session::is_blocking_chrome_automation_error(&error) {
                        return Err(error);
                    }
                    errors.push(format!("{profile_id} 静默请求：{error}"));
                }
            }
        }
        let background_marker = format!(
            "openhub-models-background-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let background_script = chrome_models_bridge_script(
            inferred_type,
            legacy_user_id.as_deref(),
            &background_marker,
        );
        let background_url = base_url
            .join(&format!("/#{}", background_marker))
            .map_err(|_| "无法生成 Chrome 后台模型同步地址")?
            .to_string();
        let background_profile = profile_id.clone();
        let background_marker_for_task = background_marker.clone();
        match tauri::async_runtime::spawn_blocking(move || {
            chrome_session::run_javascript_in_background_chrome_profile(
                &background_url,
                &background_profile,
                &background_marker_for_task,
                &background_script,
                Duration::from_secs(15),
            )
        })
        .await
        .map_err(|error| format!("Chrome 后台模型同步任务失败：{error}"))?
        {
            Ok(value) => {
                merge_api_keys(&mut discovered_keys, parse_chrome_models_keys(&value));
                match parse_chrome_models_result(&value) {
                    Ok(result) => {
                        return cache_profile_api_counts(
                            &database,
                            site_id.as_deref(),
                            requested_profile_id.as_deref(),
                            result,
                        )
                    }
                    Err(error) => errors.push(format!("{profile_id} 后台请求：{error}")),
                }
            }
            Err(error) => {
                if chrome_session::is_blocking_chrome_automation_error(&error) {
                    return Err(error);
                }
                errors.push(format!("{profile_id} 后台请求：{error}"));
            }
        }
        let marker = format!(
            "openhub-models-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let script = chrome_models_bridge_script(inferred_type, legacy_user_id.as_deref(), &marker);
        let target_url = base_url
            .join(&format!("/#{}", marker))
            .map_err(|_| "无法生成 Chrome 模型同步地址")?
            .to_string();
        let bridge_profile = profile_id.clone();
        let bridge_marker = marker.clone();
        match tauri::async_runtime::spawn_blocking(move || {
            chrome_session::run_javascript_in_chrome_profile(
                &target_url,
                &bridge_profile,
                &bridge_marker,
                &script,
                Duration::from_secs(30),
            )
        })
        .await
        .map_err(|error| format!("Chrome 模型同步任务失败：{error}"))?
        {
            Ok(value) => {
                merge_api_keys(&mut discovered_keys, parse_chrome_models_keys(&value));
                match parse_chrome_models_result(&value) {
                    Ok(result) => {
                        return cache_profile_api_counts(
                            &database,
                            site_id.as_deref(),
                            requested_profile_id.as_deref(),
                            result,
                        )
                    }
                    Err(error) => errors.push(format!("{profile_id}：{error}")),
                }
            }
            Err(error) => {
                if chrome_session::is_blocking_chrome_automation_error(&error) {
                    return Err(error);
                }
                errors.push(format!("{profile_id}：{error}"));
            }
        }
    }

    let pricing_url = base_url
        .join("/api/pricing")
        .map_err(|_| "无法生成 /api/pricing 地址")?;
    match request_json(
        chrome_request_headers(client.get(pricing_url), base_url.as_str(), &user_agent),
        "公开模型接口",
    )
    .await
    {
        Ok(value) => {
            let models = parse_site_models(&value);
            if !models.is_empty() {
                return cache_profile_api_counts(
                    &database,
                    site_id.as_deref(),
                    requested_profile_id.as_deref(),
                    SiteModelsResult {
                        models,
                        source: "pricing".into(),
                        keys: discovered_keys.clone(),
                        key_groups: discovered_key_groups.clone(),
                    },
                );
            }
            errors.push("/api/pricing 返回空模型列表".into());
        }
        Err(error) => errors.push(error),
    }

    let models_url = base_url
        .join("/v1/models")
        .map_err(|_| "无法生成 /v1/models 地址")?;
    match request_json(
        chrome_request_headers(client.get(models_url), base_url.as_str(), &user_agent),
        "无鉴权模型接口",
    )
    .await
    {
        Ok(value) => {
            let models = parse_site_models(&value);
            if !models.is_empty() {
                return cache_profile_api_counts(
                    &database,
                    site_id.as_deref(),
                    requested_profile_id.as_deref(),
                    SiteModelsResult {
                        models,
                        source: "models".into(),
                        keys: discovered_keys,
                        key_groups: discovered_key_groups,
                    },
                );
            }
            errors.push("/v1/models 返回空模型列表".into());
        }
        Err(error) => errors.push(error),
    }
    if !discovered_keys.is_empty() {
        let source = if is_sub2api(&system_type) {
            "sub2api-key"
        } else {
            "newapi-key"
        };
        return cache_profile_api_counts(
            &database,
            site_id.as_deref(),
            requested_profile_id.as_deref(),
            SiteModelsResult {
                models: Vec::new(),
                source: source.into(),
                keys: discovered_keys,
                key_groups: discovered_key_groups,
            },
        );
    }
    errors.dedup();
    if errors.is_empty() {
        Err("站点没有返回可用模型".into())
    } else {
        Err(format!(
            "获取模型失败：{}",
            errors.into_iter().take(4).collect::<Vec<_>>().join("；")
        ))
    }
}
