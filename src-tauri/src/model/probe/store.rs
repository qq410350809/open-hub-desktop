use super::types::*;
use crate::context::AppContext;
use crate::models::Database;
use rusqlite::params;
use std::sync::Arc;

/// 建表（幂等）：在 Database::open 末尾调用。
pub(crate) fn ensure_model_test_tables(connection: &rusqlite::Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS model_test_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                started_at TEXT NOT NULL,
                finished_at TEXT,
                status TEXT NOT NULL DEFAULT 'running',
                target_count INTEGER NOT NULL DEFAULT 0,
                prompt_count INTEGER NOT NULL DEFAULT 0,
                config_json TEXT NOT NULL DEFAULT '{}',
                summary_json TEXT
            );
            CREATE TABLE IF NOT EXISTS model_test_results (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id INTEGER NOT NULL REFERENCES model_test_runs(id) ON DELETE CASCADE,
                channel_id TEXT NOT NULL,
                channel_name TEXT NOT NULL,
                model TEXT NOT NULL,
                prompt_id TEXT NOT NULL,
                prompt_name TEXT NOT NULL,
                category TEXT NOT NULL DEFAULT '',
                ok INTEGER NOT NULL DEFAULT 0,
                duration_ms INTEGER,
                prompt_tokens INTEGER,
                completion_tokens INTEGER,
                tokens_per_sec REAL,
                auto_check_json TEXT,
                score REAL,
                judge_json TEXT,
                error TEXT,
                response_text TEXT,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_model_test_results_run ON model_test_results(run_id);
            CREATE INDEX IF NOT EXISTS idx_model_test_runs_started ON model_test_runs(started_at DESC);",
        )
        .map_err(|error| error.to_string())
}

pub(crate) fn insert_run(
    database: &Database,
    started_at: &str,
    params: &RunParams,
) -> Result<i64, String> {
    let config_json = serde_json::to_string(params).unwrap_or_else(|_| "{}".to_string());
    let connection = database.lock_conn()?;
    connection
        .execute(
            "INSERT INTO model_test_runs (started_at, status, target_count, prompt_count, config_json)
             VALUES (?1, 'running', ?2, ?3, ?4)",
            params![
                started_at,
                params.targets.len() as i64,
                params.prompts.len() as i64,
                config_json
            ],
        )
        .map_err(|error| error.to_string())?;
    Ok(connection.last_insert_rowid())
}

