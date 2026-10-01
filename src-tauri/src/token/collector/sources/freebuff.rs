use crate::token::collector::normalizer::project_key_or_label;
use crate::token::collector::time_utils::iso_from_millis;
use crate::token::collector::types::{
    database_fingerprint, float_number, normalize_usage, number, open_readonly_sqlite, token_session,
    CachedDatabase, InputSemantics, LocalDatabaseSession, RawUsage, UsageEvent, UNKNOWN_FREEBUFF_MODEL,
};
use rusqlite::Connection;
use serde_json::Value as JsonValue;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// 会话未归属任何项目（`threads.project_path` 为空）时的归组标签。
pub const FREEBUFF_DEFAULT_PROJECT_LABEL: &str = "Freebuff";

/// Freebuff Desktop 的本地数据目录。客户端按 XDG 约定落在 `$XDG_CONFIG_HOME/freebuff-desktop`
/// （macOS 上实测 `~/.config/freebuff-desktop`）；`FREEBUFF_DESKTOP_HOME` 可覆盖，
/// 与 `PI_DESKTOP_HOME` / `CLAUDE_CONFIG_DIR` 等源的覆盖惯例一致。
pub fn freebuff_data_dir(home: &Path) -> PathBuf {
    if let Some(dir) =
        crate::token::collector::aggregator::env_path_override("FREEBUFF_DESKTOP_HOME")
    {
        return dir;
    }
    let base = crate::token::collector::aggregator::env_path_override("XDG_CONFIG_HOME")
        .unwrap_or_else(|| home.join(".config"));
    base.join("freebuff-desktop")
}

/// 单个 Freebuff Desktop 数据根下按项目分库的库文件：`projects/<slug>-<uuid>/desktop-v2.db`
/// （另有 `-wal` / `-shm` 伴生文件）。目录与文件名都排序，保证聚合层缓存键 `freebuff_{idx}`
/// 不随目录枚举顺序漂移。
///
/// 只认项目库里的 `desktop*.db`：数据根下的 `state.json.orchestrator-lock.sqlite`（进程锁）
/// 与 `state.json*` 都不是用量来源；`-wal` / `-shm` 因不以 `.db` 结尾天然被排除。
fn freebuff_databases_in(root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let Ok(entries) = fs::read_dir(root.join("projects")) else {
        return paths;
    };
    let mut project_dirs = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    project_dirs.sort();
    for project_dir in project_dirs {
        let Ok(files) = fs::read_dir(&project_dir) else {
            continue;
        };
        let mut databases = files
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file()
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(|name| name.starts_with("desktop") && name.ends_with(".db"))
                        .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        databases.sort();
        paths.extend(databases);
    }
    paths
}

/// 数据根的候选顺序：`FREEBUFF_DESKTOP_HOME` 覆盖 → `$XDG_CONFIG_HOME/freebuff-desktop` →
/// `~/.config/freebuff-desktop`（macOS / Linux 实测落点）→
/// `~/Library/Application Support/freebuff-desktop`（macOS 图形应用惯例）→
/// `~/AppData/Roaming/freebuff-desktop`（Windows 惯例）。
/// 与 copilot / cursor 源同时探测多个根的做法一致。
fn freebuff_candidate_roots(home: &Path) -> Vec<PathBuf> {
    if let Some(dir) =
        crate::token::collector::aggregator::env_path_override("FREEBUFF_DESKTOP_HOME")
    {
        return vec![dir];
    }
    let mut roots = Vec::new();
    if let Some(base) = crate::token::collector::aggregator::env_path_override("XDG_CONFIG_HOME") {
        roots.push(base.join("freebuff-desktop"));
    }
    for candidate in [
        home.join(".config").join("freebuff-desktop"),
        home.join("Library")
            .join("Application Support")
            .join("freebuff-desktop"),
        home.join("AppData")
            .join("Roaming")
            .join("freebuff-desktop"),
    ] {
        if !roots.contains(&candidate) {
            roots.push(candidate);
        }
    }
    roots
}

/// 客户端只用一个数据根，因此取**第一个真的装了项目库**的候选根，
/// 避免同一项目在两个根下被重复计数。
pub fn freebuff_db_paths(home: &Path) -> Vec<PathBuf> {
    // 测试/排障用的显式覆盖：与 `OPENHUB_CATPAWAI_DB_PATH` 同一惯例。
    if let Ok(value) = std::env::var("OPENHUB_FREEBUFF_DB_PATH") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return vec![path];
        }
    }

    for root in freebuff_candidate_roots(home) {
        let paths = freebuff_databases_in(&root);
        if !paths.is_empty() {
            return paths;
        }
    }
    Vec::new()
}

