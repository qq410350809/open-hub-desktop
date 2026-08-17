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

// v13：修复 ZCode/OpenCode 用户消息嵌套 modelID 导致的对话轮次无法归属到具体模型（如 GLM-5.3）的问题；
const CACHE_VERSION: i64 = 13;
const CACHE_TTL: Duration = Duration::from_secs(5);
const UNKNOWN_CODEX_MODEL: &str = "codex-unknown-model";
const UNKNOWN_CLAUDE_MODEL: &str = "claude-unknown-model";
const UNKNOWN_OPENCODE_MODEL: &str = "opencode-unknown-model";
const UNKNOWN_COMMAND_CODE_MODEL: &str = "command-code-unknown-model";
const UNKNOWN_ANTIGRAVITY_MODEL: &str = "antigravity-unknown-model";
const UNKNOWN_KIRO_MODEL: &str = "kiro-auto-model";
const UNKNOWN_DSH_MODEL: &str = "dsh-unknown-model";
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

pub(crate) fn kiro_v2_session_root(home: &Path) -> PathBuf {
    home.join(".kiro").join("sessions")
}

/// Kiro 0.x 把会话保存在 VS Code globalStorage 中。新版 Kiro 会把这些
/// v1 JSON 会话迁移到 `~/.kiro/sessions/**/messages.jsonl`，但 Intel Mac 上
/// 经常仍停留在旧版目录，因此两种扩展标识都兼容。
pub(crate) fn kiro_legacy_session_roots(home: &Path) -> Vec<PathBuf> {
    let mut storage_roots = Vec::new();
    #[cfg(target_os = "macos")]
    {
        let global_storage = home
            .join("Library")
            .join("Application Support")
            .join("Kiro")
            .join("User")
            .join("globalStorage");
        storage_roots.push(global_storage.join("kiro.kiroagent"));
        storage_roots.push(global_storage.join("kiro.kiro-agent"));
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(app_data) = std::env::var_os("APPDATA") {
            let global_storage = PathBuf::from(app_data)
                .join("Kiro")
                .join("User")
                .join("globalStorage");
            storage_roots.push(global_storage.join("kiro.kiroagent"));
            storage_roots.push(global_storage.join("kiro.kiro-agent"));
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let global_storage = home
            .join(".config")
            .join("Kiro")
            .join("User")
            .join("globalStorage");
        storage_roots.push(global_storage.join("kiro.kiroagent"));
        storage_roots.push(global_storage.join("kiro.kiro-agent"));
    }

    storage_roots
        .into_iter()
        .flat_map(|root| [root.join("workspace-sessions"), root.join("sessions")])
        .collect()
}

fn kiro_v2_session_id(path: &Path) -> Option<String> {
    fs::read_to_string(kiro_session_metadata_path(path))
        .ok()
        .and_then(|text| serde_json::from_str::<JsonValue>(&text).ok())
        .and_then(|metadata| {
            metadata
                .get("id")
                .and_then(JsonValue::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            path.parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
}

fn kiro_legacy_session_id(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn normalized_kiro_session_id(value: &str) -> &str {
    let value = value.trim();
    value
        .strip_prefix("sess_")
        .or_else(|| value.strip_prefix("sess-"))
        .unwrap_or(value)
}

fn is_kiro_legacy_session_file(path: &Path) -> bool {
    path.extension().and_then(|value| value.to_str()) == Some("json")
        && path
            .file_name()
            .and_then(|value| value.to_str())
            .map(|name| {
                name != "sessions.json"
                    && !name.starts_with("._migration-")
                    && !name.starts_with(".migrated-")
            })
            .unwrap_or(false)
}

fn collect_kiro_source_files(home: &Path) -> Vec<(String, PathBuf)> {
    let mut v2_files = Vec::new();
    collect_jsonl_files(
        &kiro_v2_session_root(home),
        &|path| path.file_name().and_then(|name| name.to_str()) == Some("messages.jsonl"),
        &mut v2_files,
    );
    let migrated_ids = v2_files
        .iter()
        .filter_map(|path| kiro_v2_session_id(path))
        .map(|id| normalized_kiro_session_id(&id).to_string())
        .collect::<HashSet<_>>();

    let mut legacy_files = Vec::new();
    for root in kiro_legacy_session_roots(home) {
        collect_jsonl_files(&root, &is_kiro_legacy_session_file, &mut legacy_files);
    }
    legacy_files.retain(|path| {
        kiro_legacy_session_id(path)
            .map(|id| !migrated_ids.contains(normalized_kiro_session_id(&id)))
            .unwrap_or(false)
    });

    v2_files
        .into_iter()
        .map(|path| ("kiro".to_string(), path))
        .chain(
            legacy_files
                .into_iter()
                .map(|path| ("kiro-legacy".to_string(), path)),
        )
        .collect()
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

/// 清除 OpenHub 自己维护的 Token 解析缓存，不删除任何来源工具的原始日志。
pub(crate) fn clear_local_cache() -> Result<(), String> {
    let _guard = collector_lock()
        .lock()
        .map_err(|_| "OpenHub Token 采集锁异常".to_string())?;
    if let Ok(mut cache) = memory_cache().lock() {
        *cache = None;
    }
    if let Some(path) = collector_cache_path() {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "清除 Token 采集缓存失败（{}）：{error}",
                    path.display()
                ));
            }
        }
        let tmp = path.with_extension("json.tmp");
        match fs::remove_file(&tmp) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "清除 Token 临时缓存失败（{}）：{error}",
                    tmp.display()
                ));
            }
        }
    }
    Ok(())
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
    // 子代理文件位于 <项目>/<会话>/subagents/ 下，项目目录要再往上一层。
    let mut parent = path.parent();
    while parent
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map(|name| name == "subagents")
        .unwrap_or(false)
    {
        parent = parent.and_then(Path::parent);
    }
    let raw = parent
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
    let offset_secs = tz_offset_secs(value);
    if offset_secs == 0 {
        return Some(format!(
            "{prefix}:{:02}:00.000Z",
            if minute < 30 { 0 } else { 30 }
        ));
    }
    // 带时区偏移的时间戳（如 +08:00）：先归一到 UTC 再取半小时桶，
    // 否则本地时间会被当成 UTC，桶位错开数小时。
    let year: i64 = value.get(0..4)?.parse().ok()?;
    let month: i64 = value.get(5..7)?.parse().ok()?;
    let day: i64 = value.get(8..10)?.parse().ok()?;
    let hour: i64 = value.get(11..13)?.parse().ok()?;
    let days = days_from_civil(year, month, day);
    let utc_secs = days * 86_400 + hour * 3_600 + i64::from(minute) * 60 - offset_secs;
    let tod = utc_secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(utc_secs.div_euclid(86_400));
    Some(format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:00.000Z",
        tod / 3_600,
        if (tod % 3_600) / 60 < 30 { 0 } else { 30 }
    ))
}

