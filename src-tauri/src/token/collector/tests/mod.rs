use super::*;
use crate::models::TokenSessionTokens;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[test]
fn zcode_provider_filter_matches_vendor_segments_only() {
    assert!(zcode_provider_allowed("builtin:zai-start-plan"));
    assert!(zcode_provider_allowed(
        "b87fa901-a05f-4afa-8c18-beccb90f9ef6"
    ));
    assert!(!zcode_provider_allowed(""));
    assert!(!zcode_provider_allowed("anthropic"));
    assert!(!zcode_provider_allowed("builtin:anthropic"));
    assert!(!zcode_provider_allowed("openai/gpt-5"));
    assert!(zcode_provider_allowed("my-openai-relay"));
    assert!(zcode_provider_allowed("anthropic-proxy-123"));
}

#[test]
fn zcode_database_extracts_nested_model_on_user_turn() {
    let user_msg = json!({
        "role": "user",
        "model": {
            "providerID": "builtin:zai-start-plan",
            "modelID": "GLM-5.3",
            "variant": "max"
        }
    });
    assert_eq!(zcode_model(&user_msg), "GLM-5.3");
    assert_eq!(zcode_provider(&user_msg), "builtin:zai-start-plan");
}

#[test]
fn codex_usage_normalization_infers_inclusive_vs_independent() {
    // 原生 OpenAI Responses 口径（实测占主导，99/113 会话）：input_tokens 为**总输入**，
    // cached 是其中的子集，上游 total = input + output。
    // 旧实现把 cached 当独立分量再叠加一次：命中率从 cached/input（≈98%）腰斩成
    // cached/(input+cached)（≈50%），total 虚高约 44%——这正是「Codex 缓存率过低」的根因。
    let native = CodexUsage {
        input_tokens: 469471,
        cached_input_tokens: 45888,
        output_tokens: 867,
        total_tokens: 470338,
        ..Default::default()
    }
    .normalized();
    assert_eq!(native.input_tokens, 469471 - 45888);
    assert_eq!(native.cached_input_tokens, 45888);
    assert_eq!(native.output_tokens, 867);
    assert_eq!(native.total_tokens, 470338);

    // Anthropic 类中转口径：三分量独立（cached 可大于 input），total = input + cached + output。
    let relay = CodexUsage {
        input_tokens: 822,
        cached_input_tokens: 23872,
        output_tokens: 118,
        total_tokens: 24812,
        ..Default::default()
    }
    .normalized();
    assert_eq!(relay.input_tokens, 822);
    assert_eq!(relay.cached_input_tokens, 23872);
    assert_eq!(relay.total_tokens, 24812);

    // total 缺失（delta 途中被丢弃）时按 cached 是否为 input 子集判别。
    let no_total = CodexUsage {
        input_tokens: 40000,
        cached_input_tokens: 38000,
        output_tokens: 500,
        ..Default::default()
    }
    .normalized();
    assert_eq!(no_total.input_tokens, 2000);
    assert_eq!(no_total.total_tokens, 40500);

    // 恒等式：fresh + cached + output == total 在两种口径下都必须成立。
    for usage in [native, relay, no_total] {
        assert_eq!(
            usage.input_tokens + usage.cached_input_tokens + usage.output_tokens,
            usage.total_tokens
        );
    }
}

#[test]
fn codex_user_message_is_human_filters_system_context() {
    let env = json!({"content": [{"type": "input_text", "text": "<environment_context>  <cwd>/app</cwd>"}]});
    assert!(!codex_user_message_is_human(&env));

    let goal = json!({"content": [{"type": "input_text", "text": "<codex_internal_context source=\"goal\"> keep going"}]});
    assert!(!codex_user_message_is_human(&goal));

    let aborted =
        json!({"content": [{"type": "input_text", "text": "<turn_aborted> interrupted"}]});
    assert!(!codex_user_message_is_human(&aborted));

    let real = json!({"content": [{"type": "input_text", "text": "帮我修复这个 bug"}]});
    assert!(codex_user_message_is_human(&real));

    let svg = json!({"content": [{"type": "input_text", "text": "<svg width=\"24\">...</svg>"}]});
    assert!(codex_user_message_is_human(&svg));
}

