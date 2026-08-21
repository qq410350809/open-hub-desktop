use crate::models::TokenSessionTokens;
use crate::token::collector::normalizer::normalize_workspace_project_key;
use crate::token::collector::time_utils::{iso_from_millis, update_bounds};
use crate::token::collector::types::{
    fingerprint, number, token_session, CachedFile, UsageEvent, UNKNOWN_DSH_MODEL,
};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[allow(dead_code)]
pub fn dsh_sessions_dir(home: &Path) -> PathBuf {
    home.join(".dsh").join("sessions")
}

pub fn dsh_user_is_human(payload: &JsonValue) -> bool {
    let data = payload.get("data").unwrap_or(&JsonValue::Null);
    let kind = data
        .get("source")
        .or_else(|| payload.get("source"))
        .and_then(|source| source.get("kind"))
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    if !kind.is_empty() {
        return kind == "user";
    }
    let content = data
        .get("content")
        .or_else(|| payload.get("content"))
        .unwrap_or(&JsonValue::Null);
    let text = dsh_content_text(content);
    let trimmed = text.trim_start();
    !(trimmed.starts_with("<system-reminder>")
        || trimmed.starts_with("Current runtime context")
        || trimmed.starts_with("[Request")
        || trimmed.starts_with("<environment"))
}

