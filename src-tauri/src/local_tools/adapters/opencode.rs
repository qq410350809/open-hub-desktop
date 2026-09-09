//! OpenCode：`~/.config/opencode/opencode.json`。
//!
//! 管辖字段（其余如 mcp / command / instructions 一律不动）：
//! - 供应商：`provider.*`（name/npm/options.baseURL/options.apiKey），
//!   `disabled_providers[]` 开关
//! - 模型：`provider.*.models.*`（name/limit.context/limit.output），
//!   思考档：`provider.*.models.*.variants.{low..max}.reasoningEffort`
//! - 默认：顶层 `model`（"provider/model" 形式）

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use super::{effort_options, snapshot_skeleton, ToolAdapter};
use crate::local_tools::fsutil::{atomic_write, read_text};
use crate::local_tools::mark::{is_managed_id, JSON_MARK_KEY, MANAGER_VALUE};
use crate::local_tools::types::{
    DefaultsSection, ModelEntry, ProviderEntry, ThinkingSection, ToolId, ToolConfigPatch,
    ToolConfigSnapshot,
};
use super::{content_hash, json_str, json_u64};

pub(crate) struct OpencodeAdapter;

const EFFORTS: &[&str] = &["low", "medium", "high", "max"];

fn config_path(home: &Path) -> PathBuf {
    home.join(".config").join("opencode").join("opencode.json")
}

impl OpencodeAdapter {
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
            .ok_or_else(|| "opencode.json 顶层不是对象，已跳过编辑".to_string())
    }

    /// 汇总所有供应商的模型条目（含 variants 思考档）。
    fn collect_models(root: &Map<String, Value>) -> Vec<ModelEntry> {
        let mut models = Vec::new();
        let Some(providers) = root.get("provider").and_then(|v| v.as_object()) else {
            return models;
        };
        for (provider_id, provider) in providers {
            let Some(provider_obj) = provider.as_object() else {
                continue;
            };
            let Some(provider_models) = provider_obj.get("models").and_then(|v| v.as_object())
            else {
                continue;
            };
            for (model_id, model) in provider_models {
                let obj = model.as_object();
                let context = obj
                    .and_then(|o| o.get("limit"))
                    .and_then(|l| json_u64(l, "context"))
                    .unwrap_or(0);
                let output = obj
                    .and_then(|o| o.get("limit"))
                    .and_then(|l| json_u64(l, "output"))
                    .unwrap_or(0);
                models.push(ModelEntry {
                    id: format!("{provider_id}/{model_id}"),
                    name: obj
                        .and_then(|o| json_str(&Value::Object(o.clone()), "name"))
                        .unwrap_or_else(|| model_id.clone()),
                    provider: provider_id.clone(),
                    context_window: context,
                    max_output: output,
                });
            }
        }
        models
    }
}

impl ToolAdapter for OpencodeAdapter {
    fn id(&self) -> ToolId {
        ToolId::Opencode
    }

    fn config_files(&self, home: &Path) -> Vec<(String, String, PathBuf)> {
        vec![(
            "config".into(),
            "opencode.json".into(),
            config_path(home),
        )]
    }

    fn detect(&self, home: &Path) -> (bool, String) {
        let root = home.join(".config").join("opencode");
        (root.is_dir(), root.display().to_string())
    }

