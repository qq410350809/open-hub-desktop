use crate::db::*;
use crate::models::*;
use crate::site::library::{detect_platform, is_newapi, is_sub2api};
use crate::site::library::*;
use rusqlite::{params, Connection, OptionalExtension};
use std::time::Duration;
use tauri::State;

pub(crate) fn read_site(connection: &Connection, id: &str) -> Result<Option<SiteRecord>, String> {
    let mut stmt = connection
        .prepare(
            "SELECT id, name, description, registration_limit, icon, api_base_url, system_type,
                supports_immersive_translation, supports_ldc, supports_checkin, supports_nsfw,
                checkin_url, checkin_note, benefit_url, rate_limit, status_url,
                is_only_maintainer_visible, requires_invite_code, is_runaway, is_fake_charity,
                has_pending_report, is_personal, is_pending, use_system_proxy, use_proxy_pool, favorite, hidden, updated_at
         FROM directory_sites WHERE id = ?1",
        )
        .map_err(|e| e.to_string())?;

    let mut site_iter = stmt
        .query_map([id], |row| {
            Ok(SiteRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                registration_limit: row.get(3)?,
                icon: row.get(4)?,
                api_base_url: row.get(5)?,
                system_type: row.get(6)?,
                supports_immersive_translation: row.get::<_, i64>(7)? != 0,
                supports_ldc: row.get::<_, i64>(8)? != 0,
                supports_checkin: row.get::<_, i64>(9)? != 0,
                supports_nsfw: row.get::<_, i64>(10)? != 0,
                checkin_url: row.get(11)?,
                checkin_note: row.get(12)?,
                benefit_url: row.get(13)?,
                rate_limit: row.get(14)?,
                status_url: row.get(15)?,
                is_only_maintainer_visible: row.get::<_, i64>(16)? != 0,
                requires_invite_code: row.get::<_, i64>(17)? != 0,
                is_runaway: row.get::<_, i64>(18)? != 0,
                is_fake_charity: row.get::<_, i64>(19)? != 0,
                has_pending_report: row.get::<_, i64>(20)? != 0,
                is_personal: row.get::<_, i64>(21)? != 0,
                is_pending: row.get::<_, i64>(22)? != 0,
                use_system_proxy: row.get::<_, i64>(23)? != 0,
                use_proxy_pool: row.get::<_, i64>(24)? != 0,
                favorite: row.get::<_, i64>(25)? != 0,
                hidden: row.get::<_, i64>(26)? != 0,
                updated_at: row.get(27)?,
                tags: vec![],
                maintainers: vec![],
                extension_links: vec![],
            })
        })
        .map_err(|e| e.to_string())?;

    let mut site = match site_iter.next() {
        Some(Ok(s)) => s,
        _ => return Ok(None),
    };

    let mut stmt_tags = connection
        .prepare("SELECT tag FROM site_tags WHERE site_id = ?1")
        .unwrap();
    site.tags = stmt_tags
        .query_map([id], |r| r.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    let mut stmt_maint = connection.prepare("SELECT name, maintainer_id, username, profile_url FROM site_maintainers WHERE site_id = ?1").unwrap();
    site.maintainers = stmt_maint
        .query_map([id], |r| {
            Ok(Maintainer {
                name: r.get(0)?,
                id: r.get(1)?,
                username: r.get(2)?,
                profile_url: r.get(3)?,
            })
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    let mut stmt_ext = connection
        .prepare("SELECT label, url FROM site_extensions WHERE site_id = ?1")
        .unwrap();
    site.extension_links = stmt_ext
        .query_map([id], |r| {
            Ok(ExtensionLink {
                label: r.get(0)?,
                url: r.get(1)?,
            })
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    Ok(Some(site))
}

#[tauri::command]
pub fn list_library(database: State<'_, Database>) -> Result<LibraryData, String> {
    let connection = database.lock_conn()?;
    let mut statement = connection
        .prepare(
            "SELECT id, name, description, registration_limit, icon, api_base_url, system_type,
                supports_immersive_translation, supports_ldc, supports_checkin, supports_nsfw,
                checkin_url, checkin_note, benefit_url, rate_limit, status_url,
                is_only_maintainer_visible, requires_invite_code, is_runaway, is_fake_charity,
                has_pending_report, is_personal, is_pending, use_system_proxy, use_proxy_pool, favorite, hidden, updated_at
         FROM directory_sites
         ORDER BY is_personal DESC, is_pending DESC, datetime(updated_at) DESC, rowid DESC",
        )
        .map_err(|e| e.to_string())?;

    let mut sites: Vec<SiteRecord> = statement
        .query_map([], |row| {
            Ok(SiteRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                registration_limit: row.get(3)?,
                icon: row.get(4)?,
                api_base_url: row.get(5)?,
                system_type: row.get(6)?,
                supports_immersive_translation: row.get::<_, i64>(7)? != 0,
                supports_ldc: row.get::<_, i64>(8)? != 0,
                supports_checkin: row.get::<_, i64>(9)? != 0,
                supports_nsfw: row.get::<_, i64>(10)? != 0,
                checkin_url: row.get(11)?,
                checkin_note: row.get(12)?,
                benefit_url: row.get(13)?,
                rate_limit: row.get(14)?,
                status_url: row.get(15)?,
                is_only_maintainer_visible: row.get::<_, i64>(16)? != 0,
                requires_invite_code: row.get::<_, i64>(17)? != 0,
                is_runaway: row.get::<_, i64>(18)? != 0,
                is_fake_charity: row.get::<_, i64>(19)? != 0,
                has_pending_report: row.get::<_, i64>(20)? != 0,
                is_personal: row.get::<_, i64>(21)? != 0,
                is_pending: row.get::<_, i64>(22)? != 0,
                use_system_proxy: row.get::<_, i64>(23)? != 0,
                use_proxy_pool: row.get::<_, i64>(24)? != 0,
                favorite: row.get::<_, i64>(25)? != 0,
                hidden: row.get::<_, i64>(26)? != 0,
                updated_at: row.get(27)?,
                tags: vec![],
                maintainers: vec![],
                extension_links: vec![],
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let mut stmt = connection
        .prepare("SELECT site_id, tag FROM site_tags")
        .unwrap();
    for row in stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
    {
        if let Ok((site_id, tag)) = row {
            if let Some(s) = sites.iter_mut().find(|s| s.id == site_id) {
                s.tags.push(tag);
            }
        }
    }

    let mut stmt = connection
        .prepare("SELECT site_id, name, maintainer_id, username, profile_url FROM site_maintainers")
        .unwrap();
    for row in stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Maintainer {
                    name: r.get(1)?,
                    id: r.get(2)?,
                    username: r.get(3)?,
                    profile_url: r.get(4)?,
                },
            ))
        })
        .unwrap()
    {
        if let Ok((site_id, m)) = row {
            if let Some(s) = sites.iter_mut().find(|s| s.id == site_id) {
                s.maintainers.push(m);
            }
        }
    }

    let mut stmt = connection
        .prepare("SELECT site_id, label, url FROM site_extensions")
        .unwrap();
    for row in stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                ExtensionLink {
                    label: r.get(1)?,
                    url: r.get(2)?,
                },
            ))
        })
        .unwrap()
    {
        if let Ok((site_id, ext)) = row {
            if let Some(s) = sites.iter_mut().find(|s| s.id == site_id) {
                s.extension_links.push(ext);
            }
        }
    }

    let payload: SeedPayload =
        serde_json::from_str(SEED_JSON).map_err(|error| error.to_string())?;
    let usage_sites = read_cached_usage_sites(&connection)?;
    Ok(LibraryData {
        sites,
        suggested_tags: payload.tags,
        usage_sites,
    })
}

