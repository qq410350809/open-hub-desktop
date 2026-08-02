use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{Emitter, Manager, State};
use url::Url;

mod chrome_local_storage;
mod chrome_session;

const SEED_JSON: &str = include_str!("../resources/sites.json");
const NOW_SQL: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";
const REMOTE_ROOT_URL: &str = "https://ldoh.105117.xyz/";
const REMOTE_USER_URL: &str = "https://ldoh.105117.xyz/api/ld/user";
const REMOTE_SITES_URL: &str = "https://ldoh.105117.xyz/api/sites";
const REMOTE_SESSION_COOKIE: &str = "ld_auth_session";
const NETWORK_PROXY_KEY: &str = "network_proxy";
const ZERO_V_ZERO_CONSOLE_URL: &str = "https://0v0.club/";

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
    #[serde(
        alias = "siteType",
        alias = "site_type",
        alias = "apiType",
        alias = "api_type",
        alias = "platform",
        alias = "system"
    )]
    system_type: String,
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
    usage_sites: Vec<chrome_session::ChromeSiteSessionMatch>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncSitesResult {
    added: usize,
    updated: usize,
    total: usize,
    profile_name: String,
    account_name: String,
    user_name: String,
    runaway: bool,
    site_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncSitesProgress {
    run_id: u64,
    stage: String,
    status: String,
    message: String,
}

fn emit_sync_progress(
    app: &tauri::AppHandle,
    run_id: u64,
    stage: &str,
    status: &str,
    message: String,
) {
    let _ = app.emit(
        "sync-sites-progress",
        SyncSitesProgress {
            run_id,
            stage: stage.into(),
            status: status.into(),
            message,
        },
    );
}

fn emit_chrome_account_progress(
    app: &tauri::AppHandle,
    run_id: u64,
    stage: &str,
    status: &str,
    message: impl Into<String>,
) {
    let _ = app.emit(
        "chrome-account-sync-progress",
        SyncSitesProgress {
            run_id,
            stage: stage.into(),
            status: status.into(),
            message: message.into(),
        },
    );
}

fn emit_optional_sync_progress(
    app: &tauri::AppHandle,
    run_id: Option<u64>,
    stage: &str,
    status: &str,
    message: String,
) {
    if let Some(run_id) = run_id {
        emit_sync_progress(app, run_id, stage, status, message);
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteUserInfo {
    name: String,
    username: String,
    avatar_url: String,
    profile_name: String,
    account_name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChromeUsageScanResult {
    scanned: usize,
    detected: usize,
    accounts: usize,
    warnings: usize,
    newly_marked: usize,
    sites: Vec<chrome_session::ChromeSiteSessionMatch>,
}

#[derive(Debug, Default)]
struct SiteAccountSnapshot {
    username: String,
    remaining: Option<f64>,
    used: Option<f64>,
    total: Option<f64>,
    unit: String,
}

#[derive(Debug, Clone, Default)]
struct CheckinSnapshot {
    enabled: bool,
    checked_in_today: bool,
    error: String,
}

#[derive(Debug)]
struct SiteAccountRefresh {
    account: SiteAccountSnapshot,
    is_valid: bool,
    sync_error: String,
    checkin: CheckinSnapshot,
}

enum NewApiAuth {
    Legacy {
        cookie_header: String,
        user_id: String,
    },
}

fn site_matches_requested_scope(
    site_id: &str,
    requested_site_id: Option<&str>,
    site_id_was_supplied: bool,
    requested_site_ids: &HashSet<String>,
    site_ids_were_supplied: bool,
) -> bool {
    if let Some(requested) = requested_site_id {
        requested == site_id
    } else if site_id_was_supplied {
        false
    } else if site_ids_were_supplied {
        requested_site_ids.contains(site_id)
    } else {
        true
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChromeBridgeAccountResult {
    ok: bool,
    #[serde(default)]
    error: String,
    account: Option<serde_json::Value>,
    #[serde(default)]
    checkin_enabled: bool,
    #[serde(default)]
    checked_in_today: bool,
    #[serde(default)]
    checkin_error: String,
}

impl Database {
    fn open(path: &Path) -> Result<Self, String> {
        let mut connection = Connection::open(path).map_err(|error| error.to_string())?;

        let has_data_col: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('directory_sites') WHERE name='data'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if has_data_col > 0 {
            connection.execute_batch("DROP TABLE directory_sites; DROP TABLE IF EXISTS site_tags; DROP TABLE IF EXISTS site_maintainers; DROP TABLE IF EXISTS site_extensions; DROP TABLE IF EXISTS app_meta;").ok();
        }

        connection
            .execute_batch(
                "
                PRAGMA foreign_keys = ON;
                PRAGMA journal_mode = WAL;

                CREATE TABLE IF NOT EXISTS directory_sites (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    description TEXT NOT NULL,
                    registration_limit INTEGER NOT NULL,
                    icon TEXT NOT NULL,
                    api_base_url TEXT NOT NULL,
                    system_type TEXT NOT NULL DEFAULT '',
                    supports_immersive_translation INTEGER NOT NULL,
                    supports_ldc INTEGER NOT NULL,
                    supports_checkin INTEGER NOT NULL,
                    supports_nsfw INTEGER NOT NULL,
                    checkin_url TEXT NOT NULL,
                    checkin_note TEXT NOT NULL,
                    benefit_url TEXT NOT NULL,
                    rate_limit TEXT NOT NULL,
                    status_url TEXT NOT NULL,
                    is_only_maintainer_visible INTEGER NOT NULL,
                    requires_invite_code INTEGER NOT NULL,
                    is_runaway INTEGER NOT NULL,
                    is_fake_charity INTEGER NOT NULL,
                    has_pending_report INTEGER NOT NULL,
                    is_personal INTEGER NOT NULL,
                    favorite INTEGER NOT NULL DEFAULT 0,
                    hidden INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );

                CREATE TABLE IF NOT EXISTS site_tags (
                    site_id TEXT NOT NULL,
                    tag TEXT NOT NULL,
                    FOREIGN KEY(site_id) REFERENCES directory_sites(id) ON DELETE CASCADE
                );

                CREATE TABLE IF NOT EXISTS site_maintainers (
                    site_id TEXT NOT NULL,
                    name TEXT NOT NULL,
                    maintainer_id TEXT NOT NULL,
                    username TEXT NOT NULL,
                    profile_url TEXT NOT NULL,
                    FOREIGN KEY(site_id) REFERENCES directory_sites(id) ON DELETE CASCADE
                );

                CREATE TABLE IF NOT EXISTS site_extensions (
                    site_id TEXT NOT NULL,
                    label TEXT NOT NULL,
                    url TEXT NOT NULL,
                    FOREIGN KEY(site_id) REFERENCES directory_sites(id) ON DELETE CASCADE
                );

                CREATE TABLE IF NOT EXISTS site_accounts (
                    site_id TEXT NOT NULL,
                    profile_id TEXT NOT NULL,
                    domain TEXT NOT NULL,
                    cookie_count INTEGER NOT NULL,
                    cookie_names TEXT NOT NULL,
                    profile_name TEXT NOT NULL,
                    account_name TEXT NOT NULL,
                    username TEXT NOT NULL DEFAULT '',
                    api_key_count INTEGER NOT NULL DEFAULT 0,
                    api_model_count INTEGER NOT NULL DEFAULT 0,
                    remaining REAL,
                    used REAL,
                    total REAL,
                    unit TEXT NOT NULL DEFAULT '',
                    is_valid INTEGER NOT NULL DEFAULT 0,
                    sync_error TEXT NOT NULL DEFAULT '',
                    checkin_enabled INTEGER NOT NULL DEFAULT 0,
                    checked_in_today INTEGER NOT NULL DEFAULT 0,
                    checkin_error TEXT NOT NULL DEFAULT '',
                    checkin_date TEXT NOT NULL DEFAULT '',
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    PRIMARY KEY (site_id, profile_id, domain),
                    FOREIGN KEY(site_id) REFERENCES directory_sites(id) ON DELETE CASCADE
                );

                CREATE TABLE IF NOT EXISTS app_meta (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_directory_sites_favorite ON directory_sites(favorite);
                CREATE INDEX IF NOT EXISTS idx_directory_sites_hidden ON directory_sites(hidden);
                CREATE INDEX IF NOT EXISTS idx_directory_sites_updated ON directory_sites(updated_at);
                CREATE INDEX IF NOT EXISTS idx_site_accounts_site ON site_accounts(site_id);
                ",
            )
            .map_err(|error| error.to_string())?;

        ensure_site_account_columns(&connection)?;

        let has_system_type: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('directory_sites') WHERE name='system_type'",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if has_system_type == 0 {
            connection
                .execute(
                    "ALTER TABLE directory_sites ADD COLUMN system_type TEXT NOT NULL DEFAULT ''",
                    [],
                )
                .map_err(|error| error.to_string())?;
        }

        seed_database(&mut connection)?;
        migrate_legacy_favorites_to_personal(&connection)?;
        connection
            .execute(
                "UPDATE directory_sites
                 SET system_type = CASE
                    WHEN LOWER(checkin_url) LIKE '%/console/%' THEN 'NewAPI'
                    WHEN LOWER(checkin_url) LIKE '%/profile%'
                      OR LOWER(checkin_url) LIKE '%/dashboard%' THEN 'Sub2API'
                    ELSE system_type
                 END
                 WHERE TRIM(system_type) = ''",
                [],
            )
            .map_err(|error| error.to_string())?;
        Ok(Self(std::sync::Mutex::new(connection)))
    }
}

fn migrate_legacy_favorites_to_personal(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "UPDATE directory_sites SET is_personal = 1 WHERE favorite = 1;
             UPDATE directory_sites SET favorite = 0 WHERE favorite <> 0;",
        )
        .map_err(|error| error.to_string())
}

fn ensure_site_account_columns(connection: &Connection) -> Result<(), String> {
    for (name, definition) in [
        ("username", "TEXT NOT NULL DEFAULT ''"),
        ("api_key_count", "INTEGER NOT NULL DEFAULT 0"),
        ("api_model_count", "INTEGER NOT NULL DEFAULT 0"),
        ("remaining", "REAL"),
        ("used", "REAL"),
        ("total", "REAL"),
        ("unit", "TEXT NOT NULL DEFAULT ''"),
        ("is_valid", "INTEGER NOT NULL DEFAULT 0"),
        ("sync_error", "TEXT NOT NULL DEFAULT ''"),
        ("checkin_enabled", "INTEGER NOT NULL DEFAULT 0"),
        ("checked_in_today", "INTEGER NOT NULL DEFAULT 0"),
        ("checkin_error", "TEXT NOT NULL DEFAULT ''"),
        ("checkin_date", "TEXT NOT NULL DEFAULT ''"),
    ] {
        let exists = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('site_accounts') WHERE name = ?1",
                [name],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            > 0;
        if !exists {
            connection
                .execute(
                    &format!("ALTER TABLE site_accounts ADD COLUMN {name} {definition}"),
                    [],
                )
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn read_cached_usage_sites(
    connection: &Connection,
) -> Result<Vec<chrome_session::ChromeSiteSessionMatch>, String> {
    let mut statement = connection
        .prepare(
            "SELECT site_id, profile_id, domain, cookie_count, cookie_names, profile_name, account_name,
                    username, api_key_count, api_model_count,
                    remaining, used, total, unit, is_valid, sync_error,
                    checkin_enabled,
                    CASE WHEN checkin_date = date('now', 'localtime') THEN checked_in_today ELSE 0 END,
                    checkin_error, updated_at
             FROM site_accounts
             ORDER BY site_id, rowid",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            let cookie_names_json = row.get::<_, String>(4)?;
            Ok((
                row.get::<_, String>(0)?,
                chrome_session::ChromeSessionInfo {
                    profile_id: row.get(1)?,
                    domain: row.get(2)?,
                    cookie_count: row.get::<_, i64>(3)?.max(0) as usize,
                    cookie_names: serde_json::from_str(&cookie_names_json).unwrap_or_default(),
                    profile_name: row.get(5)?,
                    account_name: row.get(6)?,
                    username: row.get(7)?,
                    api_key_count: row.get::<_, i64>(8)?.max(0) as usize,
                    api_model_count: row.get::<_, i64>(9)?.max(0) as usize,
                    remaining: row.get(10)?,
                    used: row.get(11)?,
                    total: row.get(12)?,
                    unit: row.get(13)?,
                    is_valid: row.get::<_, i64>(14)? != 0,
                    sync_error: row.get(15)?,
                    checkin_enabled: row.get::<_, i64>(16)? != 0,
                    checked_in_today: row.get::<_, i64>(17)? != 0,
                    checkin_error: row.get(18)?,
                    account_updated_at: row.get(19)?,
                },
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;

    let mut sites: Vec<chrome_session::ChromeSiteSessionMatch> = Vec::new();
    for (site_id, session) in rows {
        if let Some(site) = sites.iter_mut().find(|site| site.site_id == site_id) {
            site.sessions.push(session);
        } else {
            sites.push(chrome_session::ChromeSiteSessionMatch {
                site_id,
                sessions: vec![session],
            });
        }
    }
    Ok(sites)
}

fn read_network_proxy(database: &Database) -> Result<String, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    connection
        .query_row(
            "SELECT value FROM app_meta WHERE key = ?1",
            [NETWORK_PROXY_KEY],
            |row| row.get(0),
        )
        .optional()
        .map(|value| value.unwrap_or_default())
        .map_err(|error| error.to_string())
}

fn persist_site_system_types(
    database: &Database,
    system_types: &HashMap<String, String>,
) -> Result<(), String> {
    if system_types.is_empty() {
        return Ok(());
    }
    let mut connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    for (site_id, system_type) in system_types {
        transaction
            .execute(
                "UPDATE directory_sites SET system_type = ?2 WHERE id = ?1",
                params![site_id, system_type],
            )
            .map_err(|error| error.to_string())?;
    }
    transaction.commit().map_err(|error| error.to_string())
}

fn normalize_network_proxy(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(String::new());
    }

    let url = Url::parse(value).map_err(|_| "代理地址必须是完整的 http:// 或 https:// 地址")?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("代理地址必须是完整的 http:// 或 https:// 地址".into());
    }
    reqwest::Proxy::all(value).map_err(|_| "代理地址格式无效".to_string())?;
    Ok(value.to_string())
}

fn build_http_client(
    database: &Database,
    timeout: Duration,
    redirects: usize,
    purpose: &str,
) -> Result<reqwest::Client, String> {
    let proxy_url = read_network_proxy(database)?;
    let mut builder = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::limited(redirects));
    if proxy_url.is_empty() {
        builder = builder.no_proxy();
    } else {
        let proxy = reqwest::Proxy::all(&proxy_url).map_err(|_| "已配置的网络代理地址无效")?;
        builder = builder.proxy(proxy);
    }
    builder
        .build()
        .map_err(|error| format!("无法初始化{purpose}：{error}"))
}

#[tauri::command]
fn get_network_proxy(database: State<'_, Database>) -> Result<String, String> {
    read_network_proxy(&database)
}

#[tauri::command]
fn set_network_proxy(database: State<'_, Database>, proxy_url: String) -> Result<String, String> {
    let proxy_url = normalize_network_proxy(&proxy_url)?;
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    connection
        .execute(
            "INSERT INTO app_meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![NETWORK_PROXY_KEY, proxy_url],
        )
        .map_err(|error| error.to_string())?;
    Ok(proxy_url)
}

fn insert_site_transaction(
    transaction: &rusqlite::Transaction,
    site: &SiteRecord,
) -> Result<(), String> {
    transaction.execute(
        "INSERT OR REPLACE INTO directory_sites (
            id, name, description, registration_limit, icon, api_base_url, system_type,
            supports_immersive_translation, supports_ldc, supports_checkin, supports_nsfw,
            checkin_url, checkin_note, benefit_url, rate_limit, status_url,
            is_only_maintainer_visible, requires_invite_code, is_runaway, is_fake_charity,
            has_pending_report, is_personal, favorite, hidden, created_at, updated_at
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7,
            ?8, ?9, ?10, ?11,
            ?12, ?13, ?14, ?15, ?16,
            ?17, ?18, ?19, ?20,
            ?21, ?22, ?23, ?24,
            COALESCE(NULLIF(?25, ''), CURRENT_TIMESTAMP), COALESCE(NULLIF(?25, ''), CURRENT_TIMESTAMP)
        )",
        params![
            site.id, site.name, site.description, site.registration_limit, site.icon, site.api_base_url, site.system_type,
            site.supports_immersive_translation, site.supports_ldc, site.supports_checkin, site.supports_nsfw,
            site.checkin_url, site.checkin_note, site.benefit_url, site.rate_limit, site.status_url,
            site.is_only_maintainer_visible, site.requires_invite_code, site.is_runaway, site.is_fake_charity,
            site.has_pending_report, site.is_personal, site.favorite, site.hidden, site.updated_at
        ],
    ).map_err(|error| error.to_string())?;

    transaction
        .execute("DELETE FROM site_tags WHERE site_id = ?1", [&site.id])
        .map_err(|error| error.to_string())?;
    for tag in &site.tags {
        transaction
            .execute(
                "INSERT INTO site_tags (site_id, tag) VALUES (?1, ?2)",
                params![site.id, tag],
            )
            .map_err(|error| error.to_string())?;
    }

    transaction
        .execute(
            "DELETE FROM site_maintainers WHERE site_id = ?1",
            [&site.id],
        )
        .map_err(|error| error.to_string())?;
    for maintainer in &site.maintainers {
        transaction.execute(
            "INSERT INTO site_maintainers (site_id, name, maintainer_id, username, profile_url) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![site.id, maintainer.name, maintainer.id, maintainer.username, maintainer.profile_url],
        ).map_err(|error| error.to_string())?;
    }

    transaction
        .execute("DELETE FROM site_extensions WHERE site_id = ?1", [&site.id])
        .map_err(|error| error.to_string())?;
    for ext in &site.extension_links {
        transaction
            .execute(
                "INSERT INTO site_extensions (site_id, label, url) VALUES (?1, ?2, ?3)",
                params![site.id, ext.label, ext.url],
            )
            .map_err(|error| error.to_string())?;
    }

    Ok(())
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
            insert_site_transaction(&transaction, &site)?;
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
    site.system_type = canonical_system_type(&site.system_type);
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

fn canonical_system_type(value: &str) -> String {
    let compact = value
        .trim()
        .to_ascii_lowercase()
        .replace(['-', '_', ' '], "");
    match compact.as_str() {
        "sub2api" => "Sub2API".into(),
        "newapi" => "NewAPI".into(),
        "0v0" | "zerovzero" => "0v0".into(),
        _ => String::new(),
    }
}

fn is_zero_v_zero_site(name: &str, api_base_url: &str, system_type: &str) -> bool {
    system_type.trim().eq_ignore_ascii_case("0v0")
        || name.trim().eq_ignore_ascii_case("0v0")
        || Url::parse(api_base_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
            .is_some_and(|host| {
                matches!(
                    host.as_str(),
                    "0v0.club" | "docs.0v0.club" | "docs.0v0.xyz" | "api.0v0.club"
                )
            })
}

fn account_base_url(name: &str, api_base_url: &str, system_type: &str) -> String {
    if is_zero_v_zero_site(name, api_base_url, system_type) {
        ZERO_V_ZERO_CONSOLE_URL.into()
    } else {
        api_base_url.to_string()
    }
}

fn infer_remote_system_type(site: &serde_json::Map<String, serde_json::Value>) -> String {
    for key in [
        "systemType",
        "system_type",
        "siteType",
        "site_type",
        "apiType",
        "api_type",
        "platform",
        "system",
        "type",
    ] {
        if let Some(value) = site.get(key).and_then(serde_json::Value::as_str) {
            let system_type = canonical_system_type(value);
            if !system_type.is_empty() {
                return system_type;
            }
        }
    }
    for (key, system_type) in [
        ("isSub2Api", "Sub2API"),
        ("is_sub2api", "Sub2API"),
        ("isNewApi", "NewAPI"),
        ("is_newapi", "NewAPI"),
    ] {
        if site
            .get(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            return system_type.into();
        }
    }
    if let Some(tags) = site.get("tags").and_then(serde_json::Value::as_array) {
        for tag in tags.iter().filter_map(serde_json::Value::as_str) {
            let system_type = canonical_system_type(tag);
            if !system_type.is_empty() {
                return system_type;
            }
        }
    }

    let urls = ["checkinUrl", "checkin_url", "apiBaseUrl", "api_base_url"]
        .iter()
        .filter_map(|key| site.get(*key).and_then(serde_json::Value::as_str))
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    if urls.iter().any(|url| url.contains("/console/")) {
        "NewAPI".into()
    } else if urls
        .iter()
        .any(|url| url.contains("/profile") || url.contains("/dashboard"))
    {
        "Sub2API".into()
    } else {
        String::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EndpointProbe {
    status: reqwest::StatusCode,
    is_json: bool,
    is_challenge: bool,
}

#[derive(Debug)]
struct DiscoveryResponse {
    status: reqwest::StatusCode,
    content_type: String,
    body: String,
}

impl DiscoveryResponse {
    fn endpoint_probe(&self) -> EndpointProbe {
        EndpointProbe {
            status: self.status,
            is_json: serde_json::from_str::<serde_json::Value>(&self.body).is_ok(),
            is_challenge: shield_page_response(
                self.status,
                &self.content_type,
                false,
                self.body.as_bytes(),
            ),
        }
    }

    fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_str(&self.body).ok()
    }
}

fn normalize_import_base_url(value: &str) -> Result<Url, String> {
    let mut url = Url::parse(value.trim())
        .map_err(|_| "站点 URL 必须是完整的 http:// 或 https:// 地址".to_string())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("站点 URL 必须是完整的 http:// 或 https:// 地址".into());
    }
    url.set_username("")
        .map_err(|_| "站点 URL 不能包含登录凭据".to_string())?;
    url.set_password(None)
        .map_err(|_| "站点 URL 不能包含登录凭据".to_string())?;
    url.set_query(None);
    url.set_fragment(None);
    url.set_path("/");
    Ok(url)
}

async fn fetch_discovery_resource(
    client: reqwest::Client,
    url: Url,
    accept: &'static str,
) -> Option<DiscoveryResponse> {
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, accept)
        .header(reqwest::header::USER_AGENT, "OpenHub-Desktop/0.3")
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let bytes = response.bytes().await.ok()?;
    let body = String::from_utf8_lossy(&bytes[..bytes.len().min(1_048_576)]).into_owned();
    Some(DiscoveryResponse {
        status,
        content_type,
        body,
    })
}

fn json_data_object(
    value: &serde_json::Value,
) -> Option<&serde_json::Map<String, serde_json::Value>> {
    value
        .get("data")
        .and_then(serde_json::Value::as_object)
        .or_else(|| value.as_object())
}

fn discovered_json_string(value: &serde_json::Value, keys: &[&str]) -> String {
    json_data_object(value)
        .and_then(|object| {
            keys.iter()
                .find_map(|key| object.get(*key).and_then(serde_json::Value::as_str))
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
        .to_string()
}

fn discovered_json_bool(value: &serde_json::Value, keys: &[&str]) -> bool {
    json_data_object(value).is_some_and(|object| {
        keys.iter().any(|key| {
            object
                .get(*key)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        })
    })
}

fn decode_basic_html_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .trim()
        .to_string()
}

fn html_attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    for quote in ['\"', '\''] {
        let marker = format!("{name}={quote}");
        if let Some(marker_start) = lower.find(&marker) {
            let start = marker_start + marker.len();
            let end = tag[start..].find(quote)? + start;
            return Some(decode_basic_html_entities(&tag[start..end]));
        }
    }
    None
}

fn html_title(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let Some(open_start) = lower.find("<title") else {
        return String::new();
    };
    let Some(open_end) = lower[open_start..].find('>') else {
        return String::new();
    };
    let content_start = open_start + open_end + 1;
    let Some(content_end) = lower[content_start..].find("</title>") else {
        return String::new();
    };
    decode_basic_html_entities(&html[content_start..content_start + content_end])
}

fn html_meta_description(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut offset = 0;
    while let Some(start) = lower[offset..].find("<meta") {
        let start = offset + start;
        let Some(end) = lower[start..].find('>') else {
            break;
        };
        let tag = &html[start..=start + end];
        let is_description = html_attribute(tag, "name")
            .or_else(|| html_attribute(tag, "property"))
            .is_some_and(|value| {
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "description" | "og:description"
                )
            });
        if is_description {
            if let Some(content) = html_attribute(tag, "content") {
                return content;
            }
        }
        offset = start + end + 1;
        if offset >= lower.len() {
            break;
        }
    }
    String::new()
}

fn html_icon_href(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut offset = 0;
    while let Some(start) = lower[offset..].find("<link") {
        let start = offset + start;
        let Some(end) = lower[start..].find('>') else {
            break;
        };
        let tag = &html[start..=start + end];
        if html_attribute(tag, "rel")
            .is_some_and(|value| value.to_ascii_lowercase().contains("icon"))
        {
            if let Some(href) = html_attribute(tag, "href") {
                return href;
            }
        }
        offset = start + end + 1;
        if offset >= lower.len() {
            break;
        }
    }
    String::new()
}

fn resolve_discovered_url(base_url: &Url, value: &str) -> String {
    Url::parse(value.trim())
        .ok()
        .or_else(|| base_url.join(value.trim()).ok())
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .map(|url| url.to_string())
        .unwrap_or_default()
}

fn endpoint_probe_exists(probe: EndpointProbe) -> bool {
    probe.status == reqwest::StatusCode::UNAUTHORIZED
        || (probe.is_json
            && (probe.status.is_success() || probe.status == reqwest::StatusCode::FORBIDDEN))
}

fn shield_page_response(
    status: reqwest::StatusCode,
    content_type: &str,
    security_gateway_header: bool,
    body: &[u8],
) -> bool {
    if serde_json::from_slice::<serde_json::Value>(body).is_ok() {
        return false;
    }
    let first = body
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace());
    let looks_html = content_type.contains("text/html") || first == Some(b'<');
    if !looks_html && !security_gateway_header {
        return false;
    }
    let lower = String::from_utf8_lossy(&body[..body.len().min(200_000)]).to_ascii_lowercase();
    security_gateway_header
        || matches!(status.as_u16(), 403 | 429 | 503)
        || [
            "cf-chl-",
            "challenge-platform",
            "cloudflare ray id",
            "just a moment",
            "attention required",
            "acw_sc__v2",
            "acw_tc",
            "cdn_sec_tc",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
}

fn system_type_from_probes(
    newapi_probe: Option<EndpointProbe>,
    sub2api_probe: Option<EndpointProbe>,
) -> Option<&'static str> {
    if newapi_probe.is_some_and(endpoint_probe_exists) {
        Some("NewAPI")
    } else if sub2api_probe.is_some_and(endpoint_probe_exists) {
        Some("Sub2API")
    } else if newapi_probe.is_some_and(|probe| probe.status == reqwest::StatusCode::NOT_FOUND)
        && sub2api_probe.is_some_and(|probe| probe.status == reqwest::StatusCode::NOT_FOUND)
    {
        Some("")
    } else {
        None
    }
}

async fn probe_endpoint(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
) -> Option<EndpointProbe> {
    let url = Url::parse(base_url).ok()?.join(path).ok()?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, "OpenHub-Desktop/0.3")
        .timeout(Duration::from_secs(6))
        .send()
        .await
        .ok()?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let security_gateway_header = response.headers().contains_key("x-tengine-error")
        || response
            .headers()
            .get(reqwest::header::SERVER)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("ESA"))
        || response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .any(|value| {
                let lower = value.to_ascii_lowercase();
                lower.starts_with("acw_") || lower.starts_with("cdn_sec_")
            });
    let body = response.bytes().await.ok()?;
    let is_json = serde_json::from_slice::<serde_json::Value>(&body).is_ok();
    Some(EndpointProbe {
        status,
        is_json,
        is_challenge: shield_page_response(status, &content_type, security_gateway_header, &body),
    })
}

async fn probe_site_system_type_details(
    client: &reqwest::Client,
    base_url: &str,
) -> (Option<String>, bool) {
    let newapi_job = tauri::async_runtime::spawn({
        let client = client.clone();
        let base_url = base_url.to_string();
        async move { probe_endpoint(&client, &base_url, "/api/status").await }
    });
    let sub2api_job = tauri::async_runtime::spawn({
        let client = client.clone();
        let base_url = base_url.to_string();
        async move { probe_endpoint(&client, &base_url, "/setup/status").await }
    });
    let newapi_probe = newapi_job.await.ok().flatten();
    let sub2api_probe = sub2api_job.await.ok().flatten();
    let challenge = newapi_probe.is_some_and(|probe| probe.is_challenge)
        || sub2api_probe.is_some_and(|probe| probe.is_challenge);
    (
        system_type_from_probes(newapi_probe, sub2api_probe).map(str::to_string),
        challenge,
    )
}

async fn probe_site_system_type(client: &reqwest::Client, base_url: &str) -> Option<String> {
    probe_site_system_type_details(client, base_url).await.0
}

fn chrome_system_probe_script(marker: &str) -> String {
    let marker = serde_json::to_string(marker).unwrap_or_else(|_| "\"\"".into());
    r#"(() => {
  const token = __OPENHUB_MARKER__;
  const pending = "__OPENHUB_PENDING__";
  if (!/^https?:$/.test(window.location.protocol)) return pending;
  const previous = window.__openHubSystemProbe;
  if (previous && previous.token === token) {
    return previous.result ? JSON.stringify(previous.result) : pending;
  }
  const bridge = { token, result: null };
  window.__openHubSystemProbe = bridge;
  const probe = async (path) => {
    try {
      const response = await fetch(path, {
        method: "GET",
        credentials: "include",
        cache: "no-store",
        headers: { Accept: "application/json" },
        signal: AbortSignal.timeout(12000)
      });
      const text = await response.text();
      let isJson = false;
      try { JSON.parse(text); isJson = true; } catch (_) {}
      return { status: response.status, isJson };
    } catch (_) {
      return null;
    }
  };
  Promise.all([probe("/api/status"), probe("/setup/status")])
    .then(([newapi, sub2api]) => { bridge.result = { ok: true, newapi, sub2api }; })
    .catch((error) => { bridge.result = { ok: false, error: String(error) }; });
  return pending;
})()"#
        .replace("__OPENHUB_MARKER__", &marker)
}

fn parse_chrome_system_probe(value: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(value).ok()?;
    if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    let parse = |name: &str| {
        let value = value.get(name)?;
        Some(EndpointProbe {
            status: reqwest::StatusCode::from_u16(value.get("status")?.as_u64()?.try_into().ok()?)
                .ok()?,
            is_json: value.get("isJson")?.as_bool()?,
            is_challenge: false,
        })
    };
    system_type_from_probes(parse("newapi"), parse("sub2api")).map(str::to_string)
}

async fn probe_site_system_type_via_chrome(
    base_url: &str,
    profile_ids: &[String],
) -> Option<String> {
    let marker = format!(
        "openhub-system-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let script = chrome_system_probe_script(&marker);
    let existing_attempt = tauri::async_runtime::spawn_blocking({
        let base_url = base_url.to_string();
        let script = script.clone();
        move || {
            chrome_session::run_javascript_in_existing_chrome_tab(
                &base_url,
                &script,
                Duration::from_secs(15),
            )
        }
    })
    .await
    .ok()?;
    if let Ok(Some(value)) = existing_attempt {
        if let Some(system_type) = parse_chrome_system_probe(&value) {
            return Some(system_type);
        }
    }

    let profile_id = profile_ids.first()?.clone();
    let mut target_url = Url::parse(base_url).ok()?.join("/api/status").ok()?;
    target_url.set_fragment(Some(&marker));
    let background_attempt = tauri::async_runtime::spawn_blocking({
        let target_url = target_url.to_string();
        let marker = marker.clone();
        move || {
            chrome_session::run_javascript_in_background_chrome_profile(
                &target_url,
                &profile_id,
                &marker,
                &script,
                Duration::from_secs(20),
            )
        }
    })
    .await
    .ok()?;
    background_attempt
        .ok()
        .and_then(|value| parse_chrome_system_probe(&value))
}

fn cached_profile_ids_for_sites(
    database: &Database,
    site_ids: &HashSet<String>,
) -> Result<HashMap<String, Vec<String>>, String> {
    if site_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let mut statement = connection
        .prepare(
            "SELECT site_id, profile_id FROM site_accounts
             WHERE TRIM(profile_id) <> ''
             ORDER BY site_id, is_valid DESC, updated_at DESC, profile_id",
        )
        .map_err(|error| error.to_string())?;
    let mut profiles = HashMap::<String, Vec<String>>::new();
    for row in statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| error.to_string())?
    {
        let (site_id, profile_id) = row.map_err(|error| error.to_string())?;
        if !site_ids.contains(&site_id) {
            continue;
        }
        let entry = profiles.entry(site_id).or_default();
        if !entry.contains(&profile_id) {
            entry.push(profile_id);
        }
    }
    Ok(profiles)
}

async fn probe_site_system_types(
    client: &reqwest::Client,
    targets: Vec<(String, String)>,
    profile_ids: HashMap<String, Vec<String>>,
) -> HashMap<String, String> {
    let jobs = targets
        .into_iter()
        .filter(|(_, base_url)| !base_url.trim().is_empty())
        .map(|(site_id, base_url)| {
            let client = client.clone();
            tauri::async_runtime::spawn(async move {
                let (system_type, challenge) =
                    probe_site_system_type_details(&client, &base_url).await;
                (site_id, base_url, system_type, challenge)
            })
        })
        .collect::<Vec<_>>();

    let mut detected = HashMap::new();
    let mut challenge_targets = Vec::new();
    for job in jobs {
        if let Ok((site_id, base_url, system_type, challenge)) = job.await {
            if let Some(system_type) = system_type {
                detected.insert(site_id, system_type);
            } else if challenge {
                challenge_targets.push((site_id, base_url));
            }
        }
    }
    for (site_id, base_url) in challenge_targets {
        if let Some(system_type) = probe_site_system_type_via_chrome(
            &base_url,
            profile_ids
                .get(&site_id)
                .map(Vec::as_slice)
                .unwrap_or_default(),
        )
        .await
        {
            detected.insert(site_id, system_type);
        }
    }
    detected
}

fn normalize_remote_url(value: &str, base_url: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return String::new();
    }

    let parsed = Url::parse(value).ok().or_else(|| {
        if value.starts_with('/') {
            Url::parse(base_url).ok()?.join(value).ok()
        } else {
            None
        }
    });

    parsed
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .map(|url| url.to_string())
        .unwrap_or_default()
}

