use crate::site::sync;
use crate::db::*;
use crate::models::*;
use crate::site::library::*;
use std::{collections::HashSet, time::Duration};
use tauri::{Manager, State};

pub(crate) fn remote_user_string(value: &serde_json::Value, paths: &[&str]) -> String {
    paths
        .iter()
        .find_map(|path| value.pointer(path).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
        .to_string()
}

pub(crate) fn remote_user_name(value: &serde_json::Value) -> String {
    remote_user_string(
        value,
        &[
            "/user/displayName",
            "/user/display_name",
            "/user/name",
            "/user/username",
            "/data/displayName",
            "/data/display_name",
            "/data/name",
            "/data/username",
            "/displayName",
            "/display_name",
            "/name",
            "/username",
            "/user/login",
            "/data/login",
            "/login",
        ],
    )
}

pub(crate) fn remote_user_username(value: &serde_json::Value) -> String {
    remote_user_string(
        value,
        &[
            "/user/username",
            "/user/login",
            "/data/username",
            "/data/login",
            "/username",
            "/login",
        ],
    )
}

pub(crate) fn remote_user_avatar(value: &serde_json::Value) -> String {
    remote_user_string(
        value,
        &[
            "/user/avatarUrl",
            "/user/avatar_url",
            "/user/avatar",
            "/data/avatarUrl",
            "/data/avatar_url",
            "/data/avatar",
            "/avatarUrl",
            "/avatar_url",
            "/avatar",
        ],
    )
}

pub(crate) fn remote_sites_from_json(value: serde_json::Value) -> Result<Vec<SiteRecord>, String> {
    let mut sites = if value.is_array() {
        value
    } else if let Some(sites) = value.get("sites") {
        sites.clone()
    } else if let Some(sites) = value.pointer("/data/sites") {
        sites.clone()
    } else if let Some(data) = value.get("data").filter(|data| data.is_array()) {
        data.clone()
    } else {
        return Err("远端站点接口返回格式不正确：缺少 sites 列表".into());
    };

    if let Some(items) = sites.as_array_mut() {
        for item in items {
            if let Some(site) = item.as_object_mut() {
                let system_type = infer_remote_system_type(site);
                if !system_type.is_empty() {
                    site.insert("systemType".into(), serde_json::Value::String(system_type));
                }
            }
        }
    }

    serde_json::from_value(sites).map_err(|error| format!("远端站点数据格式不正确：{error}"))
}

pub(crate) async fn authenticated_remote_session(
    app: &tauri::AppHandle,
    database: &Database,
) -> Result<(sync::ChromeCookieSession, serde_json::Value), String> {
    let home_dir = app
        .path()
        .home_dir()
        .map_err(|error| format!("无法定位用户目录：{error}"))?;
    let sessions = tauri::async_runtime::spawn_blocking(move || {
        sync::read_chrome_cookie_sessions_from_home(
            &home_dir,
            REMOTE_ROOT_URL,
            REMOTE_SESSION_COOKIE,
        )
    })
    .await
    .map_err(|error| format!("读取 Chrome 登录会话任务失败：{error}"))??;

    let client = build_http_client(database, Duration::from_secs(20), 5, "同步请求")?;

    for session in sessions {
        let cookie = reqwest::header::HeaderValue::from_str(&session.cookie_header)
            .map_err(|_| "Chrome 登录 Cookie 格式无效".to_string())?;
        let response = client
            .get(REMOTE_USER_URL)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::USER_AGENT, "OpenHub-Desktop/0.3")
            .header(reqwest::header::COOKIE, cookie)
            .send()
            .await
            .map_err(|error| format!("无法连接用户信息接口：{error}"))?;

        if matches!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        ) {
            continue;
        }
        if !response.status().is_success() {
            return Err(format!(
                "用户信息接口请求失败（HTTP {}）",
                response.status().as_u16()
            ));
        }
        let user = response
            .json::<serde_json::Value>()
            .await
            .map_err(|error| format!("用户信息接口返回格式不正确：{error}"))?;
        return Ok((session, user));
    }

    Err("Chrome 中找到的 ldoh 登录会话均已失效，请先在 Chrome 登录后重试".into())
}

#[tauri::command]
pub async fn get_remote_user(
    app: tauri::AppHandle,
    database: State<'_, Database>,
) -> Result<RemoteUserInfo, String> {
    let (session, user) = authenticated_remote_session(&app, &database).await?;
    let username = remote_user_username(&user);
    let name = {
        let value = remote_user_name(&user);
        if value.is_empty() {
            if !username.is_empty() {
                username.clone()
            } else if !session.account_name.is_empty() {
                session.account_name.clone()
            } else {
                "已登录用户".to_string()
            }
        } else {
            value
        }
    };

    Ok(RemoteUserInfo {
        name,
        username,
        avatar_url: remote_user_avatar(&user),
        profile_name: session.profile_name,
        account_name: session.account_name,
    })
}

