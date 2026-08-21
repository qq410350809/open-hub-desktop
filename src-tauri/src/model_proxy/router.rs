use super::adapters::{
    normalize_chat_messages, AnthropicProtocolAdapter, GeminiProtocolAdapter,
    ResponsesProtocolAdapter,
};
use super::balancer::{
    build_client_for_candidate, format_upstream_error_message, get_node_display_name,
    get_sorted_egress_candidates, record_failover_event, resolve_channel,
    select_channel_api_key,
};
use super::stream::{
    clean_sse_stream, openai_to_anthropic_sse_stream, openai_to_gemini_sse_stream,
    openai_to_responses_sse_stream,
};
use super::types::{
    current_timestamp, generate_req_id, ChannelModelFetchError, ChannelModelList,
    ModelProxyConfig, ModelProxyContext, ProxyRequestLog,
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
use tauri::Manager;

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

/// 构建鉴权失败的 ProxyRequestLog 并记录到上下文
async fn record_auth_failure(
    ctx: &ModelProxyContext,
    req_id: &str,
    path: &str,
    model: &str,
    stream: bool,
    dur: u64,
    req_body_str: Option<String>,
) {
    ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);
    ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
    ctx.record_log(ProxyRequestLog {
        id: req_id.to_string(),
        timestamp: current_timestamp(),
        method: "POST".to_string(),
        path: path.to_string(),
        channel_id: "opencode".to_string(),
        model: model.to_string(),
        stream,
        status_code: 401,
        duration_ms: dur,
        ttft_ms: None,
        prompt_tokens: None,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
        completion_tokens: None,
        reasoning_tokens: None,
        total_tokens: None,
        error_message: Some("鉴权未通过：请求未携带有效的 API Key".to_string()),
        request_body: req_body_str,
        response_body: None,
        node_name: Some("直连通道".to_string()),
    })
    .await;
}

/// 构建请求基础 ProxyRequestLog（成功路径）
#[allow(dead_code)]
fn build_request_log(
    req_id: &str,
    path: &str,
    chan_alias: &str,
    model: &str,
    stream: bool,
    status_code: u16,
    duration_ms: u64,
    req_body_str: Option<String>,
    node_name: String,
) -> ProxyRequestLog {
    ProxyRequestLog {
        id: req_id.to_string(),
        timestamp: current_timestamp(),
        method: "POST".to_string(),
        path: path.to_string(),
        channel_id: chan_alias.to_string(),
        model: model.to_string(),
        stream,
        status_code,
        duration_ms,
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
        node_name: Some(node_name),
    }
}

