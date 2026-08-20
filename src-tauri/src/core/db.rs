use crate::chrome_session;
use crate::models::*;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Serialize};
use std::{path::Path, time::Duration};

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

        let has_legacy_catalog: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('model_catalog_models') WHERE name='canonical_key'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if has_legacy_catalog > 0 {
            connection.execute_batch("DROP TABLE IF EXISTS model_catalog_entries; DROP TABLE IF EXISTS model_catalog_models; DROP TABLE IF EXISTS model_catalog_sources; DROP TABLE IF EXISTS model_catalog_providers;").ok();
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
                    use_proxy_pool INTEGER NOT NULL DEFAULT 0,
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
                    browser_fallback_failed_at INTEGER NOT NULL DEFAULT 0,
                    browser_fallback_fail_count INTEGER NOT NULL DEFAULT 0,
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
                    groups_json TEXT NOT NULL DEFAULT '{}',
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

                CREATE TABLE IF NOT EXISTS token_cache_snapshots (
                    kind TEXT PRIMARY KEY,
                    payload_json TEXT NOT NULL,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );

                CREATE TABLE IF NOT EXISTS model_catalog_sources (
                    source TEXT PRIMARY KEY,
                    url TEXT NOT NULL,
                    fetched_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    record_count INTEGER NOT NULL DEFAULT 0,
                    raw_json TEXT NOT NULL
                );

                CREATE TABLE IF NOT EXISTS model_catalog_providers (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL DEFAULT '',
                    npm TEXT,
                    api TEXT,
                    doc TEXT,
                    tier TEXT,
                    subscription INTEGER NOT NULL DEFAULT 0,
                    model_count INTEGER NOT NULL DEFAULT 0,
                    date_modified TEXT,
                    raw_json TEXT NOT NULL DEFAULT '{}'
                );

                CREATE TABLE IF NOT EXISTS model_catalog_models (
                    id TEXT PRIMARY KEY,
                    slug TEXT NOT NULL DEFAULT '',
                    name TEXT NOT NULL DEFAULT '',
                    lab TEXT NOT NULL DEFAULT '',
                    kind TEXT NOT NULL DEFAULT '',
                    family TEXT,
                    knowledge TEXT,
                    status TEXT NOT NULL DEFAULT 'ga',
                    open_weights INTEGER NOT NULL DEFAULT 0,
                    reasoning INTEGER NOT NULL DEFAULT 0,
                    tool_call INTEGER NOT NULL DEFAULT 0,
                    attachment INTEGER NOT NULL DEFAULT 0,
                    structured INTEGER NOT NULL DEFAULT 0,
                    temperature INTEGER NOT NULL DEFAULT 0,
                    input_modalities_json TEXT NOT NULL DEFAULT '[]',
                    context_length INTEGER NOT NULL DEFAULT 0,
                    context_min INTEGER NOT NULL DEFAULT 0,
                    context_max INTEGER NOT NULL DEFAULT 0,
                    max_output_tokens INTEGER NOT NULL DEFAULT 0,
                    ref_provider TEXT,
                    ref_official INTEGER NOT NULL DEFAULT 0,
                    ref_input_cost REAL NOT NULL DEFAULT 0,
                    ref_output_cost REAL NOT NULL DEFAULT 0,
                    ref_cache_read_cost REAL NOT NULL DEFAULT 0,
                    min_provider TEXT,
                    min_input_cost REAL NOT NULL DEFAULT 0,
                    min_output_cost REAL NOT NULL DEFAULT 0,
                    min_cache_read_cost REAL NOT NULL DEFAULT 0,
                    price_spread REAL NOT NULL DEFAULT 0,
                    blended_min REAL,
                    blended_trusted REAL,
                    blended_ref REAL,
                    host_count INTEGER NOT NULL DEFAULT 0,
                    priced_host_count INTEGER NOT NULL DEFAULT 0,
                    free_host_count INTEGER NOT NULL DEFAULT 0,
                    sub_host_count INTEGER NOT NULL DEFAULT 0,
                    host_providers_json TEXT NOT NULL DEFAULT '[]',
                    aa_idx REAL,
                    aa_coding REAL,
                    aa_agentic REAL,
                    aa_speed REAL,
                    aa_ttft REAL,
                    aa_task_cost REAL,
                    aa_json TEXT,
                    benchmark_count INTEGER NOT NULL DEFAULT 0,
                    release_date TEXT,
                    last_updated TEXT,
                    raw_json TEXT NOT NULL DEFAULT '{}',
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );

                CREATE INDEX IF NOT EXISTS idx_model_catalog_models_lab ON model_catalog_models(lab);
                CREATE INDEX IF NOT EXISTS idx_model_catalog_models_kind ON model_catalog_models(kind);
                CREATE INDEX IF NOT EXISTS idx_model_catalog_models_status ON model_catalog_models(status);
                CREATE INDEX IF NOT EXISTS idx_model_catalog_providers_name ON model_catalog_providers(name);

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
                -- 同一站点同一 Chrome Profile 只允许一条账号缓存，杜绝重复/串行账号行。
                DELETE FROM site_accounts
                WHERE rowid NOT IN (
                    SELECT MIN(rowid) FROM site_accounts GROUP BY site_id, profile_id
                );
                CREATE UNIQUE INDEX IF NOT EXISTS idx_site_accounts_site_profile
                    ON site_accounts(site_id, profile_id);
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
                    is_enabled INTEGER NOT NULL DEFAULT 1,
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
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

                CREATE TABLE IF NOT EXISTS proxy_channels (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    node_id TEXT NOT NULL DEFAULT '',
                    test_url TEXT NOT NULL DEFAULT '',
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );

                CREATE INDEX IF NOT EXISTS idx_proxy_channels_node ON proxy_channels(node_id);

                CREATE TABLE IF NOT EXISTS account_proxy_channels (
                    profile_id TEXT PRIMARY KEY,
                    channel_id TEXT NOT NULL,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    FOREIGN KEY(channel_id) REFERENCES proxy_channels(id) ON DELETE CASCADE
                );

                CREATE INDEX IF NOT EXISTS idx_account_proxy_channels_channel ON account_proxy_channels(channel_id);

                -- 旧的“每个账号独立固定节点”实现会造成站点内不同账号 IP 抖动，已废弃。
                DROP TABLE IF EXISTS account_proxy_nodes;
                DROP TABLE IF EXISTS site_proxy_channels;

                ",
            )
            .map_err(|error| error.to_string())?;

        migrate_account_proxy_channels_to_profile(&connection)?;
        ensure_site_account_columns(&connection)?;
        ensure_site_model_cache_columns(&connection)?;
        ensure_charity_feed_schema(&connection)?;
        ensure_charity_sync_log_columns(&connection)?;
        ensure_charity_feed_sources_table(&connection)?;
        ensure_proxy_pool_node_columns(&connection)?;
        ensure_opencode_proxy_logs_table(&connection)?;
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

        let has_use_proxy_pool: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('directory_sites') WHERE name='use_proxy_pool'",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if has_use_proxy_pool == 0 {
            connection
                .execute(
                    "ALTER TABLE directory_sites ADD COLUMN use_proxy_pool INTEGER NOT NULL DEFAULT 0",
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
                    WHEN LOWER(checkin_url) LIKE '%/console/%' THEN 'new-api'
                    WHEN LOWER(checkin_url) LIKE '%/profile%'
                      OR LOWER(checkin_url) LIKE '%/dashboard%' THEN 'sub2api'
                    ELSE system_type
                 END
                 WHERE TRIM(system_type) = ''",
                [],
            )
            .map_err(|error| error.to_string())?;
        // 平台类型归一化（校验键大小写无关，幂等）：迁移 metapi 的规范类型名。
        // 旧值 NewAPI/Sub2API 等统一写成 new-api/sub2api，前端按规范名展示与过滤。
        connection
            .execute_batch(
                "UPDATE directory_sites SET system_type = 'new-api'
                   WHERE LOWER(TRIM(system_type)) IN ('newapi', 'new api', 'new_api', 'new-api',
                                                      'vo-api', 'voapi', 'super-api', 'superapi',
                                                      'rix-api', 'rixapi', 'neo-api', 'neoapi',
                                                      'wonggongyi');
                 UPDATE directory_sites SET system_type = 'one-api'
                   WHERE LOWER(TRIM(system_type)) IN ('oneapi', 'one api', 'one_api', 'one-api');
                 UPDATE directory_sites SET system_type = 'one-hub'
                   WHERE LOWER(TRIM(system_type)) IN ('onehub', 'one hub', 'one-hub');
                 UPDATE directory_sites SET system_type = 'done-hub'
                   WHERE LOWER(TRIM(system_type)) IN ('donehub', 'done hub', 'done-hub');
                 UPDATE directory_sites SET system_type = 'sub2api'
                   WHERE LOWER(TRIM(system_type)) IN ('sub 2api', 'sub2 api', 'sub2api');
                 UPDATE directory_sites SET system_type = 'claude'
                   WHERE LOWER(TRIM(system_type)) = 'anthropic';
                 UPDATE directory_sites SET system_type = 'cliproxyapi'
                   WHERE LOWER(TRIM(system_type)) IN ('cpa', 'cli-proxy-api', 'cliproxapi');
                 UPDATE directory_sites SET system_type = 'antigravity'
                   WHERE LOWER(TRIM(system_type)) = 'anti-gravity';
                 UPDATE directory_sites SET system_type = 'codex'
                   WHERE LOWER(TRIM(system_type)) IN ('chatgpt-codex', 'chatgpt codex');
                 UPDATE directory_sites SET system_type = 'gemini'
                   WHERE LOWER(TRIM(system_type)) = 'google';",
            )
            .map_err(|error| error.to_string())?;
        Ok(Self(std::sync::Mutex::new(connection)))
    }
}

