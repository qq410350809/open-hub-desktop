use crate::db;
use crate::models::{
    Database, RawConversation, RawLogReport, RawRequest, RawSession, RequestHealthBucket,
    RequestHealthReport, RequestHealthSourceSummary, TokenCollectorSyncReport, TokenStatsReport,
    TokenUsageBucket, TokenUsageReport,
};
use crate::token_collector;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};
use tauri::{AppHandle, Manager, State};

/// Token 查询接口只读取 OpenHub SQLite 快照，不触发日志扫描。
#[tauri::command]
pub async fn get_token_stats(
    database: State<'_, Database>,
    from: Option<String>,
    to: Option<String>,
    refresh: Option<bool>,
) -> Result<TokenStatsReport, String> {
    let _ = refresh;
    query_token_stats(&database, from, to)
}

/// 手动触发一次本地日志采集并写入 SQLite；查询仍由独立接口完成。
#[tauri::command]
pub async fn sync_token_data(
    app: AppHandle,
    force: Option<bool>,
) -> Result<TokenCollectorSyncReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let database = app.state::<Database>();
        collect_token_data(&database, force.unwrap_or(false))
    })
    .await
    .map_err(|error| format!("OpenHub Token 采集任务失败：{error}"))?
}

/// CatPawAI 仍由 OpenHub 直接读取本地 SQLite，并合并进统一小时桶。
const CATPAWAI_SOURCE: &str = "catpawai";
const CATPAWAI_UNKNOWN_MODEL: &str = "catpawai-unknown-model";

/// CatPawAI 的会话与逐请求 Token 数据保存在本地 SQLite。
/// 环境变量便于测试或适配自定义数据目录，默认覆盖当前 macOS/通用 HOME 布局。
fn catpawai_db_path() -> Option<PathBuf> {
    if let Ok(value) = std::env::var("OPENHUB_CATPAWAI_DB_PATH") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    [
        home.join(".sankuai")
            .join("CatPawAI")
            .join("sqliteDB")
            .join("globalCache.sqlite"),
        home.join("Library")
            .join("Application Support")
            .join("CatPawAI")
            .join("sqliteDB")
            .join("globalCache.sqlite"),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

fn sqlite_table_exists(conn: &Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count > 0)
    .unwrap_or(false)
}

fn catpawai_nonempty_string(value: &JsonValue, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn catpawai_selected_model(value: &JsonValue) -> Option<String> {
    catpawai_nonempty_string(value, "selectedModelName")
        .or_else(|| {
            value
                .get("submitEditorState")
                .and_then(|state| catpawai_nonempty_string(state, "selectedModelName"))
        })
        .or_else(|| {
            value
                .get("submitEditorState")
                .and_then(|state| state.get("selectedModelInfo"))
                .and_then(|info| catpawai_nonempty_string(info, "modelTypeName"))
        })
}

fn catpawai_actual_model(value: &JsonValue) -> Option<String> {
    catpawai_nonempty_string(value, "actualUseModelName").or_else(|| {
        value
            .get("blockData")
            .and_then(|block| catpawai_nonempty_string(block, "actualUseModelName"))
    })
}

/// CatPawAI 某些版本把 actualUseModelName 写成内部数字 ID。
/// 这类值无法用于展示或定价，需回退到同一会话最近一次 selectedModelName。
fn catpawai_model_is_resolved(model: &str) -> bool {
    let value = model.trim();
    !value.is_empty()
        && !value.eq_ignore_ascii_case("unknown")
        && !value.chars().all(|ch| ch.is_ascii_digit())
}

fn catpawai_usage(value: &JsonValue) -> Option<&JsonValue> {
    value
        .get("tokenUsage")
        .filter(|usage| usage.is_object())
        .or_else(|| {
            value
                .get("blockData")
                .and_then(|block| block.get("usage"))
                .filter(|usage| usage.is_object())
        })
}

fn catpawai_number(value: &JsonValue, keys: &[&str]) -> i64 {
    keys.iter()
        .find_map(|key| {
            value.get(*key).and_then(|number| {
                number
                    .as_i64()
                    .or_else(|| number.as_u64().and_then(|n| i64::try_from(n).ok()))
                    .or_else(|| number.as_f64().map(|n| n as i64))
                    .or_else(|| number.as_str().and_then(|n| n.parse::<i64>().ok()))
            })
        })
        .unwrap_or(0)
        .max(0)
}

fn load_catpawai_projects(conn: &Connection) -> BTreeMap<String, String> {
    let mut projects = BTreeMap::new();
    if sqlite_table_exists(conn, "t_conversations") {
        if let Ok(mut stmt) =
            conn.prepare("SELECT conversation_id, workspace_id, title FROM t_conversations")
        {
            if let Ok(rows) = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            }) {
                for row in rows.flatten() {
                    let (conversation_id, workspace_id, title) = row;
                    let project = workspace_id
                        .filter(|value| !value.trim().is_empty())
                        .or_else(|| title.filter(|value| !value.trim().is_empty()))
                        .unwrap_or_else(|| "CatPawAI".to_string());
                    projects.insert(conversation_id, project);
                }
            }
        }
    }
    // 兼容旧版 CatPawAI 表结构；新表优先，旧表仅补缺失会话。
    if sqlite_table_exists(conn, "t_conversation") {
        if let Ok(mut stmt) =
            conn.prepare("SELECT conversation_id, project_path, history_title FROM t_conversation")
        {
            if let Ok(rows) = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            }) {
                for row in rows.flatten() {
                    let (conversation_id, project_path, title) = row;
                    projects.entry(conversation_id).or_insert_with(|| {
                        project_path
                            .filter(|value| !value.trim().is_empty())
                            .or_else(|| title.filter(|value| !value.trim().is_empty()))
                            .unwrap_or_else(|| "CatPawAI".to_string())
                    });
                }
            }
        }
    }
    projects
}

fn catpawai_bucket_mut<'a>(
    buckets: &'a mut BTreeMap<(String, String, String), TokenUsageBucket>,
    model: String,
    project_key: String,
    timestamp: String,
) -> &'a mut TokenUsageBucket {
    let key = (model.clone(), project_key.clone(), timestamp.clone());
    buckets.entry(key).or_insert_with(|| TokenUsageBucket {
        source: CATPAWAI_SOURCE.to_string(),
        model,
        project_key,
        timestamp,
        total_tokens: 0,
        billable_total_tokens: 0,
        input_tokens: 0,
        cached_input_tokens: 0,
        cache_creation_input_tokens: 0,
        output_tokens: 0,
        reasoning_output_tokens: 0,
        conversation_count: 0,
        cost_usd: 0.0,
        pricing_available: false,
        estimated_tokens: 0,
    })
}

/// 直读 CatPawAI 的逐请求 tokenUsage，聚合为 OpenHub 使用的小时桶。
/// prompt_tokens 包含 cachedTokens/cacheWriteTokens，必须拆出后再计 fresh input，
/// 否则成本计算与缓存命中率都会重复计算缓存 Token。
fn read_catpawai_buckets_from_path(path: &Path) -> Result<Vec<TokenUsageBucket>, String> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("无法读取 CatPawAI 数据库（{}）：{error}", path.display()))?;
    if !sqlite_table_exists(&conn, "t_ui_messages") {
        return Err("CatPawAI 数据库缺少 t_ui_messages 表".to_string());
    }
    let projects = load_catpawai_projects(&conn);
    let mut stmt = conn
        .prepare(
            "SELECT conversation_id, message_type, create_time, content \
             FROM t_ui_messages ORDER BY conversation_id ASC, create_time ASC, id ASC",
        )
        .map_err(|error| format!("CatPawAI 消息查询准备失败：{error}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|error| format!("CatPawAI 消息查询失败：{error}"))?;

    let mut current_models = BTreeMap::<String, String>::new();
    let mut buckets = BTreeMap::<(String, String, String), TokenUsageBucket>::new();
    for row in rows.flatten() {
        let (conversation_id, message_type, create_time, content) = row;
        let Ok(value) = serde_json::from_str::<JsonValue>(&content) else {
            continue;
        };
        if let Some(selected) = catpawai_selected_model(&value) {
            if catpawai_model_is_resolved(&selected) {
                current_models.insert(conversation_id.clone(), selected);
            }
        }
        let actual_model = catpawai_actual_model(&value);
        let model = actual_model
            .as_deref()
            .filter(|model| catpawai_model_is_resolved(model))
            .map(str::to_string)
            .or_else(|| current_models.get(&conversation_id).cloned())
            .unwrap_or_else(|| CATPAWAI_UNKNOWN_MODEL.to_string());
        let normalized_ms = if create_time > 0 && create_time < 100_000_000_000 {
            create_time.saturating_mul(1000)
        } else {
            create_time
        };
        let Some(timestamp) = hour_key_from_millis(normalized_ms) else {
            continue;
        };
        let project_key = projects
            .get(&conversation_id)
            .cloned()
            .unwrap_or_else(|| "CatPawAI".to_string());

        if message_type == "user_prompt" {
            catpawai_bucket_mut(
                &mut buckets,
                model.clone(),
                project_key.clone(),
                timestamp.clone(),
            )
            .conversation_count += 1;
        }

        let Some(usage) = catpawai_usage(&value) else {
            continue;
        };
        let prompt = catpawai_number(
            usage,
            &[
                "prompt_tokens",
                "promptTokens",
                "input_tokens",
                "inputTokens",
            ],
        );
        let completion = catpawai_number(
            usage,
            &[
                "completion_tokens",
                "completionTokens",
                "output_tokens",
                "outputTokens",
            ],
        );
        let cached_from_details = usage
            .get("promptTokensDetails")
            .or_else(|| usage.get("prompt_tokens_details"))
            .map(|details| catpawai_number(details, &["cachedTokens", "cached_tokens"]))
            .unwrap_or(0);
        let cache_read = catpawai_number(
            usage,
            &[
                "cacheReadTokens",
                "cache_read_tokens",
                "cached_input_tokens",
                "cachedInputTokens",
            ],
        )
        .max(cached_from_details);
        let cache_write = catpawai_number(
            usage,
            &[
                "cacheWriteTokens",
                "cache_write_tokens",
                "cache_creation_input_tokens",
                "cacheCreationInputTokens",
            ],
        );
        let reasoning = usage
            .get("completionTokensDetails")
            .or_else(|| usage.get("completion_tokens_details"))
            .map(|details| catpawai_number(details, &["reasoningTokens", "reasoning_tokens"]))
            .unwrap_or_else(|| {
                catpawai_number(usage, &["reasoning_output_tokens", "reasoningOutputTokens"])
            });
        let total = catpawai_number(usage, &["total_tokens", "totalTokens"])
            .max(prompt.saturating_add(completion));
        if total <= 0 {
            continue;
        }
        let fresh_input = prompt.saturating_sub(cache_read.saturating_add(cache_write));
        let bucket = catpawai_bucket_mut(&mut buckets, model, project_key, timestamp);
        bucket.total_tokens += total;
        bucket.billable_total_tokens += total;
        bucket.input_tokens += fresh_input;
        bucket.cached_input_tokens += cache_read;
        bucket.cache_creation_input_tokens += cache_write;
        bucket.output_tokens += completion;
        bucket.reasoning_output_tokens += reasoning.min(completion);
    }
    Ok(buckets.into_values().collect())
}

fn read_catpawai_buckets() -> Result<Vec<TokenUsageBucket>, String> {
    let Some(path) = catpawai_db_path() else {
        return Ok(Vec::new());
    };
    read_catpawai_buckets_from_path(&path)
}

fn is_catpawai_source(source: &str) -> bool {
    let normalized = source
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    normalized == "catpawai" || normalized == "catpaw"
}

fn merge_catpawai_usage(mut report: TokenUsageReport) -> Result<TokenUsageReport, String> {
    let catpawai_buckets = read_catpawai_buckets()?;
    if !report
        .buckets
        .iter()
        .any(|bucket| is_catpawai_source(&bucket.source))
    {
        report.buckets.extend(catpawai_buckets);
    }
    report.buckets.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.source.cmp(&right.source))
            .then_with(|| left.model.cmp(&right.model))
            .then_with(|| left.project_key.cmp(&right.project_key))
    });
    report.start_date.clear();
    report.end_date.clear();
    for bucket in &report.buckets {
        let day = bucket.timestamp.get(..10).unwrap_or("");
        if day.is_empty() {
            continue;
        }
        if report.start_date.is_empty() || day < report.start_date.as_str() {
            report.start_date = day.to_string();
        }
        if report.end_date.is_empty() || day > report.end_date.as_str() {
            report.end_date = day.to_string();
        }
    }
    report.available = !report.buckets.is_empty();
    Ok(report)
}

