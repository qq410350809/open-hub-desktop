use super::normalize::{raw_key, rule_base_name};
use super::types::*;
use crate::models::Database;
use rusqlite::{params, Connection};

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

/// 正式模型候选池：模型目录里的规范名，供 AI 选择而非自由生成。
pub fn official_catalog(connection: &Connection) -> Result<Vec<(String, String, String)>, String> {
    let mut statement = connection
        .prepare(
            "SELECT name, COALESCE(slug,''), COALESCE(lab,'')
             FROM model_catalog_models
             ORDER BY lab, name",
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
        // 只接受落在目录里的正式名，AI 编造的名字直接丢弃。
        let matched = catalog
            .iter()
            .find(|(name, _, _)| name.eq_ignore_ascii_case(item.official_model.trim()));
        let (official, slug, lab) = match matched {
            Some((name, slug, lab)) => (name.clone(), Some(slug.clone()), Some(lab.clone())),
            None => continue,
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

/// 手工修改单条映射，来源标记为 manual 并置为已确认。
pub fn set_mapping_manually(
    database: &Database,
    raw_model: &str,
    official_model: &str,
) -> Result<ModelMapping, String> {
    let key = raw_key(raw_model);
    if key.is_empty() {
        return Err("原始模型名不能为空".to_string());
    }
    let connection = database.lock_conn()?;
    let trimmed = official_model.trim();
    let (official, slug, lab) = if trimmed.is_empty() {
        (String::new(), None, None)
    } else {
        official_catalog(&connection)?
            .into_iter()
            .find(|(name, _, _)| name.eq_ignore_ascii_case(trimmed))
            .map(|(name, slug, lab)| (name, Some(slug), Some(lab)))
            .ok_or_else(|| format!("正式模型不在模型目录中：{trimmed}"))?
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
                CREATE TABLE model_catalog_models (
                    id TEXT PRIMARY KEY,
                    slug TEXT,
                    name TEXT NOT NULL,
                    lab TEXT
                );",
            )
            .unwrap();
        Database(std::sync::Mutex::new(connection))
    }

    fn insert_catalog(database: &Database, name: &str) {
        let connection = database.lock_conn().unwrap();
        connection
            .execute(
                "INSERT INTO model_catalog_models (id, slug, name, lab) VALUES (?1, ?2, ?1, ?3)",
                params![name, name.to_lowercase(), "test-lab"],
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
    fn apply_ai_results_drops_names_outside_catalog() {
        let database = test_database();
        register_raw_models(&database, &["mystery-model".to_string()]).unwrap();
        let applied = apply_ai_results(
            &database,
            &[AiMappingItem {
                raw_model: "mystery-model".to_string(),
                official_model: "Not-In-Catalog".to_string(),
                lab: None,
                confidence: 0.9,
                reason: None,
            }],
            false,
        )
        .unwrap();
        assert_eq!(applied, 0);
        let rows = list_mappings(&database).unwrap();
        assert!(!rows[0].confirmed);
        assert!(rows[0].official_model.is_empty());
    }
}
