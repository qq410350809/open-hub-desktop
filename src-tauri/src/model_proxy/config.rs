use super::types::{default_channels, ChannelConfig, ModelProxyConfig, OpencodeProxyConfig};
use rusqlite::{params, Connection, OptionalExtension};

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

pub fn sanitize_model_proxy_config(config: &mut ModelProxyConfig) {
    if config.channels.is_empty() {
        config.channels = default_channels();
    }
    let mut seen_aliases = std::collections::HashSet::new();
    for ch in &mut config.channels {
        sanitize_channel_config(ch);
        let eff = ch.effective_alias();
        if seen_aliases.contains(&eff) {
            let disambiguated = format!("{}_{}", eff, &ch.id[..ch.id.len().min(4)]);
            ch.alias = Some(disambiguated);
        }
        seen_aliases.insert(ch.effective_alias());
    }
}

pub fn load_model_proxy_config(conn: &Connection) -> ModelProxyConfig {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM app_meta WHERE key = 'opencode_proxy_config'",
            [],
            |row| row.get(0),
        )
        .optional()
        .unwrap_or(None);

    let mut cfg = raw
        .and_then(|json_str| serde_json::from_str::<ModelProxyConfig>(&json_str).ok())
        .unwrap_or_default();

    sanitize_model_proxy_config(&mut cfg);
    cfg
}

#[allow(dead_code)]
pub fn load_opencode_proxy_config(conn: &Connection) -> OpencodeProxyConfig {
    load_model_proxy_config(conn)
}

pub fn save_model_proxy_config(conn: &Connection, config: &ModelProxyConfig) -> Result<(), String> {
    let mut c = config.clone();
    sanitize_model_proxy_config(&mut c);
    let json_str =
        serde_json::to_string(&c).map_err(|e| format!("序列化模型网关配置失败: {e}"))?;
    conn.execute(
        "INSERT INTO app_meta (key, value) VALUES ('opencode_proxy_config', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![json_str],
    )
    .map_err(|e| format!("保存模型网关配置失败: {e}"))?;
    Ok(())
}

#[allow(dead_code)]
pub fn save_opencode_proxy_config(
    conn: &Connection,
    config: &OpencodeProxyConfig,
) -> Result<(), String> {
    save_model_proxy_config(conn, config)
}
