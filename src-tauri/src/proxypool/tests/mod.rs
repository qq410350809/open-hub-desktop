use crate::models::*;
use crate::proxypool::commands::*;
use crate::proxypool::geoip::*;
use crate::proxypool::parser::*;
use crate::proxypool::rotator::*;
use crate::proxypool::runtime::*;
use crate::proxypool::tester::*;
use crate::proxypool::types::*;
use maxminddb::Reader;
use std::time::Duration;
use url::Url;

#[test]
fn proxy_test_task_can_be_cancelled_and_released() {
    let runtime = ProxyRuntime::new(std::env::temp_dir().join("openhub-proxy-cancel-test"));
    let lease = runtime.start_proxy_test().unwrap();
    assert!(runtime.start_proxy_test().is_err());
    assert!(runtime.cancel_proxy_test().unwrap());
    assert!(lease.cancellation.is_cancelled());
    drop(lease);
    assert!(!runtime.cancel_proxy_test().unwrap());
    assert!(runtime.start_proxy_test().is_ok());
}

#[test]
fn deduplicates_nodes_without_using_display_name() {
    let first = parse_subscription("proxies:\n  - name: HK A\n    type: ss\n    server: hk.example.com\n    port: 443\n    cipher: aes-128-gcm\n    password: secret\n").unwrap();
    let second = parse_subscription("proxies:\n  - name: Another name\n    type: ss\n    server: hk.example.com\n    port: 443\n    cipher: aes-128-gcm\n    password: secret\n").unwrap();
    assert_eq!(first[0].id, second[0].id);
}

#[test]
fn keeps_different_credentials_as_different_nodes() {
    let first = parse_subscription("proxies:\n  - name: A\n    type: ss\n    server: hk.example.com\n    port: 443\n    cipher: aes-128-gcm\n    password: secret-a\n").unwrap();
    let second = parse_subscription("proxies:\n  - name: B\n    type: ss\n    server: hk.example.com\n    port: 443\n    cipher: aes-128-gcm\n    password: secret-b\n").unwrap();
    assert_ne!(first[0].id, second[0].id);
}

#[test]
fn rejects_incomplete_shadowsocks_nodes_before_runtime_start() {
    let node = parse_subscription(
        "proxies:\n  - name: bad\n    type: ss\n    server: hk.example.com\n    port: 443\n",
    )
    .unwrap();
    assert!(basic_node_config_error(&node[0].raw_json).is_some());
}

#[test]
fn rejects_proxy_nodes_with_out_of_range_ports() {
    let node = parse_subscription(
        "proxies:\n  - name: bad-port\n    type: http\n    server: example.com\n    port: 70000\n",
    )
    .unwrap();
    assert!(basic_node_config_error(&node[0].raw_json).is_some());
}

#[test]
fn parses_vmess_websocket_options() {
    let line = "vmess://eyJwb3J0Ijo4MDAsInBzIjoiSEstd3MiLCJ0bHMiOiIiLCJpZCI6InV1aWQtMSIsImFpZCI6IjIiLCJ2IjoiMiIsImhvc3QiOiJiY2UuYmRzdGF0aWMuY29tIiwidHlwZSI6Im5vbmUiLCJwYXRoIjoiLyIsIm5ldCI6IndzIiwiYWRkIjoiaGtnNC5pZ2NhY2hlcy5jb20ifQ==";
    let node = parse_uri_node(line).expect("vmess parse");
    assert_eq!(node.proxy_type, "vmess");
    assert_eq!(
        node.raw_json.get("network").and_then(|v| v.as_str()),
        Some("ws")
    );
    assert_eq!(
        node.raw_json
            .pointer("/ws-opts/path")
            .and_then(|v| v.as_str()),
        Some("/")
    );
    assert_eq!(
        node.raw_json
            .pointer("/ws-opts/headers/Host")
            .and_then(|v| v.as_str()),
        Some("bce.bdstatic.com")
    );
}

#[test]
fn parses_ssocks_and_base64_https_proxy_uris() {
    use base64::{engine::general_purpose, Engine as _};
    let body = [
        "ssocks://dXNlcjpwYXNzQGhvc3QuZXhhbXBsZS5jb206MTA4MA==?remarks=HK-Socks&method=auto",
        "https://dXNlcjpwYXNzQGhvc3QuZXhhbXBsZS5jb206ODQ0Mw==#HK-HTTPS",
        "anytls://secret@any.example.com:443#AnyTLS-Node",
        "tuic://uuid-1:pass-1@tuic.example.com:8443?alpn=h3&congestion_control=bbr#TUIC-Node",
        "vmess://eyJwb3J0Ijo0NDMsInBzIjoiVk0iLCJhZGQiOiJ2bS5leGFtcGxlLmNvbSIsImlkIjoidXVpZC0yIiwiYWlkIjowLCJzY3kiOiJhdXRvIiwidGxzIjoiIn0=",
    ]
    .join("\n");
    let encoded = general_purpose::STANDARD.encode(body.as_bytes());
    let nodes = parse_subscription(&encoded).unwrap();
    assert!(nodes.len() >= 5, "got {}", nodes.len());
    assert!(nodes
        .iter()
        .any(|n| n.proxy_type == "socks5" && n.name.contains("HK-Socks")));
    assert!(nodes
        .iter()
        .any(|n| n.proxy_type == "http" && n.name.contains("HK-HTTPS")));
    assert!(nodes.iter().any(|n| n.proxy_type == "anytls"));
    assert!(nodes.iter().any(|n| n.proxy_type == "tuic"));
    assert!(nodes.iter().any(|n| n.proxy_type == "vmess"));
}

