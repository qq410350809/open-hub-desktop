use super::*;
use crate::models::{ProxyNode, ProxyNodeTestProgress};
use crate::proxypool::runtime::load_state;
use crate::proxypool::tester::{apply_exit_ip_geoip, normalize_test_run_id, write_probe_result};
use crate::proxypool::types::ProxyRuntime;
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const IP: &str = "1.1.1.1";

fn payload() -> Value {
    json!({
        "status": "success", "query": IP,
        "isp": "Example Network", "org": "Example Organization",
        "as": "AS13335 Example Organization",
        "hosting": false, "mobile": false, "proxy": false
    })
}

fn parse(value: Value) -> ProxyIpInfo {
    parse_response(IP, &serde_json::to_vec(&value).unwrap())
}

fn database() -> Database {
    Database::open(std::path::Path::new(":memory:")).unwrap()
}

fn runtime() -> ProxyRuntime {
    // load_state 不启动运行时、不创建目录；如未来需要临时文件，也留在父级 scratch。
    ProxyRuntime::new(
        std::path::PathBuf::from(
            std::env::var_os("PI_SCRATCH_DIR")
                .unwrap_or_else(|| std::env::temp_dir().into_os_string()),
        )
        .join("ip-info-offline-test"),
    )
}

#[test]
fn classifies_hosting_mobile_and_otherwise_residential() {
    let info = parse(payload());
    assert_eq!(
        (info.kind.as_str(), info.status.as_str(), info.asn.as_str()),
        ("residential", "success", "AS13335")
    );
    assert_eq!(info.isp, "Example Network");
    assert_eq!(info.organization, "Example Organization");
    assert_eq!(info.source, "ip-api.com");
    assert!(DateTime::parse_from_rfc3339(&info.checked_at).is_ok());

    let mut value = payload();
    value["hosting"] = json!(true);
    assert_eq!(parse(value).kind, "hosting");
    let mut value = payload();
    value["mobile"] = json!(true);
    assert_eq!(parse(value).kind, "mobile");
    // hosting 优先级高于 mobile：机房 IP 被标记为移动网段时仍按机房展示。
    let mut value = payload();
    value["hosting"] = json!(true);
    value["mobile"] = json!(true);
    assert_eq!(parse(value).kind, "hosting");
}

#[test]
fn anonymous_tier_response_without_type_fields_is_not_misread() {
    // 免费版 ipapi.is 只返回 11 个字段，没有 hosting/mobile；这种响应必须判失败，
    // 而不是因为缺少字段就静默降级成住宅或未知。
    let anonymous = json!({"ip": IP, "is_bogon": false, "company": "Example ISP", "asn": "AS13335 Example"});
    let info = parse_response(IP, &serde_json::to_vec(&anonymous).unwrap());
    assert_eq!((info.kind.as_str(), info.status.as_str()), ("unknown", "error"));
    assert!(!info.error.is_empty());
}

#[test]
fn provider_failure_and_malformed_shapes_are_not_success() {
    let mut value = payload();
    value["status"] = json!("fail");
    value["message"] = json!("invalid query");
    let info = parse(value);
    assert_eq!((info.kind.as_str(), info.status.as_str()), ("unknown", "error"));

    // 字段缺失或类型错误都不算成功。
    for key in ["status", "query", "hosting", "mobile"] {
        let mut value = payload();
        value.as_object_mut().unwrap().remove(key);
        let info = parse(value);
        assert_eq!(info.status, "error", "{key}");
        assert_eq!(info.kind, "unknown", "{key}");
    }
    let mut value = payload();
    value["hosting"] = json!("false");
    assert_eq!(parse(value).status, "error");

    // 非 JSON、超长响应与不可解析内容都按失败处理。
    assert_eq!(parse_response(IP, b"<html>error</html>").status, "error");
    assert_eq!(parse_response(IP, &vec![b' '; BODY_LIMIT + 1]).status, "error");

    // 缺失 ISP/组织/AS 时仍可分类，只是详情为空，不能变成失败。
    let info = parse_response(
        IP,
        br#"{"status":"success","query":"1.1.1.1","hosting":false,"mobile":false}"#,
    );
    assert_eq!(info.kind, "residential");
    assert!(info.asn.is_empty());
    assert!(info.organization.is_empty());
}

