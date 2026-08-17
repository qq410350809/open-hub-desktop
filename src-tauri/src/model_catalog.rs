use crate::db::build_http_client;
use crate::models::Database;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};

pub const LLMPRICING_MANIFEST_URL: &str = "https://llmpricing.dev/rows/manifest.json";
pub const LLMPRICING_BASE_URL: &str = "https://llmpricing.dev/rows";
const CATALOG_SCHEMA_VERSION: &str = "8";
const CATALOG_SCHEMA_META_KEY: &str = "model_catalog_schema_version";

pub(crate) struct ModelCatalogRuntime {
    syncing: AtomicBool,
}

impl ModelCatalogRuntime {
    pub(crate) fn new() -> Self {
        Self {
            syncing: AtomicBool::new(false),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelCatalogProvider {
    pub id: String,
    pub name: String,
    pub npm: Option<String>,
    pub api: Option<String>,
    pub doc: Option<String>,
    pub tier: Option<String>,
    pub subscription: bool,
    pub count: usize,
    pub date_modified: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelCatalogSourceStatus {
    pub source: String,
    pub url: String,
    pub fetched_at: String,
    pub record_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelCatalogItem {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub lab: String,
    pub kind: String,
    pub family: Option<String>,
    pub knowledge: Option<String>,
    pub status: String,
    pub open_weights: bool,
    pub reasoning: bool,
    pub tool_call: bool,
    pub attachment: bool,
    pub structured: bool,
    pub temperature: bool,
    pub input_modalities: Vec<String>,
    pub context_length: i64,
    pub context_min: i64,
    pub context_max: i64,
    pub max_output_tokens: i64,
    pub ref_provider: Option<String>,
    pub ref_official: bool,
    pub ref_input_cost: f64,
    pub ref_output_cost: f64,
    pub ref_cache_read_cost: f64,
    pub min_provider: Option<String>,
    pub min_input_cost: f64,
    pub min_output_cost: f64,
    pub min_cache_read_cost: f64,
    pub price_spread: f64,
    pub blended_min: Option<f64>,
    pub blended_trusted: Option<f64>,
    pub blended_ref: Option<f64>,
    pub host_count: usize,
    pub priced_host_count: usize,
    pub free_host_count: usize,
    pub sub_host_count: usize,
    pub host_providers: Vec<String>,
    pub aa_idx: Option<f64>,
    pub aa_coding: Option<f64>,
    pub aa_agentic: Option<f64>,
    pub aa_speed: Option<f64>,
    pub aa_ttft: Option<f64>,
    pub aa_task_cost: Option<f64>,
    pub benchmark_count: usize,
    pub release_date: Option<String>,
    pub last_updated: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelCatalogSnapshot {
    pub models: Vec<ModelCatalogItem>,
    pub providers: Vec<ModelCatalogProvider>,
    pub total: usize,
    pub last_synced_at: String,
    pub synced_today: bool,
    pub sources: Vec<ModelCatalogSourceStatus>,
    pub meta: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelCatalogDetail {
    pub model: ModelCatalogItem,
    pub providers: Vec<ModelCatalogProvider>,
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelCatalogSyncResult {
    pub synced: bool,
    pub skipped: bool,
    pub message: String,
    pub snapshot: ModelCatalogSnapshot,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogSyncReport {
    pub provider_count: usize,
    pub model_count: usize,
    pub shard_count: usize,
}

struct SyncGuard<'a>(&'a AtomicBool);
impl Drop for SyncGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn numeric(value: Option<&Value>) -> f64 {
    match value {
        Some(Value::Number(number)) => number.as_f64().unwrap_or(0.0).max(0.0),
        Some(Value::String(text)) => text.parse::<f64>().unwrap_or(0.0).max(0.0),
        _ => 0.0,
    }
}

fn opt_numeric(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(text)) => text.parse::<f64>().ok(),
        _ => None,
    }
}

fn integer(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(number)) => number
            .as_i64()
            .unwrap_or_else(|| number.as_f64().unwrap_or(0.0) as i64),
        Some(Value::String(text)) => text.parse::<i64>().unwrap_or(0),
        _ => 0,
    }
    .max(0)
}

fn boolean(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        Some(Value::String(s)) => s.eq_ignore_ascii_case("true") || s == "1",
        _ => false,
    }
}

fn text(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn opt_text(value: Option<&Value>) -> Option<String> {
    let t = text(value);
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

async fn fetch_json(
    client: &reqwest::Client,
    source: &str,
    url: &str,
) -> Result<(String, Value), String> {
    let mut last_error = String::new();
    for attempt in 1..=3 {
        let response = client
            .get(url)
            .timeout(Duration::from_secs(15))
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::USER_AGENT, "Mozilla/5.0 OpenHub/1.0")
            .send()
            .await;

        match response {
            Ok(resp) => {
                if let Err(e) = resp.error_for_status_ref() {
                    last_error = format!("{source} HTTP 状态错误：{e}");
                } else {
                    match resp.text().await {
                        Ok(raw) => match serde_json::from_str::<Value>(&raw) {
                            Ok(parsed) => return Ok((raw, parsed)),
                            Err(e) => last_error = format!("{source} JSON 解析失败：{e}"),
                        },
                        Err(e) => last_error = format!("{source} 读取失败：{e}"),
                    }
                }
            }
            Err(e) => last_error = format!("{source} 下载失败：{e}"),
        }
        if attempt < 3 {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    Err(last_error)
}

fn ensure_catalog_schema(connection: &mut rusqlite::Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS app_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS model_catalog_sources (
                source TEXT PRIMARY KEY,
                url TEXT NOT NULL,
                fetched_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                record_count INTEGER NOT NULL DEFAULT 0,
                raw_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS model_catalog_providers (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL DEFAULT '',
                npm TEXT,
                api TEXT,
                doc TEXT,
                tier TEXT,
                subscription INTEGER NOT NULL DEFAULT 0,
                model_count INTEGER NOT NULL DEFAULT 0,
                date_modified TEXT,
                raw_json TEXT NOT NULL DEFAULT '{}'
            );
            CREATE TABLE IF NOT EXISTS model_catalog_models (
                id TEXT PRIMARY KEY,
                slug TEXT NOT NULL DEFAULT '',
                name TEXT NOT NULL DEFAULT '',
                lab TEXT NOT NULL DEFAULT '',
                kind TEXT NOT NULL DEFAULT '',
                family TEXT,
                knowledge TEXT,
                status TEXT NOT NULL DEFAULT 'ga',
                open_weights INTEGER NOT NULL DEFAULT 0,
                reasoning INTEGER NOT NULL DEFAULT 0,
                tool_call INTEGER NOT NULL DEFAULT 0,
                attachment INTEGER NOT NULL DEFAULT 0,
                structured INTEGER NOT NULL DEFAULT 0,
                temperature INTEGER NOT NULL DEFAULT 0,
                input_modalities_json TEXT NOT NULL DEFAULT '[]',
                context_length INTEGER NOT NULL DEFAULT 0,
                context_min INTEGER NOT NULL DEFAULT 0,
                context_max INTEGER NOT NULL DEFAULT 0,
                max_output_tokens INTEGER NOT NULL DEFAULT 0,
                ref_provider TEXT,
                ref_official INTEGER NOT NULL DEFAULT 0,
                ref_input_cost REAL NOT NULL DEFAULT 0,
                ref_output_cost REAL NOT NULL DEFAULT 0,
                ref_cache_read_cost REAL NOT NULL DEFAULT 0,
                min_provider TEXT,
                min_input_cost REAL NOT NULL DEFAULT 0,
                min_output_cost REAL NOT NULL DEFAULT 0,
                min_cache_read_cost REAL NOT NULL DEFAULT 0,
                price_spread REAL NOT NULL DEFAULT 0,
                blended_min REAL,
                blended_trusted REAL,
                blended_ref REAL,
                host_count INTEGER NOT NULL DEFAULT 0,
                priced_host_count INTEGER NOT NULL DEFAULT 0,
                free_host_count INTEGER NOT NULL DEFAULT 0,
                sub_host_count INTEGER NOT NULL DEFAULT 0,
                host_providers_json TEXT NOT NULL DEFAULT '[]',
                aa_idx REAL,
                aa_coding REAL,
                aa_agentic REAL,
                aa_speed REAL,
                aa_ttft REAL,
                aa_task_cost REAL,
                aa_json TEXT,
                benchmark_count INTEGER NOT NULL DEFAULT 0,
                release_date TEXT,
                last_updated TEXT,
                raw_json TEXT NOT NULL DEFAULT '{}',
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE INDEX IF NOT EXISTS idx_model_catalog_models_lab ON model_catalog_models(lab);
            CREATE INDEX IF NOT EXISTS idx_model_catalog_models_kind ON model_catalog_models(kind);
            CREATE INDEX IF NOT EXISTS idx_model_catalog_models_status ON model_catalog_models(status);
            CREATE INDEX IF NOT EXISTS idx_model_catalog_providers_name ON model_catalog_providers(name);",
        )
        .map_err(|error| error.to_string())
}

fn clear_legacy_catalog_if_needed(connection: &mut rusqlite::Connection) -> Result<(), String> {
    connection
        .execute(
            "CREATE TABLE IF NOT EXISTS app_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            [],
        )
        .map_err(|error| error.to_string())?;

    let schema_version = connection
        .query_row(
            "SELECT value FROM app_meta WHERE key = ?1",
            [CATALOG_SCHEMA_META_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| error.to_string())?
        .unwrap_or_default();

    if schema_version != CATALOG_SCHEMA_VERSION {
        let transaction = connection
            .transaction()
            .map_err(|error| error.to_string())?;
        let _ = transaction.execute("DROP TABLE IF EXISTS model_catalog_entries", []);
        let _ = transaction.execute("DROP TABLE IF EXISTS model_catalog_models", []);
        let _ = transaction.execute("DROP TABLE IF EXISTS model_catalog_providers", []);
        let _ = transaction.execute("DROP TABLE IF EXISTS model_catalog_sources", []);

        transaction
            .execute(
                "INSERT INTO app_meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![CATALOG_SCHEMA_META_KEY, CATALOG_SCHEMA_VERSION],
            )
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())?;
    }

    ensure_catalog_schema(connection)
}

fn is_synced_today(database: &Database) -> Result<bool, String> {
    let mut connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    clear_legacy_catalog_if_needed(&mut connection)?;
    let count = connection
        .query_row(
            "SELECT COUNT(*) FROM model_catalog_sources
             WHERE source = 'llmpricing_manifest'
               AND date(fetched_at, 'localtime') = date('now', 'localtime')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| error.to_string())?;
    Ok(count > 0)
}

fn parse_model_item_from_json(row: &Value) -> Option<ModelCatalogItem> {
    let obj = row.as_object()?;
    let id = text(obj.get("id"));
    if id.is_empty() {
        return None;
    }
    let slug = text(obj.get("slug"));
    let name = text(obj.get("name"));
    let lab = text(obj.get("lab"));
    let kind = text(obj.get("kind"));
    let family = opt_text(obj.get("family"));
    let knowledge = opt_text(obj.get("knowledge"));
    let status_str = text(obj.get("status"));
    let status = if status_str.is_empty() {
        "ga".to_string()
    } else {
        status_str
    };

    let open_weights = boolean(obj.get("openWeights"));
    let reasoning = boolean(obj.get("reasoning"));
    let tool_call = boolean(obj.get("toolCall"));
    let attachment = boolean(obj.get("attachment"));
    let structured = boolean(obj.get("structured"));
    let temperature = boolean(obj.get("temperature"));
    let input_modalities = string_array(obj.get("inputModalities"));

    let context_length = integer(obj.get("context"));
    let (context_min, context_max) = if let Some(arr) = obj.get("contextRange").and_then(Value::as_array) {
        if arr.len() >= 2 {
            (integer(arr.first()), integer(arr.get(1)))
        } else {
            (context_length, context_length)
        }
    } else {
        (context_length, context_length)
    };
    let max_output_tokens = integer(obj.get("outputLimit"));

    let ref_obj = obj.get("ref").and_then(Value::as_object);
    let ref_provider = ref_obj.and_then(|r| r.get("provider")).and_then(Value::as_str).map(str::to_string);
    let ref_official = boolean(obj.get("refOfficial"));
    let ref_input_cost = numeric(ref_obj.and_then(|r| r.get("input")));
    let ref_output_cost = numeric(ref_obj.and_then(|r| r.get("output")));
    let ref_cache_read_cost = numeric(ref_obj.and_then(|r| r.get("cacheRead")));

    let min_obj = obj.get("min").and_then(Value::as_object);
    let min_provider = min_obj.and_then(|r| r.get("provider")).and_then(Value::as_str).map(str::to_string);
    let min_input_cost = numeric(min_obj.and_then(|r| r.get("input")));
    let min_output_cost = numeric(min_obj.and_then(|r| r.get("output")));
    let min_cache_read_cost = numeric(min_obj.and_then(|r| r.get("cacheRead")));
    let price_spread = numeric(obj.get("spread"));

    let blended_min = opt_numeric(obj.get("blendedMin"));
    let blended_trusted = opt_numeric(obj.get("blendedTrusted"));
    let blended_ref = opt_numeric(obj.get("blendedRef"));

    let host_count = integer(obj.get("hostCount")) as usize;
    let priced_host_count = integer(obj.get("pricedHostCount")) as usize;
    let free_host_count = integer(obj.get("freeHostCount")) as usize;
    let sub_host_count = integer(obj.get("subHostCount")) as usize;
    let host_providers = string_array(obj.get("hostProviders"));

    let aa_obj = obj.get("aa").and_then(Value::as_object);
    let aa_idx = opt_numeric(aa_obj.and_then(|a| a.get("idx")));
    let aa_coding = opt_numeric(aa_obj.and_then(|a| a.get("coding")));
    let aa_agentic = opt_numeric(aa_obj.and_then(|a| a.get("agentic")));
    let aa_speed = opt_numeric(aa_obj.and_then(|a| a.get("speed")));
    let aa_ttft = opt_numeric(aa_obj.and_then(|a| a.get("ttft")));
    let aa_task_cost = opt_numeric(aa_obj.and_then(|a| a.get("taskCost")));
    let benchmark_count = integer(obj.get("benchmarkCount")) as usize;

    let release_date = opt_text(obj.get("releaseDate"));
    let last_updated = opt_text(obj.get("lastUpdated"));

    Some(ModelCatalogItem {
        id,
        slug,
        name,
        lab,
        kind,
        family,
        knowledge,
        status,
        open_weights,
        reasoning,
        tool_call,
        attachment,
        structured,
        temperature,
        input_modalities,
        context_length,
        context_min,
        context_max,
        max_output_tokens,
        ref_provider,
        ref_official,
        ref_input_cost,
        ref_output_cost,
        ref_cache_read_cost,
        min_provider,
        min_input_cost,
        min_output_cost,
        min_cache_read_cost,
        price_spread,
        blended_min,
        blended_trusted,
        blended_ref,
        host_count,
        priced_host_count,
        free_host_count,
        sub_host_count,
        host_providers,
        aa_idx,
        aa_coding,
        aa_agentic,
        aa_speed,
        aa_ttft,
        aa_task_cost,
        benchmark_count,
        release_date,
        last_updated,
    })
}

fn read_model_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ModelCatalogItem> {
    let input_modalities_json: String = row.get(14)?;
    let host_providers_json: String = row.get(33)?;

    Ok(ModelCatalogItem {
        id: row.get(0)?,
        slug: row.get(1)?,
        name: row.get(2)?,
        lab: row.get(3)?,
        kind: row.get(4)?,
        family: row.get(5)?,
        knowledge: row.get(6)?,
        status: row.get(7)?,
        open_weights: row.get::<_, i64>(8)? != 0,
        reasoning: row.get::<_, i64>(9)? != 0,
        tool_call: row.get::<_, i64>(10)? != 0,
        attachment: row.get::<_, i64>(11)? != 0,
        structured: row.get::<_, i64>(12)? != 0,
        temperature: row.get::<_, i64>(13)? != 0,
        input_modalities: serde_json::from_str(&input_modalities_json).unwrap_or_default(),
        context_length: row.get(15)?,
        context_min: row.get(16)?,
        context_max: row.get(17)?,
        max_output_tokens: row.get(18)?,
        ref_provider: row.get(19)?,
        ref_official: row.get::<_, i64>(20)? != 0,
        ref_input_cost: row.get(21)?,
        ref_output_cost: row.get(22)?,
        ref_cache_read_cost: row.get(23)?,
        min_provider: row.get(24)?,
        min_input_cost: row.get(25)?,
        min_output_cost: row.get(26)?,
        min_cache_read_cost: row.get(27)?,
        price_spread: row.get(28)?,
        blended_min: row.get(29)?,
        blended_trusted: row.get(30)?,
        blended_ref: row.get(31)?,
        host_count: row.get::<_, i64>(32)?.max(0) as usize,
        priced_host_count: row.get::<_, i64>(34)?.max(0) as usize,
        free_host_count: row.get::<_, i64>(35)?.max(0) as usize,
        sub_host_count: row.get::<_, i64>(36)?.max(0) as usize,
        host_providers: serde_json::from_str(&host_providers_json).unwrap_or_default(),
        aa_idx: row.get(37)?,
        aa_coding: row.get(38)?,
        aa_agentic: row.get(39)?,
        aa_speed: row.get(40)?,
        aa_ttft: row.get(41)?,
        aa_task_cost: row.get(42)?,
        benchmark_count: row.get::<_, i64>(43)?.max(0) as usize,
        release_date: row.get(44)?,
        last_updated: row.get(45)?,
    })
}

fn read_provider_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ModelCatalogProvider> {
    Ok(ModelCatalogProvider {
        id: row.get(0)?,
        name: row.get(1)?,
        npm: row.get(2)?,
        api: row.get(3)?,
        doc: row.get(4)?,
        tier: row.get(5)?,
        subscription: row.get::<_, i64>(6)? != 0,
        count: row.get::<_, i64>(7)?.max(0) as usize,
        date_modified: row.get(8)?,
    })
}

fn persist_catalog_llmpricing(
    database: &Database,
    manifest_raw: &str,
    manifest: &Value,
    shards_data: &[(String, String, Value)],
) -> Result<CatalogSyncReport, String> {
    let mut connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    ensure_catalog_schema(&mut connection)?;

    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;

    let fetched_at: String = transaction
        .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |row| {
            row.get(0)
        })
        .map_err(|error| error.to_string())?;

    transaction
        .execute("DELETE FROM model_catalog_models", [])
        .map_err(|error| error.to_string())?;
    transaction
        .execute("DELETE FROM model_catalog_providers", [])
        .map_err(|error| error.to_string())?;
    transaction
        .execute("DELETE FROM model_catalog_sources", [])
        .map_err(|error| error.to_string())?;

    let providers_map = manifest
        .get("providers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let mut provider_count = 0;
    for (p_id, p_val) in &providers_map {
        let p_name = text(p_val.get("name"));
        let p_npm = opt_text(p_val.get("npm"));
        let p_api = opt_text(p_val.get("api"));
        let p_doc = opt_text(p_val.get("doc"));
        let p_tier = opt_text(p_val.get("tier"));
        let p_sub = if boolean(p_val.get("subscription")) { 1 } else { 0 };
        let p_count = integer(p_val.get("count"));
        let p_date = opt_text(p_val.get("dateModified"));

        transaction
            .execute(
                "INSERT INTO model_catalog_providers (
                    id, name, npm, api, doc, tier, subscription, model_count, date_modified, raw_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    p_id,
                    p_name,
                    p_npm,
                    p_api,
                    p_doc,
                    p_tier,
                    p_sub,
                    p_count,
                    p_date,
                    p_val.to_string(),
                ],
            )
            .map_err(|error| error.to_string())?;
        provider_count += 1;
    }

    transaction
        .execute(
            "INSERT INTO model_catalog_sources (source, url, fetched_at, record_count, raw_json)
             VALUES ('llmpricing_manifest', ?1, ?2, ?3, ?4)",
            params![
                LLMPRICING_MANIFEST_URL,
                fetched_at,
                provider_count as i64,
                manifest_raw
            ],
        )
        .map_err(|error| error.to_string())?;

    let mut model_count = 0;
    let mut deduplicated_models: BTreeMap<String, (ModelCatalogItem, String)> = BTreeMap::new();

    for (shard_name, shard_raw, shard_json) in shards_data {
        let shard_items = shard_json.as_array().cloned().unwrap_or_default();
        let shard_len = shard_items.len();

        for item_val in shard_items {
            if let Some(item) = parse_model_item_from_json(&item_val) {
                deduplicated_models.insert(item.id.clone(), (item, item_val.to_string()));
            }
        }

        let shard_url = format!("{LLMPRICING_BASE_URL}/{shard_name}");
        transaction
            .execute(
                "INSERT INTO model_catalog_sources (source, url, fetched_at, record_count, raw_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    shard_name,
                    shard_url,
                    fetched_at,
                    shard_len as i64,
                    shard_raw
                ],
            )
            .map_err(|error| error.to_string())?;
    }

    for (item, raw_str) in deduplicated_models.values() {
        let aa_json = if item.aa_idx.is_some() || item.aa_coding.is_some() || item.aa_speed.is_some() {
            Some(json!({
                "idx": item.aa_idx,
                "coding": item.aa_coding,
                "agentic": item.aa_agentic,
                "speed": item.aa_speed,
                "ttft": item.aa_ttft,
                "taskCost": item.aa_task_cost,
            }).to_string())
        } else {
            None
        };

        transaction
            .execute(
                "INSERT INTO model_catalog_models (
                    id, slug, name, lab, kind, family, knowledge, status,
                    open_weights, reasoning, tool_call, attachment, structured, temperature,
                    input_modalities_json, context_length, context_min, context_max, max_output_tokens,
                    ref_provider, ref_official, ref_input_cost, ref_output_cost, ref_cache_read_cost,
                    min_provider, min_input_cost, min_output_cost, min_cache_read_cost, price_spread,
                    blended_min, blended_trusted, blended_ref,
                    host_count, priced_host_count, free_host_count, sub_host_count, host_providers_json,
                    aa_idx, aa_coding, aa_agentic, aa_speed, aa_ttft, aa_task_cost, aa_json, benchmark_count,
                    release_date, last_updated, raw_json, updated_at
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                    ?9, ?10, ?11, ?12, ?13, ?14,
                    ?15, ?16, ?17, ?18, ?19,
                    ?20, ?21, ?22, ?23, ?24,
                    ?25, ?26, ?27, ?28, ?29,
                    ?30, ?31, ?32,
                    ?33, ?34, ?35, ?36, ?37,
                    ?38, ?39, ?40, ?41, ?42, ?43, ?44, ?45,
                    ?46, ?47, ?48, ?49
                 )",
                params![
                    item.id,
                    item.slug,
                    item.name,
                    item.lab,
                    item.kind,
                    item.family,
                    item.knowledge,
                    item.status,
                    if item.open_weights { 1 } else { 0 },
                    if item.reasoning { 1 } else { 0 },
                    if item.tool_call { 1 } else { 0 },
                    if item.attachment { 1 } else { 0 },
                    if item.structured { 1 } else { 0 },
                    if item.temperature { 1 } else { 0 },
                    serde_json::to_string(&item.input_modalities).unwrap_or_else(|_| "[]".into()),
                    item.context_length,
                    item.context_min,
                    item.context_max,
                    item.max_output_tokens,
                    item.ref_provider,
                    if item.ref_official { 1 } else { 0 },
                    item.ref_input_cost,
                    item.ref_output_cost,
                    item.ref_cache_read_cost,
                    item.min_provider,
                    item.min_input_cost,
                    item.min_output_cost,
                    item.min_cache_read_cost,
                    item.price_spread,
                    item.blended_min,
                    item.blended_trusted,
                    item.blended_ref,
                    item.host_count as i64,
                    item.priced_host_count as i64,
                    item.free_host_count as i64,
                    item.sub_host_count as i64,
                    serde_json::to_string(&item.host_providers).unwrap_or_else(|_| "[]".into()),
                    item.aa_idx,
                    item.aa_coding,
                    item.aa_agentic,
                    item.aa_speed,
                    item.aa_ttft,
                    item.aa_task_cost,
                    aa_json,
                    item.benchmark_count as i64,
                    item.release_date,
                    item.last_updated,
                    raw_str,
                    fetched_at,
                ],
            )
            .map_err(|error| error.to_string())?;
        model_count += 1;
    }

    transaction
        .execute(
            "INSERT INTO app_meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![CATALOG_SCHEMA_META_KEY, CATALOG_SCHEMA_VERSION],
        )
        .map_err(|error| error.to_string())?;

    transaction.commit().map_err(|error| error.to_string())?;

    Ok(CatalogSyncReport {
        provider_count,
        model_count,
        shard_count: shards_data.len(),
    })
}

#[tauri::command]
pub fn get_model_catalog(database: State<'_, Database>) -> Result<ModelCatalogSnapshot, String> {
    get_model_catalog_inner(&database)
}

pub(crate) fn get_model_catalog_inner(database: &Database) -> Result<ModelCatalogSnapshot, String> {
    let mut connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    clear_legacy_catalog_if_needed(&mut connection)?;

    let mut statement = connection
        .prepare(
            "SELECT id, slug, name, lab, kind, family, knowledge, status,
                    open_weights, reasoning, tool_call, attachment, structured, temperature,
                    input_modalities_json, context_length, context_min, context_max, max_output_tokens,
                    ref_provider, ref_official, ref_input_cost, ref_output_cost, ref_cache_read_cost,
                    min_provider, min_input_cost, min_output_cost, min_cache_read_cost, price_spread,
                    blended_min, blended_trusted, blended_ref,
                    host_count, host_providers_json, priced_host_count, free_host_count, sub_host_count,
                    aa_idx, aa_coding, aa_agentic, aa_speed, aa_ttft, aa_task_cost, benchmark_count,
                    release_date, last_updated
             FROM model_catalog_models
             ORDER BY lab, name COLLATE NOCASE, id",
        )
        .map_err(|error| error.to_string())?;

    let models = statement
        .query_map([], read_model_row)
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;

    let mut provider_statement = connection
        .prepare(
            "SELECT id, name, npm, api, doc, tier, subscription, model_count, date_modified
             FROM model_catalog_providers
             ORDER BY name COLLATE NOCASE",
        )
        .map_err(|error| error.to_string())?;

    let providers = provider_statement
        .query_map([], read_provider_row)
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;

    let mut source_statement = connection
        .prepare(
            "SELECT source, url, fetched_at, record_count
             FROM model_catalog_sources
             ORDER BY source",
        )
        .map_err(|error| error.to_string())?;

    let sources = source_statement
        .query_map([], |row| {
            Ok(ModelCatalogSourceStatus {
                source: row.get(0)?,
                url: row.get(1)?,
                fetched_at: row.get(2)?,
                record_count: row.get::<_, i64>(3)?.max(0) as usize,
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;

    let last_synced_at = sources
        .iter()
        .map(|source| source.fetched_at.as_str())
        .max()
        .unwrap_or_default()
        .to_string();

    let synced_today = connection
        .query_row(
            "SELECT COUNT(*) FROM model_catalog_sources
             WHERE source = 'llmpricing_manifest'
               AND date(fetched_at, 'localtime') = date('now', 'localtime')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;

    let manifest_raw: Option<String> = connection
        .query_row(
            "SELECT raw_json FROM model_catalog_sources WHERE source = 'llmpricing_manifest'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;

    let meta = manifest_raw
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|val| val.get("meta").cloned())
        .unwrap_or(json!({}));

    Ok(ModelCatalogSnapshot {
        total: models.len(),
        models,
        providers,
        last_synced_at,
        synced_today,
        sources,
        meta,
    })
}

#[tauri::command]
pub fn get_model_catalog_detail(
    database: State<'_, Database>,
    canonical_key: Option<String>,
    id: Option<String>,
) -> Result<ModelCatalogDetail, String> {
    let key = id.or(canonical_key).unwrap_or_default();
    get_model_catalog_detail_inner(&database, &key)
}

pub(crate) fn get_model_catalog_detail_inner(
    database: &Database,
    key: &str,
) -> Result<ModelCatalogDetail, String> {
    let mut connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    clear_legacy_catalog_if_needed(&mut connection)?;

    let model_res = connection.query_row(
        "SELECT id, slug, name, lab, kind, family, knowledge, status,
                open_weights, reasoning, tool_call, attachment, structured, temperature,
                input_modalities_json, context_length, context_min, context_max, max_output_tokens,
                ref_provider, ref_official, ref_input_cost, ref_output_cost, ref_cache_read_cost,
                min_provider, min_input_cost, min_output_cost, min_cache_read_cost, price_spread,
                blended_min, blended_trusted, blended_ref,
                host_count, host_providers_json, priced_host_count, free_host_count, sub_host_count,
                aa_idx, aa_coding, aa_agentic, aa_speed, aa_ttft, aa_task_cost, benchmark_count,
                release_date, last_updated
         FROM model_catalog_models
         WHERE id = ?1 OR slug = ?1",
        [key],
        read_model_row,
    );

    let model = match model_res {
        Ok(m) => m,
        Err(_) => {
            // Try prefix/case-insensitive match
            let found = connection.query_row(
                "SELECT id, slug, name, lab, kind, family, knowledge, status,
                        open_weights, reasoning, tool_call, attachment, structured, temperature,
                        input_modalities_json, context_length, context_min, context_max, max_output_tokens,
                        ref_provider, ref_official, ref_input_cost, ref_output_cost, ref_cache_read_cost,
                        min_provider, min_input_cost, min_output_cost, min_cache_read_cost, price_spread,
                        blended_min, blended_trusted, blended_ref,
                        host_count, host_providers_json, priced_host_count, free_host_count, sub_host_count,
                        aa_idx, aa_coding, aa_agentic, aa_speed, aa_ttft, aa_task_cost, benchmark_count,
                        release_date, last_updated
                 FROM model_catalog_models
                 WHERE id LIKE ?1 COLLATE NOCASE OR slug LIKE ?1 COLLATE NOCASE
                 LIMIT 1",
                [format!("%{key}%")],
                read_model_row,
            ).optional().map_err(|e| e.to_string())?;

            found.ok_or_else(|| format!("未找到模型：{key}"))?
        }
    };

    let raw_text: String = connection
        .query_row(
            "SELECT raw_json FROM model_catalog_models WHERE id = ?1",
            [&model.id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?
        .unwrap_or_else(|| "{}".into());

    let raw: Value = serde_json::from_str(&raw_text).unwrap_or(Value::Null);

    // Query matched providers for host_providers
    let mut matched_providers = Vec::new();
    if !model.host_providers.is_empty() {
        let mut placeholders = Vec::new();
        let mut params_vec = Vec::new();
        for p in &model.host_providers {
            placeholders.push("?");
            params_vec.push(p.as_str());
        }
        let sql = format!(
            "SELECT id, name, npm, api, doc, tier, subscription, model_count, date_modified
             FROM model_catalog_providers
             WHERE id IN ({})
             ORDER BY tier, name COLLATE NOCASE",
            placeholders.join(",")
        );

        let mut p_stmt = connection.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = p_stmt
            .query_map(rusqlite::params_from_iter(params_vec), read_provider_row)
            .map_err(|e| e.to_string())?;

        for r in rows {
            if let Ok(p) = r {
                matched_providers.push(p);
            }
        }
    }

    Ok(ModelCatalogDetail {
        model,
        providers: matched_providers,
        raw,
    })
}

#[tauri::command]
pub async fn sync_model_catalog(
    app: AppHandle,
    database: State<'_, Database>,
    runtime: State<'_, ModelCatalogRuntime>,
    force: Option<bool>,
) -> Result<ModelCatalogSyncResult, String> {
    sync_model_catalog_inner(&app, &database, &runtime, force.unwrap_or(false)).await
}

pub(crate) async fn sync_model_catalog_inner(
    app: &AppHandle,
    database: &Database,
    runtime: &ModelCatalogRuntime,
    force: bool,
) -> Result<ModelCatalogSyncResult, String> {
    if !force && is_synced_today(database)? {
        return Ok(ModelCatalogSyncResult {
            synced: false,
            skipped: true,
            message: "今天已经同步过模型参数".into(),
            snapshot: get_model_catalog_inner(database)?,
        });
    }

    if runtime
        .syncing
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err("模型参数正在同步，请稍候".into());
    }

    let _guard = SyncGuard(&runtime.syncing);
    let _ = app.emit("model-catalog-sync-status", json!({ "status": "syncing" }));

    let client = build_http_client(database, Duration::from_secs(60), 5, "模型参数同步")?;
    
    // 1. Fetch manifest
    let (manifest_raw, manifest) = fetch_json(&client, "LLMPricing Manifest", LLMPRICING_MANIFEST_URL).await?;

    let shards_array = manifest
        .get("shards")
        .and_then(Value::as_array)
        .ok_or_else(|| "LLMPricing Manifest 缺少 shards 列表".to_string())?;

    let mut shards_data = Vec::with_capacity(shards_array.len());

    // 2. Fetch each shard
    for (idx, shard_val) in shards_array.iter().enumerate() {
        let shard_name = shard_val.as_str().ok_or_else(|| "Shard 名称格式无效".to_string())?;
        let shard_url = format!("{LLMPRICING_BASE_URL}/{shard_name}");
        let progress_msg = format!("正在下载模型分片 {}/{} ({})", idx + 1, shards_array.len(), shard_name);
        let _ = app.emit("model-catalog-sync-status", json!({ "status": "syncing", "message": progress_msg }));

        let (shard_raw, shard_json) = fetch_json(&client, shard_name, &shard_url).await?;
        shards_data.push((shard_name.to_string(), shard_raw, shard_json));
    }

    // 3. Persist
    let report = persist_catalog_llmpricing(database, &manifest_raw, &manifest, &shards_data)?;
    let snapshot = get_model_catalog_inner(database)?;

    let message = format!(
        "模型参数同步完成：LLMPricing 收录 {} 个供应商、{} 个模型（共 {} 个分片）",
        report.provider_count,
        report.model_count,
        report.shard_count,
    );

    let _ = app.emit(
        "model-catalog-sync-status",
        json!({ "status": "complete", "message": message }),
    );

    Ok(ModelCatalogSyncResult {
        synced: true,
        skipped: false,
        message,
        snapshot,
    })
}

/// 一次性同步入口（供 `cargo run --example sync_model_catalog` 使用）
pub fn sync_model_catalog_once(db_path: &str) -> Result<CatalogSyncReport, String> {
    let database = Database::open(std::path::Path::new(db_path))?;
    let runtime = ModelCatalogRuntime::new();
    let runtime_ref = &runtime;
    let database_ref = &database;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("创建异步运行时失败：{error}"))?;

    rt.block_on(async move { fetch_and_persist_once(database_ref, runtime_ref).await })
}

async fn fetch_and_persist_once(
    database: &Database,
    runtime: &ModelCatalogRuntime,
) -> Result<CatalogSyncReport, String> {
    if runtime
        .syncing
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err("模型参数正在同步，请稍候".into());
    }

    let _guard = SyncGuard(&runtime.syncing);
    let client = build_http_client(database, Duration::from_secs(60), 5, "模型参数同步")?;

    println!("正在获取 LLMPricing Manifest: {LLMPRICING_MANIFEST_URL} ...");
    let (manifest_raw, manifest) = fetch_json(&client, "LLMPricing Manifest", LLMPRICING_MANIFEST_URL).await?;

    let shards_array = manifest
        .get("shards")
        .and_then(Value::as_array)
        .ok_or_else(|| "LLMPricing Manifest 缺少 shards 列表".to_string())?;

    let mut shards_data = Vec::with_capacity(shards_array.len());
    for (idx, shard_val) in shards_array.iter().enumerate() {
        let shard_name = shard_val.as_str().ok_or_else(|| "Shard 名称格式无效".to_string())?;
        let shard_url = format!("{LLMPRICING_BASE_URL}/{shard_name}");
        println!("正在获取分片 [{}/{}]: {} ...", idx + 1, shards_array.len(), shard_name);
        let (shard_raw, shard_json) = fetch_json(&client, shard_name, &shard_url).await?;
        shards_data.push((shard_name.to_string(), shard_raw, shard_json));
    }

    println!("正在入库并更新本地模型参数缓存 ...");
    persist_catalog_llmpricing(database, &manifest_raw, &manifest, &shards_data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_llmpricing_sample_model() {
        let sample = json!({
            "id": "zhipuai/glm-5.2",
            "slug": "glm52",
            "name": "GLM-5.2",
            "lab": "zhipuai",
            "kind": "text",
            "family": "glm",
            "knowledge": null,
            "status": "ga",
            "openWeights": true,
            "reasoning": true,
            "toolCall": true,
            "attachment": false,
            "structured": true,
            "temperature": true,
            "inputModalities": ["text", "image"],
            "context": 1000000,
            "contextRange": [96000, 1049000],
            "outputLimit": 131072,
            "ref": {
                "provider": "zai",
                "input": 1.4,
                "output": 4.4,
                "cacheRead": 0.26
            },
            "refOfficial": true,
            "min": {
                "provider": "nano-gpt",
                "input": 0.42,
                "output": 1.32,
                "cacheRead": 0.078
            },
            "spread": 5.5,
            "hostCount": 80,
            "pricedHostCount": 69,
            "freeHostCount": 3,
            "subHostCount": 6,
            "hostProviders": ["umans-ai-coding-plan", "nvidia", "nano-gpt"],
            "aa": {
                "idx": 52.6,
                "coding": 68.8,
                "agentic": 45.7,
                "speed": 139.0,
                "ttft": 1.37,
                "taskCost": 0.3206,
                "variant": "max"
            },
            "blendedMin": 0.645,
            "blendedTrusted": 1.075,
            "blendedRef": 2.15,
            "releaseDate": "2026-06-13",
            "lastUpdated": "2026-06-13",
            "benchmarkCount": 19
        });

        let item = parse_model_item_from_json(&sample).expect("Should parse sample model");
        assert_eq!(item.id, "zhipuai/glm-5.2");
        assert_eq!(item.name, "GLM-5.2");
        assert_eq!(item.lab, "zhipuai");
        assert_eq!(item.kind, "text");
        assert!(item.open_weights);
        assert!(item.reasoning);
        assert!(item.tool_call);
        assert_eq!(item.context_length, 1_000_000);
        assert_eq!(item.context_min, 96_000);
        assert_eq!(item.context_max, 1_049_000);
        assert_eq!(item.max_output_tokens, 131_072);
        assert_eq!(item.ref_provider.as_deref(), Some("zai"));
        assert_eq!(item.ref_input_cost, 1.4);
        assert_eq!(item.ref_output_cost, 4.4);
        assert_eq!(item.min_provider.as_deref(), Some("nano-gpt"));
        assert_eq!(item.min_input_cost, 0.42);
        assert_eq!(item.price_spread, 5.5);
        assert_eq!(item.aa_idx, Some(52.6));
        assert_eq!(item.aa_speed, Some(139.0));
        assert_eq!(item.host_count, 80);
        assert_eq!(item.host_providers.len(), 3);
    }

    #[test]
    fn legacy_catalog_cache_is_cleared_on_version_upgrade() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE model_catalog_sources (source TEXT PRIMARY KEY);
                 CREATE TABLE model_catalog_models (canonical_key TEXT PRIMARY KEY);
                 CREATE TABLE model_catalog_entries (id INTEGER PRIMARY KEY);
                 INSERT INTO app_meta (key, value) VALUES ('model_catalog_schema_version', '7');
                 INSERT INTO model_catalog_sources (source) VALUES ('openrouter');
                 INSERT INTO model_catalog_models (canonical_key) VALUES ('openai/gpt-primary');
                 INSERT INTO model_catalog_entries (id) VALUES (1);",
            )
            .unwrap();

        clear_legacy_catalog_if_needed(&mut connection).unwrap();

        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM model_catalog_models", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "model_catalog_models should be cleared");

        let version: String = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key = ?1",
                [CATALOG_SCHEMA_META_KEY],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, CATALOG_SCHEMA_VERSION);
    }
}
