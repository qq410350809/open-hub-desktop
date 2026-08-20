use crate::models::TokenSessionTokens;
use crate::token_collector::types::{
    fingerprint, token_session, CachedFile, UsageEvent,
};
use serde_json::Value as JsonValue;
use std::fs;
use std::path::{Path, PathBuf};

pub fn collect_zed_source_files(home: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();

    #[cfg(target_os = "macos")]
    let base = home.join("Library").join("Application Support").join("Zed").join("conversations");
    #[cfg(not(target_os = "macos"))]
    let base = home.join(".local").join("share").join("zed").join("conversations");

    if let Ok(entries) = fs::read_dir(base) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                files.push(path);
            }
        }
    }

    files
}

pub fn parse_zed_file(path: &Path) -> CachedFile {
    let Ok(content) = fs::read_to_string(path) else {
        return CachedFile {
            fingerprint: fingerprint(path),
            ..Default::default()
        };
    };

    let Ok(data) = serde_json::from_str::<JsonValue>(&content) else {
        return CachedFile {
            fingerprint: fingerprint(path),
            ..Default::default()
        };
    };

    let session_id = data
        .get("id")
        .and_then(JsonValue::as_str)
        .or_else(|| path.file_stem().and_then(|s| s.to_str()))
        .unwrap_or("zed-session")
        .to_string();

    let model_name = data
        .get("model")
        .and_then(JsonValue::as_str)
        .unwrap_or("claude-3-5-sonnet")
        .to_string();

    let project_key = "zed-workspace".to_string();
    let mut events = Vec::new();
    let first_ts = String::new();
    let last_ts = String::new();
    let mut user_msg_count = 0i64;

    let mut total_in = 0i64;
    let mut total_out = 0i64;
    let mut total_all = 0i64;

    if let Some(messages) = data.get("messages").and_then(JsonValue::as_array) {
        for (idx, msg) in messages.iter().enumerate() {
            let role = msg.get("role").and_then(JsonValue::as_str).unwrap_or("");
            let text = msg.get("text").or_else(|| msg.get("body")).and_then(JsonValue::as_str).unwrap_or("");
            let text_tokens = (text.len() / 4).max(1) as i64;

            if role == "user" {
                user_msg_count += 1;
                total_in += text_tokens;
            } else if role == "assistant" {
                total_out += text_tokens;
                let total = text_tokens;
                total_all += total;

                events.push(UsageEvent {
                    id: format!("zed_{session_id}_{idx}"),
                    source: "zed".to_string(),
                    model: model_name.clone(),
                    project_key: project_key.clone(),
                    timestamp: String::new(),
                    input_tokens: (text.len() / 6).max(1) as i64,
                    cached_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                    output_tokens: text_tokens,
                    reasoning_output_tokens: 0,
                    total_tokens: total,
                    conversation_count: 0,
                    cost_usd: 0.0,
                    pricing_available: false,
                    estimated_tokens: total,
                });
            }
        }
    }

    let mut sessions = Vec::new();
    if total_all > 0 || user_msg_count > 0 {
        sessions.push(token_session(
            session_id,
            "zed",
            project_key,
            model_name,
            first_ts,
            last_ts,
            user_msg_count,
            TokenSessionTokens {
                input_tokens: total_in,
                cached_input_tokens: 0,
                cache_creation_input_tokens: 0,
                output_tokens: total_out,
                reasoning_output_tokens: 0,
                total_tokens: total_all,
            },
            0.0,
        ));
    }

    CachedFile {
        fingerprint: fingerprint(path),
        events,
        sessions,
    }
}
