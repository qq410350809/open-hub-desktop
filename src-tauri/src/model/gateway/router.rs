use super::balancer::resolve_channel_api_keys;
use super::handlers::{
    handle_chat_completions, handle_gemini_generate, handle_messages, handle_responses,
};
use super::types::{
    effective_aux_url, ChannelConfig, ChannelModelFetchError, ChannelModelList, ModelProxyConfig,
    ModelProxyContext, ModelProxySyncProgress, ModelProxySyncStage, GATEWAY_USER_AGENT,
    MODELS_FETCH_CHANNEL_TIMEOUT, MODELS_FETCH_TOTAL_TIMEOUT,
};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value as JsonValue};
use std::collections::HashSet;
use std::time::Duration;
use tokio::time::{timeout_at, Instant};
use tower_http::cors::{Any, CorsLayer};
use tracing::warn;

#[cfg(test)]
#[path = "tests/model_sync.rs"]
mod model_sync_tests;

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

/// Fetch selected enabled channels. Omitted channel/request IDs retain startup/bulk behavior.
pub async fn fetch_upstream_models_inner(
    ctx: &ModelProxyContext,
    channel_id: Option<&str>,
    request_id: Option<&str>,
) -> (Vec<ChannelModelList>, Vec<ChannelModelFetchError>) {
    fetch_upstream_models_with_timeout(ctx, channel_id, request_id, MODELS_FETCH_CHANNEL_TIMEOUT).await
}

/// A single deadline covers key resolution and every endpoint/key/main/aux attempt.
/// The smaller budget parameter also permits real HTTP timeout regression tests.
pub(super) async fn fetch_upstream_models_with_timeout(
    ctx: &ModelProxyContext,
    channel_id: Option<&str>,
    request_id: Option<&str>,
    channel_timeout: Duration,
) -> (Vec<ChannelModelList>, Vec<ChannelModelFetchError>) {
    let config = ctx.config.read().await.clone();
    let bus = ctx.app_ctx.read().await.as_ref().map(|app| app.event_bus.clone());
    let mut channel_models = Vec::new();
    let mut fetch_errors = Vec::new();

    for ch in &config.channels {
        if !ch.enabled || channel_id.is_some_and(|id| id != ch.id) {
            continue;
        }
        let deadline = Instant::now() + channel_timeout;
        let progress = SyncProgress {
            bus: bus.as_ref(),
            request_id,
            channel_id: &ch.id,
        };
        progress.emit(ModelProxySyncStage::Resolving, "正在解析渠道 API Key", None, None);
        // Site-key resolution takes a synchronous SQLite mutex. Keep that work off
        // the async executor so contention cannot prevent the deadline from firing.
        let key_ctx = ctx.clone();
        let key_channel = ch.clone();
        let runtime = tokio::runtime::Handle::current();
        let keys_task = tokio::task::spawn_blocking(move || {
            runtime.block_on(resolve_channel_api_keys(&key_ctx, &key_channel))
        });
        let outcome = match timeout_at(deadline, keys_task).await {
            Ok(Ok(api_keys)) => fetch_channel_models(ctx, ch, &api_keys, deadline, &progress).await,
            Ok(Err(_)) => Err("解析渠道 API Key 失败".to_string()),
            Err(_) => Err("渠道模型同步超时（解析 API Key）".to_string()),
        };

        match outcome {
            Ok(list) => {
                channel_models.push(ChannelModelList {
                    channel_id: ch.id.clone(),
                    channel_name: ch.name.clone(),
                    alias: ch.effective_alias(),
                    models: list,
                });
                progress.emit(ModelProxySyncStage::Saving, "正在保存已获取的模型列表", None, None);
            }
            Err(error) => {
                warn!("[ModelGateway] 模型列表拉取失败：channel_id={} error={}", ch.id, error);
                fetch_errors.push(ChannelModelFetchError {
                    channel_id: ch.id.clone(),
                    channel_name: ch.name.clone(),
                    alias: ch.effective_alias(),
                    error,
                });
            }
        }
    }

    // Replace only successful channels. Failed refreshes must keep the last known models,
    // including a bulk refresh where just some of the enabled channels failed.
    {
        let mut cached = ctx.cached_channel_models.write().await;
        cached.retain(|entry| {
            (channel_id.is_some() || config.channels.iter().any(|ch| ch.id == entry.channel_id))
                && !channel_models.iter().any(|fresh| fresh.channel_id == entry.channel_id)
        });
        cached.extend(channel_models.iter().cloned());
    }
    {
        let mut cached = ctx.cached_fetch_errors.write().await;
        if let Some(selected_id) = channel_id {
            cached.retain(|entry| entry.channel_id != selected_id);
            cached.extend(fetch_errors.iter().cloned());
        } else {
            *cached = fetch_errors.clone();
        }
    }

    if !channel_models.is_empty() {
        if let Err(e) = save_channel_models_to_db(ctx, &channel_models).await {
            warn!("[ModelGateway] 保存渠道模型到数据库失败: {}", e);
        }
    }
    (channel_models, fetch_errors)
}

