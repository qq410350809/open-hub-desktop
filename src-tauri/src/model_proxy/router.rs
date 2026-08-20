use super::adapters::{
    normalize_chat_messages, GeminiProtocolAdapter, ResponsesProtocolAdapter,
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

pub fn create_model_proxy_router(ctx: ModelProxyContext) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/healthz", get(handle_healthz))
        .route("/v1/models", get(handle_models))
        .route("/v1/chat/completions", post(handle_chat_completions))
        .route("/v1/responses", post(handle_responses))
        .route("/v1/messages", post(handle_messages))
        .route("/v1beta/models", get(handle_gemini_models))
        .route("/v1beta/models/*model_action", post(handle_gemini_generate))
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

    let checks = json!([
        {
            "name": "本地模型反代网关 (Gateway)",
            "endpoint": format!("http://127.0.0.1:{}/v1", config.port),
            "status": "ok",
            "message": format!("网关正常运行中，已运行 {} 秒", uptime),
            "auth": auth_desc
        },
        {
            "name": "Google Gemini 兼容端点",
            "endpoint": format!("http://127.0.0.1:{}/v1beta", config.port),
            "status": "ok",
            "message": "已支持 /v1beta/models/* 原生请求",
            "auth": "Header 或 ?key="
        },
        {
            "name": "Anthropic Claude 兼容端点",
            "endpoint": format!("http://127.0.0.1:{}/v1/messages", config.port),
            "status": "ok",
            "message": "已支持 Claude Desktop / Cline / Cursor 等工具直连",
            "auth": "x-api-key 或 Bearer"
        },
        {
            "name": "多渠道负载均衡与代理池",
            "endpoint": proxy_pool_desc,
            "status": "ok",
            "message": format!("共配置 {} 个上游渠道，聚合 {} 个模型", config.channels.len(), models_count),
            "auth": "多 Key 自动轮询"
        }
    ]);

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

    Json(json!({
        "object": "list",
        "data": model_items
    }))
    .into_response()
}

/// GET /v1beta/models (Google Gemini 格式)
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

