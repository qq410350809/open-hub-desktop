use crate::models::{
    RawConversation, RawLogReport, RawRequest, RawSession, RequestHealthBucket, RequestHealthReport,
    RequestHealthSourceSummary, TokenStatsReport, TokenUsageBucket, TokenUsageReport,
};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value as JsonValue;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::thread;
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

    let stdout =
        String::from_utf8(output.stdout).map_err(|error| format!("tokentracker 输出编码异常：{error}"))?;
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

/// tokentracker 的用量原始存储：~/.tokentracker/tracker/cursors.json
/// 仪表盘汇总数据来自其 hourly.buckets（覆盖所有工具的每小时用量）。
fn cursors_json_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).map(|home| {
        home.join(".tokentracker").join("tracker").join("cursors.json")
    })
}

fn read_cursors_buckets() -> Result<Vec<TokenUsageBucket>, String> {
    let path = cursors_json_path().ok_or("无法定位用户目录")?;
    let text = fs::read_to_string(&path).map_err(|error| {
        format!(
            "无法读取 tokentracker 用量数据（{}）：{error}",
            path.display()
        )
    })?;
    let value: JsonValue =
        serde_json::from_str(&text).map_err(|error| format!("tokentracker 用量数据解析失败：{error}"))?;
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

/// 读取 tokentracker 全部工具的小时用量桶（cursors.json），
/// 供 Token 统计页做汇总/趋势/热力图/每日细目使用。
#[tauri::command]
pub async fn get_token_usage() -> Result<TokenUsageReport, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let buckets = read_cursors_buckets()?;
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
    let Ok(text) = fs::read_to_string(path) else { return };
    let session_id = path
        .file_stem()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let number = |field: &JsonValue, key: &str| -> i64 {
        field.get(key).and_then(JsonValue::as_f64).map(|value| value as i64).unwrap_or(0)
    };
    let mut model = String::new();
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut message_count = 0i64;
    let mut conv_index = 0i64;
    let mut session_tokens = 0i64;
    let mut current: Option<(RawConversation, String)> = None;

    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<JsonValue>(line) else { continue };
        if value.get("isSidechain").and_then(JsonValue::as_bool).unwrap_or(false) {
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
        let uuid = value.get("uuid").and_then(JsonValue::as_str).unwrap_or("").to_string();
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
        let usage = value.get("message").and_then(|message| message.get("usage"));
        let Some(usage) = usage.filter(|u| u.is_object()) else { continue };
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
    let Ok(text) = fs::read_to_string(path) else { return };
    let session_id = path
        .file_stem()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut message_count = 0i64;

    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<JsonValue>(line) else { continue };
        if value.get("type").and_then(JsonValue::as_str) != Some("response_item") {
            continue;
        }
        let payload = value.get("payload");
        if payload.and_then(|p| p.get("type")).and_then(JsonValue::as_str) != Some("message") {
            continue;
        }
        let role = payload.and_then(|p| p.get("role")).and_then(JsonValue::as_str).unwrap_or("");
        if role != "user" && role != "assistant" {
            continue;
        }
        let ts = value.get("timestamp").and_then(JsonValue::as_str).unwrap_or("").to_string();
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
    let Ok(entries) = fs::read_dir(dir) else { return };
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
        parse_claude_file(&path, "OpenHub", &mut sessions, &mut conversations, &mut requests);

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
                total_tokens: totals.get("total_tokens").and_then(JsonValue::as_f64).unwrap() as i64,
                billable_total_tokens: totals.get("billable_total_tokens").and_then(JsonValue::as_f64).unwrap() as i64,
                input_tokens: totals.get("input_tokens").and_then(JsonValue::as_f64).unwrap() as i64,
                cached_input_tokens: totals.get("cached_input_tokens").and_then(JsonValue::as_f64).unwrap() as i64,
                cache_creation_input_tokens: totals.get("cache_creation_input_tokens").and_then(JsonValue::as_f64).unwrap() as i64,
                output_tokens: totals.get("output_tokens").and_then(JsonValue::as_f64).unwrap() as i64,
                reasoning_output_tokens: totals.get("reasoning_output_tokens").and_then(JsonValue::as_f64).unwrap() as i64,
                conversation_count: totals.get("conversation_count").and_then(JsonValue::as_f64).unwrap() as i64,
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
    // 手动格式化为 UTC ISO 小时，避免额外 chrono 依赖
    // 使用简易算法：基于 unix 秒
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

fn walk_jsonl_files(dir: &Path, on_file: &mut dyn FnMut(&Path)) {
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
            if is_jsonl {
                on_file(&path);
            }
        }
    }
}

