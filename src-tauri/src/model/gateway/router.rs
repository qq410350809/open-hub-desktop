use super::adapters::{
    normalize_chat_messages, AnthropicProtocolAdapter, GeminiProtocolAdapter,
    OpenAiProtocolAdapter, ResponsesProtocolAdapter,
};
use super::balancer::{
    is_free_opencode_model, is_opencode_channel, resolve_channel, select_channel_api_key,
};
use super::dispatcher::{execute_resilient_egress, EgressRequestMeta};
use super::logger::{record_attempt_failure, record_auth_failure_log, ProxyLogParams};
use super::stream::{
    clean_sse_stream, openai_to_anthropic_sse_stream, openai_to_gemini_sse_stream,
    openai_to_responses_sse_stream,
};
use super::types::{
    current_timestamp, generate_req_id, ChannelModelFetchError, ChannelModelList, ModelProxyConfig,
    ModelProxyContext, ProxyRequestLog,
};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value as JsonValue};
use std::sync::atomic::Ordering;
use std::time::Instant;
use tower_http::cors::{Any, CorsLayer};

pub fn create_model_proxy_router(ctx: ModelProxyContext) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        // Health / Gateway info
        .route("/", get(handle_healthz))
        .route("/health", get(handle_healthz))
        .route("/healthz", get(handle_healthz))
        .route("/v1/health", get(handle_healthz))
        // OpenAI Models
        .route("/v1/models", get(handle_models))
        .route("/models", get(handle_models))
        .route("/v1/models/{model_id}", get(handle_single_model))
        .route("/models/{model_id}", get(handle_single_model))
        // OpenAI Chat Completions
        .route("/v1/chat/completions", post(handle_chat_completions))
        .route("/chat/completions", post(handle_chat_completions))
        // OpenAI Responses
        .route("/v1/responses", post(handle_responses))
        .route("/responses", post(handle_responses))
        // OpenAI Embeddings
        .route("/v1/embeddings", post(handle_embeddings))
        .route("/embeddings", post(handle_embeddings))
        // Anthropic Messages
        .route("/v1/messages", post(handle_messages))
        .route("/messages", post(handle_messages))
        // Google Gemini
        .route("/v1/gemini/models", get(handle_gemini_models))
        .route("/v1/gemini/models/{*model_action}", post(handle_gemini_generate))
        .route("/v1beta/models", get(handle_gemini_models))
        .route("/v1beta/models/{*model_action}", post(handle_gemini_generate))
        .layer(cors)
        .with_state(ctx)
}

#[allow(dead_code)]
pub fn create_opencode_proxy_router(ctx: ModelProxyContext) -> Router {
    create_model_proxy_router(ctx)
}

/// 统一鉴权检查
pub async fn check_auth(
    headers: &HeaderMap,
    uri: &Uri,
    config: &ModelProxyConfig,
) -> Result<(), Response> {
    if config.api_key.trim().is_empty() {
        return Ok(());
    }

    let auth_header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .or_else(|| headers.get("x-api-key").and_then(|v| v.to_str().ok()))
        .or_else(|| headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()))
        .unwrap_or_default();

    let mut token = auth_header
        .strip_prefix("Bearer ")
        .or_else(|| auth_header.strip_prefix("bearer "))
        .unwrap_or(auth_header)
        .trim();

    // 如果 Header 为空，尝试从 Query string (如 ?key=xxx) 读取
    if token.is_empty() {
        if let Some(query) = uri.query() {
            for param in query.split('&') {
                if let Some((k, v)) = param.split_once('=') {
                    if k == "key" || k == "api_key" {
                        token = v.trim();
                        break;
                    }
                }
            }
        }
    }

    if token == config.api_key.trim() {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": {
                    "message": "Invalid local API Key (本地访问密钥校验未通过)",
                    "type": "invalid_request_error",
                    "code": "invalid_api_key"
                }
            })),
        )
            .into_response())
    }
}