pub(crate) fn finish_run(
    database: &Database,
    run_id: i64,
    status: &str,
    summary: &RunSummary,
) -> Result<(), String> {
    let summary_json = serde_json::to_string(summary).unwrap_or_else(|_| "{}".to_string());
    let connection = database.lock_conn()?;
    connection
        .execute(
            "UPDATE model_test_runs
               SET finished_at = ?2, status = ?3, summary_json = ?4
             WHERE id = ?1",
            params![
                run_id,
                crate::model::gateway::current_timestamp(),
                status,
                summary_json
            ],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn row_to_result(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProbeResult> {
    let auto_check_json: Option<String> = row.get("auto_check_json")?;
    let judge_json: Option<String> = row.get("judge_json")?;
    Ok(ProbeResult {
        channel_id: row.get("channel_id")?,
        channel_name: row.get("channel_name")?,
        model: row.get("model")?,
        prompt_id: row.get("prompt_id")?,
        prompt_name: row.get("prompt_name")?,
        category: row.get("category")?,
        ok: row.get::<_, i64>("ok")? != 0,
        duration_ms: row.get("duration_ms")?,
        prompt_tokens: row.get("prompt_tokens")?,
        completion_tokens: row.get("completion_tokens")?,
        tokens_per_sec: row.get("tokens_per_sec")?,
        auto_check: auto_check_json.and_then(|json| serde_json::from_str(&json).ok()),
        score: row.get("score")?,
        judge: judge_json.and_then(|json| serde_json::from_str(&json).ok()),
        error: row.get("error")?,
        response_text: row.get("response_text")?,
    })
}

pub(crate) fn insert_result(database: &Database, run_id: i64, result: &ProbeResult) -> Result<(), String> {
    let connection = database.lock_conn()?;
    connection
        .execute(
            "INSERT INTO model_test_results (
                run_id, channel_id, channel_name, model, prompt_id, prompt_name, category,
                ok, duration_ms, prompt_tokens, completion_tokens, tokens_per_sec,
                auto_check_json, score, judge_json, error, response_text, created_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
            params![
                run_id,
                result.channel_id,
                result.channel_name,
                result.model,
                result.prompt_id,
                result.prompt_name,
                result.category,
                result.ok as i64,
                result.duration_ms.map(|v| v as i64),
                result.prompt_tokens.map(|v| v as i64),
                result.completion_tokens.map(|v| v as i64),
                result.tokens_per_sec,
                result.auto_check.as_ref().and_then(|c| serde_json::to_string(c).ok()),
                result.score,
                result.judge.as_ref().and_then(|c| serde_json::to_string(c).ok()),
                result.error,
                result.response_text,
                crate::model::gateway::current_timestamp(),
            ],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn list_runs(database: &Database, limit: u32) -> Result<Vec<TestRunRecord>, String> {
    let connection = database.lock_conn()?;
    let mut statement = connection
        .prepare(
            "SELECT id, started_at, finished_at, status, target_count, prompt_count,
                    config_json, summary_json
               FROM model_test_runs ORDER BY id DESC LIMIT ?1",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![limit as i64], |row| {
            let config_json: String = row.get("config_json")?;
            let summary_json: Option<String> = row.get("summary_json")?;
            Ok(TestRunRecord {
                id: row.get("id")?,
                started_at: row.get("started_at")?,
                finished_at: row.get("finished_at")?,
                status: row.get("status")?,
                target_count: row.get("target_count")?,
                prompt_count: row.get("prompt_count")?,
                config: serde_json::from_str(&config_json).unwrap_or(serde_json::json!({})),
                summary: summary_json.and_then(|json| serde_json::from_str(&json).ok()),
            })
        })
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())
}

/// 历史运行列表里遗留的 running 批次标记为 error（进程崩溃兜底，避免永远 running）。
/// `exclude_run_id` 用于排除当前正在运行的批次。
pub(crate) fn reap_stale_runs(
    database: &Database,
    exclude_run_id: Option<i64>,
) -> Result<(), String> {
    let connection = database.lock_conn()?;
    connection
        .execute(
            "UPDATE model_test_runs SET status = 'error', finished_at = ?1
               WHERE status = 'running' AND id != COALESCE(?2, -1)",
            params![
                crate::model::gateway::current_timestamp(),
                exclude_run_id
            ],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn get_run_results(
    database: &Database,
    run_id: i64,
) -> Result<Vec<ProbeResult>, String> {
    let connection = database.lock_conn()?;
    let mut statement = connection
        .prepare(
            "SELECT id, run_id, channel_id, channel_name, model, prompt_id, prompt_name, category,
                    ok, duration_ms, prompt_tokens, completion_tokens, tokens_per_sec,
                    auto_check_json, score, judge_json, error, response_text, created_at
               FROM model_test_results WHERE run_id = ?1 ORDER BY id",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![run_id], row_to_result)
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())
}

pub(crate) fn delete_run(database: &Database, run_id: i64) -> Result<u64, String> {
    let connection = database.lock_conn()?;
    let affected = connection
        .execute("DELETE FROM model_test_results WHERE run_id = ?1", params![run_id])
        .map_err(|error| error.to_string())?;
    connection
        .execute("DELETE FROM model_test_runs WHERE id = ?1", params![run_id])
        .map_err(|error| error.to_string())?;
    Ok(affected as u64)
}

/// 自定义提示词列表（app_meta JSON）。
pub(crate) fn get_custom_prompts(database: &Database) -> Result<Vec<ProbePrompt>, String> {
    let raw = crate::db::read_meta(database, CUSTOM_PROMPTS_META_KEY)?;
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&raw).map_err(|error| format!("自定义提示词解析失败：{error}"))
}

pub(crate) fn save_custom_prompts(
    database: &Database,
    prompts: &[ProbePrompt],
) -> Result<(), String> {
    let json = serde_json::to_string(prompts).map_err(|error| error.to_string())?;
    let connection = database.lock_conn()?;
    crate::db::write_meta(&connection, CUSTOM_PROMPTS_META_KEY, &json)
}

/// 上次运行配置（页面恢复用）。
pub(crate) fn get_last_config(ctx: &Arc<AppContext>) -> Result<Option<serde_json::Value>, String> {
    let raw = crate::db::read_meta(&ctx.database, LAST_CONFIG_META_KEY)?;
    if raw.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|error| format!("上次运行配置解析失败：{error}"))
}

pub(crate) fn save_last_config(
    ctx: &Arc<AppContext>,
    config: &serde_json::Value,
) -> Result<(), String> {
    let json = serde_json::to_string(config).map_err(|error| error.to_string())?;
    let connection = ctx.database.lock_conn()?;
    crate::db::write_meta(&connection, LAST_CONFIG_META_KEY, &json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_db(name: &str) -> Database {
        let dir = std::env::temp_dir().join(format!("openhub-probe-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{name}.sqlite3"));
        let _ = std::fs::remove_file(&path);
        Database::open(&path).expect("open memory db")
    }

    #[test]
    fn run_lifecycle_roundtrip() {
        let database = memory_db("lifecycle");
        let params = RunParams {
            targets: vec![ProbeTarget { channel_id: "c1".into(), model: "m1".into() }],
            prompts: vec![],
            concurrency: 2,
            timeout_seconds: 60,
            judge: None,
        };
        let run_id = insert_run(&database, "2026-01-01 00:00:00", &params).unwrap();
        let result = ProbeResult {
            channel_id: "c1".into(),
            channel_name: "渠道一".into(),
            model: "m1".into(),
            prompt_id: "p1".into(),
            prompt_name: "题目一".into(),
            category: "推理".into(),
            ok: true,
            duration_ms: Some(1234),
            prompt_tokens: Some(10),
            completion_tokens: Some(20),
            tokens_per_sec: Some(16.2),
            auto_check: Some(AutoCheckOutcome {
                kind: "number".into(),
                passed: true,
                detail: "命中 42".into(),
            }),
            score: Some(10.0),
            judge: None,
            error: None,
            response_text: Some("答案是 42".into()),
        };
        insert_result(&database, run_id, &result).unwrap();
        finish_run(&database, run_id, "finished", &RunSummary::default()).unwrap();

        let runs = list_runs(&database, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "finished");
        assert_eq!(runs[0].target_count, 1);

        let results = get_run_results(&database, run_id).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].model, "m1");
        assert_eq!(results[0].score, Some(10.0));
        assert_eq!(results[0].auto_check.as_ref().unwrap().kind, "number");

        delete_run(&database, run_id).unwrap();
        assert!(list_runs(&database, 10).unwrap().is_empty());
        assert!(get_run_results(&database, run_id).unwrap().is_empty());
    }

    #[test]
    fn custom_prompts_roundtrip() {
        let database = memory_db("prompts");
        assert!(get_custom_prompts(&database).unwrap().is_empty());
        let prompts = vec![ProbePrompt {
            id: "custom-1".into(),
            name: "我的题".into(),
            category: "自定义".into(),
            text: "1+1=?".into(),
            max_tokens: 64,
            temperature: 0.0,
            check: None,
            judge: true,
        }];
        save_custom_prompts(&database, &prompts).unwrap();
        let loaded = get_custom_prompts(&database).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "我的题");
        assert!(loaded[0].judge);
    }
}
