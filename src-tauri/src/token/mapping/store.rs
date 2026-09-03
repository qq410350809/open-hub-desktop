use super::normalize::{raw_key, rule_base_name};
use super::types::*;
use crate::models::Database;
use rusqlite::{params, Connection, OptionalExtension};

fn row_mapping(row: &rusqlite::Row<'_>) -> rusqlite::Result<ModelMapping> {
    Ok(ModelMapping {
        raw_key: row.get(0)?,
        raw_model: row.get(1)?,
        official_model: row.get(2)?,
        official_slug: row.get(3)?,
        lab: row.get(4)?,
        origin: row.get(5)?,
        confidence: row.get(6)?,
        reason: row.get(7)?,
        confirmed: row.get::<_, i64>(8)? != 0,
        updated_at: row.get(9)?,
    })
}

const SELECT_COLUMNS: &str = "raw_key, raw_model, official_model, official_slug, lab, origin,
     confidence, reason, confirmed, updated_at";

pub fn list_mappings(database: &Database) -> Result<Vec<ModelMapping>, String> {
    let connection = database.lock_conn()?;
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM token_model_mappings
         ORDER BY confirmed DESC, official_model COLLATE NOCASE, raw_key"
    );
    let mut statement = connection.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], row_mapping)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

/// 登记原始模型名。已存在的行不覆盖，避免抹掉已确认结果。
pub fn register_raw_models(database: &Database, names: &[String]) -> Result<usize, String> {
    if names.is_empty() {
        return Ok(0);
    }
    let mut connection = database.lock_conn()?;
    let transaction = connection.transaction().map_err(|e| e.to_string())?;
    let mut inserted = 0usize;
    for name in names {
        let key = raw_key(name);
        if key.is_empty() {
            continue;
        }
        inserted += transaction
            .execute(
                "INSERT OR IGNORE INTO token_model_mappings (raw_key, raw_model, origin)
                 VALUES (?1, ?2, ?3)",
                params![key, name.trim(), ORIGIN_RULE],
            )
            .map_err(|e| e.to_string())?;
    }
    transaction.commit().map_err(|e| e.to_string())?;
    Ok(inserted)
}