pub(crate) fn query_token_usage(database: &Database) -> Result<TokenUsageReport, String> {
    Ok(db::read_token_usage_snapshot(database)?.unwrap_or_default())
}

pub(crate) fn query_token_stats(
    database: &Database,
    from: Option<String>,
    to: Option<String>,
) -> Result<TokenStatsReport, String> {
    let sessions = db::read_token_sessions_snapshot(database)?.unwrap_or_default();
    Ok(token_collector::build_token_stats(sessions, from, to))
}

pub(crate) fn query_token_health(database: &Database) -> Result<RequestHealthReport, String> {
    Ok(db::read_token_health_snapshot(database)?.unwrap_or_default())
}

/// 只查询 SQLite 中的 Token 用量快照。
#[tauri::command]
pub async fn get_token_usage(database: State<'_, Database>) -> Result<TokenUsageReport, String> {
    query_token_usage(&database)
}

/// 解析一个 Claude 会话 jsonl 文件为 会话 + 对话 + 请求。
/// 对话 = 每次 user 消息开新轮；请求 = 每条带 usage 的 assistant 消息（真实 API 请求，含 token 数）；
/// 跳过子代理线程。
fn parse_claude_file(
    path: &Path,
    project: &str,
    sessions: &mut Vec<RawSession>,
    conversations: &mut Vec<RawConversation>,
    requests: &mut Vec<RawRequest>,
) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let session_id = path
        .file_stem()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let number = |field: &JsonValue, key: &str| -> i64 {
        field
            .get(key)
            .and_then(JsonValue::as_f64)
            .map(|value| value as i64)
            .unwrap_or(0)
    };
    let mut model = String::new();
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut message_count = 0i64;
    let mut conv_index = 0i64;
    let mut session_tokens = 0i64;
    let mut current: Option<(RawConversation, String)> = None;

    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };
        if value
            .get("isSidechain")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        let kind = value.get("type").and_then(JsonValue::as_str).unwrap_or("");
        if kind != "user" && kind != "assistant" {
            continue;
        }
        let ts = value
            .get("timestamp")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        let uuid = value
            .get("uuid")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        let msg_model = value
            .get("message")
            .and_then(|message| message.get("model"))
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        if model.is_empty() && !msg_model.is_empty() {
            model = msg_model.clone();
        }
        if first_ts.is_empty() {
            first_ts = ts.clone();
        }
        last_ts = ts.clone();
        message_count += 1;

        if kind == "user" {
            if let Some((conv, _)) = current.take() {
                conversations.push(conv);
            }
            conv_index += 1;
            current = Some((
                RawConversation {
                    id: format!("{session_id}#{conv_index}"),
                    session_id: session_id.clone(),
                    source: "claude".into(),
                    project: project.to_string(),
                    index: conv_index,
                    started_at: ts.clone(),
                    ..Default::default()
                },
                ts.clone(),
            ));
            continue;
        }

        // assistant：提取真实 API 请求的 token 用量
        let usage = value
            .get("message")
            .and_then(|message| message.get("usage"));
        let Some(usage) = usage.filter(|u| u.is_object()) else {
            continue;
        };
        let input = number(usage, "input_tokens");
        let cache_read = number(usage, "cache_read_input_tokens");
        let cache_creation = number(usage, "cache_creation_input_tokens");
        let output = number(usage, "output_tokens");
        let total = input + cache_read + cache_creation + output;
        if total <= 0 {
            continue;
        }
        session_tokens += total;
        if let Some((conv, conv_last)) = current.as_mut() {
            conv.request_count += 1;
            if !msg_model.is_empty() {
                conv.model = msg_model.clone();
            }
            if ts > *conv_last {
                *conv_last = ts.clone();
            }
            conv.total_tokens += total;
            requests.push(RawRequest {
                id: if uuid.is_empty() {
                    format!("{session_id}#{message_count}")
                } else {
                    uuid
                },
                session_id: session_id.clone(),
                conversation_id: conv.id.clone(),
                source: "claude".into(),
                timestamp: ts,
                role: kind.to_string(),
                model: msg_model,
                input_tokens: input,
                cache_read_tokens: cache_read,
                cache_creation_tokens: cache_creation,
                output_tokens: output,
                total_tokens: total,
            });
        }
    }
    if let Some((mut conv, last)) = current.take() {
        conv.ended_at = last;
        conversations.push(conv);
    }
    sessions.push(RawSession {
        id: session_id,
        source: "claude".into(),
        project: project.to_string(),
        started_at: first_ts,
        ended_at: last_ts,
        message_count,
        conversation_count: conv_index,
        model,
        total_tokens: session_tokens,
    });
}

/// 解析一个 Codex rollout 文件为会话（Codex rollout 无逐请求 token 用量，只统计会话结构）。
fn parse_codex_file(path: &Path, sessions: &mut Vec<RawSession>) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let session_id = path
        .file_stem()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut message_count = 0i64;

    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };
        if value.get("type").and_then(JsonValue::as_str) != Some("response_item") {
            continue;
        }
        let payload = value.get("payload");
        if payload
            .and_then(|p| p.get("type"))
            .and_then(JsonValue::as_str)
            != Some("message")
        {
            continue;
        }
        let role = payload
            .and_then(|p| p.get("role"))
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        if role != "user" && role != "assistant" {
            continue;
        }
        let ts = value
            .get("timestamp")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        if first_ts.is_empty() {
            first_ts = ts.clone();
        }
        last_ts = ts.clone();
        message_count += 1;
    }
    sessions.push(RawSession {
        id: session_id,
        source: "codex".into(),
        project: String::new(),
        started_at: first_ts,
        ended_at: last_ts,
        message_count,
        conversation_count: 0,
        model: String::new(),
        total_tokens: 0,
    });
}

fn collect_codex_files(dir: &Path, sessions: &mut Vec<RawSession>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_codex_files(&path, sessions);
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
            .unwrap_or(false)
        {
            parse_codex_file(&path, sessions);
        }
    }
}

