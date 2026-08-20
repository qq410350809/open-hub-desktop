use crate::token_collector::normalizer::basename_or_fallback;
use crate::token_collector::time_utils::iso_from_millis;
use crate::token_collector::types::{
    database_fingerprint, number, open_readonly_sqlite, token_session, CachedDatabase,
    LocalDatabaseSession, UsageEvent,
};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub fn mimo_db_path(home: &Path) -> PathBuf {
    crate::token_collector::aggregator::env_path_override("XDG_DATA_HOME")
        .unwrap_or_else(|| home.join(".local").join("share"))
        .join("mimocode")
        .join("mimocode.db")
}

pub fn mimo_provider(value: &JsonValue) -> String {
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

pub fn mimo_model(value: &JsonValue) -> String {
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
        .unwrap_or_else(|| "mimo-unknown-model".to_string())
}

pub fn parse_mimo_database(path: &Path) -> CachedDatabase {
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
                let provider = mimo_provider(&value);
                if provider != "mimo" && provider != "xiaomi" {
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
                let project_key = basename_or_fallback(session_dir, "MiMo");
                let model = mimo_model(&value);

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
                        let input = number(tokens, &["input"]);
                        let output = number(tokens, &["output"]);
                        let reasoning = number(tokens, &["reasoning"]);
                        let total = input + cached + cache_creation + output + reasoning;

                        if let Some(session) = sessions.get_mut(&session_id) {
                            session.tokens.input_tokens += input;
                            session.tokens.cached_input_tokens += cached;
                            session.tokens.cache_creation_input_tokens += cache_creation;
                            session.tokens.output_tokens += output;
                            session.tokens.reasoning_output_tokens += reasoning;
                            session.tokens.total_tokens += total;
                        }

                        events.push(UsageEvent {
                            id: format!("mimo_{id}"),
                            source: "mimo".to_string(),
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
                        id: format!("mimo_{id}"),
                        source: "mimo".to_string(),
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
                "mimo",
                basename_or_fallback(&session.directory, "MiMo"),
                if session.model.is_empty() {
                    "mimo-unknown-model".to_string()
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

    CachedDatabase {
        fingerprint: database_fingerprint(path),
        events,
        sessions: parsed_sessions,
    }
}
