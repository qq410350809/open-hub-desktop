use crate::context::EventBus;
use crate::models::*;
use crate::proxypool::geoip::{classify_ip, geoip_country, open_geoip_reader};
use crate::proxypool::runtime::{
    append_controller_path, controller_client, controller_url, ensure_global_runtime, load_state,
    runtime_controller_port, speed_test_plan,
};
use crate::proxypool::types::*;
use futures_util::future;
use rusqlite::params;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
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

/// 经本地代理端口（mihomo lane listener）做一次流式下载测吞吐。
/// 下载上限 500KB（收满即停，URL bytes 参数同步封顶）：从首字节起按 50ms
/// 分桶累计到达字节，取字节最多的单桶为峰值——TCP 慢启动的低速首桶自然
/// 被稳态峰值桶覆盖；首字节后 1s 未收满即主动断开（标本已够，慢节点不拖
/// 整体超时），总量不足最小样本线视为无有效吞吐。返回按峰值桶速率外推的
/// 等效 500KB 下载耗时（毫秒），与前端 MB/s 换算口径一致。
/// 注意：mihomo 对 CONNECT 先回 200 再拨号上游，纯 CONNECT 计时测不到
/// 节点连通性（只有本地回环耗时），因此延迟指标用真实 GET 计时，见
/// `lane_public_fetch_latency` 相关探测；两者由调用方并行执行、一体写回。
pub(crate) async fn download_throughput_probe(proxy_url: String, target: String) -> Option<i64> {
    let client = match reqwest::Client::builder()
        .no_proxy()
        .proxy(match reqwest::Proxy::all(&proxy_url) {
            Ok(proxy) => proxy,
            Err(_) => return None,
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
            return None;
        }
    };
    // 峰值 50ms 桶字节数 / 总采样字节数（probe 结束或被超时中断后可读）
    let peak_bucket_bytes = Arc::new(AtomicI64::new(0));
    let total_bytes = Arc::new(AtomicI64::new(0));
    let probe = {
        let peak_bucket_bytes = peak_bucket_bytes.clone();
        let total_bytes = total_bytes.clone();
        async move {
            let mut response = match client.get(&target).send().await {
                Ok(response) => response,
                Err(_) => return,
            };
            if !response.status().is_success() {
                return;
            }
            // 从首字节起按 50ms 分桶累计；收满 500KB 或超过 1s 传输窗口即停
            // （窗口内没收满说明节点吞吐有限，已收标本对峰值桶计量足够）。
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
            // 峰值 = 字节数最多的单个 50ms 桶；换算为等效 500KB 耗时
            let peak_bucket = buckets.values().copied().max().unwrap_or(0);
            peak_bucket_bytes.store(peak_bucket as i64, Ordering::Relaxed);
            total_bytes.store(received as i64, Ordering::Relaxed);
        }
    };
    let _ = tokio::time::timeout(Duration::from_millis(SPEED_TEST_TIMEOUT_MS), probe).await;

    let total = total_bytes.load(Ordering::Relaxed);
    let peak = peak_bucket_bytes.load(Ordering::Relaxed);
    // 网速 = 峰值桶字节 ÷ 50ms；channel_latency_ms 存等效 500KB 耗时
    if total >= SPEED_TEST_MIN_SAMPLE_BYTES as i64 && peak > 0 {
        Some(
            (SPEED_TEST_REF_BYTES as f64 * SPEED_TEST_PEAK_BUCKET_MS as f64 / peak as f64)
                .round() as i64,
        )
    } else {
        None
    }
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
/// （unified-delay），扣除建链一次性成本，数值即传统面板口径的"延迟"——
/// 200ms 级节点显示 ~200ms，这是唯一符合用户直觉的口径
/// （经 lane 的完整 HTTP/HTTPS 请求天然含 3~5 倍 RTT，必然偏大；
/// mihomo 对 CONNECT 先回 200 再拨号，lane 侧 CONNECT 计时只有本地回环）。
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

/// 单个回显服务的探测结果：
/// - Success：拿到合法 IP 与端到端耗时
/// - BadResponse：服务侧异常（非 2xx / 响应体不是 IP / 被劫持）——可换服务重试
/// - Unreachable：传输层不通（超时/连接失败）——换服务也没意义，节点判死
enum EchoProbeOutcome {
    Success(i64, String),
    BadResponse,
    Unreachable,
}

/// 出口 IP 抓取（不作为延迟指标）：经 lane 向 IP 回显服务发起 GET，取回
/// 出口公网 IP 用于落库与国家分组纠错。响应体必须能解析为 IP（防止把
/// 错误页当成功）。抓取失败不影响节点判定，仅放弃本次分组纠错。
async fn ip_echo_latency(proxy_url: &str, echo_url: &str) -> EchoProbeOutcome {
    let client = match reqwest::Client::builder()
        .no_proxy()
        .proxy(match reqwest::Proxy::all(proxy_url) {
            Ok(proxy) => proxy,
            Err(_) => return EchoProbeOutcome::Unreachable,
        })
        .connect_timeout(Duration::from_millis(LANE_DELAY_TIMEOUT_MS))
        .timeout(Duration::from_millis(LANE_DELAY_TIMEOUT_MS))
        .pool_max_idle_per_host(0)
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            warn!("构建延迟探测客户端失败：{error}");
            return EchoProbeOutcome::Unreachable;
        }
    };
    let started = Instant::now();
    let fetch = async {
        let response = client.get(echo_url).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let body = response.text().await.ok()?;
        let ip = body.trim();
        if ip.parse::<IpAddr>().is_err() {
            return None;
        }
        Some(ip.to_string())
    };
    match tokio::time::timeout(Duration::from_millis(LANE_DELAY_TIMEOUT_MS), fetch).await {
        Ok(Some(exit_ip)) => EchoProbeOutcome::Success(started.elapsed().as_millis() as i64, exit_ip),
        Ok(None) => EchoProbeOutcome::BadResponse,
        Err(_) => EchoProbeOutcome::Unreachable,
    }
}