/// 从原始日志解析 会话/对话/请求 三级列表。
/// 会话 = 会话文件；对话 = 每次用户提问轮；请求 = 每条消息。
#[tauri::command]
pub async fn get_token_raw_logs() -> Result<RawLogReport, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let home = std::env::var_os("HOME").ok_or("无法定位用户目录")?;
        let home = PathBuf::from(home);
        let mut sessions: Vec<RawSession> = Vec::new();
        let mut conversations: Vec<RawConversation> = Vec::new();
        let mut requests: Vec<RawRequest> = Vec::new();

        let claude_root = home.join(".claude").join("projects");
        if let Ok(projects) = fs::read_dir(&claude_root) {
            for project_entry in projects.flatten() {
                let project_dir = project_entry.path();
                if !project_dir.is_dir() {
                    continue;
                }
                let project = project_dir
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default();
                if let Ok(files) = fs::read_dir(&project_dir) {
                    for file in files.flatten() {
                        let path = file.path();
                        if !path.is_file() {
                            continue;
                        }
                        if path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .map(|name| name.ends_with(".jsonl"))
                            .unwrap_or(false)
                        {
                            parse_claude_file(
                                &path,
                                &project,
                                &mut sessions,
                                &mut conversations,
                                &mut requests,
                            );
                        }
                    }
                }
            }
        }

        let codex_root = home.join(".codex").join("sessions");
        collect_codex_files(&codex_root, &mut sessions);

        Ok(RawLogReport {
            available: !sessions.is_empty(),
            sessions,
            conversations,
            requests,
        })
    })
    .await
    .map_err(|error| format!("原始日志解析失败：{error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_queries_read_only_database_snapshots() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE token_cache_snapshots (
                    kind TEXT PRIMARY KEY,
                    payload_json TEXT NOT NULL,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );",
            )
            .unwrap();
        let database = Database(std::sync::Mutex::new(connection));
        let usage = TokenUsageReport {
            available: true,
            buckets: vec![TokenUsageBucket {
                source: "antigravity".to_string(),
                model: "gemini-pro-default".to_string(),
                timestamp: "2026-08-12T01:00:00.000Z".to_string(),
                total_tokens: 321,
                ..Default::default()
            }],
            ..Default::default()
        };
        let sessions = vec![crate::models::TokenSession {
            version: 1,
            session_hash: "openhub:antigravity:db-test".to_string(),
            source: "antigravity".to_string(),
            model: "gemini-pro-default".to_string(),
            started_at: "2026-08-12T01:00:00.000Z".to_string(),
            ended_at: "2026-08-12T01:01:00.000Z".to_string(),
            turns: 1,
            total_tokens: 321,
            ..Default::default()
        }];
        let health = RequestHealthReport {
            available: true,
            buckets: vec![RequestHealthBucket {
                hour: "2026-08-12T01:00:00.000Z".to_string(),
                dialogues: 1,
                requests: 2,
                success: 2,
                failed: 0,
            }],
            ..Default::default()
        };
        db::write_token_snapshots(&database, &usage, &sessions, &health).unwrap();

        assert_eq!(
            query_token_usage(&database).unwrap().buckets[0].total_tokens,
            321
        );
        assert_eq!(
            query_token_stats(
                &database,
                Some("2026-08-12".to_string()),
                Some("2026-08-12".to_string()),
            )
            .unwrap()
            .summary
            .total_tokens,
            321
        );
        assert_eq!(
            query_token_health(&database).unwrap().buckets[0].requests,
            2
        );
    }

    #[test]
    fn reads_catpawai_usage_and_resolves_numeric_model_ids() {
        let path = std::env::temp_dir().join(format!(
            "openhub-catpawai-usage-{}.sqlite",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE t_conversations (
                conversation_id TEXT PRIMARY KEY,
                workspace_id TEXT,
                title TEXT
            );
            CREATE TABLE t_ui_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                conversation_id TEXT NOT NULL,
                message_type TEXT NOT NULL,
                create_time INTEGER NOT NULL,
                content TEXT NOT NULL
            );
            "#,
        )
        .unwrap();
        conn.execute(
            "INSERT INTO t_conversations (conversation_id, workspace_id, title) VALUES (?1, ?2, ?3)",
            rusqlite::params!["conversation-1", "/Applications/custom/OpenHub", "OpenHub"],
        )
        .unwrap();
        let insert = |message_type: &str, create_time: i64, content: &str| {
            conn.execute(
                "INSERT INTO t_ui_messages (conversation_id, message_type, create_time, content) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params!["conversation-1", message_type, create_time, content],
            )
            .unwrap();
        };
        insert(
            "user_prompt",
            1_786_413_900_000,
            r#"{"selectedModelName":"glm-5.2","submitEditorState":{"selectedModelName":"glm-5.2"}}"#,
        );
        insert(
            "tool",
            1_786_413_960_000,
            r#"{"actualUseModelName":"100000000037","tokenUsage":{"prompt_tokens":100,"completion_tokens":20,"promptTokensDetails":{"cachedTokens":60},"cacheWriteTokens":10,"completionTokensDetails":{"reasoningTokens":5},"total_tokens":120}}"#,
        );
        // 兼容 tokenUsage 只保存在 blockData.usage 的历史版本。
        insert(
            "text",
            1_786_414_020_000,
            r#"{"blockData":{"actualUseModelName":"100000000037","usage":{"prompt_tokens":50,"completion_tokens":10,"promptTokensDetails":{"cachedTokens":20},"total_tokens":60}}}"#,
        );
        drop(conn);

        let buckets = read_catpawai_buckets_from_path(&path).unwrap();
        let _ = fs::remove_file(&path);
        assert_eq!(buckets.len(), 1);
        let bucket = &buckets[0];
        assert_eq!(bucket.source, CATPAWAI_SOURCE);
        assert_eq!(bucket.model, "glm-5.2");
        assert_eq!(bucket.project_key, "/Applications/custom/OpenHub");
        assert_eq!(bucket.timestamp, "2026-08-11T02:00:00.000Z");
        assert_eq!(bucket.total_tokens, 180);
        assert_eq!(bucket.billable_total_tokens, 180);
        assert_eq!(bucket.input_tokens, 60); // (100-60-10) + (50-20)
        assert_eq!(bucket.cached_input_tokens, 80);
        assert_eq!(bucket.cache_creation_input_tokens, 10);
        assert_eq!(bucket.output_tokens, 30);
        assert_eq!(bucket.reasoning_output_tokens, 5);
        assert_eq!(bucket.conversation_count, 1);
        let serialized = serde_json::to_value(bucket).unwrap();
        assert_eq!(
            serialized.get("projectKey").and_then(JsonValue::as_str),
            Some("/Applications/custom/OpenHub")
        );
    }

    #[test]
    fn recognizes_catpawai_source_aliases_for_deduplication() {
        assert!(is_catpawai_source("catpawai"));
        assert!(is_catpawai_source("CatPaw-AI"));
        assert!(is_catpawai_source("catpaw"));
        assert!(!is_catpawai_source("claude"));
    }

    #[test]
    fn parses_claude_session_into_conversations_and_requests() {
        let dir = std::env::temp_dir().join("openhub-tt-test-claude");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session-abc.jsonl");
        fs::write(
            &path,
            r#"{"type":"queue-operation","timestamp":"2026-08-01T00:00:00.000Z"}
{"type":"user","uuid":"u1","isSidechain":false,"timestamp":"2026-08-01T00:00:01.000Z","message":{"role":"user","model":"deepseek-v4-flash"}}
{"type":"assistant","uuid":"a1","isSidechain":false,"timestamp":"2026-08-01T00:00:02.000Z","message":{"role":"assistant","model":"deepseek-v4-flash","usage":{"input_tokens":100,"cache_read_input_tokens":50,"cache_creation_input_tokens":10,"output_tokens":40}}}
{"type":"user","uuid":"u2","isSidechain":false,"timestamp":"2026-08-01T00:00:03.000Z","message":{"role":"user"}}
{"type":"assistant","uuid":"a2","isSidechain":false,"timestamp":"2026-08-01T00:00:04.000Z","message":{"role":"assistant","model":"deepseek-v4-flash","usage":{"input_tokens":200,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":60}}}
{"type":"user","uuid":"s1","isSidechain":true,"timestamp":"2026-08-01T00:00:05.000Z","message":{"role":"user"}}
"#,
        )
        .unwrap();
        let mut sessions = Vec::new();
        let mut conversations = Vec::new();
        let mut requests = Vec::new();
        parse_claude_file(
            &path,
            "OpenHub",
            &mut sessions,
            &mut conversations,
            &mut requests,
        );

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "session-abc");
        assert_eq!(sessions[0].message_count, 4);
        assert_eq!(sessions[0].conversation_count, 2);
        assert_eq!(sessions[0].model, "deepseek-v4-flash");
        assert_eq!(sessions[0].total_tokens, 460); // 200+50+10+40 + 200+0+0+60
        assert_eq!(conversations.len(), 2);
        assert_eq!(conversations[0].request_count, 1);
        assert_eq!(conversations[0].total_tokens, 200);
        assert_eq!(conversations[1].request_count, 1);
        assert_eq!(conversations[1].total_tokens, 260);
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|r| r.role == "assistant"));
        assert_eq!(requests[0].input_tokens, 100);
        assert_eq!(requests[0].cache_read_tokens, 50);
        assert_eq!(requests[0].total_tokens, 200);
        // 子代理消息被跳过
        assert!(requests.iter().all(|r| r.id != "s1"));
        // camelCase 序列化
        let serialized = serde_json::to_value(&sessions[0]).unwrap();
        assert!(serialized.get("messageCount").is_some());
        assert!(serialized.get("conversationCount").is_some());
        assert!(serialized.get("totalTokens").is_some());
    }

    #[test]
    fn parses_cursors_hourly_buckets() {
        let payload = r#"{
            "hourly": {
                "buckets": {
                    "codex|gpt-5.6-sol|2026-07-29T01:00:00.000Z": {
                        "totals": {
                            "input_tokens": 751496,
                            "cached_input_tokens": 750328,
                            "cache_creation_input_tokens": 0,
                            "output_tokens": 24267,
                            "reasoning_output_tokens": 4113,
                            "total_tokens": 1526091,
                            "billable_total_tokens": 1526091,
                            "conversation_count": 26
                        }
                    },
                    "claude|deepseek-v4-flash|2026-08-01T10:30:00.000Z": {
                        "totals": {
                            "input_tokens": 100,
                            "cached_input_tokens": 0,
                            "cache_creation_input_tokens": 0,
                            "output_tokens": 50,
                            "reasoning_output_tokens": 0,
                            "total_tokens": 150,
                            "billable_total_tokens": 150,
                            "conversation_count": 2
                        }
                    }
                }
            }
        }"#;
        let value: JsonValue = serde_json::from_str(payload).unwrap();
        let buckets = value
            .get("hourly")
            .and_then(|h| h.get("buckets"))
            .and_then(JsonValue::as_object)
            .unwrap();
        let mut parsed = Vec::new();
        for (key, v) in buckets {
            let parts = key.split('|').collect::<Vec<_>>();
            let totals = v.get("totals").cloned().unwrap_or(JsonValue::Null);
            parsed.push(TokenUsageBucket {
                source: parts[0].to_string(),
                model: parts[1].to_string(),
                project_key: String::new(),
                timestamp: parts[2].to_string(),
                total_tokens: totals
                    .get("total_tokens")
                    .and_then(JsonValue::as_f64)
                    .unwrap() as i64,
                billable_total_tokens: totals
                    .get("billable_total_tokens")
                    .and_then(JsonValue::as_f64)
                    .unwrap() as i64,
                input_tokens: totals
                    .get("input_tokens")
                    .and_then(JsonValue::as_f64)
                    .unwrap() as i64,
                cached_input_tokens: totals
                    .get("cached_input_tokens")
                    .and_then(JsonValue::as_f64)
                    .unwrap() as i64,
                cache_creation_input_tokens: totals
                    .get("cache_creation_input_tokens")
                    .and_then(JsonValue::as_f64)
                    .unwrap() as i64,
                output_tokens: totals
                    .get("output_tokens")
                    .and_then(JsonValue::as_f64)
                    .unwrap() as i64,
                reasoning_output_tokens: totals
                    .get("reasoning_output_tokens")
                    .and_then(JsonValue::as_f64)
                    .unwrap() as i64,
                conversation_count: totals
                    .get("conversation_count")
                    .and_then(JsonValue::as_f64)
                    .unwrap() as i64,
                cost_usd: 0.0,
                pricing_available: false,
                estimated_tokens: 0,
            });
        }
        assert_eq!(parsed.len(), 2);
        let codex = parsed
            .iter()
            .find(|bucket| bucket.source == "codex")
            .expect("codex bucket");
        assert_eq!(codex.model, "gpt-5.6-sol");
        assert_eq!(codex.total_tokens, 1_526_091);
        assert_eq!(codex.conversation_count, 26);
        let serialized = serde_json::to_value(codex).unwrap();
        assert!(serialized.get("totalTokens").is_some());
        assert!(serialized.get("conversationCount").is_some());
        assert!(serialized.get("costUsd").is_some());
        assert!(serialized.get("pricingAvailable").is_some());
    }

    #[test]
    fn parses_a_legacy_snake_case_sessions_payload() {
        let payload = r#"{
            "available": true,
            "session_count": 1,
            "sessions": [
                {
                    "version": 10,
                    "session_hash": "abc123",
                    "source": "claude",
                    "project_key": "OpenHub",
                    "model": "deepseek-v4-flash",
                    "started_at": "2026-08-07T14:27:07.365Z",
                    "ended_at": "2026-08-07T14:31:10.586Z",
                    "active_ms": 243222,
                    "turns": 3,
                    "edit_turns": 0,
                    "retry_turns": 0,
                    "subagent_calls": 0,
                    "subagent_types": {},
                    "tokens": {
                        "input_tokens": 100,
                        "cached_input_tokens": 0,
                        "cache_creation_input_tokens": 0,
                        "output_tokens": 50,
                        "reasoning_output_tokens": 0,
                        "total_tokens": 150
                    },
                    "provenance": {
                        "source": "local-session-log",
                        "confidence": "observed",
                        "retry_confidence": "inferred",
                        "content_retained": false
                    },
                    "duration_ms": 243222,
                    "total_tokens": 150,
                    "cost_usd": 0.01,
                    "productive": false,
                    "first_pass": false,
                    "one_shot": false,
                    "tokens_per_edit": null,
                    "cost_per_edit": null
                }
            ],
            "summary": {
                "sessions": 1,
                "productive_sessions": 0,
                "one_shot_sessions": 0,
                "edit_turns": 0,
                "retries": 0,
                "total_tokens": 150,
                "cost_usd": 0.01,
                "edit_tokens": 0,
                "edit_cost_usd": 0,
                "productive_rate": 0,
                "one_shot_rate": null,
                "edit_sessions": 0,
                "first_pass_sessions": 0,
                "edit_session_rate": 0,
                "first_pass_rate": null,
                "tokens_per_edit": null,
                "cost_per_edit": null
            },
            "by_model": [
                {
                    "model": "deepseek-v4-flash",
                    "sessions": 1,
                    "productive_sessions": 0,
                    "one_shot_sessions": 0,
                    "edit_turns": 0,
                    "retries": 0,
                    "total_tokens": 150,
                    "cost_usd": 0.01,
                    "edit_tokens": 0,
                    "edit_cost_usd": 0,
                    "productive_rate": 0,
                    "one_shot_rate": null,
                    "edit_sessions": 0,
                    "first_pass_sessions": 0,
                    "edit_session_rate": 0,
                    "first_pass_rate": null,
                    "tokens_per_edit": null,
                    "cost_per_edit": null
                }
            ],
            "subagents": [],
            "provenance": {
                "source": "local-session-log",
                "confidence": "observed",
                "privacy": "metadata-only"
            }
        }"#;

        let report: TokenStatsReport = serde_json::from_str(payload).unwrap();
        assert!(report.available);
        assert_eq!(report.session_count, 1);
        assert_eq!(report.sessions.len(), 1);
        assert_eq!(report.sessions[0].source, "claude");
        assert_eq!(report.sessions[0].tokens.total_tokens, 150);
        assert_eq!(report.summary.total_tokens, 150);
        assert_eq!(report.by_model[0].model, "deepseek-v4-flash");
        assert_eq!(report.summary.one_shot_rate, None);

        // 前端拿到的键是 camelCase。
        let serialized = serde_json::to_value(&report).unwrap();
        assert!(serialized.get("sessionCount").is_some());
        assert!(serialized.get("byModel").is_some());
        assert!(serialized["summary"].get("totalTokens").is_some());
        assert!(serialized["summary"].get("oneShotRate").is_some());
        assert!(serialized["sessions"][0].get("totalTokens").is_some());
    }

    #[test]
    fn defaults_tolerate_a_missing_optional_rates() {
        let payload = r#"{
            "available": true,
            "session_count": 0,
            "sessions": [],
            "summary": {
                "sessions": 0,
                "productive_sessions": 0,
                "one_shot_sessions": 0,
                "edit_turns": 0,
                "retries": 0,
                "total_tokens": 0,
                "cost_usd": 0,
                "edit_tokens": 0,
                "edit_cost_usd": 0,
                "productive_rate": 0,
                "one_shot_rate": null,
                "edit_sessions": 0,
                "first_pass_sessions": 0,
                "edit_session_rate": 0,
                "first_pass_rate": null,
                "tokens_per_edit": null,
                "cost_per_edit": null
            },
            "by_model": [],
            "subagents": [],
            "provenance": {}
        }"#;
        let report: TokenStatsReport = serde_json::from_str(payload).unwrap();
        assert!(report.available);
        assert_eq!(report.sessions.len(), 0);
        assert_eq!(report.summary.productive_rate, 0.0);
    }
}

