use super::handlers::{
    handle_chat_completions, handle_gemini_generate, handle_messages, handle_responses,
};
use super::types::{ChannelModelList, ModelProxyConfig, ModelProxyContext};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value as JsonValue};
use tower_http::cors::{Any, CorsLayer};

pub use super::model_sync::fetch_upstream_models_inner;

#[allow(dead_code)]
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
        .route(
            "/v1/gemini/models/{*model_action}",
            post(handle_gemini_generate),
        )
        .route("/v1beta/models", get(handle_gemini_models))
        .route(
            "/v1beta/models/{*model_action}",
            post(handle_gemini_generate),
        )
        .layer(cors)
        .with_state(ctx)
}

/// Router mounted by the main OpenHub service. It intentionally excludes the
/// root/legacy aliases so `/` and `/api/*` remain owned by the Web service.
pub fn create_shared_model_proxy_router(ctx: ModelProxyContext) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/v1/health", get(handle_healthz))
        .route("/v1/models", get(handle_models))
        .route("/v1/models/{model_id}", get(handle_single_model))
        .route("/v1/chat/completions", post(handle_chat_completions))
        .route("/v1/responses", post(handle_responses))
        .route("/v1/messages", post(handle_messages))
        .route("/v1/gemini/models", get(handle_gemini_models))
        .route(
            "/v1/gemini/models/{*model_action}",
            post(handle_gemini_generate),
        )
        .route("/v1beta/models", get(handle_gemini_models))
        .route(
            "/v1beta/models/{*model_action}",
            post(handle_gemini_generate),
        )
        .layer(cors)
        .with_state(ctx)
}

/// 统一鉴权检查
pub fn gateway_disabled_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": {
                "message": "模型网关当前未启用",
                "type": "server_error",
                "code": "gateway_disabled"
            }
        })),
    )
        .into_response()
}

pub async fn check_auth(
    headers: &HeaderMap,
    _uri: &Uri,
    config: &ModelProxyConfig,
) -> Result<(), Response> {
    let auth_header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .or_else(|| headers.get("x-api-key").and_then(|v| v.to_str().ok()))
        .or_else(|| headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()))
        .unwrap_or_default();

    let token = auth_header
        .strip_prefix("Bearer ")
        .or_else(|| auth_header.strip_prefix("bearer "))
        .unwrap_or(auth_header)
        .trim();

    if !config.api_key.trim().is_empty() && token == config.api_key.trim() {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": {
                    "message": "Invalid API key (模型接口 API Key 校验未通过)",
                    "type": "invalid_request_error",
                    "code": "invalid_api_key"
                }
            })),
        )
            .into_response())
    }
}

