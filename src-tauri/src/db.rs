use crate::chrome_session;
use crate::models::*;
use rusqlite::{params, Connection, OptionalExtension};
use std::{collections::HashMap, path::Path, time::Duration};

impl Database {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
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
                PRAGMA busy_timeout = 5000;

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
                    is_pending INTEGER NOT NULL DEFAULT 0,
                    use_system_proxy INTEGER NOT NULL DEFAULT 0,
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
                    newapi_token TEXT NOT NULL DEFAULT '',
                    newapi_user_id TEXT NOT NULL DEFAULT '',
                    PRIMARY KEY (site_id, profile_id, domain),
                    FOREIGN KEY(site_id) REFERENCES directory_sites(id) ON DELETE CASCADE
                );

                CREATE TABLE IF NOT EXISTS site_model_cache (
                    site_id TEXT NOT NULL,
                    profile_id TEXT NOT NULL,
                    profile_name TEXT NOT NULL DEFAULT '',
                    account_name TEXT NOT NULL DEFAULT '',
                    username TEXT NOT NULL DEFAULT '',
                    api_source TEXT NOT NULL DEFAULT '',
                    keys_json TEXT NOT NULL DEFAULT '[]',
                    models_json TEXT NOT NULL DEFAULT '[]',
                    error TEXT NOT NULL DEFAULT '',
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    PRIMARY KEY (site_id, profile_id),
                    FOREIGN KEY(site_id) REFERENCES directory_sites(id) ON DELETE CASCADE
                );

                CREATE TABLE IF NOT EXISTS app_meta (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );

                CREATE TABLE IF NOT EXISTS charity_feed_items (
                    feed_id TEXT NOT NULL,
                    guid TEXT NOT NULL,
                    title TEXT NOT NULL,
                    link TEXT NOT NULL,
                    author TEXT NOT NULL DEFAULT '',
                    published_at TEXT NOT NULL DEFAULT '',
                    summary TEXT NOT NULL DEFAULT '',
                    categories TEXT NOT NULL DEFAULT '[]',
                    reply_count INTEGER NOT NULL DEFAULT 0,
                    views INTEGER NOT NULL DEFAULT 0,
                    like_count INTEGER NOT NULL DEFAULT 0,
                    last_activity_at TEXT NOT NULL DEFAULT '',
                    pinned INTEGER NOT NULL DEFAULT 0,
                    posters TEXT NOT NULL DEFAULT '[]',
                    first_seen_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    last_seen_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    PRIMARY KEY (feed_id, guid)
                );

                CREATE INDEX IF NOT EXISTS idx_directory_sites_favorite ON directory_sites(favorite);
                CREATE INDEX IF NOT EXISTS idx_directory_sites_hidden ON directory_sites(hidden);
                CREATE INDEX IF NOT EXISTS idx_directory_sites_updated ON directory_sites(updated_at);
                CREATE INDEX IF NOT EXISTS idx_site_accounts_site ON site_accounts(site_id);
                CREATE INDEX IF NOT EXISTS idx_site_model_cache_site ON site_model_cache(site_id);
                                CREATE INDEX IF NOT EXISTS idx_charity_feed_seen ON charity_feed_items(feed_id, last_seen_at);
                CREATE INDEX IF NOT EXISTS idx_charity_feed_published ON charity_feed_items(feed_id, published_at DESC);

                CREATE TABLE IF NOT EXISTS charity_sync_logs (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                    feed_id TEXT NOT NULL,
                    feed_name TEXT NOT NULL DEFAULT '',
                    stage TEXT NOT NULL DEFAULT '',
                    status TEXT NOT NULL DEFAULT '',
                    message TEXT NOT NULL DEFAULT '',
                    node_name TEXT NOT NULL DEFAULT '',
                    duration_ms INTEGER NOT NULL DEFAULT 0
                );
                CREATE INDEX IF NOT EXISTS idx_charity_sync_logs_created
                    ON charity_sync_logs(created_at DESC, id DESC);

                CREATE TABLE IF NOT EXISTS proxy_subscriptions (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    url TEXT NOT NULL,
                    node_count INTEGER NOT NULL DEFAULT 0,
                    last_error TEXT NOT NULL DEFAULT '',
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );

                CREATE TABLE IF NOT EXISTS proxy_nodes (
                    id TEXT PRIMARY KEY,
                    subscription_id TEXT NOT NULL,
                    name TEXT NOT NULL,
                    proxy_type TEXT NOT NULL DEFAULT '',
                    server TEXT NOT NULL DEFAULT '',
                    port INTEGER NOT NULL DEFAULT 0,
                    cipher TEXT NOT NULL DEFAULT '',
                    udp INTEGER NOT NULL DEFAULT 0,
                    raw_json TEXT NOT NULL DEFAULT '',
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    FOREIGN KEY(subscription_id) REFERENCES proxy_subscriptions(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_proxy_nodes_subscription ON proxy_nodes(subscription_id);

                CREATE TABLE IF NOT EXISTS proxy_pool_nodes (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    proxy_type TEXT NOT NULL DEFAULT '',
                    server TEXT NOT NULL DEFAULT '',
                    port INTEGER NOT NULL DEFAULT 0,
                    cipher TEXT NOT NULL DEFAULT '',
                    udp INTEGER NOT NULL DEFAULT 0,
                    raw_json TEXT NOT NULL DEFAULT '',
                    latency_ms INTEGER,
                    test_status TEXT NOT NULL DEFAULT '',
                    tested_at TEXT NOT NULL DEFAULT '',
                    country_code TEXT NOT NULL DEFAULT '',
                    country_name TEXT NOT NULL DEFAULT '',
                    classification TEXT NOT NULL DEFAULT '',
                    primary_ip TEXT NOT NULL DEFAULT '',
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );

                CREATE TABLE IF NOT EXISTS proxy_subscription_nodes (
                    subscription_id TEXT NOT NULL,
                    node_id TEXT NOT NULL,
                    PRIMARY KEY (subscription_id, node_id),
                    FOREIGN KEY(subscription_id) REFERENCES proxy_subscriptions(id) ON DELETE CASCADE,
                    FOREIGN KEY(node_id) REFERENCES proxy_pool_nodes(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_proxy_subscription_nodes_node ON proxy_subscription_nodes(node_id);

                ",
            )
            .map_err(|error| error.to_string())?;

        ensure_site_account_columns(&connection)?;
        ensure_charity_feed_schema(&connection)?;
        ensure_charity_sync_log_columns(&connection)?;
        ensure_proxy_pool_node_columns(&connection)?;
        reset_expired_checkin_states(&connection)?;

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

        let has_use_system_proxy: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('directory_sites') WHERE name='use_system_proxy'",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if has_use_system_proxy == 0 {
            connection
                .execute(
                    "ALTER TABLE directory_sites ADD COLUMN use_system_proxy INTEGER NOT NULL DEFAULT 0",
                    [],
                )
                .map_err(|error| error.to_string())?;
        }

        let has_is_pending: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('directory_sites') WHERE name='is_pending'",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if has_is_pending == 0 {
            connection
                .execute(
                    "ALTER TABLE directory_sites ADD COLUMN is_pending INTEGER NOT NULL DEFAULT 0",
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

/// 将跨天的签到缓存归零。签到状态只对 `checkin_date` 当天有效，
/// 不能把昨天的成功状态带到今天；同时清理昨天遗留的错误提示。
pub(crate) fn reset_expired_checkin_states(connection: &Connection) -> Result<usize, String> {
    connection
        .execute(
            "UPDATE site_accounts
             SET checked_in_today = 0,
                 checkin_error = '',
                 checkin_date = date('now', 'localtime')
             WHERE COALESCE(checkin_date, '') <> date('now', 'localtime')",
            [],
        )
        .map_err(|error| error.to_string())
}

pub(crate) fn migrate_legacy_favorites_to_personal(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "UPDATE directory_sites SET is_personal = 1 WHERE favorite = 1;
             UPDATE directory_sites SET favorite = 0 WHERE favorite <> 0;",
        )
        .map_err(|error| error.to_string())
}

pub(crate) fn ensure_site_account_columns(connection: &Connection) -> Result<(), String> {
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
        ("newapi_token", "TEXT NOT NULL DEFAULT ''"),
        ("newapi_user_id", "TEXT NOT NULL DEFAULT ''"),
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

pub(crate) fn ensure_charity_feed_schema(connection: &Connection) -> Result<(), String> {
    let has_feed_id = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('charity_feed_items') WHERE name = 'feed_id'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| error.to_string())?
        > 0;
    if has_feed_id {
        // execute 只能跑单条 SQL；多条索引语句必须用 execute_batch。
        connection
            .execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_charity_feed_seen
                 ON charity_feed_items(feed_id, last_seen_at);
                 CREATE INDEX IF NOT EXISTS idx_charity_feed_published
                 ON charity_feed_items(feed_id, published_at DESC);",
            )
            .map_err(|error| error.to_string())?;
        ensure_charity_feed_metric_columns(connection)?;
        return Ok(());
    }
    connection
        .execute_batch(
            "DROP INDEX IF EXISTS idx_charity_feed_seen;
             ALTER TABLE charity_feed_items RENAME TO charity_feed_items_legacy;
             CREATE TABLE charity_feed_items (
               feed_id TEXT NOT NULL,
               guid TEXT NOT NULL,
               title TEXT NOT NULL,
               link TEXT NOT NULL,
               author TEXT NOT NULL DEFAULT '',
               published_at TEXT NOT NULL DEFAULT '',
               summary TEXT NOT NULL DEFAULT '',
               categories TEXT NOT NULL DEFAULT '[]',
               reply_count INTEGER NOT NULL DEFAULT 0,
               views INTEGER NOT NULL DEFAULT 0,
               like_count INTEGER NOT NULL DEFAULT 0,
               last_activity_at TEXT NOT NULL DEFAULT '',
               pinned INTEGER NOT NULL DEFAULT 0,
               posters TEXT NOT NULL DEFAULT '[]',
               first_seen_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               last_seen_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               PRIMARY KEY (feed_id, guid)
             );
             INSERT INTO charity_feed_items
               (feed_id, guid, title, link, author, published_at, summary, categories, first_seen_at, last_seen_at)
             SELECT '1515', guid, title, link, author, published_at, summary, categories, first_seen_at, last_seen_at
             FROM charity_feed_items_legacy;
             DROP TABLE charity_feed_items_legacy;
             CREATE INDEX idx_charity_feed_seen ON charity_feed_items(feed_id, last_seen_at);
             CREATE INDEX IF NOT EXISTS idx_charity_feed_published ON charity_feed_items(feed_id, published_at DESC);",
        )
        .map_err(|error| error.to_string())
}

pub(crate) fn ensure_charity_feed_metric_columns(connection: &Connection) -> Result<(), String> {
    for (name, definition) in [
        ("reply_count", "INTEGER NOT NULL DEFAULT 0"),
        ("views", "INTEGER NOT NULL DEFAULT 0"),
        ("like_count", "INTEGER NOT NULL DEFAULT 0"),
        ("last_activity_at", "TEXT NOT NULL DEFAULT ''"),
        ("pinned", "INTEGER NOT NULL DEFAULT 0"),
        ("posters", "TEXT NOT NULL DEFAULT '[]'"),
    ] {
        let exists = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('charity_feed_items') WHERE name = ?1",
                [name],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            > 0;
        if !exists {
            connection
                .execute(
                    &format!("ALTER TABLE charity_feed_items ADD COLUMN {name} {definition}"),
                    [],
                )
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn ensure_charity_sync_log_columns(connection: &Connection) -> Result<(), String> {
    let has_duration: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('charity_sync_logs') WHERE name='duration_ms'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if has_duration == 0 {
        connection
            .execute_batch(
                "ALTER TABLE charity_sync_logs ADD COLUMN duration_ms INTEGER NOT NULL DEFAULT 0;",
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(crate) fn read_cached_usage_sites(
    connection: &Connection,
) -> Result<Vec<chrome_session::ChromeSiteSessionMatch>, String> {
    // 运行中跨过午夜时也要即时归零，不能只依赖应用启动时的迁移。
    reset_expired_checkin_states(connection)?;
    let mut statement = connection
        .prepare(
            "SELECT sa.site_id, sa.profile_id, sa.domain, sa.cookie_count, sa.cookie_names,
                    sa.profile_name, sa.account_name, sa.username,
                    sa.api_key_count, sa.api_model_count,
                    sa.remaining, sa.used, sa.total, sa.unit, sa.is_valid, sa.sync_error,
                    sa.checkin_enabled,
                    CASE WHEN sa.checkin_date = date('now', 'localtime') THEN sa.checked_in_today ELSE 0 END,
                    sa.checkin_error, sa.updated_at, sa.newapi_token, sa.newapi_user_id,
                    CASE WHEN smc.profile_id IS NULL THEN 0 ELSE 1 END,
                    COALESCE(smc.error, '')
             FROM site_accounts sa
             LEFT JOIN site_model_cache smc
               ON smc.site_id = sa.site_id AND smc.profile_id = sa.profile_id
             ORDER BY sa.site_id, sa.rowid",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            let cookie_names_json = row.get::<_, String>(4)?;
            let newapi_token = row.get::<_, String>(20)?;
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
                    api_counts_synced: row.get::<_, i64>(22)? != 0,
                    api_sync_error: row.get(23)?,
                    has_access_token: !newapi_token.is_empty(),
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
                    newapi_token,
                    newapi_user_id: row.get(21)?,
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

pub(crate) fn read_network_proxy(database: &Database) -> Result<String, String> {
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

pub(crate) fn persist_site_system_types(
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

pub(crate) fn read_proxy_ignore_addresses(database: &Database) -> Result<String, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    connection
        .query_row(
            "SELECT value FROM app_meta WHERE key = ?1",
            [PROXY_IGNORE_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map(|value| {
            value
                .filter(|item| !item.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_PROXY_IGNORE.to_string())
        })
        .map_err(|error| error.to_string())
}

pub(crate) fn build_http_client(
    database: &Database,
    timeout: Duration,
    redirects: usize,
    purpose: &str,
) -> Result<reqwest::Client, String> {
    build_http_client_with_proxy(database, timeout, redirects, purpose)
}

pub(crate) fn build_http_client_for_site(
    database: &Database,
    _site_id: &str,
    timeout: Duration,
    redirects: usize,
    purpose: &str,
) -> Result<reqwest::Client, String> {
    build_http_client_with_proxy(database, timeout, redirects, purpose)
}

pub(crate) fn build_http_client_with_proxy(
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
        let ignore = read_proxy_ignore_addresses(database)?;
        let proxy = reqwest::Proxy::all(&proxy_url)
            .map_err(|_| "代理池当前出口地址无效")?
            .no_proxy(reqwest::NoProxy::from_string(&ignore));
        builder = builder.proxy(proxy);
    }
    builder
        .build()
        .map_err(|error| format!("无法初始化{purpose}：{error}"))
}

pub(crate) fn insert_site_transaction(
    transaction: &rusqlite::Transaction,
    site: &SiteRecord,
) -> Result<(), String> {
    transaction.execute(
        "INSERT OR REPLACE INTO directory_sites (
            id, name, description, registration_limit, icon, api_base_url, system_type,
            supports_immersive_translation, supports_ldc, supports_checkin, supports_nsfw,
            checkin_url, checkin_note, benefit_url, rate_limit, status_url,
            is_only_maintainer_visible, requires_invite_code, is_runaway, is_fake_charity,
            has_pending_report, is_personal, is_pending, use_system_proxy, favorite, hidden, created_at, updated_at
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7,
            ?8, ?9, ?10, ?11,
            ?12, ?13, ?14, ?15, ?16,
            ?17, ?18, ?19, ?20,
            ?21, ?22, ?23, ?24, ?25, ?26,
            COALESCE(NULLIF(?27, ''), CURRENT_TIMESTAMP), COALESCE(NULLIF(?27, ''), CURRENT_TIMESTAMP)
        )",
        params![
            site.id, site.name, site.description, site.registration_limit, site.icon, site.api_base_url, site.system_type,
            site.supports_immersive_translation, site.supports_ldc, site.supports_checkin, site.supports_nsfw,
            site.checkin_url, site.checkin_note, site.benefit_url, site.rate_limit, site.status_url,
            site.is_only_maintainer_visible, site.requires_invite_code, site.is_runaway, site.is_fake_charity,
            site.has_pending_report, site.is_personal, site.is_pending, site.use_system_proxy, site.favorite, site.hidden, site.updated_at
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

pub(crate) fn seed_database(connection: &mut Connection) -> Result<(), String> {
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
            site.use_system_proxy = false;
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

pub(crate) fn ensure_proxy_pool_node_columns(connection: &Connection) -> Result<(), String> {
    for (name, definition) in [
        ("country_code", "TEXT NOT NULL DEFAULT ''"),
        ("country_name", "TEXT NOT NULL DEFAULT ''"),
        ("classification", "TEXT NOT NULL DEFAULT ''"),
        ("primary_ip", "TEXT NOT NULL DEFAULT ''"),
    ] {
        let exists = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('proxy_pool_nodes') WHERE name = ?1",
                [name],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            > 0;
        if !exists {
            connection
                .execute(
                    &format!("ALTER TABLE proxy_pool_nodes ADD COLUMN {name} {definition}"),
                    [],
                )
                .map_err(|error| error.to_string())?;
        }
    }
    // 已有数据：先用节点名快速回填国家，避免首次分组再全量分析。
    connection
        .execute(
            "UPDATE proxy_pool_nodes
             SET country_code = CASE
                WHEN country_code IS NULL OR TRIM(country_code) = '' THEN ''
                ELSE country_code
             END",
            [],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}
