use super::*;
use serde_json::json;

#[test]
fn parses_opencode_proxy_default_config() {
    let cfg = ModelProxyConfig::default();
    assert!(cfg.enabled);
    assert_eq!(cfg.port, default_model_proxy_port());
    assert_eq!(cfg.channels.len(), 1);
    assert_eq!(cfg.channels[0].id, "opencode");
    assert_eq!(cfg.channels[0].effective_alias(), "opencode");
}

#[test]
fn sanitize_always_provisions_gateway_api_key_and_shared_host() {
    use crate::model::gateway::config::sanitize_model_proxy_config;

    let mut config = ModelProxyConfig {
        listen_host: "0.0.0.0".to_string(),
        ..ModelProxyConfig::default()
    };
    sanitize_model_proxy_config(&mut config);
    assert_eq!(config.listen_host, "127.0.0.1");
    // 网关 API Key 必须自动生成，空 Key 不再代表免密访问。
    assert!(config.api_key.starts_with("sk-openhub-"));

    let mut existing_key = ModelProxyConfig {
        api_key: "sk-custom".to_string(),
        ..ModelProxyConfig::default()
    };
    sanitize_model_proxy_config(&mut existing_key);
    assert_eq!(existing_key.api_key, "sk-custom");
}

#[test]
fn channel_alias_fallback_and_normalization() {
    let mut ch = ChannelConfig {
        id: "MyChannel".to_string(),
        name: "Test".to_string(),
        description: "".to_string(),
        enabled: true,
        protocol: "openai".to_string(),
        base_url: "https://api.openai.com/v1".to_string(),
        api_key: "".to_string(),
        api_keys: None,
        use_proxy_pool: false,
        alias: Some(" VIP_Channel ".to_string()),
        site_id: None,
        use_fixed_proxy: false,
        fixed_proxy_node: None,
        priority: None,
        weight: None,
        enabled_models: None,
        model_redirects: None,
        rate_limit_rpm: None,
        stats_id: None,
        key_groups: None,
        key_rules: None,
        model_proxy_rules: None,
    };
    assert_eq!(ch.effective_alias(), "vip_channel");
    ch.alias = None;
    assert_eq!(ch.effective_alias(), "mychannel");
}

#[test]
fn resolves_channels_with_alias_prefix_or_model_whitelist() {
    let cfg = ModelProxyConfig {
        enabled: true,
        listen_host: "127.0.0.1".to_string(),
        port: 8088,
        api_key: "".to_string(),
        channels: vec![
            ChannelConfig {
                id: "opencode".to_string(),
                name: "OpenCode".to_string(),
                description: "".to_string(),
                enabled: true,
                protocol: "openai".to_string(),
                base_url: "https://opencode.ai/zen/v1".to_string(),
                api_key: "".to_string(),
                api_keys: None,
                use_proxy_pool: false,
                alias: Some("opencode".to_string()),
                site_id: None,
                use_fixed_proxy: false,
                fixed_proxy_node: None,
                priority: Some(1),
                weight: Some(100),
                enabled_models: None,
                model_redirects: None,
                rate_limit_rpm: None,
                stats_id: None,
                key_groups: None,
                key_rules: None,
                model_proxy_rules: None,
            },
            ChannelConfig {
                id: "vip".to_string(),
                name: "VIP Channel".to_string(),
                description: "".to_string(),
                enabled: true,
                protocol: "openai".to_string(),
                base_url: "https://api.vip.com/v1".to_string(),
                api_key: "sk-vip".to_string(),
                api_keys: Some(vec!["sk-vip-1".to_string(), "sk-vip-2".to_string()]),
                use_proxy_pool: false,
                alias: Some("vip".to_string()),
                site_id: None,
                use_fixed_proxy: false,
                fixed_proxy_node: None,
                priority: Some(2),
                weight: Some(100),
                enabled_models: Some(vec!["claude-3-5-sonnet-20241022".to_string()]),
                model_redirects: None,
                rate_limit_rpm: None,
                stats_id: None,
                key_groups: None,
                key_rules: None,
                model_proxy_rules: None,
            },
        ],
        timeout_seconds: 300,
        record_request_body: false,
        max_retries: 0,
        next_channel_stats_id: 101,
        log_retention_days: None,
        model_channel_order: None,
    };

    // 1. 显式别名前缀
    let (ch, model) = resolve_channel(&cfg, "vip/gpt-4o").expect("should resolve");
    assert_eq!(ch.id, "vip");
    assert_eq!(model, "gpt-4o");

    // 2. 白名单自动匹配
    let (ch, model) = resolve_channel(&cfg, "claude-3-5-sonnet-20241022").expect("should resolve");
    assert_eq!(ch.id, "vip");
    assert_eq!(model, "claude-3-5-sonnet-20241022");

    // 3. 默认回退到 opencode
    let (ch, model) = resolve_channel(&cfg, "deepseek-chat").expect("should resolve");
    assert_eq!(ch.id, "opencode");
    assert_eq!(model, "deepseek-chat");
}

/// 构造最小可用渠道：全暴露（白名单为 None）
fn order_test_channel(id: &str, alias: &str, enabled: bool) -> ChannelConfig {
    ChannelConfig {
        id: id.to_string(),
        name: id.to_string(),
        description: "".to_string(),
        enabled,
        protocol: "openai".to_string(),
        base_url: format!("https://{alias}.example.com/v1"),
        api_key: String::new(),
        api_keys: None,
        use_proxy_pool: false,
        alias: Some(alias.to_string()),
        site_id: None,
        use_fixed_proxy: false,
        fixed_proxy_node: None,
        priority: None,
        weight: None,
        enabled_models: None,
        model_redirects: None,
        rate_limit_rpm: None,
        stats_id: None,
        key_groups: None,
        key_rules: None,
        model_proxy_rules: None,
    }
}

#[test]
fn model_channel_order_overrides_whitelist_array_order() {
    use std::collections::HashMap;

    // a 与 b 同时提供 shared-model（白名单都包含），数组序 b 在前 → 默认选 b
    let mut cfg = ModelProxyConfig {
        channels: vec![
            order_test_channel("opencode", "opencode", true),
            {
                let mut ch = order_test_channel("b", "b", true);
                ch.enabled_models = Some(vec!["shared-model".to_string()]);
                ch
            },
            {
                let mut ch = order_test_channel("a", "a", true);
                ch.enabled_models = Some(vec!["shared-model".to_string()]);
                ch
            },
        ],
        ..ModelProxyConfig::default()
    };
    let (ch, _) = resolve_channel(&cfg, "shared-model").expect("should resolve");
    assert_eq!(ch.id, "b");

    // 配置路由顺序 a 先于 b 后，改选 a；大小写不敏感、键为小写
    let mut order = HashMap::new();
    order.insert(
        "shared-model".to_string(),
        vec!["a".to_string(), "b".to_string()],
    );
    cfg.model_channel_order = Some(order);
    let (ch, _) = resolve_channel(&cfg, "Shared-Model").expect("should resolve");
    assert_eq!(ch.id, "a");

    // 首选渠道禁用时自动落到第二候选
    cfg.channels[2].enabled = false;
    let (ch, _) = resolve_channel(&cfg, "shared-model").expect("should resolve");
    assert_eq!(ch.id, "b");

    // 首选渠道被禁用且其余不匹配时，回退原数组序逻辑（opencode 兜底仍可服务）
    cfg.channels[1].enabled = false;
    let (ch, _) = resolve_channel(&cfg, "shared-model").expect("should resolve");
    assert_eq!(ch.id, "opencode");
}

