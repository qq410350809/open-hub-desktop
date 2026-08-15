use crate::account_sync::*;
use crate::chrome_local_storage;
use crate::chrome_session;
use crate::db::*;
use crate::models::*;
use crate::platform_detect::{is_newapi, is_sub2api, is_zero_v_zero};
use crate::proxy_pool;
use crate::site_ops::*;
use rusqlite::{params, OptionalExtension};
use serde_json;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tauri::{Manager, State};
use url::Url;

#[tauri::command]
pub async fn mark_sites_with_chrome_sessions(
    app: tauri::AppHandle,
    database: State<'_, Database>,
    site_id: Option<String>,
    site_ids: Option<Vec<String>>,
    run_id: Option<u64>,
    extract_only: Option<bool>,
    refresh_pending: Option<bool>,
) -> Result<ChromeUsageScanResult, String> {
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
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
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
                let stored_system_type = row.get::<_, String>(4)?;
                let system_type = if is_zero_v_zero_site(&name, &api_base_url, &stored_system_type)
                {
                    "0v0".to_string()
                } else {
                    stored_system_type
                };
                let api_base_url = account_base_url(&name, &api_base_url, &system_type);
                let mut urls = Vec::with_capacity(4);
                if !api_base_url.trim().is_empty() {
                    let account_paths: &[&str] = if is_newapi(&system_type) {
                        &["/api/user/auth/refresh", "/api/user/self"]
                    } else if is_sub2api(&system_type) {
                        &["/api/v1/auth/me"]
                    } else if is_zero_v_zero(&system_type) {
                        &["/api/user/self", "/api/user/stats"]
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
    let account_refresh_site_ids = {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        let condition = if refresh_pending {
            "is_pending = 1"
        } else {
            "is_personal = 1"
        };
        let mut statement = connection
            .prepare(&format!("SELECT id FROM directory_sites WHERE {condition}"))
            .map_err(|error| error.to_string())?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?
            .collect::<Result<HashSet<_>, _>>()
            .map_err(|error| error.to_string())?;
        ids
    };
    let checkin_site_ids = {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        let mut statement = connection
            .prepare(
                "SELECT id FROM directory_sites
                 WHERE is_personal = 1 AND supports_checkin = 1",
            )
            .map_err(|error| error.to_string())?;
        let site_ids = statement
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
        } else if is_zero_v_zero(system_type) {
            &["/api/user/self", "/api/user/stats"]
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
        &app,
        run_id,
        "chrome-scan",
        "running",
        format!("开始提取 {} 个本地站点的 Chrome 会话数据", targets.len()),
    );
    let (current_month, previous_checkins, cached_accounts, cached_model_keys) = {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
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
                Ok((
                    (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                    keys,
                ))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<HashMap<_, _>, _>>()
            .map_err(|error| error.to_string())?;
        (current_month, previous_checkins, cached_accounts, cached_model_keys)
    };
    let scanned = targets.len();
    let scan_targets = targets
        .iter()
        .map(|(id, _, urls, _, _)| (id.clone(), urls.clone()))
        .collect::<Vec<_>>();
    let home_dir = app
        .path()
        .home_dir()
        .map_err(|error| format!("无法定位用户目录：{error}"))?;
    let mut matched_sites = if scanned == 0 {
        Vec::new()
    } else {
        let scan_home_dir = home_dir.clone();
        tauri::async_runtime::spawn_blocking(move || {
            chrome_session::site_sessions_from_home(&scan_home_dir, &scan_targets)
        })
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
    let profiles = tauri::async_runtime::spawn_blocking({
        let home_dir = home_dir.clone();
        move || chrome_session::profile_identities_from_home(&home_dir)
    })
    .await
    .map_err(|error| format!("读取 Chrome Profile 任务失败：{error}"))??;
    emit_optional_sync_progress(
        &app,
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
                Some(chrome_local_storage::LocalStorageTarget {
                    site_id: site_id.clone(),
                    profile_id: profile.id.clone(),
                    origin: origin.clone()?,
                })
            })
        })
        .collect::<Vec<_>>();
    let local_storage = tauri::async_runtime::spawn_blocking({
        let home_dir = home_dir.clone();
        move || chrome_local_storage::read_local_storage_from_home(&home_dir, &local_targets)
    })
    .await
    .map_err(|error| format!("读取 Chrome Local Storage 任务失败：{error}"))?
    .into_iter()
    .map(|item| ((item.site_id, item.profile_id), (item.values, item.error)))
    .collect::<HashMap<_, _>>();
    emit_optional_sync_progress(
        &app,
        run_id,
        "chrome-local-storage",
        "success",
        format!(
            "已分析 {} 组站点与 Profile 的 Local Storage",
            local_storage.len()
        ),
    );
    let profile_map = profiles
        .into_iter()
        .map(|profile| (profile.id.clone(), profile))
        .collect::<HashMap<_, _>>();

    let mut locally_inferred_types = HashMap::new();
    // newapi2（刷新令牌模式）的依据是浏览器里存在 new_api_refresh cookie；
    // 同域 Chrome 会话若带该 cookie，说明站点用 refresh token 认证而非纯 Cookie。
    let refresh_cookie_site_ids = matched_sites
        .iter()
        .filter(|site| {
            site.sessions.iter().any(|session| {
                has_newapi_refresh_cookie_name(session.cookie_names.iter().map(String::as_str))
            })
        })
        .map(|site| site.site_id.clone())
        .collect::<HashSet<_>>();
    for (site_id, _, _, api_base_url, system_type) in &mut targets {
        if is_zero_v_zero(system_type) {
            locally_inferred_types.insert(site_id.clone(), "0v0".into());
            continue;
        }
        // 优先相信浏览器真实账号结构：该域 Local Storage 本身是 NewAPI 或 Sub2API。
        // 即使库里已经有旧类型，也允许纠正，避免错误类型被永久保留。
        let inferred_from_accounts =
            infer_system_type_from_local_accounts(local_storage.iter().filter_map(
                |((local_site_id, _), (values, error))| {
                    (local_site_id == site_id && error.is_empty()).then_some(values)
                },
            ));
        // Local Storage 无结论时，退而使用 API 地址中的高置信产品名提示。
        let mut inferred = if inferred_from_accounts.is_empty() {
            system_type_hint_from_url(api_base_url.as_str()).unwrap_or("")
        } else {
            inferred_from_accounts
        };
        // NewAPI 进一步细分：带 new_api_refresh cookie 的账号走刷新令牌认证（newapi2），
        // 否则保持 Cookie 模式（new-api）。检测顺序上 cookie 证据优先于类型名。
        if is_newapi(inferred) && refresh_cookie_site_ids.contains(site_id) {
            inferred = "newapi2";
        }
        if !inferred.is_empty() && !inferred.eq_ignore_ascii_case(system_type) {
            let previous = system_type.clone();
            *system_type = inferred.into();
            locally_inferred_types.insert(site_id.clone(), inferred.into());
            if !previous.is_empty() {
                emit_optional_sync_progress(
                    &app,
                    run_id,
                    "site-type-correct",
                    "info",
                    format!("站点类型纠正：{site_id} 由 {previous} 调整为 {inferred}"),
                );
            }
        }
    }
    if !extract_only {
        persist_site_system_types(&database, &locally_inferred_types)?;
        if !locally_inferred_types.is_empty() {
            emit_optional_sync_progress(
                &app,
                run_id,
                "site-type-local",
                "success",
                format!(
                    "已通过 Chrome Local Storage 补充 {} 个站点类型",
                    locally_inferred_types.len()
                ),
            );
        }
    }

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
            let local_session = local_storage
                .get(&(site.site_id.clone(), session.profile_id.clone()))
                .filter(|(_, error)| error.is_empty())
                .map(|(values, _)| values);
            let has_local =
                local_session.is_some_and(|values| has_local_account_session(system_type, values));
            has_local
                || has_account_session_candidate(
                    system_type,
                    &HashMap::new(),
                    &session.cookie_names,
                )
        });
        !site.sessions.is_empty()
    });

    for ((site_id, profile_id), (values, _)) in &local_storage {
        let Some((base_url, system_type, _)) = account_targets.get(site_id) else {
            continue;
        };
        if !has_local_account_session(system_type, values) {
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
                matched_sites.push(chrome_session::ChromeSiteSessionMatch {
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
            .push(chrome_session::ChromeSessionInfo {
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
                newapi_token: String::new(),
                newapi_user_id: String::new(),
            });
    }

    // 账号缓存在重启/重新扫描前是被下面“全删后重插”覆盖的，这里先把缓存里的
    // 签到/余额/令牌并回扫描结果，避免上一轮的账号业务数据被无会话结果冲掉。
    for site in &mut matched_sites {
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
            if session.newapi_token.is_empty() && !cached.newapi_token.is_empty() {
                session.newapi_token = cached.newapi_token.clone();
                session.has_access_token = true;
            }
            if session.newapi_user_id.is_empty() {
                session.newapi_user_id = cached.newapi_user_id.clone();
            }
        }
    }

    // Local Storage 里能解析出账号的，也视为浏览器有会话（即使 Cookie 查询因 path/分区漏掉）。
    for ((site_id, _), (values, error)) in &local_storage {
        if error.is_empty() && has_local_account_session("", values) {
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
        &app,
        run_id,
        "chrome-scan",
        "success",
        format!(
            "Chrome 扫描完成：浏览器会话站点 {browser_session_count} 个，账号候选 {candidate_sites} 个 / {candidate_accounts} 个会话"
        ),
    );
    if !extract_only && !matched_sites.is_empty() {
        let chrome_user_agent = chrome_session::chrome_user_agent();
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
                let auth_label = if is_newapi(&system_type) {
                    if has_refresh_cookie {
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
                    &app,
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
                let cookie_endpoint = if has_refresh_cookie {
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
                let app = app.clone();
                let job = tauri::async_runtime::spawn(async move {
                    let needs_cookie = is_newapi(&system_type)
                        || (system_type.trim().is_empty()
                            && (parse_newapi_local_account(&local_values).is_ok()
                                || has_refresh_cookie));
                    let cookie_header = if needs_cookie {
                        let profile_id_for_cookie = profile_id.clone();
                        tauri::async_runtime::spawn_blocking(move || {
                            chrome_session::read_chrome_cookie_header_from_home(
                                &cookie_home_dir,
                                &cookie_base_url,
                                &profile_id_for_cookie,
                            )
                        })
                        .await
                        .map_err(|error| format!("读取 Chrome Cookie 任务失败：{error}"))?
                    } else {
                        Ok(String::new())
                    };
                    proxy_pool::with_account_proxy(
                        &app,
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
                    session.checkin_enabled = refresh.checkin.enabled;
                    session.checked_in_today = refresh.checkin.checked_in_today;
                    session.checkin_error = refresh.checkin.error;
                    session.newapi_token = refresh.newapi_token;
                    session.has_access_token = !session.newapi_token.is_empty();
                    session.newapi_user_id = refresh.newapi_user_id;
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
                    "今日未签到".into()
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
            let has_warning = (!session.sync_error.is_empty() && !is_refresh_handoff)
                || !session.checkin_error.is_empty();
            emit_optional_sync_progress(
                &app,
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
            (session.sync_error != NEWAPI_REFRESH_HANDOFF_MESSAGE
                && !session.sync_error.is_empty())
                || !session.checkin_error.is_empty()
        })
        .count();

    let mut connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
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
    if !extract_only {
        if let Some(site_id) = &requested_site_id {
            transaction
                .execute("DELETE FROM site_accounts WHERE site_id = ?1", [site_id])
                .map_err(|error| error.to_string())?;
        } else if has_site_scope {
            for site_id in &requested_site_ids {
                transaction
                    .execute("DELETE FROM site_accounts WHERE site_id = ?1", [site_id])
                    .map_err(|error| error.to_string())?;
            }
        } else {
            transaction
                .execute("DELETE FROM site_accounts", [])
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
                            checkin_date, updated_at, newapi_token, newapi_user_id
                         ) VALUES (
                            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                            ?13, ?14, ?15, ?16, ?17, ?18, ?19, date('now', 'localtime'), CURRENT_TIMESTAMP,
                            ?20, ?21
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
        &app,
        run_id,
        "chrome-cache",
        "success",
        if extract_only {
            format!("浏览器会话提取完成：有会话 {detected} 个站点，新待定 {newly_marked} 个")
        } else if refresh_pending {
            format!("待定站点额度缓存已写入 SQLite：{accounts} 个账号，{warnings} 个警告")
        } else {
            format!("在用站点额度缓存已写入 SQLite：{accounts} 个账号，{warnings} 个警告")
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
