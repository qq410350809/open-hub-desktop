use super::balancer::normalize_key_group_id;
use super::types::{
    default_channels, default_key_group_mode, ChannelConfig, ModelProxyConfig, OpencodeProxyConfig,
    KEY_GROUP_MODE_INDEPENDENT,
};
use getrandom::fill as fill_random;
use rusqlite::Connection;
use std::collections::HashMap;

/// 已下线的固定预设分组 ID：分组现一律由 Key 自身的 groupName 决定，
/// 存量配置中的这两个预设组在加载时清除，其成员 Key 回落到 groupName 分组。
const LEGACY_PRESET_KEY_GROUP_IDS: [&str; 2] = ["primary", "backup"];

fn ensure_gateway_api_key(config: &mut ModelProxyConfig) {
    if !config.api_key.trim().is_empty() {
        return;
    }
    let mut bytes = [0u8; 24];
    if fill_random(&mut bytes).is_ok() {
        config.api_key = format!("sk-openhub-{}", hex::encode(bytes));
    }
}

pub fn sanitize_channel_config(channel: &mut ChannelConfig) {
    if let Some(alias) = &channel.alias {
        let trimmed = alias.trim().to_lowercase();
        if trimmed.is_empty() {
            channel.alias = None;
        } else {
            channel.alias = Some(trimmed);
        }
    }

    // 清洗渠道自定义 Key 分组：id 即 Key 的 groupName；
    // 历史的 primary/backup 预设组已下线（分组一律由 groupName 决定），加载时剔除。
    if let Some(groups) = channel.key_groups.take() {
        let mut seen = std::collections::HashSet::new();
        let cleaned: Vec<_> = groups
            .into_iter()
            .filter_map(|mut g| {
                // 组 ID 与 Key 的 groupName 同源，需走同一套归一（「默认分组」→ default）
                g.id = if g.id.trim().is_empty() {
                    String::new()
                } else {
                    normalize_key_group_id(&g.id)
                };
                g.name = g.name.trim().to_string();
                if g.id.is_empty()
                    || LEGACY_PRESET_KEY_GROUP_IDS.contains(&g.id.as_str())
                    || !seen.insert(g.id.clone())
                {
                    None
                } else {
                    if g.name.is_empty() {
                        g.name = g.id.clone();
                    }
                    if g.mode != KEY_GROUP_MODE_INDEPENDENT {
                        g.mode = default_key_group_mode();
                    }
                    Some(g)
                }
            })
            .collect();
        channel.key_groups = if cleaned.is_empty() {
            None
        } else {
            Some(cleaned)
        };
    }

    // 清洗渠道单 Key 规则：指向已下线预设组的 group_id 清空，
    // 使该 Key 回落到自身 groupName 决定的分组
    if let Some(rules) = channel.key_rules.take() {
        let mut seen = std::collections::HashSet::new();
        let cleaned: Vec<_> = rules
            .into_iter()
            .filter_map(|mut r| {
                r.key = r.key.trim().to_string();
                // 空 group_id 保持为空（= 不覆盖，跟随 Key 自身 groupName）；
                // 非空则与分组 ID 同源归一；指向已下线预设组的归属清空以回落 groupName
                r.group_id = if r.group_id.trim().is_empty() {
                    String::new()
                } else {
                    normalize_key_group_id(&r.group_id)
                };
                if LEGACY_PRESET_KEY_GROUP_IDS.contains(&r.group_id.as_str()) {
                    r.group_id = String::new();
                }
                if r.key.is_empty() || !seen.insert(r.key.clone()) {
                    None
                } else {
                    Some(r)
                }
            })
            .collect();
        channel.key_rules = if cleaned.is_empty() {
            None
        } else {
            Some(cleaned)
        };
    }

    // 清洗模型级代理出口规则：模型名去空格去重，mode 白名单校验，
    // 旧 fixed 值归一为 custom_node（语义相同：固定单一出口节点）；
    // direct/pool 语义下 node_id/channel_id 无意义直接剥离；
    // fixed_channel 剥离 node_id 保留 channel_id；custom_node 剥离 channel_id 保留 node_id；
    // mode=direct 且与渠道级默认一致时仍保留（显式覆盖）；
    // protocol（模型级上游协议覆盖）白名单校验，空/非法值清空 = 跟随渠道级
    if let Some(rules) = channel.model_proxy_rules.take() {
        let mut seen = std::collections::HashSet::new();
        let cleaned: Vec<_> = rules
            .into_iter()
            .filter_map(|mut r| {
                r.model = r.model.trim().to_string();
                r.mode = r.mode.trim().to_lowercase();
                if r.mode == "fixed" {
                    r.mode = "custom_node".to_string();
                }
                if !matches!(
                    r.mode.as_str(),
                    "direct" | "pool" | "custom_node" | "fixed_channel"
                ) {
                    return None;
                }
                if let Some(ref node) = r.node_id {
                    let node = node.trim().to_string();
                    r.node_id = if node.is_empty() || r.mode != "custom_node" {
                        None
                    } else {
                        Some(node)
                    };
                }
                if let Some(ref ch) = r.channel_id {
                    let ch = ch.trim().to_string();
                    r.channel_id = if ch.is_empty() || r.mode != "fixed_channel" {
                        None
                    } else {
                        Some(ch)
                    };
                }
                if let Some(protocol) = &r.protocol {
                    let protocol = protocol.trim().to_lowercase();
                    r.protocol = if VALID_CHANNEL_PROTOCOLS.contains(&protocol.as_str()) {
                        Some(protocol)
                    } else {
                        None
                    };
                }
                if r.model.is_empty() || !seen.insert(r.model.to_lowercase()) {
                    None
                } else {
                    Some(r)
                }
            })
            .collect();
        channel.model_proxy_rules = if cleaned.is_empty() {
            None
        } else {
            Some(cleaned)
        };
    }
}