/// GET /healthz
pub async fn handle_healthz(State(ctx): State<ModelProxyContext>) -> Response {
    let config = ctx.config.read().await;
    let models_count = {
        let models = ctx.cached_channel_models.read().await;
        models
            .iter()
            .map(|entry| {
                let channel = config.channels.iter().find(|c| c.id == entry.channel_id);
                let allowed = channel.and_then(|c| c.enabled_models.as_ref());
                match allowed {
                    None => entry.models.len(),
                    Some(allowed) => entry.models.iter().filter(|m| allowed.contains(m)).count(),
                }
            })
            .sum::<usize>()
    };
    let uptime = ctx
        .started_at
        .read()
        .await
        .map(|t| t.elapsed().as_secs())
        .unwrap_or(0);

    let auth_desc = if config.api_key.is_empty() {
        "免密直接访问"
    } else {
        "Bearer Key 校验"
    };

    let opencode_chan = config.channels.iter().find(|c| c.id == "opencode");
    let proxy_pool_desc = match opencode_chan {
        Some(c) if c.use_proxy_pool => "直连优先 + 代理池按速排序故障转移",
        Some(c) if c.use_fixed_proxy => "代理池固定通道（不直连）",
        _ => "直接连接 (直连)",
    };

    // 端点配置：数据驱动
    let endpoints: &[(&str, &str, &str, &str)] = &[
        ("本地模型反代网关 (Gateway)", "/v1", "网关正常运行中，已运行 {uptime} 秒", "auth"),
        ("Google Gemini 兼容端点", "/v1/gemini", "已支持 /v1/gemini/models/* 原生请求", "Header 或 ?key="),
        ("Anthropic Claude 兼容端点", "/v1/messages", "已支持 Claude Desktop / Cline / Cursor 等工具直连", "x-api-key 或 Bearer"),
    ];

    let mut checks: Vec<JsonValue> = endpoints
        .iter()
        .map(|(name, path, msg_tmpl, auth)| {
            let message = if msg_tmpl.contains("{uptime}") {
                msg_tmpl.replace("{uptime}", &uptime.to_string())
            } else {
                msg_tmpl.to_string()
            };
            json!({
                "name": name,
                "endpoint": format!("http://127.0.0.1:{}{}", config.port, path),
                "status": "ok",
                "message": message,
                "auth": if auth == &"auth" { auth_desc } else { auth }
            })
        })
        .collect();

    // 动态端点：多渠道负载均衡信息
    checks.push(json!({
        "name": "多渠道负载均衡与代理池",
        "endpoint": proxy_pool_desc,
        "status": "ok",
        "message": format!("共配置 {} 个上游渠道，聚合 {} 个模型", config.channels.len(), models_count),
        "auth": "多 Key 自动轮询"
    }));

    Json(json!({
        "status": "ok",
        "service": "OpenHub Local LLM Gateway",
        "port": config.port,
        "uptimeSeconds": uptime,
        "modelsCount": models_count,
        "channelsCount": config.channels.len(),
        "checks": checks
    }))
    .into_response()
}

/// GET /v1/models (OpenAI 格式)
pub async fn handle_models(
    headers: HeaderMap,
    uri: Uri,
    State(ctx): State<ModelProxyContext>,
) -> Response {
    let config = ctx.config.read().await;
    if let Err(res) = check_auth(&headers, &uri, &config).await {
        return res;
    }

    let channel_models = ctx.cached_channel_models.read().await.clone();
    let mut model_items = Vec::new();

    for entry in &channel_models {
        let channel = config.channels.iter().find(|c| c.id == entry.channel_id);
        if let Some(ch) = channel {
            if !ch.enabled {
                continue;
            }
            let eff_alias = ch.effective_alias();
            let allowed_models = ch.enabled_models.as_ref();

            for m in &entry.models {
                if let Some(allowed) = allowed_models {
                    if !allowed.contains(m) {
                        continue;
                    }
                }

                let full_id = format!("{eff_alias}/{m}");
                model_items.push(json!({
                    "id": full_id,
                    "object": "model",
                    "created": 1700000000,
                    "owned_by": eff_alias,
                    "permission": [],
                    "root": m,
                    "parent": null
                }));

                // 默认 opencode 渠道的模型额外注入无前缀的裸模型名
                if ch.id == "opencode" {
                    model_items.push(json!({
                        "id": m,
                        "object": "model",
                        "created": 1700000000,
                        "owned_by": "opencode",
                        "permission": [],
                        "root": m,
                        "parent": null
                    }));
                }
            }
        }
    }

    // 补充显式配置的 enabled_models（若尚未从上游拉取到）
    for ch in &config.channels {
        if !ch.enabled {
            continue;
        }
        let eff_alias = ch.effective_alias();
        if let Some(explicit_models) = &ch.enabled_models {
            for m in explicit_models {
                let full_id = format!("{eff_alias}/{m}");
                if !model_items.iter().any(|item| item.get("id").and_then(JsonValue::as_str) == Some(&full_id)) {
                    model_items.push(json!({
                        "id": full_id,
                        "object": "model",
                        "created": 1700000000,
                        "owned_by": eff_alias,
                        "permission": [],
                        "root": m,
                        "parent": null
                    }));
                    if ch.id == "opencode" {
                        model_items.push(json!({
                            "id": m,
                            "object": "model",
                            "created": 1700000000,
                            "owned_by": "opencode",
                            "permission": [],
                            "root": m,
                            "parent": null
                        }));
                    }
                }
            }
        }
    }

    // 兜底保底模型：避免任何情况下返回空数组给客户端
    if model_items.is_empty() {
        let defaults = [
            "deepseek-v4-flash-free",
            "glm-4-flash-free",
            "qwen-2.5-coder-32b",
            "claude-3-7-sonnet",
            "gpt-4o",
        ];
        for m in defaults {
            model_items.push(json!({
                "id": format!("opencode/{m}"),
                "object": "model",
                "created": 1700000000,
                "owned_by": "opencode",
                "permission": [],
                "root": m,
                "parent": null
            }));
            model_items.push(json!({
                "id": m,
                "object": "model",
                "created": 1700000000,
                "owned_by": "opencode",
                "permission": [],
                "root": m,
                "parent": null
            }));
        }
    }

    Json(json!({
        "object": "list",
        "data": model_items
    }))
    .into_response()
}

