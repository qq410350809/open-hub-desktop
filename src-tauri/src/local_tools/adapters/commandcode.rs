//! Command Code：`~/.commandcode/config.json`。
//!
//! 管辖字段（其余如 installed / firstMessageSent 一律不动）：
//! - 默认：`provider`（供应商标识）、`model`（默认模型）
//! - 思考：`reasoningEffort`（按模型 ID 的 map）
//!
//! 该工具无 baseURL/Key 明文（走 DeepSeek 官方 OAuth），
//! 供应商维度只暴露 provider 名称。

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::{content_hash, json_str, snapshot_skeleton, ToolAdapter};
use crate::local_tools::fsutil::{atomic_write, read_text};
use crate::local_tools::types::{
    DefaultsSection, ProviderEntry, ThinkingSection, ToolId, ToolConfigPatch, ToolConfigSnapshot,
};

pub(crate) struct CommandCodeAdapter;

const EFFORTS: &[&str] = &["off", "low", "medium", "high", "max"];

fn config_path(home: &Path) -> PathBuf {
    home.join(".commandcode").join("config.json")
}

impl CommandCodeAdapter {
    fn parse(path: &Path) -> Result<Option<Map<String, Value>>, String> {
        let Some(text) = read_text(path)? else {
            return Ok(None);
        };
        if text.trim().is_empty() {
            return Ok(None);
        }
        serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .map(Some)
            .ok_or_else(|| "config.json 顶层不是对象，已跳过编辑".to_string())
    }
}

impl ToolAdapter for CommandCodeAdapter {
    fn id(&self) -> ToolId {
        ToolId::CommandCode
    }

    fn config_files(&self, home: &Path) -> Vec<(String, String, PathBuf)> {
        vec![("config".into(), "config.json".into(), config_path(home))]
    }

    fn detect(&self, home: &Path) -> (bool, String) {
        let root = home.join(".commandcode");
        (root.is_dir(), root.display().to_string())
    }

    fn effect_note(&self) -> &'static str {
        "重启 Command Code 会话后生效"
    }

    fn snapshot(&self, home: &Path) -> Result<ToolConfigSnapshot, String> {
        let path = config_path(home);
        let mut snap = snapshot_skeleton(self, home);
        let Some(root) = Self::parse(&path)? else {
            snap.warning = "尚未生成 config.json（首次运行 Command Code 后会创建）".into();
            return Ok(snap);
        };

        let provider = json_str(&Value::Object(root.clone()), "provider").unwrap_or_default();
        let model = json_str(&Value::Object(root.clone()), "model").unwrap_or_default();
        let efforts: std::collections::BTreeMap<String, String> = root
            .get("reasoningEffort")
            .and_then(|v| v.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        snap.providers.push(ProviderEntry {
            id: provider.clone(),
            name: provider.clone(),
            ..Default::default()
        });
        let model_ids: Vec<String> = {
            let mut ids: std::collections::BTreeSet<String> =
                efforts.keys().cloned().collect();
            if !model.is_empty() {
                ids.insert(model.clone());
            }
            ids.into_iter().collect()
        };
        snap.models = model_ids
            .into_iter()
            .map(|id| crate::local_tools::types::ModelEntry {
                id,
                name: String::new(),
                provider: provider.clone(),
                ..Default::default()
            })
            .collect();
        snap.defaults = DefaultsSection {
            model,
            provider,
            reasoning_effort: String::new(),
            reasoning_effort_options: super::effort_options(EFFORTS),
            per_model_effort: efforts,
        };
        snap.thinking = ThinkingSection::default();
        snap.content_hash = content_hash(&[read_text(&path)?.unwrap_or_default()]);
        Ok(snap)
    }

    fn apply(&self, home: &Path, patch: &ToolConfigPatch) -> Result<Vec<String>, String> {
        let path = config_path(home);
        let mut root = Self::parse(&path)?.unwrap_or_default();

        if let Some(provider) = patch.providers.first() {
            if provider.id.is_empty() {
                root.remove("provider");
            } else {
                root.insert("provider".into(), Value::String(provider.id.clone()));
            }
        }
        if patch.defaults.model.is_empty() {
            root.remove("model");
        } else {
            root.insert("model".into(), Value::String(patch.defaults.model.clone()));
        }
        if patch.defaults.per_model_effort.is_empty() {
            root.remove("reasoningEffort");
        } else {
            root.insert(
                "reasoningEffort".into(),
                Value::Object(
                    patch
                        .defaults
                        .per_model_effort
                        .iter()
                        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                        .collect(),
                ),
            );
        }

        let text = serde_json::to_string_pretty(&Value::Object(root)).map_err(|e| e.to_string())?
            + "\n";
        atomic_write(&path, &text)?;
        Ok(vec!["config.json".to_string()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("openhub-lt-cc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".commandcode")).unwrap();
        dir
    }

    #[test]
    fn roundtrip_preserves_unknown_fields() {
        let home = temp_home();
        let path = config_path(&home);
        std::fs::write(
            &path,
            r#"{"provider":"command-code","installed":true,"firstMessageSent":true,
"reasoningEffort":{"deepseek/deepseek-v4-flash":"max"},
"model":"deepseek/deepseek-v4-flash","tasteLearning":false}"#,
        )
        .unwrap();

        let adapter = CommandCodeAdapter;
        let snap = adapter.snapshot(&home).unwrap();
        assert_eq!(snap.defaults.model, "deepseek/deepseek-v4-flash");
        assert_eq!(
            snap.defaults.per_model_effort.get("deepseek/deepseek-v4-flash").map(String::as_str),
            Some("max")
        );

        let mut defaults = snap.defaults.clone();
        defaults.model = "deepseek/deepseek-v4-pro".into();
        defaults.per_model_effort.insert(
            "deepseek/deepseek-v4-pro".into(),
            "high".into(),
        );
        let patch = ToolConfigPatch {
            base_hash: snap.content_hash.clone(),
            providers: snap.providers.clone(),
            models: vec![],
            defaults,
            context: Default::default(),
            thinking: Default::default(),
        };
        adapter.apply(&home, &patch).unwrap();

        let out: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(out["installed"], true, "未知字段保留");
        assert_eq!(out["tasteLearning"], false);
        assert_eq!(out["model"], "deepseek/deepseek-v4-pro");
        assert_eq!(out["reasoningEffort"]["deepseek/deepseek-v4-pro"], "high");

        let _ = std::fs::remove_dir_all(&home);
    }
}