/// 渠道上游目标协议白名单
const VALID_CHANNEL_PROTOCOLS: [&str; 4] = ["openai", "openai-responses", "anthropic", "gemini"];

/// 已下线的内置固化渠道：其 stats_id 落在 1-100 保留段，前端视为不可删除的
/// 内置渠道，存量安装的配置中必须由后端在加载时自动清除。
const RETIRED_BUILTIN_CHANNEL_IDS: [&str; 1] = ["alpha"];

pub fn sanitize_model_proxy_config(config: &mut ModelProxyConfig) {
    ensure_gateway_api_key(config);
    config.listen_host = "127.0.0.1".to_string();
    if config.channels.is_empty() {
        config.channels = default_channels();
    }
    config
        .channels
        .retain(|ch| !RETIRED_BUILTIN_CHANNEL_IDS.contains(&ch.id.as_str()));
    let mut seen_aliases = std::collections::HashSet::new();
    for ch in &mut config.channels {
        // OpenCode 官方渠道个性化固化策略见 policies/opencode.rs
        super::policies::opencode::pin_channel_config(ch);
        // 目标协议白名单校验：非法/历史遗留值回退为 OpenAI 兼容
        let p = ch.protocol.trim().to_lowercase();
        ch.protocol = if VALID_CHANNEL_PROTOCOLS.contains(&p.as_str()) {
            p
        } else {
            "openai".to_string()
        };
        sanitize_channel_config(ch);
        if ch
            .site_id
            .as_deref()
            .is_some_and(|site_id| !site_id.trim().is_empty())
        {
            // 站点关联渠道的 Key 只从 site_model_cache 运行时读取，避免在网关配置中留存副本。
            ch.api_key.clear();
            ch.api_keys = None;
        }
        let eff = ch.effective_alias();
        if seen_aliases.contains(&eff) {
            let disambiguated = format!("{}_{}", eff, &ch.id[..ch.id.len().min(4)]);
            ch.alias = Some(disambiguated);
        }
        seen_aliases.insert(ch.effective_alias());
    }

    // 统计维度稳定数字 ID：opencode 固定为 1，动态渠道从 101 递增（1-100 预留给内置渠道）。
    // 已分配的 ID 永不改动，改别名/改编码不影响历史统计；计数器只前进不回退。
    let mut next = config.next_channel_stats_id.max(101);
    for ch in &mut config.channels {
        if ch.id == "opencode" {
            continue;
        }
        if ch.stats_id.is_none() {
            ch.stats_id = Some(next as u32);
            next += 1;
        } else if let Some(sid) = ch.stats_id {
            if sid as u64 >= 101 {
                next = next.max(sid as u64 + 1);
            }
        }
    }
    config.next_channel_stats_id = next;

    sanitize_model_channel_order(config);
}

