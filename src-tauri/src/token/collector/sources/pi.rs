use crate::token::collector::normalizer::project_key_or_label;
use crate::token::collector::time_utils::iso_from_millis;
use crate::token::collector::types::{
    database_fingerprint, normalize_usage, number, open_readonly_sqlite, token_session,
    CachedDatabase, InputSemantics, LocalDatabaseSession, RawUsage, UsageEvent, UNKNOWN_PI_MODEL,
};
use serde_json::Value as JsonValue;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// 会话未归属任何项目（pi 的 `sessions.project_id` 可为空）时的归组标签。
pub const PI_DEFAULT_PROJECT_LABEL: &str = "PI Desktop";

/// PI-Desktop 的本地数据目录；`PI_DESKTOP_HOME` 可覆盖（与其它源的环境变量覆盖惯例一致）。
pub fn pi_data_dir(home: &Path) -> PathBuf {
    crate::token::collector::aggregator::env_path_override("PI_DESKTOP_HOME")
        .unwrap_or_else(|| home.join(".pi-desktop"))
}

pub fn pi_db_path(home: &Path) -> PathBuf {
    pi_data_dir(home).join("pi.sqlite")
}

/// `turns.usage_json` 口径（真实库 28 行零例外实测）：`totalTokens = inputTokens + outputTokens
/// + cacheReadTokens + cacheWriteTokens`，且 `inputTokens` 是全新输入、不含缓存命中，对应
/// [`InputSemantics::Fresh`]。注意 pi 自报的 `totalTokens` 把缓存写入也算了进去，而本仓库统一
/// 口径 `total = 全新输入 + 缓存命中 + 输出` 刻意不含缓存写入（写入与思考 token 独立上报），
/// 因此本源的 total 会比客户端自报值小一个 `cacheWriteTokens` 的量，属预期差异。
/// `reasoningTokens` 是输出里思考的部分，同样独立上报、不计入 total。
pub fn pi_usage(value: &JsonValue) -> Option<RawUsage> {
    if !value.is_object() {
        return None;
    }
    Some(RawUsage {
        input: number(value, &["inputTokens", "input_tokens"]),
        semantics: InputSemantics::Fresh,
        cache_read: number(value, &["cacheReadTokens", "cache_read_tokens"]),
        cache_write: number(value, &["cacheWriteTokens", "cache_write_tokens"]),
        output: number(value, &["outputTokens", "output_tokens"]),
        reasoning: number(value, &["reasoningTokens", "reasoning_tokens"]),
    })
}

