use crate::db;
use crate::models::{
    Database, LocalAgentEnvOverride, LocalAgentPathEntry, LocalAgentPaths, LocalAgentPathsReport,
    RawConversation, RawLogReport, RawRequest, RawSession, RequestHealthBucket,
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
use tauri::{AppHandle, Emitter, Manager, State};

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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TokenCollectorProgress {
    stage: String,
    status: String,
    message: String,
}

fn emit_token_collector_progress(
    app: Option<&AppHandle>,
    stage: &str,
    status: &str,
    message: impl Into<String>,
) {
    let Some(app) = app else { return };
    let _ = app.emit(
        "token-collector-progress",
        TokenCollectorProgress {
            stage: stage.into(),
            status: status.into(),
            message: message.into(),
        },
    );
}

/// 手动触发一次本地日志采集并写入 SQLite；查询仍由独立接口完成。
#[tauri::command]
pub async fn sync_token_data(
    app: AppHandle,
    force: Option<bool>,
) -> Result<TokenCollectorSyncReport, String> {
    let force = force.unwrap_or(false);
    let worker_app = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        emit_token_collector_progress(
            Some(&worker_app),
            "prepare",
            "running",
            if force {
                "已创建完整刷新任务"
            } else {
                "已创建增量采集任务"
            },
        );
        let database = worker_app.state::<Database>();
        let result = collect_token_data(&database, force, Some(&worker_app));
        if let Err(error) = &result {
            emit_token_collector_progress(Some(&worker_app), "error", "error", error.clone());
        }
        result
    })
    .await;

    match result {
        Ok(result) => result,
        Err(error) => {
            let message = format!("OpenHub Token 采集任务失败：{error}");
            emit_token_collector_progress(Some(&app), "error", "error", message.clone());
            Err(message)
        }
    }
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
        request_count: 0,
        cost_usd: 0.0,
        pricing_available: false,
        estimated_tokens: 0,
        estimated_input_tokens: 0,
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
        // 每条带 usage 的消息就是一次真实 API 请求。
        bucket.request_count += 1;
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
/// 对话 = 每次真人 user 消息开新轮（子代理任务 prompt 不开新轮）；
/// 请求 = 每条带 usage 的 assistant 消息（真实 API 请求，含 token 数，含子代理请求），
/// 同一 message.id 按内容块拆多行只计一次。
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
    // 已计入请求的 message.id（同一请求按内容块拆多行，usage 相同）
    let mut counted_message_ids: HashSet<String> = HashSet::new();

    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };
        let is_sidechain = value
            .get("isSidechain")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
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
            // 子代理任务 prompt 不开新对话轮
            if is_sidechain {
                continue;
            }
            let content = value
                .get("message")
                .and_then(|message| message.get("content"))
                .cloned()
                .unwrap_or(JsonValue::Null);
            if !token_collector::claude_user_line_is_human(&value, &content) {
                continue;
            }
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

        // assistant：提取真实 API 请求的 token 用量（含子代理请求；message.id 去重）
        let message_id = value
            .get("message")
            .and_then(|message| message.get("id"))
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
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
        if !message_id.is_empty() && !counted_message_ids.insert(message_id.clone()) {
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
                // message.id 是一次 API 请求的标识（按内容块拆多行时相同）；
                // 旧日志缺失时回退行级 uuid。
                id: if message_id.is_empty() {
                    if uuid.is_empty() {
                        format!("{session_id}#{message_count}")
                    } else {
                        uuid
                    }
                } else {
                    message_id
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

        let claude_root = crate::token_collector::claude_config_dir(&home).join("projects");
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

        let codex_base = crate::token_collector::codex_home(&home);
        // Codex 会把归档任务移入 archived_sessions/；原始日志视图两处都要扫，
        // 否则会话归档后在“原始日志”里消失（usage 采集器早已是双目录扫描）。
        collect_codex_files(&codex_base.join("sessions"), &mut sessions);
        collect_codex_files(&codex_base.join("archived_sessions"), &mut sessions);

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

// ---------------------------------------------------------------------------
// 本地 AI Agent 路径诊断
// ---------------------------------------------------------------------------

fn path_display(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn size_human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// 路径附加信息：文件显示大小，目录显示直属条目数；不存在时为空。
fn path_detail(path: &Path) -> String {
    if path.is_file() {
        return fs::metadata(path)
            .map(|meta| size_human(meta.len()))
            .unwrap_or_default();
    }
    if path.is_dir() {
        let count = fs::read_dir(path)
            .map(|entries| entries.take(1001).count())
            .unwrap_or(0);
        return if count == 0 {
            String::new()
        } else if count > 1000 {
            "1000+ 项".to_string()
        } else {
            format!("{count} 项")
        };
    }
    String::new()
}

/// 追加一条路径条目；path 为 None 表示该平台不适用（不展示）。
fn push_agent_path(
    entries: &mut Vec<LocalAgentPathEntry>,
    kind: &str,
    label: &str,
    path: Option<&Path>,
) {
    let Some(path) = path else {
        return;
    };
    entries.push(LocalAgentPathEntry {
        kind: kind.to_string(),
        label: label.to_string(),
        exists: path.exists(),
        detail: path_detail(path),
        path: path_display(path),
    });
}

fn finish_agent(
    source: &str,
    name: &str,
    root: Option<&Path>,
    mut entries: Vec<LocalAgentPathEntry>,
) -> LocalAgentPaths {
    // 根目录未设置时，退而使用第一条路径所在目录作为展示根。
    let root_path = match root {
        Some(path) => path_display(path),
        None => entries
            .first()
            .map(|entry| {
                Path::new(&entry.path)
                    .parent()
                    .map(path_display)
                    .unwrap_or_else(|| entry.path.clone())
            })
            .unwrap_or_default(),
    };
    entries.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.path.cmp(&right.path))
    });
    let detected = entries.iter().any(|entry| entry.exists);
    LocalAgentPaths {
        source: source.to_string(),
        name: name.to_string(),
        root: root_path,
        detected,
        paths: entries,
        collected_sessions: 0,
        collected_events: 0,
    }
}

