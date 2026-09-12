//! 本地工具配置管理的类型定义。
//!
//! 快照模型：后端把每个工具的配置文件解析为结构化
//! 「供应商 / 模型 / 默认项 / 上下文 / 思考级别」五块，
//! 前端编辑整体提交，后端只把管辖字段合并回原文件，
//! 其余字段（MCP、权限、快捷键等）原样保留。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 工具标识，与 token 采集 source 命名保持一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolId {
    Claude,
    Codex,
    Opencode,
    Zcode,
    Antigravity,
    Dsh,
}

impl ToolId {
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolId::Claude => "claude",
            ToolId::Codex => "codex",
            ToolId::Opencode => "opencode",
            ToolId::Zcode => "zcode",
            ToolId::Antigravity => "antigravity",
            ToolId::Dsh => "dsh",
        }
    }

    /// 允许从字符串反查（HTTP/IPC 层入参）。
    pub fn from_str_value(value: &str) -> Option<Self> {
        Some(match value {
            "claude" => ToolId::Claude,
            "codex" => ToolId::Codex,
            "opencode" => ToolId::Opencode,
            "zcode" => ToolId::Zcode,
            "antigravity" => ToolId::Antigravity,
            "dsh" => ToolId::Dsh,
            _ => return None,
        })
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            ToolId::Claude => "Claude Code",
            ToolId::Codex => "Codex CLI",
            ToolId::Opencode => "OpenCode",
            ToolId::Zcode => "ZCode",
            ToolId::Antigravity => "Google Antigravity",
            ToolId::Dsh => "DeepSeek CLI (DSH)",
        }
    }

    /// 该工具如何使用供应商：一路接入 / 一次切一家 / 一次加载全部。
    pub fn provider_mode(&self) -> ProviderMode {
        match self {
            ToolId::Claude | ToolId::Antigravity => ProviderMode::Single,
            ToolId::Codex => ProviderMode::Switch,
            ToolId::Opencode | ToolId::Zcode | ToolId::Dsh => ProviderMode::All,
        }
    }
}

/// 工具对供应商列表的使用方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderMode {
    /// 配置里只有一路接入，不能新增或删除供应商。
    #[default]
    Single,
    /// 配置里可存多家，运行时一次只用当前选中的那家。
    Switch,
    /// 配置里存多家，运行时一次加载全部。
    All,
}

impl ProviderMode {
    #[cfg(test)]
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderMode::Single => "single",
            ProviderMode::Switch => "switch",
            ProviderMode::All => "all",
        }
    }
}

/// 供应商条目（跨工具统一抽象）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ProviderEntry {
    /// 供应商标识（工具配置里的 key，如 codex 的 provider id）。
    pub id: String,
    /// 显示名。
    pub name: String,
    /// API 基础地址。
    pub base_url: String,
    /// API Key（明文，工具本身即明文存储；UI 掩码展示）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_key: String,
    /// 协议类型（openai / anthropic / responses / gemini…，按工具语义）。
    pub protocol: String,
    /// 该供应商下的模型 ID 列表（展示用，编辑走 models 分区）。
    pub models: Vec<String>,
}

/// 模型条目（跨工具统一抽象）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ModelEntry {
    pub id: String,
    /// 显示名（工具无此概念时与 id 相同）。
    pub name: String,
    /// 所属供应商标识。
    pub provider: String,
    /// 上下文窗口 token 数；0 表示工具不支持该字段。
    pub context_window: u64,
    /// 最大输出 token 数；0 表示不支持。
    pub max_output: u64,
}

/// 默认模型与思考级别。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct DefaultsSection {
    /// 默认模型 ID（按工具语义可能是 model / model_provider+model 组合）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// 默认供应商（工具支持显式指定时才有值）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
    /// 思考级别（minimal/low/medium/high/max…，按工具限定）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reasoning_effort: String,
    /// 该工具支持的思考级别档位（下拉选项）。
    pub reasoning_effort_options: Vec<String>,
    /// 按模型的思考级别映射（如 opencode variants）。
    pub per_model_effort: BTreeMap<String, String>,
}

/// 上下文相关设置。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ContextSection {
    /// 上下文窗口 token 数；None 表示工具无此字段。
    pub context_window: Option<u64>,
    /// 自动压缩阈值 token 数。
    pub auto_compact_token_limit: Option<u64>,
    /// 单次最大输出 token 数。
    pub max_output_tokens: Option<u64>,
    /// 思考 token 预算（claude MAX_THINKING_TOKENS）。
    pub max_thinking_tokens: Option<u64>,
}

