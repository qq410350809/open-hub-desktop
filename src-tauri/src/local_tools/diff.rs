//! 「反代清单 ↔ 磁盘配置」的一致性比对。
//!
//! 语义：给定一份由清单组装出的 patch，回答「当前配置文件是否已经就是这份 patch 的产物」。
//!
//! 实现：把真实配置复制到临时镜像目录，用**同一个适配器**把 patch 应用到镜像，
//! 再快照镜像，与真实快照按管辖分区逐字段比较（供应商 / 模型 / 默认项 / 上下文 / 思考）。
//! 复用 `apply` + `snapshot` 而不是另写一份「预期结果推导」，保证比对口径与真实写入
//! 结果永不漂移：适配器各自的编码（claude 三档映射、dsh 的 `name|efforts`、
//! opencode 的 `provider/model`）都自动被覆盖。
//!
//! 只比较本软件管辖的条目（`openhub-` 前缀供应商及其模型）；用户自有的第三方供应商
//! 与工具的其他配置一律忽略，否则装了别的 provider 的用户会永远显示不一致。
//! API Key 存在独立凭据文件（codex `auth.json`、dsh `.credentials.yaml`）的工具，
//! 额外比对这两个文件里的受管键。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::adapters::ToolAdapter;
use super::mark::is_managed_id;
use super::types::{ProviderEntry, ToolConfigPatch, ToolConfigSnapshot};

/// 差异列表上限：超出后只回一条汇总，避免单次比对返回过长文本。
const MAX_DIFFERENCES: usize = 12;

/// 一次比对的目标：`key` 由调用方指定（行标识或整单标识），原样回传。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ToolDiffTarget {
    pub key: String,
    pub patch: ToolConfigPatch,
}

/// 单个目标的比对结果。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ToolDiffEntry {
    pub key: String,
    /// 磁盘现状与这份 patch 的预期结果一致。
    pub consistent: bool,
    /// 人类可读的差异说明（一致时为空）。
    pub differences: Vec<String>,
}

/// 比对报告：条目级结果 + 磁盘上现有的受管条目摘要（供前端做行级判定）。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ToolDiffReport {
    pub tool: String,
    pub tool_name: String,
    /// 磁盘上现有的受管供应商标识。
    pub managed_providers: Vec<String>,
    /// 磁盘上现有的受管供应商所属模型 ID。
    pub managed_models: Vec<String>,
    pub entries: Vec<ToolDiffEntry>,
}

/// 临时镜像目录：随 Drop 清理，避免比对过程留下残留。
struct Mirror {
    home: PathBuf,
}

impl Drop for Mirror {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}

/// 建立镜像：按适配器声明的配置文件列表，把真实内容复制到镜像 home 的同名相对路径下。
///
/// 落盘前校验适配器在镜像 home 下解析出的路径确实落在镜像内 —— 若某工具按环境变量
/// （如 codex 的 `CODEX_HOME`）重定向配置目录，镜像里的 `apply` 会写回真实目录，
/// 此时直接报错中止，绝不让「只读比对」产生任何磁盘副作用。
fn create_mirror(adapter: &dyn ToolAdapter, home: &Path) -> Result<Mirror, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mirror_home =
        std::env::temp_dir().join(format!("openhub-lt-diff-{}-{nanos:x}", std::process::id()));
    fs::create_dir_all(&mirror_home).map_err(|error| format!("创建比对目录失败：{error}"))?;
    let mirror = Mirror { home: mirror_home };

    for (_kind, label, real) in adapter.config_files(home) {
        let Some(relative) = real.strip_prefix(home).ok() else {
            return Err(format!(
                "{} 不在用户主目录下，无法安全比对（配置目录可能被环境变量重定向）",
                real.display()
            ));
        };
        if !real.is_file() {
            continue;
        }
        let target = mirror.home.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("创建比对目录失败：{error}"))?;
        }
        fs::copy(&real, &target)
            .map_err(|error| format!("复制 {label} 到比对目录失败：{error}"))?;
    }

    // 适配器若忽略传入的 home（环境变量重定向），apply 会写到镜像之外 —— 必须先拦住。
    for (_kind, label, path) in adapter.config_files(&mirror.home) {
        if !path.starts_with(&mirror.home) {
            return Err(format!(
                "{label} 的路径解析到了镜像之外（{}），暂不支持该环境下的配置比对",
                path.display()
            ));
        }
    }
    Ok(mirror)
}

