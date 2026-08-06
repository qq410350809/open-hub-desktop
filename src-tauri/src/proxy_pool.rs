use crate::db::build_http_client;
use crate::models::*;
use base64::{engine::general_purpose, Engine as _};
use futures_util::{future, pin_mut, stream, StreamExt};
use maxminddb::{geoip2, Reader};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Map as JsonMap, Value as JsonValue};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, State};
use tokio_util::sync::CancellationToken;
use url::Url;

const DEFAULT_RUNTIME_PROXY_PORT: u16 = 17890;
const DEFAULT_RUNTIME_CONTROLLER_PORT: u16 = 19090;
const RUNTIME_SECRET: &str = "openhub-local-proxy-runtime";
const RUNTIME_GROUP: &str = "OpenHub";
const BATCH_PROXY_TEST_TIMEOUT_MS: &str = "3000";
const BATCH_PROXY_TEST_CONCURRENCY: usize = 50;

#[derive(Debug, Clone)]
struct ParsedNode {
    id: String,
    name: String,
    proxy_type: String,
    server: String,
    port: i64,
    cipher: String,
    udp: bool,
    raw_json: JsonValue,
}

#[derive(Debug, Clone)]
struct RuntimeNode {
    id: String,
    config: JsonValue,
}

struct RuntimeState {
    child: Option<Child>,
    config_hash: String,
    engine_path: String,
    last_error: String,
    proxy_port: u16,
    controller_port: u16,
}

struct ActiveProxyTest {
    id: u64,
    cancellation: CancellationToken,
}

pub(crate) struct ProxyRuntime {
    directory: PathBuf,
    inner: Mutex<RuntimeState>,
    active_test: Mutex<Option<ActiveProxyTest>>,
    next_test_id: AtomicU64,
}

struct ProxyTestLease<'a> {
    runtime: &'a ProxyRuntime,
    id: u64,
    cancellation: CancellationToken,
}

impl ProxyRuntime {
    pub(crate) fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            inner: Mutex::new(RuntimeState {
                child: None,
                config_hash: String::new(),
                engine_path: String::new(),
                last_error: String::new(),
                proxy_port: 0,
                controller_port: 0,
            }),
            active_test: Mutex::new(None),
            next_test_id: AtomicU64::new(1),
        }
    }

    fn start_proxy_test(&self) -> Result<ProxyTestLease<'_>, String> {
        let mut active = self
            .active_test
            .lock()
            .map_err(|_| "测速任务状态锁定失败")?;
        if active.is_some() {
            return Err("已有代理测速任务正在进行".into());
        }
        let id = self.next_test_id.fetch_add(1, Ordering::Relaxed);
        let cancellation = CancellationToken::new();
        *active = Some(ActiveProxyTest {
            id,
            cancellation: cancellation.clone(),
        });
        Ok(ProxyTestLease {
            runtime: self,
            id,
            cancellation,
        })
    }

    fn cancel_proxy_test(&self) -> Result<bool, String> {
        let active = self
            .active_test
            .lock()
            .map_err(|_| "测速任务状态锁定失败")?;
        let Some(test) = active.as_ref() else {
            return Ok(false);
        };
        test.cancellation.cancel();
        Ok(true)
    }
}

impl Drop for ProxyTestLease<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.runtime.active_test.lock() {
            if active.as_ref().is_some_and(|test| test.id == self.id) {
                *active = None;
            }
        }
    }
}