/// 所有候选节点耗尽后，记录失败日志并返回错误响应
#[allow(dead_code)]
async fn record_exhausted_error(
    ctx: &ModelProxyContext,
    req_id: &str,
    path: &str,
    chan_alias: &str,
    model: &str,
    stream: bool,
    last_status: StatusCode,
    last_error: &str,
    dur: u64,
    req_body_str: Option<String>,
) -> Response {
    ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
    ctx.record_log(ProxyRequestLog {
        id: req_id.to_string(),
        timestamp: current_timestamp(),
        method: "POST".to_string(),
        path: path.to_string(),
        channel_id: chan_alias.to_string(),
        model: model.to_string(),
        stream,
        status_code: last_status.as_u16(),
        duration_ms: dur,
        ttft_ms: None,
        prompt_tokens: None,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
        completion_tokens: None,
        reasoning_tokens: None,
        total_tokens: None,
        error_message: Some(last_error.to_string()),
        request_body: req_body_str,
        response_body: None,
        node_name: Some("已尝试所有候选节点".to_string()),
    })
    .await;

    (
        last_status,
        Json(json!({
            "error": {
                "message": last_error,
                "type": "upstream_error",
                "code": last_status.as_u16()
            }
        })),
    )
        .into_response()
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

    // 端点配置：数据驱动，新增端点只需在此添加一行
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

/// POST /v1/embeddings (OpenAI Embeddings 转发)
pub async fn handle_embeddings(
    headers: HeaderMap,
    uri: Uri,
    State(ctx): State<ModelProxyContext>,
    Json(mut body): Json<JsonValue>,
) -> Response {
    let config = ctx.config.read().await;
    if let Err(res) = check_auth(&headers, &uri, &config).await {
        return res;
    }

    let raw_model = body
        .get("model")
        .and_then(JsonValue::as_str)
        .unwrap_or("text-embedding-3-small")
        .to_string();
    let (chan, model_to_send) = match resolve_channel(&config, &raw_model) {
        Some((c, m)) => (c, m),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": { "type": "api_error", "message": "未找到可用的上游渠道" } })),
            )
                .into_response();
        }
    };
    body["model"] = JsonValue::String(model_to_send);
    let upstream_url = format!("{}/embeddings", chan.base_url.trim_end_matches('/'));
    let channel_api_key = select_channel_api_key(&ctx, chan);
    let candidates = get_sorted_egress_candidates(&ctx, chan).await;
    let candidate_id = candidates.first().map(|s| s.as_str()).unwrap_or("__direct__");
    let client = build_client_for_candidate(&ctx, candidate_id).await;

    let mut req_builder = client
        .post(&upstream_url)
        .header("Content-Type", "application/json")
        .json(&body);

    if !channel_api_key.is_empty() {
        req_builder = req_builder.header("Authorization", format!("Bearer {channel_api_key}"));
    }

    match req_builder.send().await {
        Ok(resp) => {
            let status = resp.status();
            let bytes = resp.bytes().await.unwrap_or_default();
            (status, [("content-type", "application/json")], bytes).into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": { "message": format!("上游 Embeddings 请求失败: {e}") } })),
        )
            .into_response(),
    }
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
    let config = ctx.config.read().await;

    // 解析路径: 如 "gemini-1.5-flash:generateContent" 或 "gemini-1.5-flash:streamGenerateContent"
    let (raw_model, is_stream) = if let Some((m, action)) = model_action.rsplit_once(':') {
        let stream = action.contains("stream");
        (m.trim_start_matches('/'), stream)
    } else {
        (model_action.trim_start_matches('/'), false)
    };

    let req_body_str = if config.record_request_body {
        Some(serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string()))
    } else {
        None
    };

    if let Err(res) = check_auth(&headers, &uri, &config).await {
        let dur = start_time.elapsed().as_millis() as u64;
        ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);
        ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
        ctx.record_log(ProxyRequestLog {
            id: req_id,
            timestamp: current_timestamp(),
            method: "POST".to_string(),
            path: format!("/v1/gemini/models/{model_action}"),
            channel_id: "opencode".to_string(),
            model: raw_model.to_string(),
            stream: is_stream,
            status_code: 401,
            duration_ms: dur,
            ttft_ms: None,
            prompt_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
            completion_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            error_message: Some("鉴权未通过：请求未携带有效的 API Key".to_string()),
            request_body: req_body_str,
            response_body: None,
            node_name: Some("直连通道".to_string()),
        })
        .await;
        return res;
    }

    ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);

    let (chan, model_to_send) = match resolve_channel(&config, raw_model) {
        Some((c, m)) => (c, m),
        None => {
            ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": { "code": 503, "message": "未找到可用的上游渠道，请检查渠道是否启用", "status": "UNAVAILABLE" } })),
            )
                .into_response();
        }
    };
    let stripped_model = model_to_send.clone();
    let chan_alias = chan.effective_alias();

    let openai_body =
        GeminiProtocolAdapter::gemini_request_to_openai(&body, &stripped_model, is_stream);
    let upstream_url = format!("{}/chat/completions", chan.base_url.trim_end_matches('/'));
    let channel_api_key = select_channel_api_key(&ctx, chan);
    let candidates = get_sorted_egress_candidates(&ctx, chan).await;
    let max_retries = (config.max_retries as usize).min(candidates.len().saturating_sub(1));

    let mut last_error = String::new();
    let mut last_status = StatusCode::BAD_GATEWAY;

    for (cand_idx, cand_id) in candidates.iter().enumerate() {
        if cand_idx > max_retries && cand_idx > 0 {
            break;
        }

        let cand_start = Instant::now();
        let client = build_client_for_candidate(&ctx, cand_id).await;

        let mut req_builder = client
            .post(&upstream_url)
            .header("Content-Type", "application/json")
            .json(&openai_body);

        if !channel_api_key.is_empty() {
            req_builder = req_builder.header("Authorization", format!("Bearer {channel_api_key}"));
        }

        match req_builder.send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    let log = ProxyRequestLog {
                        id: req_id.clone(),
                        timestamp: current_timestamp(),
                        method: "POST".to_string(),
                        path: format!("/v1/gemini/models/{model_action}"),
                        channel_id: chan_alias.clone(),
                        model: raw_model.to_string(),
                        stream: is_stream,
                        status_code: status.as_u16(),
                        duration_ms: start_time.elapsed().as_millis() as u64,
                        ttft_ms: None,
                        prompt_tokens: None,
                        prompt_cache_hit_tokens: None,
                        prompt_cache_miss_tokens: None,
                        completion_tokens: None,
                        reasoning_tokens: None,
                        total_tokens: None,
                        error_message: None,
                        request_body: req_body_str.clone(),
                        response_body: None,
                        node_name: Some(get_node_display_name(&ctx, cand_id).await),
                    };

                    ctx.metrics.successful_requests.fetch_add(1, Ordering::Relaxed);

                    if is_stream {
                        let stream_body = openai_to_gemini_sse_stream(
                            resp.bytes_stream(),
                            ctx.clone(),
                            log,
                            start_time,
                            raw_model.to_string(),
                        );
                        return Response::builder()
                            .status(StatusCode::OK)
                            .header("Content-Type", "text/event-stream")
                            .header("Cache-Control", "no-cache")
                            .header("Connection", "keep-alive")
                            .body(stream_body)
                            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
                    } else {
                        let resp_bytes = resp.bytes().await.unwrap_or_default();
                        let openai_resp = serde_json::from_slice::<JsonValue>(&resp_bytes).unwrap_or_default();
                        let gemini_resp = GeminiProtocolAdapter::openai_response_to_gemini(&openai_resp, raw_model);

                        let dur = start_time.elapsed().as_millis() as u64;
                        let mut final_log = log;
                        final_log.duration_ms = dur;
                        if let Some(usage) = gemini_resp.get("usageMetadata") {
                            final_log.prompt_tokens = usage.get("promptTokenCount").and_then(JsonValue::as_u64);
                            final_log.completion_tokens = usage.get("candidatesTokenCount").and_then(JsonValue::as_u64);
                            final_log.total_tokens = usage.get("totalTokenCount").and_then(JsonValue::as_u64);
                        }
                        ctx.record_log(final_log).await;

                        return Json(gemini_resp).into_response();
                    }
                } else {
                    let err_bytes = resp.bytes().await.unwrap_or_default();
                    let err_text = String::from_utf8_lossy(&err_bytes).to_string();
                    let formatted = format_upstream_error_message(status.as_u16(), &err_text);
                    last_status = status;
                    last_error = formatted.clone();

                    if cand_idx < candidates.len() - 1 && cand_idx < max_retries {
                        record_failover_event(
                            &ctx,
                            &req_id,
                            &format!("/v1/gemini/models/{model_action}"),
                            &chan_alias,
                            raw_model,
                            is_stream,
                            status.as_u16(),
                            formatted,
                            cand_start.elapsed().as_millis() as u64,
                            req_body_str.clone(),
                            cand_id,
                        )
                        .await;
                    }
                }
            }
            Err(e) => {
                let formatted = format!("连接节点失败: {e}");
                last_error = formatted.clone();
                last_status = StatusCode::BAD_GATEWAY;

                if cand_idx < candidates.len() - 1 && cand_idx < max_retries {
                    record_failover_event(
                        &ctx,
                        &req_id,
                        &format!("/v1/gemini/models/{model_action}"),
                        &chan_alias,
                        raw_model,
                        is_stream,
                        502,
                        formatted,
                        cand_start.elapsed().as_millis() as u64,
                        req_body_str.clone(),
                        cand_id,
                    )
                    .await;
                }
            }
        }
    }

    let dur = start_time.elapsed().as_millis() as u64;
    ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
    ctx.record_log(ProxyRequestLog {
        id: req_id,
        timestamp: current_timestamp(),
        method: "POST".to_string(),
        path: format!("/v1/gemini/models/{model_action}"),
        channel_id: chan_alias,
        model: raw_model.to_string(),
        stream: is_stream,
        status_code: last_status.as_u16(),
        duration_ms: dur,
        ttft_ms: None,
        prompt_tokens: None,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
        completion_tokens: None,
        reasoning_tokens: None,
        total_tokens: None,
        error_message: Some(last_error.clone()),
        request_body: req_body_str,
        response_body: None,
        node_name: Some("已尝试所有候选节点".to_string()),
    })
    .await;

    (
        last_status,
        Json(json!({
            "error": {
                "code": last_status.as_u16(),
                "message": last_error,
                "status": "UNAVAILABLE"
            }
        })),
    )
        .into_response()
}

