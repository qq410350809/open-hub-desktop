mod account_sync;
pub mod app_menu;
mod auto_sync;
mod charity_monitor;
mod chrome_local_storage;
mod chrome_session;
mod chrome_usage;
mod db;
mod detect_all;
mod file_export;
mod geoip;
mod mihomo_kernel;
mod models;
pub use detect_all::run_library_detect;
mod model_catalog;
pub use model_catalog::sync_model_catalog_once;
mod models_fetch;
mod opencode_proxy;
mod platform_detect;
mod proxy_pool;
mod remote_sync;
mod single_instance;
mod site_crud;
mod site_ops;
mod system_detect;
mod token_collector;
mod token_stats;
mod web_server;

use models::*;

#[cfg(test)]
use account_sync::*;
#[cfg(test)]
use db::*;
#[cfg(test)]
use models_fetch::*;
#[cfg(test)]
use platform_detect::*;
#[cfg(test)]
use site_ops::*;

use std::fs;
use tauri::Manager;

#[cfg(test)]
mod tests {
    use super::*;
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
                    error TEXT NOT NULL DEFAULT '',
                    keys_json TEXT NOT NULL DEFAULT '[]',
                    models_json TEXT NOT NULL DEFAULT '[]',
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
        let keys =
            format!("Sub2API Key 接口 HTTP 401：Token has expired{SUB2API_AUTH_FAILURE_HINT}");
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
                    { "key": "sk-****masked" }
                ]
            }
        });
        assert_eq!(
            parse_api_keys(&sub2api),
            ["raw-key-value", "sk-sub2api-enabled", "sub2-raw-key-value"]
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
            true,
        );

        assert!(script.contains("fetch(\"/api/user/auth/refresh\""));
        assert!(script
            .contains("method: \"POST\", credentials: \"include\", cache: \"no-store\", headers"));
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
            script.find("const checkinResponse").unwrap()
                < script.find("fetch(\"/api/user/self\"").unwrap()
        );
        assert!(script
            .contains("method: \"GET\", credentials: \"include\", cache: \"no-store\", headers"));
        // 访问令牌逻辑只允许在 useRefreshAuth 分支内执行。
        assert!(
            script.find("if (useRefreshAuth) {").unwrap()
                < script.find("fetch(\"/api/user/token\"").unwrap()
        );
        assert!(script.contains("const useSessionCookies = !apiToken"));
        assert!(script.contains("if (useSessionCookies) {"));
        assert!(script.contains("const requestTimeout = 30000"));
        assert_eq!(
            script
                .matches("AbortSignal.timeout(requestTimeout)")
                .count(),
            6
        );
        assert!(!script.contains("account: accessToken"));
        assert!(!script.contains("if (Date.now() - previous.started < 3000) return pending;"));
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
        let script =
            chrome_account_bridge_script(Some(user_id), month, marker, false, false, false);

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
            #[cfg(target_os = "macos")]
            {
                // 让系统/WebKit 优先走简体中文资源（对部分系统菜单项生效）。
                let _ = std::process::Command::new("defaults")
                    .args([
                        "write",
                        "com.dfeer.openhub.desktop",
                        "AppleLanguages",
                        "-array",
                        "zh-Hans",
                        "en",
                    ])
                    .status();
            }
            app_menu::install_chinese_menu(app)?;

            // 菜单刷新：文件 → 刷新 → 后端直接全量刷新 + 通知前端刷新 UI。
            app.on_menu_event(move |app_handle, event| {
                if event.id() == "file-refresh" {
                    eprintln!("[OpenHub] 菜单 file-refresh 触发");
                    let handle = app_handle.clone();
                    tauri::async_runtime::spawn(async move {
                        let database = handle.state::<crate::models::Database>();
                        let monitor =
                            handle.state::<crate::charity_monitor::CharityMonitorRuntime>();
                        match crate::charity_monitor::refresh_all_charity_feeds(database, monitor)
                            .await
                        {
                            Ok(_) => {
                                eprintln!("[OpenHub] 全量刷新已提交");
                            }
                            Err(err) => {
                                eprintln!("[OpenHub] 全量刷新失败：{err}");
                            }
                        }
                        let _ = tauri::Emitter::emit(&handle, "menu-refresh-requested", ());
                    });
                }
            });

            let app_data_dir = app
                .path()
                .app_data_dir()
                .map_err(|error| error.to_string())?;
            fs::create_dir_all(&app_data_dir)?;
            // 先关掉旧实例，再开数据库/绑端口，避免端口顺延导致浏览器指向旧实例。
            single_instance::claim(&app_data_dir);
            let database = Database::open(&app_data_dir.join("sites.sqlite3"))
                .map_err(std::io::Error::other)?;
            // 升级阶段先把现有采集缓存迁入 SQLite，页面首次查询即可得到完整快照。
            if let Err(error) = token_stats::seed_token_database_from_caches(&database) {
                eprintln!("[OpenHub] Token 缓存迁移到数据库失败：{error}");
            }
            // 首次启动时若 AppData 尚无文件，先秒级释放安装包自带的内置基础版内核与 GeoIP 数据库
            if let Err(e) = crate::mihomo_kernel::ensure_bundled_assets_installed(app.handle()) {
                eprintln!("[OpenHub] 释放内置资源提示：{e}");
            }
            let proxy_runtime = proxy_pool::ProxyRuntime::new(app_data_dir.join("proxy-runtime"));
            let charity_runtime = charity_monitor::CharityMonitorRuntime::new();
            let auto_sync_runtime = auto_sync::AutoSyncRuntime::default();
            let model_catalog_runtime = model_catalog::ModelCatalogRuntime::new();
            let opencode_proxy_state =
                opencode_proxy::OpencodeProxyState::new_with_app(Some(app.handle().clone()));
            app.manage(database);
            app.manage(proxy_runtime);
            app.manage(charity_runtime);
            app.manage(auto_sync_runtime);
            app.manage(model_catalog_runtime);
            app.manage(opencode_proxy_state);
            // 启动时清理历史订阅里遗留的测速结果后缀，避免旧库节点名继续显示脏数据。
            if let Err(error) =
                proxy_pool::repair_stored_node_names(&app.state::<crate::models::Database>())
            {
                eprintln!("[OpenHub] 修复代理节点名称失败：{error}");
            }
            // Token 采集与页面查询完全解耦：后台每 20 秒增量入库。
            token_stats::start_token_collector(app.handle().clone());

            // 轻量模式：常驻本地 HTTP 服务（浏览器访问内核）。
            let web_server = match web_server::start(app.handle().clone()) {
                Ok(handle) => handle,
                Err(error) => {
                    eprintln!("OpenHub 轻量模式服务启动失败：{error}");
                    web_server::WebServerHandle::disabled()
                }
            };
            app.manage(web_server);
            web_server::apply_startup_lightweight_mode(app.handle());

            // 启动阶段禁止阻塞 UI 线程：
            // 1) 恢复代理在后台
            // 2) 检查模型参数当天是否已同步
            // 3) 公益监听延后启动
            // 前端启动后调用 sync_model_catalog(false)：后端以本地日期判断当天是否已同步；
            // 页面保持打开跨过午夜时，前端计时器会再次调用同一命令。

            let restore_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                let result = tauri::async_runtime::spawn_blocking({
                    let restore_handle = restore_handle.clone();
                    move || {
                        let database = restore_handle.state::<crate::models::Database>();
                        let runtime = restore_handle.state::<crate::proxy_pool::ProxyRuntime>();
                        proxy_pool::restore_saved_proxy(&database, &runtime);
                    }
                })
                .await;
                if let Err(error) = result {
                    eprintln!("OpenHub 后台恢复代理失败：{error}");
                }
                // 代理恢复后再启动公益监听，避免启动瞬间抢锁/抢内核。
                // 前端 onMounted 会 request_charity_round，循环启动后立刻消费 force。
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                charity_monitor::start_charity_monitor(restore_handle.clone());
                // 自动会话同步：账号保活 / 失效恢复 / 模型刷新全程后台化，
                // 与公益监听错开启动（调度器内部还有首轮延迟）。
                auto_sync::start_auto_sync(restore_handle.clone());

                // 启动 OpenCode 独立反代服务
                let database = restore_handle.state::<crate::models::Database>();
                let proxy_state = restore_handle.state::<crate::opencode_proxy::OpencodeProxyState>();
                let proxy_cfg = {
                    let conn = database.0.lock().ok();
                    conn.map(|c| opencode_proxy::load_opencode_proxy_config(&c)).unwrap_or_default()
                };
                *proxy_state.context.config.write().await = proxy_cfg.clone();
                if proxy_cfg.enabled {
                    if let Err(e) = opencode_proxy::start_opencode_proxy_server(&proxy_state).await {
                        eprintln!("[OpenHub] OpenCode 反代服务启动失败: {e}");
                    }
                }
            });

            // 启动时后台异步检测核心组件是否缺失，若缺失则全自动静默下载
            let auto_download_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(600)).await;

                // 1. 检测 Mihomo 内核
                let has_mihomo = crate::mihomo_kernel::resolve_mihomo_binary(Some(&auto_download_handle)).is_some();
                if !has_mihomo {
                    eprintln!("[OpenHub] 启动组件检测：未检测到 Mihomo 内核，启动后台自动拉取…");
                    match crate::mihomo_kernel::download_or_update_mihomo_kernel(auto_download_handle.clone(), None).await {
                        Ok(status) => eprintln!("[OpenHub] Mihomo 内核自动安装成功 ({})", status.version),
                        Err(e) => eprintln!("[OpenHub] Mihomo 内核自动安装失败：{e}"),
                    }
                }

                // 2. 检测 GeoIP 数据库
                let has_geoip = crate::geoip::get_app_geoip_path(&auto_download_handle)
                    .map(|p| p.is_file())
                    .unwrap_or(false);
                if !has_geoip {
                    eprintln!("[OpenHub] 启动组件检测：未检测到 GeoIP 数据库，启动后台自动拉取…");
                    match crate::geoip::download_or_update_geoip(auto_download_handle.clone(), None).await {
                        Ok(_) => eprintln!("[OpenHub] GeoIP 数据库自动下载成功并已就绪"),
                        Err(e) => eprintln!("[OpenHub] GeoIP 数据库自动下载失败：{e}"),
                    }
                }
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            // 关闭主窗口视为退出整个应用：macOS 默认关窗不退出，
            // 若进程常驻，轻量模式服务会一直占用端口。
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { .. } = event {
                    window.app_handle().exit(0);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            site_crud::list_library,
            site_crud::create_site,
            site_crud::import_site,
            site_crud::update_site,
            site_crud::delete_site,
            site_crud::toggle_personal,
            site_crud::toggle_pending,
            site_crud::cycle_usage_state,
            site_crud::set_usage_state,
            site_crud::toggle_hidden,
            site_crud::toggle_runaway,
            proxy_pool::get_proxy_pool_state,
            proxy_pool::analyze_proxy_nodes,
            proxy_pool::save_proxy_subscription,
            proxy_pool::delete_proxy_subscription,
            proxy_pool::refresh_proxy_subscription,
            proxy_pool::set_proxy_pool_settings,
            proxy_pool::save_proxy_channel,
            proxy_pool::delete_proxy_channel,
            proxy_pool::set_proxy_channel_node,
            proxy_pool::assign_account_proxy_channel,
            proxy_pool::unassign_account_proxy_channel,
            proxy_pool::test_proxy_channel_nodes,
            proxy_pool::set_active_proxy_node,
            proxy_pool::clear_active_proxy_node,
            proxy_pool::delete_invalid_proxy_nodes,
            proxy_pool::test_proxy_node,
            proxy_pool::test_proxy_nodes,
            proxy_pool::test_all_proxy_nodes,
            proxy_pool::cancel_proxy_node_tests,
            remote_sync::get_remote_user,
            chrome_usage::mark_sites_with_chrome_sessions,
            chrome_usage::delete_site_account,
            account_sync::sync_site_account_via_chrome,
            auto_sync::get_auto_sync_settings,
            auto_sync::set_auto_sync_settings,
            auto_sync::get_auto_sync_status,
            auto_sync::request_auto_sync_round,
            remote_sync::sync_remote_sites,
            system_detect::detect_site_system_types,
            models_fetch::get_system_fonts,
            models_fetch::fetch_site_models_json,
            models_fetch::get_site_model_cache,
            models_fetch::get_all_site_model_caches,
            models_fetch::clear_site_model_cache_for_site,
            models_fetch::save_site_model_cache_for_account,
            model_catalog::get_model_catalog,
            model_catalog::get_model_catalog_detail,
            model_catalog::sync_model_catalog,
            chrome_session::list_chrome_sessions,
            chrome_session::read_chrome_session,
            chrome_session::open_url_in_chrome_profile,
            chrome_session::close_chrome_sync_tabs,
            charity_monitor::get_charity_feed,
            charity_monitor::fetch_charity_feed,
            charity_monitor::mark_charity_feed_read,
            charity_monitor::get_charity_unread_total,
            charity_monitor::get_charity_today_count,
            charity_monitor::get_charity_proxy_pool_summary,
            charity_monitor::get_charity_sync_logs,
            charity_monitor::clear_charity_sync_logs,
            charity_monitor::set_charity_monitor_visible,
            charity_monitor::request_charity_round,
            charity_monitor::list_charity_sources,
            charity_monitor::add_charity_source,
            charity_monitor::update_charity_source,
            charity_monitor::remove_charity_source,
            charity_monitor::refresh_all_charity_feeds,
            token_stats::get_token_stats,
            token_stats::sync_token_data,
            token_stats::get_token_usage,
            token_stats::get_token_raw_logs,
            token_stats::get_token_request_health,
            token_stats::get_local_agent_paths,
            web_server::get_lightweight_mode_state,
            web_server::enter_lightweight_mode,
            web_server::show_main_window,
            opencode_proxy::get_opencode_proxy_config,
            opencode_proxy::save_opencode_proxy_config_cmd,
            opencode_proxy::get_opencode_proxy_status,
            opencode_proxy::start_opencode_proxy,
            opencode_proxy::stop_opencode_proxy,
            opencode_proxy::fetch_opencode_models,
            opencode_proxy::test_opencode_proxy_health,
            opencode_proxy::get_opencode_proxy_logs,
            opencode_proxy::get_opencode_channel_stats,
            opencode_proxy::clear_opencode_proxy_logs,
            file_export::save_export_file,
            mihomo_kernel::get_mihomo_kernel_status,
            mihomo_kernel::check_mihomo_kernel_update,
            mihomo_kernel::download_or_update_mihomo_kernel,
            geoip::get_geoip_status,
            geoip::download_or_update_geoip
        ])
        .build(tauri::generate_context!())
        .expect("error while building Tauri application")
        .run(|app_handle, event| {
            // 退出应用时同步停止轻量模式服务，避免端口被常驻进程占用。
            if let tauri::RunEvent::ExitRequested { .. } = event {
                let server = app_handle.state::<std::sync::Arc<web_server::WebServerHandle>>();
                web_server::stop(&server);
            }
            // macOS：点击 Dock 图标时重新显示轻量模式下隐藏的窗口。
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                let _ = web_server::show_main_window(app_handle.clone());
            }
        });
}
