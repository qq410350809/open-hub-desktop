use crate::models::{TokenSession, TokenSessionTokens, TokenUsageReport};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// v16：修复 VS Code Copilot 会话增量操作日志（kind 0/1/2）解析，强制全量重扫。
pub const CACHE_VERSION: i64 = 19;
pub const CACHE_TTL: Duration = Duration::from_secs(5);
pub const UNKNOWN_CODEX_MODEL: &str = "codex-unknown-model";
pub const UNKNOWN_CLAUDE_MODEL: &str = "claude-unknown-model";
pub const UNKNOWN_OPENCODE_MODEL: &str = "opencode-unknown-model";
pub const UNKNOWN_COMMAND_CODE_MODEL: &str = "command-code-unknown-model";
pub const UNKNOWN_ANTIGRAVITY_MODEL: &str = "antigravity-unknown-model";
pub const UNKNOWN_KIRO_MODEL: &str = "kiro-auto-model";
pub const UNKNOWN_DSH_MODEL: &str = "dsh-unknown-model";
pub const UNKNOWN_COPILOT_MODEL: &str = "copilot-auto-model";
pub const LOCAL_ESTIMATED_CONTEXT_LIMIT: i64 = 64_000;

pub fn xdg_data_home(home: &Path) -> PathBuf {
    crate::token::collector::aggregator::env_path_override("XDG_DATA_HOME")
        .unwrap_or_else(|| home.join(".local").join("share"))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FileFingerprint {
    pub size: u64,
    pub modified_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DatabaseFingerprint {
    pub database: FileFingerprint,
    pub wal: FileFingerprint,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UsageEvent {
    pub id: String,
    pub source: String,
    pub model: String,
    pub project_key: String,
    pub timestamp: String,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_output_tokens: i64,
    pub total_tokens: i64,
    pub conversation_count: i64,
    pub cost_usd: f64,
    pub pricing_available: bool,
    pub estimated_tokens: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CachedFile {
    pub fingerprint: FileFingerprint,
    pub events: Vec<UsageEvent>,
    pub sessions: Vec<TokenSession>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CachedDatabase {
    pub fingerprint: DatabaseFingerprint,
    pub events: Vec<UsageEvent>,
    pub sessions: Vec<TokenSession>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CollectorEnvelope {
    pub version: i64,
    pub updated_at: String,
    pub files: BTreeMap<String, CachedFile>,
    pub databases: BTreeMap<String, CachedDatabase>,
}

#[derive(Debug, Clone, Default)]
pub struct CollectedData {
    pub usage: TokenUsageReport,
    pub sessions: Vec<TokenSession>,
    pub changed: bool,
    pub scanned_files: usize,
    pub reused_files: usize,
}

pub struct CollectorMemoryCache {
    pub data: CollectedData,
    pub fetched_at: Instant,
}

pub fn memory_cache() -> &'static Mutex<Option<CollectorMemoryCache>> {
    static CACHE: OnceLock<Mutex<Option<CollectorMemoryCache>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

pub fn collector_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub fn collector_cache_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("OPENHUB_TOKEN_CACHE_PATH") {
        return Some(PathBuf::from(path));
    }
    let home = PathBuf::from(std::env::var_os("HOME")?);
    // 目录名随运行形态隔离：dev 写 -dev 后缀目录，避免污染正式版采集缓存。
    let dir_name = crate::core::profile::app_support_dir_name();
    #[cfg(target_os = "macos")]
    {
        return Some(
            home.join("Library")
                .join("Application Support")
                .join(dir_name)
                .join("token-collector-cache.json"),
        );
    }
    #[cfg(target_os = "windows")]
    {
        return std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|path| path.join(dir_name).join("token-collector-cache.json"));
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Some(
            home.join(".local")
                .join("share")
                .join(dir_name)
                .join("token-collector-cache.json"),
        )
    }
}

pub fn fingerprint(path: &Path) -> FileFingerprint {
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

pub fn database_fingerprint(path: &Path) -> DatabaseFingerprint {
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    DatabaseFingerprint {
        database: fingerprint(path),
        wal: fingerprint(&wal),
    }
}

pub fn read_envelope() -> CollectorEnvelope {
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

pub fn write_envelope(envelope: &CollectorEnvelope) {
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

pub fn collect_jsonl_files(root: &Path, accept: &dyn Fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
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

pub fn number(value: &JsonValue, keys: &[&str]) -> i64 {
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

pub fn float_number(value: &JsonValue, keys: &[&str]) -> f64 {
    for key in keys {
        if let Some(number) = value.get(*key).and_then(JsonValue::as_f64) {
            if number.is_finite() {
                return number.max(0.0);
            }
        }
    }
    0.0
}

pub fn open_readonly_sqlite(path: &Path) -> Option<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()
}

pub fn token_session(
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
    TokenSession {
        version: 1,
        session_hash: format!("openhub:{source}:{id}"),
        source: source.to_string(),
        project_key,
        model,
        started_at,
        ended_at,
        active_ms: 0,
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
        duration_ms: 0,
        total_tokens,
        cost_usd,
        productive: turns > 0 && total_tokens > 0,
        first_pass: false,
        one_shot: turns == 1,
        tokens_per_edit: None,
        cost_per_edit: None,
    }
}

#[derive(Default)]
#[allow(dead_code)]
pub struct LocalDatabaseSession {
    pub directory: String,
    pub model: String,
    pub started_at: String,
    pub ended_at: String,
    pub turns: i64,
    pub cost_usd: f64,
    pub tokens: TokenSessionTokens,
}

#[derive(Default)]
pub struct SourceCollectStats {
    pub sessions: usize,
    pub events: usize,
    pub updated_at: String,
}
