use super::*;
use crate::context::{AppContext, EventBus};
use std::sync::Arc;

#[tokio::test]
async fn log_daily_and_hourly_stats_commit_or_rollback_together() {
    let database =
        Arc::new(crate::models::Database::open(std::path::Path::new(":memory:")).unwrap());
    let app = Arc::new(AppContext {
        database: database.clone(),
        proxy_runtime: Arc::new(crate::proxypool::ProxyRuntime::new(
            std::path::PathBuf::from("unused-test-runtime"),
        )),
        charity_runtime: Arc::new(crate::charity::CharityMonitorRuntime::new()),
        model_catalog_runtime: Arc::new(crate::model::catalog::ModelCatalogRuntime::new()),
        event_bus: EventBus::new(),
        data_dir: std::path::PathBuf::new(),
        resource_dir: None,
        capabilities: crate::context::Capabilities::server_defaults(),
        login: crate::context::LoginManager::new("admin".into(), "password".into()),
    });
    let state = ModelProxyState::new();
    state.attach_ctx(app).await;
    let log = |id: &str| {
        super::super::logger::ProxyLogParams::new_failure(
            id.into(),
            "/v1/chat/completions".into(),
            "test-channel".into(),
            "test-model".into(),
            false,
            502,
            10,
            Some("test upstream error".into()),
            None,
            None,
        )
        .into_log()
    };
    state.context.record_log(log("committed")).await;
    {
        let conn = database.lock_conn().unwrap();
        for table in [
            "model_proxy_logs",
            "channel_daily_stats",
            "channel_hourly_stats",
        ] {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 1, "{table}");
        }
        conn.execute_batch("CREATE TRIGGER fail_hourly BEFORE INSERT ON channel_hourly_stats BEGIN SELECT RAISE(ABORT, 'simulated hourly failure'); END;").unwrap();
    }
    state.context.record_log(log("must-rollback")).await;
    let conn = database.lock_conn().unwrap();
    let logs: i64 = conn
        .query_row("SELECT COUNT(*) FROM model_proxy_logs", [], |row| {
            row.get(0)
        })
        .unwrap();
    let daily: i64 = conn
        .query_row(
            "SELECT SUM(total_requests) FROM channel_daily_stats",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let hourly: i64 = conn
        .query_row(
            "SELECT SUM(total_requests) FROM channel_hourly_stats",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        (logs, daily, hourly),
        (1, 1, 1),
        "a failed hourly insert must rollback both preceding writes"
    );
}
