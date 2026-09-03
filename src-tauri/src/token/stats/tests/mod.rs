// Token stats tests
use super::*;
use crate::db as app_db;
use crate::models::{
    Database, RequestHealthBucket, RequestHealthReport, TokenStatsReport, TokenUsageBucket,
    TokenUsageReport,
};
use rusqlite::Connection;
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

#[test]
fn token_queries_read_only_database_snapshots() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE token_cache_snapshots (
                    kind TEXT PRIMARY KEY,
                    payload_json TEXT NOT NULL,
                    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                );",
        )
        .unwrap();
    let database = Database(std::sync::Mutex::new(connection));
    let usage = TokenUsageReport {
        available: true,
        buckets: vec![TokenUsageBucket {
            source: "antigravity".to_string(),
            model: "gemini-pro-default".to_string(),
            timestamp: "2026-08-12T01:00:00.000Z".to_string(),
            total_tokens: 321,
            ..Default::default()
        }],
        ..Default::default()
    };
    let sessions = vec![crate::models::TokenSession {
        version: 1,
        session_hash: "openhub:antigravity:db-test".to_string(),
        source: "antigravity".to_string(),
        model: "gemini-pro-default".to_string(),
        started_at: "2026-08-12T01:00:00.000Z".to_string(),
        ended_at: "2026-08-12T01:01:00.000Z".to_string(),
        turns: 1,
        total_tokens: 321,
        ..Default::default()
    }];
    let health = RequestHealthReport {
        available: true,
        buckets: vec![RequestHealthBucket {
            hour: "2026-08-12T01:00:00.000Z".to_string(),
            dialogues: 1,
            requests: 2,
            success: 2,
            failed: 0,
        }],
        ..Default::default()
    };
    app_db::write_token_snapshots(&database, &usage, &sessions, &health).unwrap();

    assert_eq!(
        query_token_usage(&database).unwrap().buckets[0].total_tokens,
        321
    );
    assert_eq!(
        query_token_stats(
            &database,
            Some("2026-08-12".to_string()),
            Some("2026-08-12".to_string()),
        )
        .unwrap()
        .summary
        .total_tokens,
        321
    );
    assert_eq!(
        query_token_health(&database).unwrap().buckets[0].requests,
        2
    );
}

#[test]
fn reads_catpawai_usage_and_resolves_numeric_model_ids() {
    let path = std::env::temp_dir().join(format!(
        "openhub-catpawai-usage-{}.sqlite",
        std::process::id()
    ));
    let _ = fs::remove_file(&path);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        r#"
            CREATE TABLE t_conversations (
                conversation_id TEXT PRIMARY KEY,
                workspace_id TEXT,
                title TEXT
            );
            CREATE TABLE t_ui_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                conversation_id TEXT NOT NULL,
                message_type TEXT NOT NULL,
                create_time INTEGER NOT NULL,
                content TEXT NOT NULL
            );
            "#,
    )
    .unwrap();
    conn.execute(
        "INSERT INTO t_conversations (conversation_id, workspace_id, title) VALUES (?1, ?2, ?3)",
        rusqlite::params![
            "conv-1",
            "file:///Users/wusuoming/Documents/IdeaProjects/sz-v4.code-workspace",
            "修复工单",
        ],
    )
    .unwrap();

    let prompt_payload = serde_json::json!({
        "selectedModelName": "DeepSeek-V3",
        "submitEditorState": {
            "selectedModelInfo": {
                "modelTypeName": "DeepSeek-V3"
            }
        }
    })
    .to_string();
    conn.execute(
            "INSERT INTO t_ui_messages (conversation_id, message_type, create_time, content) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["conv-1", "user_prompt", 1_786_594_200_000i64, prompt_payload],
        )
        .unwrap();

    let numeric_model_payload = serde_json::json!({
        "actualUseModelName": "88",
        "blockData": {
            "actualUseModelName": "88",
            "usage": {
                "prompt_tokens": 1500,
                "completion_tokens": 300,
                "total_tokens": 1800,
                "promptTokensDetails": {
                    "cachedTokens": 500
                },
                "completionTokensDetails": {
                    "reasoningTokens": 100
                }
            }
        }
    })
    .to_string();
    conn.execute(
            "INSERT INTO t_ui_messages (conversation_id, message_type, create_time, content) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["conv-1", "chat_item", 1_786_594_260_000i64, numeric_model_payload],
        )
        .unwrap();

    let buckets = read_catpawai_buckets_from_path(&path).unwrap();
    assert_eq!(buckets.len(), 1);
    let bucket = &buckets[0];
    assert_eq!(bucket.source, "catpawai");
    assert_eq!(bucket.model, "DeepSeek-V3");
    assert_eq!(bucket.project_key, "sz-v4");
    assert_eq!(bucket.conversation_count, 1);
    assert_eq!(bucket.request_count, 1);
    assert_eq!(bucket.total_tokens, 1800);
    assert_eq!(
        bucket.input_tokens, 1000,
        "fresh input should be prompt - cached"
    );
    assert_eq!(bucket.cached_input_tokens, 500);
    assert_eq!(bucket.output_tokens, 300);
    assert_eq!(bucket.reasoning_output_tokens, 100);

    let _ = fs::remove_file(&path);
}