const TOKEN_USAGE_SNAPSHOT: &str = "usage";
const TOKEN_SESSIONS_SNAPSHOT: &str = "sessions";
const TOKEN_HEALTH_SNAPSHOT: &str = "health";

fn serialize_snapshot<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|error| format!("Token 数据序列化失败：{error}"))
}

fn read_snapshot<T: DeserializeOwned>(
    database: &Database,
    kind: &str,
) -> Result<Option<T>, String> {
    let payload = {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        connection
            .query_row(
                "SELECT payload_json FROM token_cache_snapshots WHERE kind = ?1",
                [kind],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
    };
    payload
        .map(|payload| {
            serde_json::from_str(&payload)
                .map_err(|error| format!("Token 数据库快照解析失败（{kind}）：{error}"))
        })
        .transpose()
}

pub(crate) fn has_token_snapshots(database: &Database) -> Result<bool, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM token_cache_snapshots WHERE kind = ?1)",
            [TOKEN_USAGE_SNAPSHOT],
            |row| row.get::<_, i64>(0),
        )
        .map(|value| value != 0)
        .map_err(|error| error.to_string())
}

pub(crate) fn clear_token_snapshots(database: &Database) -> Result<usize, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    connection
        .execute("DELETE FROM token_cache_snapshots", [])
        .map_err(|error| error.to_string())
}

