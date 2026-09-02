use crate::charity::db::*;
use crate::charity::feed::items_from_topic_list;
use crate::charity::types::*;
use crate::context::{home_dir, spawn, spawn_blocking, AppContext, EventBus};
use crate::models::Database;
use crate::proxypool::{self, is_http_forbidden_error, is_transport_error, ProxyRuntime};
use crate::site::sync;
use std::sync::{atomic::AtomicUsize, atomic::Ordering, Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

#[allow(dead_code)]
pub fn rotate_fast_nodes(
    nodes: &[(String, String, i64)],
    offset: usize,
) -> Vec<(String, String, i64)> {
    if nodes.is_empty() {
        return Vec::new();
    }
    let offset = offset % nodes.len();
    let mut rotated = Vec::with_capacity(nodes.len());
    rotated.extend_from_slice(&nodes[offset..]);
    rotated.extend_from_slice(&nodes[..offset]);
    rotated
}

pub fn build_mihomo_client(proxy_url: &str) -> Result<reqwest::Client, String> {
    let proxy = reqwest::Proxy::all(proxy_url).map_err(|_| "Mihomo 混合端口地址无效")?;
    reqwest::Client::builder()
        .timeout(CHARITY_REQUEST_TIMEOUT)
        .connect_timeout(Duration::from_secs(8))
        .http2_adaptive_window(true)
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(0)
        .tcp_keepalive(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .proxy(proxy)
        .build()
        .map_err(|error| format!("无法初始化代理请求客户端：{error}"))
}

pub fn is_charity_sync_cancelled(error: &str) -> bool {
    error.starts_with(CHARITY_SYNC_CANCELLED_PREFIX)
}

pub fn ban_ttl_for_error(error: &str) -> Duration {
    let lower = error.to_ascii_lowercase();
    if is_http_forbidden_error(error) {
        CHARITY_BAN_FORBIDDEN
    } else if lower.contains("http 429")
        || lower.contains("status 429")
        || lower.contains("too many requests")
    {
        CHARITY_BAN_RATE_LIMITED
    } else if is_transport_error(error) {
        CHARITY_BAN_UNREACHABLE
    } else if lower.contains("超时") {
        CHARITY_BAN_TIMEOUT
    } else {
        CHARITY_BAN_DEFAULT
    }
}

pub fn eject_node_from_charity_candidate(
    monitor: &CharityMonitorRuntime,
    queue: &Arc<Mutex<CharityNodeQueue>>,
    node: &CharityNodeRef,
    error: &str,
) {
    let ttl = ban_ttl_for_error(error);
    monitor.ban_node(&node.id, ttl);
    monitor.clear_preferred_node(&node.id);
    if let Ok(mut q) = queue.lock() {
        let _ = q.remove_id(&node.id);
    }
}

/// 粘性排序：上次成功的节点排最前且不轮换；没有粘性节点时才按轮换取候选。
pub fn order_nodes_sticky(
    nodes: Vec<CharityNodeRef>,
    preferred: Option<&str>,
    round_robin: &AtomicUsize,
) -> Vec<CharityNodeRef> {
    let mut nodes = nodes;
    if nodes.is_empty() {
        return nodes;
    }
    if let Some(preferred) = preferred {
        if let Some(position) = nodes.iter().position(|node| node.id == preferred) {
            let node = nodes.remove(position);
            nodes.insert(0, node);
            return nodes;
        }
    }
    let offset = round_robin.fetch_add(1, Ordering::Relaxed) % nodes.len();
    if offset > 0 {
        nodes.rotate_left(offset);
    }
    nodes
}

pub fn build_charity_node_queue(
    database: &Database,
    monitor: &CharityMonitorRuntime,
) -> Result<Vec<CharityNodeRef>, String> {
    monitor.purge_expired_bans();
    let raw =
        proxypool::list_prioritized_fast_proxy_nodes(database, CHARITY_FAST_NODE_MAX_LATENCY_MS)?;
    let mut nodes = raw
        .into_iter()
        .filter(|(id, _, _)| !monitor.is_banned(id))
        .map(|(id, name, latency_ms)| CharityNodeRef {
            id,
            name,
            latency_ms,
        })
        .collect::<Vec<_>>();
    nodes = order_nodes_sticky(
        nodes,
        monitor.preferred_node().as_deref(),
        &monitor.node_round_robin,
    );
    if nodes.len() > CHARITY_PREPARE_NODE_LIMIT {
        nodes.truncate(CHARITY_PREPARE_NODE_LIMIT);
    }
    Ok(nodes)
}

pub async fn request_topic_list(
    client: &reqwest::Client,
    source: &CharityFeedSource,
    cookie_header: Option<&str>,
) -> Result<(String, String), String> {
    let request_url = {
        let mut url = url::Url::parse(&source.json_url)
            .map_err(|error| format!("标签地址解析失败：{error}"))?;
        url.query_pairs_mut().append_pair(
            "t",
            &std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis().to_string())
                .unwrap_or_default(),
        );
        url
    };
    let mut request = client
        .get(request_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
        .header(reqwest::header::USER_AGENT, sync::chrome_user_agent())
        .header("Sec-Fetch-Dest", "empty")
        .header("Sec-Fetch-Mode", "cors")
        .header("Sec-Fetch-Site", "same-origin")
        .header(
            "Referer",
            format!("https://linux.do/tag/{}/l/latest", source.id),
        );
    if let Some(cookie_header) = cookie_header.filter(|value| !value.trim().is_empty()) {
        request = request.header(reqwest::header::COOKIE, cookie_header);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("{}标签请求失败：{error}", source.name))?;
    let protocol = format!("{:?}", response.version());
    if !response.status().is_success() {
        return Err(format!(
            "{}标签请求失败（HTTP {}，{protocol}）",
            source.name,
            response.status().as_u16()
        ));
    }
    let body = response
        .text()
        .await
        .map_err(|error| format!("{}标签内容读取失败：{error}", source.name))?;
    if !body.trim_start().starts_with('{') {
        return Err(format!(
            "{}标签返回了非 JSON 内容，可能需要完成 Linux.do 安全验证",
            source.name
        ));
    }
    Ok((body, protocol))
}

pub async fn fetch_topic_body(
    _ctx: &Arc<AppContext>,
    client: reqwest::Client,
    source: &CharityFeedSource,
) -> Result<(String, String, String, String), String> {
    // linux.do 对无 Cookie 的匿名 JSON 请求会命中 CDN 边缘缓存，HTTP 200 也可能返回
    // 数小时前的旧列表（`t` 时间戳绕不开），所以必须优先带 Chrome 登录 Cookie 请求
    // （与浏览器一致），匿名请求只作兜底，保证 Cookie 全部失败时仍能拿到数据。
    let mut errors = Vec::new();
    if let Some(home_dir) = home_dir() {
        let profiles = match spawn_blocking({
            let home_dir = home_dir.clone();
            move || sync::profile_identities_from_home(&home_dir)
        })
        .await
        {
            Ok(Ok(profiles)) => profiles,
            Ok(Err(error)) => {
                errors.push(error);
                Vec::new()
            }
            Err(error) => {
                errors.push(format!("读取 Chrome Profile 任务失败：{error}"));
                Vec::new()
            }
        };

        let jobs = profiles
            .into_iter()
            .map(|profile| {
                let client = client.clone();
                let home_dir = home_dir.clone();
                let source_clone = source.clone();
                spawn(async move {
                    let profile_id = profile.id.clone();
                    let json_url_for_cookie = source_clone.json_url.clone();
                    let cookie_header = spawn_blocking(move || {
                        sync::read_chrome_cookie_header_from_home(
                            &home_dir,
                            &json_url_for_cookie,
                            &profile_id,
                        )
                    })
                    .await
                    .map_err(|error| format!("读取 Chrome Cookie 任务失败：{error}"))??;
                    let (body, protocol) =
                        request_topic_list(&client, &source_clone, Some(&cookie_header)).await?;
                    Ok::<_, String>((body, profile.name, profile.account_name, protocol))
                })
            })
            .collect::<Vec<_>>();
        for job in jobs {
            match job.await {
                Ok(Ok(result)) => return Ok(result),
                Ok(Err(error)) => errors.push(error),
                Err(error) => errors.push(format!("Chrome 标签请求任务失败：{error}")),
            }
        }
    }

    match request_topic_list(&client, source, None).await {
        Ok((body, protocol)) => Ok((body, String::new(), String::new(), protocol)),
        Err(error) => {
            errors.push(error);
            Err(errors
                .last()
                .cloned()
                .unwrap_or_else(|| format!("无法读取 Linux.do {}标签", source.name)))
        }
    }
}

/// 取本轮尝试的节点：粘住上次成功的节点（窥视复用、不出队，并行 feed 共用同一节点）；
/// 粘性节点不在候选里（被剔除/掉出快节点名单）时才取队首新节点。
pub(crate) fn take_attempt_node(
    monitor: &CharityMonitorRuntime,
    queue: &Arc<Mutex<CharityNodeQueue>>,
) -> Option<CharityNodeRef> {
    if let Some(preferred) = monitor.preferred_node() {
        if let Ok(q) = queue.lock() {
            if let Some(node) = q.nodes.iter().find(|node| node.id == preferred).cloned() {
                return Some(node);
            }
        }
    }
    queue.lock().ok().and_then(|mut q| q.pop_front())
}

/// 合并拉取地址：filter.json 一次查询全部标签（逗号为任一匹配，只认标签名称不认 slug），
/// q 参数交给 url crate 编码，时间戳防缓存仍由 request_topic_list 统一追加。
pub fn combined_filter_json_url(feed_names: &[String]) -> String {
    let mut url = url::Url::parse("https://linux.do/filter.json?").expect("静态地址必然可解析");
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("q", &format!("tag:{} order:created", feed_names.join(",")));
    }
    url.to_string()
}

/// 按帖子的 tags 名称把合并结果拆分到各标签源（名称与标签源 name 精确匹配）。
pub fn split_items_by_feed(
    items: &[CharityFeedItem],
    sources: &[CharityFeedSource],
) -> Vec<(String, Vec<CharityFeedItem>)> {
    sources
        .iter()
        .map(|source| {
            let owned = items
                .iter()
                .filter(|item| item.categories.iter().any(|tag| tag == &source.name))
                .cloned()
                .collect::<Vec<_>>();
            (source.id.clone(), owned)
        })
        .collect()
}

/// 合并拉取一轮里单个标签的结局。
#[derive(Debug, Clone)]
pub struct CombinedFeedOutcome {
    pub feed_id: String,
    pub feed_name: String,
    pub status: &'static str,
    pub new_count: usize,
    pub updated_count: usize,
}

/// 整轮合并同步：一次请求覆盖全部标准标签，按 tags 拆分入库；
/// 每轮只写一条"本轮汇总"日志（运行中 → 结束时更新为汇总说明 + 明细表格数据）。
pub async fn sync_round_combined(
    ctx: &Arc<AppContext>,
    database: &Database,
    runtime: &ProxyRuntime,
    sources: &[CharityFeedSource],
    stage: &str,
    cancellation: &CancellationToken,
    queue: &Arc<Mutex<CharityNodeQueue>>,
) -> Vec<CombinedFeedOutcome> {
    let monitor = &ctx.charity_runtime;
    let bus = EventBus::clone(&ctx.event_bus);
    let round_started = Instant::now();
    let fail_all = |message: &str| -> Vec<CombinedFeedOutcome> {
        sources
            .iter()
            .map(|source| {
                let _ = write_feed_sync_meta(database, &source.id, "error", message, "", 0);
                if let Ok(mut errors) = monitor.last_errors.lock() {
                    errors.insert(source.id.clone(), message.to_string());
                }
                emit_charity_progress(
                    &bus,
                    CharitySyncProgress {
                        feed_id: source.id.clone(),
                        feed_name: source.name.clone(),
                        stage: stage.into(),
                        status: "error".into(),
                        message: message.into(),
                        used_node_id: String::new(),
                        used_node_name: String::new(),
                        new_count: 0,
                        updated_count: 0,
                        unread_count: 0,
                    },
                );
                CombinedFeedOutcome {
                    feed_id: source.id.clone(),
                    feed_name: source.name.clone(),
                    status: "failed",
                    new_count: 0,
                    updated_count: 0,
                }
            })
            .collect()
    };

    let feed_names = sources
        .iter()
        .map(|source| source.name.clone())
        .collect::<Vec<_>>();
    let combined_source = CharityFeedSource {
        id: "round".into(),
        name: "本轮汇总".into(),
        json_url: combined_filter_json_url(&feed_names),
        enabled: true,
        sort_order: 0,
    };
    let round_log_id = append_charity_sync_log(
        database,
        "round",
        "本轮汇总",
        stage,
        "running",
        "合并请求进行中（全部标签一次拉取）",
        "",
        "",
    );

    let client = match build_mihomo_client(&proxypool::runtime_proxy_url_pub(runtime)) {
        Ok(client) => client,
        Err(error) => {
            let message = format!("合并同步失败：无法初始化代理客户端：{error}");
            if let Some(id) = round_log_id {
                update_charity_sync_log(
                    database,
                    id,
                    "error",
                    &message,
                    "",
                    round_started.elapsed().as_millis() as i64,
                    "",
                );
            }
            return fail_all(&message);
        }
    };

    let mut last_error = String::new();
    let mut attempts = 0usize;
    while attempts < CHARITY_MAX_NODE_ATTEMPTS {
        if cancellation.is_cancelled() {
            let message = format!("{CHARITY_SYNC_CANCELLED_PREFIX}：合并同步");
            if let Some(id) = round_log_id {
                update_charity_sync_log(
                    database,
                    id,
                    "cancelled",
                    &message,
                    "",
                    round_started.elapsed().as_millis() as i64,
                    "",
                );
            }
            return sources
                .iter()
                .map(|source| CombinedFeedOutcome {
                    feed_id: source.id.clone(),
                    feed_name: source.name.clone(),
                    status: "cancelled",
                    new_count: 0,
                    updated_count: 0,
                })
                .collect();
        }
        let Some(node) = take_attempt_node(monitor, queue) else {
            break;
        };
        if monitor.is_banned(&node.id) {
            continue;
        }
        attempts += 1;
        let attempt_started = Instant::now();
        let running_msg = format!(
            "合并请求进行中 · {}（{}ms）· 第{}次",
            node.name, node.latency_ms, attempts
        );
        if let Some(id) = round_log_id {
            touch_running_charity_sync_log(database, id, &running_msg, &node.name, 0);
        }

        if let Err(error) =
            proxypool::select_proxy_node_transient(database, runtime, &node.id).await
        {
            let message = format!("{}: 切换代理失败：{error}", node.name);
            eject_node_from_charity_candidate(monitor, queue, &node, &message);
            last_error = message;
            continue;
        }
        let mut waited = Duration::from_millis(0);
        while waited < Duration::from_millis(60) {
            let slice = Duration::from_millis(30);
            tokio::time::sleep(slice).await;
            waited += slice;
        }

        let request_deadline = Instant::now() + CHARITY_REQUEST_TIMEOUT;
        let fetch_future = fetch_topic_body(ctx, client.clone(), &combined_source);
        tokio::pin!(fetch_future);
        let fetch_result = loop {
            if cancellation.is_cancelled() {
                break Err(format!("{CHARITY_SYNC_CANCELLED_PREFIX}：合并同步"));
            }
            let left = request_deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break Err(format!(
                    "{}: 请求超时（单次请求 {}s）",
                    node.name,
                    CHARITY_REQUEST_TIMEOUT.as_secs()
                ));
            }
            let tick = left.min(Duration::from_millis(500));
            tokio::select! {
                _ = cancellation.cancelled() => {
                    break Err(format!("{CHARITY_SYNC_CANCELLED_PREFIX}：合并同步"));
                }
                result = &mut fetch_future => break result,
                _ = tokio::time::sleep(tick) => {
                    if let Some(id) = round_log_id {
                        let msg = format!(
                            "合并请求进行中 · {} · 已用 {:.1}s",
                            node.name,
                            attempt_started.elapsed().as_secs_f64()
                        );
                        touch_running_charity_sync_log(
                            database,
                            id,
                            &msg,
                            &node.name,
                            attempt_started.elapsed().as_millis() as i64,
                        );
                    }
                }
            }
        };

        match fetch_result {
            Ok((body, profile_name, account_name, protocol)) => {
                match items_from_topic_list(&body) {
                    Ok(items) => {
                        let mut outcomes = Vec::with_capacity(sources.len());
                        let mut feeds_detail = Vec::with_capacity(sources.len());
                        let mut summary_parts = Vec::with_capacity(sources.len());
                        let mut total_new = 0usize;
                        let mut total_updated = 0usize;
                        tokio::task::block_in_place(|| {
                            let split = split_items_by_feed(&items, sources);
                            for (source, owned) in sources.iter().zip(&split) {
                                let outcome = if owned.1.is_empty() {
                                    let message = "本轮合并结果中无该标签新帖";
                                    let _ = write_feed_sync_meta(
                                        database, &source.id, "success", message, &node.name, 0,
                                    );
                                    if let Ok(mut errors) = monitor.last_errors.lock() {
                                        errors.remove(&source.id);
                                    }
                                    CombinedFeedOutcome {
                                        feed_id: source.id.clone(),
                                        feed_name: source.name.clone(),
                                        status: "success",
                                        new_count: 0,
                                        updated_count: 0,
                                    }
                                } else {
                                    match persist_feed(
                                        database,
                                        source,
                                        owned.1.clone(),
                                        profile_name.clone(),
                                        account_name.clone(),
                                    ) {
                                        Ok(result) => {
                                            if let Ok(mut errors) = monitor.last_errors.lock() {
                                                errors.remove(&source.id);
                                            }
                                            CombinedFeedOutcome {
                                                feed_id: source.id.clone(),
                                                feed_name: source.name.clone(),
                                                status: "success",
                                                new_count: result.new_count,
                                                updated_count: result.updated_count,
                                            }
                                        }
                                        Err(error) => {
                                            let message = format!("入库失败：{error}");
                                            let _ = write_feed_sync_meta(
                                                database, &source.id, "error", &message,
                                                &node.name, 0,
                                            );
                                            if let Ok(mut errors) = monitor.last_errors.lock() {
                                                errors.insert(source.id.clone(), message);
                                            }
                                            CombinedFeedOutcome {
                                                feed_id: source.id.clone(),
                                                feed_name: source.name.clone(),
                                                status: "failed",
                                                new_count: 0,
                                                updated_count: 0,
                                            }
                                        }
                                    }
                                };
                                total_new += outcome.new_count;
                                total_updated += outcome.updated_count;
                                summary_parts.push(format!(
                                    "{} 新增{} / 更新{}",
                                    outcome.feed_name, outcome.new_count, outcome.updated_count
                                ));
                                feeds_detail.push(serde_json::json!({
                                    "id": outcome.feed_id,
                                    "name": outcome.feed_name,
                                    "status": outcome.status,
                                    "new": outcome.new_count,
                                    "updated": outcome.updated_count,
                                }));
                                emit_charity_progress(
                                    &bus,
                                    CharitySyncProgress {
                                        feed_id: outcome.feed_id.clone(),
                                        feed_name: outcome.feed_name.clone(),
                                        stage: stage.into(),
                                        status: outcome.status.to_string(),
                                        message: String::new(),
                                        used_node_id: String::new(),
                                        used_node_name: node.name.clone(),
                                        new_count: outcome.new_count,
                                        updated_count: outcome.updated_count,
                                        unread_count: 0,
                                    },
                                );
                                outcomes.push(outcome);
                            }
                        });
                        monitor.set_preferred_node(&node.id);
                        if let Ok(mut q) = queue.lock() {
                            q.push_back_if_absent(node.clone());
                        }
                        let any_failed = outcomes.iter().any(|outcome| outcome.status == "failed");
                        let clock = chrono::Local::now().format("%H:%M:%S");
                        let message = format!(
                            "{clock} 本轮同步完成（合并请求，{protocol}，{} · 第{attempts}次） · {} · 合计新增 {total_new} / 更新 {total_updated}",
                            node.name,
                            summary_parts.join(" · ")
                        );
                        let detail = serde_json::json!({
                            "totalNew": total_new,
                            "totalUpdated": total_updated,
                            "feeds": feeds_detail,
                        })
                        .to_string();
                        if let Some(id) = round_log_id {
                            update_charity_sync_log(
                                database,
                                id,
                                if any_failed { "error" } else { "success" },
                                &message,
                                &node.name,
                                round_started.elapsed().as_millis() as i64,
                                &detail,
                            );
                        }
                        return outcomes;
                    }
                    Err(error) => {
                        let raw = format!("{}: {error}", node.name);
                        eject_node_from_charity_candidate(monitor, queue, &node, &raw);
                        last_error = raw;
                    }
                }
            }
            Err(error) => {
                if is_charity_sync_cancelled(&error) {
                    if let Some(id) = round_log_id {
                        update_charity_sync_log(
                            database,
                            id,
                            "cancelled",
                            &error,
                            &node.name,
                            round_started.elapsed().as_millis() as i64,
                            "",
                        );
                    }
                    return sources
                        .iter()
                        .map(|source| CombinedFeedOutcome {
                            feed_id: source.id.clone(),
                            feed_name: source.name.clone(),
                            status: "cancelled",
                            new_count: 0,
                            updated_count: 0,
                        })
                        .collect();
                }
                eject_node_from_charity_candidate(monitor, queue, &node, &error);
                last_error = error;
            }
        }
    }

    let message = if last_error.is_empty() {
        format!(
            "合并同步失败：本轮没有可尝试的 ≤{}ms 候选节点",
            CHARITY_FAST_NODE_MAX_LATENCY_MS
        )
    } else {
        format!(
            "{}（合并请求已尝试 {attempts}/{} 个节点）",
            last_error, CHARITY_MAX_NODE_ATTEMPTS
        )
    };
    if let Some(id) = round_log_id {
        update_charity_sync_log(
            database,
            id,
            "error",
            &message,
            "",
            round_started.elapsed().as_millis() as i64,
            "",
        );
    }
    fail_all(&message)
}

