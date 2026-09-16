use super::ai;
use super::store;
use super::types::*;
use crate::context::{AppContext, Managed};
use crate::model::gateway::types::{ChannelConfig, ModelProxyState};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
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
    /// 是否为「原厂模型」：模型目录中存在该模型的官方渠道，且该渠道是
    /// **已核实的原厂供应商**（`model_catalog_providers.is_first_party = 1`，
    /// 与模型目录页「原厂自营」同一份口径），按注册表 `id`（目录 slug）或
    /// `name` 匹配。
    ///
    /// 用户手工添加（`source = 'user'`）恒为 `true`。目录未同步或结构过旧时
    /// 一律 `false`，绝不臆造。
    pub first_party: bool,
}

/// 表是否存在指定列。用于兼容尚未按权威 DDL 重建的旧库。
fn has_column(
    connection: &rusqlite::Connection,
    table: &str,
    column: &str,
) -> Result<bool, String> {
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
            rusqlite::params![table, column],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(count > 0)
}

/// 已核实的原厂（自营）渠道 id 集合。
///
/// 判据：`model_catalog_providers.is_first_party = 1`——即原厂别名表
/// （[`lab_registry::LAB_OFFICIAL_HOSTS`]）登记过的自营渠道，与模型目录页
/// 「原厂自营」供应商矩阵同一份口径。
///
/// ⚠️ 刻意**不**用 `tier = 'lab'` 兜底：tier 只描述渠道自身性质，未登记的
/// 自营云（如 `sarvam`）也会是 `tier = lab`，但它们不在已核实的原厂名单里；
/// 放进来就会重新出现「超出原厂范围」。
///
/// providers 表缺失或仍是旧结构时返回空集。
fn vetted_official_providers(connection: &rusqlite::Connection) -> Result<HashSet<String>, String> {
    if !has_column(connection, "model_catalog_providers", "is_first_party")? {
        return Ok(HashSet::new());
    }
    let mut statement = connection
        .prepare("SELECT id FROM model_catalog_providers WHERE is_first_party = 1")
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|id| id.trim().to_lowercase())
        .filter(|id| !id.is_empty())
        .collect())
}

