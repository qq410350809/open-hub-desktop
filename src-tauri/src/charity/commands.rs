use crate::charity::db::*;
use crate::charity::feed::charity_tag_json_url;
use crate::charity::fetcher::{is_charity_sync_cancelled, sync_feed_with_fast_nodes};
use crate::charity::types::*;
use crate::context::{AppContext, EventBus, Managed};
use rusqlite::params;
use std::sync::Arc;

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_charity_feed(
    ctx: Managed<'_, Arc<AppContext>>,
    feed_id: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
    keyword: Option<String>,
    filter: Option<String>,
) -> Result<CharityFeedResult, String> {
    let database = &*ctx.database;
    let requested = feed_id.as_deref().unwrap_or(DEFAULT_CHARITY_FEED_ID);
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(CHARITY_PAGE_SIZE);
    let keyword = keyword.unwrap_or_default();
    let filter = filter.unwrap_or_default();
    if requested == "all" {
        return tokio::task::block_in_place(|| {
            load_all_feed_items_from_db(&database, offset, limit, &keyword, &filter)
        });
    }
    let source = charity_feed_source(&database, requested)?;
    // 不再把后台轮询的 last_errors 贴到查询结果上：切标签是纯读库操作，
    // 附带上一轮同步的失败消息会让人误以为点击本身触发了网络请求。
    // 同步失败已由 charity_sync_logs 记录，前端从同步日志查看。
    let result = tokio::task::block_in_place(|| {
        load_feed_items_from_db(&database, &source, offset, limit, &keyword, &filter)
    })?;
    Ok(result)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn mark_charity_feed_read(
    ctx: Managed<'_, Arc<AppContext>>,
    feed_id: Option<String>,
) -> Result<usize, String> {
    let database = &*ctx.database;
    let requested = feed_id.as_deref().unwrap_or(DEFAULT_CHARITY_FEED_ID);
    tokio::task::block_in_place(|| {
        let now = {
            let connection = database.lock_db();
            connection
                .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|error| error.to_string())?
        };
        if requested == "all" {
            let mut total = 0usize;
            let sources = load_charity_sources(&database)?;
            for source in &sources {
                write_app_meta(&database, &feed_meta_keys(&source.id).read_at, &now)?;
                total += unread_count_for_feed(&database, &source.id)?;
            }
            return Ok(total);
        }
        let source = charity_feed_source(&database, requested)?;
        write_app_meta(&database, &feed_meta_keys(&source.id).read_at, &now)?;
        unread_count_for_feed(&database, &source.id)
    })
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_charity_today_count(ctx: Managed<'_, Arc<AppContext>>) -> Result<usize, String> {
    let database = &*ctx.database;
    tokio::task::block_in_place(|| {
        let (utc_start, utc_end) = local_day_utc_range_secs();
        let connection = database.lock_conn()?;
        let count = connection
            .query_row(
                "SELECT COUNT(DISTINCT guid) FROM charity_feed_items
                 WHERE published_at IS NOT NULL
                   AND unixepoch(published_at) >= ?1
                   AND unixepoch(published_at) < ?2",
                params![utc_start, utc_end],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?;
        Ok(count.max(0) as usize)
    })
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_charity_unread_total(ctx: Managed<'_, Arc<AppContext>>) -> Result<usize, String> {
    let database = &*ctx.database;
    tokio::task::block_in_place(|| {
        let mut total = 0usize;
        let sources = load_charity_sources(&database)?;
        for source in &sources {
            total += unread_count_for_feed(&database, &source.id)?;
        }
        Ok(total)
    })
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn fetch_charity_feed(
    ctx: Managed<'_, Arc<AppContext>>,
    feed_id: Option<String>,
) -> Result<CharityFeedResult, String> {
    let database = &*ctx.database;
    let bus: EventBus = ctx.event_bus.clone();
    let monitor = &ctx.charity_runtime;
    let source = charity_feed_source(
        &database,
        feed_id.as_deref().unwrap_or(DEFAULT_CHARITY_FEED_ID),
    )?;
    let Some(cancellation) = monitor.try_begin_sync() else {
        let mut local = tokio::task::block_in_place(|| {
            load_feed_items_from_db(&database, &source, 0, CHARITY_PAGE_SIZE, "", "all")
        })?;
        local.message = "后台同步进行中，已返回本地数据".into();
        local.status = if local.status.is_empty() {
            "local".into()
        } else {
            local.status
        };
        emit_charity_progress(
            &bus,
            CharitySyncProgress {
                feed_id: source.id.clone(),
                feed_name: source.name.clone(),
                stage: "manual".into(),
                status: "skipped".into(),
                message: local.message.clone(),
                used_node_id: String::new(),
                used_node_name: String::new(),
                new_count: 0,
                updated_count: 0,
                unread_count: local.unread_count,
            },
        );
        return Ok(local);
    };
    let sync_result = sync_feed_with_fast_nodes(
        &ctx,
        database,
        &ctx.proxy_runtime,
        &source,
        "manual",
        &cancellation,
        None,
        false,
    )
    .await;
    monitor.end_sync();
    match &sync_result {
        Ok(_) => {
            if let Ok(mut errors) = monitor.last_errors.lock() {
                errors.remove(&source.id);
            }
        }
        Err(error) => {
            if !is_charity_sync_cancelled(error) {
                if let Ok(mut errors) = monitor.last_errors.lock() {
                    errors.insert(source.id.to_string(), error.clone());
                }
            }
        }
    }
    let mut local = tokio::task::block_in_place(|| {
        load_feed_items_from_db(&database, &source, 0, CHARITY_PAGE_SIZE, "", "all")
    })?;
    if let Err(error) = sync_result {
        if local.message.is_empty() {
            local.message = error;
            local.status = "error".into();
        }
    }
    Ok(local)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_charity_proxy_pool_summary(
    ctx: Managed<'_, Arc<AppContext>>,
) -> Result<CharityProxyPoolSummary, String> {
    let database = &*ctx.database;
    tokio::task::block_in_place(|| {
        let connection = database.lock_conn()?;
        let valid_count = connection
            .query_row(
                "SELECT COUNT(*) FROM proxy_pool_nodes
                 WHERE test_status = 'success' AND latency_ms IS NOT NULL AND latency_ms > 0",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            .max(0) as usize;
        let candidate_count = connection
            .query_row(
                "SELECT COUNT(*) FROM proxy_pool_nodes
                 WHERE test_status = 'success' AND latency_ms IS NOT NULL AND latency_ms > 0
                   AND latency_ms <= ?1",
                [CHARITY_FAST_NODE_MAX_LATENCY_MS],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            .max(0) as usize;

        let banned = ctx.charity_runtime.active_banned_ids();
        if banned.is_empty() {
            return Ok(CharityProxyPoolSummary {
                valid_count,
                candidate_count,
            });
        }
        let placeholders = banned.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let mut params = banned
            .iter()
            .map(|id| rusqlite::types::Value::Text(id.clone()))
            .collect::<Vec<_>>();
        let valid_after = connection
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM proxy_pool_nodes
                     WHERE test_status = 'success' AND latency_ms IS NOT NULL AND latency_ms > 0
                       AND id IN ({placeholders})"
                ),
                rusqlite::params_from_iter(params.iter()),
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            .max(0) as usize;
        let mut candidate_params = Vec::with_capacity(params.len() + 1);
        candidate_params.push(rusqlite::types::Value::Integer(
            CHARITY_FAST_NODE_MAX_LATENCY_MS,
        ));
        candidate_params.append(&mut params);
        let candidate_after = connection
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM proxy_pool_nodes
                     WHERE test_status = 'success' AND latency_ms IS NOT NULL AND latency_ms > 0
                       AND latency_ms <= ?1
                       AND id IN ({placeholders})"
                ),
                rusqlite::params_from_iter(candidate_params.iter()),
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            .max(0) as usize;
        Ok(CharityProxyPoolSummary {
            valid_count: valid_count.saturating_sub(valid_after),
            candidate_count: candidate_count.saturating_sub(candidate_after),
        })
    })
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_charity_sync_logs(
    ctx: Managed<'_, Arc<AppContext>>,
    limit: Option<usize>,
) -> Result<Vec<CharitySyncLogEntry>, String> {
    let database = &*ctx.database;
    tokio::task::block_in_place(|| list_charity_sync_logs(&database, limit.unwrap_or(120)))
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn clear_charity_sync_logs(ctx: Managed<'_, Arc<AppContext>>) -> Result<(), String> {
    let database = &*ctx.database;
    tokio::task::block_in_place(|| clear_charity_sync_logs_db(&database))
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn set_charity_monitor_visible(
    ctx: Managed<'_, Arc<AppContext>>,
    visible: bool,
) -> Result<(), String> {
    ctx.charity_runtime.set_visible(visible);
    Ok(())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn request_charity_round(ctx: Managed<'_, Arc<AppContext>>) -> Result<(), String> {
    ctx.charity_runtime.request_round();
    Ok(())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn refresh_all_charity_feeds_impl(
    ctx: &Arc<AppContext>,
) -> Result<CharityRefreshAllResult, String> {
    let database = &*ctx.database;
    let monitor = &ctx.charity_runtime;
    let cancelled_active_round = monitor.cancel_active_sync();
    let cancelled_log_count = tokio::task::block_in_place(|| {
        cancel_running_charity_sync_logs(&database, "已被新的“立即刷新全部标签”任务取消")
    })?;
    monitor.request_round();
    Ok(CharityRefreshAllResult {
        cancelled_active_round,
        cancelled_log_count,
        feed_count: load_charity_sources(&database)
            .map(|v| v.len())
            .unwrap_or(0),
    })
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn list_charity_sources(
    ctx: Managed<'_, Arc<AppContext>>,
) -> Result<Vec<CharityFeedSource>, String> {
    let database = &*ctx.database;
    tokio::task::block_in_place(|| load_all_charity_sources(&database))
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn add_charity_source(
    ctx: Managed<'_, Arc<AppContext>>,
    id: String,
    name: String,
) -> Result<CharityFeedSource, String> {
    let database = &*ctx.database;
    let id = id.trim().to_string();
    let name = name.trim().to_string();
    if id.is_empty() || name.is_empty() {
        return Err("标签 ID 和名称不能为空".into());
    }
    let json_url = charity_tag_json_url(&id);
    tokio::task::block_in_place(|| {
        let connection = database.lock_db();
        let max_sort: i64 = connection
            .query_row(
                "SELECT COALESCE(MAX(sort_order), 0) FROM charity_feed_sources",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        connection
            .execute(
                "INSERT INTO charity_feed_sources (id, name, json_url, enabled, sort_order)
                 VALUES (?1, ?2, ?3, 1, ?4)",
                params![id, name, json_url, max_sort + 1],
            )
            .map_err(|error| format!("添加标签源失败：{error}"))?;
        Ok(CharityFeedSource {
            id,
            name,
            json_url,
            enabled: true,
            sort_order: max_sort + 1,
        })
    })
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn update_charity_source(
    ctx: Managed<'_, Arc<AppContext>>,
    id: String,
    name: Option<String>,
    enabled: Option<bool>,
) -> Result<(), String> {
    let database = &*ctx.database;
    tokio::task::block_in_place(|| {
        let connection = database.lock_db();
        if let Some(name) = name {
            let name = name.trim().to_string();
            if !name.is_empty() {
                connection
                    .execute(
                        "UPDATE charity_feed_sources SET name = ?2 WHERE id = ?1",
                        params![id, name],
                    )
                    .map_err(|error| error.to_string())?;
            }
        }
        if let Some(enabled) = enabled {
            connection
                .execute(
                    "UPDATE charity_feed_sources SET enabled = ?2 WHERE id = ?1",
                    params![id, enabled as i64],
                )
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    })
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn remove_charity_source(
    ctx: Managed<'_, Arc<AppContext>>,
    id: String,
) -> Result<usize, String> {
    let database = &*ctx.database;
    let removed_items = tokio::task::block_in_place(|| {
        let connection = database.lock_db();
        remove_charity_source_db(&connection, &id)
    })?;
    if let Ok(mut errors) = ctx.charity_runtime.last_errors.lock() {
        errors.remove(&id);
    }
    Ok(removed_items)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn refresh_all_charity_feeds(
    ctx: Managed<'_, Arc<AppContext>>,
) -> Result<CharityRefreshAllResult, String> {
    refresh_all_charity_feeds_impl(&ctx).await
}
