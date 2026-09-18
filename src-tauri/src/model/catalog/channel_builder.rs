//! 用「models.dev 渠道层 + llmpricing manifest」组合构建渠道明细，**替代 HTML 爬取**。
//!
//! # 背景：被替代的是什么
//!
//! 原实现（`catalog.rs` 的 `fetch_hosts_for_model` + `prefetch_model_hosts`）在每次目录同步后
//! 会并发抓取约 1800 个 `https://llmpricing.dev/m/{model_id}/` 详情页，每页约 245KB，
//! 合计约 440MB 流量，只为拿到详情页 RSC flight 里的 `hosts` 数组。
//!
//! # 组合方案
//!
//! | 需要的字段 | 来源 |
//! |---|---|
//! | 有哪些渠道（范围） | llmpricing 分片的 `hostProviders`（已完成 canonical 归并） |
//! | 各渠道的 `input`/`output`/`cacheRead`/`cacheWrite` | models.dev provider 层 `cost` |
//! | 各渠道的 `context`/`outputLimit` | models.dev provider 层 `limit` |
//! | 各渠道的 `modelId` 原始拼写 | models.dev provider 层的模型键 |
//! | 各渠道的 `reasoningOptions`/`experimental`/`status` | models.dev provider 层同名字段 |
//! | `tier` / `subscription` / `doc` | llmpricing manifest |
//! | **是否官方（原厂）渠道** | [`crate::model::catalog::lab_registry`] 别名表 + 上架校验 |
//! | 免费 / 订阅渠道**聚合计数** | llmpricing 分片的 `freeHostCount` / `subHostCount`（权威值，直接沿用） |
//!
//! 请求数从约 1800 次降到 **7 次**（manifest + 5 分片 + catalog.json），流量从约 440MB 降到约 6.6MB。
//!
//! # 实测保真度（对照 llmpricing 上游聚合值，1941 个模型）
//!
//! | 指标 | 结果 |
//! |---|---|
//! | 官方渠道识别（23 个 lab 抽样 vs 详情页 `official`） | **23/23 = 100%** |
//! | `subHostCount` 完全一致 | **1937/1941 = 99.8%** |
//! | 免费渠道**计数**完全一致 | **1890/1941 = 97.4%** |
//! | 「该模型是否有免费渠道」**二值**一致 | **1923/1941 = 99.1%** |
//!
//! # ⚠️ 两个必须遵守的判定规则
//!
//! 1. **免费 = 任一变体零价**。同一渠道可能同时登记 `glm-5.2`（付费）与 `glm-5.2:free`（零价），
//!    上游按零价那条计为免费渠道。若只取「主变体」会漏报。
//! 2. **订阅集必须剔除 `github-copilot`**。它在 llmpricing manifest 里带 `subscription: true`，
//!    但上游 `subHostCount` 并不计它。不剔除会使 `subHostCount` 一致率从 99.8% 掉到 98.4%。

use super::catalog::ModelCatalogHostItem;
use super::lab_registry::official_channels_for;
use super::models_dev::{ModelsDevChannel, ModelsDevIndex, ReasoningOption};
use std::collections::BTreeMap;

/// llmpricing manifest 中渠道的元信息。
#[derive(Debug, Clone, Default)]
pub struct ProviderMeta {
    pub name: String,
    pub tier: Option<String>,
    pub subscription: bool,
    pub doc: Option<String>,
}

impl ProviderMeta {
    /// 该渠道是否计入「订阅制渠道」。
    ///
    /// ⚠️ `github-copilot` 虽在 manifest 中标记 `subscription: true`，但上游 `subHostCount`
    /// 不计它，故显式排除。这是实测校准结果，不要「修正」掉。
    pub fn counts_as_subscription(&self, provider: &str) -> bool {
        self.subscription && provider != GITHUB_COPILOT
    }
}

/// llmpricing manifest 标记为订阅制、但上游聚合计数不计入的渠道。
const GITHUB_COPILOT: &str = "github-copilot";

