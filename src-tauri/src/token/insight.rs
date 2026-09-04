//! 范围化 AI 用量洞察。
//!
//! 证据包完全由前端基于当前选中时间范围的用量桶确定性计算得出；
//! AI 只负责解读证据。每个结论必须引用证据 ID，无法对应的结论会被丢弃，
//! 保证报告中的每句话都能追溯回具体数据。

use crate::model::gateway::types::ModelProxyContext;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashSet;

/// 单个证据项。AI 结论的 `evidence` 必须引用这里的 id。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InsightEvidence {
    pub id: String,
    /// 证据的中文说明，会原样进入提示词。
    pub summary: String,
    /// 证据关键数值（如 token 总量、变化百分比），供 AI 复核。
    #[serde(default)]
    pub value: String,
}

/// 前端提交的证据包。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InsightEvidencePacket {
    /// 用户可读的时间范围说明（如 2026-09-01 ~ 2026-09-04）。
    pub range_label: String,
    /// 提交时使用的分析模型（用于结果溯源）。
    pub analysis_model: String,
    pub evidence: Vec<InsightEvidence>,
}

/// AI 返回的单条发现。绑定到证据 ID 才会被采纳。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawInsightFinding {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub severity: String,
    #[serde(default)]
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawInsightResponse {
    #[serde(default)]
    pub headline: String,
    #[serde(default)]
    pub findings: Vec<RawInsightFinding>,
    #[serde(default)]
    pub recommendations: Vec<RawInsightFinding>,
}

/// 校验并裁剪 AI 输出：引用不存在证据的条目会被丢弃。
pub fn validate_insight_response(
    raw: &RawInsightResponse,
    evidence_ids: &HashSet<String>,
) -> RawInsightResponse {
    let sanitize_list = |items: &[RawInsightFinding]| -> Vec<RawInsightFinding> {
        items
            .iter()
            .filter(|item| {
                !item.title.trim().is_empty()
                    && !item.evidence.is_empty()
                    && item
                        .evidence
                        .iter()
                        .any(|id| evidence_ids.contains(id.trim()))
            })
            .map(|item| RawInsightFinding {
                title: item.title.trim().chars().take(120).collect(),
                detail: item.detail.trim().chars().take(500).collect(),
                severity: normalize_severity(&item.severity),
                evidence: item
                    .evidence
                    .iter()
                    .filter(|id| evidence_ids.contains(id.trim()))
                    .map(|id| id.trim().to_string())
                    .collect(),
            })
            .take(12)
            .collect()
    };
    RawInsightResponse {
        headline: raw.headline.trim().chars().take(200).collect(),
        findings: sanitize_list(&raw.findings),
        recommendations: sanitize_list(&raw.recommendations),
    }
}

fn normalize_severity(value: &str) -> String {
    let lowered = value.trim().to_ascii_lowercase();
    match lowered.as_str() {
        "info" | "low" | "medium" | "high" => lowered,
        _ => "info".to_string(),
    }
}

pub fn build_prompt(packet: &InsightEvidencePacket) -> String {
    let evidence = packet
        .evidence
        .iter()
        .map(|item| format!("- [{}] {}（{}）", item.id, item.summary, item.value))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "你在解读 OpenHub 本地 Token 统计（时间范围：{range}）。以下证据由程序从用量数据中确定性计算得出：\n\n\
         {evidence}\n\n\
         规则：\n\
         1. 每条 finding/recommendation 的 evidence 数组必须引用上面真实存在的证据 ID，可多条；\n\
         2. 不允许编造证据之外的数字或趋势；证据不足以支撑结论时直接不输出该条；\n\
         3. severity 只能是 info / low / medium / high；\n\
         4. title 一句话，detail 用中文补充可操作的解释；\n\
         5. recommendations 给出基于证据的改进或排查动作，不重复 findings；\n\
         6. headline 用一句中文概括本期用量最重要的变化。\n\n\
         只输出 JSON，形如：\n\
         {{\"headline\":\"...\",\"findings\":[{{\"title\":\"...\",\"detail\":\"...\",\"severity\":\"info\",\"evidence\":[\"...\"]}}],\
         \"recommendations\":[{{\"title\":\"...\",\"detail\":\"...\",\"severity\":\"info\",\"evidence\":[\"...\"]}}]}}",
        range = packet.range_label,
        evidence = evidence,
    )
}