/// GET /v1/models/:model_id (单个模型查询)
pub async fn handle_single_model(
    headers: HeaderMap,
    uri: Uri,
    Path(model_id): Path<String>,
    State(ctx): State<ModelProxyContext>,
) -> Response {
    let config = ctx.config.read().await;
    if let Err(res) = check_auth(&headers, &uri, &config).await {
        return res;
    }
    Json(json!({
        "id": model_id,
        "object": "model",
        "created": 1700000000,
        "owned_by": "openhub",
        "permission": [],
        "root": model_id,
        "parent": null
    }))
    .into_response()
}

/// GET /v1/gemini/models (Google Gemini 格式)
pub async fn handle_gemini_models(
    headers: HeaderMap,
    uri: Uri,
    State(ctx): State<ModelProxyContext>,
) -> Response {
    let config = ctx.config.read().await;
    if let Err(res) = check_auth(&headers, &uri, &config).await {
        return res;
    }

    let channel_models = ctx.cached_channel_models.read().await.clone();
    let mut gemini_models = Vec::new();

    for entry in channel_models {
        let channel = config.channels.iter().find(|c| c.id == entry.channel_id);
        if let Some(ch) = channel {
            if !ch.enabled {
                continue;
            }
            let eff_alias = ch.effective_alias();
            let allowed_models = ch.enabled_models.as_ref();

            for m in entry.models {
                if let Some(allowed) = allowed_models {
                    if !allowed.contains(&m) {
                        continue;
                    }
                }

                let full_id = format!("{eff_alias}/{m}");
                gemini_models.push(json!({
                    "name": format!("models/{full_id}"),
                    "version": "001",
                    "displayName": format!("{eff_alias}: {m}"),
                    "description": format!("Model {m} via channel {eff_alias}"),
                    "supportedGenerationMethods": ["generateContent", "countTokens"]
                }));
            }
        }
    }

    Json(json!({
        "models": gemini_models
    }))
    .into_response()
}

/// POST /v1/embeddings (OpenAI Embeddings 转发)
pub async fn handle_embeddings(
    headers: HeaderMap,
    uri: Uri,
    State(ctx): State<ModelProxyContext>,
    Json(mut body): Json<JsonValue>,
) -> Response {
    let start_time = Instant::now();
    let req_id = generate_req_id();
    let config = ctx.config.read().await.clone();
    let req_body_str = if config.record_request_body {
        Some(serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string()))
    } else {
        None
    };

    let raw_model = body
        .get("model")
        .and_then(JsonValue::as_str)
        .unwrap_or("text-embedding-3-small")
        .to_string();

    if let Err(res) = check_auth(&headers, &uri, &config).await {
        record_auth_failure_log(
            &ctx,
            &req_id,
            "/v1/embeddings",
            &raw_model,
            false,
            start_time.elapsed().as_millis() as u64,
            req_body_str,
        ).await;
        return res;
    }

    ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);

    let (chan, model_to_send) = match resolve_channel(&config, &raw_model) {
        Some(pair) => pair,
        None => {
            let dur = start_time.elapsed().as_millis() as u64;
            record_attempt_failure(
                &ctx,
                ProxyLogParams::new_failure(
                    req_id.clone(),
                    "/v1/embeddings".to_string(),
                    "opencode".to_string(),
                    raw_model.clone(),
                    false,
                    404,
                    dur,
                    Some(format!("未找到支持模型 '{raw_model}' 的可用渠道")),
                    req_body_str,
                    None,
                ),
            ).await;
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": {
                        "message": format!("No available channel for model '{raw_model}'"),
                        "type": "invalid_request_error",
                        "code": "model_not_found"
                    }
                })),
            ).into_response();
        }
    };

    body["model"] = JsonValue::String(model_to_send);
    let upstream_url = format!("{}/embeddings", chan.base_url.trim_end_matches('/'));
    let channel_api_key = select_channel_api_key(&ctx, chan);
    let chan_alias = chan.effective_alias();

    let meta = EgressRequestMeta {
        req_id: req_id.clone(),
        path: "/v1/embeddings".to_string(),
        channel_id: chan_alias.clone(),
        model: raw_model.clone(),
        stream: false,
        req_body_str: req_body_str.clone(),
    };

    let success = match execute_resilient_egress(
        &ctx,
        chan,
        &config,
        meta,
        &upstream_url,
        &channel_api_key,
        &body,
    ).await {
        Ok(s) => s,
        Err(err_resp) => return err_resp,
    };

    let bytes = success.response.bytes().await.unwrap_or_default();
    ctx.record_log(ProxyRequestLog {
        id: success.attempt_req_id,
        timestamp: current_timestamp(),
        method: "POST".to_string(),
        path: "/v1/embeddings".to_string(),
        channel_id: chan_alias,
        model: raw_model,
        stream: false,
        status_code: success.status.as_u16(),
        duration_ms: success.cand_start.elapsed().as_millis() as u64,
        ttft_ms: None,
        prompt_tokens: None,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
        completion_tokens: None,
        reasoning_tokens: None,
        total_tokens: None,
        error_message: None,
        request_body: req_body_str,
        response_body: None,
        node_name: Some(success.node_display),
    }).await;

    (success.status, [("content-type", "application/json")], bytes).into_response()
}

