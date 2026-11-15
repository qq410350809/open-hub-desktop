//! HTTP handler tests use an in-memory database and never contact configured sites.
use super::*;
use crate::context::{Capabilities, EventBus, LoginManager};
use crate::models::{Database, SiteRecord};

fn service() -> Arc<ServerShared> {
    let database = Arc::new(Database::open(Path::new(":memory:")).unwrap());
    {
        let mut connection = database.lock_conn().unwrap();
        let transaction = connection.transaction().unwrap();
        crate::db::insert_site_transaction(
            &transaction,
            &SiteRecord {
                id: "rpc-site".into(),
                name: "RPC site".into(),
                api_base_url: "https://example.invalid/".into(),
                ..SiteRecord::default()
            },
        )
        .unwrap();
        for profile in ["A", "B"] {
            transaction.execute(
                "INSERT INTO site_accounts (site_id, profile_id, domain, cookie_count, cookie_names, profile_name, account_name)
                 VALUES ('rpc-site', ?1, 'example.invalid', 1, '[]', ?1, ?1)",
                [profile],
            ).unwrap();
        }
        transaction.commit().unwrap();
    }
    let data_dir = std::env::temp_dir().join("openhub-site-rpc-tests");
    let ctx = Arc::new(AppContext {
        database,
        proxy_runtime: Arc::new(crate::proxypool::ProxyRuntime::new(data_dir.join("proxy"))),
        charity_runtime: Arc::new(crate::charity::CharityMonitorRuntime::new()),
        model_catalog_runtime: Arc::new(crate::model::catalog::ModelCatalogRuntime::new()),
        event_bus: EventBus::new(),
        data_dir: data_dir.clone(),
        resource_dir: None,
        capabilities: Capabilities::server_defaults(),
        login: LoginManager::new("admin".into(), "test-password".into()),
    });
    ServerShared::new(ctx, ModelProxyState::new(), data_dir.join("dist"))
}

async fn rpc(
    shared: &Arc<ServerShared>,
    token: Option<&str>,
    command: &str,
    args: Value,
) -> (StatusCode, Value) {
    let mut headers = HeaderMap::new();
    if let Some(token) = token {
        headers.insert("x-openhub-token", token.parse().unwrap());
    }
    let response = rpc_handler(
        State(shared.clone()),
        headers,
        Bytes::from(serde_json::to_vec(&json!({"command":command, "args":args})).unwrap()),
    )
    .await;
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn site_cache_rpc_add_read_remove_preserves_other_accounts() {
    let shared = service();
    let token = shared.ctx.login.create_session().unwrap();
    let (_, library) = rpc(&shared, Some(&token), "list_library", json!({})).await;
    assert!(library["data"]["Ok"]["sites"]
        .as_array()
        .unwrap()
        .iter()
        .any(|site| site["id"] == "rpc-site"));
    for (profile, key) in [("A", "test-key-a"), ("B", "test-key-b")] {
        let (_, added) = rpc(
            &shared,
            Some(&token),
            "add_site_model_cache_key",
            json!({
                "siteId":"rpc-site", "profileId":profile, "key":key,
                "groupName":"test-group", "profileName":profile, "username":profile,
            }),
        )
        .await;
        assert_eq!(added, json!({"data":{"Ok":true}}));
    }
    let (_, duplicate) = rpc(
        &shared,
        Some(&token),
        "add_site_model_cache_key",
        json!({
            "site_id":"rpc-site", "profile_id":"A", "key":"test-key-a",
        }),
    )
    .await;
    assert_eq!(duplicate, json!({"data":{"Ok":false}}));
    let (_, cached) = rpc(
        &shared,
        Some(&token),
        "get_site_model_cache",
        json!({"siteId":"rpc-site"}),
    )
    .await;
    let accounts = cached["data"]["Ok"]["accounts"].as_array().unwrap();
    let account_a = accounts
        .iter()
        .find(|account| account["profileId"] == "A")
        .unwrap();
    assert_eq!(account_a["keys"], json!(["test-key-a"]));
    assert_eq!(account_a["keyGroups"]["test-key-a"], "test-group");

    let (_, removed) = rpc(
        &shared,
        Some(&token),
        "remove_site_model_cache_key",
        json!({
            "site_id":"rpc-site", "profile_id":"A", "key":"test-key-a",
        }),
    )
    .await;
    assert_eq!(removed, json!({"data":{"Ok":true}}));
    let (_, cached) = rpc(
        &shared,
        Some(&token),
        "get_site_model_cache",
        json!({"site_id":"rpc-site"}),
    )
    .await;
    let accounts = cached["data"]["Ok"]["accounts"].as_array().unwrap();
    let account_a = accounts
        .iter()
        .find(|account| account["profileId"] == "A")
        .unwrap();
    let account_b = accounts
        .iter()
        .find(|account| account["profileId"] == "B")
        .unwrap();
    assert_eq!(account_a["keys"], json!([]));
    assert_eq!(account_b["keys"], json!(["test-key-b"]));
}

#[tokio::test]
async fn site_model_rpc_uses_domain_errors_and_validates_parameters() {
    let shared = service();
    let token = shared.ctx.login.create_session().unwrap();
    let (_, no_keys) = rpc(
        &shared,
        Some(&token),
        "sync_models_for_cached_keys",
        json!({"siteId":"rpc-site"}),
    )
    .await;
    assert!(no_keys["data"]["Err"]
        .as_str()
        .unwrap()
        .contains("请先添加 Key"));
    for command in ["sync_models_for_cached_keys", "refresh_site_model_health"] {
        let (_, missing_site) =
            rpc(&shared, Some(&token), command, json!({"site_id":"missing"})).await;
        assert_eq!(missing_site, json!({"data":{"Err":"站点不存在"}}));
        let (_, missing_arg) = rpc(&shared, Some(&token), command, json!({})).await;
        assert_eq!(missing_arg, json!({"error":"缺少参数：siteId"}));
    }
    let (_, invalid) = rpc(
        &shared,
        Some(&token),
        "add_site_model_cache_key",
        json!({
            "siteId":"rpc-site", "profileId":"A", "key":"test-key", "groupName":7,
        }),
    )
    .await;
    assert!(invalid["error"]
        .as_str()
        .unwrap()
        .contains("参数 groupName 格式错误"));
}

#[tokio::test]
async fn site_cache_rpc_requires_a_valid_management_session() {
    let shared = service();
    for command in [
        "sync_models_for_cached_keys",
        "refresh_site_model_health",
        "add_site_model_cache_key",
        "remove_site_model_cache_key",
    ] {
        let (status, body) = rpc(&shared, None, command, json!({"siteId":"rpc-site"})).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["code"], "AUTH_REQUIRED");
    }
}
