use crate::charity::types::*;
use crate::context::EventBus;
use crate::models::Database;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;

pub fn load_charity_sources(database: &Database) -> Result<Vec<CharityFeedSource>, String> {
    let connection = database.lock_db();
    let mut statement = connection
        .prepare(
            "SELECT id, name, json_url, enabled, sort_order, upstream_protocol FROM charity_feed_sources
             WHERE enabled = 1 ORDER BY sort_order, id",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok(CharityFeedSource {
                id: row.get(0)?,
                name: row.get(1)?,
                json_url: row.get(2)?,
                enabled: row.get::<_, i64>(3).unwrap_or(1) != 0,
                sort_order: row.get(4).unwrap_or(0),
                upstream_protocol: row.get(5).ok(),
            })
        })
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

pub fn load_all_charity_sources(database: &Database) -> Result<Vec<CharityFeedSource>, String> {
    let connection = database.lock_db();
    let mut statement = connection
        .prepare(
            "SELECT id, name, json_url, enabled, sort_order, upstream_protocol FROM charity_feed_sources
             ORDER BY sort_order, id",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok(CharityFeedSource {
                id: row.get(0)?,
                name: row.get(1)?,
                json_url: row.get(2)?,
                enabled: row.get::<_, i64>(3).unwrap_or(1) != 0,
                sort_order: row.get(4).unwrap_or(0),
                upstream_protocol: row.get(5).ok(),
            })
        })
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

pub fn charity_feed_source(
    database: &Database,
    feed_id: &str,
) -> Result<CharityFeedSource, String> {
    let feed_id = feed_id.trim();
    let connection = database.lock_db();
    connection
        .query_row(
            "SELECT id, name, json_url, enabled, sort_order, upstream_protocol FROM charity_feed_sources WHERE id = ?1",
            params![feed_id],
            |row| {
                Ok(CharityFeedSource {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    json_url: row.get(2)?,
                    enabled: row.get::<_, i64>(3).unwrap_or(1) != 0,
                    sort_order: row.get(4).unwrap_or(0),
                    upstream_protocol: row.get(5).ok(),
                })
            },
        )
        .map_err(|_| format!("不支持的 Linux.do 标签：{feed_id}"))
}

/// 删除标签源：独属于该标签的帖子一并删除，仍属于其他订阅标签的帖子保留。
/// 返回被连带删除的帖子行数。
pub fn remove_charity_source_db(connection: &Connection, feed_id: &str) -> Result<usize, String> {
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| error.to_string())?;
    let removed_items = transaction
        .execute(
            "DELETE FROM charity_feed_items
             WHERE feed_id = ?1
               AND guid NOT IN (
                 SELECT guid FROM charity_feed_items WHERE feed_id <> ?1
               )",
            params![feed_id],
        )
        .map_err(|error| format!("删除标签帖子失败：{error}"))?;
    transaction
        .execute(
            "DELETE FROM charity_feed_sources WHERE id = ?1",
            params![feed_id],
        )
        .map_err(|error| format!("删除标签源失败：{error}"))?;
    let keys = feed_meta_keys(feed_id);
    for key in [
        keys.initialized,
        keys.source_url,
        keys.fetched_at,
        keys.read_at,
        keys.last_status,
        keys.last_message,
        keys.last_node,
        keys.last_updated,
    ] {
        let _ = transaction.execute("DELETE FROM app_meta WHERE key = ?1", params![key]);
    }
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(removed_items)
}

pub fn append_charity_sync_log(
    database: &Database,
    feed_id: &str,
    feed_name: &str,
    stage: &str,
    status: &str,
    message: &str,
    node_name: &str,
    detail_json: &str,
) -> Option<i64> {
    let connection = database.lock_db();
    if connection
        .execute(
            "INSERT INTO charity_sync_logs
             (feed_id, feed_name, stage, status, message, node_name, duration_ms, detail_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)",
            params![
                feed_id,
                feed_name,
                stage,
                status,
                message,
                node_name,
                detail_json
            ],
        )
        .is_err()
    {
        return None;
    }
    let id = connection.last_insert_rowid();
    let _ = connection.execute(
        "DELETE FROM charity_sync_logs
         WHERE id NOT IN (
           SELECT id FROM charity_sync_logs
           ORDER BY created_at DESC, id DESC
           LIMIT ?1
         )",
        params![CHARITY_SYNC_LOG_LIMIT as i64],
    );
    Some(id)
}

