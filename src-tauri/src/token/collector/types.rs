use crate::models::{TokenSession, TokenSessionTokens, TokenUsageReport};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// v22：Codex input 口径实测修正——原生 OpenAI Responses 的 input_tokens 为总输入（含缓存命中），
/// 需拆分全新输入，否则缓存命中率被腰斩、total 虚高；中转独立口径按事件自动判别。
/// total 口径不变：total = 全新输入 + 缓存命中 + 输出；缓存写入与思考 token 独立。
pub const CACHE_VERSION: i64 = 22;
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

/// 上游 input 字段的缓存语义：决定 normalize_usage 如何拆分全新输入。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputSemantics {
    /// input 即全新输入，不含缓存（Anthropic / codex / opencode / mimo）。
    #[default]
    Fresh,
    /// input 为总输入，已包含缓存命中（OpenAI prompt_tokens / copilot / zcode 类）。
    InclusiveOfCacheRead,
    /// input 为总输入，已包含缓存命中与缓存写入（如某些网关把 cache write 并入 prompt）。
    InclusiveOfAllCache,
}

/// 各源解析出的原始用量（未拆分口径）。
#[derive(Debug, Clone, Copy, Default)]
pub struct RawUsage {
    pub input: i64,
    pub semantics: InputSemantics,
    pub cache_read: i64,
    pub cache_write: i64,
    pub output: i64,
    pub reasoning: i64,
}

/// 统一归一化（全链路唯一口径）：
/// total = 全新输入 + 缓存命中 + 输出；缓存写入与思考 token 独立上报，不计入 total。
/// 返回 (fresh_input, cache_read, cache_write, output, reasoning, total)。
pub fn normalize_usage(raw: RawUsage) -> (i64, i64, i64, i64, i64, i64) {
    let read = raw.cache_read.max(0);
    let write = raw.cache_write.max(0);
    let fresh = match raw.semantics {
        InputSemantics::Fresh => raw.input.max(0),
        InputSemantics::InclusiveOfCacheRead => raw.input.saturating_sub(read).max(0),
        InputSemantics::InclusiveOfAllCache => {
            raw.input.saturating_sub(read).saturating_sub(write).max(0)
        }
    };
    let total = fresh.saturating_add(read).saturating_add(raw.output.max(0));
    (fresh, read, write, raw.output.max(0), raw.reasoning.max(0), total)
}

/// 读取 OpenAI 式 `prompt_tokens_details.cached_tokens`（camelCase/snake_case 兼容）。
/// 这是 prompt 已含缓存时的权威缓存命中字段。
pub fn openai_cached_from_details(usage: &JsonValue) -> i64 {
    usage
        .get("promptTokensDetails")
        .or_else(|| usage.get("prompt_tokens_details"))
        .map(|details| number(details, &["cachedTokens", "cached_tokens"]))
        .unwrap_or(0)
}

/// 读取 OpenAI 式 `completion_tokens_details.reasoning_tokens`。
pub fn openai_reasoning_from_details(usage: &JsonValue) -> i64 {
    usage
        .get("completionTokensDetails")
        .or_else(|| usage.get("completion_tokens_details"))
        .map(|details| number(details, &["reasoningTokens", "reasoning_tokens"]))
        .unwrap_or(0)
}

pub fn open_readonly_sqlite(path: &Path) -> Option<Connection> {
    // 修复：添加重试逻辑处理数据库锁定
    // 当其他进程持有写锁时，等待最多 3 次（每次 100ms）
    // 避免静默失败导致永久数据丢失

    const MAX_RETRIES: u32 = 3;
    const RETRY_DELAY_MS: u64 = 100;

    for attempt in 0..MAX_RETRIES {
        match Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            Ok(conn) => {
                // 设置 busy_timeout 以处理短暂的锁定
                let _ = conn.busy_timeout(Duration::from_millis(500));
                return Some(conn);
            }
            Err(e) => {
                let is_locked = e.to_string().to_lowercase().contains("lock");
                if is_locked && attempt < MAX_RETRIES - 1 {
                    // 数据库被锁定，等待后重试
                    std::thread::sleep(Duration::from_millis(RETRY_DELAY_MS));
                    continue;
                } else if is_locked {
                    // 最后一次重试仍然失败
                    eprintln!(
                        "[SQLite] 警告：数据库被锁定，跳过采集: {}",
                        path.display()
                    );
                    eprintln!("[SQLite] 关闭正在使用该数据库的应用后重试");
                    return None;
                } else {
                    // 其他错误（文件不存在、权限等）
                    return None;
                }
            }
        }
    }

    None
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
