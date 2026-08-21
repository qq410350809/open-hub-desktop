use crate::models::{TokenUsageBucket, TokenUsageReport};
use crate::token::collector::sources::catpawai::{
    catpawai_actual_model, catpawai_model_is_resolved, catpawai_selected_model, catpawai_usage,
    normalize_catpawai_usage_numbers, CATPAWAI_UNKNOWN_MODEL,
};
use crate::token::stats::types::*;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub fn catpawai_db_path() -> Option<PathBuf> {
    if let Ok(value) = std::env::var("OPENHUB_CATPAWAI_DB_PATH") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    [
        home.join(".sankuai")
            .join("CatPawAI")
            .join("sqliteDB")
            .join("globalCache.sqlite"),
        home.join("Library")
            .join("Application Support")
            .join("CatPawAI")
            .join("sqliteDB")
            .join("globalCache.sqlite"),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

pub fn catpawai_number(value: &JsonValue, keys: &[&str]) -> i64 {
    keys.iter()
        .find_map(|key| {
            value.get(*key).and_then(|number| {
                number
                    .as_i64()
                    .or_else(|| number.as_u64().and_then(|n| i64::try_from(n).ok()))
                    .or_else(|| number.as_f64().map(|n| n as i64))
                    .or_else(|| number.as_str().and_then(|n| n.parse::<i64>().ok()))
            })
        })
        .unwrap_or(0)
        .max(0)
}

pub fn load_catpawai_projects(conn: &Connection) -> BTreeMap<String, String> {
    let mut projects = BTreeMap::new();
    if sqlite_table_exists(conn, "t_conversations") {
        if let Ok(mut stmt) =
            conn.prepare("SELECT conversation_id, workspace_id, title FROM t_conversations")
        {
            if let Ok(rows) = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            }) {
                for row in rows.flatten() {
                    let (conversation_id, workspace_id, title) = row;
                    let raw_project = workspace_id
                        .filter(|value| !value.trim().is_empty())
                        .or_else(|| title.filter(|value| !value.trim().is_empty()))
                        .unwrap_or_else(|| "CatPawAI".to_string());
                    let project = crate::token::collector::normalize_workspace_project_key(
                        &raw_project,
                        "CatPawAI",
                    );
                    projects.insert(conversation_id, project);
                }
            }
        }
    }
    if sqlite_table_exists(conn, "t_conversation") {
        if let Ok(mut stmt) =
            conn.prepare("SELECT conversation_id, project_path, history_title FROM t_conversation")
        {
            if let Ok(rows) = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            }) {
                for row in rows.flatten() {
                    let (conversation_id, project_path, title) = row;
                    projects.entry(conversation_id).or_insert_with(|| {
                        let raw_project = project_path
                            .filter(|value| !value.trim().is_empty())
                            .or_else(|| title.filter(|value| !value.trim().is_empty()))
                            .unwrap_or_else(|| "CatPawAI".to_string());
                        crate::token::collector::normalize_workspace_project_key(
                            &raw_project,
                            "CatPawAI",
                        )
                    });
                }
            }
        }
    }
    projects
}

/// 去重中间结构：一条消息的完整 usage 快照。
struct UsageEntry {
    model: String,
    project_key: String,
    timestamp: String,
    total: i64,
    prompt: i64,
    completion: i64,
    cache_read: i64,
    cache_write: i64,
    reasoning: i64,
}

pub fn catpawai_bucket_mut<'a>(
    buckets: &'a mut BTreeMap<(String, String, String), TokenUsageBucket>,
    model: String,
    project_key: String,
    timestamp: String,
) -> &'a mut TokenUsageBucket {
    let key = (model.clone(), project_key.clone(), timestamp.clone());
    buckets.entry(key).or_insert_with(|| TokenUsageBucket {
        source: CATPAWAI_SOURCE.to_string(),
        model,
        project_key,
        timestamp,
        total_tokens: 0,
        billable_total_tokens: 0,
        input_tokens: 0,
        cached_input_tokens: 0,
        cache_creation_input_tokens: 0,
        output_tokens: 0,
        reasoning_output_tokens: 0,
        conversation_count: 0,
        request_count: 0,
        cost_usd: 0.0,
        pricing_available: false,
        estimated_tokens: 0,
        estimated_input_tokens: 0,
    })
}

