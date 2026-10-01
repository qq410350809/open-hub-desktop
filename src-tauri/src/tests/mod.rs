use crate::proxypool::*;
use crate::*;
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};

#[test]
fn sites_default_to_direct_network_access() {
    assert!(!SiteRecord::default().use_system_proxy);
}

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
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    newapi_token TEXT NOT NULL DEFAULT '',
                    newapi_user_id TEXT NOT NULL DEFAULT '',
                    browser_fallback_failed_at INTEGER NOT NULL DEFAULT 0,
                    browser_fallback_fail_count INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE site_model_cache (
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
                    PRIMARY KEY (site_id, profile_id)
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
    connection
        .execute(
            "INSERT INTO site_model_cache (site_id, profile_id, error)
                 VALUES ('site-b', 'Default', '')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE site_accounts SET newapi_token = 'secret-access-token'
                 WHERE site_id = 'site-b' AND profile_id = 'Default'",
            [],
        )
        .unwrap();

    let cached = read_cached_usage_sites(&connection).unwrap();
    assert_eq!(cached.len(), 2);
    assert_eq!(cached[0].site_id, "site-a");
    assert_eq!(cached[0].sessions.len(), 2);
    assert_eq!(cached[0].sessions[0].profile_id, "Default");
    assert_eq!(cached[0].sessions[0].cookie_names, ["session", "token"]);
    assert_eq!(cached[0].sessions[0].api_key_count, 0);
    assert_eq!(cached[0].sessions[0].api_model_count, 0);
    assert!(!cached[0].sessions[0].api_counts_synced);
    assert_eq!(cached[1].site_id, "site-b");
    assert!(cached[1].sessions[0].api_counts_synced);
    assert!(cached[1].sessions[0].has_access_token);
    let serialized = serde_json::to_value(&cached[1].sessions[0]).unwrap();
    assert_eq!(
        serialized
            .get("hasAccessToken")
            .and_then(|value| value.as_bool()),
        Some(true)
    );
    assert!(serialized.get("newapiToken").is_none());
    assert!(!serialized.to_string().contains("secret-access-token"));
    assert_eq!(cached[1].sessions[0].cookie_count, 3);
}

#[test]
fn cached_usage_sites_exclude_site_level_key_cache_rows() {
    // site_model_cache 里 profile_id='' 的行是站点级 Key 缓存（无 Chrome 账号
    // 时拉取/手动管理的 Key），不是会话：必须从 usageSites 中剔除，否则前端
    // 会渲染出只有时间戳的幽灵账号行。read_cached_usage_sites 依赖的表结构
    // 与外键用最小 schema 复刻即可。
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE directory_sites (
                    id TEXT PRIMARY KEY,
                    is_runaway INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE site_accounts (
                    site_id TEXT NOT NULL,
                    profile_id TEXT NOT NULL,
                    domain TEXT NOT NULL DEFAULT '',
                    cookie_count INTEGER NOT NULL DEFAULT 0,
                    cookie_names TEXT NOT NULL DEFAULT '[]',
                    profile_name TEXT NOT NULL DEFAULT '',
                    account_name TEXT NOT NULL DEFAULT '',
                    username TEXT NOT NULL DEFAULT '',
                    api_key_count INTEGER NOT NULL DEFAULT 0,
                    api_model_count INTEGER NOT NULL DEFAULT 0,
                    remaining REAL,
                    used REAL,
                    total REAL,
                    unit TEXT NOT NULL DEFAULT '',
                    is_valid INTEGER NOT NULL DEFAULT 1,
                    sync_error TEXT NOT NULL DEFAULT '',
                    checkin_enabled INTEGER NOT NULL DEFAULT 0,
                    checked_in_today INTEGER NOT NULL DEFAULT 0,
                    checkin_date TEXT NOT NULL DEFAULT '',
                    checkin_error TEXT NOT NULL DEFAULT '',
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    newapi_token TEXT NOT NULL DEFAULT '',
                    newapi_user_id TEXT NOT NULL DEFAULT '',
                    browser_fallback_failed_at INTEGER NOT NULL DEFAULT 0,
                    browser_fallback_fail_count INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (site_id, profile_id)
                 );
                 CREATE TABLE site_model_cache (
                    site_id TEXT NOT NULL,
                    profile_id TEXT NOT NULL,
                    profile_name TEXT NOT NULL DEFAULT '',
                    account_name TEXT NOT NULL DEFAULT '',
                    username TEXT NOT NULL DEFAULT '',
                    api_source TEXT NOT NULL DEFAULT '',
                    keys_json TEXT NOT NULL DEFAULT '[]',
                    groups_json TEXT NOT NULL DEFAULT '{}',
                    models_json TEXT NOT NULL DEFAULT '[]',
                    key_models_json TEXT NOT NULL DEFAULT '{}',
                    error TEXT NOT NULL DEFAULT '',
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    PRIMARY KEY (site_id, profile_id)
                 );
                 INSERT INTO directory_sites (id) VALUES ('site-a');
                 INSERT INTO site_accounts (site_id, profile_id, domain, cookie_count, cookie_names, profile_name, account_name)
                    VALUES ('site-a', 'Profile 11', 'a.example', 1, '[\"session\"]', 'Profile 11', 'a@example.com');
                 INSERT INTO site_model_cache (site_id, profile_id, keys_json)
                    VALUES ('site-a', '', '[\"sk-site-level\"]');",
        )
        .unwrap();

    let cached = read_cached_usage_sites(&connection).unwrap();
    assert_eq!(cached.len(), 1, "站点级缓存行不得成为独立会话");
    assert_eq!(cached[0].site_id, "site-a");
    assert_eq!(cached[0].sessions.len(), 1);
    assert_eq!(cached[0].sessions[0].profile_id, "Profile 11");
}

#[test]
fn manual_key_add_creates_row_for_scanned_account() {
    // 回归保护：会话已扫描进 site_accounts 但还没同步过 Key 的账号，手动
    // 添加 Key 必须能建行（此前一律报「目标账号不存在：请先同步会话…」），
    // 并从账号表回填展示名；未扫描到的 profile_id 仍拒绝凭空造行，避免
    // read_cached_usage_sites 的缓存行 UNION 分支渲染出不存在的账号。
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE site_accounts (
                    site_id TEXT NOT NULL,
                    profile_id TEXT NOT NULL,
                    profile_name TEXT NOT NULL DEFAULT '',
                    account_name TEXT NOT NULL DEFAULT '',
                    username TEXT NOT NULL DEFAULT '',
                    PRIMARY KEY (site_id, profile_id)
                 );
                 CREATE TABLE site_model_cache (
                    site_id TEXT NOT NULL,
                    profile_id TEXT NOT NULL,
                    profile_name TEXT NOT NULL DEFAULT '',
                    account_name TEXT NOT NULL DEFAULT '',
                    username TEXT NOT NULL DEFAULT '',
                    api_source TEXT NOT NULL DEFAULT '',
                    keys_json TEXT NOT NULL DEFAULT '[]',
                    groups_json TEXT NOT NULL DEFAULT '{}',
                    models_json TEXT NOT NULL DEFAULT '[]',
                    key_models_json TEXT NOT NULL DEFAULT '{}',
                    health_json TEXT NOT NULL DEFAULT '{}',
                    error TEXT NOT NULL DEFAULT '',
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    PRIMARY KEY (site_id, profile_id)
                 );
                 INSERT INTO site_accounts (site_id, profile_id, profile_name, account_name, username)
                    VALUES ('site-ai', 'Profile 11', 'Profile 11', 'a@example.com', 'alice');",
        )
        .unwrap();
    let database = Database(std::sync::Mutex::new(connection));

    let added =
        add_site_model_cache_key_inner(&database, "site-ai", "Profile 11", "  sk-new  ", "", "", "")
            .unwrap();
    assert!(added, "已扫描账号应允许直接建行添加 Key");
    let row = database
        .lock_conn()
        .unwrap()
        .query_row(
            "SELECT keys_json, groups_json, profile_name, account_name, username, error
             FROM site_model_cache WHERE site_id = 'site-ai' AND profile_id = 'Profile 11'",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(row.0, r#"["sk-new"]"#, "Key 应去空白后落库");
    assert_eq!(row.1, r#"{"sk-new":"默认分组"}"#, "空分组应落默认分组");
    assert_eq!(row.2, "Profile 11", "展示名应从 site_accounts 回填");
    assert_eq!(row.3, "a@example.com");
    assert_eq!(row.4, "alice");
    assert_eq!(row.5, "", "手动建行不应带错误状态");

    // 重复添加同一 Key 返回 false（不报错、不重复入库）。
    let again =
        add_site_model_cache_key_inner(&database, "site-ai", "Profile 11", "sk-new", "g", "", "")
            .unwrap();
    assert!(!again);

    // 未扫描到的 profile_id 仍报「目标账号不存在」，且不落行。
    let unknown =
        add_site_model_cache_key_inner(&database, "site-ai", "Profile 99", "sk-x", "", "", "")
            .expect_err("未扫描到的账号应拒绝建行");
    assert!(
        unknown.contains("目标账号不存在"),
        "未知账号应报目标账号不存在，实际 {:?}",
        unknown
    );
    let ghost: i64 = database
        .lock_conn()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM site_model_cache WHERE profile_id = 'Profile 99'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ghost, 0, "未知账号不得凭空建行");

    // 站点级（profile_id 为空）保持原行为：允许自动建行。
    let site_level =
        add_site_model_cache_key_inner(&database, "site-ai", "", "sk-site", "", "", "").unwrap();
    assert!(site_level);
}

