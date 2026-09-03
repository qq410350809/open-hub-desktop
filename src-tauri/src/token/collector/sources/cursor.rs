use crate::models::TokenSessionTokens;
use crate::token::collector::time_utils::{iso_from_millis, update_bounds};
use crate::token::collector::types::{
    database_fingerprint, normalize_usage, number, open_readonly_sqlite,
    openai_cached_from_details, openai_reasoning_from_details, token_session, CachedDatabase,
    InputSemantics, RawUsage, UsageEvent,
};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// 路径短哈希：多个 state.vscdb（global + 各 workspace）共用递增下标时，
/// 事件 id 必须带上库标识，否则聚合去重会互相覆盖。
fn db_path_tag(path: &Path) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.to_string_lossy().hash(&mut hasher);
    format!("{:08x}", hasher.finish() as u32)
}

#[derive(Default)]
struct CursorSessionAccumulator {
    session_id: String,
    project_key: String,
    model: String,
    first_timestamp: String,
    last_timestamp: String,
    input_tokens: i64,
    cached_input_tokens: i64,
    cache_creation_input_tokens: i64,
    output_tokens: i64,
    total_tokens: i64,
    user_message_count: i64,
}

pub fn collect_cursor_db_paths(home: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(target_os = "macos")]
    let base = home
        .join("Library")
        .join("Application Support")
        .join("Cursor")
        .join("User");
    #[cfg(target_os = "windows")]
    let base = home
        .join("AppData")
        .join("Roaming")
        .join("Cursor")
        .join("User");
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let base = home.join(".config").join("Cursor").join("User");

    let global_db = base.join("globalStorage").join("state.vscdb");
    if global_db.is_file() {
        paths.push(global_db);
    }

    let ws_dir = base.join("workspaceStorage");
    if let Ok(entries) = std::fs::read_dir(ws_dir) {
        for entry in entries.flatten() {
            let db_file = entry.path().join("state.vscdb");
            if db_file.is_file() {
                paths.push(db_file);
            }
        }
    }

    paths
}

/// Cursor 上报口径极其有限：通常只有 total（tokenCount），input/output 可能缺失。
/// 统一约束 input + output == total：缺谁由 total 补齐，绝不两边硬推导致加总溢出。
/// 缓存命中只信 prompt_tokens_details.cached_tokens；无法拆分时整体记为估算。
pub fn cursor_usage(bubble: &JsonValue, total_tokens: i64) -> RawUsage {
    let input = number(bubble, &["inputTokens", "promptTokens"]);
    let output = number(bubble, &["outputTokens", "completionTokens"]);
    let cached = openai_cached_from_details(bubble).max(number(bubble, &["cachedTokens"]));

    let (input, output) = match (input, output) {
        (0, 0) => (total_tokens, 0),
        (i, 0) => (i, (total_tokens - i).max(0)),
        (0, o) => ((total_tokens - o).max(0), o),
        (i, o) => (i, o),
    };
    let output = if output == 0 {
        total_tokens.saturating_sub(input)
    } else {
        output
    };

    RawUsage {
        input,
        // Cursor 的 input/promptTokens 若非零，遵循 OpenAI 习惯（含缓存）；
        // 无缓存字段时与 Fresh 等价（减 0）。
        semantics: InputSemantics::InclusiveOfCacheRead,
        cache_read: cached,
        output,
        reasoning: openai_reasoning_from_details(bubble),
        ..Default::default()
    }
}