#[test]
fn sanitize_trims_and_prunes_model_channel_order() {
    use crate::model::gateway::config::sanitize_model_proxy_config;
    use std::collections::HashMap;

    let mut order = HashMap::new();
    // 带空白/大写 key；含未知渠道与重复渠道；单渠道条目视为无意义
    order.insert(
        "  Shared-Model  ".to_string(),
        vec![
            "a".to_string(),
            "ghost".to_string(),
            "a".to_string(),
            "b".to_string(),
        ],
    );
    order.insert("solo".to_string(), vec!["a".to_string()]);
    order.insert("blank".to_string(), vec![]);
    let mut config = ModelProxyConfig {
        channels: vec![
            order_test_channel("opencode", "opencode", true),
            order_test_channel("a", "a", true),
            order_test_channel("b", "b", true),
        ],
        model_channel_order: Some(order),
        ..ModelProxyConfig::default()
    };
    sanitize_model_proxy_config(&mut config);

    let cleaned = config.model_channel_order.expect("should keep valid entry");
    assert_eq!(
        cleaned.get("shared-model"),
        Some(&vec!["a".to_string(), "b".to_string()])
    );
    assert!(cleaned.get("solo").is_none());
    assert!(cleaned.get("blank").is_none());

    // 全部条目无效时清空为 None
    let mut config = ModelProxyConfig {
        channels: vec![order_test_channel("opencode", "opencode", true)],
        model_channel_order: Some(HashMap::from([(
            "x".to_string(),
            vec!["ghost".to_string()],
        )])),
        ..ModelProxyConfig::default()
    };
    sanitize_model_proxy_config(&mut config);
    assert!(config.model_channel_order.is_none());
}

#[tokio::test]
async fn multi_key_round_robin_selection() {
    let ch = ChannelConfig {
        id: "multi".to_string(),
        name: "Multi Key".to_string(),
        description: "".to_string(),
        enabled: true,
        protocol: "openai".to_string(),
        base_url: "https://api.example.com/v1".to_string(),
        api_key: "single-key".to_string(),
        api_keys: Some(vec![
            "key-1".to_string(),
            "key-2".to_string(),
            "key-3".to_string(),
        ]),
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
        stats_id: None,
        key_groups: None,
        key_rules: None,
        model_proxy_rules: None,
    };

    let keys = ch.get_effective_keys();
    assert_eq!(keys, vec!["key-1", "key-2", "key-3"]);

    let state = ModelProxyState::new_with_app(None);
    let k1 = select_channel_api_key(&state.context, &ch).await;
    let k2 = select_channel_api_key(&state.context, &ch).await;
    let k3 = select_channel_api_key(&state.context, &ch).await;
    let k4 = select_channel_api_key(&state.context, &ch).await;

    assert_eq!(k1, "key-1");
    assert_eq!(k2, "key-2");
    assert_eq!(k3, "key-3");
    assert_eq!(k4, "key-1");
}

#[tokio::test]
async fn channel_key_groups_failover_and_filtering() {
    use crate::model::gateway::types::{ChannelKeyRule, KeyGroupItem};

    let ch = ChannelConfig {
        id: "grouped_channel".to_string(),
        name: "Grouped Channel".to_string(),
        description: "".to_string(),
        enabled: true,
        protocol: "openai".to_string(),
        base_url: "https://api.example.com/v1".to_string(),
        api_key: "".to_string(),
        api_keys: Some(vec![
            "key-primary-1".to_string(),
            "key-primary-2".to_string(),
            "key-backup-1".to_string(),
            "key-disabled".to_string(),
            "key-gpt4-only".to_string(),
        ]),
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
        stats_id: None,
        // 定义分组优先级：primary 优先级最高，backup 其次，disabled_group 禁用
        key_groups: Some(vec![
            KeyGroupItem {
                id: "primary".to_string(),
                name: "主力组".to_string(),
                enabled: true,
            },
            KeyGroupItem {
                id: "backup".to_string(),
                name: "备用组".to_string(),
                enabled: true,
            },
            KeyGroupItem {
                id: "disabled_group".to_string(),
                name: "停用组".to_string(),
                enabled: false,
            },
        ]),
        key_rules: Some(vec![
            ChannelKeyRule {
                key: "key-primary-1".to_string(),
                group_id: "primary".to_string(),
                enabled: true,
                supported_models: None, // 支持全部
            },
            ChannelKeyRule {
                key: "key-primary-2".to_string(),
                group_id: "primary".to_string(),
                enabled: true,
                supported_models: None, // 支持全部
            },
            ChannelKeyRule {
                key: "key-backup-1".to_string(),
                group_id: "backup".to_string(),
                enabled: true,
                supported_models: None,
            },
            ChannelKeyRule {
                key: "key-disabled".to_string(),
                group_id: "primary".to_string(),
                enabled: false, // 单 Key 禁用
                supported_models: None,
            },
            ChannelKeyRule {
                key: "key-gpt4-only".to_string(),
                group_id: "primary".to_string(),
                enabled: true,
                supported_models: Some(vec!["gpt-4".to_string()]), // 仅支持 gpt-4
            },
        ]),
        model_proxy_rules: None,
    };

    let state = ModelProxyState::new_with_app(None);

    // 1. 请求 gpt-3.5-turbo：key-gpt4-only 不支持，key-disabled 被禁用
    // 应当返回 2 个分组：[ [key-primary-1, key-primary-2], [key-backup-1] ]
    let groups = resolve_channel_key_groups_for_model(&state.context, &ch, "gpt-3.5-turbo").await;
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0], vec!["key-primary-1", "key-primary-2"]);
    assert_eq!(groups[1], vec!["key-backup-1"]);

    // 2. 请求 gpt-4：key-gpt4-only 支持，进入主力组
    let groups_gpt4 = resolve_channel_key_groups_for_model(&state.context, &ch, "gpt-4").await;
    assert_eq!(groups_gpt4.len(), 2);
    assert_eq!(
        groups_gpt4[0],
        vec!["key-primary-1", "key-primary-2", "key-gpt4-only"]
    );
    assert_eq!(groups_gpt4[1], vec!["key-backup-1"]);

    // 3. 禁用主力组：自动只剩下备用组
    let mut ch_disabled_primary = ch.clone();
    ch_disabled_primary.key_groups.as_mut().unwrap()[0].enabled = false;
    let groups_backup_only =
        resolve_channel_key_groups_for_model(&state.context, &ch_disabled_primary, "gpt-4").await;
    assert_eq!(groups_backup_only.len(), 1);
    assert_eq!(groups_backup_only[0], vec!["key-backup-1"]);
}