#[test]
fn catpawai_parses_gateway_format_with_separate_cache_read() {
    let path = std::env::temp_dir().join(format!(
        "openhub_catpawai_gateway_test_{}.sqlite",
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        r#"
            CREATE TABLE t_ui_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                conversation_id TEXT NOT NULL,
                message_type TEXT NOT NULL,
                create_time INTEGER NOT NULL,
                content TEXT NOT NULL
            );
            CREATE TABLE t_conversations (
                conversation_id TEXT PRIMARY KEY,
                workspace_id TEXT,
                title TEXT
            );
            "#,
    )
    .unwrap();
    conn.execute(
        "INSERT INTO t_conversations (conversation_id, workspace_id, title) VALUES (?1, ?2, ?3)",
        rusqlite::params!["conv-gw", "ai-agent", "网关测试",],
    )
    .unwrap();

    // 格式 2：prompt 仅包含全新输入 1134，cacheReadTokens 10201 独立上报
    let gateway_usage_payload = serde_json::json!({
        "selectedModelName": "glm-5.2",
        "tokenUsage": {
            "prompt_tokens": 1134,
            "cacheReadTokens": 10201,
            "completion_tokens": 138,
            "total_tokens": 1272
        }
    })
    .to_string();
    conn.execute(
            "INSERT INTO t_ui_messages (conversation_id, message_type, create_time, content) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["conv-gw", "tool", 1_786_594_260_000i64, gateway_usage_payload],
        )
        .unwrap();

    let buckets = read_catpawai_buckets_from_path(&path).unwrap();
    assert_eq!(buckets.len(), 1);
    let bucket = &buckets[0];
    assert_eq!(bucket.source, "catpawai");
    assert_eq!(bucket.model, "glm-5.2");
    assert_eq!(bucket.project_key, "ai-agent");
    assert_eq!(bucket.request_count, 1);
    assert_eq!(bucket.input_tokens, 1134, "fresh input");
    assert_eq!(bucket.cached_input_tokens, 10201, "cached input");
    assert_eq!(bucket.output_tokens, 138, "output tokens");
    assert_eq!(
        bucket.total_tokens, 11473,
        "total should include fresh + cache + output"
    );

    let _ = fs::remove_file(&path);
}

