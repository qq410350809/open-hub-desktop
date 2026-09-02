use serde::{Deserialize, Serialize};

/// 映射来源：规则推导 / AI 分析 / 手工修改。
pub const ORIGIN_RULE: &str = "rule";
pub const ORIGIN_AI: &str = "ai";
pub const ORIGIN_MANUAL: &str = "manual";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelMapping {
    /// 归一后的主键（小写、去前缀）
    pub raw_key: String,
    /// 首次见到的原始模型名，用于界面展示
    pub raw_model: String,
    /// 正式模型名；空串表示尚未确定
    pub official_model: String,
    #[serde(default)]
    pub official_slug: Option<String>,
    #[serde(default)]
    pub lab: Option<String>,
    pub origin: String,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub reason: Option<String>,
    pub confirmed: bool,
    #[serde(default)]
    pub updated_at: String,
}

/// 待分析条目：原始名 + 规则层猜测的基名，一起交给 AI 提高命中率。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingModel {
    pub raw_key: String,
    pub raw_model: String,
    pub rule_base: String,
}

/// AI 返回的单条判定结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiMappingItem {
    pub raw_model: String,
    #[serde(default)]
    pub official_model: String,
    #[serde(default)]
    pub lab: Option<String>,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeReport {
    /// 本次实际送给 AI 的条目数
    pub analyzed: usize,
    /// 因已确认而跳过的条目数
    pub skipped_confirmed: usize,
    /// 成功写回映射的条目数
    pub resolved: usize,
    /// AI 未能给出正式模型的原始名
    #[serde(default)]
    pub unresolved: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}