/// 清洗「多渠道共同提供的模型」路由顺序：key 统一小写，value 去重并剔除
/// 已不存在的渠道 ID；空表清为 None。顺序（先后关系）保持用户配置不变。
fn sanitize_model_channel_order(config: &mut ModelProxyConfig) {
    let Some(order) = config.model_channel_order.take() else {
        return;
    };
    let channel_ids: std::collections::HashSet<&str> =
        config.channels.iter().map(|ch| ch.id.as_str()).collect();
    let cleaned: HashMap<String, Vec<String>> = order
        .into_iter()
        .filter_map(|(model, ids)| {
            let key = model.trim().to_lowercase();
            if key.is_empty() {
                return None;
            }
            let mut seen = std::collections::HashSet::new();
            let ids: Vec<String> = ids
                .into_iter()
                .filter(|id| channel_ids.contains(id.as_str()) && seen.insert(id.clone()))
                .collect();
            if ids.len() < 2 {
                // 单渠道顺序无排序意义，视为未配置
                None
            } else {
                Some((key, ids))
            }
        })
        .collect();
    if cleaned.is_empty() {
        config.model_channel_order = None;
    } else {
        config.model_channel_order = Some(cleaned);
    }
}

pub fn load_model_proxy_config(conn: &Connection) -> ModelProxyConfig {
    let raw = crate::db::read_meta_conn(conn, "opencode_proxy_config").unwrap_or_default();

    let had_api_key = serde_json::from_str::<ModelProxyConfig>(&raw)
        .ok()
        .is_some_and(|config| !config.api_key.trim().is_empty());
    // 解析失败绝不能静默回退默认值 —— 那会把用户全部渠道/开关"重置"，
    // 表现为「保存了重启又恢复」。失败时保留原文并高声报错，便于定位。
    let parse_result = serde_json::from_str::<ModelProxyConfig>(&raw);
    if let Err(error) = &parse_result {
        if !raw.trim().is_empty() {
            tracing::error!(
                "[ModelGateway] 反代配置解析失败（回退默认值，原配置前 400 字符: {}）: {error}",
                raw.chars().take(400).collect::<String>()
            );
        }
    }
    let mut cfg = parse_result.unwrap_or_default();

    sanitize_model_proxy_config(&mut cfg);
    if !had_api_key {
        if let Ok(serialized) = serde_json::to_string(&cfg) {
            let _ = crate::db::write_meta(conn, "opencode_proxy_config", &serialized);
        }
    }
    cfg
}

#[allow(dead_code)]
pub fn load_opencode_proxy_config(conn: &Connection) -> OpencodeProxyConfig {
    load_model_proxy_config(conn)
}

pub fn save_model_proxy_config(conn: &Connection, config: &ModelProxyConfig) -> Result<(), String> {
    let mut c = config.clone();
    sanitize_model_proxy_config(&mut c);
    let json_str = serde_json::to_string(&c).map_err(|e| format!("序列化模型网关配置失败: {e}"))?;
    crate::db::write_meta(conn, "opencode_proxy_config", &json_str)
}