/// 经进程内网关入口请求一次洞察解读，返回严格校验后的结果。
pub async fn request_insight(
    ctx: &ModelProxyContext,
    model: &str,
    packet: &InsightEvidencePacket,
) -> Result<RawInsightResponse, String> {
    let prompt = build_prompt(packet);
    let payload =
        crate::model::gateway::handlers::chat::internal_chat_completion(ctx, model, &prompt).await?;
    let content = payload
        .pointer("/choices/0/message/content")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "AI 响应缺少 message.content".to_string())?;
    let parsed = extract_json(content).ok_or_else(|| "AI 响应不是可解析的 JSON".to_string())?;
    serde_json::from_value::<RawInsightResponse>(parsed)
        .map_err(|error| format!("AI 洞察响应结构不符合约定：{error}"))
}

/// 与映射模块共享同一 JSON 抽取逻辑，避免循环依赖则保留一份精简实现。
fn extract_json(content: &str) -> Option<JsonValue> {
    if let Ok(value) = serde_json::from_str::<JsonValue>(content.trim()) {
        return Some(value);
    }
    let start = content.find('{')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in content[start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    let slice = &content[start..start + offset + 1];
                    return serde_json::from_str(slice).ok();
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet() -> InsightEvidencePacket {
        InsightEvidencePacket {
            range_label: "2026-09-01 ~ 2026-09-04".to_string(),
            analysis_model: "test-model".to_string(),
            evidence: vec![
                InsightEvidence {
                    id: "total".to_string(),
                    summary: "本期总消耗".to_string(),
                    value: "1.2M tokens".to_string(),
                },
                InsightEvidence {
                    id: "peak".to_string(),
                    summary: "峰值日".to_string(),
                    value: "2026-09-03 600k tokens".to_string(),
                },
            ],
        }
    }

    #[test]
    fn prompt_includes_evidence_ids_and_constraints() {
        let prompt = build_prompt(&packet());
        assert!(prompt.contains("[total]"));
        assert!(prompt.contains("[peak]"));
        assert!(prompt.contains("2026-09-01 ~ 2026-09-04"));
        assert!(prompt.contains("不允许编造"));
    }

    #[test]
    fn validate_drops_findings_without_valid_evidence() {
        let raw = RawInsightResponse {
            headline: "消耗上涨".to_string(),
            findings: vec![
                RawInsightFinding {
                    title: "有效发现".to_string(),
                    detail: "引用了真实证据".to_string(),
                    severity: "high".into(),
                    evidence: vec!["total".to_string()],
                },
                RawInsightFinding {
                    title: "幻觉发现".to_string(),
                    detail: "引用了不存在的证据".to_string(),
                    severity: "info".into(),
                    evidence: vec!["made-up".to_string()],
                },
                RawInsightFinding {
                    title: "无引用".to_string(),
                    detail: "没有证据".to_string(),
                    severity: "info".into(),
                    evidence: vec![],
                },
            ],
            recommendations: vec![],
        };
        let ids: HashSet<String> = ["total".to_string(), "peak".to_string()].into();
        let validated = validate_insight_response(&raw, &ids);
        assert_eq!(validated.findings.len(), 1);
        assert_eq!(validated.findings[0].evidence, vec!["total"]);
        assert_eq!(validated.findings[0].severity, "high");
    }

    #[test]
    fn validate_normalizes_unknown_severity() {
        let raw = RawInsightResponse {
            headline: String::new(),
            findings: vec![RawInsightFinding {
                title: "标题".to_string(),
                detail: String::new(),
                severity: "urgent".into(),
                evidence: vec!["peak".to_string()],
            }],
            recommendations: vec![],
        };
        let ids: HashSet<String> = ["peak".to_string()].into();
        let validated = validate_insight_response(&raw, &ids);
        assert_eq!(validated.findings[0].severity, "info");
    }
}
