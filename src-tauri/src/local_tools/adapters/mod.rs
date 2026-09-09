//! 工具适配器：把各工具异构的配置文件解析为统一快照，
//! 并把编辑后的分区合并回原文件（只动管辖字段）。
//!
//! 约定：
//! - `snapshot` 实时读盘，文件缺失时返回「检测到但未初始化」的空快照；
//! - `apply` 前由 commands 层完成备份与 content_hash 冲突校验；
//! - 合并只触碰本模块管辖的字段，工具其余配置原样保留。

#[cfg(test)]
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::types::{
    ContextSection, DefaultsSection, ModelEntry, ProviderEntry, ThinkingSection, ToolId,
    ToolConfigFile, ToolConfigPatch, ToolConfigSnapshot,
};

/// 配置内容整体哈希（冲突检测用）。FNV-1a 64 位，足够检测误覆盖。
pub(crate) fn content_hash(parts: &[String]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in part.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// 适配器统一接口。`home` 为用户主目录（测试注入临时目录）。
/// Send + Sync：适配器经 `&'static dyn` 传入 spawn_blocking 闭包。
pub(crate) trait ToolAdapter: Send + Sync {
    fn id(&self) -> ToolId;

    /// 关键配置文件列表（kind/label/绝对路径）。
    fn config_files(&self, home: &Path) -> Vec<(String, String, PathBuf)>;

    /// 解析当前配置为结构化快照。
    fn snapshot(&self, home: &Path) -> Result<ToolConfigSnapshot, String>;

    /// 把编辑后的分区合并写回。
    /// 返回被写入的文件 label 列表（供备份记录）。
    fn apply(&self, home: &Path, patch: &ToolConfigPatch) -> Result<Vec<String>, String>;

    /// 检测安装状态与根目录。
    fn detect(&self, home: &Path) -> (bool, String);

    /// 生效方式说明。
    fn effect_note(&self) -> &'static str;
}

/// 适配器公共帮助：构造快照骨架。
pub(crate) fn snapshot_skeleton(
    adapter: &dyn ToolAdapter,
    home: &Path,
) -> ToolConfigSnapshot {
    let files = adapter
        .config_files(home)
        .into_iter()
        .map(|(kind, label, path)| {
            let exists = path.is_file();
            ToolConfigFile {
                kind,
                label,
                path: path.display().to_string(),
                exists,
            }
        })
        .collect();
    ToolConfigSnapshot {
        tool: adapter.id().as_str().to_string(),
        tool_name: adapter.id().display_name().to_string(),
        files,
        effect_note: adapter.effect_note().to_string(),
        provider_mode: adapter.id().provider_mode(),
        ..Default::default()
    }
}

// —— 各格式公共小工具 ——

/// 从 JSON 对象里取字符串。
pub(crate) fn json_str(value: &serde_json::Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

/// 从 JSON 对象里取 u64。
pub(crate) fn json_u64(value: &serde_json::Value, key: &str) -> Option<u64> {
    value.get(key).and_then(|v| v.as_u64())
}

/// 规范化思考级别选项集合（去空、去重、保序）。
pub(crate) fn effort_options(values: &[&str]) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    values
        .iter()
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
        .filter(|v| seen.insert(v.clone()))
        .collect()
}

/// BTreeMap 快捷构造（测试辅助）。
#[cfg(test)]
pub(crate) fn map_from(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[cfg(test)]
pub(crate) fn status_only_snapshot(
    adapter: &dyn ToolAdapter,
    home: &Path,
    warning: &str,
) -> ToolConfigSnapshot {
    let mut snap = snapshot_skeleton(adapter, home);
    snap.warning = warning.to_string();
    snap
}

#[allow(dead_code)]
pub(crate) fn defaults_empty() -> DefaultsSection {
    DefaultsSection::default()
}

#[allow(dead_code)]
pub(crate) fn context_empty() -> ContextSection {
    ContextSection::default()
}

#[allow(dead_code)]
pub(crate) fn thinking_empty() -> ThinkingSection {
    ThinkingSection::default()
}

#[allow(dead_code)]
pub(crate) fn provider(id: &str, name: &str, base_url: &str) -> ProviderEntry {
    ProviderEntry {
        id: id.to_string(),
        name: name.to_string(),
        base_url: base_url.to_string(),
        ..Default::default()
    }
}

#[allow(dead_code)]
pub(crate) fn model(id: &str, provider: &str) -> ModelEntry {
    ModelEntry {
        id: id.to_string(),
        name: id.to_string(),
        provider: provider.to_string(),
        ..Default::default()
    }
}

// —— 适配器注册表 ——

/// 适配器保留，但不进本地工具列表：Antigravity 只有一路 Gemini 接入，无法管理多家第三方站点。
#[allow(dead_code)]
pub(crate) mod antigravity;
pub(crate) mod claude;
pub(crate) mod codex;
pub(crate) mod commandcode;
pub(crate) mod dsh;
pub(crate) mod opencode;
pub(crate) mod zcode;

/// 全部 A 级适配器（B 级工具不在此表，list 时静态生成）。
pub(crate) fn full_adapters() -> Vec<&'static dyn ToolAdapter> {
    vec![
        &claude::ClaudeAdapter,
        &codex::CodexAdapter,
        &opencode::OpencodeAdapter,
        &zcode::ZcodeAdapter,
        &dsh::DshAdapter,
        &commandcode::CommandCodeAdapter,
    ]
}

pub(crate) fn find_adapter(tool: ToolId) -> Option<&'static dyn ToolAdapter> {
    full_adapters().into_iter().find(|a| a.id() == tool)
}
