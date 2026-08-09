use crate::models::{
    RawConversation, RawLogReport, RawRequest, RawSession, RequestHealthBucket,
    RequestHealthReport, RequestHealthSourceSummary, TokenStatsReport, TokenTrackerSyncReport,
    TokenUsageBucket, TokenUsageReport,
};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// 探测 tokentracker CLI 可执行文件，顺序：
/// 1) OPENHUB_TOKENTRACKER_PATH 环境变量显式指定
/// 2) 常见安装路径
/// 3) PATH 中的 tokentracker
fn find_tokentracker_binary() -> Option<PathBuf> {
    if let Ok(value) = std::env::var("OPENHUB_TOKENTRACKER_PATH") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
    }
    for path in [
        "/usr/local/bin/tokentracker",
        "/opt/homebrew/bin/tokentracker",
        "/usr/bin/tokentracker",
    ] {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|path| path.join("tokentracker"))
            .find(|path| path.is_file())
    })
}

fn run_tokentracker_sessions(
    from: Option<String>,
    to: Option<String>,
    refresh: bool,
) -> Result<TokenStatsReport, String> {
    let binary = find_tokentracker_binary().ok_or_else(|| {
        "未找到 tokentracker CLI。请先安装：npm i -g tokentracker-cli（或设置 OPENHUB_TOKENTRACKER_PATH 指向可执行文件）"
            .to_string()
    })?;

    let mut command = Command::new(binary);
    command
        .arg("sessions")
        .arg("--format")
        .arg("json")
        // 跳过 git 结果分析，只读取本地会话日志，速度更快。
        .arg("--no-git");
    if let Some(from) = from.filter(|value| !value.trim().is_empty()) {
        command.arg("--from").arg(from.trim());
    }
    if let Some(to) = to.filter(|value| !value.trim().is_empty()) {
        command.arg("--to").arg(to.trim());
    }
    if refresh {
        command.arg("--refresh");
    }

    let output = command
        .output()
        .map_err(|error| format!("执行 tokentracker 失败：{error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("tokentracker sessions 执行失败：{}", output.status)
        } else {
            format!("tokentracker sessions 执行失败：{detail}")
        });
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("tokentracker 输出编码异常：{error}"))?;
    serde_json::from_str(&stdout).map_err(|error| format!("tokentracker 输出解析失败：{error}"))
}

/// 读取 tokentracker CLI 的 token 统计报告。
/// - from / to：YYYY-MM-DD 日期范围（可空，空则统计全部）
/// - refresh：为 true 时强制重读本地会话日志（否则用 tokentracker 缓存，速度更快）
#[tauri::command]
pub async fn get_token_stats(
    from: Option<String>,
    to: Option<String>,
    refresh: Option<bool>,
) -> Result<TokenStatsReport, String> {
    // CLI 解析本地日志在 spawn_blocking 中执行，避免阻塞 UI 线程。
    tauri::async_runtime::spawn_blocking(move || {
        run_tokentracker_sessions(from, to, refresh.unwrap_or(false))
    })
    .await
    .map_err(|error| format!("Token 统计任务执行失败：{error}"))?
}

/// Tokentracker 本地增量同步的进程内协调器。
/// OpenHub 只触发本地解析，不调用上传/发布流程；同步锁仍由 tokentracker 自己负责。
struct TokenTrackerSyncCache {
    report: TokenTrackerSyncReport,
    finished_at: Instant,
}

fn tokentracker_sync_cache() -> &'static Mutex<Option<TokenTrackerSyncCache>> {
    static CACHE: OnceLock<Mutex<Option<TokenTrackerSyncCache>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

fn tokentracker_sync_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

const TOKENTRACKER_SYNC_TTL: Duration = Duration::from_secs(8);