pub(crate) fn normalize_url_key(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Ok(parsed) = url::Url::parse(trimmed) {
        let scheme = parsed.scheme().to_lowercase();
        let host = parsed.host_str().unwrap_or("").to_lowercase();
        let port = parsed.port();
        let path = parsed.path().trim_end_matches('/').to_string();
        let port_str = match (scheme.as_str(), port) {
            ("http", Some(80)) | ("https", Some(443)) | (_, None) => String::new(),
            (_, Some(p)) => format!(":{}", p),
        };
        let query_str = parsed
            .query()
            .map(|q| format!("?{}", q))
            .unwrap_or_default();
        format!("{}://{}{}{}{}", scheme, host, port_str, path, query_str)
    } else {
        let mut s = trimmed.to_string();
        while s.ends_with('/') {
            s.pop();
        }
        s
    }
}

struct ExistingLocalSite {
    id: String,
    favorite: bool,
    hidden: bool,
    is_personal: bool,
    is_pending: bool,
    use_system_proxy: bool,
    use_proxy_pool: bool,
    system_type: String,
}

#[tauri::command]
pub async fn sync_remote_sites(
    app: tauri::AppHandle,
    database: State<'_, Database>,
    _runaway: Option<bool>,
    run_id: u64,
) -> Result<SyncSitesResult, String> {
    emit_sync_progress(
        &app,
        run_id,
        "session",
        "running",
        "正在读取并验证 Chrome 登录会话".into(),
    );
    let (session, user) = authenticated_remote_session(&app, &database).await?;
    emit_sync_progress(
        &app,
        run_id,
        "session",
        "success",
        format!("Chrome {} 登录会话验证完成", session.profile_name),
    );
    let client = build_http_client(&database, Duration::from_secs(30), 5, "同步请求")?;
    let cookie = reqwest::header::HeaderValue::from_str(&session.cookie_header)
        .map_err(|_| "Chrome 登录 Cookie 格式无效".to_string())?;

    emit_sync_progress(
        &app,
        run_id,
        "download",
        "running",
        "正在同时请求存活站点与跑路站点列表".into(),
    );

    let alive_request = client
        .get(REMOTE_SITES_URL)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, "OpenHub-Desktop/0.3")
        .header(reqwest::header::COOKIE, cookie.clone())
        .send();

    let runaway_url = format!("{REMOTE_SITES_URL}?mode=runaway");
    let runaway_request = client
        .get(runaway_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, "OpenHub-Desktop/0.3")
        .header(reqwest::header::COOKIE, cookie)
        .send();

    let (alive_response, runaway_response) = tokio::try_join!(alive_request, runaway_request)
        .map_err(|error| format!("无法连接站点同步接口：{error}"))?;

    if matches!(
        alive_response.status(),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
    ) || matches!(
        runaway_response.status(),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
    ) {
        return Err("Chrome 登录会话已失效，请重新登录后重试".into());
    }

    if !alive_response.status().is_success() {
        return Err(format!(
            "存活站点同步接口请求失败（HTTP {}）",
            alive_response.status().as_u16()
        ));
    }
    if !runaway_response.status().is_success() {
        return Err(format!(
            "跑路站点同步接口请求失败（HTTP {}）",
            runaway_response.status().as_u16()
        ));
    }

    emit_sync_progress(
        &app,
        run_id,
        "download",
        "success",
        "存活与跑路站点接口响应正常".into(),
    );
    emit_sync_progress(
        &app,
        run_id,
        "parse",
        "running",
        "正在解析并校验远端存活与跑路站点数据".into(),
    );

    let (alive_json, runaway_json) = tokio::try_join!(
        async {
            alive_response
                .json::<serde_json::Value>()
                .await
                .map_err(|error| format!("存活站点接口返回格式不正确：{error}"))
        },
        async {
            runaway_response
                .json::<serde_json::Value>()
                .await
                .map_err(|error| format!("跑路站点接口返回格式不正确：{error}"))
        }
    )?;

    let mut alive_sites = remote_sites_from_json(alive_json)?;
    for site in &mut alive_sites {
        site.is_runaway = false;
    }

    let mut runaway_sites = remote_sites_from_json(runaway_json)?;
    for site in &mut runaway_sites {
        site.is_runaway = true;
    }

    let alive_count = alive_sites.len();
    let runaway_count = runaway_sites.len();
    let mut all_remote_sites = Vec::with_capacity(alive_count + runaway_count);
    all_remote_sites.extend(alive_sites);
    all_remote_sites.extend(runaway_sites);

    emit_sync_progress(
        &app,
        run_id,
        "parse",
        "success",
        format!(
            "已解析 {} 条远端站点记录（存活 {} 条，跑路 {} 条）",
            all_remote_sites.len(),
            alive_count,
            runaway_count
        ),
    );
    emit_sync_progress(
        &app,
        run_id,
        "save",
        "running",
        "正在写入本地数据库并保留本地类型与在用状态".into(),
    );

    let mut connection = database.lock_conn()?;
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;

    let mut existing_map: std::collections::HashMap<String, ExistingLocalSite> =
        std::collections::HashMap::new();
    let mut existing_ids: HashSet<String> = HashSet::new();

    {
        let mut statement = transaction
            .prepare(
                "SELECT id, api_base_url, favorite, hidden, is_personal, is_pending, use_system_proxy, use_proxy_pool, system_type FROM directory_sites",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? != 0,
                    row.get::<_, i64>(3)? != 0,
                    row.get::<_, i64>(4)? != 0,
                    row.get::<_, i64>(5)? != 0,
                    row.get::<_, i64>(6)? != 0,
                    row.get::<_, i64>(7)? != 0,
                    row.get::<_, String>(8)?,
                ))
            })
            .map_err(|error| error.to_string())?;

        for item in rows {
            let (
                id,
                api_base_url,
                favorite,
                hidden,
                is_personal,
                is_pending,
                use_system_proxy,
                use_proxy_pool,
                system_type,
            ) = item.map_err(|error| error.to_string())?;
            existing_ids.insert(id.clone());
            let key = normalize_url_key(&api_base_url);
            if !key.is_empty() {
                existing_map.insert(
                    key,
                    ExistingLocalSite {
                        id,
                        favorite,
                        hidden,
                        is_personal,
                        is_pending,
                        use_system_proxy,
                        use_proxy_pool,
                        system_type,
                    },
                );
            }
        }
    }

    let mut added = 0_usize;
    let mut updated = 0_usize;
    let mut synced_ids = HashSet::new();
    let mut processed_url_keys = HashSet::new();

    for mut site in all_remote_sites {
        let url_key = normalize_url_key(&site.api_base_url);
        if url_key.is_empty() && site.id.trim().is_empty() {
            continue;
        }
        if !url_key.is_empty() && !processed_url_keys.insert(url_key.clone()) {
            // 同一批次中重复出现的站点地址，跳过后续重复项
            continue;
        }

        let existing_match = if !url_key.is_empty() {
            existing_map.get(&url_key)
        } else {
            None
        };

        if let Some(existing) = existing_match {
            site.id = existing.id.clone();
            site.favorite = existing.favorite;
            site.hidden = existing.hidden;
            site.is_personal = existing.is_personal;
            site.is_pending = existing.is_pending && !existing.is_personal;
            site.use_system_proxy = existing.use_system_proxy;
            site.use_proxy_pool = existing.use_proxy_pool;
            // 已存在站点的类型一律冻结保留：本地类型可能来自用户手工调整或既有证据，
            // 全量同步只允许为“新增站点”提供类型；本地非空时绝不改写，
            // 本地为空（历史遗留缺类型）时才用远端值补齐。
            if !existing.system_type.trim().is_empty() {
                site.system_type = existing.system_type.clone();
            }
            updated += 1;
        } else {
            site.id = site.id.trim().to_string();
            if site.id.is_empty() || existing_ids.contains(&site.id) {
                site.id = generated_id();
            }
            existing_ids.insert(site.id.clone());
            site.favorite = false;
            site.hidden = false;
            site.is_personal = false;
            site.is_pending = false;
            site.use_system_proxy = false;
            site.use_proxy_pool = false;
            added += 1;
        }

        if !synced_ids.insert(site.id.clone()) {
            continue;
        }

        let site_name = site.name.clone();
        let site = normalize_remote_site(site)
            .map_err(|error| format!("同步站点「{site_name}」失败：{error}"))?;
        insert_site_transaction(&transaction, &site)?;
    }

    transaction.commit().map_err(|error| error.to_string())?;
    emit_sync_progress(
        &app,
        run_id,
        "save",
        "success",
        format!("本地写入完成：新增 {added}，更新 {updated}"),
    );

    let user_name = {
        let api_name = remote_user_name(&user);
        if !api_name.is_empty() {
            api_name
        } else if !session.account_name.trim().is_empty() {
            session.account_name.clone()
        } else {
            session.profile_name.clone()
        }
    };
    let site_ids = synced_ids.into_iter().collect();
    Ok(SyncSitesResult {
        added,
        updated,
        total: added + updated,
        profile_name: session.profile_name,
        account_name: session.account_name,
        user_name,
        runaway: false,
        site_ids,
    })
}