/// 把 patch 应用到镜像，返回镜像（凭据比对还要读它）与「写入后应有的快照」。
fn apply_to_mirror(
    adapter: &dyn ToolAdapter,
    home: &Path,
    patch: &ToolConfigPatch,
) -> Result<(Mirror, ToolConfigSnapshot), String> {
    let mirror = create_mirror(adapter, home)?;
    adapter.apply(&mirror.home, patch)?;
    let snapshot = adapter.snapshot(&mirror.home)?;
    Ok((mirror, snapshot))
}

/// 受管供应商的显示名：名称为空时退回标识。
fn display_name(provider: &ProviderEntry) -> &str {
    if provider.name.trim().is_empty() {
        provider.id.as_str()
    } else {
        provider.name.as_str()
    }
}

/// 展示跳过字段用的占位（值可能为空）。
fn show(value: &str) -> &str {
    if value.trim().is_empty() {
        "（未设置）"
    } else {
        value
    }
}

/// token 数值的展示：0 表示工具不支持或未设置。
fn show_limit(value: u64) -> String {
    if value == 0 {
        "（未设置）".to_string()
    } else {
        value.to_string()
    }
}

/// 比较两份快照的管辖分区，返回人类可读差异。
fn compare_snapshots(actual: &ToolConfigSnapshot, expected: &ToolConfigSnapshot) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    // —— 供应商 ——
    let actual_providers: BTreeMap<&str, &ProviderEntry> = actual
        .providers
        .iter()
        .filter(|p| is_managed_id(&p.id))
        .map(|p| (p.id.as_str(), p))
        .collect();
    let expected_providers: BTreeMap<&str, &ProviderEntry> = expected
        .providers
        .iter()
        .filter(|p| is_managed_id(&p.id))
        .map(|p| (p.id.as_str(), p))
        .collect();

    for (id, want) in &expected_providers {
        match actual_providers.get(id) {
            None => out.push(format!("供应商「{}」未写入配置", display_name(want))),
            Some(have) => {
                if have.base_url.trim_end_matches('/') != want.base_url.trim_end_matches('/') {
                    out.push(format!(
                        "供应商「{}」的接入地址不一致：当前 {}，清单为 {}",
                        display_name(want),
                        show(&have.base_url),
                        show(&want.base_url)
                    ));
                }
                if have.api_key != want.api_key {
                    out.push(format!("供应商「{}」的 API Key 不一致", display_name(want)));
                }
                if have.protocol != want.protocol {
                    out.push(format!(
                        "供应商「{}」的协议不一致：当前 {}，清单为 {}",
                        display_name(want),
                        show(&have.protocol),
                        show(&want.protocol)
                    ));
                }
            }
        }
    }
    for (id, have) in &actual_providers {
        if !expected_providers.contains_key(id) {
            out.push(format!(
                "配置里多出供应商「{}」，清单中已不存在",
                display_name(have)
            ));
        }
    }

    // —— 模型（只比较受管供应商名下的）——
    //
    // 逐模型比较标识与 limit（上下文窗口 / 最大输出）：只比 ID 的话，
    // 改了窗口大小仍会判为一致，徽标就成了谎报。
    let managed_provider_ids: BTreeSet<&str> = expected_providers
        .keys()
        .chain(actual_providers.keys())
        .copied()
        .collect();
    let model_limits = |snapshot: &ToolConfigSnapshot| -> BTreeMap<String, (String, u64, u64)> {
        snapshot
            .models
            .iter()
            .filter(|m| managed_provider_ids.contains(m.provider.as_str()))
            .map(|m| {
                (
                    m.id.clone(),
                    (m.name.clone(), m.context_window, m.max_output),
                )
            })
            .collect()
    };
    let actual_models = model_limits(actual);
    let expected_models = model_limits(expected);
    for (id, want) in &expected_models {
        match actual_models.get(id) {
            None => out.push(format!("模型「{id}」未写入配置")),
            Some(have) => {
                if have.1 != want.1 {
                    out.push(format!(
                        "模型「{id}」的上下文窗口不一致：当前 {}，清单为 {}",
                        show_limit(have.1),
                        show_limit(want.1)
                    ));
                }
                if have.2 != want.2 {
                    out.push(format!(
                        "模型「{id}」的最大输出不一致：当前 {}，清单为 {}",
                        show_limit(have.2),
                        show_limit(want.2)
                    ));
                }
            }
        }
    }
    for id in actual_models.keys() {
        if !expected_models.contains_key(id) {
            out.push(format!("配置里多出模型「{id}」，清单中已不存在"));
        }
    }

    // —— 默认项 ——
    let have = &actual.defaults;
    let want = &expected.defaults;
    if have.model != want.model {
        out.push(format!(
            "默认模型不一致：当前 {}，清单为 {}",
            show(&have.model),
            show(&want.model)
        ));
    }
    if have.provider != want.provider {
        out.push(format!(
            "默认供应商不一致：当前 {}，清单为 {}",
            show(&have.provider),
            show(&want.provider)
        ));
    }
    if have.reasoning_effort != want.reasoning_effort {
        out.push(format!(
            "思考级别不一致：当前 {}，清单为 {}",
            show(&have.reasoning_effort),
            show(&want.reasoning_effort)
        ));
    }
    let effort_keys: BTreeSet<&String> = have
        .per_model_effort
        .keys()
        .chain(want.per_model_effort.keys())
        .collect();
    for key in effort_keys {
        let current = have
            .per_model_effort
            .get(key)
            .map(String::as_str)
            .unwrap_or("");
        let target = want
            .per_model_effort
            .get(key)
            .map(String::as_str)
            .unwrap_or("");
        if current != target {
            out.push(format!(
                "思考档映射「{key}」不一致：当前 {}，清单为 {}",
                show(current),
                show(target)
            ));
        }
    }

    // —— 上下文与思考 ——
    let context_fields: [(&str, Option<u64>, Option<u64>); 4] = [
        (
            "上下文窗口",
            actual.context.context_window,
            expected.context.context_window,
        ),
        (
            "自动压缩阈值",
            actual.context.auto_compact_token_limit,
            expected.context.auto_compact_token_limit,
        ),
        (
            "最大输出",
            actual.context.max_output_tokens,
            expected.context.max_output_tokens,
        ),
        (
            "思考 token 预算",
            actual.thinking.max_thinking_tokens,
            expected.thinking.max_thinking_tokens,
        ),
    ];
    for (label, current, target) in context_fields {
        if current != target {
            out.push(format!(
                "{label} 不一致：当前 {}，清单为 {}",
                current.map_or_else(|| "（未设置）".to_string(), |v| v.to_string()),
                target.map_or_else(|| "（未设置）".to_string(), |v| v.to_string())
            ));
        }
    }
    if actual.thinking.effort_level != expected.thinking.effort_level {
        out.push(format!(
            "思考级别（effortLevel）不一致：当前 {}，清单为 {}",
            show(&actual.thinking.effort_level),
            show(&expected.thinking.effort_level)
        ));
    }

    out
}

