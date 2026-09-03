use crate::models::TokenSessionTokens;
use crate::token::collector::time_utils::{iso_from_millis, update_bounds};
use crate::token::collector::types::{
    fingerprint, float_number, normalize_usage, number, token_session, CachedFile, InputSemantics,
    RawUsage, UsageEvent,
};
use serde_json::Value as JsonValue;
use std::fs;
use std::path::{Path, PathBuf};

pub fn collect_cline_source_files(home: &Path) -> Vec<(String, PathBuf)> {
    let mut files = Vec::new();
    let mut search_roots = Vec::new();

    #[cfg(target_os = "macos")]
    {
        let vscode_user = home
            .join("Library")
            .join("Application Support")
            .join("Code")
            .join("User")
            .join("globalStorage");
        search_roots.push((
            "cline",
            vscode_user.join("saoudrizwan.claude-dev").join("tasks"),
        ));
        search_roots.push((
            "roo-code",
            vscode_user.join("rooveterinaryinc.roo-cline").join("tasks"),
        ));
    }
    #[cfg(target_os = "windows")]
    {
        let vscode_user = home
            .join("AppData")
            .join("Roaming")
            .join("Code")
            .join("User")
            .join("globalStorage");
        search_roots.push((
            "cline",
            vscode_user.join("saoudrizwan.claude-dev").join("tasks"),
        ));
        search_roots.push((
            "roo-code",
            vscode_user.join("rooveterinaryinc.roo-cline").join("tasks"),
        ));
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let vscode_user = home
            .join(".config")
            .join("Code")
            .join("User")
            .join("globalStorage");
        search_roots.push((
            "cline",
            vscode_user.join("saoudrizwan.claude-dev").join("tasks"),
        ));
        search_roots.push((
            "roo-code",
            vscode_user.join("rooveterinaryinc.roo-cline").join("tasks"),
        ));
    }

    for (source_name, tasks_dir) in search_roots {
        if let Ok(entries) = fs::read_dir(tasks_dir) {
            for entry in entries.flatten() {
                let ui_msg = entry.path().join("ui_messages.json");
                if ui_msg.is_file() {
                    files.push((source_name.to_string(), ui_msg));
                }
            }
        }
    }

    files
}

pub fn parse_cline_file(source_name: &str, path: &Path) -> CachedFile {
    let Ok(content) = fs::read_to_string(path) else {
        return CachedFile {
            fingerprint: fingerprint(path),
            ..Default::default()
        };
    };

    let Ok(messages) = serde_json::from_str::<JsonValue>(&content) else {
        return CachedFile {
            fingerprint: fingerprint(path),
            ..Default::default()
        };
    };

    let task_id = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("cline-task")
        .to_string();

    let mut events = Vec::new();
    let mut model_name = "claude-3-7-sonnet".to_string();
    let project_key = "cline-task".to_string();
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut user_msg_count = 0i64;

    let mut total_in = 0i64;
    let mut total_cached = 0i64;
    let mut total_cache_write = 0i64;
    let mut total_out = 0i64;
    let mut total_all = 0i64;
    let mut total_cost = 0.0f64;

    if let Some(arr) = messages.as_array() {
        for (idx, msg) in arr.iter().enumerate() {
            let ts_ms = msg.get("ts").and_then(JsonValue::as_i64).unwrap_or(0);
            let iso_ts = if ts_ms > 0 {
                iso_from_millis(ts_ms)
            } else {
                String::new()
            };
            update_bounds(&mut first_ts, &mut last_ts, &iso_ts);

            if let Some(say) = msg.get("say").and_then(JsonValue::as_str) {
                if say == "user_feedback" || say == "task" {
                    user_msg_count += 1;
                }
            }

            if let Some(m) = msg
                .get("apiConfiguration")
                .and_then(|a| a.get("apiModelId"))
                .and_then(JsonValue::as_str)
            {
                if !m.is_empty() {
                    model_name = m.to_string();
                }
            }

            // 提取 token 计量
            if let Some(usage) = msg.get("tokens").or_else(|| msg.get("tokenUsage")) {
                let in_tok = number(usage, &["tokensIn", "inputTokens", "promptTokens"]);
                let out_tok = number(usage, &["tokensOut", "outputTokens", "completionTokens"]);
                let cache_read = number(
                    usage,
                    &["cacheReads", "cachedTokens", "cache_read_input_tokens"],
                );
                let cache_write = number(usage, &["cacheWrites", "cache_creation_input_tokens"]);
                let cost =
                    float_number(usage, &["totalCost", "cost"]).max(float_number(msg, &["cost"]));
                // Cline 的 tokensIn 为全新输入（Anthropic 口径，不含缓存），
                // total = 全新输入 + 缓存命中 + 输出；缓存写入独立上报，不计入 total。
                // 仅 totalTokens/tokens 可用时整体兜底（语义不明，标记估算）。
                let is_fallback = in_tok + out_tok + cache_read == 0;
                let (in_tok, cache_read, _cache_write, out_tok, _reasoning, total) =
                    if !is_fallback {
                        normalize_usage(RawUsage {
                            input: in_tok,
                            semantics: InputSemantics::Fresh,
                            cache_read,
                            cache_write,
                            output: out_tok,
                            ..Default::default()
                        })
                    } else {
                        let fallback = number(usage, &["totalTokens", "tokens"]);
                        (fallback, 0, 0, 0, 0, fallback)
                    };

                if total > 0 || cost > 0.0 {
                    total_in += in_tok;
                    total_cached += cache_read;
                    total_cache_write += cache_write;
                    total_out += out_tok;
                    total_all += total;
                    total_cost += cost;

                    events.push(UsageEvent {
                        id: format!("{source_name}_{task_id}_{idx}"),
                        source: source_name.to_string(),
                        model: model_name.clone(),
                        project_key: project_key.clone(),
                        timestamp: iso_ts,
                        input_tokens: in_tok,
                        cached_input_tokens: cache_read,
                        cache_creation_input_tokens: cache_write,
                        output_tokens: out_tok,
                        reasoning_output_tokens: 0,
                        total_tokens: total,
                        conversation_count: 0,
                        cost_usd: cost,
                        pricing_available: cost > 0.0,
                        estimated_tokens: if is_fallback { total } else { 0 },
                    });
                }
            }
        }
    }

    let mut sessions = Vec::new();
    if total_all > 0 || user_msg_count > 0 {
        sessions.push(token_session(
            task_id,
            source_name,
            project_key,
            model_name,
            first_ts,
            last_ts,
            user_msg_count,
            TokenSessionTokens {
                input_tokens: total_in,
                cached_input_tokens: total_cached,
                cache_creation_input_tokens: total_cache_write,
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
