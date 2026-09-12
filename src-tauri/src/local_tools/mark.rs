//! OpenHub 写入第三方工具配置时的个性化标识。
//!
//! 约定：只合并本软件管辖的供应商条目，用户原有第三方供应商一律保留。
//! 标识用于区分「本软件写入」和「用户/第三方原文」，备份还原时也靠它判断。

pub const MANAGED_PREFIX: &str = "openhub-";
pub const MANAGER_VALUE: &str = "OpenHub";
pub const JSON_MARK_KEY: &str = "x-openhub";
pub const TOML_MARK_KEY: &str = "x_openhub";
pub const TOML_FINGERPRINT_KEY: &str = "x_openhub_fingerprint";
pub const YAML_MARK_KEY: &str = "x-openhub";
pub const YAML_FINGERPRINT_KEY: &str = "x-openhub-fingerprint";
pub const CLAUDE_ENV_MARK: &str = "OPENHUB_MANAGED";
pub const COMMENT: &str = "managed-by: OpenHub";

/// 内容指纹值前缀。指纹随标识一起写进配置文件（值形如 `openhub-fp-<16 位 hex>`），
/// 统一前缀让校验对 JSON / TOML / YAML 一视同仁：按文本扫描即可，不必逐格式解析。
pub const FINGERPRINT_PREFIX: &str = "openhub-fp-";
/// 指纹长度（FNV-1a 64 位 hex）。
pub const FINGERPRINT_LEN: usize = 16;
/// 适配器写入时放的占位值，`stamp` 落盘前把它换成真实指纹。
pub const FINGERPRINT_PLACEHOLDER: &str = "openhub-fp-0000000000000000";

/// 配置文件相对本软件上次写入的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ManagedState {
    /// 文件不存在，或没有本软件的标识：不是我们写的（或是旧版本写的）。
    #[default]
    Unmanaged,
    /// 标识与指纹俱在且指纹吻合：自上次写入后没人动过，可直接覆盖。
    Intact,
    /// 有标识但指纹对不上：我们写入之后被用户或其他软件改过，覆盖前必须备份。
    Modified,
}

/// 扫描文本里全部指纹值的位置：返回每个 hex 段的起始偏移。
fn fingerprint_offsets(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(pos) = text[from..].find(FINGERPRINT_PREFIX) {
        let start = from + pos + FINGERPRINT_PREFIX.len();
        let end = start + FINGERPRINT_LEN;
        if text.len() >= end && text[start..end].bytes().all(|b| b.is_ascii_hexdigit()) {
            out.push(start);
            from = end;
        } else {
            from = start;
        }
    }
    out
}

/// 把全部指纹值归零（占位形态），指纹就是对这份归零文本求的哈希。
fn normalized(text: &str, offsets: &[usize]) -> String {
    let mut out = text.to_string();
    for &start in offsets {
        out.replace_range(start..start + FINGERPRINT_LEN, &"0".repeat(FINGERPRINT_LEN));
    }
    out
}

/// 落盘前盖章：把文本里的指纹占位换成对整份内容求出的指纹。
/// 文本里没有占位/指纹时原样返回（非受管文件）。
pub fn stamp(text: &str) -> String {
    let offsets = fingerprint_offsets(text);
    if offsets.is_empty() {
        return text.to_string();
    }
    let base = normalized(text, &offsets);
    let fp = super::adapters::content_hash(&[base.clone()]);
    let mut out = base;
    for &start in &offsets {
        out.replace_range(start..start + FINGERPRINT_LEN, &fp);
    }
    out
}

/// 校验文件内容相对本软件上次写入的状态。
pub fn inspect(text: &str) -> ManagedState {
    let offsets = fingerprint_offsets(text);
    if offsets.is_empty() {
        return ManagedState::Unmanaged;
    }
    let expected = super::adapters::content_hash(&[normalized(text, &offsets)]);
    let intact = offsets
        .iter()
        .all(|&start| &text[start..start + FINGERPRINT_LEN] == expected);
    if intact {
        ManagedState::Intact
    } else {
        ManagedState::Modified
    }
}

pub fn is_managed_id(id: &str) -> bool {
    id.starts_with(MANAGED_PREFIX)
}

pub fn sanitize_id_part(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "item".into()
    } else {
        trimmed.chars().take(48).collect()
    }
}

#[cfg(test)]
pub fn provider_id(channel_id: &str, account: &str, key_index: usize) -> String {
    let channel = sanitize_id_part(channel_id);
    let account = sanitize_id_part(account);
    format!("{MANAGED_PREFIX}{channel}_{account}_{key_index}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_then_inspect_detects_foreign_edits() {
        let text = format!(
            "{{\n  \"x-openhub\": {{\"fingerprint\": \"{FINGERPRINT_PLACEHOLDER}\"}},\n  \"model\": \"a\"\n}}\n"
        );
        let stamped = stamp(&text);
        assert!(!stamped.contains(FINGERPRINT_PLACEHOLDER), "占位应被替换");
        assert_eq!(inspect(&stamped), ManagedState::Intact);
        // 幂等：对已盖章文本再盖一次结果不变
        assert_eq!(stamp(&stamped), stamped);

        // 别人改了内容（指纹仍在）→ Modified
        let edited = stamped.replace("\"model\": \"a\"", "\"model\": \"b\"");
        assert_eq!(inspect(&edited), ManagedState::Modified);
        // 没有标识 → Unmanaged
        assert_eq!(inspect("{\"model\": \"a\"}"), ManagedState::Unmanaged);
        assert_eq!(stamp("plain"), "plain");
    }

    #[test]
    fn multiple_occurrences_share_one_fingerprint() {
        let text = format!(
            "[a]\nx_openhub_fingerprint = \"{FINGERPRINT_PLACEHOLDER}\"\n[b]\nx_openhub_fingerprint = \"{FINGERPRINT_PLACEHOLDER}\"\n"
        );
        let stamped = stamp(&text);
        let values: Vec<&str> = stamped
            .match_indices(FINGERPRINT_PREFIX)
            .map(|(i, _)| {
                &stamped
                    [i + FINGERPRINT_PREFIX.len()..i + FINGERPRINT_PREFIX.len() + FINGERPRINT_LEN]
            })
            .collect();
        assert_eq!(values.len(), 2);
        assert_eq!(values[0], values[1]);
        assert_eq!(inspect(&stamped), ManagedState::Intact);
        // 删掉一个供应商段落 → 内容变了 → Modified
        let cut = stamped.split("[b]").next().unwrap().to_string();
        assert_eq!(inspect(&cut), ManagedState::Modified);
    }

    #[test]
    fn managed_ids_are_stable_and_safe() {
        assert!(is_managed_id("openhub-site_a_acc_0"));
        assert!(!is_managed_id("custom"));
        assert_eq!(sanitize_id_part("Foo Bar/站点"), "foo_bar");
        assert_eq!(
            provider_id("site_abc", "acc 1", 0),
            "openhub-site_abc_acc_1_0"
        );
    }
}
