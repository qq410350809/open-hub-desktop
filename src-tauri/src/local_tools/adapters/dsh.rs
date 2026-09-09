//! DeepSeek CLI (DSH)：`~/.dsh/settings.yaml`。
//!
//! 管辖字段（其余顶层键如 ui-onboarding 一律不动）：
//! - 供应商：`llm-pi-ai.providers.*`
//!   （displayName / apiKeyEnv / api / baseURL / models[]）
//! - 模型：每供应商 `models[]`（id/contextWindow/maxTokens）
//! - 思考档：`models[].reasoningEfforts`（map，如 off/high/max）
//!
//! 默认模型选择存于 DSH 会话/profile，不在该文件，故不管理 defaults。

use std::path::{Path, PathBuf};

use serde_yaml::{Mapping, Value as Yaml};

use super::{content_hash, snapshot_skeleton, ToolAdapter};
use crate::local_tools::fsutil::{atomic_write, read_text};
use crate::local_tools::mark::{is_managed_id, MANAGER_VALUE, YAML_MARK_KEY};
use crate::local_tools::types::{
    ModelEntry, ProviderEntry, ToolId, ToolConfigPatch, ToolConfigSnapshot,
};

pub(crate) struct DshAdapter;

const SECTION: &str = "llm-pi-ai";
const PROVIDERS_KEY: &str = "providers";

fn settings_path(home: &Path) -> PathBuf {
    home.join(".dsh").join("settings.yaml")
}

impl DshAdapter {
    fn parse(path: &Path) -> Result<Option<Mapping>, String> {
        let Some(text) = read_text(path)? else {
            return Ok(None);
        };
        if text.trim().is_empty() {
            return Ok(None);
        }
        serde_yaml::from_str::<Yaml>(&text)
            .ok()
            .and_then(|v| v.as_mapping().cloned())
            .map(Some)
            .ok_or_else(|| "settings.yaml 顶层不是映射，已跳过编辑".to_string())
    }

    fn yaml_str(value: &Yaml, key: &str) -> String {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    }

    fn yaml_u64(value: &Yaml, key: &str) -> u64 {
        value.get(key).and_then(|v| v.as_u64()).unwrap_or(0)
    }
}

impl ToolAdapter for DshAdapter {
    fn id(&self) -> ToolId {
        ToolId::Dsh
    }

    fn config_files(&self, home: &Path) -> Vec<(String, String, PathBuf)> {
        vec![(
            "config".into(),
            "settings.yaml".into(),
            settings_path(home),
        )]
    }

    fn detect(&self, home: &Path) -> (bool, String) {
        let root = home.join(".dsh");
        (root.is_dir(), root.display().to_string())
    }

