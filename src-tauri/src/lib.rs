mod account_sync;
mod charity_monitor;
mod chrome_local_storage;
mod chrome_session;
mod chrome_usage;
mod db;
mod models;
mod models_fetch;
mod proxy_pool;
mod remote_sync;
mod site_crud;
mod site_ops;
mod system_detect;
mod token_stats;

use models::*;

#[cfg(test)]
use account_sync::*;
#[cfg(test)]
use db::*;
#[cfg(test)]
use models_fetch::*;
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
                    newapi_user_id TEXT NOT NULL DEFAULT ''
                );
                CREATE TABLE site_model_cache (
                    site_id TEXT NOT NULL,
                    profile_id TEXT NOT NULL,
                    error TEXT NOT NULL DEFAULT '',
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
    fn refreshes_access_tokens_only_after_http_401() {
        assert!(access_token_was_rejected("账号接口 HTTP 401：访问令牌无效"));
        assert!(!access_token_was_rejected(
            "账号接口 HTTP 403：Cloudflare 安全验证"
        ));
        assert!(!access_token_was_rejected("账号接口请求失败：连接超时"));
        assert!(!access_token_was_rejected("账号接口返回的 JSON 无法解析"));
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
        assert!(script.contains("keyResponse.status === 401"));
        assert!(!script.contains("!keyResponse.ok || extractKeys(keyResponse.data).length === 0"));
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
            true,
        );

        assert!(script.contains("fetch(\"/api/user/auth/refresh\""));
        assert!(script
            .contains("method: \"POST\", credentials: \"include\", cache: \"no-store\", headers"));
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
        assert!(script.contains("const allowChallengeNavigation = true"));
        assert!(script.contains("return \"__OPENHUB_PROFILE_MISMATCH__\""));
        assert!(
            script.find("const checkinResponse").unwrap()
                < script.find("fetch(\"/api/user/self\"").unwrap()
        );
        assert!(script.contains("const requestTimeout = 30000"));
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
        assert!(!script.contains("isAnyRouter"));
        assert!(!script.contains("fetch(\"/api/user/sign_in\""));
        assert!(script.contains("`/api/user/checkin?month=${encodeURIComponent(\"2026-08\")}`"));
        assert!(script.contains("fetch(\"/api/user/checkin\""));
        assert!(script.contains("method: \"POST\""));
        assert_eq!(script.matches("fetch(").count(), 5);
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
            let app_data_dir = app
                .path()
                .app_data_dir()
                .map_err(|error| error.to_string())?;
            fs::create_dir_all(&app_data_dir)?;
            let database = Database::open(&app_data_dir.join("sites.sqlite3"))
                .map_err(std::io::Error::other)?;
            let proxy_runtime = proxy_pool::ProxyRuntime::new(app_data_dir.join("proxy-runtime"));
            let charity_runtime = charity_monitor::CharityMonitorRuntime::new();
            app.manage(database);
            app.manage(proxy_runtime);
            app.manage(charity_runtime);

            // 启动阶段禁止阻塞 UI 线程：
            // 1) 恢复代理在后台
            // 2) 公益监听延后启动
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
                charity_monitor::start_charity_monitor(restore_handle);
            });

            Ok(())
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
            site_crud::toggle_hidden,
            site_crud::toggle_runaway,
            proxy_pool::get_proxy_pool_state,
            proxy_pool::analyze_proxy_nodes,
            proxy_pool::save_proxy_subscription,
            proxy_pool::delete_proxy_subscription,
            proxy_pool::refresh_proxy_subscription,
            proxy_pool::set_proxy_pool_settings,
            proxy_pool::set_active_proxy_node,
            proxy_pool::clear_active_proxy_node,
            proxy_pool::delete_invalid_proxy_nodes,
            proxy_pool::test_proxy_node,
            proxy_pool::test_proxy_nodes,
            proxy_pool::test_all_proxy_nodes,
            proxy_pool::cancel_proxy_node_tests,
            remote_sync::get_remote_user,
            chrome_usage::mark_sites_with_chrome_sessions,
            account_sync::sync_site_account_via_chrome,
            remote_sync::sync_remote_sites,
            system_detect::detect_site_system_types,
            models_fetch::get_system_fonts,
            models_fetch::fetch_site_models_json,
            models_fetch::get_site_model_cache,
            models_fetch::clear_site_model_cache_for_site,
            models_fetch::save_site_model_cache_for_account,
            chrome_session::list_chrome_sessions,
            chrome_session::read_chrome_session,
            chrome_session::open_url_in_chrome_profile,
            charity_monitor::get_charity_feed,
            charity_monitor::fetch_charity_feed,
            charity_monitor::mark_charity_feed_read,
            charity_monitor::get_charity_unread_total,
            charity_monitor::get_charity_sync_logs,
            charity_monitor::clear_charity_sync_logs,
            charity_monitor::set_charity_monitor_visible,
            charity_monitor::request_charity_round,
            charity_monitor::refresh_all_charity_feeds,
            token_stats::get_token_stats,
            token_stats::sync_token_tracker,
            token_stats::get_token_usage,
            token_stats::get_token_raw_logs,
            token_stats::get_token_request_health
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}
