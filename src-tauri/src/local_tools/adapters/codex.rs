//! Codex CLI：`~/.codex/config.toml` + `auth.json`。
//!
//! 管辖字段（其余表如 mcp_servers / projects / plugins 一律不动）：
//! - 供应商：`model_providers.*`（name/base_url/wire_api）+ 顶层 `model_provider` 切换
//! - 模型：顶层 `model`
//! - 上下文：`model_context_window` / `model_auto_compact_token_limit`
//! - 思考：`model_reasoning_effort`
//!
//! 使用 toml_edit 保留既有注释与排版。API Key 写 `auth.json` 的
//! `OPENAI_API_KEY`（Codex 官方约定），并在供应商上标注
//! `requires_openai_auth`（编辑时保留用户原值）。

use std::path::{Path, PathBuf};

use super::{content_hash, effort_options, snapshot_skeleton, ToolAdapter};
use crate::local_tools::fsutil::{atomic_write_stamped, read_text};
use crate::local_tools::mark::{
    COMMENT, FINGERPRINT_PLACEHOLDER, JSON_MARK_KEY, MANAGED_PREFIX, TOML_FINGERPRINT_KEY,
    TOML_MARK_KEY,
};
use crate::local_tools::types::{
    ContextSection, DefaultsSection, ModelEntry, ProviderEntry, ThinkingSection, ToolConfigPatch,
    ToolConfigSnapshot, ToolId,
};

pub(crate) struct CodexAdapter;

const EFFORTS: &[&str] = &["minimal", "low", "medium", "high", "max", "xhigh", "ultra"];

fn codex_home(home: &Path) -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"))
}

fn config_path(home: &Path) -> PathBuf {
    codex_home(home).join("config.toml")
}

fn auth_path(home: &Path) -> PathBuf {
    codex_home(home).join("auth.json")
}

impl ToolAdapter for CodexAdapter {
    fn id(&self) -> ToolId {
        ToolId::Codex
    }

    fn config_files(&self, home: &Path) -> Vec<(String, String, PathBuf)> {
        vec![
            ("config".into(), "config.toml".into(), config_path(home)),
            ("auth".into(), "auth.json".into(), auth_path(home)),
        ]
    }

    fn detect(&self, home: &Path) -> (bool, String) {
        let root = codex_home(home);
        (root.is_dir(), root.display().to_string())
    }

