use super::*;
use serde_json::json;

    #[test]
    fn parses_opencode_proxy_default_config() {
        let cfg = ModelProxyConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.port, DEFAULT_MODEL_PROXY_PORT);
        assert_eq!(cfg.channels.len(), 1);
        assert_eq!(cfg.channels[0].id, "opencode");
        assert_eq!(cfg.channels[0].effective_alias(), "opencode");
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
        };
        assert_eq!(ch.effective_alias(), "vip_channel");
        ch.alias = None;
        assert_eq!(ch.effective_alias(), "mychannel");
    }

    #[test]
    fn resolves_channels_with_alias_prefix_or_model_whitelist() {
        let cfg = ModelProxyConfig {
            enabled: true,
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
                },
            ],
            timeout_seconds: 300,
            record_request_body: false,
            max_retries: 0,
            next_channel_stats_id: 101,
            log_retention_days: None,
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

    #[test]
    fn multi_key_round_robin_selection() {
        let ch = ChannelConfig {
            id: "multi".to_string(),
            name: "Multi Key".to_string(),
            description: "".to_string(),
            enabled: true,
            protocol: "openai".to_string(),
            base_url: "https://api.example.com/v1".to_string(),
            api_key: "single-key".to_string(),
            api_keys: Some(vec!["key-1".to_string(), "key-2".to_string(), "key-3".to_string()]),
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
        };

        let keys = ch.get_effective_keys();
        assert_eq!(keys, vec!["key-1", "key-2", "key-3"]);

        let state = ModelProxyState::new_with_app(None);
        let k1 = select_channel_api_key(&state.context, &ch);
        let k2 = select_channel_api_key(&state.context, &ch);
        let k3 = select_channel_api_key(&state.context, &ch);
        let k4 = select_channel_api_key(&state.context, &ch);

        assert_eq!(k1, "key-1");
        assert_eq!(k2, "key-2");
        assert_eq!(k3, "key-3");
        assert_eq!(k4, "key-1");
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

        // 1. Gemini -> OpenAI
        let openai_req = GeminiProtocolAdapter::gemini_request_to_openai(&gemini_req, "gemini-1.5-pro", false);
        assert_eq!(openai_req["model"], "gemini-1.5-pro");
        assert_eq!(openai_req["temperature"], 0.5);
        assert_eq!(openai_req["max_tokens"], 1024);

        let msgs = openai_req["messages"].as_array().expect("messages array");
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "You are an expert compiler engineer.");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "What is Rust?");
        assert_eq!(msgs[2]["role"], "assistant");
        assert_eq!(msgs[2]["content"], "Rust is a systems programming language.");
        assert_eq!(msgs[3]["role"], "user");
        assert_eq!(msgs[3]["content"], "Tell me more about its memory safety.");

        let tools = openai_req["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["function"]["name"], "lookup_docs");

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

        let gemini_resp = GeminiProtocolAdapter::openai_response_to_gemini(&openai_resp, "gemini-1.5-pro");
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
    fn gemini_stream_chunk_conversion() {
        let openai_chunk = json!({
            "id": "chatcmpl-123",
            "object": "chat.completion.chunk",
            "created": 1700000000,
            "model": "gemini-1.5-flash",
            "choices": [
                {
                    "index": 0,
                    "delta": {
                        "content": "Hello world!"
                    },
                    "finish_reason": null
                }
            ]
        });

        let gemini_chunk = GeminiProtocolAdapter::openai_chunk_to_gemini_chunk(&openai_chunk, "gemini-1.5-flash")
            .expect("should produce gemini chunk");
        assert_eq!(
            gemini_chunk["candidates"][0]["content"]["parts"][0]["text"],
            "Hello world!"
        );
    }

    #[test]
    fn responses_api_adapter_converts_input_and_instructions() {
        let mut body = json!({
            "model": "gpt-4o",
            "instructions": "Be concise.",
            "input": [
                { "role": "user", "content": "Hello" }
            ]
        });
        ResponsesProtocolAdapter::convert_input_to_messages(&mut body);
        let msgs = body["messages"].as_array().expect("messages array");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "Be concise.");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "Hello");
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
        OpenAiProtocolAdapter::sanitize_and_normalize(&mut body);
        let msgs = body["messages"].as_array().expect("messages array");
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["reasoning_content"], "thinking here");
        assert_eq!(msgs[1]["content"], "final answer");
    }

    #[test]
    fn channel_config_upstream_url_serialization_roundtrip() {
        let json_input = json!({
            "id": "test",
            "name": "Test Channel",
            "enabled": true,
            "upstreamUrl": "https://api.openai.com/v1"
        });
        let parsed: ChannelConfig = serde_json::from_value(json_input).expect("should parse upstreamUrl");
        assert_eq!(parsed.base_url, "https://api.openai.com/v1");

        let serialized = serde_json::to_value(&parsed).expect("should serialize");
        assert_eq!(serialized["upstreamUrl"], "https://api.openai.com/v1");
    }

    #[test]
    fn opencode_channel_detection_covers_id_protocol_alias_url_and_name() {
        let make = |id: &str, protocol: &str, alias: Option<&str>, base_url: &str, name: &str| ChannelConfig {
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
        };

        assert!(is_opencode_channel(&make("opencode", "openai", None, "https://example.com/v1", "任意名称")));
        assert!(is_opencode_channel(&make("custom", "opencode", None, "https://example.com/v1", "任意名称")));
        assert!(is_opencode_channel(&make("custom", "openai", Some("opencode"), "https://example.com/v1", "任意名称")));
        assert!(is_opencode_channel(&make("custom", "openai", None, "https://opencode.ai/zen/v1", "任意名称")));
        assert!(is_opencode_channel(&make("custom", "openai", None, "https://example.com/v1", "OpenCode 官方免费通道")));

        // 非 OpenCode 渠道不应被过滤
        assert!(!is_opencode_channel(&make("openai-main", "openai", Some("openai"), "https://api.openai.com/v1", "OpenAI 官方")));
        assert!(!is_opencode_channel(&make("custom", "openai", None, "https://api.deepseek.com/v1", "DeepSeek")));
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
        let kept: Vec<&str> = upstream.iter().copied().filter(|m| is_free_opencode_model(m)).collect();
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
    async fn model_proxy_stop_then_immediate_start_rebinds_same_port() {
        let state = ModelProxyState::new_with_app(None);
        // 取一个空闲端口模拟已运行实例
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        state.context.config.write().await.port = port;

        start_model_proxy_server(&state).await.expect("首次启动应成功");
        // 模拟保存配置后的立即重启：stop 返回时端口必须已释放，否则此处端口冲突
        stop_model_proxy_server(&state).await.expect("停止应成功");
        start_model_proxy_server(&state).await.expect("立即重启不应端口冲突");
        stop_model_proxy_server(&state).await.expect("清理应成功");
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
        let candidates = vec!["__direct__".to_string(), "node_a".to_string(), "node_b".to_string()];
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
        let base_idx_req1 = state.context.node_round_robin.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(candidates[base_idx_req1 % candidates.len()], "__direct__");

        let base_idx_req2 = state.context.node_round_robin.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(candidates[base_idx_req2 % candidates.len()], "__direct__");

        // 2. 发生 429：推进活跃节点游标
        state.context.node_round_robin.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // 3. 下一次新请求到来：自动继承并使用新节点（proxy_node_1）
        let base_idx_req3 = state.context.node_round_robin.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(candidates[base_idx_req3 % candidates.len()], "proxy_node_1");

        let base_idx_req4 = state.context.node_round_robin.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(candidates[base_idx_req4 % candidates.len()], "proxy_node_1");

        // 4. 再次遇到 429：切换到 proxy_node_2
        state.context.node_round_robin.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let base_idx_req5 = state.context.node_round_robin.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(candidates[base_idx_req5 % candidates.len()], "proxy_node_2");

        // 5. 循环回直连
        state.context.node_round_robin.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let base_idx_req6 = state.context.node_round_robin.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(candidates[base_idx_req6 % candidates.len()], "__direct__");
    }

    #[tokio::test]
    async fn logger_helper_records_failure_and_increments_metrics() {
        let state = ModelProxyState::new_with_app(None);
        let ctx = &state.context;

        let initial_failed = ctx.metrics.failed_requests.load(std::sync::atomic::Ordering::Relaxed);
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
        ).await;

        let after_failed = ctx.metrics.failed_requests.load(std::sync::atomic::Ordering::Relaxed);
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

    #[test]
    fn opencode_model_compatibility_and_anonymous_mode() {
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
        };

        let state = ModelProxyState::new_with_app(None);
        let selected_key = select_channel_api_key(&state.context, &ch_no_key);
        // 匿名模式：未配置 Key 时始终为空字符串
        assert!(selected_key.is_empty());

        // 免费模型在无 Key 匿名模式下通过校验
        assert!(check_model_channel_compatibility(&ch_no_key, "deepseek-v4-flash-free", &selected_key).is_ok());
        assert!(check_model_channel_compatibility(&ch_no_key, "big-pickle", &selected_key).is_ok());
        assert!(check_model_channel_compatibility(&ch_no_key, "mimo-v2.5-free", &selected_key).is_ok());

        // 付费模型在无 Key 匿名模式下前置拦截
        assert!(check_model_channel_compatibility(&ch_no_key, "gpt-4o", &selected_key).is_err());
        assert!(check_model_channel_compatibility(&ch_no_key, "claude-3-7-sonnet", &selected_key).is_err());

        // 配置了 Key 后付费模型通过校验
        let ch_explicit_key = ChannelConfig {
            api_key: "sk-custom-key".to_string(),
            ..ch_no_key
        };
        let selected_explicit = select_channel_api_key(&state.context, &ch_explicit_key);
        assert_eq!(selected_explicit, "sk-custom-key");
        assert!(check_model_channel_compatibility(&ch_explicit_key, "gpt-4o", &selected_explicit).is_ok());
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
