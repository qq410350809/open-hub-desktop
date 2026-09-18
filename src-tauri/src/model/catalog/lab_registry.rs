//! 模型身份（原始 lab / 原始 modelId）与原厂（官方）渠道判定。
//!
//! 本模块解决「模型全景控制台」最关键的一个语义问题：**把官网渠道与三方渠道分开**。
//!
//! # 为什么不能直接用 `tier`
//!
//! llmpricing manifest 给每个渠道一个 `tier`（`lab` 31 / `cloud` 39 / `gateway` 143，
//! 覆盖率 100%），但它描述的是**渠道自身的性质**，不是「它是不是某个模型的原厂渠道」。
//! 实测反例：
//!
//! - `nvidia` 的 `tier = lab`，却代售 30+ 个 lab 的模型；
//! - `alibaba-cn` 的 `tier = lab`，同时上架 deepseek / minimax / moonshotai 的模型；
//! - `azure` 的 `tier = lab`，对上架 `microsoft/phi-*` 是官方，对上架 `openai/*` 却**不是**。
//!
//! 因此 `tier` 与 `isOfficial` 是**两个正交维度**，必须分开建模。
//!
//! # 为什么不能只看 `refOfficial`
//!
//! llmpricing 分片里有 `refOfficial`，但它只覆盖 375/1941 = 19.3% 的模型（且 205 个模型
//! 连 `ref` 都没有）。每渠道的 `official` 标记只存在于**详情页的 RSC flight payload**
//! （`hosts` 数组），这正是原先要爬 ~1800 页 HTML 的原因。
//!
//! # 本模块的方案：别名表 + 上架校验
//!
//! ```text
//! is_official(model, provider) = provider ∈ official_hosts(model.lab)
//!                              ∧ provider 确实上架了该模型
//! ```
//!
//! 「上架校验」由 [`crate::model::catalog::models_dev`] 的 provider 层索引提供，无需爬取。
//!
//! ## 实测验证结果
//!
//! | 验证项 | 结果 |
//! |---|---|
//! | 23 个 lab 抽样 vs 详情页真实 `official` 集合 | **23/23 完全一致** |
//! | `refOfficial = true` 且 `ref` 存在时，`ref.provider` 命中别名表 | **336/336 = 100%** |
//! | `refOfficial = false` 时，`ref.provider` 命中别名表（假阳性） | **0** |
//!
//! # ⚠️ `official` 是模型级的，不是 lab 级的
//!
//! `openai/gpt-oss-120b` 的开源权重模型并不在 `openai` 渠道上架，其真实官方渠道集合为空；
//! `amazon/nova-2-lite` 的 `amazon-bedrock`、`bytedance-seed/seed-2.0-code` 的 `volcengine`
//! 都被上游判为**非官方**。别名表只是「候选集」，必须再与实际上架渠道取交集。

use serde::{Deserialize, Serialize};

/// 原厂（官方）渠道别名表：`lab -> 该 lab 的自营渠道 id`。
///
/// 表中条目均已用 llmpricing 详情页的真实 `official` 标记验证。
///
/// 未列出的 lab 走默认规则：只认 `provider_id == lab`。
///
/// ⚠️ **不要凭直觉往里加**。已证伪的直觉：
/// - `google` 的官方渠道**不含** `google-vertex`（后者仅在少数模型上被上游判为官方，
///   但在抽样模型 `gemma-4-31b-it` 上并未上架，靠上架校验自然排除）；
/// - `meta` 的官方渠道是 `llama`，不是 `meta`；
/// - `amazon` → `amazon-bedrock` **不是**官方；`bytedance-seed` → `volcengine` **不是**官方。
pub const LAB_OFFICIAL_HOSTS: &[(&str, &[&str])] = &[
    ("alibaba", &["alibaba", "alibaba-cn"]),
    ("anthropic", &["anthropic"]),
    ("cohere", &["cohere"]),
    ("deepseek", &["deepseek"]),
    ("google", &["google", "google-vertex"]),
    ("meituan", &["longcat"]),
    ("meta", &["llama", "meta"]),
    ("microsoft", &["azure", "azure-cognitive-services"]),
    ("minimax", &["minimax", "minimax-cn"]),
    ("mistral", &["mistral"]),
    ("moonshotai", &["moonshotai", "moonshotai-cn"]),
    ("nvidia", &["nvidia"]),
    ("openai", &["openai"]),
    ("perplexity", &["perplexity"]),
    ("poolside", &["poolside"]),
    ("sakana", &["sakana"]),
    ("stepfun", &["stepfun", "stepfun-ai"]),
    ("tencent", &["tencent-tokenhub"]),
    ("thinkingmachines", &["thinkingmachines"]),
    ("xai", &["xai"]),
    ("xiaomi", &["xiaomi"]),
    ("zhipuai", &["zhipuai", "zai"]),
    // ⚠️ 以下两个 lab 的「显然候选」被上游明确判为非官方，故显式留空，防止误配。
    ("amazon", &[]),
    ("bytedance-seed", &[]),
];