pub fn update_charity_sync_log(
    database: &Database,
    id: i64,
    status: &str,
    message: &str,
    node_name: &str,
    duration_ms: i64,
    detail_json: &str,
) {
    let connection = database.lock_db();
    let _ = connection.execute(
        "UPDATE charity_sync_logs
         SET status = ?1, message = ?2, node_name = ?3, duration_ms = ?4, detail_json = ?5
         WHERE id = ?6 AND status = 'running'",
        params![status, message, node_name, duration_ms, detail_json, id],
    );
}

pub fn touch_running_charity_sync_log(
    database: &Database,
    id: i64,
    message: &str,
    node_name: &str,
    duration_ms: i64,
) {
    let connection = database.lock_db();
    let _ = connection.execute(
        "UPDATE charity_sync_logs
         SET message = ?1, node_name = ?2, duration_ms = ?3
         WHERE id = ?4 AND status = 'running'",
        params![message, node_name, duration_ms, id],
    );
}

pub fn emit_running_progress(
    bus: &EventBus,
    source: &CharityFeedSource,
    stage: &str,
    message: &str,
    node_name: &str,
) {
    emit_charity_progress(
        bus,
        CharitySyncProgress {
            feed_id: source.id.clone(),
            feed_name: source.name.clone(),
            stage: stage.into(),
            status: "running".into(),
            message: message.into(),
            used_node_id: String::new(),
            used_node_name: node_name.into(),
            new_count: 0,
            updated_count: 0,
            unread_count: 0,
        },
    );
}

pub fn emit_charity_progress(bus: &EventBus, progress: CharitySyncProgress) {
    bus.emit("charity-sync-progress", progress);
}

/// 发送公益监听新消息通知（语音提醒 + 系统通知）
pub fn emit_charity_new_message_notification(
    bus: &EventBus,
    feed_name: &str,
    new_count: usize,
    updated_count: usize,
) {
    if new_count > 0 || updated_count > 0 {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let notification = serde_json::json!({
            "feedName": feed_name,
            "newCount": new_count,
            "updatedCount": updated_count,
            "timestamp": timestamp,
        });
        bus.emit("charity-new-message", notification);
    }
}

pub fn finish_charity_sync_log(
    bus: &EventBus,
    database: &Database,
    log_id: Option<i64>,
    source: &CharityFeedSource,
    stage: &str,
    status: &str,
    message: &str,
    node_name: &str,
    duration_ms: i64,
    new_count: usize,
    updated_count: usize,
    unread_count: usize,
) {
    // 同步成功且有新消息时，发送通知
    if status == "success" && (new_count > 0 || updated_count > 0) {
        emit_charity_new_message_notification(bus, &source.name, new_count, updated_count);
    }

    if let Some(id) = log_id {
        let detail_json = serde_json::json!({
            "new": new_count,
            "updated": updated_count,
            "unread": unread_count,
        })
        .to_string();
        update_charity_sync_log(
            database,
            id,
            status,
            message,
            node_name,
            duration_ms,
            &detail_json,
        );
    }
    emit_charity_progress(
        bus,
        CharitySyncProgress {
            feed_id: source.id.clone(),
            feed_name: source.name.clone(),
            stage: stage.into(),
            status: status.into(),
            message: message.into(),
            used_node_id: String::new(),
            used_node_name: node_name.into(),
            new_count,
            updated_count,
            unread_count,
        },
    );
}