    fn effect_note(&self) -> &'static str {
        "重启 DeepSeek CLI (DSH) 会话后生效"
    }

    fn snapshot(&self, home: &Path) -> Result<ToolConfigSnapshot, String> {
        let path = settings_path(home);
        let mut snap = snapshot_skeleton(self, home);
        let Some(root) = Self::parse(&path)? else {
            snap.warning = "尚未生成 settings.yaml（首次运行 DSH 后会创建）".into();
            return Ok(snap);
        };

        let section = root.get(SECTION).cloned().unwrap_or(Yaml::Mapping(Mapping::new()));
        let providers = section
            .get(PROVIDERS_KEY)
            .and_then(|v| v.as_mapping())
            .cloned()
            .unwrap_or_default();

        for (key, provider) in providers.iter() {
            let Some(provider_id) = key.as_str() else {
                continue;
            };
            let obj = provider.clone();
            let models: Vec<ModelEntry> = provider
                .get("models")
                .and_then(|v| v.as_sequence())
                .map(|seq| {
                    seq.iter()
                        .map(|model| {
                            let efforts: Vec<String> = model
                                .get("reasoningEfforts")
                                .and_then(|v| v.as_mapping())
                                .map(|m| {
                                    m.iter()
                                        .filter_map(|(k, _)| k.as_str())
                                        .map(str::to_string)
                                        .collect()
                                })
                                .unwrap_or_default();
                            ModelEntry {
                                id: Self::yaml_str(&model, "id"),
                                name: {
                                    let n = Self::yaml_str(&model, "name");
                                    if n.is_empty() { Self::yaml_str(&model, "id") } else { n }
                                },
                                provider: provider_id.to_string(),
                                context_window: Self::yaml_u64(&model, "contextWindow"),
                                max_output: Self::yaml_u64(&model, "maxTokens"),
                            }
                            .with_efforts(efforts)
                        })
                        .collect()
                })
                .unwrap_or_default();
            snap.providers.push(ProviderEntry {
                id: provider_id.to_string(),
                name: {
                    let n = Self::yaml_str(&obj, "displayName");
                    if n.is_empty() { provider_id.to_string() } else { n }
                },
                base_url: Self::yaml_str(&obj, "baseURL"),
                api_key: String::new(),
                protocol: Self::yaml_str(&obj, "api"),
                models: models.iter().map(|m| m.id.clone()).collect(),
            });
            snap.models.extend(models);
        }

        snap.content_hash = content_hash(&[read_text(&path)?.unwrap_or_default()]);
        Ok(snap)
    }

    fn apply(&self, home: &Path, patch: &ToolConfigPatch) -> Result<Vec<String>, String> {
        let path = settings_path(home);
        let mut root = Self::parse(&path)?.unwrap_or_default();

        // 只 upsert OpenHub 供应商，用户原有 llm-pi-ai.providers 一律保留。
        let existing_section = root
            .get(Yaml::String(SECTION.into()))
            .and_then(|v| v.as_mapping())
            .cloned()
            .unwrap_or_default();
        let mut providers = existing_section
            .get(Yaml::String(PROVIDERS_KEY.into()))
            .and_then(|v| v.as_mapping())
            .cloned()
            .unwrap_or_default();
        let stale: Vec<Yaml> = providers
            .keys()
            .filter(|key| {
                key.as_str()
                    .map(|id| is_managed_id(id) && !patch.providers.iter().any(|p| p.id == id))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        for key in stale {
            providers.remove(&key);
        }
        for provider in &patch.providers {
            if !is_managed_id(&provider.id) {
                continue;
            }
            let mut provider_map = providers
                .get(Yaml::String(provider.id.clone()))
                .and_then(|v| v.as_mapping())
                .cloned()
                .unwrap_or_default();
            if !provider.name.is_empty() {
                provider_map.insert(
                    Yaml::String("displayName".into()),
                    Yaml::String(provider.name.clone()),
                );
            }
            if !provider.protocol.is_empty() {
                provider_map.insert(
                    Yaml::String("api".into()),
                    Yaml::String(provider.protocol.clone()),
                );
            }
            if !provider.base_url.is_empty() {
                provider_map.insert(
                    Yaml::String("baseURL".into()),
                    Yaml::String(provider.base_url.clone()),
                );
            }
            let env_name = format!(
                "OPENHUB_{}_API_KEY",
                crate::local_tools::mark::sanitize_id_part(&provider.id).to_ascii_uppercase()
            );
            provider_map.insert(
                Yaml::String("apiKeyEnv".into()),
                Yaml::String(env_name),
            );
            provider_map.insert(
                Yaml::String(YAML_MARK_KEY.into()),
                Yaml::String(MANAGER_VALUE.into()),
            );

            let mut models = Vec::new();
            for model in patch.models.iter().filter(|m| m.provider == provider.id) {
                let mut model_map = Mapping::new();
                model_map.insert(Yaml::String("id".into()), Yaml::String(model.id.clone()));
                if model.name != model.id {
                    model_map.insert(Yaml::String("name".into()), Yaml::String(model.name.clone()));
                }
                if model.context_window > 0 {
                    model_map.insert(
                        Yaml::String("contextWindow".into()),
                        Yaml::Number(model.context_window.into()),
                    );
                }
                if model.max_output > 0 {
                    model_map.insert(
                        Yaml::String("maxTokens".into()),
                        Yaml::Number(model.max_output.into()),
                    );
                }
                // 思考档：per_model_effort value 形如 "off,high,max"。
                let effort_key = format!("{}/{}", provider.id, model.id);
                if let Some(efforts) = patch.defaults.per_model_effort.get(&effort_key) {
                    let mut map = Mapping::new();
                    for effort in efforts.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                        // 与既有文件一致：非 off 档映射到自身，off 为 null。
                        let value = if effort == "off" {
                            Yaml::Null
                        } else {
                            Yaml::String(effort.to_string())
                        };
                        map.insert(Yaml::String(effort.to_string()), value);
                    }
                    model_map.insert(Yaml::String("reasoningEfforts".into()), Yaml::Mapping(map));
                }
                models.push(Yaml::Mapping(model_map));
            }
            if !models.is_empty() {
                provider_map.insert(Yaml::String("models".into()), Yaml::Sequence(models));
            }
            providers.insert(Yaml::String(provider.id.clone()), Yaml::Mapping(provider_map));
        }

        let section = root
            .get(Yaml::String(SECTION.into()))
            .and_then(|v| v.as_mapping())
            .cloned()
            .unwrap_or_default();
        let mut section = section;
        section.insert(
            Yaml::String(PROVIDERS_KEY.into()),
            Yaml::Mapping(providers),
        );
        root.insert(Yaml::String(SECTION.into()), Yaml::Mapping(section));

        let text = serde_yaml::to_string(&Yaml::Mapping(root)).map_err(|e| e.to_string())?;
        atomic_write(&path, &text)?;
        Ok(vec!["settings.yaml".to_string()])
    }
}