/// 小时桶累加器：dialogues / requests / success / failed
#[derive(Clone, Default)]
struct HealthAgg {
    dialogues: i64,
    requests: i64,
    success: i64,
    failed: i64,
}

fn hour_key_from_ts(ts: &str) -> Option<String> {
    // 接受:
    // - 2026-08-06T04:59:44.123Z
    // - 2026-08-06T04:59:44Z
    // - 2026-08-06T04:59:44+08:00
    let cleaned = ts.trim();
    if cleaned.len() < 13 {
        return None;
    }
    // 取到小时：YYYY-MM-DDTHH
    let prefix = &cleaned[..13];
    if !(prefix.as_bytes().get(4) == Some(&b'-')
        && prefix.as_bytes().get(7) == Some(&b'-')
        && prefix.as_bytes().get(10) == Some(&b'T'))
    {
        return None;
    }
    Some(format!("{prefix}:00:00.000Z"))
}

fn hour_key_from_millis(ms: i64) -> Option<String> {
    if ms <= 0 {
        return None;
    }
    let secs = ms / 1000;
    let t = UNIX_EPOCH + Duration::from_secs(secs as u64);
    let datetime = t.duration_since(UNIX_EPOCH).ok()?;
    let total_secs = datetime.as_secs() as i64;
    let days = total_secs.div_euclid(86_400);
    let tod = total_secs.rem_euclid(86_400);
    let hour = tod / 3600;
    let (y, m, d) = civil_from_days(days);
    Some(format!("{y:04}-{m:02}-{d:02}T{hour:02}:00:00.000Z"))
}

/// Howard Hinnant civil_from_days (UTC)
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

fn json_i64(value: &JsonValue, key: &str) -> i64 {
    value
        .get(key)
        .and_then(|v| v.as_f64().or_else(|| v.as_i64().map(|n| n as f64)))
        .map(|n| n as i64)
        .unwrap_or(0)
}

/// 用户主动取消 / 中断：计请求，但不算模型失败（避免健康格子被误标红）
fn is_user_cancelled_error(err: &JsonValue) -> bool {
    let mut parts: Vec<String> = Vec::new();
    fn walk(v: &JsonValue, out: &mut Vec<String>) {
        match v {
            JsonValue::String(s) => out.push(s.to_ascii_lowercase()),
            JsonValue::Object(map) => {
                for (k, val) in map {
                    out.push(k.to_ascii_lowercase());
                    walk(val, out);
                }
            }
            JsonValue::Array(arr) => {
                for val in arr {
                    walk(val, out);
                }
            }
            _ => {}
        }
    }
    walk(err, &mut parts);
    let blob = parts.join(" ");
    blob.contains("cancel")
        || blob.contains("aborted")
        || blob.contains("abort")
        || blob.contains("interrupted")
        || blob.contains("user_cancelled")
        || blob.contains("cancelled_by_user")
}