/// Codex:
/// - 对话: event_msg.user_message
/// - 请求: event_msg.token_count
/// - 成功/失败样本: task_complete 无/有 error
fn collect_codex_activity(
    dir: &Path,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_codex_activity(&path, map, sources);
            continue;
        }
        let is_rollout = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
            .unwrap_or(false);
        if !is_rollout {
            continue;
        }
        let Ok(file) = fs::File::open(&path) else {
            continue;
        };
        for line in BufReader::new(file).lines().flatten() {
            let Ok(value) = serde_json::from_str::<JsonValue>(&line) else {
                continue;
            };
            if value.get("type").and_then(JsonValue::as_str) != Some("event_msg") {
                continue;
            }
            // 兼容 payload / msg 两种结构
            let payload = value
                .get("payload")
                .or_else(|| value.get("msg"))
                .cloned()
                .unwrap_or(JsonValue::Null);
            let Some(p_type) = payload.get("type").and_then(JsonValue::as_str) else {
                continue;
            };
            let ts = value
                .get("timestamp")
                .and_then(JsonValue::as_str)
                .unwrap_or("");
            let Some(hour) = hour_key_from_ts(ts) else {
                continue;
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

/// Claude:
/// - 对话: type=user 且含 text/image；排除仅 tool_result
/// - 请求: type=assistant 且 usage>0；API error 计 request+failed
fn collect_claude_activity(
    dir: &Path,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
) {
    walk_jsonl_files(dir, &mut |path| {
        let Ok(file) = fs::File::open(path) else {
            return;
        };
        for line in BufReader::new(file).lines().flatten() {
            let Ok(value) = serde_json::from_str::<JsonValue>(&line) else {
                continue;
            };
            let Some(type_name) = value.get("type").and_then(JsonValue::as_str) else {
                continue;
            };
            let ts = value
                .get("timestamp")
                .and_then(JsonValue::as_str)
                .unwrap_or("");
            let Some(hour) = hour_key_from_ts(ts) else {
                continue;
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
                continue;
            }

            if type_name != "assistant" {
                continue;
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
    });
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

fn collect_sqlite_message_activity(
    db_path: &Path,
    source: &str,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
    // None = 不过滤 provider；Some(set) = 仅这些 provider 的 assistant 计请求，
    // 且对话只统计“至少有一次匹配 provider assistant”的 session 内 user。
    provider_allow: Option<&HashSet<&str>>,
) {
    let Some(conn) = open_readonly_sqlite(db_path) else {
        return;
    };
    let Ok(mut stmt) = conn.prepare("SELECT session_id, time_created, data FROM message") else {
        return;
    };
    let rows = stmt.query_map([], |row| {
        let sid: String = row.get(0)?;
        let time_created: i64 = row.get(1)?;
        let data: String = row.get(2)?;
        Ok((sid, time_created, data))
    });
    let Ok(rows) = rows else {
        return;
    };

    // 单次扫描：provider 过滤场景下先缓存 user，等确认 session 合法后再回放
    let mut pending_users: Vec<(String, String)> = Vec::new(); // (session_id, hour)
    let mut allowed_sessions = HashSet::<String>::new();
    let filter = provider_allow.is_some();

    for row in rows.flatten() {
        let (session_id, time_created, data) = row;
        let Ok(value) = serde_json::from_str::<JsonValue>(&data) else {
            continue;
        };
        let role = value
            .get("role")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        let hour = value
            .get("time")
            .and_then(|t| {
                t.get("completed")
                    .or_else(|| t.get("created"))
                    .and_then(JsonValue::as_i64)
            })
            .and_then(hour_key_from_millis)
            .or_else(|| hour_key_from_millis(time_created));
        let Some(hour) = hour else {
            continue;
        };

        if role == "user" {
            if filter {
                pending_users.push((session_id, hour));
            } else {
                record(map, sources, source, hour, 1, 0, 0, 0);
            }
            continue;
        }

        if role != "assistant" {
            continue;
        }

        if let Some(allow) = provider_allow {
            let provider = value
                .get("providerID")
                .or_else(|| value.get("providerId"))
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            if !allow.contains(provider.as_str()) {
                continue;
            }
        }

        let err = value.get("error").filter(|e| !e.is_null());
        if let Some(err) = err {
            if filter {
                allowed_sessions.insert(session_id.clone());
            }
            if is_user_cancelled_error(err) {
                // 用户取消：计请求，不算失败
                record(map, sources, source, hour, 0, 1, 0, 0);
            } else {
                // 真实失败：请求 + 失败（success 由前端用 requests-failed 推导展示）
                record(map, sources, source, hour, 0, 1, 0, 1);
            }
            continue;
        }
        if assistant_tokens_positive(&value) {
            if filter {
                allowed_sessions.insert(session_id.clone());
            }
            // 有 token 的 assistant = 成功请求
            record(map, sources, source, hour, 0, 1, 1, 0);
        }
    }

    if filter {
        for (session_id, hour) in pending_users {
            if allowed_sessions.contains(&session_id) {
                record(map, sources, source, hour, 1, 0, 0, 0);
            }
        }
    }
}

/// Antigravity transcript.jsonl
fn collect_antigravity_activity(
    root: &Path,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
) {
    walk_jsonl_files(root, &mut |path| {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if name != "transcript.jsonl" {
            return;
        }
        let Ok(file) = fs::File::open(path) else {
            return;
        };
        for line in BufReader::new(file).lines().flatten() {
            let Ok(value) = serde_json::from_str::<JsonValue>(&line) else {
                continue;
            };
            let type_name = value.get("type").and_then(JsonValue::as_str).unwrap_or("");
            let source_name = value.get("source").and_then(JsonValue::as_str).unwrap_or("");
            let ts = value
                .get("created_at")
                .or_else(|| value.get("timestamp"))
                .and_then(JsonValue::as_str)
                .unwrap_or("");
            let Some(hour) = hour_key_from_ts(ts) else {
                continue;
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
    });
}

/// 进程内缓存：避免每次进入 Token 页都全量扫 Codex/Claude/Mimo 等日志。
struct ActivityCache {
    report: RequestHealthReport,
    fetched_at: Instant,
}

fn activity_cache() -> &'static Mutex<Option<ActivityCache>> {
    static CACHE: OnceLock<Mutex<Option<ActivityCache>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

const ACTIVITY_CACHE_TTL: Duration = Duration::from_secs(5 * 60);

fn merge_activity(
    into_map: &mut BTreeMap<String, HealthAgg>,
    into_sources: &mut BTreeMap<String, HealthAgg>,
    from_map: BTreeMap<String, HealthAgg>,
    from_sources: BTreeMap<String, HealthAgg>,
) {
    for (hour, agg) in from_map {
        let entry = into_map.entry(hour).or_default();
        entry.dialogues += agg.dialogues;
        entry.requests += agg.requests;
        entry.success += agg.success;
        entry.failed += agg.failed;
    }
    for (source, agg) in from_sources {
        let entry = into_sources.entry(source).or_default();
        entry.dialogues += agg.dialogues;
        entry.requests += agg.requests;
        entry.success += agg.success;
        entry.failed += agg.failed;
    }
}

fn build_activity_report() -> RequestHealthReport {
    let home = match std::env::var_os("HOME") {
        Some(h) => PathBuf::from(h),
        None => {
            return RequestHealthReport {
                available: false,
                buckets: vec![],
                by_source: vec![],
            };
        }
    };

    // 各工具并行采集，缩短首次进入等待
    let codex_root = home.join(".codex").join("sessions");
    let claude_root = home.join(".claude").join("projects");
    let opencode_db = home.join(".local").join("share").join("opencode").join("opencode.db");
    let mimo_db = home.join(".local").join("share").join("mimocode").join("mimocode.db");
    let zcode_db = home.join(".zcode").join("cli").join("db").join("db.sqlite");
    let gemini_root = home.join(".gemini");

    let handles = [
        thread::spawn(move || {
            let mut map = BTreeMap::new();
            let mut sources = BTreeMap::new();
            if codex_root.is_dir() {
                collect_codex_activity(&codex_root, &mut map, &mut sources);
            }
            (map, sources)
        }),
        thread::spawn(move || {
            let mut map = BTreeMap::new();
            let mut sources = BTreeMap::new();
            if claude_root.is_dir() {
                collect_claude_activity(&claude_root, &mut map, &mut sources);
            }
            (map, sources)
        }),
        thread::spawn(move || {
            let mut map = BTreeMap::new();
            let mut sources = BTreeMap::new();
            if opencode_db.is_file() {
                collect_sqlite_message_activity(&opencode_db, "opencode", &mut map, &mut sources, None);
            }
            (map, sources)
        }),
        thread::spawn(move || {
            let mut map = BTreeMap::new();
            let mut sources = BTreeMap::new();
            if mimo_db.is_file() {
                let allow = HashSet::from(["mimo", "xiaomi"]);
                collect_sqlite_message_activity(&mimo_db, "mimo", &mut map, &mut sources, Some(&allow));
            }
            (map, sources)
        }),
        thread::spawn(move || {
            let mut map = BTreeMap::new();
            let mut sources = BTreeMap::new();
            if zcode_db.is_file() {
                collect_sqlite_message_activity(&zcode_db, "zcode", &mut map, &mut sources, None);
            }
            (map, sources)
        }),
        thread::spawn(move || {
            let mut map = BTreeMap::new();
            let mut sources = BTreeMap::new();
            if gemini_root.is_dir() {
                collect_antigravity_activity(&gemini_root, &mut map, &mut sources);
            }
            (map, sources)
        }),
    ];

    let mut map = BTreeMap::new();
    let mut sources = BTreeMap::new();
    for handle in handles {
        if let Ok((part_map, part_sources)) = handle.join() {
            merge_activity(&mut map, &mut sources, part_map, part_sources);
        }
    }

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
/// - refresh=true 强制重扫；否则 5 分钟进程内缓存，避免首次后反复等待。
#[tauri::command]
pub async fn get_token_request_health(refresh: Option<bool>) -> Result<RequestHealthReport, String> {
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

        let report = build_activity_report();
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
}