/// POST /v1/chat/completions
pub async fn handle_chat_completions(
    headers: HeaderMap,
    uri: Uri,
    State(ctx): State<ModelProxyContext>,
    Json(mut body): Json<JsonValue>,
) -> Response {
    let start_time = Instant::now();
    let req_id = generate_req_id();
    let config = ctx.config.read().await;

    let req_body_str = if config.record_request_body {
        Some(serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string()))
    } else {
        None
    };

    if let Err(res) = check_auth(&headers, &uri, &config).await {
        let dur = start_time.elapsed().as_millis() as u64;
        ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);
        ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
        ctx.record_log(ProxyRequestLog {
            id: req_id,
            timestamp: current_timestamp(),
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            channel_id: "opencode".to_string(),
            model: body
                .get("model")
                .and_then(JsonValue::as_str)
                .unwrap_or("deepseek-v4-flash-free")
                .to_string(),
            stream: body
                .get("stream")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false),
            status_code: 401,
            duration_ms: dur,
            ttft_ms: None,
            prompt_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
            completion_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            error_message: Some("鉴权未通过：请求未携带有效的 Bearer API Key".to_string()),
            request_body: req_body_str,
            response_body: None,
            node_name: Some("直连通道".to_string()),
        })
        .await;
        return res;
    }

    ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);

    let raw_model = body
        .get("model")
        .and_then(JsonValue::as_str)
        .unwrap_or("deepseek-v4-flash-free")
        .to_string();
    let (chan, model_to_send) = match resolve_channel(&config, &raw_model) {
        Some((c, m)) => (c, m),
        None => {
            ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": { "type": "api_error", "message": "未找到可用的上游渠道，请检查渠道是否启用" } })),
            )
                .into_response();
        }
    };
    let stripped_model = model_to_send;
    let chan_alias = chan.effective_alias();

    body["model"] = JsonValue::String(stripped_model.clone());
    normalize_chat_messages(&mut body);

    let upstream_url = format!("{}/chat/completions", chan.base_url.trim_end_matches('/'));
    let channel_api_key = select_channel_api_key(&ctx, chan);
    let is_stream = body
        .get("stream")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);

    let candidates = get_sorted_egress_candidates(&ctx, chan).await;
    let max_retries = (config.max_retries as usize).min(candidates.len().saturating_sub(1));

    let mut last_error = String::new();
    let mut last_status = StatusCode::BAD_GATEWAY;

    for (cand_idx, cand_id) in candidates.iter().enumerate() {
        if cand_idx > max_retries && cand_idx > 0 {
            break;
        }

        let cand_start = Instant::now();
        let client = build_client_for_candidate(&ctx, cand_id).await;

        let mut req_builder = client
            .post(&upstream_url)
            .header("Content-Type", "application/json")
            .json(&body);

        if !channel_api_key.is_empty() {
            req_builder = req_builder.header("Authorization", format!("Bearer {channel_api_key}"));
        }

        match req_builder.send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    let log = ProxyRequestLog {
                        id: req_id.clone(),
                        timestamp: current_timestamp(),
                        method: "POST".to_string(),
                        path: "/v1/chat/completions".to_string(),
                        channel_id: chan_alias.clone(),
                        model: raw_model.clone(),
                        stream: is_stream,
                        status_code: status.as_u16(),
                        duration_ms: start_time.elapsed().as_millis() as u64,
                        ttft_ms: None,
                        prompt_tokens: None,
                        prompt_cache_hit_tokens: None,
                        prompt_cache_miss_tokens: None,
                        completion_tokens: None,
                        reasoning_tokens: None,
                        total_tokens: None,
                        error_message: None,
                        request_body: req_body_str.clone(),
                        response_body: None,
                        node_name: Some(get_node_display_name(&ctx, cand_id).await),
                    };

                    ctx.metrics
                        .successful_requests
                        .fetch_add(1, Ordering::Relaxed);

                    if is_stream {
                        let stream_body = clean_sse_stream(
                            resp.bytes_stream(),
                            ctx.clone(),
                            log,
                            start_time,
                            raw_model,
                        );
                        return Response::builder()
                            .status(StatusCode::OK)
                            .header("Content-Type", "text/event-stream")
                            .header("Cache-Control", "no-cache")
                            .header("Connection", "keep-alive")
                            .body(stream_body)
                            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
                    } else {
                        let resp_bytes = resp.bytes().await.unwrap_or_default();
                        let dur = start_time.elapsed().as_millis() as u64;

                        let mut prompt_toks = None;
                        let mut comp_toks = None;
                        let mut reas_toks = None;
                        let mut cache_toks = None;
                        let mut total_toks = None;
                        let mut has_reasoning = false;

                        if let Ok(mut jv) = serde_json::from_slice::<JsonValue>(&resp_bytes) {
                            if let Some(usage) = jv.get("usage").and_then(JsonValue::as_object) {
                                prompt_toks =
                                    usage.get("prompt_tokens").and_then(JsonValue::as_u64);
                                comp_toks =
                                    usage.get("completion_tokens").and_then(JsonValue::as_u64);
                                total_toks = usage.get("total_tokens").and_then(JsonValue::as_u64);

                                if let Some(details) = usage
                                    .get("prompt_tokens_details")
                                    .and_then(JsonValue::as_object)
                                {
                                    cache_toks =
                                        details.get("cached_tokens").and_then(JsonValue::as_u64);
                                }
                                if let Some(details) = usage
                                    .get("completion_tokens_details")
                                    .and_then(JsonValue::as_object)
                                {
                                    reas_toks = details
                                        .get("reasoning_tokens")
                                        .and_then(JsonValue::as_u64);
                                    if reas_toks.is_some() {
                                        has_reasoning = true;
                                    }
                                }
                            }

                            if let Some(msg) = jv.pointer_mut("/choices/0/message") {
                                let mut extracted_reasoning = None;
                                if let Some(content) = msg.get_mut("content") {
                                    if let Some(s) = content.as_str() {
                                        if let (Some(start), Some(end)) =
                                            (s.find("<think>"), s.find("</think>"))
                                        {
                                            if start < end {
                                                let reasoning =
                                                    s[start + 7..end].trim().to_string();
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
                                ctx.metrics
                                    .total_reasoning_requests
                                    .fetch_add(1, Ordering::Relaxed);
                            }
                            if let Some(p) = prompt_toks {
                                ctx.metrics
                                    .total_prompt_tokens
                                    .fetch_add(p, Ordering::Relaxed);
                            }
                            if let Some(c) = comp_toks {
                                ctx.metrics
                                    .total_completion_tokens
                                    .fetch_add(c, Ordering::Relaxed);
                            }
                            if let Some(r) = reas_toks {
                                ctx.metrics
                                    .total_reasoning_tokens
                                    .fetch_add(r, Ordering::Relaxed);
                            }
                            if let Some(h) = cache_toks {
                                ctx.metrics
                                    .total_cache_hit_tokens
                                    .fetch_add(h, Ordering::Relaxed);
                            }
                            if let Some(t) = total_toks {
                                ctx.metrics.total_tokens.fetch_add(t, Ordering::Relaxed);
                            }

                            return Json(jv).into_response();
                        }

                        let mut final_log = log;
                        final_log.duration_ms = dur;
                        ctx.record_log(final_log).await;
                        return (StatusCode::OK, resp_bytes).into_response();
                    }
                } else {
                    let err_bytes = resp.bytes().await.unwrap_or_default();
                    let err_text = String::from_utf8_lossy(&err_bytes).to_string();
                    let formatted = format_upstream_error_message(status.as_u16(), &err_text);
                    last_status = status;
                    last_error = formatted.clone();

                    if cand_idx < candidates.len() - 1 && cand_idx < max_retries {
                        record_failover_event(
                            &ctx,
                            &req_id,
                            "/v1/chat/completions",
                            &chan_alias,
                            &raw_model,
                            is_stream,
                            status.as_u16(),
                            formatted,
                            cand_start.elapsed().as_millis() as u64,
                            req_body_str.clone(),
                            cand_id,
                        )
                        .await;
                    }
                }
            }
            Err(e) => {
                let formatted = format!("连接节点失败: {e}");
                last_error = formatted.clone();
                last_status = StatusCode::BAD_GATEWAY;

                if cand_idx < candidates.len() - 1 && cand_idx < max_retries {
                    record_failover_event(
                        &ctx,
                        &req_id,
                        "/v1/chat/completions",
                        &chan_alias,
                        &raw_model,
                        is_stream,
                        502,
                        formatted,
                        cand_start.elapsed().as_millis() as u64,
                        req_body_str.clone(),
                        cand_id,
                    )
                    .await;
                }
            }
        }
    }

    let dur = start_time.elapsed().as_millis() as u64;
    ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
    ctx.record_log(ProxyRequestLog {
        id: req_id,
        timestamp: current_timestamp(),
        method: "POST".to_string(),
        path: "/v1/chat/completions".to_string(),
        channel_id: chan_alias,
        model: raw_model.to_string(),
        stream: is_stream,
        status_code: last_status.as_u16(),
        duration_ms: dur,
        ttft_ms: None,
        prompt_tokens: None,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
        completion_tokens: None,
        reasoning_tokens: None,
        total_tokens: None,
        error_message: Some(last_error.clone()),
        request_body: req_body_str,
        response_body: None,
        node_name: Some("已尝试所有候选节点".to_string()),
    })
    .await;

    (
        last_status,
        Json(json!({
            "error": {
                "message": last_error,
                "type": "upstream_error",
                "code": last_status.as_u16()
            }
        })),
    )
        .into_response()
}