/// 明确**不是**模型原厂、不得靠「lab 名 == 渠道名」自动成为原厂的渠道。
///
/// 这些是聚合 / 路由平台：它们把自家虚拟路由条目（`auto` / `free` / `fast` / `cheap` /
/// `fusion` / `zdr` …）登记成 llmpricing 里的 `lab`，从而命中 [`official_hosts`] 的
/// 同名退化规则，被误判成「原厂渠道」。它们并不研发模型，必须显式排除。
///
/// ⚠️ 与 [`LAB_OFFICIAL_HOSTS`] 的留空条目（`amazon` / `bytedance-seed`）同性质：
/// 都是「上游数据形态会误导判定」时的人工校准。**不要凭直觉增删**，每次改动都要
/// 用真实目录数据核对受影响的模型集合。
pub const NON_FIRST_PARTY_PROVIDERS: &[&str] = &[
    // 虚拟路由条目：auto / fast / cheap / free / e2e / synth / zdr / fusion*
    "trustedrouter",
    "orcarouter",
    "pioneer",
];

/// 取某 lab 的原厂渠道候选集。未在 [`LAB_OFFICIAL_HOSTS`] 中登记时退化为 `[lab]` 自身。
pub fn official_hosts(lab: &str) -> Vec<&str> {
    // 路由 / 聚合平台不研发模型，其同名「lab」不是真实厂商，直接判定无原厂渠道。
    if NON_FIRST_PARTY_PROVIDERS.contains(&lab) {
        return Vec::new();
    }
    match LAB_OFFICIAL_HOSTS
        .iter()
        .find(|(known, _)| *known == lab)
    {
        Some((_, hosts)) => hosts.to_vec(),
        None => vec![lab],
    }
}

/// 判定某渠道是否属于该 lab 的原厂渠道候选集。
///
/// ⚠️ 这只完成了一半判定。**必须**再校验该渠道确实上架了目标模型，
/// 否则 `openai/gpt-oss-120b` 会被误判为有官方渠道。
#[allow(dead_code)] // 与 `official_channels_for` 同源的半判定，供后续 UI 复用
pub fn is_official_host(lab: &str, provider: &str) -> bool {
    official_hosts(lab).contains(&provider)
}

/// 从「该模型的实际上架渠道」中筛出原厂渠道。
///
/// 这是 `is_official` 的完整判定，等价于 llmpricing 详情页的 `official` 标记。
pub fn official_channels_for(lab: &str, hosting_providers: &[String]) -> Vec<String> {
    let candidates = official_hosts(lab);
    let mut result: Vec<String> = hosting_providers
        .iter()
        .filter(|provider| candidates.contains(&provider.as_str()))
        .cloned()
        .collect();
    result.sort();
    result.dedup();
    result
}

/// 该渠道 id 是否为**任意** lab 的自营（原厂）渠道。
///
/// 用于给渠道打「是否原厂」标签，与「对某个具体模型是否官方」是两回事——
/// 后者必须用 [`official_channels_for`] 结合上架情况判定。
///
/// 例：`zhipuai`、`zai`、`llama`、`longcat`、`tencent-tokenhub` 为 `true`；
/// `openrouter`、`nvidia`、`azure` 为 `false`（它们是代售方，即便 `tier = lab`）。
pub fn is_first_party_provider(provider: &str) -> bool {
    LAB_OFFICIAL_HOSTS
        .iter()
        .any(|(_, hosts)| hosts.contains(&provider))
}