pub fn list_charity_sync_logs(
    database: &Database,
    limit: usize,
) -> Result<Vec<CharitySyncLogEntry>, String> {
    let limit = limit.clamp(1, CHARITY_SYNC_LOG_LIMIT);
    let connection = database.lock_conn()?;
    let mut statement = connection
        .prepare(
            "SELECT id, created_at, feed_id, feed_name, stage, status, message, node_name, duration_ms, detail_json
             FROM charity_sync_logs
             ORDER BY created_at DESC, id DESC
             LIMIT ?1",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![limit as i64], |row| {
            let detail_raw: String = row.get(9)?;
            let detail = if detail_raw.trim().is_empty() {
                None
            } else {
                serde_json::from_str::<serde_json::Value>(&detail_raw).ok()
            };
            Ok(CharitySyncLogEntry {
                id: row.get(0)?,
                at: row.get(1)?,
                feed_id: row.get(2)?,
                feed_name: row.get(3)?,
                stage: row.get(4)?,
                status: row.get(5)?,
                message: row.get(6)?,
                node_name: row.get(7)?,
                duration_ms: row.get(8)?,
                detail,
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    Ok(rows)
}

pub fn persist_feed(
    database: &Database,
    source: &CharityFeedSource,
    mut items: Vec<CharityFeedItem>,
    source_profile_name: String,
    source_account_name: String,
) -> Result<CharityFeedResult, String> {
    let keys = feed_meta_keys(&source.id);
    let initialized_key = keys.initialized.clone();
    let source_key = keys.source_url.clone();
    let fetched_key = keys.fetched_at.clone();
    let mut connection = database.lock_conn()?;
    let initialized = connection
        .query_row(
            "SELECT value FROM app_meta WHERE key = ?1",
            [&initialized_key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| error.to_string())?
        .is_some();
    let existing = {
        let mut statement = connection
            .prepare(
                "SELECT guid, title, link, published_at
                 FROM charity_feed_items WHERE feed_id = ?1",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([source.id.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    (
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ),
                ))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<HashMap<_, _>, _>>()
            .map_err(|error| error.to_string())?;
        rows
    };
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    let mut new_count = 0;
    let mut updated_count = 0;
    for item in &mut items {
        if let Some((title, link, published_at)) = existing.get(&item.id) {
            if title != &item.title || link != &item.link || published_at != &item.published_at {
                updated_count += 1;
            }
        } else if initialized {
            item.is_new = true;
            new_count += 1;
        }
        transaction
            .execute(
                "INSERT INTO charity_feed_items
                 (feed_id, guid, title, link, author, published_at, summary, categories,
                  reply_count, views, like_count, last_activity_at, pinned, posters,
                  first_seen_at, last_seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
                 ON CONFLICT(feed_id, guid) DO UPDATE SET
                   title = excluded.title,
                   link = excluded.link,
                   author = excluded.author,
                   published_at = excluded.published_at,
                   summary = excluded.summary,
                   categories = excluded.categories,
                   reply_count = excluded.reply_count,
                   views = excluded.views,
                   like_count = excluded.like_count,
                   last_activity_at = excluded.last_activity_at,
                   pinned = excluded.pinned,
                   posters = excluded.posters,
                   last_seen_at = CURRENT_TIMESTAMP",
                params![
                    source.id,
                    item.id,
                    item.title,
                    item.link,
                    item.author,
                    item.published_at,
                    item.summary,
                    serde_json::to_string(&item.categories).map_err(|error| error.to_string())?,
                    item.reply_count,
                    item.views,
                    item.like_count,
                    item.last_activity_at,
                    item.pinned,
                    serde_json::to_string(&item.posters).map_err(|error| error.to_string())?,
                ],
            )
            .map_err(|error| error.to_string())?;
    }
    let read_key = keys.read_at.clone();
    if !initialized {
        transaction
            .execute(
                "INSERT OR REPLACE INTO app_meta (key, value) VALUES (?1, CURRENT_TIMESTAMP)",
                params![read_key],
            )
            .map_err(|error| error.to_string())?;
    }
    transaction
        .execute(
            "INSERT OR REPLACE INTO app_meta (key, value) VALUES
             (?1, '1'), (?2, ?3), (?4, CURRENT_TIMESTAMP)",
            params![initialized_key, source_key, source.json_url, fetched_key],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "DELETE FROM charity_feed_items
             WHERE feed_id = ?1 AND guid NOT IN (
               SELECT guid FROM charity_feed_items
               WHERE feed_id = ?1
               ORDER BY last_seen_at DESC, rowid DESC LIMIT 120
             )",
            [source.id.as_str()],
        )
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())?;
    let fetched_at = connection
        .query_row(
            "SELECT value FROM app_meta WHERE key = ?1",
            [&fetched_key],
            |row| row.get::<_, String>(0),
        )
        .map_err(|error| error.to_string())?;
    let unread_count = {
        let read_at: String = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key = ?1",
                [&keys.read_at],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .unwrap_or_default();
        if read_at.trim().is_empty() {
            0usize
        } else {
            connection
                .query_row(
                    "SELECT COUNT(*) FROM charity_feed_items
                     WHERE feed_id = ?1 AND first_seen_at > ?2",
                    params![source.id.as_str(), read_at],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| error.to_string())?
                .max(0) as usize
        }
    };
    Ok(CharityFeedResult {
        feed_id: source.id.clone(),
        feed_name: source.name.clone(),
        items,
        fetched_at,
        changed: new_count > 0 || updated_count > 0,
        new_count,
        updated_count,
        initialized,
        source_profile_name,
        source_account_name,
        status: "success".into(),
        message: String::new(),
        used_node_id: String::new(),
        used_node_name: String::new(),
        unread_count,
        skipped: false,
        total_count: 0,
        offset: 0,
        limit: CHARITY_PAGE_SIZE,
        has_more: false,
    })
}

/// 属性快捷筛选下推为 SQL 条件；today 内嵌本地日界的 i64 时间戳（无注入风险）。
pub fn charity_filter_clause(filter: &str) -> String {
    match filter {
        "hot" => " AND (reply_count >= 20 OR views >= 500)".into(),
        "pinned" => " AND pinned = 1".into(),
        "today" => {
            let (utc_start, utc_end) = local_day_utc_range_secs();
            format!(
                " AND published_at IS NOT NULL
                   AND unixepoch(published_at) >= {utc_start}
                   AND unixepoch(published_at) < {utc_end}"
            )
        }
        _ => String::new(),
    }
}

/// 前端列 key → 数据库排序列。白名单映射，杜绝把任意字符串拼进 ORDER BY。
fn charity_sort_column(key: &str) -> &'static str {
    match key {
        "title" => "title COLLATE NOCASE",
        "author" => "author COLLATE NOCASE",
        "replyCount" => "reply_count",
        "views" => "views",
        "lastActivityAt" => "last_activity_at",
        _ => "published_at",
    }
}

fn charity_sort_direction(order: &str) -> &'static str {
    if order.eq_ignore_ascii_case("asc") {
        "ASC"
    } else {
        "DESC"
    }
}

