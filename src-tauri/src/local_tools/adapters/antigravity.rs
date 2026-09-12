//! Google Antigravity / Gemini CLI：`~/.gemini/.env` + `~/.gemini/settings.json`。
//!
//! 管辖字段：
//! - `.env`：`GEMINI_API_KEY` / `GOOGLE_GEMINI_BASE_URL`（逐行替换，保留注释与顺序）
//! - `settings.json`：`model`（默认模型）
//!
//! 其余 env 键与 settings 键一律不动。

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::{content_hash, json_str, snapshot_skeleton, ToolAdapter};
use crate::local_tools::fsutil::{atomic_write, read_text};
use crate::local_tools::types::{
    DefaultsSection, ProviderEntry, ToolConfigPatch, ToolConfigSnapshot, ToolId,
};

pub(crate) struct AntigravityAdapter;

fn gemini_home(home: &Path) -> PathBuf {
    std::env::var_os("GEMINI_CLI_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".gemini"))
}

fn env_path(home: &Path) -> PathBuf {
    gemini_home(home).join(".env")
}

fn settings_path(home: &Path) -> PathBuf {
    gemini_home(home).join("settings.json")
}

/// 逐行替换 .env 的指定 KEY=VALUE；KEY 不存在时追加到末尾。
/// 注释、空行、未知键、顺序全部保留；原文以换行结尾则输出同样以换行结尾。
fn upsert_env_lines(text: &str, pairs: &[(&str, String)]) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    for (key, value) in pairs {
        let mut replaced = false;
        for line in &mut lines {
            let trimmed = line.trim_start();
            if trimmed.starts_with(key) && trimmed[key.len()..].trim_start().starts_with('=') {
                *line = format!("{key}={value}");
                replaced = true;
            }
        }
        if !replaced {
            lines.push(format!("{key}={value}"));
        }
    }
    let mut out = lines.join("\n");
    if text.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    out
}

impl AntigravityAdapter {
    fn parse_settings(path: &Path) -> Result<Option<Map<String, Value>>, String> {
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
            .ok_or_else(|| "settings.json 顶层不是对象，已跳过编辑".to_string())
    }

    /// 解析 .env 的 KEY=VALUE（忽略注释）。
    fn parse_env(text: &str) -> Vec<(String, String)> {
        text.lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    return None;
                }
                trimmed
                    .split_once('=')
                    .map(|(k, v)| (k.trim().to_string(), v.to_string()))
            })
            .collect()
    }
}

impl ToolAdapter for AntigravityAdapter {
    fn id(&self) -> ToolId {
        ToolId::Antigravity
    }

    fn config_files(&self, home: &Path) -> Vec<(String, String, PathBuf)> {
        vec![
            ("env".into(), ".env".into(), env_path(home)),
            ("config".into(), "settings.json".into(), settings_path(home)),
        ]
    }

    fn detect(&self, home: &Path) -> (bool, String) {
        let root = gemini_home(home);
        (root.is_dir(), root.display().to_string())
    }

