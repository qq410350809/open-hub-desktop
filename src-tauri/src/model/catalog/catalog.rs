use crate::context::{AppContext, Managed};
use crate::db::build_http_client;
use crate::model::catalog::channel_builder::{build_hosts, HostBuildInput, ProviderMeta};
use crate::model::catalog::lab_registry::{official_channels_for, resolve_identity};
use crate::model::catalog::models_dev::{
    normalize_model_id, ReasoningOption, ModelsDevIndex, MODELS_DEV_CATALOG_URL,
    MODELS_DEV_ETAG_META_KEY, MODELS_DEV_SOURCE,
};
use crate::model::catalog::nearest::{Candidate, Match, NearestIndex};
use crate::models::Database;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

pub const LLMPRICING_MANIFEST_URL: &str = "https://llmpricing.dev/rows/manifest.json";
pub const LLMPRICING_BASE_URL: &str = "https://llmpricing.dev/rows";
/// 目录表结构版本（人读语义）。
///
/// - `9`：llmpricing 单源，渠道明细靠 HTML 爬取（~1800 页）。
/// - `10`：引入 models.dev 作为主数据源，新增模型身份、官网/三方分层计数、免费渠道明细；
///   渠道明细改为组合推导，**移除 HTML 爬取**。
/// - `11`：补 `subscription_channel_providers_json` / `official_channel_providers_json`。
///   （这次是因为 `10` 发布后才加列、忘了 bump 导致线上报 no such column，故同时引入
///   DDL 指纹自动对账，见 [`catalog_schema_fingerprint`]。）
const CATALOG_SCHEMA_VERSION: &str = "11";
const CATALOG_SCHEMA_META_KEY: &str = "model_catalog_schema_version";
/// 存放 [`CATALOG_SCHEMA_DDL`] 的指纹，用于「只改 DDL 没 bump 版本号」时也能自动重建。
const CATALOG_SCHEMA_FINGERPRINT_META_KEY: &str = "model_catalog_schema_fingerprint";

pub struct ModelCatalogRuntime {
    syncing: AtomicBool,
}

