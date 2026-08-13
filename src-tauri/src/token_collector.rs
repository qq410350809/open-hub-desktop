use crate::models::{
    TokenCollectorSyncReport, TokenModelStat, TokenSession, TokenSessionTokens, TokenStatsReport,
    TokenSummary, TokenUsageBucket, TokenUsageReport,
};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

const CACHE_VERSION: i64 = 6;
const CACHE_TTL: Duration = Duration::from_secs(5);
const UNKNOWN_CODEX_MODEL: &str = "codex-unknown-model";
const UNKNOWN_CLAUDE_MODEL: &str = "claude-unknown-model";
const UNKNOWN_OPENCODE_MODEL: &str = "opencode-unknown-model";
const UNKNOWN_COMMAND_CODE_MODEL: &str = "command-code-unknown-model";
const UNKNOWN_ANTIGRAVITY_MODEL: &str = "antigravity-unknown-model";
const UNKNOWN_KIRO_MODEL: &str = "kiro-auto-model";
const LOCAL_ESTIMATED_CONTEXT_LIMIT: i64 = 64_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct FileFingerprint {
    size: u64,
    modified_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct DatabaseFingerprint {
    database: FileFingerprint,
    wal: FileFingerprint,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct UsageEvent {
    id: String,
    source: String,
    model: String,
    project_key: String,
    timestamp: String,
    input_tokens: i64,
    cached_input_tokens: i64,
    cache_creation_input_tokens: i64,
    output_tokens: i64,
    reasoning_output_tokens: i64,
    total_tokens: i64,
    conversation_count: i64,
    cost_usd: f64,
    pricing_available: bool,
    estimated_tokens: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct CachedFile {
    fingerprint: FileFingerprint,
    events: Vec<UsageEvent>,
    sessions: Vec<TokenSession>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct CachedDatabase {
    fingerprint: DatabaseFingerprint,
    events: Vec<UsageEvent>,
    sessions: Vec<TokenSession>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct CollectorEnvelope {
    version: i64,
    updated_at: String,
    files: BTreeMap<String, CachedFile>,
    databases: BTreeMap<String, CachedDatabase>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CollectedData {
    pub(crate) usage: TokenUsageReport,
    pub(crate) sessions: Vec<TokenSession>,
    pub(crate) changed: bool,
    pub(crate) scanned_files: usize,
    pub(crate) reused_files: usize,
}

struct CollectorMemoryCache {
    data: CollectedData,
    fetched_at: Instant,
}

fn memory_cache() -> &'static Mutex<Option<CollectorMemoryCache>> {
    static CACHE: OnceLock<Mutex<Option<CollectorMemoryCache>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

fn collector_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn collector_cache_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("OPENHUB_TOKEN_CACHE_PATH") {
        return Some(PathBuf::from(path));
    }
    let home = PathBuf::from(std::env::var_os("HOME")?);
    #[cfg(target_os = "macos")]
    {
        return Some(
            home.join("Library")
                .join("Application Support")
                .join("com.dfeer.openhub.desktop")
                .join("token-collector-cache.json"),
        );
    }
    #[cfg(target_os = "windows")]
    {
        return std::env::var_os("APPDATA").map(PathBuf::from).map(|path| {
            path.join("com.dfeer.openhub.desktop")
                .join("token-collector-cache.json")
        });
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Some(
            home.join(".local")
                .join("share")
                .join("com.dfeer.openhub.desktop")
                .join("token-collector-cache.json"),
        )
    }
}

fn fingerprint(path: &Path) -> FileFingerprint {
    let Ok(metadata) = fs::metadata(path) else {
        return FileFingerprint::default();
    };
    FileFingerprint {
        size: metadata.len(),
        modified_ms: metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_millis() as u64)
            .unwrap_or(0),
    }
}

fn is_command_code_transcript_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| {
            name.ends_with(".jsonl")
                && name != "history.jsonl"
                && !name.ends_with(".checkpoints.jsonl")
                && !name.ends_with(".prompts.jsonl")
        })
        .unwrap_or(false)
}

fn command_code_json_value<'a>(value: &'a JsonValue, keys: &[&str]) -> Option<&'a JsonValue> {
    for key in keys {
        if let Some(found) = value.get(*key) {
            return Some(found);
        }
    }
    None
}

fn command_code_usage(value: &JsonValue) -> Option<&JsonValue> {
    command_code_json_value(value, &["usage", "tokenUsage", "token_usage"])
        .filter(|usage| usage.is_object())
        .or_else(|| {
            value
                .get("message")
                .and_then(|message| {
                    command_code_json_value(message, &["usage", "tokenUsage", "token_usage"])
                })
                .filter(|usage| usage.is_object())
        })
        .or_else(|| {
            value
                .get("metadata")
                .and_then(|metadata| {
                    command_code_json_value(metadata, &["usage", "tokenUsage", "token_usage"])
                })
                .filter(|usage| usage.is_object())
        })
}

fn local_content_chars(value: &JsonValue) -> (i64, i64) {
    fn walk(value: &JsonValue, ascii: &mut i64, non_ascii: &mut i64) {
        match value {
            JsonValue::String(text) => {
                for ch in text.chars() {
                    if ch.is_ascii() {
                        *ascii += 1;
                    } else {
                        *non_ascii += 1;
                    }
                }
            }
            JsonValue::Array(items) => {
                for item in items {
                    walk(item, ascii, non_ascii);
                }
            }
            JsonValue::Object(fields) => {
                for item in fields.values() {
                    walk(item, ascii, non_ascii);
                }
            }
            _ => {}
        }
    }
    let mut ascii = 0i64;
    let mut non_ascii = 0i64;
    walk(value, &mut ascii, &mut non_ascii);
    (ascii, non_ascii)
}

fn estimate_local_content_tokens(content: &JsonValue) -> i64 {
    let (ascii, non_ascii) = local_content_chars(content);
    ascii
        .saturating_add(3)
        .div_euclid(4)
        .saturating_add(non_ascii)
        // 给 role / block type / 消息边界留少量协议开销。
        .saturating_add(4)
}

fn command_code_meta_path(path: &Path) -> PathBuf {
    path.with_extension("meta.json")
}

fn command_code_fingerprint(path: &Path) -> FileFingerprint {
    let transcript = fingerprint(path);
    let sidecar = fingerprint(&command_code_meta_path(path));
    FileFingerprint {
        size: transcript.size.saturating_add(sidecar.size),
        modified_ms: transcript.modified_ms.max(sidecar.modified_ms),
    }
}

fn antigravity_session_root(path: &Path) -> Option<PathBuf> {
    path.parent()?.parent()?.parent().map(Path::to_path_buf)
}

fn antigravity_session_id(path: &Path) -> String {
    antigravity_session_root(path)
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_string()
}

fn antigravity_data_root(path: &Path) -> Option<PathBuf> {
    antigravity_session_root(path)?
        .parent()?
        .parent()
        .map(Path::to_path_buf)
}

fn antigravity_database_path(path: &Path) -> Option<PathBuf> {
    let session_id = antigravity_session_id(path);
    if session_id.is_empty() {
        return None;
    }
    Some(
        antigravity_data_root(path)?
            .join("conversations")
            .join(format!("{session_id}.db")),
    )
}

fn antigravity_fingerprint(path: &Path) -> FileFingerprint {
    let transcript = fingerprint(path);
    let database = antigravity_database_path(path)
        .map(|database| database_fingerprint(&database))
        .unwrap_or_default();
    FileFingerprint {
        size: transcript
            .size
            .saturating_add(database.database.size)
            .saturating_add(database.wal.size),
        modified_ms: transcript
            .modified_ms
            .max(database.database.modified_ms)
            .max(database.wal.modified_ms),
    }
}

fn kiro_session_metadata_path(path: &Path) -> PathBuf {
    path.parent()
        .map(|parent| parent.join("session.json"))
        .unwrap_or_else(|| path.with_file_name("session.json"))
}

fn kiro_fingerprint(path: &Path) -> FileFingerprint {
    let transcript = fingerprint(path);
    let metadata = fingerprint(&kiro_session_metadata_path(path));
    FileFingerprint {
        size: transcript.size.saturating_add(metadata.size),
        modified_ms: transcript.modified_ms.max(metadata.modified_ms),
    }
}

fn source_file_fingerprint(source: &str, path: &Path) -> FileFingerprint {
    match source {
        "command-code" => command_code_fingerprint(path),
        "antigravity" => antigravity_fingerprint(path),
        "kiro" => kiro_fingerprint(path),
        _ => fingerprint(path),
    }
}

fn database_fingerprint(path: &Path) -> DatabaseFingerprint {
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    DatabaseFingerprint {
        database: fingerprint(path),
        wal: fingerprint(&wal),
    }
}