fn cursors_file_state() -> (bool, u64, u64, String) {
    let Some(path) = cursors_json_path() else {
        return (false, 0, 0, String::new());
    };
    let Ok(metadata) = fs::metadata(&path) else {
        return (false, 0, 0, String::new());
    };
    let size = metadata.len();
    let modified_ms = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0);
    let updated_at = fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<JsonValue>(&text).ok())
        .and_then(|value| {
            value
                .get("updatedAt")
                .and_then(JsonValue::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default();
    (true, size, modified_ms, updated_at)
}

fn run_tokentracker_local_sync() -> Result<TokenTrackerSyncReport, String> {
    let binary = find_tokentracker_binary().ok_or_else(|| {
        "未找到 tokentracker CLI，无法执行本地增量同步。请先安装 tokentracker-cli，或设置 OPENHUB_TOKENTRACKER_PATH".to_string()
    })?;
    let before = cursors_file_state();
    let started = Instant::now();

    // 当前 tokentracker 版本中，--auto --background 会跳过云端上传；
    // --all-local-sources 让已安装的本地工具都参与增量检查。
    let output = Command::new(&binary)
        .arg("sync")
        .arg("--auto")
        .arg("--background")
        .arg("--all-local-sources")
        .env("CI", "1")
        .output()
        .map_err(|error| format!("启动 tokentracker 本地同步失败：{error}"))?;

    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("tokentracker 本地同步失败：{}", output.status)
        } else {
            format!("tokentracker 本地同步失败：{detail}")
        });
    }

    let after = cursors_file_state();
    let changed = before != after;
    let updated_at = if !after.3.is_empty() {
        after.3.clone()
    } else {
        chrono_like_now_iso()
    };
    Ok(TokenTrackerSyncReport {
        available: true,
        changed,
        skipped: !changed,
        elapsed_ms: started.elapsed().as_millis() as i64,
        updated_at,
        message: if changed {
            "Tokentracker 已完成本地增量同步".to_string()
        } else {
            "本地数据没有变化，已复用 Tokentracker 增量游标".to_string()
        },
    })
}

fn chrono_like_now_iso() -> String {
    // 不新增 chrono 依赖；这里仅作为无 cursors.updatedAt 时的状态时间。
    String::new()
}

/// 触发 tokentracker 本地增量同步。
#[tauri::command]
pub async fn sync_token_tracker(force: Option<bool>) -> Result<TokenTrackerSyncReport, String> {
    let force = force.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || {
        if !force {
            if let Ok(guard) = tokentracker_sync_cache().lock() {
                if let Some(cache) = guard.as_ref() {
                    if cache.finished_at.elapsed() < TOKENTRACKER_SYNC_TTL {
                        return Ok(TokenTrackerSyncReport {
                            ..cache.report.clone()
                        });
                    }
                }
            }
        }

        let _guard = tokentracker_sync_lock()
            .lock()
            .map_err(|_| "Tokentracker 同步锁异常".to_string())?;
        let report = run_tokentracker_local_sync()?;
        if let Ok(mut cache) = tokentracker_sync_cache().lock() {
            *cache = Some(TokenTrackerSyncCache {
                report: report.clone(),
                finished_at: Instant::now(),
            });
        }
        Ok(report)
    })
    .await
    .map_err(|error| format!("Tokentracker 同步任务执行失败：{error}"))?
}

/// Tokentracker 本地读模型：queue.jsonl。
/// Tokentracker 自己的界面读取 queue.jsonl，并按 source/model/hour_start 保留最新累计行；
/// OpenHub 必须复用同一口径，不能只读 hourly.buckets，否则会漏掉历史修正后的 Codex 数据。
fn queue_json_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).map(|home| {
        home.join(".tokentracker")
            .join("tracker")
            .join("queue.jsonl")
    })
}

/// tokentracker 的用量原始存储：~/.tokentracker/tracker/cursors.json
/// 作为 queue.jsonl 不可用时的兼容 fallback。
fn cursors_json_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).map(|home| {
        home.join(".tokentracker")
            .join("tracker")
            .join("cursors.json")
    })
}

fn queue_number(value: &JsonValue, key: &str) -> i64 {
    value
        .get(key)
        .and_then(|value| value.as_f64().or_else(|| value.as_i64().map(|n| n as f64)))
        .map(|value| value as i64)
        .unwrap_or(0)
}

