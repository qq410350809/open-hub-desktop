//! ZCode：`~/.zcode/v2/config.json`。
//!
//! 管辖字段（其余如 agents / plugins 等一律不动）：
//! - 供应商：`provider.*`（name/kind: anthropic|openai/options.baseURL/options.apiKey）
//! - 模型：`provider.*.models.*`（limit.context/limit.output）
//!
//! 注：ZCode 的默认模型/会话内选择存于会话状态而非该文件，
//! 因此本适配器不管理 defaults，只管理供应商与模型清单。

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use super::{content_hash, json_str, json_u64, snapshot_skeleton, ToolAdapter};
use crate::local_tools::fsutil::{atomic_write_stamped, read_text};
use crate::local_tools::mark::{
    is_managed_id, FINGERPRINT_PLACEHOLDER, JSON_MARK_KEY, MANAGER_VALUE,
};
use crate::local_tools::types::{
    ContextSection, ModelEntry, ProviderEntry, ThinkingSection, ToolConfigPatch,
    ToolConfigSnapshot, ToolId,
};

pub(crate) struct ZcodeAdapter;

fn config_path(home: &Path) -> PathBuf {
    home.join(".zcode").join("v2").join("config.json")
}

impl ZcodeAdapter {
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

impl ToolAdapter for ZcodeAdapter {
    fn id(&self) -> ToolId {
        ToolId::Zcode
    }

    fn config_files(&self, home: &Path) -> Vec<(String, String, PathBuf)> {
        vec![("config".into(), "v2/config.json".into(), config_path(home))]
    }

    fn detect(&self, home: &Path) -> (bool, String) {
        let root = home.join(".zcode");
        (root.is_dir(), root.display().to_string())
    }

    fn effect_note(&self) -> &'static str {
        "重启 ZCode 后生效"
    }

