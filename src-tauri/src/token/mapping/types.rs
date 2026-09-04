use serde::{Deserialize, Serialize};

/// 映射来源：规则推导 / AI 分析 / 手工修改。
pub const ORIGIN_RULE: &str = "rule";
pub const ORIGIN_AI: &str = "ai";
pub const ORIGIN_MANUAL: &str = "manual";

/// 映射审核状态。`confirmed` 保留为旧客户端兼容字段，且只在已批准时为 true。
pub const REVIEW_PENDING: &str = "pending";
pub const REVIEW_SUGGESTED: &str = "suggested";
pub const REVIEW_APPROVED: &str = "approved";
pub const REVIEW_REJECTED: &str = "rejected";

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
    #[serde(default = "default_review_status")]
    pub review_status: String,
    pub confirmed: bool,
    #[serde(default)]
    pub updated_at: String,
}

fn default_review_status() -> String {
    REVIEW_PENDING.to_string()
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

/// 可供 AI 选择的正式模型。AI 只能返回当前条目的候选名称，不能创建新目录项。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OfficialModelCandidate {
    pub name: String,
    pub id: String,
    pub lab: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MappingAnalyzeProgress {
    pub stage: String,
    pub processed: usize,
    pub total: usize,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeReport {
    /// 本次实际送给 AI 的条目数
    pub analyzed: usize,
    /// 因已确认而跳过的条目数
    pub skipped_confirmed: usize,
    /// AI 生成并通过本地校验、等待人工审核的建议数。
    pub resolved: usize,
    /// 因返回不属于本批、候选不合法或置信度不合法而拒绝的条目数。
    #[serde(default)]
    pub rejected_invalid: usize,
    /// 注入提示词的已确认标准映射条数（批次间取最大值）
    #[serde(default)]
    pub standards_used: usize,
    /// AI 未能给出正式模型的原始名
    #[serde(default)]
    pub unresolved: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}
