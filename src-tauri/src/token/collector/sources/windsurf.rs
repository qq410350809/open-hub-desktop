use crate::models::TokenSessionTokens;
use crate::token::collector::types::{
    database_fingerprint, normalize_usage, number, open_readonly_sqlite,
    openai_cached_from_details, openai_reasoning_from_details, token_session, CachedDatabase,
    InputSemantics, RawUsage, UsageEvent,
};
use serde_json::Value as JsonValue;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

fn db_path_tag(path: &Path) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.to_string_lossy().hash(&mut hasher);
    format!("{:08x}", hasher.finish() as u32)
}

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
    let path_tag = db_path_tag(path);

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
    let mut total_cached = 0i64;
    let mut total_out = 0i64;
    let mut total_all = 0i64;
    let mut user_msg_count = 0i64;
    let mut first_ts = String::new();
    let mut last_ts = String::new();
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
                    if role.contains("user") {
                        user_msg_count += 1;
                    }

                    let cached = openai_cached_from_details(step).max(number(step, &["cachedTokens"]));
                    let input = number(step, &["inputTokens", "promptTokens", "input_tokens"]);
                    let output =
                        number(step, &["outputTokens", "completionTokens", "output_tokens"]);
                    let fallback_total = number(step, &["tokens", "totalTokens"]);
                    if input + output <= 0 && fallback_total <= 0 {
                        continue;
                    }
                    // [OI] 式 promptTokens 含缓存命中；缺 output 时由 total 补齐。
                    let raw = if input > 0 || output > 0 {
                        RawUsage {
                            input,
                            semantics: InputSemantics::InclusiveOfCacheRead,
                            cache_read: cached,
                            output,
                            reasoning: openai_reasoning_from_details(step),
                            ..Default::default()
                        }
                    } else {
                        RawUsage {
                            input: fallback_total.saturating_sub(cached).max(0),
                            semantics: InputSemantics::Fresh,
                            cache_read: cached,
                            output: 0,
                            reasoning: openai_reasoning_from_details(step),
                            ..Default::default()
                        }
                    };
                    let (fresh, cached_read, _write, out, _reasoning, total) = normalize_usage(raw);
                    if total <= 0 {
                        continue;
                    }

                    // 提取时间戳（如果有）
                    let timestamp = step
                        .get("timestamp")
                        .or_else(|| step.get("createdAt"))
                        .or_else(|| step.get("created_at"))
                        .and_then(|v| {
                            if let Some(ms) = v.as_i64() {
                                Some(crate::token::collector::time_utils::iso_from_millis(ms))
                            } else {
                                v.as_str().map(|s| s.to_string())
                            }
                        })
                        .unwrap_or_default();

                    // 更新会话时间边界
                    if !timestamp.is_empty() {
                        crate::token::collector::time_utils::update_bounds(
                            &mut first_ts,
                            &mut last_ts,
                            &timestamp,
                        );
                    }

                    total_in += fresh;
                    total_cached += cached_read;
                    total_out += out;
                    total_all += total;

                    // 多个 state.vscdb（global + workspace）都从 idx=0 起，
                    // 事件 id 必须带库标识，否则聚合去重互相覆盖。
                    events.push(UsageEvent {
                        id: format!("windsurf_{path_tag}_{idx}"),
                        source: "windsurf".to_string(),
                        model: model_name.clone(),
                        project_key: "windsurf-workspace".to_string(),
                        timestamp,
                        input_tokens: fresh,
                        cached_input_tokens: cached_read,
                        cache_creation_input_tokens: 0,
                        output_tokens: out,
                        reasoning_output_tokens: 0,
                        total_tokens: total,
                        conversation_count: 0,
                        cost_usd: 0.0,
                        pricing_available: false,
                        estimated_tokens: if cached_read == 0 && input == 0 { total } else { 0 },
                    });
                }
            }
        }
    }

    if total_all > 0 || user_msg_count > 0 {
        sessions.push(token_session(
            format!("windsurf-cascade-{path_tag}"),
            "windsurf",
            "windsurf-workspace".to_string(),
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

    CachedDatabase {
        fingerprint: database_fingerprint(path),
        events,
        sessions,
    }
}
