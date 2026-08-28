use crate::context::EventBus;
use crate::models::*;
use crate::proxypool::runtime::{
    append_controller_path, controller_client, controller_url, ensure_global_runtime, load_state,
    runtime_controller_port, speed_test_plan,
};
use crate::proxypool::types::*;
use futures_util::future;
use rusqlite::params;
use rusqlite::params_from_iter;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tracing::warn;
use url::Url;

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

/// 经本地代理端口（mihomo lane listener）做一次流式下载测速。
/// 请求 10MB 上限的流，按 100ms 时间桶统计到达字节：窗口（900ms）或采样
/// 目标到达即停，网速取最大连续 3 桶（300ms 滑窗）的平均速率——既跳过
/// 慢启动，又摊平 chunk 级调度突发。并行 lane 共享总带宽，短窗口峰值法
/// 不被长下载的带宽争抢拖垮。
/// 返回 (响应头到达耗时 ms, 等效 500KB 下载耗时 ms)：
/// - 超时前没收到响应头 → (None, None)
/// - 总采样 < 最小采样量或无峰值 → (Some, None)（有连通但无有效吞吐）
/// - 采样完成 → (Some, Some)，等效耗时 = 500KB ÷ 峰值速率
pub(crate) async fn measure_get_probe(
    proxy_url: String,
    target: String,
) -> (Option<i64>, Option<i64>) {
    let client = match reqwest::Client::builder()
        .no_proxy()
        .proxy(match reqwest::Proxy::all(&proxy_url) {
            Ok(proxy) => proxy,
            Err(_) => return (None, None),
        })
        .connect_timeout(Duration::from_millis(SPEED_TEST_TIMEOUT_MS))
        .timeout(Duration::from_millis(SPEED_TEST_TIMEOUT_MS))
        // 禁用连接复用：每个节点探测必须建立全新链路，否则切换节点后
        // 可能复用上一个节点的 CONNECT 隧道，导致测速串节点
        .pool_max_idle_per_host(0)
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            warn!("构建下载测速客户端失败：{error}");
            return (None, None);
        }
    };
    let started = Instant::now();
    let ttfb_ms = Arc::new(AtomicI64::new(-1));
    // 峰值窗口（连续 K 桶）字节数 / 总采样字节数（probe 结束或被超时中断后可读）
    let peak_window_bytes = Arc::new(AtomicI64::new(0));
    let total_bytes = Arc::new(AtomicI64::new(0));
    let probe = {
        let ttfb_ms = ttfb_ms.clone();
        let peak_window_bytes = peak_window_bytes.clone();
        let total_bytes = total_bytes.clone();
        async move {
            let mut response = match client.get(&target).send().await {
                Ok(response) => response,
                Err(_) => return,
            };
            let ttfb = started.elapsed().as_millis() as i64;
            ttfb_ms.store(ttfb, Ordering::Relaxed);
            if !response.status().is_success() {
                return;
            }
            // 按 100ms 时间桶累计到达字节；窗口（或采样目标）一到即停，
            // 不必下载完整文件——并行 lane 共享带宽，短窗口+峰值统计
            // 既能跳过慢启动，又不被长下载的带宽争抢拖垮。
            let header_at = Instant::now();
            let mut buckets: HashMap<u64, u64> = HashMap::new();
            let mut received: u64 = 0;
            loop {
                match response.chunk().await {
                    Ok(Some(chunk)) => {
                        let elapsed_ms = header_at.elapsed().as_millis() as u64;
                        *buckets
                            .entry(elapsed_ms / SPEED_TEST_PEAK_BUCKET_MS)
                            .or_insert(0) += chunk.len() as u64;
                        received += chunk.len() as u64;
                        if received >= SPEED_TEST_TARGET_BYTES {
                            break;
                        }
                        if elapsed_ms >= SPEED_TEST_TRANSFER_WINDOW_MS {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            // 峰值取最大连续 K 桶的字节和（从桶 1 起：桶 0 不完整且处于慢启动
            // 起点）；缺失桶按 0 计。单桶突发被 K 桶窗口摊薄。
            let max_index = buckets.keys().copied().max().unwrap_or(0);
            let mut peak_window: u64 = 0;
            for start in 1..=max_index {
                let sum: u64 = (0..SPEED_TEST_PEAK_WINDOW_BUCKETS)
                    .map(|offset| buckets.get(&(start + offset)).copied().unwrap_or(0))
                    .sum();
                peak_window = peak_window.max(sum);
            }
            peak_window_bytes.store(peak_window as i64, Ordering::Relaxed);
            total_bytes.store(received as i64, Ordering::Relaxed);
        }
    };
    let _ = tokio::time::timeout(Duration::from_millis(SPEED_TEST_TIMEOUT_MS), probe).await;

    let ttfb = {
        let value = ttfb_ms.load(Ordering::Relaxed);
        (value >= 0).then_some(value)
    };
    let total = total_bytes.load(Ordering::Relaxed);
    let peak = peak_window_bytes.load(Ordering::Relaxed);
    // 网速 = 峰值窗口字节数 ÷ 窗口时长；channel_latency_ms 存等效 500KB 耗时
    let window_ms = (SPEED_TEST_PEAK_BUCKET_MS * SPEED_TEST_PEAK_WINDOW_BUCKETS) as f64;
    let effective_ms = if total >= SPEED_TEST_MIN_SAMPLE_BYTES as i64 && peak > 0 {
        Some((SPEED_TEST_REF_BYTES as f64 * window_ms / peak as f64).round() as i64)
    } else {
        None
    };
    (ttfb, effective_ms)
}

/// 通过控制器 API 把 lane 的 select 组切换到指定节点
async fn select_lane_node(
    client: &reqwest::Client,
    controller_port: u16,
    group: &str,
    node: &str,
) -> Result<(), String> {
    let mut url =
        Url::parse(&controller_url(controller_port, "/proxies/")).map_err(|e| e.to_string())?;
    append_controller_path(&mut url, &[group]).map_err(|e| e.to_string())?;
    let response = client
        .put(url)
        .bearer_auth(RUNTIME_SECRET)
        .timeout(Duration::from_secs(3))
        .json(&json!({ "name": node }))
        .send()
        .await
        .map_err(|error| format!("切换测速节点失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!("切换测速节点失败：HTTP {}", response.status()));
    }
    Ok(())
}

/// 经控制器 delay 接口测节点连通延迟。mihomo 内部做两次测量取小
/// （unified-delay），扣除建链一次性成本，数值即传统面板口径的"延迟"
/// （几十~几百 ms）。拨号真实经节点（DialContext 走代理），无假测速问题——
/// 网速由独立的 GET 下载负责。
pub(crate) async fn controller_proxy_delay(
    client: &reqwest::Client,
    controller_port: u16,
    proxy_name: &str,
    url: &str,
) -> Option<i64> {
    let mut endpoint =
        Url::parse(&controller_url(controller_port, "/proxies/")).ok()?;
    append_controller_path(&mut endpoint, &[proxy_name, "delay"]).ok()?;
    endpoint
        .query_pairs_mut()
        .append_pair("timeout", &SPEED_TEST_TIMEOUT_MS.to_string())
        .append_pair("url", url);
    let response = client
        .get(endpoint)
        .bearer_auth(RUNTIME_SECRET)
        .timeout(Duration::from_millis(SPEED_TEST_TIMEOUT_MS + 2_000))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let value: serde_json::Value = response.json().await.ok()?;
    value.get("delay").and_then(|delay| delay.as_i64())
}

pub async fn run_proxy_node_pool(
    bus: &EventBus,
    database: &Database,
    runtime: &ProxyRuntime,
    requested_node_ids: Option<HashSet<String>>,
    channel_test: bool,
) -> Result<ProxyPoolState, String> {
    let nodes = {
        let connection = database.lock_conn()?;
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

    let progress_event = if channel_test {
        "proxy-channel-test-progress"
    } else {
        "proxy-node-test-progress"
    };

    let test_lease = runtime.start_proxy_test()?;
    let cancellation = test_lease.cancellation.clone();

    // 全局单实例：测速 lane 是实例内预配的固定 SPEED-lane，不再拉临时进程。
    // 配置装载全量节点且代理名已改写为节点 id，切组直接用节点 id 寻址。
    let _op_guard = runtime.runtime_op_lock.lock().await;
    tokio::task::block_in_place(|| ensure_global_runtime(database, runtime))?;
    let plan = speed_test_plan(runtime)?;
    let lane_count = plan.lanes.len().min(total).max(1);
    let lanes = &plan.lanes[..lane_count];
    let ordered_ids: Vec<String> = testable_nodes.iter().map(|(id, _)| id.clone()).collect();
    let controller_port = runtime_controller_port(runtime)?;

    // 测速前清空本次目标节点的旧网速数据：取消/漏测的节点不残留过期网速，
    // 前端网速档位过滤只认本轮测速写回的新结果。
    {
        let connection = database.lock_conn()?;
        for chunk in testable_nodes.chunks(500) {
            let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "UPDATE proxy_pool_nodes SET channel_latency_ms=NULL, channel_test_status='', channel_tested_at='' WHERE id IN ({placeholders})"
            );
            let ids = chunk.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>();
            connection
                .execute(&sql, params_from_iter(ids))
                .map_err(|error| error.to_string())?;
        }
    }

    // 两指标一次落库：delay 连通成功保留延迟；下载完成写网速耗时；
    // delay 失败两列都置 error（连不通的节点不再白等下载超时）。
    let full_success_sql = "UPDATE proxy_pool_nodes SET latency_ms=?2, test_status='success', channel_latency_ms=?3, channel_test_status='success', channel_tested_at=CURRENT_TIMESTAMP, tested_at=CURRENT_TIMESTAMP WHERE id=?1";
    let header_only_sql = "UPDATE proxy_pool_nodes SET latency_ms=?2, test_status='success', channel_latency_ms=NULL, channel_test_status='error', channel_tested_at=CURRENT_TIMESTAMP, tested_at=CURRENT_TIMESTAMP WHERE id=?1";
    let fail_sql = "UPDATE proxy_pool_nodes SET latency_ms=NULL, test_status='error', channel_latency_ms=NULL, channel_test_status='error', channel_tested_at=CURRENT_TIMESTAMP, tested_at=CURRENT_TIMESTAMP WHERE id=?1";
    let write_probe_result = |node_id: &str,
                              ttfb: Option<i64>,
                              download_ms: Option<i64>|
     -> Result<(), String> {
        let connection = database.lock_conn()?;
        if let (Some(ttfb), Some(download_ms)) = (ttfb, download_ms) {
            connection
                .execute(full_success_sql, params![node_id, ttfb, download_ms])
                .map_err(|error| error.to_string())?;
        } else if let Some(ttfb) = ttfb {
            connection
                .execute(header_only_sql, params![node_id, ttfb])
                .map_err(|error| error.to_string())?;
        } else {
            connection
                .execute(fail_sql, [node_id])
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    };

    let completed = Arc::new(AtomicUsize::new(0));
    let client = controller_client()?;

    // 延迟口径：控制器 delay 接口的测试 URL（设置页"测速地址"，默认 gstatic 204）
    let delay_url = {
        let stored = crate::db::read_meta(database, PROXY_SPEED_TEST_URL_KEY).unwrap_or_default();
        if stored.trim().is_empty()
            || crate::proxypool::runtime::is_slow_or_blocked_speed_test_url(&stored)
        {
            DEFAULT_PROXY_SPEED_TEST_URL.to_string()
        } else {
            stored
        }
    };

    let lane_futures = lanes
        .iter()
        .enumerate()
        .map(|(lane_idx, lane)| {
            let my_nodes: Vec<String> = ordered_ids
                .iter()
                .skip(lane_idx)
                .step_by(lanes.len())
                .cloned()
                .collect();
            let client = client.clone();
            let bus = bus.clone();
            let cancellation = cancellation.clone();
            let group_name = lane.group_name.clone();
            let listen_port = lane.listen_port;
            let delay_url = delay_url.clone();
            let completed = Arc::clone(&completed);
            async move {
                for node_id in my_nodes {
                    if cancellation.is_cancelled() {
                        break;
                    }
                    bus.emit(
                        progress_event,
                        ProxyNodeTestProgress {
                            node_id: node_id.clone(),
                            phase: "started".to_string(),
                            status: "testing".to_string(),
                            stage: "speed".to_string(),
                            total,
                            ..Default::default()
                        },
                    );
                    if let Err(error) =
                        select_lane_node(&client, controller_port, &group_name, &node_id).await
                    {
                        warn!("切换测速节点失败 {node_id}: {error}");
                        let _ = write_probe_result(&node_id, None, None);
                        let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                        bus.emit(
                            progress_event,
                            ProxyNodeTestProgress {
                                node_id,
                                phase: "completed".to_string(),
                                latency_ms: None,
                                speed_ms: None,
                                status: "error".to_string(),
                                stage: "speed".to_string(),
                                completed: done,
                                total,
                            },
                        );
                        continue;
                    }
                    // 连通延迟：控制器 delay 接口（unified-delay 双测取小）。
                    // 连不通直接判死，不再白等 5s 下载超时。
                    let latency =
                        controller_proxy_delay(&client, controller_port, &node_id, &delay_url)
                            .await;
                    let (download_ms, status) = if latency.is_some() {
                        let proxy_url = format!("http://127.0.0.1:{listen_port}");
                        let (_, download_ms) =
                            measure_get_probe(proxy_url, CHANNEL_SPEED_TEST_URL.to_string()).await;
                        (download_ms, "success")
                    } else {
                        (None, "error")
                    };
                    let _ = write_probe_result(&node_id, latency, download_ms);
                    let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                    bus.emit(
                        progress_event,
                        ProxyNodeTestProgress {
                            node_id,
                            phase: "completed".to_string(),
                            latency_ms: latency,
                            speed_ms: download_ms,
                            status: status.to_string(),
                            stage: "speed".to_string(),
                            completed: done,
                            total,
                        },
                    );
                }
            }
        })
        .collect::<Vec<_>>();
    future::join_all(lane_futures).await;

    drop(_op_guard);
    drop(test_lease);
    load_state(database, runtime)
}