fn read_envelope() -> CollectorEnvelope {
    let Some(path) = collector_cache_path() else {
        return CollectorEnvelope::default();
    };
    let Ok(text) = fs::read_to_string(path) else {
        return CollectorEnvelope::default();
    };
    let Ok(envelope) = serde_json::from_str::<CollectorEnvelope>(&text) else {
        return CollectorEnvelope::default();
    };
    if envelope.version != CACHE_VERSION {
        return CollectorEnvelope::default();
    }
    envelope
}

fn write_envelope(envelope: &CollectorEnvelope) {
    let Some(path) = collector_cache_path() else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(bytes) = serde_json::to_vec(envelope) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, bytes).is_ok() {
        let _ = fs::rename(tmp, path);
    }
}

fn collect_jsonl_files(root: &Path, accept: &dyn Fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_files(&path, accept, out);
        } else if accept(&path) {
            out.push(path);
        }
    }
}

fn number(value: &JsonValue, keys: &[&str]) -> i64 {
    for key in keys {
        if let Some(value) = value.get(*key) {
            if let Some(number) = value.as_i64() {
                return number.max(0);
            }
            if let Some(number) = value.as_u64() {
                return (number.min(i64::MAX as u64)) as i64;
            }
            if let Some(number) = value.as_f64() {
                if number.is_finite() {
                    return (number as i64).max(0);
                }
            }
        }
    }
    0
}

fn float_number(value: &JsonValue, keys: &[&str]) -> f64 {
    for key in keys {
        if let Some(number) = value.get(*key).and_then(JsonValue::as_f64) {
            if number.is_finite() {
                return number.max(0.0);
            }
        }
    }
    0.0
}

fn basename_or_fallback(path: &str, fallback: &str) -> String {
    let trimmed = path.trim().trim_end_matches(['/', '\\']);
    if !trimmed.is_empty() {
        if let Some(name) = Path::new(trimmed)
            .file_name()
            .and_then(|name| name.to_str())
        {
            if !name.trim().is_empty() {
                return name.trim().to_string();
            }
        }
    }
    fallback.to_string()
}

fn claude_project_from_path(path: &Path) -> String {
    let raw = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .trim_matches('-');
    raw.rsplit('-')
        .find(|part| !part.trim().is_empty())
        .unwrap_or("Claude")
        .to_string()
}

fn command_code_project_from_path(path: &Path) -> String {
    let raw = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .trim_matches('-');
    raw.rsplit('-')
        .find(|part| !part.trim().is_empty())
        .unwrap_or("Command Code")
        .to_string()
}

