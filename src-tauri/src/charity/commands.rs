use crate::charity::db::*;
use crate::charity::feed::charity_tag_json_url;
use crate::charity::fetcher::{is_charity_sync_cancelled, sync_feed_with_fast_nodes};
use crate::charity::types::*;
use crate::models::Database;
use crate::proxypool::ProxyRuntime;
use rusqlite::params;
use tauri::{AppHandle, State};

#[tauri::command]
pub async fn get_charity_feed(
    database: State<'_, Database>,
    runtime: State<'_, CharityMonitorRuntime>,
    feed_id: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
    keyword: Option<String>,
) -> Result<CharityFeedResult, String> {
    let requested = feed_id.as_deref().unwrap_or(DEFAULT_CHARITY_FEED_ID);
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(CHARITY_PAGE_SIZE);
    let keyword = keyword.unwrap_or_default();
    if requested == "all" {
        return tokio::task::block_in_place(|| {
            load_all_feed_items_from_db(&database, offset, limit, &keyword)
        });
    }
    let source = charity_feed_source(&database, requested)?;
    let mut result = tokio::task::block_in_place(|| {
        load_feed_items_from_db(&database, &source, offset, limit, &keyword)
    })?;
    if let Ok(errors) = runtime.last_errors.lock() {
        if let Some(message) = errors.get(&source.id) {
            if result.message.is_empty() {
                result.message = message.clone();
                if result.status == "local" {
                    result.status = "error".into();
                }
            }
        }
    }
    Ok(result)
}

#[tauri::command]
pub async fn mark_charity_feed_read(
    database: State<'_, Database>,
    feed_id: Option<String>,
) -> Result<usize, String> {
    let requested = feed_id.as_deref().unwrap_or(DEFAULT_CHARITY_FEED_ID);
    tokio::task::block_in_place(|| {
        let now = {
            let connection = database
                .0
                .lock()
                .map_err(|_| "本地数据库锁定失败".to_string())?;
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

#[tauri::command]
pub async fn get_charity_today_count(database: State<'_, Database>) -> Result<usize, String> {
    tokio::task::block_in_place(|| {
        let (utc_start, utc_end) = local_day_utc_range_secs();
        let connection = database.lock_conn()?;
        let count = connection
            .query_row(
                "SELECT COUNT(*) FROM charity_feed_items
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

#[tauri::command]
pub async fn get_charity_unread_total(database: State<'_, Database>) -> Result<usize, String> {
    tokio::task::block_in_place(|| {
        let mut total = 0usize;
        let sources = load_charity_sources(&database)?;
        for source in &sources {
            total += unread_count_for_feed(&database, &source.id)?;
        }
        Ok(total)
    })
}

#[tauri::command]
pub async fn fetch_charity_feed(
    app: AppHandle,
    database: State<'_, Database>,
    runtime: State<'_, ProxyRuntime>,
    monitor: State<'_, CharityMonitorRuntime>,
    feed_id: Option<String>,
) -> Result<CharityFeedResult, String> {
    let source = charity_feed_source(
        &database,
        feed_id.as_deref().unwrap_or(DEFAULT_CHARITY_FEED_ID),
    )?;
    let Some(cancellation) = monitor.try_begin_sync() else {
        let mut local = tokio::task::block_in_place(|| {
            load_feed_items_from_db(&database, &source, 0, CHARITY_PAGE_SIZE, "")
        })?;
        local.message = "后台同步进行中，已返回本地数据".into();
        local.status = if local.status.is_empty() {
            "local".into()
        } else {
            local.status
        };
        emit_charity_progress(
            &app,
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
        &app,
        &database,
        &runtime,
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
        load_feed_items_from_db(&database, &source, 0, CHARITY_PAGE_SIZE, "")
    })?;
    if let Err(error) = sync_result {
        if local.message.is_empty() {
            local.message = error;
            local.status = "error".into();
        }
    }
    Ok(local)
}

#[tauri::command]
pub async fn get_charity_proxy_pool_summary(
    database: State<'_, Database>,
    monitor: State<'_, CharityMonitorRuntime>,
) -> Result<CharityProxyPoolSummary, String> {
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

        let banned = monitor.active_banned_ids();
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

#[tauri::command]
pub async fn get_charity_sync_logs(
    database: State<'_, Database>,
    limit: Option<usize>,
) -> Result<Vec<CharitySyncLogEntry>, String> {
    tokio::task::block_in_place(|| list_charity_sync_logs(&database, limit.unwrap_or(120)))
}

#[tauri::command]
pub async fn clear_charity_sync_logs(database: State<'_, Database>) -> Result<(), String> {
    tokio::task::block_in_place(|| clear_charity_sync_logs_db(&database))
}

#[tauri::command]
pub fn set_charity_monitor_visible(
    monitor: State<'_, CharityMonitorRuntime>,
    visible: bool,
) -> Result<(), String> {
    monitor.set_visible(visible);
    Ok(())
}

#[tauri::command]
pub fn request_charity_round(monitor: State<'_, CharityMonitorRuntime>) -> Result<(), String> {
    monitor.request_round();
    Ok(())
}

#[tauri::command]
pub async fn refresh_all_charity_feeds(
    database: State<'_, Database>,
    monitor: State<'_, CharityMonitorRuntime>,
) -> Result<CharityRefreshAllResult, String> {
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

#[tauri::command]
pub async fn list_charity_sources(
    database: State<'_, Database>,
) -> Result<Vec<CharityFeedSource>, String> {
    tokio::task::block_in_place(|| load_all_charity_sources(&database))
}

#[tauri::command]
pub async fn add_charity_source(
    database: State<'_, Database>,
    id: String,
    name: String,
    json_url: Option<String>,
) -> Result<CharityFeedSource, String> {
    let id = id.trim().to_string();
    let name = name.trim().to_string();
    if id.is_empty() || name.is_empty() {
        return Err("标签 ID 和名称不能为空".into());
    }
    let json_url = json_url
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| charity_tag_json_url(&id));
    tokio::task::block_in_place(|| {
        let connection = database
            .0
            .lock()
            .map_err(|_| "本地数据库锁定失败".to_string())?;
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

#[tauri::command]
pub async fn update_charity_source(
    database: State<'_, Database>,
    id: String,
    name: Option<String>,
    json_url: Option<String>,
    enabled: Option<bool>,
) -> Result<(), String> {
    tokio::task::block_in_place(|| {
        let connection = database
            .0
            .lock()
            .map_err(|_| "本地数据库锁定失败".to_string())?;
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
        if let Some(json_url) = json_url {
            let json_url = json_url.trim().to_string();
            if !json_url.is_empty() {
                connection
                    .execute(
                        "UPDATE charity_feed_sources SET json_url = ?2 WHERE id = ?1",
                        params![id, json_url],
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

#[tauri::command]
pub async fn remove_charity_source(
    database: State<'_, Database>,
    id: String,
) -> Result<(), String> {
    tokio::task::block_in_place(|| {
        let connection = database
            .0
            .lock()
            .map_err(|_| "本地数据库锁定失败".to_string())?;
        connection
            .execute(
                "DELETE FROM charity_feed_sources WHERE id = ?1",
                params![id],
            )
            .map_err(|error| format!("删除标签源失败：{error}"))?;
        Ok(())
    })
}
