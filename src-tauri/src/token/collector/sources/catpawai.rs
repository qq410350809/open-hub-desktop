use crate::models::{TokenSession, TokenSessionTokens};
use crate::token::collector::normalizer::normalize_workspace_project_key;
use crate::token::collector::time_utils::{iso_from_millis, update_bounds};
use crate::token::collector::types::{
    database_fingerprint, fingerprint, number, open_readonly_sqlite, token_session, CachedDatabase,
    CachedFile, LocalDatabaseSession, UsageEvent,
};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const CATPAWAI_UNKNOWN_MODEL: &str = "catpawai-unknown-model";

pub fn catpawai_db_paths(home: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(value) = std::env::var("OPENHUB_CATPAWAI_DB_PATH") {
        let p = PathBuf::from(value);
        if p.is_file() {
            paths.push(p);
            return paths;
        }
    }
    let default_paths = [
        home.join(".sankuai")
            .join("CatPawAI")
            .join("sqliteDB")
            .join("globalCache.sqlite"),
        home.join("Library")
            .join("Application Support")
            .join("CatPawAI")
            .join("sqliteDB")
            .join("globalCache.sqlite"),
    ];
    for p in default_paths {
        if p.is_file() && !paths.contains(&p) {
            paths.push(p);
        }
    }
    paths
}

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