pub fn read_catpawai_buckets_from_path(path: &Path) -> Result<Vec<TokenUsageBucket>, String> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("无法读取 CatPawAI 数据库（{}）：{error}", path.display()))?;
    if !sqlite_table_exists(&conn, "t_ui_messages") {
        return Err("CatPawAI 数据库缺少 t_ui_messages 表".to_string());
    }
    let projects = load_catpawai_projects(&conn);
    let mut stmt = conn
        .prepare(
            "SELECT id, conversation_id, message_type, create_time, content \
             FROM t_ui_messages ORDER BY conversation_id ASC, create_time ASC, id ASC",
        )
        .map_err(|error| format!("CatPawAI 消息查询准备失败：{error}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(|error| format!("CatPawAI 消息查询失败：{error}"))?;

    let mut current_models = BTreeMap::<String, String>::new();
    let mut buckets = BTreeMap::<(String, String, String), TokenUsageBucket>::new();
    let mut deduped_usage: BTreeMap<(String, i64), UsageEntry> = BTreeMap::new();
    for row in rows.flatten() {
        let (_row_id, conversation_id, message_type, create_time, content) = row;
        let Ok(value) = serde_json::from_str::<JsonValue>(&content) else {
            continue;
        };
        if let Some(selected) = catpawai_selected_model(&value) {
            if catpawai_model_is_resolved(&selected) {
                current_models.insert(conversation_id.clone(), selected);
            }
        }
        let actual_model = catpawai_actual_model(&value);
        let model = actual_model
            .as_deref()
            .filter(|model| catpawai_model_is_resolved(model))
            .map(str::to_string)
            .or_else(|| current_models.get(&conversation_id).cloned())
            .unwrap_or_else(|| CATPAWAI_UNKNOWN_MODEL.to_string());
        let normalized_ms = if create_time > 0 && create_time < 100_000_000_000 {
            create_time.saturating_mul(1000)
        } else {
            create_time
        };
        let Some(timestamp) = hour_key_from_millis(normalized_ms) else {
            continue;
        };
        let project_key = projects
            .get(&conversation_id)
            .cloned()
            .unwrap_or_else(|| "CatPawAI".to_string());

        if message_type == "user_prompt" {
            catpawai_bucket_mut(
                &mut buckets,
                model.clone(),
                project_key.clone(),
                timestamp.clone(),
            )
            .conversation_count += 1;
        }

        let Some(usage) = catpawai_usage(&value) else {
            continue;
        };
        let prompt = catpawai_number(
            usage,
            &[
                "prompt_tokens",
                "promptTokens",
                "input_tokens",
                "inputTokens",
            ],
        );
        let completion = catpawai_number(
            usage,
            &[
                "completion_tokens",
                "completionTokens",
                "output_tokens",
                "outputTokens",
            ],
        );
        let raw_total = catpawai_number(usage, &["total_tokens", "totalTokens"]);
        let cached_from_details = usage
            .get("promptTokensDetails")
            .or_else(|| usage.get("prompt_tokens_details"))
            .map(|details| catpawai_number(details, &["cachedTokens", "cached_tokens"]))
            .unwrap_or(0);
        let cache_read_field = catpawai_number(
            usage,
            &[
                "cacheReadTokens",
                "cache_read_tokens",
                "cached_input_tokens",
                "cachedInputTokens",
            ],
        );
        let cache_write = catpawai_number(
            usage,
            &[
                "cacheWriteTokens",
                "cache_write_tokens",
                "cache_creation_input_tokens",
                "cacheCreationInputTokens",
            ],
        );
        let reasoning = usage
            .get("completionTokensDetails")
            .or_else(|| usage.get("completion_tokens_details"))
            .map(|details| catpawai_number(details, &["reasoningTokens", "reasoning_tokens"]))
            .unwrap_or_else(|| {
                catpawai_number(usage, &["reasoning_output_tokens", "reasoningOutputTokens"])
            });

        let (fresh_input, cached_input, cache_write_tok, output_tok, reasoning_tok, total) =
            normalize_catpawai_usage_numbers(
                prompt,
                completion,
                raw_total,
                cache_read_field,
                cache_write,
                cached_from_details,
                reasoning,
            );

        if total <= 0 {
            continue;
        }
        let entry = UsageEntry {
            model: model.clone(),
            project_key: project_key.clone(),
            timestamp: timestamp.clone(),
            total,
            prompt: fresh_input,
            completion: output_tok,
            cache_read: cached_input,
            cache_write: cache_write_tok,
            reasoning: reasoning_tok,
        };
        let dedup_key = (conversation_id.clone(), create_time);
        let should_replace = deduped_usage
            .get(&dedup_key)
            .map(|existing| entry.total > existing.total)
            .unwrap_or(true);
        if should_replace {
            deduped_usage.insert(dedup_key, entry);
        }
    }

    // 去重后再聚合到 buckets
    for UsageEntry {
        model,
        project_key,
        timestamp,
        total,
        prompt,
        completion,
        cache_read,
        cache_write,
        reasoning,
    } in deduped_usage.into_values()
    {
        let bucket = catpawai_bucket_mut(&mut buckets, model, project_key, timestamp);
        bucket.request_count += 1;
        bucket.total_tokens += total;
        bucket.billable_total_tokens += total;
        bucket.input_tokens += prompt;
        bucket.cached_input_tokens += cache_read;
        bucket.cache_creation_input_tokens += cache_write;
        bucket.output_tokens += completion;
        bucket.reasoning_output_tokens += reasoning;
    }
    Ok(buckets.into_values().collect())
}

pub fn read_catpawai_buckets() -> Result<Vec<TokenUsageBucket>, String> {
    let Some(path) = catpawai_db_path() else {
        return Ok(Vec::new());
    };
    read_catpawai_buckets_from_path(&path)
}

pub fn is_catpawai_source(source: &str) -> bool {
    let normalized = source
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    normalized == "catpawai" || normalized == "catpaw"
}

pub fn merge_catpawai_usage(mut report: TokenUsageReport) -> Result<TokenUsageReport, String> {
    let catpawai_buckets = read_catpawai_buckets()?;
    if !report
        .buckets
        .iter()
        .any(|bucket| is_catpawai_source(&bucket.source))
    {
        report.buckets.extend(catpawai_buckets);
    }
    report.buckets.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.source.cmp(&right.source))
            .then_with(|| left.model.cmp(&right.model))
            .then_with(|| left.project_key.cmp(&right.project_key))
    });
    report.start_date.clear();
    report.end_date.clear();
    for bucket in &report.buckets {
        let day = bucket.timestamp.get(..10).unwrap_or("");
        if day.is_empty() {
            continue;
        }
        if report.start_date.is_empty() || day < report.start_date.as_str() {
            report.start_date = day.to_string();
        }
        if report.end_date.is_empty() || day > report.end_date.as_str() {
            report.end_date = day.to_string();
        }
    }
    report.available = !report.buckets.is_empty();
    Ok(report)
}