#[test]
fn catpawai_deduplicates_repeated_usage_rows() {
    let usage_partial = serde_json::json!({
        "actualUseModelName": "claude-sonnet-4",
        "blockData": {
            "usage": {
                "prompt_tokens": 1000,
                "completion_tokens": 200,
                "total_tokens": 1200,
                "cached_input_tokens": 300,
                "cache_creation_input_tokens": 100
            }
        }
    })
    .to_string();
    let usage_final = serde_json::json!({
        "actualUseModelName": "claude-sonnet-4",
        "blockData": {
            "usage": {
                "prompt_tokens": 5000,
                "completion_tokens": 800,
                "total_tokens": 5800,
                "cached_input_tokens": 2000,
                "cache_creation_input_tokens": 500
            }
        }
    })
    .to_string();

    let tmp = std::env::temp_dir().join(format!(
        "openhub-catpawai-dedup-test-{}.sqlite",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    {
        let conn = rusqlite::Connection::open(&tmp).unwrap();
        conn.execute_batch(
            "CREATE TABLE t_ui_messages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    conversation_id TEXT NOT NULL,
                    message_type TEXT NOT NULL,
                    create_time INTEGER NOT NULL,
                    content TEXT NOT NULL
                );
                CREATE TABLE t_conversations (
                    conversation_id TEXT PRIMARY KEY,
                    workspace_id TEXT,
                    title TEXT
                );",
        )
        .unwrap();
        conn.execute(
                "INSERT INTO t_ui_messages (conversation_id, message_type, create_time, content) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params!["conv-dedup", "chat_item", 1_786_594_260_000i64, usage_partial],
            )
            .unwrap();
        conn.execute(
                "INSERT INTO t_ui_messages (conversation_id, message_type, create_time, content) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params!["conv-dedup", "chat_item", 1_786_594_260_000i64, usage_final],
            )
            .unwrap();
    }

    let buckets = read_catpawai_buckets_from_path(&tmp).unwrap();
    assert_eq!(buckets.len(), 1, "should have exactly 1 bucket after dedup");
    let bucket = &buckets[0];
    assert_eq!(bucket.request_count, 1, "should dedup to 1 request");
    // total = 全新输入 + 缓存命中 + 输出；缓存写入(500)独立上报，不计入 total
    assert_eq!(bucket.total_tokens, 7800, "should keep the larger total");
    assert_eq!(bucket.input_tokens, 5000, "should keep the larger prompt");
    assert_eq!(
        bucket.cached_input_tokens, 2000,
        "should keep the larger cache_read"
    );
    assert_eq!(bucket.cache_creation_input_tokens, 500);

    let _ = fs::remove_file(&tmp);
}

#[test]
fn parses_token_stats_empty_payload() {
    let payload = r#"{
            "available": true,
            "session_count": 0,
            "sessions": [],
            "summary": {
                "sessions": 0,
                "productive_sessions": 0,
                "one_shot_sessions": 0,
                "edit_turns": 0,
                "retries": 0,
                "total_tokens": 0,
                "cost_usd": 0,
                "edit_tokens": 0,
                "edit_cost_usd": 0,
                "productive_rate": 0,
                "one_shot_rate": null,
                "edit_sessions": 0,
                "first_pass_sessions": 0,
                "edit_session_rate": 0,
                "first_pass_rate": null,
                "tokens_per_edit": null,
                "cost_per_edit": null
            },
            "by_model": [],
            "subagents": [],
            "provenance": {}
        }"#;
    let report: TokenStatsReport = serde_json::from_str(payload).unwrap();
    assert!(report.available);
    assert_eq!(report.sessions.len(), 0);
    assert_eq!(report.summary.productive_rate, 0.0);
}

// Activity tests

#[test]
fn hour_key_normalizes_variants() {
    assert_eq!(
        hour_key_from_ts("2026-08-06T04:59:44.123Z").as_deref(),
        Some("2026-08-06T04:00:00.000Z")
    );
    assert_eq!(
        hour_key_from_ts("2026-08-06T04:59:44Z").as_deref(),
        Some("2026-08-06T04:00:00.000Z")
    );
    assert_eq!(
        hour_key_from_ts("2026-08-06T04:59:44+08:00").as_deref(),
        Some("2026-08-05T20:00:00.000Z")
    );
    assert_eq!(
        hour_key_from_ts("2026-08-06T04:59:44+0800").as_deref(),
        Some("2026-08-05T20:00:00.000Z")
    );
    assert_eq!(
        hour_key_from_ts("2026-08-06T23:59:44-05:00").as_deref(),
        Some("2026-08-07T04:00:00.000Z")
    );
}