/// 从一个凭据文件文本里提取本软件管辖的键值对。
///
/// 支持 JSON（codex `auth.json`）与 YAML（dsh `.credentials.yaml`）两种形态：
/// - `OPENHUB_*_API_KEY`：dsh 的按供应商隔离键，前缀即标识，直接收录；
/// - `OPENAI_API_KEY`：codex 用官方约定键名，只有文件带 `x-openhub` 标识时才认；
/// - `x-openhub` 本身：值为对象，记录是否受管，供上面判断用。
fn managed_credentials(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if text.trim().is_empty() {
        return out;
    }

    let (mark_present, pairs): (bool, Vec<(String, String)>) = if let Ok(json) =
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(text)
    {
        (
            json.contains_key(super::mark::JSON_MARK_KEY),
            json.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect(),
        )
    } else if let Ok(serde_yaml::Value::Mapping(map)) = serde_yaml::from_str(text) {
        (
            map.keys()
                .any(|k| k.as_str() == Some(super::mark::YAML_MARK_KEY)),
            map.iter()
                .filter_map(|(k, v)| Some((k.as_str()?.to_string(), v.as_str()?.to_string())))
                .collect(),
        )
    } else {
        return out;
    };

    for (key, value) in pairs {
        let ours = key.starts_with("OPENHUB_") && key.ends_with("_API_KEY");
        let codex_official = key == "OPENAI_API_KEY" && mark_present;
        if ours || codex_official {
            out.insert(key, value);
        }
    }
    out
}

