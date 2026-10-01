//! OpenCode：`~/.config/opencode/opencode.json`。
//!
//! 管辖字段（其余如 mcp / command / instructions 一律不动）：
//! - 供应商：`provider.*`（name/npm/options.baseURL/options.apiKey），
//!   `disabled_providers[]` 开关
//! - 模型：`provider.*.models.*`（name/limit.context/limit.output），
//!   思考档：`provider.*.models.*.variants.{low..max}.reasoningEffort`，
//!   逐模型默认档：`provider.*.models.*.options.reasoningEffort`（OpenCode 唯一落点）
//! - 默认：顶层 `model`（"provider/model" 形式）；无全局思考级别字段，读回恒定未设置

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use super::{content_hash, json_str, json_u64};
use super::{effort_options, snapshot_skeleton, ToolAdapter};
use crate::local_tools::fsutil::{atomic_write_stamped, read_text};
use crate::local_tools::mark::{
    is_managed_id, FINGERPRINT_PLACEHOLDER, JSON_MARK_KEY, MANAGER_VALUE,
};
use crate::local_tools::types::{
    DefaultsSection, ModelEntry, ProviderEntry, ThinkingSection, ToolConfigPatch,
    ToolConfigSnapshot, ToolId,
};

pub(crate) struct OpencodeAdapter;
const EFFORTS: &[&str] = &["low", "medium", "high", "max"];

// opencode.json 没有全局思考级别字段：`apply` 只会把全局值扇出到
// 各模型自己的 `options.reasoningEffort`。逐模型默认档（目录最高档）
// 写入的正是同一个字段——从任意模型反推全局，都会把逐模型值误当成
// 全局设置，下次保存还会把这个「发明的全局值」扩散到其它模型，
// 配置比对也会跟着误报不一致。因此全局读回恒定「未设置」，
// 磁盘上的真实档位由 `ModelEntry.reasoning_effort` 逐模型承载。

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
                    // 逐模型默认思考级别回读：写入点就是 options.reasoningEffort。
                    reasoning_effort: obj
                        .and_then(|o| o.get("options"))
                        .and_then(|options| json_str(options, "reasoningEffort"))
                        .unwrap_or_default(),
                });
            }
        }
        models
    }

    /// 汇总各模型的思考档（`models[].variants` 的键），键为 `供应商/模型`，
    /// 值与 `apply` 写入的口径一致（形如 `low,max`）。
    ///
    /// 必须回读：`apply` 会按 per_model_effort 重建 variants，读取不回填的话
    /// 界面看不到已配档位，且保存时会把用户原有 variants 一并清掉。
    /// 没有 variants 且没有默认 effort 时回填空值，避免再次保存继承全局档位。
    /// 只有 options.reasoningEffort 时不倒推档位集合；默认值由 ModelEntry 独立回读。
    fn collect_efforts(root: &Map<String, Value>) -> std::collections::BTreeMap<String, String> {
        let mut out = std::collections::BTreeMap::new();
        let Some(providers) = root.get("provider").and_then(|v| v.as_object()) else {
            return out;
        };
        for (provider_id, provider) in providers {
            let Some(models) = provider
                .as_object()
                .and_then(|obj| obj.get("models"))
                .and_then(|v| v.as_object())
            else {
                continue;
            };
            for (model_id, model) in models {
                let variants = model
                    .as_object()
                    .and_then(|obj| obj.get("variants"))
                    .and_then(|v| v.as_object());
                if let Some(variants) = variants {
                    let mut efforts: Vec<String> = variants.keys().cloned().collect();
                    efforts.sort();
                    out.insert(format!("{provider_id}/{model_id}"), efforts.join(","));
                } else {
                    let has_default_effort = model
                        .get("options")
                        .and_then(|options| json_str(options, "reasoningEffort"))
                        .is_some_and(|effort| !effort.trim().is_empty());
                    // 不把 options.reasoningEffort 变成 variants；但完全未设 effort 的
                    // 模型必须保留空覆盖，不能在 roundtrip 时继承其他模型推导的全局值。
                    if !has_default_effort {
                        out.insert(format!("{provider_id}/{model_id}"), String::new());
                    }
                }
            }
        }
        out
    }
}