fn command_code_sidecar_model(path: &Path) -> String {
    let Ok(text) = fs::read_to_string(command_code_meta_path(path)) else {
        return String::new();
    };
    serde_json::from_str::<JsonValue>(&text)
        .ok()
        .and_then(|value| {
            value
                .get("model")
                .and_then(JsonValue::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_default()
}

fn update_bounds(first: &mut String, last: &mut String, timestamp: &str) {
    if timestamp.is_empty() {
        return;
    }
    if first.is_empty() || timestamp < first.as_str() {
        *first = timestamp.to_string();
    }
    if last.is_empty() || timestamp > last.as_str() {
        *last = timestamp.to_string();
    }
}

fn half_hour_key(timestamp: &str) -> Option<String> {
    let value = timestamp.trim();
    if value.len() < 16 {
        return None;
    }
    let prefix = value.get(..13)?;
    if prefix.as_bytes().get(4) != Some(&b'-')
        || prefix.as_bytes().get(7) != Some(&b'-')
        || prefix.as_bytes().get(10) != Some(&b'T')
    {
        return None;
    }
    let minute = value.get(14..16)?.parse::<u32>().ok()?;
    Some(format!(
        "{prefix}:{:02}:00.000Z",
        if minute < 30 { 0 } else { 30 }
    ))
}

fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

fn iso_from_millis(ms: i64) -> String {
    if ms <= 0 {
        return String::new();
    }
    let seconds = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    let hour = time / 3600;
    let minute = (time % 3600) / 60;
    let second = time % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn duration_ms(started_at: &str, ended_at: &str) -> i64 {
    // 展示层目前不依赖精确时长；原始日志不保证统一时区格式，避免在此猜测。
    if started_at.is_empty() || ended_at.is_empty() || started_at == ended_at {
        0
    } else {
        0
    }
}

fn token_session(
    id: String,
    source: &str,
    project_key: String,
    model: String,
    started_at: String,
    ended_at: String,
    turns: i64,
    tokens: TokenSessionTokens,
    cost_usd: f64,
) -> TokenSession {
    let total_tokens = tokens.total_tokens;
    let duration = duration_ms(&started_at, &ended_at);
    TokenSession {
        version: 1,
        session_hash: format!("openhub:{source}:{id}"),
        source: source.to_string(),
        project_key,
        model,
        started_at,
        ended_at,
        active_ms: duration,
        turns,
        edit_turns: 0,
        retry_turns: 0,
        subagent_calls: 0,
        subagent_types: json!({}),
        tokens,
        provenance: json!({
            "source": "openhub-local-collector",
            "confidence": "observed",
            "privacy": "metadata-only"
        }),
        duration_ms: duration,
        total_tokens,
        cost_usd,
        productive: turns > 0 && total_tokens > 0,
        first_pass: false,
        one_shot: turns == 1,
        tokens_per_edit: None,
        cost_per_edit: None,
    }
}

fn claude_user_is_human(content: &JsonValue) -> bool {
    match content {
        JsonValue::String(text) => !text.trim().is_empty(),
        JsonValue::Array(items) => items.iter().any(|item| {
            matches!(
                item.get("type").and_then(JsonValue::as_str),
                Some("text") | Some("image")
            ) || item.get("text").and_then(JsonValue::as_str).is_some()
        }),
        _ => false,
    }
}

fn parse_claude_file(path: &Path) -> CachedFile {
    let Ok(text) = fs::read_to_string(path) else {
        return CachedFile {
            fingerprint: fingerprint(path),
            ..Default::default()
        };
    };
    let fallback_id = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_string();
    let mut session_id = fallback_id.clone();
    let mut project_key = claude_project_from_path(path);
    let mut model = String::new();
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut user_events: BTreeMap<String, String> = BTreeMap::new();
    let mut usage_events: BTreeMap<String, UsageEvent> = BTreeMap::new();

    for (index, line) in text.lines().enumerate() {
        let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };
        if value
            .get("isSidechain")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        if let Some(value) = value
            .get("sessionId")
            .or_else(|| value.get("session_id"))
            .and_then(JsonValue::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            session_id = value.to_string();
        }
        if let Some(cwd) = value
            .get("cwd")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            project_key = basename_or_fallback(cwd, &project_key);
        }
        let kind = value.get("type").and_then(JsonValue::as_str).unwrap_or("");
        if kind != "user" && kind != "assistant" {
            continue;
        }
        let timestamp = value
            .get("timestamp")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        update_bounds(&mut first_ts, &mut last_ts, &timestamp);

        if kind == "user" {
            let content = value
                .get("message")
                .and_then(|message| message.get("content"))
                .unwrap_or(&JsonValue::Null);
            if !claude_user_is_human(content) {
                continue;
            }
            let id = value
                .get("uuid")
                .and_then(JsonValue::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("{session_id}:user:{index}"));
            user_events.entry(id).or_insert(timestamp);
            continue;
        }

        let message = value.get("message").unwrap_or(&JsonValue::Null);
        let message_model = message
            .get("model")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .trim();
        if !message_model.is_empty() {
            model = message_model.to_string();
        }
        let Some(usage) = message.get("usage").filter(|usage| usage.is_object()) else {
            continue;
        };
        let input = number(usage, &["input_tokens", "inputTokens"]);
        let cached = number(
            usage,
            &[
                "cache_read_input_tokens",
                "cacheReadInputTokens",
                "cached_input_tokens",
            ],
        );
        let cache_creation = number(
            usage,
            &["cache_creation_input_tokens", "cacheCreationInputTokens"],
        );
        let output = number(usage, &["output_tokens", "outputTokens"]);
        let total = input
            .saturating_add(cached)
            .saturating_add(cache_creation)
            .saturating_add(output);
        if total <= 0 || timestamp.is_empty() {
            continue;
        }
        let message_id = message
            .get("id")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.is_empty())
            .or_else(|| value.get("uuid").and_then(JsonValue::as_str))
            .map(str::to_string)
            .unwrap_or_else(|| format!("{session_id}:assistant:{index}"));
        let request_id = value
            .get("requestId")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or("");
        let dedup_id = if request_id.is_empty() {
            message_id
        } else {
            format!("{message_id}:{request_id}")
        };
        let event = UsageEvent {
            id: dedup_id.clone(),
            source: "claude".to_string(),
            model: if message_model.is_empty() {
                UNKNOWN_CLAUDE_MODEL.to_string()
            } else {
                message_model.to_string()
            },
            project_key: project_key.clone(),
            timestamp,
            input_tokens: input,
            cached_input_tokens: cached,
            cache_creation_input_tokens: cache_creation,
            output_tokens: output,
            reasoning_output_tokens: 0,
            total_tokens: total,
            conversation_count: 0,
            cost_usd: 0.0,
            pricing_available: false,
            estimated_tokens: 0,
        };
        let should_replace = usage_events
            .get(&dedup_id)
            .map(|existing| event.total_tokens > existing.total_tokens)
            .unwrap_or(true);
        if should_replace {
            usage_events.insert(dedup_id, event);
        }
    }

    if model.is_empty() {
        model = UNKNOWN_CLAUDE_MODEL.to_string();
    }
    let mut events = usage_events.into_values().collect::<Vec<_>>();
    events.extend(user_events.into_iter().map(|(id, timestamp)| UsageEvent {
        id: format!("u:{id}"),
        source: "claude".to_string(),
        model: model.clone(),
        project_key: project_key.clone(),
        timestamp,
        conversation_count: 1,
        ..Default::default()
    }));
    let tokens = events
        .iter()
        .fold(TokenSessionTokens::default(), |mut total, event| {
            total.input_tokens += event.input_tokens;
            total.cached_input_tokens += event.cached_input_tokens;
            total.cache_creation_input_tokens += event.cache_creation_input_tokens;
            total.output_tokens += event.output_tokens;
            total.reasoning_output_tokens += event.reasoning_output_tokens;
            total.total_tokens += event.total_tokens;
            total
        });
    let turns = events.iter().map(|event| event.conversation_count).sum();
    let session = token_session(
        session_id,
        "claude",
        project_key,
        model,
        first_ts,
        last_ts,
        turns,
        tokens,
        0.0,
    );
    CachedFile {
        fingerprint: fingerprint(path),
        events,
        sessions: vec![session],
    }
}

fn parse_command_code_file(path: &Path) -> CachedFile {
    let file_fingerprint = command_code_fingerprint(path);
    let Ok(text) = fs::read_to_string(path) else {
        return CachedFile {
            fingerprint: file_fingerprint,
            ..Default::default()
        };
    };
    let fallback_id = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_string();
    let mut session_id = fallback_id;
    let mut project_key = command_code_project_from_path(path);
    let mut session_model = command_code_sidecar_model(path);
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut user_event_count = 0i64;
    let mut assistant_message_count = 0i64;
    let mut exact_usage_events = 0i64;
    let mut estimated_usage_events = 0i64;
    let mut visible_context_tokens = 0i64;
    let mut events = Vec::<UsageEvent>::new();

    for (index, line) in text.lines().enumerate() {
        let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };
        let entry_type = value.get("type").and_then(JsonValue::as_str).unwrap_or("");
        if entry_type == "session" {
            if let Some(id) = value
                .get("id")
                .and_then(JsonValue::as_str)
                .filter(|value| !value.trim().is_empty())
            {
                session_id = id.to_string();
            }
            if let Some(cwd) = value
                .get("cwd")
                .and_then(JsonValue::as_str)
                .filter(|value| !value.trim().is_empty())
            {
                project_key = basename_or_fallback(cwd, &project_key);
            }
            continue;
        }

        let message = if entry_type == "message" {
            value.get("message").unwrap_or(&JsonValue::Null)
        } else {
            &value
        };
        let role = message
            .get("role")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        if role != "user" && role != "assistant" && role != "tool" {
            continue;
        }
        if let Some(id) = value
            .get("sessionId")
            .or_else(|| value.get("session_id"))
            .and_then(JsonValue::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            session_id = id.to_string();
        }
        let timestamp = value
            .get("timestamp")
            .or_else(|| value.get("metadata").and_then(|meta| meta.get("timestamp")))
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        update_bounds(&mut first_ts, &mut last_ts, &timestamp);

        let entry_model = value
            .get("model")
            .and_then(JsonValue::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("");
        if !entry_model.is_empty() {
            session_model = entry_model.to_string();
        }
        let effective_model = if entry_model.is_empty() {
            if session_model.is_empty() {
                UNKNOWN_COMMAND_CODE_MODEL.to_string()
            } else {
                session_model.clone()
            }
        } else {
            entry_model.to_string()
        };

        let id = value
            .get("id")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{session_id}:{role}:{index}"));
        let content = message.get("content").unwrap_or(&JsonValue::Null);
        let content_tokens = estimate_local_content_tokens(content);
        if role == "tool" {
            // 工具结果也会进入下一次模型请求上下文，但不单独形成用户对话或模型请求。
            visible_context_tokens = visible_context_tokens.saturating_add(content_tokens);
            continue;
        }
        if role == "user" {
            if claude_user_is_human(content) {
                user_event_count += 1;
                events.push(UsageEvent {
                    id: format!("u:{id}"),
                    source: "command-code".to_string(),
                    model: effective_model,
                    project_key: project_key.clone(),
                    timestamp,
                    conversation_count: 1,
                    ..Default::default()
                });
            }
            visible_context_tokens = visible_context_tokens.saturating_add(content_tokens);
            continue;
        }

        assistant_message_count += 1;
        // V3 优先使用 Command Code 直接持久化的精确 usage。
        // V2 没有 usage，但完整可见消息上下文仍在：按「当前可见累计上下文 + 本次输出」
        // 生成保守估算，且单独标记 estimated_tokens，绝不冒充来源精确值。
        if let Some(usage) = command_code_usage(&value) {
            let input_tokens = number(usage, &["inputTokens", "input_tokens"]);
            let output_tokens = number(usage, &["outputTokens", "output_tokens"]);
            let cached_input_tokens = number(
                usage,
                &["cacheReadTokens", "cache_read_tokens", "cachedInputTokens"],
            );
            let cache_creation_input_tokens = number(
                usage,
                &[
                    "cacheWriteTokens",
                    "cache_write_tokens",
                    "cacheCreationInputTokens",
                ],
            );
            let total_tokens = input_tokens
                .saturating_add(output_tokens)
                .saturating_add(cached_input_tokens)
                .saturating_add(cache_creation_input_tokens);
            let cost_usd = float_number(usage, &["costUsd", "cost_usd"]);
            exact_usage_events += 1;
            events.push(UsageEvent {
                id,
                source: "command-code".to_string(),
                model: effective_model,
                project_key: project_key.clone(),
                timestamp,
                input_tokens,
                cached_input_tokens,
                cache_creation_input_tokens,
                output_tokens,
                reasoning_output_tokens: 0,
                total_tokens,
                conversation_count: 0,
                cost_usd,
                pricing_available: cost_usd > 0.0,
                estimated_tokens: 0,
            });
        } else {
            let input_tokens = visible_context_tokens
                .saturating_add(32)
                .min(LOCAL_ESTIMATED_CONTEXT_LIMIT);
            let output_tokens = content_tokens;
            let total_tokens = input_tokens.saturating_add(output_tokens);
            estimated_usage_events += 1;
            events.push(UsageEvent {
                id,
                source: "command-code".to_string(),
                model: effective_model,
                project_key: project_key.clone(),
                timestamp,
                input_tokens,
                output_tokens,
                total_tokens,
                conversation_count: 0,
                estimated_tokens: total_tokens,
                ..Default::default()
            });
        }
        visible_context_tokens = visible_context_tokens.saturating_add(content_tokens);
    }

    if session_model.is_empty() {
        session_model = UNKNOWN_COMMAND_CODE_MODEL.to_string();
    }
    let tokens = events
        .iter()
        .fold(TokenSessionTokens::default(), |mut total, event| {
            total.input_tokens += event.input_tokens;
            total.cached_input_tokens += event.cached_input_tokens;
            total.cache_creation_input_tokens += event.cache_creation_input_tokens;
            total.output_tokens += event.output_tokens;
            total.reasoning_output_tokens += event.reasoning_output_tokens;
            total.total_tokens += event.total_tokens;
            total
        });
    let turns = user_event_count;
    let cost_usd = events.iter().map(|event| event.cost_usd).sum();
    let mut session = token_session(
        session_id,
        "command-code",
        project_key,
        session_model,
        first_ts,
        last_ts,
        turns,
        tokens,
        cost_usd,
    );
    session.productive = turns > 0 && assistant_message_count > 0;
    session.provenance = json!({
        "source": "openhub-local-collector",
        "confidence": "observed",
        "privacy": "metadata-only",
        "tokenUsage": if estimated_usage_events > 0 {
            if exact_usage_events > 0 { "mixed-observed-and-estimated" } else { "estimated-v2-local-context" }
        } else { "observed-v3" },
        "assistantMessages": assistant_message_count,
        "exactUsageEvents": exact_usage_events,
        "estimatedUsageEvents": estimated_usage_events,
        "estimationMethod": if estimated_usage_events > 0 { "visible-context-chars-v1" } else { "none" },
        "estimatedContextLimit": if estimated_usage_events > 0 { LOCAL_ESTIMATED_CONTEXT_LIMIT } else { 0 }
    });
    CachedFile {
        fingerprint: file_fingerprint,
        events,
        sessions: vec![session],
    }
}

fn find_ascii_model_token(bytes: &[u8]) -> String {
    const PREFIXES: [&[u8]; 3] = [b"gemini-", b"claude-", b"gpt-"];
    for index in 0..bytes.len() {
        let Some(prefix) = PREFIXES
            .iter()
            .find(|prefix| bytes[index..].starts_with(prefix))
        else {
            continue;
        };
        let mut end = index + prefix.len();
        while end < bytes.len()
            && (bytes[end].is_ascii_alphanumeric() || matches!(bytes[end], b'-' | b'_' | b'.'))
        {
            end += 1;
        }
        if end > index + prefix.len() {
            return String::from_utf8_lossy(&bytes[index..end]).to_string();
        }
    }
    String::new()
}

fn antigravity_database_metadata(path: &Path) -> (String, String) {
    let Some(database_path) = antigravity_database_path(path) else {
        return (String::new(), String::new());
    };
    let Some(conn) = open_readonly_sqlite(&database_path) else {
        return (String::new(), String::new());
    };

    let model = conn
        .query_row(
            "SELECT data FROM gen_metadata ORDER BY idx ASC LIMIT 1",
            [],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .ok()
        .map(|data| find_ascii_model_token(&data[..data.len().min(16_384)]))
        .unwrap_or_default();

    let project = conn
        .query_row(
            "SELECT data FROM trajectory_metadata_blob ORDER BY id ASC LIMIT 1",
            [],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .ok()
        .and_then(|data| {
            let marker = b"file:///";
            let start = data
                .windows(marker.len())
                .position(|value| value == marker)?;
            let mut end = start;
            while end < data.len() && data[end].is_ascii_graphic() {
                end += 1;
            }
            let encoded = String::from_utf8_lossy(&data[start..end]);
            let decoded =
                percent_encoding::percent_decode_str(encoded.trim_start_matches("file://"))
                    .decode_utf8_lossy()
                    .to_string();
            let mut candidate = decoded.trim().to_string();
            while !candidate.is_empty() && !Path::new(&candidate).exists() {
                candidate.pop();
            }
            (!candidate.is_empty()).then_some(candidate)
        })
        .map(|path| basename_or_fallback(&path, "Antigravity"))
        .unwrap_or_default();

    (model, project)
}

fn antigravity_fallback_project(path: &Path) -> String {
    antigravity_data_root(path)
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map(|name| {
            if name.ends_with("-cli") {
                "Antigravity CLI"
            } else if name.ends_with("-ide") {
                "Antigravity IDE"
            } else {
                "Antigravity"
            }
        })
        .unwrap_or("Antigravity")
        .to_string()
}

fn kiro_session_id(path: &Path, metadata: &JsonValue) -> String {
    metadata
        .get("id")
        .and_then(JsonValue::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            path.parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            path.file_stem()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "kiro-session".to_string())
}

fn kiro_project_from_metadata(metadata: &JsonValue) -> String {
    metadata
        .get("workspacePaths")
        .and_then(JsonValue::as_array)
        .and_then(|paths| paths.iter().find_map(JsonValue::as_str))
        .map(|path| basename_or_fallback(path, "Kiro"))
        .unwrap_or_else(|| "Kiro".to_string())
}

fn kiro_model_from_metadata(metadata: &JsonValue) -> String {
    metadata
        .get("modelId")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| UNKNOWN_KIRO_MODEL.to_string())
}

fn parse_kiro_file(path: &Path) -> CachedFile {
    let file_fingerprint = kiro_fingerprint(path);
    let metadata = fs::read_to_string(kiro_session_metadata_path(path))
        .ok()
        .and_then(|text| serde_json::from_str::<JsonValue>(&text).ok())
        .unwrap_or(JsonValue::Null);
    let session_id = kiro_session_id(path, &metadata);
    let project_key = kiro_project_from_metadata(&metadata);
    let model = kiro_model_from_metadata(&metadata);
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut visible_context_tokens = 0i64;
    let mut turns = 0i64;
    let mut assistant_responses = 0i64;
    let mut events = Vec::<UsageEvent>::new();

    let Ok(text) = fs::read_to_string(path) else {
        return CachedFile {
            fingerprint: file_fingerprint,
            ..Default::default()
        };
    };
    for (index, line) in text.lines().enumerate() {
        let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };
        let timestamp = value
            .get("timestamp")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        update_bounds(&mut first_ts, &mut last_ts, &timestamp);
        let payload = value.get("payload").unwrap_or(&JsonValue::Null);
        let payload_type = payload
            .get("type")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        let event_id = value
            .get("id")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{session_id}:{payload_type}:{index}"));

        match payload_type {
            "user" => {
                let content = payload.get("content").unwrap_or(&JsonValue::Null);
                let content_tokens = estimate_local_content_tokens(content);
                if !claude_user_is_human(content) {
                    continue;
                }
                turns += 1;
                events.push(UsageEvent {
                    id: format!("u:{event_id}"),
                    source: "kiro".to_string(),
                    model: model.clone(),
                    project_key: project_key.clone(),
                    timestamp,
                    conversation_count: 1,
                    ..Default::default()
                });
                visible_context_tokens = visible_context_tokens.saturating_add(content_tokens);
            }
            "assistant" => {
                assistant_responses += 1;
                let content = payload.get("content").unwrap_or(&JsonValue::Null);
                let output_tokens = estimate_local_content_tokens(content);
                let input_tokens = visible_context_tokens
                    .saturating_add(32)
                    .min(LOCAL_ESTIMATED_CONTEXT_LIMIT);
                let total_tokens = input_tokens.saturating_add(output_tokens);
                events.push(UsageEvent {
                    id: event_id,
                    source: "kiro".to_string(),
                    model: model.clone(),
                    project_key: project_key.clone(),
                    timestamp,
                    input_tokens,
                    output_tokens,
                    total_tokens,
                    estimated_tokens: total_tokens,
                    ..Default::default()
                });
                visible_context_tokens = visible_context_tokens.saturating_add(output_tokens);
            }
            "tool_call" | "tool_result" => {
                visible_context_tokens =
                    visible_context_tokens.saturating_add(estimate_local_content_tokens(payload));
            }
            _ => {}
        }
    }

    let tokens = events
        .iter()
        .fold(TokenSessionTokens::default(), |mut total, event| {
            total.input_tokens += event.input_tokens;
            total.cached_input_tokens += event.cached_input_tokens;
            total.cache_creation_input_tokens += event.cache_creation_input_tokens;
            total.output_tokens += event.output_tokens;
            total.reasoning_output_tokens += event.reasoning_output_tokens;
            total.total_tokens += event.total_tokens;
            total
        });
    let mut session = token_session(
        session_id,
        "kiro",
        project_key,
        model,
        first_ts,
        last_ts,
        turns,
        tokens,
        0.0,
    );
    session.productive = turns > 0 && assistant_responses > 0;
    session.provenance = json!({
        "source": "openhub-local-collector",
        "confidence": "estimated",
        "privacy": "metadata-only",
        "tokenUsage": "estimated-kiro-local-context",
        "assistantResponses": assistant_responses,
        "estimationMethod": "visible-context-chars-v1",
        "estimatedContextLimit": LOCAL_ESTIMATED_CONTEXT_LIMIT,
        "modelId": metadata.get("modelId").cloned().unwrap_or(JsonValue::Null)
    });
    CachedFile {
        fingerprint: file_fingerprint,
        events,
        sessions: vec![session],
    }
}

fn parse_antigravity_file(path: &Path) -> CachedFile {
    let file_fingerprint = antigravity_fingerprint(path);
    let Ok(text) = fs::read_to_string(path) else {
        return CachedFile {
            fingerprint: file_fingerprint,
            ..Default::default()
        };
    };
    let mut session_id = antigravity_session_id(path);
    if session_id.is_empty() {
        session_id = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_string();
    }
    let (database_model, database_project) = antigravity_database_metadata(path);
    let model = if database_model.is_empty() {
        UNKNOWN_ANTIGRAVITY_MODEL.to_string()
    } else {
        database_model
    };
    let project_key = if database_project.is_empty() {
        antigravity_fallback_project(path)
    } else {
        database_project
    };
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut visible_context_tokens = 0i64;
    let mut turns = 0i64;
    let mut planner_responses = 0i64;
    let mut events = Vec::<UsageEvent>::new();

    for (index, line) in text.lines().enumerate() {
        let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };
        let timestamp = value
            .get("created_at")
            .or_else(|| value.get("timestamp"))
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        update_bounds(&mut first_ts, &mut last_ts, &timestamp);
        let kind = value.get("type").and_then(JsonValue::as_str).unwrap_or("");
        let source = value
            .get("source")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        let content_tokens =
            estimate_local_content_tokens(value.get("content").unwrap_or(&JsonValue::Null));
        let thinking_tokens =
            estimate_local_content_tokens(value.get("thinking").unwrap_or(&JsonValue::Null));
        let tool_call_tokens =
            estimate_local_content_tokens(value.get("tool_calls").unwrap_or(&JsonValue::Null));
        let error_tokens =
            estimate_local_content_tokens(value.get("error").unwrap_or(&JsonValue::Null));
        let context_delta = content_tokens
            .saturating_add(thinking_tokens)
            .saturating_add(tool_call_tokens)
            .saturating_add(error_tokens);
        let event_id = value
            .get("step_index")
            .and_then(JsonValue::as_i64)
            .map(|step| format!("{session_id}:{step}"))
            .unwrap_or_else(|| format!("{session_id}:{index}"));

        if kind == "USER_INPUT" && source == "USER_EXPLICIT" {
            turns += 1;
            events.push(UsageEvent {
                id: format!("u:{event_id}"),
                source: "antigravity".to_string(),
                model: model.clone(),
                project_key: project_key.clone(),
                timestamp,
                conversation_count: 1,
                ..Default::default()
            });
            visible_context_tokens = visible_context_tokens.saturating_add(context_delta);
            continue;
        }

        if kind == "PLANNER_RESPONSE" {
            planner_responses += 1;
            let input_tokens = visible_context_tokens
                .saturating_add(32)
                .min(LOCAL_ESTIMATED_CONTEXT_LIMIT);
            let output_tokens = content_tokens.saturating_add(tool_call_tokens);
            let reasoning_output_tokens = thinking_tokens;
            let total_tokens = input_tokens
                .saturating_add(output_tokens)
                .saturating_add(reasoning_output_tokens);
            events.push(UsageEvent {
                id: event_id,
                source: "antigravity".to_string(),
                model: model.clone(),
                project_key: project_key.clone(),
                timestamp,
                input_tokens,
                output_tokens,
                reasoning_output_tokens,
                total_tokens,
                estimated_tokens: total_tokens,
                ..Default::default()
            });
        }
        visible_context_tokens = visible_context_tokens.saturating_add(context_delta);
    }

    let tokens = events
        .iter()
        .fold(TokenSessionTokens::default(), |mut total, event| {
            total.input_tokens += event.input_tokens;
            total.cached_input_tokens += event.cached_input_tokens;
            total.cache_creation_input_tokens += event.cache_creation_input_tokens;
            total.output_tokens += event.output_tokens;
            total.reasoning_output_tokens += event.reasoning_output_tokens;
            total.total_tokens += event.total_tokens;
            total
        });
    let mut session = token_session(
        session_id,
        "antigravity",
        project_key,
        model,
        first_ts,
        last_ts,
        turns,
        tokens,
        0.0,
    );
    session.productive = turns > 0 && planner_responses > 0;
    session.provenance = json!({
        "source": "openhub-local-collector",
        "confidence": "estimated",
        "privacy": "metadata-only",
        "tokenUsage": "estimated-antigravity-local-context",
        "plannerResponses": planner_responses,
        "estimationMethod": "visible-context-chars-v1",
        "estimatedContextLimit": LOCAL_ESTIMATED_CONTEXT_LIMIT
    });
    CachedFile {
        fingerprint: file_fingerprint,
        events,
        sessions: vec![session],
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CodexUsage {
    input_tokens: i64,
    cached_input_tokens: i64,
    cache_creation_input_tokens: i64,
    output_tokens: i64,
    reasoning_output_tokens: i64,
    total_tokens: i64,
}

impl CodexUsage {
    fn from_json(value: &JsonValue) -> Option<Self> {
        if !value.is_object() {
            return None;
        }
        let usage = Self {
            input_tokens: number(value, &["input_tokens"]),
            cached_input_tokens: number(value, &["cached_input_tokens"]),
            cache_creation_input_tokens: number(
                value,
                &["cache_creation_input_tokens", "cache_write_input_tokens"],
            ),
            output_tokens: number(value, &["output_tokens"]),
            reasoning_output_tokens: number(value, &["reasoning_output_tokens"]),
            total_tokens: number(value, &["total_tokens"]),
        };
        Some(usage)
    }

    fn subtract(self, other: Self) -> Option<Self> {
        if self.input_tokens < other.input_tokens
            || self.cached_input_tokens < other.cached_input_tokens
            || self.cache_creation_input_tokens < other.cache_creation_input_tokens
            || self.output_tokens < other.output_tokens
            || self.reasoning_output_tokens < other.reasoning_output_tokens
            || self.total_tokens < other.total_tokens
        {
            return None;
        }
        Some(Self {
            input_tokens: self.input_tokens - other.input_tokens,
            cached_input_tokens: self.cached_input_tokens - other.cached_input_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens
                - other.cache_creation_input_tokens,
            output_tokens: self.output_tokens - other.output_tokens,
            reasoning_output_tokens: self.reasoning_output_tokens - other.reasoning_output_tokens,
            total_tokens: self.total_tokens - other.total_tokens,
        })
    }

    fn diff(self, other: Self) -> Self {
        Self {
            input_tokens: (self.input_tokens - other.input_tokens).max(0),
            cached_input_tokens: (self.cached_input_tokens - other.cached_input_tokens).max(0),
            cache_creation_input_tokens: (self.cache_creation_input_tokens
                - other.cache_creation_input_tokens)
                .max(0),
            output_tokens: (self.output_tokens - other.output_tokens).max(0),
            reasoning_output_tokens: (self.reasoning_output_tokens - other.reasoning_output_tokens)
                .max(0),
            total_tokens: (self.total_tokens - other.total_tokens).max(0),
        }
    }

    fn normalized(self) -> Self {
        let fresh_input = self.input_tokens.saturating_sub(self.cached_input_tokens);
        let total = fresh_input
            .saturating_add(self.cached_input_tokens)
            .saturating_add(self.cache_creation_input_tokens)
            .saturating_add(self.output_tokens);
        Self {
            input_tokens: fresh_input,
            total_tokens: total,
            ..self
        }
    }
}

#[derive(Default)]
struct CodexUsageState {
    last_total: Option<CodexUsage>,
    baselines: Vec<CodexUsage>,
}

impl CodexUsageState {
    fn touch(&mut self, usage: CodexUsage) {
        if let Some(index) = self.baselines.iter().position(|item| *item == usage) {
            self.baselines.remove(index);
        }
        self.baselines.push(usage);
        if self.baselines.len() > 32 {
            self.baselines.remove(0);
        }
        self.last_total = Some(usage);
    }

    fn consume(
        &mut self,
        last_usage: Option<CodexUsage>,
        total_usage: Option<CodexUsage>,
    ) -> Option<CodexUsage> {
        let Some(total) = total_usage else {
            return last_usage;
        };
        if self.baselines.contains(&total) {
            self.touch(total);
            return None;
        }
        if let Some(last) = last_usage {
            if let Some(previous) = total.subtract(last) {
                if self.baselines.contains(&previous) {
                    self.touch(total);
                    return Some(last);
                }
                if self.last_total.is_some() {
                    self.touch(total);
                    return Some(last);
                }
            }
        }
        if let Some(active) = self.last_total {
            if total.total_tokens >= active.total_tokens {
                let delta = total.diff(active);
                if last_usage
                    .map(|last| delta.total_tokens <= last.total_tokens)
                    .unwrap_or(true)
                {
                    self.touch(total);
                    return Some(delta);
                }
            }
        }
        self.touch(total);
        last_usage
    }
}

fn parse_codex_file(path: &Path) -> CachedFile {
    let Ok(text) = fs::read_to_string(path) else {
        return CachedFile {
            fingerprint: fingerprint(path),
            ..Default::default()
        };
    };
    let fallback_id = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_string();
    let mut session_id = fallback_id;
    let mut project_key = "Codex".to_string();
    let mut current_model = String::new();
    let mut session_model = String::new();
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut usage_state = CodexUsageState::default();
    let mut seen_usage = HashSet::<String>::new();
    let mut events = Vec::<UsageEvent>::new();

    for (index, line) in text.lines().enumerate() {
        let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };
        let timestamp = value
            .get("timestamp")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        update_bounds(&mut first_ts, &mut last_ts, &timestamp);
        let kind = value.get("type").and_then(JsonValue::as_str).unwrap_or("");
        let payload = value.get("payload").unwrap_or(&JsonValue::Null);
        if kind == "session_meta" {
            if let Some(id) = payload
                .get("id")
                .or_else(|| payload.get("session_id"))
                .and_then(JsonValue::as_str)
                .filter(|value| !value.is_empty())
            {
                session_id = id.to_string();
            }
            if let Some(cwd) = payload.get("cwd").and_then(JsonValue::as_str) {
                project_key = basename_or_fallback(cwd, &project_key);
            }
            if current_model.is_empty() {
                if let Some(provider) = payload
                    .get("model_provider")
                    .and_then(JsonValue::as_str)
                    .filter(|value| !value.is_empty())
                {
                    current_model = provider.to_string();
                }
            }
            continue;
        }
        if kind == "turn_context" {
            if let Some(cwd) = payload.get("cwd").and_then(JsonValue::as_str) {
                project_key = basename_or_fallback(cwd, &project_key);
            }
            if let Some(model) = payload
                .get("model")
                .and_then(JsonValue::as_str)
                .filter(|value| !value.is_empty())
            {
                current_model = model.to_string();
                session_model = model.to_string();
            }
            continue;
        }
        if kind != "event_msg" {
            continue;
        }
        match payload
            .get("type")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
        {
            "user_message" => {
                let id = payload
                    .get("client_id")
                    .or_else(|| payload.get("turn_id"))
                    .and_then(JsonValue::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{session_id}:user:{index}"));
                events.push(UsageEvent {
                    id: format!("u:{id}"),
                    source: "codex".to_string(),
                    model: if current_model.is_empty() {
                        UNKNOWN_CODEX_MODEL.to_string()
                    } else {
                        current_model.clone()
                    },
                    project_key: project_key.clone(),
                    timestamp,
                    conversation_count: 1,
                    ..Default::default()
                });
            }
            "token_count" => {
                let info = payload.get("info").unwrap_or(&JsonValue::Null);
                let last_usage = info.get("last_token_usage").and_then(CodexUsage::from_json);
                let total_usage = info
                    .get("total_token_usage")
                    .and_then(CodexUsage::from_json);
                let signature = format!("{session_id}:{timestamp}:{last_usage:?}:{total_usage:?}");
                let Some(delta) = usage_state.consume(last_usage, total_usage) else {
                    continue;
                };
                if !seen_usage.insert(signature.clone()) {
                    continue;
                }
                let delta = delta.normalized();
                if delta.total_tokens <= 0 || timestamp.is_empty() {
                    continue;
                }
                let model = if current_model.is_empty() {
                    UNKNOWN_CODEX_MODEL.to_string()
                } else {
                    current_model.clone()
                };
                if session_model.is_empty() {
                    session_model = model.clone();
                }
                events.push(UsageEvent {
                    id: signature,
                    source: "codex".to_string(),
                    model,
                    project_key: project_key.clone(),
                    timestamp,
                    input_tokens: delta.input_tokens,
                    cached_input_tokens: delta.cached_input_tokens,
                    cache_creation_input_tokens: delta.cache_creation_input_tokens,
                    output_tokens: delta.output_tokens,
                    reasoning_output_tokens: delta.reasoning_output_tokens,
                    total_tokens: delta.total_tokens,
                    conversation_count: 0,
                    cost_usd: 0.0,
                    pricing_available: false,
                    estimated_tokens: 0,
                });
            }
            _ => {}
        }
    }

    if session_model.is_empty() {
        session_model = if current_model.is_empty() {
            UNKNOWN_CODEX_MODEL.to_string()
        } else {
            current_model
        };
    }
    let tokens = events
        .iter()
        .fold(TokenSessionTokens::default(), |mut total, event| {
            total.input_tokens += event.input_tokens;
            total.cached_input_tokens += event.cached_input_tokens;
            total.cache_creation_input_tokens += event.cache_creation_input_tokens;
            total.output_tokens += event.output_tokens;
            total.reasoning_output_tokens += event.reasoning_output_tokens;
            total.total_tokens += event.total_tokens;
            total
        });
    let turns = events.iter().map(|event| event.conversation_count).sum();
    let session = token_session(
        session_id,
        "codex",
        project_key,
        session_model,
        first_ts,
        last_ts,
        turns,
        tokens,
        0.0,
    );
    CachedFile {
        fingerprint: fingerprint(path),
        events,
        sessions: vec![session],
    }
}

fn open_readonly_sqlite(path: &Path) -> Option<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()
}

#[derive(Default)]
struct LocalDatabaseSession {
    directory: String,
    model: String,
    started_at: String,
    ended_at: String,
    turns: i64,
    cost_usd: f64,
    tokens: TokenSessionTokens,
}

fn database_provider(value: &JsonValue) -> String {
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

fn database_message_allowed(source: &str, value: &JsonValue) -> bool {
    let provider = database_provider(value);
    match source {
        // MiMo 数据库会镜像 Claude 会话；只保留 MiMo 自己的 provider。
        "mimo" => provider == "mimo" || provider == "xiaomi",
        // ZCode 的内置/自定义 provider 都属于自身数据；仅排除会被其他采集器读取的子代理。
        "zcode" => {
            !provider.is_empty()
                && !provider.contains("anthropic")
                && !provider.contains("openai")
                && !provider.contains("google")
        }
        _ => true,
    }
}

fn unknown_database_model(source: &str) -> String {
    match source {
        "mimo" => "mimo-unknown-model",
        "zcode" => "zcode-unknown-model",
        _ => UNKNOWN_OPENCODE_MODEL,
    }
    .to_string()
}

fn parse_local_database(path: &Path, source: &str) -> CachedDatabase {
    let Some(connection) = open_readonly_sqlite(path) else {
        return CachedDatabase {
            fingerprint: database_fingerprint(path),
            ..Default::default()
        };
    };
    let fallback_project = match source {
        "mimo" => "MiMo",
        "zcode" => "ZCode",
        _ => "OpenCode",
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
                if !database_message_allowed(source, &value) {
                    continue;
                }
                let role = value.get("role").and_then(JsonValue::as_str).unwrap_or("");
                if role != "user" && role != "assistant" {
                    continue;
                }
                let session = sessions.entry(session_id.clone()).or_default();
                let directory = value
                    .get("path")
                    .and_then(|path| path.get("cwd"))
                    .and_then(JsonValue::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or(&session.directory)
                    .to_string();
                if session.directory.is_empty() && !directory.is_empty() {
                    session.directory = directory.clone();
                }
                let project_key = basename_or_fallback(&directory, fallback_project);
                let timestamp = value
                    .get("time")
                    .and_then(|time| time.get("completed").or_else(|| time.get("created")))
                    .and_then(JsonValue::as_i64)
                    .map(iso_from_millis)
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| iso_from_millis(time_created));
                update_bounds(&mut session.started_at, &mut session.ended_at, &timestamp);
                let model = value
                    .get("modelID")
                    .or_else(|| value.get("modelId"))
                    .and_then(JsonValue::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| unknown_database_model(source));
                if session.model.is_empty() && !model.is_empty() {
                    session.model = model.clone();
                }
                if role == "user" {
                    session.turns += 1;
                    events.push(UsageEvent {
                        id: format!("u:{session_id}:{id}"),
                        source: source.to_string(),
                        model,
                        project_key,
                        timestamp,
                        conversation_count: 1,
                        ..Default::default()
                    });
                    continue;
                }

                let tokens = value.get("tokens").unwrap_or(&JsonValue::Null);
                let cache = tokens.get("cache").unwrap_or(&JsonValue::Null);
                let input = number(tokens, &["input"]);
                let cached = number(cache, &["read"]);
                let cache_creation = number(cache, &["write"]);
                let output = number(tokens, &["output"]);
                let reasoning = number(tokens, &["reasoning"]);
                // OpenCode 系工具的 tokens.total 在部分版本为空或口径不一致；
                // 统一按五个明确分项求和，避免缓存 Token 被漏算或重复算。
                let total = input
                    .saturating_add(cached)
                    .saturating_add(cache_creation)
                    .saturating_add(output)
                    .saturating_add(reasoning);
                if total <= 0 {
                    continue;
                }
                let cost = float_number(&value, &["cost"]);
                session.tokens.input_tokens += input;
                session.tokens.cached_input_tokens += cached;
                session.tokens.cache_creation_input_tokens += cache_creation;
                session.tokens.output_tokens += output;
                session.tokens.reasoning_output_tokens += reasoning;
                session.tokens.total_tokens += total;
                session.cost_usd += cost;
                events.push(UsageEvent {
                    id: format!("{session_id}:{id}"),
                    source: source.to_string(),
                    model,
                    project_key,
                    timestamp,
                    input_tokens: input,
                    cached_input_tokens: cached,
                    cache_creation_input_tokens: cache_creation,
                    output_tokens: output,
                    reasoning_output_tokens: reasoning,
                    total_tokens: total,
                    conversation_count: 0,
                    cost_usd: cost,
                    pricing_available: cost > 0.0,
                    estimated_tokens: 0,
                });
            }
        }
    }

    let token_sessions = sessions
        .into_iter()
        .filter(|(_, session)| session.turns > 0 || session.tokens.total_tokens > 0)
        .map(|(id, mut session)| {
            if session.model.is_empty() {
                session.model = unknown_database_model(source);
            }
            token_session(
                id,
                source,
                basename_or_fallback(&session.directory, fallback_project),
                session.model,
                session.started_at,
                session.ended_at,
                session.turns,
                session.tokens,
                session.cost_usd,
            )
        })
        .collect::<Vec<_>>();
    CachedDatabase {
        fingerprint: database_fingerprint(path),
        events,
        sessions: token_sessions,
    }
}