/// POST /v1/responses
pub async fn handle_responses(
    headers: HeaderMap,
    uri: Uri,
    State(ctx): State<ModelProxyContext>,
    Json(mut body): Json<JsonValue>,
) -> Response {
    let start_time = Instant::now();
    let req_id = generate_req_id();
    let config = ctx.config.read().await;

    let req_body_str = if config.record_request_body {
        Some(serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string()))
    } else {
        None
    };

    if let Err(res) = check_auth(&headers, &uri, &config).await {
        let dur = start_time.elapsed().as_millis() as u64;
        ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);
        ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
        ctx.record_log(ProxyRequestLog {
            id: req_id,
            timestamp: current_timestamp(),
            method: "POST".to_string(),
            path: "/v1/responses".to_string(),
            channel_id: "opencode".to_string(),
            model: body
                .get("model")
                .and_then(JsonValue::as_str)
                .unwrap_or("deepseek-v4-flash-free")
                .to_string(),
            stream: body
                .get("stream")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false),
            status_code: 401,
            duration_ms: dur,
            ttft_ms: None,
            prompt_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
            completion_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            error_message: Some("鉴权未通过：请求未携带有效的 Bearer API Key".to_string()),
            request_body: req_body_str,
            response_body: None,
            node_name: Some("直连通道".to_string()),
        })
        .await;
        return res;
    }

    ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);

    let raw_model = body
        .get("model")
        .and_then(JsonValue::as_str)
        .unwrap_or("deepseek-v4-flash-free")
        .to_string();
    let (chan, model_to_send) = match resolve_channel(&config, &raw_model) {
        Some((c, m)) => (c, m),
        None => {
            ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": { "type": "api_error", "message": "未找到可用的上游渠道，请检查渠道是否启用" } })),
            )
                .into_response();
        }
    };
    let stripped_model = model_to_send;
    let chan_alias = chan.effective_alias();

    body["model"] = JsonValue::String(stripped_model.clone());
    ResponsesProtocolAdapter::convert_input_to_messages(&mut body);
    normalize_chat_messages(&mut body);

    let upstream_url = format!("{}/chat/completions", chan.base_url.trim_end_matches('/'));
    let channel_api_key = select_channel_api_key(&ctx, chan);
    let is_stream = body
        .get("stream")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);

    let candidates = get_sorted_egress_candidates(&ctx, chan).await;
    let max_retries = (config.max_retries as usize).min(candidates.len().saturating_sub(1));

    let mut last_error = String::new();
    let mut last_status = StatusCode::BAD_GATEWAY;

    for (cand_idx, cand_id) in candidates.iter().enumerate() {
        if cand_idx > max_retries && cand_idx > 0 {
            break;
        }

        let cand_start = Instant::now();
        let client = build_client_for_candidate(&ctx, cand_id).await;

        let mut req_builder = client
            .post(&upstream_url)
            .header("Content-Type", "application/json")
            .json(&body);

        if !channel_api_key.is_empty() {
            req_builder = req_builder.header("Authorization", format!("Bearer {channel_api_key}"));
        }

        match req_builder.send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    let log = ProxyRequestLog {
                        id: req_id.clone(),
                        timestamp: current_timestamp(),
                        method: "POST".to_string(),
                        path: "/v1/responses".to_string(),
                        channel_id: chan_alias.clone(),
                        model: raw_model.to_string(),
                        stream: is_stream,
                        status_code: status.as_u16(),
                        duration_ms: start_time.elapsed().as_millis() as u64,
                        ttft_ms: None,
                        prompt_tokens: None,
                        prompt_cache_hit_tokens: None,
                        prompt_cache_miss_tokens: None,
                        completion_tokens: None,
                        reasoning_tokens: None,
                        total_tokens: None,
                        error_message: None,
                        request_body: req_body_str.clone(),
                        response_body: None,
                        node_name: Some(get_node_display_name(&ctx, cand_id).await),
                    };

                    ctx.metrics
                        .successful_requests
                        .fetch_add(1, Ordering::Relaxed);

                    if is_stream {
                        let stream_body = openai_to_responses_sse_stream(
                            resp.bytes_stream(),
                            ctx.clone(),
                            log,
                            start_time,
                            raw_model.to_string(),
                        );
                        return Response::builder()
                            .status(StatusCode::OK)
                            .header("Content-Type", "text/event-stream")
                            .header("Cache-Control", "no-cache")
                            .header("Connection", "keep-alive")
                            .body(stream_body)
                            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
                    } else {
                        let resp_bytes = resp.bytes().await.unwrap_or_default();
                        let dur = start_time.elapsed().as_millis() as u64;

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
                        return (StatusCode::OK, resp_bytes).into_response();
                    }
                } else {
                    let err_bytes = resp.bytes().await.unwrap_or_default();
                    let err_text = String::from_utf8_lossy(&err_bytes).to_string();
                    let formatted = format_upstream_error_message(status.as_u16(), &err_text);
                    last_status = status;
                    last_error = formatted.clone();

                    if cand_idx < candidates.len() - 1 && cand_idx < max_retries {
                        record_failover_event(
                            &ctx,
                            &req_id,
                            "/v1/responses",
                            &chan_alias,
                            &raw_model,
                            is_stream,
                            status.as_u16(),
                            formatted,
                            cand_start.elapsed().as_millis() as u64,
                            req_body_str.clone(),
                            cand_id,
                        )
                        .await;
                    }
                }
            }
            Err(e) => {
                let formatted = format!("连接节点失败: {e}");
                last_error = formatted.clone();
                last_status = StatusCode::BAD_GATEWAY;

                if cand_idx < candidates.len() - 1 && cand_idx < max_retries {
                    record_failover_event(
                        &ctx,
                        &req_id,
                        "/v1/responses",
                        &chan_alias,
                        &raw_model,
                        is_stream,
                        502,
                        formatted,
                        cand_start.elapsed().as_millis() as u64,
                        req_body_str.clone(),
                        cand_id,
                    )
                    .await;
                }
            }
        }
    }

    let dur = start_time.elapsed().as_millis() as u64;
    ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
    ctx.record_log(ProxyRequestLog {
        id: req_id,
        timestamp: current_timestamp(),
        method: "POST".to_string(),
        path: "/v1/responses".to_string(),
        channel_id: chan_alias,
        model: raw_model.to_string(),
        stream: is_stream,
        status_code: last_status.as_u16(),
        duration_ms: dur,
        ttft_ms: None,
        prompt_tokens: None,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
        completion_tokens: None,
        reasoning_tokens: None,
        total_tokens: None,
        error_message: Some(last_error.clone()),
        request_body: req_body_str,
        response_body: None,
        node_name: Some("已尝试所有候选节点".to_string()),
    })
    .await;

    (
        last_status,
        Json(json!({
            "error": {
                "message": last_error,
                "type": "upstream_error",
                "code": last_status.as_u16()
            }
        })),
    )
        .into_response()
}