#[test]
fn cache_round_trip_preserves_sessions() {
    let session = token_session(
        "session-1".to_string(),
        "codex",
        "OpenHub".to_string(),
        "gpt-test".to_string(),
        "2026-08-12T01:00:00.000Z".to_string(),
        "2026-08-12T01:01:00.000Z".to_string(),
        1,
        TokenSessionTokens {
            input_tokens: 10,
            output_tokens: 2,
            total_tokens: 12,
            ..Default::default()
        },
        0.0,
    );
    let envelope = CollectorEnvelope {
        version: CACHE_VERSION,
        files: BTreeMap::from([(
            "/tmp/session.jsonl".to_string(),
            CachedFile {
                sessions: vec![session],
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    let encoded = serde_json::to_string(&envelope).unwrap();
    let decoded: CollectorEnvelope = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.files["/tmp/session.jsonl"].sessions.len(), 1);
    assert_eq!(
        decoded.files["/tmp/session.jsonl"].sessions[0].total_tokens,
        12
    );
}

#[test]
fn duplicate_events_are_counted_once() {
    let event = UsageEvent {
        id: "request-1".to_string(),
        source: "claude".to_string(),
        model: "model-1".to_string(),
        project_key: "OpenHub".to_string(),
        timestamp: "2026-08-12T03:10:00.000Z".to_string(),
        input_tokens: 10,
        output_tokens: 2,
        total_tokens: 12,
        ..Default::default()
    };
    let report = aggregate_events(vec![event.clone(), event]);
    assert_eq!(report.buckets.len(), 1);
    assert_eq!(report.buckets[0].total_tokens, 12);
    assert_eq!(report.buckets[0].request_count, 1);
}

#[test]
fn aggregate_counts_requests_and_dialogues_separately() {
    let request_event = UsageEvent {
        id: "msg-1".to_string(),
        source: "claude".to_string(),
        model: "model-1".to_string(),
        project_key: "OpenHub".to_string(),
        timestamp: "2026-08-12T03:10:00.000Z".to_string(),
        input_tokens: 10,
        output_tokens: 2,
        total_tokens: 12,
        ..Default::default()
    };
    let user_event = UsageEvent {
        id: "u:user-1".to_string(),
        source: "claude".to_string(),
        model: "model-1".to_string(),
        project_key: "OpenHub".to_string(),
        timestamp: "2026-08-12T03:09:00.000Z".to_string(),
        conversation_count: 1,
        ..Default::default()
    };
    let report = aggregate_events(vec![request_event, user_event]);
    assert_eq!(report.buckets.len(), 1);
    assert_eq!(report.buckets[0].request_count, 1);
    assert_eq!(report.buckets[0].conversation_count, 1);
}

fn temp_command_code_dir(name: &str) -> PathBuf {
    let nonce = UNIX_EPOCH
        .elapsed()
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "openhub-command-code-{name}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn antigravity_model_token_parser_reads_supported_prefixes() {
    assert_eq!(
        find_ascii_model_token(b"\0\x01prefix claude-opus-4-6-thinking\0suffix"),
        "claude-opus-4-6"
    );
    assert_eq!(
        find_ascii_model_token(b"prefix gemini-3.6-flash-high suffix"),
        "gemini-3.6-flash"
    );
    assert_eq!(
        find_ascii_model_token(b"\xaa\x01\x17Gemini 3.7 Flash (High)\x00"),
        "gemini-3.7-flash"
    );
    assert_eq!(
        find_ascii_model_token(b"\xaa\x01\x14Gemini 3.1 Pro (Low)\x00"),
        "gemini-3.1-pro"
    );
    assert_eq!(
        find_ascii_model_token(b"\xaa\x01\x1cClaude Opus 4.6 (Thinking)\x00"),
        "claude-opus-4.6"
    );
    assert_eq!(find_ascii_model_token(b"\xaa\x01\x06GPT-4o\x00"), "gpt-4o");
    assert_eq!(
        find_ascii_model_token(b"\xaa\x01\x0bDeepSeek-V3\x00"),
        "deepseek-v3"
    );
    assert_eq!(find_ascii_model_token(b"no model here"), "");
}

#[test]
fn antigravity_transcript_estimates_planner_usage() {
    let dir = temp_command_code_dir("antigravity");
    let brain = dir.join("antigravity-ide").join("brain").join("session-ag");
    let path = brain
        .join(".system_generated")
        .join("logs")
        .join("transcript.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
            &path,
            concat!(
                r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-08-06T04:59:44Z","content":"hello"}"#,
                "\n",
                r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-08-06T04:59:45Z","content":"answer","thinking":"reason","tool_calls":[{"name":"list_dir","args":{"DirectoryPath":"/tmp"}}]}"#,
                "\n",
                r#"{"step_index":2,"source":"MODEL","type":"LIST_DIRECTORY","status":"DONE","created_at":"2026-08-06T04:59:46Z","content":"tool result"}"#,
                "\n"
            ),
        )
        .unwrap();

    let parsed = parse_antigravity_file(&path);
    assert_eq!(parsed.sessions.len(), 1);
    let session = &parsed.sessions[0];
    assert_eq!(session.source, "antigravity");
    assert_eq!(session.turns, 1);
    assert!(session.total_tokens > 0);
    assert_eq!(session.tokens.total_tokens, session.total_tokens);
    assert!(session.tokens.reasoning_output_tokens > 0);
    assert_eq!(
        session.provenance.get("tokenUsage"),
        Some(&json!("estimated-antigravity-local-context"))
    );
    let usage = parsed
        .events
        .iter()
        .find(|event| event.estimated_tokens > 0)
        .unwrap();
    assert_eq!(usage.model, UNKNOWN_ANTIGRAVITY_MODEL);
    assert_eq!(usage.total_tokens, usage.estimated_tokens);
    assert!(usage.input_tokens > 0);
    assert!(usage.output_tokens > 0);
    assert!(usage.reasoning_output_tokens > 0);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn command_code_transcript_filter_excludes_sidecars() {
    assert!(is_command_code_transcript_path(Path::new("session.jsonl")));
    assert!(!is_command_code_transcript_path(Path::new(
        "session.checkpoints.jsonl"
    )));
    assert!(!is_command_code_transcript_path(Path::new(
        "session.prompts.jsonl"
    )));
    assert!(!is_command_code_transcript_path(Path::new("history.jsonl")));
    assert!(!is_command_code_transcript_path(Path::new(
        "session.meta.json"
    )));
}

#[test]
fn kiro_messages_estimate_visible_context_and_ignore_credit_summary() {
    let dir = temp_command_code_dir("kiro");
    let session_dir = dir.join("session-kiro");
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(
        session_dir.join("session.json"),
        r#"{"id":"sess-kiro","workspacePaths":["/tmp/OpenHub"],"modelId":"auto"}"#,
    )
    .unwrap();
    let path = session_dir.join("messages.jsonl");
    fs::write(
            &path,
            concat!(
                r#"{"id":"u1","timestamp":"2026-08-13T05:00:00.000Z","payload":{"type":"user","content":"hello"}}"#,
                "\n",
                r#"{"id":"t1","timestamp":"2026-08-13T05:00:01.000Z","payload":{"type":"tool_result","content":"local result"}}"#,
                "\n",
                r#"{"id":"a1","timestamp":"2026-08-13T05:00:02.000Z","payload":{"type":"assistant","content":"done"}}"#,
                "\n",
                r#"{"id":"s1","timestamp":"2026-08-13T05:00:03.000Z","payload":{"type":"usage_summary","status":"success","requestIds":["req-1"],"promptTurnSummaries":[{"unit":"credit","usage":1.2}]}}"#,
                "\n"
            ),
        )
        .unwrap();

    let parsed = parse_kiro_file(&path);
    assert_eq!(parsed.sessions.len(), 1);
    let session = &parsed.sessions[0];
    assert_eq!(session.source, "kiro");
    assert_eq!(session.project_key, "OpenHub");
    assert_eq!(session.model, "auto");
    assert_eq!(session.turns, 1);
    assert_eq!(parsed.events.len(), 2);
    let assistant = parsed.events.iter().find(|event| event.id == "a1").unwrap();
    assert!(assistant.estimated_tokens > 0);
    assert_eq!(assistant.total_tokens, assistant.estimated_tokens);
    assert_eq!(
        session.provenance.get("tokenUsage"),
        Some(&json!("estimated-kiro-local-context"))
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn kiro_v1_global_storage_session_is_parsed_on_legacy_macs() {
    let dir = temp_command_code_dir("kiro-v1");
    let path = dir.join("sess-intel.json");
    fs::write(
            &path,
            r#"{
                "title":"Intel Mac session",
                "sessionId":"sess-intel",
                "workspaceDirectory":"/Users/test/Projects/OpenHub",
                "selectedModel":"claude-sonnet",
                "createdAt":"2026-08-01T01:00:00.000Z",
                "history":[
                    {"timestamp":"2026-08-01T01:00:01.000Z","message":{"role":"user","content":"hello from Intel"}},
                    {"timestamp":"2026-08-01T01:00:02.000Z","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}},
                    {"timestamp":"2026-08-01T01:00:03.000Z","message":{"role":"system","content":"system context"}}
                ]
            }"#,
        )
        .unwrap();

    let parsed = parse_kiro_legacy_file(&path);
    assert_eq!(parsed.sessions.len(), 1);
    let session = &parsed.sessions[0];
    assert_eq!(session.session_hash, "openhub:kiro:sess-intel");
    assert_eq!(session.project_key, "OpenHub");
    assert_eq!(session.model, "claude-sonnet");
    assert_eq!(session.turns, 1);
    assert!(session.total_tokens > 0);
    assert_eq!(parsed.events.len(), 2);
    assert_eq!(
        session.provenance.get("storageFormat"),
        Some(&json!("kiro-global-storage-v1"))
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn kiro_v2_session_suppresses_same_id_legacy_copy() {
    let home = temp_command_code_dir("kiro-dedup");
    let v2_dir = home
        .join(".kiro")
        .join("sessions")
        .join("workspace")
        .join("sess-shared");
    fs::create_dir_all(&v2_dir).unwrap();
    fs::write(v2_dir.join("messages.jsonl"), "{}\n").unwrap();
    fs::write(v2_dir.join("session.json"), r#"{"id":"sess-shared"}"#).unwrap();

    let legacy_root = kiro_legacy_session_roots(&home).into_iter().next().unwrap();
    fs::create_dir_all(&legacy_root).unwrap();
    fs::write(
        legacy_root.join("shared.json"),
        r#"{"title":"old","history":[]}"#,
    )
    .unwrap();
    fs::write(
        legacy_root.join("sess-legacy-only.json"),
        r#"{"title":"old only","history":[]}"#,
    )
    .unwrap();

    let files = collect_kiro_source_files(&home);
    assert!(files.iter().any(|(source, path)| {
        source == "kiro"
            && path.file_name().and_then(|name| name.to_str()) == Some("messages.jsonl")
    }));
    assert!(!files.iter().any(|(source, path)| {
        source == "kiro-legacy"
            && path.file_name().and_then(|name| name.to_str()) == Some("shared.json")
    }));
    assert!(files.iter().any(|(source, path)| {
        source == "kiro-legacy"
            && path.file_name().and_then(|name| name.to_str()) == Some("sess-legacy-only.json")
    }));
    let _ = fs::remove_dir_all(home);
}

#[test]
fn command_code_v2_estimates_tokens_from_local_visible_context() {
    let dir = temp_command_code_dir("v2");
    let path = dir.join("session-v2.jsonl");
    fs::write(
            &path,
            concat!(
                r#"{"id":"user-1","timestamp":"2026-07-14T03:00:00.000Z","sessionId":"session-v2","role":"user","content":[{"type":"text","text":"hello"}],"metadata":{"version":2}}"#,
                "\n",
                r#"{"id":"assistant-1","timestamp":"2026-07-14T03:01:00.000Z","sessionId":"session-v2","role":"assistant","content":[{"type":"text","text":"hi"}],"metadata":{"version":2}}"#,
                "\n",
                r#"{"id":"tool-1","timestamp":"2026-07-14T03:02:00.000Z","sessionId":"session-v2","role":"tool","content":[{"type":"text","text":"a long local tool result"}],"metadata":{"version":2}}"#,
                "\n",
                r#"{"id":"assistant-2","timestamp":"2026-07-14T03:03:00.000Z","sessionId":"session-v2","role":"assistant","content":[{"type":"text","text":"done"}],"metadata":{"version":2}}"#,
                "\n"
            ),
        )
        .unwrap();

    let parsed = parse_command_code_file(&path);
    assert_eq!(parsed.sessions.len(), 1);
    assert_eq!(parsed.sessions[0].turns, 1);
    assert!(parsed.sessions[0].total_tokens > 0);
    assert_eq!(parsed.events.len(), 3);
    assert_eq!(parsed.events[0].conversation_count, 1);
    let estimates = parsed
        .events
        .iter()
        .filter(|event| event.estimated_tokens > 0)
        .collect::<Vec<_>>();
    assert_eq!(estimates.len(), 2);
    assert!(estimates
        .iter()
        .all(|estimate| estimate.total_tokens == estimate.estimated_tokens));
    assert!(estimates[1].input_tokens > estimates[0].input_tokens);
    assert!(estimates.iter().all(|estimate| estimate.output_tokens > 0));
    assert_eq!(
        parsed.sessions[0].provenance.get("tokenUsage"),
        Some(&json!("estimated-v2-local-context"))
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn command_code_v3_reads_exact_usage_and_sidecar_model() {
    let dir = temp_command_code_dir("v3");
    let path = dir.join("session-v3.jsonl");
    fs::write(
        command_code_meta_path(&path),
        r#"{"model":"deepseek/deepseek-v4-pro"}"#,
    )
    .unwrap();
    fs::write(
            &path,
            concat!(
                r#"{"type":"session","version":3,"id":"session-v3","timestamp":"2026-08-12T01:00:00.000Z","cwd":"/tmp/OpenHub"}"#,
                "\n",
                r#"{"type":"message","id":"user-1","parentId":null,"timestamp":"2026-08-12T01:01:00.000Z","message":{"role":"user","content":[{"type":"text","text":"hello"}]}}"#,
                "\n",
                r#"{"type":"message","id":"assistant-1","parentId":"user-1","timestamp":"2026-08-12T01:02:00.000Z","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]},"usage":{"inputTokens":100,"outputTokens":20,"cacheReadTokens":30,"cacheWriteTokens":5,"costUsd":0.25}}"#,
                "\n"
            ),
        )
        .unwrap();

    let parsed = parse_command_code_file(&path);
    assert_eq!(parsed.sessions.len(), 1);
    let session = &parsed.sessions[0];
    assert_eq!(session.source, "command-code");
    assert_eq!(session.project_key, "OpenHub");
    assert_eq!(session.model, "deepseek/deepseek-v4-pro");
    assert_eq!(session.turns, 1);
    assert_eq!(session.tokens.input_tokens, 100);
    assert_eq!(session.tokens.cached_input_tokens, 30);
    assert_eq!(session.tokens.cache_creation_input_tokens, 5);
    assert_eq!(session.tokens.output_tokens, 20);
    // total = 全新输入 + 缓存命中 + 输出；缓存写入(5)独立上报，不计入 total
    assert_eq!(session.total_tokens, 150);
    assert!((session.cost_usd - 0.25).abs() < f64::EPSILON);
    assert_eq!(
        session.provenance.get("tokenUsage"),
        Some(&json!("observed-v3"))
    );
    let usage_event = parsed
        .events
        .iter()
        .find(|event| event.id == "assistant-1")
        .unwrap();
    assert_eq!(usage_event.total_tokens, 150);
    assert_eq!(usage_event.estimated_tokens, 0);
    assert!(usage_event.pricing_available);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn half_hour_bucket_rounds_down() {
    assert_eq!(
        half_hour_key("2026-08-12T03:29:59.000Z").as_deref(),
        Some("2026-08-12T03:00:00.000Z")
    );
    assert_eq!(
        half_hour_key("2026-08-12T03:30:01.000Z").as_deref(),
        Some("2026-08-12T03:30:00.000Z")
    );
}

#[test]
fn half_hour_bucket_normalizes_timezone_offset() {
    assert_eq!(
        half_hour_key("2026-08-12T03:29:59.000+08:00").as_deref(),
        Some("2026-08-11T19:00:00.000Z")
    );
    assert_eq!(
        half_hour_key("2026-08-12T23:45:00.000-05:00").as_deref(),
        Some("2026-08-13T04:30:00.000Z")
    );
    assert_eq!(tz_offset_secs("2026-08-12T03:29:59.000Z"), 0);
    assert_eq!(tz_offset_secs("2026-08-12T03:29:59+08:00"), 28_800);
    assert_eq!(tz_offset_secs("2026-08-12T03:29:59-0500"), -18_000);
    assert_eq!(tz_offset_secs("2026-08-12T03:29:59.000"), 0);
}

#[test]
fn claude_user_line_origin_kind_refines_human_detection() {
    let line = json!({
        "origin": {"kind": "human"},
        "message": {"role": "user", "content": "hello"}
    });
    assert!(claude_user_line_is_human(
        &line,
        &line["message"]["content"]
    ));
    let notification = json!({
        "origin": {"kind": "task-notification"},
        "message": {"role": "user", "content": "background task done"}
    });
    assert!(!claude_user_line_is_human(
        &notification,
        &notification["message"]["content"]
    ));
    let legacy = json!({"message": {"role": "user", "content": "hello"}});
    assert!(claude_user_line_is_human(
        &legacy,
        &legacy["message"]["content"]
    ));
}

#[test]
fn claude_user_is_human_excludes_injected_messages() {
    assert!(claude_user_is_human(
        &json!([{"type": "text", "text": "hello"}])
    ));
    assert!(claude_user_is_human(&json!("hello")));
    assert!(!claude_user_is_human(
        &json!([{"type": "tool_result", "content": "ok"}])
    ));
    assert!(claude_user_is_human(&json!(
        "<command-name>/compact</command-name>"
    )));
    assert!(!claude_user_is_human(&json!(
        [{"type": "text", "text": "<local-command-stdout>done</local-command-stdout>"}]
    )));
    assert!(!claude_user_is_human(&json!(
        [{"type": "text", "text": "<command-stdout>done</command-stdout>"}]
    )));
    assert!(!claude_user_is_human(&json!(
        [{"type": "text", "text": "[Request interrupted by user for tool use]"}]
    )));
    assert!(!claude_user_is_human(&json!([
        {"type": "tool_result", "content": "ok"},
        {"type": "text", "text": "<system-reminder>…</system-reminder>"}
    ])));
}

#[test]
fn claude_turns_attach_to_their_own_assistant_model() {
    let dir = std::env::temp_dir().join(format!("openhub-claude-model-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("session.jsonl");
    fs::write(
            &path,
            concat!(
                r#"{"type":"user","uuid":"u1","timestamp":"2026-08-13T05:00:00.000Z","message":{"role":"user","content":"hello"}}"#,
                "\n",
                r#"{"type":"assistant","timestamp":"2026-08-13T05:00:01.000Z","message":{"model":"model-a","usage":{"input_tokens":10,"output_tokens":20}}}"#,
                "\n",
                r#"{"type":"user","uuid":"u2","timestamp":"2026-08-13T05:01:00.000Z","message":{"role":"user","content":"second"}}"#,
                "\n",
                r#"{"type":"assistant","timestamp":"2026-08-13T05:01:01.000Z","message":{"model":"model-b","usage":{"input_tokens":5,"output_tokens":5}}}"#,
                "\n",
            ),
        )
        .unwrap();

    let parsed = parse_claude_file(&path);
    let u1 = parsed
        .events
        .iter()
        .find(|e| e.id == "u:u1")
        .expect("u1 event");
    let u2 = parsed
        .events
        .iter()
        .find(|e| e.id == "u:u2")
        .expect("u2 event");
    assert_eq!(u1.model, "model-a");
    assert_eq!(u2.model, "model-b");
    assert_eq!(u1.conversation_count, 1);
    assert_eq!(u2.conversation_count, 1);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn dsh_user_is_human_only_counts_real_user_kind() {
    assert!(dsh_user_is_human(&json!({
        "data": {"source": {"kind": "user"}, "content": [{"type": "text", "text": "hi"}]}
    })));
    assert!(dsh_user_is_human(&json!({"data": {"content": "hi"}})));
    assert!(!dsh_user_is_human(&json!({
        "data": {"source": {"kind": "plugin"}, "content": [{"type": "text", "text": "background job finished"}]}
    })));
    assert!(!dsh_user_is_human(&json!({
        "data": {"source": {"kind": "skill-catalog"}, "content": [{"type": "text", "text": "<system-reminder>…</system-reminder>"}]}
    })));
    assert!(!dsh_user_is_human(&json!({
        "data": {"content": [{"type": "text", "text": "Current runtime context. This snapshot supersedes…"}]}
    })));
}

#[test]
fn copilot_model_normalization_cleans_vendor_and_provider_prefixes() {
    assert_eq!(
        normalize_copilot_model_name("deepseek/deepseek-v4-flash"),
        "deepseek-v4-flash"
    );
    assert_eq!(
        normalize_copilot_model_name("opencodezen/deepseek-v4-flash-free"),
        "deepseek-v4-flash-free"
    );
    // opencode-copilot-chat 扩展的动态模型 ID：三段式标识符 + vendor 前缀 + 会话戳
    assert_eq!(
        normalize_copilot_model_name(
            "opencodezen/OpenCode Zen/opencodezen:x-preview-f-free::session-2026-05-21-b"
        ),
        "x-preview-f-free"
    );
    assert_eq!(
        normalize_copilot_model_name("opencodezen:x-preview-f-free::session-2026-05-21-b"),
        "x-preview-f-free"
    );
    assert_eq!(
        normalize_copilot_model_name("opencodezen:big-pickle::session-2026-06-01-a"),
        "big-pickle"
    );
    // 纯净模型名不受清洗影响
    assert_eq!(
        normalize_copilot_model_name("ox-alpha-free"),
        "ox-alpha-free"
    );
    assert_eq!(
        normalize_copilot_model_name("agent-host-claude:@provider=anthropic:sonnet"),
        "claude-3-7-sonnet"
    );
    assert_eq!(
        normalize_copilot_model_name("agent-host-claude:@provider=anthropic:fable"),
        "claude-3-5-haiku"
    );
    assert_eq!(
        normalize_copilot_model_name("claude-haiku-4.5"),
        "claude-haiku-4.5"
    );
    assert_eq!(normalize_copilot_model_name("gpt-4o"), "gpt-4o");
    assert_eq!(normalize_copilot_model_name(""), UNKNOWN_COPILOT_MODEL);
}

#[test]
fn copilot_vscode_chat_session_parses_tokens_and_dialogues() {
    let dir = temp_command_code_dir("copilot");
    let path = dir.join("chat.jsonl");
    fs::write(
        &path,
        json!({
            "kind": 0,
            "v": {
                "sessionId": "test-copilot-123",
                "creationDate": 1787214440974i64,
                "customTitle": "测试 Copilot 会话",
                "requests": [
                    {
                        "requestId": "req-1",
                        "message": { "text": "写一个快速排序" },
                        "promptTokens": 1500,
                        "completionTokens": 200,
                        "cachedTokens": 500,
                        "modelId": "deepseek/deepseek-v4-flash",
                        "response": [
                            {
                                "kind": "thinking",
                                "value": "思考如何用 Rust 写快排..."
                            },
                            {
                                "value": "这是快速排序的代码..."
                            }
                        ]
                    }
                ]
            }
        })
        .to_string(),
    )
    .unwrap();

    let parsed = parse_copilot_file(&path);
    assert_eq!(parsed.sessions.len(), 1);
    let s = &parsed.sessions[0];
    assert_eq!(s.source, "copilot");
    assert_eq!(s.session_hash, "openhub:copilot:test-copilot-123");
    assert_eq!(s.turns, 1);
    // promptTokens=1500 为上游总量（含 cachedTokens=500），input 必须是拆分后的全新输入，
    // 否则 input+cached+output 会双计缓存（total = fresh + cached + output = 1000+500+200）。
    assert_eq!(s.tokens.input_tokens, 1000);
    assert_eq!(s.tokens.output_tokens, 200);
    assert_eq!(s.tokens.cached_input_tokens, 500);
    assert_eq!(s.tokens.total_tokens, 1700);
    assert!(s.tokens.reasoning_output_tokens > 0);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn copilot_delta_operation_log_replays_requests() {
    // VS Code 新版格式：首行全量快照（requests 为空），请求以 kind:2 追加、
    // token 数以 kind:1 按路径补写。旧解析器只能读到空 requests，统计全部丢失。
    let dir = temp_command_code_dir("copilot-delta");
    let path = dir.join("chat.jsonl");
    let lines = [
        json!({
            "kind": 0,
            "v": {
                "version": 3,
                "sessionId": "delta-session-1",
                "creationDate": 1787619825144i64,
                "requests": [],
                "inputState": {
                    "selectedModel": { "identifier": "claude-sonnet-4.5" }
                }
            }
        })
        .to_string(),
        json!({
            "kind": 2,
            "k": ["requests"],
            "v": [{
                "requestId": "req-delta-1",
                "message": { "text": "用 Rust 写一个快速排序" },
                "modelId": "claude-sonnet-4.5",
                "response": [{ "value": "快速排序代码如下……" }]
            }]
        })
        .to_string(),
        json!({ "kind": 1, "k": ["requests", 0, "promptTokens"], "v": 3200 }).to_string(),
        json!({ "kind": 1, "k": ["requests", 0, "completionTokens"], "v": 480 }).to_string(),
        json!({ "kind": 1, "k": ["requests", 0, "cachedTokens"], "v": 800 }).to_string(),
        // 第二条请求：无显式 token，应按文本长度估算。
        json!({
            "kind": 2,
            "k": ["requests"],
            "v": [{
                "requestId": "req-delta-2",
                "message": { "text": "再优化一下边界条件" },
                "response": [{ "value": "已补充边界条件处理。" }]
            }]
        })
        .to_string(),
    ];
    fs::write(&path, lines.join("\n")).unwrap();

    let parsed = parse_copilot_file(&path);
    assert_eq!(parsed.sessions.len(), 1);
    let s = &parsed.sessions[0];
    assert_eq!(s.session_hash, "openhub:copilot:delta-session-1");
    assert_eq!(s.turns, 2);
    // 第一条请求的补写 token 必须被回放出来；第二条请求无显式 token，按文本字节数估算。
    // promptTokens=3200 为上游总量（含 cachedTokens=800），input 记拆分后的全新输入。
    let req2_prompt_estimate = ("再优化一下边界条件".len() as i64 / 4).max(1) + 128;
    let req2_output_estimate = ("已补充边界条件处理。".len() as i64 / 4).max(1);
    assert_eq!(s.tokens.input_tokens, (3200 - 800) + req2_prompt_estimate);
    assert_eq!(s.tokens.output_tokens, 480 + req2_output_estimate);
    assert_eq!(s.tokens.cached_input_tokens, 800);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn workspace_project_key_normalization_rules() {
    assert_eq!(
        normalize_workspace_project_key("00071cc1-b2c0-46d5-8053-828995d94944", "Codex"),
        "临时任务 / 独立会话"
    );
    assert_eq!(
        normalize_workspace_project_key(
            "file:///Users/wusuoming/Documents/IdeaProjects/sz-v4.code-workspace",
            "CatPawAI"
        ),
        "sz-v4"
    );
    assert_eq!(
        normalize_workspace_project_key("/Applications/custom/OpenHub/src-tauri", "OpenHub"),
        "OpenHub"
    );
    assert_eq!(
        decode_encoded_dash_path("-Applications-custom-dsh-client"),
        "dsh-client"
    );
    assert_eq!(
        decode_encoded_dash_path(
            "-Users-wusuoming--copilot-chats-08b47634-e580-4133-b163-2ebefb43f8e3"
        ),
        "临时任务 / 独立会话"
    );
}

#[test]
fn cline_parser_reads_tokens_and_cost() {
    let dir = std::env::temp_dir().join(format!(
        "openhub_cline_test_{}",
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("ui_messages.json");
    fs::write(
        &path,
        json!([
            {
                "ts": 1724000000000_i64,
                "type": "say",
                "say": "user_feedback",
                "text": "Hello Cline"
            },
            {
                "ts": 1724000001000_i64,
                "type": "say",
                "say": "api_req_started",
                "apiConfiguration": { "apiModelId": "claude-3-7-sonnet" },
                "tokens": {
                    "tokensIn": 2500,
                    "tokensOut": 450,
                    "cacheReads": 1200,
                    "cacheWrites": 300,
                    "totalCost": 0.035
                }
            }
        ])
        .to_string(),
    )
    .unwrap();

    let parsed = parse_cline_file("cline", &path);
    assert_eq!(parsed.sessions.len(), 1);
    let s = &parsed.sessions[0];
    assert_eq!(s.source, "cline");
    assert_eq!(s.tokens.input_tokens, 2500);
    assert_eq!(s.tokens.output_tokens, 450);
    assert_eq!(s.tokens.cached_input_tokens, 1200);
    assert_eq!(s.tokens.cache_creation_input_tokens, 300);
    assert!((s.cost_usd - 0.035).abs() < 0.001);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn continue_parser_reads_tokens_and_messages() {
    let dir = std::env::temp_dir().join(format!(
        "openhub_continue_test_{}",
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("session.json");
    fs::write(
        &path,
        json!({
            "sessionId": "cont-12345",
            "modelTitle": "gpt-4o",
            "workspaceDirectory": "/Users/test/workspace/my-app",
            "history": [
                { "role": "user", "content": "Help me fix bug", "promptTokens": 120 },
                { "role": "assistant", "content": "Fixed!", "completionTokens": 50 }
            ]
        })
        .to_string(),
    )
    .unwrap();

    let parsed = parse_continue_file(&path);
    assert_eq!(parsed.sessions.len(), 1);
    let s = &parsed.sessions[0];
    assert_eq!(s.source, "continue");
    assert_eq!(s.project_key, "my-app");
    assert_eq!(s.tokens.input_tokens, 120);
    assert_eq!(s.tokens.output_tokens, 50);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn aider_parser_extracts_tokens_from_chat_history() {
    let dir = std::env::temp_dir().join(format!(
        "openhub_aider_test_{}",
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::create_dir_all(&dir);
    let path = dir.join(".aider.chat.history.md");
    fs::write(
            &path,
            "# Aider chat\n#### claude-3-5-sonnet\n> Tokens: 3.5k sent, 620 received. Cost: $0.02 message, $0.05 session.\n",
        ).unwrap();

    let parsed = parse_aider_file(&path);
    assert_eq!(parsed.sessions.len(), 1);
    let s = &parsed.sessions[0];
    assert_eq!(s.source, "aider");
    assert_eq!(s.tokens.input_tokens, 3500);
    assert_eq!(s.tokens.output_tokens, 620);
    assert!((s.cost_usd - 0.02).abs() < 0.001);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn goose_and_catpawai_parsers_parse_valid_records() {
    let dir = std::env::temp_dir().join(format!(
        "openhub_misc_test_{}",
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::create_dir_all(&dir);
    let goose_file = dir.join("goose.jsonl");
    fs::write(
        &goose_file,
        format!(
            "{}\n",
            json!({
                "model": "gpt-4o",
                "role": "user",
                "content": "query"
            })
        ) + &json!({
            "model": "gpt-4o",
            "role": "assistant",
            "usage": { "prompt_tokens": 800, "completion_tokens": 150 }
        })
        .to_string(),
    )
    .unwrap();

    let parsed_goose = parse_goose_file(&goose_file);
    assert_eq!(parsed_goose.sessions.len(), 1);
    assert_eq!(parsed_goose.sessions[0].tokens.input_tokens, 800);
    assert_eq!(parsed_goose.sessions[0].tokens.output_tokens, 150);

    let catpaw_file = dir.join("catpawai.jsonl");
    fs::write(
        &catpaw_file,
        json!({
            "model": "deepseek-coder",
            "role": "assistant",
            "usage": { "prompt_tokens": 500, "completion_tokens": 200 }
        })
        .to_string(),
    )
    .unwrap();

    let parsed_catpaw = parse_catpawai_file("catpawai", &catpaw_file);
    assert_eq!(parsed_catpaw.sessions.len(), 1);
    assert_eq!(parsed_catpaw.sessions[0].tokens.input_tokens, 500);
    assert_eq!(parsed_catpaw.sessions[0].tokens.output_tokens, 200);

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn catpawai_database_parser_extracts_sessions_and_normalized_events() {
    let dir = std::env::temp_dir().join(format!(
        "openhub_catpawai_collector_test_{}",
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::create_dir_all(&dir);
    let db_path = dir.join("globalCache.sqlite");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        r#"
            CREATE TABLE t_conversations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                conversation_id TEXT NOT NULL UNIQUE,
                parent_conversation_id TEXT DEFAULT NULL,
                title TEXT DEFAULT NULL,
                mis TEXT NOT NULL DEFAULT '',
                version TEXT NOT NULL DEFAULT '',
                starred INTEGER DEFAULT 0,
                workspace_id TEXT DEFAULT NULL,
                ide_type TEXT DEFAULT NULL,
                create_time INTEGER DEFAULT 1782541719482,
                update_time INTEGER DEFAULT 1783384584742
            );
            CREATE TABLE t_ui_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                conversation_id TEXT NOT NULL,
                message_id TEXT NOT NULL,
                message_type TEXT NOT NULL,
                message_sub_type TEXT DEFAULT NULL,
                sub_conversation_id TEXT DEFAULT NULL,
                content TEXT DEFAULT NULL,
                create_time INTEGER DEFAULT 1782541723492,
                update_time INTEGER DEFAULT 1782541723492,
                status INTEGER DEFAULT 1
            );
            "#,
    )
    .unwrap();

    conn.execute(
            "INSERT INTO t_conversations (conversation_id, workspace_id, title, create_time, update_time) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                "conv-agent",
                "/Users/wusuoming/Documents/IdeaProjects/ai-agent",
                "代码逻辑分析与优化建议",
                1_782_541_719_482i64,
                1_783_384_584_742i64,
            ],
        )
        .unwrap();

    // 1. user prompt
    let prompt_payload = serde_json::json!({
        "messageId": "msg-prompt-1",
        "messageType": "user_prompt",
        "selectedModelName": "glm-5.2"
    })
    .to_string();
    conn.execute(
            "INSERT INTO t_ui_messages (conversation_id, message_id, message_type, create_time, content) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["conv-agent", "msg-prompt-1", "user_prompt", 1_782_541_720_000i64, prompt_payload],
        ).unwrap();

    // 2. tool message with Format 2 tokenUsage (fresh prompt 1134, cacheReadTokens 10201, output 138)
    let tool_payload = serde_json::json!({
        "messageId": "msg-tool-1",
        "messageType": "tool",
        "actualUseModelName": "glm-5.2",
        "tokenUsage": {
            "prompt_tokens": 1134,
            "cacheReadTokens": 10201,
            "completion_tokens": 138,
            "total_tokens": 1272
        }
    })
    .to_string();
    conn.execute(
            "INSERT INTO t_ui_messages (conversation_id, message_id, message_type, create_time, content) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["conv-agent", "msg-tool-1", "tool", 1_782_541_725_000i64, tool_payload],
        ).unwrap();

    let parsed = crate::token::collector::sources::catpawai::parse_catpawai_database(&db_path);
    assert_eq!(parsed.sessions.len(), 1);
    let session = &parsed.sessions[0];
    assert_eq!(session.session_hash, "openhub:catpawai:conv-agent");
    assert_eq!(session.source, "catpawai");
    assert_eq!(session.project_key, "ai-agent");
    assert_eq!(session.model, "glm-5.2");
    assert_eq!(session.turns, 1);
    assert_eq!(session.tokens.input_tokens, 1134);
    assert_eq!(session.tokens.cached_input_tokens, 10201);
    assert_eq!(session.tokens.output_tokens, 138);
    assert_eq!(session.tokens.total_tokens, 11473);

    // Events
    assert_eq!(parsed.events.len(), 2);
    let prompt_event = &parsed.events[0];
    assert_eq!(prompt_event.conversation_count, 1);
    let usage_event = &parsed.events[1];
    assert_eq!(usage_event.input_tokens, 1134);
    assert_eq!(usage_event.cached_input_tokens, 10201);
    assert_eq!(usage_event.total_tokens, 11473);

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn catpawai_real_db_parses_if_exists() {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap();
    let db_paths = crate::token::collector::sources::catpawai::catpawai_db_paths(&home);
    if let Some(db_path) = db_paths.first() {
        let parsed = crate::token::collector::sources::catpawai::parse_catpawai_database(db_path);
        println!(
            "CatPawAI real DB test: found {} sessions, {} events",
            parsed.sessions.len(),
            parsed.events.len()
        );
        assert!(!parsed.events.is_empty(), "Real DB should have events");
        assert!(!parsed.sessions.is_empty(), "Real DB should have sessions");
        let total_fresh: i64 = parsed.events.iter().map(|e| e.input_tokens).sum();
        let total_cache: i64 = parsed.events.iter().map(|e| e.cached_input_tokens).sum();
        let total_out: i64 = parsed.events.iter().map(|e| e.output_tokens).sum();
        let total_all: i64 = parsed.events.iter().map(|e| e.total_tokens).sum();
        println!(
            "CatPawAI real DB totals: Fresh={}, Cache={}, Output={}, Total={}",
            total_fresh, total_cache, total_out, total_all
        );
        assert_eq!(total_fresh + total_cache + total_out, total_all);
    }
}

/// 本机装有 opencode 族工具时，用真实 DB 验证统一口径恒等式：
/// fresh + cached + output == total（zcode 为含缓存口径、opencode/mimo 为独立口径）。
#[test]
fn opencode_family_real_db_identity_if_exists() {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap();
    let cases = [
        (
            "zcode",
            crate::token::collector::sources::zcode::zcode_db_path(&home),
            crate::token::collector::sources::zcode::parse_zcode_database
                as fn(&std::path::Path) -> CachedDatabase,
        ),
        (
            "opencode",
            opencode_db_path(&home),
            parse_opencode_database as fn(&std::path::Path) -> CachedDatabase,
        ),
        (
            "mimo",
            mimo_db_path(&home),
            parse_mimo_database as fn(&std::path::Path) -> CachedDatabase,
        ),
    ];
    let mut checked = 0usize;
    for (name, path, parse) in cases {
        if !path.is_file() {
            continue;
        }
        checked += 1;
        let parsed = parse(&path);
        let fresh: i64 = parsed.events.iter().map(|e| e.input_tokens).sum();
        let cached: i64 = parsed.events.iter().map(|e| e.cached_input_tokens).sum();
        let out: i64 = parsed.events.iter().map(|e| e.output_tokens).sum();
        let total: i64 = parsed.events.iter().map(|e| e.total_tokens).sum();
        let write: i64 = parsed
            .events
            .iter()
            .map(|e| e.cache_creation_input_tokens)
            .sum();
        println!(
            "{name} real DB: events={}, Fresh={}, CacheRead={}, CacheWrite={}, Output={}, Total={}",
            parsed.events.len(),
            fresh,
            cached,
            write,
            out,
            total
        );
        assert_eq!(
            fresh + cached + out,
            total,
            "{name}: total 必须等于 fresh + cached + output"
        );
        for event in &parsed.events {
            assert!(
                event.input_tokens >= 0 && event.cached_input_tokens >= 0,
                "{name}: 拆分不得产生负值"
            );
        }
    }
    if checked == 0 {
        println!("本机未安装 opencode 族工具，跳过真实 DB 探针");
    }
}

/// 本机有 Codex 会话时，验证真实数据上的恒等式 fresh + cached + output == total
/// 与非负拆分（原生 inclusive 与中转 independent 两种口径混合存在，按事件判别）。
#[test]
fn codex_real_sessions_identity_if_exists() {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap();
    let base = codex_home(&home);
    let mut files = Vec::new();
    for root in [base.join("sessions"), base.join("archived_sessions")] {
        collect_jsonl_files(
            &root,
            &|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
                    .unwrap_or(false)
            },
            &mut files,
        );
    }
    if files.is_empty() {
        println!("本机无 Codex 会话，跳过真实探针");
        return;
    }
    let mut checked = 0usize;
    let mut fresh_sum = 0i64;
    let mut cached_sum = 0i64;
    for path in files.iter().rev().take(16) {
        let parsed = parse_codex_file(path);
        if parsed.events.is_empty() {
            continue;
        }
        checked += 1;
        for event in &parsed.events {
            assert_eq!(
                event.input_tokens + event.cached_input_tokens + event.output_tokens,
                event.total_tokens,
                "codex 事件恒等式破坏: {} {}",
                event.id,
                event.model
            );
            assert!(event.input_tokens >= 0, "fresh 拆分不得为负");
            fresh_sum += event.input_tokens;
            cached_sum += event.cached_input_tokens;
        }
    }
    println!(
        "codex real files checked: {checked}, fresh={fresh_sum}, cached={cached_sum}, hit={:.1}%",
        100.0 * cached_sum as f64 / (fresh_sum + cached_sum).max(1) as f64
    );
    assert!(checked > 0, "存在 rollout 文件但未解析出事件");
}

#[test]
fn copilot_transcript_counts_every_agent_turn_as_request() {
    let dir = std::env::temp_dir().join(format!("openhub-test-transcript-{}", std::process::id()));
    let transcripts = dir
        .join("ws-hash")
        .join("GitHub.copilot-chat")
        .join("transcripts");
    fs::create_dir_all(&transcripts).unwrap();
    let path = transcripts.join("abc.jsonl");
    let lines = [
        json!({"type":"session.start","data":{"sessionId":"ses-1","startTime":"2026-08-25T01:38:44.725Z"},"timestamp":"2026-08-25T01:38:44.725Z"}),
        json!({"type":"user.message","data":{"content":"修复这个bug"},"id":"u1","timestamp":"2026-08-25T01:39:00.000Z"}),
        // agent 第一轮：思考 + 工具调用
        json!({"type":"assistant.message","data":{"messageId":"m1","content":"","toolRequests":[{"toolCallId":"c1","name":"read_file","arguments":"{\"filePath\":\"main.rs\"}"}],"reasoningText":"need to read the file first"},"timestamp":"2026-08-25T01:39:05.000Z"}),
        json!({"type":"assistant.turn_end","data":{"turnId":"0"},"id":"t1","timestamp":"2026-08-25T01:39:06.000Z"}),
        // agent 第二轮：最终回答
        json!({"type":"assistant.message","data":{"messageId":"m2","content":"已修复该问题。"},"timestamp":"2026-08-25T01:39:20.000Z"}),
        json!({"type":"assistant.turn_end","data":{"turnId":"1"},"id":"t2","timestamp":"2026-08-25T01:39:21.000Z"}),
    ];
    let text = lines
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join("\n");
    fs::write(&path, &text).unwrap();

    let cached = parse_copilot_file(&path);
    // transcript 只统计对话轮次；token/请求数由 vscode-opencode 精确日志负责（防双计）
    assert_eq!(
        cached
            .events
            .iter()
            .filter(|e| e.conversation_count == 1)
            .count(),
        1,
        "user.message 记一次对话"
    );
    assert!(
        !cached.events.iter().any(|e| e.total_tokens > 0),
        "transcript 不再产生估算 token 事件"
    );
    assert_eq!(cached.sessions.len(), 1);
    assert_eq!(cached.sessions[0].turns, 1);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn vscode_opencode_log_parses_precise_usage_with_cache_hits() {
    let dir = std::env::temp_dir().join(format!("openhub-test-oclog-{}", std::process::id()));
    let output_dir = dir.join("output_logging_20260825T093509");
    fs::create_dir_all(&output_dir).unwrap();
    let path = output_dir.join("6-OpenCode.log");
    let text = [
        "[activate] extension host ready",
        "[request] url=https://opencode.ai/zen/v1/chat/completions payloadBytes=94005",
        "[stream-summary model=x-preview-f-free] textChars=69 toolCalls=0 reasoningChars=173",
        "[response-summary] status=200 durationMs=11845 ttfbMs=7238 promptTokens=45646 completionTokens=278 totalTokens=45924 cachedTokens=45376 finishReason=stop totalBytes=18169 totalEvents=87",
        "[retry] transient 503 (attempt 1/2); retrying in 1119ms…",
        "[stream-summary model=big-pickle] textChars=0 toolCalls=1 reasoningChars=113",
        "[response-summary] status=200 durationMs=9000 ttfbMs=1000 promptTokens=23055 completionTokens=603 totalTokens=23658 cachedTokens=0 finishReason=tool_calls totalBytes=38330 totalEvents=170",
        "[response-summary] status=503 durationMs=10 promptTokens=999 completionTokens=9 totalTokens=1008 cachedTokens=0",
    ]
    .join("\n");
    fs::write(&path, &text).unwrap();

    let cached = parse_vscode_opencode_log_file(&path);
    assert_eq!(cached.events.len(), 2, "仅成功的 response-summary 计入");
    let first = &cached.events[0];
    assert_eq!(first.source, "vscode-opencode");
    assert_eq!(first.model, "x-preview-f-free");
    assert_eq!(first.cached_input_tokens, 45376, "缓存命中必须保留");
    assert_eq!(first.input_tokens, 45646 - 45376, "input 扣除缓存命中部分");
    assert_eq!(first.output_tokens, 278);
    assert_eq!(first.total_tokens, 45924);
    assert_eq!(first.estimated_tokens, 0, "精确数据不应标记为估算");
    assert_eq!(cached.events[1].model, "big-pickle");
    assert_eq!(cached.events[1].cached_input_tokens, 0);
    // 时间取自 output_logging_ 目录名（本地时区）
    assert!(!cached.events[0].timestamp.is_empty());
    assert!(cached.events[0].timestamp.starts_with("2026-08-25T"));
    assert_eq!(cached.sessions.len(), 1);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn normalize_usage_covers_all_input_semantics() {
    use crate::token::collector::types::InputSemantics;

    // Fresh：input 即全新输入（Anthropic / codex / opencode / mimo 语义）
    let (fresh, read, write, out, reasoning, total) = normalize_usage(RawUsage {
        input: 100,
        semantics: InputSemantics::Fresh,
        cache_read: 80,
        cache_write: 30,
        output: 10,
        reasoning: 6,
    });
    assert_eq!((fresh, read, write, out, reasoning, total), (100, 80, 30, 10, 6, 190));

    // InclusiveOfCacheRead：OpenAI 语义，prompt 含缓存命中，必须拆分避免双计
    let (fresh, read, _write, out, _r, total) = normalize_usage(RawUsage {
        input: 1500,
        semantics: InputSemantics::InclusiveOfCacheRead,
        cache_read: 500,
        output: 200,
        ..Default::default()
    });
    assert_eq!((fresh, read, out, total), (1000, 500, 200, 1700));

    // InclusiveOfAllCache：zcode 语义，input 已含缓存读（真实样本 input=15474, read=11776）
    let (fresh, read, _write, out, _r, total) = normalize_usage(RawUsage {
        input: 15474,
        semantics: InputSemantics::InclusiveOfAllCache,
        cache_read: 11776,
        output: 122,
        ..Default::default()
    });
    assert_eq!((fresh, read, out, total), (3698, 11776, 122, 15596));

    // 负数防御：input < cached 时 fresh 不得为负
    let (fresh, _read, _write, _out, _r, total) = normalize_usage(RawUsage {
        input: 822,
        semantics: InputSemantics::InclusiveOfCacheRead,
        cache_read: 23872,
        output: 118,
        ..Default::default()
    });
    assert_eq!(fresh, 0);
    assert_eq!(total, 23872 + 118);

    // 恒等式：fresh + read + output == total（三种语义均须成立）
    for semantics in [
        InputSemantics::Fresh,
        InputSemantics::InclusiveOfCacheRead,
        InputSemantics::InclusiveOfAllCache,
    ] {
        let (f, r, _w, o, _rr, t) = normalize_usage(RawUsage {
            input: 900,
            semantics,
            cache_read: 300,
            cache_write: 120,
            output: 45,
            ..Default::default()
        });
        assert_eq!(f + r + o, t, "semantics={semantics:?}");
    }
}

#[test]
fn openai_details_helpers_read_camel_and_snake() {
    let camel = json!({"promptTokensDetails": {"cachedTokens": 45376},
                        "completionTokensDetails": {"reasoningTokens": 88}});
    assert_eq!(openai_cached_from_details(&camel), 45376);
    assert_eq!(openai_reasoning_from_details(&camel), 88);

    let snake = json!({"prompt_tokens_details": {"cached_tokens": 12},
                        "completion_tokens_details": {"reasoning_tokens": 3}});
    assert_eq!(openai_cached_from_details(&snake), 12);
    assert_eq!(openai_reasoning_from_details(&snake), 3);

    assert_eq!(openai_cached_from_details(&json!({})), 0);
}

#[test]
fn goose_parser_splits_cached_tokens_from_openai_prompt() {
    let dir = std::env::temp_dir().join(format!(
        "openhub_goose_cache_test_{}",
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("goose-cache.jsonl");
    fs::write(
        &path,
        json!({
            "model": "gpt-5",
            "role": "assistant",
            "usage": {
                "prompt_tokens": 45646,
                "completion_tokens": 278,
                "prompt_tokens_details": { "cached_tokens": 45376 }
            }
        })
        .to_string(),
    )
    .unwrap();

    let parsed = parse_goose_file(&path);
    assert_eq!(parsed.sessions.len(), 1);
    let s = &parsed.sessions[0];
    // prompt_tokens 含缓存时必须拆分：fresh = 45646 - 45376 = 270，避免 fresh+cached 双计。
    assert_eq!(s.tokens.input_tokens, 270);
    assert_eq!(s.tokens.cached_input_tokens, 45376);
    assert_eq!(s.tokens.output_tokens, 278);
    assert_eq!(s.tokens.total_tokens, 270 + 45376 + 278);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn cursor_usage_keeps_input_plus_output_equal_total() {
    use crate::token::collector::sources::cursor::cursor_usage;
    use crate::token::collector::types::normalize_usage;

    // 只有 tokenCount：input 补满 total，output 为 0
    let (input, cached, _w, output, _r, total) =
        normalize_usage(cursor_usage(&json!({"tokenCount": 1200}), 1200));
    assert_eq!((input, cached, output, total), (1200, 0, 0, 1200));

    // input + output 齐全：原样保留
    let (input, _c, _w, output, _r, total) = normalize_usage(cursor_usage(
        &json!({"tokenCount": 1200, "inputTokens": 900, "outputTokens": 300}),
        1200,
    ));
    assert_eq!((input, output, total), (900, 300, 1200));

    // 只有 output：input 由 total 倒推
    let (input, _c, _w, output, _r, total) = normalize_usage(cursor_usage(
        &json!({"tokenCount": 1200, "outputTokens": 300}),
        1200,
    ));
    assert_eq!((input, output, total), (900, 300, 1200));

    // 带缓存明细：promptTokens=1500 含 cached=500，fresh=1000
    let (input, cached, _w, output, _r, total) = normalize_usage(cursor_usage(
        &json!({
            "tokenCount": 1700,
            "promptTokens": 1500,
            "outputTokens": 200,
            "prompt_tokens_details": { "cached_tokens": 500 }
        }),
        1700,
    ));
    assert_eq!((input, cached, output, total), (1000, 500, 200, 1700));
}

#[test]
fn catpawai_normalize_uses_raw_total_to_avoid_double_count() {
    // 格式 2（网关独立式）：cacheReadTokens 独立上报，prompt 即全新输入。
    let (fresh, cached, _w, out, _r, total) = normalize_catpawai_usage_numbers(
        1134, 138, 1272, 10201, 0, 0, 0,
    );
    assert_eq!((fresh, cached, out, total), (1134, 10201, 138, 11473));

    // 格式 1（OpenAI 嵌入式）：cached_tokens 在 details 里，prompt 含缓存需扣减。
    let (fresh, cached, _w, out, _r, total) = normalize_catpawai_usage_numbers(
        5000, 800, 5800, 0, 0, 2000, 0,
    );
    assert_eq!((fresh, cached, out, total), (3000, 2000, 800, 5800));

    // 两缓存字段并存 + raw_total 证明缓存独立计入：prompt 维持全新输入，不双扣。
    let (fresh, cached, _w, out, _r, total) = normalize_catpawai_usage_numbers(
        1134, 138, 1134 + 10201 + 138, 10201, 0, 10201, 0,
    );
    assert_eq!((fresh, cached, out, total), (1134, 10201, 138, 11473));

    // 仅 total 可用：以总量扣缓存拆分。
    let (fresh, cached, _w, out, _r, total) =
        normalize_catpawai_usage_numbers(0, 0, 6000, 1000, 0, 0, 0);
    assert_eq!((fresh, cached, out, total), (5000, 1000, 0, 6000));
}