/// `messages.metrics_json.usage` 的口径（依据客户端自身实现与真实库实测）：
/// `totalTokens = inputTokens + outputTokens`，且 `cachedInputTokens <= inputTokens`，
/// 即 `inputTokens` 是**总输入**、已含缓存命中，对应 [`InputSemantics::InclusiveOfCacheRead`]，
/// 归一化后全新输入 = `inputTokens - cachedInputTokens`。
/// `reasoningOutputTokens` 是输出里思考的部分，独立上报、不计入 total。
///
/// `cachedInputTokens` 被夹到 `inputTokens` 以内（仅当 input 有效时）：客户端自报 total 不含缓存写入，
/// 若上游给出 `cached > input` 的脏数据，夹取可保证本仓库口径的 total 不会反超客户端自报值。
pub fn freebuff_usage(value: &JsonValue) -> Option<RawUsage> {
    if !value.is_object() {
        return None;
    }
    let input = number(
        value,
        &["inputTokens", "input_tokens", "promptTokens", "prompt_tokens"],
    );
    let raw_cached = number(value, &["cachedInputTokens", "cached_input_tokens"]);
    let cached = if input > 0 {
        raw_cached.min(input)
    } else {
        raw_cached
    };
    let output = number(
        value,
        &[
            "outputTokens",
            "output_tokens",
            "completionTokens",
            "completion_tokens",
        ],
    );
    let reasoning = number(
        value,
        &["reasoningOutputTokens", "reasoning_output_tokens"],
    );
    if input == 0 && cached == 0 && output == 0 {
        return None;
    }
    Some(RawUsage {
        input,
        semantics: InputSemantics::InclusiveOfCacheRead,
        cache_read: cached,
        cache_write: 0,
        output,
        reasoning,
    })
}

/// 模型解析优先级：`threads.model`（用户在该线程显式选定的模型）→ 库内记录默认模型 → unknown。
///
/// 桌面端**不落库逐请求模型**，`threads.model` 为空表示「用客户端默认模型」；该默认模型在库里
/// 只以 `freebuff_storage_metadata.default_model_migration` 留痕，故用它兜底。
/// 两处都取不到时退回 [`UNKNOWN_FREEBUFF_MODEL`]，不凭空猜测模型名。
pub fn freebuff_resolve_model(thread_model: Option<&str>, database_default: Option<&str>) -> String {
    thread_model
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            database_default
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or(UNKNOWN_FREEBUFF_MODEL)
        .to_string()
}