fn normalize_remote_site(mut site: SiteRecord) -> Result<SiteRecord, String> {
    let base_url = site.api_base_url.trim().to_string();
    site.checkin_url = normalize_remote_url(&site.checkin_url, &base_url);
    site.benefit_url = normalize_remote_url(&site.benefit_url, &base_url);
    site.status_url = normalize_remote_url(&site.status_url, &base_url);

    for maintainer in &mut site.maintainers {
        maintainer.profile_url = normalize_remote_url(&maintainer.profile_url, &base_url);
    }
    site.extension_links = site
        .extension_links
        .into_iter()
        .filter_map(|mut link| {
            link.url = normalize_remote_url(&link.url, &base_url);
            (!link.url.is_empty()).then_some(link)
        })
        .collect();

    normalize_site(site)
}

fn generated_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("local-{nanos:x}")
}

fn read_site(connection: &Connection, id: &str) -> Result<Option<SiteRecord>, String> {
    let mut stmt = connection
        .prepare(
            "SELECT id, name, description, registration_limit, icon, api_base_url, system_type,
                supports_immersive_translation, supports_ldc, supports_checkin, supports_nsfw,
                checkin_url, checkin_note, benefit_url, rate_limit, status_url,
                is_only_maintainer_visible, requires_invite_code, is_runaway, is_fake_charity,
                has_pending_report, is_personal, favorite, hidden, updated_at
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
                favorite: row.get::<_, i64>(22)? != 0,
                hidden: row.get::<_, i64>(23)? != 0,
                updated_at: row.get(24)?,
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
fn list_library(database: State<'_, Database>) -> Result<LibraryData, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let mut statement = connection
        .prepare(
            "SELECT id, name, description, registration_limit, icon, api_base_url, system_type,
                supports_immersive_translation, supports_ldc, supports_checkin, supports_nsfw,
                checkin_url, checkin_note, benefit_url, rate_limit, status_url,
                is_only_maintainer_visible, requires_invite_code, is_runaway, is_fake_charity,
                has_pending_report, is_personal, favorite, hidden, updated_at
         FROM directory_sites
         ORDER BY is_personal DESC, datetime(updated_at) DESC, rowid DESC",
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
                favorite: row.get::<_, i64>(22)? != 0,
                hidden: row.get::<_, i64>(23)? != 0,
                updated_at: row.get(24)?,
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

fn remote_user_string(value: &serde_json::Value, paths: &[&str]) -> String {
    paths
        .iter()
        .find_map(|path| value.pointer(path).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
        .to_string()
}

fn remote_user_name(value: &serde_json::Value) -> String {
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

fn remote_user_username(value: &serde_json::Value) -> String {
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

fn remote_user_avatar(value: &serde_json::Value) -> String {
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

fn remote_sites_from_json(value: serde_json::Value) -> Result<Vec<SiteRecord>, String> {
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

async fn authenticated_remote_session(
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
async fn get_remote_user(
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

fn json_number(value: &serde_json::Value, pointer: &str) -> Option<f64> {
    let value = value.pointer(pointer)?;
    let number = value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse::<f64>().ok())?;
    number.is_finite().then_some(number)
}

fn json_string(value: &serde_json::Value, pointers: &[&str]) -> String {
    pointers
        .iter()
        .find_map(|pointer| value.pointer(pointer))
        .and_then(|value| match value {
            serde_json::Value::String(value) => Some(value.trim().to_string()),
            serde_json::Value::Number(value) => Some(value.to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

fn api_error_message(value: &serde_json::Value, fallback: &str) -> String {
    let message = json_string(
        value,
        &[
            "/message",
            "/msg",
            "/error/message",
            "/error/msg",
            "/error",
            "/detail",
            "/data/message",
            "/data/error/message",
        ],
    );
    if message.is_empty() {
        fallback.to_string()
    } else {
        message
    }
}

fn parse_local_json(value: &str) -> Option<serde_json::Value> {
    let parsed = serde_json::from_str::<serde_json::Value>(value).ok()?;
    if let serde_json::Value::String(nested) = &parsed {
        serde_json::from_str(nested).ok().or(Some(parsed))
    } else {
        Some(parsed)
    }
}

fn local_scalar(value: &str) -> String {
    parse_local_json(value)
        .and_then(|value| match value {
            serde_json::Value::String(value) => Some(value),
            serde_json::Value::Number(value) => Some(value.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| value.trim().to_string())
}

fn parse_newapi_local_account(
    values: &HashMap<String, String>,
) -> Result<SiteAccountSnapshot, String> {
    let user = values
        .get("user")
        .and_then(|value| parse_local_json(value))
        .filter(serde_json::Value::is_object)
        .ok_or_else(|| "Chrome Local Storage 中没有有效的 NewAPI user 数据".to_string())?;
    let status = values
        .get("status")
        .and_then(|value| parse_local_json(value));
    let quota = json_number(&user, "/quota")
        .or_else(|| json_number(&user, "/data/quota"))
        .unwrap_or(0.0);
    let used_quota = json_number(&user, "/used_quota")
        .or_else(|| json_number(&user, "/data/used_quota"))
        .unwrap_or(0.0);
    let quota_per_unit = values
        .get("quota_per_unit")
        .map(|value| local_scalar(value))
        .and_then(|value| value.parse::<f64>().ok())
        .or_else(|| json_number(&user, "/quota_per_unit"))
        .or_else(|| json_number(&user, "/data/quota_per_unit"))
        .or_else(|| {
            status
                .as_ref()
                .and_then(|value| json_number(value, "/quota_per_unit"))
        })
        .or_else(|| {
            status
                .as_ref()
                .and_then(|value| json_number(value, "/data/quota_per_unit"))
        })
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(500_000.0);
    let display_type = values
        .get("quota_display_type")
        .map(|value| local_scalar(value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            json_string(&user, &["/quota_display_type", "/data/quota_display_type"])
        });
    let display_type = if display_type.is_empty() {
        status
            .as_ref()
            .map(|value| json_string(value, &["/quota_display_type", "/data/quota_display_type"]))
            .unwrap_or_default()
    } else {
        display_type
    };
    Ok(SiteAccountSnapshot {
        username: json_string(&user, &["/username", "/data/username"]),
        remaining: Some(quota / quota_per_unit),
        used: Some(used_quota / quota_per_unit),
        total: Some((quota + used_quota) / quota_per_unit),
        unit: if display_type.is_empty() {
            "USD".into()
        } else {
            display_type.to_ascii_uppercase()
        },
    })
}

fn parse_sub2api_account(value: &serde_json::Value) -> Result<SiteAccountSnapshot, String> {
    let code_valid = value
        .get("code")
        .is_some_and(|code| code.as_i64() == Some(0) || code.as_str() == Some("0"));
    let status_valid = value
        .pointer("/data/status")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|status| status.eq_ignore_ascii_case("active"));
    if !code_valid && !status_valid {
        return Err(api_error_message(value, "Sub2API 返回的账号数据无效"));
    }
    let remaining = ["/data/remaining", "/data/quota/remaining", "/data/balance"]
        .iter()
        .find_map(|pointer| json_number(value, pointer));
    let remaining = remaining.ok_or_else(|| "Sub2API 响应缺少有效的余额字段".to_string())?;
    let unit = json_string(value, &["/data/unit", "/data/quota/unit"]);
    Ok(SiteAccountSnapshot {
        username: json_string(value, &["/data/username"]),
        remaining: Some(remaining),
        used: None,
        total: None,
        unit: if unit.is_empty() { "USD".into() } else { unit },
    })
}

fn parse_sub2api_local_account(
    values: &HashMap<String, String>,
) -> Result<SiteAccountSnapshot, String> {
    let user = values
        .get("auth_user")
        .and_then(|value| parse_local_json(value))
        .filter(serde_json::Value::is_object)
        .ok_or_else(|| "Chrome Local Storage 中没有有效的 Sub2API auth_user 数据".to_string())?;
    let remaining = [
        "/remaining",
        "/quota/remaining",
        "/balance",
        "/data/remaining",
        "/data/quota/remaining",
        "/data/balance",
    ]
    .iter()
    .find_map(|pointer| json_number(&user, pointer))
    .unwrap_or(0.0);
    let unit = json_string(
        &user,
        &["/unit", "/quota/unit", "/data/unit", "/data/quota/unit"],
    );
    Ok(SiteAccountSnapshot {
        username: json_string(&user, &["/username", "/data/username"]),
        remaining: Some(remaining),
        used: None,
        total: None,
        unit: if unit.is_empty() { "USD".into() } else { unit },
    })
}

fn zero_v_zero_token(values: &HashMap<String, String>) -> Option<String> {
    values
        .get("0v0_token")
        .map(|value| local_scalar(value))
        .filter(|value| !value.is_empty())
}

fn parse_zero_v_zero_self(value: &serde_json::Value) -> Result<SiteAccountSnapshot, String> {
    if value.get("success").and_then(serde_json::Value::as_bool) != Some(true)
        || !value
            .pointer("/data")
            .is_some_and(serde_json::Value::is_object)
    {
        return Err(api_error_message(value, "0v0 返回的账号数据无效"));
    }
    let username = json_string(value, &["/data/username", "/data/display_name"]);
    let has_id = value.pointer("/data/id").is_some_and(|id| {
        id.as_u64().is_some_and(|id| id > 0) || id.as_str().is_some_and(|id| !id.trim().is_empty())
    });
    if username.is_empty() && !has_id {
        return Err("0v0 账号响应缺少用户标识".into());
    }
    let quota = json_number(value, "/data/quota").unwrap_or(0.0);
    let used_quota = json_number(value, "/data/used_quota").unwrap_or(0.0);
    Ok(SiteAccountSnapshot {
        username,
        remaining: Some(quota / 500_000.0),
        used: Some(used_quota / 500_000.0),
        total: Some((quota + used_quota) / 500_000.0),
        unit: "USD".into(),
    })
}

fn apply_zero_v_zero_stats(
    account: &mut SiteAccountSnapshot,
    value: &serde_json::Value,
) -> Result<(), String> {
    if value.get("success").and_then(serde_json::Value::as_bool) != Some(true)
        || !value
            .pointer("/data")
            .is_some_and(serde_json::Value::is_object)
    {
        return Err(api_error_message(value, "0v0 返回的额度统计无效"));
    }
    let remaining = json_number(value, "/data/total_quota")
        .ok_or_else(|| "0v0 额度统计缺少 total_quota".to_string())?;
    let used = json_number(value, "/data/used_quota").unwrap_or(0.0);
    account.remaining = Some(remaining / 500_000.0);
    account.used = Some(used / 500_000.0);
    account.total = Some((remaining + used) / 500_000.0);
    account.unit = "USD".into();
    Ok(())
}

fn has_local_account_session(system_type: &str, values: &HashMap<String, String>) -> bool {
    let has_newapi = parse_newapi_local_account(values).is_ok();
    let has_sub2api = parse_sub2api_local_account(values).is_ok();
    let has_zero_v_zero = zero_v_zero_token(values).is_some();
    match system_type.trim().to_ascii_lowercase().as_str() {
        "newapi" => has_newapi,
        "sub2api" => has_sub2api,
        "0v0" => has_zero_v_zero,
        _ => has_newapi || has_sub2api || has_zero_v_zero,
    }
}

fn has_account_session_candidate(
    system_type: &str,
    values: &HashMap<String, String>,
    cookie_names: &[String],
) -> bool {
    if has_local_account_session(system_type, values) {
        return true;
    }
    let system_type = system_type.trim().to_ascii_lowercase();
    (system_type.is_empty() || system_type == "newapi")
        && has_newapi_refresh_cookie_name(cookie_names.iter().map(String::as_str))
}

fn infer_system_type_from_local_accounts<'a>(
    accounts: impl IntoIterator<Item = &'a HashMap<String, String>>,
) -> &'static str {
    let mut has_newapi = false;
    let mut has_sub2api = false;
    let mut has_zero_v_zero = false;
    for values in accounts {
        has_newapi |= parse_newapi_local_account(values).is_ok();
        has_sub2api |= parse_sub2api_local_account(values).is_ok();
        has_zero_v_zero |= zero_v_zero_token(values).is_some();
    }
    if has_zero_v_zero {
        "0v0"
    } else if has_newapi {
        "NewAPI"
    } else if has_sub2api {
        "Sub2API"
    } else {
        ""
    }
}

fn parse_newapi_account(value: &serde_json::Value) -> Result<SiteAccountSnapshot, String> {
    if value.get("success").and_then(serde_json::Value::as_bool) != Some(true)
        || !value
            .pointer("/data")
            .is_some_and(serde_json::Value::is_object)
    {
        return Err(api_error_message(value, "NewAPI 返回的账号数据无效"));
    }
    let quota = json_number(value, "/data/quota")
        .ok_or_else(|| "NewAPI 响应缺少有效的 quota".to_string())?;
    let used_quota = json_number(value, "/data/used_quota").unwrap_or(0.0);
    Ok(SiteAccountSnapshot {
        username: json_string(value, &["/data/username"]),
        remaining: Some(quota / 500_000.0),
        used: Some(used_quota / 500_000.0),
        total: Some((quota + used_quota) / 500_000.0),
        unit: "USD".into(),
    })
}

fn newapi_user_id(values: &HashMap<String, String>) -> Option<String> {
    let user = values
        .get("user")
        .and_then(|value| parse_local_json(value))
        .filter(serde_json::Value::is_object)?;
    let id = json_string(&user, &["/id", "/data/id"]);
    (!id.is_empty()).then_some(id)
}

fn has_newapi_refresh_cookie_name<'a>(names: impl IntoIterator<Item = &'a str>) -> bool {
    names
        .into_iter()
        .any(|name| name.trim() == "new_api_refresh")
}

fn is_any_router_site(base_url: &str) -> bool {
    Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| host == "anyrouter.top" || host.ends_with(".anyrouter.top"))
}

fn cookie_header_has_name(cookie_header: &str, expected_name: &str) -> bool {
    cookie_header.split(';').any(|pair| {
        pair.trim()
            .split_once('=')
            .is_some_and(|(name, _)| name.trim() == expected_name)
    })
}

fn apply_newapi_auth(
    request: reqwest::RequestBuilder,
    auth: &NewApiAuth,
) -> reqwest::RequestBuilder {
    match auth {
        NewApiAuth::Legacy {
            cookie_header,
            user_id,
        } => request
            .header(reqwest::header::COOKIE, cookie_header)
            .header("new-api-user", user_id),
    }
}

fn parse_newapi_checkin_status(value: &serde_json::Value) -> Result<(bool, bool), String> {
    if value.get("success").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(api_error_message(value, "签到状态数据无效"));
    }
    let enabled = value
        .pointer("/data/enabled")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| "签到状态缺少 enabled 字段".to_string())?;
    let checked_in_today = value
        .pointer("/data/stats/checked_in_today")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| "签到状态缺少 checked_in_today 字段".to_string())?;
    Ok((enabled, checked_in_today))
}

fn json_boolish(value: &serde_json::Value) -> Option<bool> {
    match value {
        serde_json::Value::Bool(value) => Some(*value),
        serde_json::Value::Number(value) => value.as_i64().map(|value| value != 0),
        serde_json::Value::String(value) => match value.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "checked" | "checked_in" | "success" => Some(true),
            "false" | "0" | "no" | "unchecked" | "not_checked" | "not_checked_in" | "pending" => {
                Some(false)
            }
            _ => None,
        },
        _ => None,
    }
}

fn sub2api_response_succeeded(value: &serde_json::Value) -> bool {
    value.get("success").and_then(json_boolish) == Some(true)
        || value
            .get("code")
            .is_some_and(|code| code.as_i64() == Some(0) || code.as_str() == Some("0"))
}

fn parse_sub2api_checkin_status(value: &serde_json::Value) -> Result<bool, String> {
    if value.get("success").and_then(json_boolish) == Some(false)
        || value.get("code").is_some_and(|code| {
            code.as_i64().is_some_and(|code| code != 0)
                || code.as_str().is_some_and(|code| code != "0")
        })
    {
        return Err(api_error_message(value, "Sub2API 签到状态数据无效"));
    }
    [
        "/data/checked_in_today",
        "/data/checked_in",
        "/data/is_checked_in",
        "/data/has_checked_in",
        "/data/today_checked",
        "/checked_in_today",
        "/checked_in",
        "/is_checked_in",
        "/has_checked_in",
        "/today_checked",
        "/data/status",
        "/status",
        "/data",
    ]
    .iter()
    .find_map(|pointer| value.pointer(pointer).and_then(json_boolish))
    .ok_or_else(|| "Sub2API 签到状态缺少今日签到字段".to_string())
}

async fn request_json(
    request: reqwest::RequestBuilder,
    label: &str,
) -> Result<serde_json::Value, String> {
    let response = request
        .send()
        .await
        .map_err(|error| format!("{label}请求失败：{error:#}"))?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let body = response
        .bytes()
        .await
        .map_err(|error| format!("{label}响应读取失败：{error:#}"))?;
    let body = body
        .strip_prefix(&[0xef, 0xbb, 0xbf])
        .unwrap_or(body.as_ref());
    let value = match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(value) => value,
        Err(error) => {
            let first = body
                .iter()
                .copied()
                .find(|byte| !byte.is_ascii_whitespace());
            if content_type.contains("text/html") || first == Some(b'<') {
                let reason = if status == reqwest::StatusCode::FORBIDDEN {
                    "Cloudflare 安全验证拦截了直接请求，请先用对应 Chrome 账号打开站点并通过验证"
                } else {
                    "站点返回了网页而不是 API 数据"
                };
                return Err(format!(
                    "{label} HTTP {} 返回 HTML：{reason}",
                    status.as_u16()
                ));
            }
            return Err(format!("{label}返回的 JSON 无法解析：{error}"));
        }
    };
    if !status.is_success() {
        return Err(format!(
            "{label} HTTP {}：{}",
            status.as_u16(),
            api_error_message(&value, "请求失败")
        ));
    }
    Ok(value)
}

fn chrome_request_headers(
    request: reqwest::RequestBuilder,
    base_url: &str,
    user_agent: &str,
) -> reqwest::RequestBuilder {
    let major = user_agent
        .split("Chrome/")
        .nth(1)
        .and_then(|value| value.split('.').next())
        .unwrap_or("120");
    request
        .header(reqwest::header::ACCEPT, "application/json, text/plain, */*")
        .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
        .header(reqwest::header::REFERER, base_url)
        .header(reqwest::header::USER_AGENT, user_agent)
        .header(
            "sec-ch-ua",
            format!(
                "\"Not_A Brand\";v=\"99\", \"Chromium\";v=\"{major}\", \"Google Chrome\";v=\"{major}\""
            ),
        )
        .header("sec-ch-ua-mobile", "?0")
        .header("sec-ch-ua-platform", "\"macOS\"")
        .header("sec-fetch-dest", "empty")
        .header("sec-fetch-mode", "cors")
        .header("sec-fetch-site", "same-origin")
}

async fn refresh_newapi_checkin(
    client: &reqwest::Client,
    base_url: &str,
    auth: &NewApiAuth,
    user_agent: &str,
    current_month: &str,
    previous: CheckinSnapshot,
) -> CheckinSnapshot {
    let endpoint = match Url::parse(base_url).and_then(|url| url.join("/api/user/checkin")) {
        Ok(url) => url,
        Err(_) => {
            return CheckinSnapshot {
                error: "无法生成签到接口地址".into(),
                ..previous
            }
        }
    };
    let mut query_url = endpoint.clone();
    query_url
        .query_pairs_mut()
        .append_pair("month", current_month);
    let headers = |request: reqwest::RequestBuilder| {
        apply_newapi_auth(chrome_request_headers(request, base_url, user_agent), auth)
    };
    let value = match request_json(headers(client.get(query_url)), "签到状态接口").await {
        Ok(value) => value,
        Err(error) => return CheckinSnapshot { error, ..previous },
    };
    let (enabled, checked_in_today) = match parse_newapi_checkin_status(&value) {
        Ok(status) => status,
        Err(error) => return CheckinSnapshot { error, ..previous },
    };
    if !enabled || checked_in_today {
        return CheckinSnapshot {
            enabled,
            checked_in_today,
            error: String::new(),
        };
    }
    let value = match request_json(headers(client.post(endpoint)), "签到接口").await {
        Ok(value) => value,
        Err(error) => {
            return CheckinSnapshot {
                enabled,
                checked_in_today: false,
                error,
            }
        }
    };
    if value.get("success").and_then(serde_json::Value::as_bool) == Some(true) {
        CheckinSnapshot {
            enabled: true,
            checked_in_today: true,
            error: String::new(),
        }
    } else {
        CheckinSnapshot {
            enabled: true,
            checked_in_today: false,
            error: api_error_message(&value, "签到失败"),
        }
    }
}

async fn refresh_sub2api_checkin(
    client: &reqwest::Client,
    base_url: &str,
    auth_token: &str,
    user_agent: &str,
    previous: CheckinSnapshot,
) -> CheckinSnapshot {
    let base_url = match Url::parse(base_url) {
        Ok(url) => url,
        Err(_) => {
            return CheckinSnapshot {
                error: "站点 API 地址无效".into(),
                ..previous
            }
        }
    };
    let status_url = match base_url.join("/api/v1/redeem/checkin/status") {
        Ok(url) => url,
        Err(_) => {
            return CheckinSnapshot {
                error: "无法生成 Sub2API 签到状态接口地址".into(),
                ..previous
            }
        }
    };
    let checkin_url = match base_url.join("/api/v1/redeem/checkin") {
        Ok(url) => url,
        Err(_) => {
            return CheckinSnapshot {
                error: "无法生成 Sub2API 签到接口地址".into(),
                ..previous
            }
        }
    };
    let headers = |request: reqwest::RequestBuilder| {
        chrome_request_headers(request, base_url.as_str(), user_agent).bearer_auth(auth_token)
    };
    let value = match request_json(headers(client.get(status_url)), "Sub2API 签到状态接口").await
    {
        Ok(value) => value,
        Err(error) => return CheckinSnapshot { error, ..previous },
    };
    let checked_in_today = match parse_sub2api_checkin_status(&value) {
        Ok(value) => value,
        Err(error) => return CheckinSnapshot { error, ..previous },
    };
    if checked_in_today {
        return CheckinSnapshot {
            enabled: true,
            checked_in_today: true,
            error: String::new(),
        };
    }
    let value = match request_json(headers(client.post(checkin_url)), "Sub2API 签到接口").await
    {
        Ok(value) => value,
        Err(error) => {
            return CheckinSnapshot {
                enabled: true,
                checked_in_today: false,
                error,
            }
        }
    };
    if sub2api_response_succeeded(&value) {
        CheckinSnapshot {
            enabled: true,
            checked_in_today: true,
            error: String::new(),
        }
    } else {
        CheckinSnapshot {
            enabled: true,
            checked_in_today: false,
            error: api_error_message(&value, "Sub2API 签到失败"),
        }
    }
}

async fn fetch_site_account(
    client: &reqwest::Client,
    base_url: &str,
    system_type: &str,
    local_values: &HashMap<String, String>,
    local_error: &str,
    cookie_header: Result<String, String>,
    user_agent: &str,
    current_month: &str,
    should_checkin: bool,
    previous_checkin: CheckinSnapshot,
) -> Result<SiteAccountRefresh, String> {
    let previous_checkin = if should_checkin {
        previous_checkin
    } else {
        CheckinSnapshot::default()
    };
    let inferred_type;
    let system_type = if matches!(
        system_type.trim().to_ascii_lowercase().as_str(),
        "newapi" | "sub2api" | "0v0"
    ) {
        system_type
    } else if zero_v_zero_token(local_values).is_some() {
        inferred_type = "0v0".to_string();
        &inferred_type
    } else if parse_newapi_local_account(local_values).is_ok() {
        inferred_type = "NewAPI".to_string();
        &inferred_type
    } else if parse_sub2api_local_account(local_values).is_ok() {
        inferred_type = "Sub2API".to_string();
        &inferred_type
    } else {
        inferred_type = probe_site_system_type(client, base_url)
            .await
            .unwrap_or_default();
        &inferred_type
    };
    if system_type.eq_ignore_ascii_case("0v0") {
        if !local_error.is_empty() {
            return Err(local_error.to_string());
        }
        let token = zero_v_zero_token(local_values)
            .ok_or_else(|| "Chrome Local Storage 中没有 0v0_token".to_string())?;
        let base_url =
            Url::parse(ZERO_V_ZERO_CONSOLE_URL).map_err(|_| "0v0 控制台地址无效".to_string())?;
        let self_url = base_url
            .join("/api/user/self")
            .map_err(|_| "无法生成 0v0 账号接口地址".to_string())?;
        let stats_url = base_url
            .join("/api/user/stats")
            .map_err(|_| "无法生成 0v0 额度接口地址".to_string())?;
        let self_job = tauri::async_runtime::spawn({
            let client = client.clone();
            let token = token.clone();
            let user_agent = user_agent.to_string();
            async move {
                request_json(
                    chrome_request_headers(
                        client.get(self_url),
                        ZERO_V_ZERO_CONSOLE_URL,
                        &user_agent,
                    )
                    .bearer_auth(token),
                    "0v0 账号接口",
                )
                .await
            }
        });
        let stats_job = tauri::async_runtime::spawn({
            let client = client.clone();
            let user_agent = user_agent.to_string();
            async move {
                request_json(
                    chrome_request_headers(
                        client.get(stats_url),
                        ZERO_V_ZERO_CONSOLE_URL,
                        &user_agent,
                    )
                    .bearer_auth(token),
                    "0v0 额度接口",
                )
                .await
            }
        });
        let self_value = self_job
            .await
            .map_err(|error| format!("0v0 账号同步任务失败：{error}"))??;
        let mut account = parse_zero_v_zero_self(&self_value)?;
        let sync_error = match stats_job.await {
            Ok(Ok(value)) => apply_zero_v_zero_stats(&mut account, &value).err(),
            Ok(Err(error)) => Some(error),
            Err(error) => Some(format!("0v0 额度同步任务失败：{error}")),
        }
        .unwrap_or_default();
        return Ok(SiteAccountRefresh {
            account,
            is_valid: true,
            sync_error,
            checkin: CheckinSnapshot::default(),
        });
    }
    if system_type.eq_ignore_ascii_case("NewAPI") {
        let local_account = parse_newapi_local_account(local_values).ok();
        let cookie_header = match cookie_header {
            Ok(value) => value,
            Err(error) => {
                return match local_account {
                    Some(account) => Ok(SiteAccountRefresh {
                        account,
                        is_valid: true,
                        sync_error: error,
                        checkin: previous_checkin,
                    }),
                    None => Err(error),
                }
            }
        };
        let has_refresh_cookie = cookie_header_has_name(&cookie_header, "new_api_refresh");
        if has_refresh_cookie {
            return Err("新版 NewAPI 刷新认证必须在对应 Chrome Profile 中执行".into());
        }
        let user_id = match newapi_user_id(local_values) {
            Some(value) => value,
            None => {
                return match local_account {
                    Some(account) => Ok(SiteAccountRefresh {
                        account,
                        is_valid: true,
                        sync_error: "NewAPI 本地 user 数据缺少用户 ID".into(),
                        checkin: previous_checkin,
                    }),
                    None => Err(if local_error.is_empty() {
                        "没有找到可用的 NewAPI 登录凭据".into()
                    } else {
                        local_error.to_string()
                    }),
                }
            }
        };
        let auth = NewApiAuth::Legacy {
            cookie_header: cookie_header.clone(),
            user_id,
        };
        if is_any_router_site(base_url) {
            return match local_account {
                Some(account) => Ok(SiteAccountRefresh {
                    account,
                    is_valid: true,
                    sync_error: "Any Router 传统 NewAPI 会话需要通过 Chrome 获取远程账号数据"
                        .into(),
                    checkin: previous_checkin,
                }),
                None => Err("Any Router 本地 user 数据无效".into()),
            };
        }
        let checkin = if should_checkin {
            refresh_newapi_checkin(
                client,
                base_url,
                &auth,
                user_agent,
                current_month,
                previous_checkin,
            )
            .await
        } else {
            CheckinSnapshot::default()
        };
        let endpoint = Url::parse(base_url)
            .map_err(|_| "站点 API 地址无效".to_string())?
            .join("/api/user/self")
            .map_err(|_| "无法生成账号接口地址".to_string())?;
        let request = apply_newapi_auth(
            chrome_request_headers(client.get(endpoint), base_url, user_agent),
            &auth,
        );
        let remote = match request_json(request, "账号接口")
            .await
            .and_then(|value| parse_newapi_account(&value))
        {
            Ok(account) => account,
            Err(error) => {
                return match local_account {
                    Some(account) => Ok(SiteAccountRefresh {
                        account,
                        is_valid: true,
                        sync_error: error,
                        checkin,
                    }),
                    None => Err(format!("账号接口失败：{error}")),
                }
            }
        };
        return Ok(SiteAccountRefresh {
            account: remote,
            is_valid: true,
            sync_error: String::new(),
            checkin,
        });
    }
    if !local_error.is_empty() {
        return Err(local_error.to_string());
    }
    let local_account = parse_sub2api_local_account(local_values)?;
    let auth_token = local_values
        .get("auth_token")
        .map(|value| local_scalar(value))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Sub2API 本地数据中没有 auth_token".to_string());
    let auth_token = match auth_token {
        Ok(value) => value,
        Err(error) => {
            return Ok(SiteAccountRefresh {
                account: local_account,
                is_valid: true,
                sync_error: error,
                checkin: previous_checkin,
            })
        }
    };
    let endpoint = match system_type.trim().to_ascii_lowercase().as_str() {
        "sub2api" => "/api/v1/auth/me",
        _ => {
            return Ok(SiteAccountRefresh {
                account: local_account,
                is_valid: true,
                sync_error: "站点类型未识别，未请求账号接口".into(),
                checkin: previous_checkin,
            })
        }
    };
    let url = Url::parse(base_url)
        .map_err(|_| "站点 API 地址无效".to_string())?
        .join(endpoint)
        .map_err(|_| "无法生成账号接口地址".to_string())?;
    let checkin = if should_checkin {
        refresh_sub2api_checkin(client, base_url, &auth_token, user_agent, previous_checkin).await
    } else {
        CheckinSnapshot::default()
    };
    let request =
        chrome_request_headers(client.get(url), base_url, user_agent).bearer_auth(&auth_token);
    let account = request_json(request, "账号接口")
        .await
        .and_then(|value| parse_sub2api_account(&value));
    match account {
        Ok(account) => Ok(SiteAccountRefresh {
            account,
            is_valid: true,
            sync_error: String::new(),
            checkin,
        }),
        Err(error) => Ok(SiteAccountRefresh {
            account: local_account,
            is_valid: true,
            sync_error: error,
            checkin,
        }),
    }
}

#[tauri::command]
async fn mark_sites_with_chrome_sessions(
    app: tauri::AppHandle,
    database: State<'_, Database>,
    site_id: Option<String>,
    site_ids: Option<Vec<String>>,
    run_id: Option<u64>,
) -> Result<ChromeUsageScanResult, String> {
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
                 WHERE is_personal = 1
                   AND (TRIM(checkin_url) <> '' OR TRIM(api_base_url) <> '')",
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
                    let account_paths: &[&str] =
                        match system_type.trim().to_ascii_lowercase().as_str() {
                            "newapi" => &["/api/user/auth/refresh", "/api/user/self"],
                            "sub2api" => &["/api/v1/auth/me"],
                            "0v0" => &["/api/user/self", "/api/user/stats"],
                            _ => &[],
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
                    urls.push(checkin_url);
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
    emit_optional_sync_progress(
        &app,
        run_id,
        "site-type-probe",
        "running",
        format!(
            "正在通过 /api/status 与 /setup/status 检测 {} 个站点类型",
            targets.len()
        ),
    );
    let probe_client = build_http_client(&database, Duration::from_secs(8), 3, "站点类型探测")?;
    let probe_site_ids = targets
        .iter()
        .map(|(site_id, _, _, _, _)| site_id.clone())
        .collect::<HashSet<_>>();
    let probe_profile_ids = cached_profile_ids_for_sites(&database, &probe_site_ids)?;
    let probed_types = probe_site_system_types(
        &probe_client,
        targets
            .iter()
            .filter(|(_, _, _, _, system_type)| !system_type.eq_ignore_ascii_case("0v0"))
            .map(|(id, _, urls, api_base_url, _)| {
                let base_url = if api_base_url.trim().is_empty() {
                    urls.first().cloned().unwrap_or_default()
                } else {
                    api_base_url.clone()
                };
                (id.clone(), base_url)
            })
            .collect(),
        probe_profile_ids,
    )
    .await;
    for (site_id, _, urls, api_base_url, system_type) in &mut targets {
        if !system_type.eq_ignore_ascii_case("0v0") {
            if let Some(probed_type) = probed_types.get(site_id) {
                *system_type = probed_type.clone();
            }
        }
        let account_paths: &[&str] = match system_type.trim().to_ascii_lowercase().as_str() {
            "newapi" => &["/api/user/self", "/api/user/auth/refresh"],
            "sub2api" => &["/api/v1/auth/me"],
            "0v0" => &["/api/user/self", "/api/user/stats"],
            _ => &[],
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
    persist_site_system_types(&database, &probed_types)?;
    emit_optional_sync_progress(
        &app,
        run_id,
        "site-type-probe",
        "success",
        format!(
            "status 类型检测完成：确认 {} 个，{} 个待本地账号数据补充",
            probed_types
                .values()
                .filter(|system_type| !system_type.is_empty())
                .count(),
            targets
                .iter()
                .filter(|(_, _, _, _, system_type)| system_type.is_empty())
                .count()
        ),
    );
    emit_optional_sync_progress(
        &app,
        run_id,
        "chrome-scan",
        "running",
        format!("开始扫描 {} 个在用站点的 Chrome 账号", targets.len()),
    );
    let (current_month, previous_checkins) = {
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
        (current_month, previous_checkins)
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
    for (site_id, _, _, _, system_type) in &mut targets {
        if system_type.eq_ignore_ascii_case("0v0") {
            locally_inferred_types.insert(site_id.clone(), "0v0".into());
            continue;
        }
        if matches!(
            system_type.trim().to_ascii_lowercase().as_str(),
            "newapi" | "sub2api"
        ) {
            continue;
        }
        let inferred = infer_system_type_from_local_accounts(local_storage.iter().filter_map(
            |((local_site_id, _), (values, error))| {
                (local_site_id == site_id && error.is_empty()).then_some(values)
            },
        ));
        if !inferred.is_empty() {
            *system_type = inferred.into();
            locally_inferred_types.insert(site_id.clone(), inferred.into());
        }
    }
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
            });
    }

    let candidate_sites = matched_sites.len();
    let candidate_accounts = matched_sites
        .iter()
        .map(|site| site.sessions.len())
        .sum::<usize>();
    emit_optional_sync_progress(
        &app,
        run_id,
        "chrome-scan",
        "success",
        format!("Chrome 扫描完成：识别 {candidate_sites} 个站点、{candidate_accounts} 个账号候选"),
    );
    if !matched_sites.is_empty() {
        let client = build_http_client(&database, Duration::from_secs(12), 3, "账号同步请求")?;
        let chrome_user_agent = chrome_session::chrome_user_agent();
        let mut jobs = Vec::new();
        for (site_index, site) in matched_sites.iter().enumerate() {
            let Some((base_url, system_type, site_name)) = account_targets.get(&site.site_id)
            else {
                continue;
            };
            for (session_index, session) in site.sessions.iter().enumerate() {
                let client = client.clone();
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
                let auth_label = if system_type.eq_ignore_ascii_case("NewAPI") {
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
                    .get(&(site_id, profile_id.clone()))
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
                let job = tauri::async_runtime::spawn(async move {
                    let needs_cookie = system_type.eq_ignore_ascii_case("NewAPI")
                        || (system_type.trim().is_empty()
                            && (parse_newapi_local_account(&local_values).is_ok()
                                || has_refresh_cookie));
                    let cookie_header = if needs_cookie {
                        tauri::async_runtime::spawn_blocking(move || {
                            chrome_session::read_chrome_cookie_header_from_home(
                                &cookie_home_dir,
                                &cookie_base_url,
                                &profile_id,
                            )
                        })
                        .await
                        .map_err(|error| format!("读取 Chrome Cookie 任务失败：{error}"))?
                    } else {
                        Ok(String::new())
                    };
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
            if !session.sync_error.is_empty() {
                details.push(format!("额度刷新失败：{}", session.sync_error));
            }
            if !session.checkin_error.is_empty() {
                details.push(format!("签到失败：{}", session.checkin_error));
            }
            let has_warning = !session.sync_error.is_empty() || !session.checkin_error.is_empty();
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

    let detected = matched_sites
        .iter()
        .filter(|site| site.sessions.iter().any(|session| session.is_valid))
        .count();
    let accounts = matched_sites
        .iter()
        .flat_map(|site| &site.sessions)
        .filter(|session| session.is_valid)
        .count();

    let warnings = matched_sites
        .iter()
        .flat_map(|site| &site.sessions)
        .filter(|session| !session.sync_error.is_empty() || !session.checkin_error.is_empty())
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
    let newly_marked = 0_usize;
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
        for session in &site.sessions {
            let cookie_names =
                serde_json::to_string(&session.cookie_names).map_err(|error| error.to_string())?;
            transaction
                .execute(
                    "INSERT INTO site_accounts (
                        site_id, profile_id, domain, cookie_count, cookie_names,
                        profile_name, account_name, username, api_key_count, api_model_count,
                        remaining, used, total, unit, is_valid, sync_error,
                        checkin_enabled, checked_in_today, checkin_error,
                        checkin_date, updated_at
                     ) VALUES (
                        ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                        ?13, ?14, ?15, ?16, ?17, ?18, ?19, date('now', 'localtime'), CURRENT_TIMESTAMP
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
                    ],
                )
                .map_err(|error| error.to_string())?;
        }
    }
    transaction.commit().map_err(|error| error.to_string())?;
    emit_optional_sync_progress(
        &app,
        run_id,
        "chrome-cache",
        "success",
        format!("Chrome 账号缓存已写入 SQLite：{accounts} 个账号，{warnings} 个警告"),
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

fn chrome_account_bridge_script(
    user_id: Option<&str>,
    current_month: &str,
    marker: &str,
    use_refresh_auth: bool,
    should_checkin: bool,
    is_any_router: bool,
    allow_challenge_navigation: bool,
) -> String {
    let user_id =
        serde_json::to_string(user_id.unwrap_or_default()).unwrap_or_else(|_| "\"\"".into());
    let current_month = serde_json::to_string(current_month).unwrap_or_else(|_| "\"\"".into());
    let marker = serde_json::to_string(marker).unwrap_or_else(|_| "\"\"".into());
    r#"(() => {
  const token = __OPENHUB_MARKER__;
  const legacyUserId = __OPENHUB_USER_ID__;
  const useRefreshAuth = __OPENHUB_USE_REFRESH_AUTH__;
  const shouldCheckin = __OPENHUB_SHOULD_CHECKIN__;
  const isAnyRouter = __OPENHUB_IS_ANY_ROUTER__;
  const allowChallengeNavigation = __OPENHUB_ALLOW_CHALLENGE_NAVIGATION__;
  const requestTimeout = isAnyRouter ? 12000 : 30000;
  const pending = "__OPENHUB_PENDING__";
  if (window.location.protocol !== "http:" && window.location.protocol !== "https:") {
    return pending;
  }
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
  const previous = window.__openHubAccountSync;
  if (previous && previous.token === token) {
    if (previous.result) return JSON.stringify(previous.result);
    if (previous.state !== "challenge" || Date.now() - previous.started < 3000) return pending;
  }
  const bridge = { token, started: Date.now(), state: "running", result: null };
  window.__openHubAccountSync = bridge;
  const readResponse = async (response) => {
    const contentType = (response.headers.get("content-type") || "").toLowerCase();
    const text = await response.text();
    const lower = text.slice(0, 100000).toLowerCase();
    const isHtml = contentType.includes("text/html") || /^\s*<!doctype html|^\s*<html/i.test(text);
    const isChallenge = isHtml && (
      [403, 429, 503].includes(response.status) ||
      lower.includes("cf-chl-") || lower.includes("challenge-platform") ||
      lower.includes("just a moment") || lower.includes("attention required") ||
      lower.includes("cloudflare ray id")
    );
    if (isChallenge) {
      return { challenge: true, status: response.status };
    }
    if (isHtml) {
      return { status: response.status, error: "账号接口返回 HTML，站点 API 地址或系统类型可能不正确" };
    }
    try {
      return { status: response.status, data: JSON.parse(text) };
    } catch (_) {
      return { status: response.status, error: "接口没有返回 JSON" };
    }
  };
  const messageOf = (value, fallback) =>
    value && (value.message || value.msg || value.error) || fallback;
  (async () => {
    const headers = { "Accept": "application/json" };
    if (useRefreshAuth) {
      const refreshResponse = await readResponse(await fetch("/api/user/auth/refresh", {
        method: "POST", credentials: "include", cache: "no-store", headers,
        signal: AbortSignal.timeout(requestTimeout)
      }));
      if (refreshResponse.challenge) {
        if (!allowChallengeNavigation) {
          bridge.result = { ok: false, error: "Cloudflare 验证仍需要浏览器交互" };
          return;
        }
        bridge.state = "challenge";
        bridge.started = Date.now();
        window.location.assign(`/#${token}`);
        return;
      }
      const accessToken = refreshResponse.data?.data?.access_token ||
        refreshResponse.data?.data?.accessToken || refreshResponse.data?.data?.token ||
        refreshResponse.data?.access_token || refreshResponse.data?.accessToken ||
        refreshResponse.data?.token || "";
      if (!accessToken) {
        bridge.result = {
          ok: false,
          error: messageOf(
            refreshResponse.data,
            refreshResponse.error || `刷新认证接口 HTTP ${refreshResponse.status}`
          )
        };
        return;
      }
      headers.Authorization = `Bearer ${accessToken}`;
    } else if (legacyUserId) {
      headers["New-Api-User"] = legacyUserId;
    } else {
      bridge.result = {
        ok: false,
        error: "传统 NewAPI 会话缺少用户 ID"
      };
      return;
    }
    let checkinEnabled = false;
    let checkedInToday = false;
    let checkinError = "";
    if (shouldCheckin) {
      try {
        if (isAnyRouter) {
          checkinEnabled = true;
          const postResponse = await readResponse(await fetch("/api/user/sign_in", {
            method: "POST", credentials: "include", cache: "no-store", headers,
            signal: AbortSignal.timeout(requestTimeout)
          }));
          const postMessage = messageOf(postResponse.data, postResponse.error || "");
          const alreadyChecked = /已(?:经)?签到|already (?:signed|checked)/i.test(String(postMessage));
          if (postResponse.challenge) {
            checkinError = "Cloudflare 拦截了 Any Router 签到请求";
          } else if (
            alreadyChecked ||
            (postResponse.status >= 200 && postResponse.status < 300 && (
              postResponse.data?.success === true || postResponse.data?.code === 0 ||
              postResponse.data?.code === "0"
            ))
          ) {
            checkedInToday = true;
          } else {
            checkinError = postMessage || `Any Router 签到接口 HTTP ${postResponse.status}`;
          }
        } else {
          const checkinUrl = `/api/user/checkin?month=${encodeURIComponent(__OPENHUB_MONTH__)}`;
          const checkinResponse = await readResponse(await fetch(checkinUrl, {
            method: "GET", credentials: "include", cache: "no-store", headers,
            signal: AbortSignal.timeout(requestTimeout)
          }));
          if (checkinResponse.challenge) {
            checkinError = "Cloudflare 拦截了签到状态请求";
          } else if (checkinResponse.error || checkinResponse.status < 200 || checkinResponse.status >= 300) {
            checkinError = messageOf(
              checkinResponse.data,
              checkinResponse.error || `签到状态接口 HTTP ${checkinResponse.status}`
            );
          } else if (checkinResponse.data && checkinResponse.data.success === true) {
            checkinEnabled = checkinResponse.data.data?.enabled === true;
            checkedInToday = checkinResponse.data.data?.stats?.checked_in_today === true;
            if (checkinEnabled && !checkedInToday) {
              const postResponse = await readResponse(await fetch("/api/user/checkin", {
                method: "POST", credentials: "include", cache: "no-store", headers,
                signal: AbortSignal.timeout(requestTimeout)
              }));
              if (postResponse.challenge) {
                checkinError = "Cloudflare 拦截了签到请求";
              } else if (
                postResponse.error || postResponse.status < 200 || postResponse.status >= 300 ||
                !postResponse.data || postResponse.data.success !== true
              ) {
                checkinError = messageOf(
                  postResponse.data,
                  postResponse.error || `签到接口 HTTP ${postResponse.status}`
                );
              } else {
                checkedInToday = true;
              }
            }
          } else {
            checkinError = messageOf(checkinResponse.data, "签到状态数据无效");
          }
        }
      } catch (error) {
        checkinError = String(error && error.message || error);
      }
    }
    const selfResponse = await readResponse(await fetch("/api/user/self", {
      method: "GET", credentials: "include", cache: "no-store", headers,
      signal: AbortSignal.timeout(requestTimeout)
    }));
    if (selfResponse.challenge) {
      if (!allowChallengeNavigation) {
        bridge.result = { ok: false, error: "Cloudflare 验证仍需要浏览器交互" };
        return;
      }
      bridge.state = "challenge";
      bridge.started = Date.now();
      if (window.location.pathname !== "/api/user/self") {
        window.location.assign(`/api/user/self#${token}`);
      }
      return;
    }
    if (selfResponse.error || selfResponse.status < 200 || selfResponse.status >= 300) {
      bridge.result = {
        ok: false,
        error: messageOf(selfResponse.data, selfResponse.error || `账号接口 HTTP ${selfResponse.status}`)
      };
      return;
    }
    bridge.result = {
      ok: true,
      account: selfResponse.data,
      checkinEnabled,
      checkedInToday,
      checkinError
    };
  })().catch((error) => {
    const message = String(error && error.message || error);
    if (message.includes("Failed to parse URL")) {
      bridge.started = 0;
      return;
    }
    bridge.result = { ok: false, error: message };
  });
  return pending;
})()"#
        .replace("__OPENHUB_USER_ID__", &user_id)
        .replace("__OPENHUB_MONTH__", &current_month)
        .replace(
            "__OPENHUB_USE_REFRESH_AUTH__",
            if use_refresh_auth { "true" } else { "false" },
        )
        .replace("__OPENHUB_SHOULD_CHECKIN__", if should_checkin { "true" } else { "false" })
        .replace("__OPENHUB_IS_ANY_ROUTER__", if is_any_router { "true" } else { "false" })
        .replace(
            "__OPENHUB_ALLOW_CHALLENGE_NAVIGATION__",
            if allow_challenge_navigation {
                "true"
            } else {
                "false"
            },
        )
        .replace("__OPENHUB_MARKER__", &marker)
}

fn parse_chrome_account_bridge_result(
    value: &str,
) -> Result<(SiteAccountSnapshot, ChromeBridgeAccountResult), String> {
    let result = serde_json::from_str::<ChromeBridgeAccountResult>(value)
        .map_err(|error| format!("Chrome 返回的账号数据格式无效：{error}"))?;
    if !result.ok {
        return Err(if result.error.is_empty() {
            "Chrome 账号请求失败".into()
        } else {
            format!("Chrome 账号请求失败：{}", result.error)
        });
    }
    let account = result
        .account
        .as_ref()
        .ok_or_else(|| "Chrome 返回结果缺少账号数据".to_string())
        .and_then(parse_newapi_account)?;
    Ok((account, result))
}

#[tauri::command]
async fn sync_site_account_via_chrome(
    app: tauri::AppHandle,
    database: State<'_, Database>,
    site_id: String,
    profile_id: String,
    run_id: u64,
) -> Result<chrome_session::ChromeSessionInfo, String> {
    let site_id = site_id.trim().to_string();
    let profile_id = profile_id.trim().to_string();
    if site_id.is_empty() || profile_id.is_empty() {
        return Err("站点或 Chrome Profile 标识为空".into());
    }
    let (
        site_name,
        api_base_url,
        system_type,
        checkin_url,
        supports_checkin,
        current_month,
        cookie_names,
    ) = {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        let site = connection
            .query_row(
                "SELECT name, api_base_url, system_type, checkin_url, supports_checkin
                 FROM directory_sites WHERE id = ?1 AND is_personal = 1",
                [&site_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)? != 0,
                    ))
                },
            )
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "找不到对应的在用站点".to_string())?;
        let cookie_names_json: Option<String> = connection
            .query_row(
                "SELECT cookie_names FROM site_accounts WHERE site_id = ?1 AND profile_id = ?2",
                params![site_id, profile_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        let Some(cookie_names_json) = cookie_names_json else {
            return Err("该 Chrome Profile 尚未建立本地账号缓存，请先同步会话".into());
        };
        let cookie_names =
            serde_json::from_str::<Vec<String>>(&cookie_names_json).unwrap_or_default();
        let current_month: String = connection
            .query_row("SELECT strftime('%Y-%m', 'now', 'localtime')", [], |row| {
                row.get(0)
            })
            .map_err(|error| error.to_string())?;
        (
            site.0,
            site.1,
            site.2,
            site.3,
            site.4,
            current_month,
            cookie_names,
        )
    };
    if !system_type.eq_ignore_ascii_case("NewAPI") {
        return Err("当前仅对 NewAPI 账号提供 Chrome 同步".into());
    }

    emit_chrome_account_progress(
        &app,
        run_id,
        "local-account",
        "running",
        "正在读取所选 Chrome Profile 的本地账号",
    );

    let base_url = Url::parse(&api_base_url).map_err(|_| "站点 API 地址无效")?;
    let is_any_router = is_any_router_site(base_url.as_str());
    let origin = base_url.origin().ascii_serialization();
    if origin == "null" {
        return Err("站点 API 地址缺少有效来源".into());
    }
    let home_dir = app
        .path()
        .home_dir()
        .map_err(|error| format!("无法定位用户目录：{error}"))?;
    let local_target = chrome_local_storage::LocalStorageTarget {
        site_id: site_id.clone(),
        profile_id: profile_id.clone(),
        origin,
    };
    let local_match = tauri::async_runtime::spawn_blocking({
        let home_dir = home_dir.clone();
        move || chrome_local_storage::read_local_storage_from_home(&home_dir, &[local_target])
    })
    .await
    .map_err(|error| format!("读取 Chrome Local Storage 任务失败：{error}"))?
    .into_iter()
    .next();
    let has_refresh_cookie =
        has_newapi_refresh_cookie_name(cookie_names.iter().map(String::as_str));
    let local_values = local_match
        .as_ref()
        .filter(|item| item.error.is_empty())
        .map(|item| &item.values);
    let local_account_valid =
        local_values.is_some_and(|values| parse_newapi_local_account(values).is_ok());
    let user_id = local_values.and_then(newapi_user_id);
    if !has_refresh_cookie && !local_account_valid {
        return Err(local_match
            .and_then(|item| (!item.error.is_empty()).then_some(item.error))
            .unwrap_or_else(|| "没有找到可用的 NewAPI 本地账号或刷新会话".into()));
    }
    emit_chrome_account_progress(
        &app,
        run_id,
        "local-account",
        "success",
        if has_refresh_cookie {
            "认证策略：NewAPI 刷新令牌（new_api_refresh → Bearer Token）"
        } else {
            "认证策略：传统 NewAPI 会话（session Cookie + New-Api-User）"
        },
    );

    let silent_timeout = if has_refresh_cookie {
        Duration::from_secs(35)
    } else if is_any_router {
        Duration::from_secs(12)
    } else {
        Duration::from_secs(20)
    };
    let background_timeout = if has_refresh_cookie {
        Duration::from_secs(35)
    } else if is_any_router {
        Duration::from_secs(20)
    } else {
        Duration::from_secs(25)
    };
    let visible_timeout = if has_refresh_cookie {
        Duration::from_secs(120)
    } else if is_any_router {
        Duration::from_secs(45)
    } else {
        Duration::from_secs(60)
    };

    let mut resolved_account = None;
    if user_id.is_some() {
        let silent_marker = format!(
            "openhub-silent-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "系统时间异常")?
                .as_nanos()
        );
        let silent_javascript = chrome_account_bridge_script(
            user_id.as_deref(),
            &current_month,
            &silent_marker,
            has_refresh_cookie,
            supports_checkin,
            is_any_router,
            false,
        );
        emit_chrome_account_progress(
            &app,
            run_id,
            "browser-bypass",
            "running",
            "正在尝试复用已打开的同账号 Chrome 页面，不切换窗口",
        );
        let silent_attempt = tauri::async_runtime::spawn_blocking({
            let base_url = base_url.to_string();
            move || {
                chrome_session::run_javascript_in_existing_chrome_tab(
                    &base_url,
                    &silent_javascript,
                    silent_timeout,
                )
            }
        })
        .await;
        match silent_attempt {
            Ok(Ok(Some(value))) => match parse_chrome_account_bridge_result(&value) {
                Ok(parsed) => {
                    emit_chrome_account_progress(
                        &app,
                        run_id,
                        "browser-bypass",
                        "success",
                        "已通过现有 Chrome 页面静默获取账号数据",
                    );
                    resolved_account = Some(parsed);
                }
                Err(error) => emit_chrome_account_progress(
                    &app,
                    run_id,
                    "browser-bypass",
                    "success",
                    format!("现有页面静默请求未通过，继续尝试后台 Chrome：{error}"),
                ),
            },
            Ok(Ok(None)) => emit_chrome_account_progress(
                &app,
                run_id,
                "browser-bypass",
                "success",
                "没有找到已打开的同账号站点页面，继续尝试后台 Chrome",
            ),
            Ok(Err(error)) => emit_chrome_account_progress(
                &app,
                run_id,
                "browser-bypass",
                "success",
                format!("现有页面静默请求不可用，继续尝试后台 Chrome：{error}"),
            ),
            Err(error) => emit_chrome_account_progress(
                &app,
                run_id,
                "browser-bypass",
                "success",
                format!("现有页面静默任务失败，继续尝试后台 Chrome：{error}"),
            ),
        }
    } else {
        emit_chrome_account_progress(
            &app,
            run_id,
            "browser-bypass",
            "info",
            "本地账号缺少可核验的用户 ID，跳过静默请求以避免串用 Chrome 账号",
        );
    }

    if resolved_account.is_none() {
        let marker = format!(
            "openhub-background-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "系统时间异常")?
                .as_nanos()
        );
        let mut browser_url = if !checkin_url.trim().is_empty() {
            Url::parse(&checkin_url).unwrap_or_else(|_| base_url.clone())
        } else {
            base_url
                .join("/console/personal")
                .map_err(|_| "无法生成 Chrome 验证地址")?
        };
        if browser_url.origin() != base_url.origin() {
            browser_url = base_url
                .join("/console/personal")
                .map_err(|_| "无法生成 Chrome 验证地址")?;
        }
        browser_url.set_fragment(Some(&marker));
        let javascript = chrome_account_bridge_script(
            user_id.as_deref(),
            &current_month,
            &marker,
            has_refresh_cookie,
            supports_checkin,
            is_any_router,
            true,
        );
        emit_chrome_account_progress(
            &app,
            run_id,
            "browser-background",
            "running",
            "正在后台打开对应 Chrome 账号并尝试自动通过验证",
        );
        let background_attempt = tauri::async_runtime::spawn_blocking({
            let browser_url = browser_url.to_string();
            let profile_id = profile_id.clone();
            let marker = marker.clone();
            move || {
                chrome_session::run_javascript_in_background_chrome_profile(
                    &browser_url,
                    &profile_id,
                    &marker,
                    &javascript,
                    background_timeout,
                )
            }
        })
        .await;
        match background_attempt {
            Ok(Ok(value)) => match parse_chrome_account_bridge_result(&value) {
                Ok(parsed) => {
                    emit_chrome_account_progress(
                        &app,
                        run_id,
                        "browser-background",
                        "success",
                        "后台 Chrome 已完成账号请求，临时标签已关闭",
                    );
                    resolved_account = Some(parsed);
                }
                Err(error) => emit_chrome_account_progress(
                    &app,
                    run_id,
                    "browser-background",
                    "success",
                    format!("后台请求仍需人工验证，将显示 Chrome：{error}"),
                ),
            },
            Ok(Err(error)) => emit_chrome_account_progress(
                &app,
                run_id,
                "browser-background",
                "success",
                format!("后台请求未完成，将显示 Chrome：{error}"),
            ),
            Err(error) => emit_chrome_account_progress(
                &app,
                run_id,
                "browser-background",
                "success",
                format!("后台 Chrome 任务失败，将显示浏览器：{error}"),
            ),
        }
    }

    let (account, result) = match resolved_account {
        Some(parsed) => parsed,
        None => {
            let marker = format!(
                "openhub-sync-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| "系统时间异常")?
                    .as_nanos()
            );
            let mut browser_url = if !checkin_url.trim().is_empty() {
                Url::parse(&checkin_url).unwrap_or_else(|_| base_url.clone())
            } else {
                base_url
                    .join("/console/personal")
                    .map_err(|_| "无法生成 Chrome 验证地址")?
            };
            if browser_url.origin() != base_url.origin() {
                browser_url = base_url
                    .join("/console/personal")
                    .map_err(|_| "无法生成 Chrome 验证地址")?;
            }
            browser_url.set_fragment(Some(&marker));
            let javascript = chrome_account_bridge_script(
                user_id.as_deref(),
                &current_month,
                &marker,
                has_refresh_cookie,
                supports_checkin,
                is_any_router,
                true,
            );
            emit_chrome_account_progress(
                &app,
                run_id,
                "chrome-request",
                "running",
                "静默请求未能完成，正在打开 Chrome；如出现验证，请在浏览器中完成",
            );
            let bridge_result = tauri::async_runtime::spawn_blocking({
                let browser_url = browser_url.to_string();
                let profile_id = profile_id.clone();
                let marker = marker.clone();
                move || {
                    chrome_session::run_javascript_in_chrome_profile(
                        &browser_url,
                        &profile_id,
                        &marker,
                        &javascript,
                        visible_timeout,
                    )
                }
            })
            .await
            .map_err(|error| format!("Chrome 同步任务失败：{error}"))??;
            let parsed = parse_chrome_account_bridge_result(&bridge_result)?;
            emit_chrome_account_progress(
                &app,
                run_id,
                "chrome-request",
                "success",
                "Chrome 已返回账号接口数据",
            );
            parsed
        }
    };

    emit_chrome_account_progress(
        &app,
        run_id,
        "account-cache",
        "running",
        "正在更新 SQLite 账号缓存",
    );

    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let changed = connection
        .execute(
            "UPDATE site_accounts
             SET username = ?1, remaining = ?2, used = ?3, total = ?4, unit = ?5,
                 is_valid = 1, sync_error = '', checkin_enabled = ?6,
                 checked_in_today = ?7, checkin_error = ?8,
                 checkin_date = date('now', 'localtime'), updated_at = CURRENT_TIMESTAMP
             WHERE site_id = ?9 AND profile_id = ?10",
            params![
                account.username,
                account.remaining,
                account.used,
                account.total,
                account.unit,
                result.checkin_enabled,
                result.checked_in_today,
                result.checkin_error,
                site_id,
                profile_id,
            ],
        )
        .map_err(|error| error.to_string())?;
    if changed == 0 {
        return Err(format!("没有更新到 {site_name} 的账号缓存"));
    }
    let session = read_cached_usage_sites(&connection)?
        .into_iter()
        .find(|site| site.site_id == site_id)
        .and_then(|site| {
            site.sessions
                .into_iter()
                .find(|session| session.profile_id == profile_id)
        })
        .ok_or_else(|| "读取 Chrome 同步后的账号缓存失败".to_string())?;
    emit_chrome_account_progress(
        &app,
        run_id,
        "account-cache",
        "success",
        "账号额度与签到状态已保存到 SQLite",
    );
    Ok(session)
}

#[tauri::command]
async fn sync_remote_sites(
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
                "SELECT favorite, hidden, is_personal, system_type FROM directory_sites WHERE id = ?1",
                [&site.id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)? != 0,
                        row.get::<_, i64>(1)? != 0,
                        row.get::<_, i64>(2)? != 0,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if let Some((favorite, hidden, is_personal, system_type)) = existing {
            site.favorite = favorite;
            site.hidden = hidden;
            site.is_personal = is_personal;
            if site.system_type.trim().is_empty() {
                site.system_type = system_type;
            }
            updated += 1;
        } else {
            site.favorite = false;
            site.hidden = false;
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

#[tauri::command]
async fn detect_site_system_types(
    app: tauri::AppHandle,
    database: State<'_, Database>,
    site_ids: Vec<String>,
    run_id: u64,
) -> Result<usize, String> {
    let site_ids = site_ids.into_iter().collect::<HashSet<_>>();
    if site_ids.is_empty() {
        return Ok(0);
    }
    let targets = {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        let mut statement = connection
            .prepare(
                "SELECT id, api_base_url FROM directory_sites
                 WHERE TRIM(api_base_url) <> '' AND TRIM(system_type) = ''",
            )
            .map_err(|error| error.to_string())?;
        let targets = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| error.to_string())?
            .filter_map(|row| row.ok())
            .filter(|(site_id, _)| site_ids.contains(site_id))
            .collect::<Vec<_>>();
        targets
    };
    emit_sync_progress(
        &app,
        run_id,
        "detect",
        "running",
        format!("已转入后台，并发检测 {} 个站点类型", targets.len()),
    );
    let client = build_http_client(&database, Duration::from_secs(8), 3, "站点类型探测")?;
    let target_site_ids = targets
        .iter()
        .map(|(site_id, _)| site_id.clone())
        .collect::<HashSet<_>>();
    let profile_ids = cached_profile_ids_for_sites(&database, &target_site_ids)?;
    let detected = probe_site_system_types(&client, targets, profile_ids).await;
    let detected_count = detected.len();
    let mut connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    for (site_id, system_type) in detected {
        transaction
            .execute(
                "UPDATE directory_sites SET system_type = ?2 WHERE id = ?1",
                params![site_id, system_type],
            )
            .map_err(|error| error.to_string())?;
    }
    transaction.commit().map_err(|error| error.to_string())?;
    emit_sync_progress(
        &app,
        run_id,
        "detect",
        "success",
        format!("后台类型检测完成，已处理 {detected_count} 个站点"),
    );
    Ok(detected_count)
}

#[tauri::command]
fn create_site(database: State<'_, Database>, mut input: SiteRecord) -> Result<SiteRecord, String> {
    input.id = generated_id();
    input.favorite = false;
    input.hidden = false;
    let input = normalize_site(input)?;
    let mut connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;

    let transaction = connection.transaction().map_err(|e| e.to_string())?;
    insert_site_transaction(&transaction, &input)?;
    transaction.commit().map_err(|e| e.to_string())?;

    read_site(&connection, &input.id)?.ok_or_else(|| "创建站点失败".into())
}

#[tauri::command]
async fn import_site(
    database: State<'_, Database>,
    site_url: String,
) -> Result<SiteRecord, String> {
    let base_url = normalize_import_base_url(&site_url)?;
    let canonical_url = base_url.to_string();
    {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
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
        client,
        base_url
            .join("/setup/status")
            .map_err(|error| error.to_string())?,
        "application/json",
    ));
    let root_response = root_job.await.ok().flatten();
    let newapi_response = newapi_job.await.ok().flatten();
    let sub2api_response = sub2api_job.await.ok().flatten();
    if root_response.is_none() && newapi_response.is_none() && sub2api_response.is_none() {
        return Err("无法连接该站点，请检查 URL 或网络代理后重试".into());
    }

    let newapi_probe = newapi_response
        .as_ref()
        .map(DiscoveryResponse::endpoint_probe);
    let sub2api_probe = sub2api_response
        .as_ref()
        .map(DiscoveryResponse::endpoint_probe);
    let system_type = system_type_from_probes(newapi_probe, sub2api_probe)
        .unwrap_or_default()
        .to_string();
    let newapi_json = newapi_response.as_ref().and_then(DiscoveryResponse::json);
    let sub2api_json = sub2api_response.as_ref().and_then(DiscoveryResponse::json);
    let status_sources = match system_type.as_str() {
        "NewAPI" => newapi_json.iter().collect::<Vec<_>>(),
        "Sub2API" => sub2api_json.iter().collect::<Vec<_>>(),
        _ => Vec::new(),
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
    let checkin_url = if supports_checkin && system_type == "NewAPI" {
        base_url
            .join("/console/personal")
            .map(|url| url.to_string())
            .unwrap_or_default()
    } else if supports_checkin && system_type == "Sub2API" {
        base_url
            .join("/dashboard")
            .map(|url| url.to_string())
            .unwrap_or_default()
    } else {
        String::new()
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
        ..SiteRecord::default()
    };
    site = normalize_site(site)?;

    let mut connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
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
fn update_site(
    database: State<'_, Database>,
    id: String,
    mut input: SiteRecord,
) -> Result<SiteRecord, String> {
    input.id = id.clone();
    let mut input = normalize_site(input)?;
    let mut connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;

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
            has_pending_report=?19, is_personal=?20, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id=?21",
        params![
            input.name, input.description, input.registration_limit, input.icon, input.api_base_url,
            input.supports_immersive_translation, input.supports_ldc, input.supports_checkin, input.supports_nsfw,
            input.checkin_url, input.checkin_note, input.benefit_url, input.rate_limit, input.status_url,
            input.is_only_maintainer_visible, input.requires_invite_code, input.is_runaway, input.is_fake_charity,
            input.has_pending_report, input.is_personal, id
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
fn toggle_personal(database: State<'_, Database>, id: String) -> Result<SiteRecord, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let changed = connection
        .execute(
            &format!(
                "UPDATE directory_sites
                 SET is_personal = CASE is_personal WHEN 0 THEN 1 ELSE 0 END,
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

#[tauri::command]
fn get_system_fonts() -> Vec<String> {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SiteModelItem {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    owned_by: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SiteModelsResult {
    models: Vec<SiteModelItem>,
    source: String,
    keys: Vec<String>,
}

fn json_array_at<'a>(
    value: &'a serde_json::Value,
    pointers: &[&str],
) -> Option<&'a Vec<serde_json::Value>> {
    pointers
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(serde_json::Value::as_array))
}

fn parse_site_models(value: &serde_json::Value) -> Vec<SiteModelItem> {
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

fn api_key_is_enabled(item: &serde_json::Value) -> bool {
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

fn normalize_api_key_value(value: &str) -> Option<String> {
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

fn parse_api_keys(value: &serde_json::Value) -> Vec<String> {
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
    let mut keys = Vec::new();
    for item in items.iter().filter(|item| api_key_is_enabled(item)) {
        let (value, prefix) = match item {
            serde_json::Value::String(value) => (value.trim().to_string(), String::new()),
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
            ),
            _ => continue,
        };
        let Some(value) = normalize_api_key_value(&value) else {
            continue;
        };
        keys.push(value.clone());
        if !prefix.is_empty() && !value.starts_with(&prefix) {
            keys.push(format!("{prefix}{value}"));
        }
    }
    keys.sort();
    keys.dedup();
    keys
}

fn parse_newapi_token_ids(value: &serde_json::Value) -> Vec<String> {
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

fn parse_revealed_api_key(value: &serde_json::Value) -> Option<String> {
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

async fn reveal_newapi_keys(
    client: &reqwest::Client,
    base_url: &Url,
    auth: &NewApiAuth,
    user_agent: &str,
    token_list: &serde_json::Value,
) -> Result<Vec<String>, String> {
    let mut keys = parse_api_keys(token_list);
    if !keys.is_empty() {
        return Ok(keys);
    }
    let token_ids = parse_newapi_token_ids(token_list);
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
        Ok(keys)
    }
}

async fn fetch_models_with_keys(
    client: &reqwest::Client,
    base_url: &Url,
    keys: Vec<String>,
    visible_keys: Vec<String>,
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
            if let Some(user_id) = newapi_user_id {
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

fn chrome_models_bridge_script(
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
  const extractKeys = (value) => activeKeyItems(value)
    .flatMap((item) => {
      const key = String(typeof item === "string" ? item : item.key || item.api_key || item.apiKey || item.plain_key || item.plainKey || item.secret_key || item.secretKey || item.token || item.secret || item.value || "").replace(/^Bearer\s+/i, "").trim();
      const prefix = typeof item === "object" && item ? String(item.key_prefix || item.keyPrefix || item.prefix || "") : "";
      return prefix && !key.startsWith(prefix) ? [key, `${prefix}${key}`] : [key];
    })
    .filter((key) => key.length >= 8 && !/\s|\*|…|\.\.\./.test(key));
  const extractTokenIds = (value) => activeKeyItems(value)
    .map((item) => typeof item === "object" && item ? item.id ?? item.token_id ?? item.tokenId ?? "" : "")
    .map((id) => String(id))
    .filter((id) => id.length > 0 && id.length <= 64 && /^[A-Za-z0-9_-]+$/.test(id));
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
    if (systemType !== "sub2api" && (!keyResponse.ok || extractKeys(keyResponse.data).length === 0)) {
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
    if (systemType !== "sub2api" && !keys.length) {
      for (const tokenId of extractTokenIds(keyResponse.data)) {
        const revealResponse = await readJson(`/api/token/${encodeURIComponent(tokenId)}/key`, { method: "POST", headers });
        const revealedKey = extractRevealedKey(revealResponse.data);
        if (revealResponse.ok && revealedKey) keys.push(revealedKey);
      }
    }
    visibleKeys = [...new Set(keys)];
    if (dashboardAccessToken) keys.push(dashboardAccessToken);
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
          bridge.result = { ok: true, models, source, keys: visibleKeys };
          return;
        }
      }
    }
    throw new Error(`/v1/models 未返回模型（HTTP ${lastStatus}${lastError ? `：${lastError}` : ""}）`);
  })().catch((error) => {
    bridge.result = { ok: false, error: error && error.message || String(error), keys: visibleKeys };
  });
  return pending;
})()"#
        .replace("__OPENHUB_SYSTEM_TYPE__", &system_type)
        .replace("__OPENHUB_USER_ID__", &user_id)
        .replace("__OPENHUB_MARKER__", &marker)
}

fn parse_chrome_models_result(value: &str) -> Result<SiteModelsResult, String> {
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
    })
}

fn parse_chrome_models_keys(value: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(value)
        .map(|value| parse_api_keys(&value))
        .unwrap_or_default()
}

fn merge_api_keys(target: &mut Vec<String>, keys: impl IntoIterator<Item = String>) {
    target.extend(keys);
    target.sort();
    target.dedup();
}

fn cache_profile_api_counts(
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

#[tauri::command]
async fn fetch_site_models_json(
    app: tauri::AppHandle,
    database: State<'_, Database>,
    url: String,
    site_id: Option<String>,
    profile_id: Option<String>,
) -> Result<SiteModelsResult, String> {
    let client = build_http_client(&database, Duration::from_secs(6), 3, "站点模型请求")?;
    let mut base = url.trim().to_string();
    if !base.starts_with("http://") && !base.starts_with("https://") {
        base = format!("https://{base}");
    }
    if !base.ends_with('/') {
        base.push('/');
    }
    let base_url = Url::parse(&base).map_err(|_| "站点 API 地址无效".to_string())?;
    let user_agent = chrome_session::chrome_user_agent();
    let (system_type, mut profile_ids) = if let Some(site_id) = site_id.as_deref() {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        let system_type = connection
            .query_row(
                "SELECT system_type FROM directory_sites WHERE id = ?1",
                [site_id],
                |row| row.get::<_, String>(0),
            )
            .unwrap_or_default();
        let mut statement = connection
            .prepare("SELECT profile_id FROM site_accounts WHERE site_id = ?1 AND is_valid = 1 GROUP BY profile_id ORDER BY max(updated_at) DESC")
            .map_err(|error| error.to_string())?;
        let profile_ids = statement
            .query_map([site_id], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        (system_type, profile_ids)
    } else {
        (String::new(), Vec::new())
    };
    let requested_profile_id = profile_id.clone();
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

    for profile_id in &profile_ids {
        let values = local_values.get(profile_id).cloned().unwrap_or_default();
        let inferred_type = if system_type.trim().is_empty() {
            if parse_newapi_local_account(&values).is_ok() {
                "NewAPI"
            } else if parse_sub2api_local_account(&values).is_ok() {
                "Sub2API"
            } else {
                ""
            }
        } else {
            system_type.as_str()
        };
        if inferred_type.eq_ignore_ascii_case("NewAPI") {
            let Some(user_id) = newapi_user_id(&values) else {
                errors.push(format!("{profile_id}：NewAPI 本地 user 缺少用户 ID"));
                continue;
            };
            let token_url = base_url
                .join("/api/token/?p=1&size=20")
                .map_err(|_| "无法生成 /api/token 地址")?;
            let cookie_home = home_dir.clone();
            let cookie_target = token_url.to_string();
            let cookie_profile = profile_id.clone();
            let cookie_header = tauri::async_runtime::spawn_blocking(move || {
                chrome_session::read_chrome_cookie_header_from_home(
                    &cookie_home,
                    &cookie_target,
                    &cookie_profile,
                )
            })
            .await
            .map_err(|error| format!("读取 Chrome Cookie 任务失败：{error}"))?;
            match cookie_header {
                Ok(cookie_header) => {
                    let model_user_id = user_id.clone();
                    let auth = NewApiAuth::Legacy {
                        cookie_header,
                        user_id,
                    };
                    let request = apply_newapi_auth(
                        chrome_request_headers(
                            client.get(token_url),
                            base_url.as_str(),
                            &user_agent,
                        ),
                        &auth,
                    );
                    match request_json(request, "NewAPI Key 接口").await {
                        Ok(value) => {
                            match reveal_newapi_keys(&client, &base_url, &auth, &user_agent, &value)
                                .await
                            {
                                Ok(keys) => {
                                    merge_api_keys(&mut discovered_keys, keys.iter().cloned());
                                    match fetch_models_with_keys(
                                        &client,
                                        &base_url,
                                        keys.clone(),
                                        keys,
                                        &user_agent,
                                        "newapi-key",
                                        Some(&model_user_id),
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
                        Err(error) => errors.push(format!("{profile_id}：{error}")),
                    }
                }
                Err(error) => errors.push(format!("{profile_id}：{error}")),
            }
        } else if inferred_type.eq_ignore_ascii_case("Sub2API") {
            let auth_token = values
                .get("auth_token")
                .map(|value| local_scalar(value))
                .filter(|value| !value.is_empty());
            let Some(auth_token) = auth_token else {
                errors.push(format!("{profile_id}：Sub2API 本地数据中没有 auth_token"));
                continue;
            };
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
                    merge_api_keys(&mut discovered_keys, visible_keys.iter().cloned());
                    let mut keys = visible_keys.clone();
                    keys.push(dashboard_token);
                    match fetch_models_with_keys(
                        &client,
                        &base_url,
                        keys,
                        visible_keys,
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
        let values = local_values.get(profile_id).cloned().unwrap_or_default();
        let inferred_type = if system_type.trim().is_empty() {
            if parse_sub2api_local_account(&values).is_ok() {
                "Sub2API"
            } else {
                "NewAPI"
            }
        } else {
            system_type.as_str()
        };
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
                    Duration::from_secs(35),
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
                Err(error) => errors.push(format!("{profile_id} 静默请求：{error}")),
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
                Duration::from_secs(35),
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
            Err(error) => errors.push(format!("{profile_id} 后台请求：{error}")),
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
                Duration::from_secs(45),
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
            Err(error) => errors.push(format!("{profile_id}：{error}")),
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
                    },
                );
            }
            errors.push("/v1/models 返回空模型列表".into());
        }
        Err(error) => errors.push(error),
    }
    if !discovered_keys.is_empty() {
        let source = if system_type.eq_ignore_ascii_case("Sub2API") {
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

#[cfg(test)]
mod tests {
    use super::{
        account_base_url, api_error_message, apply_zero_v_zero_stats, cache_profile_api_counts,
        chrome_account_bridge_script, chrome_models_bridge_script, chrome_system_probe_script,
        cookie_header_has_name, discovered_json_bool, discovered_json_string,
        has_account_session_candidate, has_newapi_refresh_cookie_name, html_icon_href,
        html_meta_description, html_title, infer_remote_system_type,
        infer_system_type_from_local_accounts, is_any_router_site,
        migrate_legacy_favorites_to_personal, normalize_import_base_url, normalize_network_proxy,
        normalize_remote_url, parse_api_keys, parse_chrome_models_keys, parse_chrome_models_result,
        parse_newapi_checkin_status, parse_newapi_local_account, parse_newapi_token_ids,
        parse_revealed_api_key, parse_site_models, parse_sub2api_account,
        parse_sub2api_checkin_status, parse_sub2api_local_account, parse_zero_v_zero_self,
        read_cached_usage_sites, shield_page_response, site_matches_requested_scope,
        sub2api_response_succeeded, system_type_from_probes, zero_v_zero_token, Database,
        EndpointProbe, SiteModelItem, SiteModelsResult,
    };
    use rusqlite::{params, Connection};
    use std::collections::{HashMap, HashSet};

    #[test]
    fn migrates_legacy_favorites_to_personal_sites() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE directory_sites (
                    id TEXT PRIMARY KEY,
                    is_personal INTEGER NOT NULL DEFAULT 0,
                    favorite INTEGER NOT NULL DEFAULT 0
                 );
                 INSERT INTO directory_sites (id, is_personal, favorite) VALUES
                    ('favorite', 0, 1),
                    ('personal', 1, 0),
                    ('unused', 0, 0);",
            )
            .unwrap();

        migrate_legacy_favorites_to_personal(&connection).unwrap();

        let states = connection
            .prepare("SELECT id, is_personal, favorite FROM directory_sites ORDER BY id")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            states,
            vec![
                ("favorite".into(), 1, 0),
                ("personal".into(), 1, 0),
                ("unused".into(), 0, 0),
            ]
        );
    }

    #[test]
    fn normalizes_import_urls_to_the_site_origin() {
        assert_eq!(
            normalize_import_base_url(" https://example.com/console/?tab=1#account ")
                .unwrap()
                .as_str(),
            "https://example.com/"
        );
        assert!(normalize_import_base_url("ftp://example.com").is_err());
        assert!(normalize_import_base_url("example.com").is_err());
    }

    #[test]
    fn extracts_import_metadata_from_status_json() {
        let status = serde_json::json!({
            "success": true,
            "data": {
                "name": "Example AI",
                "description": "Public API service",
                "logo": "/logo.png",
                "checkin_enabled": true
            }
        });
        assert_eq!(discovered_json_string(&status, &["name"]), "Example AI");
        assert_eq!(discovered_json_string(&status, &["logo"]), "/logo.png");
        assert!(discovered_json_bool(&status, &["checkin_enabled"]));
    }

    #[test]
    fn extracts_import_metadata_from_html() {
        let html = r#"<!doctype html><html><head>
            <title>Example &amp; AI</title>
            <meta property='og:description' content='Fast &amp; reliable'>
            <link rel="shortcut icon" href="/assets/icon.png">
        </head></html>"#;
        assert_eq!(html_title(html), "Example & AI");
        assert_eq!(html_meta_description(html), "Fast & reliable");
        assert_eq!(html_icon_href(html), "/assets/icon.png");
    }

    #[test]
    fn keeps_chrome_session_sync_inside_the_requested_site_scope() {
        let selected = HashSet::from(["site-a".to_string(), "site-b".to_string()]);

        assert!(site_matches_requested_scope(
            "site-c",
            None,
            false,
            &HashSet::new(),
            false,
        ));
        assert!(site_matches_requested_scope(
            "site-a",
            Some("site-a"),
            true,
            &HashSet::new(),
            false,
        ));
        assert!(!site_matches_requested_scope(
            "site-b",
            Some("site-a"),
            true,
            &HashSet::new(),
            false,
        ));
        assert!(site_matches_requested_scope(
            "site-b", None, false, &selected, true,
        ));
        assert!(!site_matches_requested_scope(
            "site-c", None, false, &selected, true,
        ));
        assert!(!site_matches_requested_scope(
            "site-a",
            None,
            false,
            &HashSet::new(),
            true,
        ));
    }

    #[test]
    fn rebuilds_cached_site_accounts_from_sqlite() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE site_accounts (
                    site_id TEXT NOT NULL,
                    profile_id TEXT NOT NULL,
                    domain TEXT NOT NULL,
                    cookie_count INTEGER NOT NULL,
                    cookie_names TEXT NOT NULL,
                    profile_name TEXT NOT NULL,
                    account_name TEXT NOT NULL,
                    username TEXT NOT NULL DEFAULT '',
                    api_key_count INTEGER NOT NULL DEFAULT 0,
                    api_model_count INTEGER NOT NULL DEFAULT 0,
                    remaining REAL,
                    used REAL,
                    total REAL,
                    unit TEXT NOT NULL DEFAULT '',
                    is_valid INTEGER NOT NULL DEFAULT 0,
                    sync_error TEXT NOT NULL DEFAULT '',
                    checkin_enabled INTEGER NOT NULL DEFAULT 0,
                    checked_in_today INTEGER NOT NULL DEFAULT 0,
                    checkin_error TEXT NOT NULL DEFAULT '',
                    checkin_date TEXT NOT NULL DEFAULT '',
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );",
            )
            .unwrap();
        for row in [
            (
                "site-a",
                "Default",
                "a.example",
                2_i64,
                r#"["session","token"]"#,
                "个人资料 1",
                "a@example.com",
            ),
            (
                "site-a",
                "Profile 2",
                "a.example",
                1_i64,
                r#"["session"]"#,
                "工作",
                "work@example.com",
            ),
            (
                "site-b",
                "Default",
                "b.example",
                3_i64,
                r#"["a","b","c"]"#,
                "个人资料 1",
                "a@example.com",
            ),
        ] {
            connection
                .execute(
                    "INSERT INTO site_accounts (
                        site_id, profile_id, domain, cookie_count, cookie_names,
                        profile_name, account_name
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![row.0, row.1, row.2, row.3, row.4, row.5, row.6],
                )
                .unwrap();
        }

        let cached = read_cached_usage_sites(&connection).unwrap();
        assert_eq!(cached.len(), 2);
        assert_eq!(cached[0].site_id, "site-a");
        assert_eq!(cached[0].sessions.len(), 2);
        assert_eq!(cached[0].sessions[0].profile_id, "Default");
        assert_eq!(cached[0].sessions[0].cookie_names, ["session", "token"]);
        assert_eq!(cached[0].sessions[0].api_key_count, 0);
        assert_eq!(cached[0].sessions[0].api_model_count, 0);
        assert_eq!(cached[1].site_id, "site-b");
        assert_eq!(cached[1].sessions[0].cookie_count, 3);
    }

    #[test]
    fn caches_only_the_profile_api_counts() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE site_accounts (
                    site_id TEXT NOT NULL,
                    profile_id TEXT NOT NULL,
                    api_key_count INTEGER NOT NULL DEFAULT 0,
                    api_model_count INTEGER NOT NULL DEFAULT 0
                 );
                 INSERT INTO site_accounts (site_id, profile_id) VALUES ('site-a', 'Default');",
            )
            .unwrap();
        let database = Database(std::sync::Mutex::new(connection));
        let result = SiteModelsResult {
            models: vec![SiteModelItem {
                id: "gpt-5".into(),
                owned_by: None,
            }],
            source: "newapi-key".into(),
            keys: vec!["sk-one".into(), "sk-two".into()],
        };
        cache_profile_api_counts(&database, Some("site-a"), Some("Default"), result).unwrap();
        let connection = database.0.lock().unwrap();
        let counts = connection
            .query_row(
                "SELECT api_key_count, api_model_count FROM site_accounts WHERE site_id = 'site-a' AND profile_id = 'Default'",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap();
        assert_eq!(counts, (2, 1));
    }

    #[test]
    fn extracts_newapi_account_from_local_storage() {
        let values = HashMap::from([
            (
                "user".into(),
                r#"{"username":"wudixm","quota":10000000,"used_quota":2500000}"#.into(),
            ),
            ("quota_display_type".into(), r#""CNY""#.into()),
            ("quota_per_unit".into(), "1000000".into()),
        ]);
        let account = parse_newapi_local_account(&values).unwrap();
        assert_eq!(account.username, "wudixm");
        assert_eq!(account.remaining, Some(10.0));
        assert_eq!(account.used, Some(2.5));
        assert_eq!(account.total, Some(12.5));
        assert_eq!(account.unit, "CNY");
    }

    #[test]
    fn newapi_local_account_requires_an_object_and_defaults_missing_quota_to_zero() {
        let invalid = HashMap::from([("user".into(), r#""signed-in""#.into())]);
        assert!(parse_newapi_local_account(&invalid).is_err());

        let valid = HashMap::from([("user".into(), r#"{"id":10288,"username":"wudixm"}"#.into())]);
        let account = parse_newapi_local_account(&valid).unwrap();
        assert_eq!(account.remaining, Some(0.0));
        assert_eq!(account.used, Some(0.0));
        assert_eq!(account.total, Some(0.0));
    }

    #[test]
    fn recognizes_newapi_refresh_cookie_without_local_user() {
        let values = HashMap::new();
        let cookie_names = vec!["new_api_refresh".to_string()];

        assert!(has_newapi_refresh_cookie_name(
            cookie_names.iter().map(String::as_str)
        ));
        assert!(has_account_session_candidate(
            "NewAPI",
            &values,
            &cookie_names
        ));
        assert!(!has_account_session_candidate(
            "Sub2API",
            &values,
            &cookie_names
        ));
        assert!(cookie_header_has_name(
            "status=active; new_api_refresh=redacted",
            "new_api_refresh"
        ));
        assert!(!cookie_header_has_name(
            "new_api_refresh_backup=redacted",
            "new_api_refresh"
        ));
    }

    #[test]
    fn infers_newapi_from_valid_local_user_after_inconclusive_status_probe() {
        let any_router = HashMap::from([(
            "user".into(),
            r#"{"id":162120,"username":"linuxdo_162120","quota":0}"#.into(),
        )]);
        assert_eq!(
            infer_system_type_from_local_accounts([&any_router]),
            "NewAPI"
        );
    }

    #[test]
    fn extracts_newapi_checkin_status() {
        let value = serde_json::json!({
            "data": {
                "enabled": true,
                "max_quota": 12_500_000,
                "min_quota": 12_500_000,
                "stats": {
                    "checked_in_today": false,
                    "checkin_count": 0,
                    "records": [],
                    "total_checkins": 9,
                    "total_quota": 112_500_000
                }
            },
            "success": true
        });
        assert_eq!(parse_newapi_checkin_status(&value).unwrap(), (true, false));
    }

    #[test]
    fn extracts_sub2api_balance_and_default_unit() {
        let value = serde_json::json!({
            "code": 0,
            "data": {
                "username": "ass120",
                "status": "active",
                "balance": 79.2340617
            }
        });
        let account = parse_sub2api_account(&value).unwrap();
        assert_eq!(account.username, "ass120");
        assert_eq!(account.remaining, Some(79.2340617));
        assert_eq!(account.unit, "USD");
    }

    #[test]
    fn extracts_sub2api_daily_checkin_status() {
        for (value, expected) in [
            (
                serde_json::json!({ "code": 0, "data": { "checked_in_today": true } }),
                true,
            ),
            (
                serde_json::json!({ "code": 0, "data": { "checked_in": false } }),
                false,
            ),
            (
                serde_json::json!({ "success": true, "data": { "is_checked_in": 1 } }),
                true,
            ),
            (
                serde_json::json!({ "success": true, "data": "not_checked_in" }),
                false,
            ),
        ] {
            assert_eq!(parse_sub2api_checkin_status(&value).unwrap(), expected);
        }
        assert!(parse_sub2api_checkin_status(
            &serde_json::json!({ "code": 1, "message": "unauthorized" })
        )
        .is_err());
        assert!(sub2api_response_succeeded(
            &serde_json::json!({ "code": 0, "data": {} })
        ));
        assert!(sub2api_response_succeeded(
            &serde_json::json!({ "success": true })
        ));
    }

    #[test]
    fn extracts_sub2api_account_from_local_storage() {
        let values = HashMap::from([(
            "auth_user".into(),
            r#"{"username":"ass120","status":"active","balance":79.2340617}"#.into(),
        )]);
        let account = parse_sub2api_local_account(&values).unwrap();
        assert_eq!(account.username, "ass120");
        assert_eq!(account.remaining, Some(79.2340617));
        assert_eq!(account.unit, "USD");
    }

    #[test]
    fn extracts_zero_v_zero_account_and_stats_without_exposing_the_token() {
        let values = HashMap::from([("0v0_token".into(), r#""secret-token""#.into())]);
        assert_eq!(zero_v_zero_token(&values).as_deref(), Some("secret-token"));

        let self_value = serde_json::json!({
            "success": true,
            "data": {
                "id": 871,
                "username": "zero-user",
                "quota": 10_000_000,
                "used_quota": 2_500_000
            }
        });
        let mut account = parse_zero_v_zero_self(&self_value).unwrap();
        assert_eq!(account.username, "zero-user");
        assert_eq!(account.remaining, Some(20.0));
        assert_eq!(account.used, Some(5.0));
        assert_eq!(account.total, Some(25.0));

        let stats = serde_json::json!({
            "success": true,
            "data": { "total_quota": 25_000_000, "used_quota": 1_000_000 }
        });
        apply_zero_v_zero_stats(&mut account, &stats).unwrap();
        assert_eq!(account.remaining, Some(50.0));
        assert_eq!(account.used, Some(2.0));
        assert_eq!(account.total, Some(52.0));
        assert_eq!(account.unit, "USD");
    }

    #[test]
    fn maps_zero_v_zero_document_and_api_domains_to_the_console() {
        assert_eq!(
            account_base_url("0v0", "https://docs.0v0.club/", ""),
            "https://0v0.club/"
        );
        assert_eq!(
            account_base_url("Other", "https://api.0v0.club/v1", ""),
            "https://0v0.club/"
        );
        assert_eq!(
            account_base_url("Other", "https://example.com/", "NewAPI"),
            "https://example.com/"
        );
    }

    #[test]
    fn sub2api_local_account_requires_auth_user_and_defaults_missing_balance_to_zero() {
        let token_only = HashMap::from([("auth_token".into(), r#""secret""#.into())]);
        assert!(parse_sub2api_local_account(&token_only).is_err());

        let valid = HashMap::from([("auth_user".into(), r#"{"username":"ass120"}"#.into())]);
        let account = parse_sub2api_local_account(&valid).unwrap();
        assert_eq!(account.remaining, Some(0.0));
    }

    #[test]
    fn extracts_enabled_api_keys_from_newapi_and_sub2api_responses() {
        let newapi = serde_json::json!({
            "success": true,
            "data": {
                "items": [
                    { "key": "sk-newapi-enabled", "status": 1 },
                    { "key": "sk-newapi-disabled", "status": 0 },
                    { "key": "sk-newapi-expired", "status": 1, "expired_time": 1 }
                ]
            }
        });
        assert_eq!(parse_api_keys(&newapi), ["sk-newapi-enabled"]);

        let sub2api = serde_json::json!({
            "data": {
                "keys": [
                    { "api_key": "sk-sub2api-enabled", "is_active": true },
                    { "apiKey": "sk-sub2api-disabled", "is_active": false },
                    { "secret_key": "raw-key-value", "key_prefix": "sub2-" },
                    { "key": "sk-****masked" }
                ]
            }
        });
        assert_eq!(
            parse_api_keys(&sub2api),
            ["raw-key-value", "sk-sub2api-enabled", "sub2-raw-key-value"]
        );

        let masked_newapi = serde_json::json!({
            "data": {
                "items": [
                    { "id": 567, "key": "sk-****masked", "status": 1 },
                    { "id": 568, "key": "sk-****disabled", "status": 0 }
                ]
            }
        });
        assert!(parse_api_keys(&masked_newapi).is_empty());
        assert_eq!(parse_newapi_token_ids(&masked_newapi), ["567"]);
        assert_eq!(
            parse_revealed_api_key(&serde_json::json!({
                "success": true,
                "data": "sk-newapi-revealed"
            })),
            Some("sk-newapi-revealed".into())
        );
    }

    #[test]
    fn extracts_openai_style_api_error_messages() {
        let value = serde_json::json!({
            "error": {
                "message": "令牌无效",
                "type": "invalid_request_error"
            }
        });
        assert_eq!(api_error_message(&value, "请求失败"), "令牌无效");
    }

    #[test]
    fn normalizes_nested_and_root_model_lists_without_duplicates() {
        let nested = serde_json::json!({
            "data": {
                "models": [
                    { "id": "gpt-5", "owned_by": "openai" },
                    { "model_name": "claude-sonnet", "owner": "anthropic" },
                    { "id": "gpt-5", "owned_by": "duplicate" }
                ]
            }
        });
        let models = parse_site_models(&nested);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "claude-sonnet");
        assert_eq!(models[1].id, "gpt-5");

        let root = serde_json::json!(["qwen-max", { "name": "deepseek-v3" }]);
        assert_eq!(
            parse_site_models(&root)
                .into_iter()
                .map(|model| model.id)
                .collect::<Vec<_>>(),
            ["deepseek-v3", "qwen-max"]
        );
    }

    #[test]
    fn chrome_models_bridge_keeps_keys_inside_the_same_origin_script() {
        let script = chrome_models_bridge_script("NewAPI", Some("10288"), "openhub-models-123");

        assert!(script.contains("keyPath = \"/api/token/?p=1&size=20\""));
        assert!(script.contains("keyPath = \"/api/v1/keys?page=1\""));
        assert!(!script.contains("keyPath = \"/api/token?p=0&size=100\""));
        assert!(!script.contains("keyPath = \"/v1/keys\""));
        assert!(script.contains("`/api/token/${encodeURIComponent(tokenId)}/key`"));
        assert!(script.contains(
            "`/api/token/${encodeURIComponent(tokenId)}/key`, { method: \"POST\", headers }"
        ));
        assert!(!script.contains(
            "`/api/token/${encodeURIComponent(tokenId)}/key`, { method: \"GET\", headers }"
        ));
        assert!(script.contains("readJson(\"/v1/models\""));
        assert!(script.contains("readJson(\"/api/user/auth/refresh\""));
        assert!(script.contains("return \"__OPENHUB_PROFILE_MISMATCH__\""));
        assert!(!script.contains("http://"));
        assert!(!script.contains("https://"));

        let result = parse_chrome_models_result(
            r#"{"ok":true,"source":"newapi-key","keys":["sk-model-key"],"models":[{"id":"gpt-5","ownedBy":"openai"}]}"#,
        )
        .unwrap();
        assert_eq!(result.source, "newapi-key");
        assert_eq!(result.keys, ["sk-model-key"]);
        assert_eq!(result.models[0].id, "gpt-5");

        assert_eq!(
            parse_chrome_models_keys(
                r#"{"ok":false,"error":"模型接口 HTTP 401","keys":["sk-partial-key"]}"#,
            ),
            ["sk-partial-key"]
        );
    }

    #[test]
    fn validates_network_proxy_urls() {
        assert_eq!(normalize_network_proxy("  ").unwrap(), "");
        assert_eq!(
            normalize_network_proxy(" http://127.0.0.1:7890 ").unwrap(),
            "http://127.0.0.1:7890"
        );
        assert!(normalize_network_proxy("https://proxy.example.com:8443").is_ok());
        assert!(normalize_network_proxy("socks5://127.0.0.1:1080").is_err());
        assert!(normalize_network_proxy("127.0.0.1:7890").is_err());
    }

    #[test]
    fn normalizes_remote_optional_urls_without_rejecting_the_sync() {
        let base_url = "https://magic.example/api/v1";

        assert_eq!(
            normalize_remote_url("/console/checkin", base_url),
            "https://magic.example/console/checkin"
        );
        assert_eq!(
            normalize_remote_url("https://status.magic.example/", base_url),
            "https://status.magic.example/"
        );
        assert_eq!(normalize_remote_url("magic.example/checkin", base_url), "");
        assert_eq!(normalize_remote_url("javascript:alert(1)", base_url), "");
    }

    #[test]
    fn recognizes_supported_remote_site_systems() {
        let explicit = serde_json::json!({ "siteType": "sub2api" });
        assert_eq!(
            infer_remote_system_type(explicit.as_object().unwrap()),
            "Sub2API"
        );

        let inferred = serde_json::json!({
            "checkinUrl": "https://example.com/console/personal"
        });
        assert_eq!(
            infer_remote_system_type(inferred.as_object().unwrap()),
            "NewAPI"
        );

        let unknown = serde_json::json!({ "apiBaseUrl": "https://example.com/" });
        assert!(infer_remote_system_type(unknown.as_object().unwrap()).is_empty());
    }

    #[test]
    fn classifies_site_system_probes_and_rejects_html_fallbacks() {
        let probe = |status, is_json| {
            Some(EndpointProbe {
                status,
                is_json,
                is_challenge: false,
            })
        };
        assert_eq!(
            system_type_from_probes(probe(reqwest::StatusCode::OK, true), None),
            Some("NewAPI")
        );
        assert_eq!(
            system_type_from_probes(
                probe(reqwest::StatusCode::UNAUTHORIZED, false),
                probe(reqwest::StatusCode::OK, true),
            ),
            Some("NewAPI")
        );
        assert_eq!(
            system_type_from_probes(
                probe(reqwest::StatusCode::NOT_FOUND, true),
                probe(reqwest::StatusCode::OK, true),
            ),
            Some("Sub2API")
        );
        assert_eq!(
            system_type_from_probes(
                probe(reqwest::StatusCode::NOT_FOUND, true),
                probe(reqwest::StatusCode::NOT_FOUND, true),
            ),
            Some("")
        );
        assert_eq!(
            system_type_from_probes(None, probe(reqwest::StatusCode::NOT_FOUND, true)),
            None
        );
        assert_eq!(
            system_type_from_probes(
                probe(reqwest::StatusCode::OK, false),
                probe(reqwest::StatusCode::OK, false),
            ),
            None
        );
    }

    #[test]
    fn recognizes_security_gateway_pages_without_treating_regular_html_as_a_shield() {
        assert!(shield_page_response(
            reqwest::StatusCode::OK,
            "text/html; charset=utf-8",
            true,
            b"compressed gateway response",
        ));
        assert!(shield_page_response(
            reqwest::StatusCode::FORBIDDEN,
            "text/html",
            false,
            b"<!doctype html><title>Just a moment</title>",
        ));
        assert!(!shield_page_response(
            reqwest::StatusCode::OK,
            "text/html",
            false,
            b"<!doctype html><title>API console</title>",
        ));
    }

    #[test]
    fn chrome_system_probe_requests_both_status_endpoints_in_parallel() {
        let script = chrome_system_probe_script("openhub-system-123");
        assert!(script.contains("Promise.all([probe(\"/api/status\"), probe(\"/setup/status\")])"));
        assert_eq!(script.matches("AbortSignal.timeout(12000)").count(), 1);
        assert!(!script.contains("http://"));
        assert!(!script.contains("https://"));
    }

    #[test]
    fn chrome_account_bridge_uses_only_fixed_same_origin_endpoints() {
        let script = chrome_account_bridge_script(
            Some("10288"),
            "2026-08",
            "openhub-sync-123",
            true,
            true,
            false,
            true,
        );

        assert!(script.contains("fetch(\"/api/user/auth/refresh\""));
        assert!(script.contains("fetch(\"/api/user/self\""));
        assert!(script.contains("`/api/user/checkin?month=${encodeURIComponent(\"2026-08\")}`"));
        assert!(script.contains("fetch(\"/api/user/checkin\""));
        assert_eq!(script.matches("fetch(").count(), 5);
        assert!(!script.contains("http://"));
        assert!(!script.contains("https://"));
        assert!(script.contains("window.location.protocol !== \"http:\""));
        assert!(script.contains("message.includes(\"Failed to parse URL\")"));
        assert!(script.contains("previous.state !== \"challenge\""));
        assert!(script.contains("state: \"running\""));
        assert!(script.contains("bridge.state = \"challenge\""));
        assert!(script.contains("window.location.assign(`/api/user/self#${token}`)"));
        assert!(script.contains("const shouldCheckin = true"));
        assert!(script.contains("const useRefreshAuth = true"));
        assert!(script.contains("const isAnyRouter = false"));
        assert!(script.contains("const allowChallengeNavigation = true"));
        assert!(script.contains("return \"__OPENHUB_PROFILE_MISMATCH__\""));
        assert!(
            script.find("const checkinResponse").unwrap()
                < script.find("fetch(\"/api/user/self\"").unwrap()
        );
        assert!(script.contains("const requestTimeout = isAnyRouter ? 12000 : 30000"));
        assert_eq!(
            script
                .matches("AbortSignal.timeout(requestTimeout)")
                .count(),
            5
        );
        assert!(!script.contains("account: accessToken"));
        assert!(!script.contains("if (Date.now() - previous.started < 3000) return pending;"));
    }

    #[test]
    fn any_router_bridge_uses_only_the_site_specific_checkin_endpoint() {
        let script = chrome_account_bridge_script(
            Some("10288"),
            "2026-08",
            "openhub-sync-anyrouter",
            false,
            true,
            true,
            false,
        );

        assert!(script.contains("const useRefreshAuth = false"));
        assert!(script.contains("const isAnyRouter = true"));
        assert!(script.contains("if (isAnyRouter)"));
        assert!(script.contains("fetch(\"/api/user/sign_in\""));
        assert!(script.contains("method: \"POST\""));
        assert_eq!(script.matches("fetch(").count(), 5);
        assert!(is_any_router_site("https://anyrouter.top/"));
        assert!(is_any_router_site("https://api.anyrouter.top/v1"));
        assert!(!is_any_router_site("https://not-anyrouter.example/"));
        assert!(script.contains("已(?:经)?签到|already (?:signed|checked)"));
    }

    #[test]
    fn chrome_account_bridge_json_escapes_embedded_values() {
        let user_id = "10288\"; window.injected = true; //";
        let month = "2026-08\nnext";
        let marker = "openhub-sync-\"quoted";
        let script =
            chrome_account_bridge_script(Some(user_id), month, marker, false, false, false, false);

        assert!(script.contains(&format!(
            "const legacyUserId = {}",
            serde_json::to_string(user_id).unwrap()
        )));
        assert!(script.contains(&format!(
            "encodeURIComponent({})",
            serde_json::to_string(month).unwrap()
        )));
        assert!(script.contains(&format!(
            "const token = {}",
            serde_json::to_string(marker).unwrap()
        )));
        assert!(script.contains("const shouldCheckin = false"));
        assert!(script.contains("const allowChallengeNavigation = false"));
        assert!(!script.contains("const legacyUserId = \"10288\"; window.injected"));
    }
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
            import_site,
            update_site,
            delete_site,
            toggle_personal,
            toggle_hidden,
            toggle_runaway,
            get_network_proxy,
            set_network_proxy,
            get_remote_user,
            mark_sites_with_chrome_sessions,
            sync_site_account_via_chrome,
            sync_remote_sites,
            detect_site_system_types,
            get_system_fonts,
            fetch_site_models_json,
            chrome_session::list_chrome_sessions,
            chrome_session::read_chrome_session,
            chrome_session::open_url_in_chrome_profile
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}
