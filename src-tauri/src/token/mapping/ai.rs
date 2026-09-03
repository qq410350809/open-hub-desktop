use super::types::*;
use crate::model::gateway::types::ModelProxyContext;
use serde_json::Value as JsonValue;

/// 单批送给 AI 的待分析条目数。批太大容易触发上游截断与漏条。
pub const BATCH_SIZE: usize = 40;
/// 候选池注入上限：全量 2500+ 条会把提示词撑爆，按厂商前缀预筛后再截断。
const CANDIDATE_LIMIT: usize = 260;

/// 按待分析条目的词元与候选正式名做粗筛，缩小注入 AI 的候选池。
pub fn shortlist_candidates(
    batch: &[PendingModel],
    catalog: &[(String, String, String)],
) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    for item in batch {
        for piece in item
            .rule_base
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|piece| piece.len() >= 3)
        {
            let lowered = piece.to_lowercase();
            if !tokens.contains(&lowered) {
                tokens.push(lowered);
            }
        }
    }
    let mut hits: Vec<String> = Vec::new();
    for (name, _, lab) in catalog {
        let haystack = format!("{} {}", name.to_lowercase(), lab.to_lowercase());
        if tokens.iter().any(|token| haystack.contains(token)) {
            hits.push(name.clone());
        }
        if hits.len() >= CANDIDATE_LIMIT {
            break;
        }
    }
    hits
}