/// 比对凭据文件里的受管键（API Key 单独存放的工具）。
fn compare_credentials(adapter: &dyn ToolAdapter, home: &Path, mirror_home: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for (kind, label, real) in adapter.config_files(home) {
        if kind != "auth" {
            continue;
        }
        let Some(relative) = real.strip_prefix(home).ok() else {
            continue;
        };
        let have = super::fsutil::read_text(&real)
            .ok()
            .flatten()
            .unwrap_or_default();
        let want = super::fsutil::read_text(&mirror_home.join(relative))
            .ok()
            .flatten()
            .unwrap_or_default();
        let have = managed_credentials(&have);
        let want = managed_credentials(&want);
        let keys: BTreeSet<&String> = have.keys().chain(want.keys()).collect();
        for key in keys {
            if have.get(key) != want.get(key) {
                out.push(format!("{label} 里的 {key} 与清单不一致"));
            }
        }
    }
    out
}

/// 截断差异列表，超出部分折成一条汇总。
fn finalize(mut differences: Vec<String>) -> Vec<String> {
    if differences.len() > MAX_DIFFERENCES {
        let extra = differences.len() - MAX_DIFFERENCES + 1;
        differences.truncate(MAX_DIFFERENCES - 1);
        differences.push(format!("另有 {extra} 处差异…"));
    }
    differences
}

