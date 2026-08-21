use crate::models::TokenSessionTokens;
use crate::token::collector::normalizer::{
    claude_project_from_path, is_common_subfolder, normalize_workspace_project_key,
};
use crate::token::collector::time_utils::update_bounds;
use crate::token::collector::types::{
    fingerprint, number, token_session, CachedFile, UsageEvent, UNKNOWN_CLAUDE_MODEL,
};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub fn claude_config_dir(home: &Path) -> PathBuf {
    crate::token::collector::aggregator::env_path_override("CLAUDE_CONFIG_DIR")
        .unwrap_or_else(|| home.join(".claude"))
}

pub fn claude_user_is_human(content: &JsonValue) -> bool {
    fn human_text(text: &str) -> bool {
        let trimmed = text.trim();
        !trimmed.is_empty()
            && !trimmed.starts_with("[Request interrupted")
            && !trimmed.starts_with("<local-command-stdout>")
            && !trimmed.starts_with("<command-stdout>")
    }
    match content {
        JsonValue::String(text) => human_text(text),
        JsonValue::Array(items) => {
            if items
                .iter()
                .any(|item| item.get("type").and_then(JsonValue::as_str) == Some("tool_result"))
            {
                return false;
            }
            items.iter().any(|item| {
                if let Some(text) = item.get("text").and_then(JsonValue::as_str) {
                    return human_text(text);
                }
                matches!(item.get("type").and_then(JsonValue::as_str), Some("image"))
            })
        }
        _ => false,
    }
}

pub fn claude_user_line_is_human(value: &JsonValue, content: &JsonValue) -> bool {
    if !claude_user_is_human(content) {
        return false;
    }
    match value
        .get("origin")
        .and_then(|origin| origin.get("kind"))
        .and_then(JsonValue::as_str)
    {
        Some(kind) => kind == "human",
        None => true,
    }
}

pub fn parse_claude_file(path: &Path) -> CachedFile {
    let Ok(text) = fs::read_to_string(path) else {
        return CachedFile {
            fingerprint: fingerprint(path),
            ..Default::default()
        };
    };
    let fallback_id = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_string();
    let mut session_id = fallback_id.clone();
    let mut project_key = claude_project_from_path(path);
    let is_subagent_file = path
        .components()
        .any(|component| component.as_os_str() == "subagents");
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
        let is_sidechain = value
            .get("isSidechain")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        if !is_sidechain {
            if let Some(value) = value
                .get("sessionId")
                .or_else(|| value.get("session_id"))
                .and_then(JsonValue::as_str)
                .filter(|value| !value.trim().is_empty())
            {
                session_id = value.to_string();
            }
        }
        if let Some(cwd) = value
            .get("cwd")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            let resolved = normalize_workspace_project_key(cwd, &project_key);
            if !resolved.is_empty()
                && (project_key == "Claude"
                    || is_common_subfolder(&project_key)
                    || (!is_common_subfolder(&resolved) && resolved != "Claude"))
            {
                project_key = resolved;
            }
        }
        let kind = value.get("type").and_then(JsonValue::as_str).unwrap_or("");
        if kind != "user" && kind != "assistant" {
            continue;
        }
        let timestamp = value
            .get("timestamp")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        update_bounds(&mut first_ts, &mut last_ts, &timestamp);

        if kind == "user" {
            if is_sidechain {
                continue;
            }
            let content = value
                .get("message")
                .and_then(|message| message.get("content"))
                .unwrap_or(&JsonValue::Null);
            if !claude_user_line_is_human(&value, content) {
                continue;
            }
            last_user_ts = timestamp.clone();
            let id = value
                .get("uuid")
                .and_then(JsonValue::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("{session_id}:user:{index}"));
            user_events.entry(id.clone()).or_insert(timestamp);
            pending_user_ids.push(id);
            continue;
        }

        let message = value.get("message").unwrap_or(&JsonValue::Null);
        let message_model = message
            .get("model")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .trim();
        if !message_model.is_empty() {
            model = message_model.to_string();
        }
        let Some(usage) = message.get("usage").filter(|usage| usage.is_object()) else {
            continue;
        };
        let input = number(usage, &["input_tokens", "inputTokens"]);
        let cached = number(
            usage,
            &[
                "cache_read_input_tokens",
                "cacheReadInputTokens",
                "cached_input_tokens",
            ],
        );
        let cache_creation = number(
            usage,
            &["cache_creation_input_tokens", "cacheCreationInputTokens"],
        );
        let output = number(usage, &["output_tokens", "outputTokens"]);
        let total = input
            .saturating_add(cached)
            .saturating_add(cache_creation)
            .saturating_add(output);
        if total <= 0 || timestamp.is_empty() {
            continue;
        }
        let turn_model = if message_model.is_empty() {
            UNKNOWN_CLAUDE_MODEL.to_string()
        } else {
            message_model.to_string()
        };
        for pending_id in pending_user_ids.drain(..) {
            user_models.entry(pending_id).or_insert(turn_model.clone());
        }
        let message_id = message
            .get("id")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.is_empty())
            .or_else(|| value.get("uuid").and_then(JsonValue::as_str))
            .map(str::to_string)
            .unwrap_or_else(|| format!("{session_id}:assistant:{index}"));
        let request_id = value
            .get("requestId")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or("");
        let dedup_id = if request_id.is_empty() {
            message_id
        } else {
            format!("{message_id}:{request_id}")
        };
        let event = UsageEvent {
            id: dedup_id.clone(),
            source: "claude".to_string(),
            model: if message_model.is_empty() {
                UNKNOWN_CLAUDE_MODEL.to_string()
            } else {
                message_model.to_string()
            },
            project_key: project_key.clone(),
            timestamp: if last_user_ts.is_empty() {
                timestamp
            } else {
                last_user_ts.clone()
            },
            input_tokens: input,
            cached_input_tokens: cached,
            cache_creation_input_tokens: cache_creation,
            output_tokens: output,
            reasoning_output_tokens: 0,
            total_tokens: total,
            conversation_count: 0,
            cost_usd: 0.0,
            pricing_available: false,
            estimated_tokens: 0,
        };
        let should_replace = usage_events
            .get(&dedup_id)
            .map(|existing| event.total_tokens > existing.total_tokens)
            .unwrap_or(true);
        if should_replace {
            usage_events.insert(dedup_id, event);
        }
    }

    if model.is_empty() {
        model = UNKNOWN_CLAUDE_MODEL.to_string();
    }
    let mut events = usage_events.into_values().collect::<Vec<_>>();
    events.extend(user_events.into_iter().map(|(id, timestamp)| {
        UsageEvent {
            id: format!("u:{id}"),
            source: "claude".to_string(),
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
    let turns = events.iter().map(|event| event.conversation_count).sum();
    let session = token_session(
        if is_subagent_file {
            format!("{session_id}:agent:{fallback_id}")
        } else {
            session_id
        },
        "claude",
        project_key,
        model,
        first_ts,
        last_ts,
        turns,
        tokens,
        0.0,
    );
    CachedFile {
        fingerprint: fingerprint(path),
        events,
        sessions: vec![session],
    }
}