    fn effect_note(&self) -> &'static str {
        "重启 Codex CLI 会话后生效"
    }

    fn snapshot(&self, home: &Path) -> Result<ToolConfigSnapshot, String> {
        let path = config_path(home);
        let mut snap = snapshot_skeleton(self, home);
        let Some(text) = read_text(&path)? else {
            snap.warning = "尚未生成 config.toml（首次运行 Codex 后会创建）".into();
            return Ok(snap);
        };
        let doc = text
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| format!("config.toml 解析失败：{error}"))?;

        // 供应商表。
        if let Some(providers) = doc.get("model_providers").and_then(|v| v.as_table_like()) {
            for (id, item) in providers.iter() {
                let table = match item.as_table_like() {
                    Some(t) => t,
                    None => continue,
                };
                let get = |key: &str| {
                    table
                        .get(key)
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                snap.providers.push(ProviderEntry {
                    id: id.to_string(),
                    name: get("name"),
                    base_url: get("base_url"),
                    api_key: String::new(),
                    protocol: get("wire_api"),
                    models: Vec::new(),
                });
            }
        }

        let get_str = |key: &str| {
            doc.get(key)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let get_i64 = |key: &str| doc.get(key).and_then(|v| v.as_integer()).map(|v| v as u64);

        let active_provider = get_str("model_provider");
        let active_model = get_str("model");
        for provider in &mut snap.providers {
            provider.models = vec![active_model.clone()];
        }

        snap.models = if active_model.is_empty() {
            Vec::new()
        } else {
            vec![ModelEntry {
                id: active_model.clone(),
                name: active_model.clone(),
                provider: active_provider.clone(),
                context_window: get_i64("model_context_window").unwrap_or(0),
                max_output: 0,
            }]
        };

        snap.defaults = DefaultsSection {
            model: active_model,
            provider: active_provider,
            reasoning_effort: get_str("model_reasoning_effort"),
            reasoning_effort_options: effort_options(EFFORTS),
            per_model_effort: Default::default(),
        };
        snap.context = ContextSection {
            context_window: get_i64("model_context_window"),
            auto_compact_token_limit: get_i64("model_auto_compact_token_limit"),
            max_output_tokens: None,
            max_thinking_tokens: None,
        };
        snap.thinking = ThinkingSection {
            effort_level: get_str("model_reasoning_effort"),
            effort_level_options: effort_options(EFFORTS),
            max_thinking_tokens: None,
        };
        snap.content_hash = content_hash(&[text, read_text(&auth_path(home))?.unwrap_or_default()]);
        Ok(snap)
    }

    fn apply(&self, home: &Path, patch: &ToolConfigPatch) -> Result<Vec<String>, String> {
        let path = config_path(home);
        let text = read_text(&path)?.unwrap_or_default();
        let mut doc = text
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| format!("config.toml 解析失败：{error}"))?;

        // 只 upsert OpenHub 管辖的供应商，用户原有 model_providers 一律保留。
        // 若某条 OpenHub 供应商从 patch 消失，则只删除 openhub- 前缀条目。
        let existing_ids: Vec<String> = doc
            .get("model_providers")
            .and_then(|v| v.as_table_like())
            .map(|t| t.iter().map(|(k, _)| k.to_string()).collect())
            .unwrap_or_default();
        for id in &existing_ids {
            if crate::local_tools::mark::is_managed_id(id)
                && !patch.providers.iter().any(|p| p.id == *id)
            {
                if let Some(providers) = doc
                    .get_mut("model_providers")
                    .and_then(|v| v.as_table_like_mut())
                {
                    providers.remove(id);
                }
            }
        }
        for provider in &patch.providers {
            if !crate::local_tools::mark::is_managed_id(&provider.id) {
                continue;
            }
            // toml_edit：不存在则创建子表（保留其余键不动）。
            if doc.get("model_providers").is_none() {
                doc.insert(
                    "model_providers",
                    toml_edit::Item::Table(toml_edit::Table::new()),
                );
            }
            let providers = doc
                .get_mut("model_providers")
                .and_then(|v| v.as_table_like_mut())
                .ok_or("model_providers 不是表，已中止写入")?;
            let mut table = providers
                .get(&provider.id)
                .and_then(|v| v.as_table_like())
                .map(|t| {
                    let mut copy = toml_edit::Table::new();
                    for (key, item) in t.iter() {
                        copy.insert(key, item.clone());
                    }
                    copy
                })
                .unwrap_or_default();
            set_str(&mut table, "name", &provider.name);
            set_str(&mut table, "base_url", &provider.base_url);
            if !provider.protocol.is_empty() {
                set_str(&mut table, "wire_api", &provider.protocol);
            }
            set_str(
                &mut table,
                TOML_MARK_KEY,
                crate::local_tools::mark::MANAGER_VALUE,
            );
            // 落盘时盖成真实指纹（各供应商表同值），用于判断此后有没有被外部改过
            set_str(&mut table, TOML_FINGERPRINT_KEY, FINGERPRINT_PLACEHOLDER);
            if table.decor().prefix().is_none() {
                table.decor_mut().set_prefix(format!("# {COMMENT}\n"));
            }
            providers.insert(&provider.id, toml_edit::Item::Table(table));
        }

        // 顶层标量。
        set_top_str(&mut doc, "model_provider", &patch.defaults.provider);
        set_top_str(&mut doc, "model", &patch.defaults.model);
        set_top_str(
            &mut doc,
            "model_reasoning_effort",
            &patch.defaults.reasoning_effort,
        );
        match patch.context.context_window {
            Some(value) => set_top_int(&mut doc, "model_context_window", value as i64),
            None => {
                doc.remove("model_context_window");
            }
        }
        match patch.context.auto_compact_token_limit {
            Some(value) => set_top_int(&mut doc, "model_auto_compact_token_limit", value as i64),
            None => {
                doc.remove("model_auto_compact_token_limit");
            }
        }

        let new_text = doc.to_string();
        atomic_write_stamped(&path, &new_text)?;

        // auth.json：仅当任一供应商带了 API Key 时写入（Codex 约定单 Key）。
        let mut written = vec!["config.toml".to_string()];
        if let Some(key) = patch
            .providers
            .iter()
            .filter(|p| {
                crate::local_tools::mark::is_managed_id(&p.id)
                    || p.id.starts_with(MANAGED_PREFIX)
                    || patch.defaults.provider == p.id
            })
            .map(|p| p.api_key.as_str())
            .find(|k| !k.is_empty())
        {
            let auth = auth_path(home);
            let existing = read_text(&auth)?
                .and_then(|text| {
                    serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&text).ok()
                })
                .unwrap_or_default();
            let mut existing = existing;
            existing.insert(
                "OPENAI_API_KEY".into(),
                serde_json::Value::String(key.to_string()),
            );
            existing.insert(
                JSON_MARK_KEY.into(),
                serde_json::json!({
                    "managed": true,
                    "manager": crate::local_tools::mark::MANAGER_VALUE,
                    "fingerprint": FINGERPRINT_PLACEHOLDER,
                }),
            );
            let out = serde_json::to_string_pretty(&existing).map_err(|e| e.to_string())? + "\n";
            atomic_write_stamped(&auth, &out)?;
            written.push("auth.json".to_string());
        }
        Ok(written)
    }
}

