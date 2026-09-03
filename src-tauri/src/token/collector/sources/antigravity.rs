use crate::models::TokenSessionTokens;
use crate::token::collector::normalizer::{
    basename_or_fallback, extract_antigravity_project_from_transcript,
    normalize_workspace_project_key,
};
use crate::token::collector::sources::commandcode::estimate_local_content_tokens;
use crate::token::collector::time_utils::update_bounds;
use crate::token::collector::types::{
    database_fingerprint, fingerprint, open_readonly_sqlite, token_session, CachedFile,
    FileFingerprint, UsageEvent, LOCAL_ESTIMATED_CONTEXT_LIMIT, UNKNOWN_ANTIGRAVITY_MODEL,
};
use serde_json::{json, Value as JsonValue};
use std::fs;
use std::path::{Path, PathBuf};

pub fn antigravity_session_root(path: &Path) -> Option<PathBuf> {
    path.parent()?.parent()?.parent().map(Path::to_path_buf)
}

pub fn antigravity_session_id(path: &Path) -> String {
    antigravity_session_root(path)
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_string()
}

pub fn antigravity_data_root(path: &Path) -> Option<PathBuf> {
    antigravity_session_root(path)?
        .parent()?
        .parent()
        .map(Path::to_path_buf)
}

pub fn antigravity_database_path(path: &Path) -> Option<PathBuf> {
    let session_id = antigravity_session_id(path);
    if session_id.is_empty() {
        return None;
    }
    Some(
        antigravity_data_root(path)?
            .join("conversations")
            .join(format!("{session_id}.db")),
    )
}

pub fn antigravity_fingerprint(path: &Path) -> FileFingerprint {
    let transcript = fingerprint(path);
    let database = antigravity_database_path(path)
        .map(|database| database_fingerprint(&database))
        .unwrap_or_default();
    FileFingerprint {
        size: transcript
            .size
            .saturating_add(database.database.size)
            .saturating_add(database.wal.size),
        modified_ms: transcript
            .modified_ms
            .max(database.database.modified_ms)
            .max(database.wal.modified_ms),
    }
}

fn is_model_noise(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "gemini-pro-agent"
        || lower == "gemini-pro-default"
        || lower == "claude-login"
        || lower == "claude-code-gui"
        || lower.starts_with("gpt-migration-")
        || lower.starts_with("gpt-update-")
        || lower.starts_with("claude-unknown-model")
        || lower.starts_with("antigravity-unknown-model")
}

pub fn normalize_model_slug(raw: &str) -> String {
    let mut s = raw.trim();
    if let Some(pos) = s.rfind('(') {
        if s.ends_with(')') {
            s = s[..pos].trim();
        }
    }
    let mut s = s.to_ascii_lowercase();
    for suffix in [
        "-high",
        "-medium",
        "-low",
        "-thinking",
        "_high",
        "_medium",
        "_low",
        "_thinking",
    ] {
        if s.ends_with(suffix) {
            s.truncate(s.len() - suffix.len());
            break;
        }
    }
    let mut result = String::with_capacity(s.len());
    let mut last_dash = false;
    for c in s.chars() {
        if c == ' ' || c == '_' || c == '-' {
            if !last_dash && !result.is_empty() {
                result.push('-');
                last_dash = true;
            }
        } else if c.is_ascii_alphanumeric() || c == '.' {
            result.push(c);
            last_dash = false;
        }
    }
    result.trim_end_matches('-').to_string()
}