#[tauri::command]
pub fn create_site(
    database: State<'_, Database>,
    mut input: SiteRecord,
) -> Result<SiteRecord, String> {
    input.id = generated_id();
    input.favorite = false;
    input.hidden = false;
    let input = normalize_site(input)?;
    let mut connection = database.lock_conn()?;

    let transaction = connection.transaction().map_err(|e| e.to_string())?;
    insert_site_transaction(&transaction, &input)?;
    transaction.commit().map_err(|e| e.to_string())?;

    read_site(&connection, &input.id)?.ok_or_else(|| "创建站点失败".into())
}

#[tauri::command]
pub async fn import_site(
    database: State<'_, Database>,
    site_url: String,
    usage_state: Option<String>,
) -> Result<SiteRecord, String> {
    let base_url = normalize_import_base_url(&site_url)?;
    let canonical_url = base_url.to_string();
    {
        let connection = database.lock_conn()?;
        let existing = connection
            .query_row(
                "SELECT name FROM directory_sites
                 WHERE RTRIM(api_base_url, '/') = RTRIM(?1, '/') LIMIT 1",
                [&canonical_url],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if let Some(name) = existing {
            return Err(format!("站点「{name}」已经存在"));
        }
    }

    let client = build_http_client(&database, Duration::from_secs(12), 5, "站点资料采集")?;
    let root_job = tauri::async_runtime::spawn(fetch_discovery_resource(
        client.clone(),
        base_url.clone(),
        "text/html,application/xhtml+xml,application/json;q=0.8,*/*;q=0.5",
    ));
    let newapi_job = tauri::async_runtime::spawn(fetch_discovery_resource(
        client.clone(),
        base_url
            .join("/api/status")
            .map_err(|error| error.to_string())?,
        "application/json",
    ));
    let sub2api_job = tauri::async_runtime::spawn(fetch_discovery_resource(
        client.clone(),
        base_url
            .join("/setup/status")
            .map_err(|error| error.to_string())?,
        "application/json",
    ));
    let detect_job = tauri::async_runtime::spawn({
        let client = client.clone();
        let canonical_url = canonical_url.clone();
        async move { detect_platform(&client, &canonical_url).await }
    });
    let root_response = root_job.await.ok().flatten();
    let newapi_response = newapi_job.await.ok().flatten();
    let sub2api_response = sub2api_job.await.ok().flatten();
    if root_response.is_none() && newapi_response.is_none() && sub2api_response.is_none() {
        return Err("无法连接该站点，请检查 URL 或网络代理后重试".into());
    }

    // 平台类型：移植自 metapi 的 detectPlatform 流水线。
    let detected = detect_job.await.ok();
    let system_type = detected
        .and_then(|detection| detection.platform)
        .unwrap_or_default();
    let newapi_json = newapi_response.as_ref().and_then(DiscoveryResponse::json);
    let sub2api_json = sub2api_response.as_ref().and_then(DiscoveryResponse::json);
    let status_sources = if is_newapi(&system_type) {
        newapi_json.iter().collect::<Vec<_>>()
    } else if is_sub2api(&system_type) {
        sub2api_json.iter().collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let first_status_string = |keys: &[&str]| {
        status_sources
            .iter()
            .map(|value| discovered_json_string(value, keys))
            .find(|value| !value.is_empty())
            .unwrap_or_default()
    };
    let html = root_response
        .as_ref()
        .filter(|response| {
            response.content_type.contains("html")
                || response.body.to_ascii_lowercase().contains("<html")
        })
        .map(|response| response.body.as_str())
        .unwrap_or_default();
    let page_title = html_title(html);
    let title_is_challenge = ["just a moment", "attention required", "cloudflare"]
        .iter()
        .any(|marker| page_title.to_ascii_lowercase().contains(marker));
    let host_name = base_url
        .host_str()
        .unwrap_or("未命名站点")
        .trim_start_matches("www.")
        .to_string();
    let name = first_status_string(&["name", "site_name", "siteName", "system_name", "systemName"]);
    let name = if name.is_empty() && !page_title.is_empty() && !title_is_challenge {
        page_title
    } else if name.is_empty() {
        host_name
    } else {
        name
    };
    let description = first_status_string(&[
        "description",
        "site_description",
        "siteDescription",
        "system_description",
        "systemDescription",
    ]);
    let description = if description.is_empty() && !title_is_challenge {
        html_meta_description(html)
    } else {
        description
    };
    let discovered_icon =
        first_status_string(&["logo", "logo_url", "logoUrl", "icon", "icon_url", "iconUrl"]);
    let discovered_icon = if discovered_icon.is_empty() {
        html_icon_href(html)
    } else {
        discovered_icon
    };
    let icon = if discovered_icon.is_empty() {
        base_url
            .join("/favicon.ico")
            .map(|url| url.to_string())
            .unwrap_or_default()
    } else {
        resolve_discovered_url(&base_url, &discovered_icon)
    };
    let supports_checkin = status_sources.iter().any(|value| {
        discovered_json_bool(
            value,
            &[
                "checkin_enabled",
                "checkinEnabled",
                "enable_checkin",
                "enableCheckin",
            ],
        )
    });
    let checkin_url = if supports_checkin && is_newapi(&system_type) {
        base_url
            .join("/console/personal")
            .map(|url| url.to_string())
            .unwrap_or_default()
    } else if supports_checkin && is_sub2api(&system_type) {
        base_url
            .join("/dashboard")
            .map(|url| url.to_string())
            .unwrap_or_default()
    } else {
        String::new()
    };

    let usage_state = usage_state.unwrap_or_else(|| "all".into());
    let (is_personal, is_pending) = match usage_state.as_str() {
        "personal" => (true, false),
        "pending" => (false, true),
        "all" | "" => (false, false),
        _ => return Err("无效的站点归类，只支持全部、在用或待定".into()),
    };
    let mut site = SiteRecord {
        id: generated_id(),
        name: name.chars().take(100).collect(),
        description: description.chars().take(800).collect(),
        icon,
        api_base_url: canonical_url,
        system_type,
        supports_checkin,
        checkin_url,
        use_system_proxy: false,
        is_personal,
        is_pending,
        ..SiteRecord::default()
    };
    site = normalize_site(site)?;

    let mut connection = database.lock_conn()?;
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    let duplicate = transaction
        .query_row(
            "SELECT name FROM directory_sites
             WHERE RTRIM(api_base_url, '/') = RTRIM(?1, '/') LIMIT 1",
            [&site.api_base_url],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    if let Some(name) = duplicate {
        return Err(format!("站点「{name}」已经存在"));
    }
    insert_site_transaction(&transaction, &site)?;
    transaction.commit().map_err(|error| error.to_string())?;
    read_site(&connection, &site.id)?.ok_or_else(|| "导入站点失败".into())
}

#[tauri::command]
pub fn update_site(
    database: State<'_, Database>,
    id: String,
    mut input: SiteRecord,
) -> Result<SiteRecord, String> {
    input.id = id.clone();
    let mut input = normalize_site(input)?;
    let mut connection = database.lock_conn()?;

    let old_site = read_site(&connection, &id)?.ok_or_else(|| "找不到要更新的站点".to_string())?;
    input.favorite = old_site.favorite;
    input.hidden = old_site.hidden;

    // We don't need to preserve created_at from input since insert_site_transaction handles it via COALESCE and updated_at is mapped correctly?
    // Wait, insert_site_transaction sets created_at to COALESCE(NULLIF(?24, ''), CURRENT_TIMESTAMP) which is updated_at.
    // In fact, if we use INSERT OR REPLACE, it deletes the old row and creates a new one! This implies we lose created_at and we lose favorite/hidden unless we preserve them.
    // Yes! That's why I fetched old_site and mapped them back.
    // BUT we need to use a proper UPDATE or manually map all fields.
    // Let's rewrite insert_site_transaction into a pure UPDATE if it exists, to preserve created_at if we can.
    // Wait! Since we already do a transaction, let's just use an UPDATE statement for update_site!
    let transaction = connection.transaction().map_err(|e| e.to_string())?;
    transaction.execute(
        "UPDATE directory_sites SET
            name=?1, description=?2, registration_limit=?3, icon=?4, api_base_url=?5,
            supports_immersive_translation=?6, supports_ldc=?7, supports_checkin=?8, supports_nsfw=?9,
            checkin_url=?10, checkin_note=?11, benefit_url=?12, rate_limit=?13, status_url=?14,
            is_only_maintainer_visible=?15, requires_invite_code=?16, is_runaway=?17, is_fake_charity=?18,
            has_pending_report=?19, is_personal=?20, is_pending=?21, use_system_proxy=?22,
            use_proxy_pool=?23, system_type=?24, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id=?25",
        params![
            input.name, input.description, input.registration_limit, input.icon, input.api_base_url,
            input.supports_immersive_translation, input.supports_ldc, input.supports_checkin, input.supports_nsfw,
            input.checkin_url, input.checkin_note, input.benefit_url, input.rate_limit, input.status_url,
            input.is_only_maintainer_visible, input.requires_invite_code, input.is_runaway, input.is_fake_charity,
            input.has_pending_report, input.is_personal, input.is_pending, input.use_system_proxy, input.use_proxy_pool, input.system_type, id
        ],
    ).map_err(|e| e.to_string())?;

    transaction
        .execute("DELETE FROM site_tags WHERE site_id = ?1", [&id])
        .map_err(|error| error.to_string())?;
    for tag in &input.tags {
        transaction
            .execute(
                "INSERT INTO site_tags (site_id, tag) VALUES (?1, ?2)",
                params![id, tag],
            )
            .map_err(|error| error.to_string())?;
    }

    transaction
        .execute("DELETE FROM site_maintainers WHERE site_id = ?1", [&id])
        .map_err(|error| error.to_string())?;
    for maintainer in &input.maintainers {
        transaction.execute(
            "INSERT INTO site_maintainers (site_id, name, maintainer_id, username, profile_url) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, maintainer.name, maintainer.id, maintainer.username, maintainer.profile_url],
        ).map_err(|error| error.to_string())?;
    }

    transaction
        .execute("DELETE FROM site_extensions WHERE site_id = ?1", [&id])
        .map_err(|error| error.to_string())?;
    for ext in &input.extension_links {
        transaction
            .execute(
                "INSERT INTO site_extensions (site_id, label, url) VALUES (?1, ?2, ?3)",
                params![id, ext.label, ext.url],
            )
            .map_err(|error| error.to_string())?;
    }

    transaction.commit().map_err(|e| e.to_string())?;

    read_site(&connection, &input.id)?.ok_or_else(|| "读取更新后的站点失败".into())
}

#[tauri::command]
pub fn delete_site(database: State<'_, Database>, id: String) -> Result<(), String> {
    let connection = database.lock_conn()?;
    let changed = connection
        .execute("DELETE FROM directory_sites WHERE id = ?1", [id])
        .map_err(|error| error.to_string())?;
    if changed == 0 {
        return Err("找不到要删除的站点".into());
    }
    Ok(())
}

#[tauri::command]
pub fn toggle_personal(database: State<'_, Database>, id: String) -> Result<SiteRecord, String> {
    let connection = database.lock_conn()?;
    let changed = connection
        .execute(
            &format!(
                "UPDATE directory_sites
                 SET is_personal = CASE is_personal WHEN 0 THEN 1 ELSE 0 END,
                     is_pending = CASE
                       WHEN is_personal = 0 THEN 0
                       ELSE is_pending
                     END,
                     favorite = 0, updated_at = {NOW_SQL}
                 WHERE id = ?1"
            ),
            [&id],
        )
        .map_err(|error| error.to_string())?;
    if changed == 0 {
        return Err("找不到该站点".into());
    }
    read_site(&connection, &id)?.ok_or_else(|| "读取站点失败".into())
}

fn next_usage_state(is_personal: bool, is_pending: bool) -> (bool, bool) {
    // 单按钮按“未在用 → 在用 → 待定 → 未在用”循环。
    // 如果历史数据出现两个标记同时为 true，以 is_personal 优先并归零，
    // 这样写回时可以自动恢复互斥约束。
    if is_personal {
        (false, true)
    } else if is_pending {
        (false, false)
    } else {
        (true, false)
    }
}

#[tauri::command]
pub fn cycle_usage_state(database: State<'_, Database>, id: String) -> Result<SiteRecord, String> {
    let connection = database.lock_conn()?;
    let site = read_site(&connection, &id)?.ok_or_else(|| "找不到该站点".to_string())?;
    let (is_personal, is_pending) = next_usage_state(site.is_personal, site.is_pending);

    connection
        .execute(
            &format!(
                "UPDATE directory_sites
                 SET is_personal = ?1, is_pending = ?2, favorite = 0, updated_at = {NOW_SQL}
                 WHERE id = ?3"
            ),
            params![is_personal, is_pending, id],
        )
        .map_err(|error| error.to_string())?;

    read_site(&connection, &site.id)?.ok_or_else(|| "读取站点失败".into())
}

#[tauri::command]
pub fn toggle_pending(database: State<'_, Database>, id: String) -> Result<SiteRecord, String> {
    let connection = database.lock_conn()?;
    // 待定与在用互斥：标为待定时清除在用；取消待定仅清待定。
    let changed = connection
        .execute(
            &format!(
                "UPDATE directory_sites
                 SET is_pending = CASE is_pending WHEN 0 THEN 1 ELSE 0 END,
                     is_personal = CASE
                       WHEN is_pending = 0 THEN 0
                       ELSE is_personal
                     END,
                     favorite = 0, updated_at = {NOW_SQL}
                 WHERE id = ?1"
            ),
            [&id],
        )
        .map_err(|error| error.to_string())?;
    if changed == 0 {
        return Err("找不到该站点".into());
    }
    read_site(&connection, &id)?.ok_or_else(|| "读取站点失败".into())
}

#[tauri::command]
pub fn set_usage_state(
    database: State<'_, Database>,
    id: String,
    state: String,
) -> Result<SiteRecord, String> {
    let (is_personal, is_pending) = match state.as_str() {
        "personal" => (true, false),
        "pending" => (false, true),
        "unused" => (false, false),
        _ => return Err("未知的使用状态".into()),
    };
    let connection = database.lock_conn()?;
    let changed = connection
        .execute(
            &format!(
                "UPDATE directory_sites
                 SET is_personal = ?1, is_pending = ?2, favorite = 0, updated_at = {NOW_SQL}
                 WHERE id = ?3"
            ),
            params![is_personal, is_pending, id],
        )
        .map_err(|error| error.to_string())?;
    if changed == 0 {
        return Err("找不到该站点".into());
    }
    read_site(&connection, &id)?.ok_or_else(|| "读取站点失败".into())
}

#[tauri::command]
pub fn toggle_hidden(database: State<'_, Database>, id: String) -> Result<SiteRecord, String> {
    let connection = database.lock_conn()?;
    let changed = connection
        .execute(
            "UPDATE directory_sites SET hidden = CASE hidden WHEN 0 THEN 1 ELSE 0 END WHERE id = ?1",
            [&id],
        )
        .map_err(|error| error.to_string())?;
    if changed == 0 {
        return Err("找不到该站点".into());
    }
    read_site(&connection, &id)?.ok_or_else(|| "读取站点失败".into())
}

#[tauri::command]
pub fn toggle_runaway(database: State<'_, Database>, id: String) -> Result<SiteRecord, String> {
    let connection = database.lock_conn()?;
    let changed = connection
        .execute(
            &format!("UPDATE directory_sites SET is_runaway = CASE is_runaway WHEN 0 THEN 1 ELSE 0 END, updated_at = {NOW_SQL} WHERE id = ?1"),
            [&id],
        )
        .map_err(|error| error.to_string())?;
    if changed == 0 {
        return Err("找不到该站点".into());
    }
    read_site(&connection, &id)?.ok_or_else(|| "读取站点失败".into())
}

#[cfg(test)]
mod tests {
    use super::next_usage_state;

    #[test]
    fn cycles_unused_to_personal() {
        assert_eq!(next_usage_state(false, false), (true, false));
    }

    #[test]
    fn cycles_personal_to_pending() {
        assert_eq!(next_usage_state(true, false), (false, true));
    }

    #[test]
    fn cycles_pending_to_unused() {
        assert_eq!(next_usage_state(false, true), (false, false));
    }

    #[test]
    fn repairs_invalid_dual_state_by_preferring_personal() {
        // 异常双标记时按 is_personal 优先，进入“在用 → 待定”
        assert_eq!(next_usage_state(true, true), (false, true));
    }
}