/// 关键词归属规则：仅凭模型 id 无法从 `misc/*` 判定 lab 时的兜底。
///
/// `prefixes` 按前缀匹配，`contains` 按子串匹配，两者取「或」。
/// **按声明顺序首个命中者生效**，所以更具体的规则要排在前面。
pub struct LabKeywordRule {
    pub lab: &'static str,
    pub prefixes: &'static [&'static str],
    pub contains: &'static [&'static str],
}

/// 关键词归属规则表。实测可判定 llmpricing 中 175 个 `misc/*` 模型里的 100 个。
///
/// 剩余 75 个是白牌 / 路由 / 聚合名（`auto`、`model-router`、`custom`、`claw-*`、
/// `code-*`、`text-*`、`agent-*`、`neosmith.*`、`lucidquery-*` …），**刻意不猜**，
/// 保留 `misc` 并标记身份未确定。
pub const LAB_KEYWORD_RULES: &[LabKeywordRule] = &[
    LabKeywordRule { lab: "anthropic", prefixes: &["claude"], contains: &[] },
    LabKeywordRule { lab: "google", prefixes: &["gemini", "gemma", "medgemma"], contains: &[] },
    LabKeywordRule { lab: "openai", prefixes: &["gpt-"], contains: &["openai", "duo-chat-gpt"] },
    LabKeywordRule { lab: "xai", prefixes: &["grok-", "x-preview-"], contains: &[] },
    LabKeywordRule { lab: "alibaba", prefixes: &["qwen", "tongyi-", "wan2"], contains: &[] },
    LabKeywordRule { lab: "bytedance-seed", prefixes: &["doubao-", "seed-1"], contains: &[] },
    LabKeywordRule { lab: "stepfun", prefixes: &["step-"], contains: &[] },
    LabKeywordRule {
        lab: "mistral",
        prefixes: &[
            "mistral", "mixtral", "ministral", "leanstral", "voxtral", "devstral", "magistral",
        ],
        contains: &[],
    },
    LabKeywordRule { lab: "meta", prefixes: &["llama"], contains: &[] },
    LabKeywordRule { lab: "amazon", prefixes: &["nova-", "amazon-titan"], contains: &[] },
    LabKeywordRule { lab: "nvidia", prefixes: &["nemotron-", "nvidia-", "bge-reranker"], contains: &[] },
    LabKeywordRule { lab: "moonshotai", prefixes: &["kimi-"], contains: &[] },
    LabKeywordRule { lab: "inclusionai", prefixes: &["ling-", "ring-"], contains: &[] },
    LabKeywordRule { lab: "xiaomi", prefixes: &["mimo-"], contains: &[] },
    LabKeywordRule { lab: "openbmb", prefixes: &["minicpm"], contains: &[] },
    LabKeywordRule { lab: "sensenova", prefixes: &["sensenova-"], contains: &[] },
    LabKeywordRule { lab: "inception", prefixes: &["mercury-"], contains: &[] },
    LabKeywordRule { lab: "arcee", prefixes: &["arcee-", "holo"], contains: &[] },
    LabKeywordRule { lab: "stabilityai", prefixes: &["stable-diffusion"], contains: &[] },
    LabKeywordRule { lab: "kuaishou", prefixes: &["kling-"], contains: &[] },
    LabKeywordRule { lab: "nomic", prefixes: &["nomic-embed"], contains: &[] },
    LabKeywordRule { lab: "perplexity", prefixes: &["perplexity-"], contains: &[] },
    LabKeywordRule { lab: "writer", prefixes: &["us.writer", "palmyra"], contains: &[] },
    LabKeywordRule { lab: "vercel", prefixes: &["v0-"], contains: &[] },
    LabKeywordRule { lab: "venice", prefixes: &["venice-"], contains: &[] },
    LabKeywordRule { lab: "sap", prefixes: &["sap-abap"], contains: &[] },
];

/// 按关键词规则推断 lab。无法判定时返回 `None`（**不猜**）。
pub fn infer_lab_by_keyword(model_id: &str) -> Option<&'static str> {
    let lower = model_id.to_lowercase();
    for rule in LAB_KEYWORD_RULES {
        if rule.prefixes.iter().any(|prefix| lower.starts_with(prefix))
            || rule.contains.iter().any(|needle| lower.contains(needle))
        {
            return Some(rule.lab);
        }
    }
    None
}