/// 读取库级默认模型。键名在真实库里实测为 `default_model_migration`，同时容忍 `default_model`。
fn freebuff_default_model(connection: &Connection) -> Option<String> {
    for key in ["default_model_migration", "default_model"] {
        let Ok(value) = connection.query_row(
            "SELECT value FROM freebuff_storage_metadata WHERE key = ?1",
            [key],
            |row| row.get::<_, String>(0),
        ) else {
            continue;
        };
        let trimmed = value.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    None
}

/// 采集 Freebuff Desktop 本地会话库（默认 `~/.config/freebuff-desktop/projects/<项目>/desktop-v2.db`）。
///
/// 口径要点：
/// - 一个 `threads` 行 = 一个会话，`project_path` 直接是项目根绝对路径，交给
///   [`project_key_or_label`] 归组（与其它源共用同一套工作区归并逻辑）。
/// - `messages.metrics_json.usage` 已由客户端按**每次助手应答**落库，因此一条带用量的
///   assistant 消息 = 一个请求事件，无需像 pi 源那样再拆分；`user` 消息 = 一轮对话，只贡献
///   `conversation_count`，与 claude / codex 等源「user 消息 = 对话、assistant 消息 = 请求」口径一致。
/// - 时间戳取消息自身的 `ts`（**毫秒**）；会话起止取该线程消息的真实活动窗口，取不到时退回
///   线程行的 `created_at` / `updated_at`。仍在进行中的应答尚无用量，自然不计入。
/// - `costUsd` 实测恒为 0（免费/额度制），因此仅在真的出现正数时才标记 `pricing_available`，
///   不做任何价格推算。
pub fn parse_freebuff_database(path: &Path) -> CachedDatabase {
    let Some(connection) = open_readonly_sqlite(path) else {
        return CachedDatabase {
            fingerprint: database_fingerprint(path),
            ..Default::default()
        };
    };

    let database_default = freebuff_default_model(&connection);

    // 会话表：项目路径、模型与起止时间。旧库缺 threads 表时下面的懒建会话仍然生效。
    let mut sessions = BTreeMap::<String, LocalDatabaseSession>::new();
    if let Ok(mut statement) = connection.prepare(
        "SELECT id, project_path, model, created_at, updated_at FROM threads",
    ) {
        if let Ok(rows) = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0).unwrap_or_default(),
                row.get::<_, Option<String>>(1).unwrap_or_default(),
                row.get::<_, Option<String>>(2).unwrap_or_default(),
                row.get::<_, Option<i64>>(3).unwrap_or_default(),
                row.get::<_, Option<i64>>(4).unwrap_or_default(),
            ))
        }) {
            for (id, project_path, model, created_at, updated_at) in rows.flatten() {
                if id.trim().is_empty() {
                    continue;
                }
                sessions.insert(
                    id,
                    LocalDatabaseSession {
                        directory: project_path.unwrap_or_default(),
                        model: freebuff_resolve_model(
                            model.as_deref(),
                            database_default.as_deref(),
                        ),
                        started_at: iso_from_millis(created_at.unwrap_or_default()),
                        ended_at: iso_from_millis(updated_at.unwrap_or_default()),
                        ..Default::default()
                    },
                );
            }
        }
    }

    // 线程真实活动窗口（消息级 min/max），用于覆盖线程行的 created_at / updated_at。
    let mut bounds = BTreeMap::<String, (i64, i64)>::new();
    let mut events = Vec::<UsageEvent>::new();
    if let Ok(mut statement) = connection.prepare(
        "SELECT seq, thread_id, role, ts, metrics_json FROM messages ORDER BY seq ASC",
    ) {
        if let Ok(rows) = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0).unwrap_or_default(),
                row.get::<_, String>(1).unwrap_or_default(),
                row.get::<_, String>(2).unwrap_or_default(),
                row.get::<_, i64>(3).unwrap_or_default(),
                row.get::<_, Option<String>>(4).unwrap_or_default(),
            ))
        }) {
            for (seq, thread_id, role, ts, metrics_json) in rows.flatten() {
                // 缺 threads 表时按消息懒建会话，保证用量仍能统计。
                let session = sessions.entry(thread_id.clone()).or_default();
                let model = if session.model.trim().is_empty() {
                    freebuff_resolve_model(None, database_default.as_deref())
                } else {
                    session.model.clone()
                };
                let project_key =
                    project_key_or_label(&session.directory, FREEBUFF_DEFAULT_PROJECT_LABEL);
                let timestamp = iso_from_millis(ts);

                if ts > 0 {
                    let entry = bounds.entry(thread_id.clone()).or_insert((ts, ts));
                    entry.0 = entry.0.min(ts);
                    entry.1 = entry.1.max(ts);
                }

                if role == "user" {
                    // 一条用户消息 = 一轮对话，只贡献 conversation_count。
                    session.turns += 1;
                    events.push(UsageEvent {
                        id: freebuff_event_id(&thread_id, seq, "user"),
                        source: "freebuff".to_string(),
                        model,
                        project_key,
                        timestamp,
                        conversation_count: 1,
                        ..Default::default()
                    });
                    continue;
                }
                if role != "assistant" {
                    continue;
                }

                let metrics = metrics_json
                    .as_deref()
                    .and_then(|text| serde_json::from_str::<JsonValue>(text).ok());
                let Some(metrics) = metrics else {
                    continue;
                };
                let usage = metrics
                    .get("usage")
                    .and_then(freebuff_usage)
                    .or_else(|| freebuff_usage(&metrics));
                let Some(raw) = usage else {
                    continue;
                };
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

                let cost = float_number(&metrics, &["costUsd", "cost_usd"]);
                let pricing_available = cost > 0.0;
                if pricing_available {
                    session.cost_usd += cost;
                }

                events.push(UsageEvent {
                    id: freebuff_event_id(&thread_id, seq, "assistant"),
                    source: "freebuff".to_string(),
                    model,
                    project_key,
                    timestamp,
                    input_tokens: input,
                    cached_input_tokens: cached,
                    cache_creation_input_tokens: cache_creation,
                    output_tokens: output,
                    reasoning_output_tokens: reasoning,
                    // 与全链路口径一致：total = 全新输入 + 缓存命中 + 输出。
                    total_tokens: total,
                    conversation_count: 0,
                    cost_usd: cost,
                    pricing_available,
                    estimated_tokens: 0,
                });
            }
        }
    }

    let parsed_sessions = sessions
        .into_iter()
        .filter(|(_, session)| session.turns > 0 || session.tokens.total_tokens > 0)
        .map(|(thread_id, mut session)| {
            if let Some((start, end)) = bounds.get(&thread_id) {
                session.started_at = iso_from_millis(*start);
                session.ended_at = iso_from_millis(*end);
            }
            let project_key =
                project_key_or_label(&session.directory, FREEBUFF_DEFAULT_PROJECT_LABEL);
            let model = freebuff_resolve_model(
                Some(session.model.as_str()),
                database_default.as_deref(),
            );
            token_session(
                thread_id,
                "freebuff",
                project_key,
                model,
                session.started_at,
                session.ended_at,
                session.turns,
                session.tokens,
                session.cost_usd,
            )
        })
        .collect::<Vec<_>>();

    CachedDatabase {
        fingerprint: database_fingerprint(path),
        events,
        sessions: parsed_sessions,
    }
}