    fn effect_note(&self) -> &'static str {
        "重启 OpenCode 会话后生效"
    }

    fn snapshot(&self, home: &Path) -> Result<ToolConfigSnapshot, String> {
        let path = config_path(home);
        let mut snap = snapshot_skeleton(self, home);
        let Some(root) = Self::parse(&path)? else {
            snap.warning = "尚未生成 opencode.json（首次运行 OpenCode 后会创建）".into();
            return Ok(snap);
        };

        let disabled: Vec<String> = root
            .get("disabled_providers")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        if let Some(providers) = root.get("provider").and_then(|v| v.as_object()) {
            for (id, provider) in providers {
                let Some(obj) = provider.as_object() else {
                    continue;
                };
                let options = obj.get("options").cloned().unwrap_or(Value::Null);
                let models: Vec<String> = obj
                    .get("models")
                    .and_then(|v| v.as_object())
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default();
                snap.providers.push(ProviderEntry {
                    id: id.clone(),
                    name: json_str(&Value::Object(obj.clone()), "name").unwrap_or_else(|| id.clone()),
                    base_url: json_str(&options, "baseURL").unwrap_or_default(),
                    api_key: json_str(&options, "apiKey").unwrap_or_default(),
                    protocol: json_str(&Value::Object(obj.clone()), "npm").unwrap_or_default(),
                    models,
                    ..Default::default()
                });
                if disabled.contains(id) {
                    if let Some(last) = snap.providers.last_mut() {
                        last.name = format!("{}（已停用）", last.name);
                    }
                }
            }
        }

        snap.models = Self::collect_models(&root);
        snap.defaults = DefaultsSection {
            model: json_str(&Value::Object(root.clone()), "model").unwrap_or_default(),
            provider: String::new(),
            reasoning_effort: String::new(),
            reasoning_effort_options: effort_options(EFFORTS),
            per_model_effort: Default::default(),
        };
        snap.thinking = ThinkingSection {
            effort_level: String::new(),
            effort_level_options: effort_options(EFFORTS),
            max_thinking_tokens: None,
        };
        snap.content_hash = content_hash(&[read_text(&path)?.unwrap_or_default()]);
        Ok(snap)
    }

    fn apply(&self, home: &Path, patch: &ToolConfigPatch) -> Result<Vec<String>, String> {
        let path = config_path(home);
        let mut root: Map<String, Value> = Self::parse(&path)?.unwrap_or_default();

        // 只 upsert OpenHub 供应商，用户原有 provider.* 一律保留。
        let mut providers_new = root
            .get("provider")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let stale: Vec<String> = providers_new
            .keys()
            .filter(|id| is_managed_id(id) && !patch.providers.iter().any(|p| p.id == **id))
            .cloned()
            .collect();
        for id in stale {
            providers_new.remove(&id);
        }
        for provider in &patch.providers {
            if !is_managed_id(&provider.id) {
                continue;
            }
            let name = provider.name.trim_end_matches("（已停用）").to_string();
            let mut options = Map::new();
            if !provider.base_url.is_empty() {
                options.insert("baseURL".into(), Value::String(provider.base_url.clone()));
            }
            if !provider.api_key.is_empty() {
                options.insert("apiKey".into(), Value::String(provider.api_key.clone()));
            }
            let mut provider_obj = Map::new();
            provider_obj.insert("name".into(), Value::String(name.clone()));
            if !provider.protocol.is_empty() {
                provider_obj.insert("npm".into(), Value::String(provider.protocol.clone()));
            }
            provider_obj.insert("options".into(), Value::Object(options));

            // 模型：只重建属于该供应商的条目；variants 思考档从 per_model_effort 推导
            // （key 形如 "provider/model"，value 形如 "low,high,max"）。
            let mut models_new = Map::new();
            for model in patch.models.iter().filter(|m| m.provider == provider.id) {
                // 兼容 "provider/model" 与 "alias/model"：只剥供应商标识，保留渠道路由别名。
                let short = model
                    .id
                    .strip_prefix(&format!("{}/", provider.id))
                    .unwrap_or(&model.id);
                let mut model_obj = Map::new();
                model_obj.insert("name".into(), Value::String(model.name.clone()));
                if model.context_window > 0 || model.max_output > 0 {
                    let mut limit = Map::new();
                    if model.context_window > 0 {
                        limit.insert("context".into(), json!(model.context_window));
                    }
                    if model.max_output > 0 {
                        limit.insert("output".into(), json!(model.max_output));
                    }
                    model_obj.insert("limit".into(), Value::Object(limit));
                }
                let effort_key = format!("{}/{}", provider.id, short);
                if let Some(efforts) = patch.defaults.per_model_effort.get(&effort_key) {
                    let variants: Map<String, Value> = efforts
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(|e| {
                            (
                                e.to_string(),
                                json!({ "reasoningEffort": e }),
                            )
                        })
                        .collect();
                    if !variants.is_empty() {
                        model_obj.insert("variants".into(), Value::Object(variants));
                    }
                }
                models_new.insert(short.to_string(), Value::Object(model_obj));
            }
            if !models_new.is_empty() {
                provider_obj.insert("models".into(), Value::Object(models_new));
            }
            provider_obj.insert(
                JSON_MARK_KEY.into(),
                json!({ "managed": true, "manager": MANAGER_VALUE }),
            );
            providers_new.insert(provider.id.clone(), Value::Object(provider_obj));
        }
        root.insert("provider".into(), Value::Object(providers_new));

        // disabled_providers：保留用户原有项，只同步 OpenHub 条目的停用状态。
        let mut disabled: Vec<String> = root
            .get("disabled_providers")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .filter(|id| !is_managed_id(id))
                    .collect()
            })
            .unwrap_or_default();
        for provider in &patch.providers {
            if is_managed_id(&provider.id) && provider.name.ends_with("（已停用）") {
                disabled.push(provider.id.clone());
            }
        }
        disabled.sort();
        disabled.dedup();
        if disabled.is_empty() {
            root.remove("disabled_providers");
        } else {
            root.insert(
                "disabled_providers".into(),
                Value::Array(disabled.into_iter().map(Value::String).collect()),
            );
        }

        // 默认模型。
        if patch.defaults.model.is_empty() {
            root.remove("model");
        } else {
            root.insert("model".into(), Value::String(patch.defaults.model.clone()));
        }

        let text = serde_json::to_string_pretty(&Value::Object(root)).map_err(|e| e.to_string())?
            + "\n";
        atomic_write(&path, &text)?;
        Ok(vec!["opencode.json".to_string()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "openhub-lt-oc-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".config/opencode")).unwrap();
        dir
    }

    #[test]
    fn apply_preserves_user_providers_and_other_keys() {
        let home = temp_home();
        let path = config_path(&home);
        std::fs::write(
            &path,
            r#"{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {"x": {"type": "local", "command": ["a"]}},
  "instructions": ["中文回复"],
  "provider": {
    "p1": {
      "name": "p1",
      "npm": "@ai-sdk/openai-compatible",
      "options": {"baseURL": "http://old/v1"},
      "models": {
        "m1": {"name": "m1", "limit": {"context": 100000, "output": 32000},
               "variants": {"low": {"reasoningEffort": "low"}, "max": {"reasoningEffort": "max"}}}
      }
    }
  },
  "model": "p1/m1"
}"#,
        )
        .unwrap();

        let adapter = OpencodeAdapter;
        let snap = adapter.snapshot(&home).unwrap();
        assert_eq!(snap.providers.len(), 1);
        assert_eq!(snap.models.len(), 1);
        assert_eq!(snap.models[0].context_window, 100_000);
        assert_eq!(snap.defaults.model, "p1/m1");

        let patch = ToolConfigPatch {
            base_hash: snap.content_hash.clone(),
            providers: vec![ProviderEntry {
                id: "openhub-site_a_acc_0".into(),
                name: "站点 A".into(),
                base_url: "http://127.0.0.1:17896/v1".into(),
                api_key: "sk-openhub-test".into(),
                protocol: "@ai-sdk/openai-compatible".into(),
                models: vec!["alias/m1".into()],
            }],
            models: vec![ModelEntry {
                id: "alias/m1".into(),
                name: "m1".into(),
                provider: "openhub-site_a_acc_0".into(),
                context_window: 500_000,
                max_output: 32_000,
            }],
            defaults: DefaultsSection {
                model: "p1/m1".into(),
                ..Default::default()
            },
            context: Default::default(),
            thinking: ThinkingSection::default(),
        };
        adapter.apply(&home, &patch).unwrap();

        let out: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(out["mcp"]["x"]["type"], "local", "mcp 必须保留");
        assert_eq!(out["instructions"][0], "中文回复");
        assert_eq!(out["provider"]["p1"]["models"]["m1"]["limit"]["context"], 100_000, "用户供应商不得被改写");
        assert_eq!(out["provider"]["openhub-site_a_acc_0"]["options"]["baseURL"], "http://127.0.0.1:17896/v1");
        assert_eq!(out["provider"]["openhub-site_a_acc_0"]["models"]["alias/m1"]["limit"]["context"], 500_000);
        assert_eq!(out["model"], "p1/m1");

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn apply_merges_openhub_providers_and_keeps_user_ones() {
        let home = temp_home();
        let path = config_path(&home);
        std::fs::write(
            &path,
            r#"{
  "mcp": {"x": {"type": "local"}},
  "provider": {
    "p1": {"name": "p1", "options": {"baseURL": "http://old/v1"}}
  }
}"#,
        )
        .unwrap();

        let adapter = OpencodeAdapter;
        let snap = adapter.snapshot(&home).unwrap();
        let patch = ToolConfigPatch {
            base_hash: snap.content_hash.clone(),
            providers: vec![ProviderEntry {
                id: "openhub-site_a_acc_0".into(),
                name: "站点 A".into(),
                base_url: "http://127.0.0.1:17896/v1".into(),
                api_key: "sk-openhub-test".into(),
                protocol: "@ai-sdk/openai-compatible".into(),
                models: vec![],
            }],
            models: vec![],
            defaults: Default::default(),
            context: Default::default(),
            thinking: Default::default(),
        };
        adapter.apply(&home, &patch).unwrap();
        let out: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(out["mcp"]["x"]["type"], "local");
        assert_eq!(out["provider"]["p1"]["name"], "p1", "用户供应商必须保留");
        assert_eq!(
            out["provider"]["openhub-site_a_acc_0"]["options"]["baseURL"],
            "http://127.0.0.1:17896/v1"
        );
        assert_eq!(
            out["provider"]["openhub-site_a_acc_0"]["x-openhub"]["manager"],
            "OpenHub"
        );

        let _ = std::fs::remove_dir_all(&home);
    }
}
