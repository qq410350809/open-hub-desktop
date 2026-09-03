use crate::models::TokenSessionTokens;
use crate::token::collector::normalizer::normalize_workspace_project_key;
use crate::token::collector::time_utils::update_bounds;
use crate::token::collector::types::{
    fingerprint, normalize_usage, number, openai_cached_from_details, token_session, CachedFile,
    InputSemantics, RawUsage, UsageEvent,
};
use serde_json::Value as JsonValue;
use std::fs;
use std::path::{Path, PathBuf};

pub fn collect_continue_source_files(home: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let sessions_dir = home.join(".continue").join("sessions");
    if let Ok(entries) = fs::read_dir(sessions_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                files.push(path);
            }
        }
    }
    files
}

pub fn parse_continue_file(path: &Path) -> CachedFile {
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
        .get("sessionId")
        .and_then(JsonValue::as_str)
        .or_else(|| path.file_stem().and_then(|s| s.to_str()))
        .unwrap_or("continue-session")
        .to_string();

    let mut model_name = data
        .get("modelTitle")
        .or_else(|| data.get("model"))
        .and_then(JsonValue::as_str)
        .unwrap_or("continue-model")
        .to_string();

    let mut project_key = "continue-project".to_string();
    if let Some(workspace) = data.get("workspaceDirectory").and_then(JsonValue::as_str) {
        project_key = normalize_workspace_project_key(workspace, "continue-project");
    }

    let mut events = Vec::new();
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut user_msg_count = 0i64;

    let mut total_in = 0i64;
    let mut total_cached = 0i64;
    let mut total_out = 0i64;
    let mut total_all = 0i64;

    if let Some(history) = data.get("history").and_then(JsonValue::as_array) {
        for (idx, item) in history.iter().enumerate() {
            let role = item
                .get("role")
                .or_else(|| item.get("message").and_then(|m| m.get("role")))
                .and_then(JsonValue::as_str)
                .unwrap_or("");
            let content = item
                .get("content")
                .or_else(|| item.get("message").and_then(|m| m.get("content")));

            if let Some(m) = item
                .get("model")
                .or_else(|| item.get("modelTitle"))
                .and_then(JsonValue::as_str)
            {
                if !m.is_empty() {
                    model_name = m.to_string();
                }
            }

            if role == "user" {
                user_msg_count += 1;
            }

            // Continue 会在 promptLogs / contextItems 里带 token 或者直接带 promptTokens
            let prompt_tok = number(item, &["promptTokens", "inputTokens", "prompt_tokens"]);
            let comp_tok = number(
                item,
                &["completionTokens", "outputTokens", "completion_tokens"],
            );
            // OpenAI 式 promptTokens 已含缓存命中；按明细拆分全新输入。
            let cached_tok = openai_cached_from_details(item);
            let (fresh, cached_read, _write, out, _reasoning, total) = if prompt_tok + comp_tok > 0
            {
                normalize_usage(RawUsage {
                    input: prompt_tok,
                    semantics: InputSemantics::InclusiveOfCacheRead,
                    cache_read: cached_tok,
                    output: comp_tok,
                    ..Default::default()
                })
            } else {
                // 如果没有精准计量，根据内容字数估算 (4 字符 ≈ 1 token)
                let text_len = match content {
                    Some(JsonValue::String(s)) => s.len(),
                    _ => 0,
                };
                let estimated = if text_len > 0 {
                    ((text_len / 4).max(1)) as i64
                } else {
                    0
                };
                (estimated, 0, 0, 0, 0, estimated)
            };
            let is_estimated = prompt_tok + comp_tok == 0;

            if total > 0 {
                let ts_str = item
                    .get("timestamp")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("");
                let iso_ts = if !ts_str.is_empty() {
                    ts_str.to_string()
                } else {
                    String::new()
                };
                update_bounds(&mut first_ts, &mut last_ts, &iso_ts);

                total_in += fresh;
                total_cached += cached_read;
                total_out += out;
                total_all += total;

                events.push(UsageEvent {
                    id: format!("continue_{session_id}_{idx}"),
                    source: "continue".to_string(),
                    model: model_name.clone(),
                    project_key: project_key.clone(),
                    timestamp: iso_ts,
                    input_tokens: fresh,
                    cached_input_tokens: cached_read,
                    cache_creation_input_tokens: 0,
                    output_tokens: out,
                    reasoning_output_tokens: 0,
                    total_tokens: total,
                    conversation_count: 0,
                    cost_usd: 0.0,
                    pricing_available: false,
                    estimated_tokens: if is_estimated { total } else { 0 },
                });
            }
        }
    }

    let mut sessions = Vec::new();
    if total_all > 0 || user_msg_count > 0 {
        sessions.push(token_session(
            session_id,
            "continue",
            project_key,
            model_name,
            first_ts,
            last_ts,
            user_msg_count,
            TokenSessionTokens {
                input_tokens: total_in,
                cached_input_tokens: total_cached,
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
