use crate::token::stats::types::*;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::Path;

pub const PI_SOURCE: &str = "pi";

/// 增量采集 PI-Desktop 客户端（`~/.pi-desktop/pi.sqlite`）的请求健康统计。
///
/// 口径说明：
/// - 数据来源为 `turns`（一次 agent 执行）+ `messages`。**一行 turn 不等于一次请求**：
///   agent 循环里每条 assistant 消息就是一次模型调用，一轮长任务常有上百次调用。
///   因此 `dialogues` 取该 turn 的**用户消息**条数，`requests` 取 **assistant 消息**条数，
///   与 health.rs 里 claude / codex 等源「user = 对话、assistant = 请求」的口径一致。
/// - 增量水位线取 `ended_at`（而不是 `started_at`）：pi 的 usage 只在 turn
///   结束时才落库，若用 `started_at` 做水位，扫描时仍处于 `running` 的 turn
///   会在之后完成时被永久跳过。`ended_at IS NOT NULL` 也天然排除了 in-flight
///   的 running turn。
/// - 归类沿用仓库既有口径（参见 `crate::token::stats::types::is_user_cancelled_error`
///   在 health.rs 中的用法）：用户主动中断「计请求、既不计成功也不计失败」。
///   - completed            → 成功 = 请求数（该轮的调用都跑完了）
///   - error                → 失败 1 次（只知整轮以错误收尾，无法定位是哪一次调用）
///   - aborted / 其它未知值 → 成功 0、失败 0（用户中断或无法归类）
pub fn collect_pi_activity_incremental(
    db_path: &Path,
    map: &mut BTreeMap<String, HealthAgg>,
    sources_map: &mut BTreeMap<String, HealthAgg>,
    cursor: &mut SqliteCursor,
) {
    let Some(conn) = open_readonly_sqlite(db_path) else {
        return;
    };
    if !sqlite_table_exists(&conn, "turns") {
        return;
    }
    let since = cursor.max_time_created;
    let mut max_time = since;
    for (status, ended_at, dialogues, requests) in read_pi_turn_activity(&conn, since) {
        if ended_at > max_time {
            max_time = ended_at;
        }
        let Some(hour) = hour_key_from_millis(ended_at) else {
            continue;
        };
        record_pi_turn(map, sources_map, &hour, &status, dialogues, requests);
    }
    cursor.max_time_created = max_time;
}

/// 读取水位线之后的 turn 及其消息条数：`(status, ended_at, 对话数, 请求数)`。
///
/// 优先按 `messages` 统计；旧库 / 精简库缺 `messages` 表时退化成「1 轮 1 对话 1 请求」，
/// 保证仍能给出请求健康数据。
fn read_pi_turn_activity(conn: &Connection, since: i64) -> Vec<(String, i64, i64, i64)> {
    let with_messages = "SELECT t.status, t.ended_at, \
                SUM(CASE WHEN m.role = 'user' THEN 1 ELSE 0 END), \
                SUM(CASE WHEN m.role = 'assistant' THEN 1 ELSE 0 END) \
         FROM turns t LEFT JOIN messages m ON m.turn_id = t.id \
         WHERE t.ended_at IS NOT NULL AND t.ended_at > ?1 \
         GROUP BY t.id, t.status, t.ended_at ORDER BY t.ended_at ASC";
    let without_messages = "SELECT status, ended_at, 1, 1 FROM turns \
         WHERE ended_at IS NOT NULL AND ended_at > ?1 ORDER BY ended_at ASC";

    for sql in [with_messages, without_messages] {
        let Ok(mut stmt) = conn.prepare(sql) else {
            continue;
        };
        let Ok(rows) = stmt.query_map([since], |row| {
            Ok((
                row.get::<_, String>(0).unwrap_or_default(),
                row.get::<_, i64>(1).unwrap_or_default(),
                row.get::<_, i64>(2).unwrap_or_default(),
                row.get::<_, i64>(3).unwrap_or_default(),
            ))
        }) else {
            continue;
        };
        return rows
            .flatten()
            .map(|(status, ended_at, dialogues, requests)| {
                (status, ended_at, dialogues.max(0), requests.max(0))
            })
            .collect();
    }
    Vec::new()
}

/// 单轮 turn → 健康聚合：既无提问也无应答的 turn 不产生统计。
fn record_pi_turn(
    map: &mut BTreeMap<String, HealthAgg>,
    sources_map: &mut BTreeMap<String, HealthAgg>,
    hour: &str,
    status: &str,
    dialogues: i64,
    requests: i64,
) {
    if dialogues <= 0 && requests <= 0 {
        return;
    }
    let (success, failed) = match status {
        "completed" => (requests, 0),
        "error" => (0, 1),
        _ => (0, 0),
    };
    record(
        map,
        sources_map,
        PI_SOURCE,
        hour.to_string(),
        dialogues,
        requests,
        success,
        failed,
    );
}