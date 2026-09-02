use crate::models::{Database, ACTIVE_PROXY_NODE_KEY};
use crate::proxypool::runtime::{
    ensure_account_instance, ensure_default_proxy_channel, ensure_global_runtime,
    read_account_proxy_channel_id, read_meta, runtime_proxy_url, select_runtime_node,
};
use crate::proxypool::types::*;
use rusqlite::{params, OptionalExtension};
use std::time::Duration;
use tracing::warn;

pub fn list_prioritized_fast_proxy_nodes(
    database: &Database,
    max_latency_ms: i64,
) -> Result<Vec<(String, String, i64)>, String> {
    let connection = database.lock_conn()?;
    let mut statement = connection
        .prepare(
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

pub fn list_channel_candidate_nodes(
    database: &Database,
    max_latency_ms: i64,
) -> Result<Vec<(String, String, i64)>, String> {
    let connection = database.lock_conn()?;
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

pub fn channel_candidate_nodes(
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

/// 全局单实例配置装载全量节点，无需按候选子集重建配置（重建会瞬断所有出口）。
/// 仅确保实例在跑；node_ids 保留参数签名兼容既有调用方（公益抢号每轮候选）。
pub async fn prepare_proxy_nodes_transient(
    database: &Database,
    runtime: &ProxyRuntime,
    _node_ids: &[String],
) -> Result<(), String> {
    let _guard = runtime.runtime_op_lock.lock().await;
    tokio::task::block_in_place(|| ensure_global_runtime(database, runtime))?;
    Ok(())
}

pub async fn select_proxy_node_transient(
    database: &Database,
    runtime: &ProxyRuntime,
    node_id: &str,
) -> Result<(), String> {
    let _guard = runtime.runtime_op_lock.lock().await;
    if select_runtime_node(runtime, node_id).await.is_err() {
        tokio::task::block_in_place(|| ensure_global_runtime(database, runtime))?;
        select_runtime_node(runtime, node_id).await?;
    }
    Ok(())
}

pub async fn restore_proxy_node_transient(
    database: &Database,
    runtime: &ProxyRuntime,
) -> Result<(), String> {
    let active_id = read_meta(database, ACTIVE_PROXY_NODE_KEY)?;
    if active_id.trim().is_empty() {
        return Ok(());
    }
    let runtime_name = {
        let connection = database.lock_conn()?;
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
    if select_runtime_node(runtime, &runtime_name).await.is_err() {
        tokio::task::block_in_place(|| ensure_global_runtime(database, runtime))?;
        select_runtime_node(runtime, &runtime_name).await?;
    }
    Ok(())
}

pub fn runtime_proxy_url_pub(runtime: &ProxyRuntime) -> String {
    runtime_proxy_url(runtime)
}

pub fn is_http_forbidden_error(error: &str) -> bool {
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

pub fn is_transport_error(error: &str) -> bool {
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

pub fn account_proxy_failure_ttl(error: &str) -> Duration {
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

pub fn read_site_uses_proxy_pool(database: &Database, site_id: &str) -> Result<bool, String> {
    let connection = database.lock_conn()?;
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

pub fn write_channel_node(
    database: &Database,
    channel_id: &str,
    node_id: &str,
) -> Result<(), String> {
    let connection = database.lock_conn()?;
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

pub fn build_proxy_client_with_url(
    database: &Database,
    proxy_url: &str,
    timeout: Duration,
    redirects: usize,
    purpose: &str,
) -> Result<wreq::Client, String> {
    let ignore = crate::db::read_proxy_ignore_addresses(database)?;
    crate::db::build_site_http_client_with_proxy(proxy_url, &ignore, timeout, redirects, purpose)
}

pub async fn rotate_account_instance_node(
    database: &Database,
    runtime: &ProxyRuntime,
    profile_id: &str,
    failed_node_id: &str,
    error: &str,
) -> Result<String, String> {
    // 固定通道不参与任何自动轮换：出口节点由用户固定，失败就失败。
    if let Ok(Some(channel_id)) = read_account_proxy_channel_id(database, profile_id) {
        if !channel_id.trim().is_empty() {
            return Err("账号已绑定固定通道，出口节点不自动轮换".to_string());
        }
    }
    // 调用方未告知失败节点时，取 lane 当前选中节点参与排除与封禁，
    // 避免轮换后又选中同一个节点。
    let current_node = if !failed_node_id.is_empty() {
        failed_node_id.to_string()
    } else {
        runtime
            .account_lane_current_node(profile_id)
            .unwrap_or_default()
    };
    if !current_node.is_empty() {
        runtime.account_ban_node(&current_node, account_proxy_failure_ttl(error));
    }
    let candidates = channel_candidate_nodes(database, runtime, &current_node)?;
    // 轮换也逐个验活：下一个候选本身可能已经是死节点（出口 IP 风控、仅在刚启动时
    // 曾被验证过、节点网络抖动），直接盲切换极易落到新死节点上。
    let mut next_id = String::new();
    for (candidate_id, _, _) in &candidates {
        if crate::proxypool::runtime::probe_node_alive_pub(runtime, candidate_id) {
            next_id = candidate_id.clone();
            break;
        }
        warn!("轮换候选节点探活失败 {candidate_id}");
        runtime.account_ban_node(
            candidate_id,
            crate::proxypool::types::ACCOUNT_NODE_PROBE_FAIL_TTL,
        );
    }
    if next_id.is_empty() {
        return Err("代理池所有候选节点均不可用".to_string());
    }
    // lane 化后轮换只切组，不再杀账号实例；出口端口保持不变，
    // 既有 client/URL 继续有效。
    tokio::task::block_in_place(|| {
        ensure_account_instance(database, runtime, profile_id, Some(&next_id))
    })?;
    Ok(next_id)
}

pub fn proxy_url_for_account(
    database: &Database,
    runtime: &ProxyRuntime,
    site_id: &str,
    profile_id: &str,
) -> Result<Option<String>, String> {
    if !read_site_uses_proxy_pool(database, site_id)? {
        return Ok(None);
    }
    let port =
        crate::proxypool::runtime::ensure_account_instance(database, runtime, profile_id, None)?;
    Ok(Some(format!("http://127.0.0.1:{port}")))
}

pub async fn with_account_proxy<T, F, Fut>(
    database: &Database,
    runtime: &ProxyRuntime,
    site_id: &str,
    profile_id: &str,
    timeout: Duration,
    redirects: usize,
    purpose: &str,
    mut request: F,
) -> Result<T, String>
where
    F: FnMut(wreq::Client) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    if !read_site_uses_proxy_pool(database, site_id)? {
        let client = crate::db::build_site_http_client(database, timeout, redirects, purpose)?;
        return request(client).await;
    }

    let account_port =
        crate::proxypool::runtime::ensure_account_instance(database, runtime, profile_id, None)?;
    // lane 端口在轮换时保持不变（切组而非杀进程），重试可继续复用同一 URL
    let account_proxy_url = format!("http://127.0.0.1:{account_port}");
    // 固定通道语义：账号绑定通道后，出口节点由用户手动固定。请求失败就失败，
    // 不重试、不拉黑节点、不改写通道节点——否则“固定出口”就失去意义
    // （且 cf_clearance 绑定出口 IP，自动换节点只会让浏览器里的验证作废）。
    if read_account_proxy_channel_id(database, profile_id)
        .ok()
        .flatten()
        .is_some_and(|channel_id| !channel_id.trim().is_empty())
    {
        let client =
            build_proxy_client_with_url(database, &account_proxy_url, timeout, redirects, purpose)?;
        return request(client).await;
    }

    let mut last_error = String::new();
    let mut current_failed_node: Option<String> = None;

    for attempt in 0..ACCOUNT_PROXY_MAX_ATTEMPTS {
        let client =
            build_proxy_client_with_url(database, &account_proxy_url, timeout, redirects, purpose)?;
        match request(client).await {
            Ok(result) => return Ok(result),
            Err(error) => {
                let should_retry = attempt + 1 < ACCOUNT_PROXY_MAX_ATTEMPTS
                    && (is_http_forbidden_error(&error) || is_transport_error(&error));
                last_error = error.clone();
                if should_retry {
                    let failed_node = current_failed_node.as_deref().unwrap_or("");
                    match rotate_account_instance_node(
                        database,
                        runtime,
                        profile_id,
                        failed_node,
                        &error,
                    )
                    .await
                    {
                        Ok(new_node_id) => {
                            current_failed_node = Some(new_node_id);
                            tokio::time::sleep(Duration::from_millis(300)).await;
                            continue;
                        }
                        Err(rotate_err) => {
                            warn!("账号 {profile_id} 代理节点切换失败: {rotate_err}");
                        }
                    }
                }
                break;
            }
        }
    }
    Err(last_error)
}