fn bump(
    map: &mut BTreeMap<String, HealthAgg>,
    hour: String,
    dialogues: i64,
    requests: i64,
    success: i64,
    failed: i64,
) {
    let entry = map.entry(hour).or_default();
    entry.dialogues += dialogues;
    entry.requests += requests;
    entry.success += success;
    entry.failed += failed;
}

fn bump_source(
    sources: &mut BTreeMap<String, HealthAgg>,
    source: &str,
    dialogues: i64,
    requests: i64,
    success: i64,
    failed: i64,
) {
    let entry = sources.entry(source.to_string()).or_default();
    entry.dialogues += dialogues;
    entry.requests += requests;
    entry.success += success;
    entry.failed += failed;
}

fn record(
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
    source: &str,
    hour: String,
    dialogues: i64,
    requests: i64,
    success: i64,
    failed: i64,
) {
    bump(map, hour, dialogues, requests, success, failed);
    bump_source(sources, source, dialogues, requests, success, failed);
}

fn open_readonly_sqlite(path: &Path) -> Option<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()
}

// ---------------------------------------------------------------------------
// 逐行事件处理器（全量与增量共用同一套口径）
// ---------------------------------------------------------------------------

fn codex_on_line(
    value: &JsonValue,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
) {
    if value.get("type").and_then(JsonValue::as_str) != Some("event_msg") {
        return;
    }
    // 兼容 payload / msg 两种结构
    let payload = value
        .get("payload")
        .or_else(|| value.get("msg"))
        .cloned()
        .unwrap_or(JsonValue::Null);
    let Some(p_type) = payload.get("type").and_then(JsonValue::as_str) else {
        return;
    };
    let ts = value
        .get("timestamp")
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    let Some(hour) = hour_key_from_ts(ts) else {
        return;
    };
    match p_type {
        "user_message" => record(map, sources, "codex", hour, 1, 0, 0, 0),
        // token_count ≈ 一次模型请求；成功率用「请求 - 已知失败」推算，不再用 task_complete 成功样本
        "token_count" => record(map, sources, "codex", hour, 0, 1, 0, 0),
        "task_complete" => {
            if let Some(err) = payload.get("error").filter(|e| !e.is_null()) {
                if !is_user_cancelled_error(err) {
                    // 仅计真实失败样本（429/5xx 等），不额外计请求（请求已由 token_count 覆盖）
                    record(map, sources, "codex", hour, 0, 0, 0, 1);
                }
            }
        }
        _ => {}
    }
}

fn claude_user_is_human(content: &JsonValue) -> bool {
    match content {
        JsonValue::String(text) => !text.trim().is_empty(),
        JsonValue::Array(items) => items.iter().any(|item| {
            matches!(
                item.get("type").and_then(JsonValue::as_str),
                Some("text") | Some("image")
            ) || item.get("text").and_then(JsonValue::as_str).is_some()
        }),
        _ => false,
    }
}

fn claude_on_line(
    value: &JsonValue,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
) {
    let Some(type_name) = value.get("type").and_then(JsonValue::as_str) else {
        return;
    };
    let ts = value
        .get("timestamp")
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    let Some(hour) = hour_key_from_ts(ts) else {
        return;
    };

    if type_name == "user" {
        let content = value
            .get("message")
            .and_then(|m| m.get("content"))
            .cloned()
            .unwrap_or(JsonValue::Null);
        if claude_user_is_human(&content) {
            record(map, sources, "claude", hour, 1, 0, 0, 0);
        }
        return;
    }

    if type_name != "assistant" {
        return;
    }
    let is_api_error = value
        .get("isApiErrorMessage")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
        || value.get("error").is_some();
    let usage = value.get("message").and_then(|m| m.get("usage"));
    let usage_tokens = usage
        .map(|u| {
            json_i64(u, "input_tokens")
                + json_i64(u, "output_tokens")
                + json_i64(u, "cache_read_input_tokens")
                + json_i64(u, "cache_creation_input_tokens")
        })
        .unwrap_or(0);
    if is_api_error {
        record(map, sources, "claude", hour, 0, 1, 0, 1);
    } else if usage_tokens > 0 {
        record(map, sources, "claude", hour, 0, 1, 1, 0);
    }
}

fn command_code_on_line(
    value: &JsonValue,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
) {
    let entry_type = value.get("type").and_then(JsonValue::as_str).unwrap_or("");
    let message = if entry_type == "message" {
        value.get("message").unwrap_or(&JsonValue::Null)
    } else {
        value
    };
    let role = message
        .get("role")
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    if role != "user" && role != "assistant" {
        return;
    }
    let timestamp = value
        .get("timestamp")
        .or_else(|| value.get("metadata").and_then(|meta| meta.get("timestamp")))
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    let Some(hour) = hour_key_from_ts(timestamp) else {
        return;
    };
    if role == "user" {
        let content = message.get("content").unwrap_or(&JsonValue::Null);
        if claude_user_is_human(content) {
            record(map, sources, "command-code", hour, 1, 0, 0, 0);
        }
    } else {
        // V2/V3 的每个 assistant 条目都对应一次已返回的模型调用。
        // V2 没有 Token usage，但请求活跃度仍可从本地会话精确恢复。
        record(map, sources, "command-code", hour, 0, 1, 1, 0);
    }
}

fn antigravity_on_line(
    value: &JsonValue,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
) {
    let type_name = value.get("type").and_then(JsonValue::as_str).unwrap_or("");
    let source_name = value
        .get("source")
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    let ts = value
        .get("created_at")
        .or_else(|| value.get("timestamp"))
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    let Some(hour) = hour_key_from_ts(ts) else {
        return;
    };
    match type_name {
        "USER_INPUT" if source_name == "USER_EXPLICIT" => {
            record(map, sources, "antigravity", hour, 1, 0, 0, 0);
        }
        "PLANNER_RESPONSE" => {
            record(map, sources, "antigravity", hour, 0, 1, 1, 0);
        }
        "ERROR_MESSAGE" => {
            record(map, sources, "antigravity", hour, 0, 1, 0, 1);
        }
        _ => {}
    }
}

fn kiro_on_line(
    value: &JsonValue,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
) {
    let payload = value.get("payload").unwrap_or(&JsonValue::Null);
    let type_name = payload
        .get("type")
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    let ts = value
        .get("timestamp")
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    let Some(hour) = hour_key_from_ts(ts) else {
        return;
    };
    match type_name {
        "user" => {
            let content = payload.get("content").unwrap_or(&JsonValue::Null);
            if claude_user_is_human(content) {
                record(map, sources, "kiro", hour, 1, 0, 0, 0);
            }
        }
        "usage_summary" => {
            let request_count = payload
                .get("requestIds")
                .and_then(JsonValue::as_array)
                .map(|ids| ids.len() as i64)
                .unwrap_or(0);
            if request_count <= 0 {
                return;
            }
            let status = payload
                .get("status")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            if status == "failed" || status == "error" {
                record(
                    map,
                    sources,
                    "kiro",
                    hour,
                    0,
                    request_count,
                    0,
                    request_count,
                );
            } else {
                record(
                    map,
                    sources,
                    "kiro",
                    hour,
                    0,
                    request_count,
                    request_count,
                    0,
                );
            }
        }
        _ => {}
    }
}

fn assistant_tokens_positive(data: &JsonValue) -> bool {
    let Some(tokens) = data.get("tokens") else {
        return false;
    };
    json_i64(tokens, "input")
        + json_i64(tokens, "output")
        + json_i64(tokens, "reasoning")
        + json_i64(tokens, "total")
        > 0
}

fn message_hour(value: &JsonValue, time_created: i64) -> Option<String> {
    value
        .get("time")
        .and_then(|t| {
            t.get("completed")
                .or_else(|| t.get("created"))
                .and_then(JsonValue::as_i64)
        })
        .and_then(hour_key_from_millis)
        .or_else(|| hour_key_from_millis(time_created))
}

// ---------------------------------------------------------------------------
// 增量游标
// ---------------------------------------------------------------------------

/// JSONL 文件游标：文件未重写时只读新增字节。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct FileCursor {
    inode: u64,
    size: u64,
    mtime_ms: u64,
    offset: u64,
}

type FileCursorMap = BTreeMap<String, FileCursor>;

/// SQLite 消息游标：只处理 time_created 大于上次游标的新消息。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct SqliteCursor {
    max_time_created: i64,
    #[serde(default)]
    allowed_sessions: HashSet<String>,
    /// (time_created, hour)；当 session 之后出现正规 assistant 时才回放
    #[serde(default)]
    session_users: BTreeMap<String, Vec<(i64, String)>>,
}

#[cfg(unix)]
fn metadata_ino(meta: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.ino()
}

#[cfg(not(unix))]
fn metadata_ino(_meta: &fs::Metadata) -> u64 {
    0
}