#[test]
fn gemini_request_and_response_bidirectional_translation() {
    let gemini_req = json!({
        "contents": [
            {
                "role": "user",
                "parts": [
                    { "text": "What is Rust?" }
                ]
            },
            {
                "role": "model",
                "parts": [
                    { "text": "Rust is a systems programming language." }
                ]
            },
            {
                "role": "user",
                "parts": [
                    { "text": "Tell me more about its memory safety." }
                ]
            }
        ],
        "systemInstruction": {
            "parts": [
                { "text": "You are an expert compiler engineer." }
            ]
        },
        "generationConfig": {
            "temperature": 0.5,
            "maxOutputTokens": 1024,
            "topP": 0.95
        },
        "tools": [
            {
                "functionDeclarations": [
                    {
                        "name": "lookup_docs",
                        "description": "Look up Rust standard library docs",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "symbol": { "type": "string" }
                            },
                            "required": ["symbol"]
                        }
                    }
                ]
            }
        ]
    });

    // 1. Gemini -> UniversalRequest
    let ur = crate::model::gateway::parsers::gemini_to_universal(&gemini_req, "gemini-1.5-pro");
    assert_eq!(ur.model, "gemini-1.5-pro");
    assert_eq!(ur.temperature, Some(0.5));
    assert_eq!(ur.max_tokens, Some(1024));

    assert_eq!(ur.system.len(), 1);
    assert!(
        matches!(&ur.system[0].kind,
            crate::model::gateway::ir::PartKind::Text { text } if text == "You are an expert compiler engineer."),
        "systemInstruction 必须归入 system: {:?}",
        ur.system
    );
    assert_eq!(ur.messages.len(), 3);
    assert_eq!(ur.messages[0].role, crate::model::gateway::ir::Role::User);
    assert_eq!(
        ur.messages[1].role,
        crate::model::gateway::ir::Role::Assistant
    );
    assert_eq!(ur.messages[2].role, crate::model::gateway::ir::Role::User);

    assert_eq!(ur.tools.len(), 1);
    assert_eq!(ur.tools[0].name, "lookup_docs");

    // 2. OpenAI -> Gemini
    let openai_resp = json!({
        "id": "chatcmpl-123",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "gpt-4o",
        "choices": [
            {
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Rust guarantees memory safety through its ownership and borrowing system."
                },
                "finish_reason": "stop"
            }
        ],
        "usage": {
            "prompt_tokens": 50,
            "completion_tokens": 15,
            "total_tokens": 65
        }
    });

    let gemini_resp =
        GeminiProtocolAdapter::openai_response_to_gemini(&openai_resp, "gemini-1.5-pro");
    assert_eq!(gemini_resp["modelVersion"], "gemini-1.5-pro");
    let candidate = &gemini_resp["candidates"][0];
    assert_eq!(candidate["finishReason"], "STOP");
    assert_eq!(
        candidate["content"]["parts"][0]["text"],
        "Rust guarantees memory safety through its ownership and borrowing system."
    );
    assert_eq!(gemini_resp["usageMetadata"]["promptTokenCount"], 50);
    assert_eq!(gemini_resp["usageMetadata"]["candidatesTokenCount"], 15);
    assert_eq!(gemini_resp["usageMetadata"]["totalTokenCount"], 65);
}

#[test]
fn gemini_stream_ir_emitter_produces_text_part() {
    use crate::model::gateway::ir::UniversalStreamEvent;
    use crate::model::gateway::stream::GeminiEmitter;
    let mut emitter = GeminiEmitter::default();
    let out = emitter.on_event(&UniversalStreamEvent::TextDelta("Hello world!".into()));
    assert_eq!(out.len(), 1);
    let payload: serde_json::Value =
        serde_json::from_str(out[0].trim_start_matches("data: ").trim()).unwrap();
    assert_eq!(
        payload["candidates"][0]["content"]["parts"][0]["text"],
        "Hello world!"
    );
}

#[test]
fn responses_entry_parses_into_universal_request() {
    use crate::model::gateway::ir::{PartKind, Role};
    let body = json!({
        "model": "gpt-4o",
        "instructions": "Be concise.",
        "input": [
            { "role": "user", "content": "Hello" }
        ]
    });
    let ur = crate::model::gateway::parsers::responses_to_universal(&body, "gpt-4o");
    assert_eq!(ur.system.len(), 1);
    assert!(matches!(&ur.system[0].kind, PartKind::Text { text } if text == "Be concise."));
    assert_eq!(ur.messages.len(), 1);
    assert_eq!(ur.messages[0].role, Role::User);
    assert!(matches!(&ur.messages[0].parts[0].kind, PartKind::Text { text } if text == "Hello"));
}

#[test]
fn sanitizes_and_normalizes_developer_and_model_roles() {
    let mut body = json!({
        "model": "gpt-4o",
        "messages": [
            { "role": "developer", "content": "system prompt" },
            { "role": "model", "content": "<think>thinking here</think>final answer" }
        ]
    });
    let ur = crate::model::gateway::parsers::chat_to_universal(&body, "gpt-4o");
    use crate::model::gateway::ir::{PartKind, Role};
    assert_eq!(
        ur.messages.len(),
        1,
        "developer 归一入 system，不占 messages"
    );
    assert_eq!(ur.system.len(), 1);
    assert!(matches!(&ur.system[0].kind, PartKind::Text { text } if text == "system prompt"));
    assert_eq!(ur.messages[0].role, Role::Assistant);
    // 内联 <think> 必须拆分为思考块 + 正文块
    assert!(matches!(
        &ur.messages[0].parts[0].kind,
        PartKind::Thinking { text, .. } if text == "thinking here"
    ));
    assert!(matches!(
        &ur.messages[0].parts[1].kind,
        PartKind::Text { text } if text == "final answer"
    ));
}

#[test]
fn channel_config_upstream_url_serialization_roundtrip() {
    let json_input = json!({
        "id": "test",
        "name": "Test Channel",
        "enabled": true,
        "upstreamUrl": "https://api.openai.com/v1"
    });
    let parsed: ChannelConfig =
        serde_json::from_value(json_input).expect("should parse upstreamUrl");
    assert_eq!(parsed.base_url, "https://api.openai.com/v1");

    let serialized = serde_json::to_value(&parsed).expect("should serialize");
    assert_eq!(serialized["upstreamUrl"], "https://api.openai.com/v1");
}

#[test]
fn opencode_channel_detection_covers_id_protocol_alias_url_and_name() {
    let make =
        |id: &str, protocol: &str, alias: Option<&str>, base_url: &str, name: &str| ChannelConfig {
            id: id.to_string(),
            name: name.to_string(),
            description: "".to_string(),
            enabled: true,
            protocol: protocol.to_string(),
            base_url: base_url.to_string(),
            api_key: "sk-test".to_string(),
            api_keys: None,
            use_proxy_pool: false,
            alias: alias.map(|s| s.to_string()),
            site_id: None,
            use_fixed_proxy: false,
            fixed_proxy_node: None,
            priority: None,
            weight: None,
            enabled_models: None,
            model_redirects: None,
            rate_limit_rpm: None,
            stats_id: None,
            key_groups: None,
            key_rules: None,
            model_proxy_rules: None,
        };

    assert!(is_opencode_channel(&make(
        "opencode",
        "openai",
        None,
        "https://example.com/v1",
        "任意名称"
    )));
    assert!(is_opencode_channel(&make(
        "custom",
        "opencode",
        None,
        "https://example.com/v1",
        "任意名称"
    )));
    assert!(is_opencode_channel(&make(
        "custom",
        "openai",
        Some("opencode"),
        "https://example.com/v1",
        "任意名称"
    )));
    assert!(is_opencode_channel(&make(
        "custom",
        "openai",
        None,
        "https://opencode.ai/zen/v1",
        "任意名称"
    )));
    assert!(is_opencode_channel(&make(
        "custom",
        "openai",
        None,
        "https://example.com/v1",
        "OpenCode 免费"
    )));

    // 非 OpenCode 渠道不应被过滤
    assert!(!is_opencode_channel(&make(
        "openai-main",
        "openai",
        Some("openai"),
        "https://api.openai.com/v1",
        "OpenAI 官方"
    )));
    assert!(!is_opencode_channel(&make(
        "custom",
        "openai",
        None,
        "https://api.deepseek.com/v1",
        "DeepSeek"
    )));
}

