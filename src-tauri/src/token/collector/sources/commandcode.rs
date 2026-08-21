use crate::models::TokenSessionTokens;
use crate::token::collector::normalizer::{
    command_code_project_from_path, command_code_sidecar_model, is_common_subfolder,
    normalize_workspace_project_key,
};
use crate::token::collector::sources::claude::claude_user_is_human;
use crate::token::collector::time_utils::update_bounds;
use crate::token::collector::types::{
    fingerprint, float_number, number, token_session, CachedFile, FileFingerprint, UsageEvent,
    LOCAL_ESTIMATED_CONTEXT_LIMIT, UNKNOWN_COMMAND_CODE_MODEL,
};
use serde_json::{json, Value as JsonValue};
use std::fs;
use std::path::{Path, PathBuf};

pub fn is_command_code_transcript_path(path: &Path) -> bool {
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

pub fn command_code_json_value<'a>(value: &'a JsonValue, keys: &[&str]) -> Option<&'a JsonValue> {
    for key in keys {
        if let Some(found) = value.get(*key) {
            return Some(found);
        }
    }
    None
}

pub fn command_code_usage(value: &JsonValue) -> Option<&JsonValue> {
    command_code_json_value(value, &["usage", "tokenUsage", "token_usage"])
        .filter(|usage| usage.is_object())
        .or_else(|| {
            value
                .get("message")
                .and_then(|message| {
                    command_code_json_value(message, &["usage", "tokenUsage", "token_usage"])
                })
                .filter(|usage| usage.is_object())
        })
        .or_else(|| {
            value
                .get("metadata")
                .and_then(|metadata| {
                    command_code_json_value(metadata, &["usage", "tokenUsage", "token_usage"])
                })
                .filter(|usage| usage.is_object())
        })
}

pub fn local_content_chars(value: &JsonValue) -> (i64, i64) {
    fn walk(value: &JsonValue, ascii: &mut i64, non_ascii: &mut i64) {
        match value {
            JsonValue::String(text) => {
                for ch in text.chars() {
                    if ch.is_ascii() {
                        *ascii += 1;
                    } else {
                        *non_ascii += 1;
                    }
                }
            }
            JsonValue::Array(items) => {
                for item in items {
                    walk(item, ascii, non_ascii);
                }
            }
            JsonValue::Object(fields) => {
                for item in fields.values() {
                    walk(item, ascii, non_ascii);
                }
            }
            _ => {}
        }
    }
    let mut ascii = 0i64;
    let mut non_ascii = 0i64;
    walk(value, &mut ascii, &mut non_ascii);
    (ascii, non_ascii)
}

pub fn estimate_local_content_tokens(content: &JsonValue) -> i64 {
    let (ascii, non_ascii) = local_content_chars(content);
    ascii
        .saturating_add(3)
        .div_euclid(4)
        .saturating_add(non_ascii)
        .saturating_add(4)
}

pub fn command_code_meta_path(path: &Path) -> PathBuf {
    path.with_extension("meta.json")
}

pub fn command_code_fingerprint(path: &Path) -> FileFingerprint {
    let transcript = fingerprint(path);
    let sidecar = fingerprint(&command_code_meta_path(path));
    FileFingerprint {
        size: transcript.size.saturating_add(sidecar.size),
        modified_ms: transcript.modified_ms.max(sidecar.modified_ms),
    }
}

