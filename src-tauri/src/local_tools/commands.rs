//! 本地工具配置管理的 Tauri / RPC 命令层。
//!
//! 全部命令实时读盘（无 DB 缓存），写入前按「标识 + 指纹」判定是否需要备份
//! （无标识 / 被外部改过才备份，我们上次写入后未被动过的直接覆盖），
//! 并以 content_hash 做保存冲突检测。仅本地数据平面可用
//! （web_server.is_local_only_command 拦截 HTTP 调用）。
//!
//! 页面仅收录支持结构化编辑的工具（无 token 记录但支持编辑的也会显示）。

use std::path::{Path, PathBuf};

use crate::context::spawn_blocking;
use crate::local_tools::adapters::{find_adapter, full_adapters};
use crate::local_tools::backup;
use crate::local_tools::mark::ManagedState;
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
        _ => PathBuf::from(home)
            .join("Library")
            .join("Application Support"),
    };
    Ok(base.join(crate::core::profile::app_support_dir_name()))
}

/// 全部支持结构化编辑的工具概览。
///
/// 只做存在性探测（stat 配置根目录/关键文件），不解析配置内容；
/// 供应商数、默认模型等摘要在选中工具后由 get_local_tool_config 按需读取。
pub(crate) fn build_tool_list(home: &Path) -> ToolListReport {
    let mut tools = Vec::new();

    for adapter in full_adapters() {
        let (detected, root) = adapter.detect(home);
        let id = adapter.id();
        tools.push(ToolOverview {
            tool: id.as_str().to_string(),
            tool_name: id.display_name().to_string(),
            detected,
            has_token_records: false,
            collected_sessions: 0,
            collected_events: 0,
            root,
            provider_count: 0,
            default_model: String::new(),
            effect_note: adapter.effect_note().to_string(),
            provider_mode: id.provider_mode(),
        });
    }

    // 展示顺序按名称。
    tools.sort_by(|a, b| a.tool_name.cmp(&b.tool_name));

    ToolListReport {
        available: true,
        home: home.display().to_string(),
        tools,
        collected_at: String::new(),
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

        let data_dir = app_data_dir()?;
        save_with_policy(adapter, &home, &data_dir, &patch)
    })
    .await
    .map_err(|error| format!("配置保存失败：{error}"))?
}