#[test]
fn opencode_free_model_filter_keeps_only_free_models() {
    // 与 OpenCode zen /v1/models 实际返回一致的抽样
    let upstream = [
        "claude-sonnet-5",
        "gpt-5.1-codex-max",
        "gemini-3.5-flash",
        "grok-4.6",
        "deepseek-v4-pro",
        "kimi-k3",
        "big-pickle",
        "deepseek-v4-flash-free",
        "mimo-v2.5-free",
        "nemotron-3-ultra-free",
        "laguna-s-2.1-free",
        "muse-spark-1.2-contributor-free",
    ];
    let kept: Vec<&str> = upstream
        .iter()
        .copied()
        .filter(|m| is_free_opencode_model(m))
        .collect();
    assert_eq!(
        kept,
        vec![
            "big-pickle",
            "deepseek-v4-flash-free",
            "mimo-v2.5-free",
            "nemotron-3-ultra-free",
            "laguna-s-2.1-free",
            "muse-spark-1.2-contributor-free",
        ]
    );
}

#[tokio::test]
async fn model_proxy_shared_routes_toggle_without_binding_a_second_port() {
    let state = ModelProxyState::new_with_app(None);
    assert!(!state
        .context
        .route_enabled
        .load(std::sync::atomic::Ordering::Acquire));

    start_model_proxy_server(&state)
        .await
        .expect("共享路由启动应成功");
    assert!(state
        .context
        .route_enabled
        .load(std::sync::atomic::Ordering::Acquire));

    stop_model_proxy_server(&state)
        .await
        .expect("共享路由停止应成功");
    assert!(!state
        .context
        .route_enabled
        .load(std::sync::atomic::Ordering::Acquire));
}

#[tokio::test]
async fn gateway_auth_accepts_headers_and_rejects_query_or_missing_keys() {
    use axum::http::{HeaderMap, HeaderValue, Uri};
    let config = ModelProxyConfig {
        api_key: "sk-test".to_string(),
        ..ModelProxyConfig::default()
    };
    let uri: Uri = "/v1/models?key=sk-test".parse().unwrap();

    let mut bearer = HeaderMap::new();
    bearer.insert("authorization", HeaderValue::from_static("Bearer sk-test"));
    assert!(check_auth(&bearer, &uri, &config).await.is_ok());

    let mut x_api = HeaderMap::new();
    x_api.insert("x-api-key", HeaderValue::from_static("sk-test"));
    assert!(check_auth(&x_api, &"/v1/models".parse().unwrap(), &config)
        .await
        .is_ok());

    let empty = HeaderMap::new();
    assert!(check_auth(&empty, &uri, &config).await.is_err());

    let mut wrong = HeaderMap::new();
    wrong.insert("authorization", HeaderValue::from_static("Bearer wrong"));
    assert!(check_auth(&wrong, &uri, &config).await.is_err());

    let empty_config = ModelProxyConfig {
        api_key: String::new(),
        ..config
    };
    assert!(
        check_auth(&bearer, &"/v1/models".parse().unwrap(), &empty_config)
            .await
            .is_err()
    );
}
#[test]
fn current_timestamp_format_is_standard_datetime() {
    let ts = current_timestamp();
    // 验证格式为 "YYYY-MM-DD HH:MM:SS" (19 字符)
    assert_eq!(ts.len(), 19);
    assert_eq!(&ts[4..5], "-");
    assert_eq!(&ts[7..8], "-");
    assert_eq!(&ts[10..11], " ");
    assert_eq!(&ts[13..14], ":");
    assert_eq!(&ts[16..17], ":");
    assert!(ts.chars().take(4).all(|c| c.is_ascii_digit()));
}

#[test]
fn attempt_request_id_formatting_and_node_cycling() {
    let base_req_id = "req_123456";
    let candidates = vec![
        "__direct__".to_string(),
        "node_a".to_string(),
        "node_b".to_string(),
    ];
    let max_retries = 3;
    let total_attempts_allowed = max_retries + 1; // 4 次尝试
    let base_node_idx = 0;

    let mut attempt_ids = Vec::new();
    let mut selected_nodes = Vec::new();

    for attempt_idx in 0..total_attempts_allowed {
        let cand_id = &candidates[(base_node_idx + attempt_idx) % candidates.len()];
        let attempt_req_id = if attempt_idx == 0 {
            base_req_id.to_string()
        } else {
            format!("{base_req_id}#{}", attempt_idx + 1)
        };
        attempt_ids.push(attempt_req_id);
        selected_nodes.push(cand_id.as_str());
    }

    assert_eq!(
        attempt_ids,
        vec!["req_123456", "req_123456#2", "req_123456#3", "req_123456#4"]
    );
    assert_eq!(
        selected_nodes,
        vec!["__direct__", "node_a", "node_b", "__direct__"]
    );
}

#[test]
fn node_switching_only_on_429_and_persists_for_next_requests() {
    let state = ModelProxyState::new_with_app(None);
    let candidates = vec![
        "__direct__".to_string(),
        "proxy_node_1".to_string(),
        "proxy_node_2".to_string(),
    ];

    // 1. 正常成功请求：保持当前活跃节点（直连）不变
    let base_idx_req1 = state
        .context
        .node_round_robin
        .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(candidates[base_idx_req1 % candidates.len()], "__direct__");

    let base_idx_req2 = state
        .context
        .node_round_robin
        .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(candidates[base_idx_req2 % candidates.len()], "__direct__");

    // 2. 发生 429：推进活跃节点游标
    state
        .context
        .node_round_robin
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    // 3. 下一次新请求到来：自动继承并使用新节点（proxy_node_1）
    let base_idx_req3 = state
        .context
        .node_round_robin
        .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(candidates[base_idx_req3 % candidates.len()], "proxy_node_1");

    let base_idx_req4 = state
        .context
        .node_round_robin
        .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(candidates[base_idx_req4 % candidates.len()], "proxy_node_1");

    // 4. 再次遇到 429：切换到 proxy_node_2
    state
        .context
        .node_round_robin
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let base_idx_req5 = state
        .context
        .node_round_robin
        .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(candidates[base_idx_req5 % candidates.len()], "proxy_node_2");

    // 5. 循环回直连
    state
        .context
        .node_round_robin
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let base_idx_req6 = state
        .context
        .node_round_robin
        .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(candidates[base_idx_req6 % candidates.len()], "__direct__");
}

#[tokio::test]
async fn logger_helper_records_failure_and_increments_metrics() {
    let state = ModelProxyState::new_with_app(None);
    let ctx = &state.context;

    let initial_failed = ctx
        .metrics
        .failed_requests
        .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(initial_failed, 0);

    record_attempt_failure(
        ctx,
        ProxyLogParams::new_failure(
            "req_test_999".to_string(),
            "/v1/chat/completions".to_string(),
            "opencode".to_string(),
            "gpt-4o".to_string(),
            false,
            429,
            150,
            Some("Rate limit exceeded".to_string()),
            Some("{\"model\":\"gpt-4o\"}".to_string()),
            Some("直连通道".to_string()),
        ),
    )
    .await;

    let after_failed = ctx
        .metrics
        .failed_requests
        .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(after_failed, 1);
}