pub fn parse_command_code_file(path: &Path) -> CachedFile {
    let file_fingerprint = command_code_fingerprint(path);
    let Ok(text) = fs::read_to_string(path) else {
        return CachedFile {
            fingerprint: file_fingerprint,
            ..Default::default()
        };
    };
    let fallback_id = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_string();
    let mut session_id = fallback_id;
    let mut project_key = command_code_project_from_path(path);
    let mut session_model = command_code_sidecar_model(path);
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut user_event_count = 0i64;
    let mut assistant_message_count = 0i64;
    let mut exact_usage_events = 0i64;
    let mut estimated_usage_events = 0i64;
    let mut visible_context_tokens = 0i64;
    let mut events = Vec::<UsageEvent>::new();

    for (index, line) in text.lines().enumerate() {
        let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };
        let entry_type = value.get("type").and_then(JsonValue::as_str).unwrap_or("");
        if entry_type == "session" {
            if let Some(id) = value
                .get("id")
                .and_then(JsonValue::as_str)
                .filter(|value| !value.trim().is_empty())
            {
                session_id = id.to_string();
            }
            if let Some(cwd) = value
                .get("cwd")
                .and_then(JsonValue::as_str)
                .filter(|value| !value.trim().is_empty())
            {
                let resolved = normalize_workspace_project_key(cwd, &project_key);
                if !resolved.is_empty()
                    && (project_key == "Command Code"
                        || is_common_subfolder(&project_key)
                        || (!is_common_subfolder(&resolved) && resolved != "Command Code"))
                {
                    project_key = resolved;
                }
            }
            continue;
        }

        let message = if entry_type == "message" {
            value.get("message").unwrap_or(&JsonValue::Null)
        } else {
            &value
        };
        let role = message
            .get("role")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        if role != "user" && role != "assistant" && role != "tool" {
            continue;
        }
        if let Some(id) = value
            .get("sessionId")
            .or_else(|| value.get("session_id"))
            .and_then(JsonValue::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            session_id = id.to_string();
        }
        let timestamp = value
            .get("timestamp")
            .or_else(|| value.get("metadata").and_then(|meta| meta.get("timestamp")))
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        update_bounds(&mut first_ts, &mut last_ts, &timestamp);

        let entry_model = value
            .get("model")
            .and_then(JsonValue::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("");
        if !entry_model.is_empty() {
            session_model = entry_model.to_string();
        }
        let effective_model = if entry_model.is_empty() {
            if session_model.is_empty() {
                UNKNOWN_COMMAND_CODE_MODEL.to_string()
            } else {
                session_model.clone()
            }
        } else {
            entry_model.to_string()
        };

        let id = value
            .get("id")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{session_id}:{role}:{index}"));
        let content = message.get("content").unwrap_or(&JsonValue::Null);
        let content_tokens = estimate_local_content_tokens(content);
        if role == "tool" {
            visible_context_tokens = visible_context_tokens.saturating_add(content_tokens);
            continue;
        }
        if role == "user" {
            if claude_user_is_human(content) {
                user_event_count += 1;
                events.push(UsageEvent {
                    id: format!("u:{id}"),
                    source: "command-code".to_string(),
                    model: effective_model,
                    project_key: project_key.clone(),
                    timestamp,
                    conversation_count: 1,
                    ..Default::default()
                });
            }
            visible_context_tokens = visible_context_tokens.saturating_add(content_tokens);
            continue;
        }

        assistant_message_count += 1;
        if let Some(usage) = command_code_usage(&value) {
            let input_tokens = number(usage, &["inputTokens", "input_tokens"]);
            let output_tokens = number(usage, &["outputTokens", "output_tokens"]);
            let cached_input_tokens = number(
                usage,
                &["cacheReadTokens", "cache_read_tokens", "cachedInputTokens"],
            );
            let cache_creation_input_tokens = number(
                usage,
                &[
                    "cacheWriteTokens",
                    "cache_write_tokens",
                    "cacheCreationInputTokens",
                ],
            );
            let total_tokens = input_tokens
                .saturating_add(output_tokens)
                .saturating_add(cached_input_tokens)
                .saturating_add(cache_creation_input_tokens);
            let cost_usd = float_number(usage, &["costUsd", "cost_usd"]);
            exact_usage_events += 1;
            events.push(UsageEvent {
                id,
                source: "command-code".to_string(),
                model: effective_model,
                project_key: project_key.clone(),
                timestamp,
                input_tokens,
                cached_input_tokens,
                cache_creation_input_tokens,
                output_tokens,
                reasoning_output_tokens: 0,
                total_tokens,
                conversation_count: 0,
                cost_usd,
                pricing_available: cost_usd > 0.0,
                estimated_tokens: 0,
            });
        } else {
            let input_tokens = visible_context_tokens
                .saturating_add(32)
                .min(LOCAL_ESTIMATED_CONTEXT_LIMIT);
            let output_tokens = content_tokens;
            let total_tokens = input_tokens.saturating_add(output_tokens);
            estimated_usage_events += 1;
            events.push(UsageEvent {
                id,
                source: "command-code".to_string(),
                model: effective_model,
                project_key: project_key.clone(),
                timestamp,
                input_tokens,
                output_tokens,
                total_tokens,
                conversation_count: 0,
                estimated_tokens: total_tokens,
                ..Default::default()
            });
        }
        visible_context_tokens = visible_context_tokens.saturating_add(content_tokens);
    }

    if session_model.is_empty() {
        session_model = UNKNOWN_COMMAND_CODE_MODEL.to_string();
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
    let turns = user_event_count;
    let cost_usd = events.iter().map(|event| event.cost_usd).sum();
    let mut session = token_session(
        session_id,
        "command-code",
        project_key,
        session_model,
        first_ts,
        last_ts,
        turns,
        tokens,
        cost_usd,
    );
    session.productive = turns > 0 && assistant_message_count > 0;
    session.provenance = json!({
        "source": "openhub-local-collector",
        "confidence": "observed",
        "privacy": "metadata-only",
        "tokenUsage": if estimated_usage_events > 0 {
            if exact_usage_events > 0 { "mixed-observed-and-estimated" } else { "estimated-v2-local-context" }
        } else { "observed-v3" },
        "assistantMessages": assistant_message_count,
        "exactUsageEvents": exact_usage_events,
        "estimatedUsageEvents": estimated_usage_events,
        "estimationMethod": if estimated_usage_events > 0 { "visible-context-chars-v1" } else { "none" },
        "estimatedContextLimit": if estimated_usage_events > 0 { LOCAL_ESTIMATED_CONTEXT_LIMIT } else { 0 }
    });
    CachedFile {
        fingerprint: file_fingerprint,
        events,
        sessions: vec![session],
    }
}
