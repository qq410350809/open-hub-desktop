use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// 客观题自动判分规则。
/// - contains / not_contains：value 为逗号分隔关键词（大小写不敏感，contains 任一命中即通过）
/// - number：value 为期望数值（容忍 tolerance 绝对误差），从回答中提取数字比对
/// - json：回答须为可解析 JSON（容忍 ```json 包裹）；value 非空时还须包含该子串
/// - exact：去除全部空白并小写化后与 value 完全一致
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CheckSpec {
    pub kind: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub tolerance: f64,
}

/// 指纹题的家族期望答案：patterns 为大小写不敏感子串，回答命中任一即视为该家族。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FamilyExpectation {
    pub family: String,
    pub patterns: Vec<String>,
}

/// 一道内置探测题（验真目录由后端 fingerprints.rs 维护，前端只按 id 勾选）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectionProbe {
    pub id: String,
    pub name: String,
    /// identity 身份自述 | fingerprint 判别指纹 | capability 降智能力
    pub category: String,
    pub description: String,
    pub text: String,
    /// 同义变体问法（答案不变）：发送时随机选一种，去除同质化
    #[serde(default)]
    pub variants: Vec<String>,
    #[serde(default = "default_probe_max_tokens")]
    pub max_tokens: u32,
    #[serde(default)]
    pub temperature: f64,
    /// 能力题：客观判分规则
    #[serde(default)]
    pub check: Option<CheckSpec>,
    /// 指纹题：各模型家族的期望答案特征
    #[serde(default)]
    pub expected: Vec<FamilyExpectation>,
    /// 一致性采样题：按运行参数的 repeats 重复发送
    #[serde(default)]
    pub repeats: bool,
}

fn default_probe_max_tokens() -> u32 {
    512
}

/// 被测目标：反代渠道 + 模型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct ProbeTarget {
    pub channel_id: String,
    pub model: String,
}

/// 一次验真检测的运行参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunParams {
    pub targets: Vec<ProbeTarget>,
    /// 勾选的探测题 id（须存在于内置目录）
    pub probe_ids: Vec<String>,
    /// 一致性采样次数（作用于 repeats 题），1-5
    #[serde(default = "default_repeats")]
    pub repeats: u32,
    #[serde(default = "default_concurrency")]
    pub concurrency: u32,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
}

fn default_repeats() -> u32 {
    3
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

/// 单条探测结果（目标 × 探测题 × 采样序号）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub channel_id: String,
    pub channel_name: String,
    pub model: String,
    pub probe_id: String,
    pub probe_name: String,
    pub category: String,
    #[serde(default)]
    pub sample_index: u32,
    pub ok: bool,
    pub duration_ms: Option<u64>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub tokens_per_sec: Option<f64>,
    #[serde(default)]
    pub auto_check: Option<AutoCheckOutcome>,
    /// 身份/指纹题命中的模型家族（未命中为 None）
    pub family_match: Option<String>,
    /// 实际发送的最终提问（随机变体 + 对话包装后），供证据回看
    pub request_text: Option<String>,
    pub error: Option<String>,
    pub response_text: Option<String>,
}

/// 检测进度事件载荷（EventBus → Tauri 窗口 + SSE）。
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
    pub probe_count: i64,
    pub repeats: i64,
    pub config: serde_json::Value,
    pub summary: Option<serde_json::Value>,
}

/// 单个目标的验真结论。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TargetVerdict {
    pub channel_id: String,
    pub channel_name: String,
    pub model: String,
    /// ok 可信 | suspicious 可疑 | impersonation 疑似冒名 | unreachable 不可达
    pub verdict: String,
    /// 从模型名推断的标称家族
    pub claimed_family: Option<String>,
    /// 指纹题投票得出的最可能家族
    pub detected_family: Option<String>,
    /// 身份自述题汇总出的家族
    pub identity_family: Option<String>,
    /// 自述身份与标称是否一致（无有效自述时为空）
    pub identity_consistent: Option<bool>,
    pub capability_passed: u32,
    pub capability_total: u32,
    /// 一致性采样：全部采样答案一致的题目占比
    pub consistency_rate: Option<f64>,
    pub total_requests: u32,
    pub ok_count: u32,
    pub avg_duration_ms: Option<u64>,
    pub avg_tokens_per_sec: Option<f64>,
    /// 人类可读的证据/疑点列表
    pub issues: Vec<String>,
    /// 全部探测明细（get_model_test_results 返回时携带）
    #[serde(default)]
    pub results: Vec<ProbeResult>,
}

/// 运行收尾的整次汇总（存 summary_json，历史列表展示结论分布）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    pub targets: Vec<TargetVerdict>,
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
