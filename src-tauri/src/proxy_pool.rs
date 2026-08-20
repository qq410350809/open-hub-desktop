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
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio_util::sync::CancellationToken;
use url::Url;

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
// 站点级账号代理：同一账号固定一个 ≤500ms 节点并持久化；失败换节点最多重试一次。
const ACCOUNT_PROXY_MAX_LATENCY_MS: i64 = 500;
const ACCOUNT_PROXY_MAX_ATTEMPTS: usize = 2;
const ACCOUNT_PROXY_BAN_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const ACCOUNT_PROXY_BAN_FORBIDDEN: Duration = Duration::from_secs(2 * 60 * 60);
const ACCOUNT_PROXY_BAN_UNREACHABLE: Duration = Duration::from_secs(2 * 60 * 60);
const ACCOUNT_PROXY_BAN_DEFAULT: Duration = Duration::from_secs(15 * 60);
const DEFAULT_PROXY_CHANNEL_ID: &str = "default";
const DEFAULT_PROXY_CHANNEL_NAME: &str = "默认通道";
const CHANNEL_SPEED_TEST_URL: &str = "https://speed.cloudflare.com/__down?bytes=500000";

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

struct InstanceState {
    child: Option<Child>,
    directory: PathBuf,
    config_hash: String,
    engine_path: String,
    last_error: String,
    proxy_port: u16,
    controller_port: u16,
}