fn aggregate_events(events: Vec<UsageEvent>) -> TokenUsageReport {
    let mut dedup = BTreeMap::<String, UsageEvent>::new();
    for event in events {
        let key = format!("{}:{}", event.source, event.id);
        let replace = dedup
            .get(&key)
            .map(|current| {
                event.total_tokens > current.total_tokens
                    || event.conversation_count > current.conversation_count
            })
            .unwrap_or(true);
        if replace {
            dedup.insert(key, event);
        }
    }
    let mut buckets = BTreeMap::<String, TokenUsageBucket>::new();
    for event in dedup.into_values() {
        let Some(timestamp) = half_hour_key(&event.timestamp) else {
            continue;
        };
        let model = if event.model.trim().is_empty() {
            format!("{}-unknown-model", event.source)
        } else {
            event.model.clone()
        };
        let key = format!(
            "{}|{}|{}|{}",
            event.source, model, event.project_key, timestamp
        );
        let bucket = buckets.entry(key).or_insert_with(|| TokenUsageBucket {
            source: event.source.clone(),
            model: model.clone(),
            project_key: event.project_key.clone(),
            timestamp: timestamp.clone(),
            ..Default::default()
        });
        bucket.input_tokens += event.input_tokens;
        bucket.cached_input_tokens += event.cached_input_tokens;
        bucket.cache_creation_input_tokens += event.cache_creation_input_tokens;
        bucket.output_tokens += event.output_tokens;
        bucket.reasoning_output_tokens += event.reasoning_output_tokens;
        bucket.total_tokens += event.total_tokens;
        bucket.billable_total_tokens += event.total_tokens;
        bucket.conversation_count += event.conversation_count;
        bucket.cost_usd += event.cost_usd;
        bucket.pricing_available |= event.pricing_available;
        bucket.estimated_tokens += event.estimated_tokens;
    }
    let mut buckets = buckets.into_values().collect::<Vec<_>>();
    buckets.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.source.cmp(&right.source))
            .then_with(|| left.model.cmp(&right.model))
            .then_with(|| left.project_key.cmp(&right.project_key))
    });
    let mut start_date = String::new();
    let mut end_date = String::new();
    let mut has_reported_cost = false;
    for bucket in &buckets {
        let day = bucket.timestamp.get(..10).unwrap_or("");
        if !day.is_empty() {
            if start_date.is_empty() || day < start_date.as_str() {
                start_date = day.to_string();
            }
            if end_date.is_empty() || day > end_date.as_str() {
                end_date = day.to_string();
            }
        }
        has_reported_cost |= bucket.pricing_available;
    }
    TokenUsageReport {
        available: !buckets.is_empty(),
        buckets,
        start_date,
        end_date,
        pricing_source: if has_reported_cost {
            "openhub-source-reported".to_string()
        } else {
            "openhub-local-no-pricing".to_string()
        },
    }
}

