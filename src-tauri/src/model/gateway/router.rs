use super::balancer::resolve_channel_api_keys;
use super::handlers::{
    handle_chat_completions, handle_gemini_generate, handle_messages, handle_responses,
};
use super::policies::opencode::{
    apply_models_probe_identity, is_free_opencode_model, is_opencode_channel,
};
use super::types::{ChannelModelFetchError, ChannelModelList, ModelProxyConfig, ModelProxyContext};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value as JsonValue};
use std::collections::HashSet;
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info, warn};

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

#[allow(dead_code)]
pub fn create_opencode_proxy_router(ctx: ModelProxyContext) -> Router {
    create_model_proxy_router(ctx)
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
                if !model_items
                    .iter()
                    .any(|item| item.get("id").and_then(JsonValue::as_str) == Some(&full_id))
                {
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
pub async fn fetch_upstream_models_inner(
    ctx: &ModelProxyContext,
    channel_id: Option<&str>,
) -> (Vec<ChannelModelList>, Vec<ChannelModelFetchError>) {
    let config = ctx.config.read().await.clone();
    let mut channel_models = Vec::new();
    let mut fetch_errors = Vec::new();

    for ch in &config.channels {
        if !ch.enabled || channel_id.is_some_and(|id| id != ch.id) {
            continue;
        }

        let api_keys = resolve_channel_api_keys(ctx, ch).await;
        if ch
            .site_id
            .as_deref()
            .is_some_and(|site_id| !site_id.trim().is_empty())
        {
            if api_keys.is_empty() {
                warn!(
                    "[ModelGateway] 站点渠道未找到运行时 API Key：channel_id={} site_id={}；请先同步该站点的模型/Key 数据",
                    ch.id,
                    ch.site_id.as_deref().unwrap_or_default()
                );
            } else {
                info!(
                    "[ModelGateway] 站点渠道使用运行时 API Key：channel_id={} site_id={} key_count={}",
                    ch.id,
                    ch.site_id.as_deref().unwrap_or_default(),
                    api_keys.len()
                );
            }
        }
        let client = ctx.default_http_client.read().await.clone();
        let base = ch.base_url.trim_end_matches('/');

        // 优先探测标准 OpenAI 端点。裸域名的 /models 常被前端 SPA fallback 成 HTML，
        // 只有 /v1/models 失败且不是鉴权失败时才回退到 /models。
        let candidates = if base.ends_with("/v1") || base.ends_with("/vbeta") {
            vec![format!("{base}/models")]
        } else {
            vec![format!("{base}/v1/models"), format!("{base}/models")]
        };

        let mut outcome: Result<Vec<String>, String> = Err("未发起请求".to_string());
        let key_attempts = api_keys.len().max(1);
        for models_url in candidates {
            let mut merged_models = Vec::new();
            let mut seen_models = HashSet::new();
            let mut candidate_error = "上游未返回可用模型".to_string();
            let mut candidate_auth_failed = false;

            // 同一端点必须遍历全部 Key：不同账号/Key 可能拥有不同模型权限。
            for key_index in 0..key_attempts {
                let api_key = api_keys
                    .get(key_index)
                    .map(String::as_str)
                    .unwrap_or_default();
                let mut req = apply_models_probe_identity(client.get(&models_url), ch, base);
                if !api_key.is_empty() {
                    req = req.header("Authorization", format!("Bearer {api_key}"));
                }

                match req.send().await {
                    Ok(resp) => {
                        let status = resp.status();
                        if !status.is_success() {
                            let body = resp.bytes().await.unwrap_or_default();
                            let summary = model_fetch_body_summary(&body);
                            warn!(
                                "[ModelGateway] 模型列表拉取候选失败：channel_id={} channel={} url={} key_attempt={}/{} status={} response={}",
                                ch.id,
                                ch.name,
                                models_url,
                                key_index + 1,
                                key_attempts,
                                status,
                                summary
                            );
                            candidate_error =
                                format!("上游返回 HTTP 状态码: {status}; response: {summary}");
                            if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
                                candidate_auth_failed = true;
                            }
                            continue;
                        }

                        let bytes = resp.bytes().await.unwrap_or_default();
                        match parse_models_payload(&bytes) {
                            Some(list) if !list.is_empty() => {
                                for model in list {
                                    if seen_models.insert(model.clone()) {
                                        merged_models.push(model);
                                    }
                                }
                            }
                            _ => {
                                // HTML fallback 与 Key 无关，不必对同一端点重复请求剩余 Key；
                                // 其他非标准响应仍继续遍历，尽量收集其余 Key 的模型。
                                let snippet =
                                    String::from_utf8_lossy(&bytes[..bytes.len().min(80)])
                                        .trim()
                                        .to_string();
                                let looks_html = snippet.to_lowercase().starts_with("<!doctype")
                                    || snippet.to_lowercase().starts_with("<html");
                                let summary = model_fetch_body_summary(&bytes);
                                warn!(
                                    "[ModelGateway] 模型列表响应解析失败：channel_id={} channel={} url={} key_attempt={}/{} status={} response={}",
                                    ch.id,
                                    ch.name,
                                    models_url,
                                    key_index + 1,
                                    key_attempts,
                                    status,
                                    summary
                                );
                                candidate_error = if looks_html {
                                    "上游返回 HTML 页面而非模型列表（upstreamUrl 可能缺少 /v1 路径）"
                                        .to_string()
                                } else {
                                    format!("上游响应中未解析到任何模型; response: {summary}")
                                };
                                if looks_html {
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "[ModelGateway] 模型列表连接失败：channel_id={} channel={} url={} key_attempt={}/{} error={}",
                            ch.id,
                            ch.name,
                            models_url,
                            key_index + 1,
                            key_attempts,
                            e
                        );
                        candidate_error = format!("连接上游失败: {e}");
                    }
                }
            }

            if !merged_models.is_empty() {
                outcome = Ok(merged_models);
                break;
            }
            outcome = Err(candidate_error);
            if candidate_auth_failed {
                break;
            }
        }

        match outcome {
            Ok(mut list) => {
                // 上游返回空列表时以白名单兜底（兼容不支持列出模型的渠道）
                if list.is_empty() {
                    if let Some(explicit) = &ch.enabled_models {
                        list.extend(explicit.clone());
                    }
                }
                // OpenCode 官方渠道模型列表过长（60+，绝大多数为非免费模型），仅保留免费模型
                if is_opencode_channel(ch) {
                    list.retain(|m| is_free_opencode_model(m));
                }
                channel_models.push(ChannelModelList {
                    channel_id: ch.id.clone(),
                    channel_name: ch.name.clone(),
                    alias: ch.effective_alias(),
                    models: list,
                });
            }
            Err(err_msg) => {
                error!(
                    "[ModelGateway] 模型列表拉取失败：channel_id={} channel={} base_url={} error={}",
                    ch.id,
                    ch.name,
                    base,
                    err_msg
                );
                fetch_errors.push(ChannelModelFetchError {
                    channel_id: ch.id.clone(),
                    channel_name: ch.name.clone(),
                    alias: ch.effective_alias(),
                    error: err_msg,
                });
            }
        }
    }

    if channel_id.is_some() {
        let selected_id = channel_id.unwrap_or_default();
        let mut cached_models = ctx.cached_channel_models.write().await;
        cached_models.retain(|item| item.channel_id != selected_id);
        cached_models.extend(channel_models.iter().cloned());
        drop(cached_models);

        let mut cached_errors = ctx.cached_fetch_errors.write().await;
        cached_errors.retain(|item| item.channel_id != selected_id);
        cached_errors.extend(fetch_errors.iter().cloned());
    } else {
        *ctx.cached_channel_models.write().await = channel_models.clone();
        *ctx.cached_fetch_errors.write().await = fetch_errors.clone();
    }

    (channel_models, fetch_errors)
}

/// 生成模型探测响应的短摘要，便于后台定位鉴权/路径/格式问题，避免日志过长。
fn model_fetch_body_summary(bytes: &[u8]) -> String {
    let summary = String::from_utf8_lossy(bytes)
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>();
    let summary = summary.trim();
    let max_chars = 240;
    if summary.chars().count() > max_chars {
        format!("{}…", summary.chars().take(max_chars).collect::<String>())
    } else if summary.is_empty() {
        "<empty>".to_string()
    } else {
        summary.to_string()
    }
}

/// 解析模型列表响应体：兼容 OpenAI（data[].id）与 Gemini（models[].name）两种格式。
/// 返回 None 表示 body 不是合法 JSON 或结构完全不匹配（如 HTML 页面）。
fn parse_models_payload(bytes: &[u8]) -> Option<Vec<String>> {
    let jv = serde_json::from_slice::<JsonValue>(bytes).ok()?;
    let items = jv
        .get("data")
        .and_then(JsonValue::as_array)
        .or_else(|| jv.get("models").and_then(JsonValue::as_array))?;
    let mut list = Vec::new();
    for item in items {
        // OpenAI 条目用 id，Gemini 条目用 name（可带 models/ 前缀）
        let key = item
            .get("id")
            .and_then(JsonValue::as_str)
            .or_else(|| item.get("name").and_then(JsonValue::as_str));
        if let Some(id) = key {
            list.push(id.trim_start_matches("models/").to_string());
        }
    }
    Some(list)
}