/// 取出待 AI 分析的条目。force = false 时只取未确认的行；
/// force = true 时重跑全部条目，但手工修改的行始终保留，不被 AI 重判。
pub fn pending_models(database: &Database, force: bool) -> Result<Vec<PendingModel>, String> {
    let connection = database.lock_conn()?;
    let sql = if force {
        "SELECT raw_key, raw_model FROM token_model_mappings
         WHERE origin != 'manual' ORDER BY raw_key"
    } else {
        "SELECT raw_key, raw_model FROM token_model_mappings
         WHERE confirmed = 0 ORDER BY raw_key"
    };
    let mut statement = connection.prepare(sql).map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], |row| {
            let raw_key: String = row.get(0)?;
            let raw_model: String = row.get(1)?;
            Ok(PendingModel {
                rule_base: rule_base_name(&raw_model),
                raw_key,
                raw_model,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

pub fn count_confirmed(database: &Database) -> Result<usize, String> {
    let connection = database.lock_conn()?;
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM token_model_mappings WHERE confirmed = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(count as usize)
}

/// 已确认映射作为 AI 分析的「标准答案」注入提示词：
/// 同族原始名必须沿用它对应的正式名，避免两批分析得出不一致结果。
/// exclude_keys 用来排除本批正在（重）判定的条目，防止同一条既当标准又当考题。
pub fn confirmed_standards(
    connection: &Connection,
    exclude_keys: &[String],
) -> Result<Vec<(String, String)>, String> {
    let mut statement = connection
        .prepare(
            "SELECT raw_model, official_model FROM token_model_mappings
             WHERE confirmed = 1 AND official_model != ''
             ORDER BY raw_key",
        )
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .filter(|(raw, _)| {
            let key = raw_key(raw);
            !exclude_keys.iter().any(|excluded| *excluded == key)
        })
        .collect())
}

/// 正式模型候选池：从 token_official_models 轻量标准库查询，替代原有的模型目录全量查询。
/// 根据待分析条目的关键词智能筛选，候选池从 2500+ 条降至 50-100 条。
pub fn official_catalog(connection: &Connection) -> Result<Vec<(String, String, String)>, String> {
    let mut statement = connection
        .prepare(
            "SELECT name, id, lab FROM token_official_models
             ORDER BY confidence DESC, lab, name
             LIMIT 300",
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
    Ok(rows)
}

/// 写回一批 AI 结果。force = false 时仅覆盖未确认的行；
/// force = true 时可覆盖已确认行，但手工修改（origin = manual）的行始终保留。
/// 新架构：AI 返回的模型名不在清单中时，自动创建占位符（来源标记为 ai）。
pub fn apply_ai_results(
    database: &Database,
    items: &[AiMappingItem],
    force: bool,
) -> Result<usize, String> {
    let mut connection = database.lock_conn()?;
    let transaction = connection.transaction().map_err(|e| e.to_string())?;
    let catalog = official_catalog(&transaction)?;
    let mut applied = 0usize;
    for item in items {
        if item.official_model.trim().is_empty() {
            continue;
        }
        let key = raw_key(&item.raw_model);
        if key.is_empty() {
            continue;
        }

        let official_trimmed = item.official_model.trim();

        // 1. 尝试在 token_official_models 中查找（大小写不敏感）
        let matched = catalog
            .iter()
            .find(|(name, _, _)| name.eq_ignore_ascii_case(official_trimmed));

        let (official, slug, lab) = match matched {
            Some((name, slug, lab)) => (name.clone(), Some(slug.clone()), Some(lab.clone())),
            None => {
                // 2. 不在清单中 → 自动创建占位符，标记来源为 ai
                let id = official_trimmed.to_lowercase().replace(" ", "-");
                let lab = extract_lab_from_name(official_trimmed);
                transaction
                    .execute(
                        "INSERT OR IGNORE INTO token_official_models
                         (id, name, lab, source, confidence)
                         VALUES (?1, ?2, ?3, 'ai', ?4)",
                        params![id, official_trimmed, lab, item.confidence],
                    )
                    .map_err(|e| e.to_string())?;

                // 使用新创建的模型
                (official_trimmed.to_string(), Some(id), Some(lab))
            }
        };

        let guard = if force {
            " AND origin != 'manual'"
        } else {
            " AND confirmed = 0"
        };
        let sql = format!(
            "UPDATE token_model_mappings
             SET official_model = ?1, official_slug = ?2, lab = ?3, origin = ?4,
                 confidence = ?5, reason = ?6, confirmed = 1,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE raw_key = ?7{guard}"
        );
        applied += transaction
            .execute(
                &sql,
                params![
                    official,
                    slug,
                    item.lab.clone().or(lab),
                    ORIGIN_AI,
                    item.confidence,
                    item.reason,
                    key
                ],
            )
            .map_err(|e| e.to_string())?;
    }
    transaction.commit().map_err(|e| e.to_string())?;
    Ok(applied)
}

/// 从模型名中提取厂商标识（简单启发式）
fn extract_lab_from_name(name: &str) -> String {
    let lower = name.to_lowercase();
    if lower.contains("gpt") || lower.contains("openai") {
        "openai".to_string()
    } else if lower.contains("claude") || lower.contains("anthropic") {
        "anthropic".to_string()
    } else if lower.contains("gemini") || lower.contains("google") {
        "google".to_string()
    } else if lower.contains("glm") || lower.contains("zhipu") {
        "zhipu".to_string()
    } else if lower.contains("qwen") || lower.contains("alibaba") {
        "alibaba".to_string()
    } else if lower.contains("deepseek") {
        "deepseek".to_string()
    } else if lower.contains("mistral") {
        "mistral".to_string()
    } else if lower.contains("llama") || lower.contains("meta") {
        "meta".to_string()
    } else {
        "unknown".to_string()
    }
}

/// 手工修改单条映射，来源标记为 manual 并置为已确认。
/// 新架构：不再要求正式模型必须在目录中，支持用户自定义任意模型。
pub fn set_mapping_manually(
    database: &Database,
    raw_model: &str,
    official_model: &str,
) -> Result<ModelMapping, String> {
    let key = raw_key(raw_model);
    if key.is_empty() {
        return Err("原始模型名不能为空".to_string());
    }
    let mut connection = database.lock_conn()?;
    let trimmed = official_model.trim();
    let (official, slug, lab) = if trimmed.is_empty() {
        (String::new(), None, None)
    } else {
        let transaction = connection.transaction().map_err(|e| e.to_string())?;

        // 1. 尝试在 token_official_models 中查找
        let existing = transaction
            .query_row(
                "SELECT id, name, lab FROM token_official_models
                 WHERE name = ?1 OR id = ?1 COLLATE NOCASE",
                params![trimmed],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| e.to_string())?;

        let (id, name, lab) = match existing {
            Some((id, name, lab)) => (id, name, lab),
            None => {
                // 2. 不存在 → 自动创建，标记来源为 user
                let id = trimmed.to_lowercase().replace(" ", "-");
                let lab = extract_lab_from_name(trimmed);
                transaction
                    .execute(
                        "INSERT INTO token_official_models
                         (id, name, lab, source, confidence)
                         VALUES (?1, ?2, ?3, 'user', 0.5)",
                        params![id, trimmed, lab],
                    )
                    .map_err(|e| e.to_string())?;
                (id, trimmed.to_string(), lab)
            }
        };

        transaction.commit().map_err(|e| e.to_string())?;
        (name, Some(id), Some(lab))
    };

    let confirmed = if official.is_empty() { 0 } else { 1 };
    connection
        .execute(
            "INSERT INTO token_model_mappings
                (raw_key, raw_model, official_model, official_slug, lab, origin,
                 confidence, reason, confirmed, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1.0, NULL, ?7,
                     strftime('%Y-%m-%dT%H:%M:%fZ','now'))
             ON CONFLICT(raw_key) DO UPDATE SET
                official_model = excluded.official_model,
                official_slug  = excluded.official_slug,
                lab            = excluded.lab,
                origin         = excluded.origin,
                confidence     = excluded.confidence,
                reason         = NULL,
                confirmed      = excluded.confirmed,
                updated_at     = excluded.updated_at",
            params![
                key,
                raw_model.trim(),
                official,
                slug,
                lab,
                ORIGIN_MANUAL,
                confirmed
            ],
        )
        .map_err(|e| e.to_string())?;
    let sql = format!("SELECT {SELECT_COLUMNS} FROM token_model_mappings WHERE raw_key = ?1");
    connection
        .query_row(&sql, params![key], row_mapping)
        .map_err(|e| e.to_string())
}

/// 从 model_catalog_models 初始化 token_official_models（一次性迁移）
/// 只迁移尚不存在的模型，不覆盖已有的用户自定义或 AI 学习的模型
pub fn migrate_catalog_to_official_models(database: &Database) -> Result<usize, String> {
    let connection = database.lock_conn()?;

    // 检查 model_catalog_models 表是否存在
    let catalog_exists: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='model_catalog_models'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;

    if catalog_exists == 0 {
        return Ok(0); // 目录表不存在，跳过迁移
    }

    // 迁移：INSERT OR IGNORE 确保不覆盖已有条目
    let migrated = connection
        .execute(
            "INSERT OR IGNORE INTO token_official_models (id, name, lab, source, confidence)
             SELECT
                LOWER(COALESCE(slug, id)) as id,
                name,
                COALESCE(lab, '') as lab,
                'catalog' as source,
                1.0 as confidence
             FROM model_catalog_models
             WHERE name IS NOT NULL AND name != ''",
            [],
        )
        .map_err(|e| e.to_string())?;

    Ok(migrated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_database() -> Database {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE token_model_mappings (
                    raw_key TEXT PRIMARY KEY,
                    raw_model TEXT NOT NULL,
                    official_model TEXT NOT NULL DEFAULT '',
                    official_slug TEXT,
                    lab TEXT,
                    origin TEXT NOT NULL DEFAULT 'rule',
                    confidence REAL NOT NULL DEFAULT 0,
                    reason TEXT,
                    confirmed INTEGER NOT NULL DEFAULT 0,
                    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
                );
                CREATE TABLE token_official_models (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    lab TEXT NOT NULL DEFAULT '',
                    aliases TEXT NOT NULL DEFAULT '[]',
                    source TEXT NOT NULL DEFAULT 'catalog',
                    confidence REAL NOT NULL DEFAULT 1.0,
                    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
                );",
            )
            .unwrap();
        Database(std::sync::Mutex::new(connection))
    }

    fn insert_catalog(database: &Database, name: &str) {
        let connection = database.lock_conn().unwrap();
        connection
            .execute(
                "INSERT INTO token_official_models (id, name, lab, source) VALUES (?1, ?2, ?3, 'catalog')",
                params![name.to_lowercase(), name, "test-lab"],
            )
            .unwrap();
    }

    #[test]
    fn force_pending_keeps_manual_rows_out() {
        let database = test_database();
        insert_catalog(&database, "GLM-5.3");
        register_raw_models(
            &database,
            &["zai-glm-5-3".to_string(), "alpha-gpt".to_string()],
        )
        .unwrap();
        set_mapping_manually(&database, "zai-glm-5-3", "GLM-5.3").unwrap();

        // 手工行 confirmed = 1 且 origin = manual，force 时也不应进入待分析清单。
        let pending = pending_models(&database, true).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].raw_model, "alpha-gpt");
    }

    #[test]
    fn confirmed_standards_exclude_pending_keys() {
        let database = test_database();
        insert_catalog(&database, "GLM-5.3");
        register_raw_models(
            &database,
            &["zai-glm-5-3".to_string(), "glm-5.3-flash".to_string()],
        )
        .unwrap();
        set_mapping_manually(&database, "zai-glm-5-3", "GLM-5.3").unwrap();
        set_mapping_manually(&database, "glm-5.3-flash", "GLM-5.3").unwrap();

        let connection = database.lock_conn().unwrap();
        let all = confirmed_standards(&connection, &[]).unwrap();
        assert_eq!(all.len(), 2);
        // 排除正在重判的条目，防止同一条既当标准又当考题。
        let excluded = confirmed_standards(&connection, &["zai-glm-5-3".to_string()]).unwrap();
        assert_eq!(excluded.len(), 1);
        assert_eq!(excluded[0].0, "glm-5.3-flash");
    }

    #[test]
    fn apply_ai_results_never_overwrites_manual_rows() {
        let database = test_database();
        insert_catalog(&database, "GLM-5.3");
        register_raw_models(&database, &["zai-glm-5-3".to_string()]).unwrap();
        let manual = set_mapping_manually(&database, "zai-glm-5-3", "GLM-5.3").unwrap();

        let applied = apply_ai_results(
            &database,
            &[AiMappingItem {
                raw_model: "zai-glm-5-3".to_string(),
                official_model: "GLM-5.3".to_string(),
                lab: None,
                confidence: 0.5,
                reason: Some("AI 重判".to_string()),
            }],
            true,
        )
        .unwrap();
        assert_eq!(applied, 0);
        let after = list_mappings(&database).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].origin, ORIGIN_MANUAL);
        assert_eq!(after[0].reason, manual.reason);
    }

    #[test]
    fn apply_ai_results_force_overwrites_confirmed_ai_rows() {
        let database = test_database();
        insert_catalog(&database, "GLM-5.3");
        register_raw_models(&database, &["zai-glm-5-3".to_string()]).unwrap();

        let applied = apply_ai_results(
            &database,
            &[AiMappingItem {
                raw_model: "zai-glm-5-3".to_string(),
                official_model: "GLM-5.3".to_string(),
                lab: None,
                confidence: 0.5,
                reason: Some("首次判定".to_string()),
            }],
            false,
        )
        .unwrap();
        assert_eq!(applied, 1);

        // force = true 时已确认的 AI 行允许被重新判定覆盖。
        let pending = pending_models(&database, true).unwrap();
        assert_eq!(pending.len(), 1);
        let reapplied = apply_ai_results(
            &database,
            &[AiMappingItem {
                raw_model: "zai-glm-5-3".to_string(),
                official_model: "GLM-5.3".to_string(),
                lab: None,
                confidence: 0.9,
                reason: Some("重判".to_string()),
            }],
            true,
        )
        .unwrap();
        assert_eq!(reapplied, 1);
        let rows = list_mappings(&database).unwrap();
        assert_eq!(rows[0].reason.as_deref(), Some("重判"));
    }

    #[test]
    fn apply_ai_results_auto_creates_new_models() {
        let database = test_database();
        register_raw_models(&database, &["mystery-model".to_string()]).unwrap();

        // AI 返回不在目录中的模型名，新架构应自动创建
        let applied = apply_ai_results(
            &database,
            &[AiMappingItem {
                raw_model: "mystery-model".to_string(),
                official_model: "Mystery LLM Pro".to_string(),
                lab: Some("mystery-lab".to_string()),
                confidence: 0.9,
                reason: Some("AI 识别新模型".to_string()),
            }],
            false,
        )
        .unwrap();

        assert_eq!(applied, 1);

        // 验证映射已创建
        let rows = list_mappings(&database).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].confirmed);
        assert_eq!(rows[0].official_model, "Mystery LLM Pro");

        // 验证 token_official_models 中自动创建了条目
        let connection = database.lock_conn().unwrap();
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM token_official_models WHERE name = 'Mystery LLM Pro'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
}
