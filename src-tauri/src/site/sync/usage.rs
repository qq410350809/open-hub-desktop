use crate::context::{spawn, spawn_blocking, AppContext, EventBus, Managed};
use crate::db::*;
use crate::models::*;
use crate::proxypool;
use crate::site::library::{
    access_token_is_user_owned, is_explicit_unknown, is_known_platform, is_newapi,
    is_newapi_refresh, is_platform, is_sub2api, uses_access_token,
};
use crate::site::sync;
use crate::site::sync::*;
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use serde_json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use url::Url;

/// 读取某账号已保存的站点令牌，供「设置站点令牌」回填。
///
/// 明文返回给调用方（界面放进密码框，默认掩码）：不回填的话用户打开就是空
/// 输入框，改一个字段顺手保存就会把原令牌清掉。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn get_site_account_token(
    ctx: Managed<'_, Arc<AppContext>>,
    site_id: String,
    profile_id: String,
) -> Result<SiteAccountToken, String> {
    let database = &*ctx.database;
    let connection = database.lock_conn()?;
    connection
        .query_row(
            "SELECT newapi_token, newapi_user_id FROM site_accounts
              WHERE site_id = ?1 AND profile_id = ?2",
            params![site_id, profile_id],
            |row| {
                Ok(SiteAccountToken {
                    token: row.get(0)?,
                    user_id: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(|error| error.to_string())
        .map(|value| {
            value.unwrap_or(SiteAccountToken {
                token: String::new(),
                user_id: String::new(),
            })
        })
}

/// 某账号已保存的站点令牌。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteAccountToken {
    pub(crate) token: String,
    pub(crate) user_id: String,
}

/// 手动维护某个账号的站点令牌（访问令牌）。
///
/// 自动同步取不到登录凭据时（站点改了登录流程、浏览器里已退出、Local Storage
/// 被清），用户可以自己从站点后台复制令牌贴进来，令牌与用户 ID 一起写入
/// `site_accounts`，之后的账号/Key/健康度同步就直接用它鉴权。
///
/// 只有用令牌鉴权的架构才允许写入（NewAPI 刷新令牌形态、白与黑）。Cookie 形态
/// 的鉴权只有浏览器会话 Cookie，没有「访问令牌」这回事——给它写一个不会生效的
/// 字段，只会让界面显示「有访问令牌」而实际仍在用 Cookie 同步，直接拒绝。
///
/// 令牌与用户 ID 都传空串表示清除：恢复成「完全依赖浏览器会话」。用户 ID
/// 允许留空——它只影响 NewAPI 的 `new-api-user` 头，令牌本身能鉴权时不影响。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn set_site_account_token(
    ctx: Managed<'_, Arc<AppContext>>,
    site_id: String,
    profile_id: String,
    token: String,
    user_id: Option<String>,
) -> Result<bool, String> {
    let database = &*ctx.database;
    let token = token.trim().to_string();
    let user_id = user_id.unwrap_or_default().trim().to_string();
    let connection = database.lock_conn()?;
    let system_type: String = connection
        .query_row(
            "SELECT system_type FROM directory_sites WHERE id = ?1",
            [site_id.as_str()],
            |row| row.get(0),
        )
        .unwrap_or_default();
    if !token.is_empty() && !uses_access_token(&system_type) {
        let label = crate::site::library::canonical_platform(&system_type);
        return Err(if label.is_empty() {
            "该站点不是用访问令牌鉴权的架构（Cookie 形态），没有可设置的站点令牌".to_string()
        } else {
            format!("{label} 不是用访问令牌鉴权的架构，没有可设置的站点令牌")
        });
    }
    let updated = connection
        .execute(
            "UPDATE site_accounts
                SET newapi_token = ?3,
                    newapi_user_id = ?4,
                    is_valid = 1,
                    sync_error = '',
                    updated_at = CURRENT_TIMESTAMP
              WHERE site_id = ?1 AND profile_id = ?2",
            params![site_id, profile_id, token, user_id],
        )
        .map_err(|error| error.to_string())?;
    if updated == 0 {
        return Err("未找到该账号记录，请先同步站点账号".to_string());
    }
    Ok(!token.is_empty())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn delete_site_account(
    ctx: Managed<'_, Arc<AppContext>>,
    site_id: String,
    profile_id: String,
) -> Result<(), String> {
    let database = &*ctx.database;
    let connection = database.lock_conn()?;
    let deleted = connection
        .execute(
            "DELETE FROM site_accounts WHERE site_id = ?1 AND profile_id = ?2",
            params![site_id, profile_id],
        )
        .map_err(|error| error.to_string())?;
    if deleted == 0 {
        return Err("未找到该账号记录".to_string());
    }
    // 连带清理该账号的模型/Key 缓存，避免残留旧数据。
    connection
        .execute(
            "DELETE FROM site_model_cache WHERE site_id = ?1 AND profile_id = ?2",
            params![site_id, profile_id],
        )
        .map_err(|error| error.to_string())?;
    // 该 profile 已无任何站点账号时，回收其代理出口 lane（修复既往实例泄漏）。
    // 注意 profile_id 可能被多个站点的账号共用，仅在最后一个引用消失时释放。
    let remaining: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM site_accounts WHERE profile_id = ?1",
            [profile_id.as_str()],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if remaining == 0 {
        ctx.proxy_runtime.release_account_lane(&profile_id);
    }
    Ok(())
}

/// 自动签到的站点范围：在用或待定，且站点支持签到。
///
/// 「待定」= 浏览器里有该站点会话、但还没归入在用——签到拿额度正是它转正的
/// 常见理由，所以一并放行。未在用（既非在用也非待定）不代执行每日签到这类
/// 写动作。额度刷新与签到范围不同：额度是只读探测，未在用站点也会被刷新。
pub(crate) const CHECKIN_SITE_IDS_SQL: &str = "SELECT id FROM directory_sites
     WHERE supports_checkin = 1 AND (is_personal = 1 OR is_pending = 1)";

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn mark_sites_with_chrome_sessions(
    ctx: Managed<'_, Arc<AppContext>>,
    site_id: Option<String>,
    site_ids: Option<Vec<String>>,
    run_id: Option<u64>,
    extract_only: Option<bool>,
    refresh_pending: Option<bool>,
) -> Result<ChromeUsageScanResult, String> {
    let database = &*ctx.database;
    let bus: EventBus = ctx.event_bus.clone();
    // extract_only=true：只提取浏览器是否有会话数据并标注待定，不探测站点类型、不刷新账号接口。
    let extract_only = extract_only.unwrap_or(false);
    // refresh_pending=true：额度同步时允许刷新待定站点，但不改变其使用状态。
    let refresh_pending = refresh_pending.unwrap_or(false);
    let site_id_was_supplied = site_id.is_some();
    let requested_site_id = site_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let site_ids_were_supplied = site_ids.is_some();
    let requested_site_ids = site_ids
        .unwrap_or_default()
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<HashSet<_>>();
    let has_site_scope = site_id_was_supplied || site_ids_were_supplied;
    let mut targets = {
        let connection = database.lock_conn()?;
        let mut statement = connection
            .prepare(
                "SELECT id, name, checkin_url, api_base_url, system_type
                 FROM directory_sites
                 WHERE TRIM(checkin_url) <> '' OR TRIM(api_base_url) <> ''",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| {
                let id = row.get::<_, String>(0)?;
                let name = row.get::<_, String>(1)?;
                let checkin_url = row.get::<_, String>(2)?;
                let api_base_url = row.get::<_, String>(3)?;
                let system_type = row.get::<_, String>(4)?;
                let mut urls = Vec::with_capacity(4);
                if !api_base_url.trim().is_empty() {
                    let account_paths: &[&str] = if is_newapi(&system_type) {
                        &["/api/user/auth/refresh", "/api/user/self"]
                    } else if is_sub2api(&system_type) {
                        &["/api/v1/auth/me"]
                    } else {
                        &[]
                    };
                    for account_url in account_paths
                        .iter()
                        .filter_map(|path| Url::parse(&api_base_url).ok()?.join(path).ok())
                    {
                        urls.push(account_url.to_string());
                    }
                    urls.push(api_base_url.clone());
                }
                if !checkin_url.trim().is_empty() && !urls.iter().any(|url| url == &checkin_url) {
                    // 签到地址只在“与 API 地址同主机”时才参与账号会话匹配。
                    // 跨主机的签到地址（常见为共享签到门户或第三方兑换站）只证明
                    // 访问过该地址，不能证明该 Chrome Profile 在本站有登录账号；
                    // 否则会把其他站点（甚至共享主机上的新 API 站点）的登录会话
                    // 串到本站名下，造成多出/重复/串站账号。
                    let host_of = |value: &str| {
                        Url::parse(value)
                            .ok()
                            .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
                    };
                    let same_host = api_base_url.trim().is_empty()
                        || host_of(&api_base_url).is_some_and(|host| {
                            host_of(&checkin_url).is_some_and(|other| other == host)
                        });
                    if same_host {
                        urls.push(checkin_url);
                    }
                }
                Ok((id, name, urls, api_base_url, system_type))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        rows.iter()
            .filter(|(id, _, _, _, _)| {
                site_matches_requested_scope(
                    id,
                    requested_site_id.as_deref(),
                    site_id_was_supplied,
                    &requested_site_ids,
                    site_ids_were_supplied,
                )
            })
            .map(|(id, name, urls, api_base_url, system_type)| {
                (
                    id.clone(),
                    name.clone(),
                    urls.clone(),
                    api_base_url.clone(),
                    system_type.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    // 跨主机签到地址（如 Fengwind 的签到在 api-welfalre.fengwind.com）：
    // 签到接口不在 API 主域上，且签到域的登录令牌键名不同（welfare_token），
    // 需要单独记录并读取该域的 Local Storage 才能查到签到状态。
    let cross_checkin_urls: HashMap<String, String> = {
        let host_of = |value: &str| {
            Url::parse(value)
                .ok()
                .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        };
        let connection = database.lock_conn()?;
        let mut statement = connection
            .prepare(
                "SELECT id, checkin_url, api_base_url, system_type
                 FROM directory_sites
                 WHERE TRIM(checkin_url) <> '' AND TRIM(api_base_url) <> ''",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        rows.into_iter()
            .filter(|(id, _, _, system_type)| {
                is_sub2api(system_type)
                    && site_matches_requested_scope(
                        id,
                        requested_site_id.as_deref(),
                        site_id_was_supplied,
                        &requested_site_ids,
                        site_ids_were_supplied,
                    )
            })
            .filter_map(|(id, checkin_url, api_base_url, _)| {
                // 同主机的签到地址直接用 API 主域探测，无需单独读取。
                let api_host = host_of(&api_base_url)?;
                let checkin_host = host_of(&checkin_url)?;
                (checkin_host != api_host).then_some((id, checkin_url))
            })
            .collect()
    };
    let account_refresh_site_ids = {
        let connection = database.lock_conn()?;
        let condition = if refresh_pending {
            "is_pending = 1"
        } else {
            "is_personal = 1"
        };
        let mut statement = connection
            .prepare(&format!("SELECT id FROM directory_sites WHERE {condition}"))
            .map_err(|error| error.to_string())?;
        let mut ids = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?
            .collect::<Result<HashSet<_>, _>>()
            .map_err(|error| error.to_string())?;
        if let Some(site_id) = &requested_site_id {
            ids.insert(site_id.clone());
        }
        for site_id in &requested_site_ids {
            ids.insert(site_id.clone());
        }
        ids
    };
    let checkin_site_ids = {
        let connection = database.lock_conn()?;
        let mut statement = connection
            .prepare(CHECKIN_SITE_IDS_SQL)
            .map_err(|error| error.to_string())?;
        let site_ids: HashSet<String> = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?
            .collect::<Result<HashSet<_>, _>>()
            .map_err(|error| error.to_string())?;
        site_ids
    };
    // 提取会话只比对浏览器数据，不做 /api/status 站点类型检测。
    // 已有 system_type 仅用于在用账号刷新路径，不影响待定标注。
    for (_, _, urls, api_base_url, system_type) in &mut targets {
        let account_paths: &[&str] = if is_newapi(system_type) {
            &["/api/user/self", "/api/user/auth/refresh"]
        } else if is_sub2api(system_type) {
            &["/api/v1/auth/me"]
        } else {
            &[]
        };
        for account_url in account_paths
            .iter()
            .filter_map(|path| Url::parse(api_base_url).ok()?.join(path).ok())
        {
            let account_url = account_url.to_string();
            if !urls.iter().any(|url| url == &account_url) {
                urls.insert(0, account_url);
            }
        }
    }
    emit_optional_sync_progress(
        &bus,
        run_id,
        "chrome-scan",
        "running",
        format!("开始提取 {} 个本地站点的 Chrome 会话数据", targets.len()),
    );
    let (current_month, previous_checkins, cached_accounts, cached_model_keys) = {
        let connection = database.lock_conn()?;
        let current_month: String = connection
            .query_row("SELECT strftime('%Y-%m', 'now', 'localtime')", [], |row| {
                row.get(0)
            })
            .map_err(|error| error.to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT site_id, profile_id, checkin_enabled,
                        CASE WHEN checkin_date = date('now', 'localtime') THEN checked_in_today ELSE 0 END,
                        checkin_error
                 FROM site_accounts",
            )
            .map_err(|error| error.to_string())?;
        let previous_checkins = statement
            .query_map([], |row| {
                Ok((
                    (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                    CheckinSnapshot {
                        enabled: row.get::<_, i64>(2)? != 0,
                        checked_in_today: row.get::<_, i64>(3)? != 0,
                        error: row.get(4)?,
                    },
                ))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<HashMap<_, _>, _>>()
            .map_err(|error| error.to_string())?;
        let cached_accounts = read_cached_usage_sites(&connection)?
            .into_iter()
            .flat_map(|site| {
                site.sessions.into_iter().map(move |session| {
                    ((site.site_id.clone(), session.profile_id.clone()), session)
                })
            })
            .collect::<HashMap<_, _>>();
        let mut key_statement = connection
            .prepare("SELECT site_id, profile_id, keys_json FROM site_model_cache")
            .map_err(|error| error.to_string())?;
        let cached_model_keys = key_statement
            .query_map([], |row| {
                let keys_json: String = row.get(2)?;
                let keys = serde_json::from_str::<Vec<String>>(&keys_json).unwrap_or_default();
                Ok(((row.get::<_, String>(0)?, row.get::<_, String>(1)?), keys))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<HashMap<_, _>, _>>()
            .map_err(|error| error.to_string())?;
        (
            current_month,
            previous_checkins,
            cached_accounts,
            cached_model_keys,
        )
    };
    let scanned = targets.len();
    let scan_targets = targets
        .iter()
        .map(|(id, _, urls, _, _)| (id.clone(), urls.clone()))
        .collect::<Vec<_>>();
    let home_dir = crate::context::home_dir().ok_or("无法定位用户目录")?;
    let mut matched_sites = if scanned == 0 {
        Vec::new()
    } else {
        let scan_home_dir = home_dir.clone();
        spawn_blocking(move || sync::site_sessions_from_home(&scan_home_dir, &scan_targets))
            .await
            .map_err(|error| format!("分析 Chrome 会话任务失败：{error}"))?
            .unwrap_or_default()
    };
    // 待定判定用“浏览器里是否有该站点会话”，不能用后面的账号候选强过滤。
    // Cookie 命中 或 后续 Local Storage 命中 都算有会话。
    let mut browser_session_site_ids = matched_sites
        .iter()
        .filter(|site| !site.sessions.is_empty())
        .map(|site| site.site_id.clone())
        .collect::<HashSet<_>>();
    let profiles = spawn_blocking({
        let home_dir = home_dir.clone();
        move || sync::profile_identities_from_home(&home_dir)
    })
    .await
    .map_err(|error| format!("读取 Chrome Profile 任务失败：{error}"))??;
    emit_optional_sync_progress(
        &bus,
        run_id,
        "chrome-profiles",
        "success",
        format!("已读取 {} 个 Chrome Profile", profiles.len()),
    );
    let local_targets = targets
        .iter()
        .flat_map(|(site_id, _, urls, api_base_url, _)| {
            let base_url = if api_base_url.trim().is_empty() {
                urls.first().cloned().unwrap_or_default()
            } else {
                api_base_url.clone()
            };
            let origin = Url::parse(&base_url)
                .ok()
                .map(|url| url.origin().ascii_serialization())
                .filter(|origin| origin != "null");
            profiles.iter().filter_map(move |profile| {
                Some(sync::LocalStorageTarget {
                    site_id: site_id.clone(),
                    profile_id: profile.id.clone(),
                    origin: origin.clone()?,
                })
            })
        })
        .collect::<Vec<_>>();
    let local_storage = spawn_blocking({
        let home_dir = home_dir.clone();
        move || sync::read_local_storage_from_home(&home_dir, &local_targets)
    })
    .await
    .map_err(|error| format!("读取 Chrome Local Storage 任务失败：{error}"))?
    .into_iter()
    .map(|item| ((item.site_id, item.profile_id), (item.values, item.error)))
    .collect::<HashMap<_, _>>();
    emit_optional_sync_progress(
        &bus,
        run_id,
        "chrome-local-storage",
        "success",
        format!(
            "已分析 {} 组站点与 Profile 的 Local Storage",
            local_storage.len()
        ),
    );
    // 跨主机签到域单独读一份 Local Storage（welfare_token 等签到域令牌在 API
    // 主域的桶里读不到）；键仍是 (site_id, profile_id)，仅签到阶段使用。
    let checkin_local_storage: HashMap<
        (String, String),
        (HashMap<String, String>, String),
    > = {
        let checkin_local_targets = cross_checkin_urls
            .iter()
            .filter_map(|(site_id, checkin_url)| {
                let origin = Url::parse(checkin_url)
                    .ok()
                    .map(|url| url.origin().ascii_serialization())
                    .filter(|origin| origin != "null")?;
                Some((site_id.clone(), origin))
            })
            .flat_map(|(site_id, origin)| {
                profiles.iter().map(move |profile| sync::LocalStorageTarget {
                    site_id: site_id.clone(),
                    profile_id: profile.id.clone(),
                    origin: origin.clone(),
                })
            })
            .collect::<Vec<_>>();
        if checkin_local_targets.is_empty() {
            HashMap::new()
        } else {
            spawn_blocking({
                let home_dir = home_dir.clone();
                move || sync::read_local_storage_from_home(&home_dir, &checkin_local_targets)
            })
            .await
            .map_err(|error| format!("读取 Chrome Local Storage 任务失败：{error}"))?
            .into_iter()
            .map(|item| ((item.site_id, item.profile_id), (item.values, item.error)))
            .collect::<HashMap<_, _>>()
        }
    };
    let profile_map = profiles
        .into_iter()
        .map(|profile| (profile.id.clone(), profile))
        .collect::<HashMap<_, _>>();

    // 站点类型冻结：已入库站点的 system_type 一律以库中存储值为准，
    // 扫描（含自动调度）不再推断、不再纠正、不写库。类型只允许两个来源：
    // 新增站点时的识别（远端同步/URL 导入）与用户在编辑表单里的手工修改。
    // 会话行为全部按每个账号的实际证据判定（refresh cookie、Local Storage 键），
    // 与库中存储的基础类型无关，因此冻结类型不影响同步语义。

    let account_targets = targets
        .iter()
        .map(|(id, name, urls, api_base_url, system_type)| {
            let base_url = if api_base_url.trim().is_empty() {
                urls.first().cloned().unwrap_or_default()
            } else {
                api_base_url.clone()
            };
            (id.clone(), (base_url, system_type.clone(), name.clone()))
        })
        .collect::<HashMap<_, _>>();

    matched_sites.retain_mut(|site| {
        let system_type = account_targets
            .get(&site.site_id)
            .map(|(_, system_type, _)| system_type.as_str())
            .unwrap_or_default();
        site.sessions.retain(|session| {
            let local_values = local_storage
                .get(&(site.site_id.clone(), session.profile_id.clone()))
                .filter(|(_, error)| error.is_empty())
                .map(|(values, _)| values);
            // 没有会话信息就不该出现这个账号：结构化账号数据与登录类 Cookie
            // 两者皆无的 Chrome 配置直接剔除，免得站点卡片上留一个点什么都做不了
            // 的空账号（同步时只会反复报「没有可用凭据」）。
            has_browser_session_evidence(system_type, local_values, &session.cookie_names)
        });
        !site.sessions.is_empty()
    });

    for ((site_id, profile_id), (values, _)) in &local_storage {
        let Some((base_url, _system_type, _)) = account_targets.get(site_id) else {
            continue;
        };
        // 只有 UI 噪音键（`iconify*` / `theme` …）的桶不算会话痕迹：站点前端
        // 写几个图标缓存就会凭空多出一个空账号；真正与登录有关的键（含残缺
        // 数据）仍作为会话候选加入，交给账号接口验证。
        if values.is_empty() || !has_session_storage_keys(values) {
            continue;
        }
        let Some(profile) = profile_map.get(profile_id) else {
            continue;
        };
        let domain = Url::parse(base_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string))
            .unwrap_or_default();
        let site_index = matched_sites
            .iter()
            .position(|site| site.site_id == *site_id)
            .unwrap_or_else(|| {
                matched_sites.push(sync::ChromeSiteSessionMatch {
                    site_id: site_id.clone(),
                    sessions: Vec::new(),
                });
                matched_sites.len() - 1
            });
        if matched_sites[site_index]
            .sessions
            .iter()
            .any(|session| session.profile_id == *profile_id)
        {
            continue;
        }
        matched_sites[site_index]
            .sessions
            .push(sync::ChromeSessionInfo {
                profile_id: profile.id.clone(),
                domain,
                cookie_count: 0,
                cookie_names: Vec::new(),
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
    }

    // 账号缓存在重启/重新扫描前是被下面“全删后重插”覆盖的，这里先把缓存里的
    // 签到/余额/令牌并回扫描结果，避免上一轮的账号业务数据被无会话结果冲掉。
    for site in &mut matched_sites {
        // 令牌要并回扫描结果的架构：newapi2，以及「令牌由用户维护」的白与黑。
        // 不并回来，下面 INSERT OR REPLACE 整行重写时就把用户填的令牌清成空串。
        let keeps_access_token = account_targets
            .get(&site.site_id)
            .is_some_and(|(_, system_type, _)| uses_access_token(system_type));
        for session in &mut site.sessions {
            let Some(cached) =
                cached_accounts.get(&(site.site_id.clone(), session.profile_id.clone()))
            else {
                continue;
            };
            session.remaining = session.remaining.or(cached.remaining);
            session.used = session.used.or(cached.used);
            session.total = session.total.or(cached.total);
            session.unit = if session.unit.is_empty() && !cached.unit.is_empty() {
                cached.unit.clone()
            } else {
                session.unit.clone()
            };
            session.username = if session.username.is_empty() && !cached.username.is_empty() {
                cached.username.clone()
            } else {
                session.username.clone()
            };
            session.is_valid = session.is_valid || cached.is_valid;
            session.sync_error = if session.sync_error.is_empty() {
                cached.sync_error.clone()
            } else {
                session.sync_error.clone()
            };
            // 签到状态：缓存在读取时已按当天折算（昨天的签到会被归零），
            // 因此扫描结果缺失时可直接沿用缓存，避免重启后签到记录丢失。
            session.checked_in_today = session.checked_in_today || cached.checked_in_today;
            session.checkin_enabled = session.checkin_enabled || cached.checkin_enabled;
            session.checkin_error = if session.checkin_error.is_empty() {
                cached.checkin_error.clone()
            } else {
                session.checkin_error.clone()
            };
            session.account_updated_at = if session.account_updated_at.is_empty() {
                cached.account_updated_at.clone()
            } else {
                session.account_updated_at.clone()
            };
            session.browser_fallback_cooldown_ms = cached.browser_fallback_cooldown_ms;
            session.browser_fallback_failed_at = cached.browser_fallback_failed_at;
            session.browser_fallback_fail_count = cached.browser_fallback_fail_count;
            if keeps_access_token
                && session.newapi_token.is_empty()
                && !cached.newapi_token.is_empty()
            {
                session.newapi_token = cached.newapi_token.clone();
                session.has_access_token = true;
            }
            if session.newapi_user_id.is_empty() {
                session.newapi_user_id = cached.newapi_user_id.clone();
            }
        }
    }

    // —— 白与黑：无凭据账号在会话装配阶段直接剔除，一开始就是最终集合 ——
    // 它的登录凭据只可能来自浏览器登录态（该 API 域的 Cookie）或用户手填的
    // 「站点令牌」，两者皆无的会话没有任何可请求的通道。这里在建单/写库之前
    // 就把这类会话从集合里拿掉：后续同步循环遇到的直接就是真实账号（不再
    // 先全量出来、循环里再逐个跳过），账号列表也不会保留空行；被剔除账号的
    // 历史残留行由写库阶段的「全删后重插」顺带清掉。
    for site in &mut matched_sites {
        let Some((base_url, system_type, _)) = account_targets.get(&site.site_id) else {
            continue;
        };
        if !is_platform(system_type, "baiheibai") {
            continue;
        }
        let base_url = base_url.clone();
        let mut kept_sessions = Vec::new();
        for session in std::mem::take(&mut site.sessions) {
            // 已配置站点令牌：无需浏览器登录态，直接保留。
            if !session.newapi_token.trim().is_empty() {
                kept_sessions.push(session);
                continue;
            }
            // 没有令牌则读该 API 域的登录 Cookie：读不到任何 Cookie 才剔除。
            let cookie_home_dir = home_dir.clone();
            let cookie_url = base_url.clone();
            let profile_id_for_cookie = session.profile_id.clone();
            let has_cookie = spawn_blocking(move || {
                sync::read_chrome_cookie_header_from_home(
                    &cookie_home_dir,
                    &cookie_url,
                    &profile_id_for_cookie,
                )
            })
            .await
            .ok()
            .and_then(|result| result.ok())
            .unwrap_or_default();
            if !has_cookie.trim().is_empty() {
                kept_sessions.push(session);
            }
        }
        site.sessions = kept_sessions;
    }

    // Local Storage 里能解析出账号的，也视为浏览器有会话（即使 Cookie 查询因 path/分区漏掉）。
    for ((site_id, _), (values, error)) in &local_storage {
        if error.is_empty() && !values.is_empty() {
            browser_session_site_ids.insert(site_id.clone());
        }
    }
    // 再补一层：只要 Cookie 扫描到任意该域会话，就保留（不依赖 new_api_refresh / local account）。
    for site in &matched_sites {
        if !site.sessions.is_empty() {
            browser_session_site_ids.insert(site.site_id.clone());
        }
    }

    let candidate_sites = matched_sites.len();
    let candidate_accounts = matched_sites
        .iter()
        .map(|site| site.sessions.len())
        .sum::<usize>();
    let browser_session_count = browser_session_site_ids.len();
    emit_optional_sync_progress(
        &bus,
        run_id,
        "chrome-scan",
        "success",
        format!(
            "Chrome 扫描完成：浏览器会话站点 {browser_session_count} 个，账号候选 {candidate_sites} 个 / {candidate_accounts} 个会话"
        ),
    );
    if !extract_only && !matched_sites.is_empty() {
        let chrome_user_agent = sync::chrome_user_agent();
        let mut jobs = Vec::new();
        for (site_index, site) in matched_sites.iter().enumerate() {
            // 额度/签到接口只刷新“在用”站点，避免全库会话比对时打爆外部接口。
            if !account_refresh_site_ids.contains(&site.site_id) {
                continue;
            }
            let Some((base_url, system_type, site_name)) = account_targets.get(&site.site_id)
            else {
                continue;
            };
            // 未知架构站点：没有可识别的签到/余额接口，不调用账号接口探测与刷新，
            // 只在下方写库块把 Chrome 账号会话持久化为站点关联（只同步账号）。
            if !is_known_platform(system_type) {
                if !site.sessions.is_empty() {
                    emit_optional_sync_progress(
                        &bus,
                        run_id,
                        &format!("chrome-account-skip-{site_index}"),
                        "info",
                        format!("{site_name}：未知架构，仅同步 Chrome 账号，跳过余额与签到"),
                    );
                }
                continue;
            }
            for (session_index, session) in site.sessions.iter().enumerate() {
                let base_url = base_url.clone();
                let system_type = system_type.clone();
                let user_agent = chrome_user_agent.clone();
                let profile_id = session.profile_id.clone();
                let profile_label = if session.account_name.is_empty() {
                    session.profile_name.clone()
                } else {
                    format!("{} · {}", session.profile_name, session.account_name)
                };
                let has_refresh_cookie =
                    has_newapi_refresh_cookie_name(session.cookie_names.iter().map(String::as_str));
                let use_refresh_auth = is_newapi_refresh(&system_type);
                let auth_label = if is_newapi(&system_type) {
                    if use_refresh_auth {
                        "刷新令牌认证"
                    } else {
                        "传统会话认证"
                    }
                } else {
                    "本地会话认证"
                };
                let site_name = site_name.clone();
                let progress_stage = format!("chrome-account-{site_index}-{session_index}");
                emit_optional_sync_progress(
                    &bus,
                    run_id,
                    &progress_stage,
                    "running",
                    format!("正在同步 {site_name} · Chrome {profile_label}（{auth_label}）"),
                );
                let site_id = site.site_id.clone();
                let should_checkin = checkin_site_ids.contains(&site.site_id);
                let current_month = current_month.clone();
                let previous_checkin = previous_checkins
                    .get(&(site_id.clone(), profile_id.clone()))
                    .cloned()
                    .unwrap_or_default();
                let cookie_home_dir = home_dir.clone();
                // Cookie 要按目标接口定域与路径：白与黑的额度接口认的是浏览器会话，
                // NewAPI 系按认证形态读 /api/user/self 或 refresh。
                let cookie_endpoint = if is_platform(&system_type, "baiheibai") {
                    "/api/user/profile"
                } else if use_refresh_auth {
                    "/api/user/auth/refresh"
                } else {
                    "/api/user/self"
                };
                let cookie_base_url = Url::parse(&base_url)
                    .ok()
                    .and_then(|url| url.join(cookie_endpoint).ok())
                    .map(|url| url.to_string())
                    .unwrap_or_else(|| base_url.clone());
                let (local_values, local_error) = local_storage
                    .get(&(site.site_id.clone(), session.profile_id.clone()))
                    .cloned()
                    .unwrap_or_else(|| {
                        (
                            HashMap::new(),
                            "Chrome Local Storage 中没有该站点的数据".into(),
                        )
                    });
                // 跨主机签到：签到地址与签到域令牌一并交给刷新阶段。
                let checkin_url = cross_checkin_urls
                    .get(&site.site_id)
                    .cloned()
                    .unwrap_or_default();
                let checkin_local_values = if checkin_url.is_empty() {
                    HashMap::new()
                } else {
                    checkin_local_storage
                        .get(&(site.site_id.clone(), session.profile_id.clone()))
                        .filter(|(_, error)| error.is_empty())
                        .map(|(values, _)| values.clone())
                        .unwrap_or_default()
                };
                let cached_token = if session.newapi_token.is_empty() {
                    None
                } else {
                    Some(session.newapi_token.clone())
                };
                let cached_uid = if session.newapi_user_id.is_empty() {
                    None
                } else {
                    Some(session.newapi_user_id.clone())
                };
                let cached_sub2api_keys = cached_model_keys
                    .get(&(site_id.clone(), profile_id.clone()))
                    .cloned()
                    .unwrap_or_default();
                let job_database = ctx.database.clone();
                let job_runtime = ctx.proxy_runtime.clone();
                let job = spawn(async move {
                    // 显式「未知类型」不去猜架构：既不当 NewAPI 读 Cookie，
                    // 也不按 Local Storage 痕迹推断，避免与用户设置相矛盾。
                    // 白与黑的额度接口只认浏览器会话（访问令牌会被 403 拒），
                    // 所以它同样要读 Cookie；NewAPI 系按自己的认证形态读。
                    let needs_cookie = !is_explicit_unknown(&system_type)
                        && (is_newapi(&system_type)
                            || is_platform(&system_type, "baiheibai")
                            || (system_type.trim().is_empty()
                                && (parse_newapi_local_account(&local_values).is_ok()
                                    || has_refresh_cookie)));
                    let cookie_header = if needs_cookie {
                        let profile_id_for_cookie = profile_id.clone();
                        spawn_blocking(move || {
                            sync::read_chrome_cookie_header_from_home(
                                &cookie_home_dir,
                                &cookie_base_url,
                                &profile_id_for_cookie,
                            )
                        })
                        .await
                        .map_err(|error| format!("读取 Chrome Cookie 任务失败：{error}"))?
                        // 读到空串 = 这个 profile 下该域没有登录 Cookie。这是
                        // 「没有凭据」，不是「读取失败」，按前者报给账号行。
                        .and_then(|cookie| {
                            // 白与黑没有 Cookie 不算致命：登录态可能只是过期了，
                            // 交给刷新阶段给出具体提示（NewAPI 系则直接当无凭据）。
                            if cookie.trim().is_empty()
                                && !is_platform(&system_type, "baiheibai")
                            {
                                Err("没有找到可用的 NewAPI 登录凭据".to_string())
                            } else {
                                Ok(cookie)
                            }
                        })
                    } else {
                        Ok(String::new())
                    };
                    proxypool::with_account_proxy(
                        &job_database,
                        &job_runtime,
                        &site_id,
                        &profile_id,
                        Duration::from_secs(12),
                        3,
                        "账号同步请求",
                        move |client| {
                            let base_url = base_url.clone();
                            let system_type = system_type.clone();
                            let local_values = local_values.clone();
                            let local_error = local_error.clone();
                            let cookie_header = cookie_header.clone();
                            let user_agent = user_agent.clone();
                            let current_month = current_month.clone();
                            let cached_token = cached_token.clone();
                            let cached_uid = cached_uid.clone();
                            let cached_sub2api_keys = cached_sub2api_keys.clone();
                            let previous_checkin = previous_checkin.clone();
                            let checkin_url = checkin_url.clone();
                            let checkin_local_values = checkin_local_values.clone();
                            async move {
                                fetch_site_account(
                                    &client,
                                    &base_url,
                                    &system_type,
                                    &local_values,
                                    &local_error,
                                    cookie_header,
                                    &user_agent,
                                    &current_month,
                                    should_checkin,
                                    previous_checkin,
                                    cached_token,
                                    cached_uid,
                                    &cached_sub2api_keys,
                                    &checkin_url,
                                    &checkin_local_values,
                                )
                                .await
                            }
                        },
                    )
                    .await
                });
                jobs.push((
                    site_index,
                    session_index,
                    site_name,
                    profile_label,
                    progress_stage,
                    job,
                ));
            }
        }
        for (site_index, session_index, site_name, profile_label, progress_stage, job) in jobs {
            // 先取归属：session 是可变借用，之后无法再从 matched_sites 读 site_id。
            let clear_site_id = matched_sites[site_index].site_id.clone();
            let clear_profile_id =
                matched_sites[site_index].sessions[session_index].profile_id.clone();
            let session = &mut matched_sites[site_index].sessions[session_index];
            match job.await {
                Ok(Ok(refresh)) => {
                    session.username = refresh.account.username;
                    session.remaining = refresh.account.remaining;
                    session.used = refresh.account.used;
                    session.total = refresh.account.total;
                    session.unit = refresh.account.unit;
                    session.is_valid = refresh.is_valid;
                    session.sync_error = refresh.sync_error;
                    // 签到状态写回不能降级：上面已把当天缓存折算进 session，
                    // 刷新失败/端点缺失返回的空快照不得把「今日已签到」打回未签到。
                    session.checkin_enabled = session.checkin_enabled || refresh.checkin.enabled;
                    session.checked_in_today =
                        session.checked_in_today || refresh.checkin.checked_in_today;
                    // 错误信息只允许刷新结果覆盖（新失败原因优先），但当天已签到时清掉陈旧错误。
                    session.checkin_error = if session.checked_in_today {
                        String::new()
                    } else if !refresh.checkin.error.is_empty() {
                        refresh.checkin.error
                    } else {
                        session.checkin_error.clone()
                    };
                    // 「令牌由用户维护」的架构（白与黑）：令牌是用户从站点后台手动贴进来的，
                    // 同步只读不写——刷新结果（对这类架构恒为空）不得覆盖它，用户 ID 同理。
                    let user_owned_token = account_targets
                        .get(&clear_site_id)
                        .is_some_and(|(_, system_type, _)| access_token_is_user_owned(system_type));
                    if !user_owned_token {
                        session.newapi_token = refresh.newapi_token;
                        session.has_access_token = !session.newapi_token.is_empty();
                        session.newapi_user_id = refresh.newapi_user_id;
                    }
                    // 真正从站点取到数据：上次同步遗留的浏览器兜底冷却一并清零，
                    // 否则直连已恢复、界面冷却倒计时却还在，自动同步继续被跳过。
                    if refresh.refreshed {
                        session.browser_fallback_failed_at = 0;
                        session.browser_fallback_fail_count = 0;
                        session.browser_fallback_cooldown_ms = 0;
                    }
                    // 凭据已续期：删掉上一次「令牌失效」留下的陈旧 Key/模型错误行，
                    // 否则卡片会一直挂着「请重新登录后同步账号」，看起来像同步没生效。
                    if refresh.refreshed {
                        drop_stale_model_credential_error(
                            database,
                            &clear_site_id,
                            &clear_profile_id,
                        );
                    }
                }
                Ok(Err(error)) => session.sync_error = error,
                Err(error) => session.sync_error = format!("账号同步任务失败：{error}"),
            }
            let amount = session.remaining.unwrap_or(0.0);
            let mut amount_text = format!("{amount:.2}");
            while amount_text.contains('.') && amount_text.ends_with('0') {
                amount_text.pop();
            }
            if amount_text.ends_with('.') {
                amount_text.pop();
            }
            if !session.unit.is_empty() {
                amount_text.push(' ');
                amount_text.push_str(&session.unit);
            }
            let mut details = vec![format!("余额 {amount_text}")];
            if session.checkin_enabled {
                details.push(if session.checked_in_today {
                    "今日已签到".into()
                } else {
                    "今日未自动签到".into()
                });
            }
            // 刷新令牌移交（本地会话失效 → 交 Chrome 同源刷新）不是失败，
            // 按信息展示，避免每次扫描都误报“额度刷新失败”。
            let is_refresh_handoff = session.sync_error == NEWAPI_REFRESH_HANDOFF_MESSAGE;
            if is_refresh_handoff {
                details.push(format!("额度刷新转 Chrome 处理：{}", session.sync_error));
            } else if !session.sync_error.is_empty() {
                details.push(format!("额度刷新失败：{}", session.sync_error));
            }
            if !session.checkin_error.is_empty() {
                details.push(format!("签到失败：{}", session.checkin_error));
            }
            // 签到失败只反映在状态小字“无法签到”上，不纳入异常告警：
            // 它每天重复出现（未签到 → 自动签到失败），按告警统计会永久误报。
            let has_warning = !session.sync_error.is_empty() && !is_refresh_handoff;
            emit_optional_sync_progress(
                &bus,
                run_id,
                &progress_stage,
                if has_warning { "error" } else { "success" },
                format!(
                    "{site_name} · Chrome {profile_label}：{}",
                    details.join("；")
                ),
            );
        }
    }

    let detected = browser_session_site_ids.len();
    let accounts = matched_sites
        .iter()
        .filter(|site| account_refresh_site_ids.contains(&site.site_id))
        .flat_map(|site| &site.sessions)
        .filter(|session| session.is_valid)
        .count();

    let warnings = matched_sites
        .iter()
        .flat_map(|site| &site.sessions)
        .filter(|session| {
            session.sync_error != NEWAPI_REFRESH_HANDOFF_MESSAGE && !session.sync_error.is_empty()
        })
        .count();

    let mut connection = database.lock_conn()?;
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    let cached_api_counts = {
        let mut statement = transaction
            .prepare(
                "SELECT site_id, profile_id, MAX(api_key_count), MAX(api_model_count)
                 FROM site_accounts
                 GROUP BY site_id, profile_id",
            )
            .map_err(|error| error.to_string())?;
        let counts = statement
            .query_map([], |row| {
                Ok((
                    (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                    (
                        row.get::<_, i64>(2)?.max(0) as usize,
                        row.get::<_, i64>(3)?.max(0) as usize,
                    ),
                ))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<HashMap<_, _>, _>>()
            .map_err(|error| error.to_string())?;
        counts
    };
    for site in &mut matched_sites {
        for session in &mut site.sessions {
            let (key_count, model_count) = cached_api_counts
                .get(&(site.site_id.clone(), session.profile_id.clone()))
                .copied()
                .unwrap_or_default();
            session.api_key_count = key_count;
            session.api_model_count = model_count;
        }
    }
    let mut newly_marked = 0_usize;
    let mut preserved_accounts = 0_usize;
    if !extract_only {
        // 只重建「本次扫到会话」的站点：未扫到会话的站点保留缓存行。
        // Chrome 会把新登录的 Cookie 攒在内存里延迟刷盘，Cookie 库也可能被
        // 短暂锁定，这些瞬时情况下直接清空会把整站账号（余额/令牌）抹掉。
        // 下次扫到会话时仍按 DELETE+INSERT 全量重建，登录态收敛不受影响。
        let rebuilt_site_ids: HashSet<String> = matched_sites
            .iter()
            .filter(|site| account_refresh_site_ids.contains(&site.site_id))
            .map(|site| site.site_id.clone())
            .collect();
        for site_id in &rebuilt_site_ids {
            transaction
                .execute("DELETE FROM site_accounts WHERE site_id = ?1", [site_id])
                .map_err(|error| error.to_string())?;
        }
        // 作用域内未扫到会话、但本地仍有账号缓存的站点：保留数据并标注原因，
        // 让卡片以“账号信息同步失败”提示而非整站消失。
        let stale_scope_site_ids: Vec<String> = if let Some(site_id) = &requested_site_id {
            vec![site_id.clone()]
        } else if has_site_scope {
            requested_site_ids.iter().cloned().collect()
        } else {
            transaction
                .prepare("SELECT DISTINCT site_id FROM site_accounts")
                .map_err(|error| error.to_string())?
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| error.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?
        };
        for site_id in &stale_scope_site_ids {
            if rebuilt_site_ids.contains(site_id) {
                continue;
            }
            preserved_accounts += transaction
                .execute(
                    "UPDATE site_accounts
                         SET sync_error = ?1
                         WHERE site_id = ?2
                           AND sync_error <> ?1",
                    params![
                        "本次未在 Chrome 扫到该站点的登录会话，已保留上次缓存（刚在浏览器登录的话，请稍等 Cookie 写入后重试；已退出登录可忽略）",
                        site_id
                    ],
                )
                .map_err(|error| error.to_string())?;
        }
        for site in &matched_sites {
            if !account_refresh_site_ids.contains(&site.site_id) {
                continue;
            }
            for session in &site.sessions {
                let cookie_names = serde_json::to_string(&session.cookie_names)
                    .map_err(|error| error.to_string())?;
                transaction
                    .execute(
                        "INSERT OR REPLACE INTO site_accounts (
                            site_id, profile_id, domain, cookie_count, cookie_names,
                            profile_name, account_name, username, api_key_count, api_model_count,
                            remaining, used, total, unit, is_valid, sync_error,
                            checkin_enabled, checked_in_today, checkin_error,
                            checkin_date, updated_at, newapi_token, newapi_user_id,
                            browser_fallback_failed_at, browser_fallback_fail_count
                         ) VALUES (
                            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                            ?13, ?14, ?15, ?16, ?17, ?18, ?19, date('now', 'localtime'), CURRENT_TIMESTAMP,
                            ?20, ?21, ?22, ?23
                         )",
                        params![
                            site.site_id,
                            session.profile_id,
                            session.domain,
                            session.cookie_count as i64,
                            cookie_names,
                            session.profile_name,
                            session.account_name,
                            session.username,
                            session.api_key_count as i64,
                            session.api_model_count as i64,
                            session.remaining,
                            session.used,
                            session.total,
                            session.unit,
                            session.is_valid,
                            session.sync_error,
                            session.checkin_enabled,
                            session.checked_in_today,
                            session.checkin_error,
                            session.newapi_token.clone(),
                            session.newapi_user_id.clone(),
                            session.browser_fallback_failed_at,
                            session.browser_fallback_fail_count,
                        ],
                    )
                    .map_err(|error| error.to_string())?;
            }
        }
    }
    // 会话比对：浏览器有该站点会话、但本地未标记“在用” → 标为“待定”。
    // 用 browser_session_site_ids（Cookie/LocalStorage 原始命中），
    // 不要用后面被账号候选规则滤掉的 matched_sites。
    let session_site_ids = browser_session_site_ids;

    if extract_only {
        let scope_site_ids: Option<HashSet<String>> = if let Some(site_id) = &requested_site_id {
            Some(HashSet::from([site_id.clone()]))
        } else if has_site_scope {
            Some(requested_site_ids.clone())
        } else {
            None
        };

        if let Some(scope) = &scope_site_ids {
            for site_id in scope {
                let is_personal: i64 = transaction
                    .query_row(
                        "SELECT is_personal FROM directory_sites WHERE id = ?1",
                        [site_id],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(|error| error.to_string())?
                    .unwrap_or(0);
                if is_personal != 0 {
                    transaction
                            .execute(
                                "UPDATE directory_sites SET is_pending = 0 WHERE id = ?1 AND is_pending <> 0",
                                [site_id],
                            )
                            .map_err(|error| error.to_string())?;
                    continue;
                }
                if session_site_ids.contains(site_id) {
                    let changed = transaction
                        .execute(
                            "UPDATE directory_sites
                                 SET is_pending = 1, favorite = 0, updated_at = CURRENT_TIMESTAMP
                                 WHERE id = ?1 AND is_personal = 0 AND is_pending = 0",
                            [site_id],
                        )
                        .map_err(|error| error.to_string())?;
                    newly_marked += changed as usize;
                } else {
                    // 作用域内已无浏览器会话：清掉旧待定
                    transaction
                            .execute(
                                "UPDATE directory_sites SET is_pending = 0 WHERE id = ?1 AND is_pending <> 0",
                                [site_id],
                            )
                            .map_err(|error| error.to_string())?;
                }
            }
        } else {
            for site_id in &session_site_ids {
                let changed = transaction
                    .execute(
                        "UPDATE directory_sites
                             SET is_pending = 1, favorite = 0, updated_at = CURRENT_TIMESTAMP
                             WHERE id = ?1 AND is_personal = 0 AND is_pending = 0",
                        [site_id],
                    )
                    .map_err(|error| error.to_string())?;
                newly_marked += changed as usize;
            }
            // 全库：在用清待定；不在会话集合里的旧待定也清掉，避免脏数据。
            transaction
                    .execute(
                        "UPDATE directory_sites SET is_pending = 0 WHERE is_personal = 1 AND is_pending <> 0",
                        [],
                    )
                    .map_err(|error| error.to_string())?;
            if !session_site_ids.is_empty() {
                let placeholders = session_site_ids
                    .iter()
                    .enumerate()
                    .map(|(index, _)| format!("?{}", index + 1))
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!(
                    "UPDATE directory_sites
                         SET is_pending = 0
                         WHERE is_pending <> 0
                           AND is_personal = 0
                           AND id NOT IN ({placeholders})"
                );
                let params = session_site_ids
                    .iter()
                    .map(|id| id.as_str())
                    .collect::<Vec<_>>();
                transaction
                    .execute(&sql, rusqlite::params_from_iter(params))
                    .map_err(|error| error.to_string())?;
            } else {
                // 一个会话都没扫到时，不批量清待定，避免误伤（例如 Chrome 暂时不可读）。
            }
        }
    }

    transaction.commit().map_err(|error| error.to_string())?;
    emit_optional_sync_progress(
        &bus,
        run_id,
        "chrome-cache",
        "success",
        if extract_only {
            format!("浏览器会话提取完成：有会话 {detected} 个站点，新待定 {newly_marked} 个")
        } else {
            let preserved_note = if preserved_accounts > 0 {
                format!("；未扫到会话，已保留 {preserved_accounts} 个缓存账号")
            } else {
                String::new()
            };
            if refresh_pending {
                format!("待定站点额度缓存已写入 SQLite：{accounts} 个账号，{warnings} 个警告{preserved_note}")
            } else {
                format!("在用站点额度缓存已写入 SQLite：{accounts} 个账号，{warnings} 个警告{preserved_note}")
            }
        },
    );

    Ok(ChromeUsageScanResult {
        scanned,
        detected,
        accounts,
        warnings,
        newly_marked,
        sites: matched_sites,
    })
}

/// 账号刚成功续期凭据（refresh 拿到有效令牌）时，顺手删掉该账号「上一次 Key/模型
/// 同步因凭据失效而失败、且没留下任何 Key/模型」的缓存行。
///
/// 卡片据此行渲染「Key 与模型同步失败，点击重试」并原样附上旧错误原文
/// （如「NewAPI Key 接口 HTTP 401…请重新登录后同步账号」）。账号同步既然已经成功，
/// 那条错误要求的补救动作就已完成，继续展示只会让人以为同步没生效；而且该行没有
/// 任何 Key/模型数据，等价于「未同步」——删掉后卡片回到「未同步，点击同步」的真实状态。
///
/// 只处理凭据类错误（401 / 令牌失效文案）：网络、解析、站点确实没有 Key 等其他失败
/// 原因与登录状态无关，保留原样以免掩盖真实问题。
fn drop_stale_model_credential_error(database: &Database, site_id: &str, profile_id: &str) {
    let Ok(connection) = database.lock_conn() else {
        return;
    };
    let existing: Option<(String, String, String)> = connection
        .query_row(
            "SELECT error, keys_json, models_json
             FROM site_model_cache WHERE site_id = ?1 AND profile_id = ?2",
            params![site_id, profile_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .ok()
        .flatten();
    let Some((error, keys_json, models_json)) = existing else {
        return;
    };
    let no_keys = matches!(keys_json.trim(), "" | "[]");
    let no_models = matches!(models_json.trim(), "" | "[]");
    if !(no_keys && no_models) || !access_token_was_rejected(&error) {
        return;
    }
    // 清理失败不影响账号同步结果，下次同步会再试。
    let _ = connection.execute(
        "DELETE FROM site_model_cache WHERE site_id = ?1 AND profile_id = ?2",
        params![site_id, profile_id],
    );
}
