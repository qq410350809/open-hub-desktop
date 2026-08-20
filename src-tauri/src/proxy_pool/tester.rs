use crate::models::*;
use crate::proxy_pool::runtime::{
    append_controller_path, controller_client, controller_url, ensure_runtime,
    is_slow_or_blocked_speed_test_url, load_state, runtime_controller_port, wait_runtime_ready,
};
use crate::proxy_pool::types::*;
use futures_util::{future, pin_mut, stream, StreamExt};
use rusqlite::{params, OptionalExtension};
use serde_json::Value as JsonValue;
use std::collections::HashSet;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use url::Url;

pub fn speed_test_candidates(configured: &str) -> Vec<String> {
    let configured = configured.trim();
    let configured = if configured.is_empty() || is_slow_or_blocked_speed_test_url(configured) {
        DEFAULT_PROXY_SPEED_TEST_URL
    } else {
        configured
    };
    vec![configured.to_string()]
}

pub fn normalize_ignore_addresses(value: &str) -> String {
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

pub async fn test_controller_proxy_delay(
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

pub async fn run_proxy_node_pool(
    app: &AppHandle,
    database: &Database,
    runtime: &ProxyRuntime,
    requested_node_ids: Option<HashSet<String>>,
    speed_test_url_override: Option<String>,
    channel_test: bool,
) -> Result<ProxyPoolState, String> {
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
    drop(speed_runtime);
    drop(test_lease);
    let state = load_state(database, runtime)?;
    let _ = succeeded;
    let _ = cancelled;
    Ok(state)
}