pub fn parse_cursor_database(path: &Path) -> CachedDatabase {
    let mut events = Vec::new();
    let mut sessions = Vec::new();
    let path_tag = db_path_tag(path);

    let Some(conn) = open_readonly_sqlite(path) else {
        return CachedDatabase {
            fingerprint: database_fingerprint(path),
            ..Default::default()
        };
    };

    let query_res = conn.prepare(
        "SELECT key, value FROM ItemTable
         WHERE key LIKE 'aiService.prompts%'
            OR key LIKE 'workbench.panel.aichat.chatdata%'
            OR key LIKE 'composer.composerData%'
            OR key LIKE 'interactive.sessions%'",
    );

    let Ok(mut stmt) = query_res else {
        return CachedDatabase {
            fingerprint: database_fingerprint(path),
            ..Default::default()
        };
    };

    let mut session_groups: BTreeMap<String, CursorSessionAccumulator> = BTreeMap::new();

    if let Ok(rows) = stmt.query_map([], |row| {
        let key: String = row.get(0)?;
        let val: String = row.get(1)?;
        Ok((key, val))
    }) {
        for (_key, val_str) in rows.flatten() {
            let Ok(val_json) = serde_json::from_str::<JsonValue>(&val_str) else {
                continue;
            };

            // A. composer.composerData / aichat.chatdata
            if let Some(tabs) = val_json.get("tabs").and_then(JsonValue::as_array) {
                for tab in tabs {
                    let tab_id = tab
                        .get("tabId")
                        .and_then(JsonValue::as_str)
                        .unwrap_or("cursor_tab");
                    let mut model_name = tab
                        .get("selectedModel")
                        .and_then(JsonValue::as_str)
                        .unwrap_or("cursor-fast")
                        .to_string();
                    if model_name.is_empty() {
                        model_name = "cursor-default".to_string();
                    }
                    let bubbles = tab.get("bubbles").and_then(JsonValue::as_array);
                    if let Some(bubbles) = bubbles {
                        for bubble in bubbles {
                            let b_type =
                                bubble.get("type").and_then(JsonValue::as_str).unwrap_or("");
                            let created_at = bubble
                                .get("createdAt")
                                .and_then(JsonValue::as_i64)
                                .unwrap_or(0);
                            let iso_ts = if created_at > 0 {
                                iso_from_millis(created_at)
                            } else {
                                String::new()
                            };

                            if b_type == "ai" || bubble.get("tokenCount").is_some() {
                                let total_tokens =
                                    number(bubble, &["tokenCount", "tokens", "totalTokens"]);
                                if total_tokens <= 0 {
                                    continue;
                                }
                                let raw = cursor_usage(bubble, total_tokens);
                                let (input, cached, _write, output, _reasoning, total) =
                                    normalize_usage(raw);
                                // 修复：仅当 input/output 完全缺失时才标记为估算
                                // cached == 0 可能是真实的零缓存命中，不应排除出统计
                                let has_breakdown = bubble.get("inputTokens").is_some()
                                    || bubble.get("promptTokens").is_some()
                                    || bubble.get("outputTokens").is_some()
                                    || bubble.get("completionTokens").is_some();
                                let estimated = !has_breakdown;

                                let group = session_groups
                                    .entry(tab_id.to_string())
                                    .or_insert_with(|| CursorSessionAccumulator {
                                        session_id: tab_id.to_string(),
                                        project_key: "cursor-workspace".to_string(),
                                        model: model_name.clone(),
                                        ..Default::default()
                                    });

                                update_bounds(
                                    &mut group.first_timestamp,
                                    &mut group.last_timestamp,
                                    &iso_ts,
                                );
                                group.input_tokens += input;
                                group.cached_input_tokens += cached;
                                group.output_tokens += output;
                                group.total_tokens += total;

                                events.push(UsageEvent {
                                    id: format!("cursor_{path_tag}_{}_{}", tab_id, created_at),
                                    source: "cursor".to_string(),
                                    model: model_name.clone(),
                                    project_key: "cursor-workspace".to_string(),
                                    timestamp: iso_ts,
                                    input_tokens: input,
                                    cached_input_tokens: cached,
                                    cache_creation_input_tokens: 0,
                                    output_tokens: output,
                                    reasoning_output_tokens: 0,
                                    total_tokens: total,
                                    conversation_count: 0,
                                    cost_usd: 0.0,
                                    pricing_available: false,
                                    estimated_tokens: if estimated { total } else { 0 },
                                });
                            } else if b_type == "user" {
                                let group = session_groups
                                    .entry(tab_id.to_string())
                                    .or_insert_with(|| CursorSessionAccumulator {
                                        session_id: tab_id.to_string(),
                                        project_key: "cursor-workspace".to_string(),
                                        model: model_name.clone(),
                                        ..Default::default()
                                    });
                                group.user_message_count += 1;
                            }
                        }
                    }
                }
            }

            // B. aiService.prompts (数组形式的请求记录)
            if let Some(prompts) = val_json.as_array() {
                for (idx, p) in prompts.iter().enumerate() {
                    let model = p
                        .get("model")
                        .and_then(JsonValue::as_str)
                        .unwrap_or("cursor-default");
                    let ts_ms = p.get("timestamp").and_then(JsonValue::as_i64).unwrap_or(0);
                    let iso_ts = if ts_ms > 0 {
                        iso_from_millis(ts_ms)
                    } else {
                        String::new()
                    };
                    let total_tokens = number(p, &["totalTokens", "tokens"]);
                    let raw = if total_tokens > 0 {
                        cursor_usage(p, total_tokens)
                    } else {
                        let input = number(p, &["inputTokens", "promptTokens"]);
                        let output = number(p, &["outputTokens", "completionTokens"]);
                        if input + output <= 0 {
                            continue;
                        }
                        RawUsage {
                            input,
                            semantics: InputSemantics::InclusiveOfCacheRead,
                            cache_read: openai_cached_from_details(p),
                            output,
                            reasoning: openai_reasoning_from_details(p),
                            ..Default::default()
                        }
                    };
                    let (input, cached, _write, output, _reasoning, total) =
                        normalize_usage(raw);
                    if total <= 0 {
                        continue;
                    }
                    // 修复：仅当完全无 token 明细时才标记估算
                    let has_breakdown = p.get("inputTokens").is_some()
                        || p.get("promptTokens").is_some()
                        || p.get("outputTokens").is_some()
                        || p.get("completionTokens").is_some();
                    let estimated = !has_breakdown;

                    events.push(UsageEvent {
                        id: format!("cursor_prompt_{path_tag}_{}_{}", ts_ms, idx),
                        source: "cursor".to_string(),
                        model: model.to_string(),
                        project_key: "cursor-workspace".to_string(),
                        timestamp: iso_ts,
                        input_tokens: input,
                        cached_input_tokens: cached,
                        cache_creation_input_tokens: 0,
                        output_tokens: output,
                        reasoning_output_tokens: 0,
                        total_tokens: total,
                        conversation_count: 0,
                        cost_usd: 0.0,
                        pricing_available: false,
                        estimated_tokens: if estimated { total } else { 0 },
                    });
                }
            }
        }
    }

    for group in session_groups.into_values() {
        if group.total_tokens > 0 || group.user_message_count > 0 {
            sessions.push(token_session(
                group.session_id,
                "cursor",
                group.project_key,
                group.model,
                group.first_timestamp,
                group.last_timestamp,
                group.user_message_count,
                TokenSessionTokens {
                    input_tokens: group.input_tokens,
                    cached_input_tokens: group.cached_input_tokens,
                    cache_creation_input_tokens: group.cache_creation_input_tokens,
                    output_tokens: group.output_tokens,
                    reasoning_output_tokens: 0,
                    total_tokens: group.total_tokens,
                },
                0.0,
            ));
        }
    }

    CachedDatabase {
        fingerprint: database_fingerprint(path),
        events,
        sessions,
    }
}