fn snapshot_from_envelope(
    envelope: &CollectorEnvelope,
    changed: bool,
    scanned_files: usize,
    reused_files: usize,
) -> CollectedData {
    let mut events = Vec::new();
    let mut session_map = BTreeMap::<String, TokenSession>::new();
    for cached in envelope.files.values() {
        events.extend(cached.events.clone());
        for session in &cached.sessions {
            let replace = session_map
                .get(&session.session_hash)
                .map(|current| session.total_tokens > current.total_tokens)
                .unwrap_or(true);
            if replace {
                session_map.insert(session.session_hash.clone(), session.clone());
            }
        }
    }
    for cached in envelope.databases.values() {
        events.extend(cached.events.clone());
        for session in &cached.sessions {
            session_map.insert(session.session_hash.clone(), session.clone());
        }
    }
    let usage = aggregate_events(events);
    let mut sessions = session_map.into_values().collect::<Vec<_>>();
    sessions.sort_by(|left, right| right.started_at.cmp(&left.started_at));
    CollectedData {
        usage,
        sessions,
        changed,
        scanned_files,
        reused_files,
    }
}

pub(crate) fn load_cached_snapshot() -> Option<CollectedData> {
    let envelope = read_envelope();
    if envelope.version != CACHE_VERSION
        || (envelope.files.is_empty() && envelope.databases.is_empty())
    {
        return None;
    }
    Some(snapshot_from_envelope(
        &envelope,
        false,
        0,
        envelope.files.len(),
    ))
}