fn find_display_model_name(bytes: &[u8]) -> Option<String> {
    const DISPLAY_PREFIXES: [&[u8]; 5] = [b"Gemini ", b"Claude ", b"GPT-", b"DeepSeek-", b"Qwen"];
    for index in 0..bytes.len() {
        let Some(prefix) = DISPLAY_PREFIXES
            .iter()
            .find(|prefix| bytes[index..].starts_with(prefix))
        else {
            continue;
        };
        let mut end = index + prefix.len();
        let mut paren_depth = 0;
        while end < bytes.len() {
            let b = bytes[end];
            if b == b'(' {
                paren_depth += 1;
                end += 1;
            } else if b == b')' {
                if paren_depth > 0 {
                    paren_depth -= 1;
                    end += 1;
                    if paren_depth == 0 {
                        break;
                    }
                } else {
                    break;
                }
            } else if b.is_ascii_alphanumeric() || matches!(b, b' ' | b'.' | b'-' | b'_') {
                end += 1;
            } else {
                break;
            }
        }
        let candidate = String::from_utf8_lossy(&bytes[index..end])
            .trim()
            .to_string();
        if candidate.len() >= 4 && !candidate.starts_with("Gemini 0") && !is_model_noise(&candidate)
        {
            let lower = candidate.to_ascii_lowercase();
            if lower.contains("flash")
                || lower.contains("pro")
                || lower.contains("sonnet")
                || lower.contains("opus")
                || lower.contains("haiku")
                || lower.contains("ultra")
                || lower.contains("gpt-")
                || lower.contains("deepseek-")
                || lower.contains("qwen")
                || lower.chars().any(|c| c.is_ascii_digit())
            {
                return Some(candidate);
            }
        }
    }
    None
}

fn find_slug_model_name(bytes: &[u8]) -> Option<String> {
    const SLUG_PREFIXES: [&[u8]; 5] = [b"gemini-", b"claude-", b"gpt-", b"deepseek-", b"qwen-"];
    for index in 0..bytes.len() {
        let Some(prefix) = SLUG_PREFIXES
            .iter()
            .find(|prefix| bytes[index..].starts_with(prefix))
        else {
            continue;
        };
        let mut end = index + prefix.len();
        while end < bytes.len()
            && (bytes[end].is_ascii_alphanumeric() || matches!(bytes[end], b'-' | b'_' | b'.'))
        {
            end += 1;
        }
        let candidate = String::from_utf8_lossy(&bytes[index..end])
            .trim_end_matches('.')
            .to_string();
        if candidate.len() > prefix.len() && !is_model_noise(&candidate) {
            return Some(candidate);
        }
    }
    None
}

pub(crate) fn find_ascii_model_token(bytes: &[u8]) -> String {
    let chunks: &[&[u8]] = if bytes.len() > 8192 {
        &[&bytes[bytes.len() - 8192..], bytes]
    } else {
        &[bytes]
    };

    for chunk in chunks {
        if let Some(model) = find_slug_model_name(chunk) {
            let normalized = normalize_model_slug(&model);
            if !normalized.is_empty() {
                return normalized;
            }
        }
    }

    for chunk in chunks {
        if let Some(model) = find_display_model_name(chunk) {
            let normalized = normalize_model_slug(&model);
            if !normalized.is_empty() {
                return normalized;
            }
        }
    }

    String::new()
}

