//! 出口类型信息是独立、尽力而为的元数据，绝不参与测速成功判定。
use crate::models::{Database, ProxyIpInfo};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::net::IpAddr;
use std::sync::{Arc, TryLockError};
use std::time::Duration;
use tokio::sync::{Mutex, Semaphore};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const SOURCE: &str = "ip-api.com";
const API_URL: &str = "http://ip-api.com/json/";
const BODY_LIMIT: usize = 64 * 1024;
const SUCCESS_TTL: i64 = 24 * 60 * 60;
const ERROR_TTL: i64 = 5 * 60;
const LOOKUP_BUDGET: Duration = Duration::from_secs(4);
const START_INTERVAL: Duration = Duration::from_millis(300);
const BUDGET_ERROR: &str = "出口 IP 类型查询超时或服务繁忙，请稍后重试";
/// 免费接口单次最多返回的字段：hosting/mobile 是我们分类的唯一依据。
const FIELDS: &str = "status,message,query,isp,org,as,hosting,mobile,proxy";

/// 统一 IPv6 压缩形式和 IPv4-mapped IPv6，缓存键与响应比对使用同一规则。
pub(crate) fn normalize_ip(value: &str) -> Option<String> {
    let ip = value.trim().parse::<IpAddr>().ok()?;
    Some(match ip {
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(|v4| v4.to_string())
            .unwrap_or_else(|| ip.to_string()),
        IpAddr::V4(ip) => ip.to_string(),
    })
}

/// 保守排除特殊用途、文档、基准测试、组播、保留和过渡地址，不把它们发给第三方。
pub(crate) fn public_ip(value: &str) -> Option<String> {
    let normalized = normalize_ip(value)?;
    let public = match normalized.parse::<IpAddr>().ok()? {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 192 && b == 0 && (c == 0 || c == 2))
                || (a == 192 && b == 88 && c == 99)
                || (a == 198 && (b == 18 || b == 19))
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            s[0] & 0xe000 == 0x2000
                && !(s[0] == 0x2001 && s[1] < 0x0200)
                && !(s[0] == 0x2001 && s[1] == 0x0db8)
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    };
    public.then_some(normalized)
}

fn error_info(ip: &str, error: &str) -> ProxyIpInfo {
    ProxyIpInfo {
        ip: ip.to_string(),
        kind: "unknown".into(),
        source: SOURCE.into(),
        checked_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        status: "error".into(),
        error: error.into(),
        ..Default::default()
    }
}

fn label(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(160)
        .collect()
}

/// ip-api.com 的 `as` 字段形如 `AS15169 Google LLC`，只取前面的 AS 号。
fn asn_number(text: &str) -> String {
    let digits = text
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_start_matches("AS")
        .trim_start_matches("as")
        .trim();
    match digits.parse::<u32>() {
        Ok(number) if number > 0 => format!("AS{number}"),
        _ => String::new(),
    }
}

/// 只信 API 显式给出的 hosting/mobile 布尔量，不按运营商名称猜住宅。
/// 两个字段都在免费响应里必定存在，缺失说明响应异常，按失败处理。
fn parse_response(ip: &str, body: &[u8]) -> ProxyIpInfo {
    let invalid = || error_info(ip, "类型服务响应无效或缺少必要字段");
    if body.len() > BODY_LIMIT {
        return error_info(ip, "类型服务响应超过安全大小限制");
    }
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return invalid();
    };
    if !value.is_object() {
        return invalid();
    }
    if value.get("status").and_then(Value::as_str) != Some("success") {
        return error_info(ip, "类型服务返回错误或配额已用尽，请稍后重试");
    }
    let response_ip = value
        .get("query")
        .and_then(Value::as_str)
        .and_then(normalize_ip);
    if response_ip.is_none() || response_ip.as_deref() != normalize_ip(ip).as_deref() {
        return error_info(ip, "类型服务返回的 IP 与本轮出口不一致");
    }
    let (Some(hosting), Some(mobile)) = (
        value.get("hosting").and_then(Value::as_bool),
        value.get("mobile").and_then(Value::as_bool),
    ) else {
        return invalid();
    };
    // 既非机房也非移动网络：免费接口不区分住宅与企业/校园线路，
    // 因此按“疑似家宽”呈现，由前端文案说明其不确定性。
    let kind = if hosting {
        "hosting"
    } else if mobile {
        "mobile"
    } else {
        "residential"
    };
    let isp = label(value.get("isp"));
    let organization = label(value.get("org"));
    ProxyIpInfo {
        ip: response_ip.unwrap(),
        kind: kind.into(),
        isp: if isp.is_empty() { organization.clone() } else { isp },
        organization,
        asn: asn_number(&label(value.get("as"))),
        source: SOURCE.into(),
        checked_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        status: "success".into(),
        error: String::new(),
    }
}

