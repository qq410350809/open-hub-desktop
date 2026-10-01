//! 保守的目录能力匹配；不复用同步模块会剥语义后缀的 normalize_model_id。
//!
//! 顺序：完整 / 裸 ID 轻归一化全等 → 保留身份的拼写标准化与已知日期、effort →
//! 受限渠道别名 → 同一安全身份内的有序 Dice。每级碰撞立即 miss，不降级任取首项。
//! 版本、尺寸、顺序、重复 token 和 thinking/turbo/flash 等语义均保留；未知差异不猜。
//! L3 只桥接已知日期的不同拼写，不用相似度推断模型身份。分数不是概率，规则也不保证
//! 上游目录 / 渠道声明正确；无法证明唯一时宁可缺省，由调用方使用父级默认值。

use std::collections::{BTreeMap, BTreeSet};

const MIN_SIMILARITY: f32 = 0.90;
const MIN_MARGIN: f32 = 0.10;
const MAX_SIMILARITY_BYTES: usize = 256;

// 只接受已知快照到基名的映射；不把任意八位数字、日期或版本当成可删除后缀。
const DATED_MODELS: &[(&str, &str)] = &[
    ("claude-sonnet-4-5", "20250929"),
    ("gpt-5", "20250807"),
    ("gpt-5-mini", "20250807"),
    ("gpt-5-nano", "20250807"),
];

// 还须候选目录条目声明支持该 effort。max / fast / thinking 等保留为模型身份。
const EFFORT_SUFFIXES: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "none"];