fn scan_jsonl_file_incremental(
    path: &Path,
    cursors: &mut FileCursorMap,
    on_line: &mut dyn FnMut(&JsonValue),
) {
    let key = path.to_string_lossy().to_string();
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    let inode = metadata_ino(&meta);
    let size = meta.len();
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0);

    let start = match cursors.get(&key) {
        Some(prev) if prev.inode == inode && size >= prev.offset => prev.offset,
        _ => 0, // 新文件 / 重写 / 截断：从头读（极少见，可接受偶发重复）
    };

    let Ok(file) = fs::File::open(path) else {
        return;
    };
    let mut reader = BufReader::new(file);
    if start > 0 && reader.seek(SeekFrom::Start(start)).is_err() {
        return;
    }
    for line in reader.lines().flatten() {
        if let Ok(value) = serde_json::from_str::<JsonValue>(&line) {
            on_line(&value);
        }
    }
    cursors.insert(
        key,
        FileCursor {
            inode,
            size,
            mtime_ms,
            offset: size,
        },
    );
}

fn collect_jsonl_incremental(
    root: &Path,
    predicate: &dyn Fn(&Path) -> bool,
    cursors: &mut FileCursorMap,
    on_line: &mut dyn FnMut(&JsonValue),
) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if !predicate(&path) {
                continue;
            }
            scan_jsonl_file_incremental(&path, cursors, on_line);
        }
    }
}

fn collect_codex_activity_incremental(
    dir: &Path,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
    cursors: &mut FileCursorMap,
) {
    collect_jsonl_incremental(
        dir,
        &|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
                .unwrap_or(false)
        },
        cursors,
        &mut |value| codex_on_line(value, map, sources),
    );
}

fn collect_claude_activity_incremental(
    dir: &Path,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
    cursors: &mut FileCursorMap,
) {
    collect_jsonl_incremental(
        dir,
        &|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("jsonl"))
                .unwrap_or(false)
        },
        cursors,
        &mut |value| claude_on_line(value, map, sources),
    );
}

fn is_command_code_activity_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| {
            name.ends_with(".jsonl")
                && name != "history.jsonl"
                && !name.ends_with(".checkpoints.jsonl")
                && !name.ends_with(".prompts.jsonl")
        })
        .unwrap_or(false)
}

fn collect_command_code_activity_incremental(
    root: &Path,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
    cursors: &mut FileCursorMap,
) {
    collect_jsonl_incremental(
        root,
        &is_command_code_activity_file,
        cursors,
        &mut |value| command_code_on_line(value, map, sources),
    );
}

fn collect_antigravity_activity_incremental(
    root: &Path,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
    cursors: &mut FileCursorMap,
) {
    collect_jsonl_incremental(
        root,
        &|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name == "transcript.jsonl")
                .unwrap_or(false)
        },
        cursors,
        &mut |value| antigravity_on_line(value, map, sources),
    );
}

fn collect_kiro_activity_incremental(
    root: &Path,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
    cursors: &mut FileCursorMap,
) {
    collect_jsonl_incremental(
        root,
        &|path| path.file_name().and_then(|name| name.to_str()) == Some("messages.jsonl"),
        cursors,
        &mut |value| kiro_on_line(value, map, sources),
    );
}