#[test]
fn claude_user_is_human_filters_non_user_input() {
    let tool_only = json!([{"type": "tool_result", "content": "ok"}]);
    assert!(!crate::token::collector::claude_user_is_human(&tool_only));

    let text = json!([{"type": "text", "text": "hello"}]);
    assert!(crate::token::collector::claude_user_is_human(&text));

    let plain = json!("hello");
    assert!(crate::token::collector::claude_user_is_human(&plain));

    let interrupted =
        json!([{"type": "text", "text": "[Request interrupted by user for tool use]"}]);
    assert!(!crate::token::collector::claude_user_is_human(&interrupted));

    let stdout =
        json!([{"type": "text", "text": "<local-command-stdout>ok</local-command-stdout>"}]);
    assert!(!crate::token::collector::claude_user_is_human(&stdout));
    let cmd = json!("<command-name>/compact</command-name>");
    assert!(crate::token::collector::claude_user_is_human(&cmd));
}

#[test]
fn assistant_tokens_positive_reads_nested() {
    let value = json!({"tokens": {"input": 10, "output": 0, "reasoning": 0}});
    assert!(assistant_tokens_positive(&value));
    let empty = json!({"tokens": {"input": 0, "output": 0}});
    assert!(!assistant_tokens_positive(&empty));
}

#[test]
fn claude_on_line_skips_sidechain_transcripts() {
    let mut map = BTreeMap::new();
    let mut sources = BTreeMap::new();
    let mut last_user_hour = None;
    let mut counted = HashSet::new();
    claude_on_line(
        &json!({
            "type": "user",
            "timestamp": "2026-08-06T04:00:00.000Z",
            "isSidechain": true,
            "message": {"role": "user", "content": "subagent prompt"}
        }),
        &mut map,
        &mut sources,
        &mut last_user_hour,
        &mut counted,
    );
    assert_eq!(map.values().map(|a| a.dialogues).sum::<i64>(), 0);
    claude_on_line(
        &json!({
            "type": "user",
            "timestamp": "2026-08-06T04:00:00.000Z",
            "message": {"role": "user", "content": "hello"}
        }),
        &mut map,
        &mut sources,
        &mut last_user_hour,
        &mut counted,
    );
    assert_eq!(map.values().map(|a| a.dialogues).sum::<i64>(), 1);
}

#[test]
fn claude_sidechain_requests_counted_but_not_dialogues() {
    let mut map = BTreeMap::new();
    let mut sources = BTreeMap::new();
    let mut last_user_hour = None;
    let mut counted = HashSet::new();
    claude_on_line(
        &json!({
            "type": "user",
            "timestamp": "2026-08-06T04:00:00.000Z",
            "message": {"role": "user", "content": "hello"}
        }),
        &mut map,
        &mut sources,
        &mut last_user_hour,
        &mut counted,
    );
    claude_on_line(
        &json!({
            "type": "assistant",
            "timestamp": "2026-08-06T04:05:00.000Z",
            "isSidechain": true,
            "message": {"id": "chatcmpl-side-1", "usage": {"input_tokens": 100, "output_tokens": 50}}
        }),
        &mut map,
        &mut sources,
        &mut last_user_hour,
        &mut counted,
    );
    let hour = map.get("2026-08-06T04:00:00.000Z").expect("bucket exists");
    assert_eq!(hour.dialogues, 1);
    assert_eq!(hour.requests, 1);
}

#[test]
fn claude_dedupes_same_message_id_lines() {
    let mut map = BTreeMap::new();
    let mut sources = BTreeMap::new();
    let mut last_user_hour = None;
    let mut counted = HashSet::new();
    let line = json!({
        "type": "assistant",
        "timestamp": "2026-08-06T04:00:00.000Z",
        "message": {"id": "chatcmpl-dup", "usage": {"input_tokens": 100, "output_tokens": 50}}
    });
    claude_on_line(
        &line,
        &mut map,
        &mut sources,
        &mut last_user_hour,
        &mut counted,
    );
    claude_on_line(
        &line,
        &mut map,
        &mut sources,
        &mut last_user_hour,
        &mut counted,
    );
    let hour = map.get("2026-08-06T04:00:00.000Z").expect("bucket exists");
    assert_eq!(hour.requests, 1);
}

