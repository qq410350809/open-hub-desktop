use crate::models::TokenSessionTokens;
use crate::token::collector::normalizer::basename_or_fallback;
use crate::token::collector::sources::claude::claude_user_is_human;
use crate::token::collector::sources::commandcode::estimate_local_content_tokens;
use crate::token::collector::time_utils::{iso_from_millis, update_bounds};
use crate::token::collector::types::{
    collect_jsonl_files, fingerprint, token_session, CachedFile, FileFingerprint, UsageEvent,
    LOCAL_ESTIMATED_CONTEXT_LIMIT, UNKNOWN_KIRO_MODEL,
};
use serde_json::{json, Value as JsonValue};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub fn kiro_session_metadata_path(path: &Path) -> PathBuf {
    path.parent()
        .map(|parent| parent.join("session.json"))
        .unwrap_or_else(|| path.with_file_name("session.json"))
}

pub fn kiro_fingerprint(path: &Path) -> FileFingerprint {
    let transcript = fingerprint(path);
    let metadata = fingerprint(&kiro_session_metadata_path(path));
    FileFingerprint {
        size: transcript.size.saturating_add(metadata.size),
        modified_ms: transcript.modified_ms.max(metadata.modified_ms),
    }
}

pub fn kiro_v2_session_root(home: &Path) -> PathBuf {
    home.join(".kiro").join("sessions")
}