fn collect_sqlite_message_activity_incremental(
    db_path: &Path,
    source: &str,
    map: &mut BTreeMap<String, HealthAgg>,
    sources_map: &mut BTreeMap<String, HealthAgg>,
    provider_allow: Option<&HashSet<&str>>,
    cursor: &mut SqliteCursor,
) {
    let Some(conn) = open_readonly_sqlite(db_path) else {
        return;
    };
    let filter = provider_allow.is_some();
    let since = cursor.max_time_created;
    let Ok(mut stmt) = conn.prepare(
        "SELECT session_id, time_created, data FROM message WHERE time_created > ?1 ORDER BY time_created ASC",
    ) else {
        return;
    };
    let rows = stmt.query_map([since], |row| {
        let sid: String = row.get(0)?;
        let time_created: i64 = row.get(1)?;
        let data: String = row.get(2)?;
        Ok((sid, time_created, data))
    });
    let Ok(rows) = rows else {
        return;
    };

    let mut new_rows: Vec<(String, i64, JsonValue)> = Vec::new();
    let mut max_tc = since;
    for row in rows.flatten() {
        let (sid, tc, data) = row;
        if tc > max_tc {
            max_tc = tc;
        }
        if let Ok(value) = serde_json::from_str::<JsonValue>(&data) {
            new_rows.push((sid, tc, value));
        }
    }
    if new_rows.is_empty() {
        return;
    }
    cursor.max_time_created = max_tc;

    if filter {
        // 1) 先扩充“正规 session”集合：本轮新增的正规 assistant 所在 session
        let mut newly_allowed: Vec<String> = Vec::new();
        for (sid, _, value) in &new_rows {
            if value.get("role").and_then(JsonValue::as_str) != Some("assistant") {
                continue;
            }
            let provider = value
                .get("providerID")
                .or_else(|| value.get("providerId"))
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            let allowed_provider = provider_allow
                .map(|allow| allow.contains(provider.as_str()))
                .unwrap_or(false);
            if allowed_provider {
                if cursor.allowed_sessions.insert(sid.clone()) {
                    newly_allowed.push(sid.clone());
                }
            }
        }
        // 2) 回放之前暂存的 user（这些 session 现在确认是正规会话）
        for sid in newly_allowed {
            if let Some(users) = cursor.session_users.remove(&sid) {
                for (_, hour) in users {
                    record(map, sources_map, source, hour, 1, 0, 0, 0);
                }
            }
        }
        // 3) 处理本轮新消息
        for (sid, tc, value) in &new_rows {
            let role = value.get("role").and_then(JsonValue::as_str).unwrap_or("");
            let Some(hour) = message_hour(value, *tc) else {
                continue;
            };
            if role == "user" {
                if cursor.allowed_sessions.contains(sid) {
                    record(map, sources_map, source, hour, 1, 0, 0, 0);
                } else {
                    let users = cursor.session_users.entry(sid.clone()).or_default();
                    if users.len() < 100_000 {
                        users.push((*tc, hour));
                    }
                }
                continue;
            }
            if role != "assistant" {
                continue;
            }
            let provider = value
                .get("providerID")
                .or_else(|| value.get("providerId"))
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            let allowed_provider = provider_allow
                .map(|allow| allow.contains(provider.as_str()))
                .unwrap_or(false);
            if !allowed_provider {
                continue;
            }
            let err = value.get("error").filter(|e| !e.is_null());
            if let Some(err) = err {
                if is_user_cancelled_error(err) {
                    // 用户取消：计请求，不算失败
                    record(map, sources_map, source, hour, 0, 1, 0, 0);
                } else {
                    // 真实失败：请求 + 失败
                    record(map, sources_map, source, hour, 0, 1, 0, 1);
                }
                continue;
            }
            if assistant_tokens_positive(value) {
                record(map, sources_map, source, hour, 0, 1, 1, 0);
            }
        }
    } else {
        for (_, tc, value) in &new_rows {
            let role = value.get("role").and_then(JsonValue::as_str).unwrap_or("");
            let Some(hour) = message_hour(value, *tc) else {
                continue;
            };
            if role == "user" {
                record(map, sources_map, source, hour, 1, 0, 0, 0);
                continue;
            }
            if role != "assistant" {
                continue;
            }
            let err = value.get("error").filter(|e| !e.is_null());
            if let Some(err) = err {
                if is_user_cancelled_error(err) {
                    record(map, sources_map, source, hour, 0, 1, 0, 0);
                } else {
                    record(map, sources_map, source, hour, 0, 1, 0, 1);
                }
                continue;
            }
            if assistant_tokens_positive(value) {
                record(map, sources_map, source, hour, 0, 1, 1, 0);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 活动结果持久化（增量游标 + 累计报告）
// ---------------------------------------------------------------------------

const ACTIVITY_CACHE_VERSION: u32 = 4;

/// 请求活动结果缓存 v4：自维护 per-source 增量游标，并覆盖 Codex 归档与 Command Code。
/// OpenHub 只缓存解析结果与游标，不复制原始日志。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ActivityCacheEnvelope {
    version: u32,
    file_cursors: BTreeMap<String, FileCursorMap>,
    sqlite_cursors: BTreeMap<String, SqliteCursor>,
    report: RequestHealthReport,
}

struct ActivityCache {
    report: RequestHealthReport,
    fetched_at: Instant,
}

fn activity_cache() -> &'static Mutex<Option<ActivityCache>> {
    static CACHE: OnceLock<Mutex<Option<ActivityCache>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

const ACTIVITY_CACHE_TTL: Duration = Duration::from_secs(15);

fn activity_cache_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("OPENHUB_ACTIVITY_CACHE_PATH") {
        return Some(PathBuf::from(path));
    }
    let home = PathBuf::from(std::env::var_os("HOME")?);
    #[cfg(target_os = "macos")]
    {
        return Some(
            home.join("Library")
                .join("Application Support")
                .join("com.dfeer.openhub.desktop")
                .join("token-activity-cache.json"),
        );
    }
    #[cfg(target_os = "windows")]
    {
        return std::env::var_os("APPDATA").map(PathBuf::from).map(|path| {
            path.join("com.dfeer.openhub.desktop")
                .join("token-activity-cache.json")
        });
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Some(
            home.join(".local")
                .join("share")
                .join("com.dfeer.openhub.desktop")
                .join("token-activity-cache.json"),
        )
    }
}

fn read_persisted_activity_cache() -> ActivityCacheEnvelope {
    let Some(path) = activity_cache_path() else {
        return ActivityCacheEnvelope::default();
    };
    let Ok(text) = fs::read_to_string(path) else {
        return ActivityCacheEnvelope::default();
    };
    let Ok(envelope) = serde_json::from_str::<ActivityCacheEnvelope>(&text) else {
        return ActivityCacheEnvelope::default();
    };
    if envelope.version != ACTIVITY_CACHE_VERSION {
        return ActivityCacheEnvelope::default();
    }
    envelope
}

fn write_persisted_activity_cache(envelope: &ActivityCacheEnvelope) {
    let Some(path) = activity_cache_path() else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(json) = serde_json::to_vec(envelope) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, json).is_ok() {
        let _ = fs::rename(tmp, path);
    }
}

fn report_to_maps(
    report: &RequestHealthReport,
) -> (BTreeMap<String, HealthAgg>, BTreeMap<String, HealthAgg>) {
    let mut map: BTreeMap<String, HealthAgg> = BTreeMap::new();
    let mut sources: BTreeMap<String, HealthAgg> = BTreeMap::new();
    for bucket in &report.buckets {
        let entry = map.entry(bucket.hour.clone()).or_default();
        entry.dialogues += bucket.dialogues;
        entry.requests += bucket.requests;
        entry.success += bucket.success;
        entry.failed += bucket.failed;
    }
    for summary in &report.by_source {
        let entry = sources.entry(summary.source.clone()).or_default();
        entry.dialogues += summary.dialogues;
        entry.requests += summary.requests;
        entry.success += summary.success;
        entry.failed += summary.failed;
    }
    (map, sources)
}

fn maps_to_report(
    map: BTreeMap<String, HealthAgg>,
    sources: BTreeMap<String, HealthAgg>,
) -> RequestHealthReport {
    let buckets = map
        .into_iter()
        .map(|(hour, agg)| RequestHealthBucket {
            hour,
            dialogues: agg.dialogues,
            requests: agg.requests,
            success: agg.success,
            failed: agg.failed,
        })
        .collect::<Vec<_>>();
    let by_source = sources
        .into_iter()
        .map(|(source, agg)| RequestHealthSourceSummary {
            source,
            dialogues: agg.dialogues,
            requests: agg.requests,
            success: agg.success,
            failed: agg.failed,
        })
        .collect::<Vec<_>>();
    RequestHealthReport {
        available: !buckets.is_empty(),
        buckets,
        by_source,
    }
}

/// 增量扫描请求活动并生成待写入数据库的健康快照。
pub(crate) fn collect_request_health_snapshot(force: bool) -> Result<RequestHealthReport, String> {
    if !force {
        if let Ok(guard) = activity_cache().lock() {
            if let Some(cache) = guard.as_ref() {
                if cache.fetched_at.elapsed() < ACTIVITY_CACHE_TTL {
                    return Ok(cache.report.clone());
                }
            }
        }
    }

    let home = std::env::var_os("HOME").ok_or("无法定位用户目录")?;
    let home = PathBuf::from(home);

    let mut envelope = read_persisted_activity_cache();
    envelope.version = ACTIVITY_CACHE_VERSION;
    if force {
        envelope.report = RequestHealthReport::default();
        envelope.file_cursors.clear();
        envelope.sqlite_cursors.clear();
    }
    let (mut map, mut sources) = report_to_maps(&envelope.report);

    let codex_home = home.join(".codex");
    for (cursor_key, codex_root) in [
        ("codex", codex_home.join("sessions")),
        ("codex-archived", codex_home.join("archived_sessions")),
    ] {
        if codex_root.is_dir() {
            let cursors = envelope
                .file_cursors
                .entry(cursor_key.to_string())
                .or_default();
            collect_codex_activity_incremental(&codex_root, &mut map, &mut sources, cursors);
        }
    }
    let claude_root = home.join(".claude").join("projects");
    if claude_root.is_dir() {
        let cursors = envelope
            .file_cursors
            .entry("claude".to_string())
            .or_default();
        collect_claude_activity_incremental(&claude_root, &mut map, &mut sources, cursors);
    }

    let command_code_root = home.join(".commandcode").join("projects");
    if command_code_root.is_dir() {
        let cursors = envelope
            .file_cursors
            .entry("command-code".to_string())
            .or_default();
        collect_command_code_activity_incremental(
            &command_code_root,
            &mut map,
            &mut sources,
            cursors,
        );
    }

    let opencode_db = home
        .join(".local")
        .join("share")
        .join("opencode")
        .join("opencode.db");
    if opencode_db.is_file() {
        let cursor = envelope
            .sqlite_cursors
            .entry("opencode".to_string())
            .or_default();
        collect_sqlite_message_activity_incremental(
            &opencode_db,
            "opencode",
            &mut map,
            &mut sources,
            None,
            cursor,
        );
    }

    let mimo_db = home
        .join(".local")
        .join("share")
        .join("mimocode")
        .join("mimocode.db");
    if mimo_db.is_file() {
        let allow = HashSet::from(["mimo", "xiaomi"]);
        let cursor = envelope
            .sqlite_cursors
            .entry("mimo".to_string())
            .or_default();
        collect_sqlite_message_activity_incremental(
            &mimo_db,
            "mimo",
            &mut map,
            &mut sources,
            Some(&allow),
            cursor,
        );
    }

    let zcode_db = home.join(".zcode").join("cli").join("db").join("db.sqlite");
    if zcode_db.is_file() {
        let cursor = envelope
            .sqlite_cursors
            .entry("zcode".to_string())
            .or_default();
        collect_sqlite_message_activity_incremental(
            &zcode_db,
            "zcode",
            &mut map,
            &mut sources,
            None,
            cursor,
        );
    }

    let gemini_root = home.join(".gemini");
    if gemini_root.is_dir() {
        let cursors = envelope
            .file_cursors
            .entry("antigravity".to_string())
            .or_default();
        collect_antigravity_activity_incremental(&gemini_root, &mut map, &mut sources, cursors);
    }

    let kiro_root = home.join(".kiro").join("sessions");
    if kiro_root.is_dir() {
        let cursors = envelope.file_cursors.entry("kiro".to_string()).or_default();
        collect_kiro_activity_incremental(&kiro_root, &mut map, &mut sources, cursors);
    }

    let report = maps_to_report(map, sources);
    envelope.report = report.clone();
    write_persisted_activity_cache(&envelope);
    if let Ok(mut guard) = activity_cache().lock() {
        *guard = Some(ActivityCache {
            report: report.clone(),
            fetched_at: Instant::now(),
        });
    }
    Ok(report)
}

fn token_collection_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub(crate) fn collect_token_data(
    database: &Database,
    force: bool,
) -> Result<TokenCollectorSyncReport, String> {
    let _guard = token_collection_lock()
        .lock()
        .map_err(|_| "Token 数据采集锁异常".to_string())?;
    let started = Instant::now();
    let snapshot = token_collector::collect_snapshot(force)?;
    // 即使主采集器文件指纹未变化，也要合并 CatPawAI 与请求健康的独立增量源。
    // 写入的是三份聚合快照而非 20MB 文件游标缓存，事务替换成本可控。
    let usage = merge_catpawai_usage(snapshot.usage.clone())?;
    let health = collect_request_health_snapshot(force)?;
    db::write_token_snapshots(database, &usage, &snapshot.sessions, &health)?;
    Ok(token_collector::sync_report(
        &snapshot,
        started.elapsed().as_millis() as i64,
    ))
}

/// 启动时优先把旧文件缓存迁移进 SQLite，避免界面等待首次扫描。
pub(crate) fn seed_token_database_from_caches(database: &Database) -> Result<bool, String> {
    if db::has_token_snapshots(database)? {
        return Ok(false);
    }
    let Some(snapshot) = token_collector::load_cached_snapshot() else {
        return Ok(false);
    };
    let usage = merge_catpawai_usage(snapshot.usage)?;
    let health = read_persisted_activity_cache().report;
    db::write_token_snapshots(database, &usage, &snapshot.sessions, &health)?;
    Ok(true)
}

/// 查询请求健康时只读取 SQLite；refresh 参数保留用于前端兼容。
#[tauri::command]
pub async fn get_token_request_health(
    database: State<'_, Database>,
    refresh: Option<bool>,
) -> Result<RequestHealthReport, String> {
    let _ = refresh;
    query_token_health(&database)
}

const TOKEN_COLLECT_INTERVAL: Duration = Duration::from_secs(20);

/// 后台采集任务：只负责增量扫描并原子写入 SQLite，不直接驱动页面状态。
pub(crate) fn start_token_collector(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(TOKEN_COLLECT_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            // tokio interval 第一次 tick 立即执行，应用启动后会立刻补一次增量采集。
            interval.tick().await;
            let handle = app.clone();
            let result = tauri::async_runtime::spawn_blocking(move || {
                let database = handle.state::<Database>();
                collect_token_data(&database, false)
            })
            .await;
            match result {
                Ok(Ok(report)) => {
                    if report.changed {
                        eprintln!("[OpenHub] Token 后台采集完成：{}", report.message);
                    }
                }
                Ok(Err(error)) => eprintln!("[OpenHub] Token 后台采集失败：{error}"),
                Err(error) => eprintln!("[OpenHub] Token 后台任务异常：{error}"),
            }
        }
    });
}