impl ToolAdapter for OpencodeAdapter {
    fn id(&self) -> ToolId {
        ToolId::Opencode
    }

    fn config_files(&self, home: &Path) -> Vec<(String, String, PathBuf)> {
        vec![("config".into(), "opencode.json".into(), config_path(home))]
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
                    name: json_str(&Value::Object(obj.clone()), "name")
                        .unwrap_or_else(|| id.clone()),
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
        // opencode.json 无全局思考级别字段：全局仅在保存时作为下发输入扇出到各模型，
        // 读回恒定未设置（磁盘档位由各模型的 options.reasoningEffort 逐模型承载）。
        let default_effort = String::new();
        snap.models = Self::collect_models(&root);
        snap.defaults = DefaultsSection {
            model: json_str(&Value::Object(root.clone()), "model").unwrap_or_default(),
            provider: String::new(),
            reasoning_effort: default_effort.clone(),
            reasoning_effort_options: effort_options(EFFORTS),
            per_model_effort: Self::collect_efforts(&root),
        };
        snap.thinking = ThinkingSection {
            effort_level: default_effort,
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
        let default_effort = patch.defaults.reasoning_effort.trim();
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
                // 档位键 = 供应商/模型（与 per_model_effort 写入口径一致）。
                // provider 前缀已在 modelIdForPatch 里处理，这里只剥一次供应商标识。
                let effort_key = format!("{}/{}", provider.id, short);
                let configured_efforts = patch.defaults.per_model_effort.get(&effort_key);
                let variants: Map<String, Value> = configured_efforts
                    .into_iter()
                    .flat_map(|efforts| efforts.split(','))
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|e| (e.to_string(), json!({ "reasoningEffort": e })))
                    .collect();
                // Some(空值) 明确禁用 effort 档位；只有未提供模型设置时才沿用旧的全局 fallback。
                // 非空档位也不能注入列表以外的默认值；这不代表禁用模型本身的思考能力。
                //
                // 默认思考级别的来源：**逐模型值优先**（目录给该模型「最高可配置档位」），
                // 其次才是全局默认。逐模型值同样受「必须在显式档位列表内」约束。
                let per_model_default = model.reasoning_effort.trim();
                let selected_effort = if per_model_default.is_empty() {
                    default_effort
                } else {
                    per_model_default
                };
                let has_default_effort = !selected_effort.is_empty()
                    && (configured_efforts.is_none() || variants.contains_key(selected_effort));

                // OpenCode 内核硬编码约束：模型未声明 reasoning: true 时，
                // 会判定为非推理模型并将 reasoningEffort 作为非法特性剥离。
                if !variants.is_empty() || has_default_effort {
                    model_obj.insert("reasoning".into(), json!(true));
                }

                // 注入默认思考级别到 options.reasoningEffort：
                // OpenCode 会话初始默认处于无变体状态，写入 options 可确保免手动按快捷键直接生效。
                if has_default_effort {
                    let mut model_options = Map::new();
                    model_options.insert(
                        "reasoningEffort".into(),
                        Value::String(selected_effort.to_string()),
                    );
                    model_obj.insert("options".into(), Value::Object(model_options));
                }

                if !variants.is_empty() {
                    model_obj.insert("variants".into(), Value::Object(variants));
                }
                models_new.insert(short.to_string(), Value::Object(model_obj));
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

        let text =
            serde_json::to_string_pretty(&Value::Object(root)).map_err(|e| e.to_string())? + "\n";
        atomic_write_stamped(&path, &text)?;
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
        assert_eq!(
            snap.defaults
                .per_model_effort
                .get("p1/m1")
                .map(String::as_str),
            Some("low,max"),
            "已配置的 variants 必须回读，否则界面显示为空且保存时会清掉"
        );

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
                reasoning_effort: "max".into(),
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
        assert_eq!(
            out["provider"]["p1"]["models"]["m1"]["limit"]["context"], 100_000,
            "用户供应商不得被改写"
        );
        assert_eq!(
            out["provider"]["openhub-site_a_acc_0"]["options"]["baseURL"],
            "http://127.0.0.1:17896/v1"
        );
        assert_eq!(
            out["provider"]["openhub-site_a_acc_0"]["models"]["alias/m1"]["limit"]["context"],
            500_000
        );
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

    #[test]
    fn apply_and_snapshot_preserves_reasoning_and_options_effort() {
        let home = temp_home();
        let path = config_path(&home);
        let adapter = OpencodeAdapter;

        let mut per_model_effort = std::collections::BTreeMap::new();
        per_model_effort.insert("openhub-site_a_acc_0/m_reason".into(), "low,high".into());

        let patch = ToolConfigPatch {
            base_hash: "".into(),
            providers: vec![ProviderEntry {
                id: "openhub-site_a_acc_0".into(),
                name: "站点 A".into(),
                base_url: "http://127.0.0.1:17896/v1".into(),
                api_key: "sk-openhub-test".into(),
                protocol: "@ai-sdk/openai-compatible".into(),
                models: vec!["m_reason".into(), "m_plain".into()],
            }],
            models: vec![
                ModelEntry {
                    id: "m_reason".into(),
                    name: "推理模型".into(),
                    provider: "openhub-site_a_acc_0".into(),
                    context_window: 128_000,
                    max_output: 8_192,
                    reasoning_effort: String::new(),
                },
                ModelEntry {
                    id: "m_plain".into(),
                    name: "普通模型".into(),
                    provider: "openhub-site_a_acc_0".into(),
                    context_window: 128_000,
                    max_output: 8_192,
                    reasoning_effort: String::new(),
                },
            ],
            defaults: DefaultsSection {
                model: "openhub-site_a_acc_0/m_reason".into(),
                reasoning_effort: "high".into(),
                per_model_effort,
                ..Default::default()
            },
            context: Default::default(),
            thinking: ThinkingSection {
                effort_level: "high".into(),
                ..Default::default()
            },
        };

        adapter.apply(&home, &patch).unwrap();
        let out: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let models = &out["provider"]["openhub-site_a_acc_0"]["models"];

        // 默认档位在模型 variants 中时，写入 reasoning: true 与 options.reasoningEffort。
        assert_eq!(models["m_reason"]["reasoning"], true);
        assert_eq!(models["m_reason"]["options"]["reasoningEffort"], "high");
        assert!(models["m_reason"]["variants"]["low"]["reasoningEffort"] == "low");
        assert!(models["m_reason"]["variants"]["high"]["reasoningEffort"] == "high");

        // 未提供模型映射时保留旧的全局 fallback；与显式空值不同。
        assert_eq!(models["m_plain"]["reasoning"], true);
        assert_eq!(models["m_plain"]["options"]["reasoningEffort"], "high");
        assert!(models["m_plain"].get("variants").is_none());

        // snapshot 回读验证
        let snap = adapter.snapshot(&home).unwrap();
        // opencode.json 无全局思考级别字段：读回恒定未设置，真实档位逐模型承载。
        assert_eq!(snap.defaults.reasoning_effort, "");
        assert_eq!(snap.thinking.effort_level, "");
        assert_eq!(
            snap.defaults
                .per_model_effort
                .get("openhub-site_a_acc_0/m_reason")
                .map(String::as_str),
            Some("high,low") // sorted
        );
        // 只有默认 effort 的模型不应被倒推成 variants，也不能读成显式禁用。
        assert!(!snap
            .defaults
            .per_model_effort
            .contains_key("openhub-site_a_acc_0/m_plain"));
        assert_eq!(
            snap.models
                .iter()
                .find(|model| model.id == "openhub-site_a_acc_0/m_plain")
                .unwrap()
                .reasoning_effort,
            "high"
        );
        adapter
            .apply(
                &home,
                &ToolConfigPatch {
                    providers: snap.providers,
                    models: snap.models,
                    defaults: snap.defaults,
                    ..Default::default()
                },
            )
            .unwrap();
        let roundtrip: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(roundtrip["provider"], out["provider"]);

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn explicit_empty_efforts_clear_variants_and_roundtrip_without_global_inheritance() {
        let home = temp_home();
        let path = config_path(&home);
        let fixture = json!({
            "instructions": ["keep"],
            "model": "openhub-site_a_acc_0/alias/reason",
            "provider": {
                "openhub-site_a_acc_0": {
                    "name": "站点 A",
                    "npm": "@ai-sdk/openai-compatible",
                    "options": {"baseURL": "https://example.invalid/v1", "customOption": "rebuilt"},
                    "customProvider": "rebuilt",
                    "models": {
                        "alias/reason": {
                            "name": "Reason | display name",
                            "limit": {"context": 128000, "output": 8192},
                            "reasoning": true,
                            "options": {"reasoningEffort": "high"},
                            "variants": {"low": {"reasoningEffort": "low"}, "high": {"reasoningEffort": "high"}}
                        },
                        "clear": {
                            "name": "Clear efforts",
                            "limit": {"context": 64000, "output": 4096},
                            "reasoning": true,
                            "options": {"reasoningEffort": "high"},
                            "variants": {"high": {"reasoningEffort": "high"}},
                            "customModel": "rebuilt"
                        },
                        "empty": {"name": "Already empty", "variants": {}},
                        "plain": {"name": "Plain", "limit": {"context": 32000, "output": 2048}},
                        "restricted": {
                            "name": "Low only",
                            "limit": {"context": 16000, "output": 1024},
                            "variants": {"low": {"reasoningEffort": "low"}}
                        }
                    }
                },
                "personal": {"customProvider": "keep", "models": {"untouched": {"customModel": "keep"}}}
            }
        });
        let adapter = OpencodeAdapter;
        // 空串、纯空白和只含分隔符的档位都不得继承全局 high。
        for empty in ["", " \t\n ", " , , "] {
            std::fs::write(&path, serde_json::to_string_pretty(&fixture).unwrap()).unwrap();
            let snap = adapter.snapshot(&home).unwrap();
            let expected_models = snap.models.clone();
            // 文件无全局档位字段；fixture 中 alias/reason 的 options 只回读到逐模型。
            assert_eq!(snap.defaults.reasoning_effort, "");
            assert_eq!(
                snap.defaults.per_model_effort["openhub-site_a_acc_0/clear"],
                "high"
            );
            assert_eq!(
                snap.defaults.per_model_effort["openhub-site_a_acc_0/empty"],
                ""
            );
            assert_eq!(
                snap.defaults.per_model_effort["openhub-site_a_acc_0/plain"],
                ""
            );

            let mut patch = ToolConfigPatch {
                providers: snap.providers,
                models: snap.models,
                defaults: snap.defaults,
                ..Default::default()
            };
            patch.defaults.reasoning_effort = " high ".into();
            patch
                .defaults
                .per_model_effort
                .insert("openhub-site_a_acc_0/clear".into(), empty.into());
            adapter.apply(&home, &patch).unwrap();

            let out: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let provider = &out["provider"]["openhub-site_a_acc_0"];
            let models = &provider["models"];
            for id in ["clear", "empty", "plain"] {
                assert!(models[id].get("variants").is_none(), "{empty:?}: {id}");
                assert!(models[id].get("options").is_none(), "{empty:?}: {id}");
                assert!(models[id].get("reasoning").is_none(), "{empty:?}: {id}");
            }
            assert_eq!(models["alias/reason"]["reasoning"], true);
            assert_eq!(models["alias/reason"]["options"]["reasoningEffort"], "high");
            assert_eq!(
                models["alias/reason"]["variants"],
                fixture["provider"]["openhub-site_a_acc_0"]["models"]["alias/reason"]["variants"]
            );
            // 有档位仍声明推理，但全局 high 不得越过仅支持 low 的模型设置。
            assert_eq!(models["restricted"]["reasoning"], true);
            assert!(models["restricted"].get("options").is_none());
            assert_eq!(
                models["restricted"]["variants"],
                json!({"low": {"reasoningEffort": "low"}})
            );
            for id in ["alias/reason", "clear", "plain", "restricted"] {
                assert_eq!(
                    models[id]["limit"],
                    fixture["provider"]["openhub-site_a_acc_0"]["models"][id]["limit"]
                );
            }
            // 沿用既有重建边界：受管供应商/模型扩展不保留，顶层和用户供应商不动。
            assert!(provider.get("customProvider").is_none());
            assert!(provider["options"].get("customOption").is_none());
            assert!(models["clear"].get("customModel").is_none());
            assert_eq!(out["instructions"], fixture["instructions"]);
            assert_eq!(out["provider"]["personal"], fixture["provider"]["personal"]);

            let reread = adapter.snapshot(&home).unwrap();
            // ⚠️ 显式清空档位的模型，其逐模型默认档也必须一起消失：否则会留下
            // 档位列表之外的非法 effort（OpenCode 只认变体列表内的默认值）。
            let mut expected_after_apply = expected_models.clone();
            for model in expected_after_apply
                .iter_mut()
                .filter(|m| m.id.ends_with("/clear"))
            {
                model.reasoning_effort.clear();
            }
            assert_eq!(reread.models, expected_after_apply);
            assert_eq!(reread.defaults.reasoning_effort, "");
            assert_eq!(reread.thinking.effort_level, "");
            let mut expected_efforts = patch.defaults.per_model_effort;
            expected_efforts.insert("openhub-site_a_acc_0/clear".into(), String::new());
            assert_eq!(reread.defaults.per_model_effort, expected_efforts);
            adapter
                .apply(
                    &home,
                    &ToolConfigPatch {
                        providers: reread.providers,
                        models: reread.models,
                        defaults: reread.defaults,
                        ..Default::default()
                    },
                )
                .unwrap();
            let roundtrip: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(roundtrip, out);
            let snap = adapter.snapshot(&home).unwrap();
            // roundtrip 读回：clear 的逐模型默认档已在 apply 时被清空，不能再期待 fixture 原值。
            assert_eq!(snap.models, expected_after_apply);
            assert_eq!(snap.defaults.per_model_effort, expected_efforts);
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn per_model_default_effort_lands_in_options_and_reads_back() {
        let home = temp_home();
        let path = config_path(&home);
        let adapter = OpencodeAdapter;
        let patch = ToolConfigPatch {
            providers: vec![ProviderEntry {
                id: "openhub-site_a_acc_0".into(),
                name: "站点 A".into(),
                base_url: "http://127.0.0.1:17896/v1".into(),
                api_key: "sk-openhub-test".into(),
                protocol: "@ai-sdk/openai-compatible".into(),
                models: vec!["alias/m1".into(), "m_global".into()],
            }],
            models: vec![
                ModelEntry {
                    id: "alias/m1".into(),
                    name: "m1".into(),
                    provider: "openhub-site_a_acc_0".into(),
                    context_window: 128_000,
                    max_output: 8_192,
                    // 目录给该模型「最高可配置档位」。
                    reasoning_effort: "max".into(),
                },
                ModelEntry {
                    id: "m_global".into(),
                    name: "m_global".into(),
                    provider: "openhub-site_a_acc_0".into(),
                    context_window: 128_000,
                    max_output: 8_192,
                    reasoning_effort: String::new(),
                },
            ],
            defaults: DefaultsSection {
                model: "openhub-site_a_acc_0/alias/m1".into(),
                provider: "openhub-site_a_acc_0".into(),
                reasoning_effort: "high".into(),
                reasoning_effort_options: vec![],
                per_model_effort: Default::default(),
            },
            context: Default::default(),
            thinking: Default::default(),
            base_hash: String::new(),
        };
        adapter.apply(&home, &patch).unwrap();

        let out: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let models = &out["provider"]["openhub-site_a_acc_0"]["models"];
        assert_eq!(models["alias/m1"]["options"]["reasoningEffort"], "max");
        assert_eq!(models["alias/m1"]["reasoning"], true);
        assert_eq!(models["m_global"]["options"]["reasoningEffort"], "high");
        assert_eq!(models["m_global"]["reasoning"], true);

        let snap = adapter.snapshot(&home).unwrap();
        let m1 = snap.models.iter().find(|m| m.id == "openhub-site_a_acc_0/alias/m1").unwrap();
        assert_eq!(m1.reasoning_effort, "max", "写入后必须能读回逐模型默认档");
        let mg = snap.models.iter().find(|m| m.id == "openhub-site_a_acc_0/m_global").unwrap();
        assert_eq!(mg.reasoning_effort, "high", "沿用全局默认的模型读回为全局值");

        // roundtrip：用读回的快照重建 patch，二次写盘不得改变内容。
        adapter
            .apply(
                &home,
                &ToolConfigPatch {
                    providers: snap.providers,
                    models: snap.models,
                    defaults: snap.defaults,
                    context: Default::default(),
                    thinking: Default::default(),
                    base_hash: snap.content_hash.clone(),
                },
            )
            .unwrap();
        let roundtrip: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(roundtrip, out, "读回再写必须与原写盘结果一致");

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn per_model_default_effort_outside_variants_is_not_injected() {
        let home = temp_home();
        let path = config_path(&home);
        let adapter = OpencodeAdapter;
        let patch = ToolConfigPatch {
            providers: vec![ProviderEntry {
                id: "openhub-site_a_acc_0".into(),
                name: "站点 A".into(),
                base_url: "http://127.0.0.1:17896/v1".into(),
                api_key: "sk-openhub-test".into(),
                protocol: "@ai-sdk/openai-compatible".into(),
                models: vec!["m1".into()],
            }],
            models: vec![ModelEntry {
                id: "m1".into(),
                name: "m1".into(),
                provider: "openhub-site_a_acc_0".into(),
                context_window: 128_000,
                max_output: 8_192,
                // 目录给了 max，但该模型显式只声明 low → 不得注入列表以外的默认值。
                reasoning_effort: "max".into(),
            }],
            defaults: DefaultsSection {
                model: "openhub-site_a_acc_0/m1".into(),
                provider: "openhub-site_a_acc_0".into(),
                reasoning_effort: "high".into(),
                reasoning_effort_options: vec![],
                per_model_effort: std::collections::BTreeMap::from([(
                    "openhub-site_a_acc_0/m1".to_string(),
                    "low".to_string(),
                )]),
            },
            context: Default::default(),
            thinking: Default::default(),
            base_hash: String::new(),
        };
        adapter.apply(&home, &patch).unwrap();

        let out: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let m1 = &out["provider"]["openhub-site_a_acc_0"]["models"]["m1"];
        assert_eq!(m1["variants"], json!({"low": {"reasoningEffort": "low"}}));
        assert!(m1.get("options").is_none(), "逐模型默认档不在档位列表内时不得注入");
        assert_eq!(m1["reasoning"], true, "有档位列表仍需声明 reasoning");

        let _ = std::fs::remove_dir_all(&home);
    }
}
