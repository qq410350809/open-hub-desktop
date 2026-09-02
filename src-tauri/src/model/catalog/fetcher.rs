use crate::context::{home_dir, spawn_blocking, AppContext, Managed};
use crate::db::*;
use crate::models::*;
use crate::proxypool;
use crate::site::library::{is_newapi, is_newapi_refresh, is_sub2api};
use crate::site::sync;
use crate::site::sync::*;
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
    let Some(items) = json_array_at(
        value,
        &["", "/data", "/data/items", "/items", "/result/items"],
    ) else {
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
    })
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
    // 同步模型（preserve_keys）时：保留库中已有 Key/分组；拉取失败时模型数据也一并保留，
    // 只更新错误信息，避免把左侧 Key 树或右侧模型列表清空。
    let existing = if preserve_keys {
        connection
            .query_row(
                "SELECT keys_json, groups_json, models_json, key_models_json, api_source
                 FROM site_model_cache WHERE site_id = ?1 AND profile_id = ?2",
                params![site_id, account.profile_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
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
            existing.as_ref().and_then(|(_, _, _, key_models_json, _)| {
                serde_json::from_str(key_models_json).ok()
            })
        })
        .unwrap_or_default();

    // 关键清理：移除已经不在 keys 中的旧 Key（保证删除的 Key 彻底从分组与模型映射中移除）
    let key_set: HashSet<&str> = keys.iter().map(String::as_str).collect();
    key_groups.retain(|k, _| key_set.contains(k.as_str()));
    key_models.retain(|k, _| key_set.contains(k.as_str()));

    let api_source = result
        .map(|item| item.source.clone())
        .or_else(|| {
            existing
                .as_ref()
                .and_then(|(_, _, _, _, source)| (!source.is_empty()).then(|| source.clone()))
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
                account.profile_name,
                account.account_name,
                account.username,
                api_source,
                serde_json::to_string(&keys).map_err(|error| error.to_string())?,
                serde_json::to_string(&key_groups).map_err(|error| error.to_string())?,
                serde_json::to_string(&models).map_err(|error| error.to_string())?,
                serde_json::to_string(&key_models).map_err(|error| error.to_string())?,
                persisted_error,
            ],
        )
        .map_err(|error| error.to_string())?;

    if result.is_some() && account.error.is_empty() {
        let _ = connection.execute(
            "UPDATE site_accounts 
             SET is_valid = 1, 
                 api_sync_error = '', 
                 sync_error = CASE 
                     WHEN sync_error LIKE '%NewAPI%' OR sync_error LIKE '%权限不足%' OR sync_error LIKE '%失效%' 
                     THEN '' 
                     ELSE sync_error 
                 END 
             WHERE site_id = ?1 AND profile_id = ?2",
            params![site_id, account.profile_id],
        );
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
    // 逐 Key 拉取：复用 fetch_models_with_keys 的候选与解析逻辑。
    // 不带站点会话 Cookie，仅 Bearer Key，与手动管理的语义一致。
    let result = fetch_models_with_keys(
        &client,
        &base_url,
        all_keys.clone(),
        all_keys,
        all_groups,
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/133.0.0.0 Safari/537.36",
        "models",
        None,
    )
    .await;

    // 写回每个账号行：本行 Key 中拉到模型的写入 key_models_json；整站模型列表
    // = 各 Key 结果合并。拉取整体失败（如全部 401）也保留已有数据，只把错误
    // 记到站点级行。
    let (key_models, _models, errors, _source) = match &result {
        Ok(result) => (
            result.key_models.clone(),
            result.models.clone(),
            result.errors.clone(),
            result.source.clone(),
        ),
        Err(error) => (
            HashMap::new(),
            Vec::new(),
            vec![error.clone()],
            "models".into(),
        ),
    };
    {
        let connection = database.lock_conn()?;
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
    // 整体失败时向前端报告，同时库中已尽量保留旧数据。
    result
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
    let trimmed = key.trim().to_string();
    if trimmed.is_empty() {
        return Err("Key 不能为空".to_string());
    }
    let group = {
        let raw = group_name.unwrap_or_default().trim().to_string();
        if raw.is_empty() {
            "默认分组".to_string()
        } else {
            raw
        }
    };
    let database = &*ctx.database;
    let connection = database.lock_conn()?;
    let row: Option<(String, String)> = connection
        .query_row(
            "SELECT keys_json, groups_json FROM site_model_cache WHERE site_id = ?1 AND profile_id = ?2",
            params![site_id, profile_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    // 行不存在时只允许站点级（profile_id 为空）自动建行：给任意 profile_id
    // 凭空造行会在会话列表里渲染出不存在的账号。
    let (row_exists, keys_json, groups_json) = match row {
        Some((keys_json, groups_json)) => (true, keys_json, groups_json),
        None if profile_id.is_empty() => (false, "[]".to_string(), "{}".to_string()),
        None => return Err("目标账号不存在：请先同步会话建立账号信息，再手动添加 Key".to_string()),
    };
    let mut keys: Vec<String> = serde_json::from_str(&keys_json).unwrap_or_default();
    if keys.iter().any(|item| item == &trimmed) {
        return Ok(false);
    }
    keys.push(trimmed);
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
                    profile_name.unwrap_or_default(),
                    username.unwrap_or_default(),
                ],
            )
            .map_err(|error| error.to_string())?;
    } else {
        connection
            .execute(
                "INSERT INTO site_model_cache (site_id, profile_id, keys_json, groups_json, error)
                 VALUES (?1, ?2, ?3, ?4, '')",
                params![
                    site_id,
                    profile_id,
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
            "SELECT profile_id, profile_name, account_name, username, api_source, keys_json, groups_json, models_json, key_models_json, error
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
            let key_models_json: String = row.get(8)?;
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
                error: row.get(9)?,
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
            "SELECT site_id, profile_id, profile_name, account_name, username, api_source, keys_json, groups_json, models_json, key_models_json, error
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
                    error: row.get(10)?,
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
                    },
                });
                entries.last_mut().expect("just pushed")
            }
        };
        if entry.cache.api_source.is_empty() && !api_source.is_empty() {
            entry.cache.api_source = api_source;
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

/// 单个站点 / 账号同步的硬性总超时：超过即强制失败。
const SITE_SYNC_TIMEOUT: Duration = Duration::from_secs(60);

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
    .map_err(|_| "站点模型同步超过 60 秒，已强制终止".to_string())?
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
                            merge_api_keys(&mut discovered_keys, keys.iter().cloned());
                            merge_api_key_groups(&mut discovered_key_groups, key_groups.clone());
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
                Err(error) => errors.push(format!("{profile_id}：{error}")),
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
                        Ok(result) => {
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
        }
    }

    let source = if is_sub2api(&system_type) {
        "sub2api-key"
    } else {
        "newapi-key"
    };
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
        },
    )
}
