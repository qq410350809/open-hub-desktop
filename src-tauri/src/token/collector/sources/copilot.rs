use crate::models::TokenSessionTokens;
use crate::token::collector::normalizer::vscode_workspace_project_from_path;
use crate::token::collector::time_utils::{iso_from_millis, update_bounds};
use crate::token::collector::types::{
    collect_jsonl_files, fingerprint, token_session, CachedFile, FileFingerprint, UsageEvent,
    LOCAL_ESTIMATED_CONTEXT_LIMIT, UNKNOWN_COPILOT_MODEL,
};
use serde_json::Value as JsonValue;
use std::fs;
use std::path::{Path, PathBuf};

pub fn normalize_copilot_model_name(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return UNKNOWN_COPILOT_MODEL.to_string();
    }
    if s.contains("@provider=") {
        let parts: Vec<&str> = s.split(':').collect();
        if let Some(last) = parts.last() {
            let last_clean = last.trim();
            if !last_clean.is_empty() {
                if last_clean == "sonnet" {
                    return "claude-3-7-sonnet".to_string();
                } else if last_clean == "fable" || last_clean == "haiku" {
                    return "claude-3-5-haiku".to_string();
                } else if last_clean == "opus" {
                    return "claude-3-opus".to_string();
                }
                return last_clean.to_string();
            }
        }
    }
    if let Some((_, model)) = s.split_once('/') {
        let model = model.trim();
        if !model.is_empty() {
            if model == "auto" {
                return UNKNOWN_COPILOT_MODEL.to_string();
            }
            return model.to_string();
        }
    }
    if s == "auto" || s == "copilotcli:auto" || s == "agent-host-copilotcli:auto" {
        return UNKNOWN_COPILOT_MODEL.to_string();
    }
    s.to_string()
}

pub fn collect_copilot_source_files(home: &Path) -> Vec<(String, PathBuf)> {
    let mut files = Vec::new();
    let mut base_dirs = Vec::new();

    #[cfg(target_os = "macos")]
    {
        let app_support = home.join("Library").join("Application Support");
        base_dirs.push(app_support.join("Code").join("User"));
        base_dirs.push(app_support.join("Code - Insiders").join("User"));
        base_dirs.push(app_support.join("VSCodium").join("User"));
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
            base_dirs.push(appdata.join("Code").join("User"));
            base_dirs.push(appdata.join("Code - Insiders").join("User"));
            base_dirs.push(appdata.join("VSCodium").join("User"));
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let config = home.join(".config");
        base_dirs.push(config.join("Code").join("User"));
        base_dirs.push(config.join("Code - Insiders").join("User"));
        base_dirs.push(config.join("VSCodium").join("User"));
    }

    for user_dir in base_dirs {
        if !user_dir.is_dir() {
            continue;
        }

        let empty_window_sessions = user_dir
            .join("globalStorage")
            .join("emptyWindowChatSessions");
        if empty_window_sessions.is_dir() {
            collect_jsonl_files(
                &empty_window_sessions,
                &|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"),
                &mut files,
            );
        }

        let workspace_storage = user_dir.join("workspaceStorage");
        if workspace_storage.is_dir() {
            if let Ok(entries) = fs::read_dir(&workspace_storage) {
                for entry in entries.flatten() {
                    let chat_sessions = entry.path().join("chatSessions");
                    if chat_sessions.is_dir() {
                        collect_jsonl_files(
                            &chat_sessions,
                            &|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"),
                            &mut files,
                        );
                    }
                }
            }
        }
    }

    let copilot_cli_root = home.join(".copilot").join("session-state");
    if copilot_cli_root.is_dir() {
        collect_jsonl_files(
            &copilot_cli_root,
            &|path| path.file_name().and_then(|n| n.to_str()) == Some("events.jsonl"),
            &mut files,
        );
    }

    files
        .into_iter()
        .map(|path| ("copilot".to_string(), path))
        .collect()
}