/// 事件 id 必须**跨库唯一**：`seq` 只是单个项目库里的自增主键，多个项目库各自从 1 开始，
/// 只用 `seq` 会让不同项目的同号消息在 `source:id` 去重时互相覆盖（参考 cursor / windsurf 的
/// `path_tag` 做法）。这里用「线程 id（UUID）+ 库内 seq」组合，既跨库唯一又随库内消息稳定。
fn freebuff_event_id(thread_id: &str, seq: i64, kind: &str) -> String {
    format!("freebuff_{thread_id}_{seq}_{kind}")
}

/// 会话哈希前缀：与 [`token_session`] 的 `openhub:{source}:{id}` 形状一致。
const FREEBUFF_SESSION_PREFIX: &str = "openhub:freebuff:";

/// 从事件 id 反解线程 id（`freebuff_<thread_id>_<seq>_<kind>`）。
fn freebuff_event_thread_id(event_id: &str) -> Option<&str> {
    let rest = event_id.strip_prefix("freebuff_")?;
    let (rest, _kind) = rest.rsplit_once('_')?;
    let (thread_id, _seq) = rest.rsplit_once('_')?;
    (!thread_id.is_empty()).then_some(thread_id)
}

/// 带历史合并的解析：**客户端回退不会抹掉已经发生的用量**。
///
/// 客户端 `editMessage`（回退 / 编辑 / 重发）会 `DELETE FROM messages WHERE seq >= ?`，
/// 被删掉的助手行连同 `metrics_json.usage` 一起从库里消失——这些 token 已经真实消耗，
/// 不该因为客户端回退就从本地统计里蒸发（采集器每 20s 重新解析整库，整条目替换会连带删掉它们）。
/// 因此把上一轮解析到的**有用量**事件并回结果：
/// - 只并回**线程仍在库中**的事件：线程被客户端删除（级联删消息）时不复活；
/// - 只并回**带 token** 的事件：回退掉的「对话」计数不重复累计，会话视图仍只反映库内当前内容；
/// - 同 id 以本轮解析为准（客户端改写同一行时取最新口径），合并是幂等的。
pub fn parse_freebuff_database_with_history(
    path: &Path,
    history: &[UsageEvent],
) -> CachedDatabase {
    let mut parsed = parse_freebuff_database(path);
    if history.is_empty() {
        return parsed;
    }
    let live_threads = parsed
        .sessions
        .iter()
        .filter_map(|session| session.session_hash.strip_prefix(FREEBUFF_SESSION_PREFIX))
        .map(str::to_string)
        .collect::<HashSet<_>>();
    let mut seen = parsed
        .events
        .iter()
        .map(|event| event.id.clone())
        .collect::<HashSet<_>>();
    let mut retained = Vec::new();
    for event in history {
        if event.total_tokens <= 0 || seen.contains(&event.id) {
            continue;
        }
        let Some(thread_id) = freebuff_event_thread_id(&event.id) else {
            continue;
        };
        if !live_threads.contains(thread_id) {
            continue;
        }
        seen.insert(event.id.clone());
        retained.push(event.clone());
    }
    parsed.events.extend(retained);
    parsed
}
