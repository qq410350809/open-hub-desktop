use super::ai;
use super::store;
use super::types::*;
use crate::context::{AppContext, Managed};
use crate::model::gateway::types::{ChannelConfig, ModelProxyState};
use std::sync::Arc;
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
    ctx: Managed<'_, Arc<AppContext>>,
) -> Result<Vec<ModelMapping>, String> {
    store::list_mappings(&ctx.database)
}

#[tauri::command]
pub fn register_token_model_names(
    ctx: Managed<'_, Arc<AppContext>>,
    names: Vec<String>,
) -> Result<usize, String> {
    store::register_raw_models(&ctx.database, &names)
}

#[tauri::command]
pub fn set_token_model_mapping(
    ctx: Managed<'_, Arc<AppContext>>,
    raw_model: String,
    official_model: String,
) -> Result<ModelMapping, String> {
    store::set_mapping_manually(&ctx.database, &raw_model, &official_model)
}

/// 用 AI 补全「原始模型名 → 正式模型」映射。
/// force = false（默认）时跳过已确认的条目，只分析新增或未决的；
/// force = true 时重跑全部条目，但手工修改（origin = manual）的行始终保留。
/// 已确认映射会作为「标准」注入提示词：同族原始名必须沿用标准正式名，
/// 保证前后多次分析结果一致；本 run 前几批的新结论也会成为后续批的标准。
/// 请求经进程内网关入口发出（免 Key、免回环端口、免网关开关），channel_id
/// 定向所选反代渠道：模型名带 {alias}/{model} 前缀走渠道前缀路由。
#[tauri::command]
pub async fn analyze_token_model_mappings(
    ctx: Managed<'_, Arc<AppContext>>,
    gateway: Managed<'_, ModelProxyState>,
    model: Option<String>,
    force: Option<bool>,
    channel_id: Option<String>,
) -> Result<AnalyzeReport, String> {
    let force = force.unwrap_or(false);
    let mut report = AnalyzeReport::default();

    let confirmed_before = store::count_confirmed(&ctx.database)?;
    let pending = store::pending_models(&ctx.database, force)?;
    if !force {
        report.skipped_confirmed = confirmed_before;
    }
    if pending.is_empty() {
        return Ok(report);
    }

    let catalog = {
        let connection = ctx.database.lock_conn()?;
        store::official_catalog(&connection)?
    };
    if catalog.is_empty() {
        return Err("模型目录为空，请先同步模型目录再做 AI 分析".to_string());
    }

    let gateway_ctx = gateway.context.clone();
    let model = model
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "请选择发起 AI 分析的分析模型".to_string())?;
    // 渠道校验与实际出网用同一份网关运行时配置，避免两处配置漂移。
    let request_model = {
        let gateway_config = gateway_ctx.config.read().await;
        resolve_request_model(&gateway_config.channels, channel_id.as_deref(), &model)?
    };

    for batch in pending.chunks(ai::BATCH_SIZE) {
        // 已确认映射作为标准答案注入：同族原始名必须沿用标准里的正式名，
        // 避免前后两批分析对同类模型给出不一致的映射结果。
        // 排除本批条目（force 重判时旧结论不再当标准），本 run 前几批的新结论
        // 则自然进入后续批次的标准池，让单次运行内部也保持一致。
        let batch_keys: Vec<String> = batch.iter().map(|item| item.raw_key.clone()).collect();
        let standards = {
            let connection = ctx.database.lock_conn()?;
            let all = store::confirmed_standards(&connection, &batch_keys)?;
            ai::select_standards(batch, &all)
        };
        report.standards_used = report.standards_used.max(standards.len());

        let mut candidates = ai::shortlist_candidates(batch, &catalog);
        if candidates.is_empty() && standards.is_empty() {
            report
                .unresolved
                .extend(batch.iter().map(|item| item.raw_model.clone()));
            continue;
        }
        candidates = ai::merge_standard_candidates(candidates, &standards);
        let prompt = ai::build_prompt(batch, &candidates, &standards);
        let items = match ai::request_mapping(&gateway_ctx, &request_model, &prompt).await {
            Ok(items) => items,
            Err(error) => {
                warn!("[token-mapping] 批次分析失败：{error}");
                report.warnings.push(error);
                continue;
            }
        };
        report.analyzed += batch.len();
        report.resolved += store::apply_ai_results(&ctx.database, &items, force)?;

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
        let resolved =
            resolve_request_model(&channels, Some("c1"), "gpt-5.6").expect("should resolve");
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