fn set_str(table: &mut toml_edit::Table, key: &str, value: &str) {
    if value.is_empty() {
        table.remove(key);
    } else if let Some(item) = table.get_mut(key) {
        // 原地替换 value：key 及其装饰（含键上注释）自动保留。
        if item.is_value() {
            *item = toml_edit::Item::Value(toml_edit::Value::from(value));
        } else {
            *item = toml_edit::value(value);
        }
    } else {
        table.insert(key, toml_edit::value(value));
    }
}

fn set_top_str(doc: &mut toml_edit::DocumentMut, key: &str, value: &str) {
    if value.is_empty() {
        doc.remove(key);
    } else if let Some(item) = doc.get_mut(key) {
        if item.is_value() {
            *item = toml_edit::Item::Value(toml_edit::Value::from(value));
        } else {
            *item = toml_edit::value(value);
        }
    } else {
        doc.insert(key, toml_edit::value(value));
    }
}

fn set_top_int(doc: &mut toml_edit::DocumentMut, key: &str, value: i64) {
    doc.insert(key, toml_edit::value(value));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "openhub-lt-codex-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".codex")).unwrap();
        dir
    }

    const SAMPLE: &str = r#"# 顶部注释必须保留
model = "gpt-5"
model_provider = "custom"
model_reasoning_effort = "high"

[model_providers.custom]
name = "custom"
base_url = "http://old:1/v1"
wire_api = "responses"

[mcp_servers.router]
command = "npx"

