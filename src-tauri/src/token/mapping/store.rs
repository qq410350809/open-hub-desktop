use super::normalize::{raw_key, rule_base_name};
use super::types::*;
use crate::models::Database;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};

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
        review_status: row.get(8)?,
        confirmed: row.get::<_, i64>(9)? != 0,
        updated_at: row.get(10)?,
    })
}

const SELECT_COLUMNS: &str = "raw_key, raw_model, official_model, official_slug, lab, origin,
     confidence, reason, review_status, confirmed, updated_at";

#[derive(Debug, Default)]
pub struct SuggestionApplyReport {
    pub suggested: usize,
    pub invalid: usize,
    pub accepted_keys: HashSet<String>,
}

pub fn list_mappings(database: &Database) -> Result<Vec<ModelMapping>, String> {
    let connection = database.lock_conn()?;
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM token_model_mappings
         ORDER BY CASE review_status
           WHEN 'suggested' THEN 0
           WHEN 'pending' THEN 1
           WHEN 'rejected' THEN 2
           ELSE 3
         END, official_model COLLATE NOCASE, raw_key"
    );
    let mut statement = connection.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], row_mapping)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

/// 登记原始模型名。已存在的行不覆盖，避免抹掉审核结果。
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
                "INSERT OR IGNORE INTO token_model_mappings
                   (raw_key, raw_model, origin, review_status)
                 VALUES (?1, ?2, ?3, ?4)",
                params![key, name.trim(), ORIGIN_RULE, REVIEW_PENDING],
            )
            .map_err(|e| e.to_string())?;
    }
    transaction.commit().map_err(|e| e.to_string())?;
    Ok(inserted)
}