/// POST /v1/messages (Anthropic 协议适配与转发)
pub async fn handle_messages(
    headers: HeaderMap,
    uri: Uri,
    State(ctx): State<ModelProxyContext>,
    Json(body): Json<JsonValue>,
) -> Response {
    let start_time = Instant::now();
    let req_id = generate_req_id();
    let config = ctx.config.read().await;

    let req_body_str = if config.record_request_body {
        Some(serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string()))
    } else {
        None
    };

    if let Err(res) = check_auth(&headers, &uri, &config).await {
        let dur = start_time.elapsed().as_millis() as u64;
        record_auth_failure(
            &ctx,
            &req_id,
            "/v1/messages",
            body.get("model").and_then(JsonValue::as_str).unwrap_or("free-claude-3-5-sonnet"),
            body.get("stream").and_then(JsonValue::as_bool).unwrap_or(false),
            dur,
            req_body_str,
        )
        .await;
        return res;
    }

    ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);

    let raw_model = body
        .get("model")
        .and_then(JsonValue::as_str)
        .unwrap_or("deepseek-v4-flash-free");
    let (chan, model_to_send) = match resolve_channel(&config, raw_model) {
        Some((c, m)) => (c, m),
        None => {
            ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": { "type": "api_error", "message": "未找到可用的上游渠道，请检查渠道是否启用" } })),
            )
                .into_response();
        }
    };
    let stripped_model = model_to_send;
    let chan_alias = chan.effective_alias();

    let is_stream = body.get("stream").and_then(JsonValue::as_bool).unwrap_or(false);
    let mut openai_payload =
        AnthropicProtocolAdapter::anthropic_request_to_openai(&body, &stripped_model, is_stream);
    normalize_chat_messages(&mut openai_payload);

    let upstream_url = format!("{}/chat/completions", chan.base_url.trim_end_matches('/'));
    let channel_api_key = select_channel_api_key(&ctx, chan);
    let candidates = get_sorted_egress_candidates(&ctx, chan).await;
    let max_retries = (config.max_retries as usize).min(candidates.len().saturating_sub(1));

    let mut last_error = String::new();
    let mut last_status = StatusCode::BAD_GATEWAY;

    for (cand_idx, cand_id) in candidates.iter().enumerate() {
        if cand_idx > max_retries && cand_idx > 0 {
            break;
        }

        let cand_start = Instant::now();
        let client = build_client_for_candidate(&ctx, cand_id).await;

        let mut req_builder = client
            .post(&upstream_url)
            .header("Content-Type", "application/json")
            .json(&openai_payload);

        if !channel_api_key.is_empty() {
            req_builder = req_builder.header("Authorization", format!("Bearer {channel_api_key}"));
        }

        match req_builder.send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    let log = ProxyRequestLog {
                        id: req_id.clone(),
                        timestamp: current_timestamp(),
                        method: "POST".to_string(),
                        path: "/v1/messages".to_string(),
                        channel_id: chan_alias.clone(),
                        model: raw_model.to_string(),
                        stream: is_stream,
                        status_code: status.as_u16(),
                        duration_ms: start_time.elapsed().as_millis() as u64,
                        ttft_ms: None,
                        prompt_tokens: None,
                        prompt_cache_hit_tokens: None,
                        prompt_cache_miss_tokens: None,
                        completion_tokens: None,
                        reasoning_tokens: None,
                        total_tokens: None,
                        error_message: None,
                        request_body: req_body_str.clone(),
                        response_body: None,
                        node_name: Some(get_node_display_name(&ctx, cand_id).await),
                    };

                    ctx.metrics
                        .successful_requests
                        .fetch_add(1, Ordering::Relaxed);

                    if is_stream {
                        let stream_body = openai_to_anthropic_sse_stream(
                            resp.bytes_stream(),
                            ctx.clone(),
                            log,
                            start_time,
                            raw_model.to_string(),
                        );
                        return Response::builder()
                            .status(StatusCode::OK)
                            .header("Content-Type", "text/event-stream")
                            .header("Cache-Control", "no-cache")
                            .header("Connection", "keep-alive")
                            .body(stream_body)
                            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
                    } else {
                        let resp_bytes = resp.bytes().await.unwrap_or_default();
                        let dur = start_time.elapsed().as_millis() as u64;

                        if let Ok(jv) = serde_json::from_slice::<JsonValue>(&resp_bytes) {
                            let (p_tok, c_tok) = AnthropicProtocolAdapter::extract_token_usage(&jv);
                            let anthropic_resp = AnthropicProtocolAdapter::openai_response_to_anthropic(&jv, &req_id, raw_model);

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
                        return (StatusCode::OK, resp_bytes).into_response();
                    }
                } else {
                    let err_bytes = resp.bytes().await.unwrap_or_default();
                    let err_text = String::from_utf8_lossy(&err_bytes).to_string();
                    let formatted = format_upstream_error_message(status.as_u16(), &err_text);
                    last_status = status;
                    last_error = formatted.clone();

                    if cand_idx < candidates.len() - 1 && cand_idx < max_retries {
                        record_failover_event(
                            &ctx,
                            &req_id,
                            "/v1/messages",
                            &chan_alias,
                            raw_model,
                            is_stream,
                            status.as_u16(),
                            formatted,
                            cand_start.elapsed().as_millis() as u64,
                            req_body_str.clone(),
                            cand_id,
                        )
                        .await;
                    }
                }
            }
            Err(e) => {
                let formatted = format!("连接节点失败: {e}");
                last_error = formatted.clone();
                last_status = StatusCode::BAD_GATEWAY;

                if cand_idx < candidates.len() - 1 && cand_idx < max_retries {
                    record_failover_event(
                        &ctx,
                        &req_id,
                        "/v1/messages",
                        &chan_alias,
                        raw_model,
                        is_stream,
                        502,
                        formatted,
                        cand_start.elapsed().as_millis() as u64,
                        req_body_str.clone(),
                        cand_id,
                    )
                    .await;
                }
            }
        }
    }

    let dur = start_time.elapsed().as_millis() as u64;
    ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
    ctx.record_log(ProxyRequestLog {
        id: req_id,
        timestamp: current_timestamp(),
        method: "POST".to_string(),
        path: "/v1/messages".to_string(),
        channel_id: chan_alias,
        model: raw_model.to_string(),
        stream: is_stream,
        status_code: last_status.as_u16(),
        duration_ms: dur,
        ttft_ms: None,
        prompt_tokens: None,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
        completion_tokens: None,
        reasoning_tokens: None,
        total_tokens: None,
        error_message: Some(last_error.clone()),
        request_body: req_body_str,
        response_body: None,
        node_name: Some("已尝试所有候选节点".to_string()),
    })
    .await;

    (
        last_status,
        Json(json!({
            "type": "error",
            "error": {
                "type": "api_error",
                "message": last_error
            }
        })),
    )
        .into_response()
}

