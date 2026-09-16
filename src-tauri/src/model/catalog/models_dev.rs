//! models.dev 数据源模块（主数据源）
//!
//! 目标：以 `https://models.dev/catalog.json` 作为模型元数据主源，为「模型全景控制台」
//! 提供**原始 lab / 原始 modelId** 身份信息，并作为判定「哪些渠道免费提供」的价格依据，
//! 从而替换原先 ~1800 页的 llmpricing HTML 爬取子系统。
//!
//! # 数据结构（单文件同时承载两层）
//!
//! - `models`：**canonical 层**。键为 `lab/modelId`（如 `openai/gpt-5.5`），值为该模型的
//!   官方规格元数据：`name` / `description` / `modalities` / `limit` / `open_weights` /
//!   `license` / `weights` / `links` / `release_date` / `knowledge` 等。**不含价格**。
//! - `providers`：**provider 层**。键为渠道 id（如 `zhipuai`、`openrouter`、`alibaba-cn`），
//!   值为该渠道的接入信息（`name` / `npm` / `api` / `doc` / `env`）与其代理的模型表；
//!   模型条目里带 `cost` / `limit` / `status` / `reasoning_options` / `interleaved` 等。
//!
//! # ⚠️ 命名空间陷阱
//!
//! 两层的 ID 处于**不同命名空间**：
//!
//! | 层 | 键形态 | 示例 |
//! |---|---|---|
//! | canonical | `lab/modelId` | `zhipuai/glm-5.2` |
//! | provider  | `provider/modelId` | `subconscious/glm-5.2`、`@cf/deepseek-ai/...` |
//!
//! 且 provider 层的 modelId 拼写由各渠道自行声明，极不统一：
//! `glm-5.2` / `z-ai/glm-5.2` / `meta/llama-3.3-70b` / `llama3.3:70b` / `gemini-3-1-pro-preview`。
//! 跨层匹配必须经 [`normalize_model_id`] 归一化，**绝不能**直接拼接或比较。
//!
//! # ⚠️ `provider` 字段的真实含义
//!
//! provider 层模型条目里偶尔出现的 `provider` 字段**不是渠道名**，而是该渠道对该模型的
//! **请求形态覆盖对象**（`{ "npm": "@ai-sdk/openai-compatible", "api": "...", "shape": "completions" }`）。
//! 不要把它当成渠道归属来用。
//!
//! # ETag 条件请求
//!
//! `catalog.json` 响应带 `etag` 与 `cache-control: public, max-age=0, must-revalidate`。
//! 携带 `If-None-Match` 命中时返回 `304 Not Modified`，可省下 ~4.9MB 流量。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

/// models.dev 全量目录（canonical 层 + provider 层，单文件）。
pub const MODELS_DEV_CATALOG_URL: &str = "https://models.dev/catalog.json";

/// 落库时用于记录 models.dev catalog 的 ETag 的 `app_meta` 键。
pub const MODELS_DEV_ETAG_META_KEY: &str = "model_catalog_models_dev_etag";

/// 写入 `model_catalog_sources` 表时使用的 source 标识。
pub const MODELS_DEV_SOURCE: &str = "models_dev_catalog";

/// 质量/量化后缀。归一化时**循环剥离**，但**不含 `-free`**。
///
/// ⚠️ 若无条件剥离 `-free`，`zenmux` 会被误判为 GLM-5.2 的免费渠道，引入假阳性。
/// 保留 `-free` 时与 llmpricing 的免费渠道判定一致率为 96%（1877/1941），是实测稳定点。
const QUALITY_SUFFIXES: &[&str] = &[
    "-instruct",
    "-preview",
    "-latest",
    "-chat",
    "-it",
    "-turbo",
    "-thinking",
    "-exp",
    "-experimental",
    "-fp8",
    "-tee",
];

