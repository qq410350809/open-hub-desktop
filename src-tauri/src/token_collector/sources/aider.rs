use crate::models::TokenSessionTokens;
use crate::token_collector::types::{
    fingerprint, token_session, CachedFile, UsageEvent,
};
use std::fs;
use std::path::{Path, PathBuf};

pub fn collect_aider_source_files(home: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();

    let user_aider_history = home.join(".aider.chat.history.md");
    if user_aider_history.is_file() {
        files.push(user_aider_history);
    }

    let user_aider_analytics = home.join(".aider").join("analytics.json");
    if user_aider_analytics.is_file() {
        files.push(user_aider_analytics);
    }

    files
}

pub fn parse_aider_file(path: &Path) -> CachedFile {
    let Ok(content) = fs::read_to_string(path) else {
        return CachedFile {
            fingerprint: fingerprint(path),
            ..Default::default()
        };
    };

    let session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("aider-session")
        .to_string();

    let project_key = "aider-workspace".to_string();
    let mut model_name = "claude-3-5-sonnet".to_string();

    let mut events = Vec::new();
    let first_ts = String::new();
    let last_ts = String::new();
    let mut asst_msg_count = 0i64;

    let mut total_in = 0i64;
    let mut total_out = 0i64;
    let mut total_cost = 0.0f64;

    // 解析 markdown 格式的聊天记录
    for (line_idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("# ") || trimmed.starts_with("#### ") {
            let possible_model = trimmed.trim_start_matches('#').trim();
            if !possible_model.is_empty() && (possible_model.contains('-') || possible_model.contains('/')) {
                model_name = possible_model.to_string();
            }
        }

        if trimmed.starts_with("> Tokens:") || trimmed.contains("Tokens:") {
            let lower = trimmed.to_ascii_lowercase();
            let mut in_tok = 0i64;
            let mut out_tok = 0i64;
            let mut cost = 0.0f64;

            if let Some(idx) = lower.find("sent") {
                let prefix = lower[..idx].trim();
                let num_str: String = prefix
                    .chars()
                    .rev()
                    .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == 'k')
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                if let Ok(val) = num_str.trim_end_matches('k').parse::<f64>() {
                    in_tok = if num_str.ends_with('k') {
                        (val * 1000.0) as i64
                    } else {
                        val as i64
                    };
                }
            }

            if let Some(idx) = lower.find("received") {
                let prefix = lower[..idx].trim();
                let num_str: String = prefix
                    .chars()
                    .rev()
                    .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == 'k')
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                if let Ok(val) = num_str.trim_end_matches('k').parse::<f64>() {
                    out_tok = if num_str.ends_with('k') {
                        (val * 1000.0) as i64
                    } else {
                        val as i64
                    };
                }
            }

            if let Some(idx) = lower.find("cost:") {
                let suffix = &lower[idx..];
                if let Some(dollar_idx) = suffix.find('$') {
                    let after_dollar = &suffix[dollar_idx + 1..];
                    let num_str: String = after_dollar
                        .chars()
                        .take_while(|c| c.is_ascii_digit() || *c == '.')
                        .collect();
                    if let Ok(val) = num_str.parse::<f64>() {
                        cost = val;
                    }
                }
            }

            if in_tok + out_tok > 0 {
                asst_msg_count += 1;
                let total = in_tok + out_tok;
                total_in += in_tok;
                total_out += out_tok;
                total_cost += cost;

                events.push(UsageEvent {
                    id: format!("aider_{session_id}_{line_idx}"),
                    source: "aider".to_string(),
                    model: model_name.clone(),
                    project_key: project_key.clone(),
                    timestamp: String::new(),
                    input_tokens: in_tok,
                    cached_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                    output_tokens: out_tok,
                    reasoning_output_tokens: 0,
                    total_tokens: total,
                    conversation_count: 0,
                    cost_usd: cost,
                    pricing_available: cost > 0.0,
                    estimated_tokens: 0,
                });
            }
        }
    }

    let mut sessions = Vec::new();
    let total_all = total_in + total_out;
    if total_all > 0 {
        sessions.push(token_session(
            session_id,
            "aider",
            project_key,
            model_name,
            first_ts,
            last_ts,
            asst_msg_count,
            TokenSessionTokens {
                input_tokens: total_in,
                cached_input_tokens: 0,
                cache_creation_input_tokens: 0,
                output_tokens: total_out,
                reasoning_output_tokens: 0,
                total_tokens: total_all,
            },
            total_cost,
        ));
    }

    CachedFile {
        fingerprint: fingerprint(path),
        events,
        sessions,
    }
}