#[test]
fn real_free_tier_response_is_classified_correctly() {
    // 以下三条是 2026-09 实际抓取的免费响应（经代理访问），用于防止再次
    // 只按文档格式写测试、却与线上返回不一致。
    let hosting = br#"{"status":"success","country":"United States","isp":"Google LLC","org":"Google Public DNS","as":"AS15169 Google LLC","mobile":false,"proxy":false,"hosting":true,"query":"8.8.8.8"}"#;
    let info = parse_response("8.8.8.8", hosting);
    assert_eq!(
        (info.kind.as_str(), info.status.as_str(), info.asn.as_str()),
        ("hosting", "success", "AS15169")
    );
    assert_eq!(info.isp, "Google LLC");
    assert_eq!(info.organization, "Google Public DNS");

    let mobile = br#"{"status":"success","country":"China","isp":"China Mobile","org":"CMNET","as":"AS9808 China Mobile","mobile":true,"proxy":false,"hosting":false,"query":"1.1.1.1"}"#;
    assert_eq!(parse_response(IP, mobile).kind, "mobile");

    let residential = br#"{"status":"success","country":"France","isp":"Proxad / Free SAS","org":"Proxad / Free SAS","as":"AS12322 Free SAS","mobile":false,"proxy":false,"hosting":false,"query":"1.1.1.1"}"#;
    let info = parse_response(IP, residential);
    assert_eq!(info.kind, "residential");
    assert_eq!(info.asn, "AS12322");

    // 免费版 ipapi.is 的匿名响应格式（无 hosting/mobile）必须判失败。
    let anonymous = r#"{"ip":"1.1.1.1","is_bogon":false,"company":"APNIC Research and Development","asn":"AS13335 Cloudflare, Inc.","city":"Brisbane","country":"Australia"}"#;
    assert_eq!(parse_response(IP, anonymous.as_bytes()).status, "error");
}

#[test]
fn progress_run_id_is_bounded_camel_case_and_backward_compatible() {
    assert_eq!(normalize_test_run_id(None, 42), "42");
    assert_eq!(normalize_test_run_id(Some("  "), 42), "42");
    assert_eq!(normalize_test_run_id(Some(" batch-42 "), 42), "batch-42");
    assert_eq!(
        normalize_test_run_id(Some(&"测".repeat(129)), 42),
        "测".repeat(128)
    );
    let legacy: ProxyNodeTestProgress = serde_json::from_value(json!({"nodeId": "old"})).unwrap();
    assert!(legacy.run_id.is_empty());
    assert_eq!(
        serde_json::to_value(ProxyNodeTestProgress::default()).unwrap()["runId"],
        ""
    );
    for phase in ["started", "completed", "ip-info"] {
        let progress = ProxyNodeTestProgress {
            run_id: "batch-42".into(),
            phase: phase.into(),
            ..Default::default()
        };
        let value = serde_json::to_value(progress).unwrap();
        assert_eq!(value["runId"], "batch-42");
        assert!(value.get("run_id").is_none());
        let decoded: ProxyNodeTestProgress = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.run_id, "batch-42");
    }
}

#[test]
fn verifies_response_ip_semantically() {
    let mut value = payload();
    value["query"] = json!("8.8.8.8");
    assert_eq!(parse(value).status, "error");
    let mut value = payload();
    value["query"] = json!("::ffff:1.1.1.1");
    assert_eq!(parse(value).ip, IP);
    let mut value = payload();
    value["query"] = json!("2606:4700:4700:0000:0000:0000:0000:1111");
    let info = parse_response("2606:4700:4700::1111", &serde_json::to_vec(&value).unwrap());
    assert_eq!(info.status, "success");
    assert_eq!(info.ip, "2606:4700:4700::1111");
}

#[test]
fn filters_non_public_and_reserved_ips() {
    for ip in [
        "",
        "example.com",
        "https://1.1.1.1",
        "1.1.1.1:80",
        "0.1.2.3",
        "10.0.0.1",
        "127.0.0.1",
        "169.254.1.1",
        "172.16.0.1",
        "172.31.0.1",
        "192.168.0.1",
        "100.64.0.1",
        "100.127.255.254",
        "192.0.0.9",
        "192.0.2.1",
        "192.88.99.1",
        "198.18.0.1",
        "198.19.255.1",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "240.1.1.1",
        "255.255.255.255",
        "::",
        "::1",
        "::ffff:192.168.1.1",
        "fc00::1",
        "fd00::1",
        "fe80::1",
        "ff02::1",
        "64:ff9b::1",
        "100::1",
        "2001::1",
        "2001:2::1",
        "2001:10::1",
        "2001:db8::1",
        "2002::1",
        "3fff::1",
        "4000::1",
    ] {
        assert!(public_ip(ip).is_none(), "{ip}");
    }
    for ip in [
        IP,
        "8.8.8.8",
        "100.128.0.1",
        "172.32.0.1",
        "2606:4700:4700::1111",
        "2001:4860:4860::8888",
    ] {
        assert!(public_ip(ip).is_some(), "{ip}");
    }
    assert_eq!(public_ip(" ::ffff:1.1.1.1 ").as_deref(), Some(IP));
}