/// 把上游任意拼写的 model id 归一化为可跨层比较的裸 id。
///
/// 处理顺序（**顺序敏感，不要调整**）：
/// 1. 小写 + 去首尾空白
/// 2. 截断 `:variant` / `@variant` 变体后缀（如 `:free`、`:thinking`、`:70b`）
/// 3. 只取最后一个 `/` 之后的部分（剥离 lab / provider 前缀）
/// 4. `.` 与 `_` 统一为 `-`
/// 5. **循环**剥离质量后缀（`-instruct`、`-preview` …）
/// 6. 补字母-数字边界：`llama3` → `llama-3`
/// 7. 压缩连续 `-` 并去首尾 `-`
///
/// 自检样例：
/// ```text
/// glm-5.2:free           -> glm-5-2
/// z-ai/glm-5.2           -> glm-5-2
/// meta/llama-3.3-70b     -> llama-3-3-70b
/// llama3.3:70b           -> llama-3-3
/// gemini-3-1-pro-preview -> gemini-3-1-pro
/// GLM-5.2                -> glm-5-2
/// ```
pub fn normalize_model_id(raw: &str) -> String {
    let mut s = raw.trim().to_lowercase();

    // 2. 截断 :variant / @variant
    if let Some(idx) = s.find(|c| c == ':' || c == '@') {
        s.truncate(idx);
    }

    // 3. 只保留最后一段
    if let Some(idx) = s.rfind('/') {
        s = s[idx + 1..].to_string();
    }

    // 4. . _ -> -
    s = s.replace(['.', '_'], "-");

    // 5. 循环剥离质量后缀
    loop {
        let mut stripped = false;
        for &suffix in QUALITY_SUFFIXES {
            if let Some(rest) = s.strip_suffix(suffix) {
                if !rest.is_empty() {
                    s = rest.to_string();
                    stripped = true;
                    break;
                }
            }
        }
        if !stripped {
            break;
        }
    }

    // 6. 补字母-数字边界：llama3 -> llama-3
    let chars: Vec<char> = s.chars().collect();
    let mut with_boundary = String::with_capacity(s.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && chars[i - 1].is_ascii_alphabetic() && c.is_ascii_digit() {
            with_boundary.push('-');
        }
        with_boundary.push(c);
    }

    // 7. 压缩连续 - 并去首尾
    let mut squeezed = String::with_capacity(with_boundary.len());
    let mut prev_dash = false;
    for c in with_boundary.chars() {
        if c == '-' {
            if prev_dash {
                continue;
            }
            prev_dash = true;
        } else {
            prev_dash = false;
        }
        squeezed.push(c);
    }

    squeezed.trim_matches('-').to_string()
}

/// canonical id（`lab/modelId`）拆出 lab 部分。无 `/` 时整体视为 lab。
pub fn canonical_lab(canonical_id: &str) -> &str {
    canonical_id.split('/').next().unwrap_or(canonical_id)
}

/// canonical id（`lab/modelId`）拆出 modelId 部分（`/` 之后，保留原拼写）。
pub fn canonical_model_id(canonical_id: &str) -> &str {
    match canonical_id.split_once('/') {
        Some((_, model_id)) => model_id,
        None => canonical_id,
    }
}

/// provider 层单个模型条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsDevProvider {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub env: Option<Vec<String>>,
    #[serde(default)]
    pub npm: Option<String>,
    #[serde(default)]
    pub api: Option<String>,
    #[serde(default)]
    pub doc: Option<String>,
    /// 该渠道声明的模型表：`modelId(原拼写) -> 条目`。
    #[serde(default)]
    pub models: BTreeMap<String, Value>,
}

/// 反序列化后的 models.dev 目录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsDevCatalog {
    /// canonical 层：`lab/modelId -> 元数据`。
    #[serde(default)]
    pub models: BTreeMap<String, Value>,
    /// provider 层：`providerId -> 渠道`。
    #[serde(default)]
    pub providers: BTreeMap<String, ModelsDevProvider>,
}

impl ModelsDevCatalog {
    /// 从已解析的 JSON 值反序列化。
    pub fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone())
            .map_err(|error| format!("models.dev catalog 结构解析失败：{error}"))
    }

    /// canonical 层模型数量。
    #[allow(dead_code)] // 规模统计接口，`ModelsDevIndex::summary()` 已覆盖主用例
    pub fn canonical_count(&self) -> usize {
        self.models.len()
    }

    /// provider 层渠道数量。
    #[allow(dead_code)]
    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }

    /// provider 层渠道-模型条目总数（含重复模型在不同渠道的多条记录）。
    #[allow(dead_code)]
    pub fn channel_entry_count(&self) -> usize {
        self.providers
            .values()
            .map(|provider| provider.models.len())
            .sum()
    }
}

