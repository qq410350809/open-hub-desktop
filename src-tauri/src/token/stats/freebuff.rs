use crate::token::collector::{freebuff_db_paths, freebuff_usage, normalize_usage};
use crate::token::stats::types::*;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::path::Path;

pub const FREEBUFF_SOURCE: &str = "freebuff";

/// 增量采集 Freebuff Desktop 客户端（`~/.config/freebuff-desktop/projects/*/desktop-v2.db`）
/// 的请求健康统计。
///
/// 口径与用量采集器（`token::collector::sources::freebuff`）严格对齐，保证「请求健康」与
/// 「Token 用量」两处对同一条消息给出同一种判定：
/// - `role='user'`  → 1 轮对话；
/// - `role='assistant'` 且能解析出正用量 → 1 次请求 + 1 次成功；
/// - `role='assistant'` 只有 `usageIncomplete`（客户端自报用量不完整）→ 计请求，
///   成功 / 失败都不计（与仓库内「用户中断不计成败」的既有口径一致）；
/// - 其它 assistant 行（流式进行中 / 无用量应答）不产生统计。
///
/// 水位线用 `messages.seq`：客户端「回退 / 编辑」会 `DELETE` 掉行并以更大的 seq 重写，
/// 用库内自增主键做水位既不会漏掉新行，也不会把一次回退当成新回合重复统计。
pub fn collect_freebuff_activity_incremental(
    db_path: &Path,
    map: &mut BTreeMap<String, HealthAgg>,
    sources_map: &mut BTreeMap<String, HealthAgg>,
    cursor: &mut SqliteCursor,
) {
    let Some(conn) = open_readonly_sqlite(db_path) else {
        return;
    };
    if !sqlite_table_exists(&conn, "messages") {
        return;
    }
    let since = cursor.max_time_created.max(0);
    let Ok(mut statement) = conn.prepare(
        "SELECT seq, role, ts, metrics_json FROM messages WHERE seq > ?1 ORDER BY seq ASC",
    ) else {
        return;
    };
    let Ok(rows) = statement.query_map([since], |row| {
        Ok((
            row.get::<_, i64>(0).unwrap_or_default(),
            row.get::<_, String>(1).unwrap_or_default(),
            row.get::<_, i64>(2).unwrap_or_default(),
            row.get::<_, Option<String>>(3).unwrap_or_default(),
        ))
    }) else {
        return;
    };

    let mut max_seq = since;
    for (seq, role, ts, metrics_json) in rows.flatten() {
        if seq > max_seq {
            max_seq = seq;
        }
        let Some(hour) = hour_key_from_millis(ts) else {
            continue;
        };
        match role.as_str() {
            "user" => record(map, sources_map, FREEBUFF_SOURCE, hour, 1, 0, 0, 0),
            "assistant" => {
                let metrics = metrics_json
                    .as_deref()
                    .and_then(|text| serde_json::from_str::<JsonValue>(text).ok());
                let Some(metrics) = metrics else {
                    continue;
                };
                let total = freebuff_usage_from_metrics(&metrics);
                if total > 0 {
                    record(map, sources_map, FREEBUFF_SOURCE, hour, 0, 1, 1, 0);
                    continue;
                }
                if metrics
                    .get("usageIncomplete")
                    .and_then(JsonValue::as_bool)
                    .unwrap_or(false)
                {
                    record(map, sources_map, FREEBUFF_SOURCE, hour, 0, 1, 0, 0);
                }
            }
            _ => {}
        }
    }
    cursor.max_time_created = max_seq;
}

/// 采集全部项目库的健康数据。水位线按**库路径**分别保存：一个项目一个库，
/// 用序号做键会随项目目录增删漂移，把另一个库的水位错用到本库上。
pub fn collect_freebuff_activity(
    home: &Path,
    map: &mut BTreeMap<String, HealthAgg>,
    sources_map: &mut BTreeMap<String, HealthAgg>,
    cursors: &mut BTreeMap<String, SqliteCursor>,
) {
    for db_path in freebuff_db_paths(home) {
        let key = format!("{}:{}", FREEBUFF_SOURCE, db_path.to_string_lossy());
        let cursor = cursors.entry(key).or_default();
        collect_freebuff_activity_incremental(&db_path, map, sources_map, cursor);
    }
}

/// 单条 assistant 消息的用量总量：优先读 `metrics.usage`，兼容直接平铺在 metrics 上的写法。
/// 口径与用量采集器一致（`total = 全新输入 + 缓存命中 + 输出`）。
fn freebuff_usage_from_metrics(metrics: &JsonValue) -> i64 {
    metrics
        .get("usage")
        .and_then(freebuff_usage)
        .or_else(|| freebuff_usage(metrics))
        .map(|raw| {
            let (_, _, _, _, _, total) = normalize_usage(raw);
            total
        })
        .unwrap_or(0)
}