#[test]
fn cache_ttl_and_safe_stored_parsing() {
    let mut info = parse(payload());
    let checked = DateTime::parse_from_rfc3339(&info.checked_at)
        .unwrap()
        .timestamp();
    assert!(is_fresh(&info, IP, checked + SUCCESS_TTL - 1));
    assert!(!is_fresh(&info, IP, checked + SUCCESS_TTL));
    assert!(!is_fresh(&info, IP, checked - 1));
    assert!(!is_fresh(&info, "8.8.8.8", checked));
    info = error_info(IP, "查询失败");
    let checked = DateTime::parse_from_rfc3339(&info.checked_at)
        .unwrap()
        .timestamp();
    assert!(is_fresh(&info, IP, checked + ERROR_TTL - 1));
    assert!(!is_fresh(&info, IP, checked + ERROR_TTL));
    for json in [
        "",
        "{",
        "{}",
        "null",
        r#"{"ip":"1.1.1.1","kind":"hosting"}"#,
    ] {
        assert!(parse_stored_info(json, IP).is_none());
    }
    info.source = "other-service".into();
    assert!(parse_stored_info(&serde_json::to_string(&info).unwrap(), IP).is_none());
}

#[test]
fn migration_is_idempotent_and_new_schema_has_cache_and_empty_node_info() {
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("CREATE TABLE proxy_pool_nodes (id TEXT PRIMARY KEY, name TEXT NOT NULL); INSERT INTO proxy_pool_nodes VALUES ('legacy', 'legacy');").unwrap();
    crate::db::ensure_proxy_pool_node_columns(&connection).unwrap();
    crate::db::ensure_proxy_pool_node_columns(&connection).unwrap();
    let (not_null, default): (i64, String) = connection.query_row("SELECT \"notnull\", dflt_value FROM pragma_table_info('proxy_pool_nodes') WHERE name='ip_info_json'", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!((not_null, default.as_str()), (1, "''"));
    let legacy: String = connection
        .query_row("SELECT ip_info_json FROM proxy_pool_nodes", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(legacy.is_empty());
    let info = parse(payload());
    write_cache(&connection, &info).unwrap();
    assert_eq!(read_cache(&connection, IP).unwrap().kind, "residential");

    let database = database();
    let connection = database.lock_conn().unwrap();
    connection
        .execute(
            "INSERT INTO proxy_pool_nodes (id, name) VALUES ('new', 'new')",
            [],
        )
        .unwrap();
    let new_info: String = connection
        .query_row("SELECT ip_info_json FROM proxy_pool_nodes", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(new_info.is_empty());
    write_cache(&connection, &info).unwrap();
    connection
        .execute("UPDATE proxy_ip_info_cache SET expires_at=0", [])
        .unwrap();
    assert!(read_cache(&connection, IP).is_none());
}

#[test]
fn load_state_hides_expired_mismatched_corrupt_and_legacy_info() {
    let database = database();
    let runtime = runtime();
    let info = parse(payload());
    let fresh = serde_json::to_string(&info).unwrap();
    let mut expired = info.clone();
    expired.checked_at = (Utc::now() - chrono::Duration::seconds(SUCCESS_TTL + 1)).to_rfc3339();
    {
        let connection = database.lock_conn().unwrap();
        for (id, ip, data) in [
            ("fresh", IP, fresh.as_str()),
            ("mismatch", "8.8.8.8", fresh.as_str()),
            ("corrupt", IP, "{"),
            ("legacy", IP, ""),
            ("expired", IP, &serde_json::to_string(&expired).unwrap()),
        ] {
            connection.execute("INSERT INTO proxy_pool_nodes (id, name, primary_ip, ip_info_json, country_code, country_name) VALUES (?1, ?1, ?2, ?3, 'US', '美国')", params![id, ip, data]).unwrap();
        }
    }
    let state = load_state(&database, &runtime).unwrap();
    for node in state.nodes {
        assert_eq!(node.ip_info.is_some(), node.id == "fresh", "{}", node.id);
    }
    let node: ProxyNode = serde_json::from_value(json!({"id": "old"})).unwrap();
    assert!(node.ip_info.is_none());
    let progress = serde_json::to_value(ProxyNodeTestProgress::default()).unwrap();
    assert!(progress.get("primaryIp").unwrap().is_null());
    assert!(progress.get("ipInfo").unwrap().is_null());
}

#[tokio::test]
async fn changed_exit_and_retests_clear_old_types_but_type_errors_keep_speed_results() {
    let database = database();
    let runtime = runtime();
    let cancellation = CancellationToken::new();
    let info = parse(payload());
    {
        let connection = database.lock_conn().unwrap();
        for id in ["tested", "untouched"] {
            connection.execute("INSERT INTO proxy_pool_nodes (id, name, server, primary_ip, ip_info_json) VALUES (?1, 'HK node', '8.8.4.4', ?2, ?3)", params![id, IP, serde_json::to_string(&info).unwrap()]).unwrap();
        }
    }
    write_probe_result(&database, "tested", Some(123), Some(456), Some(IP)).unwrap();
    let node = load_state(&database, &runtime)
        .unwrap()
        .nodes
        .into_iter()
        .find(|n| n.id == "tested")
        .unwrap();
    assert!(node.ip_info.is_none());
    let error = error_info(IP, "类型查询失败");
    assert_eq!(
        persist_node_info(&database, "tested", &error, &cancellation).await,
        Some(true)
    );
    let state = load_state(&database, &runtime).unwrap();
    let tested = state.nodes.iter().find(|n| n.id == "tested").unwrap();
    assert_eq!(tested.latency_ms, Some(123));
    assert_eq!(tested.channel_latency_ms, Some(456));
    assert_eq!(tested.test_status, "success");
    assert_eq!(tested.channel_test_status, "success");
    assert_eq!(tested.ip_info.as_ref().unwrap().status, "error");
    assert!(state
        .nodes
        .iter()
        .find(|n| n.id == "untouched")
        .unwrap()
        .ip_info
        .is_some());

    apply_exit_ip_geoip(&database, None, "tested", "8.8.8.8");
    assert_eq!(
        persist_node_info(&database, "tested", &info, &cancellation).await,
        Some(false)
    );
    let raw: String = database
        .lock_conn()
        .unwrap()
        .query_row(
            "SELECT ip_info_json FROM proxy_pool_nodes WHERE id='tested'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(raw.is_empty());
    write_probe_result(&database, "tested", Some(77), None, None).unwrap();
    let node = load_state(&database, &runtime)
        .unwrap()
        .nodes
        .into_iter()
        .find(|n| n.id == "tested")
        .unwrap();
    assert_eq!(node.test_status, "success");
    assert_eq!(node.channel_test_status, "error");
    assert!(
        node.primary_ip.is_empty(),
        "不能把入口服务器 IP 回填为本轮出口"
    );
    assert!(node.ip_info.is_none());
    write_probe_result(&database, "tested", None, Some(100), None).unwrap();
    let node = load_state(&database, &runtime)
        .unwrap()
        .nodes
        .into_iter()
        .find(|n| n.id == "tested")
        .unwrap();
    assert_eq!(node.test_status, "error");
    assert!(node.latency_ms.is_none() && node.channel_latency_ms.is_none());
}

#[tokio::test]
async fn eight_lanes_single_flight_and_persistent_hits_include_errors() {
    for fail in [false, true] {
        let database = database();
        let batch = IpInfoBatch::new();
        let cancellation = CancellationToken::new();
        let calls = AtomicUsize::new(0);
        let fetch = |ip: String| {
            let calls = &calls;
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                if fail {
                    error_info(&ip, "离线模拟失败")
                } else {
                    parse(payload())
                }
            }
        };
        let results = futures_util::future::join_all((0..8).map(|n| {
            batch.lookup_with(
                &database,
                if n % 2 == 0 { IP } else { "::ffff:1.1.1.1" },
                &cancellation,
                fetch,
            )
        }))
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(results
            .iter()
            .all(|r| r.as_ref().unwrap().status == if fail { "error" } else { "success" }));
        let next_batch = IpInfoBatch::new();
        let cached = next_batch
            .lookup_with(&database, IP, &cancellation, fetch)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(cached.checked_at, results[0].as_ref().unwrap().checked_at);
    }
}

#[tokio::test]
async fn independent_two_request_limit_and_start_spacing() {
    let database = database();
    let batch = IpInfoBatch::new();
    let cancellation = CancellationToken::new();
    let active = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let starts = std::sync::Mutex::new(Vec::new());
    let results = futures_util::future::join_all(
        [IP, "8.8.8.8", "9.9.9.9", "8.8.4.4"].into_iter().map(|ip| {
            batch.lookup_with(&database, ip, &cancellation, |ip| {
                let (active, peak, starts) = (&active, &peak, &starts);
                async move {
                    starts.lock().unwrap().push(Instant::now());
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(current, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(450)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    error_info(&ip, "离线模拟")
                }
            })
        }),
    )
    .await;
    assert!(results.iter().all(Option::is_some));
    assert_eq!(peak.load(Ordering::SeqCst), 2);
    for pair in starts.lock().unwrap().windows(2) {
        assert!(pair[1].duration_since(pair[0]) >= START_INTERVAL - Duration::from_millis(5));
    }
}

async fn cancel_soon(token: &CancellationToken) {
    tokio::time::sleep(Duration::from_millis(15)).await;
    token.cancel();
}

#[tokio::test]
async fn cancellation_drops_in_flight_request_without_late_result() {
    struct MarkDrop<'a>(&'a AtomicBool);
    impl Drop for MarkDrop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let database = database();
    let batch = IpInfoBatch::new();
    let token = CancellationToken::new();
    let dropped = AtomicBool::new(false);
    let query = batch.lookup_with(&database, IP, &token, |_| async {
        let _guard = MarkDrop(&dropped);
        std::future::pending::<ProxyIpInfo>().await
    });
    let (result, ()) = tokio::time::timeout(Duration::from_millis(500), async {
        tokio::join!(query, cancel_soon(&token))
    })
    .await
    .unwrap();
    assert!(result.is_none());
    assert!(dropped.load(Ordering::SeqCst));
    assert!(read_cache(&database.lock_conn().unwrap(), IP).is_none());
    assert_eq!(batch.permits.available_permits(), 2);
    fn assert_send<T: Send>(_: T) {}
    assert_send(batch.lookup(&database, IP, "http://127.0.0.1:1", &token));
}

#[tokio::test]
async fn all_lock_semaphore_throttle_and_database_waits_are_cancellable() {
    let database = database();
    for wait in ["map", "ip", "permit", "throttle", "database"] {
        let batch = IpInfoBatch::new();
        let token = CancellationToken::new();
        let slot = Arc::new(Mutex::new(None));
        batch.flights.lock().await.insert(IP.into(), slot.clone());
        let _map_guard = if wait == "map" {
            Some(batch.flights.lock().await)
        } else {
            None
        };
        let _ip_guard = if wait == "ip" {
            Some(slot.lock().await)
        } else {
            None
        };
        let _permits = if wait == "permit" {
            Some(batch.permits.acquire_many(2).await.unwrap())
        } else {
            None
        };
        if wait == "throttle" {
            *batch.next_start.lock().await = Instant::now() + Duration::from_secs(60);
        }
        let _db_guard = if wait == "database" {
            Some(database.lock_conn().unwrap())
        } else {
            None
        };
        let query = batch.lookup_with(&database, IP, &token, |_| async {
            panic!("must not fetch while waiting")
        });
        let (result, ()) = tokio::time::timeout(Duration::from_millis(500), async {
            tokio::join!(query, cancel_soon(&token))
        })
        .await
        .unwrap();
        assert!(result.is_none(), "{wait}");
    }
}

#[tokio::test]
async fn reserved_ips_never_fetch_and_precancelled_lookups_never_return_cache() {
    let database = database();
    let batch = IpInfoBatch::new();
    let token = CancellationToken::new();
    let result = batch
        .lookup_with(&database, "127.0.0.1", &token, |_| async {
            panic!("reserved IP must not leave machine")
        })
        .await;
    assert!(result.is_none());
    write_cache(&database.lock_conn().unwrap(), &parse(payload())).unwrap();
    token.cancel();
    assert!(batch
        .lookup(&database, IP, "http://127.0.0.1:1", &token)
        .await
        .is_none());
}