pub fn parse_vscode_chat_session(
    path: &Path,
    text: &str,
    file_fingerprint: FileFingerprint,
) -> CachedFile {
    let mut session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("copilot-session")
        .to_string();
    let mut session_creation_ts = String::new();
    let project_key = vscode_workspace_project_from_path(path);
    let mut model_fallback = UNKNOWN_COPILOT_MODEL.to_string();

    let mut events = Vec::<UsageEvent>::new();
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut turns = 0i64;

    for line in text.lines() {
        let Ok(root_val) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };

        let v = root_val.get("v").unwrap_or(&root_val);

        if let Some(sid) = v.get("sessionId").and_then(JsonValue::as_str) {
            if !sid.is_empty() {
                session_id = sid.to_string();
            }
        }
        if let Some(ms) = v.get("creationDate").and_then(JsonValue::as_i64) {
            session_creation_ts = iso_from_millis(ms);
        }

        if let Some(selected) = v.get("inputState").and_then(|is| is.get("selectedModel")) {
            if let Some(raw_id) = selected.get("identifier").and_then(JsonValue::as_str) {
                let m = normalize_copilot_model_name(raw_id);
                if m != UNKNOWN_COPILOT_MODEL {
                    model_fallback = m;
                }
            }
        }

        let Some(requests) = v.get("requests").and_then(JsonValue::as_array) else {
            continue;
        };

        for (req_idx, req) in requests.iter().enumerate() {
            let req_id = req
                .get("requestId")
                .and_then(JsonValue::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("{session_id}:req:{req_idx}"));

            let mut timestamp = req
                .get("modelState")
                .and_then(|ms| ms.get("completedAt"))
                .and_then(JsonValue::as_i64)
                .map(iso_from_millis)
                .or_else(|| {
                    req.get("timeSpentWaiting")
                        .and_then(JsonValue::as_i64)
                        .map(iso_from_millis)
                })
                .unwrap_or_default();
            if timestamp.is_empty() {
                timestamp = session_creation_ts.clone();
            }
            update_bounds(&mut first_ts, &mut last_ts, &timestamp);

            let mut req_model = req
                .get("result")
                .and_then(|res| res.get("metadata"))
                .and_then(|meta| meta.get("resolvedModel"))
                .and_then(JsonValue::as_str)
                .map(normalize_copilot_model_name)
                .unwrap_or_else(|| UNKNOWN_COPILOT_MODEL.to_string());

            if req_model == UNKNOWN_COPILOT_MODEL {
                if let Some(m) = req.get("modelId").and_then(JsonValue::as_str) {
                    let clean = normalize_copilot_model_name(m);
                    if clean != UNKNOWN_COPILOT_MODEL {
                        req_model = clean;
                    }
                }
            }
            if req_model == UNKNOWN_COPILOT_MODEL {
                if let Some(m) = req.get("usedModel").and_then(JsonValue::as_str) {
                    let clean = normalize_copilot_model_name(m);
                    if clean != UNKNOWN_COPILOT_MODEL {
                        req_model = clean;
                    }
                }
            }
            if req_model == UNKNOWN_COPILOT_MODEL {
                req_model = model_fallback.clone();
            }

            let user_text = req
                .get("message")
                .and_then(|m| m.get("text"))
                .and_then(JsonValue::as_str)
                .unwrap_or("");
            if !user_text.trim().is_empty() {
                turns += 1;
                events.push(UsageEvent {
                    id: format!("u:{req_id}"),
                    source: "copilot".to_string(),
                    model: req_model.clone(),
                    project_key: project_key.clone(),
                    timestamp: timestamp.clone(),
                    conversation_count: 1,
                    ..Default::default()
                });
            }

            let mut prompt_tokens = req
                .get("promptTokens")
                .and_then(JsonValue::as_i64)
                .or_else(|| {
                    req.get("result")
                        .and_then(|res| res.get("metadata"))
                        .and_then(|meta| meta.get("promptTokens"))
                        .and_then(JsonValue::as_i64)
                })
                .unwrap_or(0);

            let mut output_tokens = req
                .get("completionTokens")
                .or_else(|| req.get("outputTokens"))
                .and_then(JsonValue::as_i64)
                .or_else(|| {
                    req.get("result")
                        .and_then(|res| res.get("metadata"))
                        .and_then(|meta| meta.get("outputTokens"))
                        .and_then(JsonValue::as_i64)
                })
                .unwrap_or(0);

            let cached_tokens = req
                .get("cachedTokens")
                .and_then(JsonValue::as_i64)
                .unwrap_or(0);

            let mut reasoning_tokens = 0i64;
            if let Some(resp_arr) = req.get("response").and_then(JsonValue::as_array) {
                for item in resp_arr {
                    if item.get("kind").and_then(JsonValue::as_str) == Some("thinking") {
                        let text_len = item
                            .get("value")
                            .and_then(JsonValue::as_str)
                            .unwrap_or("")
                            .len();
                        reasoning_tokens += (text_len as i64 / 4).max(1);
                    }
                }
            }

            let mut estimated_tokens = 0i64;
            if prompt_tokens == 0 && output_tokens == 0 {
                let user_tokens = (user_text.len() as i64 / 4).max(1);
                prompt_tokens = user_tokens + 128;
                let mut resp_text_len = 0usize;
                if let Some(resp_arr) = req.get("response").and_then(JsonValue::as_array) {
                    for item in resp_arr {
                        if let Some(v) = item.get("value").and_then(JsonValue::as_str) {
                            resp_text_len += v.len();
                        }
                    }
                }
                output_tokens = (resp_text_len as i64 / 4).max(1);
                estimated_tokens = prompt_tokens + output_tokens + reasoning_tokens;
            }

            let total_tokens = prompt_tokens
                .saturating_add(output_tokens)
                .saturating_add(reasoning_tokens);

            if total_tokens > 0 || !user_text.is_empty() {
                events.push(UsageEvent {
                    id: req_id,
                    source: "copilot".to_string(),
                    model: req_model,
                    project_key: project_key.clone(),
                    timestamp,
                    input_tokens: prompt_tokens,
                    cached_input_tokens: cached_tokens,
                    cache_creation_input_tokens: 0,
                    output_tokens,
                    reasoning_output_tokens: reasoning_tokens,
                    total_tokens,
                    conversation_count: 0,
                    cost_usd: 0.0,
                    pricing_available: false,
                    estimated_tokens,
                });
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

    let session = token_session(
        session_id,
        "copilot",
        project_key,
        model_fallback,
        first_ts,
        last_ts,
        turns,
        tokens,
        0.0,
    );

    CachedFile {
        fingerprint: file_fingerprint,
        events,
        sessions: vec![session],
    }
}

pub fn parse_copilot_cli_events(
    path: &Path,
    text: &str,
    file_fingerprint: FileFingerprint,
) -> CachedFile {
    let session_id = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("copilot-cli-session")
        .to_string();

    let project_key = "Copilot CLI".to_string();
    let mut model = UNKNOWN_COPILOT_MODEL.to_string();
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut turns = 0i64;
    let mut events = Vec::<UsageEvent>::new();
    let mut visible_context_tokens = 0i64;

    for (index, line) in text.lines().enumerate() {
        let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };
        let event_type = value.get("type").and_then(JsonValue::as_str).unwrap_or("");
        let ts = value
            .get("timestamp")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        update_bounds(&mut first_ts, &mut last_ts, &ts);

        let data = value.get("data").unwrap_or(&JsonValue::Null);

        match event_type {
            "session.start" => {
                if let Some(m) = data.get("selectedModel").and_then(JsonValue::as_str) {
                    let clean = normalize_copilot_model_name(m);
                    if clean != UNKNOWN_COPILOT_MODEL {
                        model = clean;
                    }
                }
            }
            "user.message" => {
                let content = data
                    .get("content")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("");
                if !content.trim().is_empty() {
                    turns += 1;
                    let event_id = value.get("id").and_then(JsonValue::as_str).unwrap_or("");
                    events.push(UsageEvent {
                        id: format!(
                            "u:{session_id}:{}",
                            if event_id.is_empty() {
                                index.to_string()
                            } else {
                                event_id.to_string()
                            }
                        ),
                        source: "copilot".to_string(),
                        model: model.clone(),
                        project_key: project_key.clone(),
                        timestamp: ts.clone(),
                        conversation_count: 1,
                        ..Default::default()
                    });
                    visible_context_tokens += (content.len() as i64 / 4).max(1);
                }
            }
            "assistant.message" => {
                let req_id = data
                    .get("requestId")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("");
                let event_id = if !req_id.is_empty() {
                    format!("{session_id}:{req_id}")
                } else {
                    format!("{session_id}:{index}")
                };

                let output_tokens = data
                    .get("outputTokens")
                    .and_then(JsonValue::as_i64)
                    .unwrap_or_else(|| {
                        let content = data
                            .get("content")
                            .and_then(JsonValue::as_str)
                            .unwrap_or("");
                        (content.len() as i64 / 4).max(1)
                    });

                let reasoning_tokens = data
                    .get("reasoningText")
                    .and_then(JsonValue::as_str)
                    .map(|r| (r.len() as i64 / 4).max(1))
                    .unwrap_or(0);
                let input_tokens = visible_context_tokens
                    .saturating_add(64)
                    .min(LOCAL_ESTIMATED_CONTEXT_LIMIT);
                let total_tokens = input_tokens
                    .saturating_add(output_tokens)
                    .saturating_add(reasoning_tokens);

                events.push(UsageEvent {
                    id: event_id,
                    source: "copilot".to_string(),
                    model: model.clone(),
                    project_key: project_key.clone(),
                    timestamp: ts,
                    input_tokens,
                    cached_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                    output_tokens,
                    reasoning_output_tokens: reasoning_tokens,
                    total_tokens,
                    conversation_count: 0,
                    cost_usd: 0.0,
                    pricing_available: false,
                    estimated_tokens: total_tokens,
                });
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

    let session = token_session(
        session_id.clone(),
        "copilot",
        project_key,
        model,
        first_ts,
        last_ts,
        turns,
        tokens,
        0.0,
    );

    CachedFile {
        fingerprint: file_fingerprint,
        events,
        sessions: vec![session],
    }
}

pub fn parse_copilot_file(path: &Path) -> CachedFile {
    let file_fingerprint = fingerprint(path);
    let Ok(text) = fs::read_to_string(path) else {
        return CachedFile {
            fingerprint: file_fingerprint,
            ..Default::default()
        };
    };

    let is_cli_events = path.file_name().and_then(|n| n.to_str()) == Some("events.jsonl");
    if is_cli_events {
        parse_copilot_cli_events(path, &text, file_fingerprint)
    } else {
        parse_vscode_chat_session(path, &text, file_fingerprint)
    }
}