/// 正常运行仅重试待处理/已驳回条目；强制运行会重新生成现有建议，
/// 但所有已经批准的映射和所有手工映射始终受保护。
pub fn pending_models(database: &Database, force: bool) -> Result<Vec<PendingModel>, String> {
    let connection = database.lock_conn()?;
    let sql = if force {
        "SELECT raw_key, raw_model FROM token_model_mappings
         WHERE origin != 'manual' AND review_status IN ('pending', 'suggested', 'rejected')
         ORDER BY raw_key"
    } else {
        "SELECT raw_key, raw_model FROM token_model_mappings
         WHERE review_status IN ('pending', 'rejected')
         ORDER BY raw_key"
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

pub fn count_approved(database: &Database) -> Result<usize, String> {
    let connection = database.lock_conn()?;
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM token_model_mappings WHERE review_status = 'approved'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(count as usize)
}

/// 已批准映射才可作为 AI 判定时的标准答案。
pub fn approved_standards(
    connection: &Connection,
    exclude_keys: &[String],
) -> Result<Vec<(String, String)>, String> {
    let mut statement = connection
        .prepare(
            "SELECT raw_model, official_model FROM token_model_mappings
             WHERE review_status = 'approved' AND official_model != ''
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

/// 正式模型候选池。全量目录留在本地，具体条目在 AI 层按每个原始名筛选。
pub fn official_catalog(connection: &Connection) -> Result<Vec<OfficialModelCandidate>, String> {
    let mut statement = connection
        .prepare(
            "SELECT id, name, lab, aliases FROM token_official_models
             ORDER BY confidence DESC, lab, name",
        )
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], |row| {
            let aliases_json: String = row.get(3)?;
            Ok(OfficialModelCandidate {
                id: row.get(0)?,
                name: row.get(1)?,
                lab: row.get(2)?,
                aliases: serde_json::from_str(&aliases_json).unwrap_or_default(),
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

pub fn candidate_matches(candidate: &OfficialModelCandidate, value: &str) -> bool {
    candidate.name.eq_ignore_ascii_case(value)
        || candidate.id.eq_ignore_ascii_case(value)
        || candidate
            .aliases
            .iter()
            .any(|alias| alias.eq_ignore_ascii_case(value))
}

fn bounded_text(value: Option<&str>, limit: usize) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(limit).collect())
}

/// 只持久化已通过本地校验的 AI 建议：
/// - rawModel 必须属于当前提交批次，且每个 raw key 只能出现一次；
/// - officialModel 必须是该条目候选池内的模型；
/// - 置信度必须是 0..=1 的有限数。
///
/// 建议不会写入 `confirmed`，因此不会改变当前的统计聚合。
pub fn apply_ai_suggestions(
    database: &Database,
    batch: &[PendingModel],
    candidates_by_key: &HashMap<String, Vec<OfficialModelCandidate>>,
    items: &[AiMappingItem],
) -> Result<SuggestionApplyReport, String> {
    let mut connection = database.lock_conn()?;
    let transaction = connection.transaction().map_err(|e| e.to_string())?;
    let expected: HashSet<&str> = batch.iter().map(|item| item.raw_key.as_str()).collect();
    let mut seen = HashSet::<String>::new();
    let mut report = SuggestionApplyReport::default();

    for item in items {
        let key = raw_key(&item.raw_model);
        if key.is_empty() || !expected.contains(key.as_str()) || !seen.insert(key.clone()) {
            report.invalid += 1;
            continue;
        }
        let official = item.official_model.trim();
        if official.is_empty() {
            continue;
        }
        if !item.confidence.is_finite() || !(0.0..=1.0).contains(&item.confidence) {
            report.invalid += 1;
            continue;
        }
        let Some(candidate) = candidates_by_key
            .get(&key)
            .and_then(|candidates| candidates.iter().find(|candidate| candidate_matches(candidate, official)))
        else {
            report.invalid += 1;
            continue;
        };

        let changed = transaction
            .execute(
                "UPDATE token_model_mappings
                 SET official_model = ?1, official_slug = ?2, lab = ?3, origin = ?4,
                     confidence = ?5, reason = ?6, review_status = ?7, confirmed = 0,
                     updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE raw_key = ?8
                   AND origin != 'manual'
                   AND review_status != 'approved'",
                params![
                    candidate.name,
                    candidate.id,
                    candidate.lab,
                    ORIGIN_AI,
                    item.confidence,
                    bounded_text(item.reason.as_deref(), 500),
                    REVIEW_SUGGESTED,
                    key,
                ],
            )
            .map_err(|e| e.to_string())?;
        if changed > 0 {
            report.suggested += 1;
            report.accepted_keys.insert(key);
        }
    }

    transaction.commit().map_err(|e| e.to_string())?;
    Ok(report)
}

pub fn approve_mapping(database: &Database, raw_model: &str) -> Result<ModelMapping, String> {
    let key = raw_key(raw_model);
    if key.is_empty() {
        return Err("原始模型名不能为空".to_string());
    }
    let connection = database.lock_conn()?;
    let changed = connection
        .execute(
            "UPDATE token_model_mappings
             SET review_status = ?1, confirmed = 1,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE raw_key = ?2 AND official_model != ''",
            params![REVIEW_APPROVED, key],
        )
        .map_err(|e| e.to_string())?;
    if changed == 0 {
        return Err("没有可批准的模型建议".to_string());
    }
    select_mapping(&connection, &key)
}

pub fn reject_mapping(database: &Database, raw_model: &str) -> Result<ModelMapping, String> {
    let key = raw_key(raw_model);
    if key.is_empty() {
        return Err("原始模型名不能为空".to_string());
    }
    let connection = database.lock_conn()?;
    let changed = connection
        .execute(
            "UPDATE token_model_mappings
             SET review_status = ?1, confirmed = 0,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE raw_key = ?2 AND review_status = 'suggested'",
            params![REVIEW_REJECTED, key],
        )
        .map_err(|e| e.to_string())?;
    if changed == 0 {
        return Err("当前条目没有可驳回的 AI 建议".to_string());
    }
    select_mapping(&connection, &key)
}

pub fn reopen_mapping(database: &Database, raw_model: &str) -> Result<ModelMapping, String> {
    let key = raw_key(raw_model);
    if key.is_empty() {
        return Err("原始模型名不能为空".to_string());
    }
    let connection = database.lock_conn()?;
    let changed = connection
        .execute(
            "UPDATE token_model_mappings
             SET official_model = '', official_slug = NULL, lab = NULL, origin = ?1,
                 confidence = 0, reason = NULL, review_status = ?2, confirmed = 0,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE raw_key = ?3",
            params![ORIGIN_RULE, REVIEW_PENDING, key],
        )
        .map_err(|e| e.to_string())?;
    if changed == 0 {
        return Err("未找到对应的模型映射".to_string());
    }
    select_mapping(&connection, &key)
}

/// 手工修改直接视为人工批准。自定义名称只会由用户明确输入时写入标准目录。
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
    if trimmed.is_empty() {
        connection
            .execute(
                "INSERT INTO token_model_mappings
                    (raw_key, raw_model, official_model, origin, confidence, reason, review_status, confirmed, updated_at)
                 VALUES (?1, ?2, '', ?3, 0, NULL, ?4, 0, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
                 ON CONFLICT(raw_key) DO UPDATE SET
                    official_model = '', official_slug = NULL, lab = NULL, origin = excluded.origin,
                    confidence = 0, reason = NULL, review_status = excluded.review_status,
                    confirmed = 0, updated_at = excluded.updated_at",
                params![key, raw_model.trim(), ORIGIN_RULE, REVIEW_PENDING],
            )
            .map_err(|e| e.to_string())?;
        return select_mapping(&connection, &key);
    }

    let transaction = connection.transaction().map_err(|e| e.to_string())?;
    let existing = transaction
        .query_row(
            "SELECT id, name, lab FROM token_official_models
             WHERE name = ?1 COLLATE NOCASE OR id = ?1 COLLATE NOCASE",
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
        Some(found) => found,
        None => {
            let id = trimmed.to_lowercase().replace(' ', "-");
            let lab = extract_lab_from_name(trimmed);
            transaction
                .execute(
                    "INSERT INTO token_official_models (id, name, lab, source, confidence)
                     VALUES (?1, ?2, ?3, 'user', 0.5)",
                    params![id, trimmed, lab],
                )
                .map_err(|e| e.to_string())?;
            (id, trimmed.to_string(), lab)
        }
    };
    transaction
        .execute(
            "INSERT INTO token_model_mappings
                (raw_key, raw_model, official_model, official_slug, lab, origin,
                 confidence, reason, review_status, confirmed, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1.0, NULL, ?7, 1,
                     strftime('%Y-%m-%dT%H:%M:%fZ','now'))
             ON CONFLICT(raw_key) DO UPDATE SET
                official_model = excluded.official_model, official_slug = excluded.official_slug,
                lab = excluded.lab, origin = excluded.origin, confidence = excluded.confidence,
                reason = NULL, review_status = excluded.review_status, confirmed = 1,
                updated_at = excluded.updated_at",
            params![key, raw_model.trim(), name, id, lab, ORIGIN_MANUAL, REVIEW_APPROVED],
        )
        .map_err(|e| e.to_string())?;
    transaction.commit().map_err(|e| e.to_string())?;
    select_mapping(&connection, &key)
}

fn select_mapping(connection: &Connection, key: &str) -> Result<ModelMapping, String> {
    let sql = format!("SELECT {SELECT_COLUMNS} FROM token_model_mappings WHERE raw_key = ?1");
    connection
        .query_row(&sql, params![key], row_mapping)
        .map_err(|e| e.to_string())
}

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

/// 从 model_catalog_models 初始化 token_official_models（一次性操作）。
pub fn migrate_catalog_to_official_models(database: &Database) -> Result<usize, String> {
    let connection = database.lock_conn()?;
    let catalog_exists: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='model_catalog_models'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    if catalog_exists == 0 {
        return Ok(0);
    }
    connection
        .execute(
            "INSERT OR IGNORE INTO token_official_models (id, name, lab, source, confidence)
             SELECT LOWER(COALESCE(slug, id)), name, COALESCE(lab, ''), 'catalog', 1.0
             FROM model_catalog_models
             WHERE name IS NOT NULL AND name != ''",
            [],
        )
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
                    review_status TEXT NOT NULL DEFAULT 'pending',
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

    fn insert_catalog(database: &Database, id: &str, name: &str, aliases: &[&str]) {
        let connection = database.lock_conn().unwrap();
        connection
            .execute(
                "INSERT INTO token_official_models (id, name, lab, aliases, source)
                 VALUES (?1, ?2, 'test-lab', ?3, 'catalog')",
                params![id, name, serde_json::to_string(aliases).unwrap()],
            )
            .unwrap();
    }

    #[test]
    fn ai_suggestion_does_not_change_approved_lookup_state() {
        let database = test_database();
        insert_catalog(&database, "glm-53", "GLM-5.3", &["glm53"]);
        register_raw_models(&database, &["zai/glm-5.3".to_string()]).unwrap();
        let batch = pending_models(&database, false).unwrap();
        let mut candidates = HashMap::new();
        candidates.insert("glm-5.3".to_string(), official_catalog(&database.lock_conn().unwrap()).unwrap());
        let result = apply_ai_suggestions(
            &database,
            &batch,
            &candidates,
            &[AiMappingItem {
                raw_model: "glm-5.3".to_string(),
                official_model: "GLM-5.3".to_string(),
                lab: None,
                confidence: 0.91,
                reason: Some("名称相同".to_string()),
            }],
        )
        .unwrap();
        assert_eq!(result.suggested, 1);
        let row = list_mappings(&database).unwrap().remove(0);
        assert_eq!(row.review_status, REVIEW_SUGGESTED);
        assert!(!row.confirmed);
        approve_mapping(&database, "glm-5.3").unwrap();
        let row = list_mappings(&database).unwrap().remove(0);
        assert_eq!(row.review_status, REVIEW_APPROVED);
        assert!(row.confirmed);
    }

    #[test]
    fn ai_suggestion_rejects_out_of_batch_unknown_candidate_and_bad_confidence() {
        let database = test_database();
        insert_catalog(&database, "glm-53", "GLM-5.3", &[]);
        register_raw_models(&database, &["glm-5.3".to_string()]).unwrap();
        let batch = pending_models(&database, false).unwrap();
        let mut candidates = HashMap::new();
        candidates.insert("glm-5.3".to_string(), official_catalog(&database.lock_conn().unwrap()).unwrap());
        let result = apply_ai_suggestions(
            &database,
            &batch,
            &candidates,
            &[
                AiMappingItem { raw_model: "other".into(), official_model: "GLM-5.3".into(), lab: None, confidence: 0.9, reason: None },
                AiMappingItem { raw_model: "glm-5.3".into(), official_model: "Invented".into(), lab: None, confidence: 0.9, reason: None },
                AiMappingItem { raw_model: "glm-5.3".into(), official_model: "GLM-5.3".into(), lab: None, confidence: 1.2, reason: None },
            ],
        )
        .unwrap();
        assert_eq!(result.suggested, 0);
        assert_eq!(result.invalid, 3);
        let row = list_mappings(&database).unwrap().remove(0);
        assert_eq!(row.review_status, REVIEW_PENDING);
        assert!(row.official_model.is_empty());
    }

    #[test]
    fn manual_mapping_is_approved_and_force_never_requeues_it() {
        let database = test_database();
        insert_catalog(&database, "glm-53", "GLM-5.3", &[]);
        register_raw_models(&database, &["glm-5.3".to_string()]).unwrap();
        let row = set_mapping_manually(&database, "glm-5.3", "GLM-5.3").unwrap();
        assert_eq!(row.review_status, REVIEW_APPROVED);
        assert!(row.confirmed);
        assert!(pending_models(&database, true).unwrap().is_empty());
    }
}
