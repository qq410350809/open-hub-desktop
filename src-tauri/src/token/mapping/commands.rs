use super::ai;
use super::store;
use super::types::*;
use crate::context::{AppContext, Managed};
use crate::model::gateway::types::{ChannelConfig, ModelProxyState};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::warn;

/// Token 统计专用的标准模型（独立于模型目录）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenOfficialModel {
    pub id: String,
    pub name: String,
    pub lab: String,
    pub aliases: Vec<String>,
    pub source: String,
    pub confidence: f64,
    pub created_at: String,
    pub updated_at: String,
}

/// 组装 AI 请求用的模型名。必须显式指定启用渠道，避免意外从默认路由出网。
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

/// 手工选择直接代表人工批准；传空串会清除当前映射并恢复为待识别。
#[tauri::command]
pub fn set_token_model_mapping(
    ctx: Managed<'_, Arc<AppContext>>,
    raw_model: String,
    official_model: String,
) -> Result<ModelMapping, String> {
    store::set_mapping_manually(&ctx.database, &raw_model, &official_model)
}

#[tauri::command]
pub fn approve_token_model_mapping(
    ctx: Managed<'_, Arc<AppContext>>,
    raw_model: String,
) -> Result<ModelMapping, String> {
    store::approve_mapping(&ctx.database, &raw_model)
}

#[tauri::command]
pub fn reject_token_model_mapping(
    ctx: Managed<'_, Arc<AppContext>>,
    raw_model: String,
) -> Result<ModelMapping, String> {
    store::reject_mapping(&ctx.database, &raw_model)
}

#[tauri::command]
pub fn reopen_token_model_mapping(
    ctx: Managed<'_, Arc<AppContext>>,
    raw_model: String,
) -> Result<ModelMapping, String> {
    store::reopen_mapping(&ctx.database, &raw_model)
}