fn kiro_legacy_storage_roots(home: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    #[cfg(target_os = "macos")]
    {
        let global_storage = home
            .join("Library")
            .join("Application Support")
            .join("Kiro")
            .join("User")
            .join("globalStorage");
        roots.push(global_storage.join("kiro.kiroagent"));
        roots.push(global_storage.join("kiro.kiro-agent"));
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(app_data) = std::env::var_os("APPDATA") {
            let global_storage = PathBuf::from(app_data)
                .join("Kiro")
                .join("User")
                .join("globalStorage");
            roots.push(global_storage.join("kiro.kiroagent"));
            roots.push(global_storage.join("kiro.kiro-agent"));
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let global_storage = home
            .join(".config")
            .join("Kiro")
            .join("User")
            .join("globalStorage");
        roots.push(global_storage.join("kiro.kiroagent"));
        roots.push(global_storage.join("kiro.kiro-agent"));
    }
    roots
}

fn catpawai_data_roots(home: &Path) -> Vec<PathBuf> {
    let mut roots = vec![home.join(".sankuai").join("CatPawAI").join("sqliteDB")];
    #[cfg(target_os = "macos")]
    {
        roots.push(
            home.join("Library")
                .join("Application Support")
                .join("CatPawAI")
                .join("sqliteDB"),
        );
    }
    roots
}

/// 枚举 OpenHub 会读取的本地 AI Agent 及其配置 / 数据 / 数据库根目录。
/// 只读文件系统存在性，不读取任何内容，供「本地 Agent 路径诊断」弹窗展示。
fn collect_local_agent_paths(home: &Path) -> LocalAgentPathsReport {
    let mut agents = Vec::<LocalAgentPaths>::new();

    // Codex
    {
        let root = crate::token_collector::codex_home(home);
        let mut entries = Vec::new();
        push_agent_path(
            &mut entries,
            "config",
            "配置 config.toml",
            Some(&root.join("config.toml")),
        );
        push_agent_path(
            &mut entries,
            "config",
            "认证 auth.json",
            Some(&root.join("auth.json")),
        );
        push_agent_path(
            &mut entries,
            "data",
            "会话 sessions",
            Some(&root.join("sessions")),
        );
        push_agent_path(
            &mut entries,
            "data",
            "归档会话 archived_sessions",
            Some(&root.join("archived_sessions")),
        );
        agents.push(finish_agent("codex", "Codex", Some(&root), entries));
    }

    // Claude Code
    {
        let root = crate::token_collector::claude_config_dir(home);
        let mut entries = Vec::new();
        push_agent_path(
            &mut entries,
            "config",
            "项目设置 settings.json",
            Some(&root.join("settings.json")),
        );
        push_agent_path(
            &mut entries,
            "config",
            "全局配置 ~/.claude.json",
            Some(&home.join(".claude.json")),
        );
        push_agent_path(
            &mut entries,
            "data",
            "会话项目 projects",
            Some(&root.join("projects")),
        );
        agents.push(finish_agent("claude", "Claude Code", Some(&root), entries));
    }

    // Command Code
    {
        let root = home.join(".commandcode");
        let mut entries = Vec::new();
        push_agent_path(
            &mut entries,
            "data",
            "会话项目 projects",
            Some(&root.join("projects")),
        );
        agents.push(finish_agent(
            "command-code",
            "Command Code",
            Some(&root),
            entries,
        ));
    }

    // Antigravity (Gemini 客户端)
    {
        let root = home.join(".gemini");
        let mut entries = Vec::new();
        push_agent_path(
            &mut entries,
            "data",
            "转录 antigravity-cli",
            Some(&root.join("antigravity-cli")),
        );
        push_agent_path(
            &mut entries,
            "data",
            "转录 antigravity-ide",
            Some(&root.join("antigravity-ide")),
        );
        agents.push(finish_agent(
            "antigravity",
            "Antigravity (Gemini)",
            Some(&root),
            entries,
        ));
    }

    // Kiro
    {
        let root = home.join(".kiro");
        let mut entries = Vec::new();
        push_agent_path(
            &mut entries,
            "data",
            "会话 sessions (v2)",
            Some(&root.join("sessions")),
        );
        for (index, legacy) in kiro_legacy_storage_roots(home).iter().enumerate() {
            let label = if index == 0 {
                "旧版全局存储 (globalStorage)"
            } else {
                "旧版全局存储 (备用)"
            };
            push_agent_path(&mut entries, "data", label, Some(legacy));
        }
        agents.push(finish_agent("kiro", "Kiro", Some(&root), entries));
    }

    // DSH (DeepSeek)
    {
        let root = home.join(".dsh");
        let mut entries = Vec::new();
        push_agent_path(
            &mut entries,
            "data",
            "会话 sessions (.jsonl.zstd)",
            Some(&root.join("sessions")),
        );
        agents.push(finish_agent("dsh", "DSH (DeepSeek)", Some(&root), entries));
    }

    // OpenCode
    {
        let data_root = crate::token_collector::xdg_data_home(home).join("opencode");
        let mut entries = Vec::new();
        push_agent_path(
            &mut entries,
            "config",
            "配置目录",
            Some(&home.join(".config").join("opencode")),
        );
        push_agent_path(
            &mut entries,
            "database",
            "数据库 opencode.db",
            Some(&data_root.join("opencode.db")),
        );
        agents.push(finish_agent(
            "opencode",
            "OpenCode",
            Some(&data_root),
            entries,
        ));
    }

    // MiMo Code
    {
        let root = crate::token_collector::xdg_data_home(home).join("mimocode");
        let mut entries = Vec::new();
        push_agent_path(
            &mut entries,
            "database",
            "数据库 mimocode.db",
            Some(&root.join("mimocode.db")),
        );
        agents.push(finish_agent("mimo", "MiMo Code", Some(&root), entries));
    }

    // ZCode
    {
        let root = home.join(".zcode");
        let mut entries = Vec::new();
        push_agent_path(
            &mut entries,
            "database",
            "数据库 db.sqlite",
            Some(&crate::token_collector::zcode_db_path(home)),
        );
        agents.push(finish_agent("zcode", "ZCode", Some(&root), entries));
    }

    // CatPawAI
    {
        let roots = catpawai_data_roots(home);
        let primary = roots.first().cloned();
        let mut entries = Vec::new();
        for (index, data_root) in roots.iter().enumerate() {
            let label = if index == 0 {
                "数据库 globalCache.sqlite"
            } else {
                "数据库 globalCache.sqlite (备用)"
            };
            push_agent_path(
                &mut entries,
                "database",
                label,
                Some(&data_root.join("globalCache.sqlite")),
            );
        }
        agents.push(finish_agent(
            "catpawai",
            "CatPawAI",
            primary.as_deref(),
            entries,
        ));
    }

    // 路径存在 ≠ 采到了数据：附上最近一次采集的会话/事件量与缓存时间。
    let collected = crate::token_collector::collected_stats_by_source();
    let collected_at = collected
        .values()
        .next()
        .map(|stats| stats.updated_at.clone())
        .unwrap_or_default();
    for agent in &mut agents {
        if let Some(stats) = collected.get(&agent.source) {
            agent.collected_sessions = stats.sessions;
            agent.collected_events = stats.events;
        }
    }

    LocalAgentPathsReport {
        available: true,
        home: path_display(home),
        agents,
        env_overrides: collected_env_overrides(),
        collected_at: collected_at,
    }
}

/// 当前生效的路径重定向环境变量；未设置为空列表。
fn collected_env_overrides() -> Vec<LocalAgentEnvOverride> {
    [
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "XDG_DATA_HOME",
        "OPENHUB_CATPAWAI_DB_PATH",
    ]
    .iter()
    .filter_map(|key| {
        let value = std::env::var_os(key)?;
        (!value.is_empty()).then(|| LocalAgentEnvOverride {
            key: (*key).to_string(),
            value: path_display(&PathBuf::from(value)),
        })
    })
    .collect()
}

/// 只读扫描本地 AI Agent 的配置 / 数据路径（不读取日志内容）。
#[tauri::command]
pub async fn get_local_agent_paths() -> Result<LocalAgentPathsReport, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let home = std::env::var_os("HOME").ok_or("无法定位用户目录")?;
        Ok(collect_local_agent_paths(&PathBuf::from(home)))
    })
    .await
    .map_err(|error| format!("本地 Agent 路径扫描失败：{error}"))?
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
{"type":"user","uuid":"u1","isSidechain":false,"timestamp":"2026-08-01T00:00:01.000Z","message":{"role":"user","model":"deepseek-v4-flash","content":"first question"}}
{"type":"assistant","uuid":"a1","isSidechain":false,"timestamp":"2026-08-01T00:00:02.000Z","message":{"id":"msg-a1","role":"assistant","model":"deepseek-v4-flash","usage":{"input_tokens":100,"cache_read_input_tokens":50,"cache_creation_input_tokens":10,"output_tokens":40}}}
{"type":"user","uuid":"u2","isSidechain":false,"timestamp":"2026-08-01T00:00:03.000Z","message":{"role":"user","content":"second question"}}
{"type":"assistant","uuid":"a2","isSidechain":false,"timestamp":"2026-08-01T00:00:04.000Z","message":{"id":"msg-a2","role":"assistant","model":"deepseek-v4-flash","usage":{"input_tokens":200,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":60}}}
{"type":"user","uuid":"s1","isSidechain":true,"timestamp":"2026-08-01T00:00:05.000Z","message":{"role":"user"}}
{"type":"assistant","uuid":"sa1","isSidechain":true,"timestamp":"2026-08-01T00:00:06.000Z","message":{"id":"msg-sa1","role":"assistant","model":"deepseek-v4-pro","usage":{"input_tokens":30,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":10}}}
{"type":"assistant","uuid":"sa1","isSidechain":true,"timestamp":"2026-08-01T00:00:06.100Z","message":{"id":"msg-sa1","role":"assistant","model":"deepseek-v4-pro","usage":{"input_tokens":30,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":10}}}
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
        assert_eq!(sessions[0].message_count, 7);
        // 子代理任务 prompt（s1）不开新对话轮
        assert_eq!(sessions[0].conversation_count, 2);
        assert_eq!(sessions[0].model, "deepseek-v4-flash");
        // 200+50+10+40 + 200+0+0+60 + 30+0+0+10（子代理请求计入，重复行只计一次）
        assert_eq!(sessions[0].total_tokens, 500);
        assert_eq!(conversations.len(), 2);
        assert_eq!(conversations[0].request_count, 1);
        assert_eq!(conversations[0].total_tokens, 200);
        // 子代理请求归属最近一次真人对话（第二轮）
        assert_eq!(conversations[1].request_count, 2);
        assert_eq!(conversations[1].total_tokens, 300);
        assert_eq!(requests.len(), 3);
        assert!(requests.iter().all(|r| r.role == "assistant"));
        assert_eq!(requests[0].input_tokens, 100);
        assert_eq!(requests[0].cache_read_tokens, 50);
        assert_eq!(requests[0].total_tokens, 200);
        // 同一 message.id 的重复内容块行只计一次
        assert_eq!(requests.iter().filter(|r| r.id == "msg-sa1").count(), 1);
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
                request_count: 0,
                cost_usd: 0.0,
                pricing_available: false,
                estimated_tokens: 0,
                estimated_input_tokens: 0,
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
    // - 2026-08-06T04:59:44+08:00 / +0800 / +08
    // 带时区偏移的时间戳先归一到 UTC 再取小时，避免本地时间被当成 UTC 错位归桶。
    let cleaned = ts.trim();
    if cleaned.len() < 13 {
        return None;
    }
    let prefix = &cleaned[..13];
    if !(prefix.as_bytes().get(4) == Some(&b'-')
        && prefix.as_bytes().get(7) == Some(&b'-')
        && prefix.as_bytes().get(10) == Some(&b'T'))
    {
        return None;
    }
    let offset_secs = token_collector::tz_offset_secs(cleaned);
    if offset_secs == 0 {
        return Some(format!("{prefix}:00:00.000Z"));
    }
    let year: i64 = cleaned.get(0..4)?.parse().ok()?;
    let month: i64 = cleaned.get(5..7)?.parse().ok()?;
    let day: i64 = cleaned.get(8..10)?.parse().ok()?;
    let hour: i64 = cleaned.get(11..13)?.parse().ok()?;
    let days = token_collector::days_from_civil(year, month, day);
    let utc_secs = days * 86_400 + hour * 3_600 - offset_secs;
    let tod = utc_secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(utc_secs.div_euclid(86_400));
    Some(format!(
        "{y:04}-{m:02}-{d:02}T{:02}:00:00.000Z",
        tod / 3_600
    ))
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
    use_response_item_users: bool,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
) {
    let kind = value.get("type").and_then(JsonValue::as_str).unwrap_or("");
    let ts = value
        .get("timestamp")
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    let Some(hour) = hour_key_from_ts(ts) else {
        return;
    };

    // 新版 Codex 把用户消息放在 response_item(message, role=user)，不再发 event_msg(user_message)。
    if kind == "response_item" {
        if use_response_item_users {
            let payload = value.get("payload").unwrap_or(&JsonValue::Null);
            if payload.get("type").and_then(JsonValue::as_str) == Some("message")
                && payload.get("role").and_then(JsonValue::as_str) == Some("user")
                && token_collector::codex_user_message_is_human(payload)
            {
                record(map, sources, "codex", hour, 1, 0, 0, 0);
            }
        }
        return;
    }

    if kind != "event_msg" {
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

fn claude_on_line(
    value: &JsonValue,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
    last_user_hour: &mut Option<String>,
    counted_message_ids: &mut HashSet<String>,
) {
    // 子代理（Task）会话与主会话共用日志文件（或位于 subagents/ 目录）。
    // 子代理里的 user 输入是任务 prompt，不是真人对话轮；但其 assistant 响应
    // 仍是真实 API 请求，请求数应包含（对话数不包含）。
    let is_sidechain = value
        .get("isSidechain")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
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
        if is_sidechain {
            return;
        }
        let content = value
            .get("message")
            .and_then(|m| m.get("content"))
            .cloned()
            .unwrap_or(JsonValue::Null);
        if token_collector::claude_user_line_is_human(value, &content) {
            *last_user_hour = Some(hour.clone());
            record(map, sources, "claude", hour, 1, 0, 0, 0);
        }
        return;
    }

    if type_name != "assistant" {
        return;
    }
    // 同一请求（message.id）会按内容块拆成多行，usage 相同；按 id 去重防止请求重复计数。
    let message_id = value
        .get("message")
        .and_then(|m| m.get("id"))
        .and_then(JsonValue::as_str)
        .unwrap_or("")
        .to_string();
    let already_counted = !message_id.is_empty() && counted_message_ids.contains(&message_id);
    if already_counted {
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
    // 请求的时间锚定到本轮 user 请求，与 Token 采集器同口径。
    let req_hour = last_user_hour.clone().unwrap_or_else(|| hour.clone());
    if is_api_error {
        if !message_id.is_empty() {
            counted_message_ids.insert(message_id);
        }
        record(map, sources, "claude", req_hour, 0, 1, 0, 1);
    } else if usage_tokens > 0 {
        if !message_id.is_empty() {
            counted_message_ids.insert(message_id);
        }
        record(map, sources, "claude", req_hour, 0, 1, 1, 0);
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
        if token_collector::claude_user_is_human(content) {
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
            if token_collector::claude_user_is_human(content) {
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

/// DSH (DeepSeek AI CLI) 会话事件 → 请求健康。
/// user/message = 用户请求（1 对话）；assistant/message 带 data.usage = 一次真实 API 请求。
fn dsh_on_line(
    value: &JsonValue,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
    last_user_hour: &mut Option<String>,
) {
    let kind = value.get("type").and_then(JsonValue::as_str).unwrap_or("");
    let time_ms = value.get("time").and_then(JsonValue::as_i64).unwrap_or(0);
    let Some(hour) = hour_key_from_millis(time_ms) else {
        return;
    };
    match kind {
        "user/message" => {
            // 只算真实用户输入；runtime context / skill 目录等注入不算。
            if token_collector::dsh_user_is_human(value) {
                *last_user_hour = Some(hour.clone());
                record(map, sources, "dsh", hour, 1, 0, 0, 0);
            }
        }
        "assistant/message" => {
            let has_usage = value
                .get("data")
                .and_then(|data| data.get("usage"))
                .map(|usage| usage.is_object())
                .unwrap_or(false);
            if has_usage {
                // 请求的时间锚定到本轮 user 请求，与 Token 采集器同口径。
                let req_hour = last_user_hour.clone().unwrap_or_else(|| hour.clone());
                record(map, sources, "dsh", req_hour, 0, 1, 1, 0);
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

/// Codex rollout 文件需要按文件判断版本（旧版 event_msg(user_message)，新版 response_item）。
fn scan_codex_file_incremental(
    path: &Path,
    cursors: &mut FileCursorMap,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
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
        _ => 0,
    };

    let Ok(bytes) = fs::read(path) else {
        return;
    };
    let Ok(text) = String::from_utf8(bytes.clone()) else {
        return;
    };

    let has_user_message_events = text.lines().any(|line| {
        serde_json::from_str::<JsonValue>(line)
            .map(|v| {
                v.get("type").and_then(JsonValue::as_str) == Some("event_msg")
                    && v.get("payload")
                        .and_then(|p| p.get("type"))
                        .and_then(JsonValue::as_str)
                        == Some("user_message")
            })
            .unwrap_or(false)
    });
    let use_response_item_users = !has_user_message_events;

    let new_bytes: &[u8] = if start == 0 {
        bytes.as_slice()
    } else {
        &bytes[start as usize..]
    };
    let new_text = String::from_utf8_lossy(new_bytes);
    for line in new_text.lines() {
        if let Ok(value) = serde_json::from_str::<JsonValue>(line) {
            codex_on_line(&value, use_response_item_users, map, sources);
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

fn collect_codex_activity_incremental(
    dir: &Path,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
    cursors: &mut FileCursorMap,
) {
    let mut stack = vec![dir.to_path_buf()];
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
            let is_codex = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
                .unwrap_or(false);
            if !is_codex {
                continue;
            }
            scan_codex_file_incremental(&path, cursors, map, sources);
        }
    }
}

fn collect_claude_activity_incremental(
    dir: &Path,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
    cursors: &mut FileCursorMap,
) {
    let mut stack = vec![dir.to_path_buf()];
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
            let is_jsonl = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("jsonl"))
                .unwrap_or(false);
            if !is_jsonl {
                continue;
            }
            // 每个文件独立追踪最近一次用户输入的小时（供 assistant 请求锚定）
            // 与已计数的 message.id（同一请求按内容块拆多行，防重复计数）。
            let mut last_user_hour: Option<String> = None;
            let mut counted_message_ids: HashSet<String> = HashSet::new();
            scan_jsonl_file_incremental(&path, cursors, &mut |value| {
                claude_on_line(
                    value,
                    map,
                    sources,
                    &mut last_user_hour,
                    &mut counted_message_ids,
                );
            });
        }
    }
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

/// DSH 会话是 zstd 压缩的 JSONL（只会追加）。解压后按「已处理行数」做增量，
/// 末尾若为未写完的半行则留到下次扫描，避免漏行。
fn scan_zstd_jsonl_incremental(
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

    // 文件重写/截断（inode 变化或体积缩小）时从头读。
    let start_lines = match cursors.get(&key) {
        Some(prev) if prev.inode == inode && size >= prev.size => prev.offset,
        _ => 0,
    };

    let Ok(raw) = fs::read(path) else {
        return;
    };
    let Ok(bytes) = zstd::decode_all(raw.as_slice()) else {
        return;
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return;
    };

    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len() as u64;
    // 末尾半行（解析失败）留到下次；其余按游标增量处理。
    let mut end = total;
    if total > 0 && serde_json::from_str::<JsonValue>(lines.last().unwrap()).is_err() {
        end = total - 1;
    }
    let end = end.max(start_lines).min(total);
    for (index, line) in lines.iter().enumerate() {
        let index = index as u64;
        if index >= end {
            break;
        }
        if index < start_lines {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<JsonValue>(line) {
            on_line(&value);
        }
    }

    cursors.insert(
        key,
        FileCursor {
            inode,
            size,
            mtime_ms,
            offset: end,
        },
    );
}

fn collect_dsh_activity_incremental(
    root: &Path,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
    cursors: &mut FileCursorMap,
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
            let is_dsh = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.ends_with(".jsonl.zstd"))
                .unwrap_or(false);
            if !is_dsh {
                continue;
            }
            // 每个文件独立追踪最近一次用户输入的小时，供 assistant 请求锚定。
            let mut last_user_hour: Option<String> = None;
            scan_zstd_jsonl_incremental(&path, cursors, &mut |value| {
                dsh_on_line(value, map, sources, &mut last_user_hour);
            });
        }
    }
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
fn sqlite_message_provider(value: &JsonValue) -> String {
    value
        .get("providerID")
        .or_else(|| value.get("providerId"))
        .and_then(JsonValue::as_str)
        .or_else(|| {
            value
                .get("model")
                .and_then(|model| model.get("providerID").or_else(|| model.get("providerId")))
                .and_then(JsonValue::as_str)
        })
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

        let mut newly_allowed: Vec<String> = Vec::new();
        for (sid, _, value) in &new_rows {
            if value.get("role").and_then(JsonValue::as_str) != Some("assistant") {
                continue;
            }
            let provider = sqlite_message_provider(value);
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
            let provider = sqlite_message_provider(value);
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

// v7：Claude 请求按 message.id 去重（同一请求多内容块行只计一次）、子代理请求
// 开始计入、对话轮改用 origin.kind 判定、新增 CatPawAI 来源、时区偏移归一 UTC。
// 计数口径变更需要全量重扫历史文件，因此提升缓存版本使旧游标失效。
const ACTIVITY_CACHE_VERSION: u32 = 7;

/// 请求活动结果缓存：自维护 per-source 增量游标，并覆盖 Codex 归档与 Command Code。
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

fn clear_request_health_cache() -> Result<(), String> {
    if let Ok(mut cache) = activity_cache().lock() {
        *cache = None;
    }
    if let Some(path) = activity_cache_path() {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "清除 Token 请求健康缓存失败（{}）：{error}",
                    path.display()
                ));
            }
        }
        let tmp = path.with_extension("json.tmp");
        match fs::remove_file(&tmp) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "清除 Token 请求健康临时缓存失败（{}）：{error}",
                    tmp.display()
                ));
            }
        }
    }
    Ok(())
}

/// CatPawAI 请求健康增量扫描：user_prompt → 对话；带 usage 的消息 → 请求。
/// 与 read_catpawai_buckets_from_path 同口径，保证 KPI 对话数覆盖 CatPawAI。
fn collect_catpawai_activity_incremental(
    map: &mut BTreeMap<String, HealthAgg>,
    sources_map: &mut BTreeMap<String, HealthAgg>,
    cursor: &mut SqliteCursor,
) {
    let Some(path) = catpawai_db_path() else {
        return;
    };
    let Some(conn) = open_readonly_sqlite(&path) else {
        return;
    };
    if !sqlite_table_exists(&conn, "t_ui_messages") {
        return;
    }
    let since = cursor.max_time_created;
    let Ok(mut stmt) = conn.prepare(
        "SELECT message_type, create_time, content FROM t_ui_messages \
         WHERE create_time > ?1 ORDER BY create_time ASC, id ASC",
    ) else {
        return;
    };
    let rows = stmt.query_map([since], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    });
    let Ok(rows) = rows else {
        return;
    };
    let mut max_time = since;
    for row in rows.flatten() {
        let (message_type, create_time, content) = row;
        if create_time > max_time {
            max_time = create_time;
        }
        let normalized_ms = if create_time > 0 && create_time < 100_000_000_000 {
            create_time.saturating_mul(1000)
        } else {
            create_time
        };
        let Some(hour) = hour_key_from_millis(normalized_ms) else {
            continue;
        };
        if message_type == "user_prompt" {
            record(map, sources_map, CATPAWAI_SOURCE, hour, 1, 0, 0, 0);
            continue;
        }
        let Ok(value) = serde_json::from_str::<JsonValue>(&content) else {
            continue;
        };
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
        let total = catpawai_number(usage, &["total_tokens", "totalTokens"])
            .max(prompt.saturating_add(completion));
        if total > 0 {
            record(map, sources_map, CATPAWAI_SOURCE, hour, 0, 1, 1, 0);
        }
    }
    cursor.max_time_created = max_time;
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

    let codex_base = crate::token_collector::codex_home(&home);
    for (cursor_key, codex_root) in [
        ("codex", codex_base.join("sessions")),
        ("codex-archived", codex_base.join("archived_sessions")),
    ] {
        if codex_root.is_dir() {
            let cursors = envelope
                .file_cursors
                .entry(cursor_key.to_string())
                .or_default();
            collect_codex_activity_incremental(&codex_root, &mut map, &mut sources, cursors);
        }
    }
    let claude_root = crate::token_collector::claude_config_dir(&home).join("projects");
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

    let opencode_db = crate::token_collector::opencode_db_path(&home);
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

    let mimo_db = crate::token_collector::mimo_db_path(&home);
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

    let zcode_db = crate::token_collector::zcode_db_path(&home);
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

    let kiro_root = token_collector::kiro_v2_session_root(&home);
    if kiro_root.is_dir() {
        let cursors = envelope.file_cursors.entry("kiro".to_string()).or_default();
        collect_kiro_activity_incremental(&kiro_root, &mut map, &mut sources, cursors);
    }
    // Kiro 0.x 的 v1 JSON 会话没有独立请求 ID；Token 采集器仍会估算其
    // 用量与对话数，请求健康只读取新版 JSONL，避免把旧会话猜成成功请求。

    let dsh_root = home.join(".dsh").join("sessions");
    if dsh_root.is_dir() {
        let cursors = envelope.file_cursors.entry("dsh".to_string()).or_default();
        collect_dsh_activity_incremental(&dsh_root, &mut map, &mut sources, cursors);
    }

    // CatPawAI：与 Token 用量桶同口径（user_prompt=对话，带 usage=请求），
    // 使 KPI「对话数」的 health 口径覆盖该来源，与工具表对齐。
    {
        let cursor = envelope
            .sqlite_cursors
            .entry("catpawai".to_string())
            .or_default();
        collect_catpawai_activity_incremental(&mut map, &mut sources, cursor);
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
    progress_app: Option<&AppHandle>,
) -> Result<TokenCollectorSyncReport, String> {
    let _guard = token_collection_lock()
        .lock()
        .map_err(|_| "Token 数据采集锁异常".to_string())?;
    let started = Instant::now();
    if force {
        // “刷新”执行完整重建：删除 OpenHub 自己的文件/内存/SQLite 快照缓存，
        // 再从来源工具的原始本地日志重新拉取并计算；原始日志不会被删除。
        emit_token_collector_progress(
            progress_app,
            "cache",
            "running",
            "正在清除 OpenHub 本地 Token 缓存与数据库快照",
        );
        token_collector::clear_local_cache()?;
        clear_request_health_cache()?;
        db::clear_token_snapshots(database)?;
        emit_token_collector_progress(
            progress_app,
            "cache",
            "success",
            "本地缓存已清除，来源工具的原始日志保持不变",
        );
    }
    emit_token_collector_progress(
        progress_app,
        "scan",
        "running",
        "正在扫描 Codex、Claude 等工具的本地日志",
    );
    let snapshot = token_collector::collect_snapshot(force)?;
    emit_token_collector_progress(
        progress_app,
        "scan",
        "success",
        format!(
            "日志扫描完成：重扫 {} 个文件，复用 {} 个文件",
            snapshot.scanned_files, snapshot.reused_files
        ),
    );
    // 即使主采集器文件指纹未变化，也要合并 CatPawAI 与请求健康的独立增量源。
    // 写入的是三份聚合快照而非 20MB 文件游标缓存，事务替换成本可控。
    emit_token_collector_progress(
        progress_app,
        "aggregate",
        "running",
        "正在合并 Token 用量、会话与请求健康数据",
    );
    let usage = merge_catpawai_usage(snapshot.usage.clone())?;
    let health = collect_request_health_snapshot(force)?;
    emit_token_collector_progress(
        progress_app,
        "aggregate",
        "success",
        format!("数据汇总完成：{} 个会话", snapshot.sessions.len()),
    );
    emit_token_collector_progress(
        progress_app,
        "database",
        "running",
        "正在写入 OpenHub 本地数据库",
    );
    db::write_token_snapshots(database, &usage, &snapshot.sessions, &health)?;
    emit_token_collector_progress(progress_app, "database", "success", "数据库快照写入完成");
    let mut report = token_collector::sync_report(&snapshot, started.elapsed().as_millis() as i64);
    if force {
        report.changed = true;
        report.skipped = false;
        report.message = format!(
            "已清除本地 Token 缓存并重新拉取计算：重扫 {} 个文件",
            snapshot.scanned_files
        );
    }
    Ok(report)
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

/// 读取本地完整时间戳（macOS/Linux 用 /bin/date；失败时退回 UTC 近似，保证日志不中断）。
fn local_timestamp() -> String {
    if let Ok(output) = std::process::Command::new("/bin/date")
        .arg("+%Y-%m-%d %H:%M:%S")
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            let value = text.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    // fallback: UTC
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let tod = secs % 86_400;
    format!(
        "1970-01-01 {:02}:{:02}:{:02} UTC+{days}d",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

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
                collect_token_data(&database, false, None)
            })
            .await;
            match result {
                Ok(Ok(report)) => {
                    if report.changed {
                        eprintln!(
                            "[OpenHub] {} Token 后台采集完成：{}",
                            local_timestamp(),
                            report.message
                        );
                    }
                }
                Ok(Err(error)) => eprintln!(
                    "[OpenHub] {} Token 后台采集失败：{error}",
                    local_timestamp()
                ),
                Err(error) => eprintln!(
                    "[OpenHub] {} Token 后台任务异常：{error}",
                    local_timestamp()
                ),
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
        // +08:00 的本地时间 04:59 实际是 UTC 前一天 20:59
        assert_eq!(
            hour_key_from_ts("2026-08-06T04:59:44+08:00").as_deref(),
            Some("2026-08-05T20:00:00.000Z")
        );
        assert_eq!(
            hour_key_from_ts("2026-08-06T04:59:44+0800").as_deref(),
            Some("2026-08-05T20:00:00.000Z")
        );
        assert_eq!(
            hour_key_from_ts("2026-08-06T23:59:44-05:00").as_deref(),
            Some("2026-08-07T04:00:00.000Z")
        );
    }

    #[test]
    fn claude_user_is_human_filters_non_user_input() {
        let tool_only = json!([{"type": "tool_result", "content": "ok"}]);
        assert!(!token_collector::claude_user_is_human(&tool_only));

        let text = json!([{"type": "text", "text": "hello"}]);
        assert!(token_collector::claude_user_is_human(&text));

        let plain = json!("hello");
        assert!(token_collector::claude_user_is_human(&plain));

        // Esc 中断不算对话轮
        let interrupted =
            json!([{"type": "text", "text": "[Request interrupted by user for tool use]"}]);
        assert!(!token_collector::claude_user_is_human(&interrupted));

        // 斜杠命令输出回显不算；命令本身算
        let stdout =
            json!([{"type": "text", "text": "<local-command-stdout>ok</local-command-stdout>"}]);
        assert!(!token_collector::claude_user_is_human(&stdout));
        let cmd = json!("<command-name>/compact</command-name>");
        assert!(token_collector::claude_user_is_human(&cmd));
    }

    #[test]
    fn assistant_tokens_positive_reads_nested() {
        let value = json!({"tokens": {"input": 10, "output": 0, "reasoning": 0}});
        assert!(assistant_tokens_positive(&value));
        let empty = json!({"tokens": {"input": 0, "output": 0}});
        assert!(!assistant_tokens_positive(&empty));
    }

    #[test]
    fn claude_on_line_skips_sidechain_transcripts() {
        let mut map = BTreeMap::new();
        let mut sources = BTreeMap::new();
        let mut last_user_hour = None;
        let mut counted = HashSet::new();
        // 子代理消息标记 isSidechain，不应计为主对话
        claude_on_line(
            &json!({
                "type": "user",
                "timestamp": "2026-08-06T04:00:00.000Z",
                "isSidechain": true,
                "message": {"role": "user", "content": "subagent prompt"}
            }),
            &mut map,
            &mut sources,
            &mut last_user_hour,
            &mut counted,
        );
        assert_eq!(map.values().map(|a| a.dialogues).sum::<i64>(), 0);
        // 正常用户输入仍计 1 轮
        claude_on_line(
            &json!({
                "type": "user",
                "timestamp": "2026-08-06T04:00:00.000Z",
                "message": {"role": "user", "content": "hello"}
            }),
            &mut map,
            &mut sources,
            &mut last_user_hour,
            &mut counted,
        );
        assert_eq!(map.values().map(|a| a.dialogues).sum::<i64>(), 1);
    }

    #[test]
    fn claude_sidechain_requests_counted_but_not_dialogues() {
        let mut map = BTreeMap::new();
        let mut sources = BTreeMap::new();
        let mut last_user_hour = None;
        let mut counted = HashSet::new();
        // 真人输入开 1 轮
        claude_on_line(
            &json!({
                "type": "user",
                "timestamp": "2026-08-06T04:00:00.000Z",
                "message": {"role": "user", "content": "hello"}
            }),
            &mut map,
            &mut sources,
            &mut last_user_hour,
            &mut counted,
        );
        // 子代理的 assistant 响应 = 真实 API 请求，计入请求数
        claude_on_line(
            &json!({
                "type": "assistant",
                "timestamp": "2026-08-06T04:05:00.000Z",
                "isSidechain": true,
                "message": {"id": "chatcmpl-side-1", "usage": {"input_tokens": 100, "output_tokens": 50}}
            }),
            &mut map,
            &mut sources,
            &mut last_user_hour,
            &mut counted,
        );
        let hour = map.get("2026-08-06T04:00:00.000Z").expect("bucket exists");
        assert_eq!(hour.dialogues, 1);
        assert_eq!(hour.requests, 1);
    }

    #[test]
    fn claude_dedupes_same_message_id_lines() {
        let mut map = BTreeMap::new();
        let mut sources = BTreeMap::new();
        let mut last_user_hour = None;
        let mut counted = HashSet::new();
        let line = json!({
            "type": "assistant",
            "timestamp": "2026-08-06T04:00:00.000Z",
            "message": {"id": "chatcmpl-dup", "usage": {"input_tokens": 100, "output_tokens": 50}}
        });
        claude_on_line(
            &line,
            &mut map,
            &mut sources,
            &mut last_user_hour,
            &mut counted,
        );
        // 同一 message.id 的第二个内容块行（usage 相同）不应重复计数
        claude_on_line(
            &line,
            &mut map,
            &mut sources,
            &mut last_user_hour,
            &mut counted,
        );
        let hour = map.get("2026-08-06T04:00:00.000Z").expect("bucket exists");
        assert_eq!(hour.requests, 1);
    }

    #[test]
    fn claude_origin_kind_filters_non_human() {
        let mut map = BTreeMap::new();
        let mut sources = BTreeMap::new();
        let mut last_user_hour = None;
        let mut counted = HashSet::new();
        // origin.kind = task-notification 的 user 行不算对话轮
        claude_on_line(
            &json!({
                "type": "user",
                "timestamp": "2026-08-06T04:00:00.000Z",
                "origin": {"kind": "task-notification"},
                "message": {"role": "user", "content": "background task done"}
            }),
            &mut map,
            &mut sources,
            &mut last_user_hour,
            &mut counted,
        );
        assert_eq!(map.values().map(|a| a.dialogues).sum::<i64>(), 0);
        // origin.kind = human 计 1 轮
        claude_on_line(
            &json!({
                "type": "user",
                "timestamp": "2026-08-06T04:00:00.000Z",
                "origin": {"kind": "human"},
                "message": {"role": "user", "content": "hello"}
            }),
            &mut map,
            &mut sources,
            &mut last_user_hour,
            &mut counted,
        );
        assert_eq!(map.values().map(|a| a.dialogues).sum::<i64>(), 1);
    }

    #[test]
    fn claude_request_anchors_to_last_user_hour() {
        let mut map = BTreeMap::new();
        let mut sources = BTreeMap::new();
        let mut last_user_hour = None;
        let mut counted = HashSet::new();
        // 用户在 04:xx 输入，assistant 在 05:xx 才返回 → 请求应记到 04:00
        claude_on_line(
            &json!({
                "type": "user",
                "timestamp": "2026-08-06T04:10:00.000Z",
                "message": {"role": "user", "content": "hello"}
            }),
            &mut map,
            &mut sources,
            &mut last_user_hour,
            &mut counted,
        );
        claude_on_line(
            &json!({
                "type": "assistant",
                "timestamp": "2026-08-06T05:20:00.000Z",
                "message": {"usage": {"input_tokens": 10, "output_tokens": 20}}
            }),
            &mut map,
            &mut sources,
            &mut last_user_hour,
            &mut counted,
        );
        let hour04 = map
            .get("2026-08-06T04:00:00.000Z")
            .expect("request anchored to 04:00");
        assert_eq!(hour04.dialogues, 1);
        assert_eq!(hour04.requests, 1);
        assert!(!map.contains_key("2026-08-06T05:00:00.000Z"));
    }

    #[test]
    fn dsh_activity_counts_user_and_assistant_messages() {
        let mut map = BTreeMap::new();
        let mut sources = BTreeMap::new();
        let mut last_user_hour = None;

        // 用户消息 = 1 对话
        dsh_on_line(
            &json!({"type": "user/message", "time": 1786687444127u64}),
            &mut map,
            &mut sources,
            &mut last_user_hour,
        );
        // 带 source.kind 的真实用户输入 = 1 对话
        dsh_on_line(
            &json!({"type": "user/message", "time": 1786687445000u64,
                    "data": {"source": {"kind": "user"}}}),
            &mut map,
            &mut sources,
            &mut last_user_hour,
        );
        // 注入消息（runtime context / skill 目录 / 插件通知）不算对话
        dsh_on_line(
            &json!({"type": "user/message", "time": 1786687446000u64,
                    "data": {"source": {"kind": "plugin"}, "content": [{"type": "text", "text": "background job bash-1 finished"}]}}),
            &mut map,
            &mut sources,
            &mut last_user_hour,
        );
        dsh_on_line(
            &json!({"type": "user/message", "time": 1786687447000u64,
                    "data": {"content": [{"type": "text", "text": "<system-reminder>…"}]}}),
            &mut map,
            &mut sources,
            &mut last_user_hour,
        );
        // 带 usage 的 assistant = 1 请求 + 1 成功
        dsh_on_line(
            &json!({
                "type": "assistant/message",
                "time": 1786687449831u64,
                "data": {"usage": {"inputTokens": 8740, "outputTokens": 228}}
            }),
            &mut map,
            &mut sources,
            &mut last_user_hour,
        );
        // 无 usage 的 assistant 不计请求
        dsh_on_line(
            &json!({"type": "assistant/message", "time": 1786687449831u64, "data": {}}),
            &mut map,
            &mut sources,
            &mut last_user_hour,
        );
        // 无关事件忽略
        dsh_on_line(
            &json!({"type": "tool/call", "time": 1786687449831u64}),
            &mut map,
            &mut sources,
            &mut last_user_hour,
        );

        assert_eq!(map.values().map(|a| a.dialogues).sum::<i64>(), 2);
        assert_eq!(map.values().map(|a| a.requests).sum::<i64>(), 1);
        assert_eq!(map.values().map(|a| a.success).sum::<i64>(), 1);
        let dsh = sources.get("dsh").expect("dsh source should exist");
        assert_eq!(dsh.dialogues, 2);
        assert_eq!(dsh.requests, 1);
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