impl ModelCatalogRuntime {
    pub fn new() -> Self {
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
    /// 该渠道是否为某个 lab 的自营（原厂）渠道。
    ///
    /// ⚠️ 与 `tier` 无关：`tier = lab` 只说明渠道自身是模型厂商/自营云，
    /// 不代表它是任意模型的原厂渠道。例如 `nvidia`（tier=lab）代售 30+ 个 lab 的模型，
    /// `azure`（tier=lab）对上架 microsoft 模型是原厂、对上架 openai 模型却不是。
    pub is_first_party: bool,
    /// 该渠道要求的 API Key 环境变量名（models.dev `providers[*].env`）。
    ///
    /// 例如 `["OPENAI_API_KEY"]`。配置渠道时直接可用，避免用户去翻文档。
    pub env: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelCatalogHostItem {
    pub provider: String,
    pub name: String,
    pub model_id: Option<String>,
    pub tier: Option<String>,
    pub subscription: bool,
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub context: Option<i64>,
    pub output_limit: Option<i64>,
    /// 独立输入上限（models.dev `limit.input`）。总上下文可能大于单次可输入量。
    pub input_limit: Option<i64>,
    /// 输出模态（models.dev `modalities.output`）。可能是 `image` / `video` / `audio`。
    pub output_modalities: Vec<String>,
    /// 该渠道声明的思考级别选项（`reasoning_options`）。
    ///
    /// 与模型级 [`ModelCatalogItem::reasoning_options`] 的区别：这里是**该渠道**的声明，
    /// 实测约 3~7% 的渠道与模型级能力声明不一致，故分开展示。
    pub reasoning_options: Vec<ReasoningOption>,
    /// 交错推理的读取字段名（如 `reasoning_content`）。
    pub interleaved_field: Option<String>,
    /// 快速模式（`experimental.modes.fast`），含额外计费与请求覆盖。
    pub fast_mode: Option<Value>,
    pub status: Option<String>,
    pub official: bool,
    pub doc: Option<String>,
    pub is_free: bool,
    pub is_min: bool,
    pub is_ref: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelCatalogSourceStatus {
    pub source: String,
    pub url: String,
    pub fetched_at: String,
    pub record_count: usize,
}

/// models.dev **canonical 层**的附加信息。
///
/// 这些字段只在该模型的详情页有用，且大多只覆盖少数模型（license 44/395、
/// links 18/395、benchmarks 135/395），因此打包成一个 JSON 列存储，
/// 不再为每个字段单独加列。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelsDevModelExtras {
    /// 模型描述（canonical 层 `description`）。
    #[serde(default)]
    pub description: Option<String>,
    /// 开源许可证，如 `Apache 2.0` / `MIT` / `Llama 3.2 Community License`。
    #[serde(default)]
    pub license: Option<String>,
    /// 官方链接：`[{label, url, type}]`（type 如 `paper` / `model_card` / `announcement`）。
    #[serde(default)]
    pub links: Vec<Value>,
    /// 权重下载：`[{label, url, quantization?}]`。
    #[serde(default)]
    pub weights: Vec<Value>,
    /// 基准测试明细：`[{name, score, metric, source?, date?, harness?}]`。
    #[serde(default)]
    pub benchmarks: Vec<Value>,
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

    // ───────── 模型身份：原始 lab / 原始 modelId ─────────
    /// 归一化后的原始 lab。取自 models.dev canonical 层，或 llmpricing 的 `lab`，
    /// 或关键词/同名继承推断。**未确定时保持 `misc`**，绝不臆造。
    pub official_lab: String,
    /// 原始 modelId（**保留上游原拼写**，如 `z-ai/glm-5.2`、`MiniMax-M3`）。
    pub official_model_id: String,
    /// models.dev canonical id（如 `zhipuai/glm-5.2`），未定位到时为 None。
    pub canonical_id: Option<String>,
    /// 身份来源：`canonical` / `llmpricing_lab` / `keyword_rule` / `inherited` /
    /// `canonical_reverse` / `unknown`。前两者为上游直接给出，其余为本地推断。
    pub identity_source: String,
    /// 身份是否已确定。`false` 表示 `official_lab` 仍是 `misc` 占位。
    pub identity_resolved: bool,

    // ───────── 官网 / 三方渠道分层 ─────────
    /// 官方（原厂）渠道数量。判定方式：`provider ∈ 该 lab 的原厂别名表` ∧ `该渠道确实上架此模型`。
    pub official_host_count: usize,
    /// 官方（原厂）渠道 id 列表（已去重、已排序）。
    ///
    /// 前端据此即可在不拉取详情的情况下做「仅官网渠道」筛选与价格对比。
    pub official_channel_providers: Vec<String>,
    /// `tier = lab` 的渠道数量。
    pub lab_tier_host_count: usize,
    /// `tier = cloud` 的渠道数量。
    pub cloud_tier_host_count: usize,
    /// `tier = gateway` 的渠道数量。
    pub gateway_tier_host_count: usize,

    // ───────── 免费渠道（本地推导，替代 HTML 爬取）─────────
    /// 本地推导的免费渠道数量（已剔除订阅制渠道）。
    ///
    /// 判定：`input == 0 ∧ output == 0 ∧ !subscription`，其中「零价」取
    /// **任一变体零价即算**（同一渠道可能同时登记付费与 `:free` 两个条目）。
    pub free_channel_count: usize,
    /// 本地推导的免费渠道 id 列表（已去重）。
    pub free_channel_providers: Vec<String>,
    /// 订阅制渠道 id 列表（已去重）。
    ///
    /// 订阅渠道「用时不另计费」，与免费渠道语义不同：它们靠包月/包量计费，
    /// 因此**不计入** `free_channel_count`，单列出来供 UI 区分。
    pub subscription_channel_providers: Vec<String>,
    /// 免费渠道数据来源：`derived`（本地推导）/ `upstream`（仅上游聚合值，无明细）。
    pub free_channel_source: String,
    /// 本地推导的免费渠道数是否与 llmpricing 上游聚合值 `freeHostCount` 一致。
    ///
    /// 实测一致率 97.4%；不一致时以 `free_host_count`（上游值）为准，
    /// 本字段供 UI 提示数据口径差异。
    pub free_channel_count_matches: bool,

    // ───────── 思考级别与输出能力（models.dev 渠道层聚合）─────────
    /// **模型级思考级别选项**（跨全部渠道取并集）。
    ///
    /// 两种形态：`{kind:"toggle"}`（只能开/关）与
    /// `{kind:"effort", values:["none","low","medium","high","xhigh","max"]}`（多档位）。
    /// 单渠道可能只声明部分档位，模型级反映「这个模型**能**调到多高」。
    pub reasoning_options: Vec<ReasoningOption>,
    /// 模型支持的最高思考档位，便于排序/筛选；无 effort 档位时为 None。
    pub reasoning_effort_max: Option<String>,
    /// 模型级输出模态（跨渠道取并集）。可能是 `image` / `video` / `audio`，
    /// 与 `kind`（llmpricing 的分类）互补。
    pub output_modalities: Vec<String>,
    /// 模型级最大输入上限（跨渠道取最大值）。总上下文可能大于单次可输入量。
    pub max_input_tokens: Option<i64>,
    /// 支持交错推理的读取字段名（跨渠道去重，如 `reasoning_content`）。
    pub interleaved_fields: Vec<String>,
    /// 是否有渠道提供「快速模式」（`experimental.modes.fast`，额外计费档）。
    pub has_fast_mode: bool,

    // ───────── models.dev canonical 层附加信息 ─────────
    /// 描述 / 许可证 / 官方链接 / 权重下载 / 基准测试明细。
    pub models_dev_extras: ModelsDevModelExtras,
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
    pub hosts: Vec<ModelCatalogHostItem>,
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

/// 确保 `model_catalog_sources` 中存在 models.dev 来源行。
///
/// 只在缺失时插入，**不刷新 `fetched_at`**——该字段语义是「最后一次成功联网抓取的时间」，
/// 走缓存 / 304 路径时不该被改写。
fn ensure_models_dev_source_row(
    database: &Database,
    index: &ModelsDevIndex,
    raw: &str,
) -> Result<(), String> {
    let connection = database.lock_conn()?;
    connection
        .execute(
            "INSERT OR IGNORE INTO model_catalog_sources
                (source, url, fetched_at, record_count, raw_json)
             VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'), ?3, ?4)",
            params![
                MODELS_DEV_SOURCE,
                MODELS_DEV_CATALOG_URL,
                index.canonical.len() as i64,
                raw,
            ],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// 决定是否发送条件请求（`If-None-Match`）。
///
/// **只有同时具备 ETag 与本地正文时才可以发**：服务端一旦回 304，我们就必须靠本地
/// 正文重建索引；只带 ETag 不带正文等于自断后路。
///
/// 之所以单独抽出来，是因为「ETag 在 `app_meta`、正文在 `model_catalog_sources`」——
/// 两张表的生命周期不同（schema 变更会 DROP 后者），很容易出现孤儿 ETag。
fn conditional_etag_for(cached_etag: &str, has_cached_raw: bool) -> Option<&str> {
    let etag = cached_etag.trim();
    if has_cached_raw && !etag.is_empty() {
        Some(etag)
    } else {
        None
    }
}

/// 抓取 models.dev catalog 并构建索引（带 ETag 条件请求）。
///
/// 降级策略（从可靠到兜底）：
/// 1. 条件请求返回 `304` → 用本地缓存的原文重建索引，省下约 4.9MB 流量；
/// 2. 返回 `200` → 解析并把原文与 ETag 落库；
/// 3. 网络失败（已重试 3 次）→ 回退到本地缓存原文；
/// 4. 本地也无缓存 → 返回错误。
///
/// **不会**静默返回空索引——身份与免费渠道判定都依赖它，静默降级会让数据看起来正常但实际失真。
async fn fetch_models_dev_index(
    database: &Database,
    client: &reqwest::Client,
) -> Result<ModelsDevIndex, String> {
    use crate::model::catalog::models_dev::{
        build_index, fetch_catalog_raw, ModelsDevFetchOutcome,
    };

    // ⚠️ 必须先让表结构定版再读写。
    // `persist_catalog_llmpricing` 内部也会调 `clear_legacy_catalog_if_needed`，而首次同步时
    // 它会把整张 `model_catalog_sources` DROP 重建——若本函数在此之前写入来源行，
    // 那一行会被随后的 DROP 抹掉（models.dev 来源行曾经就是这样消失的）。
    // 在本函数入口先对账一次，后续 persist 的调用就会看到版本与指纹都匹配、不再重建。
    {
        let mut connection = database.lock_conn()?;
        clear_legacy_catalog_if_needed(&mut connection)?;
    }

    let (cached_etag, cached_raw) = {
        let connection = database.lock_conn()?;
        let etag = crate::db::read_meta_conn(&connection, MODELS_DEV_ETAG_META_KEY)
            .unwrap_or_default();
        let raw = connection
            .query_row(
                "SELECT raw_json FROM model_catalog_sources WHERE source = ?1",
                [MODELS_DEV_SOURCE],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        (etag, raw)
    };

    // ⚠️ ETag 与正文必须**成对**使用，否则必然踩坑：
    // ETag 存在 `app_meta`（表结构重建时会保留），正文存在 `model_catalog_sources`
    // （schema 变更时整表被 DROP）。若只带 ETag 而不带正文去发条件请求，
    // 服务端会回 304「未变更」，而我们手里没有任何东西能重建索引 ——
    // 于是报「返回 304 未变更，且本地无缓存可用」。
    // 结论：**没有正文时绝不发条件请求**。
    let conditional_etag = conditional_etag_for(&cached_etag, cached_raw.is_some());
    let mut outcome = fetch_catalog_raw(client, conditional_etag).await;

    // 兜底：万一仍然收到 304 却没有缓存（例如 ETag 与正文来自不同代、或缓存刚被清掉），
    // 去掉条件头完整重抓一次，而不是直接把同步判失败。
    if cached_raw.is_none()
        && matches!(
            &outcome,
            Ok(ModelsDevFetchOutcome {
                not_modified: true,
                ..
            })
        )
    {
        tracing::warn!(
            target: "openhub::catalog",
            "models.dev 返回 304 但本地无缓存正文，改为无条件重新抓取"
        );
        outcome = fetch_catalog_raw(client, None).await;
    }

    let build_from_cache = |reason: &str| -> Result<ModelsDevIndex, String> {
        let raw = cached_raw
            .as_deref()
            .ok_or_else(|| format!("models.dev catalog {reason}，且本地无缓存可用"))?;
        let value: Value = serde_json::from_str(raw)
            .map_err(|error| format!("models.dev 本地缓存解析失败：{error}"))?;
        let index = build_index(&value)
            .map_err(|error| format!("models.dev 本地缓存建索引失败：{error}"))?;
        // 走缓存路径时来源行可能缺失（例如库是引入 models.dev 之前建的），补一次。
        // `fetched_at` 保持原值不刷新——它记录的是**最后一次成功联网抓取**的时间。
        ensure_models_dev_source_row(database, &index, raw)?;
        Ok(index)
    };

    match outcome {
        Ok(ModelsDevFetchOutcome {
            not_modified: true, ..
        }) => {
            info!(target: "openhub::catalog", "models.dev catalog 未变更（304），复用本地缓存");
            build_from_cache("返回 304 未变更")
        }
        Ok(ModelsDevFetchOutcome {
            raw: Some(raw),
            etag,
            ..
        }) => {
            let value: Value = serde_json::from_str(&raw)
                .map_err(|error| format!("models.dev catalog JSON 解析失败：{error}"))?;
            let index = build_index(&value)?;

            let connection = database.lock_conn()?;
            connection
                .execute(
                    "INSERT INTO model_catalog_sources (source, url, fetched_at, record_count, raw_json)
                     VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'), ?3, ?4)
                     ON CONFLICT(source) DO UPDATE SET
                        url = excluded.url,
                        fetched_at = excluded.fetched_at,
                        record_count = excluded.record_count,
                        raw_json = excluded.raw_json",
                    params![
                        MODELS_DEV_SOURCE,
                        MODELS_DEV_CATALOG_URL,
                        index.canonical.len() as i64,
                        raw,
                    ],
                )
                .map_err(|error| error.to_string())?;
            if let Some(tag) = etag {
                crate::db::write_meta(&connection, MODELS_DEV_ETAG_META_KEY, &tag)?;
            }

            let (canonical, providers, channel_entries, _) = index.summary();
            info!(
                target: "openhub::catalog",
                "models.dev catalog 已更新：canonical {canonical} 个模型 / {providers} 个渠道 / {channel_entries} 条渠道记录"
            );
            Ok(index)
        }
        Ok(ModelsDevFetchOutcome { raw: None, .. }) => build_from_cache("响应体为空"),
        Err(error) => {
            tracing::warn!(
                target: "openhub::catalog",
                "models.dev catalog 下载失败（{error}），回退本地缓存"
            );
            build_from_cache("下载失败")
        }
    }
}

/// `model_catalog_*` 表的**唯一权威 DDL**。
///
/// ⚠️ 改这里就够了：`clear_legacy_catalog_if_needed` 会对本字符串取指纹并落库，
/// 指纹变化即自动 DROP 重建，**不需要手工 bump `CATALOG_SCHEMA_VERSION`**。
///
/// （曾经因为「加了列但忘了 bump 版本号」导致线上报
/// `no such column: subscription_channel_providers_json`，故改为指纹自动对账。）
const CATALOG_SCHEMA_DDL: &str = "CREATE TABLE IF NOT EXISTS app_meta (
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
                is_first_party INTEGER NOT NULL DEFAULT 0,
                env_json TEXT NOT NULL DEFAULT '[]',
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
                official_lab TEXT NOT NULL DEFAULT 'misc',
                official_model_id TEXT NOT NULL DEFAULT '',
                canonical_id TEXT,
                identity_source TEXT NOT NULL DEFAULT 'unknown',
                identity_resolved INTEGER NOT NULL DEFAULT 0,
                official_host_count INTEGER NOT NULL DEFAULT 0,
                official_channel_providers_json TEXT NOT NULL DEFAULT '[]',
                lab_tier_host_count INTEGER NOT NULL DEFAULT 0,
                cloud_tier_host_count INTEGER NOT NULL DEFAULT 0,
                gateway_tier_host_count INTEGER NOT NULL DEFAULT 0,
                free_channel_count INTEGER NOT NULL DEFAULT 0,
                free_channel_providers_json TEXT NOT NULL DEFAULT '[]',
                subscription_channel_providers_json TEXT NOT NULL DEFAULT '[]',
                free_channel_source TEXT NOT NULL DEFAULT 'upstream',
                free_channel_count_matches INTEGER NOT NULL DEFAULT 0,
                reasoning_options_json TEXT NOT NULL DEFAULT '[]',
                reasoning_effort_max TEXT,
                output_modalities_json TEXT NOT NULL DEFAULT '[]',
                max_input_tokens INTEGER,
                interleaved_fields_json TEXT NOT NULL DEFAULT '[]',
                has_fast_mode INTEGER NOT NULL DEFAULT 0,
                models_dev_extras_json TEXT NOT NULL DEFAULT '{}',
                hosts_json TEXT,
                raw_json TEXT NOT NULL DEFAULT '{}',
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE INDEX IF NOT EXISTS idx_model_catalog_models_lab ON model_catalog_models(lab);
            CREATE INDEX IF NOT EXISTS idx_model_catalog_models_kind ON model_catalog_models(kind);
            CREATE INDEX IF NOT EXISTS idx_model_catalog_models_status ON model_catalog_models(status);
            CREATE INDEX IF NOT EXISTS idx_model_catalog_models_official_lab ON model_catalog_models(official_lab);
            CREATE INDEX IF NOT EXISTS idx_model_catalog_models_free_channel ON model_catalog_models(free_channel_count);
            CREATE INDEX IF NOT EXISTS idx_model_catalog_providers_name ON model_catalog_providers(name);";

/// 对 DDL 取 64 位 FNV-1a 指纹。
///
/// 只用于「DDL 是否变化」的判定，不做安全用途。FNV-1a 实现简单、无依赖、
/// 跨平台稳定（不依赖 `DefaultHasher` 的未指定算法）。
fn catalog_schema_fingerprint() -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for byte in CATALOG_SCHEMA_DDL.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

fn ensure_catalog_schema(connection: &mut rusqlite::Connection) -> Result<(), String> {
    connection
        .execute_batch(CATALOG_SCHEMA_DDL)
        .map_err(|error| error.to_string())
}

fn clear_legacy_catalog_if_needed(connection: &mut rusqlite::Connection) -> Result<(), String> {
    connection
        .execute(
            "CREATE TABLE IF NOT EXISTS app_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            [],
        )
        .map_err(|error| error.to_string())?;

    let schema_version = crate::db::read_meta_conn(&connection, CATALOG_SCHEMA_META_KEY)?;
    let stored_fingerprint =
        crate::db::read_meta_conn(&connection, CATALOG_SCHEMA_FINGERPRINT_META_KEY)?;
    let current_fingerprint = catalog_schema_fingerprint();

    // 版本号对账「人读」的语义变更，指纹对账「DDL 实际形态」。
    // 两者任一不一致都重建——**只改 DDL 忘了 bump 版本号也不会漏**。
    let version_changed = schema_version != CATALOG_SCHEMA_VERSION;
    let ddl_changed = stored_fingerprint != current_fingerprint;

    if version_changed || ddl_changed {
        // foreign_keys 开启时 DROP 父表会先隐式清空引用它的子表，若残留子表
        // 带有无法解析的旧外键（如 canonical_key），清理本身就会失败；而
        // PRAGMA 在事务内是空操作，因此必须在事务外临时关闭外键完成清理。
        let _ = connection.execute_batch(
            "PRAGMA foreign_keys = OFF;
             DROP TABLE IF EXISTS model_catalog_entries;
             DROP TABLE IF EXISTS model_catalog_models;
             DROP TABLE IF EXISTS model_catalog_providers;
             DROP TABLE IF EXISTS model_catalog_sources;
             PRAGMA foreign_keys = ON;",
        );

        if ddl_changed {
            info!(
                target: "openhub::catalog",
                "模型目录表结构已变化（指纹 {} -> {}），重建缓存表",
                if stored_fingerprint.is_empty() {
                    "<空>".to_string()
                } else {
                    stored_fingerprint.clone()
                },
                current_fingerprint,
            );
        }

        crate::db::write_meta(connection, CATALOG_SCHEMA_META_KEY, CATALOG_SCHEMA_VERSION)?;
        crate::db::write_meta(
            connection,
            CATALOG_SCHEMA_FINGERPRINT_META_KEY,
            &current_fingerprint,
        )?;

        // ⚠️ 正文（`model_catalog_sources.raw_json`）随表被 DROP，ETag 却留在 `app_meta`。
        // 两者必须同生共死：作废 ETag，否则下次条件请求会拿到 304 而无正文可用。
        crate::db::write_meta(connection, MODELS_DEV_ETAG_META_KEY, "")?;
    }

    ensure_catalog_schema(connection)
}

/// 判断今天的目录快照是否已经完整落库。
///
/// 除了 llmpricing 的 manifest，还要求 models.dev 的来源行存在——否则说明这次快照
/// 是在引入 models.dev 之前（或该行被误删）产生的，属于**不完整快照**，
/// 应当重新同步而不是跳过。这样用户不必手动点「刷新全网数据」也能自愈。
fn is_synced_today(database: &Database) -> Result<bool, String> {
    let mut connection = database.lock_conn()?;
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
    if count == 0 {
        return Ok(false);
    }

    let models_dev_present = connection
        .query_row(
            "SELECT COUNT(*) FROM model_catalog_sources WHERE source = ?1",
            [MODELS_DEV_SOURCE],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| error.to_string())?;
    Ok(models_dev_present > 0)
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
    let (context_min, context_max) =
        if let Some(arr) = obj.get("contextRange").and_then(Value::as_array) {
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
    let ref_provider = ref_obj
        .and_then(|r| r.get("provider"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let ref_official = boolean(obj.get("refOfficial"));
    let ref_input_cost = numeric(ref_obj.and_then(|r| r.get("input")));
    let ref_output_cost = numeric(ref_obj.and_then(|r| r.get("output")));
    let ref_cache_read_cost = numeric(ref_obj.and_then(|r| r.get("cacheRead")));

    let min_obj = obj.get("min").and_then(Value::as_object);
    let min_provider = min_obj
        .and_then(|r| r.get("provider"))
        .and_then(Value::as_str)
        .map(str::to_string);
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

    // ── 身份字段的**初始值**（随后由 models.dev 索引富化覆盖）──
    // 这里只做「能从 llmpricing 行数据本身得出」的部分，不臆造。
    let raw_model_id = crate::model::catalog::models_dev::canonical_model_id(&id).to_string();
    let identity_resolved = !lab.is_empty() && lab != "misc";
    let official_lab = if identity_resolved {
        lab.clone()
    } else {
        "misc".to_string()
    };
    let identity_source = if identity_resolved {
        "llmpricing_lab".to_string()
    } else {
        "unknown".to_string()
    };

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
        official_lab,
        official_model_id: raw_model_id,
        canonical_id: None,
        identity_source,
        identity_resolved,
        official_host_count: 0,
        official_channel_providers: Vec::new(),
        lab_tier_host_count: 0,
        cloud_tier_host_count: 0,
        gateway_tier_host_count: 0,
        free_channel_count: 0,
        free_channel_providers: Vec::new(),
        subscription_channel_providers: Vec::new(),
        // 默认沿用上游聚合值口径；富化成功后改为 `derived`。
        free_channel_source: "upstream".to_string(),
        free_channel_count_matches: false,
        reasoning_options: Vec::new(),
        reasoning_effort_max: None,
        output_modalities: Vec::new(),
        max_input_tokens: None,
        interleaved_fields: Vec::new(),
        has_fast_mode: false,
        models_dev_extras: ModelsDevModelExtras::default(),
    })
}

fn read_model_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ModelCatalogItem> {
    let input_modalities_json: String = row.get(14)?;
    let host_providers_json: String = row.get(33)?;
    let free_channel_providers_json: String = row.get(56)?;
    let subscription_channel_providers_json: String = row.get(57)?;
    let official_channel_providers_json: String = row.get(60)?;
    let reasoning_options_json: String = row.get(61)?;
    let output_modalities_json: String = row.get(63)?;
    let interleaved_fields_json: String = row.get(65)?;
    let models_dev_extras_json: String = row.get(67)?;

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
        official_lab: row.get(46)?,
        official_model_id: row.get(47)?,
        canonical_id: row.get(48)?,
        identity_source: row.get(49)?,
        identity_resolved: row.get::<_, i64>(50)? != 0,
        official_host_count: row.get::<_, i64>(51)?.max(0) as usize,
        official_channel_providers: serde_json::from_str(&official_channel_providers_json)
            .unwrap_or_default(),
        lab_tier_host_count: row.get::<_, i64>(52)?.max(0) as usize,
        cloud_tier_host_count: row.get::<_, i64>(53)?.max(0) as usize,
        gateway_tier_host_count: row.get::<_, i64>(54)?.max(0) as usize,
        free_channel_count: row.get::<_, i64>(55)?.max(0) as usize,
        free_channel_providers: serde_json::from_str(&free_channel_providers_json)
            .unwrap_or_default(),
        subscription_channel_providers: serde_json::from_str(
            &subscription_channel_providers_json,
        )
        .unwrap_or_default(),
        free_channel_source: row.get(58)?,
        free_channel_count_matches: row.get::<_, i64>(59)? != 0,
        reasoning_options: serde_json::from_str(&reasoning_options_json).unwrap_or_default(),
        reasoning_effort_max: row.get(62)?,
        output_modalities: serde_json::from_str(&output_modalities_json).unwrap_or_default(),
        max_input_tokens: row.get(64)?,
        interleaved_fields: serde_json::from_str(&interleaved_fields_json).unwrap_or_default(),
        has_fast_mode: row.get::<_, i64>(66)? != 0,
        models_dev_extras: serde_json::from_str(&models_dev_extras_json).unwrap_or_default(),
    })
}

fn read_provider_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ModelCatalogProvider> {
    let env_json: String = row.get(10)?;
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
        is_first_party: row.get::<_, i64>(9)? != 0,
        env: serde_json::from_str(&env_json).unwrap_or_default(),
    })
}

/// `model_catalog_models` 的 SELECT 列清单。
///
/// ⚠️ 顺序**不是**建表顺序：历史原因 `host_providers_json` 排在 `priced_host_count` 之前。
/// 任何改动都必须同步 [`read_model_row`] 的下标。
const MODEL_SELECT_COLUMNS: &str = "id, slug, name, lab, kind, family, knowledge, status,
    open_weights, reasoning, tool_call, attachment, structured, temperature,
    input_modalities_json, context_length, context_min, context_max, max_output_tokens,
    ref_provider, ref_official, ref_input_cost, ref_output_cost, ref_cache_read_cost,
    min_provider, min_input_cost, min_output_cost, min_cache_read_cost, price_spread,
    blended_min, blended_trusted, blended_ref,
    host_count, host_providers_json, priced_host_count, free_host_count, sub_host_count,
    aa_idx, aa_coding, aa_agentic, aa_speed, aa_ttft, aa_task_cost, benchmark_count,
    release_date, last_updated,
    official_lab, official_model_id, canonical_id, identity_source, identity_resolved,
    official_host_count, lab_tier_host_count, cloud_tier_host_count, gateway_tier_host_count,
    free_channel_count, free_channel_providers_json, subscription_channel_providers_json,
    free_channel_source, free_channel_count_matches, official_channel_providers_json,
    reasoning_options_json, reasoning_effort_max, output_modalities_json, max_input_tokens,
    interleaved_fields_json, has_fast_mode, models_dev_extras_json";

/// `model_catalog_providers` 的 SELECT 列清单。
const PROVIDER_SELECT_COLUMNS: &str =
    "id, name, npm, api, doc, tier, subscription, model_count, date_modified, is_first_party, env_json";

/// 由 models.dev 索引推导出的模型增强信息。
struct ModelEnrichment {
    official_lab: String,
    official_model_id: String,
    canonical_id: Option<String>,
    identity_source: String,
    identity_resolved: bool,
    official_host_count: usize,
    official_channel_providers: Vec<String>,
    lab_tier_host_count: usize,
    cloud_tier_host_count: usize,
    gateway_tier_host_count: usize,
    free_channel_count: usize,
    free_channel_providers: Vec<String>,
    subscription_channel_providers: Vec<String>,
    free_channel_source: String,
    free_channel_count_matches: bool,
    hosts_json: Option<String>,
    reasoning_options: Vec<ReasoningOption>,
    reasoning_effort_max: Option<String>,
    output_modalities: Vec<String>,
    max_input_tokens: Option<i64>,
    interleaved_fields: Vec<String>,
    has_fast_mode: bool,
    models_dev_extras: ModelsDevModelExtras,
}

/// 为单个模型推导身份、官网/三方分层与免费渠道明细。
///
/// 所有推断都遵守「**不确定就不猜**」：lab 无法确定时保持 `misc` 并置 `identity_resolved = false`；
/// models.dev 无数据的渠道不臆造价格，只计入 `missing_models_dev`。
fn enrich_model_with_models_dev(
    index: &ModelsDevIndex,
    provider_meta: &BTreeMap<String, ProviderMeta>,
    item: &ModelCatalogItem,
    inherited_lab: Option<&str>,
    canonical_reverse_lab: Option<&str>,
) -> ModelEnrichment {
    // 身份：优先 canonical 层，其次 llmpricing lab，再退关键词 / 同名继承 / canonical 反查。
    let canonical_id = index.canonical_id_for(&item.id, Some(&item.lab));
    let identity = resolve_identity(
        canonical_id.as_deref(),
        Some(&item.lab),
        &item.official_model_id,
        inherited_lab,
        canonical_reverse_lab,
    );

    // 官网 / 三方分层：tier 描述渠道性质，official 描述是否原厂渠道，两者正交。
    let mut lab_tier = 0usize;
    let mut cloud_tier = 0usize;
    let mut gateway_tier = 0usize;
    for provider in &item.host_providers {
        match provider_meta.get(provider).map(|meta| {
            crate::model::catalog::lab_registry::ChannelTier::parse(meta.tier.as_deref())
        }) {
            Some(crate::model::catalog::lab_registry::ChannelTier::Lab) => lab_tier += 1,
            Some(crate::model::catalog::lab_registry::ChannelTier::Cloud) => cloud_tier += 1,
            Some(crate::model::catalog::lab_registry::ChannelTier::Gateway) => gateway_tier += 1,
            _ => {}
        }
    }

    let official_channels = official_channels_for(&identity.lab, &item.host_providers);

    // 免费渠道明细：用组合方案重建，替代 HTML 爬取。
    let build_input = HostBuildInput {
        lab: &identity.lab,
        model_id: &item.id,
        model_name: &item.name,
        host_providers: &item.host_providers,
        ref_provider: item.ref_provider.as_deref(),
        min_provider: item.min_provider.as_deref(),
    };
    let built = build_hosts(index, provider_meta, &build_input);
    let hosts_json = serde_json::to_string(&built.hosts).ok();

    // canonical 层附加信息（描述 / 许可证 / 链接 / 权重 / 基准明细）
    let models_dev_extras = identity
        .canonical_id
        .as_deref()
        .and_then(|cid| index.canonical.get(cid))
        .map(extract_model_extras)
        .unwrap_or_default();

    ModelEnrichment {
        official_lab: identity.lab.clone(),
        official_model_id: identity.model_id.clone(),
        canonical_id: identity.canonical_id.clone(),
        identity_source: identity.source.as_str().to_string(),
        identity_resolved: identity.resolved,
        official_host_count: official_channels.len(),
        official_channel_providers: official_channels,
        lab_tier_host_count: lab_tier,
        cloud_tier_host_count: cloud_tier,
        gateway_tier_host_count: gateway_tier,
        free_channel_count: built.stats.free,
        free_channel_providers: built.free_providers,
        subscription_channel_providers: built.subscription_providers,
        free_channel_source: "derived".to_string(),
        free_channel_count_matches: built.stats.free == item.free_host_count,
        hosts_json,
        reasoning_effort_max: built
            .reasoning_options
            .iter()
            .filter(|option| option.kind == "effort")
            .find_map(|option| option.highest_effort().map(str::to_string)),
        reasoning_options: built.reasoning_options,
        output_modalities: built.output_modalities,
        max_input_tokens: built.max_input_tokens,
        interleaved_fields: built.interleaved_fields,
        has_fast_mode: built.has_fast_mode,
        models_dev_extras,
    }
}

/// 从 canonical 层条目里抽取附加信息。
///
/// 只搬运上游已有的字段，**不做任何推断**：字段缺失就留空。
fn extract_model_extras(canonical: &Value) -> ModelsDevModelExtras {
    fn array_field(value: &Value, key: &str) -> Vec<Value> {
        value
            .get(key)
            .and_then(Value::as_array)
            .map(|items| items.iter().filter(|v| v.is_object()).cloned().collect())
            .unwrap_or_default()
    }
    ModelsDevModelExtras {
        description: canonical
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        license: canonical
            .get("license")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        links: array_field(canonical, "links"),
        weights: array_field(canonical, "weights"),
        benchmarks: array_field(canonical, "benchmarks"),
    }
}

fn persist_catalog_llmpricing(
    database: &Database,
    manifest_raw: &str,
    manifest: &Value,
    shards_data: &[(String, String, Value)],
    models_dev_index: Option<&ModelsDevIndex>,
) -> Result<CatalogSyncReport, String> {
    let mut connection = database.lock_conn()?;
    // ⚠️ 必须走版本对账而不是直接 `ensure_catalog_schema`：
    // `core/db.rs` 的迁移里也有一份 `CREATE TABLE IF NOT EXISTS model_catalog_models`
    // 的旧定义，它会先于本模块执行。若只调 `ensure_catalog_schema`，
    // `IF NOT EXISTS` 会因为表已存在而变成空操作，新增列永远不会被创建。
    // `clear_legacy_catalog_if_needed` 会按 `CATALOG_SCHEMA_VERSION` 判断并 DROP 重建，
    // 使本模块的 DDL 成为唯一权威。
    clear_legacy_catalog_if_needed(&mut connection)?;

    // 渠道元信息（tier / subscription / name / doc），供渠道明细构建使用。
    let provider_meta: BTreeMap<String, ProviderMeta> = manifest
        .get("providers")
        .and_then(Value::as_object)
        .map(|providers| {
            providers
                .iter()
                .map(|(id, entry)| {
                    (
                        id.clone(),
                        ProviderMeta {
                            name: text(entry.get("name")),
                            tier: opt_text(entry.get("tier")),
                            subscription: boolean(entry.get("subscription")),
                            doc: opt_text(entry.get("doc")),
                        },
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    // 渠道明细不再靠 HTML 爬取：改为「models.dev 渠道层 + llmpricing 范围」组合推导，
    // 每次同步直接重建，无需缓存快照，也不会产生过期缓存。
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
    // ⚠️ 只清理本次要重建的 llmpricing 来源行，**保留 models.dev 行**。
    // models.dev 的来源行由 `fetch_models_dev_index` 写入，而它在 persist 之前执行；
    // 若这里无条件 `DELETE FROM model_catalog_sources`，刚写入的 models.dev 行会被抹掉，
    // 前端「数据来源」面板就看不到主数据源了（曾经就是这样漏的）。
    transaction
        .execute(
            "DELETE FROM model_catalog_sources WHERE source != ?1",
            [MODELS_DEV_SOURCE],
        )
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
        let p_sub = if boolean(p_val.get("subscription")) {
            1
        } else {
            0
        };
        let p_count = integer(p_val.get("count"));
        let p_date = opt_text(p_val.get("dateModified"));
        let p_first_party = if crate::model::catalog::lab_registry::is_first_party_provider(p_id) {
            1
        } else {
            0
        };
        // API Key 环境变量名来自 models.dev（llmpricing manifest 没有这个字段）
        let p_env = models_dev_index
            .and_then(|index| index.provider_meta.get(p_id))
            .and_then(|meta| meta.env.clone())
            .unwrap_or_default();

        transaction
            .execute(
                "INSERT INTO model_catalog_providers (
                    id, name, npm, api, doc, tier, subscription, model_count, date_modified,
                    is_first_party, env_json, raw_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
                    p_first_party,
                    serde_json::to_string(&p_env).unwrap_or_else(|_| "[]".into()),
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

    // ── 预扫描：为 `misc/*` 模型准备可继承的 lab ──
    // 同名继承：llmpricing 内部存在同名但 lab 已知的行（如 `nousresearch/hermes-3-llama-3.1-405b`），
    // 仅当同名行的 lab **唯一**时才继承，歧义（如 `auto` 对应 3 个 lab）一律放弃。
    let mut name_to_labs: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
    for item in deduplicated_models.values().map(|(item, _)| item) {
        if !item.lab.is_empty() && item.lab != "misc" {
            name_to_labs
                .entry(normalize_model_id(&item.official_model_id))
                .or_default()
                .insert(item.lab.clone());
        }
    }
    let inherited_labs: BTreeMap<String, String> = name_to_labs
        .into_iter()
        .filter_map(|(normalized, labs)| {
            if labs.len() == 1 {
                labs.into_iter().next().map(|lab| (normalized, lab))
            } else {
                None
            }
        })
        .collect();

    for (item, raw_str) in deduplicated_models.values_mut() {
        let aa_json =
            if item.aa_idx.is_some() || item.aa_coding.is_some() || item.aa_speed.is_some() {
                Some(
                    json!({
                        "idx": item.aa_idx,
                        "coding": item.aa_coding,
                        "agentic": item.aa_agentic,
                        "speed": item.aa_speed,
                        "ttft": item.aa_ttft,
                        "taskCost": item.aa_task_cost,
                    })
                    .to_string(),
                )
            } else {
                None
            };

        // ── 用 models.dev 富化：身份 / 官网三方分层 / 免费渠道明细 ──
        let mut hosts_json: Option<String> = None;
        if let Some(index) = models_dev_index {
            let normalized = normalize_model_id(&item.official_model_id);
            let inherited_lab = inherited_labs.get(&normalized).map(String::as_str);
            let canonical_reverse_lab = index
                .canonical_candidates(&item.official_model_id)
                .first()
                .map(|candidate| {
                    crate::model::catalog::models_dev::canonical_lab(candidate).to_string()
                });
            let enrichment = enrich_model_with_models_dev(
                index,
                &provider_meta,
                item,
                inherited_lab,
                canonical_reverse_lab.as_deref(),
            );

            item.official_lab = enrichment.official_lab;
            item.official_model_id = enrichment.official_model_id;
            item.canonical_id = enrichment.canonical_id;
            item.identity_source = enrichment.identity_source;
            item.identity_resolved = enrichment.identity_resolved;
            item.official_host_count = enrichment.official_host_count;
            item.official_channel_providers = enrichment.official_channel_providers;
            item.lab_tier_host_count = enrichment.lab_tier_host_count;
            item.cloud_tier_host_count = enrichment.cloud_tier_host_count;
            item.gateway_tier_host_count = enrichment.gateway_tier_host_count;
            item.free_channel_count = enrichment.free_channel_count;
            item.free_channel_providers = enrichment.free_channel_providers;
            item.subscription_channel_providers = enrichment.subscription_channel_providers;
            item.free_channel_source = enrichment.free_channel_source;
            item.free_channel_count_matches = enrichment.free_channel_count_matches;
            item.reasoning_options = enrichment.reasoning_options;
            item.reasoning_effort_max = enrichment.reasoning_effort_max;
            item.output_modalities = enrichment.output_modalities;
            item.max_input_tokens = enrichment.max_input_tokens;
            item.interleaved_fields = enrichment.interleaved_fields;
            item.has_fast_mode = enrichment.has_fast_mode;
            item.models_dev_extras = enrichment.models_dev_extras;
            hosts_json = enrichment.hosts_json;
        }

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
                    release_date, last_updated,
                    official_lab, official_model_id, canonical_id, identity_source, identity_resolved,
                    official_host_count, official_channel_providers_json,
                    lab_tier_host_count, cloud_tier_host_count, gateway_tier_host_count,
                    free_channel_count, free_channel_providers_json, subscription_channel_providers_json,
                    free_channel_source, free_channel_count_matches,
                    reasoning_options_json, reasoning_effort_max, output_modalities_json,
                    max_input_tokens, interleaved_fields_json, has_fast_mode,
                    models_dev_extras_json,
                    hosts_json, raw_json, updated_at
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                    ?9, ?10, ?11, ?12, ?13, ?14,
                    ?15, ?16, ?17, ?18, ?19,
                    ?20, ?21, ?22, ?23, ?24,
                    ?25, ?26, ?27, ?28, ?29,
                    ?30, ?31, ?32,
                    ?33, ?34, ?35, ?36, ?37,
                    ?38, ?39, ?40, ?41, ?42, ?43, ?44, ?45,
                    ?46, ?47,
                    ?48, ?49, ?50, ?51, ?52, ?53,
                    ?54, ?55, ?56, ?57,
                    ?58, ?59, ?60, ?61,
                    ?62, ?63, ?64, ?65,
                    ?66, ?67, ?68, ?69, ?70, ?71, ?72
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
                    item.official_lab,
                    item.official_model_id,
                    item.canonical_id,
                    item.identity_source,
                    if item.identity_resolved { 1 } else { 0 },
                    item.official_host_count as i64,
                    serde_json::to_string(&item.official_channel_providers)
                        .unwrap_or_else(|_| "[]".into()),
                    item.lab_tier_host_count as i64,
                    item.cloud_tier_host_count as i64,
                    item.gateway_tier_host_count as i64,
                    item.free_channel_count as i64,
                    serde_json::to_string(&item.free_channel_providers)
                        .unwrap_or_else(|_| "[]".into()),
                    serde_json::to_string(&item.subscription_channel_providers)
                        .unwrap_or_else(|_| "[]".into()),
                    item.free_channel_source,
                    if item.free_channel_count_matches { 1 } else { 0 },
                    serde_json::to_string(&item.reasoning_options).unwrap_or_else(|_| "[]".into()),
                    item.reasoning_effort_max,
                    serde_json::to_string(&item.output_modalities).unwrap_or_else(|_| "[]".into()),
                    item.max_input_tokens,
                    serde_json::to_string(&item.interleaved_fields).unwrap_or_else(|_| "[]".into()),
                    if item.has_fast_mode { 1 } else { 0 },
                    serde_json::to_string(&item.models_dev_extras)
                        .unwrap_or_else(|_| "{}".into()),
                    hosts_json,
                    // `values_mut()` 迭代下 raw_str 是 &mut String，需转成 &str 才能作为 SQL 参数
                    raw_str.as_str(),
                    fetched_at,
                ],
            )
            .map_err(|error| error.to_string())?;
        model_count += 1;
    }

    crate::db::write_meta(
        &transaction,
        CATALOG_SCHEMA_META_KEY,
        CATALOG_SCHEMA_VERSION,
    )?;

    transaction.commit().map_err(|error| error.to_string())?;

    Ok(CatalogSyncReport {
        provider_count,
        model_count,
        shard_count: shards_data.len(),
    })
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn get_model_catalog(
    ctx: Managed<'_, Arc<AppContext>>,
) -> Result<ModelCatalogSnapshot, String> {
    get_model_catalog_inner(&ctx.database)
}

pub(crate) fn get_model_catalog_inner(database: &Database) -> Result<ModelCatalogSnapshot, String> {
    let mut connection = database.lock_conn()?;
    clear_legacy_catalog_if_needed(&mut connection)?;

    let mut statement = connection
        .prepare(&format!(
            "SELECT {MODEL_SELECT_COLUMNS}
             FROM model_catalog_models
             ORDER BY lab, name COLLATE NOCASE, id"
        ))
        .map_err(|error| error.to_string())?;

    let models = statement
        .query_map([], read_model_row)
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;

    let mut provider_statement = connection
        .prepare(&format!(
            "SELECT {PROVIDER_SELECT_COLUMNS}
             FROM model_catalog_providers
             ORDER BY name COLLATE NOCASE"
        ))
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

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_model_catalog_detail(
    ctx: Managed<'_, Arc<AppContext>>,
    canonical_key: Option<String>,
    id: Option<String>,
) -> Result<ModelCatalogDetail, String> {
    let key = id.or(canonical_key).unwrap_or_default();
    get_model_catalog_detail_inner(&ctx.database, &key).await
}

pub(crate) async fn get_model_catalog_detail_inner(
    database: &Database,
    key: &str,
) -> Result<ModelCatalogDetail, String> {
    let (model, raw, cached_hosts_json, all_providers_map) = {
        let mut connection = database.lock_conn()?;
        clear_legacy_catalog_if_needed(&mut connection)?;

        let model_res = connection.query_row(
            &format!(
                "SELECT {MODEL_SELECT_COLUMNS}
                 FROM model_catalog_models
                 WHERE id = ?1 OR slug = ?1"
            ),
            [key],
            read_model_row,
        );

        let model = match model_res {
            Ok(m) => m,
            Err(_) => {
                // Try prefix/case-insensitive match
                let found = connection.query_row(
                    &format!(
                        "SELECT {MODEL_SELECT_COLUMNS}
                         FROM model_catalog_models
                         WHERE id LIKE ?1 COLLATE NOCASE OR slug LIKE ?1 COLLATE NOCASE
                         LIMIT 1"
                    ),
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

        let cached_hosts: Option<String> = connection
            .query_row(
                "SELECT hosts_json FROM model_catalog_models WHERE id = ?1",
                [&model.id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .flatten();

        let raw: Value = serde_json::from_str(&raw_text).unwrap_or(Value::Null);

        // Load all providers into map
        let mut p_stmt = connection
            .prepare(&format!(
                "SELECT {PROVIDER_SELECT_COLUMNS} FROM model_catalog_providers"
            ))
            .map_err(|e| e.to_string())?;
        let p_rows = p_stmt
            .query_map([], read_provider_row)
            .map_err(|e| e.to_string())?;
        let mut prov_map = BTreeMap::new();
        for r in p_rows {
            if let Ok(p) = r {
                prov_map.insert(p.id.clone(), p);
            }
        }

        (model, raw, cached_hosts, prov_map)
    };

    // 渠道明细：同步时已由「models.dev 渠道层 + llmpricing 范围」组合推导并写入
    // `hosts_json`，此处直接反序列化即可，**不再回源抓取 HTML**。
    //
    // ⚠️ 不能像旧实现那样按 `input == 0 && output == 0` 重算 `is_free`：
    // 同一渠道可能同时登记付费与 `:free` 两个条目，展示价取付费那条而免费标记仍为真
    // （实测 `unorouter` 对 glm-5.2 即如此）。直接沿用构建时算好的标记才正确。
    let mut hosts_list: Vec<ModelCatalogHostItem> = cached_hosts_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();

    // 旧版本遗留的 hosts_json 结构可能不完整（如缺 isFree），做一次补全：
    // 缺失的字段按 provider 元信息与上游聚合值兜底，避免详情页出现空白。
    for host in &mut hosts_list {
        if let Some(meta) = all_providers_map.get(&host.provider) {
            if host.name.is_empty() {
                host.name = meta.name.clone();
            }
            if host.tier.is_none() {
                host.tier = meta.tier.clone();
            }
            if host.doc.is_none() {
                host.doc = meta.doc.clone();
            }
        }
    }

    let mut matched_providers = Vec::new();
    for host in &hosts_list {
        if let Some(meta) = all_providers_map.get(&host.provider) {
            if !matched_providers
                .iter()
                .any(|p: &ModelCatalogProvider| p.id == meta.id)
            {
                matched_providers.push(meta.clone());
            }
        }
    }

    // 渠道明细缺失时，用 `host_providers` + 已推导的官方/免费标记兜底，
    // 保证详情页至少能列出渠道，而不是显示为空。
    if hosts_list.is_empty() {
        for provider in &model.host_providers {
            let meta = all_providers_map.get(provider);
            let is_official = official_channels_for(&model.official_lab, &model.host_providers)
                .iter()
                .any(|id| id == provider);
            hosts_list.push(ModelCatalogHostItem {
                provider: provider.clone(),
                name: meta.map(|p| p.name.clone()).unwrap_or_else(|| provider.clone()),
                model_id: None,
                tier: meta.and_then(|p| p.tier.clone()),
                subscription: meta.map(|p| p.subscription).unwrap_or(false),
                input: None,
                output: None,
                cache_read: None,
                cache_write: None,
                context: Some(model.context_length),
                output_limit: Some(model.max_output_tokens),
                // 兜底路径没有渠道级明细，思考级别/输入上限等回落到模型级聚合值
                input_limit: model.max_input_tokens,
                output_modalities: model.output_modalities.clone(),
                reasoning_options: model.reasoning_options.clone(),
                interleaved_field: model.interleaved_fields.first().cloned(),
                fast_mode: None,
                status: None,
                official: is_official,
                doc: meta.and_then(|p| p.doc.clone()),
                is_free: model.free_channel_providers.iter().any(|id| id == provider),
                is_min: model.min_provider.as_deref() == Some(provider.as_str()),
                is_ref: model.ref_provider.as_deref() == Some(provider.as_str()) || is_official,
            });
        }
    }

    Ok(ModelCatalogDetail {
        model,
        providers: matched_providers,
        hosts: hosts_list,
        raw,
    })
}

/// 单个模型的 **models.dev 能力**（供「模型参数」按模型呈现思考档位与各项属性）。
///
/// 与整份目录快照分开返回：本地工具页只关心「这个模型支持什么」，
/// 不需要渠道价格明细。字段全部来自 [`ModelCatalogItem`] 已入库的 models.dev 富化结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelCapabilities {
    /// 思考级别选项（`toggle` / `effort` 两形态，effort 含档位列表）。
    pub reasoning_options: Vec<ReasoningOption>,
    /// 模型支持的最高思考档位。
    pub reasoning_effort_max: Option<String>,
    pub context_length: i64,
    pub max_output_tokens: i64,
    /// 独立输入上限（`limit.input`），可能小于总上下文。
    pub max_input_tokens: Option<i64>,
    pub output_modalities: Vec<String>,
    /// 交错推理的读取字段名（如 `reasoning_content`）。
    pub interleaved_fields: Vec<String>,
    pub has_fast_mode: bool,
    pub supports_temperature: bool,
    pub supports_tool_call: bool,
    pub supports_structured_output: bool,
    pub supports_reasoning: bool,
    pub open_weights: bool,

    // ───────── 匹配出处（Agent 配置页据此披露「这个默认值是从哪来的」）─────────
    /// 命中的目录条目 id（`lab/modelId`）。
    pub matched_id: String,
    /// 该条目声明的**原始 model id**（上游原拼写，如 `z-ai/glm-5.2`）。
    pub matched_model_id: String,
    /// 匹配强度：`exact` / `variant` / `alias` / `nearest`（见
    /// [`crate::model::catalog::nearest::MatchKind`]）。非 `exact` 时 UI 会加「≈」前缀。
    pub match_kind: String,
    /// 强度指示：全等类恒为 `1.0`，`nearest` 为有序字符 Dice。**不是概率**。
    pub match_score: f32,
    /// 匹配算法版本，供前端作废旧的能力缓存（包含 miss 缓存）。
    pub match_version: u32,
}

impl ModelCapabilities {
    fn from_item(item: &ModelCatalogItem, matched: &Match) -> Self {
        ModelCapabilities {
            reasoning_options: item.reasoning_options.clone(),
            reasoning_effort_max: item.reasoning_effort_max.clone(),
            context_length: item.context_length,
            max_output_tokens: item.max_output_tokens,
            max_input_tokens: item.max_input_tokens,
            output_modalities: item.output_modalities.clone(),
            interleaved_fields: item.interleaved_fields.clone(),
            has_fast_mode: item.has_fast_mode,
            supports_temperature: item.temperature,
            supports_tool_call: item.tool_call,
            supports_structured_output: item.structured,
            supports_reasoning: item.reasoning,
            open_weights: item.open_weights,
            matched_id: item.id.clone(),
            matched_model_id: item.official_model_id.clone(),
            match_kind: matched.kind.as_str().to_string(),
            match_score: matched.score,
            match_version: CAPABILITY_MATCH_VERSION,
        }
    }
}

/// 按模型 id 列表查询 models.dev 能力，返回 `请求原样 id -> 能力` 的映射。
///
/// 完整 / 裸 ID 强精确优先，再尝试保留模型身份的标准化与受限别名。
/// 歧义或未知身份差异不返回条目；不会改写 Agent 已明确自定义的参数。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn get_model_capabilities(
    ctx: Managed<'_, Arc<AppContext>>,
    keys: Vec<String>,
) -> Result<BTreeMap<String, ModelCapabilities>, String> {
    let snapshot = get_model_catalog_inner(&ctx.database)?;
    Ok(capabilities_for_keys(&snapshot.models, &keys))
}

/// 匹配算法版本：前端会把它连同缓存值一起存下，版本不符即重新请求。
///
/// - `1`（隐式）：只有归一化**全等**命中（`nearest` 上线前）。
/// - `2`：强精确、保守变体 / 渠道别名与身份约束的最近邻。
pub const CAPABILITY_MATCH_VERSION: u32 = 2;

/// 纯函数版：在给定目录条目集合里解析一批请求 key（便于离线单测，命令层只做取数）。
///
/// 匹配走 [`NearestIndex`]；碰撞拒绝，不按目录顺序任取首项。
/// **查不到的 key 不出现在结果里**（不臆造），前端据此回落到 Agent 级/父级默认值。
pub(crate) fn capabilities_for_keys(
    models: &[ModelCatalogItem],
    keys: &[String],
) -> BTreeMap<String, ModelCapabilities> {
    let index = NearestIndex::new(
        models
            .iter()
            .map(|item| Candidate {
                keys: vec![item.id.clone(), item.official_model_id.clone()],
                effort_values: item
                    .reasoning_options
                    .iter()
                    .filter(|option| option.kind == "effort")
                    .flat_map(|option| option.values.iter().cloned())
                    .collect(),
            })
            .collect(),
    );

    let mut out: BTreeMap<String, ModelCapabilities> = BTreeMap::new();
    for raw in keys {
        let Some(matched) = index.resolve(raw) else {
            continue;
        };
        out.insert(
            raw.clone(),
            ModelCapabilities::from_item(&models[matched.index], &matched),
        );
    }
    out
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn sync_model_catalog(
    ctx: Managed<'_, Arc<AppContext>>,
    force: Option<bool>,
) -> Result<ModelCatalogSyncResult, String> {
    sync_model_catalog_inner(&ctx, force.unwrap_or(false)).await
}

pub(crate) async fn sync_model_catalog_inner(
    ctx: &Arc<AppContext>,
    force: bool,
) -> Result<ModelCatalogSyncResult, String> {
    let database = &*ctx.database;
    let runtime = &*ctx.model_catalog_runtime;
    let bus = ctx.event_bus.clone();
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
    bus.emit("model-catalog-sync-status", json!({ "status": "syncing" }));

    let client = build_http_client(database, Duration::from_secs(60), 5, "模型参数同步")?;

    // 1. Fetch manifest
    let (manifest_raw, manifest) =
        fetch_json(&client, "LLMPricing Manifest", LLMPRICING_MANIFEST_URL).await?;

    let shards_array = manifest
        .get("shards")
        .and_then(Value::as_array)
        .ok_or_else(|| "LLMPricing Manifest 缺少 shards 列表".to_string())?;

    let mut shards_data = Vec::with_capacity(shards_array.len());

    // 2. Fetch each shard
    for (idx, shard_val) in shards_array.iter().enumerate() {
        let shard_name = shard_val
            .as_str()
            .ok_or_else(|| "Shard 名称格式无效".to_string())?;
        let shard_url = format!("{LLMPRICING_BASE_URL}/{shard_name}");
        let progress_msg = format!(
            "正在下载模型分片 {}/{} ({})",
            idx + 1,
            shards_array.len(),
            shard_name
        );
        bus.emit(
            "model-catalog-sync-status",
            json!({ "status": "syncing", "message": progress_msg }),
        );

        let (shard_raw, shard_json) = fetch_json(&client, shard_name, &shard_url).await?;
        shards_data.push((shard_name.to_string(), shard_raw, shard_json));
    }

    // 3. Fetch models.dev catalog（主数据源：身份 + 官网/三方分层 + 渠道明细）
    bus.emit(
        "model-catalog-sync-status",
        json!({ "status": "syncing", "message": "正在获取 models.dev 模型目录…" }),
    );
    let models_dev_index = fetch_models_dev_index(database, &client).await?;

    // 4. Persist（渠道明细在入库时组合推导，不再抓取详情页）
    let report = persist_catalog_llmpricing(
        database,
        &manifest_raw,
        &manifest,
        &shards_data,
        Some(&models_dev_index),
    )?;

    let snapshot = get_model_catalog_inner(database)?;

    let message = format!(
        "模型参数同步完成：LLMPricing 收录 {} 个供应商、{} 个模型（共 {} 个分片）· models.dev 收录 {} 个模型 / {} 个渠道 · 渠道明细由组合推导生成（无 HTML 爬取）",
        report.provider_count,
        report.model_count,
        report.shard_count,
        models_dev_index.canonical.len(),
        models_dev_index.provider_meta.len(),
    );

    bus.emit(
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
#[allow(dead_code)]
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

    info!(target: "openhub::catalog", "正在获取 LLMPricing Manifest: {LLMPRICING_MANIFEST_URL} ...");
    let (manifest_raw, manifest) =
        fetch_json(&client, "LLMPricing Manifest", LLMPRICING_MANIFEST_URL).await?;

    let shards_array = manifest
        .get("shards")
        .and_then(Value::as_array)
        .ok_or_else(|| "LLMPricing Manifest 缺少 shards 列表".to_string())?;

    let mut shards_data = Vec::with_capacity(shards_array.len());
    for (idx, shard_val) in shards_array.iter().enumerate() {
        let shard_name = shard_val
            .as_str()
            .ok_or_else(|| "Shard 名称格式无效".to_string())?;
        let shard_url = format!("{LLMPRICING_BASE_URL}/{shard_name}");
        info!(target: "openhub::catalog", "正在获取分片 [{}/{}]: {} ...", idx + 1, shards_array.len(), shard_name);
        let (shard_raw, shard_json) = fetch_json(&client, shard_name, &shard_url).await?;
        shards_data.push((shard_name.to_string(), shard_raw, shard_json));
    }

    info!(target: "openhub::catalog", "正在获取 models.dev 模型目录 ...");
    let models_dev_index = fetch_models_dev_index(database, &client).await?;

    info!(target: "openhub::catalog", "正在入库并更新本地模型参数缓存 ...");
    persist_catalog_llmpricing(
        database,
        &manifest_raw,
        &manifest,
        &shards_data,
        Some(&models_dev_index),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实端到端同步验证（**需联网**，默认忽略）。
    ///
    /// 会真正跑一遍：抓 manifest + 5 个分片 + models.dev catalog，
    /// 然后入库并校验身份/官方渠道/免费渠道的产出。
    ///
    /// ```sh
    /// cargo test --lib sync_model_catalog_end_to_end -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn sync_model_catalog_end_to_end() {
        let dir = std::env::temp_dir().join("openhub_catalog_e2e");
        std::fs::create_dir_all(&dir).expect("创建临时目录失败");
        let db_path = dir.join("catalog_e2e.sqlite");
        let _ = std::fs::remove_file(&db_path);
        let db = db_path.to_string_lossy().to_string();

        let report = sync_model_catalog_once(&db).expect("同步失败");
        eprintln!(
            "同步完成：providers={} models={} shards={}",
            report.provider_count, report.model_count, report.shard_count
        );

        let database = Database::open(std::path::Path::new(&db)).expect("打开库失败");
        let snapshot = get_model_catalog_inner(&database).expect("读取快照失败");

        let total = snapshot.models.len();
        let identity_resolved = snapshot
            .models
            .iter()
            .filter(|m| m.identity_resolved)
            .count();
        let has_canonical = snapshot
            .models
            .iter()
            .filter(|m| m.canonical_id.is_some())
            .count();
        let with_official = snapshot
            .models
            .iter()
            .filter(|m| m.official_host_count > 0)
            .count();
        let with_free = snapshot
            .models
            .iter()
            .filter(|m| m.free_channel_count > 0)
            .count();
        let upstream_free = snapshot
            .models
            .iter()
            .filter(|m| m.free_host_count > 0)
            .count();
        let free_match = snapshot
            .models
            .iter()
            .filter(|m| m.free_channel_count_matches)
            .count();
        let derived_source = snapshot
            .models
            .iter()
            .filter(|m| m.free_channel_source == "derived")
            .count();
        let with_hosts_json = snapshot
            .models
            .iter()
            .filter(|m| !m.host_providers.is_empty())
            .count();

        // 思考级别 / 输出模态 / 附加信息
        let with_reasoning_options = snapshot
            .models
            .iter()
            .filter(|m| !m.reasoning_options.is_empty())
            .count();
        let with_effort = snapshot
            .models
            .iter()
            .filter(|m| m.reasoning_effort_max.is_some())
            .count();
        let with_input_limit = snapshot
            .models
            .iter()
            .filter(|m| m.max_input_tokens.is_some())
            .count();
        let with_interleaved = snapshot
            .models
            .iter()
            .filter(|m| !m.interleaved_fields.is_empty())
            .count();
        let with_fast_mode = snapshot.models.iter().filter(|m| m.has_fast_mode).count();
        let with_extras = snapshot
            .models
            .iter()
            .filter(|m| m.models_dev_extras.license.is_some())
            .count();
        let with_benchmarks = snapshot
            .models
            .iter()
            .filter(|m| !m.models_dev_extras.benchmarks.is_empty())
            .count();
        let with_description = snapshot
            .models
            .iter()
            .filter(|m| m.models_dev_extras.description.is_some())
            .count();
        // 非纯文本输出的模型（image / video / audio）
        let non_text_output = snapshot
            .models
            .iter()
            .filter(|m| {
                !m.output_modalities.is_empty()
                    && m.output_modalities.iter().any(|modality| modality != "text")
            })
            .count();
        let effort_histogram: std::collections::BTreeMap<String, usize> = {
            let mut map = std::collections::BTreeMap::new();
            for model in &snapshot.models {
                if let Some(effort) = &model.reasoning_effort_max {
                    *map.entry(effort.clone()).or_insert(0) += 1;
                }
            }
            map
        };

        eprintln!("── 数据规模 ──");
        eprintln!("模型总数            = {total}");
        eprintln!("身份已确定          = {identity_resolved}");
        eprintln!("定位到 canonical    = {has_canonical}");
        eprintln!("有官方渠道          = {with_official}");
        eprintln!("有免费渠道(本地推导)= {with_free}");
        eprintln!("有免费渠道(上游聚合)= {upstream_free}");
        eprintln!("免费计数与上游一致  = {free_match}");
        eprintln!("免费渠道来源=derived= {derived_source}");
        eprintln!("有渠道范围          = {with_hosts_json}");
        eprintln!("免费计数一致率      = {:.1}%", free_match as f64 / total as f64 * 100.0);
        eprintln!("── 新增：思考级别与输出能力 ──");
        eprintln!("有思考级别选项      = {with_reasoning_options}");
        eprintln!("有 effort 档位      = {with_effort}");
        eprintln!("有独立输入上限      = {with_input_limit}");
        eprintln!("有交错推理字段      = {with_interleaved}");
        eprintln!("有快速模式          = {with_fast_mode}");
        eprintln!("非纯文本输出        = {non_text_output}");
        eprintln!("最高档位分布        = {effort_histogram:?}");
        eprintln!("── 新增：canonical 附加信息 ──");
        eprintln!("有描述              = {with_description}");
        eprintln!("有许可证            = {with_extras}");
        eprintln!("有基准明细          = {with_benchmarks}");

        // glm-5.2 抽样
        if let Some(glm) = snapshot.models.iter().find(|m| m.id == "zhipuai/glm-5.2") {
            eprintln!("── glm-5.2 抽样 ──");
            eprintln!("  officialLab       = {}", glm.official_lab);
            eprintln!("  officialModelId   = {}", glm.official_model_id);
            eprintln!("  canonicalId       = {:?}", glm.canonical_id);
            eprintln!("  identitySource    = {}", glm.identity_source);
            eprintln!("  officialHostCount = {}", glm.official_host_count);
            eprintln!(
                "  tier 分布         = lab {} / cloud {} / gateway {}",
                glm.lab_tier_host_count, glm.cloud_tier_host_count, glm.gateway_tier_host_count
            );
            eprintln!(
                "  freeChannelCount  = {} (上游 freeHostCount = {})",
                glm.free_channel_count, glm.free_host_count
            );
            eprintln!("  freeChannels      = {:?}", glm.free_channel_providers);
            eprintln!(
                "  subscriptionChans = {:?}",
                glm.subscription_channel_providers
            );
            eprintln!("  reasoningOptions  = {:?}", glm.reasoning_options);
            eprintln!("  最高思考档位      = {:?}", glm.reasoning_effort_max);
            eprintln!("  outputModalities  = {:?}", glm.output_modalities);
            eprintln!("  maxInputTokens    = {:?}", glm.max_input_tokens);
            eprintln!("  interleavedFields = {:?}", glm.interleaved_fields);
            eprintln!("  hasFastMode       = {}", glm.has_fast_mode);
            eprintln!("  license           = {:?}", glm.models_dev_extras.license);
            eprintln!(
                "  benchmarks        = {} 项",
                glm.models_dev_extras.benchmarks.len()
            );
        }

        // 渠道明细落库检查
        let detail = rt_block_on(get_model_catalog_detail_inner(&database, "zhipuai/glm-5.2"))
            .expect("读取详情失败");        eprintln!("── glm-5.2 渠道明细 ──");
        eprintln!("  hosts 条数 = {}", detail.hosts.len());
        for host in detail.hosts.iter().take(6) {
            eprintln!(
                "    {:<24} official={:<5} free={:<5} sub={:<5} in={:?} modelId={:?}",
                host.provider, host.official, host.is_free, host.subscription, host.input, host.model_id
            );
        }
        let mut official_hosts: Vec<&str> = detail
            .hosts
            .iter()
            .filter(|h| h.official)
            .map(|h| h.provider.as_str())
            .collect();
        official_hosts.sort();
        eprintln!("  官方渠道 = {official_hosts:?}");

        // ── 断言 ──
        assert!(total >= 1941, "模型数应不少于 llmpricing 的 1941，实际 {total}");
        // 身份未确定的只剩白牌/路由名（`auto`、`model-router`、`claw-*` …）。
        // 实测 175 个 `misc/*` 中 103 个可判定，故 1941 - 72 = 1869。
        assert!(
            identity_resolved >= 1860,
            "身份已确定数偏低：{identity_resolved}（预期约 1869）"
        );
        assert!(has_canonical >= 300, "canonical 命中偏低：{has_canonical}");
        assert!(
            free_match as f64 / total as f64 >= 0.96,
            "免费渠道计数率跌破 96%"
        );
        assert_eq!(
            official_hosts,
            vec!["zai", "zhipuai"],
            "glm-5.2 的官方渠道应为 zai + zhipuai"
        );

        // 主数据源必须出现在来源表里（曾因 persist 无条件 DELETE 而被抹掉）
        let source_count: i64 = database
            .lock_conn()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM model_catalog_sources WHERE source = ?1",
                [MODELS_DEV_SOURCE],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source_count, 1, "models.dev 来源行缺失");
        eprintln!("来源表已含 {MODELS_DEV_SOURCE} ✓");

        // ── 新增字段的覆盖度断言 ──
        assert!(
            with_reasoning_options >= 300,
            "有思考级别选项的模型数偏低：{with_reasoning_options}"
        );
        assert!(
            with_effort >= 150,
            "有 effort 档位的模型数偏低：{with_effort}"
        );
        assert!(
            with_input_limit >= 200,
            "有独立输入上限的模型数偏低：{with_input_limit}"
        );
        assert!(
            non_text_output >= 50,
            "非纯文本输出的模型数偏低：{non_text_output}"
        );
        assert!(
            with_description >= 300,
            "有描述的模型数偏低：{with_description}"
        );
    }

    /// 在测试里驱动一次 async 调用。
    fn rt_block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("创建运行时失败")
            .block_on(future)
    }

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
                 CREATE TABLE model_catalog_entries (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    canonical_key TEXT NOT NULL,
                    FOREIGN KEY(canonical_key) REFERENCES model_catalog_models(canonical_key)
                 );
                 INSERT INTO app_meta (key, value) VALUES ('model_catalog_schema_version', '7');
                 INSERT INTO model_catalog_sources (source) VALUES ('openrouter');
                 INSERT INTO model_catalog_models (canonical_key) VALUES ('openai/gpt-primary');
                 INSERT INTO model_catalog_entries (canonical_key) VALUES ('openai/gpt-primary');",
            )
            .unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
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

    /// 回归：旧版 model_catalog_entries 残留了指向 canonical_key 的外键，而新目录
    /// 主键已改为 id。外键无法解析时，开启 foreign_keys 的连接对目录表做任何写
    /// 操作都会报 "foreign key mismatch"，版本迁移清理必须能穿透这种状态。
    #[test]
    fn dangling_fk_entries_table_is_cleared_on_version_upgrade() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        // 先按旧结构建表并写入数据，再单独把 models 重建为新结构（模拟半迁移）；
        // 显式关闭外键，避免父表 DROP 被隐式删除检查拦截（模拟历史版本升级路径）
        connection
            .execute_batch(
                "PRAGMA foreign_keys = OFF;
                 CREATE TABLE app_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE model_catalog_models (canonical_key TEXT PRIMARY KEY);
                 CREATE TABLE model_catalog_entries (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    canonical_key TEXT NOT NULL,
                    FOREIGN KEY(canonical_key) REFERENCES model_catalog_models(canonical_key)
                 );
                 INSERT INTO app_meta (key, value) VALUES ('model_catalog_schema_version', '7');
                 INSERT INTO model_catalog_models (canonical_key) VALUES ('openai/gpt-primary');
                 INSERT INTO model_catalog_entries (canonical_key) VALUES ('openai/gpt-primary');
                 DROP TABLE model_catalog_models;
                 CREATE TABLE model_catalog_models (id TEXT PRIMARY KEY);
                 PRAGMA foreign_keys = ON;",
            )
            .unwrap();

        // 复现故障前提：外键无法解析时父表不可写
        let mismatched = connection
            .execute("DELETE FROM model_catalog_models", [])
            .is_err();
        assert!(mismatched, "stale FK should block writes before cleanup");

        clear_legacy_catalog_if_needed(&mut connection).unwrap();

        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM model_catalog_models", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "model_catalog_models should be recreated empty");

        let version: String = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key = ?1",
                [CATALOG_SCHEMA_META_KEY],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, CATALOG_SCHEMA_VERSION);

        // 清理后外键应恢复开启且目录表可正常写入
        let fk_on: i64 = connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fk_on, 1, "foreign_keys should be restored");
        connection
            .execute(
                "INSERT INTO model_catalog_models (id) VALUES ('openai/gpt-primary')",
                [],
            )
            .unwrap();
    }

    // ─────────────────────────────────────────────────────────────
    //  表结构一致性守卫
    // ─────────────────────────────────────────────────────────────

    /// 从 DDL 中解析出某张表的列名。
    ///
    /// 只处理本文件里这种规整的 `CREATE TABLE IF NOT EXISTS <t> ( ... );` 形态：
    /// 按顶层逗号切分列定义，取每段的首个标识符，跳过表级约束。
    fn ddl_columns(ddl: &str, table: &str) -> Vec<String> {
        let needle = format!("CREATE TABLE IF NOT EXISTS {table} (");
        let start = ddl
            .find(&needle)
            .unwrap_or_else(|| panic!("DDL 中找不到表 {table}"))
            + needle.len();

        // 找到与该 '(' 配对的 ')'
        let mut depth = 1usize;
        let mut end = start;
        for (offset, ch) in ddl[start..].char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = start + offset;
                        break;
                    }
                }
                _ => {}
            }
        }

        let body = &ddl[start..end];
        let mut columns = Vec::new();
        let mut current = String::new();
        let mut nested = 0usize;
        for ch in body.chars() {
            match ch {
                '(' => {
                    nested += 1;
                    current.push(ch);
                }
                ')' => {
                    nested = nested.saturating_sub(1);
                    current.push(ch);
                }
                ',' if nested == 0 => {
                    columns.push(std::mem::take(&mut current));
                }
                _ => current.push(ch),
            }
        }
        if !current.trim().is_empty() {
            columns.push(current);
        }

        columns
            .iter()
            .filter_map(|definition| {
                let name = definition.split_whitespace().next()?.to_string();
                let upper = name.to_uppercase();
                // 表级约束不是列
                if matches!(
                    upper.as_str(),
                    "PRIMARY" | "FOREIGN" | "UNIQUE" | "CHECK" | "CONSTRAINT"
                ) {
                    None
                } else {
                    Some(name)
                }
            })
            .collect()
    }

    /// `read_model_row` / `read_provider_row` 用**下标**取值，所以 SELECT 列清单
    /// 必须与之一一对应；而列又必须真实存在于 DDL 中。这里把「SELECT 里的列」
    /// 与「DDL 里的列」双向对齐，把「改了一边忘了另一边」变成编译期就能发现的错误。
    #[test]
    fn select_columns_match_ddl_columns() {
        let models_ddl = ddl_columns(CATALOG_SCHEMA_DDL, "model_catalog_models");
        let providers_ddl = ddl_columns(CATALOG_SCHEMA_DDL, "model_catalog_providers");

        // ① SELECT 引用的列必须存在于 DDL —— 否则线上会报
        //    `no such column: xxx`（这正是 subscription_channel_providers_json 那次事故）
        for (label, select, ddl) in [
            ("model_catalog_models", MODEL_SELECT_COLUMNS, &models_ddl),
            (
                "model_catalog_providers",
                PROVIDER_SELECT_COLUMNS,
                &providers_ddl,
            ),
        ] {
            for column in select.split(',').map(str::trim).filter(|c| !c.is_empty()) {
                assert!(
                    ddl.iter().any(|c| c == column),
                    "{label}: SELECT 引用了 DDL 中不存在的列 `{column}`"
                );
            }
        }

        // ② DDL 的列必须被 SELECT 覆盖（除非是刻意只写不读的列）
        let write_only = [
            "raw_json",
            "updated_at",
            "aa_json",
            "hosts_json",
            // 供应商表：SELECT 只需业务字段
            "raw_json",
        ];
        for (label, select, ddl) in [
            ("model_catalog_models", MODEL_SELECT_COLUMNS, &models_ddl),
            (
                "model_catalog_providers",
                PROVIDER_SELECT_COLUMNS,
                &providers_ddl,
            ),
        ] {
            let selected: Vec<&str> = select.split(',').map(str::trim).collect();
            for column in ddl {
                if write_only.contains(&column.as_str()) {
                    continue;
                }
                assert!(
                    selected.contains(&column.as_str()),
                    "{label}: DDL 列 `{column}` 未出现在 SELECT 清单中（会被静默漏读）"
                );
            }
        }

        // ③ SELECT 的列数必须与 read_*_row 的下标范围一致
        assert_eq!(
            MODEL_SELECT_COLUMNS.split(',').count(),
            68,
            "MODEL_SELECT_COLUMNS 列数变了，必须同步 read_model_row 的下标"
        );
        assert_eq!(
            PROVIDER_SELECT_COLUMNS.split(',').count(),
            11,
            "PROVIDER_SELECT_COLUMNS 列数变了，必须同步 read_provider_row 的下标"
        );
    }

    /// 回归：线上缺陷 —— 版本号未变但表里缺列。
    ///
    /// 事故现场：`CATALOG_SCHEMA_VERSION` 仍是 `"10"`，而 `subscription_channel_providers_json`
    /// / `official_channel_providers_json` 是 `"10"` 发布后才加的列。旧实现只比对版本号，
    /// 判定「无需重建」，于是 SELECT 直接报 `no such column`，整个模型目录页空白。
    ///
    /// 现在 DDL 指纹会兜住这种情况。
    #[test]
    fn stale_table_is_rebuilt_even_when_version_matches() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE model_catalog_models (id TEXT PRIMARY KEY, name TEXT);
                 INSERT INTO app_meta (key, value) VALUES ('model_catalog_schema_version', '10');
                 INSERT INTO model_catalog_models (id, name) VALUES ('openai/gpt-5', 'GPT-5');",
            )
            .unwrap();

        // 前提：此刻旧表确实缺列，SELECT 会失败
        let broken = connection
            .query_row(&format!("SELECT {MODEL_SELECT_COLUMNS} FROM model_catalog_models"), [], |row| {
                row.get::<_, String>(0)
            })
            .is_err();
        assert!(broken, "前置条件不成立：旧表应当缺列");

        // 版本号与当前一致（模拟「忘了 bump」），但指纹对不上 → 必须重建
        clear_legacy_catalog_if_needed(&mut connection).unwrap();

        // 重建后 SELECT 必须能跑通
        let count: i64 = connection
            .query_row(
                &format!("SELECT COUNT(*) FROM (SELECT {MODEL_SELECT_COLUMNS} FROM model_catalog_models)"),
                [],
                |row| row.get(0),
            )
            .expect("重建后 SELECT 仍失败");
        assert_eq!(count, 0, "旧数据应被清空");

        let fingerprint: String = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key = ?1",
                [CATALOG_SCHEMA_FINGERPRINT_META_KEY],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fingerprint, catalog_schema_fingerprint());
    }

    /// 回归：schema 重建后必须作废 models.dev 的 ETag。
    ///
    /// ETag 在 `app_meta`、正文在 `model_catalog_sources`，后者会随 schema 变更被 DROP。
    /// 若不作废 ETag，下次同步就会「带着孤儿 ETag 发条件请求 → 拿到 304 → 无正文可用」，
    /// 线上报 `models.dev catalog 返回 304 未变更，且本地无缓存可用`。
    #[test]
    fn schema_rebuild_invalidates_models_dev_etag() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        clear_legacy_catalog_if_needed(&mut connection).unwrap();
        crate::db::write_meta(&connection, MODELS_DEV_ETAG_META_KEY, "W/\"abc123\"").unwrap();

        // 模拟「DDL 变了」：把指纹篡改成别的值 → 触发重建
        connection
            .execute(
                "UPDATE app_meta SET value = 'deadbeefdeadbeef' WHERE key = ?1",
                [CATALOG_SCHEMA_FINGERPRINT_META_KEY],
            )
            .unwrap();

        clear_legacy_catalog_if_needed(&mut connection).unwrap();

        let etag: String = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key = ?1",
                [MODELS_DEV_ETAG_META_KEY],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            etag.trim().is_empty(),
            "重建后 ETag 必须被作废，否则会与已丢失的正文脱节（实际 {etag:?}）"
        );
    }

    /// 「有 ETag 没正文」时绝不能发条件请求——那等于自断后路。
    #[test]
    fn conditional_request_requires_cached_body() {
        // 正常：ETag 与正文都在 → 可以发条件请求
        assert_eq!(conditional_etag_for("W/\"abc\"", true), Some("W/\"abc\""));
        // 孤儿 ETag：正文丢了 → 必须退化为无条件请求
        assert_eq!(conditional_etag_for("W/\"abc\"", false), None);
        // 无 ETag → 无条件请求
        assert_eq!(conditional_etag_for("", true), None);
        assert_eq!(conditional_etag_for("   ", true), None);
        assert_eq!(conditional_etag_for("", false), None);
    }

    /// 写入-读回往返测试。
    ///
    /// 一次性锁住「INSERT 列数 / 占位符个数 / 参数个数 / SELECT 下标」四者的对齐——
    /// 这类错位**编译期发现不了**：
    /// - 占位符多了会运行时报 `75 values for 72 columns`；
    /// - 而 read 下标错位会**静默读到错误的值**（比如把 `has_fast_mode` 读成
    ///   `max_input_tokens`），后者更危险，只有往返比对才能发现。
    #[test]
    fn model_row_round_trips_through_database() {
        use crate::model::catalog::models_dev::build_index;

        // ── 1. 造一个最小 models.dev 索引（canonical + provider 两层）──
        let models_dev_raw = json!({
            "models": {
                "acme/rocket-1": {
                    "id": "acme/rocket-1",
                    "name": "Rocket 1",
                    "description": "A round-trip test model.",
                    "license": "MIT",
                    "attachment": true,
                    "reasoning": true,
                    "tool_call": true,
                    "release_date": "2026-01-01",
                    "last_updated": "2026-02-01",
                    "modalities": { "input": ["text", "image"], "output": ["text", "image"] },
                    "open_weights": true,
                    "limit": { "context": 200000, "input": 150000, "output": 32000 },
                    "links": [{ "label": "Paper", "url": "https://example.com/p", "type": "paper" }],
                    "weights": [{ "label": "Hugging Face", "url": "https://huggingface.co/acme" }],
                    "benchmarks": [{ "name": "MMLU", "score": 88.5, "metric": "accuracy" }]
                }
            },
            "providers": {
                "acme": {
                    "id": "acme",
                    "name": "Acme",
                    "env": ["ACME_API_KEY"],
                    "npm": "@ai-sdk/openai-compatible",
                    "api": "https://api.acme.dev/v1",
                    "doc": "https://docs.acme.dev",
                    "models": {
                        "rocket-1": {
                            "id": "rocket-1",
                            "name": "Rocket 1",
                            "attachment": true,
                            "reasoning": true,
                            "tool_call": true,
                            "release_date": "2026-01-01",
                            "last_updated": "2026-02-01",
                            "modalities": { "input": ["text", "image"], "output": ["text", "image"] },
                            "open_weights": true,
                            "limit": { "context": 200000, "input": 150000, "output": 32000 },
                            "cost": { "input": 1.0, "output": 4.0, "cache_read": 0.25 },
                            "reasoning_options": [
                                { "type": "toggle" },
                                { "type": "effort", "values": ["high", "low", "medium"] }
                            ],
                            "interleaved": { "field": "reasoning_content" },
                            "experimental": {
                                "modes": { "fast": { "cost": { "input": 2.0, "output": 8.0 } } }
                            }
                        }
                    }
                }
            }
        });
        let index = build_index(&models_dev_raw).expect("构建 models.dev 索引失败");

        // ── 2. 落库 ──
        let dir = std::env::temp_dir().join("openhub_catalog_round_trip");
        std::fs::create_dir_all(&dir).expect("创建临时目录失败");
        let db_path = dir.join("round_trip.sqlite");
        let _ = std::fs::remove_file(&db_path);
        let database = Database::open(&db_path).expect("打开库失败");

        let manifest = json!({
            "providers": {
                "acme": { "name": "Acme", "tier": "lab", "subscription": false,
                          "doc": "https://docs.acme.dev" }
            }
        });
        let shard_rows = json!([{
            "id": "acme/rocket-1",
            "slug": "rocket1",
            "name": "Rocket 1",
            "lab": "acme",
            "kind": "text",
            "family": "rocket",
            "knowledge": "2025-12",
            "status": "ga",
            "openWeights": true,
            "reasoning": true,
            "toolCall": true,
            "attachment": true,
            "structured": true,
            "temperature": true,
            "inputModalities": ["text", "image"],
            "context": 200000,
            "contextRange": [32000, 200000],
            "outputLimit": 32000,
            "ref": { "provider": "acme", "input": 1.0, "output": 4.0, "cacheRead": 0.25 },
            "refOfficial": true,
            "min": { "provider": "acme", "input": 1.0, "output": 4.0, "cacheRead": 0.25 },
            "spread": 1.0,
            "blendedMin": 1.75,
            "blendedTrusted": 1.75,
            "blendedRef": 1.75,
            "hostCount": 1,
            "pricedHostCount": 1,
            "freeHostCount": 0,
            "subHostCount": 0,
            "hostProviders": ["acme"],
            "benchmarkCount": 1,
            "releaseDate": "2026-01-01",
            "lastUpdated": "2026-02-01"
        }]);
        let shards = vec![(
            "rows-000.json".to_string(),
            "https://llmpricing.dev/rows/rows-000.json".to_string(),
            shard_rows,
        )];

        persist_catalog_llmpricing(&database, "{}", &manifest, &shards, Some(&index))
            .expect("落库失败");

        // ── 3. 读回并逐字段比对 ──
        let snapshot = get_model_catalog_inner(&database).expect("读取失败");
        let model = snapshot
            .models
            .iter()
            .find(|m| m.id == "acme/rocket-1")
            .expect("读不到刚写入的模型");

        assert_eq!(model.reasoning_options.len(), 2, "思考级别选项数不对");
        assert_eq!(model.reasoning_options[0].kind, "toggle");
        assert_eq!(model.reasoning_options[1].kind, "effort");
        assert_eq!(
            model.reasoning_options[1].values,
            vec!["low", "medium", "high"],
            "档位应按从低到高排序"
        );
        assert_eq!(model.reasoning_effort_max.as_deref(), Some("high"));
        assert_eq!(model.output_modalities, vec!["text", "image"]);
        assert_eq!(model.max_input_tokens, Some(150000));
        assert_eq!(model.interleaved_fields, vec!["reasoning_content"]);
        assert!(model.has_fast_mode, "应识别到快速模式");

        let extras = &model.models_dev_extras;
        assert_eq!(extras.description.as_deref(), Some("A round-trip test model."));
        assert_eq!(extras.license.as_deref(), Some("MIT"));
        assert_eq!(extras.links.len(), 1);
        assert_eq!(extras.weights.len(), 1);
        assert_eq!(extras.benchmarks.len(), 1);

        // 渠道级字段（走 hosts_json）
        let detail = rt_block_on(get_model_catalog_detail_inner(&database, "acme/rocket-1"))
            .expect("读取详情失败");
        let host = detail.hosts.first().expect("渠道明细为空");
        assert_eq!(host.input_limit, Some(150000), "渠道级输入上限丢失");
        assert_eq!(host.output_modalities, vec!["text", "image"]);
        assert_eq!(host.reasoning_options.len(), 2);
        assert_eq!(host.interleaved_field.as_deref(), Some("reasoning_content"));
        assert!(host.fast_mode.is_some(), "渠道级快速模式丢失");
        assert_eq!(host.cache_read, Some(0.25));

        // 供应商元数据（走 env_json）
        let provider = snapshot
            .providers
            .iter()
            .find(|p| p.id == "acme")
            .expect("读不到供应商");
        assert_eq!(provider.env, vec!["ACME_API_KEY"]);
        // `acme` 不在原厂别名表里 → **刻意不猜**，判为非原厂渠道。
        // 这正是「不确定就不猜」原则的体现：宁可标 false，也不臆造官方身份。
        assert!(
            !provider.is_first_party,
            "不在原厂别名表里的渠道不应被当作原厂"
        );
    }

    /// 指纹必须对 DDL 内容敏感：改动 DDL 而未 bump 版本号也要能识别。
    #[test]
    fn schema_fingerprint_changes_with_ddl() {
        let current = catalog_schema_fingerprint();
        assert_eq!(current.len(), 16, "指纹应为 16 位十六进制");
        assert_eq!(current, catalog_schema_fingerprint(), "指纹必须稳定");

        // 模拟「DDL 变了」：同样长度的字符串指纹也必须不同
        let mutated = CATALOG_SCHEMA_DDL.replace("official_lab", "official_labs");
        assert_ne!(mutated, CATALOG_SCHEMA_DDL, "替换应生效");
        let mutated_fingerprint = {
            const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
            const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
            let mut hash = FNV_OFFSET;
            for byte in mutated.as_bytes() {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(FNV_PRIME);
            }
            format!("{hash:016x}")
        };
        assert_ne!(mutated_fingerprint, current, "DDL 变化后指纹必须变化");
    }

    // ─────────────── capabilities_for_keys：逐模型能力解析（Agent 配置页默认参数来源） ───────────────

    /// 构造一个最小目录条目，只需 `id` / `official_model_id` / 能力字段。
    fn capability_item(id: &str, official_model_id: &str, context: i64, output: i64) -> ModelCatalogItem {
        ModelCatalogItem {
            id: id.to_string(),
            slug: String::new(),
            name: id.to_string(),
            lab: String::new(),
            kind: "text".to_string(),
            family: None,
            knowledge: None,
            status: "ga".to_string(),
            open_weights: false,
            reasoning: true,
            tool_call: true,
            attachment: false,
            structured: false,
            temperature: true,
            input_modalities: vec!["text".to_string()],
            context_length: context,
            context_min: 0,
            context_max: 0,
            max_output_tokens: output,
            ref_provider: None,
            ref_official: false,
            ref_input_cost: 0.0,
            ref_output_cost: 0.0,
            ref_cache_read_cost: 0.0,
            min_provider: None,
            min_input_cost: 0.0,
            min_output_cost: 0.0,
            min_cache_read_cost: 0.0,
            price_spread: 0.0,
            blended_min: None,
            blended_trusted: None,
            blended_ref: None,
            host_count: 0,
            priced_host_count: 0,
            free_host_count: 0,
            sub_host_count: 0,
            host_providers: Vec::new(),
            aa_idx: None,
            aa_coding: None,
            aa_agentic: None,
            aa_speed: None,
            aa_ttft: None,
            aa_task_cost: None,
            benchmark_count: 0,
            release_date: None,
            last_updated: None,
            official_lab: String::new(),
            official_model_id: official_model_id.to_string(),
            canonical_id: None,
            identity_source: "test".to_string(),
            identity_resolved: true,
            official_host_count: 0,
            official_channel_providers: Vec::new(),
            lab_tier_host_count: 0,
            cloud_tier_host_count: 0,
            gateway_tier_host_count: 0,
            free_channel_count: 0,
            free_channel_providers: Vec::new(),
            subscription_channel_providers: Vec::new(),
            free_channel_source: String::new(),
            free_channel_count_matches: false,
            reasoning_options: vec![ReasoningOption {
                kind: "effort".to_string(),
                values: vec!["low".into(), "high".into()],
            }],
            reasoning_effort_max: Some("high".to_string()),
            output_modalities: vec!["text".to_string()],
            max_input_tokens: None,
            interleaved_fields: Vec::new(),
            has_fast_mode: false,
            models_dev_extras: Default::default(),
        }
    }

    #[test]
    fn capabilities_resolve_variant_and_alias() {
        let models = vec![
            capability_item(
                "anthropic/claude-sonnet-4-5", "claude-sonnet-4-5", 200_000, 64_000,
            ),
            capability_item("openai/gpt-5-codex", "gpt-5-codex", 400_000, 128_000),
            capability_item("zhipuai/glm-5.2", "z-ai/glm-5.2", 200_000, 32_000),
            capability_item("meta/llama-3.3-70b", "llama-3.3-70b", 128_000, 8_000),
        ];
        let cases = [
            ("claude-sonnet-4-5-20250929", 0, "variant"),
            ("gpt-5-codex-high", 1, "variant"),
            ("zai-org-glm-5-2", 2, "alias"),
            ("llama3.3:70b", 3, "variant"),
            ("@cf/meta/llama-3.3-70b", 3, "alias"),
        ];
        let keys = cases
            .iter()
            .map(|(key, _, _)| key.to_string())
            .collect::<Vec<_>>();
        let out = capabilities_for_keys(&models, &keys);
        assert_eq!(out.len(), cases.len());
        for (key, index, kind) in cases {
            let value = &out[key];
            assert_eq!(value.matched_id, models[index].id, "{key}");
            assert_eq!(value.context_length, models[index].context_length);
            assert_eq!(value.max_output_tokens, models[index].max_output_tokens);
            assert_eq!(value.match_kind, kind);
            assert_eq!(value.match_score, 1.0);
            assert_eq!(value.match_version, 2);
        }
        assert!(capabilities_for_keys(&[], &keys).is_empty());
    }

    #[test]
    fn capabilities_strong_exact_keeps_each_models_specs() {
        let mut models = vec![
            capability_item("acme/rocket", "rocket", 100_000, 4_000),
            capability_item("acme/rocket-thinking", "rocket-thinking", 200_000, 16_000),
            capability_item("acme/rocket-turbo", "rocket-turbo", 50_000, 2_000),
        ];
        let keys = ["rocket", " ROCKET-THINKING ", "acme/rocket-turbo"].map(String::from);
        for _ in 0..2 {
            let out = capabilities_for_keys(&models, &keys);
            for (key, id, context, output) in [
                (keys[0].as_str(), "acme/rocket", 100_000, 4_000),
                (keys[1].as_str(), "acme/rocket-thinking", 200_000, 16_000),
                (keys[2].as_str(), "acme/rocket-turbo", 50_000, 2_000),
            ] {
                let value = &out[key];
                assert_eq!(value.matched_id, id);
                assert_eq!(value.context_length, context);
                assert_eq!(value.max_output_tokens, output);
                assert_eq!(value.match_kind, "exact");
            }
            models.reverse();
        }
    }

    #[test]
    fn capabilities_reject_bare_collisions_but_keep_qualified_ids() {
        let mut models = vec![
            capability_item("acme/rocket", "rocket", 100_000, 4_000),
            capability_item("other/rocket", "rocket", 200_000, 8_000),
        ];
        let keys = [
            "rocket", "rocket-high", "acme/rocket", "other/rocket", "unknown/rocket",
        ].map(String::from);
        for _ in 0..2 {
            let out = capabilities_for_keys(&models, &keys);
            assert_eq!(out.len(), 2);
            assert_eq!(out["acme/rocket"].context_length, 100_000);
            assert_eq!(out["other/rocket"].context_length, 200_000);
            models.reverse();
        }
    }

    #[test]
    fn capabilities_do_not_guess_when_ambiguous() {
        // 同一已知快照的两条目录记录；单独各自都能通过 L3（分数 52/54 > .90）。
        // 合并后才拒绝，验证的是唯一性而非分数门槛或身份守卫。
        let mut models = vec![
            capability_item(
                "anthropic/claude-sonnet-4-5-20250929", "claude-sonnet-4-5-20250929",
                200_000, 64_000,
            ),
            capability_item(
                "other/claude-sonnet-4-5-20250929", "claude-sonnet-4-5-20250929",
                100_000, 32_000,
            ),
        ];
        let key = "claude-sonnet-4-5-2025-09-29".to_string();
        for model in &models {
            let out = capabilities_for_keys(
                std::slice::from_ref(model), std::slice::from_ref(&key),
            );
            let value = &out[&key];
            assert_eq!(value.match_kind, "nearest");
            assert!(value.match_score >= 0.90);
            assert!((value.match_score - 52.0 / 54.0).abs() < 0.0001);
        }
        for _ in 0..2 {
            assert!(capabilities_for_keys(&models, std::slice::from_ref(&key)).is_empty());
            models.reverse();
        }
    }

    #[test]
    fn capabilities_reject_identity_siblings_including_long_names() {
        for (candidate, request) in [
            ("deepseek-v4", "deepseek-v4-flash"),
            ("llama-3.3-70b", "llama3.3:8b"),
            ("llama-3.3-70b", "llama-3-70b"),
            (
                "long-model-series-release-3-4-70b",
                "long-model-series-release-4-3-70b",
            ),
            (
                "long-model-series-release-3-3-70b",
                "long-model-series-release-3-4-70b",
            ),
            (
                "long-model-series-release-3-3-70b",
                "other-model-series-release-3-3-70b",
            ),
        ] {
            let model = capability_item(&format!("acme/{candidate}"), candidate, 100_000, 4_000);
            assert!(
                capabilities_for_keys(&[model], &[request.to_string()]).is_empty(),
                "{request}"
            );
        }
        let base = "acme-super-long-model-family-series-release-3-3-70b";
        let model = capability_item(&format!("acme/{base}"), base, 100_000, 4_000);
        let keys = [
            "flash", "mini", "codex", "coder", "pro", "max", "fast", "turbo",
            "thinking", "instruct", "preview", "fp8", "extra",
        ].map(|suffix| format!("{base}-{suffix}"));
        assert!(capabilities_for_keys(&[model], &keys).is_empty());
    }

    #[test]
    fn capabilities_serialize_all_fields_and_match_metadata() {
        for (id, official, request, kind) in [
            (
                "acme/rocket-thinking", "rocket-thinking", " ROCKET-THINKING ", "exact",
            ),
            (
                "openai/gpt-5-codex", "gpt-5-codex", "gpt-5-codex-high", "variant",
            ),
            (
                "zhipuai/glm-5.2", "z-ai/glm-5.2", "zai-org-glm-5-2", "alias",
            ),
            (
                "anthropic/claude-sonnet-4-5-20250929", "claude-sonnet-4-5-20250929",
                "claude-sonnet-4-5-2025-09-29", "nearest",
            ),
        ] {
            for enabled in [true, false] {
                let mut model = capability_item(id, official, 321_000, 12_345);
                model.reasoning_options.push(ReasoningOption {
                    kind: "toggle".into(),
                    values: vec![],
                });
                model.reasoning_effort_max = enabled.then(|| "high".to_string());
                model.max_input_tokens = enabled.then_some(300_000);
                model.output_modalities = vec!["text".into(), "audio".into()];
                model.interleaved_fields = vec!["reasoning_content".into()];
                model.has_fast_mode = enabled;
                model.temperature = !enabled;
                model.tool_call = enabled;
                model.structured = !enabled;
                model.reasoning = enabled;
                model.open_weights = !enabled;
                let out = capabilities_for_keys(&[model], &[request.to_string(), "absent".into()]);
                assert_eq!(out.len(), 1);
                let score = if kind == "nearest" { 52.0_f32 / 54.0 } else { 1.0 };
                let expected = json!({
                    "reasoningOptions": [
                        { "kind": "effort", "values": ["low", "high"] },
                        { "kind": "toggle", "values": [] }
                    ],
                    "reasoningEffortMax": enabled.then_some("high"),
                    "contextLength": 321_000,
                    "maxOutputTokens": 12_345,
                    "maxInputTokens": enabled.then_some(300_000),
                    "outputModalities": ["text", "audio"],
                    "interleavedFields": ["reasoning_content"],
                    "hasFastMode": enabled,
                    "supportsTemperature": !enabled,
                    "supportsToolCall": enabled,
                    "supportsStructuredOutput": !enabled,
                    "supportsReasoning": enabled,
                    "openWeights": !enabled,
                    "matchedId": id,
                    "matchedModelId": official,
                    "matchKind": kind,
                    "matchScore": score,
                    "matchVersion": 2
                });
                let serialized = serde_json::to_value(&out).unwrap();
                assert_eq!(serialized[request], expected, "{kind}");
                assert_eq!(serialized.as_object().unwrap().len(), 1);
            }
        }
    }

    /// 库结构已是最新（版本号与指纹都匹配）时，**不应**无谓重建。
    /// 与下一条测试配对，确保「该重建时重建、不该重建时不动」。
    #[test]
    fn up_to_date_schema_is_not_rebuilt() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        clear_legacy_catalog_if_needed(&mut connection).unwrap();

        connection
            .execute(
                "INSERT INTO model_catalog_models (id, name) VALUES ('sentinel/model', '哨兵')",
                [],
            )
            .unwrap();

        clear_legacy_catalog_if_needed(&mut connection).unwrap();

        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM model_catalog_models", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "结构已最新时不应清空数据");
    }

    /// 模拟「DDL 改了但版本号没 bump」：版本号匹配、指纹不匹配 → 必须重建。
    ///
    /// 无法在运行时真的改 DDL，因此把落库的指纹篡改成别的值——
    /// 对 `clear_legacy_catalog_if_needed` 而言与「DDL 变了」等价。
    #[test]
    fn changed_ddl_with_same_version_triggers_rebuild() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        clear_legacy_catalog_if_needed(&mut connection).unwrap();
        connection
            .execute(
                "INSERT INTO model_catalog_models (id, name) VALUES ('sentinel/model', '哨兵')",
                [],
            )
            .unwrap();

        // 前提：版本号与当前一致，只有指纹对不上
        let version: String = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key = ?1",
                [CATALOG_SCHEMA_META_KEY],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, CATALOG_SCHEMA_VERSION);
        connection
            .execute(
                "UPDATE app_meta SET value = 'deadbeefdeadbeef' WHERE key = ?1",
                [CATALOG_SCHEMA_FINGERPRINT_META_KEY],
            )
            .unwrap();

        clear_legacy_catalog_if_needed(&mut connection).unwrap();

        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM model_catalog_models", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "DDL 变化（指纹不匹配）时必须重建");

        // 指纹应被回写为当前值，避免每次都重建
        let fingerprint: String = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key = ?1",
                [CATALOG_SCHEMA_FINGERPRINT_META_KEY],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fingerprint, catalog_schema_fingerprint());
    }

    /// 回归：`persist_catalog_llmpricing` 不能抹掉 models.dev 的来源行。
    ///
    /// `fetch_models_dev_index` 先于 persist 执行并写入该行；旧实现里 persist 无条件
    /// `DELETE FROM model_catalog_sources`，刚写入的 models.dev 行被立刻删除，
    /// 前端「数据来源」面板于是永远看不到主数据源。
    #[test]
    fn persist_preserves_models_dev_source_row() {
        let dir = std::env::temp_dir().join("openhub_catalog_source_row");
        std::fs::create_dir_all(&dir).expect("创建临时目录失败");
        let db_path = dir.join("source_row.sqlite");
        let _ = std::fs::remove_file(&db_path);
        let database = Database::open(&db_path).expect("打开库失败");

        {
            let mut connection = database.lock_conn().unwrap();
            clear_legacy_catalog_if_needed(&mut connection).unwrap();
            connection
                .execute(
                    "INSERT INTO model_catalog_sources (source, url, fetched_at, record_count, raw_json)
                     VALUES (?1, ?2, '2026-01-01T00:00:00.000Z', 395, '{}')",
                    params![MODELS_DEV_SOURCE, MODELS_DEV_CATALOG_URL],
                )
                .unwrap();
        }

        // 空 manifest + 空分片：只关心 sources 表的清理行为
        let report = persist_catalog_llmpricing(
            &database,
            "{}",
            &json!({}),
            &[],
            None,
        )
        .expect("persist 失败");
        assert_eq!(report.model_count, 0);

        let connection = database.lock_conn().unwrap();
        let (count, fetched_at): (i64, String) = connection
            .query_row(
                "SELECT COUNT(*), COALESCE(MAX(fetched_at), '') FROM model_catalog_sources WHERE source = ?1",
                [MODELS_DEV_SOURCE],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 1, "models.dev 来源行被 persist 抹掉了");
        assert_eq!(
            fetched_at, "2026-01-01T00:00:00.000Z",
            "fetched_at 表示最后一次联网抓取时间，不应被 persist 改写"
        );
    }
}
