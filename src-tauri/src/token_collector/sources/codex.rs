use crate::models::TokenSessionTokens;
use crate::token_collector::normalizer::{is_common_subfolder, normalize_workspace_project_key};
use crate::token_collector::time_utils::update_bounds;
use crate::token_collector::types::{
    fingerprint, number, token_session, CachedFile, UsageEvent, UNKNOWN_CODEX_MODEL,
};
use serde_json::Value as JsonValue;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub fn codex_home(home: &Path) -> PathBuf {
    crate::token_collector::aggregator::env_path_override("CODEX_HOME")
        .unwrap_or_else(|| home.join(".codex"))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodexUsage {
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_output_tokens: i64,
    pub total_tokens: i64,
}

impl CodexUsage {
    pub fn from_json(value: &JsonValue) -> Option<Self> {
        if !value.is_object() {
            return None;
        }
        let usage = Self {
            input_tokens: number(value, &["input_tokens"]),
            cached_input_tokens: number(value, &["cached_input_tokens"]),
            cache_creation_input_tokens: number(
                value,
                &["cache_creation_input_tokens", "cache_write_input_tokens"],
            ),
            output_tokens: number(value, &["output_tokens"]),
            reasoning_output_tokens: number(value, &["reasoning_output_tokens"]),
            total_tokens: number(value, &["total_tokens"]),
        };
        Some(usage)
    }

    pub fn subtract(self, other: Self) -> Option<Self> {
        if self.input_tokens < other.input_tokens
            || self.cached_input_tokens < other.cached_input_tokens
            || self.cache_creation_input_tokens < other.cache_creation_input_tokens
            || self.output_tokens < other.output_tokens
            || self.reasoning_output_tokens < other.reasoning_output_tokens
            || self.total_tokens < other.total_tokens
        {
            return None;
        }
        Some(Self {
            input_tokens: self.input_tokens - other.input_tokens,
            cached_input_tokens: self.cached_input_tokens - other.cached_input_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens
                - other.cache_creation_input_tokens,
            output_tokens: self.output_tokens - other.output_tokens,
            reasoning_output_tokens: self.reasoning_output_tokens - other.reasoning_output_tokens,
            total_tokens: self.total_tokens - other.total_tokens,
        })
    }

    pub fn diff(self, other: Self) -> Self {
        Self {
            input_tokens: (self.input_tokens - other.input_tokens).max(0),
            cached_input_tokens: (self.cached_input_tokens - other.cached_input_tokens).max(0),
            cache_creation_input_tokens: (self.cache_creation_input_tokens
                - other.cache_creation_input_tokens)
                .max(0),
            output_tokens: (self.output_tokens - other.output_tokens).max(0),
            reasoning_output_tokens: (self.reasoning_output_tokens - other.reasoning_output_tokens)
                .max(0),
            total_tokens: (self.total_tokens - other.total_tokens).max(0),
        }
    }

    pub fn normalized(self) -> Self {
        let fresh_input = self.input_tokens.saturating_sub(self.cached_input_tokens);
        let total = fresh_input
            .saturating_add(self.cached_input_tokens)
            .saturating_add(self.cache_creation_input_tokens)
            .saturating_add(self.output_tokens);
        Self {
            input_tokens: fresh_input,
            total_tokens: total,
            ..self
        }
    }
}

#[derive(Default)]
pub struct CodexUsageState {
    pub last_total: Option<CodexUsage>,
    pub baselines: Vec<CodexUsage>,
}

impl CodexUsageState {
    pub fn touch(&mut self, usage: CodexUsage) {
        if let Some(index) = self.baselines.iter().position(|item| *item == usage) {
            self.baselines.remove(index);
        }
        self.baselines.push(usage);
        if self.baselines.len() > 32 {
            self.baselines.remove(0);
        }
        self.last_total = Some(usage);
    }

    pub fn consume(
        &mut self,
        last_usage: Option<CodexUsage>,
        total_usage: Option<CodexUsage>,
    ) -> Option<CodexUsage> {
        let Some(total) = total_usage else {
            return last_usage;
        };
        if self.baselines.contains(&total) {
            self.touch(total);
            return None;
        }
        if let Some(last) = last_usage {
            if let Some(previous) = total.subtract(last) {
                if self.baselines.contains(&previous) {
                    self.touch(total);
                    return Some(last);
                }
                if self.last_total.is_some() {
                    self.touch(total);
                    return Some(last);
                }
            }
        }
        if let Some(active) = self.last_total {
            if total.total_tokens >= active.total_tokens {
                let delta = total.diff(active);
                if last_usage
                    .map(|last| delta.total_tokens <= last.total_tokens)
                    .unwrap_or(true)
                {
                    self.touch(total);
                    return Some(delta);
                }
            }
        }
        self.touch(total);
        last_usage
    }
}

pub fn codex_user_message_is_human(payload: &JsonValue) -> bool {
    for item in payload
        .get("content")
        .and_then(JsonValue::as_array)
        .map(|items| items.as_slice())
        .unwrap_or(&[])
    {
        if let Some(text) = item.get("text").and_then(JsonValue::as_str) {
            let trimmed = text.trim_start();
            if trimmed.is_empty() {
                continue;
            }
            return !(trimmed.starts_with("<environment_context")
                || trimmed.starts_with("<codex_internal_context")
                || trimmed.starts_with("<turn_aborted"));
        }
    }
    false
}

pub fn parse_codex_file(path: &Path) -> CachedFile {
    let Ok(text) = fs::read_to_string(path) else {
        return CachedFile {
            fingerprint: fingerprint(path),
            ..Default::default()
        };
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
    let fallback_id = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_string();
    let mut session_id = fallback_id;
    let mut project_key = "Codex".to_string();
    let mut current_model = String::new();
    let mut session_model = String::new();
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut usage_state = CodexUsageState::default();
    let mut seen_usage = HashSet::<String>::new();
    let mut events = Vec::<UsageEvent>::new();

    for (index, line) in text.lines().enumerate() {
        let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };
        let timestamp = value
            .get("timestamp")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        update_bounds(&mut first_ts, &mut last_ts, &timestamp);
        let kind = value.get("type").and_then(JsonValue::as_str).unwrap_or("");
        let payload = value.get("payload").unwrap_or(&JsonValue::Null);
        if kind == "session_meta" {
            if let Some(id) = payload
                .get("id")
                .or_else(|| payload.get("session_id"))
                .and_then(JsonValue::as_str)
                .filter(|value| !value.is_empty())
            {
                session_id = id.to_string();
            }
            if let Some(cwd) = payload.get("cwd").and_then(JsonValue::as_str) {
                let resolved = normalize_workspace_project_key(cwd, &project_key);
                if !resolved.is_empty()
                    && (project_key == "Codex"
                        || is_common_subfolder(&project_key)
                        || (!is_common_subfolder(&resolved) && resolved != "Codex"))
                {
                    project_key = resolved;
                }
            }
            if current_model.is_empty() {
                if let Some(provider) = payload
                    .get("model_provider")
                    .and_then(JsonValue::as_str)
                    .filter(|value| !value.is_empty())
                {
                    current_model = provider.to_string();
                }
            }
            continue;
        }
        if kind == "turn_context" {
            if let Some(cwd) = payload.get("cwd").and_then(JsonValue::as_str) {
                let resolved = normalize_workspace_project_key(cwd, &project_key);
                if !resolved.is_empty()
                    && (project_key == "Codex"
                        || is_common_subfolder(&project_key)
                        || (!is_common_subfolder(&resolved) && resolved != "Codex"))
                {
                    project_key = resolved;
                }
            }
            if let Some(model) = payload
                .get("model")
                .and_then(JsonValue::as_str)
                .filter(|value| !value.is_empty())
            {
                current_model = model.to_string();
                session_model = model.to_string();
            }
            continue;
        }
        if kind == "response_item" {
            if !has_user_message_events
                && payload.get("type").and_then(JsonValue::as_str) == Some("message")
                && payload.get("role").and_then(JsonValue::as_str) == Some("user")
                && codex_user_message_is_human(payload)
            {
                let id = payload
                    .get("id")
                    .and_then(JsonValue::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{session_id}:user:{index}"));
                events.push(UsageEvent {
                    id: format!("u:{id}"),
                    source: "codex".to_string(),
                    model: if current_model.is_empty() {
                        UNKNOWN_CODEX_MODEL.to_string()
                    } else {
                        current_model.clone()
                    },
                    project_key: project_key.clone(),
                    timestamp,
                    conversation_count: 1,
                    ..Default::default()
                });
            }
            continue;
        }
        if kind != "event_msg" {
            continue;
        }
        match payload
            .get("type")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
        {
            "user_message" => {
                let id = payload
                    .get("client_id")
                    .or_else(|| payload.get("turn_id"))
                    .and_then(JsonValue::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{session_id}:user:{index}"));
                events.push(UsageEvent {
                    id: format!("u:{id}"),
                    source: "codex".to_string(),
                    model: if current_model.is_empty() {
                        UNKNOWN_CODEX_MODEL.to_string()
                    } else {
                        current_model.clone()
                    },
                    project_key: project_key.clone(),
                    timestamp,
                    conversation_count: 1,
                    ..Default::default()
                });
            }
            "token_count" => {
                let info = payload.get("info").unwrap_or(&JsonValue::Null);
                let last_usage = info.get("last_token_usage").and_then(CodexUsage::from_json);
                let total_usage = info
                    .get("total_token_usage")
                    .and_then(CodexUsage::from_json);
                let signature = format!("{session_id}:{timestamp}:{last_usage:?}:{total_usage:?}");
                let Some(delta) = usage_state.consume(last_usage, total_usage) else {
                    continue;
                };
                if !seen_usage.insert(signature.clone()) {
                    continue;
                }
                let delta = delta.normalized();
                if delta.total_tokens <= 0 || timestamp.is_empty() {
                    continue;
                }
                let model = if current_model.is_empty() {
                    UNKNOWN_CODEX_MODEL.to_string()
                } else {
                    current_model.clone()
                };
                if session_model.is_empty() {
                    session_model = model.clone();
                }
                events.push(UsageEvent {
                    id: signature,
                    source: "codex".to_string(),
                    model,
                    project_key: project_key.clone(),
                    timestamp,
                    input_tokens: delta.input_tokens,
                    cached_input_tokens: delta.cached_input_tokens,
                    cache_creation_input_tokens: delta.cache_creation_input_tokens,
                    output_tokens: delta.output_tokens,
                    reasoning_output_tokens: delta.reasoning_output_tokens,
                    total_tokens: delta.total_tokens,
                    conversation_count: 0,
                    cost_usd: 0.0,
                    pricing_available: false,
                    estimated_tokens: 0,
                });
            }
            _ => {}
        }
    }

    if session_model.is_empty() {
        session_model = if current_model.is_empty() {
            UNKNOWN_CODEX_MODEL.to_string()
        } else {
            current_model
        };
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
    let turns = events.iter().map(|event| event.conversation_count).sum();
    let session = token_session(
        session_id,
        "codex",
        project_key,
        session_model,
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