[projects."/tmp/x"]
trust_level = "trusted"
"#;

    #[test]
    fn apply_preserves_comments_and_other_tables() {
        let home = temp_home();
        std::fs::write(config_path(&home), SAMPLE).unwrap();

        let adapter = CodexAdapter;
        let snap = adapter.snapshot(&home).unwrap();
        assert_eq!(snap.providers.len(), 1);
        assert_eq!(snap.providers[0].base_url, "http://old:1/v1");
        assert_eq!(snap.defaults.reasoning_effort, "high");

        let mut providers = snap.providers.clone();
        providers[0].base_url = "http://127.0.0.1:17896/v1".into();
        providers.push(ProviderEntry {
            id: "openhub-gateway".into(),
            name: "OpenHub 网关".into(),
            base_url: "http://127.0.0.1:17896/v1".into(),
            api_key: String::new(),
            protocol: "responses".into(),
            models: vec![],
        });
        let patch = ToolConfigPatch {
            base_hash: snap.content_hash.clone(),
            providers,
            models: vec![],
            defaults: DefaultsSection {
                model: "gpt-5.6".into(),
                provider: "openhub-gateway".into(),
                reasoning_effort: "max".into(),
                reasoning_effort_options: vec![],
                per_model_effort: Default::default(),
            },
            context: ContextSection {
                context_window: Some(500_000),
                auto_compact_token_limit: Some(490_000),
                max_output_tokens: None,
                max_thinking_tokens: None,
            },
            thinking: ThinkingSection::default(),
        };
        adapter.apply(&home, &patch).unwrap();

        let text = std::fs::read_to_string(config_path(&home)).unwrap();
        assert!(text.contains("# 顶部注释必须保留"));
        assert!(text.contains("[mcp_servers.router]"));
        assert!(text.contains("trust_level"));
        assert!(text.contains("model_context_window = 500000"));
        assert!(text.contains("model_reasoning_effort = \"max\""));

        // 幂等：再快照再应用，字段稳定。
        let snap2 = adapter.snapshot(&home).unwrap();
        assert_eq!(snap2.providers.len(), 2);
        assert_eq!(snap2.defaults.provider, "openhub-gateway");
        assert_eq!(snap2.context.context_window, Some(500_000));
        let patch2 = ToolConfigPatch {
            base_hash: snap2.content_hash.clone(),
            providers: snap2.providers.clone(),
            models: vec![],
            defaults: snap2.defaults.clone(),
            context: snap2.context.clone(),
            thinking: snap2.thinking.clone(),
        };
        adapter.apply(&home, &patch2).unwrap();
        let snap3 = adapter.snapshot(&home).unwrap();
        assert_eq!(snap3.providers.len(), 2);

        // 删除 OpenHub 供应商：从 patch 移除 openhub- 条目后再应用。
        // 用户原有 custom 供应商必须保留。
        let patch3 = ToolConfigPatch {
            base_hash: snap3.content_hash.clone(),
            providers: vec![],
            models: vec![],
            defaults: DefaultsSection {
                model: "gpt-5".into(),
                provider: "custom".into(),
                reasoning_effort: "low".into(),
                reasoning_effort_options: vec![],
                per_model_effort: Default::default(),
            },
            context: ContextSection::default(),
            thinking: ThinkingSection::default(),
        };
        adapter.apply(&home, &patch3).unwrap();
        let text3 = std::fs::read_to_string(config_path(&home)).unwrap();
        assert!(
            !text3.contains("OpenHub 网关"),
            "删除的 OpenHub 供应商应被移除：{text3}"
        );
        assert!(
            text3.contains("[model_providers.custom]"),
            "用户原有供应商必须保留：{text3}"
        );
        assert!(text3.contains("[mcp_servers.router]"), "其他表仍在");

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn auth_json_only_written_when_key_present() {
        let home = temp_home();
        std::fs::write(config_path(&home), "model = \"gpt-5\"\n").unwrap();
        let adapter = CodexAdapter;
        let snap = adapter.snapshot(&home).unwrap();
        let patch = ToolConfigPatch {
            base_hash: snap.content_hash.clone(),
            providers: vec![ProviderEntry {
                id: "openhub-gateway".into(),
                name: "OpenHub 网关".into(),
                base_url: "http://x/v1".into(),
                api_key: "sk-abc".into(),
                protocol: "responses".into(),
                models: vec![],
            }],
            models: vec![],
            defaults: DefaultsSection {
                model: "gpt-5".into(),
                provider: "openhub-gateway".into(),
                reasoning_effort: String::new(),
                reasoning_effort_options: vec![],
                per_model_effort: Default::default(),
            },
            context: ContextSection::default(),
            thinking: ThinkingSection::default(),
        };
        let written = adapter.apply(&home, &patch).unwrap();
        assert!(written.contains(&"auth.json".to_string()));
        let auth: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(auth_path(&home)).unwrap()).unwrap();
        assert_eq!(auth["OPENAI_API_KEY"], "sk-abc");
        let _ = std::fs::remove_dir_all(&home);
    }
}