pub(crate) fn write_token_snapshots(
    database: &Database,
    usage: &TokenUsageReport,
    sessions: &[TokenSession],
    health: &RequestHealthReport,
) -> Result<(), String> {
    let payloads = [
        (TOKEN_USAGE_SNAPSHOT, serialize_snapshot(usage)?),
        (TOKEN_SESSIONS_SNAPSHOT, serialize_snapshot(&sessions)?),
        (TOKEN_HEALTH_SNAPSHOT, serialize_snapshot(health)?),
    ];
    let mut connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    for (kind, payload) in payloads {
        transaction
            .execute(
                "INSERT INTO token_cache_snapshots (kind, payload_json, updated_at)
                 VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
                 ON CONFLICT(kind) DO UPDATE SET
                    payload_json = excluded.payload_json,
                    updated_at = excluded.updated_at",
                params![kind, payload],
            )
            .map_err(|error| error.to_string())?;
    }
    transaction.commit().map_err(|error| error.to_string())
}

pub(crate) fn read_token_usage_snapshot(
    database: &Database,
) -> Result<Option<TokenUsageReport>, String> {
    read_snapshot(database, TOKEN_USAGE_SNAPSHOT)
}

pub(crate) fn read_token_sessions_snapshot(
    database: &Database,
) -> Result<Option<Vec<TokenSession>>, String> {
    read_snapshot(database, TOKEN_SESSIONS_SNAPSHOT)
}

