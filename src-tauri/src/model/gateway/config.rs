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
const VALID_CHANNEL_PROTOCOLS: [&str; 4] = ["openai", "openai-responses", "anthropic", "gemini"];

pub fn sanitize_model_proxy_config(config: &mut ModelProxyConfig) {
    ensure_gateway_api_key(config);
    config.listen_host = "127.0.0.1".to_string();
    if config.channels.is_empty() {
        config.channels = default_channels();
    }
    let mut seen_aliases = std::collections::HashSet::new();
    for ch in &mut config.channels {
        // OpenCode 官方渠道别名固定为 opencode（网关模型前缀依赖它），禁止自定义
        if ch.id == "opencode" {
            ch.alias = None;
            ch.protocol = "openai".to_string();
            ch.stats_id = Some(1);
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