#[test]
fn claude_origin_kind_filters_non_human() {
    let mut map = BTreeMap::new();
    let mut sources = BTreeMap::new();
    let mut last_user_hour = None;
    let mut counted = HashSet::new();
    claude_on_line(
        &json!({
            "type": "user",
            "timestamp": "2026-08-06T04:00:00.000Z",
            "origin": {"kind": "task-notification"},
            "message": {"role": "user", "content": "background task done"}
        }),
        &mut map,
        &mut sources,
        &mut last_user_hour,
        &mut counted,
    );
    assert_eq!(map.values().map(|a| a.dialogues).sum::<i64>(), 0);
    claude_on_line(
        &json!({
            "type": "user",
            "timestamp": "2026-08-06T04:00:00.000Z",
            "origin": {"kind": "human"},
            "message": {"role": "user", "content": "hello"}
        }),
        &mut map,
        &mut sources,
        &mut last_user_hour,
        &mut counted,
    );
    assert_eq!(map.values().map(|a| a.dialogues).sum::<i64>(), 1);
}

#[test]
fn claude_request_anchors_to_last_user_hour() {
    let mut map = BTreeMap::new();
    let mut sources = BTreeMap::new();
    let mut last_user_hour = None;
    let mut counted = HashSet::new();
    claude_on_line(
        &json!({
            "type": "user",
            "timestamp": "2026-08-06T04:10:00.000Z",
            "message": {"role": "user", "content": "hello"}
        }),
        &mut map,
        &mut sources,
        &mut last_user_hour,
        &mut counted,
    );
    claude_on_line(
        &json!({
            "type": "assistant",
            "timestamp": "2026-08-06T05:20:00.000Z",
            "message": {"usage": {"input_tokens": 10, "output_tokens": 20}}
        }),
        &mut map,
        &mut sources,
        &mut last_user_hour,
        &mut counted,
    );
    let hour04 = map
        .get("2026-08-06T04:00:00.000Z")
        .expect("request anchored to 04:00");
    assert_eq!(hour04.dialogues, 1);
    assert_eq!(hour04.requests, 1);
    assert!(!map.contains_key("2026-08-06T05:00:00.000Z"));
}

#[test]
fn dsh_activity_counts_user_and_assistant_messages() {
    let mut map = BTreeMap::new();
    let mut sources = BTreeMap::new();
    let mut last_user_hour = None;

    dsh_on_line(
        &json!({"type": "user/message", "time": 1786687444127u64}),
        &mut map,
        &mut sources,
        &mut last_user_hour,
    );
    dsh_on_line(
        &json!({"type": "user/message", "time": 1786687445000u64,
                    "data": {"source": {"kind": "user"}}}),
        &mut map,
        &mut sources,
        &mut last_user_hour,
    );
    dsh_on_line(
        &json!({"type": "user/message", "time": 1786687446000u64,
                    "data": {"source": {"kind": "plugin"}, "content": [{"type": "text", "text": "background job bash-1 finished"}]}}),
        &mut map,
        &mut sources,
        &mut last_user_hour,
    );
    dsh_on_line(
        &json!({"type": "user/message", "time": 1786687447000u64,
                    "data": {"content": [{"type": "text", "text": "<system-reminder>…"}]}}),
        &mut map,
        &mut sources,
        &mut last_user_hour,
    );
    dsh_on_line(
        &json!({
            "type": "assistant/message",
            "time": 1786687449831u64,
            "data": {"usage": {"inputTokens": 8740, "outputTokens": 228}}
        }),
        &mut map,
        &mut sources,
        &mut last_user_hour,
    );
    dsh_on_line(
        &json!({"type": "assistant/message", "time": 1786687449831u64, "data": {}}),
        &mut map,
        &mut sources,
        &mut last_user_hour,
    );
    dsh_on_line(
        &json!({"type": "tool/call", "time": 1786687449831u64}),
        &mut map,
        &mut sources,
        &mut last_user_hour,
    );

    assert_eq!(map.values().map(|a| a.dialogues).sum::<i64>(), 2);
    assert_eq!(map.values().map(|a| a.requests).sum::<i64>(), 1);
    assert_eq!(map.values().map(|a| a.success).sum::<i64>(), 1);
    let dsh = sources.get("dsh").expect("dsh source should exist");
    assert_eq!(dsh.dialogues, 2);
    assert_eq!(dsh.requests, 1);
}