pub fn build_prompt(
    batch: &[PendingModel],
    candidates: &[String],
    standards: &[(String, String)],
) -> String {
    let listed = batch
        .iter()
        .map(|item| {
            format!(
                "- 原始名：{}\n  规则猜测基名：{}",
                item.raw_model, item.rule_base
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let pool = candidates.join("\n");
    let standards_block = if standards.is_empty() {
        String::new()
    } else {
        let lines = standards
            .iter()
            .map(|(raw, official)| format!("- {} → {}", raw, official))
            .collect::<Vec<_>>()
            .join("\n");
        format!("\n\n已确认的标准映射（同族条目必须沿用同一正式模型，保持结果稳定一致）：\n{lines}")
    };
    let standards_rule = if standards.is_empty() {
        String::new()
    } else {
        "\n         5. 已确认的标准映射中存在与待判定条目同族的原始名时（大小写、\
            版本分隔符、厂商前缀或变体后缀差异），必须输出与之相同的 officialModel；\n"
            .to_string()
    };
    format!(
        "你要把本地统计到的「原始模型名」对应到「正式模型名」。\n\n\
         规则：\n\
         1. officialModel 必须逐字取自下面的候选清单，不得改写、不得自造；\n\
         2. 无法确定时把 officialModel 留空字符串，不要猜测；\n\
         3. 同一模型的大小写差异、版本分隔符差异（5-2 与 5.2）、\
            厂商前缀差异（zai-glm 与 glm）都应归到同一个正式模型；\n\
         4. confidence 用 0 到 1 的小数，reason 用一句中文说明依据。{standards_rule}\n\
         待判定条目：\n{listed}{standards_block}\n\n\
         候选正式模型清单：\n{pool}\n\n\
         只输出 JSON，形如：\n\
         {{\"items\":[{{\"rawModel\":\"...\",\"officialModel\":\"...\",\
         \"lab\":\"...\",\"confidence\":0.9,\"reason\":\"...\"}}]}}"
    )
}

/// 把标准映射的正式名并进候选池：标准名可能未落在 shortlist 命中里，
/// 但既然已确认过，就必须让 AI 能逐字取到。
pub fn merge_standard_candidates(
    candidates: Vec<String>,
    standards: &[(String, String)],
) -> Vec<String> {
    let mut merged = candidates;
    for (_, official) in standards {
        if !merged.iter().any(|name| name == official) {
            merged.push(official.clone());
        }
    }
    merged
}

/// 注入提示词的标准条目上限：确认条目可能上千，按与本批的词元重叠度
/// 相关性排序后截断；排序含 raw_key 次序，保证同一批每次拿到同一子集。
pub const STANDARDS_LIMIT: usize = 200;

pub fn select_standards(
    batch: &[PendingModel],
    standards: &[(String, String)],
) -> Vec<(String, String)> {
    if standards.len() <= STANDARDS_LIMIT {
        return standards.to_vec();
    }
    let tokens: Vec<String> = batch
        .iter()
        .flat_map(|item| {
            item.raw_model
                .split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|piece| piece.len() >= 3)
                .map(|piece| piece.to_lowercase())
        })
        .collect();
    let mut scored: Vec<(usize, &str, &str)> = standards
        .iter()
        .map(|(raw, official)| {
            let lowered = raw.to_lowercase();
            let overlap = tokens
                .iter()
                .filter(|token| lowered.contains(token.as_str()))
                .count();
            (overlap, raw.as_str(), official.as_str())
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
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
    let bytes = content.as_bytes();
    let start = content.find('{')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in content[start..].char_indices() {
        let _ = bytes;
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

/// 经进程内网关入口发一次判定请求：免 Key 免回环端口，
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
            raw_key: raw.to_lowercase(),
            raw_model: raw.to_string(),
            rule_base: base.to_string(),
        }
    }

    #[test]
    fn extract_json_handles_fenced_and_prefixed_output() {
        let fenced = "```json\n{\"items\":[]}\n```";
        assert!(extract_json(fenced).is_some());
        let prefixed = "分析结果如下：{\"items\":[]} 完毕";
        assert!(extract_json(prefixed).is_some());
    }

    #[test]
    fn extract_json_ignores_braces_inside_strings() {
        let tricky = r#"{"items":[{"reason":"含 } 符号","rawModel":"a"}]}"#;
        let value = extract_json(tricky).expect("should parse");
        assert_eq!(parse_items(&value).len(), 1);
    }

    #[test]
    fn parse_items_accepts_bare_array() {
        let value = serde_json::json!([
            { "rawModel": "glm-5.3-flash", "officialModel": "GLM-5.3", "confidence": 0.9 }
        ]);
        let items = parse_items(&value);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].official_model, "GLM-5.3");
    }

    #[test]
    fn parse_items_drops_entries_without_raw_model() {
        let value = serde_json::json!({ "items": [{ "officialModel": "GLM-5.3" }] });
        assert!(parse_items(&value).is_empty());
    }

    #[test]
    fn shortlist_narrows_catalog_by_token_overlap() {
        let catalog = vec![
            ("GLM-5.3".into(), "glm53".into(), "zhipu".into()),
            ("GPT-5.6".into(), "gpt56".into(), "openai".into()),
            ("Llama 4".into(), "llama4".into(), "meta".into()),
        ];
        let batch = vec![pending("zai-glm-5-2", "zai-glm-5.2")];
        let hits = shortlist_candidates(&batch, &catalog);
        assert!(hits.contains(&"GLM-5.3".to_string()));
        assert!(!hits.contains(&"Llama 4".to_string()));
    }

    #[test]
    fn prompt_carries_both_raw_and_rule_base() {
        let batch = vec![pending("GLM-5.3-Flash", "glm-5.3")];
        let prompt = build_prompt(&batch, &["GLM-5.3".to_string()], &[]);
        assert!(prompt.contains("GLM-5.3-Flash"));
        assert!(prompt.contains("glm-5.3"));
    }

    #[test]
    fn prompt_injects_confirmed_standards() {
        let batch = vec![pending("glm-5.3-flash", "glm-5.3")];
        let standards = vec![
            ("zai-glm-5.3".to_string(), "GLM-5.3".to_string()),
            ("gpt-5.6".to_string(), "GPT-5.6".to_string()),
        ];
        let prompt = build_prompt(&batch, &["GLM-5.3".to_string()], &standards);
        assert!(prompt.contains("标准映射"));
        assert!(prompt.contains("zai-glm-5.3 → GLM-5.3"));
        assert!(prompt.contains("必须输出与之相同"));
    }

    #[test]
    fn prompt_without_standards_has_no_standards_block() {
        let batch = vec![pending("mystery-x", "mystery-x")];
        let prompt = build_prompt(&batch, &["GLM-5.3".to_string()], &[]);
        assert!(!prompt.contains("标准映射"));
        assert!(!prompt.contains("必须输出与之相同"));
    }

    #[test]
    fn merge_standard_candidates_appends_missing_names() {
        let merged = merge_standard_candidates(
            vec!["GLM-5.3".to_string()],
            &[
                ("zai-glm-5.3".to_string(), "GLM-5.3".to_string()),
                ("gpt-5.6".to_string(), "GPT-5.6".to_string()),
            ],
        );
        assert_eq!(merged, vec!["GLM-5.3".to_string(), "GPT-5.6".to_string()]);
    }

    #[test]
    fn select_standards_passes_through_small_sets() {
        let standards = vec![("a".to_string(), "A".to_string())];
        assert_eq!(select_standards(&[], &standards), standards);
    }

    #[test]
    fn select_standards_prefers_token_overlap_and_is_deterministic() {
        let standards: Vec<(String, String)> = (0..(STANDARDS_LIMIT + 50))
            .map(|index| (format!("filler-{index:04}"), format!("F-{index:04}")))
            .chain(vec![
                ("glm-5.3-flash".to_string(), "GLM-5.3".to_string()),
                ("zai-glm-5.3".to_string(), "GLM-5.3".to_string()),
            ])
            .collect();
        let batch = vec![pending("glm-5.3-pro", "glm-5.3")];
        let selected = select_standards(&batch, &standards);
        assert_eq!(selected.len(), STANDARDS_LIMIT);
        assert!(selected.contains(&("glm-5.3-flash".to_string(), "GLM-5.3".to_string())));
        assert!(selected.contains(&("zai-glm-5.3".to_string(), "GLM-5.3".to_string())));
        // 排序含 raw_key 次序：重跑同一批得到同一子集。
        assert_eq!(selected, select_standards(&batch, &standards));
    }
}
