use super::types::*;
use serde_json::{json, Value as JsonValue};
use std::time::Duration;

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

pub fn build_prompt(batch: &[PendingModel], candidates: &[String]) -> String {
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
    format!(
        "你要把本地统计到的「原始模型名」对应到「正式模型名」。\n\n\
         规则：\n\
         1. officialModel 必须逐字取自下面的候选清单，不得改写、不得自造；\n\
         2. 无法确定时把 officialModel 留空字符串，不要猜测；\n\
         3. 同一模型的大小写差异、版本分隔符差异（5-2 与 5.2）、\
            厂商前缀差异（zai-glm 与 glm）都应归到同一个正式模型；\n\
         4. confidence 用 0 到 1 的小数，reason 用一句中文说明依据。\n\n\
         待判定条目：\n{listed}\n\n\
         候选正式模型清单：\n{pool}\n\n\
         只输出 JSON，形如：\n\
         {{\"items\":[{{\"rawModel\":\"...\",\"officialModel\":\"...\",\
         \"lab\":\"...\",\"confidence\":0.9,\"reason\":\"...\"}}]}}"
    )
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

/// 通过本地模型网关发一次判定请求，复用用户已配置的渠道与 key。
pub async fn request_mapping(
    base_url: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    timeout_seconds: u64,
) -> Result<Vec<AiMappingItem>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_seconds.max(30)))
        .build()
        .map_err(|error| error.to_string())?;
    let endpoint = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));
    let mut request = client.post(&endpoint).json(&json!({
        "model": model,
        "temperature": 0,
        "messages": [{ "role": "user", "content": prompt }],
    }));
    if !api_key.trim().is_empty() {
        request = request.bearer_auth(api_key.trim());
    }
    let response = request.send().await.map_err(|error| error.to_string())?;
    let status = response.status();
    let body = response.text().await.map_err(|error| error.to_string())?;
    if !status.is_success() {
        return Err(format!("AI 分析请求失败（{status}）：{body}"));
    }
    let payload: JsonValue = serde_json::from_str(&body).map_err(|error| error.to_string())?;
    let content = payload["choices"][0]["message"]["content"]
        .as_str()
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
        let prompt = build_prompt(&batch, &["GLM-5.3".to_string()]);
        assert!(prompt.contains("GLM-5.3-Flash"));
        assert!(prompt.contains("glm-5.3"));
    }
}
