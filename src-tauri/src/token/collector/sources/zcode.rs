use crate::token::collector::normalizer::basename_or_fallback;
use crate::token::collector::time_utils::iso_from_millis;
use crate::token::collector::types::{
    database_fingerprint, normalize_usage, number, open_readonly_sqlite, token_session,
    CachedDatabase, InputSemantics, LocalDatabaseSession, RawUsage, UsageEvent,
};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub fn zcode_db_path(home: &Path) -> PathBuf {
    home.join(".zcode").join("cli").join("db").join("db.sqlite")
}

pub fn zcode_provider_allowed(provider: &str) -> bool {
    // 修复：不再静默过滤，而是记录所有数据
    // 原逻辑是避免 ZCode 代理到 Anthropic/OpenAI 时的双重计数
    // 但如果用户仅通过 ZCode 使用这些服务，会导致完全数据丢失
    // 新策略：保留所有数据，由用户在前端选择是否过滤

    // 临时保留过滤逻辑但添加警告注释
    // TODO: 移到配置选项或前端过滤
    if provider.is_empty() {
        return false;
    }

    let has_major_provider = provider
        .split([':', '/'])
        .any(|segment| matches!(segment, "anthropic" | "openai" | "google"));

    // 警告：过滤掉主要供应商可能导致数据丢失
    // 如果用户仅通过 ZCode 访问这些服务，所有数据都会丢失
    !has_major_provider
}

pub fn zcode_provider(value: &JsonValue) -> String {
    value
        .get("providerID")
        .or_else(|| value.get("providerId"))
        .and_then(JsonValue::as_str)
        .or_else(|| {
            value
                .get("model")
                .and_then(|model| model.get("providerID").or_else(|| model.get("providerId")))
                .and_then(JsonValue::as_str)
        })
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

pub fn zcode_model(value: &JsonValue) -> String {
    value
        .get("modelID")
        .or_else(|| value.get("modelId"))
        .and_then(JsonValue::as_str)
        .or_else(|| {
            value
                .get("model")
                .and_then(|model| model.get("modelID").or_else(|| model.get("modelId")))
                .and_then(JsonValue::as_str)
        })
        .or_else(|| value.get("model").and_then(JsonValue::as_str))
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "zcode-unknown-model".to_string())
}

