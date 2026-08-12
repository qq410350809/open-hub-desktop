use crate::db::build_http_client;
use crate::models::Database;
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};

const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/models?output_modalities=all";
const LITELLM_URL: &str =
    "https://cdn.jsdelivr.net/gh/BerriAI/litellm@main/model_prices_and_context_window.json";
const REQUIRED_SOURCES: [&str; 2] = ["openrouter", "litellm"];
const CATALOG_SCHEMA_VERSION: &str = "6";
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelCatalogSourceStatus {
    source: String,
    url: String,
    fetched_at: String,
    record_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelCatalogItem {
    canonical_key: String,
    display_name: String,
    manufacturer: String,
    mode: String,
    context_length: i64,
    max_input_tokens: i64,
    max_output_tokens: i64,
    input_cost_per_token: f64,
    output_cost_per_token: f64,
    cache_read_cost_per_token: f64,
    cache_write_cost_per_token: f64,
    image_cost: f64,
    audio_input_cost_per_token: f64,
    audio_output_cost_per_token: f64,
    request_cost: f64,
    capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelCatalogSnapshot {
    models: Vec<ModelCatalogItem>,
    total: usize,
    last_synced_at: String,
    synced_today: bool,
    sources: Vec<ModelCatalogSourceStatus>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelCatalogEntryDetail {
    source: String,
    source_model_id: String,
    channel: String,
    mode: String,
    display_name: String,
    context_length: i64,
    max_input_tokens: i64,
    max_output_tokens: i64,
    input_cost_per_token: f64,
    output_cost_per_token: f64,
    cache_read_cost_per_token: f64,
    cache_write_cost_per_token: f64,
    image_cost: f64,
    audio_input_cost_per_token: f64,
    audio_output_cost_per_token: f64,
    request_cost: f64,
    raw: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelCatalogDetail {
    model: ModelCatalogItem,
    pricing: Value,
    entries: Vec<ModelCatalogEntryDetail>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelCatalogSyncResult {
    synced: bool,
    skipped: bool,
    message: String,
    snapshot: ModelCatalogSnapshot,
}

#[derive(Debug, Clone, Default)]
struct NormalizedPricing {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
    image: f64,
    audio_input: f64,
    audio_output: f64,
    request: f64,
}

#[derive(Debug, Clone, Default)]
struct PricingPresence {
    input: bool,
    output: bool,
    cache_read: bool,
    cache_write: bool,
    image: bool,
    audio_input: bool,
    audio_output: bool,
    request: bool,
}

impl PricingPresence {
    fn merge(&mut self, other: &Self) {
        self.input |= other.input;
        self.output |= other.output;
        self.cache_read |= other.cache_read;
        self.cache_write |= other.cache_write;
        self.image |= other.image;
        self.audio_input |= other.audio_input;
        self.audio_output |= other.audio_output;
        self.request |= other.request;
    }
}

impl NormalizedPricing {
    fn merge_max(&mut self, other: &Self) {
        self.input = self.input.max(other.input);
        self.output = self.output.max(other.output);
        self.cache_read = self.cache_read.max(other.cache_read);
        self.cache_write = self.cache_write.max(other.cache_write);
        self.image = self.image.max(other.image);
        self.audio_input = self.audio_input.max(other.audio_input);
        self.audio_output = self.audio_output.max(other.audio_output);
        self.request = self.request.max(other.request);
    }

    fn fill_missing(&mut self, supplement: &Self, presence: &PricingPresence) {
        if !presence.input {
            self.input = supplement.input;
        }
        if !presence.output {
            self.output = supplement.output;
        }
        if !presence.cache_read {
            self.cache_read = supplement.cache_read;
        }
        if !presence.cache_write {
            self.cache_write = supplement.cache_write;
        }
        if !presence.image {
            self.image = supplement.image;
        }
        if !presence.audio_input {
            self.audio_input = supplement.audio_input;
        }
        if !presence.audio_output {
            self.audio_output = supplement.audio_output;
        }
        if !presence.request {
            self.request = supplement.request;
        }
    }

    fn as_json(&self) -> Value {
        json!({
            "inputCostPerToken": self.input,
            "outputCostPerToken": self.output,
            "cacheReadCostPerToken": self.cache_read,
            "cacheWriteCostPerToken": self.cache_write,
            "imageCost": self.image,
            "audioInputCostPerToken": self.audio_input,
            "audioOutputCostPerToken": self.audio_output,
            "requestCost": self.request,
        })
    }
}

#[derive(Debug, Clone)]
struct CatalogEntry {
    source: String,
    source_model_id: String,
    canonical_key: String,
    display_name: String,
    manufacturer: String,
    channel: String,
    mode: String,
    context_length: i64,
    max_input_tokens: i64,
    max_output_tokens: i64,
    pricing: NormalizedPricing,
    capabilities: BTreeSet<String>,
    raw: Value,
}

#[derive(Debug, Clone)]
struct AggregateModel {
    canonical_key: String,
    display_name: String,
    manufacturer: String,
    mode: String,
    context_length: i64,
    max_input_tokens: i64,
    max_output_tokens: i64,
    pricing: NormalizedPricing,
    capabilities: BTreeSet<String>,
    source_ids: BTreeSet<String>,
    sources: BTreeSet<String>,
    channels: BTreeSet<String>,
    variant_count: usize,
    display_name_rank: u8,
    openrouter_presence: OpenRouterPresence,
}

#[derive(Debug, Clone, Default)]
struct OpenRouterPresence {
    context_length: bool,
    max_input_tokens: bool,
    max_output_tokens: bool,
    pricing: PricingPresence,
    capabilities: bool,
}

impl OpenRouterPresence {
    fn merge(&mut self, other: &Self) {
        self.context_length |= other.context_length;
        self.max_input_tokens |= other.max_input_tokens;
        self.max_output_tokens |= other.max_output_tokens;
        self.pricing.merge(&other.pricing);
        self.capabilities |= other.capabilities;
    }
}

#[derive(Debug, Clone, Default)]
struct ModelSupplement {
    display_name: String,
    mode: String,
    context_length: i64,
    max_input_tokens: i64,
    max_output_tokens: i64,
    pricing: NormalizedPricing,
    capabilities: BTreeSet<String>,
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

fn text(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn strip_known_variant_suffix(mut value: String) -> String {
    loop {
        let lower = value.to_ascii_lowercase();
        let suffix = [":free", ":batch", ":thinking", ":online"]
            .into_iter()
            .find(|suffix| lower.ends_with(suffix));
        if let Some(suffix) = suffix {
            value.truncate(value.len() - suffix.len());
        } else {
            break;
        }
    }
    value
}

fn clean_identifier(value: &str) -> String {
    let lowered = value
        .trim()
        .trim_start_matches('~')
        .to_ascii_lowercase()
        .replace('\\', "/");
    let compact = lowered
        .split('/')
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("/");
    strip_known_variant_suffix(compact)
}

fn canonical_vendor(value: &str) -> String {
    match value {
        "xai" => "x-ai".into(),
        "google_genai" | "gemini" | "vertex_ai" => "google".into(),
        "dashscope" => "qwen".into(),
        "meta" | "meta_llama" => "meta-llama".into(),
        "deepseek-ai" => "deepseek".into(),
        "zai" | "z-ai" => "z-ai".into(),
        "mistral" => "mistralai".into(),
        other => other.to_string(),
    }
}

fn model_leaf(value: &str) -> String {
    clean_identifier(value)
        .split('/')
        .next_back()
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn openrouter_key(id: &str) -> String {
    let cleaned = clean_identifier(id);
    let mut parts = cleaned.split('/').map(str::to_string).collect::<Vec<_>>();
    if let Some(first) = parts.first_mut() {
        *first = canonical_vendor(first);
    }
    parts.join("/")
}

fn litellm_key(
    source_id: &str,
    openrouter_keys: &HashSet<String>,
    openrouter_leafs: &HashMap<String, Vec<String>>,
) -> Option<String> {
    let cleaned = clean_identifier(source_id);
    let parts = cleaned.split('/').collect::<Vec<_>>();

    // 精确匹配：去掉任意前缀的 LiteLLM ID 片段，若与某个 OpenRouter canonical_key 一致即命中。
    for start in 0..parts.len() {
        let candidate = parts[start..].join("/");
        if openrouter_keys.contains(&candidate) {
            return Some(candidate);
        }
    }

    // 去掉厂商标识的 leaf 匹配：OpenRouter 里去掉厂商前缀的 ID 与 LiteLLM 的 leaf 一致时，
    // 仅在唯一对应时用以完善该 OpenRouter 模型的缺失字段；否则无法可靠归并。
    let leaf = model_leaf(&cleaned);
    if let Some(matches) = openrouter_leafs.get(&leaf) {
        if matches.len() == 1 {
            return Some(matches[0].clone());
        }
    }

    None
}

fn openrouter_mode(record: &Map<String, Value>) -> String {
    let architecture = record.get("architecture").and_then(Value::as_object);
    let output = architecture
        .and_then(|value| value.get("output_modalities"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let input = architecture
        .and_then(|value| value.get("input_modalities"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let output_values = output.iter().filter_map(Value::as_str).collect::<Vec<_>>();
    let input_values = input.iter().filter_map(Value::as_str).collect::<Vec<_>>();
    if output_values.contains(&"image") {
        "image_generation".into()
    } else if output_values.contains(&"audio") {
        "audio_speech".into()
    } else if output_values.contains(&"embedding") {
        "embedding".into()
    } else if input_values.contains(&"text") || output_values.contains(&"text") {
        "chat".into()
    } else {
        text(architecture.and_then(|value| value.get("modality")))
    }
}

fn openrouter_entry(record: &Map<String, Value>) -> Option<CatalogEntry> {
    let source_model_id = text(record.get("id"));
    if source_model_id.is_empty() {
        return None;
    }
    // OpenRouter API 也包含 auto/free/fusion/bodybuilder 等路由器产品。
    // 它们不是具体模型，不进入模型列表；完整来源 JSON 仍会原样保存。
    if clean_identifier(&source_model_id).starts_with("openrouter/") {
        return None;
    }
    let alias_slug = record
        .get("alias_target")
        .and_then(Value::as_object)
        .and_then(|value| value.get("slug"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let canonical_key = openrouter_key(if alias_slug.is_empty() {
        &source_model_id
    } else {
        alias_slug
    });
    let pricing_value = record.get("pricing").and_then(Value::as_object);
    let pricing = NormalizedPricing {
        input: numeric(pricing_value.and_then(|value| value.get("prompt"))),
        output: numeric(pricing_value.and_then(|value| value.get("completion"))),
        cache_read: numeric(pricing_value.and_then(|value| value.get("input_cache_read"))),
        cache_write: numeric(pricing_value.and_then(|value| value.get("input_cache_write"))).max(
            numeric(pricing_value.and_then(|value| value.get("input_cache_write_1h"))),
        ),
        image: numeric(pricing_value.and_then(|value| value.get("image"))).max(numeric(
            pricing_value.and_then(|value| value.get("image_output")),
        )),
        audio_input: numeric(pricing_value.and_then(|value| value.get("audio"))),
        audio_output: numeric(pricing_value.and_then(|value| value.get("audio_output"))),
        request: numeric(pricing_value.and_then(|value| value.get("request"))).max(numeric(
            pricing_value.and_then(|value| value.get("web_search")),
        )),
    };
    let mut capabilities = BTreeSet::new();
    if let Some(architecture) = record.get("architecture").and_then(Value::as_object) {
        for key in ["input_modalities", "output_modalities"] {
            if let Some(values) = architecture.get(key).and_then(Value::as_array) {
                for value in values.iter().filter_map(Value::as_str) {
                    capabilities.insert(format!("{key}:{value}"));
                }
            }
        }
    }
    if let Some(values) = record.get("supported_parameters").and_then(Value::as_array) {
        for value in values.iter().filter_map(Value::as_str) {
            capabilities.insert(value.to_string());
        }
    }
    let manufacturer = canonical_key
        .split('/')
        .next()
        .map(canonical_vendor)
        .unwrap_or_else(|| "unknown".into());
    let top_provider = record.get("top_provider").and_then(Value::as_object);
    Some(CatalogEntry {
        source: "openrouter".into(),
        source_model_id,
        canonical_key,
        display_name: text(record.get("name")),
        manufacturer,
        channel: "openrouter".into(),
        mode: openrouter_mode(record),
        context_length: integer(record.get("context_length")),
        max_input_tokens: integer(record.get("context_length")),
        max_output_tokens: integer(
            top_provider.and_then(|value| value.get("max_completion_tokens")),
        ),
        pricing,
        capabilities,
        raw: Value::Object(record.clone()),
    })
}

fn litellm_entry(
    source_model_id: &str,
    record: &Map<String, Value>,
    openrouter_keys: &HashSet<String>,
    openrouter_leafs: &HashMap<String, Vec<String>>,
) -> Option<CatalogEntry> {
    if source_model_id == "sample_spec" {
        return None;
    }
    let channel = text(record.get("litellm_provider"));
    let canonical_key = litellm_key(source_model_id, openrouter_keys, openrouter_leafs)?;
    let leaf = model_leaf(source_model_id);
    let pricing = NormalizedPricing {
        input: numeric(record.get("input_cost_per_token")),
        output: numeric(record.get("output_cost_per_token")),
        cache_read: numeric(record.get("cache_read_input_token_cost"))
            .max(numeric(record.get("input_cost_per_token_cache_hit"))),
        cache_write: numeric(record.get("cache_creation_input_token_cost")),
        image: numeric(record.get("output_cost_per_image"))
            .max(numeric(record.get("input_cost_per_image")))
            .max(numeric(record.get("output_cost_per_image_token"))),
        audio_input: numeric(record.get("input_cost_per_audio_token")),
        audio_output: numeric(record.get("output_cost_per_audio_token")),
        request: numeric(record.get("input_cost_per_request"))
            .max(numeric(record.get("input_cost_per_query"))),
    };
    let mut capabilities = BTreeSet::new();
    for (key, value) in record {
        if key.starts_with("supports_") && value.as_bool() == Some(true) {
            capabilities.insert(key.trim_start_matches("supports_").to_string());
        }
    }
    let context_length = integer(record.get("max_tokens"))
        .max(integer(record.get("max_input_tokens")))
        .max(integer(record.get("max_output_tokens")));
    let manufacturer = canonical_key
        .split('/')
        .next()
        .map(canonical_vendor)
        .unwrap_or_else(|| "unknown".into());
    Some(CatalogEntry {
        source: "litellm".into(),
        source_model_id: source_model_id.to_string(),
        canonical_key,
        display_name: leaf,
        manufacturer,
        channel: if channel.is_empty() {
            "unknown".into()
        } else {
            channel
        },
        mode: text(record.get("mode")),
        context_length,
        max_input_tokens: integer(record.get("max_input_tokens")),
        max_output_tokens: integer(record.get("max_output_tokens")),
        pricing,
        capabilities,
        raw: Value::Object(record.clone()),
    })
}

fn build_entries(openrouter: &Value, litellm: &Value) -> Result<Vec<CatalogEntry>, String> {
    let openrouter_records = openrouter
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| "OpenRouter 数据缺少 data 数组".to_string())?;
    let litellm_records = litellm
        .as_object()
        .ok_or_else(|| "LiteLLM 数据不是对象".to_string())?;

    let mut entries = Vec::with_capacity(openrouter_records.len() + litellm_records.len());
    let mut openrouter_keys = HashSet::new();
    let mut openrouter_leafs: HashMap<String, Vec<String>> = HashMap::new();
    for value in openrouter_records {
        let Some(record) = value.as_object() else {
            continue;
        };
        if let Some(entry) = openrouter_entry(record) {
            openrouter_keys.insert(entry.canonical_key.clone());
            let matches = openrouter_leafs
                .entry(model_leaf(&entry.canonical_key))
                .or_default();
            if !matches.contains(&entry.canonical_key) {
                matches.push(entry.canonical_key.clone());
            }
            entries.push(entry);
        }
    }
    for (source_model_id, value) in litellm_records {
        let Some(record) = value.as_object() else {
            continue;
        };
        if let Some(entry) =
            litellm_entry(source_model_id, record, &openrouter_keys, &openrouter_leafs)
        {
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn concise_display_name(value: &str) -> String {
    let trimmed = value.trim();
    let without_vendor = trimmed
        .split_once(':')
        .filter(|(prefix, suffix)| {
            !prefix.trim().is_empty() && prefix.trim().len() <= 40 && !suffix.trim().is_empty()
        })
        .map(|(_, suffix)| suffix.trim())
        .unwrap_or(trimmed);
    let lower = without_vendor.to_ascii_lowercase();
    for suffix in [" (free)", " (batch)", " (thinking)", " (online)"] {
        if lower.ends_with(suffix) {
            return without_vendor[..without_vendor.len() - suffix.len()]
                .trim()
                .to_string();
        }
    }
    without_vendor.to_string()
}

fn openrouter_display_name_rank(entry: &CatalogEntry) -> u8 {
    let source_id = entry.source_model_id.trim().to_ascii_lowercase();
    if source_id.starts_with('~') {
        return 0;
    }
    if [":free", ":batch", ":thinking", ":online"]
        .iter()
        .any(|suffix| source_id.ends_with(suffix))
    {
        return 1;
    }
    if openrouter_key(&entry.source_model_id) == entry.canonical_key {
        3
    } else {
        2
    }
}

fn has_non_null(object: Option<&Map<String, Value>>, key: &str) -> bool {
    object
        .and_then(|value| value.get(key))
        .is_some_and(|value| !value.is_null())
}

fn openrouter_presence(entry: &CatalogEntry) -> OpenRouterPresence {
    let Some(record) = entry.raw.as_object() else {
        return OpenRouterPresence::default();
    };
    let pricing = record.get("pricing").and_then(Value::as_object);
    OpenRouterPresence {
        context_length: entry.context_length > 0,
        max_input_tokens: entry.max_input_tokens > 0,
        max_output_tokens: entry.max_output_tokens > 0,
        pricing: PricingPresence {
            input: has_non_null(pricing, "prompt"),
            output: has_non_null(pricing, "completion"),
            cache_read: has_non_null(pricing, "input_cache_read"),
            cache_write: has_non_null(pricing, "input_cache_write")
                || has_non_null(pricing, "input_cache_write_1h"),
            image: has_non_null(pricing, "image") || has_non_null(pricing, "image_output"),
            audio_input: has_non_null(pricing, "audio"),
            audio_output: has_non_null(pricing, "audio_output"),
            request: has_non_null(pricing, "request") || has_non_null(pricing, "web_search"),
        },
        capabilities: !entry.capabilities.is_empty(),
    }
}

fn aggregate_entries(entries: &[CatalogEntry]) -> BTreeMap<String, AggregateModel> {
    let mut models = BTreeMap::new();
    // OpenRouter 决定目录成员和所有可展示的模型语义。先独立聚合主数据，
    // 避免 LiteLLM 的部署名、渠道、类型或能力标签污染模型列表。
    for entry in entries.iter().filter(|entry| entry.source == "openrouter") {
        let model = models
            .entry(entry.canonical_key.clone())
            .or_insert_with(|| AggregateModel {
                canonical_key: entry.canonical_key.clone(),
                display_name: String::new(),
                manufacturer: entry.manufacturer.clone(),
                mode: entry.mode.clone(),
                context_length: 0,
                max_input_tokens: 0,
                max_output_tokens: 0,
                pricing: NormalizedPricing::default(),
                capabilities: BTreeSet::new(),
                source_ids: BTreeSet::new(),
                sources: BTreeSet::new(),
                channels: BTreeSet::new(),
                variant_count: 0,
                display_name_rank: 0,
                openrouter_presence: OpenRouterPresence::default(),
            });
        model.openrouter_presence.merge(&openrouter_presence(entry));
        let display_name_rank = openrouter_display_name_rank(entry);
        let display_name = concise_display_name(&entry.display_name);
        if !display_name.is_empty()
            && (model.display_name.is_empty() || display_name_rank > model.display_name_rank)
        {
            model.display_name = display_name;
            model.display_name_rank = display_name_rank;
        }
        if model.manufacturer.is_empty() || model.manufacturer == "unknown" {
            model.manufacturer = entry.manufacturer.clone();
        }
        if model.mode.is_empty() {
            model.mode = entry.mode.clone();
        }
        model.context_length = model.context_length.max(entry.context_length);
        model.max_input_tokens = model.max_input_tokens.max(entry.max_input_tokens);
        model.max_output_tokens = model.max_output_tokens.max(entry.max_output_tokens);
        model.pricing.merge_max(&entry.pricing);
        model
            .capabilities
            .extend(entry.capabilities.iter().cloned());
        model
            .source_ids
            .insert(format!("{}:{}", entry.source, entry.source_model_id));
        model.sources.insert(entry.source.clone());
        model.channels.insert(entry.channel.clone());
        model.variant_count += 1;
    }

    // 先汇总所有成功映射到 OpenRouter 模型的 LiteLLM 记录。这里的最高值
    // 只用于多个 LiteLLM 候选之间选出补充值，绝不会覆盖 OpenRouter 已有字段。
    let mut supplements: BTreeMap<String, ModelSupplement> = BTreeMap::new();
    for entry in entries.iter().filter(|entry| entry.source == "litellm") {
        let supplement = supplements.entry(entry.canonical_key.clone()).or_default();
        if supplement.display_name.is_empty() && !entry.display_name.trim().is_empty() {
            supplement.display_name = concise_display_name(&entry.display_name);
        }
        if supplement.mode.is_empty() && !entry.mode.trim().is_empty() {
            supplement.mode = entry.mode.clone();
        }
        supplement.context_length = supplement.context_length.max(entry.context_length);
        supplement.max_input_tokens = supplement.max_input_tokens.max(entry.max_input_tokens);
        supplement.max_output_tokens = supplement.max_output_tokens.max(entry.max_output_tokens);
        supplement.pricing.merge_max(&entry.pricing);
        supplement
            .capabilities
            .extend(entry.capabilities.iter().cloned());
    }

    // 模型成员和已有字段完全以 OpenRouter 为准；LiteLLM 只逐字段填补空字符串、
    // 空集合或数值 0，不参与覆盖，也不会向主目录增加模型。
    for (canonical_key, supplement) in supplements {
        let Some(model) = models.get_mut(&canonical_key) else {
            continue;
        };
        if model.display_name.is_empty() {
            model.display_name = supplement.display_name;
        }
        if model.mode.is_empty() {
            model.mode = supplement.mode;
        }
        if !model.openrouter_presence.context_length {
            model.context_length = supplement.context_length;
        }
        if !model.openrouter_presence.max_input_tokens {
            model.max_input_tokens = supplement.max_input_tokens;
        }
        if !model.openrouter_presence.max_output_tokens {
            model.max_output_tokens = supplement.max_output_tokens;
        }
        model
            .pricing
            .fill_missing(&supplement.pricing, &model.openrouter_presence.pricing);
        if !model.openrouter_presence.capabilities {
            model.capabilities = supplement.capabilities;
        }
    }

    // 所有成功映射到 OpenRouter 模型的 LiteLLM 原始记录仍保留在详情中，
    // 但不代表它们参与了字段补偿。
    for entry in entries.iter().filter(|entry| entry.source == "litellm") {
        let Some(model) = models.get_mut(&entry.canonical_key) else {
            continue;
        };
        model
            .source_ids
            .insert(format!("{}:{}", entry.source, entry.source_model_id));
        model.sources.insert(entry.source.clone());
        model.channels.insert(entry.channel.clone());
        model.variant_count += 1;
    }
    models
}

async fn fetch_json(
    client: &reqwest::Client,
    source: &str,
    url: &str,
) -> Result<(String, Value), String> {
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, "OpenHub/0.3 model-catalog")
        .send()
        .await
        .map_err(|error| format!("{source} 下载失败：{error}"))?
        .error_for_status()
        .map_err(|error| format!("{source} 返回错误状态：{error}"))?;
    let raw = response
        .text()
        .await
        .map_err(|error| format!("{source} 读取失败：{error}"))?;
    let parsed = serde_json::from_str::<Value>(&raw)
        .map_err(|error| format!("{source} JSON 解析失败：{error}"))?;
    Ok((raw, parsed))
}

fn source_count(source: &str, value: &Value) -> usize {
    if source == "openrouter" {
        value
            .get("data")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0)
    } else {
        value
            .as_object()
            .map(|records| {
                records
                    .keys()
                    .filter(|key| key.as_str() != "sample_spec")
                    .count()
            })
            .unwrap_or(0)
    }
}

fn is_synced_today(database: &Database) -> Result<bool, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
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
        return Ok(false);
    }
    let count = connection
        .query_row(
            "SELECT COUNT(*) FROM model_catalog_sources
             WHERE source IN ('openrouter', 'litellm')
               AND date(fetched_at, 'localtime') = date('now', 'localtime')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| error.to_string())?;
    Ok(count == REQUIRED_SOURCES.len() as i64)
}

fn persist_catalog(
    database: &Database,
    openrouter_raw: &str,
    openrouter: &Value,
    litellm_raw: &str,
    litellm: &Value,
    entries: &[CatalogEntry],
    models: &BTreeMap<String, AggregateModel>,
) -> Result<(), String> {
    let mut connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    let fetched_at: String = transaction
        .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |row| {
            row.get(0)
        })
        .map_err(|error| error.to_string())?;

    transaction
        .execute("DELETE FROM model_catalog_entries", [])
        .map_err(|error| error.to_string())?;
    transaction
        .execute("DELETE FROM model_catalog_models", [])
        .map_err(|error| error.to_string())?;
    transaction
        .execute("DELETE FROM model_catalog_sources", [])
        .map_err(|error| error.to_string())?;

    for (source, url, raw, value) in [
        ("openrouter", OPENROUTER_URL, openrouter_raw, openrouter),
        ("litellm", LITELLM_URL, litellm_raw, litellm),
    ] {
        transaction
            .execute(
                "INSERT INTO model_catalog_sources (source, url, fetched_at, record_count, raw_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![source, url, fetched_at, source_count(source, value) as i64, raw],
            )
            .map_err(|error| error.to_string())?;
    }

    for model in models.values() {
        transaction
            .execute(
                "INSERT INTO model_catalog_models (
                    canonical_key, display_name, provider, mode, context_length,
                    max_input_tokens, max_output_tokens, input_cost_per_token,
                    output_cost_per_token, cache_read_cost_per_token,
                    cache_write_cost_per_token, image_cost, audio_input_cost_per_token,
                    audio_output_cost_per_token, request_cost, source_count, variant_count,
                    capabilities_json, source_ids_json, pricing_json, updated_at
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                    ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21
                 )",
                params![
                    model.canonical_key,
                    model.display_name,
                    model.manufacturer,
                    model.mode,
                    model.context_length,
                    model.max_input_tokens,
                    model.max_output_tokens,
                    model.pricing.input,
                    model.pricing.output,
                    model.pricing.cache_read,
                    model.pricing.cache_write,
                    model.pricing.image,
                    model.pricing.audio_input,
                    model.pricing.audio_output,
                    model.pricing.request,
                    model.sources.len() as i64,
                    model.variant_count as i64,
                    serde_json::to_string(&model.capabilities).map_err(|error| error.to_string())?,
                    serde_json::to_string(&model.source_ids).map_err(|error| error.to_string())?,
                    model.pricing.as_json().to_string(),
                    fetched_at,
                ],
            )
            .map_err(|error| error.to_string())?;
    }

    for entry in entries {
        transaction
            .execute(
                "INSERT INTO model_catalog_entries (
                    source, source_model_id, canonical_key, provider, mode, display_name,
                    context_length, max_input_tokens, max_output_tokens,
                    input_cost_per_token, output_cost_per_token, cache_read_cost_per_token,
                    cache_write_cost_per_token, image_cost, audio_input_cost_per_token,
                    audio_output_cost_per_token, request_cost, capabilities_json, raw_json
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                    ?14, ?15, ?16, ?17, ?18, ?19
                 )",
                params![
                    entry.source,
                    entry.source_model_id,
                    entry.canonical_key,
                    entry.channel,
                    entry.mode,
                    entry.display_name,
                    entry.context_length,
                    entry.max_input_tokens,
                    entry.max_output_tokens,
                    entry.pricing.input,
                    entry.pricing.output,
                    entry.pricing.cache_read,
                    entry.pricing.cache_write,
                    entry.pricing.image,
                    entry.pricing.audio_input,
                    entry.pricing.audio_output,
                    entry.pricing.request,
                    serde_json::to_string(&entry.capabilities).map_err(|error| error.to_string())?,
                    entry.raw.to_string(),
                ],
            )
            .map_err(|error| error.to_string())?;
    }

    transaction
        .execute(
            "INSERT INTO app_meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![CATALOG_SCHEMA_META_KEY, CATALOG_SCHEMA_VERSION],
        )
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())
}

fn read_model_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<ModelCatalogItem> {
    let capabilities_json = row.get::<_, String>(15)?;
    Ok(ModelCatalogItem {
        canonical_key: row.get(0)?,
        display_name: row.get(1)?,
        manufacturer: row.get(2)?,
        mode: row.get(3)?,
        context_length: row.get(4)?,
        max_input_tokens: row.get(5)?,
        max_output_tokens: row.get(6)?,
        input_cost_per_token: row.get(7)?,
        output_cost_per_token: row.get(8)?,
        cache_read_cost_per_token: row.get(9)?,
        cache_write_cost_per_token: row.get(10)?,
        image_cost: row.get(11)?,
        audio_input_cost_per_token: row.get(12)?,
        audio_output_cost_per_token: row.get(13)?,
        request_cost: row.get(14)?,
        capabilities: serde_json::from_str(&capabilities_json).unwrap_or_default(),
    })
}

#[tauri::command]
pub fn get_model_catalog(database: State<'_, Database>) -> Result<ModelCatalogSnapshot, String> {
    get_model_catalog_inner(&database)
}

pub(crate) fn get_model_catalog_inner(database: &Database) -> Result<ModelCatalogSnapshot, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let mut statement = connection
        .prepare(
            "SELECT canonical_key, display_name, provider, mode, context_length,
                    max_input_tokens, max_output_tokens, input_cost_per_token,
                    output_cost_per_token, cache_read_cost_per_token,
                    cache_write_cost_per_token, image_cost, audio_input_cost_per_token,
                    audio_output_cost_per_token, request_cost, capabilities_json
             FROM model_catalog_models
             WHERE provider <> 'openrouter'
               AND canonical_key NOT LIKE 'openrouter/%'
             ORDER BY provider, display_name COLLATE NOCASE, canonical_key",
        )
        .map_err(|error| error.to_string())?;
    let models = statement
        .query_map([], read_model_item)
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;

    let mut source_statement = connection
        .prepare(
            "SELECT source, url, fetched_at, record_count
             FROM model_catalog_sources ORDER BY source",
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
    let synced_today = sources.len() == REQUIRED_SOURCES.len()
        && connection
            .query_row(
                "SELECT COUNT(*) FROM model_catalog_sources
                 WHERE date(fetched_at, 'localtime') = date('now', 'localtime')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0)
            == REQUIRED_SOURCES.len() as i64;
    Ok(ModelCatalogSnapshot {
        total: models.len(),
        models,
        last_synced_at,
        synced_today,
        sources,
    })
}

#[tauri::command]
pub fn get_model_catalog_detail(
    database: State<'_, Database>,
    canonical_key: String,
) -> Result<ModelCatalogDetail, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let model = connection
        .query_row(
            "SELECT canonical_key, display_name, provider, mode, context_length,
                    max_input_tokens, max_output_tokens, input_cost_per_token,
                    output_cost_per_token, cache_read_cost_per_token,
                    cache_write_cost_per_token, image_cost, audio_input_cost_per_token,
                    audio_output_cost_per_token, request_cost, capabilities_json
             FROM model_catalog_models WHERE canonical_key = ?1",
            [&canonical_key],
            read_model_item,
        )
        .optional()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "模型参数不存在".to_string())?;
    let pricing_text = connection
        .query_row(
            "SELECT pricing_json FROM model_catalog_models WHERE canonical_key = ?1",
            [&canonical_key],
            |row| row.get::<_, String>(0),
        )
        .map_err(|error| error.to_string())?;
    let mut statement = connection
        .prepare(
            "SELECT source, source_model_id, provider, mode, display_name,
                    context_length, max_input_tokens, max_output_tokens,
                    input_cost_per_token, output_cost_per_token, cache_read_cost_per_token,
                    cache_write_cost_per_token, image_cost, audio_input_cost_per_token,
                    audio_output_cost_per_token, request_cost, raw_json
             FROM model_catalog_entries
             WHERE canonical_key = ?1
             ORDER BY source, source_model_id",
        )
        .map_err(|error| error.to_string())?;
    let entries = statement
        .query_map([&canonical_key], |row| {
            let raw_text = row.get::<_, String>(16)?;
            Ok(ModelCatalogEntryDetail {
                source: row.get(0)?,
                source_model_id: row.get(1)?,
                channel: row.get(2)?,
                mode: row.get(3)?,
                display_name: row.get(4)?,
                context_length: row.get(5)?,
                max_input_tokens: row.get(6)?,
                max_output_tokens: row.get(7)?,
                input_cost_per_token: row.get(8)?,
                output_cost_per_token: row.get(9)?,
                cache_read_cost_per_token: row.get(10)?,
                cache_write_cost_per_token: row.get(11)?,
                image_cost: row.get(12)?,
                audio_input_cost_per_token: row.get(13)?,
                audio_output_cost_per_token: row.get(14)?,
                request_cost: row.get(15)?,
                raw: serde_json::from_str(&raw_text).unwrap_or(Value::Null),
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    Ok(ModelCatalogDetail {
        model,
        pricing: serde_json::from_str(&pricing_text).unwrap_or(Value::Null),
        entries,
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
    let client = build_http_client(database, Duration::from_secs(40), 5, "模型参数同步")?;
    let (openrouter_result, litellm_result) = tokio::join!(
        fetch_json(&client, "OpenRouter", OPENROUTER_URL),
        fetch_json(&client, "LiteLLM", LITELLM_URL),
    );
    let (openrouter_raw, openrouter) = openrouter_result?;
    let (litellm_raw, litellm) = litellm_result?;
    let entries = build_entries(&openrouter, &litellm)?;
    let models = aggregate_entries(&entries);
    persist_catalog(
        database,
        &openrouter_raw,
        &openrouter,
        &litellm_raw,
        &litellm,
        &entries,
        &models,
    )?;
    let snapshot = get_model_catalog_inner(database)?;
    let message = format!(
        "模型参数同步完成：OpenRouter {} 条主数据，LiteLLM {} 条补偿源，生成 {} 个 OpenRouter 规范模型",
        source_count("openrouter", &openrouter),
        source_count("litellm", &litellm),
        snapshot.total,
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

/// 一次性同步入口（供 `cargo run --example sync_model_catalog` 使用），
/// 直接对裸 rusqlite 连接重跑 catalog，验证修复后列表只含 OpenRouter 模型。
pub struct CatalogSyncReport {
    pub openrouter_count: usize,
    pub litellm_count: usize,
    pub model_count: usize,
}

pub fn sync_model_catalog_once(db_path: &str) -> Result<CatalogSyncReport, String> {
    let connection = rusqlite::Connection::open(db_path)
        .map_err(|error| format!("打开数据库失败：{error}"))?;
    let runtime = ModelCatalogRuntime::new();
    let database = Database(std::sync::Mutex::new(connection));

    let runtime_ref = &runtime;
    let database_ref = &database;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("创建异步运行时失败：{error}"))?;
    let result = rt
        .block_on(async move { fetch_and_persist(database_ref, runtime_ref).await })?;
    Ok(result)
}

async fn fetch_and_persist(
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
    let client = build_http_client(database, Duration::from_secs(40), 5, "模型参数同步")?;
    let (openrouter_result, litellm_result) = tokio::join!(
        fetch_json(&client, "OpenRouter", OPENROUTER_URL),
        fetch_json(&client, "LiteLLM", LITELLM_URL),
    );
    let (openrouter_raw, openrouter) = openrouter_result?;
    let (litellm_raw, litellm) = litellm_result?;
    let entries = build_entries(&openrouter, &litellm)?;
    let models = aggregate_entries(&entries);
    persist_catalog(
        database,
        &openrouter_raw,
        &openrouter,
        &litellm_raw,
        &litellm,
        &entries,
        &models,
    )?;
    Ok(CatalogSyncReport {
        openrouter_count: source_count("openrouter", &openrouter),
        litellm_count: source_count("litellm", &litellm),
        model_count: models.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_openrouter_price_variants() {
        assert_eq!(openrouter_key("openai/gpt-5:batch"), "openai/gpt-5");
        assert_eq!(openrouter_key("xai/grok-4:free"), "x-ai/grok-4");
    }

    #[test]
    fn openrouter_router_products_do_not_enter_model_catalog() {
        let openrouter = json!({
            "data": [
                {
                    "id": "openrouter/auto",
                    "name": "Auto Router",
                    "architecture": {
                        "tokenizer": "Router",
                        "input_modalities": ["text"],
                        "output_modalities": ["text"]
                    },
                    "pricing": { "prompt": "-1", "completion": "-1" },
                    "context_length": 2000000,
                    "top_provider": {},
                    "supported_parameters": []
                },
                {
                    "id": "openai/gpt-primary",
                    "name": "OpenAI: GPT Primary",
                    "architecture": {
                        "input_modalities": ["text"],
                        "output_modalities": ["text"]
                    },
                    "pricing": { "prompt": "0.000001", "completion": "0.000002" },
                    "context_length": 128000,
                    "top_provider": {},
                    "supported_parameters": []
                }
            ]
        });
        let entries = build_entries(&openrouter, &json!({})).unwrap();
        let models = aggregate_entries(&entries);
        assert_eq!(models.len(), 1);
        assert!(models.contains_key("openai/gpt-primary"));
        assert!(!models.contains_key("openrouter/auto"));
    }

    #[test]
    fn max_price_merge_keeps_higher_value() {
        let mut current = NormalizedPricing {
            input: 0.000_001,
            output: 0.000_003,
            ..Default::default()
        };
        current.merge_max(&NormalizedPricing {
            input: 0.000_002,
            output: 0.000_002,
            ..Default::default()
        });
        assert_eq!(current.input, 0.000_002);
        assert_eq!(current.output, 0.000_003);
    }

    #[test]
    fn litellm_only_models_do_not_enter_primary_catalog() {
        let openrouter = json!({
            "data": [{
                "id": "openai/gpt-primary",
                "name": "GPT Primary",
                "architecture": { "input_modalities": ["text"], "output_modalities": ["text"] },
                "pricing": { "prompt": "0.000001", "completion": "0.000002" },
                "context_length": 128000,
                "top_provider": {},
                "supported_parameters": []
            }]
        });
        let litellm = json!({
            "gpt-primary": {
                "litellm_provider": "openai",
                "mode": "chat",
                "input_cost_per_token": 0.000003,
                "output_cost_per_token": 0.000004,
                "cache_creation_input_token_cost": 0.0000005
            },
            "unrelated-exclusive-model": {
                "litellm_provider": "exclusive_channel",
                "mode": "chat",
                "input_cost_per_token": 0.5,
                "output_cost_per_token": 1.0
            }
        });
        let entries = build_entries(&openrouter, &litellm).unwrap();
        let models = aggregate_entries(&entries);
        assert_eq!(models.len(), 1);
        assert!(models.contains_key("openai/gpt-primary"));
        let model = models.get("openai/gpt-primary").unwrap();
        assert_eq!(model.pricing.input, 0.000001);
        assert_eq!(model.pricing.output, 0.000002);
        assert_eq!(model.pricing.cache_write, 0.0000005);
    }

    #[test]
    fn litellm_only_fills_fields_missing_from_openrouter() {
        let openrouter = json!({
            "data": [{
                "id": "openai/gpt-missing",
                "name": "",
                "architecture": {},
                "pricing": { "completion": "0.000002" },
                "context_length": 0,
                "top_provider": {},
                "supported_parameters": []
            }]
        });
        let litellm = json!({
            "gpt-missing": {
                "litellm_provider": "openai",
                "mode": "chat",
                "input_cost_per_token": 0.000003,
                "output_cost_per_token": 0.000004,
                "max_input_tokens": 128000,
                "max_output_tokens": 16000,
                "supports_function_calling": true
            }
        });
        let entries = build_entries(&openrouter, &litellm).unwrap();
        let models = aggregate_entries(&entries);
        let model = models.get("openai/gpt-missing").unwrap();
        assert_eq!(model.display_name, "gpt-missing");
        assert_eq!(model.mode, "chat");
        assert_eq!(model.context_length, 128000);
        assert_eq!(model.max_input_tokens, 128000);
        assert_eq!(model.max_output_tokens, 16000);
        assert_eq!(model.pricing.input, 0.000003);
        assert_eq!(model.pricing.output, 0.000002);
        assert_eq!(
            model.capabilities,
            BTreeSet::from(["function_calling".to_string()])
        );
    }

    #[test]
    fn explicit_zero_openrouter_price_is_not_replaced() {
        let openrouter = json!({
            "data": [{
                "id": "openai/gpt-free",
                "name": "OpenAI: GPT Free",
                "architecture": { "input_modalities": ["text"], "output_modalities": ["text"] },
                "pricing": { "prompt": "0", "completion": "0" },
                "context_length": 128000,
                "top_provider": {},
                "supported_parameters": []
            }]
        });
        let litellm = json!({
            "gpt-free": {
                "litellm_provider": "openai",
                "input_cost_per_token": 0.000003,
                "output_cost_per_token": 0.000004
            }
        });
        let entries = build_entries(&openrouter, &litellm).unwrap();
        let models = aggregate_entries(&entries);
        let model = models.get("openai/gpt-free").unwrap();
        assert_eq!(model.pricing.input, 0.0);
        assert_eq!(model.pricing.output, 0.0);
    }

    #[test]
    fn litellm_never_overwrites_existing_openrouter_fields() {
        let openrouter = json!({
            "data": [{
                "id": "deepseek/deepseek-r1",
                "name": "DeepSeek: DeepSeek R1",
                "architecture": { "input_modalities": ["text"], "output_modalities": ["text"] },
                "pricing": { "prompt": "0.000001", "completion": "0.000002" },
                "context_length": 128000,
                "top_provider": { "max_completion_tokens": 8192 },
                "supported_parameters": ["reasoning"]
            }]
        });
        let litellm = json!({
            "wandb/deepseek-ai/deepseek-r1": {
                "litellm_provider": "wandb",
                "mode": "embedding",
                "input_cost_per_token": 0.135,
                "output_cost_per_token": 0.54,
                "max_input_tokens": 999999,
                "supports_vision": true
            }
        });
        let entries = build_entries(&openrouter, &litellm).unwrap();
        assert_eq!(entries.len(), 2);
        let models = aggregate_entries(&entries);
        let model = models.get("deepseek/deepseek-r1").unwrap();
        assert_eq!(model.display_name, "DeepSeek R1");
        assert_eq!(model.mode, "chat");
        assert_eq!(model.context_length, 128000);
        assert_eq!(model.pricing.input, 0.000001);
        assert_eq!(model.pricing.output, 0.000002);
        assert_eq!(
            model.capabilities,
            BTreeSet::from([
                "input_modalities:text".to_string(),
                "output_modalities:text".to_string(),
                "reasoning".to_string(),
            ])
        );
        assert_eq!(model.variant_count, 2);
    }

    #[test]
    fn matched_litellm_deployment_can_fill_an_openrouter_gap() {
        let openrouter = json!({
            "data": [{
                "id": "openai/gpt-transcribe",
                "name": "OpenAI: GPT Transcribe",
                "architecture": { "input_modalities": ["audio"], "output_modalities": ["text"] },
                "pricing": { "prompt": "0", "completion": "0" },
                "context_length": 0,
                "top_provider": {},
                "supported_parameters": []
            }]
        });
        let litellm = json!({
            "azure/gpt-transcribe": {
                "litellm_provider": "azure",
                "mode": "audio_transcription",
                "input_cost_per_audio_token": 0.0000025,
                "max_input_tokens": 16000,
                "max_output_tokens": 2000
            }
        });
        let entries = build_entries(&openrouter, &litellm).unwrap();
        let models = aggregate_entries(&entries);
        let model = models.get("openai/gpt-transcribe").unwrap();
        assert_eq!(model.display_name, "GPT Transcribe");
        assert_eq!(model.manufacturer, "openai");
        assert_eq!(model.context_length, 16000);
        assert_eq!(model.max_output_tokens, 2000);
        assert_eq!(model.pricing.audio_input, 0.0000025);
    }

    #[test]
    fn canonical_openrouter_name_wins_over_price_variant_name() {
        let openrouter = json!({
            "data": [
                {
                    "id": "openai/gpt-primary:free",
                    "name": "OpenAI: GPT Primary (free)",
                    "architecture": { "input_modalities": ["text"], "output_modalities": ["text"] },
                    "pricing": {}, "context_length": 128000, "top_provider": {}, "supported_parameters": []
                },
                {
                    "id": "openai/gpt-primary",
                    "name": "OpenAI: GPT Primary",
                    "architecture": { "input_modalities": ["text"], "output_modalities": ["text"] },
                    "pricing": {}, "context_length": 128000, "top_provider": {}, "supported_parameters": []
                }
            ]
        });
        let entries = build_entries(&openrouter, &json!({})).unwrap();
        let models = aggregate_entries(&entries);
        assert_eq!(
            models.get("openai/gpt-primary").unwrap().display_name,
            "GPT Primary"
        );
    }

    #[test]
    fn litellm_deployment_variants_map_to_openrouter_model() {
        let openrouter_keys = HashSet::from(["openai/gpt-5.4".to_string()]);
        let openrouter_leafs =
            HashMap::from([("gpt-5.4".to_string(), vec!["openai/gpt-5.4".to_string()])]);
        assert_eq!(
            litellm_key("azure/gpt-5.4", &openrouter_keys, &openrouter_leafs),
            Some("openai/gpt-5.4".to_string()),
        );
        assert_eq!(
            litellm_key(
                "openrouter/openai/gpt-5.4",
                &openrouter_keys,
                &openrouter_leafs,
            ),
            Some("openai/gpt-5.4".to_string()),
        );
    }

    #[test]
    fn litellm_unmatched_key_returns_none() {
        let openrouter_keys = HashSet::from(["openai/gpt-5.4".to_string()]);
        let openrouter_leafs =
            HashMap::from([("gpt-5.4".to_string(), vec!["openai/gpt-5.4".to_string()])]);
        assert_eq!(
            litellm_key("some-vendor/exclusive-model", &openrouter_keys, &openrouter_leafs),
            None,
        );
        assert_eq!(
            litellm_key("exclusive-model", &openrouter_keys, &openrouter_leafs),
            None,
        );
    }
}