/// 保存主体：hash 冲突检测 → 按「标识 + 指纹」决定是否备份 → 合并写入 → 重新快照。
///
/// 备份判定按文件：指纹吻合 = 上次就是我们写的且此后没人动过 → 直接覆盖不备份；
/// 无标识（用户/其他软件的原文）或指纹对不上（我们写入后被改过）→ 先备份再覆盖。
pub(crate) fn save_with_policy(
    adapter: &dyn crate::local_tools::adapters::ToolAdapter,
    home: &Path,
    data_dir: &Path,
    patch: &ToolConfigPatch,
) -> Result<ToolConfigSaveResult, String> {
    let id = adapter.id();
    // 冲突检测：当前磁盘内容哈希必须与 base_hash 一致。
    let current = adapter.snapshot(home)?;
    if !patch.base_hash.is_empty() && patch.base_hash != current.content_hash {
        return Err(conflict_error(current.tool_name.as_str()));
    }

    let files: Vec<(String, PathBuf)> = adapter
        .config_files(home)
        .into_iter()
        .map(|(_kind, label, path)| (label, path))
        .collect();
    let needs_backup: Vec<(String, &Path)> = current
        .files
        .iter()
        .filter(|f| f.exists && f.managed != ManagedState::Intact)
        .filter_map(|f| {
            files
                .iter()
                .find(|(label, _)| *label == f.label)
                .map(|(label, path)| (label.clone(), path.as_path()))
        })
        .collect();
    let backed_up: Vec<String> = needs_backup.iter().map(|(l, _)| l.clone()).collect();
    let backup_name = if needs_backup.is_empty() {
        String::new()
    } else {
        backup::create_backup(data_dir, id.as_str(), &needs_backup)?
    };

    // 合并写入。
    adapter.apply(home, patch)?;

    // 重新快照返回。
    let snapshot = adapter.snapshot(home)?;
    Ok(ToolConfigSaveResult {
        snapshot,
        backup_name,
        backed_up,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_tools::adapters::claude::ClaudeAdapter;
    use crate::local_tools::adapters::ToolAdapter;
    use crate::local_tools::types::{ModelEntry, ProviderEntry};

    fn temp_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "openhub-lt-policy-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("home").join(".claude")).unwrap();
        dir
    }

    fn patch(
        adapter: &dyn crate::local_tools::adapters::ToolAdapter,
        home: &Path,
        model: &str,
    ) -> ToolConfigPatch {
        let snap = adapter.snapshot(home).unwrap();
        ToolConfigPatch {
            base_hash: snap.content_hash,
            providers: vec![ProviderEntry {
                id: "openhub-site_a_acc_0".into(),
                name: "a".into(),
                base_url: "http://127.0.0.1:1/".into(),
                api_key: "k".into(),
                protocol: "anthropic".into(),
                models: vec![],
            }],
            models: vec![ModelEntry {
                id: model.into(),
                name: model.into(),
                provider: "openhub-site_a_acc_0".into(),
                ..Default::default()
            }],
            defaults: crate::local_tools::types::DefaultsSection {
                model: model.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn backup_only_when_file_is_foreign_or_edited_since_our_write() {
        let root = temp_root();
        let home = root.join("home");
        let data = root.join("data");
        let adapter = &ClaudeAdapter;
        let settings = home.join(".claude").join("settings.json");

        // 1. 用户原有文件，没有本软件标识 → 首次写入必须备份
        std::fs::write(&settings, "{\n  \"permissions\": {}\n}\n").unwrap();
        let snap = adapter.snapshot(&home).unwrap();
        assert_eq!(snap.files[0].managed, ManagedState::Unmanaged);
        let r = save_with_policy(adapter, &home, &data, &patch(adapter, &home, "m1")).unwrap();
        assert!(!r.backup_name.is_empty(), "无标识的原文必须先备份");
        assert_eq!(r.backed_up, vec!["settings.json"]);
        assert_eq!(
            r.snapshot.files[0].managed,
            ManagedState::Intact,
            "写入后指纹吻合"
        );
        assert!(std::fs::read_to_string(&settings)
            .unwrap()
            .contains("openhub-fp-"));

        // 2. 我们写的、没人动过 → 直接覆盖，不备份
        let r = save_with_policy(adapter, &home, &data, &patch(adapter, &home, "m2")).unwrap();
        assert!(r.backup_name.is_empty(), "指纹吻合时不应备份");
        assert!(r.backed_up.is_empty());
        assert_eq!(r.snapshot.files[0].managed, ManagedState::Intact);

        // 3. 其他软件改过（标识仍在，但内容变了）→ 必须备份
        let text = std::fs::read_to_string(&settings).unwrap();
        let edited = text.replace("\"model\": \"m2\"", "\"model\": \"someone-else\"");
        assert_ne!(text, edited);
        std::fs::write(&settings, &edited).unwrap();
        let snap = adapter.snapshot(&home).unwrap();
        assert_eq!(snap.files[0].managed, ManagedState::Modified);
        let r = save_with_policy(adapter, &home, &data, &patch(adapter, &home, "m3")).unwrap();
        assert!(!r.backup_name.is_empty(), "被外部改过必须备份");
        assert_eq!(r.snapshot.files[0].managed, ManagedState::Intact);

        // 备份里应能找到那份被外部改过的内容
        let backups = backup::list_backups(&data, "claude");
        assert_eq!(backups.len(), 2, "两次需要备份的保存各留一份");

        let _ = std::fs::remove_dir_all(&root);
    }
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