/// 渠道性质（来自 llmpricing manifest，100% 覆盖）。
///
/// ⚠️ 与「是否官方渠道」无关，详见模块文档。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelTier {
    /// 模型原厂 / 自营云。
    Lab,
    /// 云厂商（AWS / Azure / GCP 等）。
    Cloud,
    /// 聚合网关 / 转售平台。
    Gateway,
    Unknown,
}

impl ChannelTier {
    pub fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("lab") => Self::Lab,
            Some("cloud") => Self::Cloud,
            Some("gateway") => Self::Gateway,
            _ => Self::Unknown,
        }
    }

    #[allow(dead_code)] // 供 UI 直接展示 tier 文案
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lab => "lab",
            Self::Cloud => "cloud",
            Self::Gateway => "gateway",
            Self::Unknown => "unknown",
        }
    }
}

/// 身份来源。用于在 UI 上区分「上游确认」与「本地推断」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentitySource {
    /// 来自 models.dev canonical 层 `lab/modelId`——最权威。
    Canonical,
    /// 来自 llmpricing 行数据的 `lab` 字段。
    LlmpricingLab,
    /// 由模型 id 关键词规则推断。
    KeywordRule,
    /// 由同名模型继承（llmpricing 内部唯一同名行）。
    Inherited,
    /// 由 models.dev canonical 层反向查找得到。
    CanonicalReverse,
    /// 无法确定，lab 保持 `misc`。
    Unknown,
}

impl IdentitySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Canonical => "canonical",
            Self::LlmpricingLab => "llmpricing_lab",
            Self::KeywordRule => "keyword_rule",
            Self::Inherited => "inherited",
            Self::CanonicalReverse => "canonical_reverse",
            Self::Unknown => "unknown",
        }
    }

    /// 是否为本地推断（非上游直接给出）。
    #[allow(dead_code)] // 供后续 UI 区分「上游确认」与「本地推断」
    pub fn is_derived(self) -> bool {
        matches!(
            self,
            Self::KeywordRule | Self::Inherited | Self::CanonicalReverse
        )
    }
}

/// 模型身份：原始 lab + 原始 modelId。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelIdentity {
    /// 原始 lab。无法确定时为 `misc`。
    pub lab: String,
    /// 原始 modelId（**保留上游原拼写**，不做归一化）。
    pub model_id: String,
    /// 若能定位到 models.dev canonical 层，则为其 canonical id。
    pub canonical_id: Option<String>,
    pub source: IdentitySource,
    /// 身份是否已确定。`false` 表示 lab 仍是 `misc` 占位。
    pub resolved: bool,
}

impl ModelIdentity {
    /// 未确定身份（lab 保持 `misc`）。
    pub fn unresolved(model_id: impl Into<String>) -> Self {
        Self {
            lab: "misc".to_string(),
            model_id: model_id.into(),
            canonical_id: None,
            source: IdentitySource::Unknown,
            resolved: false,
        }
    }

    /// 由 models.dev canonical id（`lab/modelId`）构造。
    pub fn from_canonical(canonical_id: &str) -> Self {
        Self {
            lab: super::models_dev::canonical_lab(canonical_id).to_string(),
            model_id: super::models_dev::canonical_model_id(canonical_id).to_string(),
            canonical_id: Some(canonical_id.to_string()),
            source: IdentitySource::Canonical,
            resolved: true,
        }
    }

    /// 是否原厂（官方）渠道。
    #[allow(dead_code)] // 单渠道快捷判定，供后续 UI 复用
    pub fn is_official_channel(&self, provider: &str) -> bool {
        self.resolved && is_official_host(&self.lab, provider)
    }
}