#[allow(dead_code)]
pub fn save_opencode_proxy_config(
    conn: &Connection,
    config: &OpencodeProxyConfig,
) -> Result<(), String> {
    save_model_proxy_config(conn, config)
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn sanitize_purges_retired_builtin_alpha_channel() {
        // 存量安装迁移：alpha 曾以 stats_id=2 内置身份下发并持久化，
        // 前端视其为不可删除的内置渠道 —— 加载时必须由后端自动清除
        let mut cfg = ModelProxyConfig {
            channels: vec![
                ChannelConfig {
                    id: "opencode".to_string(),
                    name: "OpenCode".to_string(),
                    description: String::new(),
                    enabled: true,
                    protocol: "openai".to_string(),
                    base_url: "https://opencode.ai/zen/v1".to_string(),
                    api_key: String::new(),
                    api_keys: None,
                    use_proxy_pool: false,
                    alias: None,
                    site_id: None,
                    proxy_mode: None,
                    proxy_fixed_channel: None,
                    use_fixed_proxy: false,
                    fixed_proxy_node: None,
                    enabled_models: None,
                    model_redirects: None,
                    stats_id: Some(1),
                    key_groups: None,
                    key_rules: None,
                    model_proxy_rules: None,
                },
                ChannelConfig {
                    id: "alpha".to_string(),
                    name: "Ox Alpha 网页直连".to_string(),
                    description: String::new(),
                    enabled: true,
                    protocol: "web-chat".to_string(),
                    base_url: "https://oxalpha.com".to_string(),
                    api_key: String::new(),
                    api_keys: None,
                    use_proxy_pool: false,
                    alias: None,
                    site_id: None,
                    proxy_mode: None,
                    proxy_fixed_channel: None,
                    use_fixed_proxy: false,
                    fixed_proxy_node: None,
                    enabled_models: None,
                    model_redirects: None,
                    stats_id: Some(2),
                    key_groups: None,
                    key_rules: None,
                    model_proxy_rules: None,
                },
                ChannelConfig {
                    id: "site_a".to_string(),
                    name: "Site A".to_string(),
                    description: String::new(),
                    enabled: true,
                    protocol: "openai".to_string(),
                    base_url: "https://site-a.example/v1".to_string(),
                    api_key: String::new(),
                    api_keys: None,
                    use_proxy_pool: false,
                    alias: None,
                    site_id: Some("site_a".to_string()),
                    proxy_mode: None,
                    proxy_fixed_channel: None,
                    use_fixed_proxy: false,
                    fixed_proxy_node: None,
                    enabled_models: None,
                    model_redirects: None,
                    stats_id: Some(101),
                    key_groups: None,
                    key_rules: None,
                    model_proxy_rules: None,
                },
            ],
            ..Default::default()
        };
        sanitize_model_proxy_config(&mut cfg);
        assert!(
            !cfg.channels.iter().any(|c| c.id == "alpha"),
            "已下线内置渠道必须被自动清除"
        );
        // 用户自建渠道不受清理影响
        assert!(cfg.channels.iter().any(|c| c.id == "site_a"));
        assert_eq!(cfg.channels.len(), 2);
    }

    #[test]
    fn sanitize_model_proxy_rules_dedupes_and_validates_mode() {
        use super::super::types::ModelProxyRule;
        let mut ch = ChannelConfig {
            id: "ch".to_string(),
            name: "ch".to_string(),
            description: String::new(),
            enabled: true,
            protocol: "openai".to_string(),
            base_url: "https://x.example/v1".to_string(),
            api_key: String::new(),
            api_keys: None,
            use_proxy_pool: false,
            alias: None,
            site_id: None,
            proxy_mode: None,
            proxy_fixed_channel: None,
            use_fixed_proxy: false,
            fixed_proxy_node: None,
            enabled_models: None,
            model_redirects: None,
            stats_id: None,
            key_groups: None,
            key_rules: None,
            model_proxy_rules: Some(vec![
                ModelProxyRule {
                    protocol: None,
                    model: " glm-a ".into(),
                    mode: "direct".into(),
                    node_id: Some("n1".into()),
                    channel_id: None,
                },
                ModelProxyRule {
                    protocol: None,
                    model: "GLM-A".into(),
                    mode: "fixed".into(),
                    node_id: Some(" n2 ".into()),
                    channel_id: None,
                },
                ModelProxyRule {
                    protocol: None,
                    model: "glm-b".into(),
                    mode: "bogus".into(),
                    node_id: None,
                    channel_id: None,
                },
                ModelProxyRule {
                    protocol: None,
                    model: "glm-c".into(),
                    mode: "fixed".into(),
                    node_id: Some("  ".into()),
                    channel_id: None,
                },
                ModelProxyRule {
                    protocol: None,
                    model: "   ".into(),
                    mode: "direct".into(),
                    node_id: None,
                    channel_id: None,
                },
                ModelProxyRule {
                    protocol: None,
                    model: "glm-d".into(),
                    mode: "fixed_channel".into(),
                    node_id: Some("n3".into()),
                    channel_id: Some(" pc1 ".into()),
                },
                ModelProxyRule {
                    protocol: None,
                    model: "glm-e".into(),
                    mode: "custom_node".into(),
                    node_id: Some(" n4 ".into()),
                    channel_id: Some("pc2".into()),
                },
            ]),
        };
        sanitize_channel_config(&mut ch);
        let rules = ch.model_proxy_rules.unwrap();
        // 同模型（忽略大小写）只保留首条；非法 mode 与空模型被剔除
        assert_eq!(rules.len(), 4, "rules: {rules:?}");
        assert_eq!(rules[0].model, "glm-a");
        // direct 模式下 node_id 被剥离
        assert_eq!(rules[0].node_id, None);
        // 旧 fixed 归一为 custom_node；空 node_id 归一为 None
        assert_eq!(rules[1].model, "glm-c");
        assert_eq!(rules[1].mode, "custom_node");
        assert_eq!(rules[1].node_id, None);
        // fixed_channel：剥离 node_id、保留去空格后的 channel_id
        assert_eq!(rules[2].model, "glm-d");
        assert_eq!(rules[2].mode, "fixed_channel");
        assert_eq!(rules[2].node_id, None);
        assert_eq!(rules[2].channel_id.as_deref(), Some("pc1"));
        // custom_node：保留去空格后的 node_id、剥离 channel_id
        assert_eq!(rules[3].model, "glm-e");
        assert_eq!(rules[3].mode, "custom_node");
        assert_eq!(rules[3].node_id.as_deref(), Some("n4"));
        assert_eq!(rules[3].channel_id, None);
    }

    #[test]
    fn empty_model_proxy_rules_normalized_to_none() {
        use super::super::types::ModelProxyRule;
        let mut ch = ChannelConfig {
            id: "ch".to_string(),
            name: "ch".to_string(),
            description: String::new(),
            enabled: true,
            protocol: "openai".to_string(),
            base_url: "https://x.example/v1".to_string(),
            api_key: String::new(),
            api_keys: None,
            use_proxy_pool: false,
            alias: None,
            site_id: None,
            proxy_mode: None,
            proxy_fixed_channel: None,
            use_fixed_proxy: false,
            fixed_proxy_node: None,
            enabled_models: None,
            model_redirects: None,
            stats_id: None,
            key_groups: None,
            key_rules: None,
            model_proxy_rules: Some(vec![ModelProxyRule {
                    protocol: None,
                model: "m".into(),
                mode: "invalid".into(),
                node_id: None,
                channel_id: None,
            }]),
        };
        sanitize_channel_config(&mut ch);
        assert!(ch.model_proxy_rules.is_none());
    }
}
