use crate::models::TokenSessionTokens;
use crate::token_collector::time_utils::update_bounds;
use crate::token_collector::types::{
    fingerprint, number, token_session, CachedFile, UsageEvent,
};
use serde_json::Value as JsonValue;
use std::fs;
use std::path::{Path, PathBuf};

pub fn collect_goose_source_files(home: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut search_dirs = Vec::new();
    search_dirs.push(home.join(".local").join("share").join("goose").join("sessions"));
    search_dirs.push(home.join(".config").join("goose").join("sessions"));

    #[cfg(target_os = "macos")]
    search_dirs.push(home.join("Library").join("Application Support").join("goose").join("sessions"));

    for dir in search_dirs {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    files.push(path);
                }
            }
        }
    }

    files
}

pub fn parse_goose_file(path: &Path) -> CachedFile {
    let Ok(content) = fs::read_to_string(path) else {
        return CachedFile {
            fingerprint: fingerprint(path),
            ..Default::default()
        };
    };

    let session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("goose-session")
        .to_string();

    let mut model_name = "gpt-4o".to_string();
    let project_key = "goose-workspace".to_string();
    let mut events = Vec::new();
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut user_msg_count = 0i64;

    let mut total_in = 0i64;
    let mut total_out = 0i64;
    let mut total_all = 0i64;

    for (idx, line) in content.lines().enumerate() {
        let Ok(val) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };

        if let Some(m) = val.get("model").or_else(|| val.get("model_name")).and_then(JsonValue::as_str) {
            if !m.is_empty() {
                model_name = m.to_string();
            }
        }

        let role = val.get("role").and_then(JsonValue::as_str).unwrap_or("");
        if role == "user" {
            user_msg_count += 1;
        }

        if let Some(usage) = val.get("usage") {
            let in_tok = number(usage, &["prompt_tokens", "input_tokens", "promptTokens"]);
            let out_tok = number(usage, &["completion_tokens", "output_tokens", "completionTokens"]);
            let total = if in_tok + out_tok > 0 { in_tok + out_tok } else { number(usage, &["total_tokens", "tokens"]) };

            if total > 0 {
                let ts_str = val.get("timestamp").or_else(|| val.get("created_at")).and_then(JsonValue::as_str).unwrap_or("");
                let iso_ts = if !ts_str.is_empty() { ts_str.to_string() } else { String::new() };
                update_bounds(&mut first_ts, &mut last_ts, &iso_ts);

                total_in += in_tok;
                total_out += out_tok;
                total_all += total;

                events.push(UsageEvent {
                    id: format!("goose_{session_id}_{idx}"),
                    source: "goose".to_string(),
                    model: model_name.clone(),
                    project_key: project_key.clone(),
                    timestamp: iso_ts,
                    input_tokens: in_tok,
                    cached_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                    output_tokens: out_tok,
                    reasoning_output_tokens: 0,
                    total_tokens: total,
                    conversation_count: 0,
                    cost_usd: 0.0,
                    pricing_available: false,
                    estimated_tokens: 0,
                });
            }
        }
    }

    let mut sessions = Vec::new();
    if total_all > 0 || user_msg_count > 0 {
        sessions.push(token_session(
            session_id,
            "goose",
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