#[test]
fn resets_stale_checkin_state_when_local_date_changes() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute(
            "CREATE TABLE site_accounts (
                    checked_in_today INTEGER NOT NULL DEFAULT 0,
                    checkin_error TEXT NOT NULL DEFAULT '',
                    checkin_date TEXT NOT NULL DEFAULT ''
                )",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO site_accounts (checked_in_today, checkin_error, checkin_date)
                 VALUES (1, '昨天的签到错误', date('now', 'localtime', '-1 day'))",
            [],
        )
        .unwrap();

    assert_eq!(reset_expired_checkin_states(&connection).unwrap(), 1);
    let state: (i64, String, String) = connection
        .query_row(
            "SELECT checked_in_today, checkin_error, checkin_date FROM site_accounts",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(state.0, 0);
    assert!(state.1.is_empty());
    assert_eq!(
        state.2,
        connection
            .query_row("SELECT date('now', 'localtime')", [], |row| row
                .get::<_, String>(0))
            .unwrap()
    );
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
        key_groups: HashMap::new(),
        key_models: HashMap::new(),
        errors: Vec::new(),
        profile_id: String::new(),
        model_health: HashMap::new(),
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
fn sync_failure_error_is_persisted_to_model_cache() {
    // 回归保护：同步失败收集的 errors 必须写进 site_model_cache.error，
    // 即便调用方只带了 account.error 空串。目前 key 同步失败后界面只显示
    // "0 个 Key"，看不到失败原因，就是这段阵地空转导致的。
    let connection = Connection::open_in_memory().unwrap();
    // schema 与真实库对齐（site_accounts 没有 api_sync_error 列，卡片的
    // api_sync_error 展示值由 site_model_cache.error JOIN 计算得出）：
    // 若清理语句再写入不存在的列，整条 UPDATE 会失败，下方 sync_error
    // 清空断言就会失败，正好防住这类静默 bug。
    connection
        .execute_batch(
            "CREATE TABLE site_accounts (
                    site_id TEXT NOT NULL,
                    profile_id TEXT NOT NULL,
                    is_valid INTEGER NOT NULL DEFAULT 0,
                    sync_error TEXT NOT NULL DEFAULT ''
                 );
                 CREATE TABLE site_model_cache (
                    site_id TEXT NOT NULL,
                    profile_id TEXT NOT NULL,
                    profile_name TEXT NOT NULL DEFAULT '',
                    account_name TEXT NOT NULL DEFAULT '',
                    username TEXT NOT NULL DEFAULT '',
                    api_source TEXT NOT NULL DEFAULT '',
                    keys_json TEXT NOT NULL DEFAULT '[]',
                    groups_json TEXT NOT NULL DEFAULT '{}',
                    models_json TEXT NOT NULL DEFAULT '[]',
                    key_models_json TEXT NOT NULL DEFAULT '{}',
                    health_json TEXT NOT NULL DEFAULT '{}',
                    error TEXT NOT NULL DEFAULT '',
                    updated_at TEXT NOT NULL DEFAULT '',
                    PRIMARY KEY (site_id, profile_id)
                 );",
        )
        .unwrap();
    let database = Database(std::sync::Mutex::new(connection));

    let empty_result = SiteModelsResult {
        models: vec![],
        source: "newapi-key".into(),
        keys: vec![],
        key_groups: HashMap::new(),
        key_models: HashMap::new(),
        errors: vec!["Profile 11：Sub2API Key 接口请求失败".into()],
        profile_id: String::new(),
        model_health: HashMap::new(),
    };
    let account = SiteModelCacheAccount {
        profile_id: "Profile 11".into(),
        profile_name: "吴锁明".into(),
        account_name: "wusuoming@gmail.com".into(),
        username: "wusuoming".into(),
        keys: vec![],
        key_groups: HashMap::new(),
        key_models: HashMap::new(),
        model_health: HashMap::new(),
        error: "".into(),
    };
    save_site_model_cache(&database, "site-ai", &account, Some(&empty_result), false).unwrap();
    let saved: String = database
        .0
        .lock()
        .unwrap()
        .query_row(
            "SELECT error FROM site_model_cache WHERE site_id='site-ai' AND profile_id='Profile 11'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        saved.contains("Sub2API Key 接口请求失败"),
        "同步失败原因未落库，实际 error: {saved}"
    );

    // 有 Key 时错误不应落库（上次失败后本次恢复 → 错误清干净）
    let ok_result = SiteModelsResult {
        models: vec![SiteModelItem {
            id: "m1".into(),
            owned_by: None,
        }],
        source: "newapi-key".into(),
        keys: vec!["sk-live".into()],
        key_groups: HashMap::new(),
        key_models: HashMap::new(),
        errors: vec![],
        profile_id: String::new(),
        model_health: HashMap::new(),
    };
    // 先预置一条不含旧关键词（NewAPI/权限不足/失效）的历史账号同步错误，
    // 回归保护：成功同步后必须无条件清掉，卡片才不会一直挂着「账号信息同步失败」。
    database
        .0
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO site_accounts (site_id, profile_id, is_valid, sync_error)
             VALUES ('site-ai', 'Profile 11', 0, '账号同步超过 90 秒，已强制终止')",
            [],
        )
        .unwrap();
    let mut account_ok = account.clone();
    account_ok.keys = vec!["sk-live".into()];
    save_site_model_cache(&database, "site-ai", &account_ok, Some(&ok_result), false).unwrap();
    let saved2: String = database
        .0
        .lock()
        .unwrap()
        .query_row(
            "SELECT error FROM site_model_cache WHERE site_id='site-ai' AND profile_id='Profile 11'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(saved2.is_empty(), "成功时遗留错误未清：{saved2}");
    let sync_error: String = database
        .0
        .lock()
        .unwrap()
        .query_row(
            "SELECT sync_error FROM site_accounts WHERE site_id='site-ai' AND profile_id='Profile 11'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        sync_error.is_empty(),
        "成功同步后 site_accounts 历史错误未清：sync_error={sync_error:?}"
    );
}

