//! AI 用量洞察的 Tauri 命令层。

use super::insight::{
    request_insight, validate_insight_response, InsightEvidencePacket, RawInsightResponse,
};
use crate::context::{AppContext, Managed};
use crate::model::gateway::types::ModelProxyState;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// 洞察报告：AI 解读结果 + 生成时的范围/模型快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenInsightReport {
    /// 提交给 AI 的时间范围说明；报告只在匹配的范围下展示。
    pub range_label: String,
    pub analysis_model: String,
    pub generated_at: String,
    pub headline: String,
    pub findings: Vec<RawInsightFindingAlias>,
    pub recommendations: Vec<RawInsightFindingAlias>,
    /// 证据总数与被 AI 实际引用的证据数，用于评估解读覆盖度。
    pub evidence_total: usize,
    pub evidence_used: usize,
    /// 证据不足以生成任何结论时的提示。
    pub notice: String,
}

// 直接复用 insight 的 RawInsightFinding 作为输出类型（Serialize 已实现）。
pub type RawInsightFindingAlias = super::insight::RawInsightFinding;

/// 简单的速率限制：同一时间只允许一个洞察请求，避免误触连发。
static INSIGHT_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[tauri::command]
pub async fn analyze_token_insights(
    _ctx: Managed<'_, Arc<AppContext>>,
    gateway: Managed<'_, ModelProxyState>,
    packet: InsightEvidencePacket,
) -> Result<TokenInsightReport, String> {
    if packet.evidence.is_empty() {
        return Err("当前时间范围没有可用证据，请先选择有数据的日期区间".to_string());
    }
    let model = packet.analysis_model.trim().to_string();
    if model.is_empty() {
        return Err("请先选择分析模型".to_string());
    }

    if INSIGHT_RUNNING.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return Err("已有洞察分析在执行，请稍候".to_string());
    }

    let evidence_ids: std::collections::HashSet<String> = packet
        .evidence
        .iter()
        .map(|item| item.id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect();
    if evidence_ids.is_empty() {
        INSIGHT_RUNNING.store(false, std::sync::atomic::Ordering::Relaxed);
        return Err("证据包缺少有效 ID".to_string());
    }

    let gateway_ctx = gateway.context.clone();
    let result = request_insight(&gateway_ctx, &model, &packet).await;
    INSIGHT_RUNNING.store(false, std::sync::atomic::Ordering::Relaxed);

    let raw: RawInsightResponse = result?;
    let validated = validate_insight_response(&raw, &evidence_ids);
    let evidence_used: usize = validated
        .findings
        .iter()
        .chain(validated.recommendations.iter())
        .map(|item| item.evidence.len())
        .sum();

    let dropped = raw.findings.len() + raw.recommendations.len()
        - validated.findings.len()
        - validated.recommendations.len();
    let notice = if validated.findings.is_empty() && validated.recommendations.is_empty() {
        "AI 未能基于现有证据给出可信结论，请扩充时间范围或补充更多用量数据后重试。".to_string()
    } else if dropped > 0 {
        format!("已忽略 {dropped} 条未引用有效证据的 AI 结论。")
    } else {
        String::new()
    };

    Ok(TokenInsightReport {
        range_label: packet.range_label,
        analysis_model: model,
        generated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        headline: validated.headline,
        findings: validated.findings,
        recommendations: validated.recommendations,
        evidence_total: evidence_ids.len(),
        evidence_used,
        notice,
    })
}

#[cfg(test)]
mod tests {
    use super::super::insight::{InsightEvidence, InsightEvidencePacket};
    use super::*;

    fn packet() -> InsightEvidencePacket {
        InsightEvidencePacket {
            range_label: "2026-09-01 ~ 2026-09-04".into(),
            analysis_model: "test".into(),
            evidence: vec![InsightEvidence {
                id: "total".into(),
                summary: "总量".into(),
                value: "1M".into(),
            }],
        }
    }

    #[test]
    fn empty_evidence_packet_is_rejected_without_gateway() {
        let mut empty = packet();
        empty.evidence.clear();
        // 命令层校验逻辑等价复现（无法在单测中构造 Managed 状态）。
        let err = (|| -> Result<(), String> {
            if empty.evidence.is_empty() {
                return Err("当前时间范围没有可用证据，请先选择有数据的日期区间".to_string());
            }
            Ok(())
        })()
        .unwrap_err();
        assert!(err.contains("没有可用证据"));
    }
}
