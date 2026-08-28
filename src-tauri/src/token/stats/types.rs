use crate::context::EventBus;
use crate::models::{RequestHealthBucket, RequestHealthReport, RequestHealthSourceSummary};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

pub const CATPAWAI_SOURCE: &str = "catpawai";
pub const ACTIVITY_CACHE_VERSION: u32 = 8;
pub const ACTIVITY_CACHE_TTL: Duration = Duration::from_secs(15);
pub const TOKEN_COLLECT_INTERVAL: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenCollectorProgress {
    pub stage: String,
    pub status: String,
    pub message: String,
}

pub fn emit_token_collector_progress(
    bus: &EventBus,
    stage: &str,
    status: &str,
    message: impl Into<String>,
) {
    bus.emit(
        "token-collector-progress",
        TokenCollectorProgress {
            stage: stage.into(),
            status: status.into(),
            message: message.into(),
        },
    );
}

#[derive(Clone, Default)]
pub struct HealthAgg {
    pub dialogues: i64,
    pub requests: i64,
    pub success: i64,
    pub failed: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FileCursor {
    pub inode: u64,
    pub size: u64,
    pub mtime_ms: u64,
    pub offset: u64,
}

pub type FileCursorMap = BTreeMap<String, FileCursor>;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SqliteCursor {
    pub max_time_created: i64,
    #[serde(default)]
    pub allowed_sessions: HashSet<String>,
    pub session_users: BTreeMap<String, Vec<(i64, String)>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ActivityCacheEnvelope {
    pub version: u32,
    pub file_cursors: BTreeMap<String, FileCursorMap>,
    pub sqlite_cursors: BTreeMap<String, SqliteCursor>,
    pub report: RequestHealthReport,
}

pub struct ActivityCache {
    pub report: RequestHealthReport,
    pub fetched_at: Instant,
}

pub fn activity_cache() -> &'static Mutex<Option<ActivityCache>> {
    static CACHE: OnceLock<Mutex<Option<ActivityCache>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

pub fn token_collection_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub fn activity_cache_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("OPENHUB_ACTIVITY_CACHE_PATH") {
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
                .join("token-activity-cache.json"),
        );
    }
    #[cfg(target_os = "windows")]
    {
        return std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|path| path.join(dir_name).join("token-activity-cache.json"));
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Some(
            home.join(".local")
                .join("share")
                .join(dir_name)
                .join("token-activity-cache.json"),
        )
    }
}

pub fn read_persisted_activity_cache() -> ActivityCacheEnvelope {
    let Some(path) = activity_cache_path() else {
        return ActivityCacheEnvelope::default();
    };
    let Ok(text) = fs::read_to_string(path) else {
        return ActivityCacheEnvelope::default();
    };
    let Ok(envelope) = serde_json::from_str::<ActivityCacheEnvelope>(&text) else {
        return ActivityCacheEnvelope::default();
    };
    if envelope.version != ACTIVITY_CACHE_VERSION {
        return ActivityCacheEnvelope::default();
    }
    envelope
}

pub fn write_persisted_activity_cache(envelope: &ActivityCacheEnvelope) {
    let Some(path) = activity_cache_path() else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(json) = serde_json::to_vec(envelope) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, json).is_ok() {
        let _ = fs::rename(tmp, path);
    }
}

pub fn clear_request_health_cache() -> Result<(), String> {
    if let Ok(mut cache) = activity_cache().lock() {
        *cache = None;
    }
    if let Some(path) = activity_cache_path() {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "清除 Token 请求健康缓存失败（{}）：{error}",
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
                    "清除 Token 请求健康临时缓存失败（{}）：{error}",
                    tmp.display()
                ));
            }
        }
    }
    Ok(())
}

pub fn sqlite_table_exists(conn: &Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count > 0)
    .unwrap_or(false)
}

pub fn hour_key_from_ts(ts: &str) -> Option<String> {
    let cleaned = ts.trim();
    if cleaned.len() < 13 {
        return None;
    }
    let prefix = &cleaned[..13];
    if !(prefix.as_bytes().get(4) == Some(&b'-')
        && prefix.as_bytes().get(7) == Some(&b'-')
        && prefix.as_bytes().get(10) == Some(&b'T'))
    {
        return None;
    }
    let offset_secs = crate::token::collector::tz_offset_secs(cleaned);
    if offset_secs == 0 {
        return Some(format!("{prefix}:00:00.000Z"));
    }
    let year: i64 = cleaned.get(0..4)?.parse().ok()?;
    let month: i64 = cleaned.get(5..7)?.parse().ok()?;
    let day: i64 = cleaned.get(8..10)?.parse().ok()?;
    let hour: i64 = cleaned.get(11..13)?.parse().ok()?;
    let days = crate::token::collector::days_from_civil(year, month, day);
    let utc_secs = days * 86_400 + hour * 3_600 - offset_secs;
    let tod = utc_secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(utc_secs.div_euclid(86_400));
    Some(format!(
        "{y:04}-{m:02}-{d:02}T{:02}:00:00.000Z",
        tod / 3_600
    ))
}

pub fn hour_key_from_millis(ms: i64) -> Option<String> {
    if ms <= 0 {
        return None;
    }
    let secs = ms / 1000;
    let t = UNIX_EPOCH + Duration::from_secs(secs as u64);
    let datetime = t.duration_since(UNIX_EPOCH).ok()?;
    let total_secs = datetime.as_secs() as i64;
    let days = total_secs.div_euclid(86_400);
    let tod = total_secs.rem_euclid(86_400);
    let hour = tod / 3600;
    let (y, m, d) = civil_from_days(days);
    Some(format!("{y:04}-{m:02}-{d:02}T{hour:02}:00:00.000Z"))
}

