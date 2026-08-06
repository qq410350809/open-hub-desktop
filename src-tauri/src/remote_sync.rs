use crate::chrome_session;
use crate::db::*;
use crate::models::*;
use crate::site_ops::*;
use rusqlite::OptionalExtension;
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
) -> Result<(chrome_session::ChromeCookieSession, serde_json::Value), String> {
    let home_dir = app
        .path()
        .home_dir()
        .map_err(|error| format!("无法定位用户目录：{error}"))?;
    let sessions = tauri::async_runtime::spawn_blocking(move || {
        chrome_session::read_chrome_cookie_sessions_from_home(
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

#[tauri::command]
pub async fn sync_remote_sites(
    app: tauri::AppHandle,
    database: State<'_, Database>,
    runaway: bool,
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
    let sites_url = if runaway {
        format!("{REMOTE_SITES_URL}?mode=runaway")
    } else {
        REMOTE_SITES_URL.to_string()
    };
    emit_sync_progress(
        &app,
        run_id,
        "download",
        "running",
        format!("正在请求{}站点列表", if runaway { "跑路" } else { "存活" }),
    );
    let response = client
        .get(sites_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, "OpenHub-Desktop/0.3")
        .header(reqwest::header::COOKIE, cookie)
        .send()
        .await
        .map_err(|error| format!("无法连接站点同步接口：{error}"))?;
    if matches!(
        response.status(),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
    ) {
        return Err("Chrome 登录会话已失效，请重新登录后重试".into());
    }
    if !response.status().is_success() {
        return Err(format!(
            "站点同步接口请求失败（HTTP {}）",
            response.status().as_u16()
        ));
    }
    emit_sync_progress(
        &app,
        run_id,
        "download",
        "success",
        format!("站点接口响应正常（HTTP {}）", response.status().as_u16()),
    );
    emit_sync_progress(
        &app,
        run_id,
        "parse",
        "running",
        "正在解析并校验远端站点数据".into(),
    );
    let response_json = response
        .json::<serde_json::Value>()
        .await
        .map_err(|error| format!("站点同步接口返回格式不正确：{error}"))?;
    let remote_sites = remote_sites_from_json(response_json)?;
    emit_sync_progress(
        &app,
        run_id,
        "parse",
        "success",
        format!("已解析 {} 条远端站点记录", remote_sites.len()),
    );
    emit_sync_progress(
        &app,
        run_id,
        "save",
        "running",
        "正在写入本地数据库并保留本地状态".into(),
    );

    let mut connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    let mut added = 0_usize;
    let mut updated = 0_usize;
    let mut synced_ids = HashSet::new();

    for mut site in remote_sites {
        site.id = site.id.trim().to_string();
        if site.id.is_empty() {
            return Err("远端站点数据包含空 ID，已取消本次同步".into());
        }
        if !synced_ids.insert(site.id.clone()) {
            continue;
        }

        let existing = transaction
            .query_row(
                "SELECT favorite, hidden, is_personal, use_system_proxy, system_type FROM directory_sites WHERE id = ?1",
                [&site.id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)? != 0,
                        row.get::<_, i64>(1)? != 0,
                        row.get::<_, i64>(2)? != 0,
                        row.get::<_, i64>(3)? != 0,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if let Some((favorite, hidden, is_personal, use_system_proxy, system_type)) = existing {
            site.favorite = favorite;
            site.hidden = hidden;
            site.is_personal = is_personal;
            site.use_system_proxy = use_system_proxy;
            if site.system_type.trim().is_empty() {
                site.system_type = system_type;
            }
            updated += 1;
        } else {
            site.favorite = false;
            site.hidden = false;
            site.use_system_proxy = false;
            added += 1;
        }
        site.is_runaway = runaway;

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
        runaway,
        site_ids,
    })
}