/// 目录中「确为原厂模型」的 slug / name 小写集合。
///
/// 需要 `model_catalog_models.official_host_count` 与
/// `official_channel_providers_json` 两列；缺失（目录尚未同步/结构过旧）时返回空集。
fn first_party_keys(connection: &rusqlite::Connection) -> Result<HashSet<String>, String> {
    if !has_column(connection, "model_catalog_models", "official_host_count")?
        || !has_column(connection, "model_catalog_models", "official_channel_providers_json")?
    {
        return Ok(HashSet::new());
    }
    let vetted = vetted_official_providers(connection)?;
    let mut statement = connection
        .prepare(
            "SELECT slug, name, official_channel_providers_json
             FROM model_catalog_models WHERE official_host_count > 0",
        )
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let mut keys = HashSet::new();
    for (slug, name, channels_json) in rows {
        let channels: Vec<String> = serde_json::from_str(&channels_json).unwrap_or_default();
        // 供应商性质无法判定时（providers 表缺失）退回「有官方渠道即算」，
        // 宁可多给也不把候选清空。
        let is_first_party = vetted.is_empty()
            || channels
                .iter()
                .any(|channel| vetted.contains(&channel.trim().to_lowercase()));
        if !is_first_party {
            continue;
        }
        for key in [slug, name] {
            let key = key.trim().to_lowercase();
            if !key.is_empty() {
                keys.insert(key);
            }
        }
    }
    Ok(keys)
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
    let first_party = first_party_keys(&connection)?;
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
            let id: String = row.get(0)?;
            let name: String = row.get(1)?;
            let source: String = row.get(4)?;
            // 用户显式添加的自定义模型始终可选；目录匹配只对自动导入的条目生效。
            let first_party = source == "user"
                || first_party.contains(&id.trim().to_lowercase())
                || first_party.contains(&name.trim().to_lowercase());
            Ok(TokenOfficialModel {
                first_party,
                id,
                name,
                lab: row.get(2)?,
                aliases: serde_json::from_str(&aliases_json).unwrap_or_default(),
                source,
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
                    // 用户显式添加的正式模型始终视为可选目标，无需等待目录同步。
                    first_party: true,
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
pub fn migrate_token_official_models(ctx: Managed<'_, Arc<AppContext>>) -> Result<usize, String> {
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
            .filter(|item| {
                candidates_by_key
                    .get(&item.raw_key)
                    .is_some_and(|items| !items.is_empty())
            })
            .count();
        if eligible == 0 {
            report.analyzed += batch.len();
            report
                .unresolved
                .extend(batch.iter().map(|item| item.raw_model.clone()));
            processed += batch.len();
            emit_mapping_progress(
                &ctx,
                "batch",
                processed,
                total,
                format!(
                    "第 {} 批没有可信候选，已保留为待处理",
                    (processed + ai::BATCH_SIZE - 1) / ai::BATCH_SIZE
                ),
            );
            continue;
        }

        emit_mapping_progress(
            &ctx,
            "request",
            processed,
            total,
            format!(
                "正在分析第 {} 批（{} 个模型）",
                processed / ai::BATCH_SIZE + 1,
                batch.len()
            ),
        );
        let prompt = ai::build_prompt(batch, &candidates_by_key, &standards);
        let items = match ai::request_mapping(&gateway_ctx, &request_model, &prompt).await {
            Ok(items) => items,
            Err(error) => {
                warn!("[token-mapping] 批次分析失败：{error}");
                report.warnings.push(error);
                processed += batch.len();
                emit_mapping_progress(
                    &ctx,
                    "batch-error",
                    processed,
                    total,
                    "本批请求失败，可稍后重试",
                );
                continue;
            }
        };
        report.analyzed += batch.len();
        let applied =
            store::apply_ai_suggestions(&ctx.database, batch, &candidates_by_key, &items)?;
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
            format!(
                "已完成 {processed}/{total} 个模型，{} 条建议等待审核",
                report.resolved
            ),
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
        assert!(
            resolve_request_model(&[channel("c1", "x666", false)], Some("c1"), "gpt-5.6").is_err()
        );
    }

    fn catalog_connection() -> rusqlite::Connection {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE model_catalog_models (
                    id TEXT PRIMARY KEY,
                    slug TEXT NOT NULL DEFAULT '',
                    name TEXT NOT NULL DEFAULT '',
                    official_host_count INTEGER NOT NULL DEFAULT 0,
                    official_channel_providers_json TEXT NOT NULL DEFAULT '[]'
                );
                CREATE TABLE model_catalog_providers (
                    id TEXT PRIMARY KEY,
                    tier TEXT,
                    is_first_party INTEGER NOT NULL DEFAULT 0
                );",
            )
            .unwrap();
        connection
    }

    fn insert_catalog_row(
        connection: &rusqlite::Connection,
        id: &str,
        slug: &str,
        name: &str,
        official_channels: &[&str],
    ) {
        connection
            .execute(
                "INSERT INTO model_catalog_models
                    (id, slug, name, official_host_count, official_channel_providers_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    id,
                    slug,
                    name,
                    official_channels.len() as i64,
                    serde_json::to_string(official_channels).unwrap()
                ],
            )
            .unwrap();
    }

    fn insert_provider(
        connection: &rusqlite::Connection,
        id: &str,
        tier: &str,
        is_first_party: bool,
    ) {
        connection
            .execute(
                "INSERT INTO model_catalog_providers (id, tier, is_first_party)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![id, tier, is_first_party as i64],
            )
            .unwrap();
    }

    /// 只有「目录中确有原厂渠道」的条目才进候选集，且 slug / name 大小写不敏感。
    #[test]
    fn first_party_keys_keep_only_models_with_official_channels() {
        let connection = catalog_connection();
        insert_provider(&connection, "zhipuai", "lab", true);
        insert_provider(&connection, "openai", "lab", true);
        insert_catalog_row(&connection, "zhipuai/glm-5.2", "glm52", "GLM-5.2", &["zhipuai", "zai"]);
        insert_catalog_row(&connection, "openai/gpt-oss-120b", "gptoss120b", "GPT OSS 120B", &[]);

        let keys = first_party_keys(&connection).unwrap();
        assert!(keys.contains("glm52"), "slug 应命中");
        assert!(keys.contains("glm-5.2"), "name 应按小写写入");
        assert!(
            !keys.contains("gptoss120b"),
            "无原厂渠道的模型不得进入候选集"
        );
        assert!(!keys.contains("gpt oss 120b"));
    }

    /// 聚合路由方把自己的路由名登记为「官方渠道」，不得算原厂模型。
    #[test]
    fn first_party_keys_exclude_gateway_self_declared_official() {
        let connection = catalog_connection();
        insert_provider(&connection, "trustedrouter", "gateway", false);
        insert_provider(&connection, "deepseek", "lab", true);
        // 路由方自封「官方渠道」
        insert_catalog_row(&connection, "trustedrouter/auto", "auto", "Auto", &["trustedrouter"]);
        // 同一供应商同时是原厂与三方
        insert_catalog_row(
            &connection,
            "deepseek/deepseek-v4-pro",
            "deepseekv4pro",
            "DeepSeek V4 Pro",
            &["deepseek", "nano-gpt"],
        );

        let keys = first_party_keys(&connection).unwrap();
        assert!(
            !keys.contains("auto"),
            "聚合路由方自封的官方渠道不得算原厂"
        );
        assert!(keys.contains("deepseekv4pro"), "真原厂渠道应保留");
        assert!(keys.contains("deepseek v4 pro"));
    }

    /// 未登记进原厂别名表的「tier=lab」自营云不是已核实的原厂供应商——
    /// 判定必须严格按 `is_first_party` 数据，不能拿 tier 兜底。
    #[test]
    fn first_party_keys_require_vetted_first_party_provider() {
        let connection = catalog_connection();
        // sarvam-like：tier=lab 但不在原厂别名表
        insert_provider(&connection, "sarvam", "lab", false);
        insert_provider(&connection, "anthropic", "lab", true);
        insert_catalog_row(&connection, "sarvam/sarvam-105b", "sarvam105b", "Sarvam 105B", &["sarvam"]);
        insert_catalog_row(&connection, "anthropic/claude-opus-5", "claudeopus5", "Claude Opus 5", &["anthropic"]);

        let keys = first_party_keys(&connection).unwrap();
        assert!(
            !keys.contains("sarvam105b"),
            "未登记的自营云不得凭 tier=lab 混入原厂候选"
        );
        assert!(keys.contains("claudeopus5"), "已核实原厂应保留");
    }

    /// 目录表仍是旧结构（无官方渠道列）时不得报错，也不得臆造原厂。
    #[test]
    fn first_party_keys_tolerate_legacy_catalog_schema() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE model_catalog_models (
                    id TEXT PRIMARY KEY,
                    slug TEXT NOT NULL DEFAULT '',
                    name TEXT NOT NULL DEFAULT ''
                );",
            )
            .unwrap();
        assert!(first_party_keys(&connection).unwrap().is_empty());

        // 表还不存在时同样返回空集，而不是 Err。
        let empty = rusqlite::Connection::open_in_memory().unwrap();
        assert!(first_party_keys(&empty).unwrap().is_empty());
    }
}
