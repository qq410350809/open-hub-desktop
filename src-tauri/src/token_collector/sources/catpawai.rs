use crate::models::TokenSessionTokens;
use crate::token_collector::time_utils::update_bounds;
use crate::token_collector::types::{
    fingerprint, number, token_session, CachedFile, UsageEvent,
};
use serde_json::Value as JsonValue;
use std::fs;
use std::path::{Path, PathBuf};

pub fn collect_catpawai_source_files(home: &Path) -> Vec<(String, PathBuf)> {
    let mut files = Vec::new();

    let search_roots = [
        ("catpawai", home.join(".catpawai").join("logs")),
        ("openclaw", home.join(".openclaw").join("sessions")),
    ];

    for (source, dir) in search_roots {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("jsonl")
                    || path.extension().and_then(|e| e.to_str()) == Some("json")
                {
                    files.push((source.to_string(), path));
                }
            }
        }
    }

    files
}

pub fn parse_catpawai_file(source: &str, path: &Path) -> CachedFile {
    let Ok(content) = fs::read_to_string(path) else {
        return CachedFile {
            fingerprint: fingerprint(path),
            ..Default::default()
        };
    };

    let session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("catpawai-session")
        .to_string();

    let mut model_name = format!("{source}-model");
    let project_key = format!("{source}-workspace");
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
                    id: format!("{source}_{session_id}_{idx}"),
                    source: source.to_string(),
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
            source,
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