/// 对多个目标逐一比对（共用同一份真实快照）。
pub(crate) fn diff_targets(
    adapter: &dyn ToolAdapter,
    home: &Path,
    targets: &[ToolDiffTarget],
) -> Result<ToolDiffReport, String> {
    let actual = adapter.snapshot(home)?;
    let mut entries = Vec::with_capacity(targets.len());

    for target in targets {
        // 单次比对失败（配置损坏、目录被重定向等）不该让整批结果丢失。
        let differences = match apply_to_mirror(adapter, home, &target.patch) {
            Ok((mirror, expected)) => {
                let mut all = compare_snapshots(&actual, &expected);
                all.extend(compare_credentials(adapter, home, &mirror.home));
                finalize(all)
            }
            Err(error) => vec![format!("无法比对：{error}")],
        };
        entries.push(ToolDiffEntry {
            key: target.key.clone(),
            consistent: differences.is_empty(),
            differences,
        });
    }

    let managed_providers: Vec<String> = actual
        .providers
        .iter()
        .filter(|p| is_managed_id(&p.id))
        .map(|p| p.id.clone())
        .collect();
    let provider_ids: BTreeSet<&str> = managed_providers.iter().map(String::as_str).collect();
    let managed_models: Vec<String> = actual
        .models
        .iter()
        .filter(|m| provider_ids.contains(m.provider.as_str()))
        .map(|m| m.id.clone())
        .collect();

    Ok(ToolDiffReport {
        tool: actual.tool.clone(),
        tool_name: actual.tool_name.clone(),
        managed_providers,
        managed_models,
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_tools::adapters::claude::ClaudeAdapter;
    use crate::local_tools::types::{DefaultsSection, ModelEntry, ProviderEntry};

    fn temp_home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "openhub-lt-diff-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        dir
    }

    fn patch_for(home: &Path, provider: &str, model: &str) -> ToolConfigPatch {
        let snap = ClaudeAdapter.snapshot(home).unwrap();
        ToolConfigPatch {
            base_hash: snap.content_hash,
            providers: vec![ProviderEntry {
                id: provider.into(),
                name: "站点 A-账号".into(),
                base_url: "http://127.0.0.1:17896".into(),
                api_key: "sk-openhub-test".into(),
                protocol: "anthropic".into(),
                models: vec![model.into()],
            }],
            models: vec![ModelEntry {
                id: model.into(),
                name: model.into(),
                provider: provider.into(),
                ..Default::default()
            }],
            defaults: DefaultsSection {
                model: model.into(),
                provider: provider.into(),
                per_model_effort: crate::local_tools::adapters::map_from(&[("sonnet", model)]),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn consistent_after_real_apply_and_inconsistent_after_external_edit() {
        let home = temp_home("claude");
        // 用户原生配置：没有本软件标识。
        std::fs::write(home.join(".claude").join("settings.json"), "{}\n").unwrap();

        let patch = patch_for(&home, "openhub-site_a_acc_0", "alias/m1");
        let report = diff_targets(
            &ClaudeAdapter,
            &home,
            &[ToolDiffTarget {
                key: "row".into(),
                patch: patch.clone(),
            }],
        )
        .unwrap();
        assert!(!report.entries[0].consistent, "尚未写入时应判定为不一致");
        assert!(
            report.entries[0]
                .differences
                .iter()
                .any(|d| d.contains("未写入配置")),
            "{:?}",
            report.entries[0].differences
        );

        // 真正写入后应判定一致。
        ClaudeAdapter.apply(&home, &patch).unwrap();
        let report = diff_targets(
            &ClaudeAdapter,
            &home,
            &[ToolDiffTarget {
                key: "row".into(),
                patch: patch.clone(),
            }],
        )
        .unwrap();
        assert!(
            report.entries[0].consistent,
            "{:?}",
            report.entries[0].differences
        );
        assert_eq!(report.managed_providers, vec!["openhub-site_a_acc_0"]);
        assert_eq!(report.managed_models, vec!["alias/m1"]);

        // 外部改掉模型后再比对：应报出该处差异。
        let settings = home.join(".claude").join("settings.json");
        let text = std::fs::read_to_string(&settings).unwrap();
        let edited = text.replace("\"model\": \"alias/m1\"", "\"model\": \"alias/other\"");
        assert_ne!(text, edited);
        std::fs::write(&settings, edited).unwrap();
        let report = diff_targets(
            &ClaudeAdapter,
            &home,
            &[ToolDiffTarget {
                key: "row".into(),
                patch,
            }],
        )
        .unwrap();
        assert!(!report.entries[0].consistent);
        assert!(
            report.entries[0]
                .differences
                .iter()
                .any(|d| d.contains("默认模型不一致")),
            "{:?}",
            report.entries[0].differences
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn user_owned_providers_are_ignored() {
        let home = temp_home("claude-foreign");
        // 用户自己的第三方供应商（无 openhub- 前缀）不该导致不一致。
        let settings = home.join(".claude").join("settings.json");
        std::fs::write(&settings, "{}\n").unwrap();
        let patch = patch_for(&home, "openhub-site_a_acc_0", "alias/m1");
        ClaudeAdapter.apply(&home, &patch).unwrap();

        let mut root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        root["model"] = serde_json::Value::String("alias/m1".into());
        root["permissions"] = serde_json::json!({ "allow": ["Read"] });
        std::fs::write(&settings, serde_json::to_string_pretty(&root).unwrap()).unwrap();

        let report = diff_targets(
            &ClaudeAdapter,
            &home,
            &[ToolDiffTarget {
                key: "row".into(),
                patch,
            }],
        )
        .unwrap();
        assert!(
            report.entries[0].consistent,
            "无关的用户配置不应算差异：{:?}",
            report.entries[0].differences
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn diff_never_writes_to_real_config() {
        let home = temp_home("claude-readonly");
        let settings = home.join(".claude").join("settings.json");
        std::fs::write(&settings, "{\n  \"model\": \"user-model\"\n}\n").unwrap();
        let before = std::fs::read_to_string(&settings).unwrap();

        let patch = patch_for(&home, "openhub-site_a_acc_0", "alias/m1");
        let _ = diff_targets(
            &ClaudeAdapter,
            &home,
            &[ToolDiffTarget {
                key: "row".into(),
                patch,
            }],
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(&settings).unwrap(),
            before,
            "比对不得改动真实配置"
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn credentials_are_compared_for_codex_and_dsh() {
        // codex：Key 存在 auth.json 的 OPENAI_API_KEY，带受管标识时才认。
        let pairs = managed_credentials(
            r#"{"OPENAI_API_KEY":"sk-1","tokens":null,"x-openhub":{"managed":true}}"#,
        );
        assert_eq!(
            pairs.get("OPENAI_API_KEY").map(String::as_str),
            Some("sk-1")
        );
        // 没有受管标识 → 是用户自己的 Key，不算差异。
        assert!(managed_credentials(r#"{"OPENAI_API_KEY":"sk-user"}"#).is_empty());
        // dsh：按供应商隔离的 OPENHUB_*_API_KEY。
        let pairs =
            managed_credentials("OPENHUB_SITE_A_ACC_0_API_KEY: sk-2\nDEEPSEEK_API_KEY: sk-user\n");
        assert_eq!(pairs.len(), 1);
        assert_eq!(
            pairs
                .get("OPENHUB_SITE_A_ACC_0_API_KEY")
                .map(String::as_str),
            Some("sk-2")
        );
    }

    /// 逐模型参数（上下文窗口 / 最大输出 / 思考档）必须参与比对：
    /// 只比模型 ID 的话，改了窗口大小仍会判为「一致」，徽标就成了谎报。
    #[test]
    fn per_model_params_participate_in_comparison() {
        use crate::local_tools::adapters::opencode::OpencodeAdapter;

        let home = std::env::temp_dir().join(format!(
            "openhub-lt-diff-params-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".config").join("opencode")).unwrap();

        let patch = {
            let snap = OpencodeAdapter.snapshot(&home).unwrap();
            ToolConfigPatch {
                base_hash: snap.content_hash,
                providers: vec![ProviderEntry {
                    id: "openhub-gateway".into(),
                    name: "网关".into(),
                    base_url: "http://127.0.0.1:17896/v1".into(),
                    api_key: "sk-test".into(),
                    protocol: "@ai-sdk/openai-compatible".into(),
                    models: vec!["openhub-gateway/alias/m1".into()],
                }],
                models: vec![ModelEntry {
                    id: "openhub-gateway/alias/m1".into(),
                    name: "m1".into(),
                    provider: "openhub-gateway".into(),
                    context_window: 200_000,
                    max_output: 32_000,
                }],
                defaults: DefaultsSection {
                    model: "openhub-gateway/alias/m1".into(),
                    per_model_effort: crate::local_tools::adapters::map_from(&[(
                        "openhub-gateway/alias/m1",
                        "low,high",
                    )]),
                    ..Default::default()
                },
                ..Default::default()
            }
        };

        OpencodeAdapter.apply(&home, &patch).unwrap();
        let target = || ToolDiffTarget {
            key: WHOLE_KEY.into(),
            patch: patch.clone(),
        };
        let report = diff_targets(&OpencodeAdapter, &home, &[target()]).unwrap();
        assert!(
            report.entries[0].consistent,
            "刚写入应一致：{:?}",
            report.entries[0].differences
        );

        // 只改窗口大小 → 必须报出差异（旧实现只比 ID，会漏掉）。
        let mut changed = patch.clone();
        changed.models[0].context_window = 128_000;
        let report = diff_targets(
            &OpencodeAdapter,
            &home,
            &[ToolDiffTarget {
                key: WHOLE_KEY.into(),
                patch: changed,
            }],
        )
        .unwrap();
        assert!(!report.entries[0].consistent, "改了窗口必须判为不一致");
        assert!(
            report.entries[0]
                .differences
                .iter()
                .any(|d| d.contains("上下文窗口")),
            "{:?}",
            report.entries[0].differences
        );

        // 只改思考档 → 也必须报出差异。
        let mut changed = patch.clone();
        changed.defaults.per_model_effort =
            crate::local_tools::adapters::map_from(&[("openhub-gateway/alias/m1", "low,high,max")]);
        let report = diff_targets(
            &OpencodeAdapter,
            &home,
            &[ToolDiffTarget {
                key: WHOLE_KEY.into(),
                patch: changed,
            }],
        )
        .unwrap();
        assert!(!report.entries[0].consistent, "改了思考档必须判为不一致");
        assert!(
            report.entries[0]
                .differences
                .iter()
                .any(|d| d.contains("思考档")),
            "{:?}",
            report.entries[0].differences
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    /// 全部适配器统一验证：真实写入后，同一份 patch 必须被判为「一致」。
    ///
    /// 这是「模型×站点」模式可用性的核心前提 —— 该模式整单一条目标，
    /// 任何适配器侧的编码细节（claude 三档映射、dsh 的 `name|efforts`、
    /// opencode 的 `provider/model` key、codex 的单模型约束）若与比对口径不符，
    /// 徽标会永远停在「不一致」，此测试即失败。
    #[test]
    fn applied_patch_is_always_consistent_across_adapters() {
        use crate::local_tools::adapters::{codex::CodexAdapter, dsh::DshAdapter};
        use crate::local_tools::adapters::{opencode::OpencodeAdapter, zcode::ZcodeAdapter};

        let cases: Vec<(&dyn ToolAdapter, &str, &str, &str)> = vec![
            (&ClaudeAdapter, "claude", "alias/sonnet", "alias/sonnet"),
            (&CodexAdapter, "codex", "alias/gpt-5", "alias/gpt-5"),
            // OpenCode 的模型 ID 与顶层默认模型都带供应商标识：写入时由适配器剥掉标识
            // 作为 models 的 key，但顶层 `model` 必须是「供应商/模型」才指得中。
            (
                &OpencodeAdapter,
                "opencode",
                "openhub-gateway/alias/m1",
                "m1",
            ),
            (&ZcodeAdapter, "zcode", "alias/m1", "m1"),
            (&DshAdapter, "dsh", "alias/m1", "m1"),
        ];

        for (adapter, name, provider_model, model) in cases {
            let home = temp_home(name);
            let patch = {
                let snap = adapter.snapshot(&home).unwrap();
                ToolConfigPatch {
                    base_hash: snap.content_hash,
                    providers: vec![ProviderEntry {
                        id: "openhub-gateway".into(),
                        name: "OpenHub 网关 · 模型×站点".into(),
                        base_url: "http://127.0.0.1:17896/v1".into(),
                        api_key: "sk-openhub-test".into(),
                        protocol: "openai-completions".into(),
                        models: vec![provider_model.into()],
                    }],
                    models: vec![ModelEntry {
                        id: provider_model.into(),
                        name: model.into(),
                        provider: "openhub-gateway".into(),
                        ..Default::default()
                    }],
                    defaults: DefaultsSection {
                        model: provider_model.into(),
                        provider: "openhub-gateway".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            };

            adapter.apply(&home, &patch).unwrap();
            let report = diff_targets(
                adapter,
                &home,
                &[ToolDiffTarget {
                    key: WHOLE_KEY.into(),
                    patch: patch.clone(),
                }],
            )
            .unwrap();
            assert!(
                report.entries[0].consistent,
                "{name}: 刚写入的配置应判为一致，实际差异：{:?}",
                report.entries[0].differences
            );

            let _ = std::fs::remove_dir_all(&home);
        }
    }

    /// Claude 在「模型×站点」模式下的实际写入形态：一条网关接入，
    /// 顶层 model = 一行里的首个模型，opus/sonnet/haiku 三档跨行取首个模型。
    ///
    /// 三档写在 `defaults.per_model_effort`（复用为「档位 → 模型」），
    /// 落盘后由适配器读回 `per_model_effort` 与 `models`，比对必须能逐档对齐；
    /// 任一档错位都会让 claude 的整单结论永远停在「不一致」。
    #[test]
    fn claude_model_mode_write_is_consistent() {
        let home = temp_home("claude-tiers");
        std::fs::write(home.join(".claude").join("settings.json"), "{}\n").unwrap();

        let snap = ClaudeAdapter.snapshot(&home).unwrap();
        let tier_models = ["site-a/sonnet", "site-b/opus", "site-c/haiku"];
        let patch = ToolConfigPatch {
            base_hash: snap.content_hash,
            providers: vec![ProviderEntry {
                id: "openhub-gateway".into(),
                name: "OpenHub 网关 · 模型×站点".into(),
                base_url: "http://127.0.0.1:17896".into(),
                api_key: "sk-openhub-test".into(),
                protocol: "anthropic".into(),
                models: tier_models.iter().map(|m| m.to_string()).collect(),
            }],
            models: tier_models
                .iter()
                .map(|model| ModelEntry {
                    id: model.to_string(),
                    name: model.to_string(),
                    provider: "openhub-gateway".into(),
                    ..Default::default()
                })
                .collect(),
            defaults: DefaultsSection {
                model: tier_models[0].into(),
                provider: "openhub-gateway".into(),
                per_model_effort: crate::local_tools::adapters::map_from(&[
                    ("opus", "site-b/opus"),
                    ("sonnet", "site-a/sonnet"),
                    ("haiku", "site-c/haiku"),
                ]),
                ..Default::default()
            },
            ..Default::default()
        };

        ClaudeAdapter.apply(&home, &patch).unwrap();
        let report = diff_targets(
            &ClaudeAdapter,
            &home,
            &[ToolDiffTarget {
                key: WHOLE_KEY.into(),
                patch: patch.clone(),
            }],
        )
        .unwrap();
        assert!(
            report.entries[0].consistent,
            "三档映射写入后应判为一致，实际差异：{:?}",
            report.entries[0].differences
        );

        // 篡改一档（模拟用户在 Claude 里换掉 sonnet 档）→ 必须报出该档差异。
        let settings = home.join(".claude").join("settings.json");
        let text = std::fs::read_to_string(&settings).unwrap();
        let edited = text.replace("\"site-a/sonnet\"", "\"user/sonnet\"");
        assert_ne!(text, edited);
        std::fs::write(&settings, edited).unwrap();
        let report = diff_targets(
            &ClaudeAdapter,
            &home,
            &[ToolDiffTarget {
                key: WHOLE_KEY.into(),
                patch,
            }],
        )
        .unwrap();
        assert!(!report.entries[0].consistent);
        assert!(
            report.entries[0]
                .differences
                .iter()
                .any(|d| d.contains("思考档映射") || d.contains("模型")),
            "{:?}",
            report.entries[0].differences
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    /// 整单模式使用的目标键（与前端 `WHOLE_LIST_KEY` 语义一致）。
    const WHOLE_KEY: &str = "__all__";
}