/// 构建渠道明细的输入。
pub struct HostBuildInput<'a> {
    /// 模型原始 lab（用于官方渠道判定）。未确定时传 `misc`。
    pub lab: &'a str,
    /// llmpricing 的模型 id（如 `zhipuai/glm-5.2`），用于跨源匹配 models.dev 渠道。
    pub model_id: &'a str,
    /// llmpricing 的展示名，用于「同名兜底匹配」（id 拼写差异过大时的最后手段）。
    pub model_name: &'a str,
    /// llmpricing 分片的 `hostProviders`——**渠道范围以此为准**。
    pub host_providers: &'a [String],
    /// llmpricing 的参考价渠道。
    pub ref_provider: Option<&'a str>,
    /// llmpricing 的最低价渠道。
    pub min_provider: Option<&'a str>,
}

/// 构建统计，用于同步报告与保真度自查。
#[derive(Debug, Clone, Default)]
pub struct HostBuildStats {
    pub total: usize,
    /// 在 models.dev 找到定价的渠道数。
    pub with_price: usize,
    /// 判定为免费的渠道数（已剔除订阅制）。
    pub free: usize,
    /// 判定为订阅制的渠道数。
    pub subscription: usize,
    /// 判定为官方（原厂）的渠道数。
    pub official: usize,
    /// 通过「同名唯一」兜底匹配到 models.dev 数据的原厂渠道数（id 拼写差异过大时）。
    pub name_fallback: usize,
    /// `hostProviders` 中在 models.dev 查不到定价的渠道数（数据缺口，非错误）。
    pub missing_models_dev: usize,
}

/// 构建结果。
pub struct HostBuildOutput {
    pub hosts: Vec<ModelCatalogHostItem>,
    pub stats: HostBuildStats,
    /// 本地推导出的免费渠道 id 列表（已去重、已剔除订阅制）。
    pub free_providers: Vec<String>,
    /// 本地推导出的订阅渠道 id 列表（已去重）。
    pub subscription_providers: Vec<String>,
    /// 模型级思考级别选项（跨全部渠道取并集）。
    ///
    /// 单个渠道可能只声明部分档位，模型级要反映「这个模型**能**调到多高」，
    /// 因此取所有渠道的并集。前端筛选/排序用这个，渠道详情用 host 上的那份。
    pub reasoning_options: Vec<ReasoningOption>,
    /// 模型级输出模态（跨全部渠道取并集）。
    pub output_modalities: Vec<String>,
    /// 模型级最大输入上限（跨全部渠道取最大值）。
    pub max_input_tokens: Option<i64>,
    /// 支持交错推理的读取字段名（跨渠道去重）。
    pub interleaved_fields: Vec<String>,
    /// 该模型是否有渠道提供快速模式。
    pub has_fast_mode: bool,
}