/// 解析 ISO 时间戳的时区偏移（秒）：
/// `Z` → 0；`+08:00` / `+0800` / `+08` → 28800；`-05:00` → -18000；
/// 没有时区标记时按 0（UTC）处理，与旧口径保持一致。
pub(crate) fn tz_offset_secs(ts: &str) -> i64 {
    let Some(t_index) = ts.find('T') else {
        return 0;
    };
    let Some(zone_start) = ts[t_index..]
        .find(['Z', 'z', '+', '-'])
        .map(|i| t_index + i)
    else {
        return 0;
    };
    match ts.as_bytes()[zone_start] {
        b'Z' | b'z' => 0,
        sign => {
            let positive = sign != b'-';
            let digits: String = ts[zone_start + 1..]
                .chars()
                .filter(|ch| ch.is_ascii_digit())
                .collect();
            let (hours, minutes) = match digits.len() {
                4 => (
                    digits[0..2].parse::<i64>().unwrap_or(0),
                    digits[2..4].parse::<i64>().unwrap_or(0),
                ),
                2 => (digits[0..2].parse::<i64>().unwrap_or(0), 0),
                1 => (digits[0..1].parse::<i64>().unwrap_or(0), 0),
                _ => (0, 0),
            };
            let magnitude = hours * 3_600 + minutes * 60;
            if positive {
                magnitude
            } else {
                -magnitude
            }
        }
    }
}

/// Howard Hinnant days_from_civil（civil_from_days 的逆函数）：年月日 → UTC 天数。
pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let month_shifted = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * month_shifted + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
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

/// Claude Code 的 user 消息里混着非用户输入：tool_result（工具结果）、
/// <local-command-stdout>（斜杠命令输出回显）、[Request interrupted by user...]（Esc 中断）。
/// 这些算成对话轮会让对话数虚高；<command-name> 斜杠命令本身是用户操作，保留。
pub(crate) fn claude_user_is_human(content: &JsonValue) -> bool {
    fn human_text(text: &str) -> bool {
        let trimmed = text.trim();
        !trimmed.is_empty()
            && !trimmed.starts_with("[Request interrupted")
            && !trimmed.starts_with("<local-command-stdout>")
            && !trimmed.starts_with("<command-stdout>")
    }
    match content {
        JsonValue::String(text) => human_text(text),
        JsonValue::Array(items) => {
            // tool_result 与文本混在同一条消息（个别版本在结果后注入 reminder），
            // 不是新一轮用户输入，整体不算。
            if items
                .iter()
                .any(|item| item.get("type").and_then(JsonValue::as_str) == Some("tool_result"))
            {
                return false;
            }
            items.iter().any(|item| {
                if let Some(text) = item.get("text").and_then(JsonValue::as_str) {
                    return human_text(text);
                }
                matches!(item.get("type").and_then(JsonValue::as_str), Some("image"))
            })
        }
        _ => false,
    }
}

/// 判定一行 Claude user 消息是否真人输入（开启新对话轮）。
/// 新版日志带 origin.kind（实测取值 human / task-notification 等），
/// 有该字段时以其为准；旧版本缺失时回退到内容启发式 claude_user_is_human。
pub(crate) fn claude_user_line_is_human(value: &JsonValue, content: &JsonValue) -> bool {
    if !claude_user_is_human(content) {
        return false;
    }
    match value
        .get("origin")
        .and_then(|origin| origin.get("kind"))
        .and_then(JsonValue::as_str)
    {
        Some(kind) => kind == "human",
        None => true,
    }
}

/// Codex 新版把用户消息放在 response_item(message, role=user)，
/// 其中 <environment_context>/<codex_internal_context>/<turn_aborted> 是系统注入，不算用户提问。
pub(crate) fn codex_user_message_is_human(payload: &JsonValue) -> bool {
    for item in payload
        .get("content")
        .and_then(JsonValue::as_array)
        .map(|items| items.as_slice())
        .unwrap_or(&[])
    {
        if let Some(text) = item.get("text").and_then(JsonValue::as_str) {
            let trimmed = text.trim_start();
            if trimmed.is_empty() {
                continue;
            }
            return !(trimmed.starts_with("<environment_context")
                || trimmed.starts_with("<codex_internal_context")
                || trimmed.starts_with("<turn_aborted"));
        }
    }
    false
}

/// DSH 的 user/message 里混着大量非用户输入：runtime context 快照、skill 目录、
/// 插件后台任务通知等（source.kind 为 plugin / skill-catalog 等）。
/// 只有 source.kind == "user" 才是真实用户输入；旧版本没有 source.kind 时按注入文本前缀兜底。
pub(crate) fn dsh_user_is_human(payload: &JsonValue) -> bool {
    let data = payload.get("data").unwrap_or(&JsonValue::Null);
    let kind = data
        .get("source")
        .or_else(|| payload.get("source"))
        .and_then(|source| source.get("kind"))
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    if !kind.is_empty() {
        return kind == "user";
    }
    let content = data
        .get("content")
        .or_else(|| payload.get("content"))
        .unwrap_or(&JsonValue::Null);
    let text = dsh_content_text(content);
    let trimmed = text.trim_start();
    !(trimmed.starts_with("<system-reminder>")
        || trimmed.starts_with("Current runtime context")
        || trimmed.starts_with("[Request")
        || trimmed.starts_with("<environment"))
}

