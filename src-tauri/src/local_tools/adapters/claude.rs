//! Claude Code：`~/.claude/settings.json`。
//!
//! 管辖字段（其余一律不动）：
//! - 供应商：`env.ANTHROPIC_BASE_URL` / `env.ANTHROPIC_AUTH_TOKEN`
//! - 模型：顶层 `model` + `env.ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU}_MODEL`
//! - 上下文：`env.CLAUDE_CODE_MAX_OUTPUT_TOKENS`
//! - 思考：顶层 `effortLevel` + `env.MAX_THINKING_TOKENS`

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use super::{content_hash, effort_options, json_str, json_u64, snapshot_skeleton, ToolAdapter};
use crate::local_tools::fsutil::{atomic_write_stamped, read_text};
use crate::local_tools::mark::{
    CLAUDE_ENV_MARK, FINGERPRINT_PLACEHOLDER, JSON_MARK_KEY, MANAGER_VALUE,
};
use crate::local_tools::types::{
    ContextSection, DefaultsSection, ModelEntry, ProviderEntry, ThinkingSection, ToolConfigPatch,
    ToolConfigSnapshot, ToolId,
};

pub(crate) struct ClaudeAdapter;

/// Claude 官方档位；实际允许任意字符串，UI 下拉给常见档。
const EFFORT_LEVELS: &[&str] = &["low", "medium", "high", "max"];

const ANTHROPIC_ENV_BASE_URL: &str = "ANTHROPIC_BASE_URL";
const ANTHROPIC_ENV_AUTH_TOKEN: &str = "ANTHROPIC_AUTH_TOKEN";
const DEFAULT_MODEL_ENVS: &[&str] = &[
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
];

fn settings_path(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json")
}

impl ClaudeAdapter {
    /// 解析 settings.json；文件不存在返回 None。
    fn parse(path: &Path) -> Result<Option<Value>, String> {
        let Some(text) = read_text(path)? else {
            return Ok(None);
        };
        if text.trim().is_empty() {
            return Ok(None);
        }
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|error| format!("settings.json 解析失败：{error}"))
    }

    fn env_of(root: &Value) -> Map<String, Value> {
        root.get("env")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default()
    }
}

impl ToolAdapter for ClaudeAdapter {
    fn id(&self) -> ToolId {
        ToolId::Claude
    }

    fn config_files(&self, home: &Path) -> Vec<(String, String, PathBuf)> {
        vec![(
            "config".to_string(),
            "settings.json".to_string(),
            settings_path(home),
        )]
    }

    fn detect(&self, home: &Path) -> (bool, String) {
        let root = home.join(".claude");
        (
            root.is_dir() || settings_path(home).is_file(),
            root.display().to_string(),
        )
    }

