use crate::models::{Database, ACTIVE_PROXY_NODE_KEY};
use crate::proxypool::runtime::{
    ensure_channel_instance, ensure_default_proxy_channel, ensure_runtime, read_account_proxy_channel_id,
    read_meta, runtime_proxy_url, select_runtime_node,
};
use crate::proxypool::types::*;
use rusqlite::{params, OptionalExtension};
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::Manager;

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

pub async fn prepare_proxy_nodes_transient(
    database: &Database,
    runtime: &ProxyRuntime,
    node_ids: &[String],
) -> Result<(), String> {
    let _guard = runtime.runtime_op_lock.lock().await;
    let only = node_ids.iter().cloned().collect::<HashSet<_>>();
    tokio::task::block_in_place(|| ensure_runtime(database, runtime, Some(&only), None))?;
    Ok(())
}

pub async fn select_proxy_node_transient(
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
        let only = HashSet::from([runtime_name.clone()]);
        tokio::task::block_in_place(|| ensure_runtime(database, runtime, Some(&only), None))?;
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

pub fn read_site_uses_proxy_pool(
    database: &Database,
    site_id: &str,
) -> Result<bool, String> {
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

pub fn write_channel_node(database: &Database, channel_id: &str, node_id: &str) -> Result<(), String> {
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

pub async fn rotate_channel_instance_node(
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

#[allow(dead_code)]
pub async fn rotate_channel_group_node(
    database: &Database,
    runtime: &ProxyRuntime,
    channel_id: &str,
    _group_name: &str,
    failed_node_id: &str,
    error: &str,
) -> Result<String, String> {
    rotate_channel_instance_node(database, runtime, channel_id, failed_node_id, error).await
}

#[allow(dead_code)]
pub async fn select_next_shared_pool_node(
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

#[allow(dead_code)]
pub async fn rotate_shared_pool_node(
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

pub fn build_proxy_client_with_url(
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

#[allow(dead_code)]
pub fn build_channel_proxy_client_by_id(
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

#[allow(dead_code)]
pub fn build_shared_proxy_client(
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

pub async fn rotate_account_instance_node(
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
    let _ = crate::proxypool::runtime::ensure_account_instance(database, runtime, profile_id);
    Ok(next_id)
}

pub fn proxy_url_for_account(
    app: &tauri::AppHandle,
    site_id: &str,
    profile_id: &str,
) -> Result<Option<String>, String> {
    let database = app.state::<Database>();
    let runtime = app.state::<ProxyRuntime>();
    if !read_site_uses_proxy_pool(&database, site_id)? {
        return Ok(None);
    }
    let port = crate::proxypool::runtime::ensure_account_instance(&database, &runtime, profile_id)?;
    Ok(Some(format!("http://127.0.0.1:{port}")))
}

pub async fn with_account_proxy<T, F, Fut>(
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

    let account_port = crate::proxypool::runtime::ensure_account_instance(&database, &runtime, profile_id)?;
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
            Ok(result) => return Ok(result),
            Err(error) => {
                let should_retry = attempt + 1 < ACCOUNT_PROXY_MAX_ATTEMPTS
                    && (is_http_forbidden_error(&error) || is_transport_error(&error));
                last_error = error.clone();
                if should_retry {
                    let failed_node = current_failed_node.as_deref().unwrap_or("");
                    match rotate_account_instance_node(
                        &database,
                        &runtime,
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
                            eprintln!("账号 {profile_id} 代理节点切换失败: {rotate_err}");
                        }
                    }
                }
                break;
            }
        }
    }
    Err(last_error)
}
