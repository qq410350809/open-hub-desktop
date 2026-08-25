//! Clash 订阅导出：把代理池里测速达标的节点打包成 Clash/Mihomo 订阅。
//!
//! 数据流：
//! - 节点来源：SQLite `proxy_pool_nodes`（订阅解析后的 `raw_json` 本身就是 Clash 代理配置）；
//! - 达标条件：`test_status = 'success'` 且 `latency_ms <= max_latency_ms`（默认 1000ms）；
//! - 输出：完整 Clash YAML（proxies + 策略组 + 基础分流规则），由内嵌 HTTP 服务以
//!   `/api/proxy-pool/clash-sub?token=...` 形式暴露，Clash 客户端可直接订阅并随测速自动更新。

use crate::core::db::{read_meta, write_meta};
use crate::models::Database;
use serde::Serialize;
use serde_json::{json, Value as JsonValue};
use std::collections::{HashMap, HashSet};

/// 订阅令牌在 app_meta 中的存储键。
pub const CLASH_SUB_TOKEN_KEY: &str = "proxy_clash_sub_token";
/// 默认延迟阈值：≤1000ms 的节点才进入订阅。
pub const DEFAULT_CLASH_SUB_MAX_LATENCY_MS: i64 = 1000;
/// 订阅内策略组名称（与 rules 中的引用保持一致）。
const SELECT_GROUP: &str = "🚀 节点选择";
const AUTO_GROUP: &str = "⚡ 自动选择";
const AUTO_SPEED_TEST_URL: &str = "http://www.gstatic.com/generate_204";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClashSubscriptionInfo {
    pub token: String,
    pub port: u16,
    pub url: String,
    /// 默认阈值下测速达标、可进入订阅的节点数。
    pub eligible_count: usize,
    /// 代理池内有效节点总数（不含 invalid）。
    pub total_count: usize,
    pub max_latency_ms: i64,
}

/// 订阅导出用的节点视图。
pub struct ExportNode {
    pub name: String,
    #[allow(dead_code)]
    pub latency_ms: i64,
    pub country_code: String,
    pub country_name: String,
    pub config: JsonValue,
}

/// 地区分组视图：名称形如「🇭🇰 香港 · 12」，成员为该地区全部达标节点。
struct RegionGroup {
    name: String,
    node_names: Vec<String>,
}

fn generate_token() -> Result<String, String> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|error| format!("生成订阅令牌失败：{error}"))?;
    Ok(hex::encode(bytes))
}

/// 读取（或首次生成）订阅令牌：持久化到 app_meta，保证订阅 URL 长期稳定。
pub fn subscription_token(database: &Database) -> Result<String, String> {
    let stored = read_meta(database, CLASH_SUB_TOKEN_KEY)?;
    if !stored.trim().is_empty() {
        return Ok(stored);
    }
    let token = generate_token()?;
    {
        let connection = database.lock_conn()?;
        write_meta(&connection, CLASH_SUB_TOKEN_KEY, &token)?;
    }
    Ok(token)
}

/// 重置订阅令牌：旧订阅链接立即失效。
pub fn reset_subscription_token(database: &Database) -> Result<String, String> {
    let token = generate_token()?;
    {
        let connection = database.lock_conn()?;
        write_meta(&connection, CLASH_SUB_TOKEN_KEY, &token)?;
    }
    Ok(token)
}

/// 校验订阅请求携带的令牌。
pub fn verify_subscription_token(database: &Database, token: &str) -> bool {
    if token.trim().is_empty() {
        return false;
    }
    read_meta(database, CLASH_SUB_TOKEN_KEY)
        .map(|stored| !stored.is_empty() && stored == token)
        .unwrap_or(false)
}

/// 订阅访问路径（不含 host/port，供 HTTP 层挂载时对齐）。
pub const CLASH_SUB_PATH: &str = "/api/proxy-pool/clash-sub";

/// 拼接完整订阅 URL。
pub fn subscription_url(port: u16, token: &str) -> String {
    format!("http://127.0.0.1:{port}{CLASH_SUB_PATH}?token={token}")
}

/// 汇总订阅信息（用当前内嵌 HTTP 服务端口拼出完整 URL）。
pub fn clash_subscription_info(
    database: &Database,
    port: u16,
) -> Result<ClashSubscriptionInfo, String> {
    let token = subscription_token(database)?;
    Ok(ClashSubscriptionInfo {
        url: subscription_url(port, &token),
        eligible_count: count_eligible_nodes(database, DEFAULT_CLASH_SUB_MAX_LATENCY_MS)?,
        total_count: count_active_nodes(database)?,
        token,
        port,
        max_latency_ms: DEFAULT_CLASH_SUB_MAX_LATENCY_MS,
    })
}

