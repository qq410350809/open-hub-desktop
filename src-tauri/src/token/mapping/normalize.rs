// 原始模型名归一：为映射表提供稳定主键，并给出规则层的候选基名。
//
// 现有前端实现（tokenFormatters.normalizeModelName + tokenBreakdownAgg.stripSuffix）
// 有四处缺陷，这里逐一修掉：
//   1. 大小写敏感：GLM-5.3-Flash 与 glm-5.3-flash 被算作两个模型；
//   2. 只切第一个 '/'：alpha/stealth/ox-alpha 会残留 stealth/ox-alpha；
//   3. 版本分隔符不统一：glm-5-2 / glm-5.2 / zai-glm-5-2 互不合并；
//   4. `-\d{4}$` 无条件削尾：把 glm-4.5-1210 这类日期后缀之外的四位数字也削掉。

/// 映射表主键：小写、去空白、去掉全部厂商前缀路径。
pub fn raw_key(name: &str) -> String {
    let trimmed = name.trim();
    let tail = trimmed.rsplit('/').next().unwrap_or(trimmed);
    tail.trim().to_lowercase()
}

/// 把版本号里的 `-` 分隔符统一成 `.`：5-3 → 5.3、claude-sonnet-4-6 → claude-sonnet-4.6。
/// 仅在「数字-数字」之间替换，避免动到 gpt-4 或 qwen3-32b 这类正常片段。
pub fn unify_version_separators(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len());
    let mut idx = 0usize;
    while idx < chars.len() {
        let ch = chars[idx];
        if ch == '-'
            && idx > 0
            && idx + 1 < chars.len()
            && chars[idx - 1].is_ascii_digit()
            && chars[idx + 1].is_ascii_digit()
        {
            // 形如 4-20250514 的日期段不是版本号，保留原样交给后缀削减处理。
            let digits: usize = chars[idx + 1..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .count();
            // 数字段紧跟字母的是参数规模而非版本号（qwen3-32b、qwen-2.5-72b），
            // 必须保留 '-'，否则 32b 会被并进版本号变成 qwen3.32b。
            let followed_by_letter = chars
                .get(idx + 1 + digits)
                .is_some_and(|c| c.is_ascii_alphabetic());
            if digits >= 4 || followed_by_letter {
                out.push(ch);
            } else {
                out.push('.');
            }
        } else {
            out.push(ch);
        }
        idx += 1;
    }
    out
}

/// 判断一段纯数字是否像日期/快照戳（20250514、250514、0806），用于安全削尾。
fn looks_like_date_stamp(seg: &str) -> bool {
    if !seg.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    match seg.len() {
        8 => seg.starts_with("20") || seg.starts_with("19"),
        6 => true,
        4 => {
            // 仅当能读成 月日（MMDD）时才当快照戳，避免误削 glm-4.5-1210 之外的版本尾数。
            let month: u32 = seg[0..2].parse().unwrap_or(0);
            let day: u32 = seg[2..4].parse().unwrap_or(0);
            (1..=12).contains(&month) && (1..=31).contains(&day)
        }
        _ => false,
    }
}

/// 规则层候选基名：归一大小写与版本分隔符后，削掉部署变体与日期快照后缀。
/// 返回值始终保留至少一个 '-' 分段，避免把 gpt-4o-mini 削成 gpt。
pub fn rule_base_name(name: &str) -> String {
    let mut base = unify_version_separators(&raw_key(name));
    loop {
        let before = base.clone();
        for variant in [
            "-latest",
            "-preview",
            "-thinking",
            "-nothinking",
            "-flash",
            "-small",
            "-large",
            "-code",
            "-free",
            // 单列 -contributor：外层 loop 会反复收敛，-contributor-free
            // 这类组合后缀由 -free 与 -contributor 依次削去，无需穷举组合。
            "-contributor",
            "-ga",
        ] {
            if let Some(stripped) = base.strip_suffix(variant) {
                if stripped.contains('-') || stripped.contains('.') {
                    base = stripped.to_string();
                }
            }
        }
        if let Some((head, tail)) = base.rsplit_once('-') {
            if looks_like_date_stamp(tail) && (head.contains('-') || head.contains('.')) {
                base = head.to_string();
            }
        }
        if base == before {
            break;
        }
    }
    if base.is_empty() {
        raw_key(name)
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_key_is_case_insensitive_and_strips_all_prefixes() {
        assert_eq!(raw_key("GLM-5.3-Flash"), "glm-5.3-flash");
        assert_eq!(raw_key(" glm-5.3-flash "), "glm-5.3-flash");
        assert_eq!(raw_key("alpha/stealth/ox-alpha"), "ox-alpha");
        assert_eq!(raw_key("openai/gpt-5.6"), "gpt-5.6");
    }

    #[test]
    fn version_separators_unify_dash_forms() {
        assert_eq!(unify_version_separators("glm-5-2"), "glm-5.2");
        assert_eq!(
            unify_version_separators("claude-sonnet-4-6"),
            "claude-sonnet-4.6"
        );
        assert_eq!(unify_version_separators("gpt-4"), "gpt-4");
        assert_eq!(unify_version_separators("qwen3-32b"), "qwen3-32b");
    }

    #[test]
    fn date_suffixes_are_not_treated_as_versions() {
        assert_eq!(
            unify_version_separators("claude-sonnet-4-20250514"),
            "claude-sonnet-4-20250514"
        );
        assert_eq!(
            rule_base_name("claude-sonnet-4-20250514"),
            "claude-sonnet-4"
        );
    }

    #[test]
    fn dash_and_dot_forms_collapse_to_one_base() {
        assert_eq!(rule_base_name("glm-5-2"), rule_base_name("GLM-5.2"));
        assert_eq!(rule_base_name("zai-glm-5-2"), "zai-glm-5.2");
    }

    #[test]
    fn variant_suffixes_are_stripped_without_eating_the_family() {
        assert_eq!(rule_base_name("glm-5.3-flash"), "glm-5.3");
        assert_eq!(rule_base_name("gpt-5.6-preview"), "gpt-5.6");
        assert_eq!(
            rule_base_name("muse-spark-1.2-contributor-free"),
            "muse-spark-1.2"
        );
    }

    #[test]
    fn base_never_collapses_to_a_bare_word() {
        assert_eq!(rule_base_name("free"), "free");
        assert_eq!(rule_base_name("big-pickle"), "big-pickle");
    }
}