pub fn catpawai_nonempty_string(value: &JsonValue, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub fn catpawai_selected_model(value: &JsonValue) -> Option<String> {
    catpawai_nonempty_string(value, "selectedModelName")
        .or_else(|| {
            value
                .get("submitEditorState")
                .and_then(|state| catpawai_nonempty_string(state, "selectedModelName"))
        })
        .or_else(|| {
            value
                .get("submitEditorState")
                .and_then(|state| state.get("selectedModelInfo"))
                .and_then(|info| catpawai_nonempty_string(info, "modelTypeName"))
        })
}

pub fn catpawai_actual_model(value: &JsonValue) -> Option<String> {
    catpawai_nonempty_string(value, "actualUseModelName").or_else(|| {
        value
            .get("blockData")
            .and_then(|block| catpawai_nonempty_string(block, "actualUseModelName"))
    })
}

pub fn catpawai_model_is_resolved(model: &str) -> bool {
    let value = model.trim();
    !value.is_empty()
        && !value.eq_ignore_ascii_case("unknown")
        && !value.chars().all(|ch| ch.is_ascii_digit())
}

pub fn catpawai_usage(value: &JsonValue) -> Option<&JsonValue> {
    value
        .get("tokenUsage")
        .filter(|usage| usage.is_object())
        .or_else(|| {
            value
                .get("blockData")
                .and_then(|block| block.get("usage"))
                .filter(|usage| usage.is_object())
        })
}

/// 统一归一化 CatPawAI Token 计量
/// - 格式 1（OpenAI 嵌入式）：prompt_tokens 已包含 cachedTokens（prompt_tokens_details），需扣减得到 fresh_input
/// - 格式 2（新网关独立式）：prompt_tokens 仅为 fresh_input，cacheReadTokens 独立上报，总计需累加缓存
/// - 两缓存字段并存时无法从数值判定 prompt 是否含缓存，仅当 raw_total 证明缓存已被独立计入
///   （total == prompt + cache + completion）时才维持 prompt 为全新输入，避免双计
/// - 仅 total_tokens 可用时以其为总量兜底（扣缓存后拆分）
/// - total = fresh_input + 缓存命中 + 输出；缓存写入独立上报，不计入 total
pub fn normalize_catpawai_usage_numbers(
    prompt: i64,
    completion: i64,
    raw_total: i64,
    cache_read_field: i64,
    cache_write: i64,
    cached_from_details: i64,
    reasoning: i64,
) -> (i64, i64, i64, i64, i64, i64) {
    let cached_input = cache_read_field.max(cached_from_details);
    let fresh_input = if cached_from_details > 0 && cache_read_field == 0 {
        // 格式 1：prompt 含缓存命中（与写入），拆出全新输入。
        prompt
            .saturating_sub(cached_input)
            .saturating_sub(cache_write)
    } else if cache_read_field > 0
        && cached_from_details > 0
        && raw_total > 0
        && raw_total == prompt
            .saturating_add(cached_input)
            .saturating_add(completion)
    {
        // 两字段并存：raw_total 证明缓存是独立分量（未被并入 prompt），prompt 即全新输入。
        prompt
    } else if prompt == 0 && completion == 0 && raw_total > 0 {
        // 仅 total_tokens 可用：以总量扣缓存拆分。
        raw_total
            .saturating_sub(cached_input)
            .saturating_sub(cache_write)
    } else {
        prompt
    };
    // 口径：total = 全新输入 + 缓存命中 + 输出；缓存写入独立上报，不计入 total。
    let total = fresh_input
        .saturating_add(cached_input)
        .saturating_add(completion);
    let reasoning = reasoning.min(completion);
    (
        fresh_input,
        cached_input,
        cache_write,
        completion,
        reasoning,
        total,
    )
}

pub fn parse_catpawai_database(path: &Path) -> CachedDatabase {
    let Some(connection) = open_readonly_sqlite(path) else {
        return CachedDatabase {
            fingerprint: database_fingerprint(path),
            ..Default::default()
        };
    };

    let mut sessions = BTreeMap::<String, LocalDatabaseSession>::new();
    let mut projects = BTreeMap::<String, String>::new();

    // 1. 读取 t_conversations 会话元数据
    if let Ok(mut statement) = connection.prepare(
        "SELECT conversation_id, workspace_id, title, create_time, update_time FROM t_conversations",
    ) {
        if let Ok(rows) = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0).unwrap_or_default(),
                row.get::<_, Option<String>>(1).ok().flatten(),
                row.get::<_, Option<String>>(2).ok().flatten(),
                row.get::<_, i64>(3).unwrap_or_default(),
                row.get::<_, i64>(4).unwrap_or_default(),
            ))
        }) {
            for (conversation_id, workspace_id, title, create_time, update_time) in rows.flatten() {
                if conversation_id.is_empty() {
                    continue;
                }
                let raw_project = workspace_id
                    .filter(|v| !v.trim().is_empty())
                    .or_else(|| title.filter(|v| !v.trim().is_empty()))
                    .unwrap_or_else(|| "CatPawAI".to_string());
                let project = normalize_workspace_project_key(&raw_project, "CatPawAI");
                projects.insert(conversation_id.clone(), project);

                // 时间戳判定阈值：< 10^10 视为秒（需乘 1000），>= 10^10 视为毫秒
                // 10^10 毫秒 = 2001-09-09，10^10 秒 = 2286 年
                // 修复：之前阈值 10^11 导致 2001-2073 年数据被错误乘以 1000
                let started_ms = if create_time > 0 && create_time < 10_000_000_000 {
                    create_time.saturating_mul(1000)
                } else {
                    create_time
                };
                let ended_ms = if update_time > 0 && update_time < 10_000_000_000 {
                    update_time.saturating_mul(1000)
                } else {
                    update_time
                };

                sessions.insert(
                    conversation_id,
                    LocalDatabaseSession {
                        directory: raw_project,
                        started_at: iso_from_millis(started_ms),
                        ended_at: iso_from_millis(ended_ms),
                        ..Default::default()
                    },
                );
            }
        }
    }

    // 2. 读取 t_conversation 备用会话元数据
    if let Ok(mut statement) = connection.prepare(
        "SELECT conversation_id, project_path, history_title, ts, created_at FROM t_conversation",
    ) {
        if let Ok(rows) = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0).unwrap_or_default(),
                row.get::<_, Option<String>>(1).ok().flatten(),
                row.get::<_, Option<String>>(2).ok().flatten(),
                row.get::<_, i64>(3).unwrap_or_default(),
                row.get::<_, i64>(4).unwrap_or_default(),
            ))
        }) {
            for (conversation_id, project_path, title, ts, created_at) in rows.flatten() {
                if conversation_id.is_empty() {
                    continue;
                }
                let raw_project = project_path
                    .filter(|v| !v.trim().is_empty())
                    .or_else(|| title.filter(|v| !v.trim().is_empty()))
                    .unwrap_or_else(|| "CatPawAI".to_string());
                let project = normalize_workspace_project_key(&raw_project, "CatPawAI");
                projects.entry(conversation_id.clone()).or_insert(project);

                let started_ms = if created_at > 0 && created_at < 10_000_000_000 {
                    created_at.saturating_mul(1000)
                } else if ts > 0 && ts < 10_000_000_000 {
                    ts.saturating_mul(1000)
                } else {
                    created_at
                };

                sessions
                    .entry(conversation_id)
                    .or_insert_with(|| LocalDatabaseSession {
                        directory: raw_project,
                        started_at: iso_from_millis(started_ms),
                        ended_at: iso_from_millis(started_ms),
                        ..Default::default()
                    });
            }
        }
    }

    // 3. 读取 t_ui_messages 消息与 Token 用量
    let mut events = Vec::<UsageEvent>::new();
    let mut current_models = BTreeMap::<String, String>::new();

    if let Ok(mut statement) = connection.prepare(
        "SELECT id, conversation_id, message_id, message_type, create_time, content \
         FROM t_ui_messages ORDER BY conversation_id ASC, create_time ASC, id ASC",
    ) {
        if let Ok(rows) = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0).unwrap_or_default(),
                row.get::<_, String>(1).unwrap_or_default(),
                row.get::<_, String>(2).unwrap_or_default(),
                row.get::<_, String>(3).unwrap_or_default(),
                row.get::<_, i64>(4).unwrap_or_default(),
                row.get::<_, String>(5).unwrap_or_default(),
            ))
        }) {
            for (id, conversation_id, message_id, message_type, create_time, content) in
                rows.flatten()
            {
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
                    .filter(|m| catpawai_model_is_resolved(m))
                    .map(str::to_string)
                    .or_else(|| current_models.get(&conversation_id).cloned())
                    .unwrap_or_else(|| CATPAWAI_UNKNOWN_MODEL.to_string());

                let normalized_ms = if create_time > 0 && create_time < 10_000_000_000 {
                    create_time.saturating_mul(1000)
                } else {
                    create_time
                };
                let iso_ts = iso_from_millis(normalized_ms);
                let project_key = projects
                    .get(&conversation_id)
                    .cloned()
                    .unwrap_or_else(|| "CatPawAI".to_string());

                let session_entry = sessions.entry(conversation_id.clone()).or_insert_with(|| {
                    LocalDatabaseSession {
                        directory: "CatPawAI".to_string(),
                        started_at: iso_ts.clone(),
                        ended_at: iso_ts.clone(),
                        ..Default::default()
                    }
                });

                if session_entry.model.is_empty() || session_entry.model == CATPAWAI_UNKNOWN_MODEL {
                    if model != CATPAWAI_UNKNOWN_MODEL {
                        session_entry.model = model.clone();
                    }
                }

                update_bounds(
                    &mut session_entry.started_at,
                    &mut session_entry.ended_at,
                    &iso_ts,
                );

                if message_type == "user_prompt" {
                    session_entry.turns += 1;
                    events.push(UsageEvent {
                        id: format!("catpawai_{conversation_id}_p_{id}"),
                        source: "catpawai".to_string(),
                        model: model.clone(),
                        project_key: project_key.clone(),
                        timestamp: iso_ts.clone(),
                        conversation_count: 1,
                        ..Default::default()
                    });
                }

                let Some(usage) = catpawai_usage(&value) else {
                    continue;
                };

                let prompt = number(
                    usage,
                    &[
                        "prompt_tokens",
                        "promptTokens",
                        "input_tokens",
                        "inputTokens",
                    ],
                );
                let completion = number(
                    usage,
                    &[
                        "completion_tokens",
                        "completionTokens",
                        "output_tokens",
                        "outputTokens",
                    ],
                );
                let raw_total = number(usage, &["total_tokens", "totalTokens"]);
                let cached_from_details = usage
                    .get("promptTokensDetails")
                    .or_else(|| usage.get("prompt_tokens_details"))
                    .map(|details| number(details, &["cachedTokens", "cached_tokens"]))
                    .unwrap_or(0);
                let cache_read_field = number(
                    usage,
                    &[
                        "cacheReadTokens",
                        "cache_read_tokens",
                        "cached_input_tokens",
                        "cachedInputTokens",
                    ],
                );
                let cache_write = number(
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
                    .map(|details| number(details, &["reasoningTokens", "reasoning_tokens"]))
                    .unwrap_or_else(|| {
                        number(usage, &["reasoning_output_tokens", "reasoningOutputTokens"])
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

                session_entry.tokens.input_tokens += fresh_input;
                session_entry.tokens.cached_input_tokens += cached_input;
                session_entry.tokens.cache_creation_input_tokens += cache_write_tok;
                session_entry.tokens.output_tokens += output_tok;
                session_entry.tokens.reasoning_output_tokens += reasoning_tok;
                session_entry.tokens.total_tokens += total;

                let event_id = if !message_id.is_empty() {
                    format!("catpawai_{conversation_id}_{message_id}")
                } else {
                    format!("catpawai_{conversation_id}_{id}")
                };

                events.push(UsageEvent {
                    id: event_id,
                    source: "catpawai".to_string(),
                    model,
                    project_key,
                    timestamp: iso_ts,
                    input_tokens: fresh_input,
                    cached_input_tokens: cached_input,
                    cache_creation_input_tokens: cache_write_tok,
                    output_tokens: output_tok,
                    reasoning_output_tokens: reasoning_tok,
                    total_tokens: total,
                    conversation_count: 0,
                    cost_usd: 0.0,
                    pricing_available: false,
                    estimated_tokens: 0,
                });
            }
        }
    }

    let parsed_sessions = sessions
        .into_iter()
        .filter(|(_, session)| session.turns > 0 || session.tokens.total_tokens > 0)
        .map(|(conversation_id, session)| {
            let project_key = projects
                .get(&conversation_id)
                .cloned()
                .unwrap_or_else(|| normalize_workspace_project_key(&session.directory, "CatPawAI"));
            let model = if session.model.is_empty() {
                CATPAWAI_UNKNOWN_MODEL.to_string()
            } else {
                session.model
            };
            token_session(
                conversation_id,
                "catpawai",
                project_key,
                model,
                session.started_at,
                session.ended_at,
                session.turns,
                session.tokens,
                0.0,
            )
        })
        .collect::<Vec<TokenSession>>();

    CachedDatabase {
        fingerprint: database_fingerprint(path),
        events,
        sessions: parsed_sessions,
    }
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

    let mut total_fresh_in = 0i64;
    let mut total_cached_in = 0i64;
    let mut total_cache_write = 0i64;
    let mut total_out = 0i64;
    let mut total_reasoning = 0i64;
    let mut total_all = 0i64;

    for (idx, line) in content.lines().enumerate() {
        let Ok(val) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };

        if let Some(m) = val
            .get("model")
            .or_else(|| val.get("model_name"))
            .and_then(JsonValue::as_str)
        {
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
            let out_tok = number(
                usage,
                &["completion_tokens", "output_tokens", "completionTokens"],
            );
            let raw_total = number(usage, &["total_tokens", "tokens"]);
            let cache_read = number(
                usage,
                &[
                    "cacheReadTokens",
                    "cache_read_tokens",
                    "cached_input_tokens",
                    "cachedInputTokens",
                ],
            );
            let cache_write = number(
                usage,
                &[
                    "cacheWriteTokens",
                    "cache_write_tokens",
                    "cache_creation_input_tokens",
                    "cacheCreationInputTokens",
                ],
            );
            let cached_from_details = usage
                .get("promptTokensDetails")
                .or_else(|| usage.get("prompt_tokens_details"))
                .map(|details| number(details, &["cachedTokens", "cached_tokens"]))
                .unwrap_or(0);
            let reasoning = usage
                .get("completionTokensDetails")
                .or_else(|| usage.get("completion_tokens_details"))
                .map(|details| number(details, &["reasoningTokens", "reasoning_tokens"]))
                .unwrap_or_else(|| {
                    number(usage, &["reasoning_output_tokens", "reasoningOutputTokens"])
                });

            let (fresh_in, cached_in, cache_write_tok, out, reasoning_tok, total) =
                normalize_catpawai_usage_numbers(
                    in_tok,
                    out_tok,
                    raw_total,
                    cache_read,
                    cache_write,
                    cached_from_details,
                    reasoning,
                );

            if total > 0 {
                let ts_str = val
                    .get("timestamp")
                    .or_else(|| val.get("created_at"))
                    .and_then(JsonValue::as_str)
                    .unwrap_or("");
                let iso_ts = if !ts_str.is_empty() {
                    ts_str.to_string()
                } else {
                    String::new()
                };
                update_bounds(&mut first_ts, &mut last_ts, &iso_ts);

                total_fresh_in += fresh_in;
                total_cached_in += cached_in;
                total_cache_write += cache_write_tok;
                total_out += out;
                total_reasoning += reasoning_tok;
                total_all += total;

                events.push(UsageEvent {
                    id: format!("{source}_{session_id}_{idx}"),
                    source: source.to_string(),
                    model: model_name.clone(),
                    project_key: project_key.clone(),
                    timestamp: iso_ts,
                    input_tokens: fresh_in,
                    cached_input_tokens: cached_in,
                    cache_creation_input_tokens: cache_write_tok,
                    output_tokens: out,
                    reasoning_output_tokens: reasoning_tok,
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
                input_tokens: total_fresh_in,
                cached_input_tokens: total_cached_in,
                cache_creation_input_tokens: total_cache_write,
                output_tokens: total_out,
                reasoning_output_tokens: total_reasoning,
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
