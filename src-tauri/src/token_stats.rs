use crate::models::{
    RawConversation, RawLogReport, RawRequest, RawSession, RequestHealthBucket, RequestHealthReport,
    TokenStatsReport, TokenUsageBucket, TokenUsageReport,
};
use std::collections::BTreeMap;
use serde_json::Value as JsonValue;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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


/// 从 Codex rollout 提取大模型请求健康：task_started（成功请求）与 task_complete.error（失败）。
/// 按 ISO 小时聚合，供「请求健康时间线」展示失败比例。
fn collect_codex_request_health(dir: &Path, map: &mut BTreeMap<String, (i64, i64)>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_codex_request_health(&path, map);
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
            .unwrap_or(false)
        {
            let Ok(text) = fs::read_to_string(&path) else { continue };
            for line in text.lines() {
                let Ok(value) = serde_json::from_str::<JsonValue>(line) else { continue };
                if value.get("type").and_then(JsonValue::as_str) != Some("event_msg") {
                    continue;
                }
                let Some(payload) = value.get("payload") else { continue };
                let Some(p_type) = payload.get("type").and_then(JsonValue::as_str) else {
                    continue;
                };
                let ts = value
                    .get("timestamp")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("");
                if ts.len() < 13 {
                    continue;
                }
                // 失败：task_complete 带 error 对象
                let is_failure = p_type == "task_complete"
                    && payload.get("error").map(|e| e.is_object()).unwrap_or(false);
                let is_success = p_type == "task_started";
                if !is_failure && !is_success {
                    continue;
                }
                let hour = format!("{}:00:00.000Z", &ts[..13]); // 2026-08-03T03 → 2026-08-03T03:00:00.000Z
                let entry = map.entry(hour).or_insert((0, 0));
                if is_failure {
                    entry.1 += 1;
                } else {
                    entry.0 += 1;
                }
            }
        }
    }
}

/// 读取大模型请求健康数据（Codex rollout：task_started / task_complete.error），
/// 供「请求健康时间线」展示每时段请求量与失败比例。
#[tauri::command]
pub async fn get_token_request_health() -> Result<RequestHealthReport, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let home = std::env::var_os("HOME").ok_or("无法定位用户目录")?;
        let mut map = BTreeMap::new();
        let root = PathBuf::from(&home).join(".codex").join("sessions");
        if root.is_dir() {
            collect_codex_request_health(&root, &mut map);
        }
        let buckets = map
            .into_iter()
            .map(|(hour, (success, failed))| RequestHealthBucket {
                hour,
                success,
                failed,
            })
            .collect::<Vec<_>>();
        Ok(RequestHealthReport {
            available: !buckets.is_empty(),
            buckets,
        })
    })
    .await
    .map_err(|error| format!("请求健康读取失败：{error}"))?
}

