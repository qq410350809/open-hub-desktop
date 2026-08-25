use crate::models::{
    TokenCollectorSyncReport, TokenModelStat, TokenSession, TokenStatsReport, TokenSummary,
    TokenUsageBucket, TokenUsageReport,
};
use crate::token::collector::sources::*;
use crate::token::collector::time_utils::{half_hour_key, now_iso};
use crate::token::collector::types::*;
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub fn env_path_override(key: &str) -> Option<PathBuf> {
    let value = std::env::var_os(key)?;
    let path = PathBuf::from(value);
    (!path.as_os_str().is_empty()).then_some(path)
}

pub fn source_file_fingerprint(source: &str, path: &Path) -> FileFingerprint {
    match source {
        "command-code" => command_code_fingerprint(path),
        "antigravity" => antigravity_fingerprint(path),
        "kiro" => kiro_fingerprint(path),
        _ => fingerprint(path),
    }
}

pub fn aggregate_events(events: Vec<UsageEvent>) -> TokenUsageReport {
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

pub fn snapshot_from_envelope(
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

pub fn load_cached_snapshot() -> Option<CollectedData> {
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

pub fn clear_local_cache() -> Result<(), String> {
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

pub fn collected_stats_by_source() -> BTreeMap<String, SourceCollectStats> {
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

pub fn collect_uncached(force: bool) -> Result<CollectedData, String> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("无法定位用户目录")?);
    let mut envelope = if force {
        CollectorEnvelope::default()
    } else {
        read_envelope()
    };
    envelope.version = CACHE_VERSION;

    let mut files = Vec::<(String, PathBuf)>::new();
    let codex_base = codex_home(&home);
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
    files.extend(collect_copilot_source_files(&home));
    files.extend(collect_cline_source_files(&home));
    for path in collect_continue_source_files(&home) {
        files.push(("continue".to_string(), path));
    }
    for path in collect_aider_source_files(&home) {
        files.push(("aider".to_string(), path));
    }
    for path in collect_zed_source_files(&home) {
        files.push(("zed".to_string(), path));
    }
    for path in collect_goose_source_files(&home) {
        files.push(("goose".to_string(), path));
    }
    files.extend(collect_catpawai_source_files(&home));
    for path in collect_vscode_opencode_log_files(&home) {
        files.push(path);
    }
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
            "copilot" => parse_copilot_file(&path),
            "cline" | "roo-code" => parse_cline_file(&source, &path),
            "continue" => parse_continue_file(&path),
            "aider" => parse_aider_file(&path),
            "zed" => parse_zed_file(&path),
            "goose" => parse_goose_file(&path),
            "catpawai" | "openclaw" => parse_catpawai_file(&source, &path),
            "vscode-opencode" => parse_vscode_opencode_log_file(&path),
            _ => parse_claude_file(&path),
        };
        envelope.files.insert(key, parsed);
        scanned_files += 1;
        changed = true;
    }

    let mut database_sources = vec![
        (
            "opencode".to_string(),
            "opencode".to_string(),
            opencode_db_path(&home),
        ),
        ("mimo".to_string(), "mimo".to_string(), mimo_db_path(&home)),
        (
            "zcode".to_string(),
            "zcode".to_string(),
            zcode_db_path(&home),
        ),
    ];
    for (idx, db_path) in catpawai_db_paths(&home).into_iter().enumerate() {
        database_sources.push((format!("catpawai_{idx}"), "catpawai".to_string(), db_path));
    }
    for (idx, db_path) in collect_cursor_db_paths(&home).into_iter().enumerate() {
        database_sources.push((format!("cursor_{idx}"), "cursor".to_string(), db_path));
    }
    for (idx, db_path) in collect_windsurf_db_paths(&home).into_iter().enumerate() {
        database_sources.push((format!("windsurf_{idx}"), "windsurf".to_string(), db_path));
    }

    let live_databases = database_sources
        .iter()
        .filter(|(_, _, path)| path.is_file())
        .map(|(cache_key, _, _)| cache_key.clone())
        .collect::<HashSet<_>>();
    let cached_database_count = envelope.databases.len();
    envelope
        .databases
        .retain(|source, _| live_databases.contains(source));
    changed |= envelope.databases.len() != cached_database_count;

    for (cache_key, source, path) in database_sources {
        if !path.is_file() {
            continue;
        }
        let current = database_fingerprint(&path);
        let reusable = !force
            && envelope
                .databases
                .get(&cache_key)
                .map(|cached| {
                    cached.fingerprint.database.size == current.database.size
                        && cached.fingerprint.database.modified_ms == current.database.modified_ms
                        && cached.fingerprint.wal.size == current.wal.size
                        && cached.fingerprint.wal.modified_ms == current.wal.modified_ms
                })
                .unwrap_or(false);
        if !reusable {
            let parsed = match source.as_str() {
                "cursor" => parse_cursor_database(&path),
                "windsurf" => parse_windsurf_database(&path),
                "opencode" => parse_opencode_database(&path),
                "mimo" => parse_mimo_database(&path),
                "zcode" => parse_zcode_database(&path),
                "catpawai" => parse_catpawai_database(&path),
                _ => parse_opencode_database(&path),
            };
            envelope.databases.insert(cache_key, parsed);
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

pub fn collect(force: bool) -> Result<CollectedData, String> {
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

pub fn collect_snapshot(force: bool) -> Result<CollectedData, String> {
    collect(force)
}

pub fn session_in_range(session: &TokenSession, from: Option<&str>, to: Option<&str>) -> bool {
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

pub fn summary_from_sessions(sessions: &[TokenSession]) -> TokenSummary {
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

pub fn model_stats(sessions: &[TokenSession]) -> Vec<TokenModelStat> {
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

pub fn build_token_stats(
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
            "sources": [
                "codex", "claude", "command-code", "antigravity", "kiro", "dsh",
                "opencode", "mimo", "zcode", "catpawai", "copilot", "cursor",
                "cline", "roo-code", "continue", "aider", "zed", "goose", "windsurf", "openclaw"
            ]
        }),
    }
}

pub fn sync_report(data: &CollectedData, elapsed_ms: i64) -> TokenCollectorSyncReport {
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
                "OpenHub 本地数据库已是最新：{} 个文件均未变更",
                data.reused_files
            )
        },
    }
}