/// 与 tokentracker local-api.normalizeQueueRow 对齐的历史兼容修正：
/// 旧版 Codex queue 行把 cached input 包含在 input_tokens 中，
/// 需要减掉 cached_input_tokens 才能得到纯输入 Token。
fn normalize_queue_row(mut row: JsonValue) -> JsonValue {
    let source = row
        .get("source")
        .and_then(JsonValue::as_str)
        .unwrap_or("")
        .to_string();
    let input = queue_number(&row, "input_tokens");
    let cached = queue_number(&row, "cached_input_tokens");
    let output = queue_number(&row, "output_tokens");
    let total = queue_number(&row, "total_tokens");
    let is_legacy_codex = source.eq_ignore_ascii_case("codex")
        && cached > 0
        && input >= cached
        && total == input + output;
    if is_legacy_codex {
        if let Some(object) = row.as_object_mut() {
            object.insert("input_tokens".to_string(), JsonValue::from(input - cached));
        }
    }
    // queue rows use billable_total_tokens for the dashboard headline. Cursor
    // sources from older versions may have written zero even though total_tokens
    // is present; normalize them exactly as tokentracker does.
    let total_for_billing = queue_number(&row, "total_tokens");
    if source.eq_ignore_ascii_case("cursor")
        && queue_number(&row, "billable_total_tokens") < total_for_billing
    {
        if let Some(object) = row.as_object_mut() {
            object.insert(
                "billable_total_tokens".to_string(),
                JsonValue::from(total_for_billing),
            );
        }
    }
    row
}

/// 读取 Tokentracker queue.jsonl 的最新累计行。
/// queue 是 append-only，每次同步会重新写入被触碰的累计桶，不能直接相加。
fn read_queue_buckets() -> Result<Vec<TokenUsageBucket>, String> {
    let path = queue_json_path().ok_or("无法定位用户目录")?;
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("无法读取 tokentracker 队列（{}）：{error}", path.display()))?;
    let mut latest = BTreeMap::<String, JsonValue>::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<JsonValue>(line) else {
            // 与 Tokentracker dashboard 一致：单条损坏/半写入行不影响其他数据。
            continue;
        };
        let source = row.get("source").and_then(JsonValue::as_str).unwrap_or("");
        let model = row.get("model").and_then(JsonValue::as_str).unwrap_or("");
        let hour = row
            .get("hour_start")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        if source.is_empty() || model.is_empty() || hour.is_empty() {
            continue;
        }
        let key = format!("{source}|{model}|{hour}");
        latest.insert(key, normalize_queue_row(row));
    }

    let buckets = latest
        .into_values()
        .map(|row| TokenUsageBucket {
            source: row
                .get("source")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_string(),
            model: row
                .get("model")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_string(),
            timestamp: row
                .get("hour_start")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_string(),
            total_tokens: queue_number(&row, "total_tokens"),
            billable_total_tokens: queue_number(&row, "billable_total_tokens"),
            input_tokens: queue_number(&row, "input_tokens"),
            cached_input_tokens: queue_number(&row, "cached_input_tokens"),
            cache_creation_input_tokens: queue_number(&row, "cache_creation_input_tokens"),
            output_tokens: queue_number(&row, "output_tokens"),
            reasoning_output_tokens: queue_number(&row, "reasoning_output_tokens"),
            conversation_count: queue_number(&row, "conversation_count"),
        })
        .collect::<Vec<_>>();
    Ok(buckets)
}