fn resolve_candidate_models_urls(base_url: &str) -> Vec<String> {
    let clean = base_url.trim().trim_end_matches('/');
    if clean.ends_with("/v1") {
        vec![
            format!("{clean}/models"),
            format!("{}/models", clean.trim_end_matches("/v1")),
        ]
    } else {
        vec![
            format!("{clean}/v1/models"),
            format!("{clean}/models"),
        ]
    }
}

fn extract_models_from_json(jv: &JsonValue) -> Vec<String> {
    let mut models = Vec::new();
    if let Some(arr) = jv.get("data").and_then(JsonValue::as_array) {
        for item in arr {
            if let Some(id) = item.get("id").and_then(JsonValue::as_str) {
                models.push(id.to_string());
            } else if let Some(s) = item.as_str() {
                models.push(s.to_string());
            }
        }
    } else if let Some(arr) = jv.get("models").and_then(JsonValue::as_array) {
        for item in arr {
            if let Some(id) = item.get("id").and_then(JsonValue::as_str) {
                models.push(id.to_string());
            } else if let Some(s) = item.as_str() {
                models.push(s.to_string());
            }
        }
    } else if let Some(arr) = jv.as_array() {
        for item in arr {
            if let Some(id) = item.get("id").and_then(JsonValue::as_str) {
                models.push(id.to_string());
            } else if let Some(s) = item.as_str() {
                models.push(s.to_string());
            }
        }
    }
    models.sort();
    models.dedup();
    models
}