fn stop_single_instance(instance: &mut InstanceState) {
    if let Some(mut child) = instance.child.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

struct ActiveProxyTest {
    id: u64,
    cancellation: CancellationToken,
}

pub(crate) struct ProxyRuntime {
    directory: PathBuf,
    shared_instance: Mutex<InstanceState>,
    channel_instances: Mutex<HashMap<String, InstanceState>>,
    account_instances: Mutex<HashMap<String, InstanceState>>,
    active_test: Mutex<Option<ActiveProxyTest>>,
    next_test_id: AtomicU64,
    // 全局代理内核“重启/选节点”的串行锁：用户切换与公益监听等后台任务
    // 互斥操作同一 Mihomo，避免互相杀进程/覆盖选择导致切换卡死。
    runtime_op_lock: tokio::sync::Mutex<()>,
    // 整个池串行轮询模式锁：未分配固定通道的账号共享同一代理实例时串行轮换
    shared_pool_lock: tokio::sync::Mutex<()>,
    shared_pool_index: AtomicU64,
    // 账号代理节点黑名单（内存 TTL）：node_id -> 解禁时间。
    account_ban_until: Mutex<HashMap<String, Instant>>,
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
        let shared_dir = directory.join("shared");
        Self {
            directory,
            shared_instance: Mutex::new(InstanceState {
                child: None,
                directory: shared_dir,
                config_hash: String::new(),
                engine_path: String::new(),
                last_error: String::new(),
                proxy_port,
                controller_port,
            }),
            channel_instances: Mutex::new(HashMap::new()),
            account_instances: Mutex::new(HashMap::new()),
            active_test: Mutex::new(None),
            next_test_id: AtomicU64::new(1),
            runtime_op_lock: tokio::sync::Mutex::new(()),
            shared_pool_lock: tokio::sync::Mutex::new(()),
            shared_pool_index: AtomicU64::new(0),
            account_ban_until: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn channel_port(&self, channel_id: &str) -> Option<u16> {
        let instances = self.channel_instances.lock().ok()?;
        let inst = instances.get(channel_id)?;
        (inst.proxy_port > 0).then_some(inst.proxy_port)
    }

    pub(crate) fn channel_proxy_url(&self, channel_id: &str) -> Option<String> {
        let port = self.channel_port(channel_id)?;
        Some(format!("http://127.0.0.1:{port}"))
    }

    pub(crate) fn account_port(&self, profile_id: &str) -> Option<u16> {
        let instances = self.account_instances.lock().ok()?;
        let inst = instances.get(profile_id)?;
        (inst.proxy_port > 0).then_some(inst.proxy_port)
    }

    pub(crate) fn account_proxy_url(&self, profile_id: &str) -> Option<String> {
        let port = self.account_port(profile_id)?;
        Some(format!("http://127.0.0.1:{port}"))
    }

    pub(crate) fn shared_proxy_url(&self) -> Option<String> {
        let state = self.shared_instance.lock().ok()?;
        if state.proxy_port > 0 {
            Some(format!("http://127.0.0.1:{}", state.proxy_port))
        } else {
            None
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

    fn purge_account_bans(&self) {
        let Ok(mut bans) = self.account_ban_until.lock() else {
            return;
        };
        let now = Instant::now();
        bans.retain(|_, until| *until > now);
    }

    fn account_node_is_banned(&self, node_id: &str) -> bool {
        if node_id.trim().is_empty() {
            return false;
        }
        let Ok(mut bans) = self.account_ban_until.lock() else {
            return false;
        };
        let now = Instant::now();
        match bans.get(node_id) {
            Some(until) if *until > now => true,
            Some(_) => {
                bans.remove(node_id);
                false
            }
            None => false,
        }
    }

    fn account_ban_node(&self, node_id: &str, ttl: Duration) {
        if node_id.trim().is_empty() {
            return;
        }
        if let Ok(mut bans) = self.account_ban_until.lock() {
            let until = Instant::now() + ttl;
            bans.insert(node_id.to_string(), until);
        }
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
        if let Ok(mut state) = self.shared_instance.lock() {
            stop_single_instance(&mut state);
        }
        if let Ok(mut map) = self.channel_instances.lock() {
            for (_, mut inst) in map.drain() {
                stop_single_instance(&mut inst);
            }
        }
        if let Ok(mut map) = self.account_instances.lock() {
            for (_, mut inst) in map.drain() {
                stop_single_instance(&mut inst);
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

fn ensure_default_proxy_channel(connection: &rusqlite::Connection) -> Result<(), String> {
    connection
        .execute(
            "INSERT OR IGNORE INTO proxy_channels (id, name) VALUES (?1, ?2)",
            params![DEFAULT_PROXY_CHANNEL_ID, DEFAULT_PROXY_CHANNEL_NAME],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn load_channels(
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

    // 旧数据若缺国家字段，或者为 "ZZ"，且有 GeoIP 时，尝试推断后写回
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
    let _ = port;
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

    // 域名场景：优先由节点名推断；若名称无地域，尝试由域名推断
    if let Some((code, country_name)) = inferred_country(name) {
        return (code, country_name, "public".to_string(), String::new());
    }
    if let Some((code, country_name)) = inferred_country(server) {
        return (code, country_name, "public".to_string(), String::new());
    }

    (
        "ZZ".to_string(),
        "未知地区".to_string(),
        "unresolved".to_string(),
        String::new(),
    )
}

pub fn find_geoip_database(runtime: &ProxyRuntime) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("OPENHUB_GEOIP_DB") {
        candidates.push(PathBuf::from(path));
    }
    candidates.extend([
        runtime.directory.join("Country.mmdb"),
        runtime.directory.join("country.mmdb"),
        runtime.directory.join("GeoLite2-Country.mmdb"),
    ]);
    if let Some(parent) = runtime.directory.parent() {
        candidates.extend([
            parent.join("Country.mmdb"),
            parent.join("country.mmdb"),
            parent.join("GeoLite2-Country.mmdb"),
            parent.join("bin").join("Country.mmdb"),
            parent.join("bin").join("country.mmdb"),
        ]);
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        candidates.extend([
            home.join("Library/Application Support/com.dfeer.openhub.desktop/Country.mmdb"),
            home.join(".config/com.dfeer.openhub.desktop/Country.mmdb"),
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

pub fn open_geoip_reader(runtime: &ProxyRuntime) -> Option<Reader<Vec<u8>>> {
    let path = find_geoip_database(runtime)?;
    Reader::open_readfile(path).ok()
}

pub fn repair_node_locations_with_geoip(
    database: &Database,
    runtime: &ProxyRuntime,
) -> Result<usize, String> {
    let geoip_reader = match open_geoip_reader(runtime) {
        Some(r) => r,
        None => return Ok(0),
    };

    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let mut stmt = connection
        .prepare("SELECT id, name, server, port, country_code, country_name, classification, primary_ip FROM proxy_pool_nodes")
        .map_err(|e| e.to_string())?;

    struct Candidate {
        id: String,
        name: String,
        server: String,
        port: i64,
        country_code: String,
        country_name: String,
        classification: String,
        primary_ip: String,
    }

    let rows = stmt
        .query_map([], |row| {
            Ok(Candidate {
                id: row.get(0)?,
                name: row.get(1)?,
                server: row.get(2)?,
                port: row.get(3)?,
                country_code: row.get(4)?,
                country_name: row.get(5)?,
                classification: row.get(6)?,
                primary_ip: row.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let mut updated = 0;
    for c in rows {
        let is_unknown = c.country_code.trim().is_empty() || c.country_code == "ZZ";
        let (code, name, class, ip) = classify_node_location(&c.name, &c.server, c.port, Some(&geoip_reader));
        if code != "ZZ" && (is_unknown || c.country_code != code) {
            let _ = connection.execute(
                "UPDATE proxy_pool_nodes
                 SET country_code=?2, country_name=?3, classification=?4,
                     primary_ip=CASE WHEN ?5 != '' THEN ?5 ELSE primary_ip END
                 WHERE id=?1",
                params![c.id, code, name, class, ip],
            );
            updated += 1;
        }
    }

    Ok(updated)
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
    let geoip_reader = open_geoip_reader(&runtime);
    let mut groups_map: HashMap<String, ProxyIpGroup> = HashMap::new();
    let mut analyses = Vec::with_capacity(state.nodes.len());
    let mut unique_ips = HashSet::new();
    let mut resolved_nodes = 0usize;

    for node in &state.nodes {
        let mut country_code = node.country_code.trim().to_string();
        let mut country_name = node.country_name.trim().to_string();
        let mut classification = node.classification.trim().to_string();
        let primary_ip = node.primary_ip.trim().to_string();

        if country_code.is_empty() || country_name.is_empty() || country_code == "ZZ" {
            let (code, name, class, ip) =
                classify_node_location(&node.name, &node.server, node.port, geoip_reader.as_ref());
            if code != "ZZ" || country_code.is_empty() {
                country_code = code;
                country_name = name;
            }
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

fn speed_value_end(lower: &str, from: usize) -> Option<usize> {
    let bytes = lower.as_bytes();
    let mut index = from;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if index == from {
        return None;
    }
    if index < bytes.len() && bytes[index] == b'.' {
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
    }
    let rest = &lower[index..];
    let after_space = rest.trim_start_matches(' ');
    let space_len = rest.len() - after_space.len();
    let units = [
        "mb/s", "kb/s", "gb/s", "tb/s", "mbps", "kbps", "gbps", "tbps", "mbs", "kbs", "gbs", "tbs",
        "mbit/s", "kbit/s", "gbit/s", "tbit/s", "mbits/s", "kbits/s", "gbits/s", "tbits/s",
        "mb/秒", "kb/秒", "gb/秒", "tb/秒", "m/s", "k/s", "g/s", "t/s", "m/秒", "k/秒", "g/秒",
        "t/秒",
    ];
    units
        .iter()
        .find(|unit| after_space.starts_with(**unit))
        .map(|unit| index + space_len + unit.len())
}

fn speed_result_start(lower: &str, original: &str) -> Option<usize> {
    let bytes = lower.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let separated = index == 0
            || original[..index]
                .chars()
                .next_back()
                .is_some_and(|character| {
                    matches!(
                        character,
                        '|' | '｜'
                            | '·'
                            | '•'
                            | '-'
                            | '—'
                            | ':'
                            | '：'
                            | '/'
                            | '('
                            | '（'
                            | '['
                            | '【'
                            | ' '
                            | '\t'
                    )
                });
        if separated && speed_value_end(lower, index).is_some() {
            return Some(index);
        }
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index < bytes.len() && bytes[index] == b'.' {
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
        }
    }
    None
}

/// 去掉订阅里常见的测速结果后缀（如「| 延迟 123ms」「测速：88ms」「| 52MB/s」），
/// 只保留真实节点名。仅当标识词/数值前有分隔符时裁剪，避免误伤「低延迟专线」这类合法名称。
fn clean_node_name(name: &str) -> String {
    let lower = name.to_lowercase();
    let markers = [
        "测速结果",
        "测速",
        "速度测试",
        "延迟测试",
        "时延",
        "延迟",
        "速度",
        "speedtest",
        "speed test",
        "latency",
        "rtt",
        "ping",
    ];
    let mut cut = name.len();
    for marker in markers {
        let mut offset = 0;
        while let Some(found) = lower[offset..].find(marker) {
            let index = offset + found;
            let separated = name[..index]
                .chars()
                .next_back()
                .map(|character| {
                    matches!(
                        character,
                        '|' | '｜'
                            | '·'
                            | '•'
                            | '-'
                            | '—'
                            | ':'
                            | '：'
                            | '/'
                            | '('
                            | '（'
                            | '['
                            | '【'
                            | ' '
                            | '\t'
                    )
                })
                .unwrap_or(true);
            if separated {
                cut = cut.min(index);
                break;
            }
            offset = index + marker.len();
        }
    }
    if let Some(speed_cut) = speed_result_start(&lower, name) {
        cut = cut.min(speed_cut);
    }
    let cleaned = name[..cut].trim_end_matches(|character: char| {
        matches!(
            character,
            '|' | '｜'
                | '·'
                | '•'
                | '-'
                | '—'
                | ':'
                | '：'
                | '/'
                | '('
                | '（'
                | '['
                | '【'
                | ' '
                | '\t'
        )
    });
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        name.trim().to_string()
    } else {
        cleaned.to_string()
    }
}

/// 净化与规范化代理节点 JSON，杜绝 Mihomo（Clash Meta）解析配置时因字段类型不匹配致命退出（如 alpn 不是 slice/array）。
pub(crate) fn sanitize_proxy_node_json(value: &mut JsonValue) {
    let Some(object) = value.as_object_mut() else {
        return;
    };

    // 1. 规范化 alpn：Mihomo 要求必须是 slice/array (Vec<String>)，字符串会导致 Parse config error: 'alpn' is not a slice
    if let Some(alpn_val) = object.get("alpn") {
        let items: Vec<JsonValue> = match alpn_val {
            JsonValue::String(s) => s
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(|item| JsonValue::String(item.to_string()))
                .collect(),
            JsonValue::Array(arr) => arr
                .iter()
                .filter_map(JsonValue::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(|item| JsonValue::String(item.to_string()))
                .collect(),
            _ => Vec::new(),
        };
        if items.is_empty() {
            object.remove("alpn");
        } else {
            object.insert("alpn".to_string(), JsonValue::Array(items));
        }
    }

    // 2. 规范化 tls
    if let Some(tls_val) = object.get("tls") {
        if let Some(tls_str) = tls_val.as_str() {
            let is_tls = !tls_str.is_empty()
                && !tls_str.eq_ignore_ascii_case("none")
                && !tls_str.eq_ignore_ascii_case("false")
                && !tls_str.eq_ignore_ascii_case("0");
            object.insert("tls".to_string(), JsonValue::Bool(is_tls));
        }
    }

    // 3. 规范化 skip-cert-verify
    if let Some(skip_val) = object.get("skip-cert-verify") {
        if let Some(skip_str) = skip_val.as_str() {
            let is_skip = skip_str.eq_ignore_ascii_case("true") || skip_str == "1";
            object.insert("skip-cert-verify".to_string(), JsonValue::Bool(is_skip));
        }
    }

    // 4. 规范化 udp
    if let Some(udp_val) = object.get("udp") {
        if let Some(udp_str) = udp_val.as_str() {
            let is_udp = udp_str.eq_ignore_ascii_case("true") || udp_str == "1";
            object.insert("udp".to_string(), JsonValue::Bool(is_udp));
        }
    }

    // 5. 规范化 port (确保是数字)
    if let Some(port_val) = object.get("port") {
        if let Some(port_str) = port_val.as_str() {
            if let Ok(port_num) = port_str.parse::<i64>() {
                object.insert("port".to_string(), json!(port_num));
            }
        }
    }
}

fn node_from_json(mut value: JsonValue) -> Option<ParsedNode> {
    sanitize_proxy_node_json(&mut value);
    let object = value.as_object_mut()?;
    let name = clean_node_name(object.get("name")?.as_str()?.trim());
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

/// 启动时修复历史节点名与配置：去掉订阅里遗留的测速结果后缀，清洗配置（规范化 alpn 等字段），重名自动加序号。
pub(crate) fn repair_stored_node_names(database: &Database) -> Result<usize, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let rows = connection
        .prepare("SELECT id, name, raw_json FROM proxy_pool_nodes ORDER BY name COLLATE NOCASE")
        .map_err(|error| error.to_string())?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    if rows.is_empty() {
        return Ok(0);
    }
    let mut used_names = rows
        .iter()
        .map(|(_, name, _)| name.clone())
        .collect::<HashSet<_>>();
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| error.to_string())?;
    let mut repaired = 0usize;
    for (id, name, raw_json) in rows {
        let cleaned = clean_node_name(&name);
        let mut config: JsonValue = serde_json::from_str(&raw_json).unwrap_or(JsonValue::Null);
        let orig_config_str = config.to_string();
        sanitize_proxy_node_json(&mut config);

        let need_name_update = cleaned != name;
        let need_json_update = config.to_string() != orig_config_str;

        if !need_name_update && !need_json_update {
            continue;
        }

        let final_name = if need_name_update {
            unique_name(&cleaned, &mut used_names)
        } else {
            name.clone()
        };

        if let Some(object) = config.as_object_mut() {
            object.insert("name".into(), JsonValue::String(final_name.clone()));
        }
        transaction
            .execute(
                "UPDATE proxy_pool_nodes SET name = ?2, raw_json = ?3 WHERE id = ?1",
                params![id, final_name, config.to_string()],
            )
            .map_err(|error| error.to_string())?;
        repaired += 1;
    }
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(repaired)
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
        "trojan" => {
            let mut obj = json!({
                "name": name, "type": "trojan", "server": server, "port": port,
                "password": url.username(), "sni": query.get("sni").or_else(|| query.get("peer")).map(|v| v.as_ref()).unwrap_or(&server),
                "skip-cert-verify": query.get("allowInsecure").is_some_and(|v| v == "1" || v == "true"), "udp": true
            });
            if let Some(alpn) = query.get("alpn") {
                let items: Vec<String> = alpn.split(',').map(str::trim).filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
                if !items.is_empty() {
                    obj["alpn"] = json!(items);
                }
            }
            obj
        }
        "anytls" => json!({
            "name": name, "type": "anytls", "server": server, "port": port,
            "password": url.username(),
            "sni": query.get("sni").map(|v| v.as_ref()).unwrap_or(&server),
            "udp": true
        }),
        "vless" => {
            let mut obj = json!({
                "name": name, "type": "vless", "server": server, "port": port,
                "uuid": url.username(), "tls": query.get("security").is_some_and(|v| v == "tls" || v == "reality"),
                "servername": query.get("sni").map(|v| v.as_ref()).unwrap_or(&server), "udp": true
            });
            if let Some(alpn) = query.get("alpn") {
                let items: Vec<String> = alpn.split(',').map(str::trim).filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
                if !items.is_empty() {
                    obj["alpn"] = json!(items);
                }
            }
            obj
        }
        "hysteria2" | "hy2" => {
            let mut obj = json!({
                "name": name, "type": "hysteria2", "server": server, "port": port,
                "password": url.username(), "sni": query.get("sni").map(|v| v.as_ref()).unwrap_or(&server), "udp": true
            });
            if let Some(alpn) = query.get("alpn") {
                let items: Vec<String> = alpn.split(',').map(str::trim).filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
                if !items.is_empty() {
                    obj["alpn"] = json!(items);
                }
            }
            obj
        }
        "tuic" => {
            // tuic://uuid:password@host:port?alpn=h3&congestion_control=bbr#name
            let password = url.password().unwrap_or_default().to_string();
            let uuid = url.username().to_string();
            let alpn = query
                .get("alpn")
                .map(|v| {
                    v.split(',')
                        .map(str::trim)
                        .filter(|item| !item.is_empty())
                        .map(|item| item.to_string())
                        .collect::<Vec<_>>()
                })
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| vec!["h3".to_string()]);
            json!({
                "name": name,
                "type": "tuic",
                "server": server,
                "port": port,
                "uuid": uuid,
                "password": password,
                "alpn": alpn,
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
    // 1. 自定义环境变量覆盖
    if let Ok(value) = std::env::var("OPENHUB_MIHOMO_PATH") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
    }
    // 2. OpenHub 专属 AppData bin 目录（软件自带 / 在线下载自管理）
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
            sanitize_proxy_node_json(&mut config);
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

fn stop_child(state: &mut InstanceState) {
    stop_single_instance(state);
    state.config_hash.clear();
}

fn proxy_error_index(output: &str) -> Option<usize> {
    let marker = output.rfind("proxy ")? + "proxy ".len();
    let digits = output[marker..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
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

fn allocate_free_port() -> Result<u16, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| format!("无法分配代理端口：{error}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    drop(listener);
    Ok(port)
}

fn runtime_proxy_url(runtime: &ProxyRuntime) -> String {
    runtime
        .shared_instance
        .lock()
        .ok()
        .filter(|state| state.proxy_port > 0)
        .map(|state| format!("http://127.0.0.1:{}", state.proxy_port))
        .unwrap_or_else(|| PROXY_RUNTIME_URL.to_string())
}

fn runtime_controller_port(runtime: &ProxyRuntime) -> Result<u16, String> {
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
struct ChannelRuntimeConfig {
    channel_id: String,
    port: u16,
    node_names: Vec<String>,
}

fn runtime_config(
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

    // 1. 全局默认 Proxy Group
    proxy_groups.push(json!({
        "name": RUNTIME_GROUP,
        "type": "select",
        "proxies": all_node_names.clone()
    }));

    // 2. 为每个通道生成独立的专属 Listener 和专属 Proxy Group
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
        "dns": {
            "enable": true,
            "ipv6": false,
            "use-system-hosts": true,
            "enhanced-mode": "redir-host",
            "default-nameserver": ["8.8.8.8", "1.1.1.1"],
            "nameserver": ["8.8.8.8", "1.1.1.1", "system"]
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

fn spawn_dedicated_single_node_instance(
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

pub(crate) fn ensure_channel_instance(
    database: &Database,
    runtime: &ProxyRuntime,
    channel_id: &str,
) -> Result<u16, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
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

pub(crate) fn ensure_account_instance(
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

    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
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

pub(crate) fn ensure_shared_instance(
    database: &Database,
    runtime: &ProxyRuntime,
) -> Result<u16, String> {
    let (nodes, initial_hash) = runtime_nodes(database, None)?;
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

fn ensure_runtime(
    database: &Database,
    runtime: &ProxyRuntime,
    _only_ids: Option<&HashSet<String>>,
    _cancelled: Option<&CancellationToken>,
) -> Result<(), String> {
    ensure_shared_instance(database, runtime).map(|_| ())
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

fn simple_http_get(port: u16, path: &str, secret: &str, timeout: Duration) -> Result<(u16, String), String> {
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
    let mut response = String::new();
    stream.read_to_string(&mut response).map_err(|e| e.to_string())?;

    let mut lines = response.lines();
    let status_line = lines.next().ok_or_else(|| "空响应".to_string())?;
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("无效状态行: {status_line}"))?;

    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .or_else(|| response.split_once("\n\n").map(|(_, b)| b))
        .unwrap_or("");
    Ok((status_code, body.to_string()))
}

fn wait_runtime_ready(
    controller_port: u16,
    expected_nodes: usize,
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
        let (v_ok, v_err) = match simple_http_get(controller_port, "/version", RUNTIME_SECRET, Duration::from_millis(400)) {
            Ok((200..=299, _)) => (true, String::new()),
            Ok((code, _)) => (false, format!("Mihomo /version 返回 HTTP {code}")),
            Err(e) => (false, format!("Mihomo /version 未就绪: {e}")),
        };
        if !v_ok {
            last_error = v_err;
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }

        match simple_http_get(controller_port, "/proxies", RUNTIME_SECRET, Duration::from_millis(800)) {
            Ok((200..=299, body)) => {
                let count = serde_json::from_str::<JsonValue>(&body)
                    .ok()
                    .and_then(|value| value.get("proxies")?.as_object().map(|obj| obj.len()))
                    .unwrap_or(0);
                let min_required = if expected_nodes <= 40 {
                    expected_nodes.saturating_add(6)
                } else {
                    expected_nodes.min(40).saturating_add(6)
                };
                if expected_nodes == 0 || count >= min_required {
                    std::thread::sleep(Duration::from_millis(100));
                    return Ok(());
                }
                last_error = format!("proxies 仅 {count} 个，等待至少 {min_required} 个就绪(目标 {expected_nodes})");
            }
            Ok((code, _)) => {
                last_error = format!("读取 /proxies 失败：HTTP {code}");
            }
            Err(error) => {
                last_error = format!("读取 /proxies 失败：{error}");
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!("Mihomo 测速就绪超时：{last_error}"))
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

async fn select_group_node(runtime: &ProxyRuntime, group: &str, name: &str) -> Result<(), String> {
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

async fn select_runtime_node(runtime: &ProxyRuntime, name: &str) -> Result<(), String> {
    select_group_node(runtime, RUNTIME_GROUP, name).await
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
        if let Ok(mut state) = runtime.shared_instance.lock() {
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
    if let Ok(mut state) = runtime.shared_instance.lock() {
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
    let geoip_reader = open_geoip_reader(&runtime);
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
        node.name = existing_name
            .map(|name| clean_node_name(&name))
            .unwrap_or_else(|| unique_name(&node.name, &mut used_names));
        if let Some(object) = node.raw_json.as_object_mut() {
            object.insert("name".into(), json!(node.name));
        }
        let (country_code, country_name, classification, primary_ip) =
            classify_node_location(&node.name, &node.server, node.port, geoip_reader.as_ref());
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

/// 通道专属测速结果：只认 `channel_test_status`/`channel_latency_ms`，
/// 与代理池外面列表的普通延迟测速互不覆盖。
fn list_channel_candidate_nodes(
    database: &Database,
    max_latency_ms: i64,
) -> Result<Vec<(String, String, i64)>, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let mut statement = connection
        .prepare(
            "SELECT id, name, channel_latency_ms
             FROM proxy_pool_nodes
             WHERE channel_test_status = 'success'
               AND channel_latency_ms IS NOT NULL
               AND channel_latency_ms > 0
               AND channel_latency_ms <= ?1
             ORDER BY channel_latency_ms ASC, name COLLATE NOCASE",
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

pub(crate) fn is_http_forbidden_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("http 403")
        || lower.contains("status: 403")
        || lower.contains("status 403")
        || lower.contains("(403)")
        || lower.contains(" 403 ")
        || lower.contains("403 forbidden")
        || lower.ends_with("403")
        || lower.contains("error code: 403")
}

pub(crate) fn is_transport_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("error sending request")
        || lower.contains("i/o timeout")
        || lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("deadline")
        || lower.contains("connection")
        || lower.contains("connection reset")
        || lower.contains("connect error")
        || lower.contains("连接失败")
        || lower.contains("无法建立连接")
        || lower.contains("连接被重置")
}

fn account_proxy_failure_ttl(error: &str) -> Duration {
    let lower = error.to_ascii_lowercase();
    if is_http_forbidden_error(error) {
        ACCOUNT_PROXY_BAN_FORBIDDEN
    } else if is_transport_error(error) {
        ACCOUNT_PROXY_BAN_UNREACHABLE
    } else if lower.contains("超时") {
        ACCOUNT_PROXY_BAN_TIMEOUT
    } else {
        ACCOUNT_PROXY_BAN_DEFAULT
    }
}

pub(crate) fn read_site_uses_proxy_pool(
    database: &Database,
    site_id: &str,
) -> Result<bool, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    connection
        .query_row(
            "SELECT use_proxy_pool FROM directory_sites WHERE id = ?1",
            [site_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map(|value| value.unwrap_or(0) != 0)
        .map_err(|error| error.to_string())
}

fn read_account_proxy_channel_id(
    database: &Database,
    profile_id: &str,
) -> Result<Option<String>, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    connection
        .query_row(
            "SELECT channel_id FROM account_proxy_channels WHERE profile_id = ?1",
            [profile_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| error.to_string())
}

fn write_channel_node(database: &Database, channel_id: &str, node_id: &str) -> Result<(), String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    ensure_default_proxy_channel(&connection)?;
    connection
        .execute(
            "UPDATE proxy_channels
             SET node_id = ?2, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE id = ?1",
            params![channel_id, node_id],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn channel_candidate_nodes(
    database: &Database,
    runtime: &ProxyRuntime,
    exclude_node_id: &str,
) -> Result<Vec<(String, String, i64)>, String> {
    runtime.purge_account_bans();
    let raw = list_prioritized_fast_proxy_nodes(database, ACCOUNT_PROXY_MAX_LATENCY_MS)?;
    let filtered = raw
        .into_iter()
        .filter(|(id, _, _)| id != exclude_node_id && !runtime.account_node_is_banned(id))
        .collect::<Vec<_>>();
    if !filtered.is_empty() {
        return Ok(filtered);
    }
    let relaxed = list_prioritized_fast_proxy_nodes(database, 2000)?;
    Ok(relaxed
        .into_iter()
        .filter(|(id, _, _)| id != exclude_node_id && !runtime.account_node_is_banned(id))
        .collect())
}


pub(crate) async fn rotate_channel_instance_node(
    database: &Database,
    runtime: &ProxyRuntime,
    channel_id: &str,
    failed_node_id: &str,
    error: &str,
) -> Result<String, String> {
    runtime.account_ban_node(failed_node_id, account_proxy_failure_ttl(error));
    let candidates = channel_candidate_nodes(database, runtime, failed_node_id)?;
    let (next_id, _, _) = candidates
        .first()
        .cloned()
        .ok_or_else(|| "代理池中没有可用的候选节点".to_string())?;
    write_channel_node(database, channel_id, &next_id)?;
    let _ = ensure_channel_instance(database, runtime, channel_id);
    Ok(next_id)
}

pub(crate) async fn rotate_channel_group_node(
    database: &Database,
    runtime: &ProxyRuntime,
    channel_id: &str,
    _group_name: &str,
    failed_node_id: &str,
    error: &str,
) -> Result<String, String> {
    rotate_channel_instance_node(database, runtime, channel_id, failed_node_id, error).await
}

async fn select_next_shared_pool_node(
    database: &Database,
    runtime: &ProxyRuntime,
) -> Result<String, String> {
    let candidates = channel_candidate_nodes(database, runtime, "")?;
    if candidates.is_empty() {
        return Err("代理池中没有可用的候选节点".to_string());
    }
    let idx = runtime.shared_pool_index.fetch_add(1, Ordering::Relaxed) as usize;
    let (node_id, node_name, _) = &candidates[idx % candidates.len()];
    select_runtime_node(runtime, node_name).await?;
    Ok(node_id.clone())
}

async fn rotate_shared_pool_node(
    database: &Database,
    runtime: &ProxyRuntime,
    failed_node_id: &str,
    error: &str,
) -> Result<String, String> {
    if !failed_node_id.is_empty() {
        runtime.account_ban_node(failed_node_id, account_proxy_failure_ttl(error));
    }
    let candidates = channel_candidate_nodes(database, runtime, failed_node_id)?;
    if candidates.is_empty() {
        return Err("代理池中没有可用的候选节点进行轮换".to_string());
    }
    let idx = runtime.shared_pool_index.fetch_add(1, Ordering::Relaxed) as usize;
    let (next_id, next_name, _) = &candidates[idx % candidates.len()];
    select_runtime_node(runtime, next_name).await?;
    Ok(next_id.clone())
}

fn build_proxy_client_with_url(
    database: &Database,
    proxy_url: &str,
    timeout: Duration,
    redirects: usize,
    purpose: &str,
) -> Result<reqwest::Client, String> {
    let ignore = crate::db::read_proxy_ignore_addresses(database)?;
    let proxy = reqwest::Proxy::all(proxy_url)
        .map_err(|_| "代理池当前出口地址无效")?
        .no_proxy(reqwest::NoProxy::from_string(&ignore));
    reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::limited(redirects))
        .proxy(proxy)
        .build()
        .map_err(|error| format!("无法初始化{purpose}：{error}"))
}

fn build_channel_proxy_client_by_id(
    database: &Database,
    runtime: &ProxyRuntime,
    channel_id: &str,
    timeout: Duration,
    redirects: usize,
    purpose: &str,
) -> Result<reqwest::Client, String> {
    let proxy_url = runtime
        .channel_proxy_url(channel_id)
        .unwrap_or_else(|| runtime_proxy_url(runtime));
    build_proxy_client_with_url(database, &proxy_url, timeout, redirects, purpose)
}

fn build_shared_proxy_client(
    database: &Database,
    runtime: &ProxyRuntime,
    timeout: Duration,
    redirects: usize,
    purpose: &str,
) -> Result<reqwest::Client, String> {
    let proxy_url = runtime
        .shared_proxy_url()
        .unwrap_or_else(|| runtime_proxy_url(runtime));
    build_proxy_client_with_url(database, &proxy_url, timeout, redirects, purpose)
}

pub(crate) async fn rotate_account_instance_node(
    database: &Database,
    runtime: &ProxyRuntime,
    profile_id: &str,
    failed_node_id: &str,
    error: &str,
) -> Result<String, String> {
    if let Ok(Some(channel_id)) = read_account_proxy_channel_id(database, profile_id) {
        if !channel_id.trim().is_empty() {
            return rotate_channel_instance_node(database, runtime, &channel_id, failed_node_id, error).await;
        }
    }
    if !failed_node_id.is_empty() {
        runtime.account_ban_node(failed_node_id, account_proxy_failure_ttl(error));
    }
    let candidates = channel_candidate_nodes(database, runtime, failed_node_id)?;
    let (next_id, _, _) = candidates
        .first()
        .cloned()
        .ok_or_else(|| "代理池中没有可用的候选节点".to_string())?;

    if let Ok(mut instances) = runtime.account_instances.lock() {
        if let Some(mut inst) = instances.remove(profile_id) {
            stop_single_instance(&mut inst);
        }
    }
    let _ = ensure_account_instance(database, runtime, profile_id);
    Ok(next_id)
}

pub(crate) fn proxy_url_for_account(
    app: &tauri::AppHandle,
    site_id: &str,
    profile_id: &str,
) -> Result<Option<String>, String> {
    let database = app.state::<Database>();
    let runtime = app.state::<ProxyRuntime>();
    if !read_site_uses_proxy_pool(&database, site_id)? {
        return Ok(None);
    }
    let port = ensure_account_instance(&database, &runtime, profile_id)?;
    Ok(Some(format!("http://127.0.0.1:{port}")))
}

pub(crate) async fn with_account_proxy<T, F, Fut>(
    app: &tauri::AppHandle,
    site_id: &str,
    profile_id: &str,
    timeout: Duration,
    redirects: usize,
    purpose: &str,
    mut request: F,
) -> Result<T, String>
where
    F: FnMut(reqwest::Client) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let database = app.state::<Database>();
    let runtime = app.state::<ProxyRuntime>();
    if !read_site_uses_proxy_pool(&database, site_id)? {
        let client =
            crate::db::build_http_client_for_site(&database, site_id, timeout, redirects, purpose)?;
        return request(client).await;
    }

    // 每个账号拥有自己的独立专属进程与端口（无论是否绑定固定通道，完全零锁真并发）
    let account_port = ensure_account_instance(&database, &runtime, profile_id)?;
    let account_proxy_url = format!("http://127.0.0.1:{account_port}");
    let mut last_error = String::new();
    let mut current_failed_node: Option<String> = None;

    for attempt in 0..ACCOUNT_PROXY_MAX_ATTEMPTS {
        let client = build_proxy_client_with_url(
            &database,
            &account_proxy_url,
            timeout,
            redirects,
            purpose,
        )?;
        match request(client).await {
            Ok(value) => return Ok(value),
            Err(error) => {
                last_error = error;
                if attempt + 1 < ACCOUNT_PROXY_MAX_ATTEMPTS
                    && (is_transport_error(&last_error) || is_http_forbidden_error(&last_error))
                {
                    let failed = current_failed_node.as_deref().unwrap_or("");
                    if let Ok(next_id) = rotate_account_instance_node(
                        &database,
                        &runtime,
                        profile_id,
                        failed,
                        &last_error,
                    )
                    .await
                    {
                        current_failed_node = Some(next_id);
                        tokio::time::sleep(Duration::from_millis(60)).await;
                        continue;
                    }
                }
                return Err(last_error);
            }
        }
    }
    Err(last_error)
}

#[tauri::command]
pub fn set_proxy_pool_settings(
    database: State<'_, Database>,
    runtime: State<'_, ProxyRuntime>,
    ignore_addresses: String,
) -> Result<ProxyPoolState, String> {
    let ignore = normalize_ignore_addresses(&ignore_addresses);
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    write_meta(&connection, PROXY_IGNORE_KEY, &ignore)?;
    drop(connection);
    load_state(&database, &runtime)
}

#[tauri::command]
pub fn save_proxy_channel(
    database: State<'_, Database>,
    runtime: State<'_, ProxyRuntime>,
    id: Option<String>,
    name: String,
) -> Result<ProxyPoolState, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("请输入通道名称".into());
    }
    let channel_id = id
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| stable_id(&["proxy-channel", name]));
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    ensure_default_proxy_channel(&connection)?;
    connection
        .execute(
            "INSERT INTO proxy_channels (id, name, created_at, updated_at)
             VALUES (?1, ?2, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
             ON CONFLICT(id) DO UPDATE SET
               name = excluded.name,
               updated_at = CURRENT_TIMESTAMP",
            params![channel_id, name],
        )
        .map_err(|error| error.to_string())?;
    drop(connection);
    let _ = ensure_channel_instance(&database, &runtime, &channel_id);
    load_state(&database, &runtime)
}

#[tauri::command]
pub fn delete_proxy_channel(
    database: State<'_, Database>,
    runtime: State<'_, ProxyRuntime>,
    id: String,
) -> Result<ProxyPoolState, String> {
    let id = id.trim();
    if id == DEFAULT_PROXY_CHANNEL_ID {
        return Err("默认通道不能删除".into());
    }
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    ensure_default_proxy_channel(&connection)?;
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM proxy_channels", [], |row| row.get(0))
        .map_err(|error| error.to_string())?;
    if count <= 1 {
        return Err("至少保留一个通道".into());
    }
    connection
        .execute(
            "UPDATE account_proxy_channels SET channel_id = ?2 WHERE channel_id = ?1",
            params![id, DEFAULT_PROXY_CHANNEL_ID],
        )
        .map_err(|error| error.to_string())?;
    connection
        .execute("DELETE FROM proxy_channels WHERE id = ?1", [id])
        .map_err(|error| error.to_string())?;
    drop(connection);
    if let Ok(mut instances) = runtime.channel_instances.lock() {
        if let Some(mut inst) = instances.remove(id) {
            stop_single_instance(&mut inst);
        }
    }
    load_state(&database, &runtime)
}

#[tauri::command]
pub async fn set_proxy_channel_node(
    database: State<'_, Database>,
    runtime: State<'_, ProxyRuntime>,
    channel_id: String,
    node_id: String,
) -> Result<ProxyPoolState, String> {
    let channel_id = channel_id.trim();
    let node_id = node_id.trim();
    if channel_id.is_empty() || node_id.is_empty() {
        return Err("通道或节点标识为空".into());
    }
    write_channel_node(&database, channel_id, node_id)?;
    let _ = ensure_channel_instance(&database, &runtime, channel_id);
    load_state(&database, &runtime)
}

#[tauri::command]
pub fn assign_account_proxy_channel(
    database: State<'_, Database>,
    runtime: State<'_, ProxyRuntime>,
    profile_id: String,
    channel_id: String,
) -> Result<ProxyPoolState, String> {
    let profile_id = profile_id.trim();
    let channel_id = channel_id.trim();
    if profile_id.is_empty() || channel_id.is_empty() {
        return Err("账号或通道标识为空".into());
    }
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    ensure_default_proxy_channel(&connection)?;
    let current_channel: Option<String> = connection
        .query_row(
            "SELECT channel_id FROM account_proxy_channels WHERE profile_id = ?1",
            [profile_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    if let Some(existing) = current_channel {
        if existing != channel_id {
            return Err("该账号已归属其他通道，请先取消原通道分配".into());
        }
        drop(connection);
        return load_state(&database, &runtime);
    }
    connection
        .execute(
            "INSERT INTO account_proxy_channels (profile_id, channel_id, updated_at)
             VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
             ON CONFLICT(profile_id) DO UPDATE SET
               channel_id = excluded.channel_id,
               updated_at = excluded.updated_at",
            params![profile_id, channel_id],
        )
        .map_err(|error| error.to_string())?;
    drop(connection);
    load_state(&database, &runtime)
}

#[tauri::command]
pub fn unassign_account_proxy_channel(
    database: State<'_, Database>,
    runtime: State<'_, ProxyRuntime>,
    profile_id: String,
) -> Result<ProxyPoolState, String> {
    let profile_id = profile_id.trim();
    if profile_id.is_empty() {
        return Err("账号标识为空".into());
    }
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    connection
        .execute(
            "DELETE FROM account_proxy_channels WHERE profile_id = ?1",
            [profile_id],
        )
        .map_err(|error| error.to_string())?;
    drop(connection);
    load_state(&database, &runtime)
}

#[tauri::command]
pub async fn test_proxy_channel_nodes(
    app: AppHandle,
    database: State<'_, Database>,
    runtime: State<'_, ProxyRuntime>,
    channel_id: String,
) -> Result<ProxyPoolState, String> {
    let channel_id = channel_id.trim();
    // channel_id 仅保留在命令签名中以兼容旧调用；测速本身与通道无关。
    let _ = channel_id;
    let candidates = list_channel_candidate_nodes(&database, ACCOUNT_PROXY_MAX_LATENCY_MS)?;
    // 首次还没有通道专属结果时，用当前 ≤500ms 节点作为初始候选集。
    let candidates = if candidates.is_empty() {
        list_prioritized_fast_proxy_nodes(&database, ACCOUNT_PROXY_MAX_LATENCY_MS)?
    } else {
        candidates
    };
    if candidates.is_empty() {
        return Err(format!(
            "没有 ≤{ACCOUNT_PROXY_MAX_LATENCY_MS}ms 的候选节点，请先在节点列表完成测速"
        ));
    }
    let requested = candidates
        .into_iter()
        .map(|(id, _, _)| id)
        .collect::<HashSet<_>>();
    // 只测 ≤500ms 候选；固定地址下载 500KB，测出的耗时即“下载成功时长”。
    // 测速只刷新候选列表，不写入通道固定节点，保存时才固定。
    run_proxy_node_pool(
        &app,
        &database,
        &runtime,
        Some(requested),
        Some(CHANNEL_SPEED_TEST_URL.to_string()),
        true,
    )
    .await?;
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
    if let Ok(mut state) = runtime.shared_instance.lock() {
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
    speed_test_url_override: Option<String>,
    channel_test: bool,
) -> Result<ProxyPoolState, String> {
    // 测速策略（对齐 Clash Verge Rev DelayManager.checkListDelay）：
    // 1) 只测请求集合（选中来源/指定节点/全部）
    // 2) 待测节点装入 Mihomo 后并行 delay，不在节点之间重启内核
    // 3) 并发上限 10（与 Verge 前端 actualConcurrency 一致）
    // 4) 固定测速 URL；每条代理独立拨号计时
    // 5) 测速使用独立 Mihomo 运行时，不覆盖或重启用户的全局代理出口
    let configured = if let Some(override_url) = speed_test_url_override {
        let parsed = Url::parse(&override_url).map_err(|_| "测速地址格式无效".to_string())?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err("测速地址必须是 HTTP(S) 地址".into());
        }
        override_url
    } else {
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
    let progress_event = if channel_test {
        "proxy-channel-test-progress"
    } else {
        "proxy-node-test-progress"
    };
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
    let success_sql = if channel_test {
        "UPDATE proxy_pool_nodes SET channel_latency_ms=?2, channel_test_status='success', channel_tested_at=CURRENT_TIMESTAMP WHERE id=?1"
    } else {
        "UPDATE proxy_pool_nodes SET latency_ms=?2, test_status='success', tested_at=CURRENT_TIMESTAMP WHERE id=?1"
    };
    let error_sql = if channel_test {
        "UPDATE proxy_pool_nodes SET channel_latency_ms=NULL, channel_test_status='error', channel_tested_at=CURRENT_TIMESTAMP WHERE id=?1"
    } else {
        "UPDATE proxy_pool_nodes SET latency_ms=NULL, test_status='error', tested_at=CURRENT_TIMESTAMP WHERE id=?1"
    };
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
                tx.execute(success_sql, params![id, delay])
                    .map_err(|error| error.to_string())?;
            } else {
                tx.execute(error_sql, [&id])
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
                    progress_event,
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
                        progress_event,
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
                    progress_event,
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
                        progress_event,
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
                    progress_event,
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
                progress_event,
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
    run_proxy_node_pool(&app, &database, &runtime, None, None, false).await
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
    run_proxy_node_pool(&app, &database, &runtime, Some(requested), None, false).await
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
}