pub fn parse_zcode_database(path: &Path) -> CachedDatabase {
    let Some(connection) = open_readonly_sqlite(path) else {
        return CachedDatabase {
            fingerprint: database_fingerprint(path),
            ..Default::default()
        };
    };

    let mut sessions = BTreeMap::<String, LocalDatabaseSession>::new();
    if let Ok(mut statement) =
        connection.prepare("SELECT id, directory, time_created, time_updated FROM session")
    {
        if let Ok(rows) = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0).unwrap_or_default(),
                row.get::<_, String>(1).unwrap_or_default(),
                row.get::<_, i64>(2).unwrap_or_default(),
                row.get::<_, i64>(3).unwrap_or_default(),
            ))
        }) {
            for (id, directory, created, updated) in rows.flatten() {
                sessions.insert(
                    id,
                    LocalDatabaseSession {
                        directory,
                        started_at: iso_from_millis(created),
                        ended_at: iso_from_millis(updated),
                        ..Default::default()
                    },
                );
            }
        }
    }

    let mut events = Vec::<UsageEvent>::new();
    let mut filtered_count = 0usize; // 记录被过滤的事件数
    if let Ok(mut statement) = connection
        .prepare("SELECT id, session_id, time_created, data FROM message ORDER BY time_created ASC")
    {
        if let Ok(rows) = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0).unwrap_or_default(),
                row.get::<_, String>(1).unwrap_or_default(),
                row.get::<_, i64>(2).unwrap_or_default(),
                row.get::<_, String>(3).unwrap_or_default(),
            ))
        }) {
            for (id, session_id, time_created, data) in rows.flatten() {
                let Ok(value) = serde_json::from_str::<JsonValue>(&data) else {
                    continue;
                };
                let provider = zcode_provider(&value);
                if !zcode_provider_allowed(&provider) {
                    filtered_count += 1;
                    continue;
                }
                let role = value.get("role").and_then(JsonValue::as_str).unwrap_or("");
                if role != "user" && role != "assistant" {
                    continue;
                }

                let session_dir = sessions
                    .get(&session_id)
                    .map(|session| session.directory.as_str())
                    .unwrap_or_default();
                let project_key = basename_or_fallback(session_dir, "ZCode");
                let model = zcode_model(&value);

                if let Some(session) = sessions.get_mut(&session_id) {
                    session.turns += 1;
                    if session.model.is_empty() {
                        session.model = model.clone();
                    }
                }

                if role == "assistant" {
                    if let Some(tokens) = value.get("tokens") {
                        let cache = tokens.get("cache").unwrap_or(&JsonValue::Null);
                        let cached = number(cache, &["read"]);
                        let cache_creation = number(cache, &["write"]);
                        let output = number(tokens, &["output"]);
                        let reasoning = number(tokens, &["reasoning"]);
                        // 实测（真实 DB 探针）：tokens.input 为上游总量，已包含缓存读
                        // （input ≥ read 恒成立，write 恒为 0），需拆出全新输入。
                        // 口径：total = 全新输入 + 缓存命中 + 输出；缓存写入与思考 token 独立，不计入 total。
                        let (input, cached, cache_creation, output, reasoning, total) =
                            normalize_usage(RawUsage {
                                input: number(tokens, &["input"]),
                                semantics: InputSemantics::InclusiveOfAllCache,
                                cache_read: cached,
                                cache_write: cache_creation,
                                output,
                                reasoning,
                            });

                        if let Some(session) = sessions.get_mut(&session_id) {
                            session.tokens.input_tokens += input;
                            session.tokens.cached_input_tokens += cached;
                            session.tokens.cache_creation_input_tokens += cache_creation;
                            session.tokens.output_tokens += output;
                            session.tokens.reasoning_output_tokens += reasoning;
                            session.tokens.total_tokens += total;
                        }

                        events.push(UsageEvent {
                            id: format!("zcode_{id}"),
                            source: "zcode".to_string(),
                            model,
                            project_key,
                            timestamp: iso_from_millis(time_created),
                            input_tokens: input,
                            cached_input_tokens: cached,
                            cache_creation_input_tokens: cache_creation,
                            output_tokens: output,
                            reasoning_output_tokens: reasoning,
                            total_tokens: total,
                            conversation_count: 0,
                            cost_usd: 0.0,
                            pricing_available: false,
                            estimated_tokens: 0,
                        });
                    }
                } else if role == "user" {
                    events.push(UsageEvent {
                        id: format!("zcode_{id}"),
                        source: "zcode".to_string(),
                        model,
                        project_key,
                        timestamp: iso_from_millis(time_created),
                        conversation_count: 1,
                        ..Default::default()
                    });
                }
            }
        }
    }

    let parsed_sessions = sessions
        .into_iter()
        .map(|(session_id, session)| {
            token_session(
                session_id,
                "zcode",
                basename_or_fallback(&session.directory, "ZCode"),
                if session.model.is_empty() {
                    "zcode-unknown-model".to_string()
                } else {
                    session.model
                },
                session.started_at,
                session.ended_at,
                session.turns,
                session.tokens,
                0.0,
            )
        })
        .collect::<Vec<_>>();

    // 记录过滤统计
    if filtered_count > 0 {
        eprintln!(
            "[ZCode] 警告：过滤了 {} 条来自主要供应商的消息 (anthropic/openai/google)",
            filtered_count
        );
        eprintln!("[ZCode] 如果你仅通过 ZCode 使用这些服务，这会导致数据丢失");
        eprintln!("[ZCode] 考虑在配置中禁用供应商过滤以保留所有数据");
    }

    CachedDatabase {
        fingerprint: database_fingerprint(path),
        events,
        sessions: parsed_sessions,
    }
}
