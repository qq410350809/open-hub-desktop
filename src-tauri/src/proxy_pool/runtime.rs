pub(crate) use crate::db::{read_meta, write_meta};
use crate::models::*;
use crate::proxy_pool::geoip::{classify_node_location, open_geoip_reader};
use crate::proxy_pool::parser::{
    basic_node_config_error, canonical_json, sanitize_proxy_node_json, stable_id,
};
use crate::proxy_pool::rotator::channel_candidate_nodes;
use crate::proxy_pool::types::*;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value as JsonValue};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
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
    let detected = find_mihomo_binary();
    let mut path = detected
        .as_ref()
        .map(|item| item.display().to_string())
        .unwrap_or_default();
    let mut error = if detected.is_none() {
        "未检测到 OpenHub 内置 Mihomo 内核，请在【设置】中一键下载安装"
            .to_string()
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

pub fn find_mihomo_binary() -> Option<PathBuf> {
    if let Ok(value) = std::env::var("OPENHUB_MIHOMO_PATH") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let home_path = PathBuf::from(home);
        #[cfg(target_os = "macos")]
        let candidate = home_path.join("Library/Application Support/com.dfeer.openhub.desktop/bin/mihomo");
        #[cfg(target_os = "windows")]
        let candidate = home_path.join("AppData/Roaming/com.dfeer.openhub.desktop/bin/mihomo.exe");
        #[cfg(target_os = "linux")]
        let candidate = home_path.join(".config/com.dfeer.openhub.desktop/bin/mihomo");

        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub fn port_is_available(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
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

#[derive(Debug, Clone)]
pub struct ChannelRuntimeConfig {
    pub channel_id: String,
    pub port: u16,
    pub node_names: Vec<String>,
}

pub fn runtime_config(
    nodes: &[RuntimeNode],
    proxy_port: u16,
    controller_port: u16,
    channel_configs: &[ChannelRuntimeConfig],
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
        .filter_map(|node| node.get("name").and_then(JsonValue::as_str).map(String::from))
        .collect::<Vec<_>>();

    let mut listeners = Vec::new();
    let mut proxy_groups = Vec::new();

    proxy_groups.push(json!({
        "name": RUNTIME_GROUP,
        "type": "select",
        "proxies": all_node_names.clone()
    }));

    for ch in channel_configs {
        let group_name = format!("CHANNEL-{}", ch.channel_id);
        let group_proxies = if ch.node_names.is_empty() {
            &all_node_names
        } else {
            &ch.node_names
        };
        proxy_groups.push(json!({
            "name": group_name,
            "type": "select",
            "proxies": group_proxies
        }));
        listeners.push(json!({
            "name": format!("listener-{}", ch.channel_id),
            "type": "mixed",
            "port": ch.port,
            "listen": "127.0.0.1",
            "proxy": group_name
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
        eprintln!("OpenHub 代理节点已跳过：{id}：{error}");
    }
    let hash = stable_id(&[&serde_json::to_string(
        &nodes.iter().map(|node| &node.config).collect::<Vec<_>>(),
    )
    .unwrap_or_default()]);
    Ok((nodes, hash))
}

pub fn simple_http_get(port: u16, path: &str, secret: &str, timeout: Duration) -> Result<(u16, String), String> {
    use std::io::{Read, Write};
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = std::net::TcpStream::connect_timeout(&addr, timeout).map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n",
        path, port, secret
    );
    stream.write_all(request.as_bytes()).map_err(|e| e.to_string())?;

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

pub fn spawn_dedicated_single_node_instance(
    engine: &PathBuf,
    instance_dir: &PathBuf,
    node_id: &str,
    raw_json: &str,
) -> Result<InstanceState, String> {
    let mut config: JsonValue = serde_json::from_str(raw_json).map_err(|e| e.to_string())?;
    sanitize_proxy_node_json(&mut config);
    if let Some(obj) = config.as_object_mut() {
        obj.insert("name".into(), JsonValue::String(node_id.to_string()));
        for key in ["dialer-proxy", "proxy", "interface-name", "routing-mark"] {
            obj.remove(key);
        }
        let tls_on = obj.get("tls").and_then(|v| v.as_bool()).unwrap_or(false)
            || obj
                .get("type")
                .and_then(|v| v.as_str())
                .map(|t| matches!(t, "trojan" | "hysteria2" | "tuic"))
                .unwrap_or(false);
        if tls_on && !obj.contains_key("skip-cert-verify") {
            obj.insert("skip-cert-verify".into(), JsonValue::Bool(true));
        }
    }

    let node_hash = stable_id(&[node_id, &canonical_json(&config, true).to_string()]);
    let proxy_port = allocate_free_port()?;
    let controller_port = allocate_free_port()?;
    let _ = fs::create_dir_all(instance_dir);

    let single_node_config = json!({
        "mixed-port": proxy_port,
        "external-controller": format!("127.0.0.1:{controller_port}"),
        "secret": RUNTIME_SECRET,
        "allow-lan": false,
        "bind-address": "127.0.0.1",
        "mode": "rule",
        "log-level": "warning",
        "ipv6": false,
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
        "proxies": [config],
        "proxy-groups": [
            {
                "name": "GLOBAL",
                "type": "select",
                "proxies": [node_id]
            }
        ],
        "rules": [
            "IP-CIDR,127.0.0.0/8,DIRECT,no-resolve",
            "IP-CIDR,10.0.0.0/8,DIRECT,no-resolve",
            "IP-CIDR,172.16.0.0/12,DIRECT,no-resolve",
            "IP-CIDR,192.168.0.0/16,DIRECT,no-resolve",
            "MATCH,GLOBAL"
        ]
    });

    let config_path = instance_dir.join("config.yaml");
    fs::write(
        &config_path,
        serde_yaml::to_string(&single_node_config).map_err(|e| e.to_string())?,
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

    let started = Instant::now();
    let mut last_err = String::new();
    while started.elapsed() < Duration::from_secs(4) {
        match simple_http_get(controller_port, "/version", RUNTIME_SECRET, Duration::from_millis(200)) {
            Ok((200..=299, _)) => {
                return Ok(InstanceState {
                    child: Some(child),
                    directory: instance_dir.clone(),
                    config_hash: node_hash,
                    engine_path: engine.display().to_string(),
                    last_error: String::new(),
                    proxy_port,
                    controller_port,
                });
            }
            Ok((code, _)) => last_err = format!("HTTP {code}"),
            Err(e) => last_err = e,
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    Err(format!("代理实例就绪超时：{last_err}"))
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

pub fn ensure_channel_instance(
    database: &Database,
    runtime: &ProxyRuntime,
    channel_id: &str,
) -> Result<u16, String> {
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

    let (assigned_id, raw_json) = if let Some(id) = node_id {
        let row = connection
            .query_row(
                "SELECT id, raw_json FROM proxy_pool_nodes WHERE id = ?1",
                [&id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if let Some(r) = row {
            r
        } else {
            let fallback = connection
                .query_row(
                    "SELECT id, raw_json FROM proxy_pool_nodes WHERE is_enabled = 1 ORDER BY latency_ms ASC, name ASC LIMIT 1",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "代理池中没有可用的代理节点".to_string())?;
            connection
                .execute(
                    "UPDATE proxy_channels SET node_id = ?2 WHERE id = ?1",
                    params![channel_id, fallback.0],
                )
                .map_err(|error| error.to_string())?;
            fallback
        }
    } else {
        let fallback = connection
            .query_row(
                "SELECT id, raw_json FROM proxy_pool_nodes WHERE is_enabled = 1 ORDER BY latency_ms ASC, name ASC LIMIT 1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "代理池中没有可用的代理节点".to_string())?;
        connection
            .execute(
                "UPDATE proxy_channels SET node_id = ?2 WHERE id = ?1",
                params![channel_id, fallback.0],
            )
            .map_err(|error| error.to_string())?;
        fallback
    };
    drop(connection);

    let engine = find_mihomo_binary().ok_or("未检测到 OpenHub 内置 Mihomo 内核，请在【设置】中一键下载安装")?;
    let channel_dir = runtime.directory.join("channels").join(channel_id);

    let mut instances = runtime
        .channel_instances
        .lock()
        .map_err(|_| "通道代理实例状态锁定失败")?;

    if let Some(inst) = instances.get_mut(channel_id) {
        let running = if let Some(child) = inst.child.as_mut() {
            child.try_wait().map_err(|e| e.to_string())?.is_none()
        } else {
            false
        };
        if running && inst.proxy_port > 0 {
            return Ok(inst.proxy_port);
        }
        stop_single_instance(inst);
    }

    let inst = spawn_dedicated_single_node_instance(&engine, &channel_dir, &assigned_id, &raw_json)?;
    let port = inst.proxy_port;
    instances.insert(channel_id.to_string(), inst);
    Ok(port)
}

pub fn ensure_account_instance(
    database: &Database,
    runtime: &ProxyRuntime,
    profile_id: &str,
) -> Result<u16, String> {
    if let Ok(Some(channel_id)) = read_account_proxy_channel_id(database, profile_id) {
        if !channel_id.trim().is_empty() {
            return ensure_channel_instance(database, runtime, &channel_id);
        }
    }

    let engine = find_mihomo_binary().ok_or("未检测到 OpenHub 内置 Mihomo 内核，请在【设置】中一键下载安装")?;
    let account_dir = runtime.directory.join("accounts").join(profile_id);

    let mut instances = runtime
        .account_instances
        .lock()
        .map_err(|_| "账号代理实例状态锁定失败")?;

    if let Some(inst) = instances.get_mut(profile_id) {
        let running = if let Some(child) = inst.child.as_mut() {
            child.try_wait().map_err(|e| e.to_string())?.is_none()
        } else {
            false
        };
        if running && inst.proxy_port > 0 {
            return Ok(inst.proxy_port);
        }
        stop_single_instance(inst);
    }

    let candidates = channel_candidate_nodes(database, runtime, "")?;
    let (best_node_id, _, _) = candidates
        .first()
        .cloned()
        .ok_or_else(|| "代理池中没有可用的候选节点".to_string())?;

    let connection = database.lock_conn()?;
    let raw_json: String = connection
        .query_row(
            "SELECT raw_json FROM proxy_pool_nodes WHERE id = ?1",
            [&best_node_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("读取节点数据失败：{e}"))?;
    drop(connection);

    let inst = spawn_dedicated_single_node_instance(&engine, &account_dir, &best_node_id, &raw_json)?;
    let port = inst.proxy_port;
    instances.insert(profile_id.to_string(), inst);
    Ok(port)
}

#[allow(dead_code)]
pub fn ensure_shared_instance(
    database: &Database,
    runtime: &ProxyRuntime,
) -> Result<u16, String> {
    ensure_shared_instance_with_nodes(database, runtime, None)
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
    let engine = find_mihomo_binary().ok_or("未检测到 OpenHub 内置 Mihomo 内核，请在【设置】中一键下载安装")?;

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

    stop_single_instance(&mut state);

    let proxy_port = if state.proxy_port > 0 && port_is_available(state.proxy_port) {
        state.proxy_port
    } else {
        allocate_free_port()?
    };

    let controller_port = if state.controller_port > 0 && port_is_available(state.controller_port) {
        state.controller_port
    } else {
        allocate_free_port()?
    };

    let shared_dir = runtime.directory.join("shared");
    let _ = fs::create_dir_all(&shared_dir);

    let config_json = runtime_config(&nodes, proxy_port, controller_port, &[]);
    let config_path = shared_dir.join("config.yaml");
    fs::write(
        &config_path,
        serde_yaml::to_string(&config_json).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("无法写入共享代理配置: {e}"))?;

    let log_file = fs::File::create(shared_dir.join("runtime.log"))
        .map_err(|e| format!("无法创建共享代理日志: {e}"))?;
    let err_file = log_file.try_clone().map_err(|e| e.to_string())?;

    let child = Command::new(&engine)
        .arg("-d")
        .arg(&shared_dir)
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
        .map_err(|e| format!("无法启动共享代理进程: {e}"))?;

    state.child = Some(child);
    state.directory = shared_dir;
    state.config_hash = initial_hash;
    state.engine_path = engine.display().to_string();
    state.proxy_port = proxy_port;
    state.controller_port = controller_port;
    state.last_error.clear();
    drop(state);

    let started = Instant::now();
    let mut last_err = String::new();
    while started.elapsed() < Duration::from_secs(6) {
        match simple_http_get(controller_port, "/version", RUNTIME_SECRET, Duration::from_millis(200)) {
            Ok((200..=299, _)) => return Ok(proxy_port),
            Ok((code, _)) => last_err = format!("HTTP {code}"),
            Err(e) => last_err = e,
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    Err(format!("共享代理实例就绪超时：{last_err}"))
}

pub fn ensure_runtime(
    database: &Database,
    runtime: &ProxyRuntime,
    only_ids: Option<&HashSet<String>>,
    _cancelled: Option<&CancellationToken>,
) -> Result<(), String> {
    ensure_shared_instance_with_nodes(database, runtime, only_ids).map(|_| ())
}

pub fn wait_runtime_ready(
    controller_port: u16,
    _expected_nodes: usize,
    cancelled: Option<&CancellationToken>,
) -> Result<(), String> {
    let started = Instant::now();
    let deadline = Duration::from_secs(8);
    let mut last_error = "控制器尚未就绪".to_string();
    while started.elapsed() < deadline {
        if let Some(token) = cancelled {
            if token.is_cancelled() {
                return Err("测速已取消".into());
            }
        }
        match simple_http_get(controller_port, "/version", RUNTIME_SECRET, Duration::from_millis(400)) {
            Ok((200..=299, _)) => {
                std::thread::sleep(Duration::from_millis(50));
                return Ok(());
            }
            Ok((code, _)) => {
                last_error = format!("Mihomo /version 返回 HTTP {code}");
            }
            Err(e) => {
                last_error = format!("Mihomo /version 未就绪: {e}");
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!("Mihomo 测速就绪超时：{last_error}"))
}

pub async fn select_group_node(runtime: &ProxyRuntime, group: &str, name: &str) -> Result<(), String> {
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
