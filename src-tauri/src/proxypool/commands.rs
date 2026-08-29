use crate::context::{AppContext, EventBus, Managed};
use crate::db::build_http_client;
use crate::models::*;
use crate::proxypool::geoip::{classify_node_location, find_geoip_database, open_geoip_reader};
use crate::proxypool::parser::{
    basic_node_config_error, parse_subscription, stable_id, validate_source,
};
use crate::proxypool::rotator::{
    list_channel_candidate_nodes, list_prioritized_fast_proxy_nodes, write_channel_node,
};
use crate::proxypool::runtime::{
    ensure_channel_instance, ensure_default_proxy_channel, ensure_global_runtime, ensure_runtime,
    load_state, row_subscription, runtime_nodes, runtime_proxy_url, select_group_node,
    select_runtime_node, write_meta,
};
use crate::proxypool::tester::{
    measure_get_probe, normalize_ignore_addresses, run_proxy_node_pool,
};
use crate::proxypool::types::*;
use rusqlite::{params, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::warn;
use url::Url;

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn get_proxy_pool_state(ctx: Managed<'_, Arc<AppContext>>) -> Result<ProxyPoolState, String> {
    let database = &*ctx.database;
    let runtime = &*ctx.proxy_runtime;
    load_state(database, runtime)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn save_proxy_subscription(
    ctx: Managed<'_, Arc<AppContext>>,
    id: Option<String>,
    name: String,
    url: String,
) -> Result<ProxySubscription, String> {
    let database = &*ctx.database;
    let name = name.trim();
    if name.is_empty() {
        return Err("请输入名称".into());
    }
    let source = validate_source(&url)?;
    let id = id
        .filter(|item| !item.trim().is_empty())
        .unwrap_or_else(|| stable_id(&["proxy-source", &source]));
    let connection = database.lock_conn()?;
    connection.execute(
        "INSERT INTO proxy_subscriptions (id, name, url, created_at, updated_at) VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, url = excluded.url, updated_at = CURRENT_TIMESTAMP",
        params![id, name, source],
    ).map_err(|error| error.to_string())?;
    connection.query_row("SELECT id, name, url, node_count, last_error, created_at, updated_at FROM proxy_subscriptions WHERE id = ?1", [&id], row_subscription).map_err(|error| error.to_string())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn delete_proxy_subscription(
    ctx: Managed<'_, Arc<AppContext>>,
    id: String,
) -> Result<ProxyPoolState, String> {
    let database = &*ctx.database;
    let runtime = &*ctx.proxy_runtime;
    let mut connection = database.lock_conn()?;
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
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
    let active = crate::db::read_meta_conn(&transaction, ACTIVE_PROXY_NODE_KEY)?;
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

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn refresh_proxy_subscription(
    ctx: Managed<'_, Arc<AppContext>>,
    id: String,
) -> Result<ProxyPoolRefreshResult, String> {
    let database = &*ctx.database;
    let runtime = &*ctx.proxy_runtime;
    let bus: EventBus = ctx.event_bus.clone();
    let emit_progress = |stage: &str,
                         status: &str,
                         message: String,
                         completed: usize,
                         total: usize,
                         added: usize,
                         discarded: usize| {
        bus.emit(
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
        let connection = database.lock_conn()?;
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
                // 部分订阅服务按 UA 区分客户端，非 Clash UA 会被拒绝（HTTP 404）。
                // 这里伪装成 Clash 内核 UA，确保能拉取到 Clash YAML / Base64 订阅内容。
                .header("User-Agent", "clash-verge/v2.2.3")
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
            let connection = database.lock_conn()?;
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
                warn!("OpenHub 刷新来源时过滤非法节点：{}：{}", node.name, error);
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

    let mut connection = database.lock_conn()?;
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "DELETE FROM proxy_subscription_nodes WHERE subscription_id = ?1",
            [&id],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute("DELETE FROM proxy_nodes WHERE subscription_id = ?1", [&id])
        .map_err(|error| error.to_string())?;

    let total = nodes.len();
    let mut added = 0usize;
    let geoip_reader = open_geoip_reader(&runtime);

    for (index, node) in nodes.into_iter().enumerate() {
        let (country_code, country_name, classification, primary_ip) =
            classify_node_location(&node.name, &node.server, node.port, geoip_reader.as_ref());
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO proxy_pool_nodes
             (id, name, proxy_type, server, port, cipher, udp, raw_json, country_code, country_name, classification, primary_ip, is_enabled, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
            params![
                node.id,
                node.name,
                node.proxy_type,
                node.server,
                node.port,
                node.cipher,
                if node.udp { 1 } else { 0 },
                node.raw_json.to_string(),
                country_code,
                country_name,
                classification,
                primary_ip
            ],
        ).map_err(|error| error.to_string())?;
        if inserted > 0 {
            added += 1;
        }
        transaction.execute(
            "INSERT OR IGNORE INTO proxy_subscription_nodes (subscription_id, node_id) VALUES (?1, ?2)",
            params![id, node.id],
        ).map_err(|error| error.to_string())?;

        if (index + 1) % 40 == 0 || index + 1 == total {
            emit_progress(
                "saving",
                "running",
                format!("正在写入数据库 ({}/{})…", index + 1, total),
                index + 1,
                total,
                added,
                discarded,
            );
        }
    }

    transaction.execute("DELETE FROM proxy_pool_nodes WHERE id NOT IN (SELECT node_id FROM proxy_subscription_nodes)", []).map_err(|error| error.to_string())?;
    let node_count: i64 = transaction
        .query_row(
            "SELECT COUNT(DISTINCT node_id) FROM proxy_subscription_nodes WHERE subscription_id = ?1",
            [&id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "UPDATE proxy_subscriptions SET node_count = ?2, last_error = '', updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
            params![id, node_count],
        )
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())?;
    drop(connection);

    if let Ok(mut state) = runtime.shared_instance.lock() {
        state.config_hash.clear();
    }
    let _ = tokio::task::block_in_place(|| ensure_runtime(&database, &runtime, None, None));

    let subscription = {
        let connection = database.lock_conn()?;
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

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn set_proxy_pool_settings(
    ctx: Managed<'_, Arc<AppContext>>,
    ignore_addresses: String,
) -> Result<ProxyPoolState, String> {
    let database = &*ctx.database;
    let runtime = &*ctx.proxy_runtime;
    let ignore = normalize_ignore_addresses(&ignore_addresses);
    let connection = database.lock_conn()?;
    write_meta(&connection, PROXY_IGNORE_KEY, &ignore)?;
    drop(connection);
    load_state(&database, &runtime)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn save_proxy_channel(
    ctx: Managed<'_, Arc<AppContext>>,
    id: Option<String>,
    name: String,
) -> Result<ProxyPoolState, String> {
    let database = &*ctx.database;
    let runtime = &*ctx.proxy_runtime;
    let name = name.trim();
    if name.is_empty() {
        return Err("请输入通道名称".into());
    }
    let channel_id = id
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| stable_id(&["proxy-channel", name]));
    let connection = database.lock_conn()?;
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

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn delete_proxy_channel(
    ctx: Managed<'_, Arc<AppContext>>,
    id: String,
) -> Result<ProxyPoolState, String> {
    let database = &*ctx.database;
    let runtime = &*ctx.proxy_runtime;
    let id = id.trim();
    if id == DEFAULT_PROXY_CHANNEL_ID {
        return Err("默认通道不能删除".into());
    }
    let connection = database.lock_conn()?;
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
    runtime.release_channel_lane(id);
    load_state(&database, &runtime)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn set_proxy_channel_node(
    ctx: Managed<'_, Arc<AppContext>>,
    channel_id: String,
    node_id: String,
) -> Result<ProxyPoolState, String> {
    let database = &*ctx.database;
    let runtime = &*ctx.proxy_runtime;
    let channel_id = channel_id.trim();
    let node_id = node_id.trim();
    if channel_id.is_empty() || node_id.is_empty() {
        return Err("通道或节点标识为空".into());
    }
    write_channel_node(&database, channel_id, node_id)?;
    let _ = ensure_channel_instance(&database, &runtime, channel_id);
    load_state(&database, &runtime)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn assign_account_proxy_channel(
    ctx: Managed<'_, Arc<AppContext>>,
    profile_id: String,
    channel_id: String,
) -> Result<ProxyPoolState, String> {
    let database = &*ctx.database;
    let runtime = &*ctx.proxy_runtime;
    let profile_id = profile_id.trim();
    let channel_id = channel_id.trim();
    if profile_id.is_empty() || channel_id.is_empty() {
        return Err("账号或通道标识为空".into());
    }
    let connection = database.lock_conn()?;
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
    // 账号改走通道出口，其独立 lane 映射已无意义，释放回池
    runtime.release_account_lane(profile_id);
    load_state(&database, &runtime)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn unassign_account_proxy_channel(
    ctx: Managed<'_, Arc<AppContext>>,
    profile_id: String,
) -> Result<ProxyPoolState, String> {
    let database = &*ctx.database;
    let runtime = &*ctx.proxy_runtime;
    let profile_id = profile_id.trim();
    if profile_id.is_empty() {
        return Err("账号标识为空".into());
    }
    let connection = database.lock_conn()?;
    connection
        .execute(
            "DELETE FROM account_proxy_channels WHERE profile_id = ?1",
            [profile_id],
        )
        .map_err(|error| error.to_string())?;
    drop(connection);
    // 解绑后账号下次出口请求会重新轮询分配独立 lane，这里先释放旧映射
    runtime.release_account_lane(profile_id);
    load_state(&database, &runtime)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn test_proxy_channel_nodes(
    ctx: Managed<'_, Arc<AppContext>>,
    channel_id: Option<String>,
    node_ids: Option<Vec<String>>,
) -> Result<ProxyPoolState, String> {
    let database = &*ctx.database;
    let runtime = &*ctx.proxy_runtime;
    let bus = ctx.event_bus.clone();
    let _ = channel_id;
    let requested = if let Some(ids) = node_ids.filter(|list| !list.is_empty()) {
        ids.into_iter()
            .filter(|id| !id.trim().is_empty())
            .collect::<HashSet<_>>()
    } else {
        let candidates = list_channel_candidate_nodes(&database, ACCOUNT_PROXY_MAX_LATENCY_MS)?;
        let candidates = if candidates.is_empty() {
            list_prioritized_fast_proxy_nodes(&database, ACCOUNT_PROXY_MAX_LATENCY_MS)?
        } else {
            candidates
        };
        if candidates.is_empty() {
            let (all_nodes, _) = runtime_nodes(&database, None)?;
            all_nodes.into_iter().map(|n| n.id).collect::<HashSet<_>>()
        } else {
            candidates
                .into_iter()
                .map(|(id, _, _)| id)
                .collect::<HashSet<_>>()
        }
    };

    if requested.is_empty() {
        return Err("代理池中没有可测试的节点，请先添加或启用节点".to_string());
    }

    run_proxy_node_pool(&bus, database, runtime, Some(requested), true)
        .await?;
    load_state(database, runtime)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn set_active_proxy_node(
    ctx: Managed<'_, Arc<AppContext>>,
    node_id: String,
) -> Result<ProxyPoolState, String> {
    let database = &*ctx.database;
    let runtime = &*ctx.proxy_runtime;
    let runtime_name = {
        let connection = database.lock_conn()?;
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
    // 全局单实例装载全量节点，激活节点只是切 OpenHub 组，无需按子集重建配置
    let _guard = runtime.runtime_op_lock.lock().await;
    tokio::task::block_in_place(|| ensure_global_runtime(&database, &runtime))?;
    select_runtime_node(&runtime, &runtime_name).await?;
    let proxy_url = runtime_proxy_url(&runtime);
    let connection = database.lock_conn()?;
    write_meta(&connection, ACTIVE_PROXY_NODE_KEY, &node_id)?;
    write_meta(&connection, NETWORK_PROXY_KEY, &proxy_url)?;
    drop(connection);
    load_state(&database, &runtime)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn clear_active_proxy_node(
    ctx: Managed<'_, Arc<AppContext>>,
) -> Result<ProxyPoolState, String> {
    let database = &*ctx.database;
    let runtime = &*ctx.proxy_runtime;
    let connection = database.lock_conn()?;
    write_meta(&connection, ACTIVE_PROXY_NODE_KEY, "")?;
    write_meta(&connection, NETWORK_PROXY_KEY, "")?;
    drop(connection);
    load_state(&database, &runtime)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn delete_invalid_proxy_nodes(
    ctx: Managed<'_, Arc<AppContext>>,
) -> Result<ProxyPoolState, String> {
    let database = &*ctx.database;
    let runtime = &*ctx.proxy_runtime;
    let connection = database.lock_conn()?;
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

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn test_proxy_node(
    ctx: Managed<'_, Arc<AppContext>>,
    node_id: String,
) -> Result<ProxyNode, String> {
    let database = &*ctx.database;
    let runtime = &*ctx.proxy_runtime;
    {
        let connection = database.lock_conn()?;
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
    }
    // 与批量测速互斥（共用固定 SPEED lane），不再拉起临时单节点实例。
    // 全局单实例装载全量节点且代理名即节点 id，切组直接寻址。
    let test_lease = runtime.start_proxy_test()?;
    let _op_guard = runtime.runtime_op_lock.lock().await;
    tokio::task::block_in_place(|| ensure_global_runtime(database, runtime))?;
    let lane = runtime.speed_lane_slot(0)?;
    select_group_node(runtime, &lane.group_name, &node_id).await?;
    // 延时与网速一体测（与批量同源）：同一条经 lane 的下载连接，
    // 响应头到达耗时 = 连通延迟，滑窗峰值吞吐 = 网速。
    let proxy_url = format!("http://127.0.0.1:{}", lane.listen_port);
    let (latency, speed_ms) =
        measure_get_probe(proxy_url, CHANNEL_SPEED_TEST_URL.to_string()).await;
    let status = if latency.is_some() { "success" } else { "error" };
    let error_message = if latency.is_some() {
        None
    } else {
        Some("测速失败：节点无法连通（探测请求未在预算内收到响应头）".to_string())
    };
    {
        let connection = database.lock_conn()?;
        connection
            .execute(
                "UPDATE proxy_pool_nodes SET latency_ms=?2, test_status=?3, channel_latency_ms=?4, channel_test_status=?5, channel_tested_at=CURRENT_TIMESTAMP, tested_at=CURRENT_TIMESTAMP WHERE id=?1",
                params![
                    node_id,
                    latency,
                    status,
                    speed_ms,
                    if speed_ms.is_some() { "success" } else { "error" }
                ],
            )
            .map_err(|error| error.to_string())?;
    }
    drop(_op_guard);
    drop(test_lease);
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

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn cancel_proxy_node_tests(ctx: Managed<'_, Arc<AppContext>>) -> Result<bool, String> {
    let runtime = &*ctx.proxy_runtime;
    match runtime.cancel_proxy_test() {
        Ok(v) => Ok(v),
        Err(error) => {
            warn!("OpenHub 取消测速内部警告：{error}");
            Ok(false)
        }
    }
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn test_all_proxy_nodes(
    ctx: Managed<'_, Arc<AppContext>>,
) -> Result<ProxyPoolState, String> {
    let bus = ctx.event_bus.clone();
    run_proxy_node_pool(&bus, &ctx.database, &ctx.proxy_runtime, None, false).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn test_proxy_nodes(
    ctx: Managed<'_, Arc<AppContext>>,
    node_ids: Vec<String>,
) -> Result<ProxyPoolState, String> {
    let requested = node_ids
        .into_iter()
        .filter(|id| !id.trim().is_empty())
        .collect::<HashSet<_>>();
    if requested.is_empty() {
        return Err("请选择需要测速的节点".into());
    }
    let bus = ctx.event_bus.clone();
    run_proxy_node_pool(
        &bus,
        &ctx.database,
        &ctx.proxy_runtime,
        Some(requested),
        false,
    )
    .await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn analyze_proxy_nodes(ctx: Managed<'_, Arc<AppContext>>) -> Result<ProxyIpAnalysis, String> {
    let database = &*ctx.database;
    let runtime = &*ctx.proxy_runtime;
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

    let missing = state
        .nodes
        .iter()
        .filter(|node| node.country_code.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        let connection = database.lock_conn()?;
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