pub async fn fetch_upstream_models_inner(ctx: &ModelProxyContext) {
    let channels = {
        let cfg = ctx.config.read().await;
        cfg.channels.clone()
    };

    let mut result_list = Vec::new();
    let mut error_list = Vec::new();

    for ch in channels {
        if !ch.enabled {
            continue;
        }

        let candidates = get_sorted_egress_candidates(ctx, &ch).await;
        let candidate_id = candidates.first().map(|s| s.as_str()).unwrap_or("__direct__");
        let client = build_client_for_candidate(ctx, candidate_id).await;

        let candidate_urls = resolve_candidate_models_urls(&ch.base_url);
        let api_key = select_channel_api_key(ctx, &ch);

        let mut fetched_models: Option<Vec<String>> = None;
        let mut fetch_err: Option<String> = None;

        for url in candidate_urls {
            let mut req = client.get(&url);
            if !api_key.is_empty() {
                req = req.header("Authorization", format!("Bearer {api_key}"));
            }
            req = req.header("User-Agent", "OpenHub-ModelProxy/1.0");

            match req.send().await {
                Ok(resp) => {
                    if resp.status().is_success() {
                        if let Ok(jv) = resp.json::<JsonValue>().await {
                            let models = extract_models_from_json(&jv);
                            if !models.is_empty() {
                                fetched_models = Some(models);
                                break;
                            }
                        }
                    } else {
                        let status = resp.status();
                        let body_text = resp.text().await.unwrap_or_default();
                        fetch_err = Some(format!("HTTP {} 接口错误: {}", status.as_u16(), body_text));
                    }
                }
                Err(e) => {
                    fetch_err = Some(format!("网络连接失败: {e}"));
                }
            }
        }

        if fetched_models.is_none() {
            if let Some(site_id) = &ch.site_id {
                if let Some(app) = ctx.app_handle.read().await.as_ref() {
                    let database = app.state::<crate::models::Database>();
                    let local_models = {
                        if let Ok(conn) = database.0.lock() {
                            let mut list = Vec::new();
                            if let Ok(mut stmt) = conn.prepare("SELECT models_json FROM site_model_cache WHERE site_id = ?1") {
                                if let Ok(rows) = stmt.query_map([site_id.as_str()], |row| row.get::<_, String>(0)) {
                                    for r in rows.flatten() {
                                        if let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(&r) {
                                            for it in items {
                                                if let Some(id) = it.get("id").and_then(|v| v.as_str()) {
                                                    list.push(id.to_string());
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            list
                        } else {
                            Vec::new()
                        }
                    };
                    if !local_models.is_empty() {
                        let mut sorted = local_models;
                        sorted.sort();
                        sorted.dedup();
                        fetched_models = Some(sorted);
                    }
                }
            }
        }

        if let Some(mut models) = fetched_models {
            // 如果是 OpenCode 且未配置有效 API Key（免 Key 免费通道），仅展示免 Key 可用的免费模型
            let is_opencode = ch.id == "opencode"
                || ch.protocol == "opencode"
                || ch.alias.as_deref() == Some("opencode")
                || ch.base_url.contains("opencode.ai")
                || ch.name.to_lowercase().contains("opencode");
            let has_key = !ch.get_effective_keys().is_empty();
            if is_opencode && !has_key {
                models.retain(|m| {
                    let lower = m.to_lowercase();
                    lower.contains("free") || lower == "big-pickle"
                });
            }

            result_list.push(ChannelModelList {
                channel_id: ch.id.clone(),
                channel_name: ch.name.clone(),
                alias: ch.effective_alias(),
                models,
            });
        } else if let Some(error) = fetch_err {
            error_list.push(ChannelModelFetchError {
                channel_id: ch.id.clone(),
                channel_name: ch.name.clone(),
                alias: ch.effective_alias(),
                error,
            });
        }
    }

    *ctx.cached_channel_models.write().await = result_list;
    *ctx.cached_fetch_errors.write().await = error_list;
}