/// GET /healthz
pub async fn handle_healthz(
    headers: HeaderMap,
    uri: Uri,
    State(ctx): State<ModelProxyContext>,
) -> Response {
    let config = ctx.config.read().await;
    if !ctx.route_enabled.load(std::sync::atomic::Ordering::Acquire) {
        return gateway_disabled_response();
    }
    if let Err(res) = check_auth(&headers, &uri, &config).await {
        return res;
    }
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

    let endpoints: &[(&str, &str, &str)] = &[
        (
            "本地模型反代网关 (Gateway)",
            "/v1",
            "网关正常运行中，已运行 {uptime} 秒",
        ),
        (
            "Google Gemini 兼容端点",
            "/v1/gemini",
            "已支持 /v1/gemini/models/* 原生请求",
        ),
        (
            "Anthropic Claude 兼容端点",
            "/v1/messages",
            "已支持 Claude Desktop / Cline / Cursor 等工具直连",
        ),
    ];

    let mut checks: Vec<JsonValue> = endpoints
        .iter()
        .map(|(name, path, msg_tmpl)| {
            let message = if msg_tmpl.contains("{uptime}") {
                msg_tmpl.replace("{uptime}", &uptime.to_string())
            } else {
                msg_tmpl.to_string()
            };
            json!({
                "name": name,
                "endpoint": format!("/v1{}", path.strip_prefix("/v1").unwrap_or(path)),
                "status": "ok",
                "message": message,
                "auth": "API Key (Authorization: Bearer / x-api-key)",
            })
        })
        .collect();

    // 动态端点：多渠道负载均衡信息
    checks.push(json!({
        "name": "多渠道负载均衡与代理池",
        "endpoint": "/v1",
        "status": "ok",
        "message": format!("共配置 {} 个上游渠道，聚合 {} 个模型", config.channels.len(), models_count),
        "auth": "API Key (Authorization: Bearer / x-api-key)"
    }));

    Json(json!({
        "status": "ok",
        "service": "OpenHub Local LLM Gateway",
        "port": ctx.current_port.read().await.to_owned(),
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
    if !ctx.route_enabled.load(std::sync::atomic::Ordering::Acquire) {
        return gateway_disabled_response();
    }
    if let Err(res) = check_auth(&headers, &uri, &config).await {
        return res;
    }

    let channel_models = ctx.cached_channel_models.read().await.clone();
    let model_items = build_gateway_model_list(&config, &channel_models);

    Json(json!({
        "object": "list",
        "data": model_items
    }))
    .into_response()
}

fn strip_channel_alias_prefix<'a>(alias: &str, model: &'a str) -> &'a str {
    let prefix = format!("{alias}/");
    model.strip_prefix(&prefix).unwrap_or(model)
}

/// 对外暴露的模型 ID：始终带渠道别名前缀，且只带一次。
pub(crate) fn listed_model_id(alias: &str, model: &str) -> String {
    format!("{alias}/{}", strip_channel_alias_prefix(alias, model))
}

fn channel_allows_listed_model(allowed: Option<&Vec<String>>, alias: &str, model: &str) -> bool {
    let Some(allowed) = allowed else {
        return true;
    };
    let bare = strip_channel_alias_prefix(alias, model);
    allowed
        .iter()
        .any(|item| strip_channel_alias_prefix(alias, item) == bare)
}

fn push_listed_model(items: &mut Vec<JsonValue>, alias: &str, model: &str) {
    let id = listed_model_id(alias, model);
    if items
        .iter()
        .any(|item| item.get("id").and_then(JsonValue::as_str) == Some(id.as_str()))
    {
        return;
    }
    let root = strip_channel_alias_prefix(alias, model);
    items.push(json!({
        "id": id,
        "object": "model",
        "created": 1700000000,
        "owned_by": alias,
        "permission": [],
        "root": root,
        "parent": null
    }));
}

/// 组装 GET /v1/models 列表：对外模型 ID 统一带渠道别名前缀。
pub(crate) fn build_gateway_model_list(
    config: &ModelProxyConfig,
    channel_models: &[ChannelModelList],
) -> Vec<JsonValue> {
    let mut model_items = Vec::new();

    for entry in channel_models {
        let channel = config.channels.iter().find(|c| c.id == entry.channel_id);
        if let Some(ch) = channel {
            if !ch.enabled {
                continue;
            }
            let eff_alias = ch.effective_alias();
            for m in &entry.models {
                if !channel_allows_listed_model(ch.enabled_models.as_ref(), &eff_alias, m) {
                    continue;
                }
                push_listed_model(&mut model_items, &eff_alias, m);
            }
        }
    }

    for ch in &config.channels {
        if !ch.enabled {
            continue;
        }
        let eff_alias = ch.effective_alias();
        if let Some(explicit_models) = &ch.enabled_models {
            for m in explicit_models {
                push_listed_model(&mut model_items, &eff_alias, m);
            }
        }
    }

    model_items
}

/// GET /v1/models/:model_id (单个模型查询)
pub async fn handle_single_model(
    headers: HeaderMap,
    uri: Uri,
    Path(model_id): Path<String>,
    State(ctx): State<ModelProxyContext>,
) -> Response {
    let config = ctx.config.read().await;
    if !ctx.route_enabled.load(std::sync::atomic::Ordering::Acquire) {
        return gateway_disabled_response();
    }
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
    if !ctx.route_enabled.load(std::sync::atomic::Ordering::Acquire) {
        return gateway_disabled_response();
    }
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

                let full_id = listed_model_id(&eff_alias, &m);
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