/// POST /v1/gemini/models/* (Google Gemini 原生协议端点)
pub async fn handle_gemini_generate(
    headers: HeaderMap,
    uri: Uri,
    State(ctx): State<ModelProxyContext>,
    Path(model_action): Path<String>,
    Json(body): Json<JsonValue>,
) -> Response {
    let start_time = Instant::now();
    let req_id = generate_req_id();
    let config = ctx.config.read().await.clone();

    let (raw_model, is_stream) = if let Some((m, action)) = model_action.rsplit_once(':') {
        let stream = action.contains("stream");
        (m.trim_start_matches('/'), stream)
    } else {
        (model_action.trim_start_matches('/'), false)
    };

    let log_path = if uri.path().starts_with("/v1beta") {
        format!("/v1beta/models/{model_action}")
    } else {
        format!("/v1/gemini/models/{model_action}")
    };

    let req_body_str = if config.record_request_body {
        Some(serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string()))
    } else {
        None
    };

    if let Err(res) = check_auth(&headers, &uri, &config).await {
        record_auth_failure_log(
            &ctx,
            &req_id,
            &log_path,
            raw_model,
            is_stream,
            start_time.elapsed().as_millis() as u64,
            req_body_str,
        ).await;
        return res;
    }

    ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);

    let (chan, model_to_send) = match resolve_channel(&config, raw_model) {
        Some(pair) => pair,
        None => {
            let dur = start_time.elapsed().as_millis() as u64;
            record_attempt_failure(
                &ctx,
                ProxyLogParams::new_failure(
                    req_id.clone(),
                    log_path.clone(),
                    "opencode".to_string(),
                    raw_model.to_string(),
                    is_stream,
                    404,
                    dur,
                    Some(format!("未找到支持模型 '{raw_model}' 的可用渠道")),
                    req_body_str,
                    None,
                ),
            ).await;
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": {
                        "message": format!("No available channel for model '{raw_model}'"),
                        "type": "invalid_request_error",
                        "code": "model_not_found"
                    }
                })),
            ).into_response();
        }
    };

    let mut openai_body = GeminiProtocolAdapter::gemini_request_to_openai(&body, &model_to_send, is_stream);
    OpenAiProtocolAdapter::sanitize_and_normalize(&mut openai_body);

    let upstream_url = format!("{}/chat/completions", chan.base_url.trim_end_matches('/'));
    let channel_api_key = select_channel_api_key(&ctx, chan);
    let chan_alias = chan.effective_alias();

    let meta = EgressRequestMeta {
        req_id: req_id.clone(),
        path: log_path.clone(),
        channel_id: chan_alias.clone(),
        model: raw_model.to_string(),
        stream: is_stream,
        req_body_str: req_body_str.clone(),
    };

    let success = match execute_resilient_egress(
        &ctx,
        chan,
        &config,
        meta,
        &upstream_url,
        &channel_api_key,
        &openai_body,
    ).await {
        Ok(s) => s,
        Err(err_resp) => return err_resp,
    };

    let log = ProxyRequestLog {
        id: success.attempt_req_id,
        timestamp: current_timestamp(),
        method: "POST".to_string(),
        path: log_path,
        channel_id: chan_alias,
        model: raw_model.to_string(),
        stream: is_stream,
        status_code: success.status.as_u16(),
        duration_ms: success.cand_start.elapsed().as_millis() as u64,
        ttft_ms: None,
        prompt_tokens: None,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
        completion_tokens: None,
        reasoning_tokens: None,
        total_tokens: None,
        error_message: None,
        request_body: req_body_str,
        response_body: None,
        node_name: Some(success.node_display),
    };

    if is_stream {
        let stream_body = openai_to_gemini_sse_stream(
            success.response.bytes_stream(),
            ctx.clone(),
            log,
            start_time,
            raw_model.to_string(),
        );
        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/event-stream")
            .header("Cache-Control", "no-cache")
            .header("Connection", "keep-alive")
            .body(stream_body)
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    } else {
        let resp_bytes = success.response.bytes().await.unwrap_or_default();
        let openai_resp = serde_json::from_slice::<JsonValue>(&resp_bytes).unwrap_or_default();
        let gemini_resp = GeminiProtocolAdapter::openai_response_to_gemini(&openai_resp, raw_model);

        let dur = success.cand_start.elapsed().as_millis() as u64;
        let mut final_log = log;
        final_log.duration_ms = dur;
        if let Some(usage) = gemini_resp.get("usageMetadata") {
            final_log.prompt_tokens = usage.get("promptTokenCount").and_then(JsonValue::as_u64);
            final_log.completion_tokens = usage.get("candidatesTokenCount").and_then(JsonValue::as_u64);
            final_log.total_tokens = usage.get("totalTokenCount").and_then(JsonValue::as_u64);
        }
        ctx.record_log(final_log).await;

        Json(gemini_resp).into_response()
    }
}