    fn effect_note(&self) -> &'static str {
        "重启 Claude Code 会话后生效"
    }

    fn snapshot(&self, home: &Path) -> Result<ToolConfigSnapshot, String> {
        let path = settings_path(home);
        let mut snap = snapshot_skeleton(self, home);
        let Some(root) = Self::parse(&path)? else {
            snap.warning = "尚未生成 settings.json（首次运行 Claude Code 后会创建）".into();
            return Ok(snap);
        };
        let env = Self::env_of(&root);

        // 供应商：BASE_URL + AUTH_TOKEN 合并为一个逻辑供应商条目。
        let base_url =
            json_str(&Value::Object(env.clone()), ANTHROPIC_ENV_BASE_URL).unwrap_or_default();
        let auth =
            json_str(&Value::Object(env.clone()), ANTHROPIC_ENV_AUTH_TOKEN).unwrap_or_default();
        let managed_provider = root
            .get(JSON_MARK_KEY)
            .and_then(|v| v.get("provider"))
            .and_then(|v| v.as_str())
            .filter(|id| !id.is_empty())
            .unwrap_or("anthropic")
            .to_string();
        let mapped_models: Vec<String> = DEFAULT_MODEL_ENVS
            .iter()
            .filter_map(|key| json_str(&Value::Object(env.clone()), key))
            .collect();
        snap.providers.push(ProviderEntry {
            id: managed_provider.clone(),
            name: "Anthropic API（当前接入）".into(),
            base_url,
            api_key: auth,
            protocol: "anthropic".into(),
            models: mapped_models,
        });

        // 模型：顶层 model + DEFAULT_*_MODEL 三档映射。
        let top_model = json_str(&root, "model").unwrap_or_default();
        let mut per_model_effort = std::collections::BTreeMap::new();
        for key in DEFAULT_MODEL_ENVS {
            let tier = key
                .trim_start_matches("ANTHROPIC_DEFAULT_")
                .trim_end_matches("_MODEL")
                .to_lowercase();
            let Some(value) = json_str(&Value::Object(env.clone()), key) else {
                continue;
            };
            per_model_effort.insert(tier.clone(), value.clone());
            snap.models.push(ModelEntry {
                id: value,
                name: format!("{tier} 档映射"),
                provider: managed_provider.clone(),
                ..Default::default()
            });
        }

        snap.defaults = DefaultsSection {
            model: top_model,
            provider: managed_provider,
            reasoning_effort: String::new(),
            reasoning_effort_options: Vec::new(),
            per_model_effort,
        };
        snap.context = ContextSection {
            context_window: None,
            auto_compact_token_limit: None,
            max_output_tokens: json_u64(
                &Value::Object(env.clone()),
                "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
            ),
            max_thinking_tokens: None,
        };
        snap.thinking = ThinkingSection {
            effort_level: json_str(&root, "effortLevel").unwrap_or_default(),
            effort_level_options: effort_options(EFFORT_LEVELS),
            max_thinking_tokens: json_u64(&Value::Object(env), "MAX_THINKING_TOKENS"),
        };
        snap.content_hash = content_hash(&[read_text(&path)?.unwrap_or_default()]);
        Ok(snap)
    }

    fn apply(&self, home: &Path, patch: &ToolConfigPatch) -> Result<Vec<String>, String> {
        let path = settings_path(home);
        let mut root = Self::parse(&path)?.unwrap_or_else(|| json!({}));
        let obj = root
            .as_object_mut()
            .ok_or("settings.json 顶层不是对象，已中止写入")?;

        // —— env 白名单键统一收集后一次写入，避免与顶层字段交叉借用 ——
        let mut env_updates: Vec<(String, Value)> = Vec::new();
        let mut env_removals: Vec<&'static str> = Vec::new();

        // 供应商。
        if let Some(provider) = patch.providers.first() {
            if provider.base_url.is_empty() {
                env_removals.push(ANTHROPIC_ENV_BASE_URL);
                env_removals.push(ANTHROPIC_ENV_AUTH_TOKEN);
            } else {
                env_updates.push((
                    ANTHROPIC_ENV_BASE_URL.to_string(),
                    Value::String(provider.base_url.clone()),
                ));
                if provider.api_key.is_empty() {
                    env_removals.push(ANTHROPIC_ENV_AUTH_TOKEN);
                } else {
                    env_updates.push((
                        ANTHROPIC_ENV_AUTH_TOKEN.to_string(),
                        Value::String(provider.api_key.clone()),
                    ));
                }
            }
        }

        // 三档模型映射：defaults.per_model_effort 复用为「档位 → 模型 ID」。
        // 未给出的档位保持原值，避免生效时把用户已有 opus/sonnet/haiku 映射冲掉。
        for key in DEFAULT_MODEL_ENVS {
            let tier = key
                .trim_start_matches("ANTHROPIC_DEFAULT_")
                .trim_end_matches("_MODEL")
                .to_lowercase();
            if let Some(value) = patch.defaults.per_model_effort.get(&tier) {
                if value.is_empty() {
                    env_removals.push(*key);
                } else {
                    env_updates.push(((*key).to_string(), Value::String(value.clone())));
                }
            }
        }

        // 上下文与思考（env 部分）。
        match patch.context.max_output_tokens {
            Some(value) => env_updates.push((
                "CLAUDE_CODE_MAX_OUTPUT_TOKENS".to_string(),
                Value::String(value.to_string()),
            )),
            None => env_removals.push("CLAUDE_CODE_MAX_OUTPUT_TOKENS"),
        }
        match patch.thinking.max_thinking_tokens {
            Some(value) => env_updates.push((
                "MAX_THINKING_TOKENS".to_string(),
                Value::String(value.to_string()),
            )),
            None => env_removals.push("MAX_THINKING_TOKENS"),
        }

        // 应用 env 变更。
        {
            let env = obj
                .entry("env")
                .or_insert_with(|| Value::Object(Map::new()));
            let env_obj = env
                .as_object_mut()
                .ok_or("settings.json 的 env 不是对象，已中止写入")?;
            for (key, value) in env_updates {
                env_obj.insert(key, value);
            }
            for key in env_removals {
                env_obj.remove(key);
            }
        }

        // —— 顶层字段 ——
        if patch.defaults.model.is_empty() {
            obj.remove("model");
        } else {
            let value = Value::String(patch.defaults.model.clone());
            obj.insert("model".into(), value);
        }
        if patch.thinking.effort_level.is_empty() {
            obj.remove("effortLevel");
        } else {
            let value = Value::String(patch.thinking.effort_level.clone());
            obj.insert("effortLevel".into(), value);
        }

        let mut mark = serde_json::Map::new();
        mark.insert("managed".into(), json!(true));
        mark.insert("manager".into(), json!(MANAGER_VALUE));
        // 内容指纹：落盘时由 atomic_write_stamped 盖成真实值，下次保存据此判断有没有被外部改过
        mark.insert("fingerprint".into(), json!(FINGERPRINT_PLACEHOLDER));
        if let Some(provider) = patch.providers.first().filter(|p| !p.id.is_empty()) {
            mark.insert("provider".into(), json!(provider.id));
        }
        obj.insert(JSON_MARK_KEY.into(), Value::Object(mark));
        {
            let env = obj
                .entry("env")
                .or_insert_with(|| Value::Object(Map::new()));
            let env_obj = env
                .as_object_mut()
                .ok_or("settings.json 的 env 不是对象，已中止写入")?;
            env_obj.insert(CLAUDE_ENV_MARK.into(), Value::String(MANAGER_VALUE.into()));
        }

        let text = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())? + "\n";
        atomic_write_stamped(&path, &text)?;
        Ok(vec!["settings.json".to_string()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "openhub-lt-claude-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        dir
    }

    #[test]
    fn snapshot_missing_file_yields_warning() {
        let home = temp_home();
        let adapter = ClaudeAdapter;
        let snap = adapter.snapshot(&home).unwrap();
        assert!(!snap.warning.is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn apply_preserves_unknown_fields_and_is_idempotent() {
        let home = temp_home();
        let path = settings_path(&home);
        std::fs::write(
            &path,
            r#"{
  "permissions": {"allow": ["Bash"]},
  "env": {"ANTHROPIC_BASE_URL": "http://old:1", "OTHER_KEEP": "x"},
  "model": "opus"
}"#,
        )
        .unwrap();

        let adapter = ClaudeAdapter;
        let mut snap = adapter.snapshot(&home).unwrap();
        snap.providers[0].base_url = "http://127.0.0.1:17896/v1".into();
        snap.providers[0].api_key = "sk-test".into();
        snap.defaults.model = "opus[1m]".into();
        snap.defaults.per_model_effort = super::super::map_from(&[
            ("opus", "claude-opus-5"),
            ("sonnet", "claude-sonnet-5"),
            ("haiku", "claude-haiku-5"),
        ]);
        snap.context.max_output_tokens = Some(131072);
        snap.thinking.effort_level = "max".into();
        snap.thinking.max_thinking_tokens = Some(2048);

        let patch = ToolConfigPatch {
            base_hash: snap.content_hash.clone(),
            providers: snap.providers.clone(),
            models: vec![],
            defaults: snap.defaults.clone(),
            context: snap.context.clone(),
            thinking: snap.thinking.clone(),
        };
        let written = adapter.apply(&home, &patch).unwrap();
        assert_eq!(written, vec!["settings.json"]);

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("permissions"), "未知字段必须保留：{text}");
        assert!(text.contains("OTHER_KEEP"));
        assert!(
            text.contains("\"x-openhub\""),
            "必须写入 OpenHub 标识：{text}"
        );
        assert!(text.contains("OPENHUB_MANAGED"), "必须写入环境标识：{text}");

        // 再快照 + 再应用一次：幂等。
        let snap2 = adapter.snapshot(&home).unwrap();
        assert_eq!(snap2.providers[0].base_url, "http://127.0.0.1:17896/v1");
        assert_eq!(snap2.defaults.model, "opus[1m]");
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
        assert_eq!(snap3.defaults.model, snap2.defaults.model);

        let _ = std::fs::remove_dir_all(&home);
    }
}
