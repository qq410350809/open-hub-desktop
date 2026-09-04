use super::types::*;
use crate::model::gateway::types::ModelProxyContext;
use serde_json::Value as JsonValue;
use std::collections::{HashMap, HashSet};

/// 单批送给 AI 的待分析条目数。每条都有独立候选，避免无关模型互相干扰。
pub const BATCH_SIZE: usize = 20;
const CANDIDATE_LIMIT_PER_ITEM: usize = 16;
/// 注入提示词的已批准标准映射上限。
pub const STANDARDS_LIMIT: usize = 80;

fn token_set(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for piece in value
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|piece| piece.len() >= 2)
    {
        let normalized = piece.to_ascii_lowercase();
        if !tokens.contains(&normalized) {
            tokens.push(normalized);
        }
    }
    tokens
}

fn compact(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn candidate_score(item: &PendingModel, candidate: &OfficialModelCandidate) -> i32 {
    let item_compact = compact(&item.rule_base);
    let raw_compact = compact(&item.raw_model);
    let fields = std::iter::once(candidate.name.as_str())
        .chain(std::iter::once(candidate.id.as_str()))
        .chain(candidate.aliases.iter().map(String::as_str))
        .collect::<Vec<_>>();

    if fields.iter().any(|field| compact(field) == raw_compact || compact(field) == item_compact) {
        return 10_000;
    }

    let input_tokens = token_set(&format!("{} {}", item.raw_model, item.rule_base));
    let mut score = 0;
    for field in fields {
        let haystack = field.to_ascii_lowercase();
        for token in &input_tokens {
            if haystack.contains(token) {
                score += if token.len() >= 4 { 20 } else { 8 };
            }
        }
    }
    if candidate.lab.to_ascii_lowercase().contains(&input_tokens.first().cloned().unwrap_or_default()) {
        score += 2;
    }
    score
}

/// 为每个原始模型生成独立候选集。候选名称、ID 与 aliases 都参与匹配，
/// 已批准标准的目标模型会强制补入对应条目的候选集。
pub fn build_candidates_by_key(
    batch: &[PendingModel],
    catalog: &[OfficialModelCandidate],
    standards: &[(String, String)],
) -> HashMap<String, Vec<OfficialModelCandidate>> {
    let approved_names: HashSet<&str> = standards.iter().map(|(_, name)| name.as_str()).collect();
    let mut output = HashMap::new();
    for item in batch {
        let mut scored = catalog
            .iter()
            .map(|candidate| (candidate_score(item, candidate), candidate))
            .filter(|(score, _)| *score > 0)
            .collect::<Vec<_>>();
        scored.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.name.cmp(&right.1.name))
        });

        let mut candidates = Vec::new();
        for (_, candidate) in scored.into_iter().take(CANDIDATE_LIMIT_PER_ITEM) {
            candidates.push(candidate.clone());
        }
        for candidate in catalog.iter().filter(|candidate| approved_names.contains(candidate.name.as_str())) {
            if !candidates.iter().any(|current| current.id == candidate.id) {
                candidates.push(candidate.clone());
            }
        }
        output.insert(item.raw_key.clone(), candidates);
    }
    output
}