#[test]
fn local_addresses_are_always_ignored() {
    let value = normalize_ignore_addresses("example.com");
    assert!(value.contains("127.0.0.1"));
    assert!(value.contains("192.168.0.0/16"));
}

#[test]
fn speed_test_uses_selected_or_default_url() {
    let list = speed_test_candidates("https://cp.cloudflare.com/generate_204");
    assert_eq!(list, vec![DEFAULT_PROXY_SPEED_TEST_URL.to_string()]);
    let list = speed_test_candidates("http://www.gstatic.com/generate_204");
    assert_eq!(
        list,
        vec!["http://www.gstatic.com/generate_204".to_string()]
    );
}

#[test]
fn node_names_strip_speed_test_suffixes() {
    assert_eq!(clean_node_name("香港 01 | 延迟 123ms"), "香港 01");
    assert_eq!(clean_node_name("日本 02 测速：88ms"), "日本 02");
    assert_eq!(clean_node_name("新加坡 [速度测试 10Mbps]"), "新加坡");
    assert_eq!(clean_node_name("US 01 - latency 50ms"), "US 01");
    assert_eq!(clean_node_name("德国 03｜测速结果：45ms"), "德国 03");
    assert_eq!(clean_node_name("香港 01 | 52MB/s"), "香港 01");
    assert_eq!(clean_node_name("日本 02 45.5Mbps"), "日本 02");
    assert_eq!(clean_node_name("香港 02 | 0.19MBs"), "香港 02");
    assert_eq!(clean_node_name("新加坡 [12 MB/s] 01"), "新加坡");
    assert_eq!(clean_node_name("英国 04｜测速 88MB/s 延迟 30ms"), "英国 04");
    assert_eq!(clean_node_name("低延迟专线 01"), "低延迟专线 01");
    assert_eq!(clean_node_name("测试节点"), "测试节点");
    assert_eq!(clean_node_name("5G 专线 02"), "5G 专线 02");
    assert_eq!(
        clean_node_name("节点 | 剩余流量 90GB"),
        "节点 | 剩余流量 90GB"
    );
}

#[test]
fn repair_stored_node_names_cleans_and_deduplicates() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE proxy_pool_nodes (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                raw_json TEXT NOT NULL DEFAULT '{}'
            );
             INSERT INTO proxy_pool_nodes (id, name, raw_json) VALUES
                ('a', '香港 01 | 52MB/s', '{\"name\":\"香港 01 | 52MB/s\"}'),
                ('b', '香港 01 | 延迟 30ms', '{\"name\":\"香港 01 | 延迟 30ms\"}'),
                ('c', '低延迟专线 01', '{\"name\":\"低延迟专线 01\"}');",
        )
        .unwrap();
    let database = Database(std::sync::Mutex::new(connection));
    assert_eq!(repair_stored_node_names(&database).unwrap(), 2);

    let connection = database.0.lock().unwrap();
    let mut statement = connection
        .prepare("SELECT id, name FROM proxy_pool_nodes ORDER BY id")
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
            ("a".to_string(), "香港 01".to_string()),
            ("b".to_string(), "香港 01 [2]".to_string()),
            ("c".to_string(), "低延迟专线 01".to_string()),
        ]
    );
}

#[test]
fn channel_candidates_use_only_channel_test_fields() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE proxy_pool_nodes (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                latency_ms INTEGER,
                test_status TEXT NOT NULL DEFAULT '',
                channel_latency_ms INTEGER,
                channel_test_status TEXT NOT NULL DEFAULT '',
                channel_tested_at TEXT NOT NULL DEFAULT ''
            );
             INSERT INTO proxy_pool_nodes (id, name, latency_ms, test_status, channel_latency_ms, channel_test_status) VALUES
                ('a', '全局快未测通道', 100, 'success', NULL, ''),
                ('b', '通道超时', 100, 'success', 600, 'success'),
                ('c', '全局失败但通道快', 900, 'error', 200, 'success');",
        )
        .unwrap();
    let database = Database(std::sync::Mutex::new(connection));
    let rows = list_channel_candidate_nodes(&database, 500).unwrap();
    assert_eq!(
        rows,
        vec![("c".to_string(), "全局失败但通道快".to_string(), 200)]
    );
}

#[test]
fn controller_paths_do_not_contain_double_slashes() {
    let mut endpoint = Url::parse(&controller_url(19090, "/proxies/")).unwrap();
    append_controller_path(&mut endpoint, &["NodeA", "delay"]).unwrap();
    assert!(!endpoint.path().contains("//"));
    assert!(endpoint.path().ends_with("/proxies/NodeA/delay"));
}