/// ModelEntry 扩展：临时携带思考档信息（快照用）。
trait WithEfforts {
    fn with_efforts(self, efforts: Vec<String>) -> ModelEntry;
}

impl WithEfforts for ModelEntry {
    fn with_efforts(mut self, efforts: Vec<String>) -> Self {
        if !efforts.is_empty() {
            self.name = format!("{}|{}", self.name, efforts.join(","));
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_tools::types::DefaultsSection;

    fn temp_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("openhub-lt-dsh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".dsh")).unwrap();
        dir
    }

    #[test]
    fn apply_rewrites_providers_keeps_other_keys() {
        let home = temp_home();
        let path = settings_path(&home);
        std::fs::write(
            &path,
            "ui-onboarding:\n  welcomeNoticeVersion: 1\nllm-pi-ai:\n  providers:\n    fastmodel:\n      displayName: fastmodel\n      api: openai-completions\n      baseURL: https://old/v1\n      models:\n        - id: m1\n          contextWindow: 100000\n          maxTokens: 32000\n          reasoningEfforts:\n            off: null\n            high: high\n",
        )
        .unwrap();

        let adapter = DshAdapter;
        let snap = adapter.snapshot(&home).unwrap();
        assert_eq!(snap.providers.len(), 1);
        assert_eq!(snap.models.len(), 1);
        assert_eq!(snap.models[0].context_window, 100_000);

        let patch = ToolConfigPatch {
            base_hash: snap.content_hash.clone(),
            providers: vec![ProviderEntry {
                id: "openhub-site_d_acc_0".into(),
                name: "站点 D".into(),
                base_url: "http://127.0.0.1:17896/v1".into(),
                api_key: "sk-openhub-test".into(),
                protocol: "openai-completions".into(),
                models: vec!["m1".into()],
            }],
            models: vec![ModelEntry {
                id: "m1".into(),
                name: "m1".into(),
                provider: "openhub-site_d_acc_0".into(),
                context_window: 500_000,
                max_output: 32_000,
            }],
            defaults: DefaultsSection {
                per_model_effort: super::super::map_from(&[("openhub-site_d_acc_0/m1", "off,high,max")]),
                ..Default::default()
            },
            context: Default::default(),
            thinking: Default::default(),
        };
        adapter.apply(&home, &patch).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("welcomeNoticeVersion"), "其他顶层键保留：{text}");
        assert!(text.contains("fastmodel"), "用户供应商必须保留：{text}");
        assert!(text.contains("https://old/v1"), "用户供应商不得被改写：{text}");
        assert!(text.contains("openhub-site_d_acc_0"), "{text}");
        assert!(text.contains("baseURL: http://127.0.0.1:17896/v1"));
        assert!(text.contains("contextWindow: 500000"));
        assert!(text.contains("max: max"));

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn apply_merges_openhub_providers_and_keeps_user_ones() {
        let home = temp_home();
        let path = settings_path(&home);
        std::fs::write(
            &path,
            "ui-onboarding:\n  welcomeNoticeVersion: 1\nllm-pi-ai:\n  providers:\n    fastmodel:\n      displayName: fastmodel\n      api: openai-completions\n      baseURL: https://old/v1\n",
        )
        .unwrap();
        let adapter = DshAdapter;
        let snap = adapter.snapshot(&home).unwrap();
        let patch = ToolConfigPatch {
            base_hash: snap.content_hash.clone(),
            providers: vec![ProviderEntry {
                id: "openhub-site_d_acc_0".into(),
                name: "站点 D".into(),
                base_url: "http://127.0.0.1:17896/v1".into(),
                api_key: "sk-openhub-test".into(),
                protocol: "openai-completions".into(),
                models: vec![],
            }],
            models: vec![],
            defaults: Default::default(),
            context: Default::default(),
            thinking: Default::default(),
        };
        adapter.apply(&home, &patch).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("welcomeNoticeVersion"), "{text}");
        assert!(text.contains("fastmodel"), "用户供应商必须保留：{text}");
        assert!(text.contains("openhub-site_d_acc_0"), "{text}");
        assert!(text.contains("x-openhub: OpenHub"), "{text}");
        let _ = std::fs::remove_dir_all(&home);
    }
}
