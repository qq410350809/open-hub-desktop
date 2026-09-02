use super::ai;
use super::store;
use super::types::*;
use crate::model::gateway::types::ChannelConfig;
use crate::models::Database;
use tauri::State;
use tracing::warn;

/// 组装 AI 判定请求用的模型名。要求显式指定渠道：拼上渠道网关别名前缀
/// （{alias}/{model}），由网关的渠道前缀路由定向到该渠道。
pub(crate) fn resolve_request_model(
    channels: &[ChannelConfig],
    channel_id: Option<&str>,
    model: &str,
) -> Result<String, String> {
    let id = channel_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "请先选择发起 AI 分析的反代渠道".to_string())?;
    let channel = channels
        .iter()
        .find(|channel| channel.id == id)
        .ok_or_else(|| "所选反代渠道不存在或已被删除".to_string())?;
    if !channel.enabled {
        return Err("所选反代渠道未启用，请先在模型代理页开启".to_string());
    }
    Ok(format!("{}/{}", channel.effective_alias(), model))
}

#[tauri::command]
pub fn get_token_model_mappings(
    database: State<'_, Database>,
) -> Result<Vec<ModelMapping>, String> {
    store::list_mappings(&database)
}

#[tauri::command]
pub fn register_token_model_names(
    database: State<'_, Database>,
    names: Vec<String>,
) -> Result<usize, String> {
    store::register_raw_models(&database, &names)
}

#[tauri::command]
pub fn set_token_model_mapping(
    database: State<'_, Database>,
    raw_model: String,
    official_model: String,
) -> Result<ModelMapping, String> {
    store::set_mapping_manually(&database, &raw_model, &official_model)
}

/// 用 AI 补全「原始模型名 → 正式模型」映射。
/// force = false（默认）时跳过已确认的条目，只分析新增或未决的；
/// force = true 时重跑全部条目，但手工修改（origin = manual）的行始终保留。
/// channel_id 非空时，请求模型名带上该渠道的网关别名前缀（{alias}/{model}），
/// 由网关的渠道前缀路由定向到所选反代渠道。
#[tauri::command]
pub async fn analyze_token_model_mappings(
    database: State<'_, Database>,
    model: Option<String>,
    force: Option<bool>,
    channel_id: Option<String>,
) -> Result<AnalyzeReport, String> {
    let force = force.unwrap_or(false);
    let mut report = AnalyzeReport::default();

    let confirmed_before = store::count_confirmed(&database)?;
    let pending = store::pending_models(&database, force)?;
    if !force {
        report.skipped_confirmed = confirmed_before;
    }
    if pending.is_empty() {
        return Ok(report);
    }

    let catalog = {
        let connection = database.lock_conn()?;
        store::official_catalog(&connection)?
    };
    if catalog.is_empty() {
        return Err("模型目录为空，请先同步模型目录再做 AI 分析".to_string());
    }

    let config = {
        let connection = database.lock_conn()?;
        crate::model::gateway::config::load_model_proxy_config(&connection)
    };
    if !config.enabled {
        return Err("模型网关未启用，无法调用 AI 分析".to_string());
    }
    let base_url = format!("http://127.0.0.1:{}", config.port);
    let model = model.unwrap_or_else(|| "gpt-5.6".to_string());
    let request_model = resolve_request_model(&config.channels, channel_id.as_deref(), &model)?;

    for batch in pending.chunks(ai::BATCH_SIZE) {
        let candidates = ai::shortlist_candidates(batch, &catalog);
        if candidates.is_empty() {
            report
                .unresolved
                .extend(batch.iter().map(|item| item.raw_model.clone()));
            continue;
        }
        let prompt = ai::build_prompt(batch, &candidates);
        let items = match ai::request_mapping(
            &base_url,
            &config.api_key,
            &request_model,
            &prompt,
            config.timeout_seconds,
        )
        .await
        {
            Ok(items) => items,
            Err(error) => {
                warn!("[token-mapping] 批次分析失败：{error}");
                report.warnings.push(error);
                continue;
            }
        };
        report.analyzed += batch.len();
        report.resolved += store::apply_ai_results(&database, &items, force)?;

        for item in batch {
            let hit = items.iter().any(|result| {
                super::normalize::raw_key(&result.raw_model) == item.raw_key
                    && !result.official_model.trim().is_empty()
            });
            if !hit {
                report.unresolved.push(item.raw_model.clone());
            }
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(id: &str, alias: &str, enabled: bool) -> ChannelConfig {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "name": id,
            "enabled": enabled,
            "upstreamUrl": "https://example.com",
            "alias": alias,
        }))
        .expect("channel json should deserialize")
    }

    #[test]
    fn request_model_requires_explicit_channel() {
        let channels = vec![channel("c1", "x666", true)];
        let error = resolve_request_model(&channels, None, "gpt-5.6")
            .expect_err("missing channel should fail");
        assert!(error.contains("反代渠道"));
        assert!(resolve_request_model(&channels, Some(""), "gpt-5.6").is_err());
    }

    #[test]
    fn request_model_prefixes_selected_channel_alias() {
        let channels = vec![channel("c1", "x666", true)];
        let resolved = resolve_request_model(&channels, Some("c1"), "gpt-5.6")
            .expect("should resolve");
        assert_eq!(resolved, "x666/gpt-5.6");
    }

    #[test]
    fn request_model_rejects_missing_and_disabled_channels() {
        let channels = vec![channel("c1", "x666", false)];
        assert!(resolve_request_model(&channels, Some("nope"), "gpt-5.6").is_err());
        let error = resolve_request_model(&channels, Some("c1"), "gpt-5.6")
            .expect_err("disabled channel should fail");
        assert!(error.contains("未启用"));
    }
}