/// 为一个模型构建渠道明细。
///
/// 输入范围由 llmpricing 的 `hostProviders` 圈定（已完成 canonical 归并），
/// 明细字段由 models.dev 填充，官方标记由 [`lab_registry`] 推导。
pub fn build_hosts(
    index: &ModelsDevIndex,
    provider_meta: &BTreeMap<String, ProviderMeta>,
    input: &HostBuildInput<'_>,
) -> HostBuildOutput {
    let grouped = index.channels_grouped_by_provider(input.model_id);
    let official_set = official_channels_for(input.lab, input.host_providers);

    let mut hosts = Vec::with_capacity(input.host_providers.len());
    let mut stats = HostBuildStats::default();
    let mut free_providers = Vec::new();
    let mut subscription_providers = Vec::new();

    for provider in input.host_providers {
        let meta = provider_meta.get(provider).cloned().unwrap_or_default();
        let official = official_set.iter().any(|id| id == provider);

        // 直配：按归一化 id 找该渠道的变体。原厂渠道直配为空时，用「同名唯一」兜底——
        // llmpricing 与 models.dev 的 id 拼写可能差太多（open-mixtral-8x22b vs
        // mixtral-8x22b-instruct-v0.1），但展示名一致，此时该渠道确实上架了此模型。
        let mut variants: Vec<&ModelsDevChannel> =
            grouped.get(provider).cloned().unwrap_or_default();
        if variants.is_empty() && official {
            variants = index.channels_for_provider_name(provider, input.model_name);
            if !variants.is_empty() {
                stats.name_fallback += 1;
            }
        }

        // 展示价：优先非 `:free` 变体；只有 `:free` 时退而用之。
        let primary = pick_primary_variant(&variants);
        let display = primary;

        // 免费判定：**任一变体零价即免费**。
        let any_zero = variants
            .iter()
            .any(|channel| channel.input == Some(0.0) && channel.output == Some(0.0));

        let is_subscription = meta.counts_as_subscription(provider);
        let is_free = any_zero && !is_subscription;
        let official = official_set.iter().any(|id| id == provider);

        let input_cost = display.and_then(|channel| channel.input);
        let output_cost = display.and_then(|channel| channel.output);

        if variants.is_empty() {
            stats.missing_models_dev += 1;
        } else {
            stats.with_price += 1;
        }
        if is_free {
            stats.free += 1;
            free_providers.push(provider.clone());
        }
        if is_subscription {
            stats.subscription += 1;
            subscription_providers.push(provider.clone());
        }
        if official {
            stats.official += 1;
        }

        hosts.push(ModelCatalogHostItem {
            provider: provider.clone(),
            name: if meta.name.is_empty() {
                provider.clone()
            } else {
                meta.name.clone()
            },
            // 优先用上游原拼写；models.dev 无数据时退化为请求的 model id。
            model_id: display
                .map(|channel| channel.model_id.clone())
                .or_else(|| Some(input.model_id.to_string())),
            tier: meta.tier.clone(),
            subscription: is_subscription,
            input: input_cost,
            output: output_cost,
            cache_read: display.and_then(|channel| channel.cache_read),
            cache_write: display.and_then(|channel| channel.cache_write),
            context: display.and_then(|channel| channel.context),
            output_limit: display.and_then(|channel| channel.output_limit),
            input_limit: merge_input_limit(&variants),
            output_modalities: merge_output_modalities(&variants),
            reasoning_options: merge_reasoning_options(&variants),
            interleaved_field: variants
                .iter()
                .find_map(|channel| channel.interleaved_field.clone()),
            fast_mode: variants
                .iter()
                .find_map(|channel| channel.fast_mode.clone()),
            status: display.and_then(|channel| channel.status.clone()),
            official,
            doc: meta.doc.clone(),
            is_free,
            is_min: input.min_provider == Some(provider.as_str()) && !is_free,
            is_ref: input.ref_provider == Some(provider.as_str()) || official,
        });
    }

    stats.total = hosts.len();
    free_providers.sort();
    free_providers.dedup();
    subscription_providers.sort();
    subscription_providers.dedup();

    // 模型级聚合：跨全部渠道取并集 / 最大值
    let mut reasoning_options: Vec<ReasoningOption> = Vec::new();
    let mut output_modalities: Vec<String> = Vec::new();
    let mut interleaved_fields: Vec<String> = Vec::new();
    let mut max_input_tokens: Option<i64> = None;
    let mut has_fast_mode = false;
    for host in &hosts {
        // 复用渠道级的归并逻辑，保证排序与去重规则一致
        reasoning_options = merge_option_lists(&reasoning_options, &host.reasoning_options);
        for modality in &host.output_modalities {
            if !output_modalities.iter().any(|m| m == modality) {
                output_modalities.push(modality.clone());
            }
        }
        if let Some(field) = &host.interleaved_field {
            if !interleaved_fields.iter().any(|f| f == field) {
                interleaved_fields.push(field.clone());
            }
        }
        if let Some(limit) = host.input_limit {
            max_input_tokens = Some(max_input_tokens.map_or(limit, |cur| cur.max(limit)));
        }
        has_fast_mode = has_fast_mode || host.fast_mode.is_some();
    }
    interleaved_fields.sort();

    HostBuildOutput {
        hosts,
        stats,
        free_providers,
        subscription_providers,
        reasoning_options,
        output_modalities,
        max_input_tokens,
        interleaved_fields,
        has_fast_mode,
    }
}