pub fn build_prompt(
    batch: &[PendingModel],
    candidates_by_key: &HashMap<String, Vec<OfficialModelCandidate>>,
    standards: &[(String, String)],
) -> String {
    let listed = batch
        .iter()
        .map(|item| {
            let candidates = candidates_by_key
                .get(&item.raw_key)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let choices = if candidates.is_empty() {
                "（无候选，请返回空 officialModel）".to_string()
            } else {
                candidates
                    .iter()
                    .map(|candidate| {
                        let aliases = if candidate.aliases.is_empty() {
                            String::new()
                        } else {
                            format!("；别名：{}", candidate.aliases.join(", "))
                        };
                        format!("{}（ID：{}；厂商：{}{}）", candidate.name, candidate.id, candidate.lab, aliases)
                    })
                    .collect::<Vec<_>>()
                    .join("\n    ")
            };
            format!(
                "- rawModel：{}\n  规则基名：{}\n  此条允许选择的 officialModel：\n    {}",
                item.raw_model, item.rule_base, choices
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let standards_block = if standards.is_empty() {
        String::new()
    } else {
        let lines = standards
            .iter()
            .map(|(raw, official)| format!("- {} → {}", raw, official))
            .collect::<Vec<_>>()
            .join("\n");
        format!("\n\n已人工批准的标准映射（仅作同族一致性参考）：\n{lines}")
    };
    format!(
        "你在执行本地 Token 统计的模型名称识别。输出仅作为人工审核建议，不能替代人工确认。\n\
         规则：\n\
         1. 每条的 officialModel 只能逐字选自该条自身的“允许选择”清单；绝不能创造、改写或猜测清单外名称；\n\
         2. 没有可靠候选时 officialModel 必须是空字符串；\n\
         3. rawModel 必须逐字复用待判定条目的原始名，每条至多输出一次；\n\
         4. confidence 是 0 到 1 的有限小数；reason 用一句中文解释名称、版本、别名或厂商的匹配依据；\n\
         5. 不要把不同模型因为名称相似而强行合并。\n\
         待判定条目：\n{listed}{standards_block}\n\n\
         只输出 JSON，形如：\n\
         {{\"items\":[{{\"rawModel\":\"...\",\"officialModel\":\"...\",\"confidence\":0.9,\"reason\":\"...\"}}]}}"
    )
}

/// 注入提示词的已批准标准映射按与当前批次的词元重叠度裁剪，保证结果稳定。
pub fn select_standards(
    batch: &[PendingModel],
    standards: &[(String, String)],
) -> Vec<(String, String)> {
    if standards.len() <= STANDARDS_LIMIT {
        return standards.to_vec();
    }
    let tokens: Vec<String> = batch
        .iter()
        .flat_map(|item| token_set(&format!("{} {}", item.raw_model, item.rule_base)))
        .collect();
    let mut scored: Vec<(usize, &str, &str)> = standards
        .iter()
        .map(|(raw, official)| {
            let lowered = raw.to_ascii_lowercase();
            let overlap = tokens
                .iter()
                .filter(|token| lowered.contains(token.as_str()))
                .count();
            (overlap, raw.as_str(), official.as_str())
        })
        .collect();
    scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(right.1)));
    scored
        .into_iter()
        .take(STANDARDS_LIMIT)
        .map(|(_, raw, official)| (raw.to_string(), official.to_string()))
        .collect()
}

/// 从模型回复里抽出 JSON 对象：容忍 ```json 包裹与前后解释文字。
pub fn extract_json(content: &str) -> Option<JsonValue> {
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

pub fn parse_items(value: &JsonValue) -> Vec<AiMappingItem> {
    let array = value
        .get("items")
        .and_then(JsonValue::as_array)
        .or_else(|| value.as_array());
    let Some(array) = array else {
        return Vec::new();
    };
    array
        .iter()
        .filter_map(|item| serde_json::from_value::<AiMappingItem>(item.clone()).ok())
        .filter(|item| !item.raw_model.trim().is_empty())
        .collect()
}

/// 经进程内网关入口发一次判定请求：免 Key、免回环端口，
/// 渠道解析、协议转换与请求日志与普通网关请求一致。
pub async fn request_mapping(
    ctx: &ModelProxyContext,
    model: &str,
    prompt: &str,
) -> Result<Vec<AiMappingItem>, String> {
    let payload =
        crate::model::gateway::handlers::chat::internal_chat_completion(ctx, model, prompt).await?;
    let content = payload
        .pointer("/choices/0/message/content")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "AI 响应缺少 message.content".to_string())?;
    let parsed = extract_json(content).ok_or_else(|| "AI 响应不是可解析的 JSON".to_string())?;
    Ok(parse_items(&parsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(raw: &str, base: &str) -> PendingModel {
        PendingModel {
            raw_key: raw.split('/').next_back().unwrap_or(raw).to_ascii_lowercase(),
            raw_model: raw.to_string(),
            rule_base: base.to_string(),
        }
    }

    fn candidate(id: &str, name: &str, aliases: &[&str]) -> OfficialModelCandidate {
        OfficialModelCandidate {
            id: id.to_string(),
            name: name.to_string(),
            lab: "test".to_string(),
            aliases: aliases.iter().map(|alias| (*alias).to_string()).collect(),
        }
    }

    #[test]
    fn extract_json_handles_fenced_and_prefixed_output() {
        assert!(extract_json("```json\n{\"items\":[]}\n```").is_some());
        assert!(extract_json("结果：{\"items\":[]} 完毕").is_some());
    }

    #[test]
    fn candidates_are_scoped_per_raw_model_and_include_aliases() {
        let batch = vec![
            pending("zai/glm-5.3", "glm-5.3"),
            pending("claude-code", "claude-code"),
        ];
        let catalog = vec![
            candidate("glm-53", "GLM-5.3", &["zai-glm-5.3"]),
            candidate("claude-sonnet-4", "Claude Sonnet 4", &["claude-code"]),
            candidate("llama-4", "Llama 4", &[]),
        ];
        let by_key = build_candidates_by_key(&batch, &catalog, &[]);
        let glm = &by_key["glm-5.3"];
        let claude = &by_key["claude-code"];
        assert!(glm.iter().any(|item| item.name == "GLM-5.3"));
        assert!(claude.iter().any(|item| item.name == "Claude Sonnet 4"));
        assert!(!glm.iter().any(|item| item.name == "Claude Sonnet 4"));
    }

    #[test]
    fn prompt_forbids_inventing_names_and_has_per_item_candidates() {
        let batch = vec![pending("glm-5.3", "glm-5.3")];
        let mut candidates = HashMap::new();
        candidates.insert(
            "glm-5.3".to_string(),
            vec![candidate("glm-53", "GLM-5.3", &[])],
        );
        let prompt = build_prompt(&batch, &candidates, &[]);
        assert!(prompt.contains("绝不能创造"));
        assert!(prompt.contains("GLM-5.3（ID：glm-53"));
    }

    #[test]
    fn select_standards_prefers_overlap_and_is_deterministic() {
        let standards: Vec<(String, String)> = (0..(STANDARDS_LIMIT + 8))
            .map(|index| (format!("filler-{index:03}"), format!("F-{index:03}")))
            .chain(vec![("glm-5.3-flash".to_string(), "GLM-5.3".to_string())])
            .collect();
        let batch = vec![pending("glm-5.3-pro", "glm-5.3")];
        let selected = select_standards(&batch, &standards);
        assert_eq!(selected.len(), STANDARDS_LIMIT);
        assert!(selected.contains(&("glm-5.3-flash".to_string(), "GLM-5.3".to_string())));
        assert_eq!(selected, select_standards(&batch, &standards));
    }
}
