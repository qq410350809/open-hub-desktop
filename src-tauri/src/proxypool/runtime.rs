pub(crate) use crate::db::{read_meta, write_meta};
use crate::models::*;
use crate::proxypool::geoip::{classify_node_location, open_geoip_reader};
use crate::proxypool::parser::{
    basic_node_config_error, sanitize_proxy_node_json, stable_id,
};
use crate::proxypool::rotator::channel_candidate_nodes;
use crate::proxypool::types::*;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value as JsonValue};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::warn;
use url::Url;

pub fn row_subscription(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProxySubscription> {
    Ok(ProxySubscription {
        id: row.get(0)?,
        name: row.get(1)?,
        url: row.get(2)?,
        node_count: row.get(3)?,
        last_error: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

pub fn runtime_info(runtime: &ProxyRuntime) -> (bool, String, String) {
    let detected = find_mihomo_binary(runtime);
    let mut path = detected
        .as_ref()
        .map(|item| item.display().to_string())
        .unwrap_or_default();
    let mut error = if detected.is_none() {
        "未检测到 Mihomo 组件，请在代理池设置中下载并初始化".to_string()
    } else {
        String::new()
    };
    if let Ok(state) = runtime.shared_instance.lock() {
        if !state.engine_path.is_empty() {
            path = state.engine_path.clone();
        }
        if !state.last_error.is_empty() {
            error = state.last_error.clone();
        }
    }
    (!path.is_empty(), path, error)
}

pub fn ensure_default_proxy_channel(connection: &rusqlite::Connection) -> Result<(), String> {
    connection
        .execute(
            "INSERT OR IGNORE INTO proxy_channels (id, name) VALUES (?1, ?2)",
            params![DEFAULT_PROXY_CHANNEL_ID, DEFAULT_PROXY_CHANNEL_NAME],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub fn load_channels(
    connection: &rusqlite::Connection,
    nodes: &[ProxyNode],
) -> Result<(Vec<ProxyChannel>, String), String> {
    ensure_default_proxy_channel(connection)?;
    let mut statement = connection
        .prepare(
            "SELECT id, name, node_id, test_url, created_at, updated_at
             FROM proxy_channels
             ORDER BY CASE WHEN id = ?1 THEN 0 ELSE 1 END, updated_at DESC, name COLLATE NOCASE",
        )
        .map_err(|error| error.to_string())?;
    let mut channels = statement
        .query_map([DEFAULT_PROXY_CHANNEL_ID], |row| {
            let node_id: String = row.get(2)?;
            Ok(ProxyChannel {
                id: row.get(0)?,
                name: row.get(1)?,
                node: nodes.iter().find(|node| node.id == node_id).cloned(),
                node_id,
                port: None,
                test_url: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
                account_count: 0,
                accounts: Vec::new(),
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let mut account_statement = connection
        .prepare(
            "SELECT channel_id, profile_id
             FROM account_proxy_channels
             ORDER BY profile_id",
        )
        .map_err(|error| error.to_string())?;
    let mut accounts_by_channel = HashMap::<String, Vec<ProxyChannelAccount>>::new();
    let account_rows = account_statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    for (channel_id, profile_id) in account_rows {
        accounts_by_channel
            .entry(channel_id)
            .or_default()
            .push(ProxyChannelAccount { profile_id });
    }
    for channel in &mut channels {
        if let Some(accounts) = accounts_by_channel.remove(&channel.id) {
            channel.account_count = accounts.len() as i64;
            channel.accounts = accounts;
        }
    }
    let default_id = channels
        .iter()
        .find(|channel| channel.id == DEFAULT_PROXY_CHANNEL_ID)
        .map(|channel| channel.id.clone())
        .unwrap_or_else(|| DEFAULT_PROXY_CHANNEL_ID.to_string());
    Ok((channels, default_id))
}

pub fn load_state(database: &Database, runtime: &ProxyRuntime) -> Result<ProxyPoolState, String> {
    let connection = database.lock_conn()?;
    let subscriptions = connection
        .prepare(
            "SELECT id, name, url, node_count, last_error, created_at, updated_at
             FROM proxy_subscriptions ORDER BY updated_at DESC, name COLLATE NOCASE",
        )
        .map_err(|error| error.to_string())?
        .query_map([], row_subscription)
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;

    let rows = connection
        .prepare(
            "SELECT n.id, n.name, n.proxy_type, n.server, n.port, n.cipher, n.udp,
                    n.latency_ms, n.test_status, n.tested_at, n.updated_at,
                    COALESCE(n.country_code, ''), COALESCE(n.country_name, ''),
                    COALESCE(n.classification, ''), COALESCE(n.primary_ip, ''),
                    COALESCE(GROUP_CONCAT(DISTINCT s.name), ''),
                    n.channel_latency_ms,
                    COALESCE(n.channel_test_status, '')
             FROM proxy_pool_nodes n
             LEFT JOIN proxy_subscription_nodes sn ON sn.node_id = n.id
             LEFT JOIN proxy_subscriptions s ON s.id = sn.subscription_id
             GROUP BY n.id
             ORDER BY CASE WHEN n.latency_ms IS NULL THEN 1 ELSE 0 END, n.latency_ms, n.name COLLATE NOCASE",
        )
        .map_err(|error| error.to_string())?
        .query_map([], |row| {
            let names: String = row.get(15)?;
            Ok(ProxyNode {
                id: row.get(0)?,
                name: row.get(1)?,
                proxy_type: row.get(2)?,
                server: row.get(3)?,
                port: row.get(4)?,
                cipher: row.get(5)?,
                udp: row.get::<_, i64>(6)? != 0,
                latency_ms: row.get(7)?,
                test_status: row.get(8)?,
                tested_at: row.get(9)?,
                updated_at: row.get(10)?,
                country_code: row.get(11)?,
                country_name: row.get(12)?,
                classification: row.get(13)?,
                primary_ip: row.get(14)?,
                channel_latency_ms: row.get(16)?,
                channel_test_status: row.get(17)?,
                subscription_names: names
                    .split(',')
                    .filter(|item| !item.is_empty())
                    .map(str::to_string)
                    .collect(),
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;

    let geoip_reader = open_geoip_reader(runtime);
    let mut rows = rows;
    let mut dirty = false;
    for node in &mut rows {
        let is_unknown = node.country_code.trim().is_empty() || node.country_code == "ZZ";
        if !is_unknown && !node.country_name.trim().is_empty() {
            continue;
        }
        let (code, name, class, ip) =
            classify_node_location(&node.name, &node.server, node.port, geoip_reader.as_ref());
        if code != "ZZ" && (node.country_code != code || node.country_name != name) {
            node.country_code = code;
            node.country_name = name;
            if node.classification.trim().is_empty() || node.classification == "unresolved" {
                node.classification = class;
            }
            if node.primary_ip.trim().is_empty() && !ip.is_empty() {
                node.primary_ip = ip;
            }
            let _ = connection.execute(
                "UPDATE proxy_pool_nodes
                 SET country_code=?2, country_name=?3, classification=?4,
                     primary_ip=CASE WHEN ?5 != '' THEN ?5 ELSE primary_ip END
                 WHERE id=?1",
                params![
                    node.id,
                    node.country_code,
                    node.country_name,
                    node.classification,
                    node.primary_ip
                ],
            );
            dirty = true;
        }
    }
    let _ = dirty;
    let (mut channels, default_channel_id) = load_channels(&connection, &rows)?;
    for channel in &mut channels {
        channel.port = runtime.channel_port(&channel.id);
    }

    let mut active_node_id = crate::db::read_meta_conn(&connection, ACTIVE_PROXY_NODE_KEY)?;
    let active_node = rows.iter().find(|node| node.id == active_node_id).cloned();
    if active_node.is_none() {
        active_node_id.clear();
    }
    let network_proxy = crate::db::read_meta_conn(&connection, NETWORK_PROXY_KEY)?;
    let ignore_addresses = {
        let value = crate::db::read_meta_conn(&connection, PROXY_IGNORE_KEY)?;
        if value.trim().is_empty() {
            DEFAULT_PROXY_IGNORE.to_string()
        } else {
            value
        }
    };
    let speed_test_url = {
        let value = crate::db::read_meta_conn(&connection, PROXY_SPEED_TEST_URL_KEY)?;
        if value.trim().is_empty() || is_slow_or_blocked_speed_test_url(&value) {
            DEFAULT_PROXY_SPEED_TEST_URL.to_string()
        } else {
            value
        }
    };
    drop(connection);
    let (runtime_available, runtime_path, runtime_error) = runtime_info(runtime);
    let active_runtime_url = runtime_proxy_url(runtime);
    Ok(ProxyPoolState {
        subscription_count: subscriptions.len() as i64,
        node_count: rows.len() as i64,
        invalid_node_count: rows.iter().filter(|n| n.test_status == "invalid").count() as i64,
        subscriptions,
        nodes: rows,
        channels,
        default_channel_id,
        enabled: !active_node_id.is_empty() && network_proxy == active_runtime_url,
        active_node_id,
        active_node,
        ignore_addresses,
        speed_test_url,
        runtime_available,
        runtime_path,
        runtime_error,
    })
}

pub fn is_slow_or_blocked_speed_test_url(value: &str) -> bool {
    matches!(
        value.trim(),
        "https://cp.cloudflare.com/generate_204"
            | "http://cp.cloudflare.com/generate_204"
            | "https://cloudflare.com/cdn-cgi/trace"
    )
}

pub fn find_mihomo_binary(runtime: &ProxyRuntime) -> Option<PathBuf> {
    if let Ok(value) = std::env::var("OPENHUB_MIHOMO_PATH") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
    }

    let binary_names: &[&str] = if cfg!(target_os = "windows") {
        &["mihomo.exe", "mihomo"]
    } else {
        &["mihomo"]
    };
    let candidate_dirs = [
        runtime.directory.clone(),
        runtime
            .directory
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| runtime.directory.clone()),
    ];
    for directory in candidate_dirs {
        for name in binary_names {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
            let candidate = directory.join("bin").join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let home_path = PathBuf::from(home);
        let base_dirs = [
            home_path.join("Library/Application Support/com.dfeer.openhub.desktop/bin"),
            home_path.join("AppData/Roaming/com.dfeer.openhub.desktop/bin"),
            home_path.join(".config/com.dfeer.openhub.desktop/bin"),
        ];
        for base in base_dirs {
            for name in binary_names {
                let candidate = base.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

pub fn port_is_available(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// 端口上是否真的有监听者：用 connect 探测而非试绑定——试绑定会在探测
/// 瞬间占住端口，理论上可能撞上内核正在进行的监听绑定，导致该 listener
/// 永久绑定失败；connect 无副作用，且对空闲端口立即返回 ECONNREFUSED。
fn port_has_listener(port: u16) -> bool {
    let address: SocketAddr = ([127, 0, 0, 1], port).into();
    TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok()
}

pub fn allocate_free_port() -> Result<u16, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| format!("无法分配代理端口：{error}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    drop(listener);
    Ok(port)
}

pub fn runtime_proxy_url(runtime: &ProxyRuntime) -> String {
    runtime
        .shared_instance
        .lock()
        .ok()
        .filter(|state| state.proxy_port > 0)
        .map(|state| format!("http://127.0.0.1:{}", state.proxy_port))
        .unwrap_or_default()
}

pub fn runtime_controller_port(runtime: &ProxyRuntime) -> Result<u16, String> {
    runtime
        .shared_instance
        .lock()
        .map_err(|_| "代理内核运行状态锁定失败".to_string())
        .and_then(|state| {
            (state.controller_port > 0)
                .then_some(state.controller_port)
                .ok_or_else(|| "代理内核控制器尚未启动".to_string())
        })
}

/// 全局单实例配置：mixed-port + OpenHub 共享组 + 三个 lane 池
/// （SPEED 测速 / ACCT 账号 / CH 通道），每个 lane 一个 select 组
/// （组内全量节点）+ 一个绑定该组的本地监听端口。
pub fn runtime_config(
    nodes: &[RuntimeNode],
    proxy_port: u16,
    controller_port: u16,
    speed_lanes: &[LaneSlot],
    account_lanes: &[LaneSlot],
    channel_lanes: &[LaneSlot],
) -> JsonValue {
    let configs = nodes
        .iter()
        .map(|node| {
            let mut val = node.config.clone();
            sanitize_proxy_node_json(&mut val);
            val
        })
        .collect::<Vec<_>>();
    let all_node_names = configs
        .iter()
        .filter_map(|node| {
            node.get("name")
                .and_then(JsonValue::as_str)
                .map(String::from)
        })
        .collect::<Vec<_>>();

    let mut listeners = Vec::new();
    let mut proxy_groups = Vec::new();

    proxy_groups.push(json!({
        "name": RUNTIME_GROUP,
        "type": "select",
        "proxies": all_node_names.clone()
    }));

    for lane in speed_lanes
        .iter()
        .chain(account_lanes.iter())
        .chain(channel_lanes.iter())
    {
        proxy_groups.push(json!({
            "name": lane.group_name,
            "type": "select",
            "proxies": all_node_names.clone()
        }));
        listeners.push(json!({
            "name": format!("ln-{}", lane.group_name),
            "type": "mixed",
            "port": lane.listen_port,
            "listen": "127.0.0.1",
            "proxy": lane.group_name
        }));
    }

    json!({
        "mixed-port": proxy_port,
        "external-controller": format!("127.0.0.1:{controller_port}"),
        "secret": RUNTIME_SECRET,
        "allow-lan": false,
        "bind-address": "127.0.0.1",
        "mode": "rule",
        "log-level": "warning",
        "ipv6": false,
        "unified-delay": true,
        "tcp-concurrent": true,
        "find-process-mode": "off",
        "profile": {
            "store-selected": false,
            "store-fake-ip": false
        },
        "dns": {
            "enable": true,
            "ipv6": false,
            "use-system-hosts": true,
            "enhanced-mode": "redir-host",
            "default-nameserver": ["223.5.5.5", "119.29.29.29", "8.8.8.8", "1.1.1.1"],
            "nameserver": ["223.5.5.5", "119.29.29.29", "180.76.76.76", "8.8.8.8", "1.1.1.1", "system"]
        },
        "listeners": listeners,
        "proxies": configs,
        "proxy-groups": proxy_groups,
        "rules": [
            "IP-CIDR,127.0.0.0/8,DIRECT,no-resolve",
            "IP-CIDR,10.0.0.0/8,DIRECT,no-resolve",
            "IP-CIDR,172.16.0.0/12,DIRECT,no-resolve",
            "IP-CIDR,192.168.0.0/16,DIRECT,no-resolve",
            format!("MATCH,{RUNTIME_GROUP}")
        ]
    })
}

pub fn runtime_nodes(
    database: &Database,
    only_ids: Option<&HashSet<String>>,
) -> Result<(Vec<RuntimeNode>, String), String> {
    let connection = database.lock_conn()?;
    let active = crate::db::read_meta_conn(&connection, ACTIVE_PROXY_NODE_KEY)?;
    let rows = connection
        .prepare("SELECT id, raw_json FROM proxy_pool_nodes ORDER BY CASE WHEN id = ?1 THEN 0 ELSE 1 END, name COLLATE NOCASE")
        .map_err(|error| error.to_string())?
        .query_map([&active], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let rows = if let Some(only_ids) = only_ids {
        rows.into_iter()
            .filter(|(id, _)| only_ids.contains(id) || (!active.is_empty() && id == &active))
            .collect::<Vec<_>>()
    } else {
        rows
    };
    let mut nodes = Vec::new();
    let mut invalid = Vec::new();
    for (id, raw) in rows {
        let Ok(config) = serde_json::from_str::<JsonValue>(&raw) else {
            invalid.push((id, "节点配置不是有效 JSON".to_string()));
            continue;
        };
        if let Some(error) = basic_node_config_error(&config) {
            invalid.push((id, error));
        } else {
            let mut config = config;
            sanitize_proxy_node_json(&mut config);
            if let Some(object) = config.as_object_mut() {
                object.insert("name".into(), JsonValue::String(id.clone()));
                for key in ["dialer-proxy", "proxy", "interface-name", "routing-mark"] {
                    object.remove(key);
                }
                let tls_on = object.get("tls").and_then(|v| v.as_bool()).unwrap_or(false)
                    || object
                        .get("type")
                        .and_then(|v| v.as_str())
                        .map(|t| matches!(t, "trojan" | "hysteria2" | "tuic"))
                        .unwrap_or(false);
                if tls_on && !object.contains_key("skip-cert-verify") {
                    object.insert("skip-cert-verify".into(), JsonValue::Bool(true));
                }
            }
            nodes.push(RuntimeNode { id, config });
        }
    }
    for (id, error) in &invalid {
        connection.execute(
            "UPDATE proxy_pool_nodes SET latency_ms=NULL, test_status='invalid', tested_at=CURRENT_TIMESTAMP WHERE id=?1",
            [id],
        ).map_err(|db_error| db_error.to_string())?;
        if id == &active {
            write_meta(&connection, ACTIVE_PROXY_NODE_KEY, "")?;
            write_meta(&connection, NETWORK_PROXY_KEY, "")?;
        }
        warn!("OpenHub 代理节点已跳过：{id}：{error}");
    }
    let hash = stable_id(&[&serde_json::to_string(
        &nodes.iter().map(|node| &node.config).collect::<Vec<_>>(),
    )
    .unwrap_or_default()]);
    Ok((nodes, hash))
}

/// 极简 HTTP/1.1 客户端：手写请求头，读完整响应（按 Content-Length / 连接关闭判定）。
/// 供控制器就绪探测与同步切组（PUT /proxies/{group}）使用，避免同步路径引入异步客户端。
pub fn simple_http_request(
    port: u16,
    method: &str,
    path: &str,
    secret: &str,
    body: Option<&str>,
    timeout: Duration,
) -> Result<(u16, String), String> {
    use std::io::{Read, Write};
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream =
        std::net::TcpStream::connect_timeout(&addr, timeout).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {secret}\r\n"
    );
    if let Some(body) = body {
        request.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    request.push_str("Connection: close\r\n\r\n");
    let mut request_bytes = request.into_bytes();
    if let Some(body) = body {
        request_bytes.extend_from_slice(body.as_bytes());
    }
    stream
        .write_all(&request_bytes)
        .map_err(|e| e.to_string())?;

    let mut buf = Vec::with_capacity(8192);
    let mut chunk = [0u8; 4096];
    let mut header_end = None;
    let mut content_length = None;

    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if header_end.is_none() {
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        header_end = Some(pos + 4);
                        if let Ok(headers_str) = std::str::from_utf8(&buf[..pos]) {
                            for line in headers_str.lines() {
                                if let Some((k, v)) = line.split_once(':') {
                                    if k.trim().eq_ignore_ascii_case("content-length") {
                                        if let Ok(cl) = v.trim().parse::<usize>() {
                                            content_length = Some(cl);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(h_end) = header_end {
                    if let Some(cl) = content_length {
                        if buf.len() >= h_end + cl {
                            break;
                        }
                    }
                }
            }
            Err(_) => {
                if header_end.is_some() {
                    break;
                } else {
                    return Err("HTTP 读取响应超时或失败".to_string());
                }
            }
        }
    }

    let h_end = header_end.ok_or_else(|| "未读取到有效 HTTP 响应头".to_string())?;
    let headers_str = std::str::from_utf8(&buf[..h_end - 4]).map_err(|e| e.to_string())?;
    let mut lines = headers_str.lines();
    let status_line = lines.next().ok_or_else(|| "空响应".to_string())?;
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("无效状态行: {status_line}"))?;

    let body = String::from_utf8_lossy(&buf[h_end..]).to_string();
    Ok((status_code, body))
}

pub fn simple_http_get(
    port: u16,
    path: &str,
    secret: &str,
    timeout: Duration,
) -> Result<(u16, String), String> {
    simple_http_request(port, "GET", path, secret, None, timeout)
}

pub fn controller_url(port: u16, path: &str) -> String {
    format!("http://127.0.0.1:{port}{path}")
}

pub fn append_controller_path(url: &mut Url, segments: &[&str]) -> Result<(), String> {
    url.path_segments_mut()
        .map_err(|_| "控制器地址无效".to_string())?
        .pop_if_empty()
        .extend(segments);
    Ok(())
}

pub fn controller_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .pool_max_idle_per_host(BATCH_PROXY_TEST_CONCURRENCY)
        .pool_idle_timeout(Duration::from_secs(30))
        .no_proxy()
        .build()
        .map_err(|error| error.to_string())
}

/// 分配一个不与本批已用端口重复的本地端口。
/// lane 池端口在 mihomo 启动前都是空闲的，连续 allocate_free_port 可能
/// 撞出重复端口，必须批内去重。
pub fn allocate_free_port_excluding(used: &mut HashSet<u16>) -> Result<u16, String> {
    loop {
        let port = allocate_free_port()?;
        if used.insert(port) {
            return Ok(port);
        }
    }
}

fn global_instance_running(runtime: &ProxyRuntime) -> bool {
    let Ok(mut state) = runtime.shared_instance.lock() else {
        return false;
    };
    state.proxy_port > 0
        && state
            .child
            .as_mut()
            .and_then(|child| child.try_wait().ok())
            .map(|status| status.is_none())
            .unwrap_or(false)
}

/// 确保全局单实例在跑：存活则直接返回（全量节点已在配置内，无需重建），
/// 否则按全量节点拉起。lane 化后账号/通道/测速出口都依赖该实例。
pub fn ensure_global_runtime(database: &Database, runtime: &ProxyRuntime) -> Result<(), String> {
    if global_instance_running(runtime) {
        return Ok(());
    }
    ensure_runtime(database, runtime, None, None)
}

/// 确保三个 lane 池完成一次性预配。首次使用时整池分配端口；
/// 之后全局实例重启也复用同一批端口（调用前旧进程已停止）。
fn ensure_lane_pools(runtime: &ProxyRuntime, used_ports: &mut HashSet<u16>) -> Result<(), String> {
    let pools: [(&std::sync::Mutex<Vec<LaneSlot>>, usize, &str); 3] = [
        (&runtime.speed_lane_slots, SPEED_TEST_LANES, "SPEED-lane"),
        (&runtime.account_lane_slots, ACCOUNT_LANE_POOL, "ACCT-lane"),
        (&runtime.channel_lane_slots, CHANNEL_LANE_POOL, "CH-lane"),
    ];
    for (pool, expected, prefix) in pools {
        let mut slots = pool
            .lock()
            .map_err(|_| "lane 池状态锁定失败".to_string())?;
        if slots.len() == expected {
            for slot in slots.iter_mut() {
                // 复用前必须确认端口真的空闲：句柄丢失的残留内核会继续占着旧端口，
                // 新实例绑定失败后（内核只记日志不退出）lane 流量会串到旧实例——
                // 测速全失败、出口串节点都源于此。被占的端口就地换新。
                if !port_is_available(slot.listen_port) {
                    slot.listen_port = allocate_free_port_excluding(used_ports)?;
                }
                used_ports.insert(slot.listen_port);
            }
            continue;
        }
        slots.clear();
        for i in 0..expected {
            slots.push(LaneSlot {
                group_name: format!("{prefix}-{i}"),
                listen_port: allocate_free_port_excluding(used_ports)?,
            });
        }
    }
    Ok(())
}

/// 解析通道当前绑定的节点 id；节点缺失时回退到延迟最低的启用节点并回写 DB。
fn resolve_channel_node_id(database: &Database, channel_id: &str) -> Result<String, String> {
    let connection = database.lock_conn()?;
    ensure_default_proxy_channel(&connection)?;
    let node_id: Option<String> = connection
        .query_row(
            "SELECT node_id FROM proxy_channels WHERE id = ?1",
            [channel_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(|error| error.to_string())?
        .flatten()
        .filter(|s: &String| !s.trim().is_empty());

    if let Some(id) = node_id {
        let exists = connection
            .query_row(
                "SELECT 1 FROM proxy_pool_nodes WHERE id = ?1",
                [&id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .is_some();
        if exists {
            return Ok(id);
        }
    }
    let fallback: String = connection
        .query_row(
            "SELECT id FROM proxy_pool_nodes WHERE is_enabled = 1 ORDER BY latency_ms ASC, name ASC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "代理池中没有可用的代理节点".to_string())?;
    connection
        .execute(
            "UPDATE proxy_channels SET node_id = ?2 WHERE id = ?1",
            params![channel_id, fallback],
        )
        .map_err(|error| error.to_string())?;
    Ok(fallback)
}

/// 阻塞版切组：PUT /proxies/{group}。同步 ensure 路径使用（异步路径用 select_group_node）。
pub fn select_group_node_sync(runtime: &ProxyRuntime, group: &str, name: &str) -> Result<(), String> {
    let port = runtime_controller_port(runtime)?;
    let (status, _) = simple_http_request(
        port,
        "PUT",
        &format!("/proxies/{group}"),
        RUNTIME_SECRET,
        Some(&json!({ "name": name }).to_string()),
        Duration::from_secs(3),
    )?;
    if (200..=299).contains(&status) {
        Ok(())
    } else {
        Err(format!("Mihomo 切换节点返回 HTTP {status}"))
    }
}

/// 将 lane 组切到指定节点（带内存去重：重复 ensure 不重复 PUT）。
/// 全局实例重启后 lane_selected 被清空，下一次 ensure 会重新落一次选中。
fn select_lane_node_if_needed(
    runtime: &ProxyRuntime,
    group_name: &str,
    node_id: &str,
) -> Result<(), String> {
    {
        let selected = runtime
            .lane_selected
            .lock()
            .map_err(|_| "lane 选中状态锁定失败".to_string())?;
        if selected.get(group_name).map(String::as_str) == Some(node_id) {
            return Ok(());
        }
    }
    select_group_node_sync(runtime, group_name, node_id)?;
    if let Ok(mut selected) = runtime.lane_selected.lock() {
        selected.insert(group_name.to_string(), node_id.to_string());
    }
    Ok(())
}

/// 确保通道出口就绪并指向其绑定节点：分配通道 lane + 同步切组。
/// 返回该 lane 的监听端口（http://127.0.0.1:{port}）。
pub fn ensure_channel_instance(
    database: &Database,
    runtime: &ProxyRuntime,
    channel_id: &str,
) -> Result<u16, String> {
    let node_id = resolve_channel_node_id(database, channel_id)?;
    ensure_global_runtime(database, runtime)?;
    let mut used_ports = HashSet::new();
    ensure_lane_pools(runtime, &mut used_ports)?;

    let (group_name, port) = {
        let mut map = runtime
            .channel_lane_map
            .lock()
            .map_err(|_| "通道 lane 状态锁定失败".to_string())?;
        let slots = runtime
            .channel_lane_slots
            .lock()
            .map_err(|_| "通道 lane 状态锁定失败".to_string())?;
        let idx = match map.get(channel_id) {
            Some(&idx) if idx < slots.len() => idx,
            _ => {
                let used: HashSet<usize> = map.values().copied().collect();
                let idx = (0..slots.len())
                    .find(|i| !used.contains(i))
                    .ok_or_else(|| format!("通道数量已达到 lane 池上限（{CHANNEL_LANE_POOL}）"))?;
                map.insert(channel_id.to_string(), idx);
                idx
            }
        };
        let lane = &slots[idx];
        (lane.group_name.clone(), lane.listen_port)
    };
    select_lane_node_if_needed(runtime, &group_name, &node_id)?;
    Ok(port)
}

/// 确保账号出口就绪：绑定通道走通道 lane；否则分配账号 lane 并选中节点。
/// `force_node_id` 供故障轮换强制切换；None 时沿用 lane 当前选中节点
/// （新分配或全局实例重启后按游标轮询挑候选）。
/// 返回该出口的监听端口。
pub fn ensure_account_instance(
    database: &Database,
    runtime: &ProxyRuntime,
    profile_id: &str,
    force_node_id: Option<&str>,
) -> Result<u16, String> {
    if let Ok(Some(channel_id)) = read_account_proxy_channel_id(database, profile_id) {
        if !channel_id.trim().is_empty() {
            return ensure_channel_instance(database, runtime, &channel_id);
        }
    }
    ensure_global_runtime(database, runtime)?;
    let mut used_ports = HashSet::new();
    ensure_lane_pools(runtime, &mut used_ports)?;

    let (group_name, port, has_selection) = {
        let mut map = runtime
            .account_lane_map
            .lock()
            .map_err(|_| "账号 lane 状态锁定失败".to_string())?;
        let slots = runtime
            .account_lane_slots
            .lock()
            .map_err(|_| "账号 lane 状态锁定失败".to_string())?;
        let idx = match map.get(profile_id) {
            Some(&idx) if idx < slots.len() => idx,
            _ => {
                let used: HashSet<usize> = map.values().copied().collect();
                let idx = (0..slots.len()).find(|i| !used.contains(i)).ok_or_else(|| {
                    format!("账号出口 lane 池已满（{ACCOUNT_LANE_POOL}），请减少未绑定通道的账号数量")
                })?;
                map.insert(profile_id.to_string(), idx);
                idx
            }
        };
        let lane = &slots[idx];
        let has_selection = runtime
            .lane_selected
            .lock()
            .map(|selected| selected.contains_key(&lane.group_name))
            .unwrap_or(false);
        (lane.group_name.clone(), lane.listen_port, has_selection)
    };

    if let Some(node_id) = force_node_id {
        select_lane_node_if_needed(runtime, &group_name, node_id)?;
    } else if !has_selection {
        // 新分配 lane 或全局实例刚重启：按游标轮询挑候选节点，
        // 修复既往固定取第一个候选导致所有账号集中在同一节点的问题。
        let candidates = channel_candidate_nodes(database, runtime, "")?;
        let seq = runtime.account_alloc_seq.fetch_add(1, Ordering::Relaxed) as usize;
        let (node_id, _, _) = candidates
            .get(seq % candidates.len())
            .cloned()
            .ok_or_else(|| "代理池中没有可用的候选节点".to_string())?;
        select_lane_node_if_needed(runtime, &group_name, &node_id)?;
    }
    Ok(port)
}

pub fn read_account_proxy_channel_id(
    database: &Database,
    profile_id: &str,
) -> Result<Option<String>, String> {
    let connection = database.lock_conn()?;
    connection
        .query_row(
            "SELECT channel_id FROM account_proxy_channels WHERE profile_id = ?1",
            [profile_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| error.to_string())
}

pub fn ensure_shared_instance_with_nodes(
    database: &Database,
    runtime: &ProxyRuntime,
    only_ids: Option<&HashSet<String>>,
) -> Result<u16, String> {
    let (nodes, initial_hash) = runtime_nodes(database, only_ids)?;
    if nodes.is_empty() {
        return Err("代理池中没有配置有效的节点".into());
    }
    let engine =
        find_mihomo_binary(runtime).ok_or("未检测到 Mihomo 组件，请在代理池设置中下载并初始化")?;

    let proxy_port;
    let controller_port;
    {
        let mut state = runtime
            .shared_instance
            .lock()
            .map_err(|_| "共享代理实例锁定失败")?;

        let running = if let Some(child) = state.child.as_mut() {
            child.try_wait().map_err(|e| e.to_string())?.is_none()
        } else {
            false
        };

        if running && state.config_hash == initial_hash && state.proxy_port > 0 {
            return Ok(state.proxy_port);
        }

    // 需要（重）拉起：先停旧进程再复用端口。既往实现直接覆盖 child 句柄，
    // 旧 mihomo 会泄漏为孤儿进程，只能等下次启动清扫回收。
    stop_single_instance(&mut state);
    proxy_port = if state.proxy_port > 0 && port_is_available(state.proxy_port) {
        state.proxy_port
    } else {
        allocate_free_port()?
    };
    controller_port = if state.controller_port > 0 && port_is_available(state.controller_port) {
        state.controller_port
    } else {
        allocate_free_port()?
    };
    drop(state);
    }

    // 定向清扫"该杀未杀"的残留内核：它霸占的 lane 端口会让新实例绑定失败，
    // 流量串到旧实例（测速全失败、出口串节点）。必须在分配 lane 端口前执行。
    reap_stale_mihomo_in_dir(&runtime.directory);

    // lane 池端口在旧进程停止后分配/校验；批内统一去重，避免互相撞端口
    let mut used_ports = HashSet::new();
    used_ports.insert(proxy_port);
    used_ports.insert(controller_port);
    ensure_lane_pools(runtime, &mut used_ports)?;
    let speed_lanes = runtime
        .speed_lane_slots
        .lock()
        .map_err(|_| "lane 池状态锁定失败".to_string())?
        .clone();
    let account_lanes = runtime
        .account_lane_slots
        .lock()
        .map_err(|_| "lane 池状态锁定失败".to_string())?
        .clone();
    let channel_lanes = runtime
        .channel_lane_slots
        .lock()
        .map_err(|_| "lane 池状态锁定失败".to_string())?
        .clone();

    let shared_dir = runtime.directory.join("shared");
    let config_json = runtime_config(
        &nodes,
        proxy_port,
        controller_port,
        &speed_lanes,
        &account_lanes,
        &channel_lanes,
    );
    let listener_ports = speed_lanes
        .iter()
        .chain(&account_lanes)
        .chain(&channel_lanes)
        .map(|lane| lane.listen_port)
        .collect::<Vec<_>>();
    // 配置含 ~89 个组，内核解析耗时更长，就绪等待放宽到 10s
    spawn_engine_instance(
        runtime,
        &engine,
        &shared_dir,
        config_json,
        proxy_port,
        controller_port,
        initial_hash,
        Duration::from_secs(10),
        "共享代理实例",
        &listener_ports,
    )?;
    Ok(proxy_port)
}

/// 写入配置、拉起 mihomo 引擎并等待控制器就绪，随后更新实例状态。
/// `listener_ports` 为本实例配置的全部监听端口（mixed + 各 lane），
/// 就绪判定除控制器 /version 外逐一确认端口真的被新实例绑定——内核对绑定
/// 失败的 listener 只记日志不退出，跳过该校验会让半死实例把测速/出口流量
/// 引到霸占端口的残留实例上。
/// 失败时回收本次拉起的实例，避免半死实例被后续复用或遗留为孤儿进程。
#[allow(clippy::too_many_arguments)]
fn spawn_engine_instance(
    runtime: &ProxyRuntime,
    engine: &Path,
    instance_dir: &Path,
    config_json: JsonValue,
    proxy_port: u16,
    controller_port: u16,
    config_hash: String,
    ready_timeout: Duration,
    label: &str,
    listener_ports: &[u16],
) -> Result<(), String> {
    let _ = fs::create_dir_all(instance_dir);
    let config_path = instance_dir.join("config.yaml");
    fs::write(
        &config_path,
        serde_yaml::to_string(&config_json).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("无法写入代理配置: {e}"))?;

    let log_file = fs::File::create(instance_dir.join("runtime.log"))
        .map_err(|e| format!("无法创建日志: {e}"))?;
    let err_file = log_file.try_clone().map_err(|e| e.to_string())?;

    let child = Command::new(engine)
        .arg("-d")
        .arg(instance_dir)
        .arg("-f")
        .arg(&config_path)
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(err_file))
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .env_remove("all_proxy")
        .env_remove("SOCKS_PROXY")
        .env_remove("socks_proxy")
        .env("NO_PROXY", "*")
        .env("no_proxy", "*")
        .spawn()
        .map_err(|e| format!("无法启动代理进程: {e}"))?;

    {
        let mut state = runtime
            .shared_instance
            .lock()
            .map_err(|_| "共享代理实例锁定失败")?;
        state.child = Some(child);
        state.directory = instance_dir.to_path_buf();
        state.config_hash = config_hash;
        state.engine_path = engine.display().to_string();
        state.proxy_port = proxy_port;
        state.controller_port = controller_port;
        state.last_error.clear();
    }

    let started = Instant::now();
    let mut last_err = String::new();
    while started.elapsed() < ready_timeout {
        match simple_http_get(
            controller_port,
            "/version",
            RUNTIME_SECRET,
            Duration::from_millis(200),
        ) {
            Ok((200..=299, _)) => {
                // 控制器就绪后还需确认全部监听端口真的绑定成功：端口连不上
                // 说明新实例没绑上，流量会落到残留实例的旧监听。
                let unbound = std::iter::once(proxy_port)
                    .chain(listener_ports.iter().copied())
                    .filter(|port| !port_has_listener(*port))
                    .collect::<Vec<_>>();
                if unbound.is_empty() {
                    // 新进程的组选择全部复位，lane 选中记录随之失效
                    if let Ok(mut selected) = runtime.lane_selected.lock() {
                        selected.clear();
                    }
                    return Ok(());
                }
                last_err = format!("监听端口未就绪: {unbound:?}");
            }
            Ok((code, _)) => last_err = format!("HTTP {code}"),
            Err(e) => last_err = e,
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // 就绪超时：回收本次拉起的实例，避免半死实例被后续复用或遗留为孤儿进程
    if let Ok(mut state) = runtime.shared_instance.lock() {
        stop_single_instance(&mut state);
    }
    Err(format!("{label}就绪超时：{last_err}"))
}

#[derive(Debug, Clone)]
pub struct SpeedLanePlan {
    /// lane 对应的 mihomo select 组名（SPEED-lane-{i}）
    pub group_name: String,
    /// 该 lane 专属的本地监听端口（127.0.0.1），流量固定走组内当前选中节点
    pub listen_port: u16,
}

#[derive(Debug, Clone)]
pub struct SpeedTestPlan {
    pub lanes: Vec<SpeedLanePlan>,
}

/// 全局单实例内的固定测速 lane 计划。
/// 配置生成时节点名已被 runtime_nodes 改写为节点 id，因此 mihomo 代理名 == 节点 id。
pub fn speed_test_plan(runtime: &ProxyRuntime) -> Result<SpeedTestPlan, String> {
    let slots = runtime
        .speed_lane_slots
        .lock()
        .map_err(|_| "测速 lane 状态锁定失败".to_string())?;
    if slots.len() < SPEED_TEST_LANES {
        return Err("测速 lane 尚未初始化".to_string());
    }
    Ok(SpeedTestPlan {
        lanes: slots[..SPEED_TEST_LANES]
            .iter()
            .map(|lane| SpeedLanePlan {
                group_name: lane.group_name.clone(),
                listen_port: lane.listen_port,
            })
            .collect(),
    })
}

pub fn ensure_runtime(
    database: &Database,
    runtime: &ProxyRuntime,
    only_ids: Option<&HashSet<String>>,
    _cancelled: Option<&CancellationToken>,
) -> Result<(), String> {
    ensure_shared_instance_with_nodes(database, runtime, only_ids).map(|_| ())
}

pub async fn select_group_node(
    runtime: &ProxyRuntime,
    group: &str,
    name: &str,
) -> Result<(), String> {
    let port = runtime_controller_port(runtime)?;
    let mut url =
        Url::parse(&controller_url(port, "/proxies/")).map_err(|error| error.to_string())?;
    append_controller_path(&mut url, &[group])?;
    let response = controller_client()?
        .put(url)
        .bearer_auth(RUNTIME_SECRET)
        .json(&json!({ "name": name }))
        .send()
        .await
        .map_err(|error| format!("切换代理节点失败：{error}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!(
            "Mihomo 切换节点返回 HTTP {}",
            response.status().as_u16()
        ))
    }
}

pub async fn select_runtime_node(runtime: &ProxyRuntime, name: &str) -> Result<(), String> {
    select_group_node(runtime, RUNTIME_GROUP, name).await
}

pub fn restore_saved_proxy(database: &Database, runtime: &ProxyRuntime) {
    let active = read_meta(database, ACTIVE_PROXY_NODE_KEY).unwrap_or_default();
    if active.is_empty() {
        if let Ok(connection) = database.0.lock() {
            let _ = write_meta(&connection, NETWORK_PROXY_KEY, "");
        }
        return;
    }
    if let Err(error) = ensure_runtime(database, runtime, None, None) {
        if let Ok(connection) = database.0.lock() {
            let _ = write_meta(&connection, ACTIVE_PROXY_NODE_KEY, "");
            let _ = write_meta(&connection, NETWORK_PROXY_KEY, "");
        }
        if let Ok(mut state) = runtime.shared_instance.lock() {
            state.last_error = error;
        }
    } else if let Ok(connection) = database.0.lock() {
        let proxy_url = runtime_proxy_url(runtime);
        let _ = write_meta(&connection, NETWORK_PROXY_KEY, &proxy_url);
    }
}

#[allow(dead_code)]
pub fn proxy_error_index(output: &str) -> Option<usize> {
    let marker = output.rfind("proxy ")? + "proxy ".len();
    let digits = output[marker..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

// ---------------------------------------------------------------------------
// 孤儿 Mihomo 进程自动淘汰
// ---------------------------------------------------------------------------

/// 判定一条进程命令行是否属于「挂在 OpenHub 运行时下的 Mihomo 内核」。
///
/// 双重归属标识（缺一不可）：命令行同时携带本应用数据目录标识（openhub）
/// 与代理运行时目录（proxy-runtime）。
///
/// 内核名放宽为「以 mihomo 结尾」：除自研的 mihomo/mihomo.exe 外，
/// 还覆盖历史上以 verge-mihomo 等二进制拉起 OpenHub 运行时的异常孤儿
/// （实测存在 PPID=1、加载 OpenHub 配置的 verge-mihomo 残留）。
/// 正常运行的 Clash Verge 实例路径为 io.github.clash-verge-rev.*，
/// 不含归属标识，绝不会被误杀。
pub(crate) fn is_orphan_mihomo_command(command: &str) -> bool {
    let lower = command.to_lowercase();
    if !lower.contains("openhub") || !lower.contains("proxy-runtime") {
        return false;
    }
    // 内核特征：任一空白分隔 token 以 mihomo(.exe) 结尾（mihomo / verge-mihomo 等）。
    // macOS 路径普遍含空格（如 "Application Support"），无法按首个空格切出 exe，
    // 但 exe 名与其后参数之间必有空白，按 token 尾段判定即可完整覆盖。
    lower.split_whitespace().any(|token| {
        let path_end = token.trim_end_matches(['\\', '/']);
        let file_name = path_end.rsplit(['/', '\\']).next().unwrap_or(path_end);
        let stem = file_name.strip_suffix(".exe").unwrap_or(file_name);
        stem.ends_with("mihomo")
    })
}

fn list_process_commands() -> Vec<(u32, String)> {
    #[cfg(target_os = "windows")]
    {
        // PowerShell 输出 "pid|commandline"，逐行解析
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "Get-CimInstance Win32_Process | Where-Object { $_.Name -like 'mihomo*' } \
                 | ForEach-Object { \"\" + $_.ProcessId + \"|\" + $_.CommandLine }",
            ])
            .output();
        let Ok(output) = output else {
            return Vec::new();
        };
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let (pid, cmd) = line.split_once('|')?;
                Some((pid.trim().parse().ok()?, cmd.to_string()))
            })
            .collect()
    }
    #[cfg(not(target_os = "windows"))]
    {
        // macOS/Linux: ps -axo pid=,command= → "  1234 /path/to/exe args..."
        let Ok(output) = Command::new("ps").args(["-axo", "pid=,command="]).output() else {
            return Vec::new();
        };
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim_start();
                let (pid, rest) = trimmed.split_once(char::is_whitespace)?;
                Some((pid.parse().ok()?, rest.to_string()))
            })
            .collect()
    }
}

fn kill_pid(pid: u32) {
    #[cfg(target_os = "windows")]
    let _ = Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]).status();
    #[cfg(not(target_os = "windows"))]
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
}

/// 清扫历史会话遗留的 OpenHub Mihomo 孤儿进程（自动淘汰）。
///
/// 泄漏来源：应用崩溃/强杀时 `ProxyRuntime::Drop` 未执行；旧版本就绪超时路径未回收子进程。
/// 这些进程的父应用早已不在，却持续占用端口与内存。
///
/// **仅允许在应用启动早期、任何实例 spawn 之前调用**：
/// 此刻本会话尚无活跃实例，清扫天然不会误伤当前正在使用的内核。
/// 返回本次淘汰的进程数量。
pub fn reap_orphan_mihomo_processes() -> usize {
    let mut killed = 0usize;
    for (pid, command) in list_process_commands() {
        if is_orphan_mihomo_command(&command) {
            kill_pid(pid);
            killed += 1;
        }
    }
    if killed > 0 {
        warn!("[ProxyPool] 启动清扫：已自动淘汰 {killed} 个遗留的 Mihomo 孤儿进程");
    }
    killed
}

/// 实例重建前的定向清扫：回收挂在本运行时目录下的残留 Mihomo。
///
/// `stop_single_instance` 只能杀本会话句柄内的子进程；句柄丢失（历史缺陷遗留）
/// 或上一次 kill 未生效的旧内核会继续霸占 lane 监听端口，导致新实例绑定失败、
/// lane 流量串到旧实例。按完整运行时目录匹配（dev 与生产目录互不影响），
/// 在 spawn 新实例之前调用，天然不会误伤刚拉起的内核。
/// 返回本次回收的进程数量。
pub(crate) fn reap_stale_mihomo_in_dir(directory: &Path) -> usize {
    let dir_token = directory.to_string_lossy().to_lowercase();
    if dir_token.is_empty() {
        return 0;
    }
    let mut killed = 0usize;
    for (pid, command) in list_process_commands() {
        if command.to_lowercase().contains(&dir_token) && is_orphan_mihomo_command(&command) {
            kill_pid(pid);
            killed += 1;
        }
    }
    if killed > 0 {
        warn!("[ProxyPool] 实例重建：已回收 {killed} 个占用运行时目录的残留 Mihomo 进程");
    }
    killed
}