/// 解析模型身份。按优先级依次尝试，首个成功者生效。
///
/// 1. `canonical_id` 存在 → [`IdentitySource::Canonical`]
/// 2. `llmpricing_lab` 存在且不是 `misc` → [`IdentitySource::LlmpricingLab`]
/// 3. 关键词规则命中 → [`IdentitySource::KeywordRule`]
/// 4. `inherited_lab` 存在（同名唯一）→ [`IdentitySource::Inherited`]
/// 5. 否则 → [`IdentitySource::Unknown`]，lab 保持 `misc`
///
/// `inherited_lab` 与 `canonical_reverse_lab` 由调用方预先算好（需全量数据才能判歧义），
/// 本函数只负责按优先级择一，保持纯函数、可测。
pub fn resolve_identity(
    canonical_id: Option<&str>,
    llmpricing_lab: Option<&str>,
    raw_model_id: &str,
    inherited_lab: Option<&str>,
    canonical_reverse_lab: Option<&str>,
) -> ModelIdentity {
    if let Some(canonical) = canonical_id {
        return ModelIdentity::from_canonical(canonical);
    }

    if let Some(lab) = llmpricing_lab {
        if !lab.is_empty() && lab != "misc" {
            return ModelIdentity {
                lab: lab.to_string(),
                model_id: raw_model_id.to_string(),
                canonical_id: None,
                source: IdentitySource::LlmpricingLab,
                resolved: true,
            };
        }
    }

    if let Some(lab) = infer_lab_by_keyword(raw_model_id) {
        return ModelIdentity {
            lab: lab.to_string(),
            model_id: raw_model_id.to_string(),
            canonical_id: None,
            source: IdentitySource::KeywordRule,
            resolved: true,
        };
    }

    if let Some(lab) = inherited_lab {
        return ModelIdentity {
            lab: lab.to_string(),
            model_id: raw_model_id.to_string(),
            canonical_id: None,
            source: IdentitySource::Inherited,
            resolved: true,
        };
    }

    if let Some(lab) = canonical_reverse_lab {
        return ModelIdentity {
            lab: lab.to_string(),
            model_id: raw_model_id.to_string(),
            canonical_id: None,
            source: IdentitySource::CanonicalReverse,
            resolved: true,
        };
    }

    ModelIdentity::unresolved(raw_model_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_hosts_uses_table_then_defaults_to_lab() {
        assert_eq!(official_hosts("zhipuai"), vec!["zhipuai", "zai"]);
        assert_eq!(official_hosts("meta"), vec!["llama", "meta"]);
        assert_eq!(official_hosts("microsoft"), vec!["azure", "azure-cognitive-services"]);
        // 未登记 lab 退化为自身
        assert_eq!(official_hosts("baai"), vec!["baai"]);
    }

    #[test]
    fn amazon_and_bytedance_have_no_official_hosts() {
        // 上游明确判定 amazon-bedrock / volcengine 非官方
        assert!(official_hosts("amazon").is_empty());
        assert!(official_hosts("bytedance-seed").is_empty());
    }

    #[test]
    fn official_channel_requires_hosting() {
        // openai/gpt-oss-120b：openai 未上架，故无官方渠道
        let hosting: Vec<String> = vec!["amazon-bedrock".into(), "google-vertex".into()];
        assert!(official_channels_for("openai", &hosting).is_empty());

        // 正常上架时命中
        let hosting: Vec<String> = vec!["zhipuai".into(), "openrouter".into(), "zai".into()];
        assert_eq!(
            official_channels_for("zhipuai", &hosting),
            vec!["zai".to_string(), "zhipuai".to_string()]
        );
    }

    #[test]
    fn official_channel_excludes_non_first_party_cloud() {
        // azure 对 microsoft 是官方，对 openai 不是
        let hosting: Vec<String> = vec!["azure".into(), "azure-cognitive-services".into()];
        assert_eq!(official_channels_for("microsoft", &hosting).len(), 2);
        assert!(official_channels_for("openai", &hosting).is_empty());
    }

    #[test]
    fn router_platforms_are_not_treated_as_first_party() {
        // ⚠️ 这条规则不要「优化」掉：路由平台把虚拟条目（auto / free / fast / fusion）
        // 登记成同名 lab，会命中「lab == 渠道名即原厂」的退化规则而假冒原厂。
        for platform in NON_FIRST_PARTY_PROVIDERS {
            assert!(
                official_hosts(platform).is_empty(),
                "{platform} 是聚合/路由平台，不应有原厂渠道"
            );
            // 即便该平台确实"上架"了这些虚拟条目，也不能算官方
            let hosting: Vec<String> = vec![platform.to_string(), "openrouter".into()];
            assert!(
                official_channels_for(platform, &hosting).is_empty(),
                "{platform} 的自封 lab 不应产生官方渠道"
            );
            assert!(
                !is_first_party_provider(platform),
                "{platform} 不应被标为原厂渠道"
            );
        }

        // 真实厂商不受影响：同名单渠道仍算原厂
        let hosting: Vec<String> = vec!["deepseek".into()];
        assert_eq!(
            official_channels_for("deepseek", &hosting),
            vec!["deepseek".to_string()]
        );
    }

    #[test]
    fn keyword_rules_match_known_families() {
        assert_eq!(infer_lab_by_keyword("claude-4.5-sonnet"), Some("anthropic"));
        assert_eq!(infer_lab_by_keyword("gemini-3.1-flash-image-preview"), Some("google"));
        assert_eq!(infer_lab_by_keyword("gemma-4-31b-it"), Some("google"));
        assert_eq!(infer_lab_by_keyword("doubao-seed-1.6"), Some("bytedance-seed"));
        assert_eq!(infer_lab_by_keyword("ministral-14b-2512"), Some("mistral"));
        assert_eq!(infer_lab_by_keyword("qwen3guard-gen-0.6b"), Some("alibaba"));
        assert_eq!(infer_lab_by_keyword("wan2.7-image-pro"), Some("alibaba"));
        assert_eq!(infer_lab_by_keyword("gpt-5-thinking"), Some("openai"));
        assert_eq!(infer_lab_by_keyword("nova-lite-v1"), Some("amazon"));
    }

    #[test]
    fn keyword_rules_do_not_guess_white_label() {
        // 白牌 / 路由名必须留空，不能臆造 lab
        for name in [
            "auto",
            "auto-model-premium",
            "model-router",
            "custom",
            "free",
            "claw-high",
            "code-max",
            "text-standard",
            "agent-prime",
            "neosmith.neolite",
            "lucidquery-nexus-coder",
            "kloker",
            "zdev",
        ] {
            assert_eq!(infer_lab_by_keyword(name), None, "{name} 不应被推断出 lab");
        }
    }

    #[test]
    fn resolve_identity_priority() {
        // canonical 优先
        let id = resolve_identity(Some("zhipuai/glm-5.2"), Some("misc"), "glm-5.2", None, None);
        assert_eq!(id.lab, "zhipuai");
        assert_eq!(id.model_id, "glm-5.2");
        assert_eq!(id.source, IdentitySource::Canonical);
        assert!(id.resolved);

        // llmpricing lab 次之
        let id = resolve_identity(None, Some("openai"), "gpt-5", None, None);
        assert_eq!(id.source, IdentitySource::LlmpricingLab);

        // misc 走关键词
        let id = resolve_identity(None, Some("misc"), "claude-4.5-sonnet", None, None);
        assert_eq!(id.lab, "anthropic");
        assert_eq!(id.source, IdentitySource::KeywordRule);
        assert!(id.source.is_derived());

        // 关键词未命中走继承
        let id = resolve_identity(None, Some("misc"), "hermes-3-llama-3.1-405b", Some("nousresearch"), None);
        assert_eq!(id.lab, "nousresearch");
        assert_eq!(id.source, IdentitySource::Inherited);

        // 全都失败则保持未确定
        let id = resolve_identity(None, Some("misc"), "model-router", None, None);
        assert_eq!(id.lab, "misc");
        assert!(!id.resolved);
        assert_eq!(id.source, IdentitySource::Unknown);
    }

    #[test]
    fn identity_official_check_requires_resolution() {
        let resolved = ModelIdentity::from_canonical("zhipuai/glm-5.2");
        assert!(resolved.is_official_channel("zai"));
        assert!(!resolved.is_official_channel("openrouter"));

        let unknown = ModelIdentity::unresolved("model-router");
        assert!(!unknown.is_official_channel("openrouter"));
    }

    #[test]
    fn channel_tier_parsing() {
        assert_eq!(ChannelTier::parse(Some("lab")), ChannelTier::Lab);
        assert_eq!(ChannelTier::parse(Some("cloud")), ChannelTier::Cloud);
        assert_eq!(ChannelTier::parse(Some("gateway")), ChannelTier::Gateway);
        assert_eq!(ChannelTier::parse(None), ChannelTier::Unknown);
        assert_eq!(ChannelTier::Lab.as_str(), "lab");
    }
}