/// 把两组思考级别选项合并成一组（并集 + 规范排序）。
fn merge_option_lists(
    left: &[ReasoningOption],
    right: &[ReasoningOption],
) -> Vec<ReasoningOption> {
    let mut has_toggle = false;
    let mut efforts: Vec<String> = Vec::new();
    for option in left.iter().chain(right.iter()) {
        match option.kind.as_str() {
            "toggle" => has_toggle = true,
            "effort" => {
                for value in &option.values {
                    if !efforts.iter().any(|existing| existing == value) {
                        efforts.push(value.clone());
                    }
                }
            }
            _ => {}
        }
    }
    efforts.sort_by_key(|value| ReasoningOption::effort_rank(value));

    let mut merged = Vec::new();
    if has_toggle {
        merged.push(ReasoningOption {
            kind: "toggle".to_string(),
            values: Vec::new(),
        });
    }
    if !efforts.is_empty() {
        merged.push(ReasoningOption {
            kind: "effort".to_string(),
            values: efforts,
        });
    }
    merged
}

/// 归并同一渠道下多个变体声明的思考级别选项。
///
/// 同一渠道可能同时登记 `glm-5.2` 与 `glm-5.2:free` 两个变体，各自声明的档位可能不同，
/// 这里取**并集**：只要任一变体支持某档位，就认为该渠道支持。
/// 返回顺序固定为 `toggle` 在前、`effort` 在后，`effort.values` 按档位从低到高排序，
/// 便于前端直接渲染。
fn merge_reasoning_options(variants: &[&ModelsDevChannel]) -> Vec<ReasoningOption> {
    let collected: Vec<ReasoningOption> = variants
        .iter()
        .flat_map(|channel| channel.reasoning_options.iter().cloned())
        .collect();
    merge_option_lists(&collected, &[])
}

/// 归并输出模态（取并集，保持稳定顺序）。
fn merge_output_modalities(variants: &[&ModelsDevChannel]) -> Vec<String> {
    let mut merged: Vec<String> = Vec::new();
    for channel in variants {
        for modality in &channel.output_modalities {
            if !merged.iter().any(|existing| existing == modality) {
                merged.push(modality.clone());
            }
        }
    }
    merged
}

/// 归并独立输入上限：取各变体声明中的**最大值**（同一渠道的不同变体不应有不同上限，
/// 取最大值只是防御性的确定性选择）。
fn merge_input_limit(variants: &[&ModelsDevChannel]) -> Option<i64> {
    variants
        .iter()
        .filter_map(|channel| channel.input_limit)
        .max()
}