pub async fn sync_feed_with_fast_nodes(
    ctx: &Arc<AppContext>,
    database: &Database,
    runtime: &ProxyRuntime,
    source: &CharityFeedSource,
    stage: &str,
    cancellation: &CancellationToken,
    shared_queue: Option<Arc<Mutex<CharityNodeQueue>>>,
    nodes_prepared: bool,
) -> Result<CharityFeedResult, String> {
    let feed_started_at = Instant::now();
    let stage_label = if stage == "manual" {
        "手动刷新"
    } else {
        "后台轮询"
    };
    let feed_duration_ms = || feed_started_at.elapsed().as_millis() as i64;

    if cancellation.is_cancelled() {
        return Err(format!("{CHARITY_SYNC_CANCELLED_PREFIX}：{}", source.name));
    }

    let monitor_state = &ctx.charity_runtime;
    let bus = EventBus::clone(&ctx.event_bus);

    if cancellation.is_cancelled() {
        return Err(format!("{CHARITY_SYNC_CANCELLED_PREFIX}：{}", source.name));
    }

    let proxy_url = proxypool::runtime_proxy_url_pub(runtime);
    let client = match build_mihomo_client(&proxy_url) {
        Ok(client) => client,
        Err(error) => {
            let log_id = append_charity_sync_log(
                database,
                &source.id,
                &source.name,
                stage,
                "running",
                &format!("{stage_label}失败：无法初始化代理客户端"),
                "",
                "",
            );
            finish_charity_sync_log(
                &bus,
                database,
                log_id,
                source,
                stage,
                "failed",
                &error,
                "",
                feed_duration_ms(),
                0,
                0,
                0,
            );
            return Err(error);
        }
    };

    let queue = if let Some(shared) = shared_queue.clone() {
        shared
    } else {
        let nodes = match tokio::task::block_in_place(|| {
            build_charity_node_queue(database, &monitor_state)
        }) {
            Ok(nodes) => nodes,
            Err(error) => {
                let log_id = append_charity_sync_log(
                    database,
                    &source.id,
                    &source.name,
                    stage,
                    "running",
                    &format!("{stage_label}失败：读取候选节点"),
                    "",
                    "",
                );
                finish_charity_sync_log(
                    &bus,
                    database,
                    log_id,
                    source,
                    stage,
                    "failed",
                    &error,
                    "",
                    feed_duration_ms(),
                    0,
                    0,
                    0,
                );
                return Err(error);
            }
        };
        if nodes.is_empty() {
            let message =
                format!("无 ≤{CHARITY_FAST_NODE_MAX_LATENCY_MS}ms 可用公益候选节点，本轮跳过");
            let log_id = append_charity_sync_log(
                database,
                &source.id,
                &source.name,
                stage,
                "running",
                &format!("{stage_label}跳过：{}", source.name),
                "",
                "",
            );
            let local = tokio::task::block_in_place(|| {
                load_feed_items_from_db(database, source, 0, CHARITY_PAGE_SIZE, "", "all")
            })?;
            let _ = write_feed_sync_meta(database, &source.id, "skipped", &message, "", 0);
            finish_charity_sync_log(
                &bus,
                database,
                log_id,
                source,
                stage,
                "skipped",
                &message,
                "",
                feed_duration_ms(),
                0,
                0,
                local.unread_count,
            );
            let mut local = local;
            local.status = "skipped".into();
            local.skipped = true;
            local.message = message;
            return Ok(local);
        }
        if !nodes_prepared {
            let ids = nodes.iter().map(|n| n.id.clone()).collect::<Vec<_>>();
            if let Err(error) =
                proxypool::prepare_proxy_nodes_transient(database, runtime, &ids).await
            {
                let message = format!("装载快节点失败：{error}");
                let log_id = append_charity_sync_log(
                    database,
                    &source.id,
                    &source.name,
                    stage,
                    "running",
                    &format!("{stage_label}失败：装载节点"),
                    "",
                    "",
                );
                finish_charity_sync_log(
                    &bus,
                    database,
                    log_id,
                    source,
                    stage,
                    "failed",
                    &message,
                    "",
                    feed_duration_ms(),
                    0,
                    0,
                    0,
                );
                return Err(message);
            }
        }
        Arc::new(Mutex::new(CharityNodeQueue::from_nodes(nodes)))
    };

    let mut last_error = String::new();
    let mut attempts = 0usize;
    let parallel_mode = shared_queue.is_some();

    while attempts < CHARITY_MAX_NODE_ATTEMPTS {
        if cancellation.is_cancelled() {
            if !parallel_mode {
                let _ = proxypool::restore_proxy_node_transient(database, runtime).await;
            }
            return Err(format!("{CHARITY_SYNC_CANCELLED_PREFIX}：{}", source.name));
        }

        let Some(node) = take_attempt_node(&monitor_state, &queue) else {
            break;
        };
        if monitor_state.is_banned(&node.id) {
            continue;
        }

        let attempt_guard = if parallel_mode {
            Some(tokio::select! {
                _ = cancellation.cancelled() => {
                    return Err(format!("{CHARITY_SYNC_CANCELLED_PREFIX}：{}", source.name));
                }
                guard = monitor_state.proxy_sync_lock.lock() => guard,
            })
        } else {
            None
        };
        if monitor_state.is_banned(&node.id) {
            drop(attempt_guard);
            continue;
        }
        attempts += 1;

        let attempt_started = Instant::now();
        let running_msg = format!(
            "{stage_label}进行中：{} · {}（{}ms）· 第{}次",
            source.name, node.name, node.latency_ms, attempts
        );
        let log_id = append_charity_sync_log(
            database,
            &source.id,
            &source.name,
            stage,
            "running",
            &running_msg,
            &node.name,
            "",
        );
        emit_running_progress(&bus, source, stage, &running_msg, &node.name);

        if cancellation.is_cancelled() {
            if !parallel_mode {
                let _ = proxypool::restore_proxy_node_transient(database, runtime).await;
            }
            let dur = attempt_started.elapsed().as_millis() as i64;
            let message = format!("{CHARITY_SYNC_CANCELLED_PREFIX}：{}", source.name);
            finish_charity_sync_log(
                &bus,
                database,
                log_id,
                source,
                stage,
                "cancelled",
                &message,
                &node.name,
                dur,
                0,
                0,
                0,
            );
            return Err(message);
        }

        if let Err(error) =
            proxypool::select_proxy_node_transient(database, runtime, &node.id).await
        {
            let message = format!("{}: 切换代理失败：{error}", node.name);
            eject_node_from_charity_candidate(&monitor_state, &queue, &node, &message);
            let dur = attempt_started.elapsed().as_millis() as i64;
            if !parallel_mode {
                let _ = proxypool::restore_proxy_node_transient(database, runtime).await;
            }
            finish_charity_sync_log(
                &bus, database, log_id, source, stage, "failed", &message, &node.name, dur, 0, 0, 0,
            );
            last_error = message;
            continue;
        }

        let mut waited = Duration::from_millis(0);
        while waited < Duration::from_millis(60) {
            if cancellation.is_cancelled() {
                break;
            }
            let slice = Duration::from_millis(30);
            tokio::time::sleep(slice).await;
            waited += slice;
        }

        let request_deadline = Instant::now() + CHARITY_REQUEST_TIMEOUT;
        let fetch_future = fetch_topic_body(ctx, client.clone(), source);
        tokio::pin!(fetch_future);
        let fetch_result = loop {
            if cancellation.is_cancelled() {
                break Err(format!("{CHARITY_SYNC_CANCELLED_PREFIX}：{}", source.name));
            }
            let left = request_deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break Err(format!(
                    "{}: 请求超时（单次请求 {}s）",
                    node.name,
                    CHARITY_REQUEST_TIMEOUT.as_secs()
                ));
            }
            let tick = left.min(Duration::from_millis(500));
            tokio::select! {
                _ = cancellation.cancelled() => {
                    break Err(format!("{CHARITY_SYNC_CANCELLED_PREFIX}：{}", source.name));
                }
                result = &mut fetch_future => {
                    break result;
                }
                _ = tokio::time::sleep(tick) => {
                    let dur = attempt_started.elapsed().as_millis() as i64;
                    let msg = format!(
                        "{stage_label}进行中：{} · {} · 已用 {:.1}s",
                        source.name,
                        node.name,
                        dur as f64 / 1000.0
                    );
                    if let Some(id) = log_id {
                        touch_running_charity_sync_log(database, id, &msg, &node.name, dur);
                    }
                    emit_running_progress(&bus, source, stage, &msg, &node.name);
                }
            }
        };
        drop(attempt_guard);

        match fetch_result {
            Ok((body, profile_name, account_name, protocol)) => {
                match items_from_topic_list(&body) {
                    Ok(items) => {
                        let persist_result = tokio::task::block_in_place(|| {
                            persist_feed(database, source, items, profile_name, account_name)
                        });
                        match persist_result {
                            Ok(mut result) => {
                                result.used_node_id = node.id.clone();
                                result.used_node_name = node.name.clone();
                                result.status = "success".into();
                                result.message = format!(
                                    "已通过 {}（{}ms，{}，第{}次尝试）同步 · 新增 {} 条 / 更新 {} 条",
                                    node.name,
                                    node.latency_ms,
                                    protocol,
                                    attempts,
                                    result.new_count,
                                    result.updated_count
                                );
                                let _ = write_feed_sync_meta(
                                    database,
                                    &source.id,
                                    "success",
                                    &result.message,
                                    &node.name,
                                    result.updated_count + result.new_count,
                                );
                                // 记住成功节点：后续采集默认继续用它，直到 4xx/5xx 等失败才切换
                                monitor_state.set_preferred_node(&node.id);
                                if let Ok(mut q) = queue.lock() {
                                    q.push_back_if_absent(node.clone());
                                }
                                let _ = proxypool::restore_proxy_node_transient(database, runtime)
                                    .await;
                                let dur = attempt_started.elapsed().as_millis() as i64;
                                finish_charity_sync_log(
                                    &bus,
                                    database,
                                    log_id,
                                    source,
                                    stage,
                                    "success",
                                    &result.message,
                                    &node.name,
                                    dur,
                                    result.new_count,
                                    result.updated_count,
                                    result.unread_count,
                                );
                                return Ok(result);
                            }
                            Err(error) => {
                                if let Ok(mut q) = queue.lock() {
                                    q.push_back_if_absent(node.clone());
                                }
                                let message = format!("{}: 入库失败：{error}", node.name);
                                let dur = attempt_started.elapsed().as_millis() as i64;
                                if !parallel_mode {
                                    let _ =
                                        proxypool::restore_proxy_node_transient(database, runtime)
                                            .await;
                                }
                                finish_charity_sync_log(
                                    &bus, database, log_id, source, stage, "failed", &message,
                                    &node.name, dur, 0, 0, 0,
                                );
                                last_error = message;
                            }
                        }
                    }
                    Err(error) => {
                        let raw = format!("{}: {error}", node.name);
                        eject_node_from_charity_candidate(&monitor_state, &queue, &node, &raw);
                        let message = if is_http_forbidden_error(&raw) {
                            format!("{}: HTTP 403，已从公益队列剔除", node.name)
                        } else if is_transport_error(&raw) {
                            format!("{}: 节点连接失败（已从公益队列剔除）：{}", node.name, raw)
                        } else {
                            raw
                        };
                        let dur = attempt_started.elapsed().as_millis() as i64;
                        if !parallel_mode {
                            let _ =
                                proxypool::restore_proxy_node_transient(database, runtime).await;
                        }
                        finish_charity_sync_log(
                            &bus, database, log_id, source, stage, "failed", &message, &node.name,
                            dur, 0, 0, 0,
                        );
                        last_error = message;
                    }
                }
            }
            Err(error) => {
                let cancelled = is_charity_sync_cancelled(&error);
                let raw = if error.contains(':') {
                    error
                } else {
                    format!("{}: {error}", node.name)
                };
                if !cancelled {
                    eject_node_from_charity_candidate(&monitor_state, &queue, &node, &raw);
                }
                let message = if !cancelled && is_http_forbidden_error(&raw) {
                    format!("{}: HTTP 403，已从公益队列剔除", node.name)
                } else if !cancelled && is_transport_error(&raw) {
                    format!("{}: 节点连接失败（已从公益队列剔除）：{}", node.name, raw)
                } else {
                    raw
                };
                let dur = attempt_started.elapsed().as_millis() as i64;
                let status = if cancelled { "cancelled" } else { "failed" };
                if !cancelled && !parallel_mode {
                    let _ = proxypool::restore_proxy_node_transient(database, runtime).await;
                }
                finish_charity_sync_log(
                    &bus, database, log_id, source, stage, status, &message, &node.name, dur, 0, 0,
                    0,
                );
                if cancelled {
                    if !parallel_mode {
                        let _ = proxypool::restore_proxy_node_transient(database, runtime).await;
                    }
                    return Err(message);
                }
                last_error = message;
            }
        }
    }

    if !parallel_mode {
        let _ = proxypool::restore_proxy_node_transient(database, runtime).await;
    }
    let mut local = tokio::task::block_in_place(|| {
        load_feed_items_from_db(database, source, 0, CHARITY_PAGE_SIZE, "", "all")
    })
    .unwrap_or(CharityFeedResult {
        feed_id: source.id.clone(),
        feed_name: source.name.clone(),
        items: Vec::new(),
        fetched_at: String::new(),
        changed: false,
        new_count: 0,
        updated_count: 0,
        initialized: false,
        source_profile_name: String::new(),
        source_account_name: String::new(),
        status: "error".into(),
        message: String::new(),
        used_node_id: String::new(),
        used_node_name: String::new(),
        unread_count: 0,
        skipped: false,
        total_count: 0,
        offset: 0,
        limit: CHARITY_PAGE_SIZE,
        has_more: false,
    });
    local.status = "error".into();
    local.message = if last_error.is_empty() {
        format!(
            "{} 同步失败：本轮没有可尝试的 ≤{}ms 候选节点",
            source.name, CHARITY_FAST_NODE_MAX_LATENCY_MS,
        )
    } else {
        format!(
            "{}（本轮已尝试 {attempts}/{} 个节点）",
            last_error, CHARITY_MAX_NODE_ATTEMPTS
        )
    };
    let _ = write_feed_sync_meta(database, &source.id, "error", &local.message, "", 0);
    Err(local.message.clone())
}