#[test]
fn jsonl_incremental_only_reads_new_bytes() {
    use std::io::Write;
    let dir = std::env::temp_dir().join(format!("openhub-tt-jsonl-{}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    let file = dir.join("rollout-test-1.jsonl");
    let mut map: BTreeMap<String, HealthAgg> = BTreeMap::new();
    let mut sources: BTreeMap<String, HealthAgg> = BTreeMap::new();
    let mut cursors = FileCursorMap::new();

    let line1 = r#"{"type":"event_msg","timestamp":"2026-08-03T09:10:00.000Z","payload":{"type":"user_message"}}"#;
    let mut f = fs::File::create(&file).unwrap();
    writeln!(f, "{line1}").unwrap();
    drop(f);
    collect_codex_activity_incremental(&dir, &mut map, &mut sources, &mut cursors);
    assert_eq!(map.values().map(|a| a.dialogues).sum::<i64>(), 1);

    let line2 = r#"{"type":"event_msg","timestamp":"2026-08-03T09:11:00.000Z","payload":{"type":"token_count"}}"#;
    let mut f = fs::OpenOptions::new().append(true).open(&file).unwrap();
    writeln!(f, "{line2}").unwrap();
    drop(f);
    collect_codex_activity_incremental(&dir, &mut map, &mut sources, &mut cursors);
    assert_eq!(map.values().map(|a| a.dialogues).sum::<i64>(), 1);
    assert_eq!(map.values().map(|a| a.requests).sum::<i64>(), 1);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn command_code_activity_counts_v2_and_v3_messages() {
    let mut map = BTreeMap::new();
    let mut sources = BTreeMap::new();
    command_code_on_line(
        &json!({
            "id": "u1",
            "timestamp": "2026-07-14T03:00:00.000Z",
            "role": "user",
            "content": [{"type": "text", "text": "hello"}],
            "metadata": {"version": 2}
        }),
        &mut map,
        &mut sources,
    );
    command_code_on_line(
        &json!({
            "id": "a1",
            "timestamp": "2026-07-14T03:01:00.000Z",
            "role": "assistant",
            "content": [{"type": "text", "text": "hi"}],
            "metadata": {"version": 2}
        }),
        &mut map,
        &mut sources,
    );
    command_code_on_line(
        &json!({
            "type": "message",
            "id": "a2",
            "timestamp": "2026-08-12T04:01:00.000Z",
            "message": {"role": "assistant", "content": []},
            "usage": {"inputTokens": 10, "outputTokens": 2}
        }),
        &mut map,
        &mut sources,
    );

    assert_eq!(map.values().map(|value| value.dialogues).sum::<i64>(), 1);
    assert_eq!(map.values().map(|value| value.requests).sum::<i64>(), 2);
    assert_eq!(map.values().map(|value| value.success).sum::<i64>(), 2);
    let source = sources.get("command-code").unwrap();
    assert_eq!(source.dialogues, 1);
    assert_eq!(source.requests, 2);
    assert_eq!(source.success, 2);
}

#[test]
fn kiro_activity_uses_request_ids_and_ignores_credit_amount() {
    let mut map = BTreeMap::new();
    let mut sources = BTreeMap::new();
    kiro_on_line(
        &json!({
            "timestamp": "2026-08-13T05:00:00.000Z",
            "payload": {"type": "user", "content": "hello"}
        }),
        &mut map,
        &mut sources,
    );
    kiro_on_line(
        &json!({
            "timestamp": "2026-08-13T05:00:02.000Z",
            "payload": {
                "type": "usage_summary",
                "status": "success",
                "requestIds": ["a", "b"],
                "promptTurnSummaries": [{"unit": "credit", "usage": 99.0}]
            }
        }),
        &mut map,
        &mut sources,
    );
    let source = sources.get("kiro").unwrap();
    assert_eq!(source.dialogues, 1);
    assert_eq!(source.requests, 2);
    assert_eq!(source.success, 2);
    assert_eq!(source.failed, 0);
}

#[test]
fn command_code_activity_filter_excludes_checkpoint_files() {
    assert!(is_command_code_activity_file(Path::new("session.jsonl")));
    assert!(!is_command_code_activity_file(Path::new(
        "session.checkpoints.jsonl"
    )));
    assert!(!is_command_code_activity_file(Path::new(
        "session.prompts.jsonl"
    )));
    assert!(!is_command_code_activity_file(Path::new("history.jsonl")));
}

#[test]
fn sqlite_cursor_only_counts_new_rows() {
    let db_path = std::env::temp_dir().join(format!("openhub-tt-sqlite-{}.db", std::process::id()));
    let _ = fs::remove_file(&db_path);
    let conn = Connection::open(&db_path).unwrap();
    conn.execute_batch("CREATE TABLE message (session_id TEXT, time_created INTEGER, data TEXT);")
        .unwrap();

    let mut map: BTreeMap<String, HealthAgg> = BTreeMap::new();
    let mut sources: BTreeMap<String, HealthAgg> = BTreeMap::new();
    let mut cursor = SqliteCursor::default();

    let insert = |conn: &Connection, sid: &str, tc: i64, role: &str| {
        let data = format!(
            r#"{{"role":"{role}","tokens":{{"input":10,"output":5}},"time":{{"created":{tc}}}}}"#
        );
        conn.execute(
            "INSERT INTO message (session_id, time_created, data) VALUES (?1, ?2, ?3)",
            rusqlite::params![sid, tc, data],
        )
        .unwrap();
    };
    insert(&conn, "s1", 1000, "user");
    insert(&conn, "s1", 2000, "assistant");
    collect_sqlite_message_activity_incremental(
        &db_path,
        "opencode",
        &mut map,
        &mut sources,
        None,
        &mut cursor,
    );
    assert_eq!(map.values().map(|a| a.dialogues).sum::<i64>(), 1);
    assert_eq!(map.values().map(|a| a.requests).sum::<i64>(), 1);

    insert(&conn, "s2", 3000, "user");
    collect_sqlite_message_activity_incremental(
        &db_path,
        "opencode",
        &mut map,
        &mut sources,
        None,
        &mut cursor,
    );
    assert_eq!(map.values().map(|a| a.dialogues).sum::<i64>(), 2);
    assert_eq!(map.values().map(|a| a.requests).sum::<i64>(), 1);
    let _ = fs::remove_file(&db_path);
}

#[test]
fn mimo_cursor_replays_users_when_session_becomes_allowed() {
    let db_path = std::env::temp_dir().join(format!("openhub-tt-mimo-{}.db", std::process::id()));
    let _ = fs::remove_file(&db_path);
    let conn = Connection::open(&db_path).unwrap();
    conn.execute_batch("CREATE TABLE message (session_id TEXT, time_created INTEGER, data TEXT);")
        .unwrap();
    let mut map: BTreeMap<String, HealthAgg> = BTreeMap::new();
    let mut sources: BTreeMap<String, HealthAgg> = BTreeMap::new();
    let mut cursor = SqliteCursor::default();
    let allow = HashSet::from(["mimo", "xiaomi"]);

    let insert = |conn: &Connection, sid: &str, tc: i64, data: &str| {
        conn.execute(
            "INSERT INTO message (session_id, time_created, data) VALUES (?1, ?2, ?3)",
            rusqlite::params![sid, tc, data],
        )
        .unwrap();
    };

    insert(
        &conn,
        "s1",
        1000,
        r#"{"role":"user","time":{"created":1000}}"#,
    );
    collect_sqlite_message_activity_incremental(
        &db_path,
        "mimo",
        &mut map,
        &mut sources,
        Some(&allow),
        &mut cursor,
    );
    assert_eq!(map.values().map(|a| a.dialogues).sum::<i64>(), 0);

    insert(
        &conn,
        "s1",
        2000,
        r#"{"role":"assistant","providerID":"mimo","tokens":{"input":10,"output":5},"time":{"created":2000}}"#,
    );
    collect_sqlite_message_activity_incremental(
        &db_path,
        "mimo",
        &mut map,
        &mut sources,
        Some(&allow),
        &mut cursor,
    );
    assert_eq!(map.values().map(|a| a.dialogues).sum::<i64>(), 1);
    assert_eq!(map.values().map(|a| a.requests).sum::<i64>(), 1);
    let _ = fs::remove_file(&db_path);
}