// (渠道拼写, 目标厂商, 模型族)。不是可对任意模型剥离的通用前缀表。
const PREFIX_ALIASES: &[(&str, &str, &str)] = &[
    ("zhipuai", "zhipuai", "glm-"),
    ("z-ai", "zhipuai", "glm-"),
    ("zai", "zhipuai", "glm-"),
    ("zai-org", "zhipuai", "glm-"),
    ("databricks", "zhipuai", "glm-"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    Exact,
    Variant,
    Alias,
    Nearest,
}

impl MatchKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Variant => "variant",
            Self::Alias => "alias",
            Self::Nearest => "nearest",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Candidate {
    /// 原始 canonical ID 和上游 ID，不得预先使用同步模块的归一化函数。
    pub keys: Vec<String>,
    pub effort_values: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct Match {
    pub index: usize,
    pub kind: MatchKind,
    /// 全等类为 1；nearest 为有序字符 Dice（2 * LCS / 总长度），不是概率。
    pub score: f32,
}

type Hits = BTreeSet<usize>;
type KeyIndex = BTreeMap<String, Hits>;

struct IndexedCandidate {
    namespaces: BTreeSet<String>,
    keys: BTreeSet<String>,
    effort_values: BTreeSet<String>,
}

#[derive(Clone, Copy)]
enum Scope<'a> {
    Bare,
    Named(&'a str),
    Alias(&'a str),
}

/// 索引保留全部候选；同一条目的重复拼写不算碰撞，不同条目绝不合并。
pub struct NearestIndex {
    candidates: Vec<IndexedCandidate>,
    full_exact: KeyIndex,
    bare_exact: KeyIndex,
    safe_keys: KeyIndex,
}

impl NearestIndex {
    pub fn new(candidates: Vec<Candidate>) -> Self {
        let mut result = Self {
            candidates: Vec::new(),
            full_exact: BTreeMap::new(),
            bare_exact: BTreeMap::new(),
            safe_keys: BTreeMap::new(),
        };
        for (index, candidate) in candidates.into_iter().enumerate() {
            let mut indexed = IndexedCandidate {
                namespaces: BTreeSet::new(),
                keys: BTreeSet::new(),
                effort_values: candidate.effort_values.into_iter().collect(),
            };
            for raw in candidate.keys {
                let full = light_key(&raw);
                let Some((namespace, bare)) = split_id(&full) else {
                    continue;
                };
                if let Some(namespace) = namespace {
                    indexed.namespaces.insert(namespace.to_string());
                }
                let safe = safe_key(bare);
                result
                    .bare_exact
                    .entry(bare.to_string())
                    .or_default()
                    .insert(index);
                result
                    .safe_keys
                    .entry(safe.clone())
                    .or_default()
                    .insert(index);
                indexed.keys.insert(safe);
                result.full_exact.entry(full).or_default().insert(index);
            }
            result.candidates.push(indexed);
        }
        result
    }

    pub fn resolve(&self, request: &str) -> Option<Match> {
        let full = light_key(request);
        let (namespace, bare) = split_id(&full)?;
        let scope = namespace.map_or(Scope::Bare, Scope::Named);

        // 完整限定 ID 比裸 ID 强；裸 ID 碰撞不能被某个条目的同名原始 ID 抢走。
        if namespace.is_some() {
            if let Some(hits) = self.full_exact.get(&full) {
                return unique(hits, MatchKind::Exact);
            }
        }
        let hits = self.lookup(&self.bare_exact, bare, scope, None);
        if !hits.is_empty() {
            return unique(&hits, MatchKind::Exact);
        }

        let safe = safe_key(bare);
        let hits = self.lookup(&self.safe_keys, &safe, scope, None);
        if !hits.is_empty() {
            return unique(&hits, MatchKind::Variant);
        }
        let hits = self.variant_hits(&safe, scope);
        if !hits.is_empty() {
            return unique(&hits, MatchKind::Variant);
        }

        let mut aliases = Hits::new();
        if let Some(namespace) = namespace {
            aliases.extend(self.alias_hits(&safe, Scope::Alias(namespace)));
        }
        for &(prefix, owner, family) in PREFIX_ALIASES {
            // 带厂商的请求不能借渠道前缀跨厂商；未知命名空间也不会被无条件丢弃。
            if namespace == Some(prefix) && safe.starts_with(family) {
                aliases.extend(self.alias_hits(&safe, Scope::Alias(owner)));
            }
            if namespace.is_none_or(|ns| namespace_owner(ns) == owner) {
                if let Some(rest) = safe.strip_prefix(prefix).and_then(|s| s.strip_prefix('-')) {
                    if rest.starts_with(family) {
                        aliases.extend(self.alias_hits(rest, Scope::Alias(owner)));
                    }
                }
            }
        }
        if !aliases.is_empty() {
            return unique(&aliases, MatchKind::Alias);
        }

        self.nearest(&safe, namespace.map_or(Scope::Bare, Scope::Alias))
    }

    fn accepts(&self, index: usize, scope: Scope<'_>, effort: Option<&str>) -> bool {
        let candidate = &self.candidates[index];
        if effort.is_some_and(|value| !candidate.effort_values.contains(value)) {
            return false;
        }
        match scope {
            Scope::Bare => true,
            Scope::Named(name) => candidate.namespaces.contains(name),
            Scope::Alias(name) => candidate
                .namespaces
                .iter()
                .any(|ns| namespace_owner(ns) == namespace_owner(name)),
        }
    }

    fn lookup(&self, keys: &KeyIndex, key: &str, scope: Scope<'_>, effort: Option<&str>) -> Hits {
        let mut hits: Hits = keys
            .get(key)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&index| self.accepts(index, scope, None))
            .collect();
        // 身份键已经碰撞时，不能借不同能力声明把歧义过滤成一个“赢家”。
        if hits.len() <= 1 {
            hits.retain(|&index| self.accepts(index, scope, effort));
        }
        hits
    }

    fn variant_hits(&self, key: &str, scope: Scope<'_>) -> Hits {
        let mut hits = Hits::new();
        if let Some(base) = strip_known_date(key) {
            hits.extend(self.lookup(&self.safe_keys, base, scope, None));
        }
        if let Some((base, effort)) = key.rsplit_once('-') {
            if EFFORT_SUFFIXES.contains(&effort) {
                hits.extend(self.lookup(&self.safe_keys, base, scope, Some(effort)));
                if let Some(base) = strip_known_date(base) {
                    hits.extend(self.lookup(&self.safe_keys, base, scope, Some(effort)));
                }
            }
        }
        hits
    }

    fn alias_hits(&self, key: &str, scope: Scope<'_>) -> Hits {
        let hits = self.lookup(&self.safe_keys, key, scope, None);
        if hits.is_empty() {
            self.variant_hits(key, scope)
        } else {
            hits
        }
    }

    // L3 不能创造身份等价关系：仅允许完整安全键或已知日期映射后的键完全相同。
    // 不剥候选 effort（其能力可能已经特化），也不忽略任何未知 token。
    fn similarity_ranking(&self, key: &str, scope: Scope<'_>) -> Option<(usize, f32, f32)> {
        if key.len() > MAX_SIMILARITY_BYTES {
            return None;
        }
        let identity = strip_known_date(key).unwrap_or(key);
        let mut best = None;
        let mut second = 0.0_f32;
        for (index, candidate) in self.candidates.iter().enumerate() {
            if !self.accepts(index, scope, None) {
                continue;
            }
            let score = candidate
                .keys
                .iter()
                .filter_map(|other| {
                    (other.len() <= MAX_SIMILARITY_BYTES
                        && strip_known_date(other).unwrap_or(other) == identity)
                        .then(|| ordered_dice(key, other))
                })
                .fold(0.0_f32, f32::max);
            if score == 0.0 {
                continue;
            }
            match best {
                None => best = Some((index, score)),
                Some((_, previous)) if score > previous => {
                    second = second.max(previous);
                    best = Some((index, score));
                }
                Some(_) => second = second.max(score),
            }
        }
        best.map(|(index, score)| (index, score, second))
    }

    fn nearest(&self, key: &str, scope: Scope<'_>) -> Option<Match> {
        let (index, score, second) = self.similarity_ranking(key, scope)?;
        if score < MIN_SIMILARITY || score - second < MIN_MARGIN {
            return None;
        }
        Some(Match {
            index,
            kind: MatchKind::Nearest,
            score,
        })
    }
}

fn unique(hits: &Hits, kind: MatchKind) -> Option<Match> {
    if hits.len() != 1 {
        return None;
    }
    Some(Match {
        index: *hits.first()?,
        kind,
        score: 1.0,
    })
}

fn light_key(raw: &str) -> String {
    raw.trim().to_lowercase()
}

fn split_id(full: &str) -> Option<(Option<&str>, &str)> {
    if full.is_empty() || full.split('/').any(str::is_empty) {
        return None;
    }
    Some(match full.rsplit_once('/') {
        Some((namespace, bare)) => (Some(namespace), bare),
        None => (None, full),
    })
}

// 仅分隔符和字母-数字边界等价；不排序、不去重、不删尺寸或语义词。
// ':' 被保留为分隔符（llama3.3:70b），'@' 原样保留（不截断 @cf 路径）。
fn safe_key(raw: &str) -> String {
    let mut out = String::new();
    let mut previous = None;
    for c in raw.chars() {
        let c = if matches!(c, '.' | '_' | ':') { '-' } else { c };
        if c.is_ascii_digit() && previous.is_some_and(|p: char| p.is_ascii_alphabetic()) {
            out.push('-');
        }
        out.push(c);
        previous = Some(c);
    }
    out
}

fn namespace_owner(namespace: &str) -> &str {
    // Workers 的 @cf 是路径前缀，不是 @variant 后缀；只解一层可信包装。
    let namespace = namespace.strip_prefix("@cf/").unwrap_or(namespace);
    match namespace {
        "z-ai" | "zai" | "zai-org" => "zhipuai",
        "meta-llama" | "llama" => "meta",
        "deepseek-ai" => "deepseek",
        other => other,
    }
}

fn strip_known_date(key: &str) -> Option<&str> {
    DATED_MODELS.iter().find_map(|&(base, date)| {
        let suffix = key.strip_prefix(base)?.strip_prefix('-')?;
        let dashed = format!("{}-{}-{}", &date[..4], &date[4..6], &date[6..]);
        (suffix == date || suffix == dashed).then_some(base)
    })
}

// 有序字符 Dice：LCS 保留顺序和重复次数；身份守卫在评分之前独立执行。
fn ordered_dice(a: &str, b: &str) -> f32 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut row = vec![0; b.len() + 1];
    for left in &a {
        let mut diagonal = 0;
        for (j, right) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = if left == right {
                diagonal + 1
            } else {
                above.max(row[j])
            };
            diagonal = above;
        }
    }
    2.0 * row[b.len()] as f32 / (a.len() + b.len()) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_of(ids: &[&str]) -> NearestIndex {
        NearestIndex::new(
            ids.iter()
                .map(|id| Candidate {
                    keys: vec![(*id).to_string()],
                    effort_values: vec!["low".into(), "high".into(), "xhigh".into()],
                })
                .collect(),
        )
    }

    fn hit(index: &NearestIndex, request: &str) -> Option<(usize, MatchKind)> {
        index.resolve(request).map(|m| (m.index, m.kind))
    }

    #[test]
    fn strong_exact_keeps_semantic_variants_and_wins_before_reductions() {
        let ids = [
            "acme/rocket",
            "acme/rocket-thinking",
            "acme/rocket-turbo",
            "acme/rocket-high",
            "acme/zai-org-glm-5-2",
            "zhipuai/glm-5-2",
            "anthropic/claude-sonnet-4-5",
            "anthropic/claude-sonnet-4-5-20250929",
            "openai/gpt-5-codex",
            "openai/gpt-5-codex-high",
        ];
        for ordered in [ids.to_vec(), ids.iter().rev().copied().collect()] {
            let index = index_of(&ordered);
            for (i, id) in ordered.iter().enumerate() {
                for request in [
                    id.to_string(),
                    id.rsplit('/').next().unwrap().to_uppercase(),
                ] {
                    assert_eq!(
                        hit(&index, &request),
                        Some((i, MatchKind::Exact)),
                        "{request}"
                    );
                }
            }
        }
    }

    #[test]
    fn bare_collisions_refuse_and_qualified_ids_disambiguate() {
        for ids in [
            ["acme/rocket-1", "other/rocket-1"],
            ["other/rocket-1", "acme/rocket-1"],
        ] {
            let index = index_of(&ids);
            assert!(index.resolve("rocket-1").is_none());
            assert!(index.resolve("rocket_1").is_none());
            assert!(index.resolve("unknown/rocket-1").is_none());
            for (i, id) in ids.iter().enumerate() {
                assert_eq!(hit(&index, id), Some((i, MatchKind::Exact)));
            }
        }
        assert!(index_of(&["acme/rocket", "acme/rocket"])
            .resolve("acme/rocket")
            .is_none());
        let duplicate_alias = NearestIndex::new(vec![Candidate {
            keys: vec!["acme/rocket".into(), "rocket".into(), "rocket".into()],
            effort_values: vec![],
        }]);
        assert!(duplicate_alias.resolve("rocket").is_some());
        let different_efforts = NearestIndex::new(vec![
            Candidate {
                keys: vec!["acme/rocket".into()],
                effort_values: vec!["high".into()],
            },
            Candidate {
                keys: vec!["other/rocket".into()],
                effort_values: vec![],
            },
        ]);
        assert!(different_efforts.resolve("rocket-high").is_none());
        assert!(different_efforts.resolve("acme/rocket-high").is_some());
    }

    #[test]
    fn strong_spelling_precedes_safe_key_collisions() {
        let index = index_of(&["zhipuai/glm-5.2", "zhipuai/glm-5-2"]);
        assert_eq!(hit(&index, "glm-5.2"), Some((0, MatchKind::Exact)));
        assert_eq!(hit(&index, "glm-5-2"), Some((1, MatchKind::Exact)));
        assert!(index.resolve("GLM_5_2").is_none());
        assert!(index.resolve("zai-org-glm-5-2").is_none());
    }

    #[test]
    fn safe_spelling_preserves_ollama_size_and_cloudflare_path() {
        let index = index_of(&[
            "meta/llama-3.3-70b",
            "meta/llama-3.3-8b",
            "deepseek/deepseek-r1-distill-llama-70b",
            "@cf/acme/rocket-70b",
        ]);
        for (request, expected, kind) in [
            ("llama3.3:70b", 0, MatchKind::Variant),
            ("meta-llama/llama3.3:70b", 0, MatchKind::Alias),
            (
                "@cf/deepseek-ai/deepseek-r1-distill-llama-70b",
                2,
                MatchKind::Alias,
            ),
            ("@cf/acme/rocket-70b", 3, MatchKind::Exact),
        ] {
            assert_eq!(hit(&index, request), Some((expected, kind)), "{request}");
        }
        for request in [
            "llama3.3",
            "llama3.3:7b",
            "llama3.3@70b",
            "@cf/other/deepseek-r1-distill-llama-70b",
        ] {
            assert!(index.resolve(request).is_none(), "{request}");
        }
    }

    #[test]
    fn known_dates_and_declared_effort_are_variants() {
        let index = index_of(&[
            "anthropic/claude-sonnet-4-5",
            "openai/gpt-5",
            "openai/gpt-5-codex",
        ]);
        for (request, expected) in [
            ("claude-sonnet-4-5-20250929", 0),
            ("claude-sonnet-4-5-2025-09-29", 0),
            ("gpt-5-2025-08-07", 1),
            ("openai/gpt-5-20250807-high", 1),
            ("gpt-5-codex-high", 2),
        ] {
            assert_eq!(
                hit(&index, request),
                Some((expected, MatchKind::Variant)),
                "{request}"
            );
        }
        for request in [
            "gpt-5-high-high",
            "gpt-5-20259999",
            "gpt-5-2025-08-08",
            "gpt-5-max",
            "gpt-5-fast",
            "gpt-5-nothinking",
            "gpt-5:free",
        ] {
            assert!(index.resolve(request).is_none(), "{request}");
        }
        let no_effort = NearestIndex::new(vec![Candidate {
            keys: vec!["openai/gpt-5-codex".into()],
            effort_values: vec![],
        }]);
        assert!(no_effort.resolve("gpt-5-codex-high").is_none());
    }

    #[test]
    fn aliases_are_scoped_to_the_declared_vendor_and_family() {
        let index = index_of(&["zhipuai/glm-5.2", "openai/gpt-5"]);
        for request in [
            "zai-org-glm-5-2",
            "zai-glm-5-2",
            "zhipuai-glm-5-2",
            "databricks-glm-5-2",
            "databricks/glm-5.2",
            "z-ai/glm-5.2",
        ] {
            assert_eq!(
                hit(&index, request),
                Some((0, MatchKind::Alias)),
                "{request}"
            );
        }
        for request in [
            "openai/glm-5.2",
            "other/glm-5.2",
            "databricks-gpt-5",
            "openai/zai-org-glm-5-2",
            "unknown-glm-5-2",
            "anthropic/gpt-5-high",
        ] {
            assert!(index.resolve(request).is_none(), "{request}");
        }
        assert!(index_of(&["other/glm-5.2"])
            .resolve("zai-org-glm-5-2")
            .is_none());
    }

    #[test]
    fn identity_differences_are_rejected_even_for_long_names() {
        for (request, candidate) in [
            ("deepseek-v4-flash", "deepseek/deepseek-v4"),
            ("gpt-5-codex", "openai/gpt-5"),
            ("o3-mini", "openai/o3"),
            ("qwen-3-5-coder", "alibaba/qwen-3-5"),
            (
                "acme-long-model-series-release-3-3-70b",
                "acme/acme-long-model-series-release-3-70b",
            ),
            (
                "acme-long-model-series-release-3-4-70b",
                "acme/acme-long-model-series-release-4-3-70b",
            ),
            (
                "acme-long-model-series-release-3-3-8b",
                "acme/acme-long-model-series-release-3-3-70b",
            ),
            (
                "acme-long-model-series-release-3-3-70b",
                "acme/acme-long-model-series-release-3-4-70b",
            ),
            (
                "other-long-model-series-release-3-3-70b",
                "acme/acme-long-model-series-release-3-3-70b",
            ),
        ] {
            let index = index_of(&[candidate]);
            assert!(index.resolve(request).is_none(), "{request} -> {candidate}");
            assert!(index
                .similarity_ranking(&safe_key(request), Scope::Bare)
                .is_none());
        }
        let base = "acme-super-long-model-family-series-release-3-3-70b";
        for suffix in [
            "flash", "mini", "codex", "coder", "pro", "max", "fast", "turbo", "thinking",
            "instruct", "preview", "chat", "exp", "fp8", "extra",
        ] {
            let sibling = format!("{base}-{suffix}");
            assert!(
                ordered_dice(base, &sibling) >= MIN_SIMILARITY,
                "fixture must exceed the score floor"
            );
            for (candidate, request) in [(base, sibling.as_str()), (sibling.as_str(), base)] {
                let index = index_of(&[candidate]);
                assert!(index.resolve(request).is_none(), "{request} -> {candidate}");
                assert!(index.similarity_ranking(request, Scope::Bare).is_none());
            }
        }
    }

    #[test]
    fn nearest_only_bridges_known_date_spellings_and_enforces_score_floor() {
        let index = index_of(&["anthropic/claude-sonnet-4-5-20250929"]);
        let request = "claude-sonnet-4-5-2025-09-29";
        let matched = index.resolve(request).unwrap();
        assert_eq!(matched.kind, MatchKind::Nearest);
        assert!((matched.score - 52.0 / 54.0).abs() < 0.0001);

        // 身份相同但信息量不足：确实参与排名，再由分数门槛拒绝，不是守卫的假覆盖。
        let base = "claude-sonnet-4-5";
        let (_, best, second) = index.similarity_ranking(base, Scope::Bare).unwrap();
        assert!(best < MIN_SIMILARITY && best - second >= MIN_MARGIN);
        assert!(index.resolve(base).is_none());
        assert!(index.resolve("claude-sonnet-4-5-20250930").is_none());
    }

    #[test]
    fn nearest_margin_rejects_real_above_threshold_ties() {
        let ids = [
            "anthropic/claude-sonnet-4-5-20250929",
            "other/claude-sonnet-4-5-20250929",
        ];
        let request = "claude-sonnet-4-5-2025-09-29";
        for ordered in [ids.to_vec(), ids.iter().rev().copied().collect()] {
            let index = index_of(&ordered);
            let (_, best, second) = index.similarity_ranking(request, Scope::Bare).unwrap();
            assert!(best >= MIN_SIMILARITY && second >= MIN_SIMILARITY);
            assert!(best - second < MIN_MARGIN);
            assert!(index.resolve(request).is_none());
            assert!(index_of(&[ordered[0]]).resolve(request).is_some());
            let qualified = format!("anthropic/{request}");
            assert!(index.resolve(&qualified).is_some());
        }

        // 非零小分差也要拒绝，不能把 margin 守卫退化成“只拒绝完全并列”。
        let close = index_of(&[
            "anthropic/claude-sonnet-4-5-20250929",
            "other/claude-sonnet-4-5-2025-09-29",
        ]);
        let (_, best, second) = close.similarity_ranking(request, Scope::Bare).unwrap();
        assert!(second >= MIN_SIMILARITY && best > second && best - second < MIN_MARGIN);
        assert!(close.nearest(request, Scope::Bare).is_none());

        let separated = index_of(&[
            "anthropic/claude-sonnet-4-5-20250929",
            "other/claude-sonnet-4-5",
        ]);
        let (_, best, second) = separated.similarity_ranking(request, Scope::Bare).unwrap();
        assert!(best >= MIN_SIMILARITY && second > 0.0 && best - second >= MIN_MARGIN);
        assert_eq!(separated.nearest(request, Scope::Bare).unwrap().index, 0);
    }

    #[test]
    fn ordered_dice_counts_order_and_repetition() {
        assert_eq!(ordered_dice("abc", "abc"), 1.0);
        assert!(ordered_dice("abc", "cba") < 1.0);
        assert!(ordered_dice("3-3", "3") < 1.0);
        assert_eq!(ordered_dice("", "a"), 0.0);
    }

    #[test]
    fn malformed_requests_never_match() {
        let index = index_of(&["zhipuai/glm-5.2"]);
        for request in ["", "   ", "/glm-5.2", "zhipuai/", "zhipuai//glm-5.2"] {
            assert!(index.resolve(request).is_none());
        }
    }
}
