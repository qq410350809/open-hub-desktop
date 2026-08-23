use crate::models::TokenSessionTokens;
use crate::token::collector::time_utils::{iso_from_millis, update_bounds};
use crate::token::collector::types::{
    database_fingerprint, number, open_readonly_sqlite, token_session, CachedDatabase, UsageEvent,
};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Default)]
struct CursorSessionAccumulator {
    session_id: String,
    project_key: String,
    model: String,
    first_timestamp: String,
    last_timestamp: String,
    input_tokens: i64,
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

pub fn parse_cursor_database(path: &Path) -> CachedDatabase {
    let mut events = Vec::new();
    let mut sessions = Vec::new();

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
                                let input_tokens = number(bubble, &["inputTokens", "promptTokens"])
                                    .max(total_tokens * 3 / 4);
                                let output_tokens =
                                    number(bubble, &["outputTokens", "completionTokens"])
                                        .max(total_tokens.saturating_sub(input_tokens));

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
                                group.input_tokens += input_tokens;
                                group.output_tokens += output_tokens;
                                group.total_tokens += total_tokens;

                                events.push(UsageEvent {
                                    id: format!("cursor_{}_{created_at}", tab_id),
                                    source: "cursor".to_string(),
                                    model: model_name.clone(),
                                    project_key: "cursor-workspace".to_string(),
                                    timestamp: iso_ts,
                                    input_tokens,
                                    cached_input_tokens: 0,
                                    cache_creation_input_tokens: 0,
                                    output_tokens,
                                    reasoning_output_tokens: 0,
                                    total_tokens,
                                    conversation_count: 0,
                                    cost_usd: 0.0,
                                    pricing_available: false,
                                    estimated_tokens: 0,
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
                    let in_tok = number(p, &["inputTokens", "promptTokens"]);
                    let out_tok = number(p, &["outputTokens", "completionTokens"]);
                    let total = if in_tok + out_tok > 0 {
                        in_tok + out_tok
                    } else {
                        number(p, &["totalTokens", "tokens"])
                    };

                    if total > 0 {
                        events.push(UsageEvent {
                            id: format!("cursor_prompt_{}_{idx}", ts_ms),
                            source: "cursor".to_string(),
                            model: model.to_string(),
                            project_key: "cursor-workspace".to_string(),
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
                    cached_input_tokens: 0,
                    cache_creation_input_tokens: 0,
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
