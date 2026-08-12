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
// 对齐 Clash Verge Rev（src/services/delay.ts::checkListDelay）：
// - 默认 timeout 10000
// - 实际并发 min(请求并发, 节点数, 10)
// - 固定测速 URL，对已装载节点并行 /proxies/{name}/delay
const BATCH_PROXY_TEST_TIMEOUT_MS: &str = "10000";
const BATCH_PROXY_TEST_CONCURRENCY: usize = 10;
// 选中来源通常远小于此值；全量测速时按块装载，避免一次灌入 6000+
const BATCH_PROXY_TEST_NODE_CHUNK: usize = 120;

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
    // 全局代理内核“重启/选节点”的串行锁：用户切换与公益监听等后台任务
    // 互斥操作同一 Mihomo，避免互相杀进程/覆盖选择导致切换卡死。
    runtime_op_lock: tokio::sync::Mutex<()>,
}

struct ProxyTestLease<'a> {
    runtime: &'a ProxyRuntime,
    id: u64,
    cancellation: CancellationToken,
}

struct TemporaryRuntimeDirectory(PathBuf);

impl Drop for TemporaryRuntimeDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl ProxyRuntime {
    pub(crate) fn new(directory: PathBuf) -> Self {
        Self::new_with_ports(directory, 0, 0)
    }

    fn new_with_ports(directory: PathBuf, proxy_port: u16, controller_port: u16) -> Self {
        Self {
            directory,
            inner: Mutex::new(RuntimeState {
                child: None,
                config_hash: String::new(),
                engine_path: String::new(),
                last_error: String::new(),
                proxy_port,
                controller_port,
            }),
            active_test: Mutex::new(None),
            next_test_id: AtomicU64::new(1),
            runtime_op_lock: tokio::sync::Mutex::new(()),
        }
    }

    fn start_proxy_test(&self) -> Result<ProxyTestLease<'_>, String> {
        let mut active = self
            .active_test
            .lock()
            .map_err(|_| "测速任务状态锁定失败")?;
        if active.is_some() {
            return Err("已有代理测速任务正在进行，请等待上一任务结束".into());
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
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(test) = active.as_ref() else {
            return Ok(false);
        };
        // 批量测速使用独立 Mihomo 运行时；这里只发取消信号，绝不停止用户全局代理。
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
    let network = source
        .get("net")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .unwrap_or("tcp")
        .to_ascii_lowercase();
    let host = source
        .get("host")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .unwrap_or_default()
        .to_string();
    let path = source
        .get("path")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .unwrap_or("/")
        .to_string();
    let mut object = json!({
        "name": name,
        "type": "vmess",
        "server": server,
        "port": port,
        "uuid": source.get("id").and_then(JsonValue::as_str).unwrap_or_default(),
        "alterId": source.get("aid").and_then(|item| item.as_i64().or_else(|| item.as_str()?.parse().ok())).unwrap_or(0),
        "cipher": source.get("scy").and_then(JsonValue::as_str).filter(|item| !item.trim().is_empty()).unwrap_or("auto"),
        "network": network,
        "udp": true
    });
    match network.as_str() {
        "ws" => {
            let mut ws_opts = json!({ "path": path });
            if !host.is_empty() {
                ws_opts["headers"] = json!({ "Host": host });
            }
            object["ws-opts"] = ws_opts;
        }
        "h2" | "http" => {
            let mut opts = json!({ "path": [path] });
            if !host.is_empty() {
                opts["host"] = json!([host]);
            }
            object["h2-opts"] = opts;
        }
        "grpc" => {
            object["grpc-opts"] = json!({
                "grpc-service-name": path.trim_start_matches('/'),
            });
        }
        _ => {}
    }
    let tls = source
        .get("tls")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if !tls.is_empty() && !tls.eq_ignore_ascii_case("none") {
        object["tls"] = JsonValue::Bool(true);
        let sni = source
            .get("sni")
            .and_then(JsonValue::as_str)
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .unwrap_or(host.as_str());
        if !sni.is_empty() {
            object["servername"] = json!(sni);
        }
        if let Some(alpn) = source.get("alpn").and_then(JsonValue::as_str) {
            let values = alpn
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>();
            if !values.is_empty() {
                object["alpn"] = json!(values);
            }
        }
    }
    node_from_json(object)
}

fn split_userinfo_host_port(value: &str) -> Option<(String, String, String, i64)> {
    // 支持 user:pass@host:port / user@host:port
    let (userinfo, hostport) = value.rsplit_once('@')?;
    let (server, port_text) = hostport.rsplit_once(':')?;
    let port = port_text.parse::<i64>().ok()?;
    if !(1..=65535).contains(&port) || server.trim().is_empty() {
        return None;
    }
    let (username, password) = match userinfo.split_once(':') {
        Some((user, pass)) => (user.to_string(), pass.to_string()),
        None => (userinfo.to_string(), String::new()),
    };
    Some((username, password, server.trim().to_string(), port))
}

fn parse_encoded_userinfo_host_port(encoded: &str) -> Option<(String, String, String, i64)> {
    let decoded = decode_base64(encoded).and_then(|bytes| String::from_utf8(bytes).ok())?;
    split_userinfo_host_port(decoded.trim())
}

fn parse_ssocks(line: &str) -> Option<ParsedNode> {
    // iGG 等订阅常见：ssocks://base64(user:pass@host:port)?remarks=名称&method=auto
    let rest = line.strip_prefix("ssocks://")?;
    let (payload, query) = rest
        .split_once('?')
        .map(|(left, right)| (left, right))
        .unwrap_or((rest, ""));
    let (username, password, server, port) = parse_encoded_userinfo_host_port(payload)?;
    let params = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect::<HashMap<_, _>>();
    let name = params
        .get("remarks")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Socks5")
        .to_string();
    node_from_json(json!({
        "name": name,
        "type": "socks5",
        "server": server,
        "port": port,
        "username": username,
        "password": password,
        "udp": true
    }))
}

fn parse_https_or_http_proxy_uri(line: &str) -> Option<ParsedNode> {
    // 兼容两类：
    // 1) 标准 http(s)://user:pass@host:port#name
    // 2) iGG 风格 https://base64(user:pass@host:port)#name
    let lower = line.to_ascii_lowercase();
    let is_https = lower.starts_with("https://");
    let is_http = lower.starts_with("http://");
    if !is_https && !is_http {
        return None;
    }

    // 优先尝试 base64 主体（无标准 host/port 时）
    if let Some(rest) = line
        .strip_prefix("https://")
        .or_else(|| line.strip_prefix("http://"))
        .or_else(|| line.strip_prefix("HTTPS://"))
        .or_else(|| line.strip_prefix("HTTP://"))
    {
        let (payload, fragment) = rest
            .split_once('#')
            .map(|(left, right)| (left, Some(right)))
            .unwrap_or((rest, None));
        // 没有 @ 且没有明显 host:port，基本就是 base64 包一层
        if !payload.contains('@') {
            if let Some((username, password, server, port)) =
                parse_encoded_userinfo_host_port(payload)
            {
                let name = fragment
                    .and_then(|value| {
                        percent_encoding::percent_decode_str(value)
                            .decode_utf8()
                            .ok()
                    })
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| {
                        format!(
                            "{} {server}:{port}",
                            if is_https { "HTTPS" } else { "HTTP" }
                        )
                    });
                return node_from_json(json!({
                    "name": name,
                    "type": "http",
                    "server": server,
                    "port": port,
                    "username": username,
                    "password": password,
                    "tls": is_https
                }));
            }
        }
    }

