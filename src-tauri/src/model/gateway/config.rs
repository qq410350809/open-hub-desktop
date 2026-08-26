use super::types::{default_channels, ChannelConfig, ModelProxyConfig, OpencodeProxyConfig};
use getrandom::fill as fill_random;
use rusqlite::Connection;

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
}

/// 渠道上游目标协议白名单
const VALID_CHANNEL_PROTOCOLS: [&str; 5] =
    ["openai", "openai-responses", "anthropic", "gemini", "web-chat"];

pub fn sanitize_model_proxy_config(config: &mut ModelProxyConfig) {
    ensure_gateway_api_key(config);
    config.listen_host = "127.0.0.1".to_string();
    if config.channels.is_empty() {
        config.channels = default_channels();
    }
    // 内置固化渠道升级补齐：存量配置缺少后加的内置渠道时自动注入，
    // 保证老用户升级后新通道开箱可用
    for default_ch in default_channels() {
        if !config.channels.iter().any(|c| c.id == default_ch.id) {
            config.channels.push(default_ch);
        }
    }
    let mut seen_aliases = std::collections::HashSet::new();
    for ch in &mut config.channels {
        // OpenCode 官方渠道别名固定为 opencode（网关模型前缀依赖它），禁止自定义
        if ch.id == "opencode" {
            ch.alias = None;
            ch.protocol = "openai".to_string();
            ch.stats_id = Some(1);
        }
        // Alpha 网页直连渠道：协议与统计 ID 固化，别名固定为 alpha（同 opencode 语义）
        if ch.id == "alpha" {
            ch.alias = None;
            ch.protocol = "web-chat".to_string();
            ch.stats_id = Some(2);
            if ch.base_url.trim().is_empty() {
                ch.base_url = "https://oxalpha.com".to_string();
            }
        }
        // 目标协议白名单校验：非法/历史遗留值回退为 OpenAI 兼容
        let p = ch.protocol.trim().to_lowercase();
        ch.protocol = if VALID_CHANNEL_PROTOCOLS.contains(&p.as_str()) {
            p
        } else {
            "openai".to_string()
        };
        sanitize_channel_config(ch);
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

    // 展示与存储顺序：内置固化渠道（stats_id 1-100）在前，动态渠道（101+）在后。
    // 升级补齐会把后置内置渠道追加到存量列表末尾，这里按稳定数字 ID 归位；
    // 稳定排序保证同段内（尤其动态渠道之间）原有相对顺序不变。
    config
        .channels
        .sort_by_key(|ch| ch.stats_id.unwrap_or(u32::MAX));
}

pub fn load_model_proxy_config(conn: &Connection) -> ModelProxyConfig {
    let raw = crate::db::read_meta_conn(conn, "opencode_proxy_config").unwrap_or_default();

    let had_api_key = serde_json::from_str::<ModelProxyConfig>(&raw)
        .ok()
        .is_some_and(|config| !config.api_key.trim().is_empty());
    let mut cfg = serde_json::from_str::<ModelProxyConfig>(&raw).unwrap_or_default();

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
    fn sanitize_backfills_missing_builtin_alpha_channel() {
        // 存量用户升级场景：已保存配置含 opencode + 站点动态渠道，缺少 alpha
        let mut cfg = ModelProxyConfig {
            channels: vec![
                ChannelConfig {
                    id: "opencode".to_string(),
                    name: "legacy".to_string(),
                    description: String::new(),
                    enabled: true,
                    protocol: "openai".to_string(),
                    base_url: "https://opencode.ai/zen/v1".to_string(),
                    api_key: String::new(),
                    api_keys: None,
                    use_proxy_pool: false,
                    alias: None,
                    site_id: None,
                    use_fixed_proxy: false,
                    fixed_proxy_node: None,
                    priority: None,
                    weight: None,
                    enabled_models: None,
                    model_redirects: None,
                    rate_limit_rpm: None,
                    stats_id: Some(1),
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
                    alias: Some("sitea".to_string()),
                    site_id: None,
                    use_fixed_proxy: false,
                    fixed_proxy_node: None,
                    priority: None,
                    weight: None,
                    enabled_models: None,
                    model_redirects: None,
                    rate_limit_rpm: None,
                    stats_id: None,
                },
            ],
            ..Default::default()
        };
        sanitize_model_proxy_config(&mut cfg);
        let alpha = cfg
            .channels
            .iter()
            .find(|c| c.id == "alpha")
            .expect("内置 alpha 渠道必须被自动补齐");
        assert_eq!(alpha.protocol, "web-chat");
        assert_eq!(alpha.stats_id, Some(2));
        assert_eq!(alpha.effective_alias(), "alpha");
        assert_eq!(alpha.base_url, "https://oxalpha.com");
        // 补齐后必须归位到内置段（opencode 之后、站点动态渠道之前），
        // 而非追加在列表末尾
        let ids: Vec<u32> = cfg
            .channels
            .iter()
            .map(|c| c.stats_id.unwrap_or(u32::MAX))
            .collect();
        assert_eq!(
            ids,
            vec![1, 2, 101],
            "渠道顺序应为 opencode(1) → alpha(2) → 动态(101+)"
        );
    }

    #[test]
    fn sanitize_pins_alpha_protocol_and_stats_id() {
        // 用户误改协议/base_url 时固化回正确值（同 opencode 语义）
        let mut cfg = ModelProxyConfig::default();
        cfg.channels[0].protocol = "gemini".to_string();
        let mut alpha = crate::model::gateway::types::default_channels()
            .into_iter()
            .find(|c| c.id == "alpha")
            .unwrap();
        alpha.protocol = "anthropic".to_string();
        alpha.base_url = String::new();
        cfg.channels.push(alpha);
        sanitize_model_proxy_config(&mut cfg);
        let alpha = cfg.channels.iter().find(|c| c.id == "alpha").unwrap();
        assert_eq!(alpha.protocol, "web-chat", "协议必须固化");
        assert_eq!(alpha.base_url, "https://oxalpha.com", "空 base 必须回填");
        assert_eq!(alpha.stats_id, Some(2));
    }
}