fn antigravity_database_metadata(path: &Path) -> (String, String) {
    let Some(database_path) = antigravity_database_path(path) else {
        return (String::new(), String::new());
    };
    let Some(conn) = open_readonly_sqlite(&database_path) else {
        return (String::new(), String::new());
    };

    let mut model = String::new();
    if let Ok(mut stmt) = conn.prepare("SELECT data FROM gen_metadata ORDER BY idx DESC LIMIT 10") {
        if let Ok(rows) = stmt.query_map([], |row| row.get::<_, Vec<u8>>(0)) {
            for row in rows.flatten() {
                let candidate = find_ascii_model_token(&row);
                if !candidate.is_empty() {
                    model = candidate;
                    break;
                }
            }
        }
    }

    let project = conn
        .query_row(
            "SELECT data FROM trajectory_metadata_blob ORDER BY id ASC LIMIT 1",
            [],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .ok()
        .and_then(|data| {
            let marker = b"file:///";
            let start = data
                .windows(marker.len())
                .position(|value| value == marker)?;
            let mut end = start;
            while end < data.len() && data[end].is_ascii_graphic() {
                end += 1;
            }
            let encoded = String::from_utf8_lossy(&data[start..end]);
            let decoded =
                percent_encoding::percent_decode_str(encoded.trim_start_matches("file://"))
                    .decode_utf8_lossy()
                    .to_string();
            let mut candidate = decoded.trim().to_string();
            while !candidate.is_empty() && !Path::new(&candidate).exists() {
                candidate.pop();
            }
            (!candidate.is_empty()).then_some(candidate)
        })
        .map(|path| basename_or_fallback(&path, "Antigravity"))
        .unwrap_or_default();

    (model, project)
}

fn antigravity_fallback_project(path: &Path) -> String {
    antigravity_data_root(path)
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map(|name| {
            if name.ends_with("-cli") {
                "Antigravity CLI"
            } else if name.ends_with("-ide") {
                "Antigravity IDE"
            } else {
                "Antigravity"
            }
        })
        .unwrap_or("Antigravity")
        .to_string()
}

fn extract_model_from_transcript_content(content: &str) -> Option<String> {
    if !content.contains("Model Selection") {
        return None;
    }
    let marker = "Model Selection` from ";
    let start = content.find(marker)?;
    let sub = &content[start + marker.len()..];
    let to_pos = sub.find(" to ")?;
    let candidate_sub = &sub[to_pos + 4..];
    let end_pos = candidate_sub
        .find(".\n")
        .or_else(|| candidate_sub.find(". "))
        .or_else(|| candidate_sub.find(".\r"))
        .or_else(|| candidate_sub.find("."))
        .unwrap_or(candidate_sub.len());
    let candidate = candidate_sub[..end_pos].trim();
    if !candidate.is_empty() && candidate.len() < 60 && !candidate.eq_ignore_ascii_case("none") {
        let normalized = normalize_model_slug(candidate);
        if !normalized.is_empty() && !is_model_noise(&normalized) {
            return Some(normalized);
        }
    }
    None
}

pub fn parse_antigravity_file(path: &Path) -> CachedFile {
    let file_fingerprint = antigravity_fingerprint(path);
    let Ok(text) = fs::read_to_string(path) else {
        return CachedFile {
            fingerprint: file_fingerprint,
            ..Default::default()
        };
    };
    let mut session_id = antigravity_session_id(path);
    if session_id.is_empty() {
        session_id = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_string();
    }
    let (database_model, database_project) = antigravity_database_metadata(path);
    let mut model = if database_model.is_empty() {
        UNKNOWN_ANTIGRAVITY_MODEL.to_string()
    } else {
        database_model
    };
    let transcript_project = extract_antigravity_project_from_transcript(&text);
    let project_key = if let Some(tp) = transcript_project {
        tp
    } else if !database_project.is_empty() {
        normalize_workspace_project_key(&database_project, "Antigravity")
    } else {
        antigravity_fallback_project(path)
    };
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut visible_context_tokens = 0i64;
    let mut turns = 0i64;
    let mut planner_responses = 0i64;
    let mut events = Vec::<UsageEvent>::new();

    for (index, line) in text.lines().enumerate() {
        let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };
        if model == UNKNOWN_ANTIGRAVITY_MODEL {
            if let Some(content) = value.get("content").and_then(JsonValue::as_str) {
                if let Some(candidate) = extract_model_from_transcript_content(content) {
                    model = candidate;
                }
            }
        }
        let timestamp = value
            .get("created_at")
            .or_else(|| value.get("timestamp"))
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        update_bounds(&mut first_ts, &mut last_ts, &timestamp);
        let kind = value.get("type").and_then(JsonValue::as_str).unwrap_or("");
        let source = value
            .get("source")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        let content_tokens =
            estimate_local_content_tokens(value.get("content").unwrap_or(&JsonValue::Null));
        let thinking_tokens =
            estimate_local_content_tokens(value.get("thinking").unwrap_or(&JsonValue::Null));
        let tool_call_tokens =
            estimate_local_content_tokens(value.get("tool_calls").unwrap_or(&JsonValue::Null));
        let error_tokens =
            estimate_local_content_tokens(value.get("error").unwrap_or(&JsonValue::Null));
        let context_delta = content_tokens
            .saturating_add(thinking_tokens)
            .saturating_add(tool_call_tokens)
            .saturating_add(error_tokens);
        let event_id = value
            .get("step_index")
            .and_then(JsonValue::as_i64)
            .map(|step| format!("{session_id}:{step}"))
            .unwrap_or_else(|| format!("{session_id}:{index}"));

        if kind == "USER_INPUT" && source == "USER_EXPLICIT" {
            turns += 1;
            events.push(UsageEvent {
                id: format!("u:{event_id}"),
                source: "antigravity".to_string(),
                model: model.clone(),
                project_key: project_key.clone(),
                timestamp,
                conversation_count: 1,
                ..Default::default()
            });
            visible_context_tokens = visible_context_tokens.saturating_add(context_delta);
            continue;
        }

        if kind == "PLANNER_RESPONSE" {
            planner_responses += 1;
            let input_tokens = visible_context_tokens
                .saturating_add(32)
                .min(LOCAL_ESTIMATED_CONTEXT_LIMIT);
            let output_tokens = content_tokens.saturating_add(tool_call_tokens);
            let reasoning_output_tokens = thinking_tokens;
            // 思考 token 独立上报，不计入 total。
            let total_tokens = input_tokens.saturating_add(output_tokens);
            events.push(UsageEvent {
                id: event_id,
                source: "antigravity".to_string(),
                model: model.clone(),
                project_key: project_key.clone(),
                timestamp,
                input_tokens,
                output_tokens,
                reasoning_output_tokens,
                total_tokens,
                estimated_tokens: total_tokens,
                ..Default::default()
            });

            // 修复：重置上下文累积，避免长会话中输入 token 10x-100x 虚增
            // 每次响应后只保留本次输出作为下一轮的上下文基础
            // 之前的无限累积会导致第 10 轮请求被计为 10 倍输入
            visible_context_tokens = context_delta;
            continue;
        }

        // 其他事件类型（非 PLANNER_RESPONSE）才累积到上下文
        visible_context_tokens = visible_context_tokens.saturating_add(context_delta);
    }

    if model != UNKNOWN_ANTIGRAVITY_MODEL {
        for event in &mut events {
            if event.model == UNKNOWN_ANTIGRAVITY_MODEL {
                event.model = model.clone();
            }
        }
    }

    let tokens = events
        .iter()
        .fold(TokenSessionTokens::default(), |mut total, event| {
            total.input_tokens += event.input_tokens;
            total.cached_input_tokens += event.cached_input_tokens;
            total.cache_creation_input_tokens += event.cache_creation_input_tokens;
            total.output_tokens += event.output_tokens;
            total.reasoning_output_tokens += event.reasoning_output_tokens;
            total.total_tokens += event.total_tokens;
            total
        });
    let mut session = token_session(
        session_id,
        "antigravity",
        project_key,
        model,
        first_ts,
        last_ts,
        turns,
        tokens,
        0.0,
    );
    session.productive = turns > 0 && planner_responses > 0;
    session.provenance = json!({
        "source": "openhub-local-collector",
        "confidence": "estimated",
        "privacy": "metadata-only",
        "tokenUsage": "estimated-antigravity-local-context",
        "plannerResponses": planner_responses,
        "estimationMethod": "visible-context-chars-v1",
        "estimatedContextLimit": LOCAL_ESTIMATED_CONTEXT_LIMIT
    });
    CachedFile {
        fingerprint: file_fingerprint,
        events,
        sessions: vec![session],
    }
}