#[test]
fn save_site_model_cache_backfills_account_names() {
    // 回归保护：站点级同步（弹窗无有效会话分支）只带 profile_id、账号名全空。
    // 落库时必须从 site_accounts 回填该账号的展示名，否则缓存行与账号脱节，
    // Key 会显示成无名幽灵账号（黑与白公益站 Key 挂错账号的根因之一）。
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE site_accounts (
                    site_id TEXT NOT NULL,
                    profile_id TEXT NOT NULL,
                    is_valid INTEGER NOT NULL DEFAULT 0,
                    sync_error TEXT NOT NULL DEFAULT '',
                    profile_name TEXT NOT NULL DEFAULT '',
                    account_name TEXT NOT NULL DEFAULT '',
                    username TEXT NOT NULL DEFAULT ''
                 );
                 INSERT INTO site_accounts (site_id, profile_id, profile_name, account_name, username)
                 VALUES ('site-ai', 'Profile 11', '吴锁明', 'qq410350809@gmail.com', 'qq410350809@gmail.com');
                 CREATE TABLE site_model_cache (
                    site_id TEXT NOT NULL,
                    profile_id TEXT NOT NULL,
                    profile_name TEXT NOT NULL DEFAULT '',
                    account_name TEXT NOT NULL DEFAULT '',
                    username TEXT NOT NULL DEFAULT '',
                    api_source TEXT NOT NULL DEFAULT '',
                    keys_json TEXT NOT NULL DEFAULT '[]',
                    groups_json TEXT NOT NULL DEFAULT '{}',
                    models_json TEXT NOT NULL DEFAULT '[]',
                    key_models_json TEXT NOT NULL DEFAULT '{}',
                    health_json TEXT NOT NULL DEFAULT '{}',
                    error TEXT NOT NULL DEFAULT '',
                    updated_at TEXT NOT NULL DEFAULT '',
                    PRIMARY KEY (site_id, profile_id)
                 );",
        )
        .unwrap();
    let database = Database(std::sync::Mutex::new(connection));

    let result = SiteModelsResult {
        models: vec![],
        source: "newapi-key".into(),
        keys: vec!["sk-one".into()],
        key_groups: HashMap::new(),
        key_models: HashMap::new(),
        errors: Vec::new(),
        profile_id: "Profile 11".into(),
        model_health: HashMap::new(),
    };
    // 调用方只带 profile_id（与修复后的弹窗无会话分支一致）
    let account = SiteModelCacheAccount {
        profile_id: "Profile 11".into(),
        profile_name: String::new(),
        account_name: String::new(),
        username: String::new(),
        keys: vec!["sk-one".into()],
        key_groups: HashMap::new(),
        key_models: HashMap::new(),
        model_health: HashMap::new(),
        error: "".into(),
    };
    save_site_model_cache(&database, "site-ai", &account, Some(&result), false).unwrap();
    let (profile_name, account_name): (String, String) = database
        .0
        .lock()
        .unwrap()
        .query_row(
            "SELECT profile_name, account_name FROM site_model_cache
             WHERE site_id='site-ai' AND profile_id='Profile 11'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(profile_name, "吴锁明", "profile_name 未从账号表回填");
    assert_eq!(account_name, "qq410350809@gmail.com", "account_name 未从账号表回填");
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
    let cookie_names = vec!["new_api_refresh".to_string()];

    assert!(has_newapi_refresh_cookie_name(
        cookie_names.iter().map(String::as_str)
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
fn separates_newapi_cookie_and_refresh_auth_modes() {
    assert!(is_newapi("new-api"));
    assert!(!is_newapi_refresh("new-api"));
    assert!(is_newapi("newapi2"));
    assert!(is_newapi_refresh("newapi2"));
    assert!(is_newapi("anyrouter"));
    assert!(!is_newapi_refresh("anyrouter"));
    assert!(is_newapi("one-api"));
    assert!(is_newapi("one-hub"));
    assert!(is_newapi("done-hub"));
    assert!(is_newapi("veloera"));
}

#[test]
fn browser_session_evidence_accepts_any_cookie_or_local_key() {
    // 宽松判定：无 Cookie 且无 Local Storage 键才判“无会话”。
    assert!(!has_browser_session_evidence("new-api", None, 0));
    assert!(!has_browser_session_evidence(
        "new-api",
        Some(&HashMap::new()),
        0
    ));
    // 任意 Cookie（站点自定义会话名也算）即视为有会话。
    assert!(has_browser_session_evidence("new-api", None, 1));
    assert!(has_browser_session_evidence("sub2api", None, 3));
    // Local Storage 任意已知键（残缺账号数据，如仅 status）也算。
    let partial = HashMap::from([("status".to_string(), r#"{"ok":true}"#.to_string())]);
    assert!(has_browser_session_evidence("new-api", Some(&partial), 0));
    // 结构化账号数据依旧是会话证据。
    let valid = HashMap::from([("user".into(), r#"{"id":10288,"username":"wudixm"}"#.into())]);
    assert!(has_browser_session_evidence("NewAPI", Some(&valid), 0));
}

#[test]
fn refreshes_access_tokens_only_after_http_401() {
    assert!(access_token_was_rejected("账号接口 HTTP 401：访问令牌无效"));
    assert!(!access_token_was_rejected(
        "账号接口 HTTP 403：Cloudflare 安全验证"
    ));
    assert!(!access_token_was_rejected("账号接口请求失败：连接超时"));
    assert!(!access_token_was_rejected("账号接口返回的 JSON 无法解析"));
}

#[test]
fn recognizes_cloudflare_shield_errors() {
    let shield = "NewAPI Key 接口 HTTP 403 返回 HTML：Cloudflare 安全验证拦截了直接请求，请先用对应 Chrome 账号打开站点并通过验证";
    assert!(is_cloudflare_shield_error(shield));
    assert!(is_cloudflare_shield_error("接口返回 HTML：Cloudflare 拦截"));
    assert!(is_cloudflare_shield_error(
        "NewAPI Key 接口 HTTP 403 返回 HTML：站点返回了网页而不是 API 数据"
    ));
    assert!(is_cloudflare_shield_error(
        "Cloudflare 验证仍需要浏览器交互"
    ));
    // 令牌类 403 / 401 不算安全盾，应走 refresh 或错误收敛。
    assert!(!is_cloudflare_shield_error("账号接口 HTTP 403：无效的令牌"));
    assert!(!is_cloudflare_shield_error(
        "账号接口 HTTP 401：访问令牌无效"
    ));
    assert!(!is_cloudflare_shield_error("账号接口请求失败：连接超时"));
    // 能区分：盾错误不是令牌拒绝，反之亦然。
    assert!(!access_token_was_rejected(shield));
    assert!(!access_token_was_rejected(
        "NewAPI Key 接口 HTTP 403 返回 HTML：站点返回了网页而不是 API 数据"
    ));
}

#[test]
fn sub2api_consolidated_errors_are_recognized_as_auth_rejection() {
    // sub2api 分支合并判定：模型与 Key 接口都返回 401 时，两条错误
    // 都必须被 access_token_was_rejected 命中，才能收敛为一条精简提示。
    let direct = format!(
            "直接使用访问秘钥同步失败（Sub2API 模型接口 HTTP 401：Invalid API key{SUB2API_AUTH_FAILURE_HINT}），回落到 Key 接口"
        );
    let keys = format!("Sub2API Key 接口 HTTP 401：Token has expired{SUB2API_AUTH_FAILURE_HINT}");
    assert!(access_token_was_rejected(&direct));
    assert!(access_token_was_rejected(&keys));
    // 非认证失败（如模型列表为空）不触发收敛。
    assert!(!access_token_was_rejected("访问秘钥获取的模型列表为空"));
    // 提示文案明确是 Sub2API 登录令牌，而不是 NewAPI 的账号/访问令牌。
    assert!(SUB2API_AUTH_FAILURE_HINT.contains("auth_token"));
    assert!(!SUB2API_AUTH_FAILURE_HINT.contains("访问令牌"));
}

#[test]
fn translates_json_parse_errors_to_friendly_hints() {
    let err = serde_json::from_slice::<serde_json::Value>(b"true;").unwrap_err();
    let message = friendly_json_parse_error(&err, b"true;");
    assert!(message.contains("多余内容"), "{message}");
    assert!(message.contains("原文：true;"), "{message}");

    let err = serde_json::from_slice::<serde_json::Value>(b"hello").unwrap_err();
    let message = friendly_json_parse_error(&err, b"hello");
    assert!(message.contains("没有返回 JSON"), "{message}");
    assert!(message.contains("原文：hello"), "{message}");

    let err = serde_json::from_slice::<serde_json::Value>(b"{\"a\":").unwrap_err();
    let message = friendly_json_parse_error(&err, b"{\"a\":");
    assert!(message.contains("不完整"), "{message}");

    // 原文过长时只预览开头，避免日志被大段内容刷屏。
    let long = format!("{{\"a\":{}}}", "x".repeat(200));
    let err = serde_json::from_slice::<serde_json::Value>(long.as_bytes()).unwrap_err();
    let message = friendly_json_parse_error(&err, long.as_bytes());
    assert!(message.contains("原文：{"), "{message}");
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
            "true",
        ),
        (
            serde_json::json!({ "code": 0, "data": { "checked_in": false } }),
            "false",
        ),
        (
            serde_json::json!({ "success": true, "data": { "is_checked_in": 1 } }),
            "true",
        ),
        (
            serde_json::json!({ "success": true, "data": "not_checked_in" }),
            "false",
        ),
        // 字符串 code（部分发行版用 "SUCCESS"）不能被当成非 0 失败码。
        (
            serde_json::json!({ "code": "SUCCESS", "data": { "checked_in_today": true } }),
            "true",
        ),
        // 呆瓜式：只有 can_checkin / today_reward，没有显式「今日已签到」字段。
        (
            serde_json::json!({ "code": 0, "data": { "enabled": true, "can_checkin": false, "today_reward": 1.5 } }),
            "true",
        ),
        (
            serde_json::json!({ "code": 0, "data": { "enabled": true, "can_checkin": true, "today_reward": null } }),
            "false",
        ),
        // 不能签、无今日奖励、且带「不可签」信号：不能误判为已签到。
        (
            serde_json::json!({ "code": 0, "data": { "enabled": false, "can_checkin": false } }),
            "err",
        ),
        // 只有 today_reward（无 can_checkin）也视为已签到。
        (
            serde_json::json!({ "code": 0, "data": { "today_reward": 0.5 } }),
            "true",
        ),
    ] {
        match expected {
            "err" => assert!(
                parse_sub2api_checkin_status(&value).is_err(),
                "应无法确认今日签到状态：{value}"
            ),
            _ => assert_eq!(
                parse_sub2api_checkin_status(&value).unwrap(),
                expected == "true",
                "{value}"
            ),
        }
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
fn sub2api_checkin_fallback_preserves_same_day_checkin() {
    // 核心回归：状态刷新失败时，当天已签到的快照必须原样保留（且清掉陈旧错误），
    // 否则界面上「今日已签到」会被打回「今日未签到」。
    let previous = CheckinSnapshot {
        enabled: true,
        checked_in_today: true,
        error: String::new(),
    };
    let fallback = sub2api_checkin_fallback(previous, "HTTP 404".into());
    assert!(fallback.checked_in_today);
    assert!(fallback.enabled);
    assert!(fallback.error.is_empty());

    // 未签到时保留失败原因供界面展示「无法签到」。
    let fallback = sub2api_checkin_fallback(
        CheckinSnapshot {
            enabled: true,
            checked_in_today: false,
            error: String::new(),
        },
        "签到端点不存在".into(),
    );
    assert!(!fallback.checked_in_today);
    assert!(fallback.enabled);
    assert_eq!(fallback.error, "签到端点不存在");
}

#[test]
fn sub2api_checkin_probes_fork_specific_endpoint_candidates() {
    // 各发行版路径都必须在候选列表里：硬编码单一路由曾导致全站点 404。
    let status_paths: Vec<&str> = SUB2API_CHECKIN_ENDPOINTS
        .iter()
        .map(|(status, _)| *status)
        .collect();
    assert!(status_paths.contains(&"/api/v1/checkin/status"));
    assert!(status_paths.contains(&"/api/v1/check-in"));
    assert!(status_paths.contains(&"/api/checkin/status"));
    assert!(status_paths.contains(&"/api/v1/redeem/checkin/status"));
    // 每对候选的 action 路径非空。
    assert!(SUB2API_CHECKIN_ENDPOINTS
        .iter()
        .all(|(_, action)| !action.is_empty()));
}

#[test]
fn cross_host_checkin_probes_prefer_checkin_origin_token() {
    // Fengwind 型：签到在异主机（api-welfalre），令牌键是 welfare_token，
    // 主域 auth_token 在签到域会被拒（invalid token）。
    let probes = sub2api_checkin_probes(
        "https://api.fengwind.com/",
        "main-token",
        "https://api-welfalre.fengwind.com/",
        "welfare-token",
    );
    // 签到域 + 自己的令牌必须排第一。
    assert_eq!(
        probes[0],
        (
            "https://api-welfalre.fengwind.com/".to_string(),
            "welfare-token".to_string()
        )
    );
    // 签到域 + 主域令牌（可能互通的同后端双域名）与 主域 + 主域令牌 都保留。
    assert!(probes.contains(&(
        "https://api-welfalre.fengwind.com/".to_string(),
        "main-token".to_string()
    )));
    assert!(probes.contains(&(
        "https://api.fengwind.com/".to_string(),
        "main-token".to_string()
    )));
    assert_eq!(probes.len(), 3, "同 origin 同令牌不应重复：{probes:?}");

    // 常规同主机站点：只有 API 主域一条路径。
    let probes = sub2api_checkin_probes("https://api.example.com/", "tok", "", "");
    assert_eq!(probes.len(), 1);
    assert_eq!(probes[0].0, "https://api.example.com/");

    // 双双为空：返回空列表，上层回退保留旧状态。
    assert!(sub2api_checkin_probes("https://api.example.com/", "", "", "").is_empty());

    // 无 checkin_url 但签到域令牌存在：不得凭空造出跨主机候选。
    let probes = sub2api_checkin_probes("https://api.example.com/", "", "", "welfare");
    assert!(probes.is_empty());
}

#[test]
fn extracts_checkin_origin_token_per_fork_key() {
    // welfalre 系：welfare_token。
    let values = HashMap::from([("welfare_token".to_string(), " wt-1 ".to_string())]);
    assert_eq!(checkin_origin_token(&values).as_deref(), Some("wt-1"));
    // 标准 sub2api：auth_token。
    let values = HashMap::from([("auth_token".to_string(), "\"at-1\"".to_string())]);
    assert_eq!(checkin_origin_token(&values).as_deref(), Some("at-1"));
    // welfare_token 优先于 auth_token（签到域桶里两者都在时以签到令牌为准）。
    let values = HashMap::from([
        ("auth_token".to_string(), "at".to_string()),
        ("welfare_token".to_string(), "wt".to_string()),
    ]);
    assert_eq!(checkin_origin_token(&values).as_deref(), Some("wt"));
    // 空桶/空值：回退 None。
    assert!(checkin_origin_token(&HashMap::new()).is_none());
    let values = HashMap::from([("welfare_token".to_string(), "  ".to_string())]);
    assert!(checkin_origin_token(&values).is_none());
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
                { "key": "sk-newapi-enabled", "status": 1, "group": "vip" },
                { "key": "sk-newapi-disabled", "status": 0 },
                { "key": "sk-newapi-expired", "status": 1, "expired_time": 1 }
            ]
        }
    });
    assert_eq!(parse_api_keys(&newapi), ["sk-newapi-enabled"]);
    assert_eq!(
        parse_api_key_groups(&newapi).get("sk-newapi-enabled"),
        Some(&"vip".to_string())
    );

    let sub2api = serde_json::json!({
        "data": {
            "keys": [
                { "api_key": "sk-sub2api-enabled", "is_active": true },
                { "apiKey": "sk-sub2api-disabled", "is_active": false },
                { "secret_key": "raw-key-value", "key_prefix": "sub2-", "group_name": "pro" },
                { "key": "sk-****masked" },
                {
                    "api_key": "sk-sub2api-group-object",
                    "is_active": true,
                    "name": "我的 Key",
                    "group": { "id": 3, "name": "default" }
                }
            ]
        }
    });
    assert_eq!(
        parse_api_keys(&sub2api),
        ["raw-key-value", "sk-sub2api-enabled", "sk-sub2api-group-object", "sub2-raw-key-value"]
    );
    let sub2api_groups = parse_api_key_groups(&sub2api);
    assert_eq!(
        sub2api_groups.get("raw-key-value"),
        Some(&"pro".to_string())
    );
    assert_eq!(
        sub2api_groups.get("sub2-raw-key-value"),
        Some(&"pro".to_string())
    );
    // 分组标识取 group.name，而不是 Key 自身的 name
    assert_eq!(
        sub2api_groups.get("sk-sub2api-group-object"),
        Some(&"default".to_string())
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
        "sub2api"
    );

    let inferred = serde_json::json!({
        "checkinUrl": "https://example.com/console/personal"
    });
    assert_eq!(
        infer_remote_system_type(inferred.as_object().unwrap()),
        "new-api"
    );

    // 刷新令牌形态：isNewApi2 / is_newapi2 布尔标记应识别为 newapi2。
    let refresh_explicit = serde_json::json!({ "isNewApi2": true });
    assert_eq!(
        infer_remote_system_type(refresh_explicit.as_object().unwrap()),
        "newapi2"
    );
    let refresh_snake = serde_json::json!({ "is_newapi2": true });
    assert_eq!(
        infer_remote_system_type(refresh_snake.as_object().unwrap()),
        "newapi2"
    );
    // 刷新令牌优先于 Cookie 形态。
    let both = serde_json::json!({ "isNewApi": true, "is_newapi2": true });
    assert_eq!(
        infer_remote_system_type(both.as_object().unwrap()),
        "newapi2"
    );
    // 字符串值同样归一为 newapi2。
    let refresh_value = serde_json::json!({ "systemType": "newapi-refresh" });
    assert_eq!(
        infer_remote_system_type(refresh_value.as_object().unwrap()),
        "newapi2"
    );

    let unknown = serde_json::json!({ "apiBaseUrl": "https://example.com/" });
    assert!(infer_remote_system_type(unknown.as_object().unwrap()).is_empty());
}

#[test]
fn recognizes_high_confidence_system_type_url_hints() {
    assert_eq!(
        system_type_hint_from_url("https://sub2api.example.com/"),
        Some("sub2api")
    );
    assert_eq!(
        system_type_hint_from_url("https://newapi.example.com/"),
        Some("new-api")
    );
    assert_eq!(
        system_type_hint_from_url("https://new-api.example.com/"),
        Some("new-api")
    );
    assert_eq!(system_type_hint_from_url("https://api.example.com/"), None);
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
        Some("new-api")
    );
    assert_eq!(
        system_type_from_probes(
            probe(reqwest::StatusCode::UNAUTHORIZED, false),
            probe(reqwest::StatusCode::OK, true),
        ),
        Some("new-api")
    );
    assert_eq!(
        system_type_from_probes(
            probe(reqwest::StatusCode::NOT_FOUND, true),
            probe(reqwest::StatusCode::OK, true),
        ),
        Some("sub2api")
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
    assert!(script.contains(
        "Promise.all([probe(\"/api/status\"), probe(\"/setup/status\"), probeStudioFlags()])"
    ));
    // probe() 与 probeStudioFlags() 各带一个超时声明。
    assert_eq!(script.matches("AbortSignal.timeout(12000)").count(), 2);
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
        true,
        None,
    );

    assert!(script.contains("fetch(\"/api/user/auth/refresh\""));
    assert!(
        script.contains("method: \"POST\", credentials: \"include\", cache: \"no-store\", headers")
    );
    assert!(script.contains("fetch(\"/api/user/self\""));
    assert!(script.contains("`/api/user/checkin?month=${encodeURIComponent(\"2026-08\")}`"));
    assert!(script.contains("fetch(\"/api/user/checkin\""));
    assert!(script.contains("`/api/log/self?p=1&page_size=20&type="));
    assert_eq!(script.matches("fetch(").count(), 6);
    assert!(!script.contains("http://"));
    assert!(!script.contains("https://"));
    assert!(!script.contains("turnstile"));
    assert!(script.contains("window.location.protocol !== \"http:\""));
    assert!(script.contains("message.includes(\"Failed to parse URL\")"));
    assert!(script.contains("previous.state !== \"challenge\""));
    assert!(script.contains("state: \"running\""));
    assert!(script.contains("bridge.state = \"challenge\""));
    assert!(script.contains("window.location.assign(`/api/user/self#${token}`)"));
    assert!(script.contains("const shouldCheckin = true"));
    assert!(script.contains("const useRefreshAuth = true"));
    assert!(script.contains("const allowChallengeNavigation = true"));
    assert!(script.contains("return \"__OPENHUB_PROFILE_MISMATCH__\""));
    assert!(
        script.find("fetch(\"/api/user/token\"").unwrap()
            < script.find("const checkinResponse").unwrap()
    );
    assert!(
        script.find("fetch(\"/api/user/self\"").unwrap()
            < script.find("const checkinResponse").unwrap()
    );
    assert!(
        script.contains("method: \"GET\", credentials: \"include\", cache: \"no-store\", headers")
    );
    // 访问令牌逻辑只允许在 useRefreshAuth 分支内执行。
    assert!(
        script.find("if (useRefreshAuth) {").unwrap()
            < script.find("fetch(\"/api/user/token\"").unwrap()
    );
    assert!(script.contains("const useSessionCookies = !apiToken"));
    assert!(script.contains("if (useSessionCookies) {"));
    assert!(script.contains("const requestTimeout = 8000"));
    assert_eq!(
        script
            .matches("AbortSignal.timeout(requestTimeout)")
            .count(),
        6
    );
    assert!(!script.contains("account: accessToken"));
    assert!(!script.contains("if (Date.now() - previous.started < 3000) return pending;"));
    assert!(script.contains("tryParseDocumentAccount"));
    assert!(script.contains("window.location.reload()"));
    assert!(script.contains("__openHubChallengeReloads"));
}

#[test]
fn recognizes_alibaba_waf_shield_errors() {
    // 阿里云 ESA/WAF 的 JS 挑战：HTTP 200 + HTML，靠 acw_sc__v2 / var arg1 / x-tengine-error
    // 等特征识别，不能被当成普通 HTML 错误排除 Chrome 兜底。
    assert!(is_cloudflare_shield_error(
        "x-tengine-error: denied by http_custom"
    ));
    assert!(is_cloudflare_shield_error("站点返回 acw_sc__v2 挑战页"));
    assert!(is_cloudflare_shield_error("cdn_sec_tc=..."));
    assert!(is_cloudflare_shield_error("var arg1='0FA76A2C06F3'"));
    assert!(is_cloudflare_shield_error(
        "账号接口 HTTP 200 返回 HTML：站点返回了网页而不是 API 数据（可能被安全验证拦截）"
    ));
    assert!(requires_chrome_fallback(
        "账号接口 HTTP 200 返回 HTML：站点返回了网页而不是 API 数据（可能被安全验证拦截）"
    ));
}

#[test]
fn chrome_account_bridge_recognizes_alibaba_acw_challenge() {
    let script = chrome_account_bridge_script(
        Some("10288"),
        "2026-08",
        "openhub-sync-anyrouter",
        false,
        false,
        true,
        None,
    );

    // 桥接必须把 200 的阿里云 WAF 挑战页识别为 challenge，而不是普通 HTML 错误，
    // 否则挑战页 reload 后轮询循环已终止，账户/余额永远取不回来。
    assert!(script.contains("isAlibabaChallenge"));
    assert!(script.contains("var\\s+arg1\\s*="));
    assert!(script.contains("acw_sc__v2"));
    assert!(script.contains("cdn_sec_tc"));
    assert!(script.contains("x-tengine-error"));
    assert!(script.contains("denied by http_custom"));
    assert!(script.contains("isChallenge"));
    // 该测试用 legacy Cookie 模式，必须命中 /api/user/self 的 challenge 导航分支。
    assert!(script.contains("window.location.assign(`/api/user/self#${token}`)"));
    assert!(script.contains("window.location.reload()"));
    assert!(script.contains("tryParseDocumentAccount"));
}

#[test]
fn legacy_newapi_bridge_uses_standard_checkin_endpoint() {
    let script = chrome_account_bridge_script(
        Some("10288"),
        "2026-08",
        "openhub-sync-legacy",
        false,
        true,
        false,
        None,
    );

    assert!(script.contains("const useRefreshAuth = false"));
    assert!(
        script.find("if (useRefreshAuth) {").unwrap()
            < script.find("fetch(\"/api/user/token\"").unwrap()
    );
    assert!(!script.contains("isAnyRouter"));
    assert!(!script.contains("fetch(\"/api/user/sign_in\""));
    assert!(script.contains("`/api/user/checkin?month=${encodeURIComponent(\"2026-08\")}`"));
    assert!(script.contains("fetch(\"/api/user/checkin\""));
    assert!(script.contains("method: \"POST\""));
    // 静态脚本包含 refresh 分支，但常量为 false，Cookie 模式运行时不会请求令牌端点。
    assert!(script.contains("if (useRefreshAuth) {"));
}

#[test]
fn chrome_account_bridge_json_escapes_embedded_values() {
    let user_id = "10288\"; window.injected = true; //";
    let month = "2026-08\nnext";
    let marker = "openhub-sync-\"quoted";
    let script = chrome_account_bridge_script(Some(user_id), month, marker, false, false, false, None);

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

/// 真实 NewAPI `/api/perf-metrics/summary` 响应体。
/// 注意 `success_rate` 是 **0~100 的百分数**（上游 successRate() 直接返回
/// `successCount / requestCount * 100`），这一点本身就是回归测试的重点。
/// `recent_success_series` 同样是 0~100 的百分数，且**只含有流量的整点**
/// （上游 `QuerySummaryAll` 跳过 requestCount==0 的桶），界面按 window_start
/// 对号入座、缺槽画灰底。
const PERF_METRICS_SUMMARY_JSON: &str = r#"{
  "success": true,
  "data": {
    "window_start": 1750003200,
    "window_end": 1750086400,
    "models": [
      {
        "model_name": "gpt-5",
        "avg_latency_ms": 1234,
        "success_rate": 99.87,
        "avg_tps": 46.5,
        "recent_success_series": [
          {"ts": 1750046400, "success_rate": 99.9},
          {"ts": 1750003200, "success_rate": 99.87},
          {"ts": 1750032000, "success_rate": 100}
        ]
      },
      {
        "model_name": "claude-flaky",
        "avg_latency_ms": 2100,
        "success_rate": 62.5,
        "avg_tps": 30,
        "recent_success_series": [{"ts": 1750003200, "success_rate": 62.5}]
      },
      {
        "model_name": "claude-down",
        "avg_latency_ms": 0,
        "success_rate": 0,
        "avg_tps": 0,
        "recent_success_series": [{"ts": 1750003200, "success_rate": 0}]
      }
    ]
  }
}"#;

#[test]
fn parses_newapi_perf_metrics_summary_into_model_health() {
    let health = parse_perf_metrics_health(
        &serde_json::from_str(PERF_METRICS_SUMMARY_JSON).unwrap(),
        PERF_METRICS_WINDOW_HOURS,
    );

    assert_eq!(health.len(), 3, "每个模型都应有一条健康度，实际 {:?}", health);
    let gpt = health.get("gpt-5").expect("gpt-5 缺少健康度");
    assert_eq!(gpt.avg_latency_ms, Some(1234));
    assert_eq!(gpt.avg_tps, Some(46.5));
    assert_eq!(gpt.window_hours, PERF_METRICS_WINDOW_HOURS);
    // 99.87% 必须归一成 0.9987，而不是 99.87。
    assert_eq!(
        gpt.success_rate,
        Some(0.9987),
        "站点发的是 0~100 的百分数，必须归一到 0~1"
    );
    assert!(
        (gpt.success_rate.unwrap() - 0.9987).abs() < 1e-9,
        "归一后仍应保留小数位，否则 99.87% 会被显示成 100%"
    );

    // 中间档同样按 0~1 归一，不能是 62.5。
    let flaky = health.get("claude-flaky").expect("claude-flaky 缺少健康度");
    assert_eq!(flaky.success_rate, Some(0.625));

    // 0% 保持 0。
    let down = health.get("claude-down").expect("claude-down 缺少健康度");
    assert_eq!(down.success_rate, Some(0.0));
}

#[test]
fn perf_metrics_keeps_hourly_series_for_status_strip() {
    let health = parse_perf_metrics_health(
        &serde_json::from_str(PERF_METRICS_SUMMARY_JSON).unwrap(),
        PERF_METRICS_WINDOW_HOURS,
    );
    let gpt = health.get("gpt-5").expect("gpt-5 缺少健康度");

    // 窗口起点要带上：界面按它对号入座，否则逐时点无法定位到 24 个固定槽位。
    assert_eq!(gpt.window_start, Some(1750003200));

    // 序列必须按时间升序，逐点是 0~1 的归一值（0~100 百分数不能直接透传）。
    let series = &gpt.series;
    assert_eq!(series.len(), 3, "逐时序列应完整保留，实际 {:?}", series);
    assert_eq!(
        series.iter().map(|point| point.ts).collect::<Vec<_>>(),
        vec![1750003200, 1750032000, 1750046400],
        "序列应按时间升序排出"
    );
    assert_eq!(series[0].success_rate, Some(0.9987));
    assert_eq!(series[1].success_rate, Some(1.0));
    // 99.9/100 在浮点下是 0.9990000000000001，按容差比。
    assert!(
        (series[2].success_rate.unwrap() - 0.999).abs() < 1e-9,
        "逐时点同样按 0~1 归一，实际 {:?}",
        series[2].success_rate
    );

    // 只有有流量的整点才有数据点：无流量的整点不补零，界面据此留灰槽，
    // 否则「无流量」会被画成「成功率 0%」。
    assert!(
        !series.iter().any(|point| point.ts == 1750017600),
        "站点未下发的整点不该被补成 0%"
    );
}

#[test]
fn perf_metrics_series_tolerates_dirty_points_and_missing_window() {
    // 脏点（无时间戳 / 非正时间戳）应被丢弃而不是把槽位整体带偏；
    // 缺 window_start 时序列仍要保留，界面退回按最后一点倒推。
    let payload = serde_json::json!({
        "success": true,
        "data": {"models": [{
            "model_name": "gpt-5",
            "success_rate": 50,
            "recent_success_series": [
                {"success_rate": 99},
                {"ts": 0, "success_rate": 90},
                {"ts": -3600, "success_rate": 88},
                {"ts": 1750003200, "success_rate": 50},
                {"ts": "1750017600", "success_rate": 150}
            ]
        }]}
    });
    let health = parse_perf_metrics_health(&payload, PERF_METRICS_WINDOW_HOURS);
    let gpt = health.get("gpt-5").expect("gpt-5 缺少健康度");
    assert_eq!(gpt.window_start, None, "站点没给 window_start 时不该瞎猜");
    assert_eq!(
        gpt.series.iter().map(|point| point.ts).collect::<Vec<_>>(),
        vec![1750003200, 1750017600],
        "只保留带合法正时间戳的点"
    );
    // 越界的 150% 同样钳位到 1.0，不能让状态条拿到越界比率去套颜色阈值。
    assert_eq!(gpt.series[1].success_rate, Some(1.0));
    assert_eq!(gpt.series[0].success_rate, Some(0.5));
}

#[test]
fn perf_metrics_success_rate_clamps_out_of_range_percent() {
    // 站点理论上不会给出 >100 或负数，但归一后钳位能保证界面永远不会
    // 拿到一个越界比率去驱动进度条宽度与颜色阈值。
    let payload = serde_json::json!({
        "success": true,
        "data": {"models": [
            {"model_name": "over", "success_rate": 150},
            {"model_name": "under", "success_rate": -5}
        ]}
    });
    let health = parse_perf_metrics_health(&payload, PERF_METRICS_WINDOW_HOURS);
    assert_eq!(health.get("over").unwrap().success_rate, Some(1.0));
    assert_eq!(health.get("under").unwrap().success_rate, Some(0.0));
}

#[test]
fn perf_metrics_series_does_not_disturb_aggregate_rate() {
    // 逐时序列只作为状态条的原料，聚合成功率仍以顶层字段为准：
    // 低流量站点每桶非 0 即 100，若拿序列去反算聚合值就会被锯齿噪音带偏。
    let payload = serde_json::json!({
        "success": true,
        "data": {"models": [{
            "model_name": "gpt-5",
            "success_rate": 99.87,
            "recent_success_series": (0..500)
                .map(|i| serde_json::json!({"ts": i, "success_rate": if i % 2 == 0 { 100 } else { 0 }}))
                .collect::<Vec<_>>()
        }]}
    });
    let health = parse_perf_metrics_health(&payload, PERF_METRICS_WINDOW_HOURS);
    let gpt = health.get("gpt-5").expect("gpt-5 缺少健康度");
    assert_eq!(gpt.success_rate, Some(0.9987));
    assert_eq!(gpt.series.len(), 499, "ts=0 的脏点被丢弃，其余保留");
}

#[test]
fn perf_metrics_soft_fail_shapes_yield_empty_health() {
    // 老版本/魔改站点没有这条路由：404 的 JSON 错误体不能让同步失败。
    let not_found = serde_json::json!({"success": false, "message": "not found"});
    assert!(parse_perf_metrics_health(&not_found, PERF_METRICS_WINDOW_HOURS).is_empty());

    // 未登录 / 被盾：同样只是拿不到健康度。
    let unauthorized = serde_json::json!({"success": false, "message": "无权进行此操作"});
    assert!(parse_perf_metrics_health(&unauthorized, PERF_METRICS_WINDOW_HOURS).is_empty());

    // 站点未启用性能采集：data.models 为空数组。
    let empty = serde_json::json!({"success": true, "data": {"models": []}});
    assert!(parse_perf_metrics_health(&empty, PERF_METRICS_WINDOW_HOURS).is_empty());

    // 模型名为空的条目直接丢弃，不产出无名健康度。
    let nameless = serde_json::json!({
        "success": true,
        "data": {"models": [{"model_name": "", "success_rate": 0.5}]}
    });
    assert!(parse_perf_metrics_health(&nameless, PERF_METRICS_WINDOW_HOURS).is_empty());
}

#[test]
fn perf_metrics_health_is_ignored_for_non_newapi_sites() {
    // 该接口是 NewAPI 独有的，perf-metrics 路径不得被写死到其它平台的
    // 同步流程里（Sub2API 走 /api/v1/*，匿名站点走 /v1/models）。
    let sub2api = SiteModelsResult {
        models: vec![],
        source: "sub2api-key".into(),
        keys: vec!["sk-x".into()],
        key_groups: HashMap::new(),
        key_models: HashMap::new(),
        errors: Vec::new(),
        profile_id: String::new(),
        model_health: HashMap::new(),
    };
    assert!(
        sub2api.model_health.is_empty(),
        "Sub2API 不该带任何模型健康度"
    );
}

#[test]
fn chrome_key_models_bridge_also_fetches_perf_metrics() {
    let script = chrome_key_models_bridge_script(true, "10288");
    assert!(script.contains("fetch(\"/api/perf-metrics/summary?hours=24\""));
    // 健康度是纯附加信息：必须与 Key/模型分离，失败只把字段留 null。
    assert!(script.contains("health: null"));
    let health_fetch = script
        .find("fetch(\"/api/perf-metrics/summary")
        .expect("桥接脚本没有拉取性能指标");
    let token_fetch = script
        .find("fetch(\"/api/token/?p=1&size=20\"")
        .expect("桥接脚本没有拉取 Key 列表");
    assert!(
        token_fetch < health_fetch,
        "健康度必须在 Key 列表成功之后才拉，避免无谓请求"
    );
}

#[test]
fn chrome_key_models_bridge_health_cannot_break_the_main_flow() {
    let script = chrome_key_models_bridge_script(true, "10288");

    // 1) 桥接只有 10s/25s 预算，健康度必须与模型并发而不是串行多一个往返，
    //    并且自带一个远小于预算的超时。
    assert!(
        script.contains("Promise.all"),
        "模型与健康度应并发请求，否则健康度慢会把 Key 同步拖到桥接超时"
    );
    assert!(
        script.contains("AbortSignal.timeout(6000)"),
        "健康度应有独立且远小于桥接预算的超时，实际脚本：\n{script}"
    );

    // 2) 两个请求各自 catch：任一 fetch 抛错都只丢自己的字段，
    //    绝不能冒泡到外层 catch 把已拿到的 Key 结果覆盖成失败。
    let catch_count = script.matches(".catch(() =>").count();
    assert_eq!(
        catch_count, 2,
        "模型与健康度都必须有独立容错，实际 catch {catch_count} 处"
    );
}

#[test]
fn health_write_failure_never_blocks_model_cache_write() {
    // 回归保护：健康度写不进去（列缺失 / 库异常）时，Key 与模型的缓存
    // 仍然必须写成功。补偿信息不能绑架主流程。
    let database = health_test_database();
    // 模拟迁移没补上列的旧库。
    database
        .0
        .lock()
        .unwrap()
        .execute("ALTER TABLE site_model_cache DROP COLUMN health_json", [])
        .unwrap();

    let result = SiteModelsResult {
        models: vec![SiteModelItem {
            id: "gpt-5".into(),
            owned_by: None,
        }],
        source: "newapi-key".into(),
        keys: vec!["sk-one".into()],
        key_groups: HashMap::new(),
        key_models: HashMap::new(),
        errors: Vec::new(),
        profile_id: "Profile 11".into(),
        model_health: HashMap::from([(
            "gpt-5".to_string(),
            SiteModelHealth {
                success_rate: Some(0.9),
                window_hours: PERF_METRICS_WINDOW_HOURS,
                ..SiteModelHealth::default()
            },
        )]),
    };
    let account = SiteModelCacheAccount {
        profile_id: "Profile 11".into(),
        profile_name: "Profile 11".into(),
        account_name: "a@example.com".into(),
        username: "a".into(),
        keys: vec!["sk-one".into()],
        key_groups: HashMap::new(),
        key_models: HashMap::new(),
        model_health: HashMap::new(),
        error: String::new(),
    };

    save_site_model_cache(&database, "site-ai", &account, Some(&result), false)
        .expect("没有 health_json 列时也必须把 Key/模型缓存写进去");

    let connection = database.0.lock().unwrap();
    let (keys_json, models_json, error): (String, String, String) = connection
        .query_row(
            "SELECT keys_json, models_json, error FROM site_model_cache
             WHERE site_id='site-ai' AND profile_id='Profile 11'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    drop(connection);
    assert_eq!(keys_json, r#"["sk-one"]"#);
    assert!(models_json.contains("gpt-5"));
    assert_eq!(error, "", "健康度缺失不该被记成同步错误");
}

#[test]
fn chrome_key_models_bridge_result_carries_optional_health() {
    let with_health = parse_chrome_key_models_bridge_result(
        r#"{"ok":true,"tokenList":{"data":[]},"models":null,"health":PERF_METRICS_SUMMARY_JSON}"#
            .replace("PERF_METRICS_SUMMARY_JSON", PERF_METRICS_SUMMARY_JSON)
            .as_str(),
    )
    .unwrap();
    let health = with_health.health.expect("桥接结果缺少健康度");
    assert_eq!(
        parse_perf_metrics_health(&health, PERF_METRICS_WINDOW_HOURS).len(),
        3
    );

    // 站点没有该路由时桥接只返回 models，health 缺省为 None 而不是报错。
    let without_health =
        parse_chrome_key_models_bridge_result(r#"{"ok":true,"tokenList":{"data":[]}}"#).unwrap();
    assert!(without_health.health.is_none());
    assert!(without_health.models.is_none());
}

/// site_model_cache 的最小真实 schema（与 core/db.rs 的建表语句对齐）。
fn health_test_database() -> Database {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE site_accounts (
                    site_id TEXT NOT NULL,
                    profile_id TEXT NOT NULL,
                    is_valid INTEGER NOT NULL DEFAULT 0,
                    sync_error TEXT NOT NULL DEFAULT '',
                    PRIMARY KEY (site_id, profile_id)
                 );
                 CREATE TABLE site_model_cache (
                    site_id TEXT NOT NULL,
                    profile_id TEXT NOT NULL,
                    profile_name TEXT NOT NULL DEFAULT '',
                    account_name TEXT NOT NULL DEFAULT '',
                    username TEXT NOT NULL DEFAULT '',
                    api_source TEXT NOT NULL DEFAULT '',
                    keys_json TEXT NOT NULL DEFAULT '[]',
                    groups_json TEXT NOT NULL DEFAULT '{}',
                    models_json TEXT NOT NULL DEFAULT '[]',
                    key_models_json TEXT NOT NULL DEFAULT '{}',
                    health_json TEXT NOT NULL DEFAULT '{}',
                    error TEXT NOT NULL DEFAULT '',
                    updated_at TEXT NOT NULL DEFAULT '',
                    PRIMARY KEY (site_id, profile_id)
                 );",
        )
        .unwrap();
    Database(std::sync::Mutex::new(connection))
}

fn read_health_json(database: &Database, site_id: &str, profile_id: &str) -> String {
    database
        .0
        .lock()
        .unwrap()
        .query_row(
            "SELECT health_json FROM site_model_cache WHERE site_id = ?1 AND profile_id = ?2",
            params![site_id, profile_id],
            |row| row.get::<_, String>(0),
        )
        .unwrap()
}

#[test]
fn saves_perf_metrics_health_into_model_cache() {
    let database = health_test_database();
    let health = parse_perf_metrics_health(
        &serde_json::from_str(PERF_METRICS_SUMMARY_JSON).unwrap(),
        PERF_METRICS_WINDOW_HOURS,
    );
    let result = SiteModelsResult {
        models: vec![SiteModelItem {
            id: "gpt-5".into(),
            owned_by: None,
        }],
        source: "newapi-key".into(),
        keys: vec!["sk-one".into()],
        key_groups: HashMap::new(),
        key_models: HashMap::new(),
        errors: Vec::new(),
        profile_id: "Profile 11".into(),
        model_health: health.clone(),
    };
    let account = SiteModelCacheAccount {
        profile_id: "Profile 11".into(),
        profile_name: "Profile 11".into(),
        account_name: "a@example.com".into(),
        username: "a".into(),
        keys: vec!["sk-one".into()],
        key_groups: HashMap::new(),
        key_models: HashMap::new(),
        model_health: HashMap::new(),
        error: String::new(),
    };
    save_site_model_cache(&database, "site-ai", &account, Some(&result), false).unwrap();

    let stored: HashMap<String, SiteModelHealth> =
        serde_json::from_str(&read_health_json(&database, "site-ai", "Profile 11")).unwrap();
    assert_eq!(stored.len(), 3, "健康度应完整落库，实际 {:?}", stored);
    assert_eq!(stored.get("gpt-5").unwrap().success_rate, Some(0.9987));
}

#[test]
fn model_only_sync_keeps_previously_synced_health() {
    // 回归保护：「同步模型」只按缓存 Key 刷 /v1/models，不带健康度回来，
    // 且 preserve_keys=true。若此时把 health_json 清空，界面上模型健康度
    // 会随着用户点一次「同步模型」凭空消失。
    let database = health_test_database();
    let health = parse_perf_metrics_health(
        &serde_json::from_str(PERF_METRICS_SUMMARY_JSON).unwrap(),
        PERF_METRICS_WINDOW_HOURS,
    );
    let key_result = SiteModelsResult {
        models: vec![SiteModelItem {
            id: "gpt-5".into(),
            owned_by: None,
        }],
        source: "newapi-key".into(),
        keys: vec!["sk-one".into()],
        key_groups: HashMap::new(),
        key_models: HashMap::new(),
        errors: Vec::new(),
        profile_id: "Profile 11".into(),
        model_health: health,
    };
    let account = SiteModelCacheAccount {
        profile_id: "Profile 11".into(),
        profile_name: "Profile 11".into(),
        account_name: "a@example.com".into(),
        username: "a".into(),
        keys: vec!["sk-one".into()],
        key_groups: HashMap::new(),
        key_models: HashMap::new(),
        model_health: HashMap::new(),
        error: String::new(),
    };
    save_site_model_cache(&database, "site-ai", &account, Some(&key_result), false).unwrap();

    // 同步模型：result 不带健康度，account 也不带。
    let models_only = SiteModelsResult {
        models: vec![SiteModelItem {
            id: "gpt-5".into(),
            owned_by: None,
        }],
        source: "newapi-key".into(),
        keys: vec!["sk-one".into()],
        key_groups: HashMap::new(),
        key_models: HashMap::new(),
        errors: Vec::new(),
        profile_id: "Profile 11".into(),
        model_health: HashMap::new(),
    };
    save_site_model_cache(&database, "site-ai", &account, Some(&models_only), true).unwrap();

    let stored: HashMap<String, SiteModelHealth> =
        serde_json::from_str(&read_health_json(&database, "site-ai", "Profile 11")).unwrap();
    assert_eq!(
        stored.len(),
        3,
        "同步模型不该抹掉上次同步 Key 拉到的健康度，实际 {:?}",
        stored
    );
    assert_eq!(stored.get("claude-flaky").unwrap().success_rate, Some(0.625));
}