#[cfg(test)]
mod activity_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hour_key_normalizes_variants() {
        assert_eq!(
            hour_key_from_ts("2026-08-06T04:59:44.123Z").as_deref(),
            Some("2026-08-06T04:00:00.000Z")
        );
        assert_eq!(
            hour_key_from_ts("2026-08-06T04:59:44Z").as_deref(),
            Some("2026-08-06T04:00:00.000Z")
        );
        assert_eq!(
            hour_key_from_ts("2026-08-06T04:59:44+08:00").as_deref(),
            Some("2026-08-06T04:00:00.000Z")
        );
    }

    #[test]
    fn claude_user_is_human_filters_tool_result() {
        let tool_only = json!([{"type": "tool_result", "content": "ok"}]);
        assert!(!claude_user_is_human(&tool_only));

        let text = json!([{"type": "text", "text": "hello"}]);
        assert!(claude_user_is_human(&text));

        let plain = json!("hello");
        assert!(claude_user_is_human(&plain));
    }

    #[test]
    fn assistant_tokens_positive_reads_nested() {
        let value = json!({"tokens": {"input": 10, "output": 0, "reasoning": 0}});
        assert!(assistant_tokens_positive(&value));
        let empty = json!({"tokens": {"input": 0, "output": 0}});
        assert!(!assistant_tokens_positive(&empty));
    }

    #[test]
    #[ignore = "reads the current user home for a manual smoke test"]
    fn smoke_collects_active_and_archived_codex_activity() {
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
        let mut map = BTreeMap::<String, HealthAgg>::new();
        let mut sources = BTreeMap::<String, HealthAgg>::new();
        let mut active = FileCursorMap::new();
        let mut archived = FileCursorMap::new();
        collect_codex_activity_incremental(
            &home.join(".codex").join("sessions"),
            &mut map,
            &mut sources,
            &mut active,
        );
        let active_requests = map.values().map(|value| value.requests).sum::<i64>();
        collect_codex_activity_incremental(
            &home.join(".codex").join("archived_sessions"),
            &mut map,
            &mut sources,
            &mut archived,
        );
        let total_requests = map.values().map(|value| value.requests).sum::<i64>();
        let active_days = map
            .iter()
            .filter(|(_, value)| value.dialogues > 0 || value.requests > 0)
            .map(|(hour, _)| hour.get(..10).unwrap_or("").to_string())
            .collect::<HashSet<_>>();
        eprintln!(
            "codex activity: active_requests={} total_with_archived={} active_days={:?}",
            active_requests, total_requests, active_days
        );
        assert!(total_requests >= active_requests);
        assert!(!active_days.is_empty());
    }

    #[test]
    fn jsonl_incremental_only_reads_new_bytes() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("openhub-tt-jsonl-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let file = dir.join("rollout-test-1.jsonl");
        let mut map: BTreeMap<String, HealthAgg> = BTreeMap::new();
        let mut sources: BTreeMap<String, HealthAgg> = BTreeMap::new();
        let mut cursors = FileCursorMap::new();

        let line1 = r#"{"type":"event_msg","timestamp":"2026-08-03T09:10:00.000Z","payload":{"type":"user_message"}}"#;
        let mut f = fs::File::create(&file).unwrap();
        writeln!(f, "{line1}").unwrap();
        drop(f);
        collect_codex_activity_incremental(&dir, &mut map, &mut sources, &mut cursors);
        assert_eq!(map.values().map(|a| a.dialogues).sum::<i64>(), 1);

        // 追加新行，第二次只应新增这一条
        let line2 = r#"{"type":"event_msg","timestamp":"2026-08-03T09:11:00.000Z","payload":{"type":"token_count"}}"#;
        let mut f = fs::OpenOptions::new().append(true).open(&file).unwrap();
        writeln!(f, "{line2}").unwrap();
        drop(f);
        collect_codex_activity_incremental(&dir, &mut map, &mut sources, &mut cursors);
        assert_eq!(map.values().map(|a| a.dialogues).sum::<i64>(), 1);
        assert_eq!(map.values().map(|a| a.requests).sum::<i64>(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn command_code_activity_counts_v2_and_v3_messages() {
        let mut map = BTreeMap::new();
        let mut sources = BTreeMap::new();
        command_code_on_line(
            &json!({
                "id": "u1",
                "timestamp": "2026-07-14T03:00:00.000Z",
                "role": "user",
                "content": [{"type": "text", "text": "hello"}],
                "metadata": {"version": 2}
            }),
            &mut map,
            &mut sources,
        );
        command_code_on_line(
            &json!({
                "id": "a1",
                "timestamp": "2026-07-14T03:01:00.000Z",
                "role": "assistant",
                "content": [{"type": "text", "text": "hi"}],
                "metadata": {"version": 2}
            }),
            &mut map,
            &mut sources,
        );
        command_code_on_line(
            &json!({
                "type": "message",
                "id": "a2",
                "timestamp": "2026-08-12T04:01:00.000Z",
                "message": {"role": "assistant", "content": []},
                "usage": {"inputTokens": 10, "outputTokens": 2}
            }),
            &mut map,
            &mut sources,
        );

        assert_eq!(map.values().map(|value| value.dialogues).sum::<i64>(), 1);
        assert_eq!(map.values().map(|value| value.requests).sum::<i64>(), 2);
        assert_eq!(map.values().map(|value| value.success).sum::<i64>(), 2);
        let source = sources.get("command-code").unwrap();
        assert_eq!(source.dialogues, 1);
        assert_eq!(source.requests, 2);
        assert_eq!(source.success, 2);
    }

    #[test]
    fn kiro_activity_uses_request_ids_and_ignores_credit_amount() {
        let mut map = BTreeMap::new();
        let mut sources = BTreeMap::new();
        kiro_on_line(
            &json!({
                "timestamp": "2026-08-13T05:00:00.000Z",
                "payload": {"type": "user", "content": "hello"}
            }),
            &mut map,
            &mut sources,
        );
        kiro_on_line(
            &json!({
                "timestamp": "2026-08-13T05:00:02.000Z",
                "payload": {
                    "type": "usage_summary",
                    "status": "success",
                    "requestIds": ["a", "b"],
                    "promptTurnSummaries": [{"unit": "credit", "usage": 99.0}]
                }
            }),
            &mut map,
            &mut sources,
        );
        let source = sources.get("kiro").unwrap();
        assert_eq!(source.dialogues, 1);
        assert_eq!(source.requests, 2);
        assert_eq!(source.success, 2);
        assert_eq!(source.failed, 0);
    }

    #[test]
    fn command_code_activity_filter_excludes_checkpoint_files() {
        assert!(is_command_code_activity_file(Path::new("session.jsonl")));
        assert!(!is_command_code_activity_file(Path::new(
            "session.checkpoints.jsonl"
        )));
        assert!(!is_command_code_activity_file(Path::new(
            "session.prompts.jsonl"
        )));
        assert!(!is_command_code_activity_file(Path::new("history.jsonl")));
    }

    #[test]
    fn sqlite_cursor_only_counts_new_rows() {
        let db_path =
            std::env::temp_dir().join(format!("openhub-tt-sqlite-{}.db", std::process::id()));
        let _ = fs::remove_file(&db_path);
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE message (session_id TEXT, time_created INTEGER, data TEXT);",
        )
        .unwrap();

        let mut map: BTreeMap<String, HealthAgg> = BTreeMap::new();
        let mut sources: BTreeMap<String, HealthAgg> = BTreeMap::new();
        let mut cursor = SqliteCursor::default();

        let insert = |conn: &Connection, sid: &str, tc: i64, role: &str| {
            let data = format!(
                r#"{{"role":"{role}","tokens":{{"input":10,"output":5}},"time":{{"created":{tc}}}}}"#
            );
            conn.execute(
                "INSERT INTO message (session_id, time_created, data) VALUES (?1, ?2, ?3)",
                rusqlite::params![sid, tc, data],
            )
            .unwrap();
        };
        insert(&conn, "s1", 1000, "user");
        insert(&conn, "s1", 2000, "assistant");
        collect_sqlite_message_activity_incremental(
            &db_path,
            "opencode",
            &mut map,
            &mut sources,
            None,
            &mut cursor,
        );
        assert_eq!(map.values().map(|a| a.dialogues).sum::<i64>(), 1);
        assert_eq!(map.values().map(|a| a.requests).sum::<i64>(), 1);

        // 再插新行，只统计新增
        insert(&conn, "s2", 3000, "user");
        collect_sqlite_message_activity_incremental(
            &db_path,
            "opencode",
            &mut map,
            &mut sources,
            None,
            &mut cursor,
        );
        assert_eq!(map.values().map(|a| a.dialogues).sum::<i64>(), 2);
        assert_eq!(map.values().map(|a| a.requests).sum::<i64>(), 1);
        let _ = fs::remove_file(&db_path);
    }

    #[test]
    fn mimo_cursor_replays_users_when_session_becomes_allowed() {
        let db_path =
            std::env::temp_dir().join(format!("openhub-tt-mimo-{}.db", std::process::id()));
        let _ = fs::remove_file(&db_path);
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE message (session_id TEXT, time_created INTEGER, data TEXT);",
        )
        .unwrap();
        let mut map: BTreeMap<String, HealthAgg> = BTreeMap::new();
        let mut sources: BTreeMap<String, HealthAgg> = BTreeMap::new();
        let mut cursor = SqliteCursor::default();
        let allow = HashSet::from(["mimo", "xiaomi"]);

        let insert = |conn: &Connection, sid: &str, tc: i64, data: &str| {
            conn.execute(
                "INSERT INTO message (session_id, time_created, data) VALUES (?1, ?2, ?3)",
                rusqlite::params![sid, tc, data],
            )
            .unwrap();
        };

        // 第一批：只有 user（session 尚未确认是 mimo）
        insert(
            &conn,
            "s1",
            1000,
            r#"{"role":"user","time":{"created":1000}}"#,
        );
        collect_sqlite_message_activity_incremental(
            &db_path,
            "mimo",
            &mut map,
            &mut sources,
            Some(&allow),
            &mut cursor,
        );
        assert_eq!(map.values().map(|a| a.dialogues).sum::<i64>(), 0);

        // 第二批：出现 mimo assistant，之前的 user 应被回放计入
        insert(
            &conn,
            "s1",
            2000,
            r#"{"role":"assistant","providerID":"mimo","tokens":{"input":10,"output":5},"time":{"created":2000}}"#,
        );
        collect_sqlite_message_activity_incremental(
            &db_path,
            "mimo",
            &mut map,
            &mut sources,
            Some(&allow),
            &mut cursor,
        );
        assert_eq!(map.values().map(|a| a.dialogues).sum::<i64>(), 1);
        assert_eq!(map.values().map(|a| a.requests).sum::<i64>(), 1);
        let _ = fs::remove_file(&db_path);
    }
}