#[test]
fn egress_request_meta_constructs_properly() {
    let meta = EgressRequestMeta {
        req_id: "req_abc".to_string(),
        path: "/v1/chat/completions".to_string(),
        channel_id: "chan_1".to_string(),
        channel_stats_id: Some("101".to_string()),
        model: "claude-3-7-sonnet".to_string(),
        stream: true,
        req_body_str: Some("{}".to_string()),
    };
    assert_eq!(meta.req_id, "req_abc");
    assert!(meta.stream);
}

#[test]
fn opencode_free_and_non_free_model_classification() {
    // 免费模型
    assert!(is_free_opencode_model("deepseek-v4-flash-free"));
    assert!(is_free_opencode_model("opencode/deepseek-v4-flash-free"));
    assert!(is_free_opencode_model("big-pickle"));
    assert!(is_free_opencode_model("opencode/big-pickle"));
    assert!(is_free_opencode_model("mimo-v2.5-free"));
    assert!(is_free_opencode_model("nemotron-3-ultra-free"));
    assert!(is_free_opencode_model("x-preview-f-free"));

    // 付费模型（需携带 Key）
    assert!(!is_free_opencode_model("gpt-4o"));
    assert!(!is_free_opencode_model("opencode/gpt-4o"));
    assert!(!is_free_opencode_model("claude-3-7-sonnet"));
    assert!(!is_free_opencode_model("claude-sonnet-5"));
    assert!(!is_free_opencode_model("deepseek-v4-pro"));
    assert!(!is_free_opencode_model("glm-5.2"));
}

#[tokio::test]
async fn opencode_model_compatibility_and_anonymous_mode() {
    let ch_no_key = ChannelConfig {
        id: "opencode".to_string(),
        name: "OpenCode".to_string(),
        description: "".to_string(),
        enabled: true,
        protocol: "openai".to_string(),
        base_url: "https://opencode.ai/zen/v1".to_string(),
        api_key: "".to_string(),
        api_keys: None,
        use_proxy_pool: false,
        alias: Some("opencode".to_string()),
        site_id: None,
        use_fixed_proxy: false,
        fixed_proxy_node: None,
        priority: None,
        weight: None,
        enabled_models: None,
        model_redirects: None,
        rate_limit_rpm: None,
        stats_id: None,
        key_groups: None,
        key_rules: None,
        model_proxy_rules: None,
    };

    let state = ModelProxyState::new_with_app(None);
    let selected_key = select_channel_api_key(&state.context, &ch_no_key).await;
    // 匿名模式：未配置 Key 时始终为空字符串
    assert!(selected_key.is_empty());

    // 免费模型在无 Key 匿名模式下通过校验
    assert!(
        check_model_channel_compatibility(&ch_no_key, "deepseek-v4-flash-free", &selected_key)
            .is_ok()
    );
    assert!(check_model_channel_compatibility(&ch_no_key, "big-pickle", &selected_key).is_ok());
    assert!(check_model_channel_compatibility(&ch_no_key, "mimo-v2.5-free", &selected_key).is_ok());

    // 付费模型在无 Key 匿名模式下前置拦截
    assert!(check_model_channel_compatibility(&ch_no_key, "gpt-4o", &selected_key).is_err());
    assert!(
        check_model_channel_compatibility(&ch_no_key, "claude-3-7-sonnet", &selected_key).is_err()
    );

    // 配置了 Key 后付费模型通过校验
    let ch_explicit_key = ChannelConfig {
        api_key: "sk-custom-key".to_string(),
        ..ch_no_key
    };
    let selected_explicit = select_channel_api_key(&state.context, &ch_explicit_key).await;
    assert_eq!(selected_explicit, "sk-custom-key");
    assert!(
        check_model_channel_compatibility(&ch_explicit_key, "gpt-4o", &selected_explicit).is_ok()
    );
}

fn stats_channel(id: &str, alias: Option<&str>, stats_id: Option<u32>) -> ChannelConfig {
    ChannelConfig {
        id: id.to_string(),
        name: format!("Channel {id}"),
        description: String::new(),
        enabled: true,
        protocol: "openai".to_string(),
        base_url: "https://api.example.com/v1".to_string(),
        api_key: String::new(),
        api_keys: None,
        use_proxy_pool: false,
        alias: alias.map(|s| s.to_string()),
        site_id: None,
        use_fixed_proxy: false,
        fixed_proxy_node: None,
        priority: None,
        weight: None,
        enabled_models: None,
        model_redirects: None,
        rate_limit_rpm: None,
        stats_id,
        key_groups: None,
        key_rules: None,
        model_proxy_rules: None,
    }
}

#[test]
fn assigns_stable_stats_ids_with_reserved_builtin_band() {
    use crate::model::gateway::config::sanitize_model_proxy_config;

    // opencode 固定为 1（即便被篡改），动态渠道从 101 起分配
    let mut cfg = ModelProxyConfig {
        channels: vec![
            stats_channel("site_a", Some("alpha"), None),
            {
                let mut oc = stats_channel("opencode", Some("tampered"), Some(999));
                oc.base_url = "https://opencode.ai/zen/v1".to_string();
                oc
            },
            stats_channel("site_b", Some("beta"), None),
        ],
        ..ModelProxyConfig::default()
    };
    sanitize_model_proxy_config(&mut cfg);

    let by_id = |id: &str| cfg.channels.iter().find(|c| c.id == id).unwrap();
    assert_eq!(by_id("opencode").stats_id, Some(1));
    assert_eq!(by_id("site_a").stats_id, Some(101));
    assert_eq!(by_id("site_b").stats_id, Some(102));
    assert_eq!(cfg.next_channel_stats_id, 103);
    // 统计维度键与别名解耦
    assert_eq!(by_id("site_a").stats_key(), "101");

    // 改别名后 ID 不变；重复 sanitize 幂等，计数器不回退
    let mut renamed = cfg.clone();
    renamed.channels[0].alias = Some("renamed".to_string());
    renamed.channels[0].stats_id = None;
    renamed.channels.pop();
    sanitize_model_proxy_config(&mut renamed);
    assert_eq!(renamed.channels[0].stats_id, Some(103));
    assert_eq!(renamed.channels[0].stats_key(), "103");
    assert_eq!(renamed.channels[1].stats_id, Some(1));
    assert!(renamed.next_channel_stats_id >= 103);

    // 已分配 ID 的渠道保持不变
    let mut again = renamed.clone();
    sanitize_model_proxy_config(&mut again);
    assert_eq!(again.channels[0].stats_id, Some(103));
    assert_eq!(again.channels[1].stats_id, Some(1));
}

#[test]
fn legacy_config_json_without_stats_id_still_parses() {
    let raw = serde_json::json!({
        "enabled": true,
        "port": 8088,
        "apiKey": "",
        "timeoutSeconds": 300,
        "channels": [{
            "id": "legacy",
            "name": "Legacy",
            "enabled": true,
            "upstreamUrl": "https://api.example.com/v1",
            "alias": "old-alias"
        }]
    });
    let cfg: ModelProxyConfig = serde_json::from_value(raw).expect("legacy config should parse");
    assert!(cfg.channels[0].stats_id.is_none());
    assert_eq!(cfg.next_channel_stats_id, 101);
    assert_eq!(cfg.channels[0].stats_key(), "old-alias");
}

