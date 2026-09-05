use super::types::*;
use crate::models::Database;
use rusqlite::params;

/// 建表（幂等）：在 Database::open 末尾调用。
/// 旧版评测 schema（含 judge_json / prompt_count）直接清表重建——验真与评测语义不兼容。
pub(crate) fn ensure_model_test_tables(connection: &rusqlite::Connection) -> Result<(), String> {
    let legacy: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('model_test_results') WHERE name = 'judge_json'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if legacy > 0 {
        connection
            .execute_batch(
                "DROP TABLE IF EXISTS model_test_results;
                 DROP TABLE IF EXISTS model_test_runs;",
            )
            .map_err(|error| error.to_string())?;
    }
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS model_test_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                started_at TEXT NOT NULL,
                finished_at TEXT,
                status TEXT NOT NULL DEFAULT 'running',
                target_count INTEGER NOT NULL DEFAULT 0,
                probe_count INTEGER NOT NULL DEFAULT 0,
                repeats INTEGER NOT NULL DEFAULT 1,
                config_json TEXT NOT NULL DEFAULT '{}',
                summary_json TEXT
            );
            CREATE TABLE IF NOT EXISTS model_test_results (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id INTEGER NOT NULL REFERENCES model_test_runs(id) ON DELETE CASCADE,
                channel_id TEXT NOT NULL,
                channel_name TEXT NOT NULL,
                model TEXT NOT NULL,
                probe_id TEXT NOT NULL,
                probe_name TEXT NOT NULL,
                category TEXT NOT NULL DEFAULT '',
                sample_index INTEGER NOT NULL DEFAULT 0,
                ok INTEGER NOT NULL DEFAULT 0,
                duration_ms INTEGER,
                prompt_tokens INTEGER,
                completion_tokens INTEGER,
                tokens_per_sec REAL,
                auto_check_json TEXT,
                family_match TEXT,
                request_text TEXT,
                error TEXT,
                response_text TEXT,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_model_test_results_run ON model_test_results(run_id);
            CREATE INDEX IF NOT EXISTS idx_model_test_runs_started ON model_test_runs(started_at DESC);",
        )
        .map_err(|error| error.to_string())?;
    // 新版表若缺 request_text 列（对话伪装功能引入）则幂等补列
    let has_request_text: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('model_test_results') WHERE name = 'request_text'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if has_request_text == 0 {
        connection
            .execute(
                "ALTER TABLE model_test_results ADD COLUMN request_text TEXT",
                [],
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(())
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
            "INSERT INTO model_test_runs (started_at, status, target_count, probe_count, repeats, config_json)
             VALUES (?1, 'running', ?2, ?3, ?4, ?5)",
            params![
                started_at,
                params.targets.len() as i64,
                params.probe_ids.len() as i64,
                params.repeats.clamp(1, 5) as i64,
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
    Ok(ProbeResult {
        channel_id: row.get("channel_id")?,
        channel_name: row.get("channel_name")?,
        model: row.get("model")?,
        probe_id: row.get("probe_id")?,
        probe_name: row.get("probe_name")?,
        category: row.get("category")?,
        sample_index: row.get::<_, i64>("sample_index")? as u32,
        ok: row.get::<_, i64>("ok")? != 0,
        duration_ms: row.get("duration_ms")?,
        prompt_tokens: row.get("prompt_tokens")?,
        completion_tokens: row.get("completion_tokens")?,
        tokens_per_sec: row.get("tokens_per_sec")?,
        auto_check: auto_check_json.and_then(|json| serde_json::from_str(&json).ok()),
        family_match: row.get("family_match")?,
        request_text: row.get("request_text")?,
        error: row.get("error")?,
        response_text: row.get("response_text")?,
    })
}

pub(crate) fn insert_result(database: &Database, run_id: i64, result: &ProbeResult) -> Result<(), String> {
    let connection = database.lock_conn()?;
    connection
        .execute(
            "INSERT INTO model_test_results (
                run_id, channel_id, channel_name, model, probe_id, probe_name, category,
                sample_index, ok, duration_ms, prompt_tokens, completion_tokens, tokens_per_sec,
                auto_check_json, family_match, request_text, error, response_text, created_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
            params![
                run_id,
                result.channel_id,
                result.channel_name,
                result.model,
                result.probe_id,
                result.probe_name,
                result.category,
                result.sample_index as i64,
                result.ok as i64,
                result.duration_ms.map(|v| v as i64),
                result.prompt_tokens.map(|v| v as i64),
                result.completion_tokens.map(|v| v as i64),
                result.tokens_per_sec,
                result.auto_check.as_ref().and_then(|c| serde_json::to_string(c).ok()),
                result.family_match,
                result.request_text,
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
            "SELECT id, started_at, finished_at, status, target_count, probe_count, repeats,
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
                probe_count: row.get("probe_count")?,
                repeats: row.get("repeats")?,
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
            "SELECT run_id, channel_id, channel_name, model, probe_id, probe_name, category,
                    sample_index, ok, duration_ms, prompt_tokens, completion_tokens, tokens_per_sec,
                    auto_check_json, family_match, request_text, error, response_text, created_at
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
            probe_ids: vec!["cap-math".into()],
            repeats: 3,
            concurrency: 2,
            timeout_seconds: 60,
        };
        let run_id = insert_run(&database, "2026-01-01 00:00:00", &params).unwrap();
        let result = ProbeResult {
            channel_id: "c1".into(),
            channel_name: "渠道一".into(),
            model: "m1".into(),
            probe_id: "cap-math".into(),
            probe_name: "多步应用题".into(),
            category: "capability".into(),
            sample_index: 0,
            ok: true,
            duration_ms: Some(1234),
            prompt_tokens: Some(10),
            completion_tokens: Some(20),
            tokens_per_sec: Some(16.2),
            auto_check: Some(AutoCheckOutcome {
                kind: "number".into(),
                passed: true,
                detail: "命中 324".into(),
            }),
            family_match: None,
            request_text: Some("算一下利润".into()),
            error: None,
            response_text: Some("324".into()),
        };
        insert_result(&database, run_id, &result).unwrap();
        finish_run(&database, run_id, "finished", &RunSummary::default()).unwrap();

        let runs = list_runs(&database, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "finished");
        assert_eq!(runs[0].target_count, 1);
        assert_eq!(runs[0].probe_count, 1);
        assert_eq!(runs[0].repeats, 3);

        let results = get_run_results(&database, run_id).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].model, "m1");
        assert_eq!(results[0].sample_index, 0);
        assert_eq!(results[0].auto_check.as_ref().unwrap().kind, "number");
        assert_eq!(results[0].request_text.as_deref(), Some("算一下利润"));

        delete_run(&database, run_id).unwrap();
        assert!(list_runs(&database, 10).unwrap().is_empty());
        assert!(get_run_results(&database, run_id).unwrap().is_empty());
    }
}