pub fn catpawai_data_roots(home: &Path) -> Vec<PathBuf> {
    let mut roots = vec![home.join(".sankuai").join("CatPawAI").join("sqliteDB")];
    #[cfg(target_os = "macos")]
    {
        roots.push(
            home.join("Library")
                .join("Application Support")
                .join("CatPawAI")
                .join("sqliteDB"),
        );
    }
    roots
}

pub fn collect_catpawai_activity_incremental(
    map: &mut BTreeMap<String, HealthAgg>,
    sources_map: &mut BTreeMap<String, HealthAgg>,
    cursor: &mut SqliteCursor,
) {
    let Some(path) = catpawai_db_path() else {
        return;
    };
    let Some(conn) = open_readonly_sqlite(&path) else {
        return;
    };
    if !sqlite_table_exists(&conn, "t_ui_messages") {
        return;
    }
    let since = cursor.max_time_created;
    let Ok(mut stmt) = conn.prepare(
        "SELECT message_type, create_time, content FROM t_ui_messages \
         WHERE create_time > ?1 ORDER BY create_time ASC, id ASC",
    ) else {
        return;
    };
    let rows = stmt.query_map([since], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    });
    let Ok(rows) = rows else {
        return;
    };
    let mut max_time = since;
    for row in rows.flatten() {
        let (message_type, create_time, content) = row;
        if create_time > max_time {
            max_time = create_time;
        }
        let normalized_ms = if create_time > 0 && create_time < 100_000_000_000 {
            create_time.saturating_mul(1000)
        } else {
            create_time
        };
        let Some(hour) = hour_key_from_millis(normalized_ms) else {
            continue;
        };
        if message_type == "user_prompt" {
            record(map, sources_map, CATPAWAI_SOURCE, hour, 1, 0, 0, 0);
            continue;
        }
        if message_type == "error" {
            record(map, sources_map, CATPAWAI_SOURCE, hour, 0, 1, 0, 1);
            continue;
        }
        let Ok(value) = serde_json::from_str::<JsonValue>(&content) else {
            continue;
        };
        if value.get("streamStatus").and_then(JsonValue::as_str) == Some("error") {
            record(map, sources_map, CATPAWAI_SOURCE, hour, 0, 1, 0, 1);
            continue;
        }
        let Some(usage) = catpawai_usage(&value) else {
            continue;
        };
        let prompt = catpawai_number(
            usage,
            &[
                "prompt_tokens",
                "promptTokens",
                "input_tokens",
                "inputTokens",
            ],
        );
        let completion = catpawai_number(
            usage,
            &[
                "completion_tokens",
                "completionTokens",
                "output_tokens",
                "outputTokens",
            ],
        );
        let cache_read = catpawai_number(
            usage,
            &[
                "cacheReadTokens",
                "cache_read_tokens",
                "cached_input_tokens",
                "cachedInputTokens",
            ],
        );
        let total = prompt.saturating_add(cache_read).saturating_add(completion);
        if total > 0 {
            record(map, sources_map, CATPAWAI_SOURCE, hour, 0, 1, 1, 0);
        }
    }
    cursor.max_time_created = max_time;
}
