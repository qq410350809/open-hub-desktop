use crate::models::TokenSessionTokens;
use crate::token::collector::types::{
    database_fingerprint, number, open_readonly_sqlite, token_session, CachedDatabase, UsageEvent,
};
use serde_json::Value as JsonValue;
use std::path::{Path, PathBuf};

pub fn collect_windsurf_db_paths(home: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(target_os = "macos")]
    let base = home
        .join("Library")
        .join("Application Support")
        .join("Windsurf")
        .join("User");
    #[cfg(target_os = "windows")]
    let base = home
        .join("AppData")
        .join("Roaming")
        .join("Windsurf")
        .join("User");
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let base = home.join(".config").join("Windsurf").join("User");

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

pub fn parse_windsurf_database(path: &Path) -> CachedDatabase {
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
         WHERE key LIKE '%cascade%' 
            OR key LIKE '%codeium%' 
            OR key LIKE '%chatHistory%'",
    );

    let Ok(mut stmt) = query_res else {
        return CachedDatabase {
            fingerprint: database_fingerprint(path),
            ..Default::default()
        };
    };

    let mut total_in = 0i64;
    let mut total_out = 0i64;
    let mut user_msg_count = 0i64;
    let first_ts = String::new();
    let last_ts = String::new();
    let model_name = "cascade-base".to_string();

    if let Ok(rows) = stmt.query_map([], |row| {
        let key: String = row.get(0)?;
        let val: String = row.get(1)?;
        Ok((key, val))
    }) {
        for (_key, val_str) in rows.flatten() {
            let Ok(val_json) = serde_json::from_str::<JsonValue>(&val_str) else {
                continue;
            };

            if let Some(steps) = val_json
                .get("steps")
                .or_else(|| val_json.get("messages"))
                .and_then(JsonValue::as_array)
            {
                for (idx, step) in steps.iter().enumerate() {
                    let role = step
                        .get("type")
                        .or_else(|| step.get("role"))
                        .and_then(JsonValue::as_str)
                        .unwrap_or("");
                    let in_tok = number(step, &["inputTokens", "promptTokens", "input_tokens"]);
                    let out_tok =
                        number(step, &["outputTokens", "completionTokens", "output_tokens"]);
                    let total = if in_tok + out_tok > 0 {
                        in_tok + out_tok
                    } else {
                        number(step, &["tokens", "totalTokens"])
                    };

                    if role.contains("user") {
                        user_msg_count += 1;
                    }

                    if total > 0 {
                        total_in += in_tok;
                        total_out += out_tok;

                        events.push(UsageEvent {
                            id: format!("windsurf_{idx}"),
                            source: "windsurf".to_string(),
                            model: model_name.clone(),
                            project_key: "windsurf-workspace".to_string(),
                            timestamp: String::new(),
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

    let total_all = total_in + total_out;
    if total_all > 0 || user_msg_count > 0 {
        sessions.push(token_session(
            "windsurf-cascade".to_string(),
            "windsurf",
            "windsurf-workspace".to_string(),
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

    CachedDatabase {
        fingerprint: database_fingerprint(path),
        events,
        sessions,
    }
}
