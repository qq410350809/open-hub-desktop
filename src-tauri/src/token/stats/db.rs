use crate::db;
use crate::models::{
    Database, RequestHealthReport, TokenCollectorSyncReport, TokenStatsReport, TokenUsageReport,
};
use crate::token::stats::catpawai::merge_catpawai_usage;
use crate::token::stats::health::collect_request_health_snapshot;
use crate::token::stats::types::*;
use std::time::Instant;
use tauri::AppHandle;

pub fn query_token_usage(database: &Database) -> Result<TokenUsageReport, String> {
    Ok(db::read_token_usage_snapshot(database)?.unwrap_or_default())
}

pub fn query_token_stats(
    database: &Database,
    from: Option<String>,
    to: Option<String>,
) -> Result<TokenStatsReport, String> {
    let sessions = db::read_token_sessions_snapshot(database)?.unwrap_or_default();
    Ok(crate::token::collector::build_token_stats(sessions, from, to))
}

pub fn query_token_health(database: &Database) -> Result<RequestHealthReport, String> {
    Ok(db::read_token_health_snapshot(database)?.unwrap_or_default())
}

pub fn collect_token_data(
    database: &Database,
    force: bool,
    progress_app: Option<&AppHandle>,
) -> Result<TokenCollectorSyncReport, String> {
    let _guard = token_collection_lock()
        .lock()
        .map_err(|_| "Token 数据采集锁异常".to_string())?;
    let started = Instant::now();
    if force {
        emit_token_collector_progress(
            progress_app,
            "cache",
            "running",
            "正在清除 OpenHub 本地 Token 缓存与数据库快照",
        );
        crate::token::collector::clear_local_cache()?;
        clear_request_health_cache()?;
        db::clear_token_snapshots(database)?;
        emit_token_collector_progress(
            progress_app,
            "cache",
            "success",
            "本地缓存已清除，来源工具的原始日志保持不变",
        );
    }
    emit_token_collector_progress(
        progress_app,
        "scan",
        "running",
        "正在扫描 Codex、Claude 等工具的本地日志",
    );
    let snapshot = crate::token::collector::collect_snapshot(force)?;
    emit_token_collector_progress(
        progress_app,
        "scan",
        "success",
        format!(
            "日志扫描完成：重扫 {} 个文件，复用 {} 个文件",
            snapshot.scanned_files, snapshot.reused_files
        ),
    );
    emit_token_collector_progress(
        progress_app,
        "aggregate",
        "running",
        "正在合并 Token 用量、会话与请求健康数据",
    );
    let usage = merge_catpawai_usage(snapshot.usage.clone())?;
    let health = collect_request_health_snapshot(force)?;
    emit_token_collector_progress(
        progress_app,
        "aggregate",
        "success",
        format!("数据汇总完成：{} 个会话", snapshot.sessions.len()),
    );
    emit_token_collector_progress(
        progress_app,
        "database",
        "running",
        "正在写入 OpenHub 本地数据库",
    );
    db::write_token_snapshots(database, &usage, &snapshot.sessions, &health)?;
    emit_token_collector_progress(progress_app, "database", "success", "数据库快照写入完成");
    let mut report =
        crate::token::collector::sync_report(&snapshot, started.elapsed().as_millis() as i64);
    if force {
        report.changed = true;
        report.skipped = false;
        report.message = format!(
            "已清除本地 Token 缓存并重新拉取计算：重扫 {} 个文件",
            snapshot.scanned_files
        );
    }
    Ok(report)
}

pub fn seed_token_database_from_caches(database: &Database) -> Result<bool, String> {
    if db::has_token_snapshots(database)? {
        return Ok(false);
    }
    let Some(snapshot) = crate::token::collector::load_cached_snapshot() else {
        return Ok(false);
    };
    let usage = merge_catpawai_usage(snapshot.usage)?;
    let health = read_persisted_activity_cache().report;
    db::write_token_snapshots(database, &usage, &snapshot.sessions, &health)?;
    Ok(true)
}
