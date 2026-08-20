pub mod adapters;
pub mod balancer;
pub mod commands;
pub mod config;
pub mod router;
pub mod server;
pub mod stats;
pub mod stream;
pub mod types;

#[allow(unused_imports)]
pub use adapters::*;
#[allow(unused_imports)]
pub use balancer::*;
pub use commands::*;
pub use config::*;
#[allow(unused_imports)]
pub use router::*;
pub use server::*;
#[allow(unused_imports)]
pub use stats::*;
#[allow(unused_imports)]
pub use stream::*;
pub use types::*;

#[cfg(test)]
mod tests {
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
                },
            ],
            timeout_seconds: 300,
            record_request_body: false,
            max_retries: 0,
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
}
