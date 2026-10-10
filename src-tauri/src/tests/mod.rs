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
fn clears_checkin_state_for_baiheibai_rows_on_startup() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE directory_sites (
                id TEXT PRIMARY KEY,
                system_type TEXT NOT NULL DEFAULT '',
                supports_checkin INTEGER NOT NULL DEFAULT 0,
                checkin_url TEXT NOT NULL DEFAULT '',
                checkin_note TEXT NOT NULL DEFAULT ''
             );
             INSERT INTO directory_sites (id, system_type, supports_checkin, checkin_url, checkin_note) VALUES
                ('baiheibai', 'baiheibai', 1, 'https://cdk.hybgzs.com/gas-station/checkin', '信任等级越高签到奖励越多'),
                ('alias', 'hybgzs', 1, 'https://example.com/checkin', 'note'),
                ('newapi', 'new-api', 1, 'https://example.com/console/personal', 'keep');
             CREATE TABLE site_accounts (
                site_id TEXT NOT NULL,
                profile_id TEXT NOT NULL,
                checkin_enabled INTEGER NOT NULL DEFAULT 0,
                checked_in_today INTEGER NOT NULL DEFAULT 0,
                checkin_error TEXT NOT NULL DEFAULT ''
             );
             INSERT INTO site_accounts (site_id, profile_id, checkin_enabled, checked_in_today, checkin_error) VALUES
                ('baiheibai', 'p1', 1, 1, '旧错误'),
                ('alias', 'p1', 1, 0, ''),
                ('newapi', 'p1', 1, 1, '');",
        )
        .unwrap();

    clear_baiheibai_checkin_state(&connection).unwrap();

    let site_rows: Vec<(String, i64, String, String)> = connection
        .prepare(
            "SELECT id, supports_checkin, checkin_url, checkin_note
             FROM directory_sites ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        site_rows,
        vec![
            ("alias".into(), 0, "".into(), "".into()),
            ("baiheibai".into(), 0, "".into(), "".into()),
            (
                "newapi".into(),
                1,
                "https://example.com/console/personal".into(),
                "keep".into()
            ),
        ]
    );

    let account_rows: Vec<(String, i64, i64, String)> = connection
        .prepare(
            "SELECT site_id, checkin_enabled, checked_in_today, checkin_error
             FROM site_accounts ORDER BY site_id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        account_rows,
        vec![
            ("alias".into(), 0, 0, "".into()),
            ("baiheibai".into(), 0, 0, "".into()),
            ("newapi".into(), 1, 1, "".into()),
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
fn record_site_model_cache_errors_writes_back_per_account() {
    // 回归保护：站点级同步没取到任何 Key 时，失败原因要按 profile_id 写回各自
    // 缓存行，由界面在账号下方显示；不落到顶层，也不该凭空造出空壳账号行。
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE site_accounts (
                    site_id TEXT NOT NULL,
                    profile_id TEXT NOT NULL,
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
                 );
                 INSERT INTO site_model_cache (site_id, profile_id, keys_json)
                 VALUES ('site-ai', 'Profile 11', '[\"sk-keep\"]');",
        )
        .unwrap();
    let database = Database(std::sync::Mutex::new(connection));

    record_site_model_cache_errors(
        &database,
        "site-ai",
        &[
            "Profile 11：旧版 NewAPI 本地 user 缺少用户 ID".to_string(),
            // 不属于任何账号的站点级错误没有账号可挂，必须跳过。
            "站点模型同步超过 90 秒，已强制终止".to_string(),
        ],
    )
    .unwrap();

    let rows: Vec<(String, String, String)> = {
        let connection = database.0.lock().unwrap();
        let mut statement = connection
            .prepare(
                "SELECT profile_id, keys_json, error FROM site_model_cache
                  WHERE site_id='site-ai' ORDER BY profile_id",
            )
            .unwrap();
        statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(rows.len(), 1, "不该为无归属的站点级错误新增缓存行：{rows:?}");
    assert_eq!(rows[0].0, "Profile 11");
    assert_eq!(
        rows[0].1, "[\"sk-keep\"]",
        "写回失败原因时不能动已有 Key"
    );
    assert_eq!(rows[0].2, "旧版 NewAPI 本地 user 缺少用户 ID");
}

#[test]
fn record_site_model_cache_errors_fills_missing_row() {
    // 账号还没被同步过 Key（没有缓存行）时也要能显示失败原因：
    // 按账号表补一行带展示名的记录，而不是让错误凭空消失。
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE site_accounts (
                    site_id TEXT NOT NULL,
                    profile_id TEXT NOT NULL,
                    profile_name TEXT NOT NULL DEFAULT '',
                    account_name TEXT NOT NULL DEFAULT '',
                    username TEXT NOT NULL DEFAULT ''
                 );
                 INSERT INTO site_accounts (site_id, profile_id, profile_name, account_name, username)
                 VALUES ('site-ai', 'Profile 15', '猫', 'wusuoming@gmail.com', '');
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

    record_site_model_cache_errors(&database, "site-ai", &["Profile 15：读取失败".to_string()])
        .unwrap();

    let (profile_name, account_name, error, keys_json): (String, String, String, String) =
        database
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT profile_name, account_name, error, keys_json FROM site_model_cache
                 WHERE site_id='site-ai' AND profile_id='Profile 15'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
    assert_eq!(profile_name, "猫", "补行时未带出账号表展示名");
    assert_eq!(account_name, "wusuoming@gmail.com");
    assert_eq!(error, "读取失败");
    assert_eq!(keys_json, "[]", "补行不应带任何 Key");
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
fn browser_session_evidence_requires_real_login_signal() {
    // 没有会话信息就没有这个账号：既无结构化账号数据、又无登录类 Cookie 的
    // Chrome 配置不该被列成站点账号。
    assert!(!has_browser_session_evidence("new-api", None, &[]));
    assert!(!has_browser_session_evidence(
        "new-api",
        Some(&HashMap::new()),
        &[]
    ));
    // 人机验证 / 统计 Cookie 只证明访问过站点，不证明有登录会话。
    // 人机验证 / 统计 Cookie 只证明访问过站点，不证明有登录会话。
    // `cf_*` / `__cf*` 一律按前缀忽略：Cloudflare 会随验证方式新增名字
    // （cf_clearance、cf_chl_*、__cf_bm、__cf_ob、__cfwaitingroom…），
    // 逐个列名单一定会漏，所以按**包含**判定（`cf_` / `__cf` / `-cf-`）。
    for noise in [
        "cf_clearance",
        "CF_CLEARANCE",
        "cf_chl_2",
        "__cf_bm",
        "__cf_ob",
        "__cfwaitingroom",
        "x-cf-challenge",
        "acw_tc",
        " _cfuvid ",
    ] {
        assert!(
            !has_browser_session_evidence("new-api", None, &[noise.to_string()]),
            "{noise} 不该算会话证据"
        );
    }
    // 只有噪音 Cookie 的域（真实例子：黑与白公益站的 Profile 15 只剩 __cf_ob）
    // 不该凭空建出账号行。
    assert!(!has_browser_session_evidence(
        "baiheibai",
        None,
        &["__cf_ob".to_string(), "cf_clearance".to_string()]
    ));
    // Local Storage 里只有 UI 噪音键（真实例子：黑与白公益站 Profile 11
    // 只有 iconify* 与 theme）也不算会话痕迹。
    let ui_only = HashMap::from([
        ("iconify7".to_string(), "[]".to_string()),
        ("iconify11".to_string(), "[]".to_string()),
        ("theme".to_string(), "dark".to_string()),
    ]);
    assert!(!has_session_storage_keys(&ui_only));
    assert!(!has_browser_session_evidence(
        "baiheibai",
        Some(&ui_only),
        &[]
    ));
    // 与登录有关的键仍然算（哪怕只是残缺数据，交给账号接口验证）。
    let real = HashMap::from([
        ("user".to_string(), "{}".to_string()),
        ("iconify7".to_string(), "[]".to_string()),
    ]);
    assert!(has_session_storage_keys(&real));
    assert!(is_session_storage_key("user"));
    assert!(is_session_storage_key("pipi_pc_token"));
    assert!(!is_session_storage_key("iconify13"));
    assert!(!is_session_storage_key("THEME"));
    // Cloudflare 的标记键也可能落进 Local Storage，同样按包含判定。
    assert!(!is_session_storage_key("cf_clearance"));
    assert!(!is_session_storage_key("__cf_ob"));
    assert!(!has_browser_session_evidence(
        "new-api",
        None,
        &["cf_clearance".to_string()]
    ));
    // 登录类 Cookie（站点自定义会话名也算）即视为有会话。
    assert!(has_browser_session_evidence(
        "new-api",
        None,
        &["session".to_string()]
    ));
    assert!(has_browser_session_evidence(
        "sub2api",
        None,
        &["cf_clearance".to_string(), "server_name_session".to_string()]
    ));
    // 残缺的 Local Storage 键（只有 status 之类，解析不出账号）不算会话。
    let partial = HashMap::from([("status".to_string(), r#"{"ok":true}"#.to_string())]);
    assert!(!has_browser_session_evidence(
        "new-api",
        Some(&partial),
        &[]
    ));
    // 结构化账号数据依旧是会话证据。
    let valid = HashMap::from([("user".into(), r#"{"id":10288,"username":"wudixm"}"#.into())]);
    assert!(has_browser_session_evidence(
        "NewAPI",
        Some(&valid),
        &["cf_clearance".to_string()]
    ));
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
fn clears_checkin_fields_for_baiheibai_sites_on_normalize() {
    // 「白与黑」（含别名写法）没有签到集成：签到字段在所有保存路径上清零。
    let mut site = SiteRecord::default();
    site.name = "黑与白公益站".into();
    site.api_base_url = "https://ai.hybgzs.com/".into();
    site.system_type = "hybgzs".into();
    site.supports_checkin = true;
    site.checkin_url = "https://cdk.hybgzs.com/gas-station/checkin".into();
    site.checkin_note = "信任等级越高签到奖励越多".into();

    let site = normalize_site(site).unwrap();
    assert_eq!(site.system_type, "baiheibai");
    assert!(!site.supports_checkin);
    assert_eq!(site.checkin_url, "");
    assert_eq!(site.checkin_note, "");

    // NewAPI 系的签到字段不受影响，原样保留。
    let mut newapi = SiteRecord::default();
    newapi.name = "示例站".into();
    newapi.api_base_url = "https://example.com/".into();
    newapi.system_type = "new-api".into();
    newapi.supports_checkin = true;
    newapi.checkin_url = "https://example.com/console/personal".into();
    newapi.checkin_note = "每日签到".into();

    let newapi = normalize_site(newapi).unwrap();
    assert!(newapi.supports_checkin);
    assert_eq!(newapi.checkin_url, "https://example.com/console/personal");
    assert_eq!(newapi.checkin_note, "每日签到");
}

#[test]
fn parses_paginated_token_lists_from_custom_backends() {
    // GoFrame 风格分页（「白与黑」等自定义后端的常见形状）：列表在 data.list。
    let goframe = serde_json::json!({
        "code": 0,
        "message": "",
        "data": {
            "list": [{ "id": 7, "key": "sk-abcdef123456", "status": 1 }],
            "total": 1,
            "page": 1,
            "size": 10
        }
    });
    assert_eq!(parse_api_keys(&goframe), vec!["sk-abcdef123456"]);
    assert_eq!(parse_newapi_token_ids(&goframe), vec!["7"]);

    // records 分页 + 掩码 Key：明文取不到时必须拿到 ID 走揭示接口。
    let masked = serde_json::json!({
        "code": 0,
        "data": {
            "records": [{ "id": "abc-1", "key": "sk-****", "status": 1 }],
            "total": 1
        }
    });
    assert!(parse_api_keys(&masked).is_empty());
    assert_eq!(parse_newapi_token_ids(&masked), vec!["abc-1"]);
}

#[test]
fn parses_baiheibai_slot_status_health_shape() {
    // 取自「白与黑」真实响应：/api/model_health/slot_status?group=GLM&window=24h
    // （data.models[].slot_data，success_rate 为 0~100 百分数、total_requests 为请求数）。
    let payload = serde_json::json!({
        "data": {
            "cache_ttl": 30,
            "enabled": true,
            "group": "GLM",
            "models": [{
                "current_status": "red",
                "display_name": "glm-5.3-flash",
                "model_name": "glm-5.3-flash",
                "slot_data": [
                    { "end_time": 1791093312, "slot": 1, "start_time": 1791091512, "status": "red",
                      "success_count": 0, "success_rate": 0, "total_requests": 2 },
                    { "end_time": 1791095112, "slot": 2, "start_time": 1791093312, "status": "red",
                      "success_count": 1, "success_rate": 14.29, "total_requests": 7 },
                    { "end_time": 1791096912, "slot": 3, "start_time": 1791095112, "status": "red",
                      "success_count": 0, "success_rate": 0, "total_requests": 1 }
                ]
            }]
        },
        "success": true
    });

    let health = parse_model_status_health(&payload);
    let item = health
        .get("glm-5.3-flash")
        .expect("slot_status 应解析出模型健康度");
    assert_eq!(item.series.len(), 3, "slot_data 应逐格转成时间序列");
    let total: u64 = item.series.iter().filter_map(|point| point.requests).sum();
    assert_eq!(total, 10, "请求数应随序列带回");
    let rate = item
        .success_rate
        .expect("站点没给汇总时应按请求数加权算出成功率");
    // (0*2 + 0.1429*7 + 0*1) / 10 ≈ 0.1
    assert!((rate - 0.1).abs() < 0.001, "实际 {rate}");
}

#[test]
fn parses_recent_success_rates_without_timestamps() {
    // 「南梁 API」（旧版/魔改 NewAPI）只回 recent_success_rates：
    // 0~100 的数字数组、无时间戳，上游语义为升序（最后一个最新）。
    let payload = serde_json::json!({
        "data": {
            "models": [{
                "model_name": "deepseek-v4-pro-次",
                "avg_latency_ms": 30652,
                "success_rate": 96.45,
                "avg_tps": 264.16,
                "recent_success_rates": [100, 100, 0]
            }]
        },
        "success": true
    });
    let health = parse_perf_metrics_health(&payload, 24);
    let item = health
        .get("deepseek-v4-pro-次")
        .expect("应解析出模型健康度");
    assert_eq!(item.avg_latency_ms, Some(30652));
    assert_eq!(item.avg_tps, Some(264.16));
    assert_eq!(item.series.len(), 3, "recent_success_rates 应转成时间序列");
    let rates: Vec<f64> = item
        .series
        .iter()
        .map(|point| point.success_rate.expect("序列点应有成功率"))
        .collect();
    assert_eq!(rates, vec![1.0, 1.0, 0.0], "顺序应保持升序（旧→新）");
    let timestamps: Vec<i64> = item.series.iter().map(|point| point.ts).collect();
    assert!(
        timestamps.windows(2).all(|pair| pair[1] - pair[0] == 3600),
        "无时间戳时应按 1 小时间距铺开，实际 {timestamps:?}"
    );

    // 新版 NewAPI 的 recent_success_series 仍然优先，不受回退影响。
    let modern = serde_json::json!({
        "data": {
            "models": [{
                "model_name": "glm-5.3",
                "recent_success_series": [
                    {"ts": 1750003200, "success_rate": 62.5},
                    {"ts": 1750006800, "success_rate": 100}
                ],
                "recent_success_rates": [1, 2]
            }]
        }
    });
    let health = parse_perf_metrics_health(&modern, 24);
    let item = health.get("glm-5.3").unwrap();
    assert_eq!(item.series.len(), 2);
    assert_eq!(item.series[0].success_rate, Some(0.625));
    assert_eq!(item.series[0].ts, 1750003200);
}

#[test]
fn parses_perf_metrics_model_detail_series() {
    // 「CUN.AI」逐模型明细接口 `/api/perf-metrics?model=X` 的真实形状：
    // data.groups[].series 带 ts 与 0~100 的 success_rate。
    let payload = serde_json::json!({
        "data": {
            "model_name": "glm-5.3",
            "series_schema": "dbcd0a3c01b55203",
            "groups": [
                {
                    "group": "vip",
                    "success_rate": 50,
                    "series": [{ "ts": 1791100800, "success_rate": 50, "avg_latency_ms": 100 }]
                },
                {
                    "group": "default",
                    "success_rate": 76.49,
                    "series": [
                        { "ts": 1791097200, "success_rate": 87.5, "avg_latency_ms": 1036 },
                        { "ts": 1791100800, "success_rate": 88.88, "avg_latency_ms": 1083 },
                        { "ts": 1791104400, "success_rate": 38.09, "avg_latency_ms": 2269 }
                    ]
                }
            ]
        },
        "success": true
    });
    let series = parse_perf_metrics_model_detail(&payload);
    assert_eq!(series.len(), 3, "应取序列最长的分组");
    assert_eq!(series[0].ts, 1791097200);
    assert!((series[0].success_rate.unwrap() - 0.875).abs() < 1e-9);
    assert!((series[2].success_rate.unwrap() - 0.3809).abs() < 1e-9);
    assert!(series.iter().all(|point| point.requests.is_none()));

    // 没有分组数据（窗口内无流量）时返回空。
    let empty = serde_json::json!({ "data": { "model_name": "x", "groups": [] } });
    assert!(parse_perf_metrics_model_detail(&empty).is_empty());
}

#[test]
fn parses_user_model_status_health_and_models() {
    // 取自「Agent Router」真实响应：/api/user/model-status
    // 心跳按等级映射槽位（ok→1.0、warn→0.6、none→留灰），
    // 窗口汇总取 success_rate_24h，bucket_seconds 为格宽。
    let payload = serde_json::json!({
        "data": {
            "generated_at": "2026-10-05T15:25:08+08:00",
            "window_hours": 24,
            "bucket_seconds": 1200,
            "tiles": { "success_rate_24h": 96.54, "models_up": 7, "models_total": 7 },
            "models": [
                {
                    "name": "deepseek-v4-flash",
                    "status": "operational",
                    "current_tier": "ok",
                    "heartbeat": ["warn", "ok", "none", "ok"],
                    "heartbeat_start": 1791099600,
                    "success_rate_24h": 96.43,
                    "avg_latency_ms": 10292
                },
                {
                    "name": "claude-fable-5",
                    "status": "no_traffic",
                    "current_tier": "none",
                    "heartbeat": ["none", "none"],
                    "heartbeat_start": 1791099600,
                    "success_rate_24h": null,
                    "avg_latency_ms": 0
                }
            ]
        },
        "success": true
    });

    let health = parse_user_model_status_health(&payload);
    assert_eq!(health.len(), 1, "无流量模型不应产生健康度条目");
    let item = health.get("deepseek-v4-flash").expect("应解析出模型健康度");
    assert_eq!(item.window_hours, 24);
    assert_eq!(item.window_start, Some(1791099600));
    assert!((item.success_rate.unwrap() - 0.9643).abs() < 1e-9);
    assert_eq!(item.avg_latency_ms, Some(10292));
    // 心跳序列：warn=0.6、ok=1.0、none 跳过（第 3 格留灰，序列里没有 ts 对应点）
    assert_eq!(item.series.len(), 3, "none 不应产生数据点");
    assert_eq!(item.series[0].ts, 1791099600);
    assert!((item.series[0].success_rate.unwrap() - 0.6).abs() < 1e-9);
    assert_eq!(item.series[1].ts, 1791099600 + 1200);
    assert!((item.series[1].success_rate.unwrap() - 1.0).abs() < 1e-9);
    assert_eq!(item.series[2].ts, 1791099600 + 3 * 1200, "跳过 none 后 ts 仍按原始下标");

    // 模型清单同源解析（去重排序）。
    let models = parse_user_model_status_models(&payload);
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["claude-fable-5", "deepseek-v4-flash"]
    );

    // 非该接口形状：返回空，不影响其它健康度通道。
    assert!(parse_user_model_status_health(&serde_json::json!({ "success": false })).is_empty());
    assert!(parse_user_model_status_models(&serde_json::json!({ "data": {} })).is_empty());
}

#[test]
fn resolves_public_embed_health_host() {
    // x666 的逐模型健康度只在工具子域公开下发，按注册域识别（含 www 与更深子域）。
    let some = |url: &str| {
        crate::model::catalog::public_embed_health_url(&url::Url::parse(url).unwrap()).is_some()
    };
    assert!(some("https://x666.me/"));
    assert!(some("https://www.x666.me/api/"));
    assert!(some("https://tool.x666.me/"));
    assert!(!some("https://example.com/"));
    // 后缀必须落在域名边界上，不能被「x666.me.evil.com」这类主机蒙混过关。
    assert!(!some("https://x666.me.evil.com/"));
    assert!(!some("https://notx666.me/"));
}

#[test]
fn parses_x666_public_embed_status() {
    // 取自「薄荷 API」(x666) 公开嵌入接口的真实形状：
    // data 是数组，每条 = 一个模型 × slot_data（1 小时格，success_rate 为 0~100）。
    let payload = serde_json::json!({
        "data": [
            {
                "model_name": "grok-4.7",
                "current_status": "red",
                "success_rate": 72.79,
                "total_requests": 23215,
                "slot_data": [
                    { "slot": 0, "start_time": 1791108158, "end_time": 1791111758,
                      "status": "red", "success_rate": 40, "total_requests": 120 },
                    { "slot": 1, "start_time": 1791111758, "end_time": 1791115358,
                      "status": "green", "success_rate": 100, "total_requests": 80 },
                    { "slot": 2, "start_time": 1791115358, "end_time": 1791118958,
                      "status": "green", "success_rate": 100, "total_requests": 0 }
                ]
            },
            {
                "model_name": "glm-5.3-200k",
                "current_status": "red",
                "success_rate": 0,
                "total_requests": 910,
                "slot_data": [
                    { "slot": 0, "start_time": 1791108158, "end_time": 1791111758,
                      "status": "red", "success_rate": 0, "total_requests": 910 }
                ]
            },
            {
                // 无请求也无有色格子：应在解析时丢弃（界面没有可讲的状态）。
                "model_name": "idle-model",
                "current_status": "green",
                "success_rate": 100,
                "total_requests": 0,
                "slot_data": [
                    { "slot": 0, "start_time": 1791108158, "end_time": 1791111758,
                      "status": "green", "success_rate": 100, "total_requests": 0 }
                ]
            }
        ],
        "success": true
    });
    let health = parse_model_status_health(&payload);
    assert_eq!(health.len(), 2, "无流量模型应被丢弃");
    let grok = health.get("grok-4.7").expect("应解析出 grok-4.7");
    // 零请求格留灰（不出数据点），有流量的两格进入序列。
    assert_eq!(grok.series.len(), 2, "total_requests=0 的格不应产生数据点");
    assert!(grok.series.iter().all(|point| point.success_rate != Some(1.0) || point.requests != Some(0)));
    let glm = health.get("glm-5.3-200k").expect("应解析出 glm-5.3-200k");
    assert_eq!(glm.series.len(), 1);
    assert_eq!(glm.series[0].success_rate, Some(0.0), "0% 是确切失败，不是无数据");
}

#[tokio::test]
async fn reveal_newapi_keys_explains_why_no_token_id() {
    let client = wreq::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .no_proxy()
        .build()
        .expect("测试用 HTTP 客户端");
    let base = url::Url::parse("https://example.com/").unwrap();
    let auth = NewApiAuth::Legacy {
        cookie_header: String::new(),
        user_id: String::new(),
    };

    // 站点明确失败：必须回站点自己的 message，而不是笼统的「没有令牌 ID」。
    let denied = serde_json::json!({ "success": false, "message": "无权进行此操作" });
    let error = reveal_newapi_keys(&client, &base, &auth, "ua", &denied)
        .await
        .unwrap_err();
    assert!(error.contains("无权进行此操作"), "实际：{error}");

    // 确实没有令牌：说清是空列表。
    let empty = serde_json::json!({ "success": true, "data": { "items": [], "total": 0 } });
    let error = reveal_newapi_keys(&client, &base, &auth, "ua", &empty)
        .await
        .unwrap_err();
    assert!(error.contains("令牌列表为空"), "实际：{error}");

    // 形状不认识：附原文片段便于定位（这几种分支都不发起网络请求）。
    let weird = serde_json::json!({ "hello": "world" });
    let error = reveal_newapi_keys(&client, &base, &auth, "ua", &weird)
        .await
        .unwrap_err();
    assert!(
        error.contains("响应形状无法识别") && error.contains("hello"),
        "实际：{error}"
    );
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
fn explicit_unknown_system_type_is_not_the_same_as_unset() {
    // 回归保护：显式选「未知类型」必须与「从未设置」区分开。
    // 空串＝未设置，程序仍按浏览器里的痕迹自动识别架构；显式 unknown 是用户
    // 声明「不是任何已知架构」，任何架构推断都不能发生——否则界面显示未知类型，
    // 弹出来的却是 NewAPI 的流程与报错。
    assert!(is_explicit_unknown("unknown"));
    assert!(is_explicit_unknown("  UNKNOWN  "));
    assert!(
        !is_explicit_unknown(""),
        "空串是未设置，必须保留自动识别能力"
    );
    assert!(!is_explicit_unknown("new-api"));
    assert!(
        !is_newapi("unknown") && !is_sub2api("unknown"),
        "显式未知不应被当成任何已知架构"
    );
    assert_eq!(canonical_platform("unknown"), "");
    assert_eq!(
        console_page_path("unknown"),
        None,
        "显式未知不应被塞进 NewAPI 的控制台路径"
    );
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
    assert!(script.contains("Promise.all([probe(\"/api/status\"), probe(\"/setup/status\")])"));
    // 单个 probe() 带一个超时声明。
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
    // 7 个 fetch：refresh / token / self / checkin(GET) / checkin(POST) / log(self) 各一，
    // 外加 Sub2API 分支的 /api/v1/auth/me（运行期由 useSub2Api 开关跳过）。
    assert_eq!(script.matches("fetch(").count(), 7);
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
        7
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
fn recognizes_direct_request_unavailable_as_bridge_trigger() {
    // 线上实测（喵喵聚合）：/api/token 直连超时，站点既不返回 HTML 也没有盾特征，
    // 旧逻辑只认遇盾，于是 Chrome 兜底永不触发，该站点全部账号一律失败。
    assert!(is_direct_request_unavailable(
        "error sending request for url (https://ai.yangwj.me/api/token/?p=1&size=20): operation timed out"
    ));
    assert!(is_direct_request_unavailable(
        "NewAPI Key 接口响应读取失败：request or response body error: operation timed out"
    ));
    assert!(is_direct_request_unavailable("connection refused"));
    assert!(needs_browser_bridge(
        "error sending request for url (https://x/api/token): operation timed out"
    ));
    // 遇盾仍照旧触发兜底。
    assert!(needs_browser_bridge(
        "账号接口 HTTP 403 返回 HTML：站点安全验证（Cloudflare / 阿里云 WAF）拦截了直接请求"
    ));
    // 令牌类拒绝不属于直连不可用（由调用方另行处理，不重复触发）。
    assert!(!is_direct_request_unavailable("账号接口 HTTP 401：Token has expired"));
    assert!(!needs_browser_bridge("账号接口 HTTP 401：Token has expired"));
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

/// 「模型状态」增强模块（x666 这类没有 perf-metrics 的魔改 NewAPI）的响应体。
///
/// 字段口径按站点自己的前端实现：`data` 直接是模型数组，`generated_at` /
/// `ready` 在顶层；成功率是 **0~100 的百分数**；时间格按管理员配置的
/// `slot_minutes`（这里 30 分钟）切，`total_requests == 0` 的格子是站点
/// 画灰的「无请求」，**不是**成功率 0%。
fn model_status_enhancement_payload() -> serde_json::Value {
    const WINDOW_START: i64 = 1750003200;
    const SLOT_SECONDS: i64 = 1800;
    let mut slots = Vec::new();
    for index in 0..48 {
        let start = WINDOW_START + index * SLOT_SECONDS;
        // 第 1 个小时两格 100%/90% 各 10 次 → 加权 95%；
        // 第 2 个小时两格全线失败；第 4 个小时两格显式 0 请求 → 该槽留灰。
        let (rate, requests): (f64, i64) = match index {
            0 => (100.0, 10),
            1 => (90.0, 10),
            2 | 3 => (0.0, 4),
            6 | 7 => (100.0, 0),
            _ => (98.0, 6),
        };
        slots.push(serde_json::json!({
            "slot": index,
            "start_time": start,
            "end_time": start + SLOT_SECONDS,
            "status": "green",
            "total_requests": requests,
            "success_rate": rate,
        }));
    }
    serde_json::json!({
        "success": true,
        "generated_at": WINDOW_START + 48 * SLOT_SECONDS,
        "ready": true,
        "refresh_failed": false,
        "data": [
            {
                "model_name": "grok-4.7",
                "display_name": "Grok 4.7",
                "group_name": "coding-plus",
                "total_requests": 120,
                "success_count": 117,
                "error_count": 3,
                "success_rate": 97.5,
                "current_status": "green",
                "recent_avg_first_response_time": 1500,
                "recent_avg_output_token_speed": 42.5,
                "slot_data": slots
            },
            {
                // 筛掉低请求模型后仍可能回传的空壳：没有流量就不该有健康度条目。
                "model_name": "quiet-model",
                "total_requests": 0,
                "success_rate": 100,
                "slot_data": [{
                    "slot": 0,
                    "start_time": WINDOW_START,
                    "end_time": WINDOW_START + SLOT_SECONDS,
                    "status": "green",
                    "total_requests": 0,
                    "success_rate": 100
                }]
            }
        ]
    })
}

#[test]
fn parses_model_status_enhancement_into_model_health() {
    let health = parse_model_status_health(&model_status_enhancement_payload());
    assert_eq!(
        health.len(),
        1,
        "无流量模型不该产出健康度条目，实际 {:?}",
        health.keys().collect::<Vec<_>>()
    );
    let grok = health.get("grok-4.7").expect("grok-4.7 缺少健康度");

    // 0~100 百分数必须归一到 0~1，与 perf-metrics 保持同一口径；
    // 按 0~1 解读会让界面只剩 0 和 100 两个值。
    assert!(
        (grok.success_rate.unwrap() - 0.975).abs() < 1e-12,
        "站点发的是 97.5（百分数），应归一成 0.975，实际 {:?}",
        grok.success_rate
    );
    assert_eq!(
        grok.avg_latency_ms,
        Some(1500),
        "近期平均首字延迟应映射成平均延迟"
    );
    assert_eq!(grok.avg_tps, Some(42.5));
    assert_eq!(
        grok.requests,
        Some(120),
        "窗口请求总数要带回来，界面靠它说明成功率的分母"
    );
    assert_eq!(grok.window_hours, 24, "24 小时窗口");
    assert_eq!(grok.window_start, Some(1750003200), "起点取最早时间格");
}

#[test]
fn model_status_slots_roll_up_into_fixed_strip_buckets() {
    let health = parse_model_status_health(&model_status_enhancement_payload());
    let grok = health.get("grok-4.7").expect("grok-4.7 缺少健康度");
    let series = &grok.series;
    let base = 1750003200;

    // 30 分钟的时间格必须并进固定的 24 槽：第 1 槽 (100%×10 + 90%×10)/20 = 95%，
    // 槽内按请求数加权而不是简单平均。
    let first = series
        .iter()
        .find(|point| point.ts == base)
        .expect("首个槽位应有数据");
    assert!(
        (first.success_rate.unwrap() - 0.95).abs() < 1e-9,
        "槽内应按请求数加权，实际 {:?}",
        first.success_rate
    );
    assert_eq!(first.requests, Some(20));

    // 全线失败的槽照实下发 0%——它与「无流量」必须是两回事。
    let second = series
        .iter()
        .find(|point| point.ts == base + 3600)
        .expect("第 2 槽应有数据");
    assert_eq!(second.success_rate, Some(0.0));
    assert_eq!(second.requests, Some(8));

    // 显式 0 请求的两格所在槽不下发数据点 → 界面留灰，不能画成 0%。
    assert!(
        !series.iter().any(|point| point.ts == base + 3 * 3600),
        "无流量槽不该被补成 0%"
    );
    assert_eq!(series.len(), 23, "24 槽里应恰好只有 1 个灰槽");

    let timestamps = series.iter().map(|point| point.ts).collect::<Vec<_>>();
    let mut sorted = timestamps.clone();
    sorted.sort();
    assert_eq!(timestamps, sorted, "序列必须按时间升序供界面按序铺槽");
}

#[test]
fn model_status_window_follows_real_span_so_the_newest_slot_survives() {
    // 时间格粒度管理员可配（此处 1440 分钟 = 1 天）：窗口必须按真实跨度算，
    // 否则最近那一格会被挤出窗口——而它恰恰是用户最关心的「现在」。
    const WINDOW_START: i64 = 1750003200;
    const DAY: i64 = 86400;
    let slots: Vec<serde_json::Value> = (0..7)
        .map(|day| {
            serde_json::json!({
                "start_time": WINDOW_START + day * DAY,
                "end_time": WINDOW_START + (day + 1) * DAY,
                "total_requests": 5,
                "success_rate": 90.0
            })
        })
        .collect();
    let payload = serde_json::json!({
        "success": true,
        "data": [{
            "model_name": "glm-5.3-200k",
            "total_requests": 35,
            "success_rate": 90.0,
            "slot_data": slots
        }]
    });

    let health = parse_model_status_health(&payload);
    let model = health
        .get("glm-5.3-200k")
        .expect("glm-5.3-200k 缺少健康度");
    assert_eq!(model.window_hours, 168, "7 天跨度应是 168 小时");
    assert_eq!(model.window_start, Some(WINDOW_START));
    assert_eq!(
        model.series.len(),
        7,
        "7 个相隔一天的时间格应落在 7 个不同的槽"
    );
    // 序列点的 ts 是**槽**起点而非时间格起点：最近一格落在第 20 槽，
    // 只要它没被挤出 24 槽窗口，界面就能画出「现在」的状态。
    let step = model.window_hours * 3600 / MODEL_STATUS_SLOT_COUNT;
    assert!(
        6 * DAY < 24 * step,
        "窗口必须覆盖全部时间格，实际 step={step}"
    );
    assert_eq!(
        model.series.last().map(|point| point.ts),
        Some(WINDOW_START + (6 * DAY / step) * step),
        "最近一格必须留在窗口内"
    );
}

#[test]
fn site_model_health_parser_picks_the_right_shape() {
    // 同一个入口要能吃下两种响应：先按 perf-metrics 形状解析，
    // 解析不出再试「模型状态」增强模块的形状，都失败才认定本站没有健康度。
    let perf = serde_json::from_str(PERF_METRICS_SUMMARY_JSON).unwrap();
    assert_eq!(parse_site_model_health(&perf).len(), 3);

    assert_eq!(parse_site_model_health(&model_status_enhancement_payload()).len(), 1);

    // 路由不存在（404 错误体）/ 未登录，一律空映射、绝不报错。
    for payload in [
        serde_json::json!({"message": "not found", "success": false}),
        serde_json::json!({"message": "Unauthorized, not logged in and no access token provided", "success": false}),
        serde_json::json!({"success": true, "data": {"models": []}}),
    ] {
        assert!(
            parse_site_model_health(&payload).is_empty(),
            "软失败形状不该产出健康度：{payload}"
        );
    }
}

#[test]
fn chrome_key_models_bridge_also_fetches_perf_metrics() {
    let script = chrome_key_models_bridge_script(true, "10288", "/api/token/?p=1&size=20");
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
fn chrome_key_models_bridge_falls_back_to_model_status_module() {
    let script = chrome_key_models_bridge_script(true, "10288", "/api/token/?p=1&size=20");

    // x666 这类魔改 NewAPI 没有 perf-metrics（404 Invalid URL），桥接必须在
    // 同一套会话里改拉「模型状态」增强模块，且顺序固定：先要登录态的全量接口
    // （普通账号会被站点按角色拒掉），再试站点开放的公开嵌入接口。
    let mut cursor = 0usize;
    for path in [
        "\"/api/enhancements/model-status/status/all\"",
        "\"/api/enhancements/model-status/embed/status/all\"",
    ] {
        let index = script
            .find(path)
            .unwrap_or_else(|| panic!("桥接脚本缺少 {path} 的回退请求"));
        assert!(index > cursor, "{path} 应排在上一条之后");
        cursor = index;
    }

    // 回退只在原生接口没拿到时才发：先要 perf-metrics，失败才走增强模块。
    let perf_fetch = script
        .find("fetch(\"/api/perf-metrics/summary")
        .expect("桥接脚本没有拉取性能指标");
    assert!(
        perf_fetch < cursor,
        "先要原生 perf-metrics，拿不到才回退到增强模块"
    );

    // 登录态接口对普通账号可能返回 200 + success:false（x666 回「无权进行此
    // 操作」），这种响应绝不能被存成健康度，否则界面多出一坨解析不出的空数据。
    assert!(
        script.contains("payload.success !== false"),
        "桥接必须把 success:false 认成失败并继续尝试公开嵌入接口"
    );
}

#[test]
fn chrome_key_models_bridge_health_cannot_break_the_main_flow() {
    let script = chrome_key_models_bridge_script(true, "10288", "/api/token/?p=1&size=20");
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

/// 自动签到范围：在用 + 待定（都算「自己会用的站」），未在用不代执行。
#[test]
fn checkin_scope_covers_personal_and_pending_sites() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE directory_sites (
                id TEXT PRIMARY KEY,
                is_personal INTEGER NOT NULL DEFAULT 0,
                is_pending INTEGER NOT NULL DEFAULT 0,
                supports_checkin INTEGER NOT NULL DEFAULT 0
             );
             INSERT INTO directory_sites (id, is_personal, is_pending, supports_checkin) VALUES
                ('personal-checkin', 1, 0, 1),
                ('pending-checkin', 0, 1, 1),
                ('idle-checkin', 0, 0, 1),
                ('pending-no-checkin', 0, 1, 0),
                ('personal-no-checkin', 1, 0, 0);",
        )
        .unwrap();
    let mut statement = connection
        .prepare(crate::site::sync::usage::CHECKIN_SITE_IDS_SQL)
        .unwrap();
    let mut ids: Vec<String> = statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    ids.sort();
    assert_eq!(
        ids,
        vec!["pending-checkin".to_string(), "personal-checkin".to_string()],
        "仅「在用/待定 + 支持签到」参与自动签到"
    );
}

/// 一次性探测（默认忽略）：fengwind 模型市场接口的真实形状与 auth_token。
#[tokio::test]
#[ignore]
async fn probe_fengwind_model_market() {
    use std::time::Duration;

    let home = std::env::var("HOME").expect("HOME");
    let home_path = std::path::PathBuf::from(&home);
    let origin = "https://api.fengwind.com";
    let target = crate::site::sync::LocalStorageTarget {
        site_id: "fw".into(),
        profile_id: "Profile 11".into(),
        origin: origin.into(),
    };
    let matches = crate::site::sync::read_local_storage_from_home(&home_path, &[target]);
    println!("PROBE local matches={}", matches.len());
    let values = matches.first().map(|m| &m.values).cloned().unwrap_or_default();
    let keys: Vec<String> = values.keys().map(|k| {
        let chars: Vec<char> = k.chars().collect();
        if chars.len() > 40 { format!("{}...", chars[..40].iter().collect::<String>()) } else { k.clone() }
    }).collect();
    println!("PROBE storage keys={keys:?}");
    let auth_token = values
        .get("auth_token")
        .or_else(|| values.values().find(|v| v.starts_with("sk-") || !v.is_empty()))
        .cloned()
        .unwrap_or_default();
    println!("PROBE auth_token len={}", auth_token.len());
    if auth_token.is_empty() { return; }

    let client = wreq::Client::builder()
        .timeout(Duration::from_secs(20))
        .no_proxy()
        .build()
        .expect("probe client");
    let url = "https://api.fengwind.com/api/v1/model-market?group_by=model&sort_by=model&sort_order=asc&page=1&page_size=100&timezone=Asia%2FShanghai";
    let response = client
        .get(url)
        .header("accept", "application/json")
        .header("authorization", format!("Bearer {auth_token}"))
        .send()
        .await
        .expect("请求 model-market");
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    std::fs::write("/tmp/fw_market_full.json", &text).unwrap();
    println!("PROBE raw status={status} bytes={}", text.len());
    let expected_total = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.pointer("/data/total").and_then(serde_json::Value::as_u64))
        .unwrap_or(0);
    let health = crate::model::catalog::fetch_sub2api_model_market_health(
        &client,
        &url::Url::parse("https://api.fengwind.com/").unwrap(),
        &auth_token,
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/133.0.0.0 Safari/537.36",
    )
    .await;
    let with_series = health.values().filter(|h| !h.series.is_empty()).count();
    let mut names: Vec<&String> = health.keys().collect();
    names.sort();
    println!(
        "PROBE 分页聚合后 models={} (站点 total={expected_total}) with_series={with_series}",
        health.len()
    );
    println!("PROBE 样例模型(前10)={:?}", &names[..names.len().min(10)]);
    for probe in ["bge-m3", "claude-opus-4-8", "gemini-2.5-flash"] {
        if let Some(h) = health.get(probe) {
            println!(
                "PROBE {probe}: rate={:?} series={} window_h={} req={:?}",
                h.success_rate, h.series.len(), h.window_hours, h.requests
            );
        } else {
            println!("PROBE {probe}: 缺失");
        }
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
        if let Some(data) = value.get("data") {
            println!("PROBE data keys={:?}", data.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()));
            if let Some(channels) = data.get("channels").and_then(serde_json::Value::as_array) {
                println!("PROBE channels={}", channels.len());
            }
            if let Some(models) = data.get("models").and_then(serde_json::Value::as_array) {
                println!("PROBE models={}", models.len());
                if let Some(first) = models.first() {
                    println!("PROBE model[0]={}", serde_json::to_string(first).unwrap_or_default().chars().take(900).collect::<String>());
                }
            } else {
                if let Some(object) = data.as_object() {
                    for (k, v) in object {
                        let text = serde_json::to_string(v).unwrap_or_default();
                        println!("PROBE data.{k} len={} head={}", text.len(), text.chars().take(140).collect::<String>());
                    }
                }
            }
        }
    }
}

/// 一次性探测（默认忽略）：sub2api 通道（collect_site_model_health）真实站点写库。
#[tokio::test]
#[ignore]
async fn probe_fengwind_collect_health() {
    use std::time::{Duration, Instant};

    let database = health_test_database();
    let site_id = "fengwind-probe";
    let profile_id = "Profile 11";
    let home2 = std::env::var("HOME").expect("HOME");
    {
        let connection = database.0.lock().unwrap();
        connection
            .execute(
                "INSERT INTO site_model_cache (site_id, profile_id) VALUES (?1, ?2)",
                rusqlite::params![site_id, profile_id],
            )
            .unwrap();
    }
    let client = wreq::Client::builder()
        .timeout(Duration::from_secs(20))
        .no_proxy()
        .build()
        .expect("probe client");
    let base = url::Url::parse("https://api.fengwind.com/").unwrap();
    let started = Instant::now();
    let t0 = Instant::now();
    let token = crate::site::sync::read_local_storage_from_home(
        &std::path::PathBuf::from(&home2),
        &[crate::site::sync::LocalStorageTarget {
            site_id: "fw".into(),
            profile_id: profile_id.into(),
            origin: "https://api.fengwind.com".into(),
        }],
    );
    println!(
        "PROBE LS 读取 {:.2}s keys={:?}",
        t0.elapsed().as_secs_f64(),
        token.first().map(|m| m.values.len()).unwrap_or(0)
    );
    let auth_token = token
        .first()
        .and_then(|m| m.values.get("auth_token").cloned())
        .map(|v| crate::site::sync::local_scalar(&v))
        .unwrap_or_default();
    let t1 = Instant::now();
    let direct = crate::model::catalog::fetch_sub2api_model_market_health(
        &client,
        &base,
        &auth_token,
        &crate::site::sync::chrome_user_agent(),
    )
    .await;
    println!("PROBE 直调 fetch {:.2}s models={}", t1.elapsed().as_secs_f64(), direct.len());
    let health = crate::model::catalog::collect_site_model_health(
        &database,
        &client,
        &base,
        site_id,
        "sub2api",
        &[profile_id.to_string()],
    )
    .await
    .expect("collect 不应报错");
    let stored = read_health_json(&database, site_id, profile_id);
    println!(
        "PROBE collect models={} stored_bytes={} elapsed={:.1}s",
        health.len(),
        stored.len(),
        started.elapsed().as_secs_f64()
    );
    let mut names: Vec<&String> = health.keys().collect();
    names.sort();
    println!("PROBE 前 10: {:?}", &names[..names.len().min(10)]);
    assert!(!health.is_empty(), "sub2api 通道应抓到模型市场健康度");
    assert_ne!(stored, "{}", "健康度应写入 health_json");
}
