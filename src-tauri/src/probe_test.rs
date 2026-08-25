#[test]
fn probe_parse_fixed_json() {
    let raw = std::fs::read_to_string("/tmp/opencode_proxy_config_fixed.json").unwrap();
    match serde_json::from_str::<crate::model::gateway::ModelProxyConfig>(&raw) {
        Ok(cfg) => {
            println!("解析成功: {} 个渠道", cfg.channels.len());
            for ch in &cfg.channels { println!(" - {} ({})", ch.name, ch.id); }
        }
        Err(e) => println!("解析失败: {e}"),
    }
}
