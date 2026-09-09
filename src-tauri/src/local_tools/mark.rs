//! OpenHub 写入第三方工具配置时的个性化标识。
//!
//! 约定：只合并本软件管辖的供应商条目，用户原有第三方供应商一律保留。
//! 标识用于区分「本软件写入」和「用户/第三方原文」，备份还原时也靠它判断。

pub const MANAGED_PREFIX: &str = "openhub-";
pub const MANAGER_VALUE: &str = "OpenHub";
pub const JSON_MARK_KEY: &str = "x-openhub";
pub const TOML_MARK_KEY: &str = "x_openhub";
pub const YAML_MARK_KEY: &str = "x-openhub";
pub const CLAUDE_ENV_MARK: &str = "OPENHUB_MANAGED";
pub const COMMENT: &str = "managed-by: OpenHub";

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

pub fn provider_id(channel_id: &str, account: &str, key_index: usize) -> String {
    let channel = sanitize_id_part(channel_id);
    let account = sanitize_id_part(account);
    format!("{MANAGED_PREFIX}{channel}_{account}_{key_index}")
}

#[cfg(test)]
mod tests {
    use super::*;

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