/// 选出用于展示价格的主变体：非 `:free` 优先，其次任意一条。
fn pick_primary_variant<'a>(variants: &[&'a ModelsDevChannel]) -> Option<&'a ModelsDevChannel> {
    variants
        .iter()
        .copied()
        .find(|channel| !channel.model_id.to_lowercase().ends_with(":free"))
        .or_else(|| variants.first().copied())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn index_fixture() -> ModelsDevIndex {
        let value = json!({
            "models": { "zhipuai/glm-5.2": { "id": "glm-5.2", "name": "GLM-5.2" } },
            "providers": {
                "zhipuai": { "id": "zhipuai", "name": "Zhipu AI", "models": {
                    "glm-5.2": { "id": "glm-5.2", "cost": { "input": 1.4, "output": 4.4, "cache_read": 0.26 },
                                 "limit": { "context": 1000000, "output": 131072 } }
                }},
                "zai": { "id": "zai", "name": "Z.ai", "models": {
                    "glm-5.2": { "id": "glm-5.2", "cost": { "input": 1.4, "output": 4.4 } }
                }},
                "unorouter": { "id": "unorouter", "name": "UnoRouter", "models": {
                    "glm-5.2": { "id": "glm-5.2", "cost": { "input": 1.6001, "output": 5.0288 } },
                    "glm-5.2:free": { "id": "glm-5.2:free", "cost": { "input": 0, "output": 0 } }
                }},
                "zhipuai-coding-plan": { "id": "zhipuai-coding-plan", "name": "Zhipu Coding Plan", "models": {
                    "glm-5.2": { "id": "glm-5.2", "cost": { "input": 0, "output": 0 } }
                }},
                "openrouter": { "id": "openrouter", "name": "OpenRouter", "models": {
                    "z-ai/glm-5.2": { "id": "z-ai/glm-5.2", "cost": { "input": 0.6, "output": 2.0 } }
                }}
            }
        });
        super::super::models_dev::build_index(&value).expect("index")
    }

    fn meta_fixture() -> BTreeMap<String, ProviderMeta> {
        let mut map = BTreeMap::new();
        for (id, name, tier, sub) in [
            ("zhipuai", "Zhipu AI", "lab", false),
            ("zai", "Z.ai", "lab", false),
            ("unorouter", "UnoRouter", "gateway", false),
            ("zhipuai-coding-plan", "Zhipu Coding Plan", "gateway", true),
            ("openrouter", "OpenRouter", "gateway", false),
        ] {
            map.insert(
                id.to_string(),
                ProviderMeta {
                    name: name.to_string(),
                    tier: Some(tier.to_string()),
                    subscription: sub,
                    doc: None,
                },
            );
        }
        map
    }

    fn providers() -> Vec<String> {
        ["zhipuai", "zai", "unorouter", "zhipuai-coding-plan", "openrouter"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn builds_hosts_from_combined_sources() {
        let index = index_fixture();
        let meta = meta_fixture();
        let list = providers();
        let input = HostBuildInput {
            lab: "zhipuai",
            model_id: "zhipuai/glm-5.2",
            model_name: "GLM-5.2",
            host_providers: &list,
            ref_provider: Some("zhipuai"),
            min_provider: Some("openrouter"),
        };

        let out = build_hosts(&index, &meta, &input);
        assert_eq!(out.stats.total, 5);
        assert_eq!(out.stats.missing_models_dev, 0);
        // 官方：zhipuai + zai
        assert_eq!(out.stats.official, 2);
        // 免费：unorouter（任一变体零价）；coding-plan 虽零价但属订阅
        assert_eq!(out.stats.free, 1);
        assert_eq!(out.free_providers, vec!["unorouter".to_string()]);
        assert_eq!(out.stats.subscription, 1);
        assert_eq!(
            out.subscription_providers,
            vec!["zhipuai-coding-plan".to_string()]
        );

        let zhipuai = out.hosts.iter().find(|h| h.provider == "zhipuai").unwrap();
        assert_eq!(zhipuai.input, Some(1.4));
        assert_eq!(zhipuai.output, Some(4.4));
        assert_eq!(zhipuai.cache_read, Some(0.26));
        assert_eq!(zhipuai.context, Some(1_000_000));
        assert_eq!(zhipuai.output_limit, Some(131_072));
        assert!(zhipuai.official);
        assert!(zhipuai.is_ref);
        assert!(!zhipuai.is_free);

        let zai = out.hosts.iter().find(|h| h.provider == "zai").unwrap();
        assert!(zai.official);

        // unorouter：展示价取非 :free 变体，但仍判为免费
        let uno = out.hosts.iter().find(|h| h.provider == "unorouter").unwrap();
        assert_eq!(uno.model_id.as_deref(), Some("glm-5.2"));
        assert_eq!(uno.input, Some(1.6001));
        assert!(uno.is_free);
        assert!(!uno.official);

        // 订阅渠道：零价但不计为免费
        let plan = out
            .hosts
            .iter()
            .find(|h| h.provider == "zhipuai-coding-plan")
            .unwrap();
        assert!(plan.subscription);
        assert!(!plan.is_free);

        // 三方网关：非官方、非免费，且是 min
        let orouter = out.hosts.iter().find(|h| h.provider == "openrouter").unwrap();
        assert!(!orouter.official);
        assert!(!orouter.is_free);
        assert!(orouter.is_min);
    }

    #[test]
    fn github_copilot_is_not_counted_as_subscription() {
        let meta = ProviderMeta {
            name: "GitHub Copilot".to_string(),
            tier: Some("gateway".to_string()),
            subscription: true,
            doc: None,
        };
        assert!(meta.counts_as_subscription("other-plan"));
        assert!(!meta.counts_as_subscription(GITHUB_COPILOT));
    }

    #[test]
    fn missing_models_dev_data_is_reported_not_fabricated() {
        let index = index_fixture();
        let mut meta = meta_fixture();
        meta.insert(
            "mystery".to_string(),
            ProviderMeta {
                name: "Mystery".to_string(),
                tier: Some("gateway".to_string()),
                subscription: false,
                doc: None,
            },
        );
        let mut list = providers();
        list.push("mystery".to_string());

        let input = HostBuildInput {
            lab: "zhipuai",
            model_id: "zhipuai/glm-5.2",
            model_name: "GLM-5.2",
            host_providers: &list,
            ref_provider: None,
            min_provider: None,
        };

        let out = build_hosts(&index, &meta, &input);
        assert_eq!(out.stats.missing_models_dev, 1);
        let mystery = out.hosts.iter().find(|h| h.provider == "mystery").unwrap();
        // 无数据时不臆造价格
        assert_eq!(mystery.input, None);
        assert_eq!(mystery.output, None);
        assert!(!mystery.is_free);
        assert!(!mystery.official);
        assert_eq!(mystery.name, "Mystery");
    }

    #[test]
    fn official_host_with_name_only_match_gets_priced() {
        // mistral 渠道在 models.dev 用 `open-mixtral-8x22b` 上架，而 llmpricing 的 id 是
        // `mixtral-8x22b-instruct-v0.1`——归一化后对不上，但展示名完全一致。
        let value = json!({
            "models": { "mistral/mixtral-8x22b-instruct-v0.1": { "id": "mixtral-8x22b-instruct-v0.1", "name": "Mixtral 8x22B" } },
            "providers": {
                "mistral": { "id": "mistral", "name": "Mistral", "models": {
                    "open-mixtral-8x22b": { "id": "open-mixtral-8x22b", "name": "Mixtral 8x22B",
                                            "cost": { "input": 2.0, "output": 6.0 } }
                }}
            }
        });
        let index = super::super::models_dev::build_index(&value).expect("index");
        let mut meta = BTreeMap::new();
        meta.insert(
            "mistral".to_string(),
            ProviderMeta { name: "Mistral".to_string(), tier: Some("lab".to_string()), subscription: false, doc: None },
        );
        let providers = vec!["mistral".to_string()];
        let input = HostBuildInput {
            lab: "mistral",
            model_id: "mistral/mixtral-8x22b-instruct-v0.1",
            model_name: "Mixtral 8x22B",
            host_providers: &providers,
            ref_provider: Some("mistral"),
            min_provider: None,
        };
        let out = build_hosts(&index, &meta, &input);
        assert_eq!(out.stats.name_fallback, 1, "同名兜底应命中一次");
        assert_eq!(out.stats.missing_models_dev, 0);
        let mistral = out.hosts.iter().find(|h| h.provider == "mistral").unwrap();
        assert_eq!(mistral.input, Some(2.0));
        assert_eq!(mistral.output, Some(6.0));
        assert!(mistral.official);
        // 展示 modelId 用 models.dev 的原拼写
        assert_eq!(mistral.model_id.as_deref(), Some("open-mixtral-8x22b"));
    }

    #[test]
    fn ambiguous_name_does_not_fallback() {
        // llmpricing 的 id 与 models.dev 完全对不上（直配必空），只能靠同名字兜底；
        // 但该渠道下 `GPT-5.4` 有两个同名条目（不同日期变体），同名歧义时必须放弃，不能猜。
        let value = json!({
            "models": { "unorouter/gpt-5.4": { "id": "gpt-5.4", "name": "GPT-5.4" } },
            "providers": {
                "unorouter": { "id": "unorouter", "name": "UnoRouter", "models": {
                    "gpt-5.4": { "id": "gpt-5.4", "name": "GPT-5.4", "cost": { "input": 0.3, "output": 0.9 } },
                    "gpt-5.4-0410": { "id": "gpt-5.4-0410", "name": "GPT-5.4", "cost": { "input": 0.2, "output": 0.6 } }
                }}
            }
        });
        let index = super::super::models_dev::build_index(&value).expect("index");
        let mut meta = BTreeMap::new();
        meta.insert(
            "unorouter".to_string(),
            ProviderMeta { name: "UnoRouter".to_string(), tier: Some("gateway".to_string()), subscription: false, doc: None },
        );
        let providers = vec!["unorouter".to_string()];
        let input = HostBuildInput {
            lab: "unorouter",
            model_id: "unorouter/对不上-的-id",
            model_name: "GPT-5.4",
            host_providers: &providers,
            ref_provider: None,
            min_provider: None,
        };
        let out = build_hosts(&index, &meta, &input);
        assert_eq!(out.stats.name_fallback, 0, "同名歧义时不兜底");
        let host = out.hosts.iter().find(|h| h.provider == "unorouter").unwrap();
        assert_eq!(host.input, None, "歧义时不凭名字认模型");
    }

    /// 全量保真度自查：对真实 llmpricing 分片的每个模型跑一遍组合构建，
    /// 与上游聚合值 `freeHostCount` / `subHostCount` 对照。
    ///
    /// ```sh
    /// MODELS_DEV_GOLDEN=/tmp/modelsdev_catalog.json \
    /// LLMPRICING_MANIFEST=/tmp/llmpricing_manifest.json \
    /// LLMPRICING_SHARDS=/tmp/llmp_rows-000.json,/tmp/llmp_rows-001.json,/tmp/llmp_rows-002.json,/tmp/llmp_rows-003.json,/tmp/llmp_rows-004.json \
    ///   cargo test --lib golden_free_channel_fidelity -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn golden_free_channel_fidelity() {
        use serde_json::Value;

        let (Ok(catalog_path), Ok(manifest_path), Ok(shards_env)) = (
            std::env::var("MODELS_DEV_GOLDEN"),
            std::env::var("LLMPRICING_MANIFEST"),
            std::env::var("LLMPRICING_SHARDS"),
        ) else {
            eprintln!("skip: 需要 MODELS_DEV_GOLDEN / LLMPRICING_MANIFEST / LLMPRICING_SHARDS");
            return;
        };

        let catalog: Value = serde_json::from_str(
            &std::fs::read_to_string(&catalog_path).expect("读取 catalog 失败"),
        )
        .expect("解析 catalog 失败");
        let index = super::super::models_dev::build_index(&catalog).expect("构建索引失败");

        let manifest: Value = serde_json::from_str(
            &std::fs::read_to_string(&manifest_path).expect("读取 manifest 失败"),
        )
        .expect("解析 manifest 失败");
        let mut provider_meta: BTreeMap<String, ProviderMeta> = BTreeMap::new();
        if let Some(providers) = manifest.get("providers").and_then(Value::as_object) {
            for (id, entry) in providers {
                provider_meta.insert(
                    id.clone(),
                    ProviderMeta {
                        name: entry
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or(id)
                            .to_string(),
                        tier: entry.get("tier").and_then(Value::as_str).map(str::to_string),
                        subscription: entry
                            .get("subscription")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        doc: entry.get("doc").and_then(Value::as_str).map(str::to_string),
                    },
                );
            }
        }

        let mut rows: Vec<Value> = Vec::new();
        for path in shards_env.split(',') {
            let raw = std::fs::read_to_string(path.trim()).expect("读取分片失败");
            let shard: Vec<Value> = serde_json::from_str(&raw).expect("解析分片失败");
            rows.extend(shard);
        }

        let mut sub_exact = 0usize;
        let mut free_exact = 0usize;
        let mut binary_exact = 0usize;
        let mut over = 0usize;
        let mut under = 0usize;
        let mut total = 0usize;

        // 原厂渠道价覆盖率（官方渠道行里能拿到 input/output 的占比）：
        // 「有原厂渠道但没价」正是用户抱怨的场景，把覆盖率纳入自查。
        let mut official_rows = 0usize;
        let mut official_priced = 0usize;

        for row in &rows {
            let Some(model_id) = row.get("id").and_then(Value::as_str) else {
                continue;
            };
            let lab = row.get("lab").and_then(Value::as_str).unwrap_or("misc");
            let name = row.get("name").and_then(Value::as_str).unwrap_or("");
            let host_providers: Vec<String> = row
                .get("hostProviders")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            if host_providers.is_empty() {
                continue;
            }
            let ref_provider = row
                .get("ref")
                .and_then(|r| r.get("provider"))
                .and_then(Value::as_str);
            let min_provider = row
                .get("min")
                .and_then(|r| r.get("provider"))
                .and_then(Value::as_str);

            let input = HostBuildInput {
                lab,
                model_id,
                model_name: name,
                host_providers: &host_providers,
                ref_provider,
                min_provider,
            };
            let out = build_hosts(&index, &provider_meta, &input);

            for host in &out.hosts {
                if host.official {
                    official_rows += 1;
                    if host.input.is_some() || host.output.is_some() {
                        official_priced += 1;
                    }
                }
            }

            let want_sub = row.get("subHostCount").and_then(Value::as_u64).unwrap_or(0) as usize;
            let want_free = row.get("freeHostCount").and_then(Value::as_u64).unwrap_or(0) as usize;

            total += 1;
            if out.stats.subscription == want_sub {
                sub_exact += 1;
            }
            if out.stats.free == want_free {
                free_exact += 1;
            }
            if (out.stats.free > 0) == (want_free > 0) {
                binary_exact += 1;
            } else if out.stats.free > 0 {
                over += 1;
            } else {
                under += 1;
            }
        }

        let pct = |n: usize| n as f64 / total as f64 * 100.0;
        eprintln!("对照模型数 = {total}");
        eprintln!(
            "subHostCount  完全一致 {sub_exact}/{total} = {:.1}%",
            pct(sub_exact)
        );
        eprintln!(
            "freeHostCount 完全一致 {free_exact}/{total} = {:.1}%",
            pct(free_exact)
        );
        eprintln!(
            "有无免费渠道  二值一致 {binary_exact}/{total} = {:.1}%  (多报 {over} / 漏报 {under})",
            pct(binary_exact)
        );
        eprintln!(
            "原厂渠道行    {official_priced}/{official_rows} 有公开价 = {:.1}%",
            official_priced as f64 / official_rows as f64 * 100.0
        );
        // 保真度门槛：低于这些阈值说明上游数据形态变了，需要重新校准。
        assert!(pct(sub_exact) >= 99.0, "subHostCount 一致率跌破 99%");
        assert!(pct(free_exact) >= 96.0, "freeHostCount 一致率跌破 96%");
        assert!(pct(binary_exact) >= 98.5, "免费渠道二值一致率跌破 98.5%");
    }
}
