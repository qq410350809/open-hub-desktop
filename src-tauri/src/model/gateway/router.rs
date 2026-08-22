use super::balancer::{
    is_free_opencode_model, is_opencode_channel, select_channel_api_key,
};
use super::handlers::{
    handle_chat_completions, handle_gemini_generate, handle_messages, handle_responses,
};
use super::types::{
    ChannelModelFetchError, ChannelModelList, ModelProxyConfig, ModelProxyContext,
};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value as JsonValue};
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
        if is_opencode_channel(ch) || ch.base_url.contains("opencode.ai") {
            req = req
                .header("User-Agent", "opencode/1.18.18/cli")
                .header("x-opencode-client", "cli");
        } else {
            req = req.header("User-Agent", "OpenHub-Gateway/0.3.0");
        }
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