/// 按优先级依次尝试回显服务：服务侧异常（BadResponse）换下一个重试；
/// 节点侧不通（Unreachable）直接判死，避免死节点白等全部服务的超时预算。
pub(crate) async fn ip_echo_probe_with_fallback(proxy_url: &str) -> Option<(i64, String)> {
    for echo_url in PROXY_IP_ECHO_URLS {
        match ip_echo_latency(proxy_url, echo_url).await {
            EchoProbeOutcome::Success(latency_ms, exit_ip) => {
                return Some((latency_ms, exit_ip));
            }
            EchoProbeOutcome::BadResponse => continue,
            EchoProbeOutcome::Unreachable => return None,
        }
    }
    None
}

/// 出口 IP 落库并按其 geoip 归属纠错国家分组：分组按 country_code 聚合，
/// 出口 IP 的归属才是节点的真实地区（入口服务器地址常与出口不一致）。
/// geoip 不可用或查不到时仅落 IP，保留原国家归属。
pub(crate) fn apply_exit_ip_geoip(
    database: &Database,
    geoip_reader: Option<&maxminddb::Reader<Vec<u8>>>,
    node_id: &str,
    exit_ip: &str,
) {
    let Ok(parsed) = exit_ip.parse::<IpAddr>() else {
        return;
    };
    let classification = classify_ip(parsed).to_string();
    let (country_code, country_name) = geoip_reader
        .and_then(|reader| geoip_country(reader, parsed))
        .unwrap_or_default();
    let connection = match database.lock_conn() {
        Ok(connection) => connection,
        Err(_) => return,
    };
    let _ = connection.execute(
        "UPDATE proxy_pool_nodes
         SET primary_ip=?2, classification=?3,
             country_code=CASE WHEN ?4 != '' THEN ?4 ELSE country_code END,
             country_name=CASE WHEN ?5 != '' THEN ?5 ELSE country_name END
         WHERE id=?1",
        params![node_id, exit_ip, classification, country_code, country_name],
    );
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

    // 两指标一次落库：delay 连通成功保留延迟；下载完成写网速耗时；
    // delay 失败两列都置 error（连不通的节点不再白等下载超时）。
    // 不预清空旧网速：本轮被取消/漏测的节点保留历史指标，避免一次
    // 异常测速（如实例端口冲突全挂）把既有 ms/网速数据全部清掉。
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
    let geoip_reader = open_geoip_reader(runtime);

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
            let geoip_reader = geoip_reader.as_ref();
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
                    // 三路并行一体测，单节点耗时取三路最大值：
                    // - 延迟：控制器 delay（unified-delay 面板口径，200ms 节点
                    //   显示 ~200ms），未通过即整体判死；
                    // - 网速：经 lane 的真实下载吞吐；
                    // - 出口 IP：经 lane 的回显抓取，仅用于落库 + geoip 纠错
                    //   国家分组，失败不影响节点判定与延迟/网速。
                    let proxy_url = format!("http://127.0.0.1:{listen_port}");
                    let (latency, echo, download_ms) = future::join3(
                        controller_proxy_delay(&client, controller_port, &node_id, &delay_url),
                        ip_echo_probe_with_fallback(&proxy_url),
                        download_throughput_probe(
                            proxy_url.clone(),
                            CHANNEL_SPEED_TEST_URL.to_string(),
                        ),
                    )
                    .await;
                    let (download_ms, status) = if latency.is_some() {
                        (download_ms, "success")
                    } else {
                        (None, "error")
                    };
                    if let (Some(_), Some((_, exit_ip))) = (latency, echo) {
                        apply_exit_ip_geoip(database, geoip_reader, &node_id, &exit_ip);
                    }
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