fn expires_at(info: &ProxyIpInfo) -> Option<i64> {
    let ttl = match info.status.as_str() {
        "success" => SUCCESS_TTL,
        "error" => ERROR_TTL,
        _ => return None,
    };
    DateTime::parse_from_rfc3339(&info.checked_at)
        .ok()?
        .timestamp()
        .checked_add(ttl)
}

fn is_fresh(info: &ProxyIpInfo, ip: &str, now: i64) -> bool {
    let Some(expires) = expires_at(info) else {
        return false;
    };
    let ttl = if info.status == "success" {
        SUCCESS_TTL
    } else {
        ERROR_TTL
    };
    info.source == SOURCE
        && matches!(
            info.kind.as_str(),
            "hosting" | "residential" | "mobile" | "unknown"
        )
        && (info.status != "error" || info.kind == "unknown")
        && (info.status != "success" || info.error.is_empty())
        && public_ip(ip).is_some()
        && normalize_ip(&info.ip) == normalize_ip(ip)
        && now >= expires - ttl
        && now < expires
}

/// 损坏、过期、未来时间、来源不符或属于旧出口的数据都不展示。
pub(crate) fn parse_stored_info(json: &str, primary_ip: &str) -> Option<ProxyIpInfo> {
    if json.len() > BODY_LIMIT {
        return None;
    }
    let info: ProxyIpInfo = serde_json::from_str(json).ok()?;
    is_fresh(&info, primary_ip, Utc::now().timestamp()).then_some(info)
}