/// POST /v1beta/models/* (Google Gemini 原生协议端点)
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
            path: format!("/v1beta/models/{model_action}"),
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
                        path: format!("/v1beta/models/{model_action}"),
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
                            &format!("/v1beta/models/{model_action}"),
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
                        &format!("/v1beta/models/{model_action}"),
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
        path: format!("/v1beta/models/{model_action}"),
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
        ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);
        ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
        ctx.record_log(ProxyRequestLog {
            id: req_id,
            timestamp: current_timestamp(),
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            channel_id: "opencode".to_string(),
            model: body
                .get("model")
                .and_then(JsonValue::as_str)
                .unwrap_or("free-claude-3-5-sonnet")
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

    let mut openai_tools = Vec::new();
    if let Some(tools_arr) = body.get("tools").and_then(JsonValue::as_array) {
        for t in tools_arr {
            let mut name = String::new();
            let mut desc = String::new();
            let mut schema = json!({"type": "object", "properties": {}});

            if let Some(n) = t.get("name").and_then(JsonValue::as_str) {
                name = n.trim().to_string();
            }
            if let Some(d) = t.get("description").and_then(JsonValue::as_str) {
                desc = d.to_string();
            }
            if let Some(s) = t.get("input_schema").or_else(|| t.get("parameters")) {
                schema = s.clone();
            }

            if name.is_empty() {
                if let Some(f) = t.get("function").and_then(JsonValue::as_object) {
                    if let Some(n) = f.get("name").and_then(JsonValue::as_str) {
                        name = n.trim().to_string();
                    }
                    if let Some(d) = f.get("description").and_then(JsonValue::as_str) {
                        desc = d.to_string();
                    }
                    if let Some(p) = f.get("parameters") {
                        schema = p.clone();
                    }
                }
            }

            if !name.is_empty() {
                openai_tools.push(json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": desc,
                        "parameters": schema
                    }
                }));
            }
        }
    }

    let mut messages = Vec::new();

    if let Some(system_val) = body.get("system") {
        if let Some(sys_str) = system_val.as_str() {
            if !sys_str.is_empty() {
                messages.push(json!({
                    "role": "system",
                    "content": sys_str
                }));
            }
        } else if let Some(sys_arr) = system_val.as_array() {
            let mut combined = String::new();
            for item in sys_arr {
                if let Some(t) = item.get("text").and_then(JsonValue::as_str) {
                    if !combined.is_empty() {
                        combined.push('\n');
                    }
                    combined.push_str(t);
                }
            }
            if !combined.is_empty() {
                messages.push(json!({
                    "role": "system",
                    "content": combined
                }));
            }
        }
    }

    if let Some(msgs_arr) = body.get("messages").and_then(JsonValue::as_array) {
        for msg in msgs_arr {
            let role = msg.get("role").and_then(JsonValue::as_str).unwrap_or("user");
            if let Some(content_val) = msg.get("content") {
                if let Some(c_str) = content_val.as_str() {
                    messages.push(json!({
                        "role": role,
                        "content": c_str
                    }));
                } else if let Some(c_arr) = content_val.as_array() {
                    let mut text_parts = String::new();
                    let mut tool_calls = Vec::new();
                    let mut tool_results = Vec::new();

                    for block in c_arr {
                        let b_type = block.get("type").and_then(JsonValue::as_str).unwrap_or("");
                        match b_type {
                            "text" => {
                                if let Some(t) = block.get("text").and_then(JsonValue::as_str) {
                                    if !text_parts.is_empty() {
                                        text_parts.push('\n');
                                    }
                                    text_parts.push_str(t);
                                }
                            }
                            "tool_use" => {
                                let id = block.get("id").and_then(JsonValue::as_str).unwrap_or("call_default").to_string();
                                let name = block.get("name").and_then(JsonValue::as_str).unwrap_or("").to_string();
                                let input_val = block.get("input").cloned().unwrap_or_else(|| json!({}));
                                if !name.is_empty() {
                                    tool_calls.push(json!({
                                        "id": id,
                                        "type": "function",
                                        "function": {
                                            "name": name,
                                            "arguments": input_val.to_string()
                                        }
                                    }));
                                }
                            }
                            "tool_result" => {
                                let tool_use_id = block.get("tool_use_id").and_then(JsonValue::as_str).unwrap_or("call_default");
                                let mut res_str = String::new();
                                if let Some(c) = block.get("content") {
                                    if let Some(s) = c.as_str() {
                                        res_str = s.to_string();
                                    } else if let Some(arr) = c.as_array() {
                                        for part in arr {
                                            if let Some(t) = part.get("text").and_then(JsonValue::as_str) {
                                                res_str.push_str(t);
                                            }
                                        }
                                    }
                                }
                                tool_results.push(json!({
                                    "role": "tool",
                                    "tool_call_id": tool_use_id,
                                    "content": res_str
                                }));
                            }
                            _ => {}
                        }
                    }

                    if !tool_results.is_empty() {
                        for tr in tool_results {
                            messages.push(tr);
                        }
                    } else if !tool_calls.is_empty() {
                        messages.push(json!({
                            "role": "assistant",
                            "content": if text_parts.is_empty() { JsonValue::Null } else { JsonValue::String(text_parts) },
                            "tool_calls": tool_calls
                        }));
                    } else {
                        messages.push(json!({
                            "role": role,
                            "content": text_parts
                        }));
                    }
                }
            }
        }
    }

    let is_stream = body.get("stream").and_then(JsonValue::as_bool).unwrap_or(false);
    let mut openai_payload = json!({
        "model": stripped_model,
        "messages": messages,
        "stream": is_stream
    });

    if let Some(temp) = body.get("temperature").and_then(JsonValue::as_f64) {
        openai_payload["temperature"] = json!(temp);
    }
    if let Some(max_tokens) = body.get("max_tokens").and_then(JsonValue::as_i64) {
        openai_payload["max_tokens"] = json!(max_tokens);
    }
    if !openai_tools.is_empty() {
        openai_payload["tools"] = json!(openai_tools);
    }

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
                            let text = jv
                                .pointer("/choices/0/message/content")
                                .and_then(JsonValue::as_str)
                                .unwrap_or("");
                            let mut content_blocks = Vec::new();
                            if !text.is_empty() {
                                content_blocks.push(json!({
                                    "type": "text",
                                    "text": text
                                }));
                            }

                            if let Some(tool_calls) = jv
                                .pointer("/choices/0/message/tool_calls")
                                .and_then(JsonValue::as_array)
                            {
                                for tc in tool_calls {
                                    let id = tc.get("id").and_then(JsonValue::as_str).unwrap_or("call_default");
                                    let name = tc.pointer("/function/name").and_then(JsonValue::as_str).unwrap_or("tool");
                                    let args_str = tc.pointer("/function/arguments").and_then(JsonValue::as_str).unwrap_or("{}");
                                    let args_val = serde_json::from_str::<JsonValue>(args_str).unwrap_or_else(|_| json!({}));
                                    content_blocks.push(json!({
                                        "type": "tool_use",
                                        "id": id,
                                        "name": name,
                                        "input": args_val
                                    }));
                                }
                            }

                            let finish_reason = jv
                                .pointer("/choices/0/finish_reason")
                                .and_then(JsonValue::as_str);
                            let stop_reason = match finish_reason {
                                Some("stop") => "end_turn",
                                Some("length") => "max_tokens",
                                Some("tool_calls") => "tool_use",
                                _ => "end_turn",
                            };

                            let usage = jv.get("usage");
                            let p_tok = usage.and_then(|u| u.get("prompt_tokens")).and_then(JsonValue::as_u64).unwrap_or(0);
                            let c_tok = usage.and_then(|u| u.get("completion_tokens")).and_then(JsonValue::as_u64).unwrap_or(0);

                            let anthropic_resp = json!({
                                "id": format!("msg_{req_id}"),
                                "type": "message",
                                "role": "assistant",
                                "model": raw_model,
                                "content": content_blocks,
                                "stop_reason": stop_reason,
                                "stop_sequence": null,
                                "usage": {
                                    "input_tokens": p_tok,
                                    "output_tokens": c_tok
                                }
                            });

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

        let models_url = format!("{}/models", ch.base_url.trim_end_matches('/'));
        let mut req = client.get(&models_url);
        let api_key = select_channel_api_key(ctx, &ch);
        if !api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {api_key}"));
        }

        match req.send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    if let Ok(jv) = resp.json::<JsonValue>().await {
                        let mut models = Vec::new();
                        if let Some(arr) = jv.get("data").and_then(JsonValue::as_array) {
                            for item in arr {
                                if let Some(id) = item.get("id").and_then(JsonValue::as_str) {
                                    models.push(id.to_string());
                                }
                            }
                        }
                        models.sort();
                        models.dedup();
                        result_list.push(ChannelModelList {
                            channel_id: ch.id.clone(),
                            channel_name: ch.name.clone(),
                            alias: ch.effective_alias(),
                            models,
                        });
                    } else {
                        error_list.push(ChannelModelFetchError {
                            channel_id: ch.id.clone(),
                            channel_name: ch.name.clone(),
                            alias: ch.effective_alias(),
                            error: "模型列表 JSON 解析失败".to_string(),
                        });
                    }
                } else {
                    let status = resp.status();
                    let body_text = resp.text().await.unwrap_or_default();
                    error_list.push(ChannelModelFetchError {
                        channel_id: ch.id.clone(),
                        channel_name: ch.name.clone(),
                        alias: ch.effective_alias(),
                        error: format!("HTTP {} 接口错误: {}", status.as_u16(), body_text),
                    });
                }
            }
            Err(e) => {
                error_list.push(ChannelModelFetchError {
                    channel_id: ch.id.clone(),
                    channel_name: ch.name.clone(),
                    alias: ch.effective_alias(),
                    error: format!("网络连接失败: {e}"),
                });
            }
        }
    }

    *ctx.cached_channel_models.write().await = result_list;
    *ctx.cached_fetch_errors.write().await = error_list;
}