pub(crate) fn read_token_health_snapshot(
    database: &Database,
) -> Result<Option<RequestHealthReport>, String> {
    read_snapshot(database, TOKEN_HEALTH_SNAPSHOT)
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

/// 通道账号从“站点 + 账号”级升级为账号级：一个 Chrome 账号只能归属一个通道。
/// 旧表按 (site_id, profile_id) 去重时保留每个账号最近一次分配的通道。
pub(crate) fn migrate_account_proxy_channels_to_profile(
    connection: &Connection,
) -> Result<(), String> {
    let has_site_id: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('account_proxy_channels') WHERE name = 'site_id'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if has_site_id == 0 {
        return Ok(());
    }
    connection
        .execute_batch(
            "DROP INDEX IF EXISTS idx_account_proxy_channels_channel;
             ALTER TABLE account_proxy_channels RENAME TO account_proxy_channels_legacy;
             CREATE TABLE account_proxy_channels (
                 profile_id TEXT PRIMARY KEY,
                 channel_id TEXT NOT NULL,
                 updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                 FOREIGN KEY(channel_id) REFERENCES proxy_channels(id) ON DELETE CASCADE
             );
             INSERT OR IGNORE INTO account_proxy_channels (profile_id, channel_id, updated_at)
                 SELECT legacy.profile_id, legacy.channel_id, legacy.updated_at
                 FROM account_proxy_channels_legacy AS legacy
                 WHERE legacy.updated_at = (
                     SELECT MAX(inner_row.updated_at)
                     FROM account_proxy_channels_legacy AS inner_row
                     WHERE inner_row.profile_id = legacy.profile_id
                 );
             DROP TABLE account_proxy_channels_legacy;
             CREATE INDEX IF NOT EXISTS idx_account_proxy_channels_channel
                 ON account_proxy_channels(channel_id);",
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
        // 浏览器兜底失败的持久化冷却：failed_at 为 unix 毫秒，fail_count 支撑指数退避。
        // 之前冷却只存在前端内存里，应用重启即丢失，自动同步会反复拉起后台标签页。
        ("browser_fallback_failed_at", "INTEGER NOT NULL DEFAULT 0"),
        ("browser_fallback_fail_count", "INTEGER NOT NULL DEFAULT 0"),
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
    // 自动修复历史残留的 Chrome 默认占位名（'您的 Chrome' / 'Default' / '个人资料 1'）
    let _ = connection.execute(
        "UPDATE site_accounts
         SET profile_name = CASE
             WHEN account_name != '' THEN account_name
             WHEN username != '' THEN username
             ELSE profile_name
         END
         WHERE profile_name IN ('您的 Chrome', 'Default', '个人资料 1', 'Person 1', 'Profile 1')",
        [],
    );
    Ok(())
}

pub(crate) fn ensure_site_model_cache_columns(connection: &Connection) -> Result<(), String> {
    let exists = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('site_model_cache') WHERE name = 'groups_json'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| error.to_string())?
        > 0;
    if !exists {
        connection
            .execute(
                "ALTER TABLE site_model_cache ADD COLUMN groups_json TEXT NOT NULL DEFAULT '{}'",
                [],
            )
            .map_err(|error| error.to_string())?;
    }
    let has_key_models = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('site_model_cache') WHERE name = 'key_models_json'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| error.to_string())?
        > 0;
    if !has_key_models {
        connection
            .execute(
                "ALTER TABLE site_model_cache ADD COLUMN key_models_json TEXT NOT NULL DEFAULT '{}'",
                [],
            )
            .map_err(|error| error.to_string())?;
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

pub(crate) fn ensure_opencode_proxy_logs_table(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS opencode_proxy_logs (
                id TEXT PRIMARY KEY,
                timestamp TEXT NOT NULL,
                method TEXT NOT NULL,
                path TEXT NOT NULL,
                channel_id TEXT NOT NULL,
                model TEXT NOT NULL,
                stream INTEGER NOT NULL,
                status_code INTEGER NOT NULL,
                duration_ms INTEGER NOT NULL,
                ttft_ms INTEGER,
                prompt_tokens INTEGER,
                prompt_cache_hit_tokens INTEGER,
                prompt_cache_miss_tokens INTEGER,
                completion_tokens INTEGER,
                reasoning_tokens INTEGER,
                total_tokens INTEGER,
                error_message TEXT,
                request_body TEXT,
                response_body TEXT,
                node_name TEXT,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_opencode_proxy_logs_created ON opencode_proxy_logs(created_at DESC);",
        )
        .map_err(|e| e.to_string())?;

    let has_node_name: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('opencode_proxy_logs') WHERE name='node_name'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if has_node_name == 0 {
        let _ = connection.execute("ALTER TABLE opencode_proxy_logs ADD COLUMN node_name TEXT", []);
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
                    sa.browser_fallback_failed_at, sa.browser_fallback_fail_count,
                    CASE WHEN smc.profile_id IS NULL THEN 0 ELSE 1 END,
                    CASE 
                        WHEN (smc.keys_json IS NOT NULL AND smc.keys_json NOT IN ('', '[]'))
                          OR (smc.models_json IS NOT NULL AND smc.models_json NOT IN ('', '[]'))
                        THEN ''
                        ELSE COALESCE(smc.error, '')
                    END
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
            let browser_fallback_failed_at = row.get::<_, i64>(22)?;
            let browser_fallback_fail_count = row.get::<_, i64>(23)?;
            Ok((
                row.get::<_, String>(0)?,
                chrome_session::ChromeSessionInfo {
                    profile_id: row.get(1)?,
                    domain: row.get(2)?,
                    cookie_count: row.get::<_, i64>(3)?.max(0) as usize,
                    cookie_names: serde_json::from_str(&cookie_names_json).unwrap_or_default(),
                    profile_name: {
                        let raw: String = row.get(5)?;
                        let account: String = row.get(6)?;
                        if (raw == "您的 Chrome" || raw == "Default" || raw.starts_with("个人资料")) && !account.is_empty() {
                            account
                        } else {
                            raw
                        }
                    },
                    account_name: row.get(6)?,
                    username: row.get(7)?,
                    api_key_count: row.get::<_, i64>(8)?.max(0) as usize,
                    api_model_count: row.get::<_, i64>(9)?.max(0) as usize,
                    api_counts_synced: row.get::<_, i64>(24)? != 0,
                    api_sync_error: row.get(25)?,
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
                    browser_fallback_cooldown_ms:
                        crate::account_sync::browser_fallback_cooldown_remaining_ms(
                            browser_fallback_failed_at,
                            browser_fallback_fail_count,
                        ),
                    newapi_token,
                    browser_fallback_failed_at,
                    browser_fallback_fail_count,
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
            has_pending_report, is_personal, is_pending, use_system_proxy, use_proxy_pool, favorite, hidden, created_at, updated_at
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7,
            ?8, ?9, ?10, ?11,
            ?12, ?13, ?14, ?15, ?16,
            ?17, ?18, ?19, ?20,
            ?21, ?22, ?23, ?24, ?25, ?26, ?27,
            COALESCE(NULLIF(?28, ''), CURRENT_TIMESTAMP), COALESCE(NULLIF(?28, ''), CURRENT_TIMESTAMP)
        )",
        params![
            site.id, site.name, site.description, site.registration_limit, site.icon, site.api_base_url, site.system_type,
            site.supports_immersive_translation, site.supports_ldc, site.supports_checkin, site.supports_nsfw,
            site.checkin_url, site.checkin_note, site.benefit_url, site.rate_limit, site.status_url,
            site.is_only_maintainer_visible, site.requires_invite_code, site.is_runaway, site.is_fake_charity,
            site.has_pending_report, site.is_personal, site.is_pending, site.use_system_proxy, site.use_proxy_pool, site.favorite, site.hidden, site.updated_at
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
        ("channel_latency_ms", "INTEGER"),
        ("channel_test_status", "TEXT NOT NULL DEFAULT ''"),
        ("channel_tested_at", "TEXT NOT NULL DEFAULT ''"),
        ("is_enabled", "INTEGER NOT NULL DEFAULT 1"),
        ("created_at", "TEXT NOT NULL DEFAULT ''"),
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
             END,
             created_at = CASE
                WHEN created_at IS NULL OR TRIM(created_at) = '' THEN CURRENT_TIMESTAMP
                ELSE created_at
             END",
            [],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn ensure_charity_feed_sources_table(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS charity_feed_sources (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                json_url TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                sort_order INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
            );",
        )
        .map_err(|error| error.to_string())?;
    // 仅在表为空时插入默认标签
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM charity_feed_sources", [], |row| {
            row.get(0)
        })
        .map_err(|error| error.to_string())?;
    if count == 0 {
        connection
            .execute_batch(
                "INSERT INTO charity_feed_sources (id, name, json_url, sort_order) VALUES
                    ('1515', '公益推广', 'https://linux.do/tag/1515-tag/1515.json?order=created&ascending=false', 1),
                    ('1980', '公益站',   'https://linux.do/tag/1980-tag/1980.json?order=created&ascending=false', 2),
                    ('2233', '中转站',   'https://linux.do/tag/2233-tag/2233.json?order=created&ascending=false', 3),
                    ('2234', '开源推广', 'https://linux.do/tag/2234-tag/2234.json?order=created&ascending=false', 4),
                    ('1514', '高级推广', 'https://linux.do/tag/1514-tag/1514.json?order=created&ascending=false', 5),
                    ('193',  '订阅节点', 'https://linux.do/tag/193-tag/193.json?order=created&ascending=false', 6)
                ;",
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod token_snapshot_tests {
    use super::*;
    use serde_json::json;

    fn test_database() -> Database {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE token_cache_snapshots (
                    kind TEXT PRIMARY KEY,
                    payload_json TEXT NOT NULL,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );",
            )
            .unwrap();
        Database(std::sync::Mutex::new(connection))
    }

    #[test]
    fn token_snapshots_round_trip_atomically() {
        let database = test_database();
        let usage = TokenUsageReport {
            available: true,
            buckets: vec![TokenUsageBucket {
                source: "antigravity".to_string(),
                model: "gemini-pro-default".to_string(),
                project_key: "OpenHub".to_string(),
                timestamp: "2026-08-12T01:00:00.000Z".to_string(),
                total_tokens: 123,
                estimated_tokens: 123,
                ..Default::default()
            }],
            start_date: "2026-08-12".to_string(),
            end_date: "2026-08-12".to_string(),
            pricing_source: "openhub-local-no-pricing".to_string(),
        };
        let sessions = vec![TokenSession {
            version: 1,
            session_hash: "openhub:antigravity:test".to_string(),
            source: "antigravity".to_string(),
            project_key: "OpenHub".to_string(),
            model: "gemini-pro-default".to_string(),
            started_at: "2026-08-12T01:00:00.000Z".to_string(),
            ended_at: "2026-08-12T01:01:00.000Z".to_string(),
            turns: 1,
            total_tokens: 123,
            provenance: json!({"tokenUsage":"estimated-antigravity-local-context"}),
            ..Default::default()
        }];
        let health = RequestHealthReport {
            available: true,
            buckets: vec![RequestHealthBucket {
                hour: "2026-08-12T01:00:00.000Z".to_string(),
                dialogues: 1,
                requests: 2,
                success: 2,
                failed: 0,
            }],
            by_source: vec![RequestHealthSourceSummary {
                source: "antigravity".to_string(),
                dialogues: 1,
                requests: 2,
                success: 2,
                failed: 0,
            }],
        };

        write_token_snapshots(&database, &usage, &sessions, &health).unwrap();
        assert!(has_token_snapshots(&database).unwrap());
        assert_eq!(
            read_token_usage_snapshot(&database)
                .unwrap()
                .unwrap()
                .buckets[0]
                .total_tokens,
            123
        );
        assert_eq!(
            read_token_sessions_snapshot(&database).unwrap().unwrap()[0].source,
            "antigravity"
        );
        assert_eq!(
            read_token_health_snapshot(&database)
                .unwrap()
                .unwrap()
                .buckets[0]
                .requests,
            2
        );
    }

    #[test]
    fn clear_token_snapshots_removes_all_cached_reports() {
        let database = test_database();
        write_token_snapshots(
            &database,
            &TokenUsageReport::default(),
            &[],
            &RequestHealthReport::default(),
        )
        .unwrap();

        assert_eq!(clear_token_snapshots(&database).unwrap(), 3);
        assert!(!has_token_snapshots(&database).unwrap());
        assert!(read_token_usage_snapshot(&database).unwrap().is_none());
        assert!(read_token_sessions_snapshot(&database).unwrap().is_none());
        assert!(read_token_health_snapshot(&database).unwrap().is_none());
    }

    #[test]
    fn empty_token_database_returns_none_without_scanning() {
        let database = test_database();
        assert!(!has_token_snapshots(&database).unwrap());
        assert!(read_token_usage_snapshot(&database).unwrap().is_none());
        assert!(read_token_sessions_snapshot(&database).unwrap().is_none());
        assert!(read_token_health_snapshot(&database).unwrap().is_none());
    }
}

#[cfg(test)]
mod account_proxy_channel_tests {
    use super::*;

    fn legacy_database() -> Database {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE proxy_channels (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    node_id TEXT NOT NULL DEFAULT '',
                    test_url TEXT NOT NULL DEFAULT '',
                    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                 CREATE TABLE account_proxy_channels (
                    site_id TEXT NOT NULL,
                    profile_id TEXT NOT NULL,
                    channel_id TEXT NOT NULL,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    PRIMARY KEY (site_id, profile_id),
                    FOREIGN KEY(channel_id) REFERENCES proxy_channels(id) ON DELETE CASCADE
                );
                 INSERT INTO proxy_channels (id, name) VALUES ('default', '默认通道'), ('hk', '香港');
                 INSERT INTO account_proxy_channels (site_id, profile_id, channel_id, updated_at)
                 VALUES
                    ('site-a', 'profile-1', 'hk', '2026-08-15T10:00:00Z'),
                    ('site-b', 'profile-1', 'default', '2026-08-15T11:00:00Z'),
                    ('site-a', 'profile-2', 'hk', '2026-08-15T10:00:00Z');",
            )
            .unwrap();
        Database(std::sync::Mutex::new(connection))
    }

    #[test]
    fn legacy_account_rows_collapse_to_one_channel_per_profile() {
        let database = legacy_database();
        migrate_account_proxy_channels_to_profile(&database.0.lock().unwrap()).unwrap();

        let connection = database.0.lock().unwrap();
        let has_site_id: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('account_proxy_channels') WHERE name = 'site_id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(has_site_id, 0);
        let mut statement = connection
            .prepare(
                "SELECT profile_id, channel_id FROM account_proxy_channels ORDER BY profile_id",
            )
            .unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("profile-1".to_string(), "default".to_string()),
                ("profile-2".to_string(), "hk".to_string()),
            ]
        );
    }

    #[test]
    fn migration_is_idempotent_on_new_schema() {
        let database = legacy_database();
        migrate_account_proxy_channels_to_profile(&database.0.lock().unwrap()).unwrap();
        migrate_account_proxy_channels_to_profile(&database.0.lock().unwrap()).unwrap();

        let connection = database.0.lock().unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM account_proxy_channels", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn ensures_is_enabled_column_added_to_legacy_proxy_pool_nodes() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE proxy_pool_nodes (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL
                );",
            )
            .unwrap();

        ensure_proxy_pool_node_columns(&connection).unwrap();

        let has_is_enabled: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('proxy_pool_nodes') WHERE name = 'is_enabled'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(has_is_enabled, 1);

        let has_created_at: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('proxy_pool_nodes') WHERE name = 'created_at'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(has_created_at, 1);
    }
}