pub fn dsh_content_text(content: &JsonValue) -> String {
    match content {
        JsonValue::String(text) => text.clone(),
        JsonValue::Array(items) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(JsonValue::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

pub fn parse_dsh_file(path: &Path) -> CachedFile {
    let fp = fingerprint(path);
    let Ok(raw) = fs::read(path) else {
        return CachedFile {
            fingerprint: fp,
            ..Default::default()
        };
    };
    let text = match zstd::decode_all(raw.as_slice()) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => {
                return CachedFile {
                    fingerprint: fp,
                    ..Default::default()
                }
            }
        },
        Err(_) => {
            return CachedFile {
                fingerprint: fp,
                ..Default::default()
            }
        }
    };

    let mut session_id = String::new();
    let mut project_key = String::new();
    let mut model = String::new();
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut user_events: BTreeMap<String, String> = BTreeMap::new();
    let mut usage_events: BTreeMap<String, UsageEvent> = BTreeMap::new();
    let mut last_user_ts = String::new();
    let mut user_models: BTreeMap<String, String> = BTreeMap::new();
    let mut pending_user_ids: Vec<String> = Vec::new();

    for (index, line) in text.lines().enumerate() {
        let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };
        let kind = value.get("type").and_then(JsonValue::as_str).unwrap_or("");
        let time_ms = value.get("time").and_then(JsonValue::as_i64).unwrap_or(0);
        let timestamp = iso_from_millis(time_ms);

        if kind == "session" {
            if let Some(id) = value
                .get("id")
                .and_then(JsonValue::as_str)
                .filter(|v| !v.is_empty())
            {
                session_id = id.to_string();
            }
            if let Some(cwd) = value
                .get("cwd")
                .and_then(JsonValue::as_str)
                .filter(|v| !v.is_empty())
            {
                project_key = normalize_workspace_project_key(cwd, "DSH");
            }
            if !timestamp.is_empty() {
                update_bounds(&mut first_ts, &mut last_ts, &timestamp);
            }
            continue;
        }

        if kind == "user/message" {
            if !timestamp.is_empty() {
                update_bounds(&mut first_ts, &mut last_ts, &timestamp);
            }
            if dsh_user_is_human(&value) {
                last_user_ts = timestamp.clone();
                let id = format!("dsh:user:{index}");
                user_events.entry(id.clone()).or_insert(timestamp);
                pending_user_ids.push(id);
            }
            continue;
        }

        if kind == "assistant/message" {
            let data = value.get("data").unwrap_or(&JsonValue::Null);
            if !timestamp.is_empty() {
                update_bounds(&mut first_ts, &mut last_ts, &timestamp);
            }
            let msg = data.get("message").unwrap_or(&JsonValue::Null);
            if let Some(m) = msg
                .get("source")
                .and_then(|s| s.get("model"))
                .and_then(JsonValue::as_str)
                .filter(|v| !v.is_empty())
            {
                model = m.to_string();
            }
            if let Some(usage) = data.get("usage").filter(|u| u.is_object()) {
                for pending_id in pending_user_ids.drain(..) {
                    user_models
                        .entry(pending_id)
                        .or_insert_with(|| model.clone());
                }
                let anchor_ts = if last_user_ts.is_empty() {
                    timestamp.clone()
                } else {
                    last_user_ts.clone()
                };
                let event =
                    dsh_usage_event(usage, &session_id, &project_key, &model, &anchor_ts, index);
                if let Some(ev) = event {
                    let msg_id = ev.id.clone();
                    let total = ev.total_tokens;
                    let should_replace = usage_events
                        .get(&msg_id)
                        .map(|ex| total > ex.total_tokens)
                        .unwrap_or(true);
                    if should_replace {
                        usage_events.insert(msg_id, ev);
                    }
                }
            }
            continue;
        }

        if kind == "assistant/chunk" {
            let data = value.get("data").unwrap_or(&JsonValue::Null);
            if !timestamp.is_empty() {
                update_bounds(&mut first_ts, &mut last_ts, &timestamp);
            }
            if let Some(usage) = data
                .get("chunk")
                .and_then(|c| c.get("usage"))
                .filter(|u| u.is_object())
            {
                let turn = data.get("turn").and_then(JsonValue::as_i64).unwrap_or(0);
                let step = data.get("step").and_then(JsonValue::as_i64).unwrap_or(0);
                let anchor_ts = if last_user_ts.is_empty() {
                    timestamp.clone()
                } else {
                    last_user_ts.clone()
                };
                let event =
                    dsh_usage_event(usage, &session_id, &project_key, &model, &anchor_ts, index);
                if let Some(ev) = event {
                    let chunk_id = format!("{}:chunk:{}:{}", ev.id, turn, step);
                    let should_replace = usage_events
                        .get(&chunk_id)
                        .map(|ex| ev.total_tokens > ex.total_tokens)
                        .unwrap_or(true);
                    if should_replace {
                        usage_events.insert(chunk_id.clone(), UsageEvent { id: chunk_id, ..ev });
                    }
                }
            }
            continue;
        }
    }

    if session_id.is_empty() {
        session_id = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("dsh-session")
            .to_string();
    }
    if project_key.is_empty() || project_key == "DSH" {
        project_key = "临时任务 / 独立会话".to_string();
    }
    if model.is_empty() {
        model = UNKNOWN_DSH_MODEL.to_string();
    }

    let has_message_usage = usage_events.keys().any(|k| !k.contains(":chunk:"));
    if has_message_usage {
        usage_events.retain(|k, _| !k.contains(":chunk:"));
    }

    let mut events = usage_events.into_values().collect::<Vec<_>>();
    events.extend(user_events.into_iter().map(|(id, timestamp)| {
        UsageEvent {
            id: id.clone(),
            source: "dsh".to_string(),
            model: user_models
                .get(&id)
                .cloned()
                .unwrap_or_else(|| model.clone()),
            project_key: project_key.clone(),
            timestamp,
            conversation_count: 1,
            ..Default::default()
        }
    }));

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
    let turns = events.iter().map(|e| e.conversation_count).sum();
    let session = token_session(
        session_id,
        "dsh",
        project_key,
        model,
        first_ts,
        last_ts,
        turns,
        tokens,
        0.0,
    );
    CachedFile {
        fingerprint: fp,
        events,
        sessions: vec![session],
    }
}

fn dsh_usage_event(
    usage: &JsonValue,
    session_id: &str,
    project_key: &str,
    model: &str,
    timestamp: &str,
    index: usize,
) -> Option<UsageEvent> {
    let input = number(usage, &["inputTokens", "input_tokens"]);
    let cached = number(usage, &["cacheReadTokens", "cache_read_input_tokens"]);
    let output = number(usage, &["outputTokens", "output_tokens"]);
    let total = input.saturating_add(cached).saturating_add(output);
    if total <= 0 || timestamp.is_empty() {
        return None;
    }
    let id = format!("{session_id}:dsh:{index}");
    Some(UsageEvent {
        id,
        source: "dsh".to_string(),
        model: if model.is_empty() {
            UNKNOWN_DSH_MODEL.to_string()
        } else {
            model.to_string()
        },
        project_key: project_key.to_string(),
        timestamp: timestamp.to_string(),
        input_tokens: input,
        cached_input_tokens: cached,
        cache_creation_input_tokens: 0,
        output_tokens: output,
        reasoning_output_tokens: 0,
        total_tokens: total,
        conversation_count: 0,
        cost_usd: 0.0,
        pricing_available: false,
        estimated_tokens: 0,
    })
}