/// 思考级别独立设置（工具把 effort 放在顶层时使用，如 claude effortLevel）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ThinkingSection {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub effort_level: String,
    pub effort_level_options: Vec<String>,
    pub max_thinking_tokens: Option<u64>,
}

/// 单个工具配置文件涉及的文件清单。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ToolConfigFile {
    /// 类别：config / auth / env
    pub kind: String,
    pub label: String,
    pub path: String,
    pub exists: bool,
    /// 相对本软件上次写入的状态：unmanaged（无标识）/ intact（指纹吻合）/ modified（被外部改过）。
    pub managed: super::mark::ManagedState,
}

/// 工具配置结构化快照。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ToolConfigSnapshot {
    pub tool: String,
    pub tool_name: String,
    /// 配置文件清单（含存在性）。
    pub files: Vec<ToolConfigFile>,
    /// 供应商列表。
    pub providers: Vec<ProviderEntry>,
    /// 模型列表（含所属供应商）。
    pub models: Vec<ModelEntry>,
    pub defaults: DefaultsSection,
    pub context: ContextSection,
    pub thinking: ThinkingSection,
    /// 当前配置内容整体哈希（用于保存冲突检测）。
    pub content_hash: String,
    /// 写入后生效方式说明（如「重启 Codex CLI 会话后生效」）。
    pub effect_note: String,
    /// 快照解析失败时的说明（配置损坏等），为空表示正常。
    pub warning: String,
    /// 该工具如何使用供应商列表。
    pub provider_mode: ProviderMode,
}

/// 工具概览（列表页条目）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ToolOverview {
    pub tool: String,
    pub tool_name: String,
    /// 是否检测到安装（根目录或关键文件存在）。
    pub detected: bool,
    /// 是否有 token 采集记录（列表默认只展示这类工具）。
    pub has_token_records: bool,
    pub collected_sessions: usize,
    pub collected_events: usize,
    /// 关键配置/数据根目录。
    pub root: String,
    /// 摘要：供应商数 / 默认模型。
    pub provider_count: usize,
    pub default_model: String,
    /// 生效方式说明。
    pub effect_note: String,
    /// 该工具如何使用供应商列表。
    pub provider_mode: ProviderMode,
}

/// 工具列表报告。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ToolListReport {
    pub available: bool,
    pub home: String,
    pub tools: Vec<ToolOverview>,
    /// 采集缓存的最近更新时间（ISO），空表示尚无记录。
    pub collected_at: String,
}

/// 保存请求：前端提交编辑后的完整结构化分区。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ToolConfigPatch {
    /// 保存时校验的内容哈希；与磁盘不符说明文件被外部修改。
    pub base_hash: String,
    pub providers: Vec<ProviderEntry>,
    pub models: Vec<ModelEntry>,
    pub defaults: DefaultsSection,
    pub context: ContextSection,
    pub thinking: ThinkingSection,
}

/// 保存结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ToolConfigSaveResult {
    pub snapshot: ToolConfigSnapshot,
    /// 本次写入创建的备份文件名；全部文件指纹吻合（直接覆盖）时为空。
    pub backup_name: String,
    /// 本次被备份的文件 label（无标识或被外部改过的文件）。
    pub backed_up: Vec<String>,
}

/// 备份条目。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ToolBackupEntry {
    /// 备份文件名（含时间戳）。
    pub name: String,
    /// 备份对应的原始配置文件相对路径。
    pub file_label: String,
    pub size: u64,
    pub created_at: String,
}

/// 保存冲突错误：磁盘内容与 base_hash 不符。
pub(crate) fn conflict_error(tool: &str) -> String {
    format!("{tool} 的配置文件在保存期间被外部程序修改过，为避免覆盖已中止写入；请重新加载后再试")
}

#[cfg(test)]
mod tests {
    use super::{ProviderMode, ToolId};

    #[test]
    fn provider_mode_matches_tool_capability() {
        assert_eq!(ToolId::Claude.provider_mode(), ProviderMode::Single);
        assert_eq!(ToolId::Antigravity.provider_mode(), ProviderMode::Single);
        assert_eq!(ToolId::Codex.provider_mode(), ProviderMode::Switch);
        assert_eq!(ToolId::Opencode.provider_mode(), ProviderMode::All);
        assert_eq!(ToolId::Zcode.provider_mode(), ProviderMode::All);
        assert_eq!(ToolId::Dsh.provider_mode(), ProviderMode::All);
        assert_eq!(ProviderMode::Single.as_str(), "single");
        assert_eq!(ProviderMode::Switch.as_str(), "switch");
        assert_eq!(ProviderMode::All.as_str(), "all");
    }
}