/// POST /v1/chat/completions (OpenAI Chat Completions 代理核心)
pub async fn handle_chat_completions(
    headers: HeaderMap,
    uri: Uri,
    State(ctx): State<ModelProxyContext>,
    Json(mut body): Json<JsonValue>,
) -> Response {
    let start_time = Instant::now();
    let req_id = generate_req_id();
    let config = ctx.config.read().await.clone();

    let raw_model = body
        .get("model")
        .and_then(JsonValue::as_str)
        .unwrap_or("gpt-4o")
        .to_string();

    let is_stream = body
        .get("stream")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);

    let req_body_str = if config.record_request_body {
        Some(serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string()))
    } else {
        None
    };

    if let Err(res) = check_auth(&headers, &uri, &config).await {
        record_auth_failure_log(
            &ctx,
            &req_id,
            "/v1/chat/completions",
            &raw_model,
            is_stream,
            start_time.elapsed().as_millis() as u64,
            req_body_str,
        ).await;
        return res;
    }

    ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);

    let (chan, model_to_send) = match resolve_channel(&config, &raw_model) {
        Some(pair) => pair,
        None => {
            let dur = start_time.elapsed().as_millis() as u64;
            record_attempt_failure(
                &ctx,
                ProxyLogParams::new_failure(
                    req_id.clone(),
                    "/v1/chat/completions".to_string(),
                    "opencode".to_string(),
                    raw_model.clone(),
                    is_stream,
                    404,
                    dur,
                    Some(format!("未找到支持模型 '{raw_model}' 的可用渠道")),
                    req_body_str,
                    None,
                ),
            ).await;
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": {
                        "message": format!("No available channel for model '{raw_model}'"),
                        "type": "invalid_request_error",
                        "code": "model_not_found"
                    }
                })),
            ).into_response();
        }
    };

    body["model"] = JsonValue::String(model_to_send);
    OpenAiProtocolAdapter::sanitize_and_normalize(&mut body);
    if let Some(msgs) = body.get_mut("messages") {
        normalize_chat_messages(msgs);
    }

    let upstream_url = format!("{}/chat/completions", chan.base_url.trim_end_matches('/'));
    let channel_api_key = select_channel_api_key(&ctx, chan);
    let chan_alias = chan.effective_alias();

    let meta = EgressRequestMeta {
        req_id: req_id.clone(),
        path: "/v1/chat/completions".to_string(),
        channel_id: chan_alias.clone(),
        model: raw_model.clone(),
        stream: is_stream,
        req_body_str: req_body_str.clone(),
    };

    let success = match execute_resilient_egress(
        &ctx,
        chan,
        &config,
        meta,
        &upstream_url,
        &channel_api_key,
        &body,
    ).await {
        Ok(s) => s,
        Err(err_resp) => return err_resp,
    };

    let log = ProxyRequestLog {
        id: success.attempt_req_id,
        timestamp: current_timestamp(),
        method: "POST".to_string(),
        path: "/v1/chat/completions".to_string(),
        channel_id: chan_alias,
        model: raw_model.clone(),
        stream: is_stream,
        status_code: success.status.as_u16(),
        duration_ms: success.cand_start.elapsed().as_millis() as u64,
        ttft_ms: None,
        prompt_tokens: None,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
        completion_tokens: None,
        reasoning_tokens: None,
        total_tokens: None,
        error_message: None,
        request_body: req_body_str,
        response_body: None,
        node_name: Some(success.node_display),
    };

    if is_stream {
        let stream_body = clean_sse_stream(
            success.response.bytes_stream(),
            ctx.clone(),
            log,
            start_time,
            raw_model,
        );
        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/event-stream")
            .header("Cache-Control", "no-cache")
            .header("Connection", "keep-alive")
            .body(stream_body)
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    } else {
        let resp_bytes = success.response.bytes().await.unwrap_or_default();
        let dur = success.cand_start.elapsed().as_millis() as u64;

        let mut prompt_toks = None;
        let mut comp_toks = None;
        let mut reas_toks = None;
        let mut cache_toks = None;
        let mut total_toks = None;
        let mut has_reasoning = false;

        if let Ok(mut jv) = serde_json::from_slice::<JsonValue>(&resp_bytes) {
            if let Some(usage) = jv.get("usage").and_then(JsonValue::as_object) {
                prompt_toks = usage.get("prompt_tokens").and_then(JsonValue::as_u64);
                comp_toks = usage.get("completion_tokens").and_then(JsonValue::as_u64);
                total_toks = usage.get("total_tokens").and_then(JsonValue::as_u64);

                if let Some(details) = usage.get("prompt_tokens_details").and_then(JsonValue::as_object) {
                    cache_toks = details.get("cached_tokens").and_then(JsonValue::as_u64);
                }
                if let Some(details) = usage.get("completion_tokens_details").and_then(JsonValue::as_object) {
                    reas_toks = details.get("reasoning_tokens").and_then(JsonValue::as_u64);
                    if reas_toks.is_some() {
                        has_reasoning = true;
                    }
                }
            }

            if let Some(msg) = jv.pointer_mut("/choices/0/message") {
                let mut extracted_reasoning = None;
                if let Some(content) = msg.get_mut("content") {
                    if let Some(s) = content.as_str() {
                        if let (Some(start), Some(end)) = (s.find("<think>"), s.find("</think>")) {
                            if start < end {
                                let reasoning = s[start + 7..end].trim().to_string();
                                let after = s[end + 8..].trim_start().to_string();
                                *content = JsonValue::String(after);
                                extracted_reasoning = Some(reasoning);
                                has_reasoning = true;
                            }
                        }
                    }
                }
                if let Some(reasoning) = extracted_reasoning {
                    msg["reasoning_content"] = JsonValue::String(reasoning);
                }
            }

            let mut final_log = log;
            final_log.duration_ms = dur;
            final_log.prompt_tokens = prompt_toks;
            final_log.completion_tokens = comp_toks;
            final_log.reasoning_tokens = reas_toks;
            final_log.prompt_cache_hit_tokens = cache_toks;
            final_log.total_tokens = total_toks;
            ctx.record_log(final_log).await;

            if has_reasoning {
                ctx.metrics.total_reasoning_requests.fetch_add(1, Ordering::Relaxed);
            }
            if let Some(p) = prompt_toks {
                ctx.metrics.total_prompt_tokens.fetch_add(p, Ordering::Relaxed);
            }
            if let Some(c) = comp_toks {
                ctx.metrics.total_completion_tokens.fetch_add(c, Ordering::Relaxed);
            }
            if let Some(r) = reas_toks {
                ctx.metrics.total_reasoning_tokens.fetch_add(r, Ordering::Relaxed);
            }
            if let Some(h) = cache_toks {
                ctx.metrics.total_cache_hit_tokens.fetch_add(h, Ordering::Relaxed);
            }
            if let Some(t) = total_toks {
                ctx.metrics.total_tokens.fetch_add(t, Ordering::Relaxed);
            }

            return Json(jv).into_response();
        }

        let mut final_log = log;
        final_log.duration_ms = dur;
        ctx.record_log(final_log).await;
        (StatusCode::OK, resp_bytes).into_response()
    }
}