struct SyncProgress<'a> {
    bus: Option<&'a crate::context::EventBus>,
    request_id: Option<&'a str>,
    channel_id: &'a str,
}

impl SyncProgress<'_> {
    fn emit(
        &self,
        stage: ModelProxySyncStage,
        message: &str,
        endpoint: Option<&str>,
        key: Option<(usize, usize)>,
    ) {
        if let Some(bus) = self.bus {
            // Configured URLs may include userinfo/query credentials. reqwest errors and
            // upstream bodies are deliberately never interpolated into public messages.
            let endpoint = endpoint.and_then(safe_model_fetch_endpoint);
            bus.emit("model-proxy-sync-progress", ModelProxySyncProgress {
                request_id: self.request_id,
                channel_id: self.channel_id,
                stage,
                message,
                endpoint: endpoint.as_deref(),
                key_index: key.map(|(index, _)| index),
                key_total: key.map(|(_, total)| total),
            });
        }
    }
}

fn safe_model_fetch_endpoint(raw: &str) -> Option<String> {
    let mut url = url::Url::parse(raw).ok()?;
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

async fn fetch_channel_models(
    ctx: &ModelProxyContext,
    ch: &ChannelConfig,
    api_keys: &[String],
    deadline: Instant,
    progress: &SyncProgress<'_>,
) -> Result<Vec<String>, String> {
    let base = ch.base_url.trim().trim_end_matches('/');
    let aux = effective_aux_url(ch)
        .map(|aux| aux.trim_end_matches('/').to_string())
        .filter(|aux| aux != base);
    // Reserve half the remaining channel budget for aux even if main keeps responding
    // slowly for many keys. A transport failure ends that base immediately.
    let main_deadline = if aux.is_some() {
        let now = Instant::now();
        now + deadline.saturating_duration_since(now) / 2
    } else {
        deadline
    };
    let main = fetch_models_from_base(ctx, api_keys, base, main_deadline, progress).await;
    if main.is_ok() {
        return main;
    }
    if let Some(aux) = aux {
        progress.emit(ModelProxySyncStage::Fallback, "主地址失败，正在尝试辅助地址", Some(&aux), None);
        return fetch_models_from_base(ctx, api_keys, &aux, deadline, progress)
            .await
            .map_err(|aux_error| format!("主地址：{}；辅助地址：{}", main.unwrap_err(), aux_error));
    }
    main
}

/// Probe endpoint candidates and union every successful key. Deadline/transport failures
/// return any models already collected rather than discarding partial success.
async fn fetch_models_from_base(
    ctx: &ModelProxyContext,
    api_keys: &[String],
    base: &str,
    deadline: Instant,
    progress: &SyncProgress<'_>,
) -> Result<Vec<String>, String> {
    let candidates = if base.ends_with("/v1") || base.ends_with("/vbeta") {
        vec![format!("{base}/models")]
    } else {
        vec![format!("{base}/v1/models"), format!("{base}/models")]
    };
    let key_total = api_keys.len().max(1);
    let mut models = Vec::new();
    let mut seen = HashSet::new();
    let mut last_error = "上游未返回可用模型".to_string();
    for (endpoint_index, models_url) in candidates.iter().enumerate() {
        if endpoint_index > 0 {
            progress.emit(ModelProxySyncStage::Fallback, "正在尝试备用模型端点", Some(models_url), None);
        }
        let mut auth_failed = false;
        for key_index in 0..key_total {
            if Instant::now() >= deadline {
                return partial_models(models, "渠道模型同步超时".to_string());
            }
            let key = Some((key_index + 1, key_total));
            progress.emit(ModelProxySyncStage::Request, "正在请求上游模型列表", Some(models_url), key);
            let mut req = ctx.models_fetch_client
                .get(models_url)
                .header("User-Agent", GATEWAY_USER_AGENT);
            if let Some(api_key) = api_keys.get(key_index).filter(|key| !key.is_empty()) {
                req = req.header("Authorization", format!("Bearer {api_key}"));
            }
            let request_deadline = deadline.min(Instant::now() + MODELS_FETCH_TOTAL_TIMEOUT);
            let response = timeout_at(request_deadline, async {
                let resp = req.send().await?;
                let status = resp.status();
                progress.emit(
                    ModelProxySyncStage::Response,
                    &format!("已收到 HTTP {}，正在读取并解析模型列表", status.as_u16()),
                    Some(models_url),
                    key,
                );
                // Error bodies can echo credentials and are not needed for status diagnostics.
                let bytes = if status.is_success() { resp.bytes().await? } else { bytes::Bytes::new() };
                Ok::<_, reqwest::Error>((status, bytes))
            }).await;
            let (status, bytes) = match response {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => {
                    let message = if error.is_timeout() { "上游模型请求超时" } else { "连接上游失败" };
                    progress.emit(ModelProxySyncStage::Response, message, Some(models_url), key);
                    return partial_models(models, message.to_string());
                }
                Err(_) => {
                    progress.emit(ModelProxySyncStage::Response, "上游模型请求超时", Some(models_url), key);
                    return partial_models(models, "上游模型请求超时".to_string());
                }
            };
            if !status.is_success() {
                last_error = format!("上游返回 HTTP 状态码: {}", status.as_u16());
                auth_failed |= matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN);
                continue;
            }
            match parse_models_payload(&bytes) {
                Some(list) if !list.is_empty() => {
                    for model in list {
                        if seen.insert(model.clone()) {
                            models.push(model);
                        }
                    }
                    progress.emit(ModelProxySyncStage::Response, "模型列表解析成功", Some(models_url), key);
                }
                _ => {
                    let prefix = String::from_utf8_lossy(&bytes[..bytes.len().min(80)]).trim().to_lowercase();
                    let html = prefix.starts_with("<!doctype") || prefix.starts_with("<html");
                    last_error = if html {
                        "上游返回 HTML 页面而非模型列表（请检查模型接口路径）".to_string()
                    } else {
                        "上游响应中未解析到任何模型".to_string()
                    };
                    progress.emit(ModelProxySyncStage::Response, &last_error, Some(models_url), key);
                    if html { break; }
                }
            }
        }
        if !models.is_empty() { return Ok(models); }
        if auth_failed { break; }
    }
    partial_models(models, last_error)
}

fn partial_models(models: Vec<String>, error: String) -> Result<Vec<String>, String> {
    if models.is_empty() { Err(error) } else { Ok(models) }
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

/// 保存渠道模型列表到数据库，用于持久化缓存
async fn save_channel_models_to_db(
    ctx: &ModelProxyContext,
    channel_models: &[ChannelModelList],
) -> Result<(), String> {
    if let Some(app_ctx) = ctx.app_ctx.read().await.as_ref() {
        let conn = app_ctx
            .database
            .0
            .lock()
            .map_err(|e| format!("获取数据库锁失败: {}", e))?;
        for item in channel_models {
            let models_json = serde_json::to_string(&item.models).map_err(|e| e.to_string())?;
            conn.execute(
                "INSERT OR REPLACE INTO channel_model_cache (channel_id, channel_name, alias, models_json, updated_at) VALUES (?1, ?2, ?3, ?4, datetime('now'))",
                rusqlite::params![&item.channel_id, &item.channel_name, &item.alias, models_json],
            ).map_err(|e| format!("数据库写入失败: {}", e))?;
        }
        Ok(())
    } else {
        Err("应用上下文未初始化".to_string())
    }
}