fn dsh_content_text(content: &JsonValue) -> String {
    match content {
        JsonValue::String(text) => text.clone(),
        JsonValue::Array(items) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(JsonValue::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
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
    // 新版 Claude 把子代理会话放在 <会话>/subagents/*.jsonl，文件里的 sessionId 是
    // 父会话 id，直接沿用会与主会话撞 id，因此子代理文件的会话 id 单独合成。
    let is_subagent_file = path
        .components()
        .any(|component| component.as_os_str() == "subagents");
    let mut model = String::new();
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut user_events: BTreeMap<String, String> = BTreeMap::new();
    let mut usage_events: BTreeMap<String, UsageEvent> = BTreeMap::new();
    // 请求的时间锚定到「本轮 user 请求」：assistant 的用量事件挂在最近一次用户输入上，
    // 让对话轮数与请求/token 落到同一个时间桶（避免长回合跨小时时出现“有 token 无轮数”）。
    let mut last_user_ts = String::new();
    // 对话轮要归属到「本轮实际响应的模型」，而不是会话最终模型——会话中途切换模型时，
    // 否则轮数会被统一挂到最后一个模型上，导致按模型看“有 token 的模型对话数=0”。
    let mut user_models: BTreeMap<String, String> = BTreeMap::new();
    let mut pending_user_ids: Vec<String> = Vec::new();

    for (index, line) in text.lines().enumerate() {
        let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };
        let is_sidechain = value
            .get("isSidechain")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        if !is_sidechain {
            if let Some(value) = value
                .get("sessionId")
                .or_else(|| value.get("session_id"))
                .and_then(JsonValue::as_str)
                .filter(|value| !value.trim().is_empty())
            {
                session_id = value.to_string();
            }
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
            // 子代理的任务 prompt（sidechain user）不是真人对话轮，跳过；
            // 它触发的请求由下方 assistant 分支正常计入，归属当前对话。
            if is_sidechain {
                continue;
            }
            let content = value
                .get("message")
                .and_then(|message| message.get("content"))
                .unwrap_or(&JsonValue::Null);
            if !claude_user_line_is_human(&value, content) {
                continue;
            }
            last_user_ts = timestamp.clone();
            let id = value
                .get("uuid")
                .and_then(JsonValue::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("{session_id}:user:{index}"));
            user_events.entry(id.clone()).or_insert(timestamp);
            pending_user_ids.push(id);
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
        // 本轮第一个带用量的 assistant 请求决定这一轮 user 事件归属的模型。
        let turn_model = if message_model.is_empty() {
            UNKNOWN_CLAUDE_MODEL.to_string()
        } else {
            message_model.to_string()
        };
        for pending_id in pending_user_ids.drain(..) {
            user_models.entry(pending_id).or_insert(turn_model.clone());
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
            timestamp: if last_user_ts.is_empty() {
                timestamp
            } else {
                last_user_ts.clone()
            },
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
    events.extend(user_events.into_iter().map(|(id, timestamp)| {
        UsageEvent {
            id: format!("u:{id}"),
            source: "claude".to_string(),
            model: user_models
                .get(&id)
                .cloned()
                .unwrap_or_else(|| model.clone()),
            project_key: project_key.clone(),
            timestamp,
            conversation_count: 1,
            ..Default::default()
        }
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
        if is_subagent_file {
            format!("{session_id}:agent:{fallback_id}")
        } else {
            session_id
        },
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

fn is_model_noise(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "gemini-pro-agent"
        || lower == "gemini-pro-default"
        || lower == "claude-login"
        || lower == "claude-code-gui"
        || lower.starts_with("gpt-migration-")
        || lower.starts_with("gpt-update-")
        || lower.starts_with("claude-unknown-model")
        || lower.starts_with("antigravity-unknown-model")
}

fn normalize_model_slug(raw: &str) -> String {
    let mut s = raw.trim();
    if let Some(pos) = s.rfind('(') {
        if s.ends_with(')') {
            s = s[..pos].trim();
        }
    }
    let mut s = s.to_ascii_lowercase();
    for suffix in [
        "-high", "-medium", "-low", "-thinking", "_high", "_medium", "_low", "_thinking",
    ] {
        if s.ends_with(suffix) {
            s.truncate(s.len() - suffix.len());
            break;
        }
    }
    let mut result = String::with_capacity(s.len());
    let mut last_dash = false;
    for c in s.chars() {
        if c == ' ' || c == '_' || c == '-' {
            if !last_dash && !result.is_empty() {
                result.push('-');
                last_dash = true;
            }
        } else if c.is_ascii_alphanumeric() || c == '.' {
            result.push(c);
            last_dash = false;
        }
    }
    result.trim_end_matches('-').to_string()
}

fn find_display_model_name(bytes: &[u8]) -> Option<String> {
    const DISPLAY_PREFIXES: [&[u8]; 5] = [b"Gemini ", b"Claude ", b"GPT-", b"DeepSeek-", b"Qwen"];
    for index in 0..bytes.len() {
        let Some(prefix) = DISPLAY_PREFIXES
            .iter()
            .find(|prefix| bytes[index..].starts_with(prefix))
        else {
            continue;
        };
        let mut end = index + prefix.len();
        let mut paren_depth = 0;
        while end < bytes.len() {
            let b = bytes[end];
            if b == b'(' {
                paren_depth += 1;
                end += 1;
            } else if b == b')' {
                if paren_depth > 0 {
                    paren_depth -= 1;
                    end += 1;
                    if paren_depth == 0 {
                        break;
                    }
                } else {
                    break;
                }
            } else if b.is_ascii_alphanumeric() || matches!(b, b' ' | b'.' | b'-' | b'_') {
                end += 1;
            } else {
                break;
            }
        }
        let candidate = String::from_utf8_lossy(&bytes[index..end]).trim().to_string();
        if candidate.len() >= 4 && !candidate.starts_with("Gemini 0") && !is_model_noise(&candidate) {
            let lower = candidate.to_ascii_lowercase();
            if lower.contains("flash")
                || lower.contains("pro")
                || lower.contains("sonnet")
                || lower.contains("opus")
                || lower.contains("haiku")
                || lower.contains("ultra")
                || lower.contains("gpt-")
                || lower.contains("deepseek-")
                || lower.contains("qwen")
                || lower.chars().any(|c| c.is_ascii_digit())
            {
                return Some(candidate);
            }
        }
    }
    None
}

fn find_slug_model_name(bytes: &[u8]) -> Option<String> {
    const SLUG_PREFIXES: [&[u8]; 5] = [b"gemini-", b"claude-", b"gpt-", b"deepseek-", b"qwen-"];
    for index in 0..bytes.len() {
        let Some(prefix) = SLUG_PREFIXES
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
        let candidate = String::from_utf8_lossy(&bytes[index..end])
            .trim_end_matches('.')
            .to_string();
        if candidate.len() > prefix.len() && !is_model_noise(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn find_ascii_model_token(bytes: &[u8]) -> String {
    let chunks: &[&[u8]] = if bytes.len() > 8192 {
        &[&bytes[bytes.len() - 8192..], bytes]
    } else {
        &[bytes]
    };

    for chunk in chunks {
        if let Some(model) = find_slug_model_name(chunk) {
            let normalized = normalize_model_slug(&model);
            if !normalized.is_empty() {
                return normalized;
            }
        }
    }

    for chunk in chunks {
        if let Some(model) = find_display_model_name(chunk) {
            let normalized = normalize_model_slug(&model);
            if !normalized.is_empty() {
                return normalized;
            }
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

    let mut model = String::new();
    if let Ok(mut stmt) = conn.prepare("SELECT data FROM gen_metadata ORDER BY idx DESC LIMIT 10") {
        if let Ok(rows) = stmt.query_map([], |row| row.get::<_, Vec<u8>>(0)) {
            for row in rows.flatten() {
                let candidate = find_ascii_model_token(&row);
                if !candidate.is_empty() {
                    model = candidate;
                    break;
                }
            }
        }
    }

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

fn json_timestamp(value: &JsonValue) -> String {
    for key in ["timestamp", "createdAt", "created_at", "time", "date"] {
        let Some(raw) = value.get(key) else {
            continue;
        };
        if let Some(timestamp) = raw
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return timestamp.to_string();
        }
        let millis = raw
            .as_i64()
            .or_else(|| raw.as_u64().map(|value| value.min(i64::MAX as u64) as i64));
        if let Some(value) = millis {
            let millis = if value > 0 && value < 10_000_000_000 {
                value.saturating_mul(1_000)
            } else {
                value
            };
            return iso_from_millis(millis);
        }
    }
    String::new()
}

fn parse_kiro_legacy_file(path: &Path) -> CachedFile {
    let file_fingerprint = fingerprint(path);
    let Ok(text) = fs::read_to_string(path) else {
        return CachedFile {
            fingerprint: file_fingerprint,
            ..Default::default()
        };
    };
    let Ok(root) = serde_json::from_str::<JsonValue>(&text) else {
        return CachedFile {
            fingerprint: file_fingerprint,
            ..Default::default()
        };
    };
    let session_id = root
        .get("sessionId")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| kiro_legacy_session_id(path))
        .unwrap_or_else(|| "kiro-v1-session".to_string());
    let project_key = ["workspaceDirectory", "workspacePath", "cwd"]
        .into_iter()
        .find_map(|key| root.get(key).and_then(JsonValue::as_str))
        .map(|path| basename_or_fallback(path, "Kiro"))
        .unwrap_or_else(|| "Kiro".to_string());
    let model = ["modelId", "selectedModel"]
        .into_iter()
        .find_map(|key| root.get(key).and_then(JsonValue::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| UNKNOWN_KIRO_MODEL.to_string());
    let fallback_timestamp = {
        let timestamp = json_timestamp(&root);
        if timestamp.is_empty() {
            iso_from_millis(file_fingerprint.modified_ms.min(i64::MAX as u64) as i64)
        } else {
            timestamp
        }
    };
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut visible_context_tokens = 0i64;
    let mut turns = 0i64;
    let mut assistant_responses = 0i64;
    let mut events = Vec::<UsageEvent>::new();

    for (index, item) in root
        .get("history")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let message = item.get("message").unwrap_or(item);
        let role = message
            .get("role")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        let content = message.get("content").unwrap_or(&JsonValue::Null);
        let timestamp = {
            let item_timestamp = json_timestamp(item);
            if item_timestamp.is_empty() {
                let message_timestamp = json_timestamp(message);
                if message_timestamp.is_empty() {
                    fallback_timestamp.clone()
                } else {
                    message_timestamp
                }
            } else {
                item_timestamp
            }
        };
        update_bounds(&mut first_ts, &mut last_ts, &timestamp);
        let event_id = format!("{session_id}:v1:{index}:{role}");
        match role {
            "user" => {
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
                visible_context_tokens =
                    visible_context_tokens.saturating_add(estimate_local_content_tokens(content));
            }
            "assistant" => {
                assistant_responses += 1;
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
            "system" => {
                visible_context_tokens =
                    visible_context_tokens.saturating_add(estimate_local_content_tokens(content));
            }
            _ => {}
        }
    }

    let tokens = events
        .iter()
        .fold(TokenSessionTokens::default(), |mut total, event| {
            total.input_tokens += event.input_tokens;
            total.output_tokens += event.output_tokens;
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
        "tokenUsage": "estimated-kiro-v1-local-context",
        "storageFormat": "kiro-global-storage-v1",
        "assistantResponses": assistant_responses
    });
    CachedFile {
        fingerprint: file_fingerprint,
        events,
        sessions: vec![session],
    }
}

fn extract_model_from_transcript_content(content: &str) -> Option<String> {
    if !content.contains("Model Selection") {
        return None;
    }
    let marker = "Model Selection` from ";
    let start = content.find(marker)?;
    let sub = &content[start + marker.len()..];
    let to_pos = sub.find(" to ")?;
    let candidate_sub = &sub[to_pos + 4..];
    let end_pos = candidate_sub
        .find(".\n")
        .or_else(|| candidate_sub.find(". "))
        .or_else(|| candidate_sub.find(".\r"))
        .or_else(|| candidate_sub.find("."))
        .unwrap_or(candidate_sub.len());
    let candidate = candidate_sub[..end_pos].trim();
    if !candidate.is_empty() && candidate.len() < 60 && !candidate.eq_ignore_ascii_case("none") {
        let normalized = normalize_model_slug(candidate);
        if !normalized.is_empty() && !is_model_noise(&normalized) {
            return Some(normalized);
        }
    }
    None
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
    let mut model = if database_model.is_empty() {
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
        if model == UNKNOWN_ANTIGRAVITY_MODEL {
            if let Some(content) = value.get("content").and_then(JsonValue::as_str) {
                if let Some(candidate) = extract_model_from_transcript_content(content) {
                    model = candidate;
                }
            }
        }
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

    if model != UNKNOWN_ANTIGRAVITY_MODEL {
        for event in &mut events {
            if event.model == UNKNOWN_ANTIGRAVITY_MODEL {
                event.model = model.clone();
            }
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

/// DSH (DeepSeek AI CLI) 会话日志解析。
/// 日志路径: ~/.dsh/sessions/<cwd-hash>/session-<uuid>/session.jsonl.zstd
/// 格式: zstd 压缩的 JSONL，每行一个事件对象。
/// usage 来源:
///   - assistant/message 事件: data.usage (最终汇总)
///   - assistant/chunk 事件: data.chunk.usage (增量，作为 fallback)
/// 模型名: data.message.source.model
/// 时间戳: 毫秒级 epoch，需转 ISO
fn parse_dsh_file(path: &Path) -> CachedFile {
    let fp = fingerprint(path);
    let Ok(raw) = fs::read(path) else {
        return CachedFile {
            fingerprint: fp,
            ..Default::default()
        };
    };
    let text = match zstd::decode_all(raw.as_slice()) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => {
                return CachedFile {
                    fingerprint: fp,
                    ..Default::default()
                }
            }
        },
        Err(_) => {
            return CachedFile {
                fingerprint: fp,
                ..Default::default()
            }
        }
    };

    let mut session_id = String::new();
    let mut project_key = String::new();
    let mut model = String::new();
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut user_events: BTreeMap<String, String> = BTreeMap::new();
    let mut usage_events: BTreeMap<String, UsageEvent> = BTreeMap::new();
    // 请求的时间锚定到「本轮 user 请求」，与 Claude 解析器同口径。
    let mut last_user_ts = String::new();
    // 对话轮归属到本轮实际响应的模型，而非会话最终模型（与 Claude 解析器同口径）。
    let mut user_models: BTreeMap<String, String> = BTreeMap::new();
    let mut pending_user_ids: Vec<String> = Vec::new();

    for (index, line) in text.lines().enumerate() {
        let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };
        let kind = value.get("type").and_then(JsonValue::as_str).unwrap_or("");
        let time_ms = value.get("time").and_then(JsonValue::as_i64).unwrap_or(0);
        let timestamp = iso_from_millis(time_ms);

        if kind == "session" {
            if let Some(id) = value
                .get("id")
                .and_then(JsonValue::as_str)
                .filter(|v| !v.is_empty())
            {
                session_id = id.to_string();
            }
            if let Some(cwd) = value
                .get("cwd")
                .and_then(JsonValue::as_str)
                .filter(|v| !v.is_empty())
            {
                project_key = basename_or_fallback(cwd, &project_key);
            }
            if !timestamp.is_empty() {
                update_bounds(&mut first_ts, &mut last_ts, &timestamp);
            }
            continue;
        }

        if kind == "user/message" {
            if !timestamp.is_empty() {
                update_bounds(&mut first_ts, &mut last_ts, &timestamp);
            }
            // 只有真实用户输入算对话轮；runtime context / skill 目录等注入不算。
            if dsh_user_is_human(&value) {
                last_user_ts = timestamp.clone();
                let id = format!("dsh:user:{index}");
                user_events.entry(id.clone()).or_insert(timestamp);
                pending_user_ids.push(id);
            }
            continue;
        }

        if kind == "assistant/message" {
            let data = value.get("data").unwrap_or(&JsonValue::Null);
            if !timestamp.is_empty() {
                update_bounds(&mut first_ts, &mut last_ts, &timestamp);
            }
            let msg = data.get("message").unwrap_or(&JsonValue::Null);
            if let Some(m) = msg
                .get("source")
                .and_then(|s| s.get("model"))
                .and_then(JsonValue::as_str)
                .filter(|v| !v.is_empty())
            {
                model = m.to_string();
            }
            // 优先从 data.usage 取（最终汇总）
            if let Some(usage) = data.get("usage").filter(|u| u.is_object()) {
                // 本轮第一个带用量的 assistant 请求决定这一轮 user 事件归属的模型。
                for pending_id in pending_user_ids.drain(..) {
                    user_models
                        .entry(pending_id)
                        .or_insert_with(|| model.clone());
                }
                let anchor_ts = if last_user_ts.is_empty() {
                    timestamp.clone()
                } else {
                    last_user_ts.clone()
                };
                let event =
                    dsh_usage_event(usage, &session_id, &project_key, &model, &anchor_ts, index);
                if let Some(ev) = event {
                    let msg_id = ev.id.clone();
                    let total = ev.total_tokens;
                    let should_replace = usage_events
                        .get(&msg_id)
                        .map(|ex| total > ex.total_tokens)
                        .unwrap_or(true);
                    if should_replace {
                        usage_events.insert(msg_id, ev);
                    }
                }
            }
            continue;
        }

        if kind == "assistant/chunk" {
            let data = value.get("data").unwrap_or(&JsonValue::Null);
            if !timestamp.is_empty() {
                update_bounds(&mut first_ts, &mut last_ts, &timestamp);
            }
            // 从 chunk.usage 取增量 usage 作为 fallback（仅当没有 assistant/message 的汇总时）
            if let Some(usage) = data
                .get("chunk")
                .and_then(|c| c.get("usage"))
                .filter(|u| u.is_object())
            {
                let turn = data.get("turn").and_then(JsonValue::as_i64).unwrap_or(0);
                let step = data.get("step").and_then(JsonValue::as_i64).unwrap_or(0);
                let anchor_ts = if last_user_ts.is_empty() {
                    timestamp.clone()
                } else {
                    last_user_ts.clone()
                };
                let event =
                    dsh_usage_event(usage, &session_id, &project_key, &model, &anchor_ts, index);
                if let Some(ev) = event {
                    let chunk_id = format!("{}:chunk:{}:{}", ev.id, turn, step);
                    let should_replace = usage_events
                        .get(&chunk_id)
                        .map(|ex| ev.total_tokens > ex.total_tokens)
                        .unwrap_or(true);
                    if should_replace {
                        usage_events.insert(chunk_id.clone(), UsageEvent { id: chunk_id, ..ev });
                    }
                }
            }
            continue;
        }
    }

    if session_id.is_empty() {
        session_id = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("dsh-session")
            .to_string();
    }
    if project_key.is_empty() {
        project_key = "DSH".to_string();
    }
    if model.is_empty() {
        model = UNKNOWN_DSH_MODEL.to_string();
    }

    // DSH 的 assistant/message 有 data.usage 汇总时优先用它；
    // 如果只有 chunk usage（没有 message usage），则 chunk 级事件保留。
    // 如果两者都有，message 级的 id 会覆盖同 id 的 chunk 级（因为 id 相同）。
    let has_message_usage = usage_events.keys().any(|k| !k.contains(":chunk:"));
    if has_message_usage {
        usage_events.retain(|k, _| !k.contains(":chunk:"));
    }

    let mut events = usage_events.into_values().collect::<Vec<_>>();
    events.extend(user_events.into_iter().map(|(id, timestamp)| {
        UsageEvent {
            id: id.clone(),
            source: "dsh".to_string(),
            model: user_models
                .get(&id)
                .cloned()
                .unwrap_or_else(|| model.clone()),
            project_key: project_key.clone(),
            timestamp,
            conversation_count: 1,
            ..Default::default()
        }
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
    let turns = events.iter().map(|e| e.conversation_count).sum();
    let session = token_session(
        session_id,
        "dsh",
        project_key,
        model,
        first_ts,
        last_ts,
        turns,
        tokens,
        0.0,
    );
    CachedFile {
        fingerprint: fp,
        events,
        sessions: vec![session],
    }
}

fn dsh_usage_event(
    usage: &JsonValue,
    session_id: &str,
    project_key: &str,
    model: &str,
    timestamp: &str,
    index: usize,
) -> Option<UsageEvent> {
    let input = number(usage, &["inputTokens", "input_tokens"]);
    let cached = number(usage, &["cacheReadTokens", "cache_read_input_tokens"]);
    let output = number(usage, &["outputTokens", "output_tokens"]);
    let total = input.saturating_add(cached).saturating_add(output);
    if total <= 0 || timestamp.is_empty() {
        return None;
    }
    let id = format!("{session_id}:dsh:{index}");
    Some(UsageEvent {
        id,
        source: "dsh".to_string(),
        model: if model.is_empty() {
            UNKNOWN_DSH_MODEL.to_string()
        } else {
            model.to_string()
        },
        project_key: project_key.to_string(),
        timestamp: timestamp.to_string(),
        input_tokens: input,
        cached_input_tokens: cached,
        cache_creation_input_tokens: 0,
        output_tokens: output,
        reasoning_output_tokens: 0,
        total_tokens: total,
        conversation_count: 0,
        cost_usd: 0.0,
        pricing_available: false,
        estimated_tokens: 0,
    })
}

fn parse_codex_file(path: &Path) -> CachedFile {
    let Ok(text) = fs::read_to_string(path) else {
        return CachedFile {
            fingerprint: fingerprint(path),
            ..Default::default()
        };
    };
    // 判断 Codex 版本：旧版用 event_msg(user_message) 记用户轮；新版只用 response_item(message, role=user)。
    let has_user_message_events = text.lines().any(|line| {
        serde_json::from_str::<JsonValue>(line)
            .map(|v| {
                v.get("type").and_then(JsonValue::as_str) == Some("event_msg")
                    && v.get("payload")
                        .and_then(|p| p.get("type"))
                        .and_then(JsonValue::as_str)
                        == Some("user_message")
            })
            .unwrap_or(false)
    });
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
        if kind == "response_item" {
            if !has_user_message_events
                && payload.get("type").and_then(JsonValue::as_str) == Some("message")
                && payload.get("role").and_then(JsonValue::as_str) == Some("user")
                && codex_user_message_is_human(payload)
            {
                let id = payload
                    .get("id")
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

/// ZCode 的内置/自定义 provider 都属于自身数据；仅排除会被其他采集器读取的
/// 子代理 provider（anthropic/openai/google）。按 ':' '/' 分段精确匹配，
/// 避免误伤名字里恰好含这些单词的自定义中转（如 "my-openai-relay"）。
fn zcode_provider_allowed(provider: &str) -> bool {
    !provider.is_empty()
        && !provider
            .split([':', '/'])
            .any(|segment| matches!(segment, "anthropic" | "openai" | "google"))
}

fn database_message_allowed(source: &str, value: &JsonValue) -> bool {
    let provider = database_provider(value);
    match source {
        // MiMo 数据库会镜像 Claude 会话；只保留 MiMo 自己的 provider。
        "mimo" => provider == "mimo" || provider == "xiaomi",
        "zcode" => zcode_provider_allowed(&provider),
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

fn database_model(value: &JsonValue, source: &str) -> String {
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
        .unwrap_or_else(|| unknown_database_model(source))
}

/// OpenCode 系 DB 的 token 分项口径拆分，返回 (全新输入, 缓存读取, 缓存写入, 输出, 思考)。
/// ZCode 的 tokens.input 是完整 prompt，cache.read/write 是其中的子集（同 OpenAI 的
/// prompt_tokens ⊇ cached_tokens 口径），必须扣除后才是全新输入；OpenCode/MiCo 的
/// input 本身不含缓存。混用会把缓存命中率拉低近一半，total 也会重复累计缓存 Token。
fn database_token_parts(source: &str, tokens: &JsonValue) -> (i64, i64, i64, i64, i64) {
    let cache = tokens.get("cache").unwrap_or(&JsonValue::Null);
    let cached = number(cache, &["read"]);
    let cache_creation = number(cache, &["write"]);
    let input_total = number(tokens, &["input"]);
    let input = if source == "zcode" {
        input_total
            .saturating_sub(cached)
            .saturating_sub(cache_creation)
            .max(0)
    } else {
        input_total
    };
    let output = number(tokens, &["output"]);
    let reasoning = number(tokens, &["reasoning"]);
    (input, cached, cache_creation, output, reasoning)
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
                let mut model = database_model(&value, source);
                let unknown = unknown_database_model(source);
                if model == unknown && !session.model.is_empty() {
                    model = session.model.clone();
                } else if session.model.is_empty() && model != unknown {
                    session.model = model.clone();
                    for event in &mut events {
                        if event.id.starts_with(&format!("u:{session_id}:")) && event.model == unknown {
                            event.model = model.clone();
                        }
                    }
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

                let (input, cached, cache_creation, output, reasoning) =
                    database_token_parts(source, value.get("tokens").unwrap_or(&JsonValue::Null));
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
        // 一条用量事件（conversation_count == 0 且有 token）就是一次真实 API 请求；
        // 估算事件（estimated_tokens > 0，来源未上报 usage）同样代表一次模型调用。
        if event.conversation_count == 0 && (event.total_tokens > 0 || event.estimated_tokens > 0) {
            bucket.request_count += 1;
        }
        bucket.cost_usd += event.cost_usd;
        bucket.pricing_available |= event.pricing_available;
        bucket.estimated_tokens += event.estimated_tokens;
        if event.estimated_tokens > 0 {
            bucket.estimated_input_tokens += event.input_tokens;
        }
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

fn env_path_override(key: &str) -> Option<PathBuf> {
    let value = std::env::var_os(key)?;
    let path = PathBuf::from(value);
    (!path.as_os_str().is_empty()).then_some(path)
}

// —— 各工具数据目录解析：优先遵循工具自身的重定向环境变量 ——
// Claude Code 支持 CLAUDE_CONFIG_DIR、Codex 支持 CODEX_HOME、OpenCode/MiCo 遵循
// XDG_DATA_HOME；不读这些变量的话，用户一旦重定向，采集会静默归零。
// 注意：GUI 从 Finder 启动时继承不到 shell 配置的变量，dev（npm run desktop）
// 或 launchctl setenv 设置后才可见；未设置时行为与原来完全一致。

pub(crate) fn claude_config_dir(home: &Path) -> PathBuf {
    env_path_override("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude"))
}

pub(crate) fn codex_home(home: &Path) -> PathBuf {
    env_path_override("CODEX_HOME").unwrap_or_else(|| home.join(".codex"))
}

pub(crate) fn xdg_data_home(home: &Path) -> PathBuf {
    env_path_override("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local").join("share"))
}

pub(crate) fn opencode_db_path(home: &Path) -> PathBuf {
    xdg_data_home(home).join("opencode").join("opencode.db")
}

pub(crate) fn mimo_db_path(home: &Path) -> PathBuf {
    xdg_data_home(home).join("mimocode").join("mimocode.db")
}

pub(crate) fn zcode_db_path(home: &Path) -> PathBuf {
    home.join(".zcode").join("cli").join("db").join("db.sqlite")
}

#[derive(Default)]
pub(crate) struct SourceCollectStats {
    pub(crate) sessions: usize,
    pub(crate) events: usize,
    /// 采集缓存的最近更新时间（ISO）。
    pub(crate) updated_at: String,
}

/// 汇总采集缓存里每个来源的会话 / 用量事件量。
/// 「本地 Agent 路径」弹窗用它区分「路径存在」和「实际采到了数据」。
pub(crate) fn collected_stats_by_source() -> BTreeMap<String, SourceCollectStats> {
    let envelope = read_envelope();
    let mut map = BTreeMap::<String, SourceCollectStats>::new();
    fn bump(
        map: &mut BTreeMap<String, SourceCollectStats>,
        source: &str,
        sessions: usize,
        events: usize,
    ) {
        let entry = map.entry(source.to_string()).or_default();
        entry.sessions += sessions;
        entry.events += events;
    }
    for cached in envelope.files.values() {
        for session in &cached.sessions {
            bump(&mut map, &session.source, 1, 0);
        }
        for event in &cached.events {
            bump(&mut map, &event.source, 0, 1);
        }
    }
    for cached in envelope.databases.values() {
        for session in &cached.sessions {
            bump(&mut map, &session.source, 1, 0);
        }
        for event in &cached.events {
            bump(&mut map, &event.source, 0, 1);
        }
    }
    let updated_at = envelope.updated_at.clone();
    for stats in map.values_mut() {
        stats.updated_at = updated_at.clone();
    }
    map
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
    let codex_base = codex_home(&home);
    // Codex 会把归档任务从 sessions/ 移到 archived_sessions/；两处都要扫描。
    // 后续按 session id + usage 事件签名全局去重，因此文件移动不会重复计数。
    let mut codex_files = Vec::new();
    for codex_root in [
        codex_base.join("sessions"),
        codex_base.join("archived_sessions"),
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

    let claude_root = claude_config_dir(&home).join("projects");
    let mut claude_files = Vec::new();
    // subagents/ 目录（新版 Claude 的子代理会话）也纳入：其中的 API 请求同样消耗
    // token，属于当前对话的请求；解析时通过 isSidechain 标记区分子代理 user 输入。
    collect_jsonl_files(
        &claude_root,
        &|path| path.extension().and_then(|value| value.to_str()) == Some("jsonl"),
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
    files.extend(collect_kiro_source_files(&home));
    let dsh_root = home.join(".dsh").join("sessions");
    let mut dsh_files = Vec::new();
    collect_jsonl_files(
        &dsh_root,
        &|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.ends_with(".jsonl.zstd"))
                .unwrap_or(false)
        },
        &mut dsh_files,
    );
    files.extend(dsh_files.into_iter().map(|path| ("dsh".to_string(), path)));
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
            "dsh" => parse_dsh_file(&path),
            "command-code" => parse_command_code_file(&path),
            "antigravity" => parse_antigravity_file(&path),
            "kiro" => parse_kiro_file(&path),
            "kiro-legacy" => parse_kiro_legacy_file(&path),
            _ => parse_claude_file(&path),
        };
        envelope.files.insert(key, parsed);
        scanned_files += 1;
        changed = true;
    }

    let database_sources = [
        ("opencode", opencode_db_path(&home)),
        ("mimo", mimo_db_path(&home)),
        ("zcode", zcode_db_path(&home)),
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
    fn zcode_provider_filter_matches_vendor_segments_only() {
        // 生产实测形态：内置 provider 与自定义 UUID provider 都要保留。
        assert!(zcode_provider_allowed("builtin:zai-start-plan"));
        assert!(zcode_provider_allowed(
            "b87fa901-a05f-4afa-8c18-beccb90f9ef6"
        ));
        // 空值与其他采集器会重复计数的官方 provider 排除。
        assert!(!zcode_provider_allowed(""));
        assert!(!zcode_provider_allowed("anthropic"));
        assert!(!zcode_provider_allowed("builtin:anthropic"));
        assert!(!zcode_provider_allowed("openai/gpt-5"));
        // 名字里恰好含厂商单词的自定义中转不能被误伤（旧 contains 逻辑会误杀）。
        assert!(zcode_provider_allowed("my-openai-relay"));
        assert!(zcode_provider_allowed("anthropic-proxy-123"));
    }

    #[test]
    fn zcode_database_input_includes_cache_read_subset() {
        // 生产实测（GLM-5.3 高命中请求）：input=253_602、cache.read=252_608，
        // input 是完整 prompt，命中部分必须扣除才是全新输入。
        let tokens =
            json!({"input": 253_602, "output": 500, "cache": {"read": 252_608, "write": 0}});
        let (input, cached, cache_creation, output, reasoning) =
            database_token_parts("zcode", &tokens);
        assert_eq!(
            (input, cached, cache_creation, output, reasoning),
            (994, 252_608, 0, 500, 0)
        );
    }

    #[test]
    fn opencode_database_input_stays_fresh_only() {
        // OpenCode/MiMo 的 input 不含缓存分项，保持原样累加。
        let tokens = json!({"input": 1_000, "output": 100, "cache": {"read": 9_000, "write": 500}});
        let (input, cached, cache_creation, output, _) = database_token_parts("opencode", &tokens);
        assert_eq!(
            (input, cached, cache_creation, output),
            (1_000, 9_000, 500, 100)
        );
    }

    #[test]
    fn zcode_database_input_clamps_at_zero_when_cache_exceeds() {
        let tokens = json!({"input": 100, "cache": {"read": 150, "write": 10}});
        let (input, _, _, _, _) = database_token_parts("zcode", &tokens);
        assert_eq!(input, 0);
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
        assert_eq!(database_model(&user_msg, "zcode"), "GLM-5.3");
        assert_eq!(database_provider(&user_msg), "builtin:zai-start-plan");
    }

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

        // 用户粘贴的 SVG 以 < 开头，但不是系统注入，应计为用户消息
        let svg =
            json!({"content": [{"type": "input_text", "text": "<svg width=\"24\">...</svg>"}]});
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
        // 用量事件只计一次请求
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
        // 半小时桶内：1 次请求 + 1 轮对话
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
        assert_eq!(
            find_ascii_model_token(b"\xaa\x01\x06GPT-4o\x00"),
            "gpt-4o"
        );
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

    #[test]
    fn half_hour_bucket_normalizes_timezone_offset() {
        // +08:00 本地 03:29 = UTC 前一天 19:29 → 19:00 桶
        assert_eq!(
            half_hour_key("2026-08-12T03:29:59.000+08:00").as_deref(),
            Some("2026-08-11T19:00:00.000Z")
        );
        // -05:00 本地 23:45 = UTC 次日 04:45 → 04:30 桶
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
        // 旧版本无 origin 字段：回退内容启发式
        let legacy = json!({"message": {"role": "user", "content": "hello"}});
        assert!(claude_user_line_is_human(
            &legacy,
            &legacy["message"]["content"]
        ));
    }

    #[test]
    fn claude_user_is_human_excludes_injected_messages() {
        // 真实输入
        assert!(claude_user_is_human(
            &json!([{"type": "text", "text": "hello"}])
        ));
        assert!(claude_user_is_human(&json!("hello")));
        // 工具结果
        assert!(!claude_user_is_human(
            &json!([{"type": "tool_result", "content": "ok"}])
        ));
        // 斜杠命令本身算用户操作，输出回显不算
        assert!(claude_user_is_human(&json!(
            "<command-name>/compact</command-name>"
        )));
        assert!(!claude_user_is_human(&json!(
            [{"type": "text", "text": "<local-command-stdout>done</local-command-stdout>"}]
        )));
        assert!(!claude_user_is_human(&json!(
            [{"type": "text", "text": "<command-stdout>done</command-stdout>"}]
        )));
        // Esc 中断不算
        assert!(!claude_user_is_human(&json!(
            [{"type": "text", "text": "[Request interrupted by user for tool use]"}]
        )));
        // tool_result 与文本混在同一条消息（个别版本注入 reminder）不算
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
        // 每个 user 事件归属到本轮第一个 assistant 的模型，而不是会话最终模型。
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
        // 真实用户输入（带/不带 source.kind）
        assert!(dsh_user_is_human(&json!({
            "data": {"source": {"kind": "user"}, "content": [{"type": "text", "text": "hi"}]}
        })));
        assert!(dsh_user_is_human(&json!({"data": {"content": "hi"}})));
        // 注入消息
        assert!(!dsh_user_is_human(&json!({
            "data": {"source": {"kind": "plugin"}, "content": [{"type": "text", "text": "background job finished"}]}
        })));
        assert!(!dsh_user_is_human(&json!({
            "data": {"source": {"kind": "skill-catalog"}, "content": [{"type": "text", "text": "<system-reminder>…</system-reminder>"}]}
        })));
        // 旧格式无 source.kind 时按注入文本前缀兜底
        assert!(!dsh_user_is_human(&json!({
            "data": {"content": [{"type": "text", "text": "<system-reminder>…</system-reminder>"}]}
        })));
        assert!(!dsh_user_is_human(&json!({
            "data": {"content": [{"type": "text", "text": "Current runtime context. This snapshot supersedes…"}]}
        })));
    }
}
