use crate::context::{spawn_blocking, AppContext, EventBus, Managed};
use crate::models::{
    LocalAgentPathsReport, RawConversation, RawLogReport, RawRequest, RawSession,
    RequestHealthReport, TokenCollectorSyncReport, TokenStatsReport, TokenUsageReport,
};
use crate::token::stats::db::{
    collect_token_data, query_token_health, query_token_stats, query_token_usage,
};
use crate::token::stats::raw_logs::{
    collect_codex_files, collect_local_agent_paths, parse_claude_file,
};
use crate::token::stats::types::emit_token_collector_progress;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

/// Token 查询接口只读取 OpenHub SQLite 快照，不触发日志扫描。
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_token_stats(
    ctx: Managed<'_, Arc<AppContext>>,
    from: Option<String>,
    to: Option<String>,
    refresh: Option<bool>,
) -> Result<TokenStatsReport, String> {
    let _ = refresh;
    query_token_stats(&ctx.database, from, to)
}

/// 手动触发一次本地日志采集并写入 SQLite；查询仍由独立接口完成。
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn sync_token_data(
    ctx: Managed<'_, Arc<AppContext>>,
    force: Option<bool>,
) -> Result<TokenCollectorSyncReport, String> {
    let force = force.unwrap_or(false);
    let bus: EventBus = ctx.event_bus.clone();
    let worker_ctx: Arc<AppContext> = Arc::clone(&ctx);
    let worker_bus = bus.clone();
    let result = spawn_blocking(move || {
        emit_token_collector_progress(
            &worker_bus,
            "prepare",
            "running",
            if force {
                "已创建完整刷新任务"
            } else {
                "已创建增量采集任务"
            },
        );
        let result = collect_token_data(&worker_ctx.database, force, Some(&worker_bus));
        if let Err(error) = &result {
            emit_token_collector_progress(&worker_bus, "error", "error", error.clone());
        }
        result
    })
    .await;

    match result {
        Ok(result) => result,
        Err(error) => {
            let message = format!("OpenHub Token 采集任务失败：{error}");
            emit_token_collector_progress(&bus, "error", "error", message.clone());
            Err(message)
        }
    }
}

/// 只查询 SQLite 中的 Token 用量快照。
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_token_usage(
    ctx: Managed<'_, Arc<AppContext>>,
) -> Result<TokenUsageReport, String> {
    query_token_usage(&ctx.database)
}

/// 从原始日志解析 会话/对话/请求 三级列表。
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_token_raw_logs() -> Result<RawLogReport, String> {
    spawn_blocking(|| {
        let home = std::env::var_os("HOME").ok_or("无法定位用户目录")?;
        let home = PathBuf::from(home);
        let mut sessions: Vec<RawSession> = Vec::new();
        let mut conversations: Vec<RawConversation> = Vec::new();
        let mut requests: Vec<RawRequest> = Vec::new();

        let claude_root = crate::token::collector::claude_config_dir(&home).join("projects");
        if let Ok(projects) = fs::read_dir(&claude_root) {
            for project_entry in projects.flatten() {
                let project_dir = project_entry.path();
                if !project_dir.is_dir() {
                    continue;
                }
                let raw_name = project_dir
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default();
                let project =
                    crate::token::collector::normalize_workspace_project_key(&raw_name, "Claude");
                if let Ok(files) = fs::read_dir(&project_dir) {
                    for file in files.flatten() {
                        let path = file.path();
                        if !path.is_file() {
                            continue;
                        }
                        if path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .map(|name| name.ends_with(".jsonl"))
                            .unwrap_or(false)
                        {
                            parse_claude_file(
                                &path,
                                &project,
                                &mut sessions,
                                &mut conversations,
                                &mut requests,
                            );
                        }
                    }
                }
            }
        }

        let codex_base = crate::token::collector::codex_home(&home);
        collect_codex_files(&codex_base.join("sessions"), &mut sessions);
        collect_codex_files(&codex_base.join("archived_sessions"), &mut sessions);

        Ok(RawLogReport {
            available: !sessions.is_empty(),
            sessions,
            conversations,
            requests,
        })
    })
    .await
    .map_err(|error| format!("原始日志解析失败：{error}"))?
}

/// 只读扫描本地 AI Agent 的配置 / 数据路径（不读取日志内容）。
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_local_agent_paths() -> Result<LocalAgentPathsReport, String> {
    spawn_blocking(|| {
        let home = std::env::var_os("HOME").ok_or("无法定位用户目录")?;
        Ok(collect_local_agent_paths(&PathBuf::from(home)))
    })
    .await
    .map_err(|error| format!("本地 Agent 路径扫描失败：{error}"))?
}

/// 查询请求健康时只读取 SQLite；refresh 参数保留用于前端兼容。
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_token_request_health(
    ctx: Managed<'_, Arc<AppContext>>,
    refresh: Option<bool>,
) -> Result<RequestHealthReport, String> {
    let _ = refresh;
    query_token_health(&ctx.database)
}