    fn effect_note(&self) -> &'static str {
        "重启 Gemini CLI / Antigravity 会话后生效"
    }

    fn snapshot(&self, home: &Path) -> Result<ToolConfigSnapshot, String> {
        let mut snap = snapshot_skeleton(self, home);
        let env_text = read_text(&env_path(home))?.unwrap_or_default();
        let settings = Self::parse_settings(&settings_path(home))?;

        let pairs = Self::parse_env(&env_text);
        let find = |key: &str| {
            pairs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        let base_url = find("GOOGLE_GEMINI_BASE_URL");
        let api_key = find("GEMINI_API_KEY");

        let model = settings
            .as_ref()
            .and_then(|s| json_str(&Value::Object(s.clone()), "model"))
            .unwrap_or_default();

        snap.providers.push(ProviderEntry {
            id: "gemini".into(),
            name: "Gemini API（当前接入）".into(),
            base_url,
            api_key,
            protocol: "gemini".into(),
            models: Vec::new(),
        });
        snap.defaults = DefaultsSection {
            model,
            provider: "gemini".into(),
            ..Default::default()
        };
        snap.content_hash = content_hash(&[
            env_text,
            read_text(&settings_path(home))?.unwrap_or_default(),
        ]);
        Ok(snap)
    }

    fn apply(&self, home: &Path, patch: &ToolConfigPatch) -> Result<Vec<String>, String> {
        let mut written = Vec::new();

        // .env：供应商。
        if let Some(provider) = patch.providers.first() {
            let path = env_path(home);
            let text = read_text(&path)?.unwrap_or_default();
            let mut pairs: Vec<(&str, String)> = Vec::new();
            if provider.base_url.is_empty() {
                pairs.push(("GOOGLE_GEMINI_BASE_URL", String::new()));
            } else {
                pairs.push(("GOOGLE_GEMINI_BASE_URL", provider.base_url.clone()));
            }
            pairs.push(("GEMINI_API_KEY", provider.api_key.clone()));
            let new_text = upsert_env_lines(&text, &pairs);
            atomic_write(&path, &new_text)?;
            written.push(".env".to_string());
        }

        // settings.json：默认模型。
        let path = settings_path(home);
        let mut root = Self::parse_settings(&path)?.unwrap_or_default();
        if patch.defaults.model.is_empty() {
            root.remove("model");
        } else {
            root.insert("model".into(), Value::String(patch.defaults.model.clone()));
        }
        let text =
            serde_json::to_string_pretty(&Value::Object(root)).map_err(|e| e.to_string())? + "\n";
        atomic_write(&path, &text)?;
        written.push("settings.json".to_string());
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("openhub-lt-ag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".gemini")).unwrap();
        dir
    }

    #[test]
    fn env_upsert_preserves_comments_and_order() {
        let text = "# 注释\nGEMINI_API_KEY=old\nOTHER=x\n";
        let out = upsert_env_lines(text, &[("GEMINI_API_KEY", "new".into())]);
        assert_eq!(out, "# 注释\nGEMINI_API_KEY=new\nOTHER=x\n");

        let out2 = upsert_env_lines(
            "# 注释\nOTHER=x\n",
            &[("GOOGLE_GEMINI_BASE_URL", "http://g/v1".into())],
        );
        assert_eq!(
            out2,
            "# 注释\nOTHER=x\nGOOGLE_GEMINI_BASE_URL=http://g/v1\n"
        );
    }

    #[test]
    fn apply_writes_env_and_settings() {
        let home = temp_home();
        std::fs::write(env_path(&home), "# my env\nGEMINI_API_KEY=old\n").unwrap();
        std::fs::write(settings_path(&home), r#"{"theme": "dark"}"#).unwrap();

        let adapter = AntigravityAdapter;
        let snap = adapter.snapshot(&home).unwrap();
        assert_eq!(snap.providers[0].api_key, "old");

        let patch = ToolConfigPatch {
            base_hash: snap.content_hash.clone(),
            providers: vec![ProviderEntry {
                id: "gemini".into(),
                name: "gemini".into(),
                base_url: "http://127.0.0.1:17896/v1beta".into(),
                api_key: "sk-new".into(),
                protocol: "gemini".into(),
                models: vec![],
            }],
            models: vec![],
            defaults: DefaultsSection {
                model: "gemini-2.5-pro".into(),
                provider: "gemini".into(),
                ..Default::default()
            },
            context: Default::default(),
            thinking: Default::default(),
        };
        let written = adapter.apply(&home, &patch).unwrap();
        assert!(written.contains(&".env".to_string()));
        assert!(written.contains(&"settings.json".to_string()));

        let env_text = std::fs::read_to_string(env_path(&home)).unwrap();
        assert!(env_text.contains("# my env"));
        assert!(env_text.contains("GEMINI_API_KEY=sk-new"));
        assert!(env_text.contains("GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:17896/v1beta"));

        let settings: Value =
            serde_json::from_str(&std::fs::read_to_string(settings_path(&home)).unwrap()).unwrap();
        assert_eq!(settings["theme"], "dark", "theme 必须保留");
        assert_eq!(settings["model"], "gemini-2.5-pro");

        let _ = std::fs::remove_dir_all(&home);
    }
}