/// 重置令牌后重新汇总订阅信息。
pub fn regenerate_clash_subscription_info(
    database: &Database,
    port: u16,
) -> Result<ClashSubscriptionInfo, String> {
    reset_subscription_token(database)?;
    clash_subscription_info(database, port)
}

fn count_eligible_nodes(database: &Database, max_latency_ms: i64) -> Result<usize, String> {
    let connection = database.lock_conn()?;
    connection
        .query_row(
            "SELECT COUNT(*) FROM proxy_pool_nodes
             WHERE is_enabled = 1 AND test_status = 'success'
               AND latency_ms IS NOT NULL AND latency_ms <= ?1",
            [max_latency_ms],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count as usize)
        .map_err(|error| error.to_string())
}

fn count_active_nodes(database: &Database) -> Result<usize, String> {
    let connection = database.lock_conn()?;
    connection
        .query_row(
            "SELECT COUNT(*) FROM proxy_pool_nodes WHERE test_status != 'invalid'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count as usize)
        .map_err(|error| error.to_string())
}

/// 收集测速达标的启用节点，按延迟升序；导出名追加实时延迟便于客户端直读快慢。
fn collect_export_nodes(
    database: &Database,
    max_latency_ms: i64,
) -> Result<Vec<ExportNode>, String> {
    let connection = database.lock_conn()?;
    let mut statement = connection
        .prepare(
            "SELECT name, latency_ms, COALESCE(country_code, ''), COALESCE(country_name, ''), raw_json
             FROM proxy_pool_nodes
             WHERE is_enabled = 1 AND test_status = 'success'
               AND latency_ms IS NOT NULL AND latency_ms <= ?1
             ORDER BY latency_ms ASC, name COLLATE NOCASE",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([max_latency_ms], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;

    // 订阅是每次拉取即时生成的，延迟后缀始终反映最近一次测速结果。
    let mut used_names: HashSet<String> = HashSet::new();
    let mut nodes = Vec::with_capacity(rows.len());
    for (name, latency_ms, country_code, country_name, raw_json) in rows {
        let Ok(mut config) = serde_json::from_str::<JsonValue>(&raw_json) else {
            continue;
        };
        let Some(object) = config.as_object_mut() else {
            continue;
        };
        let display = super::parser::unique_name(&format!("{name} · {latency_ms}ms"), &mut used_names);
        object.insert("name".to_string(), json!(display));
        nodes.push(ExportNode {
            name: display,
            latency_ms,
            country_code,
            country_name,
            config,
        });
    }
    Ok(nodes)
}

/// 两位国家码转旗帜 emoji（如 HK → 🇭🇰）；非法码返回空串。
fn flag_emoji(code: &str) -> String {
    let upper = code.trim().to_ascii_uppercase();
    if upper.len() != 2 || !upper.chars().all(|c| c.is_ascii_alphabetic()) {
        return String::new();
    }
    upper
        .chars()
        .filter_map(|c| char::from_u32(0x1F1E6 + (c as u32 - 'A' as u32)))
        .collect()
}

/// 地区分组显示名：优先国旗 + 中文名，未解析地区归入「其他地区」。
fn region_group_name(code: &str, name: &str, count: usize) -> String {
    let normalized = code.trim().to_ascii_uppercase();
    let (flag, label) = match normalized.as_str() {
        "LOCAL" => ("🏠".to_string(), "本地节点".to_string()),
        code if code.len() == 2 && code.chars().all(|c| c.is_ascii_alphabetic()) => (
            flag_emoji(code),
            if name.trim().is_empty() {
                code.to_string()
            } else {
                name.trim().to_string()
            },
        ),
        _ => ("🌐".to_string(), "其他地区".to_string()),
    };
    if flag.is_empty() {
        format!("{label} · {count}")
    } else {
        format!("{flag} {label} · {count}")
    }
}

/// 按国家/地区聚合节点。入参已按延迟升序，因此分组顺序即「最快节点所在地区优先」。
fn build_region_groups(nodes: &[ExportNode]) -> Vec<RegionGroup> {
    struct Accumulator {
        code: String,
        name: String,
        node_names: Vec<String>,
    }
    let mut order: Vec<Accumulator> = Vec::new();
    let mut index_by_key: HashMap<String, usize> = HashMap::new();
    for node in nodes {
        let raw_code = node.country_code.trim().to_ascii_uppercase();
        // 未知地区（空码 / ZZ / 非法码）统一归并为一组，避免出现重复组名。
        let key = if raw_code == "LOCAL"
            || (raw_code.len() == 2 && raw_code.chars().all(|c| c.is_ascii_alphabetic()))
        {
            raw_code
        } else {
            "__OTHER__".to_string()
        };
        let entry = match index_by_key.get(&key) {
            Some(&index) => &mut order[index],
            None => {
                index_by_key.insert(key, order.len());
                order.push(Accumulator {
                    code: node.country_code.trim().to_string(),
                    name: node.country_name.trim().to_string(),
                    node_names: Vec::new(),
                });
                order.last_mut().expect("刚 push 过，必然存在")
            }
        };
        entry.node_names.push(node.name.clone());
    }
    order
        .into_iter()
        .map(|item| RegionGroup {
            name: region_group_name(&item.code, &item.name, item.node_names.len()),
            node_names: item.node_names,
        })
        .collect()
}

/// 基础通用配置：混合端口、DNS 与日志级别，保持最小可运行集。
fn base_config() -> JsonValue {
    json!({
        "mixed-port": 7890,
        "allow-lan": false,
        "mode": "rule",
        "log-level": "info",
        "unified-delay": true,
        "tcp-concurrent": true,
        "dns": {
            "enable": true,
            "ipv6": false,
            "enhanced-mode": "fake-ip",
            "fake-ip-range": "198.18.0.1/16",
            "fake-ip-filter": ["*.lan", "*.local", "*.localdomain", "+.market.xiaomi.com"],
            "default-nameserver": ["223.5.5.5", "119.29.29.29"],
            "nameserver": ["https://223.5.5.5/dns-query", "https://doh.pub/dns-query"],
        },
    })
}

/// 生成 Clash/Mihomo 订阅 YAML，返回 (yaml, 节点数)。
///
/// 结构：
/// - `proxies`：达标节点的原始 Clash 配置（名称统一加延迟后缀并去重）；
/// - `proxy-groups`：
///   - 「🚀 节点选择」手动组：可切「⚡ 自动选择」、各地区组或 DIRECT；
///   - 「⚡ 自动选择」全局 url-test：全部达标节点自动选最快；
///   - 各国家/地区组（如「🇭🇰 香港 · 12」）：组内 url-test 自动选该地区最快节点，
///     分组顺序按各地区最快节点延迟升序；
/// - `rules`：局域网与国内直连，其余走节点选择。
pub fn build_clash_subscription_yaml(
    database: &Database,
    max_latency_ms: i64,
) -> Result<(String, usize), String> {
    let nodes = collect_export_nodes(database, max_latency_ms)?;
    let count = nodes.len();
    let mut root = base_config();

    if count > 0 {
        let names: Vec<String> = nodes.iter().map(|node| node.name.clone()).collect();
        let configs: Vec<JsonValue> = nodes.iter().map(|node| node.config.clone()).collect();
        let regions = build_region_groups(&nodes);

        // 手动选择组：自动选择 → 各地区组 → DIRECT。
        let mut select_proxies = vec![json!(AUTO_GROUP)];
        select_proxies.extend(regions.iter().map(|region| json!(region.name)));
        select_proxies.push(json!("DIRECT"));

        let mut groups = vec![
            json!({
                "name": SELECT_GROUP,
                "type": "select",
                "proxies": select_proxies,
            }),
            json!({
                "name": AUTO_GROUP,
                "type": "url-test",
                "url": AUTO_SPEED_TEST_URL,
                "interval": 300,
                "tolerance": 50,
                "lazy": true,
                "proxies": names,
            }),
        ];
        for region in regions {
            groups.push(json!({
                "name": region.name,
                "type": "url-test",
                "url": AUTO_SPEED_TEST_URL,
                "interval": 300,
                "tolerance": 50,
                "lazy": true,
                "proxies": region.node_names,
            }));
        }

        root["proxies"] = json!(configs);
        root["proxy-groups"] = json!(groups);
        root["rules"] = json!([
            "IP-CIDR,127.0.0.0/8,DIRECT,no-resolve",
            "IP-CIDR,10.0.0.0/8,DIRECT,no-resolve",
            "IP-CIDR,172.16.0.0/12,DIRECT,no-resolve",
            "IP-CIDR,192.168.0.0/16,DIRECT,no-resolve",
            "IP-CIDR6,::1/128,DIRECT,no-resolve",
            "GEOIP,CN,DIRECT",
            format!("MATCH,{SELECT_GROUP}"),
        ]);
    }

    let yaml = serde_yaml::to_string(&root).map_err(|error| error.to_string())?;
    if count == 0 {
        return Ok((
            format!(
                "# OpenHub Clash 订阅\n# 暂无延迟 ≤ {max_latency_ms}ms 的达标节点，请先在代理池批量测速\n{yaml}"
            ),
            0,
        ));
    }
    Ok((yaml, count))
}