    let url = Url::parse(line).ok()?;
    let server = url.host_str()?.to_string();
    let port = url
        .port()
        .or_else(|| if is_https { Some(443) } else { Some(80) })? as i64;
    let fallback = format!(
        "{} {server}:{port}",
        if is_https { "HTTPS" } else { "HTTP" }
    );
    let name = decoded_fragment(&url, &fallback);
    node_from_json(json!({
        "name": name,
        "type": "http",
        "server": server,
        "port": port,
        "username": url.username(),
        "password": url.password().unwrap_or_default(),
        "tls": is_https
    }))
}

fn parse_uri_node(line: &str) -> Option<ParsedNode> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    if line.starts_with("vmess://") {
        return parse_vmess(line);
    }
    if line.starts_with("ssocks://") {
        return parse_ssocks(line);
    }
    if line.starts_with("http://")
        || line.starts_with("https://")
        || line.starts_with("HTTP://")
        || line.starts_with("HTTPS://")
    {
        return parse_https_or_http_proxy_uri(line);
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
        "anytls" => json!({
            "name": name, "type": "anytls", "server": server, "port": port,
            "password": url.username(),
            "sni": query.get("sni").map(|v| v.as_ref()).unwrap_or(&server),
            "udp": true
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
        "tuic" => {
            // tuic://uuid:password@host:port?alpn=h3&congestion_control=bbr#name
            let password = url.password().unwrap_or_default().to_string();
            let uuid = url.username().to_string();
            json!({
                "name": name,
                "type": "tuic",
                "server": server,
                "port": port,
                "uuid": uuid,
                "password": password,
                "alpn": query.get("alpn").map(|v| v.as_ref()).unwrap_or("h3"),
                "congestion-controller": query
                    .get("congestion_control")
                    .or_else(|| query.get("congestion-controller"))
                    .map(|v| v.as_ref())
                    .unwrap_or("bbr"),
                "udp": true
            })
        }
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
        "tuic" => {
            required_text(value, "uuid").is_empty() || required_text(value, "password").is_empty()
        }
        "hysteria2" => {
            required_text(value, "password").is_empty() && required_text(value, "auth").is_empty()
        }
        "http" | "socks5" => false,
        _ => false,
    };
    missing.then(|| format!("{proxy_type} 节点缺少必要的认证或加密参数"))
}

fn runtime_nodes(
    database: &Database,
    only_ids: Option<&HashSet<String>>,
) -> Result<(Vec<RuntimeNode>, String), String> {
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
            // Mihomo 控制器接口按 name 定位节点。
            // 用稳定 id 作为运行时 name，避免中文/空格/| 等展示名导致 delay 接口失败。
            let mut config = config;
            if let Some(object) = config.as_object_mut() {
                object.insert("name".into(), JsonValue::String(id.clone()));
                // 测速时禁止节点再套一层代理，否则变成双重代理，结果会大面积失败/畸高。
                for key in ["dialer-proxy", "proxy", "interface-name", "routing-mark"] {
                    object.remove(key);
                }
                // 很多订阅节点依赖跳过证书校验；缺省时补上，避免 delay 全失败。
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

fn stop_child(state: &mut RuntimeState) {
    if let Some(child) = state.child.as_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }
    state.child = None;
    state.config_hash.clear();
}

fn runtime_process_exists(marker: &str) -> bool {
    Command::new("pgrep")
        .args(["-f", marker])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn kill_stale_runtime_processes(runtime: &ProxyRuntime) {
    // 崩溃/热重载可能留下 Mihomo 占用控制端口。先 TERM，超时后 KILL，并确认端口进程退出。
    let marker = runtime.directory.display().to_string();
    if marker.trim().is_empty() {
        return;
    }
    let _ = Command::new("pkill")
        .args(["-TERM", "-f", &marker])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(800) && runtime_process_exists(&marker) {
        std::thread::sleep(Duration::from_millis(40));
    }
    if runtime_process_exists(&marker) {
        let _ = Command::new("pkill")
            .args(["-KILL", "-f", &marker])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(500) && runtime_process_exists(&marker) {
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn port_is_available(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

fn choose_runtime_ports(state: &RuntimeState) -> Result<(u16, u16), String> {
    let mut candidates = Vec::new();
    if state.proxy_port > 0 && state.controller_port > 0 {
        candidates.push((state.proxy_port, state.controller_port));
    }
    let base_proxy = if state.proxy_port > 0 {
        state.proxy_port
    } else {
        DEFAULT_RUNTIME_PROXY_PORT
    };
    let base_controller = if state.controller_port > 0 {
        state.controller_port
    } else {
        DEFAULT_RUNTIME_CONTROLLER_PORT
    };
    candidates.push((base_proxy, base_controller));
    for offset in 1..=32 {
        candidates.push((
            base_proxy.saturating_add(offset * 2),
            base_controller.saturating_add(offset * 2),
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
    // 测速内核必须“单层代理”：
    // - 节点出站直连远端，不走系统代理 / 不套 dialer-proxy
    // - 控制器请求也 no_proxy
    // - 关闭 IPv6，避免先走坏掉的 v6 导致 delay 全超时
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
        "dns": {
            "enable": true,
            "ipv6": false,
            "use-system-hosts": true,
            "enhanced-mode": "redir-host",
            "default-nameserver": ["8.8.8.8", "1.1.1.1"],
            "nameserver": ["8.8.8.8", "1.1.1.1", "system"]
        },
        "proxies": configs,
        "proxy-groups": [{ "name": RUNTIME_GROUP, "type": "select", "proxies": names }],
        // delay API 本身按节点直测；规则主要用于 mixed-port 出站，避免环回套娃。
        "rules": [
            "IP-CIDR,127.0.0.0/8,DIRECT,no-resolve",
            "IP-CIDR,10.0.0.0/8,DIRECT,no-resolve",
            "IP-CIDR,172.16.0.0/12,DIRECT,no-resolve",
            "IP-CIDR,192.168.0.0/16,DIRECT,no-resolve",
            format!("MATCH,{RUNTIME_GROUP}")
        ]
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

fn run_mihomo_test_config(
    engine: &PathBuf,
    validation_dir: &PathBuf,
    config_path: &PathBuf,
    cancelled: Option<&CancellationToken>,
) -> Result<std::process::Output, String> {
    use std::io::Read;
    let mut child = Command::new(engine)
        .arg("-t")
        .arg("-d")
        .arg(validation_dir)
        .arg("-f")
        .arg(config_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
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
        .map_err(|error| format!("无法验证 Mihomo 配置：{error}"))?;
    let started = Instant::now();
    loop {
        if cancelled.is_some_and(|token| token.is_cancelled()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("测速已取消".into());
        }
        match child.try_wait().map_err(|error| error.to_string())? {
            Some(status) => {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_end(&mut stdout);
                }
                if let Some(mut err) = child.stderr.take() {
                    let _ = err.read_to_end(&mut stderr);
                }
                return Ok(std::process::Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            None => {
                if started.elapsed() > Duration::from_secs(12) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("Mihomo 配置验证超时".into());
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        }
    }
}

fn validate_runtime_nodes(
    engine: &PathBuf,
    runtime: &ProxyRuntime,
    mut nodes: Vec<RuntimeNode>,
    proxy_port: u16,
    controller_port: u16,
    cancelled: Option<&CancellationToken>,
) -> Result<(Vec<RuntimeNode>, Vec<(String, String)>), String> {
    let validation_dir = runtime.directory.join("validate");
    let _ = fs::remove_dir_all(&validation_dir);
    fs::create_dir_all(&validation_dir)
        .map_err(|error| format!("无法创建代理配置验证目录：{error}"))?;
    let config_path = validation_dir.join("config.yaml");
    let mut invalid = Vec::new();
    for _ in 0..128 {
        if cancelled.is_some_and(|token| token.is_cancelled()) {
            return Err("测速已取消".into());
        }
        if nodes.is_empty() {
            return Err("所有代理节点配置均无效".into());
        }
        fs::write(
            &config_path,
            serde_yaml::to_string(&runtime_config(&nodes, proxy_port, controller_port))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("无法写入代理验证配置：{error}"))?;
        let output = run_mihomo_test_config(engine, &validation_dir, &config_path, cancelled)?;
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

fn ensure_runtime(
    database: &Database,
    runtime: &ProxyRuntime,
    only_ids: Option<&HashSet<String>>,
    cancelled: Option<&CancellationToken>,
) -> Result<(), String> {
    let (nodes, initial_hash) = runtime_nodes(database, only_ids)?;
    if nodes.is_empty() {
        return Err("代理池中没有配置有效的节点".into());
    }
    let engine =
        find_mihomo_binary().ok_or("未找到 Mihomo 内核，请先安装 Clash Verge 或 Clash Party")?;

    // 复用已运行实例时只短暂持锁，随后释放再 wait。
    {
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
            let port = state.controller_port;
            drop(state);
            return wait_runtime_ready(port, nodes.len(), cancelled);
        }
        stop_child(&mut state);
    }
    kill_stale_runtime_processes(runtime);
    if cancelled.is_some_and(|token| token.is_cancelled()) {
        return Err("测速已取消".into());
    }

    let (proxy_port, controller_port) = {
        let state = runtime
            .inner
            .lock()
            .map_err(|_| "代理内核运行状态锁定失败")?;
        choose_runtime_ports(&state)?
    };

    // 大批量测速时跳过 mihomo -t 全量校验（极慢且会卡死取消）；
    // 仅依赖基础字段过滤 + 启动失败日志剔除。
    let (nodes, invalid) = if nodes.len() > 80 {
        (nodes, Vec::new())
    } else {
        validate_runtime_nodes(
            &engine,
            runtime,
            nodes,
            proxy_port,
            controller_port,
            cancelled,
        )?
    };
    if cancelled.is_some_and(|token| token.is_cancelled()) {
        return Err("测速已取消".into());
    }
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
            connection
                .execute(
                    "UPDATE proxy_pool_nodes SET latency_ms=NULL, test_status='invalid', tested_at=CURRENT_TIMESTAMP WHERE id=?1",
                    [id],
                )
                .map_err(|error| error.to_string())?;
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

    // 启动进程只短暂持锁；等待就绪必须在锁外，否则 cancel 永远抢不到锁。
    {
        let mut state = runtime
            .inner
            .lock()
            .map_err(|_| "代理内核运行状态锁定失败")?;
        if cancelled.is_some_and(|token| token.is_cancelled()) {
            stop_child(&mut state);
            return Err("测速已取消".into());
        }
        // 若取消线程已清进程，确保干净后再 spawn。
        stop_child(&mut state);
        // 清除继承到的 HTTP(S)_PROXY，防止内核出站先被系统/终端代理再套一层。
        let child = Command::new(&engine)
            .arg("-d")
            .arg(&runtime.directory)
            .arg("-f")
            .arg(&config_path)
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(error_log))
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
            .map_err(|error| format!("无法启动 Mihomo：{error}"))?;
        state.child = Some(child);
        state.engine_path = engine.display().to_string();
        state.config_hash = hash;
        state.proxy_port = proxy_port;
        state.controller_port = controller_port;
        state.last_error.clear();
    }

    let address = SocketAddr::from(([127, 0, 0, 1], controller_port));
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(8) {
        if cancelled.is_some_and(|token| token.is_cancelled()) {
            if let Ok(mut state) = runtime.inner.lock() {
                stop_child(&mut state);
            }
            return Err("测速已取消".into());
        }
        if TcpStream::connect_timeout(&address, Duration::from_millis(120)).is_ok() {
            return wait_runtime_ready(controller_port, nodes.len(), cancelled);
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
            if let Ok(mut state) = runtime.inner.lock() {
                state.last_error = message.clone();
                stop_child(&mut state);
            }
            return Err(message);
        }
        if started.elapsed() >= Duration::from_millis(500)
            && startup_log.contains("Initial configuration complete")
        {
            // 端口可能稍晚才 listen，继续等 TCP；但若已能连上上面分支会返回。
        }
        // 子进程是否已退出
        if let Ok(mut state) = runtime.inner.lock() {
            if let Some(child) = state.child.as_mut() {
                if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                    let message = format!("Mihomo 启动失败：{status}");
                    state.last_error = message.clone();
                    state.child = None;
                    return Err(message);
                }
            } else {
                // 取消线程可能已清掉 child
                if cancelled.is_some_and(|token| token.is_cancelled()) {
                    return Err("测速已取消".into());
                }
            }
        }
        std::thread::sleep(Duration::from_millis(80));
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
    if let Ok(mut state) = runtime.inner.lock() {
        state.last_error = message.clone();
        stop_child(&mut state);
    }
    Err(message)
}

fn controller_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .pool_max_idle_per_host(BATCH_PROXY_TEST_CONCURRENCY)
        .pool_idle_timeout(Duration::from_secs(30))
        .no_proxy()
        .build()
        .map_err(|error| error.to_string())
}

fn wait_runtime_ready(
    controller_port: u16,
    expected_nodes: usize,
    cancelled: Option<&CancellationToken>,
) -> Result<(), String> {
    // TCP 通了不代表 proxies 已注册完；首次测速全失败多半卡在这里。
    tauri::async_runtime::block_on(async move {
        let client = controller_client()?;
        let started = Instant::now();
        let deadline = Duration::from_secs(8);
        let mut last_error = "控制器尚未就绪".to_string();
        while started.elapsed() < deadline {
            if let Some(token) = cancelled {
                if token.is_cancelled() {
                    return Err("测速已取消".into());
                }
            }
            let version_ok = client
                .get(controller_url(controller_port, "/version"))
                .bearer_auth(RUNTIME_SECRET)
                .timeout(Duration::from_millis(400))
                .send()
                .await
                .ok()
                .filter(|response| response.status().is_success())
                .is_some();
            if !version_ok {
                last_error = "Mihomo /version 未就绪".into();
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            let proxies_url = controller_url(controller_port, "/proxies");
            match client
                .get(proxies_url)
                .bearer_auth(RUNTIME_SECRET)
                .timeout(Duration::from_millis(800))
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => {
                    let count = response
                        .json::<JsonValue>()
                        .await
                        .ok()
                        .and_then(|value| value.get("proxies")?.as_object().map(|obj| obj.len()))
                        .unwrap_or(0);
                    // 小批量要求接近完整注册；大批量只要控制器可用且已有部分节点即可开始测速，
                    // 否则 500+ 节点要等很久，首轮还容易全失败。
                    // builtins 通常 6~10 个；业务节点名用 id。
                    let min_required = if expected_nodes <= 40 {
                        expected_nodes.saturating_add(6)
                    } else {
                        expected_nodes.min(40).saturating_add(6)
                    };
                    if expected_nodes == 0 || count >= min_required {
                        tokio::time::sleep(Duration::from_millis(120)).await;
                        return Ok(());
                    }
                    last_error = format!("proxies 仅 {count} 个，等待至少 {min_required} 个就绪(目标 {expected_nodes})");
                }
                Ok(response) => {
                    last_error = format!("读取 /proxies 失败：HTTP {}", response.status().as_u16());
                }
                Err(error) => {
                    last_error = format!("读取 /proxies 失败：{error}");
                }
            }
            tokio::time::sleep(Duration::from_millis(120)).await;
        }
        Err(format!("Mihomo 测速就绪超时：{last_error}"))
    })
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
        .get(endpoint.clone())
        .bearer_auth(RUNTIME_SECRET)
        .timeout(Duration::from_millis(12000))
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
    if let Err(error) = ensure_runtime(database, runtime, None, None) {
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
    let emit_progress = |stage: &str,
                         status: &str,
                         message: String,
                         completed: usize,
                         total: usize,
                         added: usize,
                         discarded: usize| {
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
            )
        {
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
            emit_progress("parsing", "running", "正在解析节点链接…".into(), 0, 0, 0, 0);
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
    transaction.commit().map_err(|error| error.to_string())?;
    drop(connection);

    // 配置变更后异步重启运行时，不阻塞导入完成反馈。
    // ensure_runtime 内部 block_on 等待内核就绪，须 block_in_place 才不触发
    // async worker 上的运行时嵌套 panic。
    let _ = tokio::task::block_in_place(|| ensure_runtime(&database, &runtime, None, None));

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

fn is_slow_or_blocked_speed_test_url(value: &str) -> bool {
    // Cloudflare generate_204 在不少节点上极慢/失败，不适合作为默认测速。
    matches!(
        value.trim(),
        "https://cp.cloudflare.com/generate_204"
            | "http://cp.cloudflare.com/generate_204"
            | "https://cloudflare.com/cdn-cgi/trace"
    )
}

fn speed_test_candidates(configured: &str) -> Vec<String> {
    let configured = configured.trim();
    let configured = if configured.is_empty() || is_slow_or_blocked_speed_test_url(configured) {
        DEFAULT_PROXY_SPEED_TEST_URL
    } else {
        configured
    };
    // 批量测速只打用户选定/默认地址，避免每个节点串多个 fallback 把结果拖成“假慢”。
    vec![configured.to_string()]
}

fn normalize_ignore_addresses(value: &str) -> String {
    let mut items = value
        .split(|character: char| character == ',' || character == '\n' || character == ';')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    for required in [
        "localhost",
        "127.0.0.1",
        "::1",
        ".local",
        "10.0.0.0/8",
        "172.16.0.0/12",
        "192.168.0.0/16",
    ] {
        if !items.iter().any(|item| item.eq_ignore_ascii_case(required)) {
            items.push(required.to_string());
        }
    }
    items.join(",")
}

pub(crate) fn list_fast_proxy_nodes(
    database: &Database,
    max_latency_ms: i64,
) -> Result<Vec<(String, String, i64)>, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let mut statement = connection
        .prepare(
            "SELECT id, name, latency_ms
             FROM proxy_pool_nodes
             WHERE test_status = 'success'
               AND latency_ms IS NOT NULL
               AND latency_ms > 0
               AND latency_ms <= ?1
             ORDER BY latency_ms ASC, name COLLATE NOCASE",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([max_latency_ms], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    Ok(rows)
}

/// 供公益监听整轮同步使用：一次性把全部待轮询快节点装入全局内核，
/// 后续每个节点只需 API 切换出口，不再反复重启 Mihomo。
/// 供公益监听使用：优先选择「有订阅来源」的节点（如 igi 专线，信誉好、IP 干净），
/// 再补充剩余 success 节点。免费公共节点（无订阅关联）常被 Cloudflare 风控，
/// 若排在最前会导致大量 403；这里把订阅节点提到队首，降低坏节点命中率。
pub(crate) fn list_prioritized_fast_proxy_nodes(
    database: &Database,
    max_latency_ms: i64,
) -> Result<Vec<(String, String, i64)>, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let mut statement = connection
        .prepare(
            // igi/香港等高质量专线节点绝对优先，其次有订阅来源的节点，
            // 最后是无来源的免费公共节点（常被 Cloudflare 风控，导致 403）。
            "SELECT n.id, n.name, n.latency_ms
             FROM proxy_pool_nodes n
             WHERE n.test_status = 'success'
               AND n.latency_ms IS NOT NULL
               AND n.latency_ms > 0
               AND n.latency_ms <= ?1
             ORDER BY
               (CASE WHEN n.name LIKE 'iGG%' OR n.name LIKE 'igi%' THEN 0
                     WHEN (SELECT COUNT(*) FROM proxy_subscription_nodes sn WHERE sn.node_id = n.id) > 0 THEN 1
                     ELSE 2 END) ASC,
               n.latency_ms ASC,
               n.name COLLATE NOCASE",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([max_latency_ms], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    Ok(rows)
}

pub(crate) async fn prepare_proxy_nodes_transient(
    database: &Database,
    runtime: &ProxyRuntime,
    node_ids: &[String],
) -> Result<(), String> {
    let _guard = runtime.runtime_op_lock.lock().await;
    let only = node_ids.iter().cloned().collect::<HashSet<_>>();
    tokio::task::block_in_place(|| ensure_runtime(database, runtime, Some(&only), None))?;
    Ok(())
}

/// 仅通过 Mihomo 控制器切换出口；若节点已不在当前内核配置中（例如用户
/// 刚切换过节点），则回退为装载单个节点后重试，避免后台任务整体失败。
pub(crate) async fn select_proxy_node_transient(
    database: &Database,
    runtime: &ProxyRuntime,
    node_id: &str,
) -> Result<(), String> {
    let _guard = runtime.runtime_op_lock.lock().await;
    if select_runtime_node(runtime, node_id).await.is_err() {
        let only = HashSet::from([node_id.to_string()]);
        tokio::task::block_in_place(|| ensure_runtime(database, runtime, Some(&only), None))?;
        select_runtime_node(runtime, node_id).await?;
    }
    Ok(())
}

/// 恢复 Mihomo 出口到用户手动开启的全局代理节点；若全局代理未开启则不动。
/// 全程不写全局代理状态。
pub(crate) async fn restore_proxy_node_transient(
    database: &Database,
    runtime: &ProxyRuntime,
) -> Result<(), String> {
    let active_id = read_meta(database, ACTIVE_PROXY_NODE_KEY)?;
    if active_id.trim().is_empty() {
        return Ok(());
    }
    let runtime_name = {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        connection
            .query_row(
                "SELECT id FROM proxy_pool_nodes WHERE id=?1",
                [active_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or("全局代理节点不存在")?
    };
    let _guard = runtime.runtime_op_lock.lock().await;
    // 当前内核已加载该节点时直接切回，避免无谓重启；
    // 否则（例如用户刚切换过节点）再装载并选择。
    if select_runtime_node(runtime, &runtime_name).await.is_err() {
        let only = HashSet::from([runtime_name.clone()]);
        tokio::task::block_in_place(|| ensure_runtime(database, runtime, Some(&only), None))?;
        select_runtime_node(runtime, &runtime_name).await?;
    }
    Ok(())
}

/// 返回当前 Mihomo 混合端口地址，供后台任务显式走代理（不依赖全局代理设置）。
pub(crate) fn runtime_proxy_url_pub(runtime: &ProxyRuntime) -> String {
    runtime_proxy_url(runtime)
}

#[tauri::command]
pub fn set_proxy_pool_settings(
    database: State<'_, Database>,
    runtime: State<'_, ProxyRuntime>,
    ignore_addresses: String,
    speed_test_url: String,
) -> Result<ProxyPoolState, String> {
    let speed_test_url = speed_test_url.trim();
    let parsed = Url::parse(speed_test_url).map_err(|_| "测速地址格式无效".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("测速地址必须是 HTTP(S) 地址".into());
    }
    let ignore = normalize_ignore_addresses(&ignore_addresses);
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    write_meta(&connection, PROXY_IGNORE_KEY, &ignore)?;
    write_meta(&connection, PROXY_SPEED_TEST_URL_KEY, speed_test_url)?;
    drop(connection);
    load_state(&database, &runtime)
}

#[tauri::command]
pub async fn set_active_proxy_node(
    database: State<'_, Database>,
    runtime: State<'_, ProxyRuntime>,
    node_id: String,
) -> Result<ProxyPoolState, String> {
    let runtime_name = {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        connection
            .query_row(
                "SELECT id FROM proxy_pool_nodes WHERE id=?1",
                [&node_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or("代理节点不存在")?
    };
    let only = HashSet::from([runtime_name.clone()]);
    // 与后台任务的出口切换互斥，避免竞态；ensure_runtime 含同步等待，必须 block_in_place。
    let _guard = runtime.runtime_op_lock.lock().await;
    tokio::task::block_in_place(|| ensure_runtime(&database, &runtime, Some(&only), None))?;
    select_runtime_node(&runtime, &runtime_name).await?;
    let proxy_url = runtime_proxy_url(&runtime);
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    write_meta(&connection, ACTIVE_PROXY_NODE_KEY, &node_id)?;
    write_meta(&connection, NETWORK_PROXY_KEY, &proxy_url)?;
    drop(connection);
    load_state(&database, &runtime)
}

#[tauri::command]
pub fn clear_active_proxy_node(
    database: State<'_, Database>,
    runtime: State<'_, ProxyRuntime>,
) -> Result<ProxyPoolState, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    write_meta(&connection, ACTIVE_PROXY_NODE_KEY, "")?;
    write_meta(&connection, NETWORK_PROXY_KEY, "")?;
    drop(connection);
    load_state(&database, &runtime)
}

#[tauri::command]
pub fn delete_invalid_proxy_nodes(
    database: State<'_, Database>,
    runtime: State<'_, ProxyRuntime>,
) -> Result<ProxyPoolState, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    connection
        .execute(
            "DELETE FROM proxy_pool_nodes WHERE test_status = 'invalid'",
            [],
        )
        .map_err(|error| error.to_string())?;
    drop(connection);
    if let Ok(mut state) = runtime.inner.lock() {
        state.config_hash.clear();
    }
    load_state(&database, &runtime)
}

#[tauri::command]
pub async fn test_proxy_node(
    database: State<'_, Database>,
    runtime: State<'_, ProxyRuntime>,
    node_id: String,
) -> Result<ProxyNode, String> {
    // 单节点测速使用独立内核，不切换、不停止用户手动开启的全局代理。
    let test_id = runtime.next_test_id.fetch_add(1, Ordering::Relaxed);
    let test_directory = runtime.directory.join(format!("single-test-{test_id}"));
    let _test_directory_cleanup = TemporaryRuntimeDirectory(test_directory.clone());
    let port_offset = ((test_id % 100) as u16).saturating_mul(2);
    let test_runtime = ProxyRuntime::new_with_ports(
        test_directory,
        37890u16.saturating_add(port_offset),
        39090u16.saturating_add(port_offset),
    );
    let only = HashSet::from([node_id.clone()]);
    tokio::task::block_in_place(|| ensure_runtime(&database, &test_runtime, Some(&only), None))?;
    let controller_port = runtime_controller_port(&test_runtime)?;
    let configured = {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        let exists = connection
            .query_row(
                "SELECT 1 FROM proxy_pool_nodes WHERE id=?1",
                [&node_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .is_some();
        if !exists {
            return Err("代理节点不存在".into());
        }
        connection
            .query_row(
                "SELECT value FROM app_meta WHERE key=?1",
                [PROXY_SPEED_TEST_URL_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .filter(|item| !item.is_empty() && !is_slow_or_blocked_speed_test_url(item))
            .unwrap_or_else(|| DEFAULT_PROXY_SPEED_TEST_URL.to_string())
    };
    let client = controller_client()?;
    let mut latency = None;
    let mut attempted = Vec::new();
    for target in speed_test_candidates(&configured) {
        attempted.push(target.clone());
        latency =
            test_controller_proxy_delay(client.clone(), controller_port, node_id.clone(), target)
                .await;
        if latency.is_some() {
            break;
        }
    }
    let status = if latency.is_some() {
        "success"
    } else {
        "error"
    };
    let error_message = if latency.is_some() {
        None
    } else {
        Some(format!("测速失败，已尝试 {} 个测速地址", attempted.len()))
    };
    {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        connection
            .execute(
                "UPDATE proxy_pool_nodes SET latency_ms=?2, test_status=?3, tested_at=CURRENT_TIMESTAMP WHERE id=?1",
                params![node_id, latency, status],
            )
            .map_err(|error| error.to_string())?;
    }
    drop(test_runtime);
    let state = load_state(&database, &runtime)?;
    let node = state
        .nodes
        .into_iter()
        .find(|item| item.id == node_id)
        .ok_or("测速后读取节点失败")?;
    if let Some(error) = error_message {
        Err(error)
    } else {
        Ok(node)
    }
}

async fn run_proxy_node_pool(
    app: &AppHandle,
    database: &Database,
    runtime: &ProxyRuntime,
    requested_node_ids: Option<HashSet<String>>,
) -> Result<ProxyPoolState, String> {
    // 测速策略（对齐 Clash Verge Rev DelayManager.checkListDelay）：
    // 1) 只测请求集合（选中来源/指定节点/全部）
    // 2) 待测节点装入 Mihomo 后并行 delay，不在节点之间重启内核
    // 3) 并发上限 10（与 Verge 前端 actualConcurrency 一致）
    // 4) 固定测速 URL；每条代理独立拨号计时
    // 5) 测速使用独立 Mihomo 运行时，不覆盖或重启用户的全局代理出口
    let configured = {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        connection
            .query_row(
                "SELECT value FROM app_meta WHERE key=?1",
                [PROXY_SPEED_TEST_URL_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .filter(|item| !item.is_empty() && !is_slow_or_blocked_speed_test_url(item))
            .unwrap_or_else(|| DEFAULT_PROXY_SPEED_TEST_URL.to_string())
    };
    let nodes = {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        let mut statement = connection
            .prepare("SELECT id, test_status FROM proxy_pool_nodes")
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        rows
    };
    let testable_nodes = nodes
        .into_iter()
        .filter(|(id, status)| {
            status != "invalid"
                && requested_node_ids
                    .as_ref()
                    .map(|requested| requested.contains(id))
                    .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    let total = testable_nodes.len();
    if total == 0 {
        return Err("没有可测速的代理节点".into());
    }

    let targets = speed_test_candidates(&configured);
    let client = controller_client()?;
    let test_lease = runtime.start_proxy_test()?;
    let cancellation = test_lease.cancellation.clone();
    // 独立目录 + 独立端口范围：避免与全局代理、热重载残留进程争抢端口。
    let test_directory = runtime
        .directory
        .join(format!("speed-test-{}", test_lease.id));
    let _test_directory_cleanup = TemporaryRuntimeDirectory(test_directory.clone());
    let port_offset = ((test_lease.id % 100) as u16).saturating_mul(2);
    let speed_runtime = ProxyRuntime::new_with_ports(
        test_directory,
        27890u16.saturating_add(port_offset),
        29090u16.saturating_add(port_offset),
    );

    let mut completed = 0usize;
    let mut succeeded = 0usize;
    let mut cancelled = false;
    let mut pending_writes: Vec<(String, Option<i64>)> = Vec::with_capacity(64);
    let mut last_flush = Instant::now();
    let flush_writes = |pending: &mut Vec<(String, Option<i64>)>| -> Result<(), String> {
        if pending.is_empty() {
            return Ok(());
        }
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        let tx = connection
            .unchecked_transaction()
            .map_err(|error| error.to_string())?;
        for (id, delay) in pending.drain(..) {
            if let Some(delay) = delay {
                tx.execute(
                    "UPDATE proxy_pool_nodes SET latency_ms=?2, test_status='success', tested_at=CURRENT_TIMESTAMP WHERE id=?1",
                    params![id, delay],
                )
                .map_err(|error| error.to_string())?;
            } else {
                tx.execute(
                    "UPDATE proxy_pool_nodes SET latency_ms=NULL, test_status='error', tested_at=CURRENT_TIMESTAMP WHERE id=?1",
                    [&id],
                )
                .map_err(|error| error.to_string())?;
            }
        }
        tx.commit().map_err(|error| error.to_string())?;
        Ok(())
    };

    for chunk in testable_nodes.chunks(BATCH_PROXY_TEST_NODE_CHUNK) {
        if cancellation.is_cancelled() {
            cancelled = true;
            break;
        }
        let chunk_ids = chunk
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<HashSet<_>>();
        // 大列表才分块装载；块内并行 delay，块与块之间才切换配置。
        // ensure_runtime 含同步等待/子进程，必须 block_in_place，否则会堵死 async worker 导致取消无响应。
        if let Err(error) = tokio::task::block_in_place(|| {
            ensure_runtime(
                database,
                &speed_runtime,
                Some(&chunk_ids),
                Some(&cancellation),
            )
        }) {
            if cancellation.is_cancelled() || error.contains("已取消") {
                cancelled = true;
                break;
            }
            // 块装载失败时，把该块记为 error 并继续，避免整次测速全挂。
            for (id, _) in chunk {
                completed += 1;
                pending_writes.push((id.clone(), None));
                let _ = app.emit(
                    "proxy-node-test-progress",
                    ProxyNodeTestProgress {
                        node_id: id.clone(),
                        phase: "completed".to_string(),
                        latency_ms: None,
                        status: "error".to_string(),
                        completed,
                        total,
                    },
                );
            }
            eprintln!("OpenHub 测速分块装载失败：{error}");
            if pending_writes.len() >= 40 || last_flush.elapsed() >= Duration::from_millis(100) {
                flush_writes(&mut pending_writes)?;
                last_flush = Instant::now();
            }
            continue;
        }
        let controller_port = match runtime_controller_port(&speed_runtime) {
            Ok(port) => port,
            Err(error) => {
                for (id, _) in chunk {
                    completed += 1;
                    pending_writes.push((id.clone(), None));
                    let _ = app.emit(
                        "proxy-node-test-progress",
                        ProxyNodeTestProgress {
                            node_id: id.clone(),
                            phase: "completed".to_string(),
                            latency_ms: None,
                            status: "error".to_string(),
                            completed,
                            total,
                        },
                    );
                }
                eprintln!("OpenHub 测速读取控制器失败：{error}");
                continue;
            }
        };
        if let Err(error) = tokio::task::block_in_place(|| {
            wait_runtime_ready(controller_port, chunk.len(), Some(&cancellation))
        }) {
            // 用户取消也会走到这里：立即结束，不要把整块标成 error。
            if cancellation.is_cancelled() || error.contains("已取消") {
                cancelled = true;
                break;
            }
            for (id, _) in chunk {
                completed += 1;
                pending_writes.push((id.clone(), None));
                let _ = app.emit(
                    "proxy-node-test-progress",
                    ProxyNodeTestProgress {
                        node_id: id.clone(),
                        phase: "completed".to_string(),
                        latency_ms: None,
                        status: "error".to_string(),
                        completed,
                        total,
                    },
                );
            }
            eprintln!("OpenHub 测速等待内核就绪失败：{error}");
            continue;
        }

        let mut results = stream::iter(chunk.to_vec())
            .map(|(id, _status)| {
                let client = client.clone();
                let targets = targets.clone();
                let app = app.clone();
                let cancellation = cancellation.clone();
                async move {
                    if cancellation.is_cancelled() {
                        return (id, None, true);
                    }
                    let _ = app.emit(
                        "proxy-node-test-progress",
                        ProxyNodeTestProgress {
                            node_id: id.clone(),
                            phase: "started".to_string(),
                            status: "testing".to_string(),
                            total,
                            ..Default::default()
                        },
                    );
                    // 与 Clash 相同：固定测速 URL + 独立 delay。
                    // 并行由 buffer_unordered 控制，不在这里串行化。
                    if cancellation.is_cancelled() {
                        return (id, None, true);
                    }
                    let target = targets
                        .first()
                        .cloned()
                        .unwrap_or_else(|| DEFAULT_PROXY_SPEED_TEST_URL.to_string());
                    let request =
                        test_controller_proxy_delay(client, controller_port, id.clone(), target);
                    let cancelled = cancellation.cancelled();
                    pin_mut!(request, cancelled);
                    match future::select(request, cancelled).await {
                        future::Either::Left((delay, _)) => (id, delay, false),
                        future::Either::Right((_, _)) => (id, None, true),
                    }
                }
            })
            // Clash Verge: actualConcurrency = min(concurrency, names.length, 10)
            .buffer_unordered(std::cmp::min(BATCH_PROXY_TEST_CONCURRENCY, chunk.len()).max(1));

        while let Some((id, delay, node_cancelled)) = results.next().await {
            if node_cancelled || cancellation.is_cancelled() {
                cancelled = true;
                let _ = app.emit(
                    "proxy-node-test-progress",
                    ProxyNodeTestProgress {
                        node_id: id,
                        phase: "completed".to_string(),
                        latency_ms: None,
                        status: "cancelled".to_string(),
                        completed,
                        total,
                    },
                );
                // 一旦取消：丢掉剩余 in-flight future，尽快返回前端。
                drop(results);
                break;
            }
            completed += 1;
            let status = if delay.is_some() { "success" } else { "error" };
            if delay.is_some() {
                succeeded += 1;
            }
            pending_writes.push((id.clone(), delay));
            if pending_writes.len() >= 40 || last_flush.elapsed() >= Duration::from_millis(100) {
                flush_writes(&mut pending_writes)?;
                last_flush = Instant::now();
            }
            let _ = app.emit(
                "proxy-node-test-progress",
                ProxyNodeTestProgress {
                    node_id: id,
                    phase: "completed".to_string(),
                    latency_ms: delay,
                    status: status.to_string(),
                    completed,
                    total,
                },
            );
        }
        if cancellation.is_cancelled() {
            cancelled = true;
            break;
        }
    }

    flush_writes(&mut pending_writes)?;
    // 先停止独立测速内核，再释放任务 lease；全局代理进程从未被触碰。
    drop(speed_runtime);
    drop(test_lease);
    let state = load_state(database, runtime)?;
    // 即使全部失败也返回状态，让前端能看到每个节点的 error；
    // 避免只丢一句总错误、无法继续排查。
    let _ = succeeded;
    let _ = cancelled;
    Ok(state)
}

#[tauri::command]
pub fn cancel_proxy_node_tests(runtime: State<'_, ProxyRuntime>) -> Result<bool, String> {
    // 前端只关心“是否发出取消”；不要因内部状态抛异常。
    match runtime.cancel_proxy_test() {
        Ok(v) => Ok(v),
        Err(error) => {
            eprintln!("OpenHub 取消测速内部警告：{error}");
            Ok(false)
        }
    }
}

#[tauri::command]
pub async fn test_all_proxy_nodes(
    app: AppHandle,
    database: State<'_, Database>,
    runtime: State<'_, ProxyRuntime>,
) -> Result<ProxyPoolState, String> {
    run_proxy_node_pool(&app, &database, &runtime, None).await
}

#[tauri::command]
pub async fn test_proxy_nodes(
    app: AppHandle,
    database: State<'_, Database>,
    runtime: State<'_, ProxyRuntime>,
    node_ids: Vec<String>,
) -> Result<ProxyPoolState, String> {
    let requested = node_ids
        .into_iter()
        .filter(|id| !id.trim().is_empty())
        .collect::<HashSet<_>>();
    if requested.is_empty() {
        return Err("请选择需要测速的节点".into());
    }
    run_proxy_node_pool(&app, &database, &runtime, Some(requested)).await
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let node = parse_subscription("proxies:\n  - name: bad-port\n    type: http\n    server: example.com\n    port: 70000\n").unwrap();
        assert!(basic_node_config_error(&node[0].raw_json).is_some());
    }

    #[test]
    fn parses_vmess_websocket_options() {
        // host/path/net must be preserved; otherwise delay test always fails.
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
        // ssocks / https base64 payload = base64("user:pass@host.example.com:1080") etc.
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
}