pub fn kiro_legacy_session_roots(home: &Path) -> Vec<PathBuf> {
    let mut storage_roots = Vec::new();
    #[cfg(target_os = "macos")]
    {
        let global_storage = home
            .join("Library")
            .join("Application Support")
            .join("Kiro")
            .join("User")
            .join("globalStorage");
        storage_roots.push(global_storage.join("kiro.kiroagent"));
        storage_roots.push(global_storage.join("kiro.kiro-agent"));
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(app_data) = std::env::var_os("APPDATA") {
            let global_storage = PathBuf::from(app_data)
                .join("Kiro")
                .join("User")
                .join("globalStorage");
            storage_roots.push(global_storage.join("kiro.kiroagent"));
            storage_roots.push(global_storage.join("kiro.kiro-agent"));
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let global_storage = home
            .join(".config")
            .join("Kiro")
            .join("User")
            .join("globalStorage");
        storage_roots.push(global_storage.join("kiro.kiroagent"));
        storage_roots.push(global_storage.join("kiro.kiro-agent"));
    }

    storage_roots
        .into_iter()
        .flat_map(|root| [root.join("workspace-sessions"), root.join("sessions")])
        .collect()
}

pub fn kiro_v2_session_id(path: &Path) -> Option<String> {
    fs::read_to_string(kiro_session_metadata_path(path))
        .ok()
        .and_then(|text| serde_json::from_str::<JsonValue>(&text).ok())
        .and_then(|metadata| {
            metadata
                .get("id")
                .and_then(JsonValue::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            path.parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
}

pub fn kiro_legacy_session_id(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub fn normalized_kiro_session_id(value: &str) -> &str {
    let value = value.trim();
    value
        .strip_prefix("sess_")
        .or_else(|| value.strip_prefix("sess-"))
        .unwrap_or(value)
}

pub fn is_kiro_legacy_session_file(path: &Path) -> bool {
    path.extension().and_then(|value| value.to_str()) == Some("json")
        && path
            .file_name()
            .and_then(|value| value.to_str())
            .map(|name| {
                name != "sessions.json"
                    && !name.starts_with("._migration-")
                    && !name.starts_with(".migrated-")
            })
            .unwrap_or(false)
}

pub fn collect_kiro_source_files(home: &Path) -> Vec<(String, PathBuf)> {
    let mut v2_files = Vec::new();
    collect_jsonl_files(
        &kiro_v2_session_root(home),
        &|path| path.file_name().and_then(|name| name.to_str()) == Some("messages.jsonl"),
        &mut v2_files,
    );
    let migrated_ids = v2_files
        .iter()
        .filter_map(|path| kiro_v2_session_id(path))
        .map(|id| normalized_kiro_session_id(&id).to_string())
        .collect::<HashSet<_>>();

    let mut legacy_files = Vec::new();
    for root in kiro_legacy_session_roots(home) {
        collect_jsonl_files(&root, &is_kiro_legacy_session_file, &mut legacy_files);
    }
    legacy_files.retain(|path| {
        kiro_legacy_session_id(path)
            .map(|id| !migrated_ids.contains(normalized_kiro_session_id(&id)))
            .unwrap_or(false)
    });

    v2_files
        .into_iter()
        .map(|path| ("kiro".to_string(), path))
        .chain(
            legacy_files
                .into_iter()
                .map(|path| ("kiro-legacy".to_string(), path)),
        )
        .collect()
}

pub fn kiro_session_id(path: &Path, metadata: &JsonValue) -> String {
    metadata
        .get("id")
        .and_then(JsonValue::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            path.parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            path.file_stem()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "kiro-session".to_string())
}

pub fn kiro_project_from_metadata(metadata: &JsonValue) -> String {
    metadata
        .get("workspacePaths")
        .and_then(JsonValue::as_array)
        .and_then(|paths| paths.iter().find_map(JsonValue::as_str))
        .map(|path| basename_or_fallback(path, "Kiro"))
        .unwrap_or_else(|| "Kiro".to_string())
}

pub fn kiro_model_from_metadata(metadata: &JsonValue) -> String {
    metadata
        .get("modelId")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| UNKNOWN_KIRO_MODEL.to_string())
}

pub fn parse_kiro_file(path: &Path) -> CachedFile {
    let file_fingerprint = kiro_fingerprint(path);
    let metadata = fs::read_to_string(kiro_session_metadata_path(path))
        .ok()
        .and_then(|text| serde_json::from_str::<JsonValue>(&text).ok())
        .unwrap_or(JsonValue::Null);
    let session_id = kiro_session_id(path, &metadata);
    let project_key = kiro_project_from_metadata(&metadata);
    let model = kiro_model_from_metadata(&metadata);
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut visible_context_tokens = 0i64;
    let mut turns = 0i64;
    let mut assistant_responses = 0i64;
    let mut events = Vec::<UsageEvent>::new();

    let Ok(text) = fs::read_to_string(path) else {
        return CachedFile {
            fingerprint: file_fingerprint,
            ..Default::default()
        };
    };
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
        let payload = value.get("payload").unwrap_or(&JsonValue::Null);
        let payload_type = payload
            .get("type")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        let event_id = value
            .get("id")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{session_id}:{payload_type}:{index}"));

        match payload_type {
            "user" => {
                let content = payload.get("content").unwrap_or(&JsonValue::Null);
                let content_tokens = estimate_local_content_tokens(content);
                if !claude_user_is_human(content) {
                    continue;
                }
                turns += 1;
                events.push(UsageEvent {
                    id: format!("u:{event_id}"),
                    source: "kiro".to_string(),
                    model: model.clone(),
                    project_key: project_key.clone(),
                    timestamp,
                    conversation_count: 1,
                    ..Default::default()
                });
                visible_context_tokens = visible_context_tokens.saturating_add(content_tokens);
            }
            "assistant" => {
                assistant_responses += 1;
                let content = payload.get("content").unwrap_or(&JsonValue::Null);
                let output_tokens = estimate_local_content_tokens(content);
                let input_tokens = visible_context_tokens
                    .saturating_add(32)
                    .min(LOCAL_ESTIMATED_CONTEXT_LIMIT);
                let total_tokens = input_tokens.saturating_add(output_tokens);
                events.push(UsageEvent {
                    id: event_id,
                    source: "kiro".to_string(),
                    model: model.clone(),
                    project_key: project_key.clone(),
                    timestamp,
                    input_tokens,
                    output_tokens,
                    total_tokens,
                    estimated_tokens: total_tokens,
                    ..Default::default()
                });
                visible_context_tokens = visible_context_tokens.saturating_add(output_tokens);
            }
            "tool_call" | "tool_result" => {
                visible_context_tokens =
                    visible_context_tokens.saturating_add(estimate_local_content_tokens(payload));
            }
            _ => {}
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
        "kiro",
        project_key,
        model,
        first_ts,
        last_ts,
        turns,
        tokens,
        0.0,
    );
    session.productive = turns > 0 && assistant_responses > 0;
    session.provenance = json!({
        "source": "openhub-local-collector",
        "confidence": "estimated",
        "privacy": "metadata-only",
        "tokenUsage": "estimated-kiro-local-context",
        "assistantResponses": assistant_responses,
        "estimationMethod": "visible-context-chars-v1",
        "estimatedContextLimit": LOCAL_ESTIMATED_CONTEXT_LIMIT,
        "modelId": metadata.get("modelId").cloned().unwrap_or(JsonValue::Null)
    });
    CachedFile {
        fingerprint: file_fingerprint,
        events,
        sessions: vec![session],
    }
}

pub fn json_timestamp(value: &JsonValue) -> String {
    for key in ["timestamp", "createdAt", "created_at", "time", "date"] {
        let Some(raw) = value.get(key) else {
            continue;
        };
        if let Some(timestamp) = raw
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return timestamp.to_string();
        }
        let millis = raw
            .as_i64()
            .or_else(|| raw.as_u64().map(|value| value.min(i64::MAX as u64) as i64));
        if let Some(value) = millis {
            let millis = if value > 0 && value < 10_000_000_000 {
                value.saturating_mul(1_000)
            } else {
                value
            };
            return iso_from_millis(millis);
        }
    }
    String::new()
}

pub fn parse_kiro_legacy_file(path: &Path) -> CachedFile {
    let file_fingerprint = fingerprint(path);
    let Ok(text) = fs::read_to_string(path) else {
        return CachedFile {
            fingerprint: file_fingerprint,
            ..Default::default()
        };
    };
    let Ok(root) = serde_json::from_str::<JsonValue>(&text) else {
        return CachedFile {
            fingerprint: file_fingerprint,
            ..Default::default()
        };
    };
    let session_id = root
        .get("sessionId")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| kiro_legacy_session_id(path))
        .unwrap_or_else(|| "kiro-v1-session".to_string());
    let project_key = ["workspaceDirectory", "workspacePath", "cwd"]
        .into_iter()
        .find_map(|key| root.get(key).and_then(JsonValue::as_str))
        .map(|path| basename_or_fallback(path, "Kiro"))
        .unwrap_or_else(|| "Kiro".to_string());
    let model = ["modelId", "selectedModel"]
        .into_iter()
        .find_map(|key| root.get(key).and_then(JsonValue::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| UNKNOWN_KIRO_MODEL.to_string());
    let fallback_timestamp = {
        let timestamp = json_timestamp(&root);
        if timestamp.is_empty() {
            iso_from_millis(file_fingerprint.modified_ms.min(i64::MAX as u64) as i64)
        } else {
            timestamp
        }
    };
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut visible_context_tokens = 0i64;
    let mut turns = 0i64;
    let mut assistant_responses = 0i64;
    let mut events = Vec::<UsageEvent>::new();

    for (index, item) in root
        .get("history")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let message = item.get("message").unwrap_or(item);
        let role = message
            .get("role")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        let content = message.get("content").unwrap_or(&JsonValue::Null);
        let timestamp = {
            let item_timestamp = json_timestamp(item);
            if item_timestamp.is_empty() {
                let message_timestamp = json_timestamp(message);
                if message_timestamp.is_empty() {
                    fallback_timestamp.clone()
                } else {
                    message_timestamp
                }
            } else {
                item_timestamp
            }
        };
        update_bounds(&mut first_ts, &mut last_ts, &timestamp);
        let event_id = format!("{session_id}:v1:{index}:{role}");
        match role {
            "user" => {
                if !claude_user_is_human(content) {
                    continue;
                }
                turns += 1;
                events.push(UsageEvent {
                    id: format!("u:{event_id}"),
                    source: "kiro".to_string(),
                    model: model.clone(),
                    project_key: project_key.clone(),
                    timestamp,
                    conversation_count: 1,
                    ..Default::default()
                });
                visible_context_tokens =
                    visible_context_tokens.saturating_add(estimate_local_content_tokens(content));
            }
            "assistant" => {
                assistant_responses += 1;
                let output_tokens = estimate_local_content_tokens(content);
                let input_tokens = visible_context_tokens
                    .saturating_add(32)
                    .min(LOCAL_ESTIMATED_CONTEXT_LIMIT);
                let total_tokens = input_tokens.saturating_add(output_tokens);
                events.push(UsageEvent {
                    id: event_id,
                    source: "kiro".to_string(),
                    model: model.clone(),
                    project_key: project_key.clone(),
                    timestamp,
                    input_tokens,
                    output_tokens,
                    total_tokens,
                    estimated_tokens: total_tokens,
                    ..Default::default()
                });
                visible_context_tokens = visible_context_tokens.saturating_add(output_tokens);
            }
            "system" => {
                visible_context_tokens =
                    visible_context_tokens.saturating_add(estimate_local_content_tokens(content));
            }
            _ => {}
        }
    }

    let tokens = events
        .iter()
        .fold(TokenSessionTokens::default(), |mut total, event| {
            total.input_tokens += event.input_tokens;
            total.output_tokens += event.output_tokens;
            total.total_tokens += event.total_tokens;
            total
        });
    let mut session = token_session(
        session_id,
        "kiro",
        project_key,
        model,
        first_ts,
        last_ts,
        turns,
        tokens,
        0.0,
    );
    session.productive = turns > 0 && assistant_responses > 0;
    session.provenance = json!({
        "source": "openhub-local-collector",
        "confidence": "estimated",
        "privacy": "metadata-only",
        "tokenUsage": "estimated-kiro-v1-local-context",
        "storageFormat": "kiro-global-storage-v1",
        "assistantResponses": assistant_responses
    });
    CachedFile {
        fingerprint: file_fingerprint,
        events,
        sessions: vec![session],
    }
}