fn collect_uncached(force: bool) -> Result<CollectedData, String> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("无法定位用户目录")?);
    let mut envelope = if force {
        CollectorEnvelope::default()
    } else {
        read_envelope()
    };
    envelope.version = CACHE_VERSION;

    let mut files = Vec::<(String, PathBuf)>::new();
    let codex_home = home.join(".codex");
    // Codex 会把归档任务从 sessions/ 移到 archived_sessions/；两处都要扫描。
    // 后续按 session id + usage 事件签名全局去重，因此文件移动不会重复计数。
    let mut codex_files = Vec::new();
    for codex_root in [
        codex_home.join("sessions"),
        codex_home.join("archived_sessions"),
    ] {
        collect_jsonl_files(
            &codex_root,
            &|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
                    .unwrap_or(false)
            },
            &mut codex_files,
        );
    }
    files.extend(
        codex_files
            .into_iter()
            .map(|path| ("codex".to_string(), path)),
    );

    let claude_root = home.join(".claude").join("projects");
    let mut claude_files = Vec::new();
    collect_jsonl_files(
        &claude_root,
        &|path| {
            path.extension().and_then(|value| value.to_str()) == Some("jsonl")
                && !path
                    .components()
                    .any(|component| component.as_os_str() == "subagents")
        },
        &mut claude_files,
    );
    files.extend(
        claude_files
            .into_iter()
            .map(|path| ("claude".to_string(), path)),
    );

    let command_code_root = home.join(".commandcode").join("projects");
    let mut command_code_files = Vec::new();
    collect_jsonl_files(
        &command_code_root,
        &is_command_code_transcript_path,
        &mut command_code_files,
    );
    files.extend(
        command_code_files
            .into_iter()
            .map(|path| ("command-code".to_string(), path)),
    );

    let gemini_root = home.join(".gemini");
    let mut antigravity_files = Vec::new();
    collect_jsonl_files(
        &gemini_root,
        &|path| {
            path.file_name().and_then(|name| name.to_str()) == Some("transcript.jsonl")
                && path.components().any(|component| {
                    matches!(
                        component.as_os_str().to_str(),
                        Some("antigravity-cli") | Some("antigravity-ide")
                    )
                })
        },
        &mut antigravity_files,
    );
    files.extend(
        antigravity_files
            .into_iter()
            .map(|path| ("antigravity".to_string(), path)),
    );
    let kiro_root = home.join(".kiro").join("sessions");
    let mut kiro_files = Vec::new();
    collect_jsonl_files(
        &kiro_root,
        &|path| path.file_name().and_then(|name| name.to_str()) == Some("messages.jsonl"),
        &mut kiro_files,
    );
    files.extend(
        kiro_files
            .into_iter()
            .map(|path| ("kiro".to_string(), path)),
    );
    files.sort_by(|left, right| left.1.cmp(&right.1));

    let live_paths = files
        .iter()
        .map(|(_, path)| path.to_string_lossy().to_string())
        .collect::<HashSet<_>>();
    let cached_file_count = envelope.files.len();
    envelope.files.retain(|path, _| live_paths.contains(path));

    let mut changed = envelope.files.len() != cached_file_count;
    let mut scanned_files = 0usize;
    let mut reused_files = 0usize;
    for (source, path) in files {
        let key = path.to_string_lossy().to_string();
        let current = source_file_fingerprint(&source, &path);
        let reusable = !force
            && envelope
                .files
                .get(&key)
                .map(|cached| {
                    cached.fingerprint.size == current.size
                        && cached.fingerprint.modified_ms == current.modified_ms
                })
                .unwrap_or(false);
        if reusable {
            reused_files += 1;
            continue;
        }
        let parsed = match source.as_str() {
            "codex" => parse_codex_file(&path),
            "command-code" => parse_command_code_file(&path),
            "antigravity" => parse_antigravity_file(&path),
            "kiro" => parse_kiro_file(&path),
            _ => parse_claude_file(&path),
        };
        envelope.files.insert(key, parsed);
        scanned_files += 1;
        changed = true;
    }

    let database_sources = [
        (
            "opencode",
            home.join(".local")
                .join("share")
                .join("opencode")
                .join("opencode.db"),
        ),
        (
            "mimo",
            home.join(".local")
                .join("share")
                .join("mimocode")
                .join("mimocode.db"),
        ),
        (
            "zcode",
            home.join(".zcode").join("cli").join("db").join("db.sqlite"),
        ),
    ];
    let live_databases = database_sources
        .iter()
        .filter(|(_, path)| path.is_file())
        .map(|(source, _)| source.to_string())
        .collect::<HashSet<_>>();
    let cached_database_count = envelope.databases.len();
    envelope
        .databases
        .retain(|source, _| live_databases.contains(source));
    changed |= envelope.databases.len() != cached_database_count;

    for (source, path) in database_sources {
        if !path.is_file() {
            continue;
        }
        let current = database_fingerprint(&path);
        let reusable = !force
            && envelope
                .databases
                .get(source)
                .map(|cached| {
                    cached.fingerprint.database.size == current.database.size
                        && cached.fingerprint.database.modified_ms == current.database.modified_ms
                        && cached.fingerprint.wal.size == current.wal.size
                        && cached.fingerprint.wal.modified_ms == current.wal.modified_ms
                })
                .unwrap_or(false);
        if !reusable {
            envelope
                .databases
                .insert(source.to_string(), parse_local_database(&path, source));
            changed = true;
        }
    }

    envelope.updated_at = now_iso();
    write_envelope(&envelope);
    Ok(snapshot_from_envelope(
        &envelope,
        changed,
        scanned_files,
        reused_files,
    ))
}