pub fn civil_from_days(days: i64) -> (i32, u32, u32) {
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

pub fn json_i64(value: &JsonValue, key: &str) -> i64 {
    value
        .get(key)
        .and_then(|v| v.as_f64().or_else(|| v.as_i64().map(|n| n as f64)))
        .map(|n| n as i64)
        .unwrap_or(0)
}

pub fn is_user_cancelled_error(err: &JsonValue) -> bool {
    let mut parts: Vec<String> = Vec::new();
    fn walk(v: &JsonValue, out: &mut Vec<String>) {
        match v {
            JsonValue::String(s) => out.push(s.to_ascii_lowercase()),
            JsonValue::Object(map) => {
                for (k, val) in map {
                    out.push(k.to_ascii_lowercase());
                    walk(val, out);
                }
            }
            JsonValue::Array(arr) => {
                for val in arr {
                    walk(val, out);
                }
            }
            _ => {}
        }
    }
    walk(err, &mut parts);
    let blob = parts.join(" ");
    blob.contains("cancel")
        || blob.contains("aborted")
        || blob.contains("abort")
        || blob.contains("interrupted")
        || blob.contains("user_cancelled")
        || blob.contains("cancelled_by_user")
}

pub fn bump(
    map: &mut BTreeMap<String, HealthAgg>,
    hour: String,
    dialogues: i64,
    requests: i64,
    success: i64,
    failed: i64,
) {
    let entry = map.entry(hour).or_default();
    entry.dialogues += dialogues;
    entry.requests += requests;
    entry.success += success;
    entry.failed += failed;
}

pub fn bump_source(
    sources: &mut BTreeMap<String, HealthAgg>,
    source: &str,
    dialogues: i64,
    requests: i64,
    success: i64,
    failed: i64,
) {
    let entry = sources.entry(source.to_string()).or_default();
    entry.dialogues += dialogues;
    entry.requests += requests;
    entry.success += success;
    entry.failed += failed;
}

pub fn record(
    map: &mut BTreeMap<String, HealthAgg>,
    sources: &mut BTreeMap<String, HealthAgg>,
    source: &str,
    hour: String,
    dialogues: i64,
    requests: i64,
    success: i64,
    failed: i64,
) {
    bump(map, hour, dialogues, requests, success, failed);
    bump_source(sources, source, dialogues, requests, success, failed);
}

pub fn open_readonly_sqlite(path: &Path) -> Option<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()
}

#[cfg(unix)]
pub fn metadata_ino(meta: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.ino()
}

#[cfg(not(unix))]
pub fn metadata_ino(_meta: &fs::Metadata) -> u64 {
    0
}

pub fn assistant_tokens_positive(data: &JsonValue) -> bool {
    let Some(tokens) = data.get("tokens") else {
        return false;
    };
    json_i64(tokens, "input")
        + json_i64(tokens, "output")
        + json_i64(tokens, "reasoning")
        + json_i64(tokens, "total")
        > 0
}

pub fn message_hour(value: &JsonValue, time_created: i64) -> Option<String> {
    value
        .get("time")
        .and_then(|t| {
            t.get("completed")
                .or_else(|| t.get("created"))
                .and_then(JsonValue::as_i64)
        })
        .and_then(hour_key_from_millis)
        .or_else(|| hour_key_from_millis(time_created))
}

pub fn report_to_maps(
    report: &RequestHealthReport,
) -> (BTreeMap<String, HealthAgg>, BTreeMap<String, HealthAgg>) {
    let mut map: BTreeMap<String, HealthAgg> = BTreeMap::new();
    let mut sources: BTreeMap<String, HealthAgg> = BTreeMap::new();
    for bucket in &report.buckets {
        let entry = map.entry(bucket.hour.clone()).or_default();
        entry.dialogues += bucket.dialogues;
        entry.requests += bucket.requests;
        entry.success += bucket.success;
        entry.failed += bucket.failed;
    }
    for summary in &report.by_source {
        let entry = sources.entry(summary.source.clone()).or_default();
        entry.dialogues += summary.dialogues;
        entry.requests += summary.requests;
        entry.success += summary.success;
        entry.failed += summary.failed;
    }
    (map, sources)
}

pub fn maps_to_report(
    map: BTreeMap<String, HealthAgg>,
    sources: BTreeMap<String, HealthAgg>,
) -> RequestHealthReport {
    let buckets = map
        .into_iter()
        .map(|(hour, agg)| RequestHealthBucket {
            hour,
            dialogues: agg.dialogues,
            requests: agg.requests,
            success: agg.success,
            failed: agg.failed,
        })
        .collect::<Vec<_>>();
    let by_source = sources
        .into_iter()
        .map(|(source, agg)| RequestHealthSourceSummary {
            source,
            dialogues: agg.dialogues,
            requests: agg.requests,
            success: agg.success,
            failed: agg.failed,
        })
        .collect::<Vec<_>>();
    RequestHealthReport {
        available: !buckets.is_empty(),
        buckets,
        preceding_buckets: Vec::new(),
        by_source,
    }
}

pub fn local_timestamp() -> String {
    if let Ok(output) = std::process::Command::new("/bin/date")
        .arg("+%Y-%m-%d %H:%M:%S")
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            let value = text.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let tod = secs % 86_400;
    format!(
        "1970-01-01 {:02}:{:02}:{:02} UTC+{days}d",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}
