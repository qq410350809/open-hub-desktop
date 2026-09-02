use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// 客观题自动判分规则。
/// - contains / not_contains：value 为逗号分隔关键词（大小写不敏感，contains 任一命中即通过）
/// - number：value 为期望数值（容忍 tolerance 绝对误差），从回答中提取数字比对
/// - json：回答须为可解析 JSON（容忍 ```json 包裹）；value 非空时还须包含该子串
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CheckSpec {
    pub kind: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub tolerance: f64,
}

/// 一条测试提示词（内置套件与自定义提示词同构，由前端随运行参数下发）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbePrompt {
    pub id: String,
    pub name: String,
    pub category: String,
    pub text: String,
    #[serde(default = "default_prompt_max_tokens")]
    pub max_tokens: u32,
    #[serde(default)]
    pub temperature: f64,
    #[serde(default)]
    pub check: Option<CheckSpec>,
    /// 开放题：由评审模型打分（0-10）
    #[serde(default)]
    pub judge: bool,
}

fn default_prompt_max_tokens() -> u32 {
    1024
}

/// 被测目标：反代渠道 + 模型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct ProbeTarget {
    pub channel_id: String,
    pub model: String,
}

/// 评审模型：同样来自反代渠道。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JudgeSpec {
    pub channel_id: String,
    pub model: String,
}

/// 一次测试运行的参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunParams {
    pub targets: Vec<ProbeTarget>,
    pub prompts: Vec<ProbePrompt>,
    #[serde(default = "default_concurrency")]
    pub concurrency: u32,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub judge: Option<JudgeSpec>,
}

fn default_concurrency() -> u32 {
    4
}

fn default_timeout_seconds() -> u64 {
    120
}

/// 客观题判分结果。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AutoCheckOutcome {
    pub kind: String,
    pub passed: bool,
    pub detail: String,
}

/// 评审模型打分结果。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct JudgeOutcome {
    pub score: Option<f64>,
    pub reason: String,
}

/// 单条测试结果。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub channel_id: String,
    pub channel_name: String,
    pub model: String,
    pub prompt_id: String,
    pub prompt_name: String,
    pub category: String,
    pub ok: bool,
    pub duration_ms: Option<u64>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub tokens_per_sec: Option<f64>,
    #[serde(default)]
    pub auto_check: Option<AutoCheckOutcome>,
    /// 0-10：客观题 10/0，评审题为评审模型打分；无评审配置时为空
    pub score: Option<f64>,
    #[serde(default)]
    pub judge: Option<JudgeOutcome>,
    pub error: Option<String>,
    pub response_text: Option<String>,
}

/// 测试进度事件载荷（EventBus → Tauri 窗口 + SSE）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunProgress {
    pub run_id: i64,
    /// running | finished | cancelled | error
    pub phase: String,
    pub completed: u32,
    pub total: u32,
    #[serde(default)]
    pub result: Option<ProbeResult>,
}

/// 历史运行记录（列表页用，config/summary 为 JSON 原文）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestRunRecord {
    pub id: i64,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub target_count: i64,
    pub prompt_count: i64,
    pub config: serde_json::Value,
    pub summary: Option<serde_json::Value>,
}

/// 单个目标的汇总（对比矩阵首列 + 排序依据）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModelSummary {
    pub channel_id: String,
    pub channel_name: String,
    pub model: String,
    pub total: u32,
    pub ok_count: u32,
    pub avg_score: Option<f64>,
    pub avg_duration_ms: Option<u64>,
    pub avg_tokens_per_sec: Option<f64>,
}

/// 运行结束后的整次汇总。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    pub models: Vec<ModelSummary>,
    pub prompts: Vec<PromptSummary>,
}

/// 单题维度汇总（横向对比哪些题拉开了差距）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PromptSummary {
    pub prompt_id: String,
    pub prompt_name: String,
    pub category: String,
    pub total: u32,
    pub ok_count: u32,
    pub avg_score: Option<f64>,
    pub avg_duration_ms: Option<u64>,
}

/// run_model_test 命令的返回：后台运行句柄。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStartInfo {
    pub run_id: i64,
    pub total: u32,
}

/// 运行中的全局状态：防重入 + 取消。
pub struct ProbeRuntime {
    pub running: AtomicBool,
    pub active_run_id: Mutex<Option<i64>>,
    pub active_cancellation: Mutex<Option<CancellationToken>>,
}

impl ProbeRuntime {
    pub fn new() -> Self {
        Self {
            running: AtomicBool::new(false),
            active_run_id: Mutex::new(None),
            active_cancellation: Mutex::new(None),
        }
    }
}

impl Default for ProbeRuntime {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) const CUSTOM_PROMPTS_META_KEY: &str = "model_test_custom_prompts";
pub(crate) const LAST_CONFIG_META_KEY: &str = "model_test_last_config";