fn collect(force: bool) -> Result<CollectedData, String> {
    if !force {
        if let Ok(guard) = memory_cache().lock() {
            if let Some(cache) = guard.as_ref() {
                if cache.fetched_at.elapsed() < CACHE_TTL {
                    return Ok(cache.data.clone());
                }
            }
        }
    }
    let _guard = collector_lock()
        .lock()
        .map_err(|_| "OpenHub Token 采集锁异常".to_string())?;
    // 多个前端命令会并发请求 usage / sessions / sync。等待锁期间若已有命令
    // 完成采集，直接复用内存结果，避免紧接着再解析一次磁盘缓存。
    if !force {
        if let Ok(guard) = memory_cache().lock() {
            if let Some(cache) = guard.as_ref() {
                if cache.fetched_at.elapsed() < CACHE_TTL {
                    return Ok(cache.data.clone());
                }
            }
        }
    }
    let data = collect_uncached(force)?;
    if let Ok(mut guard) = memory_cache().lock() {
        *guard = Some(CollectorMemoryCache {
            data: data.clone(),
            fetched_at: Instant::now(),
        });
    }
    Ok(data)
}

fn now_iso() -> String {
    let millis = UNIX_EPOCH
        .elapsed()
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    iso_from_millis(millis)
}

fn session_in_range(session: &TokenSession, from: Option<&str>, to: Option<&str>) -> bool {
    let started = session.started_at.get(..10).unwrap_or("");
    let ended = session.ended_at.get(..10).unwrap_or(started);
    if let Some(from) = from.filter(|value| !value.trim().is_empty()) {
        if !ended.is_empty() && ended < from {
            return false;
        }
    }
    if let Some(to) = to.filter(|value| !value.trim().is_empty()) {
        if !started.is_empty() && started > to {
            return false;
        }
    }
    true
}