fn read_cache(connection: &Connection, ip: &str) -> Option<ProxyIpInfo> {
    let (json, source, checked_at, expires): (String, String, String, i64) = connection
        .query_row(
            "SELECT info_json, source, checked_at, expires_at FROM proxy_ip_info_cache
             WHERE ip=?1 AND expires_at>?2",
            params![ip, Utc::now().timestamp()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .ok()??;
    let info = parse_stored_info(&json, ip)?;
    (source == info.source && checked_at == info.checked_at && Some(expires) == expires_at(&info))
        .then_some(info)
}

fn write_cache(connection: &Connection, info: &ProxyIpInfo) -> rusqlite::Result<usize> {
    connection.execute(
        "INSERT INTO proxy_ip_info_cache (ip, info_json, source, checked_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(ip) DO UPDATE SET info_json=excluded.info_json, source=excluded.source,
             checked_at=excluded.checked_at, expires_at=excluded.expires_at",
        params![
            info.ip,
            serde_json::to_string(info).unwrap_or_default(),
            info.source,
            info.checked_at,
            expires_at(info).unwrap_or(0)
        ],
    )
}

/// 不在 Tokio worker 上阻塞等数据库 Mutex；SQLite 的 busy 等待也不占查询预算。
/// 调用者以 select/timeout 包围此 future，取消会直接丢弃等待，不产生后台任务。
async fn with_cache_connection<T>(
    database: &Database,
    operation: impl FnOnce(&Connection) -> T,
) -> Option<T> {
    loop {
        {
            match database.0.try_lock() {
                Ok(connection) => {
                    connection.busy_timeout(Duration::ZERO).ok()?;
                    let result = operation(&connection);
                    let _ = connection.busy_timeout(Duration::from_secs(5));
                    return Some(result);
                }
                Err(TryLockError::Poisoned(_)) => return None,
                Err(TryLockError::WouldBlock) => {}
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn query_api(ip: &str, proxy_url: &str) -> ProxyIpInfo {
    let request = async {
        // 只接受调用方给定的 lane 代理；不使用系统代理、重定向或直连降级。
        let proxy = reqwest::Proxy::all(proxy_url).map_err(|_| "出口类型查询代理配置无效")?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .proxy(proxy)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(LOOKUP_BUDGET)
            .timeout(LOOKUP_BUDGET)
            .pool_max_idle_per_host(0)
            .build()
            .map_err(|_| "无法初始化出口类型查询客户端")?;
        // ip-api.com 的免费接口：路径里带 IP，用 fields 限制返回字段以获得 hosting/mobile。
        let mut endpoint =
            url::Url::parse(&format!("{API_URL}{ip}")).map_err(|_| "类型服务地址无效")?;
        endpoint.query_pairs_mut().append_pair("fields", FIELDS);
        let mut response = client
            .get(endpoint)
            .send()
            .await
            .map_err(|_| "经当前节点查询出口类型失败（网络错误）")?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err("类型服务配额已用尽，请稍后重试");
        }
        if !response.status().is_success() {
            return Err("类型服务暂不可用或返回了重定向");
        }
        if response
            .content_length()
            .is_some_and(|size| size > BODY_LIMIT as u64)
        {
            return Err("类型服务响应超过安全大小限制");
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "读取类型服务响应失败")?
        {
            if chunk.len() > BODY_LIMIT - body.len() {
                return Err("类型服务响应超过安全大小限制");
            }
            body.extend_from_slice(&chunk);
        }
        Ok(parse_response(ip, &body))
    };
    match tokio::time::timeout(LOOKUP_BUDGET, request).await {
        Ok(Ok(info)) => info,
        Ok(Err(error)) => error_info(ip, error),
        Err(_) => error_info(ip, BUDGET_ERROR),
    }
}

/// 单个 batch 共享：每 IP 一个带结果的互斥槽（即使 SQLite 暂不可写也去重），
/// 全部类型请求另限 2 并发、起始间隔 300ms，不占测速的并发槽/指标。
pub(crate) struct IpInfoBatch {
    flights: Mutex<HashMap<String, Arc<Mutex<Option<ProxyIpInfo>>>>>,
    permits: Semaphore,
    next_start: Mutex<Instant>,
}

impl IpInfoBatch {
    pub(crate) fn new() -> Self {
        Self {
            flights: Mutex::new(HashMap::new()),
            permits: Semaphore::new(2),
            next_start: Mutex::new(Instant::now()),
        }
    }

    pub(crate) async fn lookup(
        &self,
        database: &Database,
        exit_ip: &str,
        proxy_url: &str,
        cancellation: &CancellationToken,
    ) -> Option<ProxyIpInfo> {
        self.lookup_with(database, exit_ip, cancellation, |ip| async move {
            query_api(&ip, proxy_url).await
        })
        .await
    }

    async fn lookup_with<F, Fut>(
        &self,
        database: &Database,
        exit_ip: &str,
        cancellation: &CancellationToken,
        fetch: F,
    ) -> Option<ProxyIpInfo>
    where
        F: FnOnce(String) -> Fut,
        Fut: Future<Output = ProxyIpInfo>,
    {
        let ip = public_ip(exit_ip)?;
        let deadline = Instant::now() + LOOKUP_BUDGET;
        let lookup = async {
            let slot = self
                .flights
                .lock()
                .await
                .entry(ip.clone())
                .or_default()
                .clone();
            let mut result = slot.lock().await;
            if let Some(info) = result
                .as_ref()
                .filter(|info| is_fresh(info, &ip, Utc::now().timestamp()))
            {
                return info.clone();
            }
            // 为缓存写入预留 100ms；等 IP 锁、数据库、并发槽和节流都计入总预算。
            let query = async {
                if let Some(info) = with_cache_connection(database, |conn| read_cache(conn, &ip))
                    .await
                    .flatten()
                {
                    return info;
                }
                let _permit = self
                    .permits
                    .acquire()
                    .await
                    .expect("batch semaphore is never closed");
                {
                    let mut next = self.next_start.lock().await;
                    tokio::time::sleep_until(*next).await;
                    *next = Instant::now() + START_INTERVAL;
                }
                fetch(ip.clone()).await
            };
            let info = tokio::time::timeout_at(deadline - Duration::from_millis(100), query)
                .await
                .unwrap_or_else(|_| error_info(&ip, BUDGET_ERROR));
            *result = Some(info.clone());
            let _ = tokio::time::timeout(
                Duration::from_millis(50),
                with_cache_connection(database, |conn| write_cache(conn, &info)),
            )
            .await;
            info
        };
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => None,
            result = tokio::time::timeout_at(deadline, lookup) => {
                (!cancellation.is_cancelled()).then(|| result.unwrap_or_else(|_| error_info(&ip, BUDGET_ERROR)))
            }
        }
    }
}

/// IP 条件防止旧结果覆盖已切换出口的节点；只更新元数据，不动测速列。
/// Some(false) 表示已确认节点/出口不匹配；None 是数据库暂不可写或取消。
/// 暂时写库失败不应吞掉前端元数据事件（调用方另行检查取消）。
pub(crate) async fn persist_node_info(
    database: &Database,
    node_id: &str,
    info: &ProxyIpInfo,
    cancellation: &CancellationToken,
) -> Option<bool> {
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => None,
        result = tokio::time::timeout(Duration::from_secs(2), with_cache_connection(database, |conn| {
            conn.execute(
                "UPDATE proxy_pool_nodes SET ip_info_json=?3 WHERE id=?1 AND primary_ip=?2",
                params![node_id, info.ip, serde_json::to_string(info).unwrap_or_default()],
            ).ok().map(|count| count == 1)
        })) => result.ok().flatten().flatten(),
    }
}

#[cfg(test)]
#[path = "tests/ip_info.rs"]
mod tests;