/// POST /v1/responses (OpenAI Responses API 转发)
pub async fn handle_responses(
    headers: HeaderMap,
    uri: Uri,
    State(ctx): State<ModelProxyContext>,
    Json(body): Json<JsonValue>,
) -> Response {
    let start_time = Instant::now();
    let req_id = generate_req_id();
    let config = ctx.config.read().await.clone();

    let raw_model = body
        .get("model")
        .and_then(JsonValue::as_str)
        .unwrap_or("gpt-4o")
        .to_string();

    let is_stream = body
        .get("stream")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);

    let req_body_str = if config.record_request_body {
        Some(serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string()))
    } else {
        None
    };

    if let Err(res) = check_auth(&headers, &uri, &config).await {
        record_auth_failure_log(
            &ctx,
            &req_id,
            "/v1/responses",
            &raw_model,
            is_stream,
            start_time.elapsed().as_millis() as u64,
            req_body_str,
        ).await;
        return res;
    }

    ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);

    let (chan, model_to_send) = match resolve_channel(&config, &raw_model) {
        Some(pair) => pair,
        None => {
            let dur = start_time.elapsed().as_millis() as u64;
            record_attempt_failure(
                &ctx,
                ProxyLogParams::new_failure(
                    req_id.clone(),
                    "/v1/responses".to_string(),
                    "opencode".to_string(),
                    raw_model.clone(),
                    is_stream,
                    404,
                    dur,
                    Some(format!("未找到支持模型 '{raw_model}' 的可用渠道")),
                    req_body_str,
                    None,
                ),
            ).await;
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": {
                        "message": format!("No available channel for model '{raw_model}'"),
                        "type": "invalid_request_error",
                        "code": "model_not_found"
                    }
                })),
            ).into_response();
        }
    };

    let mut openai_body = body.clone();
    openai_body["model"] = JsonValue::String(model_to_send);
    ResponsesProtocolAdapter::convert_input_to_messages(&mut openai_body);
    OpenAiProtocolAdapter::sanitize_and_normalize(&mut openai_body);
    let upstream_url = format!("{}/chat/completions", chan.base_url.trim_end_matches('/'));
    let channel_api_key = select_channel_api_key(&ctx, chan);
    let chan_alias = chan.effective_alias();

    let meta = EgressRequestMeta {
        req_id: req_id.clone(),
        path: "/v1/responses".to_string(),
        channel_id: chan_alias.clone(),
        model: raw_model.clone(),
        stream: is_stream,
        req_body_str: req_body_str.clone(),
    };

    let success = match execute_resilient_egress(
        &ctx,
        chan,
        &config,
        meta,
        &upstream_url,
        &channel_api_key,
        &openai_body,
    ).await {
        Ok(s) => s,
        Err(err_resp) => return err_resp,
    };

    let log = ProxyRequestLog {
        id: success.attempt_req_id,
        timestamp: current_timestamp(),
        method: "POST".to_string(),
        path: "/v1/responses".to_string(),
        channel_id: chan_alias,
        model: raw_model.clone(),
        stream: is_stream,
        status_code: success.status.as_u16(),
        duration_ms: success.cand_start.elapsed().as_millis() as u64,
        ttft_ms: None,
        prompt_tokens: None,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
        completion_tokens: None,
        reasoning_tokens: None,
        total_tokens: None,
        error_message: None,
        request_body: req_body_str,
        response_body: None,
        node_name: Some(success.node_display),
    };

    if is_stream {
        let stream_body = openai_to_responses_sse_stream(
            success.response.bytes_stream(),
            ctx.clone(),
            log,
            start_time,
            raw_model,
        );
        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/event-stream")
            .header("Cache-Control", "no-cache")
            .header("Connection", "keep-alive")
            .body(stream_body)
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    } else {
        let resp_bytes = success.response.bytes().await.unwrap_or_default();
        let dur = success.cand_start.elapsed().as_millis() as u64;

        if let Ok(jv) = serde_json::from_slice::<JsonValue>(&resp_bytes) {
            let text = jv
                .pointer("/choices/0/message/content")
                .and_then(JsonValue::as_str)
                .unwrap_or("");
            let responses_output = json!({
                "id": format!("resp_{req_id}"),
                "object": "response",
                "created": 1700000000,
                "model": raw_model,
                "status": "completed",
                "output": [
                    {
                        "type": "message",
                        "role": "assistant",
                        "content": [
                            {
                                "type": "text",
                                "text": text
                            }
                        ]
                    }
                ]
            });

            let mut final_log = log;
            final_log.duration_ms = dur;
            ctx.record_log(final_log).await;
            return Json(responses_output).into_response();
        }

        let mut final_log = log;
        final_log.duration_ms = dur;
        ctx.record_log(final_log).await;
        (StatusCode::OK, resp_bytes).into_response()
    }
}