fn summary_from_sessions(sessions: &[TokenSession]) -> TokenSummary {
    let count = sessions.len() as i64;
    let productive_sessions = sessions.iter().filter(|session| session.productive).count() as i64;
    let one_shot_sessions = sessions.iter().filter(|session| session.one_shot).count() as i64;
    let total_tokens = sessions.iter().map(|session| session.total_tokens).sum();
    let cost_usd = sessions.iter().map(|session| session.cost_usd).sum();
    TokenSummary {
        sessions: count,
        productive_sessions,
        one_shot_sessions,
        total_tokens,
        cost_usd,
        productive_rate: if count > 0 {
            productive_sessions as f64 / count as f64
        } else {
            0.0
        },
        one_shot_rate: if count > 0 {
            Some(one_shot_sessions as f64 / count as f64)
        } else {
            None
        },
        ..Default::default()
    }
}

fn model_stats(sessions: &[TokenSession]) -> Vec<TokenModelStat> {
    let mut groups = BTreeMap::<String, Vec<TokenSession>>::new();
    for session in sessions {
        groups
            .entry(session.model.clone())
            .or_default()
            .push(session.clone());
    }
    let mut stats = groups
        .into_iter()
        .map(|(model, sessions)| {
            let summary = summary_from_sessions(&sessions);
            TokenModelStat {
                model,
                sessions: summary.sessions,
                productive_sessions: summary.productive_sessions,
                one_shot_sessions: summary.one_shot_sessions,
                edit_turns: summary.edit_turns,
                retries: summary.retries,
                total_tokens: summary.total_tokens,
                cost_usd: summary.cost_usd,
                edit_tokens: summary.edit_tokens,
                edit_cost_usd: summary.edit_cost_usd,
                productive_rate: summary.productive_rate,
                one_shot_rate: summary.one_shot_rate,
                edit_sessions: summary.edit_sessions,
                first_pass_sessions: summary.first_pass_sessions,
                edit_session_rate: summary.edit_session_rate,
                first_pass_rate: summary.first_pass_rate,
                tokens_per_edit: summary.tokens_per_edit,
                cost_per_edit: summary.cost_per_edit,
            }
        })
        .collect::<Vec<_>>();
    stats.sort_by(|left, right| right.total_tokens.cmp(&left.total_tokens));
    stats
}

pub(crate) fn collect_snapshot(force: bool) -> Result<CollectedData, String> {
    collect(force)
}

pub(crate) fn build_token_stats(
    sessions: Vec<TokenSession>,
    from: Option<String>,
    to: Option<String>,
) -> TokenStatsReport {
    let sessions = sessions
        .into_iter()
        .filter(|session| session_in_range(session, from.as_deref(), to.as_deref()))
        .collect::<Vec<_>>();
    let summary = summary_from_sessions(&sessions);
    let by_model = model_stats(&sessions);
    TokenStatsReport {
        available: !sessions.is_empty(),
        session_count: sessions.len() as i64,
        sessions,
        summary,
        by_model,
        subagents: Vec::new(),
        provenance: json!({
            "source": "openhub-token-database",
            "privacy": "metadata-only",
            "independent": true,
            "sources": ["codex", "claude", "command-code", "antigravity", "kiro", "opencode", "mimo", "zcode", "catpawai"]
        }),
    }
}

pub(crate) fn sync_report(data: &CollectedData, elapsed_ms: i64) -> TokenCollectorSyncReport {
    TokenCollectorSyncReport {
        available: data.usage.available || !data.sessions.is_empty(),
        changed: data.changed,
        skipped: !data.changed,
        elapsed_ms,
        updated_at: now_iso(),
        message: if data.changed {
            format!(
                "OpenHub 已增量采集并写入本地数据库：重扫 {} 个文件，复用 {} 个文件",
                data.scanned_files, data.reused_files
            )
        } else {
            format!(
                "本地日志没有变化，数据库快照已确认（复用 {} 个文件）",
                data.reused_files
            )
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_usage_normalization_separates_cached_input() {
        let usage = CodexUsage {
            input_tokens: 100,
            cached_input_tokens: 80,
            output_tokens: 10,
            total_tokens: 110,
            ..Default::default()
        }
        .normalized();
        assert_eq!(usage.input_tokens, 20);
        assert_eq!(usage.cached_input_tokens, 80);
        assert_eq!(usage.total_tokens, 110);
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
            "claude-opus-4-6-thinking"
        );
        assert_eq!(
            find_ascii_model_token(b"prefix gemini-3.6-flash-high suffix"),
            "gemini-3.6-flash-high"
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
        assert_eq!(session.total_tokens, 155);
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
        assert_eq!(usage_event.total_tokens, 155);
        assert_eq!(usage_event.estimated_tokens, 0);
        assert!(usage_event.pricing_available);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    #[ignore = "reads the current user home for a manual smoke test"]
    fn smoke_collects_real_home() {
        let first_started = Instant::now();
        let first = collect_uncached(true).expect("full collection should succeed");
        let mut by_source = BTreeMap::<String, (usize, i64)>::new();
        for bucket in &first.usage.buckets {
            let entry = by_source.entry(bucket.source.clone()).or_default();
            entry.0 += 1;
            entry.1 += bucket.total_tokens;
        }
        eprintln!(
            "full: elapsed_ms={} buckets={} sessions={} scanned={} reused={} total_tokens={} sources={:?}",
            first_started.elapsed().as_millis(),
            first.usage.buckets.len(),
            first.sessions.len(),
            first.scanned_files,
            first.reused_files,
            first
                .usage
                .buckets
                .iter()
                .map(|bucket| bucket.total_tokens)
                .sum::<i64>(),
            by_source,
        );

        let second_started = Instant::now();
        let second = collect_uncached(false).expect("incremental collection should succeed");
        eprintln!(
            "incremental: elapsed_ms={} buckets={} sessions={} scanned={} reused={} total_tokens={}",
            second_started.elapsed().as_millis(),
            second.usage.buckets.len(),
            second.sessions.len(),
            second.scanned_files,
            second.reused_files,
            second
                .usage
                .buckets
                .iter()
                .map(|bucket| bucket.total_tokens)
                .sum::<i64>()
        );
        assert!(!first.usage.buckets.is_empty());
        assert_eq!(first.sessions.len(), second.sessions.len());
        assert_eq!(
            first
                .usage
                .buckets
                .iter()
                .map(|bucket| bucket.total_tokens)
                .sum::<i64>(),
            second
                .usage
                .buckets
                .iter()
                .map(|bucket| bucket.total_tokens)
                .sum::<i64>()
        );
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
}