    fn snapshot(&self, home: &Path) -> Result<ToolConfigSnapshot, String> {
        let path = config_path(home);
        let mut snap = snapshot_skeleton(self, home);
        let Some(root) = Self::parse(&path)? else {
            snap.warning = "尚未生成 v2/config.json（首次运行 ZCode 后会创建）".into();
            return Ok(snap);
        };

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
                    name: json_str(&Value::Object(obj.clone()), "name")
                        .unwrap_or_else(|| id.clone()),
                    base_url: json_str(&options, "baseURL").unwrap_or_default(),
                    api_key: json_str(&options, "apiKey").unwrap_or_default(),
                    protocol: json_str(&Value::Object(obj.clone()), "kind").unwrap_or_default(),
                    models,
                });
            }
        }

        // 模型清单（含 limit）。
        if let Some(providers) = root.get("provider").and_then(|v| v.as_object()) {
            for (provider_id, provider) in providers {
                let Some(models) = provider.get("models").and_then(|v| v.as_object()) else {
                    continue;
                };
                for (model_id, model) in models {
                    let limit = model.get("limit").cloned().unwrap_or(Value::Null);
                    snap.models.push(ModelEntry {
                        id: model_id.clone(),
                        name: json_str(
                            &Value::Object(model.as_object().cloned().unwrap_or_default()),
                            "name",
                        )
                        .unwrap_or_else(|| model_id.clone()),
                        provider: provider_id.clone(),
                        context_window: json_u64(&limit, "context").unwrap_or(0),
                        max_output: json_u64(&limit, "output").unwrap_or(0),
                    });
                }
            }
        }

        snap.context = ContextSection::default();
        snap.thinking = ThinkingSection::default();
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
            let mut options = Map::new();
            if !provider.base_url.is_empty() {
                options.insert("baseURL".into(), Value::String(provider.base_url.clone()));
            }
            if !provider.api_key.is_empty() {
                options.insert("apiKey".into(), Value::String(provider.api_key.clone()));
            }
            let mut provider_obj = Map::new();
            provider_obj.insert("name".into(), Value::String(provider.name.clone()));
            if !provider.protocol.is_empty() {
                provider_obj.insert("kind".into(), Value::String(provider.protocol.clone()));
            }
            provider_obj.insert("options".into(), Value::Object(options));

            let mut models_new = Map::new();
            for model in patch.models.iter().filter(|m| m.provider == provider.id) {
                let mut model_obj = Map::new();
                if model.name != model.id {
                    model_obj.insert("name".into(), Value::String(model.name.clone()));
                }
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
                models_new.insert(model.id.clone(), Value::Object(model_obj));
            }
            if !models_new.is_empty() {
                provider_obj.insert("models".into(), Value::Object(models_new));
            }
            provider_obj.insert(
                JSON_MARK_KEY.into(),
                json!({
                    "managed": true,
                    "manager": MANAGER_VALUE,
                    // 落盘时盖成真实指纹（各供应商段同值），用于判断此后有没有被外部改过
                    "fingerprint": FINGERPRINT_PLACEHOLDER,
                }),
            );
            providers_new.insert(provider.id.clone(), Value::Object(provider_obj));
        }
        root.insert("provider".into(), Value::Object(providers_new));

        let text =
            serde_json::to_string_pretty(&Value::Object(root)).map_err(|e| e.to_string())? + "\n";
        atomic_write_stamped(&path, &text)?;
        Ok(vec!["v2/config.json".to_string()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "openhub-lt-zc-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".zcode/v2")).unwrap();
        dir
    }

    #[test]
    fn apply_preserves_other_top_level_keys() {
        let home = temp_home();
        let path = config_path(&home);
        std::fs::write(
            &path,
            r#"{
  "provider": {
    "p1": {
      "name": "local",
      "kind": "anthropic",
      "options": {"baseURL": "http://127.0.0.1:1/v1", "apiKey": "sk-old"},
      "models": {"m1": {"limit": {"context": 200000, "output": 64000}}}
    }
  },
  "agents": {"keep": true}
}"#,
        )
        .unwrap();

        let adapter = ZcodeAdapter;
        let snap = adapter.snapshot(&home).unwrap();
        assert_eq!(snap.providers[0].base_url, "http://127.0.0.1:1/v1");
        assert_eq!(snap.models[0].context_window, 200_000);

        let patch = ToolConfigPatch {
            base_hash: snap.content_hash.clone(),
            providers: vec![ProviderEntry {
                id: "openhub-site_z_acc_0".into(),
                name: "站点 Z".into(),
                base_url: "http://127.0.0.1:17896/v1".into(),
                api_key: "sk-openhub-test".into(),
                protocol: "openai".into(),
                models: vec!["m1".into()],
            }],
            models: vec![ModelEntry {
                id: "m1".into(),
                name: "m1".into(),
                provider: "openhub-site_z_acc_0".into(),
                context_window: 200_000,
                max_output: 128_000,
            }],
            defaults: Default::default(),
            context: Default::default(),
            thinking: Default::default(),
        };
        adapter.apply(&home, &patch).unwrap();

        let out: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(out["agents"]["keep"], true, "其他顶层键必须保留");
        assert_eq!(
            out["provider"]["p1"]["options"]["baseURL"], "http://127.0.0.1:1/v1",
            "用户供应商不得被改写"
        );
        assert_eq!(out["provider"]["p1"]["options"]["apiKey"], "sk-old");
        assert_eq!(
            out["provider"]["p1"]["models"]["m1"]["limit"]["output"],
            64000
        );
        assert_eq!(
            out["provider"]["openhub-site_z_acc_0"]["options"]["baseURL"],
            "http://127.0.0.1:17896/v1"
        );
        assert_eq!(
            out["provider"]["openhub-site_z_acc_0"]["models"]["m1"]["limit"]["output"],
            128_000
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn apply_merges_openhub_providers_and_keeps_user_ones() {
        let home = temp_home();
        let path = config_path(&home);
        std::fs::write(
            &path,
            r#"{
  "provider": {
    "p1": {
      "name": "local",
      "kind": "anthropic",
      "options": {"baseURL": "http://old/v1", "apiKey": "sk-old"}
    }
  },
  "agents": {"keep": true}
}"#,
        )
        .unwrap();
        let adapter = ZcodeAdapter;
        let snap = adapter.snapshot(&home).unwrap();
        let patch = ToolConfigPatch {
            base_hash: snap.content_hash.clone(),
            providers: vec![ProviderEntry {
                id: "openhub-site_z_acc_0".into(),
                name: "站点 Z".into(),
                base_url: "http://127.0.0.1:17896/v1".into(),
                api_key: "sk-openhub-test".into(),
                protocol: "openai".into(),
                models: vec![],
            }],
            models: vec![],
            defaults: Default::default(),
            context: Default::default(),
            thinking: Default::default(),
        };
        adapter.apply(&home, &patch).unwrap();
        let out: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(out["agents"]["keep"], true);
        assert_eq!(out["provider"]["p1"]["name"], "local");
        assert_eq!(
            out["provider"]["openhub-site_z_acc_0"]["x-openhub"]["manager"],
            "OpenHub"
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