/// 把 turn 级合计均分到 `count` 个请求事件上（余数给靠前的事件），保证各分量之和与 turn 合计一致。
fn split_even(value: i64, index: i64, count: i64) -> i64 {
    let value = value.max(0);
    if count <= 0 {
        return value;
    }
    value / count + if index < value % count { 1 } else { 0 }
}
/// 采集 PI-Desktop 本地会话库（默认 `~/.pi-desktop/pi.sqlite`）。
///
/// 口径要点：
/// - 一行 `turns` = 一次 agent 执行（可能由用户提问触发，也可能由续跑 / 调度触发），
///   其 `usage_json` 是**该次执行内所有模型调用的合计**，库里没有按请求的用量明细。
/// - **对话数**取 `messages` 里该 turn 的**用户消息**条数；**请求数**取 **assistant 消息条数**
///   （agent 循环里每条 assistant 消息就是一次模型调用）。二者因此不是 1:1：一轮长任务常有
///   上百次调用（本机实测 28 轮对话 / 622 次请求），与 claude / codex 等源的
///   「user 消息 = 对话、assistant 消息 = 请求」口径一致。
/// - turn 级合计按请求条数**均分**到各请求事件上。同一 turn 的所有事件时间戳相同
///   （都取 `ended_at`），故半桶合计与总量精确不变，只有单个事件的请求粒度是推算的。
///   必须拆开的原因：`request_count` 只由带用量的事件累加得出，不拆就永远是「1 轮 1 请求」，
///   会出现「对话数比请求数还多」这种明显不合理的读数。
/// - 只统计 `ended_at` 有效（非空且 > 0）的 turn：仍在进行中的请求用量尚未落库，
///   且与健康统计的口径保持一致。没有任何消息的 turn（起了 turn 随即中断）不产生统计。
/// - `sessions.deleted_at` 非空表示客户端里已删除，统计跟随客户端可见状态跳过。
pub fn parse_pi_database(path: &Path) -> CachedDatabase {
    let Some(connection) = open_readonly_sqlite(path) else {
        return CachedDatabase {
            fingerprint: database_fingerprint(path),
            ..Default::default()
        };
    };

    // 会话表：项目路径、模型与起止时间。旧版本库缺 sessions/projects 时 prepare 失败即跳过，
    // 后续按 turns 里的 session_id 懒建会话，保证仍能统计用量。
    // 已删除的会话只登记 id：懒建会话时必须先挡掉，否则会把已删除会话的 turn 又捡回来。
    let mut sessions = BTreeMap::<String, LocalDatabaseSession>::new();
    let mut deleted_sessions = HashSet::<String>::new();
    if let Ok(mut statement) = connection.prepare(
        "SELECT s.id, s.created_at, s.updated_at, s.model_id, p.path, s.deleted_at \
         FROM sessions s LEFT JOIN projects p ON p.id = s.project_id",
    ) {
        if let Ok(rows) = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0).unwrap_or_default(),
                row.get::<_, i64>(1).unwrap_or_default(),
                row.get::<_, i64>(2).unwrap_or_default(),
                row.get::<_, Option<String>>(3).unwrap_or_default(),
                row.get::<_, Option<String>>(4).unwrap_or_default(),
                row.get::<_, Option<i64>>(5).unwrap_or_default(),
            ))
        }) {
            for (id, created, updated, model, project_path, deleted_at) in rows.flatten() {
                if deleted_at.is_some() {
                    deleted_sessions.insert(id);
                    continue;
                }
                sessions.insert(
                    id,
                    LocalDatabaseSession {
                        directory: project_path.unwrap_or_default(),
                        model: model.unwrap_or_default(),
                        started_at: iso_from_millis(created),
                        ended_at: iso_from_millis(updated),
                        ..Default::default()
                    },
                );
            }
        }
    }

    // 每个 turn 的用户 / assistant 消息条数：对话数与请求数的唯一依据。
    // 旧库或精简库没有 messages 表时 prepare 失败 → 空表，退化成「1 轮 1 对话 1 请求」。
    let mut message_counts = BTreeMap::<String, (i64, i64)>::new();
    if let Ok(mut statement) = connection.prepare(
        "SELECT turn_id, \
                SUM(CASE WHEN role = 'user' THEN 1 ELSE 0 END), \
                SUM(CASE WHEN role = 'assistant' THEN 1 ELSE 0 END) \
         FROM messages WHERE turn_id IS NOT NULL GROUP BY turn_id",
    ) {
        if let Ok(rows) = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0).unwrap_or_default(),
                row.get::<_, i64>(1).unwrap_or_default(),
                row.get::<_, i64>(2).unwrap_or_default(),
            ))
        }) {
            for (turn_id, users, assistants) in rows.flatten() {
                message_counts.insert(turn_id, (users.max(0), assistants.max(0)));
            }
        }
    }

    let mut events = Vec::<UsageEvent>::new();
    if let Ok(mut statement) = connection.prepare(
        "SELECT id, session_id, model_id, input_tokens, output_tokens, usage_json, ended_at \
         FROM turns WHERE ended_at IS NOT NULL AND ended_at > 0 ORDER BY ended_at ASC",
    ) {
        if let Ok(rows) = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0).unwrap_or_default(),
                row.get::<_, String>(1).unwrap_or_default(),
                row.get::<_, Option<String>>(2).unwrap_or_default(),
                row.get::<_, i64>(3).unwrap_or_default(),
                row.get::<_, i64>(4).unwrap_or_default(),
                row.get::<_, Option<String>>(5).unwrap_or_default(),
                row.get::<_, i64>(6).unwrap_or_default(),
            ))
        }) {
            for (
                turn_id,
                session_id,
                model_id,
                input_tokens,
                output_tokens,
                usage_json,
                ended_at,
            ) in rows.flatten()
            {
                // 已删除会话的 turn 不计入统计（懒建会话前先挡掉）。
                if deleted_sessions.contains(&session_id) {
                    continue;
                }
                let (user_messages, assistant_messages) =
                    message_counts.get(&turn_id).copied().unwrap_or_default();
                // 既无提问也无应答的 turn（起了 turn 随即中断）不产生任何统计。
                if user_messages == 0 && assistant_messages == 0 {
                    continue;
                }
                // 用量只在 turn 结束时才确定，时间戳统一取结束时刻，与健康统计按小时归档一致。
                let timestamp = iso_from_millis(ended_at);
                let project_key = sessions
                    .get(&session_id)
                    .map(|session| {
                        project_key_or_label(&session.directory, PI_DEFAULT_PROJECT_LABEL)
                    })
                    .unwrap_or_else(|| PI_DEFAULT_PROJECT_LABEL.to_string());
                let model = model_id
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| UNKNOWN_PI_MODEL.to_string());

                let session = sessions.entry(session_id).or_default();
                // 会话轮次 = 对话数（用户提问），与前端 `estimateRequestCount` 的
                // `conversationCount` 语义一致。
                session.turns += user_messages;
                if session.model.trim().is_empty() {
                    session.model = model.clone();
                }

                // 对话事件：一条用户消息 = 一轮对话，只贡献 `conversation_count`。
                for index in 0..user_messages {
                    events.push(UsageEvent {
                        id: format!("pi_{turn_id}_user_{index}"),
                        source: "pi".to_string(),
                        model: model.clone(),
                        project_key: project_key.clone(),
                        timestamp: timestamp.clone(),
                        conversation_count: 1,
                        ..Default::default()
                    });
                }


                // 助手用量优先取 usage_json，缺失时退回 turns 的冗余列（口径同为全新输入）。
                let raw = usage_json
                    .as_deref()
                    .and_then(|text| serde_json::from_str::<JsonValue>(text).ok())
                    .and_then(|value| pi_usage(&value))
                    .unwrap_or(RawUsage {
                        input: input_tokens,
                        semantics: InputSemantics::Fresh,
                        output: output_tokens,
                        ..Default::default()
                    });
                let (input, cached, cache_creation, output, reasoning, total) =
                    normalize_usage(raw);
                if total <= 0 {
                    continue;
                }

                session.tokens.input_tokens += input;
                session.tokens.cached_input_tokens += cached;
                session.tokens.cache_creation_input_tokens += cache_creation;
                session.tokens.output_tokens += output;
                session.tokens.reasoning_output_tokens += reasoning;
                session.tokens.total_tokens += total;

                // 请求事件：按 assistant 消息条数拆开，均分 turn 级合计。
                // 没有 assistant 消息但确实产生了用量时退化为 1 条，避免漏掉 token。
                let requests = assistant_messages.max(1);
                for index in 0..requests {
                    let event_input = split_even(input, index, requests);
                    let event_cached = split_even(cached, index, requests);
                    let event_cache_creation = split_even(cache_creation, index, requests);
                    let event_output = split_even(output, index, requests);
                    let event_reasoning = split_even(reasoning, index, requests);
                    events.push(UsageEvent {
                        id: format!("pi_{turn_id}_assistant_{index}"),
                        source: "pi".to_string(),
                        model: model.clone(),
                        project_key: project_key.clone(),
                        timestamp: timestamp.clone(),
                        input_tokens: event_input,
                        cached_input_tokens: event_cached,
                        cache_creation_input_tokens: event_cache_creation,
                        output_tokens: event_output,
                        reasoning_output_tokens: event_reasoning,
                        // 与全链路口径一致：total = 全新输入 + 缓存命中 + 输出。
                        total_tokens: event_input + event_cached + event_output,
                        conversation_count: 0,
                        cost_usd: 0.0,
                        pricing_available: false,
                        estimated_tokens: 0,
                    });
                }
            }
        }
    }

    let parsed_sessions = sessions
        .into_iter()
        .filter(|(_, session)| session.turns > 0 || session.tokens.total_tokens > 0)
        .map(|(session_id, session)| {
            let project_key = project_key_or_label(&session.directory, PI_DEFAULT_PROJECT_LABEL);
            let model = if session.model.trim().is_empty() {
                UNKNOWN_PI_MODEL.to_string()
            } else {
                session.model
            };
            token_session(
                session_id,
                "pi",
                project_key,
                model,
                session.started_at,
                session.ended_at,
                session.turns,
                session.tokens,
                0.0,
            )
        })
        .collect::<Vec<_>>();

    CachedDatabase {
        fingerprint: database_fingerprint(path),
        events,
        sessions: parsed_sessions,
    }
}