#[test]
fn extracts_zero_based_mihomo_proxy_error_index() {
    assert_eq!(proxy_error_index("proxy 0: missing password"), Some(0));
    assert_eq!(proxy_error_index("no index here"), None);
}

#[test]
fn reads_country_from_available_geoip_database() {
    let Some(path) = find_geoip_database(&ProxyRuntime::new(
        std::env::temp_dir().join("openhub-geoip-test"),
    )) else {
        return;
    };
    let reader = Reader::open_readfile(path).unwrap();
    let country = geoip_country(&reader, "89.160.20.128".parse().unwrap()).unwrap();
    assert_eq!(country.0, "SE");
}

#[test]
fn classifies_proxy_failure_errors() {
    assert!(is_transport_error("error sending request for url"));
    assert!(is_transport_error("connect error: Connection refused"));
    assert!(is_transport_error("请求失败：连接失败"));
    assert!(!is_transport_error("HTTP 401 未授权"));

    assert!(is_http_forbidden_error("HTTP 403 Forbidden"));
    assert!(is_http_forbidden_error("请求失败：HTTP 403"));
    assert!(!is_http_forbidden_error("HTTP 404 Not Found"));
}

#[test]
fn assigns_ttl_for_proxy_failures() {
    assert_eq!(
        account_proxy_failure_ttl("HTTP 403 Forbidden"),
        ACCOUNT_PROXY_BAN_FORBIDDEN
    );
    assert_eq!(
        account_proxy_failure_ttl("error sending request for url"),
        ACCOUNT_PROXY_BAN_UNREACHABLE
    );
    assert_eq!(
        account_proxy_failure_ttl("请求超时"),
        ACCOUNT_PROXY_BAN_TIMEOUT
    );
    assert_eq!(
        account_proxy_failure_ttl("未知错误"),
        ACCOUNT_PROXY_BAN_DEFAULT
    );
}

#[test]
fn account_node_bans_expire() {
    let runtime = ProxyRuntime::new(std::env::temp_dir().join("openhub-account-ban-test"));
    assert!(!runtime.account_node_is_banned("node-a"));
    runtime.account_ban_node("node-a", Duration::from_millis(1));
    assert!(runtime.account_node_is_banned("node-a"));
    std::thread::sleep(Duration::from_millis(5));
    assert!(!runtime.account_node_is_banned("node-a"));
}

#[test]
fn orphan_mihomo_detection_matches_only_openhub_owned_kernels() {
    let mac_bin = "/Users/u/Library/Application Support/com.dfeer.openhub.desktop/bin/mihomo";
    let runtime_dir = "/Users/u/Library/Application Support/com.dfeer.openhub.desktop/proxy-runtime";

    // 正例：OpenHub 自管的各类实例（shared / channel / speed-test / 自定义二进制路径）
    for cmd in [
        format!("{mac_bin} -d {runtime_dir}/shared -f {runtime_dir}/shared/config.yaml"),
        format!("{mac_bin} -d {runtime_dir}/channels/default -f {runtime_dir}/channels/default/config.yaml"),
        format!("{mac_bin} -d {runtime_dir}/speed-test-1/shared -f {runtime_dir}/speed-test-1/shared/config.yaml"),
        format!("/opt/custom-tools/mihomo -d {runtime_dir}/shared"),
        "C:\\Users\\u\\AppData\\Roaming\\com.dfeer.openhub.desktop\\bin\\mihomo.exe -d C:\\...\\proxy-runtime\\shared".to_string(),
        // 实测存在的异常孤儿：verge-mihomo 二进制 + OpenHub 运行时配置（PPID=1）
        "/Applications/Clash Verge.app/Contents/MacOS/verge-mihomo -d /Users/u/Library/Application Support/com.dfeer.openhub.desktop/proxy-runtime/shared -f /Users/u/Library/Application Support/com.dfeer.openhub.desktop/proxy-runtime/shared/config.yaml".to_string(),
    ] {
        assert!(
            is_orphan_mihomo_command(&cmd),
            "应识别为 OpenHub 孤儿内核: {cmd}"
        );
    }

    // 负例：绝不能误杀的进程
    for cmd in [
        // Clash Verge 正常实例：指向自家数据目录，不含 OpenHub 归属标识
        "/Applications/Clash Verge.app/Contents/MacOS/verge-mihomo -d /Users/u/Library/Application Support/io.github.clash-verge-rev.clash-verge-rev -f .../clash-verge.yaml -ext-ctl-unix /tmp/verge/verge-mihomo.sock".to_string(),
        // 其他软件的 mihomo（无 openhub 标识）
        "/usr/local/bin/mihomo -d /etc/mihomo".to_string(),
        // OpenHub 主程序自身（exe 名不是 mihomo）
        "/Applications/OpenHub.app/Contents/MacOS/open-hub-desktop com.dfeer.openhub.desktop proxy-runtime".to_string(),
        "openhub-server --data-dir /tmp/openhub/proxy-runtime".to_string(),
        "".to_string(),
    ] {
        assert!(
            !is_orphan_mihomo_command(&cmd),
            "不得误杀: {cmd}"
        );
    }
}
