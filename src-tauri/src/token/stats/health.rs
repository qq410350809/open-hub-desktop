use crate::models::RequestHealthReport;
use crate::token::stats::catpawai::collect_catpawai_activity_incremental;
use crate::token::stats::types::*;
use serde_json::Value as JsonValue;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

pub fn codex_on_line(
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

    if kind == "response_item" {
        if use_response_item_users {
            let payload = value.get("payload").unwrap_or(&JsonValue::Null);
            if payload.get("type").and_then(JsonValue::as_str) == Some("message")
                && payload.get("role").and_then(JsonValue::as_str) == Some("user")
                && crate::token::collector::codex_user_message_is_human(payload)
            {
                record(map, sources, "codex", hour, 1, 0, 0, 0);
            }
        }
        return;
    }

    if kind != "event_msg" {
        return;
    }
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
        "token_count" => record(map, sources, "codex", hour, 0, 1, 0, 0),
        "task_complete" => {
            if let Some(err) = payload.get("error").filter(|e| !e.is_null()) {
                if !is_user_cancelled_error(err) {
                    record(map, sources, "codex", hour, 0, 0, 0, 1);
                }
            }
        }
        _ => {}
    }
}

pub fn claude_on_line(
    value: &JsonValue,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
    last_user_hour: &mut Option<String>,
    counted_message_ids: &mut HashSet<String>,
) {
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
        if crate::token::collector::claude_user_line_is_human(value, &content) {
            *last_user_hour = Some(hour.clone());
            record(map, sources, "claude", hour, 1, 0, 0, 0);
        }
        return;
    }

    if type_name != "assistant" {
        return;
    }
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

pub fn command_code_on_line(
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
        if crate::token::collector::claude_user_is_human(content) {
            record(map, sources, "command-code", hour, 1, 0, 0, 0);
        }
    } else {
        record(map, sources, "command-code", hour, 0, 1, 1, 0);
    }
}

pub fn antigravity_on_line(
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

pub fn kiro_on_line(
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
            if crate::token::collector::claude_user_is_human(content) {
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

pub fn dsh_on_line(
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
            if crate::token::collector::dsh_user_is_human(value) {
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
                let req_hour = last_user_hour.clone().unwrap_or_else(|| hour.clone());
                record(map, sources, "dsh", req_hour, 0, 1, 1, 0);
            }
        }
        _ => {}
    }
}

pub fn copilot_on_line(
    value: &JsonValue,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
) {
    let v = value.get("v").unwrap_or(value);
    if let Some(requests) = v.get("requests").and_then(JsonValue::as_array) {
        for req in requests {
            let mut ts = req
                .get("modelState")
                .and_then(|ms| ms.get("completedAt"))
                .and_then(JsonValue::as_i64)
                .map(crate::token::collector::iso_from_millis)
                .unwrap_or_default();
            if ts.is_empty() {
                if let Some(ms) = v.get("creationDate").and_then(JsonValue::as_i64) {
                    ts = crate::token::collector::iso_from_millis(ms);
                }
            }
            if let Some(hour) = hour_key_from_ts(&ts) {
                let is_error = req
                    .get("result")
                    .and_then(|r| r.get("errorDetails"))
                    .is_some();
                let user_text = req
                    .get("message")
                    .and_then(|m| m.get("text"))
                    .and_then(JsonValue::as_str)
                    .unwrap_or("");
                let dialogues = if !user_text.trim().is_empty() { 1 } else { 0 };
                if is_error {
                    record(map, sources, "copilot", hour, dialogues, 1, 0, 1);
                } else {
                    record(map, sources, "copilot", hour, dialogues, 1, 1, 0);
                }
            }
        }
        return;
    }

    let type_name = value.get("type").and_then(JsonValue::as_str).unwrap_or("");
    let ts = value
        .get("timestamp")
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    let Some(hour) = hour_key_from_ts(ts) else {
        return;
    };
    match type_name {
        "user.message" => {
            record(map, sources, "copilot", hour, 1, 0, 0, 0);
        }
        "assistant.message" => {
            record(map, sources, "copilot", hour, 0, 1, 1, 0);
        }
        _ => {}
    }
}

pub fn sqlite_message_provider(value: &JsonValue) -> String {
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

pub fn scan_jsonl_file_incremental(
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
        _ => 0,
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

pub fn collect_jsonl_incremental(
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

pub fn scan_codex_file_incremental(
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

pub fn collect_codex_activity_incremental(
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

pub fn collect_claude_activity_incremental(
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

pub fn is_command_code_activity_file(path: &Path) -> bool {
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

pub fn collect_command_code_activity_incremental(
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

pub fn collect_antigravity_activity_incremental(
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

pub fn collect_kiro_activity_incremental(
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

pub fn collect_copilot_activity_incremental(
    home: &Path,
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
    cursors: &mut FileCursorMap,
) {
    let files = crate::token::collector::collect_copilot_source_files(home);
    for (_, path) in files {
        let key = path.to_string_lossy().to_string();
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        let inode = metadata_ino(&meta);
        let size = meta.len();
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_millis() as u64)
            .unwrap_or(0);

        if let Some(cursor) = cursors.get(&key) {
            if cursor.inode == inode && cursor.size == size && cursor.mtime_ms == mtime_ms {
                continue;
            }
        }

        let Ok(file) = fs::File::open(&path) else {
            continue;
        };
        let reader = std::io::BufReader::new(file);
        for line in reader.lines().flatten() {
            if let Ok(value) = serde_json::from_str::<JsonValue>(&line) {
                copilot_on_line(&value, map, sources);
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
}

pub fn scan_zstd_jsonl_incremental(
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

pub fn collect_dsh_activity_incremental(
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
            let mut last_user_hour: Option<String> = None;
            scan_zstd_jsonl_incremental(&path, cursors, &mut |value| {
                dsh_on_line(value, map, sources, &mut last_user_hour);
            });
        }
    }
}

pub fn collect_sqlite_message_activity_incremental(
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
        for sid in newly_allowed {
            if let Some(users) = cursor.session_users.remove(&sid) {
                for (_, hour) in users {
                    record(map, sources_map, source, hour, 1, 0, 0, 0);
                }
            }
        }
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

pub fn collect_request_health_snapshot(force: bool) -> Result<RequestHealthReport, String> {
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

    let codex_base = crate::token::collector::codex_home(&home);
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
    let claude_root = crate::token::collector::claude_config_dir(&home).join("projects");
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

    let opencode_db = crate::token::collector::opencode_db_path(&home);
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

    let mimo_db = crate::token::collector::mimo_db_path(&home);
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

    let zcode_db = crate::token::collector::zcode_db_path(&home);
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

    let kiro_root = crate::token::collector::kiro_v2_session_root(&home);
    if kiro_root.is_dir() {
        let cursors = envelope.file_cursors.entry("kiro".to_string()).or_default();
        collect_kiro_activity_incremental(&kiro_root, &mut map, &mut sources, cursors);
    }

    let dsh_root = home.join(".dsh").join("sessions");
    if dsh_root.is_dir() {
        let cursors = envelope.file_cursors.entry("dsh".to_string()).or_default();
        collect_dsh_activity_incremental(&dsh_root, &mut map, &mut sources, cursors);
    }

    {
        let cursor = envelope
            .sqlite_cursors
            .entry("catpawai".to_string())
            .or_default();
        collect_catpawai_activity_incremental(&mut map, &mut sources, cursor);
    }

    {
        let cursors = envelope
            .file_cursors
            .entry("copilot".to_string())
            .or_default();
        collect_copilot_activity_incremental(&home, &mut map, &mut sources, cursors);
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
