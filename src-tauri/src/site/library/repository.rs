//! SQLite reads shared by list views and single-site mutation responses.
//! The site row has one mapping; related rows attach by ID in O(sites + relations).
use crate::db::read_cached_usage_sites;
use crate::models::{ExtensionLink, LibraryData, Maintainer, SeedPayload, SiteRecord, SEED_JSON};
use rusqlite::{params_from_iter, Connection, OptionalExtension, Row};
use std::collections::HashMap;
use std::sync::OnceLock;

const SITE_COLUMNS: &str = "id, name, description, registration_limit, icon, api_base_url, aux_base_url, system_type,
    supports_immersive_translation, supports_ldc, supports_checkin, supports_nsfw,
    checkin_url, checkin_note, benefit_url, rate_limit, status_url,
    is_only_maintainer_visible, requires_invite_code, is_runaway, is_fake_charity,
    has_pending_report, is_personal, is_pending, use_system_proxy, use_proxy_pool, favorite, hidden, updated_at";
const SITE_ORDER: &str = "is_personal DESC, is_pending DESC, datetime(updated_at) DESC, rowid DESC";

fn site_from_row(row: &Row<'_>) -> rusqlite::Result<SiteRecord> {
    Ok(SiteRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        registration_limit: row.get(3)?,
        icon: row.get(4)?,
        api_base_url: row.get(5)?,
        aux_base_url: row.get(6)?,
        system_type: row.get(7)?,
        supports_immersive_translation: row.get::<_, i64>(8)? != 0,
        supports_ldc: row.get::<_, i64>(9)? != 0,
        supports_checkin: row.get::<_, i64>(10)? != 0,
        supports_nsfw: row.get::<_, i64>(11)? != 0,
        checkin_url: row.get(12)?,
        checkin_note: row.get(13)?,
        benefit_url: row.get(14)?,
        rate_limit: row.get(15)?,
        status_url: row.get(16)?,
        is_only_maintainer_visible: row.get::<_, i64>(17)? != 0,
        requires_invite_code: row.get::<_, i64>(18)? != 0,
        is_runaway: row.get::<_, i64>(19)? != 0,
        is_fake_charity: row.get::<_, i64>(20)? != 0,
        has_pending_report: row.get::<_, i64>(21)? != 0,
        is_personal: row.get::<_, i64>(22)? != 0,
        is_pending: row.get::<_, i64>(23)? != 0,
        use_system_proxy: row.get::<_, i64>(24)? != 0,
        use_proxy_pool: row.get::<_, i64>(25)? != 0,
        favorite: row.get::<_, i64>(26)? != 0,
        hidden: row.get::<_, i64>(27)? != 0,
        updated_at: row.get(28)?,
        tags: Vec::new(),
        maintainers: Vec::new(),
        extension_links: Vec::new(),
    })
}

pub(crate) fn read_site(connection: &Connection, id: &str) -> Result<Option<SiteRecord>, String> {
    let site = connection
        .prepare_cached(&format!(
            "SELECT {SITE_COLUMNS} FROM directory_sites WHERE id = ?1"
        ))
        .and_then(|mut statement| statement.query_row([id], site_from_row).optional())
        .map_err(|error| format!("读取站点失败：{error}"))?;
    let Some(mut site) = site else {
        return Ok(None);
    };
    attach_relations(connection, std::slice::from_mut(&mut site), Some(id))?;
    Ok(Some(site))
}

pub(crate) fn read_sites(connection: &Connection) -> Result<Vec<SiteRecord>, String> {
    let mut statement = connection
        .prepare_cached(&format!(
            "SELECT {SITE_COLUMNS} FROM directory_sites ORDER BY {SITE_ORDER}"
        ))
        .map_err(|error| format!("查询站点列表失败：{error}"))?;
    let mut sites = statement
        .query_map([], site_from_row)
        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
        .map_err(|error| format!("读取站点列表失败：{error}"))?;
    attach_relations(connection, &mut sites, None)?;
    Ok(sites)
}

/// List reads issue one query for each relation table, regardless of the site count.
/// Single-site reads use the same mapping with an indexed WHERE site_id = ?1.
fn attach_relations(
    connection: &Connection,
    sites: &mut [SiteRecord],
    site_id: Option<&str>,
) -> Result<(), String> {
    if sites.is_empty() {
        return Ok(());
    }
    let positions: HashMap<_, _> = sites
        .iter()
        .enumerate()
        .map(|(index, site)| (site.id.clone(), index))
        .collect();
    for (id, tag) in related_rows(connection, "site_tags", "site_id, tag", site_id, |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        if let Some(&index) = positions.get(&id) {
            sites[index].tags.push(tag);
        }
    }
    for (id, maintainer) in related_rows(
        connection,
        "site_maintainers",
        "site_id, name, maintainer_id, username, profile_url",
        site_id,
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                Maintainer {
                    name: row.get(1)?,
                    id: row.get(2)?,
                    username: row.get(3)?,
                    profile_url: row.get(4)?,
                },
            ))
        },
    )? {
        if let Some(&index) = positions.get(&id) {
            sites[index].maintainers.push(maintainer);
        }
    }
    for (id, link) in related_rows(
        connection,
        "site_extensions",
        "site_id, label, url",
        site_id,
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                ExtensionLink {
                    label: row.get(1)?,
                    url: row.get(2)?,
                },
            ))
        },
    )? {
        if let Some(&index) = positions.get(&id) {
            sites[index].extension_links.push(link);
        }
    }
    Ok(())
}

fn related_rows<T>(
    connection: &Connection,
    table: &str,
    columns: &str,
    site_id: Option<&str>,
    map: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, String> {
    // SQL identifiers only come from the static call sites above; IDs remain bound parameters.
    let filter = if site_id.is_some() {
        " WHERE site_id = ?1"
    } else {
        ""
    };
    let mut statement = connection
        .prepare_cached(&format!(
            "SELECT {columns} FROM {table}{filter} ORDER BY rowid"
        ))
        .map_err(|error| format!("查询站点关联表 {table} 失败：{error}"))?;
    statement
        .query_map(params_from_iter(site_id), map)
        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
        .map_err(|error| format!("读取站点关联表 {table} 失败：{error}"))
}

fn suggested_tags() -> Result<Vec<String>, String> {
    static TAGS: OnceLock<Result<Vec<String>, String>> = OnceLock::new();
    TAGS.get_or_init(|| {
        serde_json::from_str::<SeedPayload>(SEED_JSON)
            .map(|seed| seed.tags)
            .map_err(|error| format!("读取预置标签失败：{error}"))
    })
    .clone()
}

pub(crate) fn read_library(connection: &Connection) -> Result<LibraryData, String> {
    Ok(LibraryData {
        sites: read_sites(connection)?,
        suggested_tags: suggested_tags()?,
        usage_sites: read_cached_usage_sites(connection)?,
    })
}

/// A caller only awaits this future; SQLite's synchronous mutex stays in the blocking pool.
pub(crate) async fn load_library(
    database: std::sync::Arc<crate::models::Database>,
) -> Result<LibraryData, String> {
    tokio::task::spawn_blocking(move || {
        let connection = database.lock_conn()?;
        read_library(&connection)
    })
    .await
    .map_err(|error| format!("读取站点库任务失败：{error}"))?
}

#[cfg(test)]
#[path = "repository_tests.rs"]
mod tests;