/// POST /v1/messages (Anthropic Messages 原生协议转发)
pub async fn handle_messages(
    headers: HeaderMap,
    uri: Uri,
    State(ctx): State<ModelProxyContext>,
    Json(body): Json<JsonValue>,
) -> Response {
    let start_time = Instant::now();
    let req_id = generate_req_id();
    let config = ctx.config.read().await.clone();

    let raw_model = body
        .get("model")
        .and_then(JsonValue::as_str)
        .unwrap_or("claude-3-7-sonnet")
        .to_string();

    let is_stream = body
        .get("stream")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);

    let req_body_str = if config.record_request_body {
        Some(serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string()))
    } else {
        None
    };

    if let Err(res) = check_auth(&headers, &uri, &config).await {
        record_auth_failure_log(
            &ctx,
            &req_id,
            "/v1/messages",
            &raw_model,
            is_stream,
            start_time.elapsed().as_millis() as u64,
            req_body_str,
        ).await;
        return res;
    }

    ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);

    let (chan, model_to_send) = match resolve_channel(&config, &raw_model) {
        Some(pair) => pair,
        None => {
            let dur = start_time.elapsed().as_millis() as u64;
            record_attempt_failure(
                &ctx,
                ProxyLogParams::new_failure(
                    req_id.clone(),
                    "/v1/messages".to_string(),
                    "opencode".to_string(),
                    raw_model.clone(),
                    is_stream,
                    404,
                    dur,
                    Some(format!("未找到支持模型 '{raw_model}' 的可用渠道")),
                    req_body_str,
                    None,
                ),
            ).await;
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "type": "error",
                    "error": {
                        "type": "not_found_error",
                        "message": format!("No available channel for model '{raw_model}'")
                    }
                })),
            ).into_response();
        }
    };

    let mut openai_payload = AnthropicProtocolAdapter::anthropic_request_to_openai(&body, &model_to_send, is_stream);
    OpenAiProtocolAdapter::sanitize_and_normalize(&mut openai_payload);
    let upstream_url = format!("{}/chat/completions", chan.base_url.trim_end_matches('/'));
    let channel_api_key = select_channel_api_key(&ctx, chan);
    let chan_alias = chan.effective_alias();

    let meta = EgressRequestMeta {
        req_id: req_id.clone(),
        path: "/v1/messages".to_string(),
        channel_id: chan_alias.clone(),
        model: raw_model.clone(),
        stream: is_stream,
        req_body_str: req_body_str.clone(),
    };

    let success = match execute_resilient_egress(
        &ctx,
        chan,
        &config,
        meta,
        &upstream_url,
        &channel_api_key,
        &openai_payload,
    ).await {
        Ok(s) => s,
        Err(err_resp) => return err_resp,
    };

    let log = ProxyRequestLog {
        id: success.attempt_req_id,
        timestamp: current_timestamp(),
        method: "POST".to_string(),
        path: "/v1/messages".to_string(),
        channel_id: chan_alias,
        model: raw_model.clone(),
        stream: is_stream,
        status_code: success.status.as_u16(),
        duration_ms: success.cand_start.elapsed().as_millis() as u64,
        ttft_ms: None,
        prompt_tokens: None,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
        completion_tokens: None,
        reasoning_tokens: None,
        total_tokens: None,
        error_message: None,
        request_body: req_body_str,
        response_body: None,
        node_name: Some(success.node_display),
    };

    if is_stream {
        let stream_body = openai_to_anthropic_sse_stream(
            success.response.bytes_stream(),
            ctx.clone(),
            log,
            start_time,
            raw_model,
        );
        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/event-stream")
            .header("Cache-Control", "no-cache")
            .header("Connection", "keep-alive")
            .body(stream_body)
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    } else {
        let resp_bytes = success.response.bytes().await.unwrap_or_default();
        let dur = success.cand_start.elapsed().as_millis() as u64;

        if let Ok(jv) = serde_json::from_slice::<JsonValue>(&resp_bytes) {
            let (p_tok, c_tok) = AnthropicProtocolAdapter::extract_token_usage(&jv);
            let anthropic_resp = AnthropicProtocolAdapter::openai_response_to_anthropic(&jv, &req_id, &raw_model);

            let mut final_log = log;
            final_log.duration_ms = dur;
            final_log.prompt_tokens = Some(p_tok);
            final_log.completion_tokens = Some(c_tok);
            final_log.total_tokens = Some(p_tok + c_tok);
            ctx.record_log(final_log).await;

            return Json(anthropic_resp).into_response();
        }

        let mut final_log = log;
        final_log.duration_ms = dur;
        ctx.record_log(final_log).await;
        (StatusCode::OK, resp_bytes).into_response()
    }
}