// ---------------------------------------------------------------------------
// OpenCode 503 同节点原地重试（不受 max_retries 名额约束）
// ---------------------------------------------------------------------------

/// 启动一个按脚本应答的本地 mock 上游：第 N 次请求返回 script[N]（越界时重复末项）。
/// 返回其地址与请求计数器。
async fn spawn_scripted_upstream(
    script: Vec<(axum::http::StatusCode, &'static str)>,
) -> (
    std::net::SocketAddr,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    use axum::{routing::post, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    assert!(!script.is_empty(), "script 至少包含一项");
    let counter = Arc::new(AtomicUsize::new(0));
    let script = Arc::new(script);
    let script_for_route = script.clone();
    let counter_for_route = counter.clone();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let script = script_for_route.clone();
            let counter = counter_for_route.clone();
            async move {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                let (status, body) = if n < script.len() {
                    script[n]
                } else {
                    script[script.len() - 1]
                };
                (status, body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, counter)
}

fn valid_chat_payload() -> &'static str {
    r#"{"choices":[{"index":0,"message":{"role":"assistant","content":"你好"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#
}

fn egress_test_channel(id: &str, base_url: String) -> ChannelConfig {
    ChannelConfig {
        id: id.to_string(),
        name: id.to_string(),
        description: String::new(),
        enabled: true,
        protocol: "openai".to_string(),
        base_url,
        api_key: String::new(),
        api_keys: None,
        // 直连：避免单测触碰代理池运行时
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
        stats_id: None,
        key_groups: None,
        key_rules: None,
        model_proxy_rules: None,
    }
}

async fn run_egress(
    channel: &ChannelConfig,
    max_retries: u32,
) -> Result<EgressSuccess, axum::response::Response> {
    use std::sync::atomic::Ordering;
    let state = ModelProxyState::new_with_app(None);
    let ctx = &state.context;
    ctx.route_enabled.store(true, Ordering::Release);
    let config = ModelProxyConfig {
        enabled: true,
        max_retries,
        ..ModelProxyConfig::default()
    };
    let upstream_url = format!(
        "{}/chat/completions",
        channel.base_url.trim_end_matches('/')
    );
    let meta = EgressRequestMeta {
        req_id: "req_503test".to_string(),
        path: "/v1/chat/completions".to_string(),
        channel_id: channel.id.clone(),
        channel_stats_id: None,
        model: "deepseek-v4-flash-free".to_string(),
        stream: false,
        req_body_str: None,
    };
    execute_resilient_egress(
        ctx,
        channel,
        &config,
        meta,
        &upstream_url,
        "",
        &json!({ "model": "deepseek-v4-flash-free" }),
        crate::model::gateway::pipeline::ClientProtocol::OpenAi,
    )
    .await
}

#[tokio::test]
async fn opencode_503_retries_inplace_once_then_succeeds_without_consuming_retry_budget() {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    // max_retries=0：常规预算只有 1 次尝试，503 原地重试必须独立于该预算生效
    let (addr, counter) = spawn_scripted_upstream(vec![
        (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            r#"{"error":{"message":"upstream unavailable"}}"#,
        ),
        (axum::http::StatusCode::OK, valid_chat_payload()),
    ])
    .await;
    let channel = egress_test_channel("opencode", format!("http://{addr}/v1"));

    let started = Instant::now();
    let success = run_egress(&channel, 0)
        .await
        .expect("首次 503 后原地重试应成功");
    let elapsed = started.elapsed();

    assert_eq!(success.status, 200);
    assert_eq!(
        counter.load(Ordering::SeqCst),
        2,
        "同一节点应恰好发送 2 次（首次 + 原地重试），且不切换节点"
    );
    assert!(
        elapsed >= Duration::from_millis(1000),
        "原地重试前必须等待 1 秒"
    );
    assert!(elapsed < Duration::from_millis(3000));
}

#[tokio::test]
async fn opencode_persistent_503_gets_one_inplace_retry_per_node_before_switching() {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    // max_retries=1：两轮尝试（两个候选位），每轮各享一次免费原地重试 → 共 4 次请求
    let (addr, counter) = spawn_scripted_upstream(vec![(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        r#"{"error":{"message":"upstream unavailable"}}"#,
    )])
    .await;
    let channel = egress_test_channel("opencode", format!("http://{addr}/v1"));

    let started = Instant::now();
    let result = run_egress(&channel, 1).await;

    assert!(result.is_err(), "持续 503 最终必须返回错误");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        4,
        "每轮尝试应含一次 503 原地重试（2 轮 × 2 次），随后才切换节点"
    );
    assert!(started.elapsed() >= Duration::from_millis(2000));
}

#[tokio::test]
async fn non_opencode_channel_does_not_inplace_retry_on_503() {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    // 5xx 原地重试已通用化：转发渠道/普通渠道同样享受每节点一次的免费原地重试。
    // max_retries=0 时仅一次机会，503 后原地重试成功即返回 200。
    let (addr, counter) = spawn_scripted_upstream(vec![
        (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            r#"{"error":{"message":"upstream unavailable"}}"#,
        ),
        (axum::http::StatusCode::OK, valid_chat_payload()),
    ])
    .await;
    let channel = egress_test_channel("plain-upstream", format!("http://{addr}/v1"));

    let success = run_egress(&channel, 0)
        .await
        .expect("普通渠道首次 503 后原地重试应成功");
    assert_eq!(success.status, 200);
    assert_eq!(counter.load(Ordering::SeqCst), 2, "同节点应恰好 2 次");
}

#[tokio::test]
async fn opencode_502_retries_inplace_once_then_succeeds() {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    // 502 与 503 同等对待：max_retries=0 时依然获得一次免费原地重试
    let (addr, counter) = spawn_scripted_upstream(vec![
        (
            axum::http::StatusCode::BAD_GATEWAY,
            r#"{"error":{"message":"bad gateway"}}"#,
        ),
        (axum::http::StatusCode::OK, valid_chat_payload()),
    ])
    .await;
    let channel = egress_test_channel("opencode", format!("http://{addr}/v1"));

    let started = Instant::now();
    let success = run_egress(&channel, 0)
        .await
        .expect("首次 502 后原地重试应成功");
    let elapsed = started.elapsed();

    assert_eq!(success.status, 200);
    assert_eq!(counter.load(Ordering::SeqCst), 2);
    assert!(elapsed >= Duration::from_millis(1000));
}

#[tokio::test]
async fn opencode_empty_200_payload_retries_inplace_then_succeeds() {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    // OpenCode 官方缺陷：返回 200 但响应体为空内容 → 必须与 5xx 一样原地重试。
    // 注意：仅有思考（reasoning_content）没有正文属于有效负载（P1-9），
    // 纯推理模型可能整段只输出思考，不再按空内容处理。
    for empty_payload in [
        "",
        r#"{"choices":[]}"#,
        r#"{"choices":[{"index":0,"message":{"role":"assistant","content":null},"finish_reason":"stop"}],"usage":{"prompt_tokens":10}}"#,
    ] {
        let (addr, counter) = spawn_scripted_upstream(vec![
            (axum::http::StatusCode::OK, empty_payload),
            (axum::http::StatusCode::OK, valid_chat_payload()),
        ])
        .await;
        let channel = egress_test_channel("opencode", format!("http://{addr}/v1"));

        let started = Instant::now();
        let success = run_egress(&channel, 0)
            .await
            .unwrap_or_else(|_| panic!("空内容「{empty_payload}」重试后应成功"));
        let elapsed = started.elapsed();

        assert_eq!(success.status, 200);
        assert_eq!(
            counter.load(Ordering::SeqCst),
            2,
            "空内容「{empty_payload}」应触发恰好一次原地重试"
        );
        assert!(elapsed >= Duration::from_millis(1000));
        // 重试成功的响应体必须完好可读（预读打包回 Response 的链路不能丢数据）
        let body = success.response.bytes().await.expect("响应体应可读取");
        assert_eq!(body, valid_chat_payload().as_bytes());
    }
}

#[tokio::test]
async fn opencode_persistent_empty_payload_returns_400_after_budget_exhausted() {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    // max_retries=0：单节点一次机会 —— 空内容原地重试仍为空后直接以 400 返回客户端
    let (addr, counter) =
        spawn_scripted_upstream(vec![(axum::http::StatusCode::OK, r#"{"choices":[]}"#)]).await;
    let channel = egress_test_channel("opencode", format!("http://{addr}/v1"));

    let started = Instant::now();
    let err = match run_egress(&channel, 0).await {
        Ok(_) => panic!("持续空内容必须按错误返回"),
        Err(resp) => resp,
    };
    let elapsed = started.elapsed();

    assert_eq!(
        err.status(),
        axum::http::StatusCode::BAD_REQUEST,
        "空返回最终必须以 400 状态码返回客户端"
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        2,
        "单节点应恰好发送 2 次（首次 + 原地重试）"
    );
    assert!(elapsed >= Duration::from_millis(1000));
}

#[tokio::test]
async fn opencode_empty_payload_participates_in_node_rotation_before_400() {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    // max_retries=1：两轮候选位，每轮含一次免费原地重试 → 共 4 次请求后以 400 收尾
    let (addr, counter) = spawn_scripted_upstream(vec![(axum::http::StatusCode::OK, "")]).await;
    let channel = egress_test_channel("opencode", format!("http://{addr}/v1"));

    let started = Instant::now();
    let err = match run_egress(&channel, 1).await {
        Ok(_) => panic!("持续空内容必须按错误返回"),
        Err(resp) => resp,
    };

    assert_eq!(err.status(), axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(counter.load(Ordering::SeqCst), 4);
    assert!(started.elapsed() >= Duration::from_millis(2000));
}

#[tokio::test]
async fn opencode_valid_200_payload_does_not_retry() {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    // 正常内容不得误判为空：仅发送 1 次、无等待
    let (addr, counter) =
        spawn_scripted_upstream(vec![(axum::http::StatusCode::OK, valid_chat_payload())]).await;
    let channel = egress_test_channel("opencode", format!("http://{addr}/v1"));

    let started = Instant::now();
    let success = run_egress(&channel, 0).await.expect("正常响应应直接成功");
    let elapsed = started.elapsed();

    assert_eq!(success.status, 200);
    assert_eq!(counter.load(Ordering::SeqCst), 1, "有效内容不应重试");
    assert!(elapsed < Duration::from_millis(2500));

    // 纯文本/HTML 等 200 负载不属于「空内容」，同样不重试
    let (addr, counter) =
        spawn_scripted_upstream(vec![(axum::http::StatusCode::OK, "<html>ok</html>")]).await;
    let channel = egress_test_channel("opencode", format!("http://{addr}/v1"));
    let _ = run_egress(&channel, 0).await;
    assert_eq!(counter.load(Ordering::SeqCst), 1);

    // 正文 + 思考并存是完整有效响应，不得误判为空内容
    let with_reasoning = r#"{"choices":[{"index":0,"message":{"role":"assistant","content":"最终答案","reasoning_content":"推理过程"},"finish_reason":"stop"}]}"#;
    let (addr, counter) =
        spawn_scripted_upstream(vec![(axum::http::StatusCode::OK, with_reasoning)]).await;
    let channel = egress_test_channel("opencode", format!("http://{addr}/v1"));
    let success = run_egress(&channel, 0).await.expect("正文+思考应直接成功");
    assert_eq!(success.status, 200);
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "有正文的响应（即便带思考）不应重试"
    );

    // 工具调用也是有效产出：无正文但有 tool_calls 不重试
    let tool_only = r#"{"choices":[{"index":0,"message":{"role":"assistant","content":"","tool_calls":[{"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a.rs\"}"}}]},"finish_reason":"tool_calls"}]}"#;
    let (addr, counter) =
        spawn_scripted_upstream(vec![(axum::http::StatusCode::OK, tool_only)]).await;
    let channel = egress_test_channel("opencode", format!("http://{addr}/v1"));
    let _ = run_egress(&channel, 0).await;
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "tool_calls 属于有效负载，不应重试"
    );
}

#[test]
fn anthropic_nonstream_response_preserves_cache_and_reasoning() {
    // 非流式保真回归：归一化口径 prompt_tokens 为总量（含缓存），
    // Anthropic 出口必须拆分 input/cache_read/cache_creation，且保留 thinking 块
    let openai_resp = json!({
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "答案正文",
                "reasoning_content": "推理过程"
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 1000,
            "completion_tokens": 50,
            "total_tokens": 1050,
            "prompt_tokens_details": {
                "cached_tokens": 800,
                "cache_creation_tokens": 100
            },
            "completion_tokens_details": { "reasoning_tokens": 30 }
        }
    });

    let resp = AnthropicProtocolAdapter::openai_response_to_anthropic(&openai_resp, "req9", "m");
    assert_eq!(
        resp["usage"]["input_tokens"], 100,
        "input 必须扣除缓存命中与写入"
    );
    assert_eq!(resp["usage"]["cache_read_input_tokens"], 800);
    assert_eq!(resp["usage"]["cache_creation_input_tokens"], 100);
    // P0-3：归一化 completion_tokens 已含推理（50 含 reasoning 30），
    // Anthropic output_tokens 直接透传，不得再叠加 reasoning 造成双重计数
    assert_eq!(
        resp["usage"]["output_tokens"], 50,
        "output 直接透传含推理的 completion"
    );
    let blocks = resp["content"].as_array().unwrap();
    assert_eq!(blocks[0]["type"], "thinking");
    assert_eq!(blocks[0]["thinking"], "推理过程");
    assert_eq!(blocks[1]["type"], "text");
    assert_eq!(blocks[1]["text"], "答案正文");
}

#[test]
fn sanitize_clears_keys_for_site_linked_channels_only() {
    use crate::model::gateway::config::sanitize_model_proxy_config;

    let mut cfg = ModelProxyConfig::default();
    // 站点关联渠道：历史配置中残留了站点 Key
    cfg.channels.push(ChannelConfig {
        id: "site_local-1".to_string(),
        name: "Fengwind API".to_string(),
        description: String::new(),
        enabled: true,
        protocol: "openai".to_string(),
        base_url: "https://api.fengwind.com/".to_string(),
        api_key: "sk-leaked-site-key".to_string(),
        api_keys: Some(vec!["sk-leaked-site-key".to_string(), "sk-2".to_string()]),
        use_proxy_pool: false,
        alias: Some("fengwind-api".to_string()),
        site_id: Some("local-1".to_string()),
        use_fixed_proxy: false,
        fixed_proxy_node: None,
        priority: None,
        weight: None,
        enabled_models: None,
        model_redirects: None,
        rate_limit_rpm: None,
        stats_id: None,
        key_groups: None,
        key_rules: None,
        model_proxy_rules: None,
    });
    // 手工渠道：Key 必须原样保留
    cfg.channels.push(ChannelConfig {
        id: "manual".to_string(),
        name: "Manual".to_string(),
        description: String::new(),
        enabled: true,
        protocol: "openai".to_string(),
        base_url: "https://api.manual.com/v1".to_string(),
        api_key: "sk-manual".to_string(),
        api_keys: Some(vec!["sk-manual".to_string(), "sk-manual-2".to_string()]),
        use_proxy_pool: false,
        alias: Some("manual".to_string()),
        site_id: None,
        use_fixed_proxy: false,
        fixed_proxy_node: None,
        priority: None,
        weight: None,
        enabled_models: None,
        model_redirects: None,
        rate_limit_rpm: None,
        stats_id: None,
        key_groups: None,
        key_rules: None,
        model_proxy_rules: None,
    });

    sanitize_model_proxy_config(&mut cfg);

    let site_ch = cfg
        .channels
        .iter()
        .find(|c| c.id == "site_local-1")
        .unwrap();
    assert!(site_ch.api_key.is_empty(), "站点渠道 apiKey 必须被清空");
    assert!(
        site_ch.api_keys.is_none(),
        "站点渠道 apiKeys 必须被清空，避免旧 Key 复活"
    );

    let manual_ch = cfg.channels.iter().find(|c| c.id == "manual").unwrap();
    assert_eq!(manual_ch.api_key, "sk-manual", "手工渠道 apiKey 不受影响");
    assert_eq!(
        manual_ch.api_keys.as_deref(),
        Some(&["sk-manual".to_string(), "sk-manual-2".to_string()][..]),
        "手工渠道 apiKeys 不受影响"
    );
}

#[tokio::test]
async fn resolve_channel_api_keys_reads_site_cache_and_dedupes() {
    use crate::context::AppContext;
    use crate::proxypool::ProxyRuntime;

    let root = std::env::temp_dir().join(format!(
        "openhub-gateway-site-key-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    let database =
        std::sync::Arc::new(crate::models::Database::open(&root.join("sites.sqlite3")).unwrap());
    {
        let conn = database.lock_conn().unwrap();
        conn.execute_batch(
            "INSERT INTO directory_sites
                (id, name, description, registration_limit, icon, api_base_url,
                 supports_immersive_translation, supports_ldc, supports_checkin, supports_nsfw,
                 checkin_url, checkin_note, benefit_url, rate_limit, status_url,
                 is_only_maintainer_visible, requires_invite_code, is_runaway, is_fake_charity,
                 has_pending_report, is_personal, use_system_proxy, use_proxy_pool, favorite, hidden)
             VALUES
                ('local-1', 'Fengwind', '', 0, '', 'https://api.fengwind.com/',
                 0, 0, 0, 0, '', '', '', '', '',
                 0, 0, 0, 0, 0, 1, 0, 0, 0, 0);
             INSERT INTO site_model_cache (site_id, profile_id, keys_json, models_json, groups_json, key_models_json)
                VALUES ('local-1', 'Profile 11', '[\"sk-99130cfe8\",\"sk-0fb61dc28\"]', '[]', '{}', '{}');
             INSERT INTO site_model_cache (site_id, profile_id, keys_json, models_json, groups_json, key_models_json)
                VALUES ('local-1', 'Profile 15', '[\"sk-99130cfe8\",\"sk-1ff241319\"]', '[]', '{}', '{}');",
        )
        .unwrap();
    }

    let app_ctx = std::sync::Arc::new(AppContext {
        database: database.clone(),
        proxy_runtime: std::sync::Arc::new(ProxyRuntime::new(root.join("proxy-runtime"))),
        charity_runtime: std::sync::Arc::new(crate::charity::CharityMonitorRuntime::new()),
        model_catalog_runtime: std::sync::Arc::new(
            crate::model::catalog::ModelCatalogRuntime::new(),
        ),
        event_bus: crate::context::EventBus::new(),
        data_dir: root.clone(),
        resource_dir: None,
        capabilities: crate::context::Capabilities::server_defaults(),
        login: crate::context::LoginManager::new("admin".into(), "password".into()),
    });

    let state = ModelProxyState::new();
    state.attach_ctx(app_ctx).await;

    // 站点渠道：配置中即使残留旧 Key，运行时也只从 site_model_cache 读取
    let site_channel = ChannelConfig {
        id: "site_local-1".to_string(),
        name: "Fengwind API".to_string(),
        description: String::new(),
        enabled: true,
        protocol: "openai".to_string(),
        base_url: "https://api.fengwind.com/".to_string(),
        api_key: "sk-leaked".to_string(),
        api_keys: Some(vec!["sk-leaked".to_string()]),
        use_proxy_pool: false,
        alias: Some("fengwind-api".to_string()),
        site_id: Some("local-1".to_string()),
        use_fixed_proxy: false,
        fixed_proxy_node: None,
        priority: None,
        weight: None,
        enabled_models: None,
        model_redirects: None,
        rate_limit_rpm: None,
        stats_id: None,
        key_groups: None,
        key_rules: None,
        model_proxy_rules: None,
    };
    let keys = super::balancer::resolve_channel_api_keys(&state.context, &site_channel).await;
    // 合并两个 profile：sk-99130cfe8 跨账号重复，应去重；最终 3 个唯一 Key
    assert_eq!(keys.len(), 3, "跨账号 Key 必须去重：{keys:?}");
    assert!(keys.contains(&"sk-99130cfe8".to_string()));
    assert!(keys.contains(&"sk-0fb61dc28".to_string()));
    assert!(keys.contains(&"sk-1ff241319".to_string()));

    // 手工渠道：不受 site_model_cache 影响，仍使用自身配置 Key
    let manual_channel = ChannelConfig {
        id: "manual".to_string(),
        name: "Manual".to_string(),
        description: String::new(),
        enabled: true,
        protocol: "openai".to_string(),
        base_url: "https://api.manual.com/v1".to_string(),
        api_key: "sk-manual".to_string(),
        api_keys: None,
        use_proxy_pool: false,
        alias: Some("manual".to_string()),
        site_id: None,
        use_fixed_proxy: false,
        fixed_proxy_node: None,
        priority: None,
        weight: None,
        enabled_models: None,
        model_redirects: None,
        rate_limit_rpm: None,
        stats_id: None,
        key_groups: None,
        key_rules: None,
        model_proxy_rules: None,
    };
    let manual_keys =
        super::balancer::resolve_channel_api_keys(&state.context, &manual_channel).await;
    assert_eq!(manual_keys, vec!["sk-manual".to_string()]);

    // 站点缓存为空时不回退到渠道残留 Key
    let empty_site_channel = ChannelConfig {
        site_id: Some("non-existent".to_string()),
        ..manual_channel
    };
    let empty_keys =
        super::balancer::resolve_channel_api_keys(&state.context, &empty_site_channel).await;
    assert!(
        empty_keys.is_empty(),
        "站点缓存无 Key 时不得回退到渠道残留 Key"
    );

    let _ = std::fs::remove_dir_all(&root);
}