pub fn load_all_feed_items_from_db(
    database: &Database,
    offset: usize,
    limit: usize,
    keyword: &str,
    filter: &str,
    sort_by: &str,
    sort_order: &str,
) -> Result<CharityFeedResult, String> {
    let limit = limit.clamp(1, CHARITY_PAGE_LIMIT_MAX);
    let filter_clause = charity_filter_clause(filter);
    let order_clause = format!(
        "{} {}, rowid DESC",
        charity_sort_column(sort_by),
        charity_sort_direction(sort_order)
    );
    let connection = database.lock_conn()?;
    // 「全部」是前端聚合的虚拟标签，没有自己的 meta 键；取各真实 feed 里最新的同步时间。
    let fetched_at = connection
        .query_row(
            "SELECT COALESCE(MAX(value), '') FROM app_meta
             WHERE key LIKE 'charity_feed_last_fetched_at:%'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map_err(|error| error.to_string())?;
    let key_pat = format!("%{}%", keyword.trim());
    let has_key = !keyword.trim().is_empty();
    let total_count = if has_key {
        connection
            .query_row(
                &format!(
                    "SELECT COUNT(DISTINCT guid) FROM charity_feed_items
                     WHERE title LIKE ?1 OR author LIKE ?1 OR categories LIKE ?1{filter_clause}"
                ),
                [&key_pat],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            .max(0) as usize
    } else {
        connection
            .query_row(
                &format!(
                    "SELECT COUNT(DISTINCT guid) FROM charity_feed_items WHERE 1 = 1{filter_clause}"
                ),
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            .max(0) as usize
    };

    let mut statement = connection
        .prepare(
            &format!(
                "SELECT guid, title, link, author, published_at, summary, categories, first_seen_at,
                        reply_count, views, like_count, last_activity_at, pinned, posters, feed_id
                 FROM charity_feed_items
                 WHERE (?3 = '' OR title LIKE ?3 OR author LIKE ?3 OR categories LIKE ?3){filter_clause}
                 ORDER BY {order_clause}
                 LIMIT ?1 OFFSET ?2"
            ),
        )
        .map_err(|error| error.to_string())?;
    let all = statement
        .query_map(params![(limit * 8) as i64, offset as i64, key_pat], |row| {
            let categories: String = row.get(6)?;
            let posters_raw: String = row.get(13)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                serde_json::from_str::<Vec<String>>(&categories).unwrap_or_default(),
                row.get::<_, String>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, i64>(12)?,
                serde_json::from_str::<Vec<String>>(&posters_raw).unwrap_or_default(),
                row.get::<_, String>(14)?,
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    drop(statement);
    drop(connection);

    let mut merged: Vec<CharityFeedItem> = Vec::new();
    for (
        guid,
        title,
        link,
        author,
        published_at,
        summary,
        categories,
        _first_seen_at,
        reply,
        views,
        likes,
        last_activity,
        pinned,
        posters,
        feed_id,
    ) in all
    {
        let feed_name = {
            let conn = database.lock_db();
            conn.query_row(
                "SELECT name FROM charity_feed_sources WHERE id = ?1",
                params![&feed_id],
                |row| row.get::<_, String>(0),
            )
            .unwrap_or_default()
        };
        if let Some(item) = merged.iter_mut().find(|existing| existing.id == guid) {
            if !item.feed_ids.iter().any(|id| id == &feed_id) {
                item.feed_ids.push(feed_id);
                item.feed_names.push(feed_name);
            }
            item.views = item.views.max(views);
            item.reply_count = item.reply_count.max(reply);
            item.like_count = item.like_count.max(likes);
            continue;
        }
        merged.push(CharityFeedItem {
            id: guid,
            title,
            link,
            author,
            published_at,
            summary,
            categories,
            is_new: false,
            reply_count: reply,
            views,
            like_count: likes,
            last_activity_at: last_activity,
            pinned: pinned != 0,
            posters,
            feed_ids: vec![feed_id],
            feed_names: vec![feed_name],
        });
    }

    let items = merged.into_iter().take(limit).collect::<Vec<_>>();
    let item_count = items.len();

    Ok(CharityFeedResult {
        feed_id: "all".into(),
        feed_name: "全部".into(),
        items,
        fetched_at,
        changed: false,
        new_count: 0,
        updated_count: 0,
        initialized: true,
        source_profile_name: String::new(),
        source_account_name: String::new(),
        status: "local".into(),
        message: String::new(),
        used_node_id: String::new(),
        used_node_name: String::new(),
        unread_count: 0,
        skipped: false,
        total_count,
        offset,
        limit,
        has_more: offset + item_count < total_count,
    })
}

pub fn load_feed_items_from_db(
    database: &Database,
    source: &CharityFeedSource,
    offset: usize,
    limit: usize,
    keyword: &str,
    filter: &str,
    sort_by: &str,
    sort_order: &str,
) -> Result<CharityFeedResult, String> {
    let limit = limit.clamp(1, CHARITY_PAGE_LIMIT_MAX);
    let keys = feed_meta_keys(&source.id);
    let connection = database.lock_conn()?;
    let read_meta =
        |key: &str| -> Result<String, String> { crate::db::read_meta_conn(&connection, key) };
    let initialized = !read_meta(&keys.initialized)?.is_empty();
    let fetched_at = read_meta(&keys.fetched_at)?;
    let read_at = read_meta(&keys.read_at)?;
    let last_status = read_meta(&keys.last_status)?;
    let last_message = read_meta(&keys.last_message)?;
    let last_node = read_meta(&keys.last_node)?;
    let last_updated = read_meta(&keys.last_updated)?.parse::<usize>().unwrap_or(0);
    let key_pat = format!("%{}%", keyword.trim());
    let has_key = !keyword.trim().is_empty();
    let filter_clause = charity_filter_clause(filter);
    let order_clause = format!(
        "{} {}, rowid DESC",
        charity_sort_column(sort_by),
        charity_sort_direction(sort_order)
    );
    let total_count = if has_key {
        connection
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM charity_feed_items
                     WHERE feed_id = ?1 AND (title LIKE ?2 OR author LIKE ?2 OR categories LIKE ?2){filter_clause}"
                ),
                params![source.id.as_str(), key_pat],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            .max(0) as usize
    } else {
        connection
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM charity_feed_items WHERE feed_id = ?1{filter_clause}"
                ),
                [source.id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            .max(0) as usize
    };
    let unread_count = if read_at.trim().is_empty() {
        0usize
    } else {
        connection
            .query_row(
                "SELECT COUNT(*) FROM charity_feed_items
                 WHERE feed_id = ?1 AND first_seen_at > ?2",
                params![source.id.as_str(), read_at],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            .max(0) as usize
    };
    let mut statement = connection
        .prepare(&format!(
            "SELECT guid, title, link, author, published_at, summary, categories, first_seen_at,
                    reply_count, views, like_count, last_activity_at, pinned, posters
             FROM charity_feed_items
             WHERE feed_id = ?1 AND (?4 = '' OR title LIKE ?4 OR author LIKE ?4 OR categories LIKE ?4){filter_clause}
             ORDER BY {order_clause}
             LIMIT ?2 OFFSET ?3"
        ))
        .map_err(|error| error.to_string())?;
    let items = statement
        .query_map(
            params![source.id.as_str(), limit as i64, offset as i64, key_pat],
            |row| {
                let categories: String = row.get(6)?;
                let first_seen_at: String = row.get(7)?;
                let posters_raw: String = row.get(13)?;
                let parsed_categories = if categories.is_empty() || categories == "[]" {
                    Vec::new()
                } else {
                    serde_json::from_str::<Vec<String>>(&categories).unwrap_or_default()
                };
                let parsed_posters = if posters_raw.is_empty() || posters_raw == "[]" {
                    Vec::new()
                } else {
                    serde_json::from_str::<Vec<String>>(&posters_raw).unwrap_or_default()
                };
                let is_new = initialized && !read_at.trim().is_empty() && first_seen_at > read_at;
                Ok(CharityFeedItem {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    link: row.get(2)?,
                    author: row.get(3)?,
                    published_at: row.get(4)?,
                    summary: row.get(5)?,
                    categories: parsed_categories,
                    feed_ids: vec![source.id.to_string()],
                    feed_names: vec![source.name.to_string()],
                    is_new,
                    reply_count: row.get(8)?,
                    views: row.get(9)?,
                    like_count: row.get(10)?,
                    last_activity_at: row.get(11)?,
                    pinned: row.get::<_, i64>(12)? != 0,
                    posters: parsed_posters,
                })
            },
        )
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    drop(statement);
    drop(connection);
    let has_more = offset + items.len() < total_count;
    let status = if last_status.is_empty() {
        "local".to_string()
    } else {
        last_status
    };
    let skipped = status == "skipped";
    Ok(CharityFeedResult {
        feed_id: source.id.clone(),
        feed_name: source.name.clone(),
        items,
        fetched_at,
        changed: false,
        new_count: 0,
        updated_count: last_updated,
        initialized,
        source_profile_name: String::new(),
        source_account_name: String::new(),
        status,
        message: last_message,
        used_node_id: String::new(),
        used_node_name: last_node,
        unread_count,
        skipped,
        total_count,
        offset,
        limit,
        has_more,
    })
}

pub fn cancel_running_charity_sync_logs(
    database: &Database,
    reason: &str,
) -> Result<usize, String> {
    let connection = database.lock_conn()?;
    connection
        .execute(
            "UPDATE charity_sync_logs
             SET status = 'cancelled',
                 message = ?1,
                 duration_ms = MAX(
                   duration_ms,
                   CAST((julianday('now') - julianday(created_at)) * 86400000 AS INTEGER)
                 )
             WHERE status = 'running'",
            [reason],
        )
        .map_err(|error| error.to_string())
}

pub fn clear_charity_sync_logs_db(database: &Database) -> Result<(), String> {
    let connection = database.lock_conn()?;
    connection
        .execute("DELETE FROM charity_sync_logs", [])
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub fn abandon_running_charity_sync_logs(database: &Database) {
    let connection = database.lock_db();
    let _ = connection.execute(
        "UPDATE charity_sync_logs
         SET status = 'failed',
             message = CASE
               WHEN trim(message) = '' THEN '应用重启，任务已中断'
               ELSE message || '（应用重启，任务已中断）'
             END,
             duration_ms = CASE WHEN duration_ms > 0 THEN duration_ms ELSE 0 END
         WHERE status = 'running'",
        [],
    );
}

pub fn write_feed_sync_meta(
    database: &Database,
    feed_id: &str,
    status: &str,
    message: &str,
    node_name: &str,
    updated_count: usize,
) -> Result<(), String> {
    let keys = feed_meta_keys(feed_id);
    let connection = database.lock_conn()?;
    for (key, value) in [
        (keys.last_status, status.to_string()),
        (keys.last_message, message.to_string()),
        (keys.last_node, node_name.to_string()),
        (keys.last_updated, updated_count.to_string()),
    ] {
        crate::db::write_meta(&connection, &key, &value)?;
    }
    Ok(())
}

pub fn read_app_meta(database: &Database, key: &str) -> Result<String, String> {
    crate::db::read_meta(database, key)
}

pub fn write_app_meta(database: &Database, key: &str, value: &str) -> Result<(), String> {
    let connection = database.lock_conn()?;
    crate::db::write_meta(&connection, key, value)
}

pub fn unread_count_for_feed(database: &Database, feed_id: &str) -> Result<usize, String> {
    let keys = feed_meta_keys(feed_id);
    let read_at = read_app_meta(database, &keys.read_at)?;
    if read_at.trim().is_empty() {
        return Ok(0);
    }
    let connection = database.lock_conn()?;
    let count = connection
        .query_row(
            "SELECT COUNT(*) FROM charity_feed_items
             WHERE feed_id = ?1 AND first_seen_at > ?2",
            params![feed_id, read_at],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| error.to_string())?;
    Ok(count.max(0) as usize)
}

pub fn local_day_utc_range_secs() -> (i64, i64) {
    let offset = local_utc_offset_secs();
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|dur| dur.as_secs() as i64)
        .unwrap_or(0);
    let local_now = now_secs + offset;
    let local_today_start = local_now - local_now.rem_euclid(86_400);
    let utc_start = local_today_start - offset;
    (utc_start, utc_start + 86_400)
}

pub fn local_utc_offset_secs() -> i64 {
    if let Ok(output) = std::process::Command::new("/bin/date").arg("+%z").output() {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            let value = text.trim();
            if value.len() == 5 && (value.starts_with('+') || value.starts_with('-')) {
                if let (Ok(hour), Ok(minute)) =
                    (value[1..3].parse::<i64>(), value[3..5].parse::<i64>())
                {
                    let sign = if value.starts_with('-') { -1 } else { 1 };
                    if hour < 24 && minute < 60 {
                        return sign * (hour * 3600 + minute * 60);
                    }
                }
            }
        }
    }
    0
}