impl Drop for ProxyRuntime {
    fn drop(&mut self) {
        if let Ok(state) = self.inner.get_mut() {
            if let Some(child) = state.child.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn stable_id(parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    hex::encode(digest.finalize())
}

fn read_meta(database: &Database, key: &str) -> Result<String, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    connection
        .query_row("SELECT value FROM app_meta WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()
        .map(|value| value.unwrap_or_default())
        .map_err(|error| error.to_string())
}

fn write_meta(connection: &rusqlite::Connection, key: &str, value: &str) -> Result<(), String> {
    connection
        .execute(
            "INSERT INTO app_meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn row_subscription(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProxySubscription> {
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

fn runtime_info(runtime: &ProxyRuntime) -> (bool, String, String) {
    let detected = find_mihomo_binary();
    let mut path = detected
        .as_ref()
        .map(|item| item.display().to_string())
        .unwrap_or_default();
    let mut error = if detected.is_none() {
        "未找到 Mihomo 内核；请安装 Clash Verge、Clash Party，或通过 OPENHUB_MIHOMO_PATH 指定内核"
            .to_string()
    } else {
        String::new()
    };
    if let Ok(state) = runtime.inner.lock() {
        if !state.engine_path.is_empty() {
            path = state.engine_path.clone();
        }
        if !state.last_error.is_empty() {
            error = state.last_error.clone();
        }
    }
    (!path.is_empty(), path, error)
}

fn load_state(database: &Database, runtime: &ProxyRuntime) -> Result<ProxyPoolState, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
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
                    COALESCE(GROUP_CONCAT(DISTINCT s.name), '')
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

    // 旧数据若缺国家字段，导入名推断后写回，避免分组时再全量分析。
    let mut rows = rows;
    let mut dirty = false;
    for node in &mut rows {
        if !node.country_code.trim().is_empty() && !node.country_name.trim().is_empty() {
            continue;
        }
        let (code, name, class, ip) =
            classify_node_location(&node.name, &node.server, node.port, None);
        node.country_code = code;
        node.country_name = name;
        if node.classification.trim().is_empty() {
            node.classification = class;
        }
        if node.primary_ip.trim().is_empty() && !ip.is_empty() {
            node.primary_ip = ip;
        }
        connection
            .execute(
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
            )
            .map_err(|error| error.to_string())?;
        dirty = true;
    }
    let _ = dirty;

    let meta = |key: &str| -> Result<String, String> {
        connection
            .query_row("SELECT value FROM app_meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map(|value| value.unwrap_or_default())
            .map_err(|error| error.to_string())
    };
    let mut active_node_id = meta(ACTIVE_PROXY_NODE_KEY)?;
    let active_node = rows.iter().find(|node| node.id == active_node_id).cloned();
    if active_node.is_none() {
        active_node_id.clear();
    }
    let network_proxy = meta(NETWORK_PROXY_KEY)?;
    let ignore_addresses = {
        let value = meta(PROXY_IGNORE_KEY)?;
        if value.trim().is_empty() {
            DEFAULT_PROXY_IGNORE.to_string()
        } else {
            value
        }
    };
    let speed_test_url = {
        let value = meta(PROXY_SPEED_TEST_URL_KEY)?;
        if value.trim().is_empty() || is_legacy_google_speed_test_url(&value) {
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

fn classify_ip(ip: IpAddr) -> &'static str {
    match ip {
        IpAddr::V4(value) => {
            if value.is_loopback() {
                "loopback"
            } else if value.is_private() {
                "private"
            } else if value.is_link_local() {
                "linkLocal"
            } else if value.is_unspecified() {
                "unspecified"
            } else {
                "public"
            }
        }
        IpAddr::V6(value) => {
            let first = value.segments()[0];
            if value.is_loopback() {
                "loopback"
            } else if value.is_unspecified() {
                "unspecified"
            } else if value.is_unicast_link_local() {
                "linkLocal"
            } else if first & 0xfe00 == 0xfc00 {
                "private"
            } else {
                "public"
            }
        }
    }
}

fn classify_node_location(
    name: &str,
    server: &str,
    port: i64,
    geoip_reader: Option<&Reader<Vec<u8>>>,
) -> (String, String, String, String) {
    // 导入时优先用节点名推断国家（快）；IP/GeoIP 仅作补充。
    if let Ok(ip) = server.parse::<IpAddr>() {
        let classification = classify_ip(ip).to_string();
        if classification == "local" {
            return (
                "LOCAL".to_string(),
                "本地网络".to_string(),
                classification,
                ip.to_string(),
            );
        }
        if let Some(reader) = geoip_reader {
            if let Some((code, country_name)) = geoip_country(reader, ip) {
                return (code, country_name, classification, ip.to_string());
            }
        }
        if let Some((code, country_name)) = inferred_country(name) {
            return (code, country_name, classification, ip.to_string());
        }
        return (
            "ZZ".to_string(),
            "未知地区".to_string(),
            classification,
            ip.to_string(),
        );
    }

    if let Some((code, country_name)) = inferred_country(name) {
        // 域名场景不在导入时做 DNS，避免 6000+ 节点卡死；仅记名称国家。
        let _ = port;
        return (code, country_name, "public".to_string(), String::new());
    }
    let _ = (port, geoip_reader);
    (
        "ZZ".to_string(),
        "未知地区".to_string(),
        "unresolved".to_string(),
        String::new(),
    )
}

fn find_geoip_database(runtime: &ProxyRuntime) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("OPENHUB_GEOIP_DB") {
        candidates.push(PathBuf::from(path));
    }
    candidates.extend([
        runtime.directory.join("Country.mmdb"),
        runtime.directory.join("GeoLite2-Country.mmdb"),
    ]);
    if let Some(parent) = runtime.directory.parent() {
        candidates.push(parent.join("Country.mmdb"));
        candidates.push(parent.join("GeoLite2-Country.mmdb"));
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        candidates.extend([
            home.join("Library/Application Support/io.github.clash-verge-rev.clash-verge-rev/Country.mmdb"),
            home.join("Library/Application Support/mihomo-party/work/country.mmdb"),
            home.join("Library/Application Support/mihomo-party/test/country.mmdb"),
            home.join(".config/mihomo/Country.mmdb"),
            home.join(".config/clash/Country.mmdb"),
            home.join(".local/share/GeoIP/GeoLite2-Country.mmdb"),
        ]);
    }
    candidates.extend([
        PathBuf::from("/opt/homebrew/share/GeoIP/GeoLite2-Country.mmdb"),
        PathBuf::from("/usr/local/share/GeoIP/GeoLite2-Country.mmdb"),
        PathBuf::from("/usr/share/GeoIP/GeoLite2-Country.mmdb"),
    ]);
    candidates.into_iter().find(|path| path.is_file())
}

fn geoip_country(reader: &Reader<Vec<u8>>, ip: IpAddr) -> Option<(String, String)> {
    let result = reader.lookup(ip).ok()?;
    let record = result.decode::<geoip2::Country>().ok()??;
    for country in [&record.country, &record.registered_country] {
        let Some(raw_code) = country.iso_code else {
            continue;
        };
        let code = raw_code.trim().to_ascii_uppercase();
        if code.len() != 2
            || !code
                .chars()
                .all(|character| character.is_ascii_alphabetic())
        {
            continue;
        }
        let name = country
            .names
            .simplified_chinese
            .or(country.names.english)
            .unwrap_or(code.as_str())
            .to_string();
        return Some((code, name));
    }
    None
}

fn inferred_country(value: &str) -> Option<(String, String)> {
    let lower = value.to_ascii_lowercase();
    let text_matches = [
        ("🇭🇰", "HK", "香港"),
        ("香港", "HK", "香港"),
        ("hong kong", "HK", "香港"),
        ("🇹🇼", "TW", "台湾"),
        ("台湾", "TW", "台湾"),
        ("taiwan", "TW", "台湾"),
        ("🇯🇵", "JP", "日本"),
        ("日本", "JP", "日本"),
        ("japan", "JP", "日本"),
        ("东京", "JP", "日本"),
        ("大阪", "JP", "日本"),
        ("🇸🇬", "SG", "新加坡"),
        ("新加坡", "SG", "新加坡"),
        ("singapore", "SG", "新加坡"),
        ("狮城", "SG", "新加坡"),
        ("🇺🇸", "US", "美国"),
        ("美国", "US", "美国"),
        ("united states", "US", "美国"),
        ("洛杉矶", "US", "美国"),
        ("硅谷", "US", "美国"),
        ("🇰🇷", "KR", "韩国"),
        ("韩国", "KR", "韩国"),
        ("korea", "KR", "韩国"),
        ("首尔", "KR", "韩国"),
        ("🇬🇧", "GB", "英国"),
        ("英国", "GB", "英国"),
        ("united kingdom", "GB", "英国"),
        ("london", "GB", "英国"),
        ("🇩🇪", "DE", "德国"),
        ("德国", "DE", "德国"),
        ("germany", "DE", "德国"),
        ("🇫🇷", "FR", "法国"),
        ("法国", "FR", "法国"),
        ("france", "FR", "法国"),
        ("🇳🇱", "NL", "荷兰"),
        ("荷兰", "NL", "荷兰"),
        ("netherlands", "NL", "荷兰"),
        ("🇨🇦", "CA", "加拿大"),
        ("加拿大", "CA", "加拿大"),
        ("canada", "CA", "加拿大"),
        ("🇦🇺", "AU", "澳大利亚"),
        ("澳大利亚", "AU", "澳大利亚"),
        ("australia", "AU", "澳大利亚"),
        ("🇷🇺", "RU", "俄罗斯"),
        ("俄罗斯", "RU", "俄罗斯"),
        ("russia", "RU", "俄罗斯"),
        ("🇮🇳", "IN", "印度"),
        ("印度", "IN", "印度"),
        ("india", "IN", "印度"),
        ("🇹🇷", "TR", "土耳其"),
        ("土耳其", "TR", "土耳其"),
        ("turkey", "TR", "土耳其"),
        ("🇧🇷", "BR", "巴西"),
        ("巴西", "BR", "巴西"),
        ("brazil", "BR", "巴西"),
        ("🇲🇾", "MY", "马来西亚"),
        ("马来西亚", "MY", "马来西亚"),
        ("malaysia", "MY", "马来西亚"),
        ("🇹🇭", "TH", "泰国"),
        ("泰国", "TH", "泰国"),
        ("thailand", "TH", "泰国"),
        ("🇻🇳", "VN", "越南"),
        ("越南", "VN", "越南"),
        ("vietnam", "VN", "越南"),
        ("🇵🇭", "PH", "菲律宾"),
        ("菲律宾", "PH", "菲律宾"),
        ("philippines", "PH", "菲律宾"),
        ("🇮🇩", "ID", "印度尼西亚"),
        ("印度尼西亚", "ID", "印度尼西亚"),
        ("indonesia", "ID", "印度尼西亚"),
        ("🇦🇪", "AE", "阿联酋"),
        ("阿联酋", "AE", "阿联酋"),
        ("dubai", "AE", "阿联酋"),
    ];
    for (pattern, code, name) in text_matches {
        if lower.contains(&pattern.to_ascii_lowercase()) {
            return Some((code.to_string(), name.to_string()));
        }
    }
    let tokens = value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|item| !item.is_empty())
        .map(|item| item.to_ascii_uppercase())
        .collect::<HashSet<_>>();
    let codes = [
        ("HK", "HK", "香港"),
        ("TW", "TW", "台湾"),
        ("JP", "JP", "日本"),
        ("SG", "SG", "新加坡"),
        ("US", "US", "美国"),
        ("USA", "US", "美国"),
        ("KR", "KR", "韩国"),
        ("UK", "GB", "英国"),
        ("GB", "GB", "英国"),
        ("DE", "DE", "德国"),
        ("FR", "FR", "法国"),
        ("NL", "NL", "荷兰"),
        ("CA", "CA", "加拿大"),
        ("AU", "AU", "澳大利亚"),
        ("RU", "RU", "俄罗斯"),
        ("IN", "IN", "印度"),
        ("TR", "TR", "土耳其"),
        ("BR", "BR", "巴西"),
        ("MY", "MY", "马来西亚"),
        ("TH", "TH", "泰国"),
        ("VN", "VN", "越南"),
        ("PH", "PH", "菲律宾"),
        ("ID", "ID", "印度尼西亚"),
        ("AE", "AE", "阿联酋"),
    ];
    codes
        .into_iter()
        .find(|(token, _, _)| tokens.contains(*token))
        .map(|(_, code, name)| (code.to_string(), name.to_string()))
}

#[tauri::command]
pub fn analyze_proxy_nodes(
    database: State<'_, Database>,
    runtime: State<'_, ProxyRuntime>,
) -> Result<ProxyIpAnalysis, String> {
    // 国家信息在导入时已写入；分组只读缓存字段，不再做 6000+ DNS/GeoIP 全量分析。
    let state = load_state(&database, &runtime)?;
    let geoip_path = find_geoip_database(&runtime);
    let mut groups_map: HashMap<String, ProxyIpGroup> = HashMap::new();
    let mut analyses = Vec::with_capacity(state.nodes.len());
    let mut unique_ips = HashSet::new();
    let mut resolved_nodes = 0usize;

    for node in &state.nodes {
        let mut country_code = node.country_code.trim().to_string();
        let mut country_name = node.country_name.trim().to_string();
        let mut classification = node.classification.trim().to_string();
        let primary_ip = node.primary_ip.trim().to_string();

        if country_code.is_empty() || country_name.is_empty() {
            let (code, name, class, ip) =
                classify_node_location(&node.name, &node.server, node.port, None);
            country_code = code;
            country_name = name;
            if classification.is_empty() {
                classification = class;
            }
            let _ = ip;
        }
        if classification.is_empty() {
            classification = if primary_ip.is_empty() {
                "unresolved".to_string()
            } else {
                "public".to_string()
            };
        }
        if !primary_ip.is_empty() {
            resolved_nodes += 1;
            unique_ips.insert(primary_ip.clone());
        }

        let key = if country_code.is_empty() {
            "ZZ".to_string()
        } else {
            country_code.clone()
        };
        let entry = groups_map
            .entry(key.clone())
            .or_insert_with(|| ProxyIpGroup {
                key: key.clone(),
                label: if country_name.is_empty() {
                    "未知地区".to_string()
                } else {
                    country_name.clone()
                },
                classification: classification.clone(),
                country_code: if country_code.is_empty() {
                    "ZZ".to_string()
                } else {
                    country_code.clone()
                },
                country_name: if country_name.is_empty() {
                    "未知地区".to_string()
                } else {
                    country_name.clone()
                },
                node_ids: Vec::new(),
                node_count: 0,
            });
        entry.node_ids.push(node.id.clone());
        entry.node_count += 1;

        analyses.push(ProxyIpNodeAnalysis {
            node_id: node.id.clone(),
            node_name: node.name.clone(),
            server: node.server.clone(),
            resolved_ips: if primary_ip.is_empty() {
                Vec::new()
            } else {
                vec![primary_ip.clone()]
            },
            primary_ip,
            classification,
            country_code: entry.country_code.clone(),
            country_name: entry.country_name.clone(),
            error: String::new(),
        });
    }

    let mut groups = groups_map.into_values().collect::<Vec<_>>();
    groups.sort_by(|left, right| {
        let left_rank = match left.country_code.as_str() {
            "ZZ" => 2,
            "LOCAL" => 1,
            _ => 0,
        };
        let right_rank = match right.country_code.as_str() {
            "ZZ" => 2,
            "LOCAL" => 1,
            _ => 0,
        };
        left_rank
            .cmp(&right_rank)
            .then_with(|| right.node_count.cmp(&left.node_count))
            .then_with(|| left.country_name.cmp(&right.country_name))
    });

    // 后台回填缺失国家字段，下次打开无需再推断。
    let missing = state
        .nodes
        .iter()
        .filter(|node| node.country_code.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        for node in missing {
            let (code, name, class, ip) =
                classify_node_location(&node.name, &node.server, node.port, None);
            let _ = connection.execute(
                "UPDATE proxy_pool_nodes
                 SET country_code=?2, country_name=?3, classification=?4,
                     primary_ip=CASE WHEN ?5 != '' THEN ?5 ELSE primary_ip END
                 WHERE id=?1",
                params![node.id, code, name, class, ip],
            );
        }
    }

    let analyzed_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs().to_string())
        .unwrap_or_default();
    Ok(ProxyIpAnalysis {
        analyzed_at,
        geoip_available: geoip_path.is_some(),
        geoip_database_path: geoip_path
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        total_nodes: analyses.len(),
        resolved_nodes,
        unresolved_nodes: analyses.len().saturating_sub(resolved_nodes),
        unique_ips: unique_ips.len(),
        nodes: analyses,
        groups,
    })
}

fn canonical_json(value: &JsonValue, remove_name: bool) -> JsonValue {
    match value {
        JsonValue::Object(map) => {
            let mut keys = map.keys().collect::<Vec<_>>();
            keys.sort();
            let mut result = JsonMap::new();
            for key in keys {
                if remove_name && key == "name" {
                    continue;
                }
                result.insert(key.clone(), canonical_json(&map[key], false));
            }
            JsonValue::Object(result)
        }
        JsonValue::Array(items) => JsonValue::Array(
            items
                .iter()
                .map(|item| canonical_json(item, false))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn node_from_json(mut value: JsonValue) -> Option<ParsedNode> {
    let object = value.as_object_mut()?;
    let name = object.get("name")?.as_str()?.trim().to_string();
    let proxy_type = object.get("type")?.as_str()?.trim().to_ascii_lowercase();
    if name.is_empty() || proxy_type.is_empty() {
        return None;
    }
    let server = object
        .get("server")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_string();
    let port = object
        .get("port")
        .and_then(|item| item.as_i64().or_else(|| item.as_str()?.parse().ok()))
        .unwrap_or_default();
    let cipher = object
        .get("cipher")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_string();
    let udp = object
        .get("udp")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let canonical = canonical_json(&value, true).to_string();
    Some(ParsedNode {
        id: stable_id(&["proxy-node", &canonical]),
        name,
        proxy_type,
        server,
        port,
        cipher,
        udp,
        raw_json: value,
    })
}

fn parse_clash_document(body: &str) -> Option<Vec<ParsedNode>> {
    let yaml: serde_yaml::Value = serde_yaml::from_str(body).ok()?;
    let value = serde_json::to_value(yaml).ok()?;
    let proxies = value.get("proxies")?.as_array()?;
    let nodes = proxies
        .iter()
        .filter_map(|item| node_from_json(item.clone()))
        .collect::<Vec<_>>();
    if nodes.is_empty() {
        None
    } else {
        Some(nodes)
    }
}

fn decode_base64(value: &str) -> Option<Vec<u8>> {
    let compact = value
        .chars()
        .filter(|char| !char.is_whitespace())
        .collect::<String>();
    for engine in [
        &general_purpose::STANDARD,
        &general_purpose::STANDARD_NO_PAD,
        &general_purpose::URL_SAFE,
        &general_purpose::URL_SAFE_NO_PAD,
    ] {
        if let Ok(decoded) = engine.decode(&compact) {
            return Some(decoded);
        }
    }
    None
}

fn decoded_fragment(url: &Url, fallback: &str) -> String {
    url.fragment()
        .and_then(|value| {
            percent_encoding::percent_decode_str(value)
                .decode_utf8()
                .ok()
        })
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn parse_vmess(line: &str) -> Option<ParsedNode> {
    let decoded = decode_base64(line.strip_prefix("vmess://")?)?;
    let source: JsonValue = serde_json::from_slice(&decoded).ok()?;
    let server = source.get("add").and_then(JsonValue::as_str)?.to_string();
    let port = source
        .get("port")
        .and_then(|item| item.as_i64().or_else(|| item.as_str()?.parse().ok()))
        .unwrap_or_default();
    let name = source
        .get("ps")
        .and_then(JsonValue::as_str)
        .filter(|item| !item.trim().is_empty())
        .unwrap_or("VMess")
        .to_string();
    let mut object = json!({
        "name": name,
        "type": "vmess",
        "server": server,
        "port": port,
        "uuid": source.get("id").and_then(JsonValue::as_str).unwrap_or_default(),
        "alterId": source.get("aid").and_then(|item| item.as_i64().or_else(|| item.as_str()?.parse().ok())).unwrap_or(0),
        "cipher": source.get("scy").and_then(JsonValue::as_str).unwrap_or("auto"),
        "udp": true
    });
    if source
        .get("tls")
        .and_then(JsonValue::as_str)
        .is_some_and(|item| !item.is_empty() && item != "none")
    {
        object["tls"] = JsonValue::Bool(true);
        if let Some(sni) = source.get("sni").and_then(JsonValue::as_str) {
            object["servername"] = json!(sni);
        }
    }
    node_from_json(object)
}

fn parse_uri_node(line: &str) -> Option<ParsedNode> {
    if line.starts_with("vmess://") {
        return parse_vmess(line);
    }
    let url = Url::parse(line).ok()?;
    let scheme = url.scheme().to_ascii_lowercase();
    let server = url.host_str()?.to_string();
    let port = url.port()? as i64;
    let fallback = format!("{} {}:{}", scheme.to_ascii_uppercase(), server, port);
    let name = decoded_fragment(&url, &fallback);
    let query = url.query_pairs().collect::<HashMap<_, _>>();
    let object = match scheme.as_str() {
        "trojan" => json!({
            "name": name, "type": "trojan", "server": server, "port": port,
            "password": url.username(), "sni": query.get("sni").or_else(|| query.get("peer")).map(|v| v.as_ref()).unwrap_or(&server),
            "skip-cert-verify": query.get("allowInsecure").is_some_and(|v| v == "1" || v == "true"), "udp": true
        }),
        "vless" => json!({
            "name": name, "type": "vless", "server": server, "port": port,
            "uuid": url.username(), "tls": query.get("security").is_some_and(|v| v == "tls" || v == "reality"),
            "servername": query.get("sni").map(|v| v.as_ref()).unwrap_or(&server), "udp": true
        }),
        "hysteria2" | "hy2" => json!({
            "name": name, "type": "hysteria2", "server": server, "port": port,
            "password": url.username(), "sni": query.get("sni").map(|v| v.as_ref()).unwrap_or(&server), "udp": true
        }),
        "http" | "https" => json!({
            "name": name, "type": "http", "server": server, "port": port,
            "username": url.username(), "password": url.password().unwrap_or_default(), "tls": scheme == "https"
        }),
        "socks" | "socks5" => json!({
            "name": name, "type": "socks5", "server": server, "port": port,
            "username": url.username(), "password": url.password().unwrap_or_default(), "udp": true
        }),
        "ss" => {
            let mut cipher = query
                .get("method")
                .map(|v| v.to_string())
                .unwrap_or_default();
            let mut password = url.password().unwrap_or_default().to_string();
            let username = url.username();
            if let Some(decoded) =
                decode_base64(username).and_then(|bytes| String::from_utf8(bytes).ok())
            {
                if let Some((method, secret)) = decoded.split_once(':') {
                    cipher = method.to_string();
                    password = secret.to_string();
                }
            }
            json!({ "name": name, "type": "ss", "server": server, "port": port, "cipher": cipher, "password": password, "udp": true })
        }
        _ => return None,
    };
    node_from_json(object)
}

fn parse_subscription(body: &str) -> Result<Vec<ParsedNode>, String> {
    if let Some(nodes) = parse_clash_document(body) {
        return Ok(nodes);
    }
    if let Some(decoded) = decode_base64(body).and_then(|bytes| String::from_utf8(bytes).ok()) {
        if let Some(nodes) = parse_clash_document(&decoded) {
            return Ok(nodes);
        }
        let nodes = decoded
            .lines()
            .filter_map(|line| parse_uri_node(line.trim()))
            .collect::<Vec<_>>();
        if !nodes.is_empty() {
            return Ok(nodes);
        }
    }
    let nodes = body
        .lines()
        .filter_map(|line| parse_uri_node(line.trim()))
        .collect::<Vec<_>>();
    if !nodes.is_empty() {
        return Ok(nodes);
    }
    Err("没有找到可识别的代理节点；支持 Clash YAML、Base64 订阅和常见代理节点链接".into())
}

fn validate_source(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("请输入订阅地址或代理节点链接".into());
    }
    if value.lines().count() > 1 {
        if value
            .lines()
            .filter(|line| !line.trim().is_empty())
            .all(|line| parse_uri_node(line.trim()).is_some())
        {
            return Ok(value.to_string());
        }
        return Err("多行导入时，每一行都必须是代理节点链接".into());
    }
    let url = Url::parse(value).map_err(|_| "链接格式无效".to_string())?;
    if matches!(url.scheme(), "http" | "https") || parse_uri_node(value).is_some() {
        Ok(value.to_string())
    } else {
        Err("仅支持 HTTP(S) 订阅地址或代理节点链接".into())
    }
}

fn unique_name(base: &str, used: &mut HashSet<String>) -> String {
    if used.insert(base.to_string()) {
        return base.to_string();
    }
    for index in 2..10000 {
        let candidate = format!("{base} [{index}]");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    format!("{base} [{}]", stable_id(&[base])[..6].to_string())
}

fn find_mihomo_binary() -> Option<PathBuf> {
    if let Ok(value) = std::env::var("OPENHUB_MIHOMO_PATH") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
    }
    for path in [
        "/Applications/Clash Verge.app/Contents/MacOS/verge-mihomo",
        "/Applications/Clash Verge.app/Contents/MacOS/verge-mihomo-alpha",
        "/Applications/Clash Party.app/Contents/Resources/sidecar/mihomo",
        "/usr/local/bin/mihomo",
        "/opt/homebrew/bin/mihomo",
    ] {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|path| path.join("mihomo"))
            .find(|path| path.is_file())
    })
}

fn required_text<'a>(value: &'a JsonValue, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .unwrap_or_default()
}

fn basic_node_config_error(value: &JsonValue) -> Option<String> {
    let proxy_type = required_text(value, "type").to_ascii_lowercase();
    let port = value
        .get("port")
        .and_then(|item| item.as_i64().or_else(|| item.as_str()?.parse().ok()))
        .unwrap_or_default();
    if required_text(value, "name").is_empty()
        || required_text(value, "server").is_empty()
        || !(1..=65535).contains(&port)
    {
        return Some("节点缺少名称、服务器或端口".into());
    }
    let missing = match proxy_type.as_str() {
        "ss" => {
            required_text(value, "cipher").is_empty() || required_text(value, "password").is_empty()
        }
        "vmess" | "vless" => required_text(value, "uuid").is_empty(),
        "trojan" | "anytls" => required_text(value, "password").is_empty(),
        "hysteria2" => {
            required_text(value, "password").is_empty() && required_text(value, "auth").is_empty()
        }
        _ => false,
    };
    missing.then(|| format!("{proxy_type} 节点缺少必要的认证或加密参数"))
}

fn runtime_nodes(database: &Database) -> Result<(Vec<RuntimeNode>, String), String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let active = connection
        .query_row(
            "SELECT value FROM app_meta WHERE key = ?1",
            [ACTIVE_PROXY_NODE_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    let rows = connection
        .prepare("SELECT id, raw_json FROM proxy_pool_nodes ORDER BY CASE WHEN id = ?1 THEN 0 ELSE 1 END, name COLLATE NOCASE")
        .map_err(|error| error.to_string())?
        .query_map([&active], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
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

fn stop_child(state: &mut RuntimeState) {
    if let Some(child) = state.child.as_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }
    state.child = None;
    state.config_hash.clear();
}

fn port_is_available(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

fn choose_runtime_ports(state: &RuntimeState) -> Result<(u16, u16), String> {
    let mut candidates = Vec::new();
    if state.proxy_port > 0 && state.controller_port > 0 {
        candidates.push((state.proxy_port, state.controller_port));
    }
    candidates.push((DEFAULT_RUNTIME_PROXY_PORT, DEFAULT_RUNTIME_CONTROLLER_PORT));
    for offset in 1..=32 {
        candidates.push((
            DEFAULT_RUNTIME_PROXY_PORT.saturating_add(offset * 2),
            DEFAULT_RUNTIME_CONTROLLER_PORT.saturating_add(offset * 2),
        ));
    }

    for (proxy_port, controller_port) in candidates {
        if proxy_port != controller_port
            && port_is_available(proxy_port)
            && port_is_available(controller_port)
        {
            return Ok((proxy_port, controller_port));
        }
    }

    for _ in 0..16 {
        let proxy_listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| format!("无法分配代理端口：{error}"))?;
        let proxy_port = proxy_listener
            .local_addr()
            .map_err(|error| format!("读取代理端口失败：{error}"))?
            .port();
        let controller_listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| format!("无法分配控制器端口：{error}"))?;
        let controller_port = controller_listener
            .local_addr()
            .map_err(|error| format!("读取控制器端口失败：{error}"))?
            .port();
        drop(controller_listener);
        drop(proxy_listener);
        if proxy_port != controller_port {
            return Ok((proxy_port, controller_port));
        }
    }

    Err("无法为 OpenHub 代理内核分配可用端口".into())
}

fn runtime_proxy_url(runtime: &ProxyRuntime) -> String {
    runtime
        .inner
        .lock()
        .ok()
        .filter(|state| state.proxy_port > 0)
        .map(|state| format!("http://127.0.0.1:{}", state.proxy_port))
        .unwrap_or_else(|| PROXY_RUNTIME_URL.to_string())
}

fn runtime_controller_port(runtime: &ProxyRuntime) -> Result<u16, String> {
    runtime
        .inner
        .lock()
        .map_err(|_| "代理内核运行状态锁定失败".to_string())
        .and_then(|state| {
            (state.controller_port > 0)
                .then_some(state.controller_port)
                .ok_or_else(|| "代理内核控制器尚未启动".to_string())
        })
}

fn runtime_config(nodes: &[RuntimeNode], proxy_port: u16, controller_port: u16) -> JsonValue {
    let configs = nodes
        .iter()
        .map(|node| node.config.clone())
        .collect::<Vec<_>>();
    let names = configs
        .iter()
        .filter_map(|node| node.get("name").and_then(JsonValue::as_str))
        .collect::<Vec<_>>();
    json!({
        "mixed-port": proxy_port,
        "external-controller": format!("127.0.0.1:{controller_port}"),
        "secret": RUNTIME_SECRET,
        "allow-lan": false,
        "bind-address": "127.0.0.1",
        "mode": "global",
        "log-level": "warning",
        "ipv6": true,
        "proxies": configs,
        "proxy-groups": [{ "name": RUNTIME_GROUP, "type": "select", "proxies": names }],
        "rules": [format!("MATCH,{RUNTIME_GROUP}")]
    })
}

fn proxy_error_index(output: &str) -> Option<usize> {
    let marker = output.rfind("proxy ")? + "proxy ".len();
    let digits = output[marker..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

fn validate_runtime_nodes(
    engine: &PathBuf,
    runtime: &ProxyRuntime,
    mut nodes: Vec<RuntimeNode>,
    proxy_port: u16,
    controller_port: u16,
) -> Result<(Vec<RuntimeNode>, Vec<(String, String)>), String> {
    let validation_dir = runtime.directory.join("validate");
    let _ = fs::remove_dir_all(&validation_dir);
    fs::create_dir_all(&validation_dir)
        .map_err(|error| format!("无法创建代理配置验证目录：{error}"))?;
    let config_path = validation_dir.join("config.yaml");
    let mut invalid = Vec::new();
    for _ in 0..128 {
        if nodes.is_empty() {
            return Err("所有代理节点配置均无效".into());
        }
        fs::write(
            &config_path,
            serde_yaml::to_string(&runtime_config(&nodes, proxy_port, controller_port))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("无法写入代理验证配置：{error}"))?;
        let output = Command::new(engine)
            .arg("-t")
            .arg("-d")
            .arg(&validation_dir)
            .arg("-f")
            .arg(&config_path)
            .output()
            .map_err(|error| format!("无法验证 Mihomo 配置：{error}"))?;
        if output.status.success() {
            let _ = fs::remove_dir_all(&validation_dir);
            return Ok((nodes, invalid));
        }
        let message = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let Some(index) = proxy_error_index(&message) else {
            return Err(format!(
                "Mihomo 配置验证失败：{}",
                message.lines().last().unwrap_or("未知错误")
            ));
        };
        if index >= nodes.len() {
            return Err(format!("Mihomo 返回了无效的节点序号 {index}"));
        }
        let node = nodes.remove(index);
        let detail = message
            .lines()
            .find(|line| line.contains("Parse config error"))
            .unwrap_or("Mihomo 无法解析此节点")
            .to_string();
        invalid.push((node.id, detail));
    }
    Err("无效代理节点过多，已停止配置验证".into())
}

fn ensure_runtime(database: &Database, runtime: &ProxyRuntime) -> Result<(), String> {
    let (nodes, initial_hash) = runtime_nodes(database)?;
    if nodes.is_empty() {
        return Err("代理池中没有配置有效的节点".into());
    }
    let engine =
        find_mihomo_binary().ok_or("未找到 Mihomo 内核，请先安装 Clash Verge 或 Clash Party")?;
    let mut state = runtime
        .inner
        .lock()
        .map_err(|_| "代理内核运行状态锁定失败")?;
    let running = if let Some(child) = state.child.as_mut() {
        child
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_none()
    } else {
        false
    };
    if running && state.config_hash == initial_hash {
        return Ok(());
    }

    stop_child(&mut state);
    let (proxy_port, controller_port) = choose_runtime_ports(&state)?;
    let (nodes, invalid) =
        validate_runtime_nodes(&engine, runtime, nodes, proxy_port, controller_port)?;
    if !invalid.is_empty() {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        let active = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key=?1",
                [ACTIVE_PROXY_NODE_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .unwrap_or_default();
        for (id, _error) in &invalid {
            connection.execute(
                "UPDATE proxy_pool_nodes SET latency_ms=NULL, test_status='invalid', tested_at=CURRENT_TIMESTAMP WHERE id=?1",
                [id],
            ).map_err(|error| error.to_string())?;
            if id == &active {
                write_meta(&connection, ACTIVE_PROXY_NODE_KEY, "")?;
                write_meta(&connection, NETWORK_PROXY_KEY, "")?;
            }
        }
    }
    let hash = stable_id(&[&serde_json::to_string(
        &nodes.iter().map(|node| &node.config).collect::<Vec<_>>(),
    )
    .unwrap_or_default()]);
    fs::create_dir_all(&runtime.directory)
        .map_err(|error| format!("无法创建代理运行目录：{error}"))?;
    let config_path = runtime.directory.join("config.yaml");
    fs::write(
        &config_path,
        serde_yaml::to_string(&runtime_config(&nodes, proxy_port, controller_port))
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("无法写入代理配置：{error}"))?;
    let log_path = runtime.directory.join("runtime.log");
    let log_file =
        fs::File::create(&log_path).map_err(|error| format!("无法创建代理内核日志：{error}"))?;
    let error_log = log_file
        .try_clone()
        .map_err(|error| format!("无法初始化代理内核日志：{error}"))?;
    let child = Command::new(&engine)
        .arg("-d")
        .arg(&runtime.directory)
        .arg("-f")
        .arg(&config_path)
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(error_log))
        .spawn()
        .map_err(|error| format!("无法启动 Mihomo：{error}"))?;
    state.child = Some(child);
    state.engine_path = engine.display().to_string();
    state.config_hash = hash;
    state.proxy_port = proxy_port;
    state.controller_port = controller_port;
    state.last_error.clear();
    let address = SocketAddr::from(([127, 0, 0, 1], controller_port));
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(6) {
        if TcpStream::connect_timeout(&address, Duration::from_millis(150)).is_ok() {
            return Ok(());
        }
        let startup_log =
            fs::read_to_string(runtime.directory.join("runtime.log")).unwrap_or_default();
        if startup_log.contains("listen error")
            || startup_log.contains("server error")
            || startup_log.contains("Parse config error")
        {
            let detail = startup_log
                .lines()
                .rev()
                .take(2)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("；");
            let message = format!("Mihomo 启动失败：{detail}");
            state.last_error = message.clone();
            stop_child(&mut state);
            return Err(message);
        }
        if started.elapsed() >= Duration::from_millis(800)
            && startup_log.contains("Initial configuration complete")
        {
            return Ok(());
        }
        if let Some(child) = state.child.as_mut() {
            if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                let message = format!("Mihomo 启动失败：{status}");
                state.last_error = message.clone();
                state.child = None;
                return Err(message);
            }
        }
        std::thread::sleep(Duration::from_millis(120));
    }
    let log = fs::read_to_string(runtime.directory.join("runtime.log")).unwrap_or_default();
    let detail = log
        .lines()
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("；");
    let message = if detail.is_empty() {
        format!("Mihomo 启动超时，请检查本地端口 {proxy_port}/{controller_port} 是否被占用")
    } else {
        format!("Mihomo 启动超时：{detail}")
    };
    state.last_error = message.clone();
    stop_child(&mut state);
    Err(message)
}

fn controller_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .pool_max_idle_per_host(BATCH_PROXY_TEST_CONCURRENCY)
        .pool_idle_timeout(Duration::from_secs(30))
        .no_proxy()
        .build()
        .map_err(|error| error.to_string())
}

async fn test_controller_proxy_delay(
    client: reqwest::Client,
    controller_port: u16,
    name: String,
    target: String,
) -> Option<i64> {
    let mut endpoint = Url::parse(&controller_url(controller_port, "/proxies/")).ok()?;
    append_controller_path(&mut endpoint, &[&name, "delay"]).ok()?;
    endpoint
        .query_pairs_mut()
        .append_pair("timeout", BATCH_PROXY_TEST_TIMEOUT_MS)
        .append_pair("url", &target);
    let response = client
        .get(endpoint)
        .bearer_auth(RUNTIME_SECRET)
        .timeout(Duration::from_millis(3200))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    response
        .json::<JsonValue>()
        .await
        .ok()?
        .get("delay")
        .and_then(JsonValue::as_i64)
        .filter(|delay| *delay > 0)
}

fn controller_url(port: u16, path: &str) -> String {
    format!("http://127.0.0.1:{port}{path}")
}

fn append_controller_path(url: &mut Url, segments: &[&str]) -> Result<(), String> {
    url.path_segments_mut()
        .map_err(|_| "控制器地址无效".to_string())?
        .pop_if_empty()
        .extend(segments);
    Ok(())
}

async fn select_runtime_node(runtime: &ProxyRuntime, name: &str) -> Result<(), String> {
    let port = runtime_controller_port(runtime)?;
    let mut url =
        Url::parse(&controller_url(port, "/proxies/")).map_err(|error| error.to_string())?;
    append_controller_path(&mut url, &[RUNTIME_GROUP])?;
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

pub(crate) fn restore_saved_proxy(database: &Database, runtime: &ProxyRuntime) {
    let active = read_meta(database, ACTIVE_PROXY_NODE_KEY).unwrap_or_default();
    if active.is_empty() {
        if let Ok(connection) = database.0.lock() {
            let _ = write_meta(&connection, NETWORK_PROXY_KEY, "");
        }
        return;
    }
    if let Err(error) = ensure_runtime(database, runtime) {
        if let Ok(connection) = database.0.lock() {
            let _ = write_meta(&connection, ACTIVE_PROXY_NODE_KEY, "");
            let _ = write_meta(&connection, NETWORK_PROXY_KEY, "");
        }
        if let Ok(mut state) = runtime.inner.lock() {
            state.last_error = error;
        }
    } else if let Ok(connection) = database.0.lock() {
        let proxy_url = runtime_proxy_url(runtime);
        let _ = write_meta(&connection, NETWORK_PROXY_KEY, &proxy_url);
    }
}

#[tauri::command]
pub fn get_proxy_pool_state(
    database: State<'_, Database>,
    runtime: State<'_, ProxyRuntime>,
) -> Result<ProxyPoolState, String> {
    load_state(&database, &runtime)
}

#[tauri::command]
pub fn save_proxy_subscription(
    database: State<'_, Database>,
    id: Option<String>,
    name: String,
    url: String,
) -> Result<ProxySubscription, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("请输入名称".into());
    }
    let source = validate_source(&url)?;
    let id = id
        .filter(|item| !item.trim().is_empty())
        .unwrap_or_else(|| stable_id(&["proxy-source", &source]));
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    connection.execute(
        "INSERT INTO proxy_subscriptions (id, name, url, created_at, updated_at) VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, url = excluded.url, updated_at = CURRENT_TIMESTAMP",
        params![id, name, source],
    ).map_err(|error| error.to_string())?;
    connection.query_row("SELECT id, name, url, node_count, last_error, created_at, updated_at FROM proxy_subscriptions WHERE id = ?1", [&id], row_subscription).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn delete_proxy_subscription(
    database: State<'_, Database>,
    runtime: State<'_, ProxyRuntime>,
    id: String,
) -> Result<ProxyPoolState, String> {
    let mut connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    // 显式清理关联记录，兼容早期数据库未启用外键级联的情况。
    transaction
        .execute(
            "DELETE FROM proxy_subscription_nodes WHERE subscription_id = ?1",
            [&id],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute("DELETE FROM proxy_nodes WHERE subscription_id = ?1", [&id])
        .map_err(|error| error.to_string())?;
    transaction
        .execute("DELETE FROM proxy_subscriptions WHERE id = ?1", [&id])
        .map_err(|error| error.to_string())?;
    transaction.execute("DELETE FROM proxy_pool_nodes WHERE id NOT IN (SELECT node_id FROM proxy_subscription_nodes)", []).map_err(|error| error.to_string())?;
    let active = transaction
        .query_row(
            "SELECT value FROM app_meta WHERE key = ?1",
            [ACTIVE_PROXY_NODE_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    if !active.is_empty() {
        let exists = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM proxy_pool_nodes WHERE id = ?1)",
                [&active],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            != 0;
        if !exists {
            write_meta(&transaction, ACTIVE_PROXY_NODE_KEY, "")?;
            write_meta(&transaction, NETWORK_PROXY_KEY, "")?;
        }
    }
    transaction.commit().map_err(|error| error.to_string())?;
    drop(connection);
    if let Ok(mut state) = runtime.inner.lock() {
        state.config_hash.clear();
    }
    load_state(&database, &runtime)
}

#[tauri::command]
pub async fn refresh_proxy_subscription(
    app: AppHandle,
    database: State<'_, Database>,
    runtime: State<'_, ProxyRuntime>,
    id: String,
) -> Result<ProxyPoolRefreshResult, String> {
    let emit_progress = |stage: &str, status: &str, message: String, completed: usize, total: usize, added: usize, discarded: usize| {
        let _ = app.emit(
            "proxy-source-progress",
            ProxySourceProgress {
                source_id: id.clone(),
                stage: stage.to_string(),
                status: status.to_string(),
                message,
                completed,
                total,
                added,
                discarded,
            },
        );
    };

    emit_progress("queued", "running", "来源已加入解析队列".into(), 0, 0, 0, 0);

    let source = {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        connection
            .query_row(
                "SELECT url FROM proxy_subscriptions WHERE id = ?1",
                [&id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or("导入源不存在")?
    };

    emit_progress(
        "fetching",
        "running",
        if source.lines().count() == 1
            && matches!(
                Url::parse(&source)
                    .ok()
                    .map(|url| url.scheme().to_string())
                    .as_deref(),
                Some("http") | Some("https")
            ) {
            "正在下载订阅内容…".into()
        } else {
            "正在读取本地节点链接…".into()
        },
        0,
        0,
        0,
        0,
    );

    let parsed: Result<Vec<ParsedNode>, String> = async {
        if source.lines().count() == 1
            && matches!(
                Url::parse(&source)
                    .ok()
                    .map(|url| url.scheme().to_string())
                    .as_deref(),
                Some("http") | Some("https")
            )
        {
            let client = build_http_client(&database, Duration::from_secs(30), 5, "代理订阅请求")?;
            let response = client
                .get(&source)
                .header("User-Agent", "OpenHub/0.3 ProxyPool")
                .send()
                .await
                .map_err(|error| format!("获取订阅失败：{error}"))?;
            if !response.status().is_success() {
                return Err(format!(
                    "订阅服务器返回 HTTP {}",
                    response.status().as_u16()
                ));
            }
            let body = response
                .text()
                .await
                .map_err(|error| format!("读取订阅失败：{error}"))?;
            emit_progress(
                "parsing",
                "running",
                format!("订阅已下载（{} 字节），正在解析节点…", body.len()),
                0,
                0,
                0,
                0,
            );
            parse_subscription(&body)
        } else {
            emit_progress(
                "parsing",
                "running",
                "正在解析节点链接…".into(),
                0,
                0,
                0,
                0,
            );
            parse_subscription(&source)
        }
    }
    .await;

    let nodes = match parsed {
        Ok(nodes) => nodes,
        Err(error) => {
            let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
            connection
                .execute(
                    "UPDATE proxy_subscriptions SET last_error = ?2, updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                    params![id, error],
                )
                .map_err(|db_error| db_error.to_string())?;
            emit_progress("error", "error", error.clone(), 0, 0, 0, 0);
            return Err(error);
        }
    };

    let raw_total = nodes.len();
    emit_progress(
        "parsing",
        "running",
        format!("已解析 {raw_total} 个原始节点，正在过滤非法配置…"),
        0,
        raw_total,
        0,
        0,
    );

    let mut discarded = 0usize;
    let nodes = nodes
        .into_iter()
        .filter_map(|node| {
            if let Some(error) = basic_node_config_error(&node.raw_json) {
                discarded += 1;
                eprintln!("OpenHub 刷新来源时过滤非法节点：{}：{}", node.name, error);
                None
            } else {
                Some(node)
            }
        })
        .collect::<Vec<_>>();

    let valid_total = nodes.len();
    emit_progress(
        "saving",
        "running",
        format!("有效节点 {valid_total} 个，开始写入并识别国家…"),
        0,
        valid_total,
        0,
        discarded,
    );

    let mut connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "DELETE FROM proxy_subscription_nodes WHERE subscription_id = ?1",
            [&id],
        )
        .map_err(|error| error.to_string())?;
    let mut used_names = transaction
        .prepare("SELECT name FROM proxy_pool_nodes")
        .map_err(|error| error.to_string())?
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?
        .collect::<Result<HashSet<_>, _>>()
        .map_err(|error| error.to_string())?;
    let mut unique_ids = HashSet::new();
    let mut saved = 0usize;
    let mut added = 0usize;
    let progress_step = (valid_total / 40).max(1);

    for mut node in nodes {
        if !unique_ids.insert(node.id.clone()) {
            continue;
        }
        let existing_name = transaction
            .query_row(
                "SELECT name FROM proxy_pool_nodes WHERE id = ?1",
                [&node.id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        let is_new = existing_name.is_none();
        node.name = existing_name.unwrap_or_else(|| unique_name(&node.name, &mut used_names));
        if let Some(object) = node.raw_json.as_object_mut() {
            object.insert("name".into(), json!(node.name));
        }
        let (country_code, country_name, classification, primary_ip) =
            classify_node_location(&node.name, &node.server, node.port, None);
        transaction
            .execute(
                "INSERT INTO proxy_pool_nodes (
                id, name, proxy_type, server, port, cipher, udp, raw_json,
                country_code, country_name, classification, primary_ip, updated_at
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, CURRENT_TIMESTAMP)
             ON CONFLICT(id) DO UPDATE SET
                proxy_type=excluded.proxy_type,
                server=excluded.server,
                port=excluded.port,
                cipher=excluded.cipher,
                udp=excluded.udp,
                raw_json=excluded.raw_json,
                country_code=excluded.country_code,
                country_name=excluded.country_name,
                classification=excluded.classification,
                primary_ip=CASE
                    WHEN excluded.primary_ip != '' THEN excluded.primary_ip
                    ELSE proxy_pool_nodes.primary_ip
                END,
                updated_at=CURRENT_TIMESTAMP",
                params![
                    node.id,
                    node.name,
                    node.proxy_type,
                    node.server,
                    node.port,
                    node.cipher,
                    node.udp as i64,
                    node.raw_json.to_string(),
                    country_code,
                    country_name,
                    classification,
                    primary_ip
                ],
            )
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO proxy_subscription_nodes (subscription_id, node_id) VALUES (?1, ?2)",
                params![id, node.id],
            )
            .map_err(|error| error.to_string())?;
        saved += 1;
        if is_new {
            added += 1;
        }
        if saved == valid_total || saved % progress_step == 0 {
            emit_progress(
                "saving",
                "running",
                format!("正在写入节点 {saved}/{valid_total}…"),
                saved,
                valid_total,
                added,
                discarded,
            );
        }
    }

    // 已被运行时或测速标记为非法的节点不应在刷新后继续保留。
    discarded += transaction
        .execute(
            "DELETE FROM proxy_pool_nodes WHERE test_status = 'invalid'",
            [],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "DELETE FROM proxy_pool_nodes WHERE id NOT IN (SELECT node_id FROM proxy_subscription_nodes)",
            [],
        )
        .map_err(|error| error.to_string())?;
    let total = transaction
        .query_row(
            "SELECT COUNT(*) FROM proxy_subscription_nodes WHERE subscription_id = ?1",
            [&id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| error.to_string())? as usize;
    transaction
        .execute(
            "UPDATE proxy_subscriptions SET node_count=?2, last_error='', updated_at=CURRENT_TIMESTAMP WHERE id=?1",
            params![id, total as i64],
        )
        .map_err(|error| error.to_string())?;
    let active = transaction
        .query_row(
            "SELECT value FROM app_meta WHERE key=?1",
            [ACTIVE_PROXY_NODE_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    if !active.is_empty() {
        let exists = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM proxy_pool_nodes WHERE id=?1)",
                [&active],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            != 0;
        if !exists {
            write_meta(&transaction, ACTIVE_PROXY_NODE_KEY, "")?;
            write_meta(&transaction, NETWORK_PROXY_KEY, "")?;
        }
    }
    transaction
        .commit()
        .map_err(|error| error.to_string())?;
    drop(connection);

    // 配置变更后异步重启运行时，不阻塞导入完成反馈。
    let _ = ensure_runtime(&database, &runtime);

    let subscription = {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        connection
            .query_row(
                "SELECT id, name, url, node_count, last_error, created_at, updated_at FROM proxy_subscriptions WHERE id = ?1",
                [&id],
                row_subscription,
            )
            .map_err(|error| error.to_string())?
    };

    emit_progress(
        "done",
        "success",
        format!("解析完成：{total} 个节点，新增 {added}，过滤 {discarded}"),
        total,
        total,
        added,
        discarded,
    );

    Ok(ProxyPoolRefreshResult {
        subscription,
        added,
        total,
        discarded,
    })
}