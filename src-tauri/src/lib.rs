use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::{Manager, State};
use url::Url;

const SEED_JSON: &str = include_str!("../resources/sites.json");
const NOW_SQL: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

struct Database(std::sync::Mutex<Connection>);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Maintainer {
    name: String,
    id: String,
    username: String,
    profile_url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ExtensionLink {
    label: String,
    url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct SiteRecord {
    id: String,
    name: String,
    description: String,
    registration_limit: i64,
    icon: String,
    api_base_url: String,
    tags: Vec<String>,
    supports_immersive_translation: bool,
    supports_ldc: bool,
    supports_checkin: bool,
    supports_nsfw: bool,
    checkin_url: String,
    checkin_note: String,
    benefit_url: String,
    maintainers: Vec<Maintainer>,
    rate_limit: String,
    status_url: String,
    extension_links: Vec<ExtensionLink>,
    is_only_maintainer_visible: bool,
    requires_invite_code: bool,
    is_runaway: bool,
    is_fake_charity: bool,
    has_pending_report: bool,
    is_personal: bool,
    favorite: bool,
    hidden: bool,
    updated_at: String,
}

#[derive(Debug, Deserialize)]
struct SeedPayload {
    sites: Vec<SiteRecord>,
    tags: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LibraryData {
    sites: Vec<SiteRecord>,
    suggested_tags: Vec<String>,
}

impl Database {
    fn open(path: &Path) -> Result<Self, String> {
        let mut connection = Connection::open(path).map_err(|error| error.to_string())?;
        connection
            .execute_batch(
                "
                PRAGMA foreign_keys = ON;
                PRAGMA journal_mode = WAL;

                CREATE TABLE IF NOT EXISTS directory_sites (
                    id          TEXT PRIMARY KEY,
                    data        TEXT NOT NULL,
                    favorite    INTEGER NOT NULL DEFAULT 0,
                    hidden      INTEGER NOT NULL DEFAULT 0,
                    created_at  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    updated_at  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );

                CREATE TABLE IF NOT EXISTS app_meta (
                    key   TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_directory_sites_favorite ON directory_sites(favorite);
                CREATE INDEX IF NOT EXISTS idx_directory_sites_hidden ON directory_sites(hidden);
                CREATE INDEX IF NOT EXISTS idx_directory_sites_updated ON directory_sites(updated_at);
                ",
            )
            .map_err(|error| error.to_string())?;

        seed_database(&mut connection)?;
        Ok(Self(std::sync::Mutex::new(connection)))
    }
}

fn seed_database(connection: &mut Connection) -> Result<(), String> {
    let seeded: Option<String> = connection
        .query_row(
            "SELECT value FROM app_meta WHERE key = 'directory_seed_version'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;

    if seeded.is_some() {
        return Ok(());
    }

    let payload: SeedPayload =
        serde_json::from_str(SEED_JSON).map_err(|error| error.to_string())?;
    let existing: i64 = connection
        .query_row("SELECT COUNT(*) FROM directory_sites", [], |row| row.get(0))
        .map_err(|error| error.to_string())?;

    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    if existing == 0 {
        for mut site in payload.sites {
            site.favorite = false;
            site.hidden = false;
            let data = serde_json::to_string(&site).map_err(|error| error.to_string())?;
            transaction
                .execute(
                    "INSERT OR IGNORE INTO directory_sites
                     (id, data, favorite, hidden, created_at, updated_at)
                     VALUES (?1, ?2, 0, 0,
                             COALESCE(NULLIF(?3, ''), CURRENT_TIMESTAMP),
                             COALESCE(NULLIF(?3, ''), CURRENT_TIMESTAMP))",
                    params![site.id, data, site.updated_at],
                )
                .map_err(|error| error.to_string())?;
        }
    }
    transaction
        .execute(
            "INSERT OR REPLACE INTO app_meta(key, value) VALUES ('directory_seed_version', '1')",
            [],
        )
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(())
}

fn validate_url(value: &str, label: &str, required: bool) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return if required {
            Err(format!("{label}不能为空"))
        } else {
            Ok(String::new())
        };
    }
    let parsed =
        Url::parse(value).map_err(|_| format!("{label}必须是完整的 http:// 或 https:// 地址"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("{label}仅支持 http:// 或 https:// 地址"));
    }
    Ok(parsed.to_string())
}

fn unique_trimmed(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter_map(|value| {
            let value = value.trim().to_string();
            if value.is_empty() || !seen.insert(value.clone()) {
                None
            } else {
                Some(value)
            }
        })
        .collect()
}

fn normalize_site(mut site: SiteRecord) -> Result<SiteRecord, String> {
    site.name = site.name.trim().to_string();
    if site.name.is_empty() {
        return Err("站点名称不能为空".into());
    }
    site.api_base_url = validate_url(&site.api_base_url, "API BASE URL", true)?;
    site.checkin_url = validate_url(&site.checkin_url, "签到页 URL", false)?;
    site.benefit_url = validate_url(&site.benefit_url, "福利站 URL", false)?;
    site.status_url = validate_url(&site.status_url, "状态页 URL", false)?;
    site.description = site.description.trim().to_string();
    site.rate_limit = site.rate_limit.trim().to_string();
    site.checkin_note = site.checkin_note.trim().to_string();
    site.registration_limit = site.registration_limit.clamp(0, 3);
    site.tags = unique_trimmed(site.tags);

    site.maintainers = site
        .maintainers
        .into_iter()
        .filter_map(|mut maintainer| {
            maintainer.name = maintainer.name.trim().to_string();
            maintainer.id = maintainer.id.trim().to_string();
            maintainer.username = maintainer.username.trim().to_string();
            maintainer.profile_url = maintainer.profile_url.trim().to_string();
            if maintainer.name.is_empty() && maintainer.profile_url.is_empty() {
                None
            } else {
                Some(maintainer)
            }
        })
        .collect();

    for maintainer in &mut site.maintainers {
        maintainer.profile_url = validate_url(&maintainer.profile_url, "维护者主页", false)?;
    }

    site.extension_links = site
        .extension_links
        .into_iter()
        .filter_map(|mut link| {
            link.label = link.label.trim().to_string();
            link.url = link.url.trim().to_string();
            if link.label.is_empty() && link.url.is_empty() {
                None
            } else {
                Some(link)
            }
        })
        .collect();

    for link in &mut site.extension_links {
        link.url = validate_url(&link.url, "扩展链接", true)?;
        if link.label.is_empty() {
            link.label = "扩展链接".into();
        }
    }

    Ok(site)
}

fn generated_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("local-{nanos:x}")
}

fn row_to_site(row: &rusqlite::Row<'_>) -> rusqlite::Result<SiteRecord> {
    let data: String = row.get(0)?;
    let mut site: SiteRecord = serde_json::from_str(&data).unwrap_or_default();
    site.favorite = row.get::<_, i64>(1)? != 0;
    site.hidden = row.get::<_, i64>(2)? != 0;
    site.updated_at = row.get(3)?;
    Ok(site)
}

fn read_site(connection: &Connection, id: &str) -> Result<Option<SiteRecord>, String> {
    connection
        .query_row(
            "SELECT data, favorite, hidden, updated_at FROM directory_sites WHERE id = ?1",
            [id],
            row_to_site,
        )
        .optional()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn list_library(database: State<'_, Database>) -> Result<LibraryData, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let mut statement = connection
        .prepare(
            "SELECT data, favorite, hidden, updated_at
             FROM directory_sites
             ORDER BY favorite DESC, datetime(updated_at) DESC, rowid DESC",
        )
        .map_err(|error| error.to_string())?;
    let sites = statement
        .query_map([], row_to_site)
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let payload: SeedPayload =
        serde_json::from_str(SEED_JSON).map_err(|error| error.to_string())?;
    Ok(LibraryData {
        sites,
        suggested_tags: payload.tags,
    })
}

#[tauri::command]
fn create_site(database: State<'_, Database>, mut input: SiteRecord) -> Result<SiteRecord, String> {
    input.id = generated_id();
    input.favorite = false;
    input.hidden = false;
    let input = normalize_site(input)?;
    let data = serde_json::to_string(&input).map_err(|error| error.to_string())?;
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    connection
        .execute(
            &format!(
                "INSERT INTO directory_sites (id, data, favorite, hidden, created_at, updated_at)
                 VALUES (?1, ?2, 0, 0, {NOW_SQL}, {NOW_SQL})"
            ),
            params![input.id, data],
        )
        .map_err(|error| error.to_string())?;
    read_site(&connection, &input.id)?.ok_or_else(|| "创建站点失败".into())
}

#[tauri::command]
fn update_site(
    database: State<'_, Database>,
    id: String,
    mut input: SiteRecord,
) -> Result<SiteRecord, String> {
    input.id = id.clone();
    let input = normalize_site(input)?;
    let data = serde_json::to_string(&input).map_err(|error| error.to_string())?;
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let changed = connection
        .execute(
            &format!("UPDATE directory_sites SET data = ?1, updated_at = {NOW_SQL} WHERE id = ?2"),
            params![data, id],
        )
        .map_err(|error| error.to_string())?;
    if changed == 0 {
        return Err("找不到要更新的站点".into());
    }
    read_site(&connection, &input.id)?.ok_or_else(|| "读取更新后的站点失败".into())
}

#[tauri::command]
fn delete_site(database: State<'_, Database>, id: String) -> Result<(), String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let changed = connection
        .execute("DELETE FROM directory_sites WHERE id = ?1", [id])
        .map_err(|error| error.to_string())?;
    if changed == 0 {
        return Err("找不到要删除的站点".into());
    }
    Ok(())
}

#[tauri::command]
fn toggle_favorite(database: State<'_, Database>, id: String) -> Result<SiteRecord, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let changed = connection
        .execute(
            "UPDATE directory_sites SET favorite = CASE favorite WHEN 0 THEN 1 ELSE 0 END WHERE id = ?1",
            [&id],
        )
        .map_err(|error| error.to_string())?;
    if changed == 0 {
        return Err("找不到该站点".into());
    }
    read_site(&connection, &id)?.ok_or_else(|| "读取站点失败".into())
}

#[tauri::command]
fn toggle_hidden(database: State<'_, Database>, id: String) -> Result<SiteRecord, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
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
fn toggle_runaway(database: State<'_, Database>, id: String) -> Result<SiteRecord, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let mut site = read_site(&connection, &id)?.ok_or_else(|| "找不到该站点".to_string())?;
    site.is_runaway = !site.is_runaway;
    let data = serde_json::to_string(&site).map_err(|error| error.to_string())?;
    connection
        .execute(
            &format!("UPDATE directory_sites SET data = ?1, updated_at = {NOW_SQL} WHERE id = ?2"),
            params![data, id],
        )
        .map_err(|error| error.to_string())?;
    read_site(&connection, &site.id)?.ok_or_else(|| "读取站点失败".into())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let app_data_dir = app
                .path()
                .app_data_dir()
                .map_err(|error| error.to_string())?;
            fs::create_dir_all(&app_data_dir)?;
            let database = Database::open(&app_data_dir.join("sites.sqlite3"))
                .map_err(std::io::Error::other)?;
            app.manage(database);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_library,
            create_site,
            update_site,
            delete_site,
            toggle_favorite,
            toggle_hidden,
            toggle_runaway
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}
