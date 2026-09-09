//! 本地工具配置管理的 Tauri / RPC 命令层。
//!
//! 全部命令实时读盘（无 DB 缓存），写入前自动备份，
//! 并以 content_hash 做保存冲突检测。仅本地数据平面可用
//! （web_server.is_local_only_command 拦截 HTTP 调用）。
//!
//! 页面仅收录支持结构化编辑的工具（无 token 记录但支持编辑的也会显示）。

use std::path::{Path, PathBuf};

use crate::context::spawn_blocking;
use crate::local_tools::adapters::{find_adapter, full_adapters};
use crate::local_tools::backup;
use crate::local_tools::types::{
    conflict_error, ToolBackupEntry, ToolConfigPatch, ToolConfigSaveResult, ToolConfigSnapshot,
    ToolId, ToolListReport, ToolOverview,
};

/// 应用数据目录（备份根）。与数据库同目录：
/// `~/Library/Application Support/{app_support_dir_name}`（macOS 语义，
/// 交叉平台走 dirs 风格手动拼接，避免依赖 tauri::Manager）。
fn app_data_dir() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("无法定位用户目录")?;
    let base = match std::env::var_os("XDG_DATA_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => PathBuf::from(home).join("Library").join("Application Support"),
    };
    Ok(base.join(crate::core::profile::app_support_dir_name()))
}

/// token 采集统计（source → sessions/events）。
fn collected_stats() -> std::collections::BTreeMap<String, crate::token::collector::SourceCollectStats>
{
    crate::token::collector::aggregator::collected_stats_by_source()
}

/// 全部支持结构化编辑的工具概览。
pub(crate) fn build_tool_list(home: &Path) -> ToolListReport {
    let stats = collected_stats();
    let mut tools = Vec::new();

    for adapter in full_adapters() {
        let (detected, root) = adapter.detect(home);
        let snap_stat = adapter.snapshot(home).ok();
        let (provider_count, default_model) = match &snap_stat {
            Some(snap) => (snap.providers.len(), snap.defaults.model.clone()),
            None => (0, String::new()),
        };
        let id = adapter.id();
        let collected = stats.get(id.as_str());
        tools.push(ToolOverview {
            tool: id.as_str().to_string(),
            tool_name: id.display_name().to_string(),
            detected,
            has_token_records: collected.is_some(),
            collected_sessions: collected.map(|s| s.sessions).unwrap_or(0),
            collected_events: collected.map(|s| s.events).unwrap_or(0),
            root,
            provider_count,
            default_model,
            effect_note: adapter.effect_note().to_string(),
            provider_mode: id.provider_mode(),
        });
    }

    // 展示顺序：有 token 记录的在前，其余按名称。
    tools.sort_by(|a, b| {
        b.has_token_records
            .cmp(&a.has_token_records)
            .then_with(|| a.tool_name.cmp(&b.tool_name))
    });

    let collected_at = stats
        .values()
        .next()
        .map(|s| s.updated_at.clone())
        .unwrap_or_default();
    ToolListReport {
        available: true,
        home: home.display().to_string(),
        tools,
        collected_at,
    }
}

/// 列出全部工具概览。
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn list_local_tools() -> Result<ToolListReport, String> {
    spawn_blocking(|| {
        let home = std::env::var_os("HOME").ok_or("无法定位用户目录")?;
        Ok(build_tool_list(&PathBuf::from(home)))
    })
    .await
    .map_err(|error| format!("本地工具扫描失败：{error}"))?
}

/// 读取单个工具的结构化配置快照。
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_local_tool_config(tool: String) -> Result<ToolConfigSnapshot, String> {
    let id = ToolId::from_str_value(&tool).ok_or_else(|| format!("未知工具：{tool}"))?;
    let adapter = find_adapter(id).ok_or_else(|| format!("{tool} 不支持结构化配置管理"))?;
    spawn_blocking(move || {
        let home = std::env::var_os("HOME").ok_or("无法定位用户目录")?;
        adapter.snapshot(&PathBuf::from(home))
    })
    .await
    .map_err(|error| format!("配置读取失败：{error}"))?
}

/// 保存工具配置：hash 冲突检测 → 备份 → 合并写入 → 返回新快照。
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn save_local_tool_config(
    tool: String,
    patch: ToolConfigPatch,
) -> Result<ToolConfigSaveResult, String> {
    let id = ToolId::from_str_value(&tool).ok_or_else(|| format!("未知工具：{tool}"))?;
    let adapter = find_adapter(id).ok_or_else(|| format!("{tool} 不支持结构化配置管理"))?;
    spawn_blocking(move || {
        let home = std::env::var_os("HOME").ok_or("无法定位用户目录")?;
        let home = PathBuf::from(home);

        // 冲突检测：当前磁盘内容哈希必须与 base_hash 一致。
        let current = adapter.snapshot(&home)?;
        if !patch.base_hash.is_empty() && patch.base_hash != current.content_hash {
            return Err(conflict_error(current.tool_name.as_str()));
        }

        // 备份涉及的全部配置文件。
        let files: Vec<(String, PathBuf)> = adapter
            .config_files(&home)
            .into_iter()
            .map(|(_kind, label, path)| (label, path))
            .collect();
        let backup_refs: Vec<(String, &Path)> = files
            .iter()
            .map(|(label, path)| (label.clone(), path.as_path()))
            .collect();
        let data_dir = app_data_dir()?;
        let backup_name = backup::create_backup(&data_dir, id.as_str(), &backup_refs)?;

        // 合并写入。
        let written = adapter.apply(&home, &patch)?;

        // 重新快照返回。
        let snapshot = adapter.snapshot(&home)?;
        let _ = written;
        Ok(ToolConfigSaveResult {
            snapshot,
            backup_name,
        })
    })
    .await
    .map_err(|error| format!("配置保存失败：{error}"))?
}

/// 列出某工具的备份。
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn list_local_tool_backups(tool: String) -> Result<Vec<ToolBackupEntry>, String> {
    let id = ToolId::from_str_value(&tool).ok_or_else(|| format!("未知工具：{tool}"))?;
    find_adapter(id).ok_or_else(|| format!("{tool} 不支持结构化配置管理"))?;
    spawn_blocking(move || {
        let data_dir = app_data_dir()?;
        Ok(backup::list_backups(&data_dir, id.as_str()))
    })
    .await
    .map_err(|error| format!("备份列表读取失败：{error}"))?
}

/// 还原某份备份。
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn restore_local_tool_backup(tool: String, name: String) -> Result<(), String> {
    let id = ToolId::from_str_value(&tool).ok_or_else(|| format!("未知工具：{tool}"))?;
    let adapter = find_adapter(id).ok_or_else(|| format!("{tool} 不支持结构化配置管理"))?;
    spawn_blocking(move || {
        let home = std::env::var_os("HOME").ok_or("无法定位用户目录")?;
        let home = PathBuf::from(home);
        let data_dir = app_data_dir()?;
        // 先备份当前状态再还原，保证还原操作本身可撤销。
        let files: Vec<(String, PathBuf)> = adapter
            .config_files(&home)
            .into_iter()
            .map(|(_kind, label, path)| (label, path))
            .collect();
        let backup_refs: Vec<(String, &Path)> = files
            .iter()
            .map(|(label, path)| (label.clone(), path.as_path()))
            .collect();
        backup::create_backup(&data_dir, id.as_str(), &backup_refs)?;
        backup::restore_backup(&data_dir, id.as_str(), &name, &files)
    })
    .await
    .map_err(|error| format!("备份还原失败：{error}"))?
}