/// 获取 Token 统计的标准模型清单。
#[tauri::command]
pub fn get_token_official_models(
    ctx: Managed<'_, Arc<AppContext>>,
) -> Result<Vec<TokenOfficialModel>, String> {
    let connection = ctx.database.lock_conn()?;
    let mut statement = connection
        .prepare(
            "SELECT id, name, lab, aliases, source, confidence, created_at, updated_at
             FROM token_official_models
             ORDER BY confidence DESC, lab, name",
        )
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], |row| {
            let aliases_json: String = row.get(3)?;
            Ok(TokenOfficialModel {
                id: row.get(0)?,
                name: row.get(1)?,
                lab: row.get(2)?,
                aliases: serde_json::from_str(&aliases_json).unwrap_or_default(),
                source: row.get(4)?,
                confidence: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

/// 添加用户显式维护的正式模型。
#[tauri::command]
pub fn add_token_official_model(
    ctx: Managed<'_, Arc<AppContext>>,
    id: String,
    name: String,
    lab: String,
) -> Result<TokenOfficialModel, String> {
    let connection = ctx.database.lock_conn()?;
    let id_trimmed = id.trim().to_lowercase().replace(' ', "-");
    let name_trimmed = name.trim();
    let lab_trimmed = lab.trim();
    if id_trimmed.is_empty() || name_trimmed.is_empty() {
        return Err("模型 ID 和名称不能为空".to_string());
    }
    connection
        .execute(
            "INSERT INTO token_official_models (id, name, lab, source, confidence)
             VALUES (?1, ?2, ?3, 'user', 0.5)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name, lab = excluded.lab,
                updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')",
            rusqlite::params![id_trimmed, name_trimmed, lab_trimmed],
        )
        .map_err(|e| e.to_string())?;
    connection
        .query_row(
            "SELECT id, name, lab, aliases, source, confidence, created_at, updated_at
             FROM token_official_models WHERE id = ?1",
            rusqlite::params![id_trimmed],
            |row| {
                let aliases_json: String = row.get(3)?;
                Ok(TokenOfficialModel {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    lab: row.get(2)?,
                    aliases: serde_json::from_str(&aliases_json).unwrap_or_default(),
                    source: row.get(4)?,
                    confidence: row.get(5)?,
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                })
            },
        )
        .map_err(|e| e.to_string())
}

/// 只允许删除用户手动创建的正式模型。AI 不会再自动创建目录项。
#[tauri::command]
pub fn remove_token_official_model(
    ctx: Managed<'_, Arc<AppContext>>,
    id: String,
) -> Result<(), String> {
    let connection = ctx.database.lock_conn()?;
    let deleted = connection
        .execute(
            "DELETE FROM token_official_models WHERE id = ?1 AND source = 'user'",
            rusqlite::params![id],
        )
        .map_err(|e| e.to_string())?;
    if deleted == 0 {
        return Err("无法删除：模型不存在或不属于用户手动添加项".to_string());
    }
    Ok(())
}

/// 从模型目录迁移数据到 token_official_models（一次性操作）。
#[tauri::command]
pub fn migrate_token_official_models(
    ctx: Managed<'_, Arc<AppContext>>,
) -> Result<usize, String> {
    store::migrate_catalog_to_official_models(&ctx.database)
}

fn emit_mapping_progress(
    ctx: &AppContext,
    stage: &str,
    processed: usize,
    total: usize,
    message: impl Into<String>,
) {
    ctx.event_bus.emit(
        "token-mapping-analysis-progress",
        MappingAnalyzeProgress {
            stage: stage.to_string(),
            processed,
            total,
            message: message.into(),
        },
    );
}

/// 用 AI 生成原始模型名到正式模型的审核建议。
///
/// AI 建议永远不会自动影响统计：仅人工批准的映射会进入聚合查表，也只有批准项会被
/// 用作后续请求的标准答案。`force` 仅重跑未批准条目，手工和已批准映射都不会被覆盖。
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
    let approved_before = store::count_approved(&ctx.database)?;
    let pending = store::pending_models(&ctx.database, force)?;
    if !force {
        report.skipped_confirmed = approved_before;
    }
    let total = pending.len();
    emit_mapping_progress(&ctx, "prepare", 0, total, "正在准备待识别模型");
    if pending.is_empty() {
        emit_mapping_progress(&ctx, "complete", 0, 0, "没有需要识别的模型");
        return Ok(report);
    }

    let catalog = {
        let connection = ctx.database.lock_conn()?;
        store::official_catalog(&connection)?
    };
    if catalog.is_empty() {
        return Err("正式模型目录为空，请先同步模型目录后再执行 AI 辅助识别".to_string());
    }

    let gateway_ctx = gateway.context.clone();
    let model = model
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "请选择发起 AI 辅助识别的分析模型".to_string())?;
    let request_model = {
        let gateway_config = gateway_ctx.config.read().await;
        resolve_request_model(&gateway_config.channels, channel_id.as_deref(), &model)?
    };

    let mut processed = 0usize;
    for batch in pending.chunks(ai::BATCH_SIZE) {
        let batch_keys: Vec<String> = batch.iter().map(|item| item.raw_key.clone()).collect();
        let standards = {
            let connection = ctx.database.lock_conn()?;
            let all = store::approved_standards(&connection, &batch_keys)?;
            ai::select_standards(batch, &all)
        };
        report.standards_used = report.standards_used.max(standards.len());
        let candidates_by_key = ai::build_candidates_by_key(batch, &catalog, &standards);
        let eligible = batch
            .iter()
            .filter(|item| candidates_by_key.get(&item.raw_key).is_some_and(|items| !items.is_empty()))
            .count();
        if eligible == 0 {
            report.analyzed += batch.len();
            report.unresolved.extend(batch.iter().map(|item| item.raw_model.clone()));
            processed += batch.len();
            emit_mapping_progress(
                &ctx,
                "batch",
                processed,
                total,
                format!("第 {} 批没有可信候选，已保留为待处理", (processed + ai::BATCH_SIZE - 1) / ai::BATCH_SIZE),
            );
            continue;
        }

        emit_mapping_progress(
            &ctx,
            "request",
            processed,
            total,
            format!("正在分析第 {} 批（{} 个模型）", processed / ai::BATCH_SIZE + 1, batch.len()),
        );
        let prompt = ai::build_prompt(batch, &candidates_by_key, &standards);
        let items = match ai::request_mapping(&gateway_ctx, &request_model, &prompt).await {
            Ok(items) => items,
            Err(error) => {
                warn!("[token-mapping] 批次分析失败：{error}");
                report.warnings.push(error);
                processed += batch.len();
                emit_mapping_progress(&ctx, "batch-error", processed, total, "本批请求失败，可稍后重试");
                continue;
            }
        };
        report.analyzed += batch.len();
        let applied = store::apply_ai_suggestions(&ctx.database, batch, &candidates_by_key, &items)?;
        report.resolved += applied.suggested;
        report.rejected_invalid += applied.invalid;
        for item in batch {
            if !applied.accepted_keys.contains(&item.raw_key) {
                report.unresolved.push(item.raw_model.clone());
            }
        }
        processed += batch.len();
        emit_mapping_progress(
            &ctx,
            "batch",
            processed,
            total,
            format!("已完成 {processed}/{total} 个模型，{} 条建议等待审核", report.resolved),
        );
    }

    emit_mapping_progress(
        &ctx,
        "complete",
        total,
        total,
        format!("识别完成：{} 条建议等待人工审核", report.resolved),
    );
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
    fn request_model_requires_explicit_enabled_channel() {
        let channels = vec![channel("c1", "x666", true)];
        assert!(resolve_request_model(&channels, None, "gpt-5.6").is_err());
        assert_eq!(
            resolve_request_model(&channels, Some("c1"), "gpt-5.6").unwrap(),
            "x666/gpt-5.6"
        );
        assert!(resolve_request_model(&[channel("c1", "x666", false)], Some("c1"), "gpt-5.6").is_err());
    }
}