/// provider 层的一条渠道记录（某个渠道提供的某个模型）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsDevChannel {
    /// 渠道 id，如 `zhipuai`、`openrouter`。
    pub provider: String,
    /// 渠道展示名，如 `Zhipu AI`。
    pub provider_name: String,
    /// 上游声明的模型 id **原拼写**（如 `z-ai/glm-5.2`），用于「原始 modelId」展示。
    pub model_id: String,
    /// 归一化后的裸 id，用于跨层匹配。
    pub normalized_id: String,
    /// 该条目内的 `id` 字段（通常等于 `model_id`）。
    pub declared_id: Option<String>,
    /// 该条目对应的 canonical id（若成功匹配上 canonical 层）。
    pub canonical_id: Option<String>,
    pub name: Option<String>,
    pub family: Option<String>,
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub context: Option<i64>,
    pub output_limit: Option<i64>,
    /// `deprecated` / `beta`，无则为 None。
    pub status: Option<String>,
    pub reasoning: bool,
    pub tool_call: bool,
    pub attachment: bool,
    pub structured_output: bool,
    pub temperature: Option<bool>,
    pub open_weights: bool,
    pub knowledge: Option<String>,
    pub release_date: Option<String>,
    pub last_updated: Option<String>,
    /// 是否声明了 `reasoning_options`。
    pub has_reasoning_options: bool,
    /// **思考级别选项**（`reasoning_options` 的完整解析）。
    ///
    /// 两种形态：
    /// - `{ type: "toggle" }` —— 只有开/关；
    /// - `{ type: "effort", values: ["low","medium","high",…] }` —— 多档位。
    ///
    /// 实测档位取值集合：`none` / `minimal` / `low` / `medium` / `high` / `xhigh` / `max`
    /// （另有极少数 `default`）。同一模型在不同渠道可能声明不同档位。
    pub reasoning_options: Vec<ReasoningOption>,
    /// 是否声明了 `interleaved`（交错推理输出）。
    pub interleaved: bool,
    /// **交错推理的读取字段名**（如 `reasoning_content` / `reasoning_details`）。
    ///
    /// 上游可能写成 `true`（未指定字段名）或 `{ field: "..." }`；
    /// 为 `true` 时返回 `None`，表示「支持交错但未指明字段」。
    pub interleaved_field: Option<String>,
    /// **独立输入上限**（`limit.input`）。与 `context` 不同：部分模型的总上下文
    /// 大于单次可输入的 token 数（如 context 400000 / input 272000）。
    pub input_limit: Option<i64>,
    /// **输出模态**（`modalities.output`）。并非全是 `text`——
    /// 实测有 `image` / `video` / `audio` 输出的模型，与 `kind` 互补。
    pub output_modalities: Vec<String>,
    /// 是否为实验性模式（`experimental.modes`，如 fast / pro 档位）。
    pub experimental: bool,
    /// **快速模式**（`experimental.modes.fast`）：额外计费档 + 请求覆盖。
    ///
    /// 形如 `{ cost: {...}, provider: { body: {...}, headers: {...} } }`。
    /// 用户最关心的是 `cost` —— 开启 fast 后单价会上涨。
    pub fast_mode: Option<Value>,
    /// 该渠道对该模型的请求形态覆盖（`provider` 字段，**不是渠道名**）。
    pub request_override: Option<Value>,
    /// 完整原始条目，落库备查。
    pub raw: Value,
}

/// 一个思考级别选项（models.dev `reasoning_options` 的元素）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningOption {
    /// `toggle` 或 `effort`。
    pub kind: String,
    /// `effort` 形态下的可选档位；`toggle` 形态为空。
    #[serde(default)]
    pub values: Vec<String>,
}

impl ReasoningOption {
    /// 档位排序权重，用于求「最高档」。
    ///
    /// `none` < `minimal` < `low` < `medium` < `high` < `xhigh` < `max`；
    /// 未知档位（如 `default`）排在 `none` 之前，避免被误当成最高档。
    pub fn effort_rank(value: &str) -> i32 {
        match value {
            "none" => 0,
            "minimal" => 1,
            "low" => 2,
            "medium" => 3,
            "high" => 4,
            "xhigh" => 5,
            "max" => 6,
            _ => -1,
        }
    }

    /// 该选项声明的最高档位（仅 `effort` 形态有意义）。
    pub fn highest_effort(&self) -> Option<&str> {
        self.values
            .iter()
            .max_by_key(|v| Self::effort_rank(v))
            .map(String::as_str)
    }
}

/// 从 models.dev 的 `reasoning_options` 字段解析出选项列表。
///
/// 容错：非数组、元素非对象、`values` 里混入 null 或非字符串都会被跳过，
/// 不因为个别脏数据丢掉整个字段。
fn parse_reasoning_options(value: Option<&Value>) -> Vec<ReasoningOption> {
    let Some(array) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    array
        .iter()
        .filter_map(|entry| {
            let obj = entry.as_object()?;
            let kind = obj.get("type").and_then(Value::as_str)?.to_string();
            let values = obj
                .get("values")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            Some(ReasoningOption { kind, values })
        })
        .collect()
}