/// 从各个启用的上游渠道拉取可用的模型列表并存入内存缓存
pub async fn fetch_upstream_models_inner(ctx: &ModelProxyContext) {
    let config = ctx.config.read().await.clone();
    let mut channel_models = Vec::new();
    let mut fetch_errors = Vec::new();

    for ch in &config.channels {
        if !ch.enabled {
            continue;
        }

        let models_url = format!("{}/models", ch.base_url.trim_end_matches('/'));
        let api_key = select_channel_api_key(ctx, ch);
        let client = &ctx.default_http_client;

        let mut req = client.get(&models_url);
        if !api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {api_key}"));
        }

        match req.send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    let mut list = Vec::new();
                    if let Ok(jv) = resp.json::<JsonValue>().await {
                        if let Some(data) = jv.get("data").and_then(JsonValue::as_array) {
                            for item in data {
                                if let Some(id) = item.get("id").and_then(JsonValue::as_str) {
                                    list.push(id.to_string());
                                }
                            }
                        } else if let Some(models) = jv.get("models").and_then(JsonValue::as_array) {
                            for item in models {
                                if let Some(name) = item.get("name").and_then(JsonValue::as_str) {
                                    list.push(name.trim_start_matches("models/").to_string());
                                }
                            }
                        }
                    }
                    if list.is_empty() {
                        if let Some(explicit) = &ch.enabled_models {
                            list.extend(explicit.clone());
                        }
                    }
                    // OpenCode 官方渠道模型列表过长（60+，绝大多数为非免费模型），仅保留免费模型
                    if is_opencode_channel(ch) {
                        list.retain(|m| is_free_opencode_model(&m));
                    }
                    channel_models.push(ChannelModelList {
                        channel_id: ch.id.clone(),
                        channel_name: ch.name.clone(),
                        alias: ch.effective_alias(),
                        models: list,
                    });
                } else {
                    let err_msg = format!("上游返回 HTTP 状态码: {}", resp.status());
                    fetch_errors.push(ChannelModelFetchError {
                        channel_id: ch.id.clone(),
                        channel_name: ch.name.clone(),
                        alias: ch.effective_alias(),
                        error: err_msg,
                    });
                }
            }
            Err(e) => {
                let err_msg = format!("连接上游失败: {e}");
                fetch_errors.push(ChannelModelFetchError {
                    channel_id: ch.id.clone(),
                    channel_name: ch.name.clone(),
                    alias: ch.effective_alias(),
                    error: err_msg,
                });
            }
        }
    }

    *ctx.cached_channel_models.write().await = channel_models;
    *ctx.cached_fetch_errors.write().await = fetch_errors;
}