fn read_cursors_buckets() -> Result<Vec<TokenUsageBucket>, String> {
    let path = cursors_json_path().ok_or("无法定位用户目录")?;
    let text = fs::read_to_string(&path).map_err(|error| {
        format!(
            "无法读取 tokentracker 用量数据（{}）：{error}",
            path.display()
        )
    })?;
    let value: JsonValue = serde_json::from_str(&text)
        .map_err(|error| format!("tokentracker 用量数据解析失败：{error}"))?;
    let buckets = value
        .get("hourly")
        .and_then(|hourly| hourly.get("buckets"))
        .and_then(JsonValue::as_object)
        .ok_or("tokentracker 用量数据结构异常（缺少 hourly.buckets）")?;

    let number = |field: &JsonValue, key: &str| -> i64 {
        field
            .get(key)
            .and_then(JsonValue::as_f64)
            .map(|value| value as i64)
            .unwrap_or(0)
    };
    let mut out = Vec::with_capacity(buckets.len());
    for (key, value) in buckets {
        // key 形如 "source|model|ISO时间戳"
        let parts = key.split('|').collect::<Vec<_>>();
        if parts.len() < 3 {
            continue;
        }
        let totals = value.get("totals").cloned().unwrap_or(JsonValue::Null);
        out.push(TokenUsageBucket {
            source: parts[0].to_string(),
            model: parts[1].to_string(),
            timestamp: parts[2].to_string(),
            total_tokens: number(&totals, "total_tokens"),
            billable_total_tokens: number(&totals, "billable_total_tokens"),
            input_tokens: number(&totals, "input_tokens"),
            cached_input_tokens: number(&totals, "cached_input_tokens"),
            cache_creation_input_tokens: number(&totals, "cache_creation_input_tokens"),
            output_tokens: number(&totals, "output_tokens"),
            reasoning_output_tokens: number(&totals, "reasoning_output_tokens"),
            conversation_count: number(&totals, "conversation_count"),
        });
    }
    Ok(out)
}

/// 读取 tokentracker 全部工具的用量桶。
/// 主路径复用 queue.jsonl（与 Tokentracker 自己的界面一致），
/// cursors.json 仅作为兼容 fallback。
#[tauri::command]
pub async fn get_token_usage() -> Result<TokenUsageReport, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let buckets = match read_queue_buckets() {
            Ok(buckets) if !buckets.is_empty() => buckets,
            _ => read_cursors_buckets()?,
        };
        let mut start_date = String::new();
        let mut end_date = String::new();
        for bucket in &buckets {
            let day = bucket.timestamp.get(..10).unwrap_or("");
            if day.is_empty() {
                continue;
            }
            if start_date.is_empty() || day < start_date.as_str() {
                start_date = day.to_string();
            }
            if end_date.is_empty() || day > end_date.as_str() {
                end_date = day.to_string();
            }
        }
        Ok(TokenUsageReport {
            available: !buckets.is_empty(),
            buckets,
            start_date,
            end_date,
        })
    })
    .await
    .map_err(|error| format!("Token 用量读取失败：{error}"))?
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
    }

    #[test]
    fn parses_a_real_tokentracker_sessions_payload() {
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

/// 请求活动结果缓存 v2：自维护 per-source 增量游标。
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

const ACTIVITY_CACHE_TTL: Duration = Duration::from_secs(5 * 60);

fn activity_cache_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).map(|home| {
        home.join(".tokentracker")
            .join("tracker")
            .join("openhub-activity-cache.json")
    })
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
    if envelope.version != 2 {
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

/// 读取多工具对话/请求健康数据。
/// - refresh=false：自维护增量游标，只扫描新增行/新消息（快）
/// - refresh=true：清空游标全量重建（慢但兜底修复）
#[tauri::command]
pub async fn get_token_request_health(
    refresh: Option<bool>,
) -> Result<RequestHealthReport, String> {
    let force = refresh.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || {
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
        if force {
            // 全量重建：清空报告与游标，由增量路径从头扫描并重填游标
            envelope.report = RequestHealthReport::default();
            envelope.file_cursors.clear();
            envelope.sqlite_cursors.clear();
        }
        let (mut map, mut sources) = report_to_maps(&envelope.report);

        let codex_root = home.join(".codex").join("sessions");
        if codex_root.is_dir() {
            let cursors = envelope
                .file_cursors
                .entry("codex".to_string())
                .or_default();
            collect_codex_activity_incremental(&codex_root, &mut map, &mut sources, cursors);
        }
        let claude_root = home.join(".claude").join("projects");
        if claude_root.is_dir() {
            let cursors = envelope
                .file_cursors
                .entry("claude".to_string())
                .or_default();
            collect_claude_activity_incremental(&claude_root, &mut map, &mut sources, cursors);
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

        // 注意：kilo/goose/craft/workbuddy/copilot 暂不进活动时间线（无可靠事件时间）

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
    })
    .await
    .map_err(|error| format!("请求健康读取失败：{error}"))?
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