/// 解析 `interleaved` 字段的读取字段名。
///
/// `{ field: "reasoning_content" }` → `Some("reasoning_content")`；
/// `true` → `None`（支持交错但未指明字段）。
fn parse_interleaved_field(value: Option<&Value>) -> Option<String> {
    let obj = value?.as_object()?;
    obj.get("field")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// 渠道条目的查询接口。部分方法目前只被回归测试使用，保留作为对外查询面。
#[allow(dead_code)]
impl ModelsDevChannel {
    /// 该渠道对该模型是否为「零价」：输入与输出单价均为 0。
    ///
    /// ⚠️ 零价 **不等于** 免费可用。订阅制渠道（如 `xiaomi-token-plan-cn`）同样报零价，
    /// 必须结合 llmpricing manifest 的 `subscription` 标记过滤。
    pub fn is_zero_cost(&self) -> bool {
        self.input == Some(0.0) && self.output == Some(0.0)
    }

}

/// 构建完成的 models.dev 索引。
#[derive(Debug, Clone)]
pub struct ModelsDevIndex {
    /// canonical id -> 元数据。
    pub canonical: BTreeMap<String, Value>,
    /// 归一化裸 id -> 该 id 在各渠道的渠道记录。
    pub provider: BTreeMap<String, Vec<ModelsDevChannel>>,
    /// canonical 归一化裸 id -> canonical id（用于反向定位原始身份）。
    pub canonical_by_normalized: BTreeMap<String, Vec<String>>,
    /// provider id -> 渠道元信息（name / npm / api / doc）。
    pub provider_meta: BTreeMap<String, ModelsDevProvider>,
}

/// models.dev 索引查询接口。部分方法目前只被回归测试使用，保留作为对外查询面。
#[allow(dead_code)]
impl ModelsDevIndex {
    /// 按归一化裸 id 查该模型的所有渠道记录。
    pub fn channels_for(&self, model_id: &str) -> &[ModelsDevChannel] {
        self.provider
            .get(&normalize_model_id(model_id))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// 带 `-free` 回退的渠道查找。
    ///
    /// llmpricing 会把免费变体单独登记成一个模型（`google/gemma-4-31b-it-free`），
    /// 其归一化结果是 `gemma-4-31b-free`，与 models.dev 的 `gemma-4-31b` 对不上。
    /// 直接查不到时剥掉 `-free` 再查一次。
    ///
    /// 实测收益：免费渠道计数一致率 96.7% → **97.4%**；
    /// 「该模型是否有免费渠道」二值一致率 98.4% → **99.1%**。
    pub fn channels_for_with_free_fallback(&self, model_id: &str) -> &[ModelsDevChannel] {
        let normalized = normalize_model_id(model_id);
        if let Some(channels) = self.provider.get(&normalized) {
            return channels;
        }
        if let Some(base) = normalized.strip_suffix("-free") {
            if let Some(channels) = self.provider.get(base) {
                return channels;
            }
        }
        &[]
    }

    /// 按渠道分组该模型的全部渠道记录（带 `-free` 回退）。
    ///
    /// 同一渠道常用多种拼写登记同一模型（`glm-5.2`、`glm-5.2:free`、`z-ai/glm-5.2`），
    /// 因此一个渠道可能对应多条记录。调用方需自行决定：
    ///
    /// - **展示价**：取「非 `:free` 变体」优先（与 llmpricing 详情页展示一致）；
    /// - **是否免费**：取「**任一变体零价即免费**」（实测 `unorouter` 对 glm-5.2 同时有
    ///   付费与 `:free` 两条记录，上游按零价那条计为免费渠道）。
    pub fn channels_grouped_by_provider(
        &self,
        model_id: &str,
    ) -> BTreeMap<String, Vec<&ModelsDevChannel>> {
        let mut grouped: BTreeMap<String, Vec<&ModelsDevChannel>> = BTreeMap::new();
        for channel in self.channels_for_with_free_fallback(model_id) {
            grouped
                .entry(channel.provider.clone())
                .or_default()
                .push(channel);
        }
        grouped
    }

    /// 按归一化裸 id 查候选 canonical id 列表。
    pub fn canonical_candidates(&self, model_id: &str) -> &[String] {
        self.canonical_by_normalized
            .get(&normalize_model_id(model_id))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// 定位该模型对应的 canonical id。
    ///
    /// 同一归一化键可能对应多个 canonical 条目（如不同 lab 的同名模型），
    /// `prefer_lab` 命中时优先返回该 lab 的条目，否则返回第一个候选。
    /// **不做模糊匹配**——宁可返回 `None` 也不认错身份。
    pub fn canonical_id_for(&self, model_id: &str, prefer_lab: Option<&str>) -> Option<String> {
        let candidates = self.canonical_candidates(model_id);
        if candidates.is_empty() {
            return None;
        }
        if let Some(lab) = prefer_lab {
            if let Some(hit) = candidates
                .iter()
                .find(|candidate| canonical_lab(candidate) == lab)
            {
                return Some(hit.clone());
            }
        }
        candidates.first().cloned()
    }

    /// 该模型的零价渠道 id 列表（**已按渠道去重**）。
    ///
    /// ⚠️ 同一渠道常以多种拼写重复登记同一模型（`glm-5.2` / `z-ai/glm-5.2` / `glm-5.2:free`），
    /// 不去重会把渠道数算多。此处只返回渠道 id，且**未**剔除订阅制渠道，调用方需再过滤。
    pub fn zero_cost_providers(&self, model_id: &str) -> Vec<String> {
        self.distinct_providers_where(model_id, |channel| channel.is_zero_cost())
    }

    /// 该模型的**免费**渠道 id 列表（已去重，并剔除订阅制渠道）。
    ///
    /// 判定规则与既有 `catalog.rs` 保持一致：
    /// `input == 0 && output == 0 && !subscription`。
    /// 订阅制标记来自 llmpricing manifest（models.dev 自身没有该字段），因此以谓词注入。
    pub fn free_providers(
        &self,
        model_id: &str,
        is_subscription: impl Fn(&str) -> bool,
    ) -> Vec<String> {
        self.distinct_providers_where(model_id, |channel| {
            channel.is_zero_cost() && !is_subscription(&channel.provider)
        })
    }

    /// 该模型去重后的全部渠道 id 列表（按渠道 id 升序）。
    pub fn distinct_providers(&self, model_id: &str) -> Vec<String> {
        self.distinct_providers_where(model_id, |_| true)
    }

    fn distinct_providers_where(
        &self,
        model_id: &str,
        predicate: impl Fn(&ModelsDevChannel) -> bool,
    ) -> Vec<String> {
        let mut seen: BTreeMap<String, ()> = BTreeMap::new();
        for channel in self.channels_for(model_id) {
            if predicate(channel) {
                seen.insert(channel.provider.clone(), ());
            }
        }
        seen.into_keys().collect()
    }

    /// 索引规模摘要：`(canonical 数, 渠道数, 渠道记录数, 归一化键数)`。
    pub fn summary(&self) -> (usize, usize, usize, usize) {
        let channel_entries: usize = self.provider.values().map(Vec::len).sum();
        (
            self.canonical.len(),
            self.provider_meta.len(),
            channel_entries,
            self.provider.len(),
        )
    }
}

/// 由已解析的 JSON 构建索引。
///
/// 同时完成 canonical 层与 provider 层的归一化索引，并把 provider 层条目回连到
/// canonical 层（`canonical_id`），回连失败时保持 `None`（不臆测）。
pub fn build_index(value: &Value) -> Result<ModelsDevIndex, String> {
    let catalog = ModelsDevCatalog::from_value(value)?;

    // canonical 归一化索引
    let mut canonical_by_normalized: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for canonical_id in catalog.models.keys() {
        let normalized = normalize_model_id(canonical_model_id(canonical_id));
        canonical_by_normalized
            .entry(normalized)
            .or_default()
            .push(canonical_id.clone());
    }

    // provider 层索引
    let mut provider: BTreeMap<String, Vec<ModelsDevChannel>> = BTreeMap::new();
    for (provider_id, provider_entry) in &catalog.providers {
        let provider_name = provider_entry
            .name
            .clone()
            .unwrap_or_else(|| provider_id.clone());

        for (model_id, model_value) in &provider_entry.models {
            let normalized_id = normalize_model_id(model_id);
            let canonical_id = canonical_by_normalized
                .get(&normalized_id)
                .and_then(|candidates| {
                    // 多候选时优先取 lab 与渠道 id 相同的那个（渠道就是原厂）
                    candidates
                        .iter()
                        .find(|cid| canonical_lab(cid) == provider_id)
                        .or_else(|| candidates.first())
                        .cloned()
                });

            let cost = model_value.get("cost");
            let limit = model_value.get("limit");

            let channel = ModelsDevChannel {
                provider: provider_id.clone(),
                provider_name: provider_name.clone(),
                model_id: model_id.clone(),
                normalized_id: normalized_id,
                declared_id: string_field(model_value, "id"),
                canonical_id,
                name: string_field(model_value, "name"),
                family: string_field(model_value, "family"),
                input: number_field(cost, "input"),
                output: number_field(cost, "output"),
                cache_read: number_field(cost, "cache_read"),
                cache_write: number_field(cost, "cache_write"),
                context: int_field(limit, "context"),
                output_limit: int_field(limit, "output"),
                status: string_field(model_value, "status"),
                reasoning: bool_field(model_value, "reasoning"),
                tool_call: bool_field(model_value, "tool_call"),
                attachment: bool_field(model_value, "attachment"),
                structured_output: bool_field(model_value, "structured_output"),
                temperature: model_value.get("temperature").and_then(Value::as_bool),
                open_weights: bool_field(model_value, "open_weights"),
                knowledge: string_field(model_value, "knowledge"),
                release_date: string_field(model_value, "release_date"),
                last_updated: string_field(model_value, "last_updated"),
                has_reasoning_options: model_value
                    .get("reasoning_options")
                    .map(|v| !v.is_null())
                    .unwrap_or(false),
                reasoning_options: parse_reasoning_options(
                    model_value.get("reasoning_options"),
                ),
                interleaved: model_value
                    .get("interleaved")
                    .map(|v| !v.is_null())
                    .unwrap_or(false),
                interleaved_field: parse_interleaved_field(model_value.get("interleaved")),
                input_limit: int_field(limit, "input"),
                output_modalities: model_value
                    .get("modalities")
                    .and_then(|m| m.get("output"))
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
                experimental: model_value
                    .get("experimental")
                    .map(|v| !v.is_null())
                    .unwrap_or(false),
                fast_mode: model_value
                    .get("experimental")
                    .and_then(|e| e.get("modes"))
                    .and_then(|m| m.get("fast"))
                    .cloned(),
                request_override: model_value.get("provider").cloned(),
                raw: model_value.clone(),
            };

            provider
                .entry(channel.normalized_id.clone())
                .or_default()
                .push(channel);
        }
    }

    Ok(ModelsDevIndex {
        canonical: catalog.models.clone(),
        provider,
        canonical_by_normalized,
        provider_meta: catalog.providers,
    })
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn bool_field(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn number_field(value: Option<&Value>, key: &str) -> Option<f64> {
    value?.get(key)?.as_f64()
}

fn int_field(value: Option<&Value>, key: &str) -> Option<i64> {
    value?.get(key)?.as_i64()
}

/// 抓取结果。
#[derive(Debug, Clone)]
pub struct ModelsDevFetchOutcome {
    /// 响应体原文。`None` 表示命中 304，内容未变更。
    pub raw: Option<String>,
    /// 本次响应的 ETag（304 时回显传入的 ETag）。
    pub etag: Option<String>,
    /// 是否命中 `304 Not Modified`。
    pub not_modified: bool,
}

impl ModelsDevFetchOutcome {
}

/// 抓取 `catalog.json`，携带 `If-None-Match` 做条件请求。
///
/// 失败重试 3 次（间隔 500ms）。返回 `not_modified = true` 表示上游内容未变更，
/// 调用方应沿用本地缓存，无需重新解析与落库。
pub async fn fetch_catalog_raw(
    client: &reqwest::Client,
    etag: Option<&str>,
) -> Result<ModelsDevFetchOutcome, String> {
    let mut last_error = String::new();

    for attempt in 1..=3 {
        let mut request = client
            .get(MODELS_DEV_CATALOG_URL)
            .timeout(Duration::from_secs(60))
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::USER_AGENT, "Mozilla/5.0 OpenHub/1.0");

        if let Some(tag) = etag {
            request = request.header(reqwest::header::IF_NONE_MATCH, tag);
        }

        match request.send().await {
            Ok(response) => {
                if response.status() == reqwest::StatusCode::NOT_MODIFIED {
                    return Ok(ModelsDevFetchOutcome {
                        raw: None,
                        etag: etag.map(str::to_string),
                        not_modified: true,
                    });
                }

                if let Err(error) = response.error_for_status_ref() {
                    last_error = format!("models.dev catalog HTTP 状态错误：{error}");
                } else {
                    let new_etag = response
                        .headers()
                        .get(reqwest::header::ETAG)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_string);

                    match response.text().await {
                        Ok(raw) => {
                            return Ok(ModelsDevFetchOutcome {
                                raw: Some(raw),
                                etag: new_etag,
                                not_modified: false,
                            })
                        }
                        Err(error) => last_error = format!("models.dev catalog 读取失败：{error}"),
                    }
                }
            }
            Err(error) => last_error = format!("models.dev catalog 下载失败：{error}"),
        }

        if attempt < 3 {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    Err(last_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_handles_variant_suffix() {
        assert_eq!(normalize_model_id("glm-5.2:free"), "glm-5-2");
        assert_eq!(normalize_model_id("glm-5.2:thinking"), "glm-5-2");
        assert_eq!(normalize_model_id("GLM-5.2"), "glm-5-2");
    }

    #[test]
    fn normalize_strips_prefix_and_punctuation() {
        assert_eq!(normalize_model_id("z-ai/glm-5.2"), "glm-5-2");
        assert_eq!(normalize_model_id("meta/llama-3.3-70b"), "llama-3-3-70b");
        assert_eq!(normalize_model_id("llama3.3:70b"), "llama-3-3");
        assert_eq!(normalize_model_id("qwen_max_2025"), "qwen-max-2025");
    }

    #[test]
    fn normalize_strips_quality_suffixes_iteratively() {
        assert_eq!(normalize_model_id("gemini-3-1-pro-preview"), "gemini-3-1-pro");
        assert_eq!(normalize_model_id("gemini-3-1-pro-fp8"), "gemini-3-1-pro");
        assert_eq!(normalize_model_id("gemma-4-31b-it"), "gemma-4-31b");
    }

    #[test]
    fn normalize_keeps_free_marker() {
        // ⚠️ 关键：-free 不得被剥离，否则 zenmux 会被误判为免费渠道
        assert_eq!(normalize_model_id("gemma-4-31b-it:free"), "gemma-4-31b");
        assert_eq!(normalize_model_id("glm-5.2-free"), "glm-5-2-free");
    }

    #[test]
    fn canonical_split() {
        assert_eq!(canonical_lab("openai/gpt-5.5"), "openai");
        assert_eq!(canonical_model_id("openai/gpt-5.5"), "gpt-5.5");
        assert_eq!(canonical_lab("standalone"), "standalone");
        assert_eq!(canonical_model_id("standalone"), "standalone");
    }

    #[test]
    fn build_index_links_provider_to_canonical() {
        let value = serde_json::json!({
            "models": {
                "zhipuai/glm-5.2": { "id": "glm-5.2", "name": "GLM-5.2" }
            },
            "providers": {
                "zhipuai": {
                    "id": "zhipuai",
                    "name": "Zhipu AI",
                    "models": {
                        "glm-5.2": { "id": "glm-5.2", "cost": { "input": 1.4, "output": 4.4 } }
                    }
                },
                "openrouter": {
                    "id": "openrouter",
                    "name": "OpenRouter",
                    "models": {
                        "z-ai/glm-5.2": { "id": "z-ai/glm-5.2", "cost": { "input": 0.0, "output": 0.0 } }
                    }
                }
            }
        });

        let index = build_index(&value).expect("index");
        let channels = index.channels_for("glm-5.2");
        assert_eq!(channels.len(), 2);

        let zhipuai = channels.iter().find(|c| c.provider == "zhipuai").unwrap();
        assert_eq!(zhipuai.canonical_id.as_deref(), Some("zhipuai/glm-5.2"));
        assert!(!zhipuai.is_zero_cost());

        let openrouter = channels.iter().find(|c| c.provider == "openrouter").unwrap();
        assert_eq!(openrouter.canonical_id.as_deref(), Some("zhipuai/glm-5.2"));
        assert!(openrouter.is_zero_cost());
        assert_eq!(openrouter.model_id, "z-ai/glm-5.2");
    }

    /// 全量归一化回归校验：对真实 catalog 快照里出现的**所有** id 导出归一化结果，
    /// 供与参考实现逐行 diff；同时构建索引并打印规模摘要，验证真实数据解析路径。
    ///
    /// ```sh
    /// MODELS_DEV_GOLDEN=/tmp/modelsdev_catalog.json \
    /// MODELS_DEV_GOLDEN_OUT=/tmp/rust_norm.tsv \
    ///   cargo test --lib golden_catalog_dump -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn golden_catalog_dump() {
        let Ok(input) = std::env::var("MODELS_DEV_GOLDEN") else {
            eprintln!("skip: MODELS_DEV_GOLDEN 未设置");
            return;
        };
        let output = std::env::var("MODELS_DEV_GOLDEN_OUT")
            .unwrap_or_else(|_| "/tmp/rust_norm.tsv".to_string());

        let raw = std::fs::read_to_string(&input).expect("读取 catalog 失败");
        let value: Value = serde_json::from_str(&raw).expect("解析 catalog 失败");

        let mut ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        if let Some(models) = value.get("models").and_then(Value::as_object) {
            for key in models.keys() {
                ids.insert(key.clone());
                ids.insert(canonical_model_id(key).to_string());
            }
        }
        if let Some(providers) = value.get("providers").and_then(Value::as_object) {
            for provider in providers.values() {
                if let Some(models) = provider.get("models").and_then(Value::as_object) {
                    for key in models.keys() {
                        ids.insert(key.clone());
                    }
                }
            }
        }

        let mut lines = String::new();
        for id in &ids {
            lines.push_str(id);
            lines.push('\t');
            lines.push_str(&normalize_model_id(id));
            lines.push('\n');
        }
        std::fs::write(&output, lines).expect("写出失败");

        let index = build_index(&value).expect("构建索引失败");
        let (canonical, providers, channel_entries, normalized_keys) = index.summary();
        let unmatched = index
            .provider
            .values()
            .flatten()
            .filter(|channel| channel.canonical_id.is_none())
            .count();
        let zero_cost = index
            .provider
            .values()
            .flatten()
            .filter(|channel| channel.is_zero_cost())
            .count();

        eprintln!(
            "ids={} -> {}\ncanonical={} providers={} channel_entries={} normalized_keys={}\nunmatched_canonical={} zero_cost_entries={}",
            ids.len(),
            output,
            canonical,
            providers,
            channel_entries,
            normalized_keys,
            unmatched,
            zero_cost
        );

        // 抽样：GLM-5.2 的渠道应当能回连到 canonical 身份
        let glm = index.channels_for("glm-5.2");
        let glm_providers: Vec<&str> = glm.iter().map(|c| c.provider.as_str()).collect();
        eprintln!(
            "glm-5.2 channels({})={:?}\nglm-5.2 zero-cost={:?}",
            glm.len(),
            glm_providers,
            index.zero_cost_providers("glm-5.2")
        );

        // 原厂渠道推导 vs llmpricing 详情页 ground truth
        // 期望值来自 23 个详情页的 RSC flight `hosts[].official`
        let expected: &[(&str, &str, &[&str])] = &[
            ("alibaba", "qwen3.7-max", &["alibaba", "alibaba-cn"]),
            ("amazon", "nova-2-lite", &[]),
            ("anthropic", "claude-sonnet-4-6", &["anthropic"]),
            ("bytedance-seed", "seed-2.0-code", &[]),
            ("cohere", "command-r-plus-08-2024", &["cohere"]),
            ("deepseek", "deepseek-v4-pro", &["deepseek"]),
            ("google", "gemma-4-31b-it", &["google"]),
            ("meituan", "longcat-2.0", &["longcat"]),
            ("meta", "llama-3.3-70b-instruct", &["llama"]),
            (
                "microsoft",
                "phi-4-multimodal-instruct",
                &["azure", "azure-cognitive-services"],
            ),
            ("minimax", "MiniMax-M3", &["minimax", "minimax-cn"]),
            ("mistral", "devstral-2512", &["mistral"]),
            ("moonshotai", "kimi-k3", &["moonshotai", "moonshotai-cn"]),
            ("nvidia", "nemotron-3-ultra-550b-a55b", &["nvidia"]),
            ("openai", "gpt-oss-120b", &[]),
            ("perplexity", "sonar", &["perplexity"]),
            ("poolside", "laguna-s-2.1", &["poolside"]),
            ("sakana", "fugu-ultra", &["sakana"]),
            ("stepfun", "step-3.7-flash", &["stepfun", "stepfun-ai"]),
            ("tencent", "hy3", &["tencent-tokenhub"]),
            ("thinkingmachines", "inkling", &["thinkingmachines"]),
            ("xai", "grok-4.6", &["xai"]),
            ("xiaomi", "mimo-v2.5-pro", &["xiaomi"]),
        ];

        let mut mismatches = 0;
        for (lab, model, want) in expected {
            let hosting: Vec<String> = index
                .distinct_providers(model);
            let got = crate::model::catalog::lab_registry::official_channels_for(lab, &hosting);
            let want_vec: Vec<String> = want.iter().map(|s| s.to_string()).collect();
            if got != want_vec {
                mismatches += 1;
                eprintln!("  DIFF {lab}/{model}: got={got:?} want={want_vec:?}");
            }
        }
        eprintln!(
            "official-channel ground truth: {}/{} matched",
            expected.len() - mismatches,
            expected.len()
        );
        assert_eq!(mismatches, 0, "原厂渠道推导与详情页 ground truth 不一致");
    }
}
