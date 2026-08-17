use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tower_http::cors::{Any, CorsLayer};

pub const DEFAULT_GATEWAY_PORT: u16 = 17896;
const FAILOVER_COOLDOWN: Duration = Duration::from_secs(30);

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayApiKeyItem {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub key: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayConfig {
    pub enabled: bool,
    pub port: u16,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub api_keys: Vec<GatewayApiKeyItem>,
    #[serde(default)]
    pub model_agg_group_modes: HashMap<String, String>,
    #[serde(default)]
    pub model_agg_hidden_nodes: Vec<String>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            port: DEFAULT_GATEWAY_PORT,
            api_key: String::new(),
            api_keys: Vec::new(),
            model_agg_group_modes: HashMap::new(),
            model_agg_hidden_nodes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatus {
    pub running: bool,
    pub port: u16,
    pub url: String,
    pub active_keys_count: usize,
    pub active_models_count: usize,
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CandidateKey {
    pub site_id: String,
    pub site_name: String,
    pub api_base_url: String,
    pub system_type: String,
    pub group: String,
    pub key: String,
    pub models: HashSet<String>,
    pub last_failed_at: Option<Instant>,
    pub fail_count: u32,
}

impl CandidateKey {
    pub fn supports_model(&self, model: &str) -> bool {
        if self.models.is_empty() {
            return true;
        }
        let requested = model.trim().to_ascii_lowercase();
        if self.models.iter().any(|m| m.to_ascii_lowercase() == requested) {
            return true;
        }
        let stripped = requested.split('/').last().unwrap_or(&requested);
        self.models.iter().any(|m| {
            let m_lower = m.to_ascii_lowercase();
            m_lower == stripped || m_lower.split('/').last().unwrap_or(&m_lower) == stripped
        })
    }

    pub fn is_cooling_down(&self) -> bool {
        if let Some(failed_at) = self.last_failed_at {
            failed_at.elapsed() < FAILOVER_COOLDOWN
        } else {
            false
        }
    }
}

#[derive(Default)]
pub struct GatewayMetrics {
    pub total_requests: AtomicU64,
    pub successful_requests: AtomicU64,
    pub failed_requests: AtomicU64,
}

#[derive(Clone)]
pub struct GatewayContext {
    pub config: Arc<RwLock<GatewayConfig>>,
    pub candidates: Arc<RwLock<Vec<CandidateKey>>>,
    pub metrics: Arc<GatewayMetrics>,
    pub rr_indices: Arc<RwLock<HashMap<String, AtomicUsize>>>,
    pub http_client: reqwest::Client,
}

pub struct GatewayState {
    pub context: GatewayContext,
    pub shutdown_tx: Arc<tokio::sync::Mutex<Option<tokio::sync::broadcast::Sender<()>>>>,
    pub current_port: Arc<RwLock<u16>>,
    pub is_running: Arc<RwLock<bool>>,
}

impl GatewayState {
    pub fn new() -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(180))
            .connect_timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let context = GatewayContext {
            config: Arc::new(RwLock::new(GatewayConfig::default())),
            candidates: Arc::new(RwLock::new(Vec::new())),
            metrics: Arc::new(GatewayMetrics::default()),
            rr_indices: Arc::new(RwLock::new(HashMap::new())),
            http_client,
        };

        Self {
            context,
            shutdown_tx: Arc::new(tokio::sync::Mutex::new(None)),
            current_port: Arc::new(RwLock::new(DEFAULT_GATEWAY_PORT)),
            is_running: Arc::new(RwLock::new(false)),
        }
    }
}

pub fn load_candidates_from_connection(
    conn: &Connection,
    config: &GatewayConfig,
) -> Vec<CandidateKey> {
    let mut stmt = match conn.prepare(
        "SELECT s.id, s.name, s.api_base_url, s.system_type, c.keys_json, c.groups_json, c.key_models_json, c.models_json
         FROM directory_sites s
         JOIN site_model_cache c ON s.id = c.site_id
         WHERE s.hidden = 0",
    ) {
        Ok(stmt) => stmt,
        Err(_) => return Vec::new(),
    };

    let rows = stmt.query_map([], |row| {
        let site_id: String = row.get(0)?;
        let site_name: String = row.get(1)?;
        let api_base_url: String = row.get(2)?;
        let system_type: String = row.get(3)?;
        let keys_json: String = row.get(4)?;
        let groups_json: String = row.get(5)?;
        let key_models_json: String = row.get(6)?;
        let models_json: String = row.get(7)?;
        Ok((
            site_id,
            site_name,
            api_base_url,
            system_type,
            keys_json,
            groups_json,
            key_models_json,
            models_json,
        ))
    });

    let Ok(rows) = rows else {
        return Vec::new();
    };

    let mut candidates = Vec::new();
    let hidden_set: HashSet<&str> = config
        .model_agg_hidden_nodes
        .iter()
        .map(String::as_str)
        .collect();

    for item in rows.flatten() {
        let (site_id, site_name, api_base_url, system_type, k_json, g_json, km_json, m_json) = item;
        let keys: Vec<String> = serde_json::from_str(&k_json).unwrap_or_default();
        let groups: HashMap<String, String> = serde_json::from_str(&g_json).unwrap_or_default();
        let key_models: HashMap<String, Vec<String>> =
            serde_json::from_str(&km_json).unwrap_or_default();
        let models: Vec<JsonValue> = serde_json::from_str(&m_json).unwrap_or_default();

        let general_models: HashSet<String> = models
            .iter()
            .filter_map(|m| {
                m.get("id")
                    .and_then(JsonValue::as_str)
                    .map(str::to_string)
            })
            .collect();

        for key in keys {
            let key_trimmed = key.trim().to_string();
            if key_trimmed.is_empty() {
                continue;
            }
            let group = groups
                .get(&key_trimmed)
                .cloned()
                .unwrap_or_else(|| "默认分组".to_string());

            let mut assigned_models: HashSet<String> = if let Some(km) = key_models.get(&key_trimmed) {
                km.iter().cloned().collect()
            } else {
                general_models.clone()
            };

            // 过滤掉被隐藏的模型
            assigned_models.retain(|m| !hidden_set.contains(m.as_str()));

            candidates.push(CandidateKey {
                site_id: site_id.clone(),
                site_name: site_name.clone(),
                api_base_url: api_base_url.clone(),
                system_type: system_type.clone(),
                group,
                key: key_trimmed,
                models: assigned_models,
                last_failed_at: None,
                fail_count: 0,
            });
        }
    }

    candidates
}

fn build_upstream_url(base: &str, endpoint: &str) -> String {
    let trimmed = base.trim().trim_end_matches('/');
    let ep = endpoint.trim().trim_start_matches('/');
    if trimmed.ends_with("/v1") && ep.starts_with("v1/") {
        format!("{}/{}", trimmed, &ep[3..])
    } else if trimmed.ends_with("/v1") {
        format!("{}/{}", trimmed, ep)
    } else if ep.starts_with("v1/") {
        format!("{}/{}", trimmed, ep)
    } else {
        format!("{}/v1/{}", trimmed, ep)
    }
}

fn check_auth(headers: &HeaderMap, config: &GatewayConfig) -> Result<(), Response> {
    let mut valid_keys: HashSet<&str> = HashSet::new();
    let legacy_key = config.api_key.trim();
    if !legacy_key.is_empty() {
        valid_keys.insert(legacy_key);
    }
    for item in &config.api_keys {
        let k = item.key.trim();
        if item.enabled && !k.is_empty() {
            valid_keys.insert(k);
        }
    }

    // 若未配置任何 API Key，则免校验
    if valid_keys.is_empty() {
        return Ok(());
    }

    let auth_header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let x_api_key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let bearer_token = auth_header
        .strip_prefix("Bearer ")
        .or_else(|| auth_header.strip_prefix("bearer "))
        .unwrap_or(auth_header)
        .trim();

    if (!bearer_token.is_empty() && valid_keys.contains(bearer_token))
        || (!x_api_key.is_empty() && valid_keys.contains(x_api_key))
    {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": {
                    "message": "Invalid OpenHub Gateway API Key",
                    "type": "invalid_request_error",
                    "code": "invalid_api_key"
                }
            })),
        )
            .into_response())
    }
}

async fn health_handler(State(ctx): State<GatewayContext>) -> Response {
    let count = ctx.candidates.read().await.len();
    Json(json!({
        "status": "ok",
        "service": "OpenHub Gateway",
        "version": "0.3.0",
        "activeKeys": count
    }))
    .into_response()
}

async fn list_models_handler(
    State(ctx): State<GatewayContext>,
    headers: HeaderMap,
) -> Response {
    let config = ctx.config.read().await.clone();
    if let Err(res) = check_auth(&headers, &config) {
        return res;
    }

    let candidates = ctx.candidates.read().await;
    let mut unique_models: HashSet<String> = HashSet::new();
    for candidate in candidates.iter() {
        for model in &candidate.models {
            unique_models.insert(model.clone());
        }
    }

    let mut model_list: Vec<String> = unique_models.into_iter().collect();
    model_list.sort();

    let data: Vec<JsonValue> = model_list
        .into_iter()
        .map(|id| {
            json!({
                "id": id,
                "object": "model",
                "created": 1700000000,
                "owned_by": "openhub-gateway"
            })
        })
        .collect();

    Json(json!({
        "object": "list",
        "data": data
    }))
    .into_response()
}

async fn forward_request(
    ctx: GatewayContext,
    headers: HeaderMap,
    body_bytes: axum::body::Bytes,
    endpoint: &str,
) -> Response {
    let config = ctx.config.read().await.clone();
    if let Err(res) = check_auth(&headers, &config) {
        return res;
    }

    ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);

    let body_json: Option<JsonValue> = serde_json::from_slice(&body_bytes).ok();
    let requested_model = body_json
        .as_ref()
        .and_then(|v| v.get("model"))
        .and_then(JsonValue::as_str)
        .unwrap_or("")
        .to_string();

    let is_streaming = body_json
        .as_ref()
        .and_then(|v| v.get("stream"))
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);

    // 1. 构建通道池（聚合模式下同名分组作为单个通道并在内部轮询，独立模式下每个 Key 作为独立通道）
    let (all_channels, flat_candidates) = {
        let candidates = ctx.candidates.read().await;
        let mut agg_map: HashMap<String, Vec<(usize, CandidateKey)>> = HashMap::new();
        let mut ind_channels: Vec<Vec<(usize, CandidateKey)>> = Vec::new();

        for (idx, c) in candidates.iter().enumerate() {
            let mode = config
                .model_agg_group_modes
                .get(&c.group)
                .map(String::as_str)
                .unwrap_or("independent");
            if mode == "aggregate" {
                agg_map
                    .entry(c.group.clone())
                    .or_default()
                    .push((idx, c.clone()));
            } else {
                ind_channels.push(vec![(idx, c.clone())]);
            }
        }

        let mut channels: Vec<Vec<(usize, CandidateKey)>> = agg_map.into_values().collect();
        channels.extend(ind_channels);
        let flat: Vec<(usize, CandidateKey)> = candidates
            .iter()
            .enumerate()
            .map(|(i, c)| (i, c.clone()))
            .collect();
        (channels, flat)
    };

    // 过滤出支持请求模型的通道
    let matched_channels: Vec<Vec<(usize, CandidateKey)>> = all_channels
        .into_iter()
        .map(|ch_keys| {
            let filtered: Vec<(usize, CandidateKey)> = ch_keys
                .iter()
                .filter(|(_, c)| requested_model.is_empty() || c.supports_model(&requested_model))
                .cloned()
                .collect();
            filtered
        })
        .filter(|keys| !keys.is_empty())
        .collect();

    // 扁平化候选列表（按通道轮询排序）
    let model_key = if requested_model.is_empty() {
        "__default__".to_string()
    } else {
        requested_model.clone()
    };

    let mut matched_candidates: Vec<(usize, CandidateKey)> = Vec::new();
    if !matched_channels.is_empty() {
        let start_ch_idx = {
            let mut rr_map = ctx.rr_indices.write().await;
            let counter = rr_map
                .entry(format!("ch:{}", model_key))
                .or_insert_with(|| AtomicUsize::new(0));
            counter.fetch_add(1, Ordering::Relaxed) % matched_channels.len()
        };

        for ch_offset in 0..matched_channels.len() {
            let ch_idx = (start_ch_idx + ch_offset) % matched_channels.len();
            let keys = &matched_channels[ch_idx];
            if keys.is_empty() {
                continue;
            }
            if keys.len() == 1 {
                matched_candidates.push(keys[0].clone());
            } else {
                // 聚合通道内部使用组级轮询计数器
                let group_name = &keys[0].1.group;
                let group_start_idx = {
                    let mut rr_map = ctx.rr_indices.write().await;
                    let counter = rr_map
                        .entry(format!("grp:{}", group_name))
                        .or_insert_with(|| AtomicUsize::new(0));
                    counter.fetch_add(1, Ordering::Relaxed) % keys.len()
                };
                for k_offset in 0..keys.len() {
                    let k_idx = (group_start_idx + k_offset) % keys.len();
                    matched_candidates.push(keys[k_idx].clone());
                }
            }
        }
    } else {
        matched_candidates = flat_candidates;
    }

    if matched_candidates.is_empty() {
        ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "error": {
                    "message": "No active candidate keys available in OpenHub Gateway pool",
                    "type": "gateway_error",
                    "code": "no_candidates"
                }
            })),
        )
            .into_response();
    }

    let mut last_error_response: Option<Response> = None;
    let attempts = matched_candidates.len().min(4);

    for attempt in 0..attempts {
        let (cand_global_idx, candidate) = &matched_candidates[attempt];

        // 若处于冷却期且还有其他备选，则跳过
        if candidate.is_cooling_down() && attempts > 1 && attempt + 1 < attempts {
            continue;
        }

        let upstream_url = build_upstream_url(&candidate.api_base_url, endpoint);
        let mut upstream_req = ctx.http_client.post(&upstream_url);

        // 透传 headers
        for (name, val) in headers.iter() {
            let name_str = name.as_str();
            if name_str == "host"
                || name_str == "authorization"
                || name_str == "x-api-key"
                || name_str == "content-length"
            {
                continue;
            }
            upstream_req = upstream_req.header(name_str, val.as_bytes());
        }

        upstream_req = upstream_req
            .header("Authorization", format!("Bearer {}", candidate.key))
            .header("Content-Type", "application/json")
            .body(body_bytes.clone());

        match upstream_req.send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    ctx.metrics
                        .successful_requests
                        .fetch_add(1, Ordering::Relaxed);

                    // 响应成功，重置失败状态
                    {
                        let mut candidates = ctx.candidates.write().await;
                        if let Some(c) = candidates.get_mut(*cand_global_idx) {
                            c.fail_count = 0;
                            c.last_failed_at = None;
                        }
                    }

                    if is_streaming {
                        let mut res_builder = Response::builder().status(status.as_u16());
                        for (k, v) in resp.headers() {
                            res_builder = res_builder.header(k.as_str(), v.as_bytes());
                        }
                        let stream = resp.bytes_stream();
                        let body = Body::from_stream(stream);
                        return res_builder.body(body).unwrap_or_else(|_| {
                            (StatusCode::INTERNAL_SERVER_ERROR, "Stream building error")
                                .into_response()
                        });
                    } else {
                        let mut res_builder = Response::builder().status(status.as_u16());
                        for (k, v) in resp.headers() {
                            res_builder = res_builder.header(k.as_str(), v.as_bytes());
                        }
                        let bytes = resp.bytes().await.unwrap_or_default();
                        return res_builder.body(Body::from(bytes)).unwrap_or_else(|_| {
                            (StatusCode::INTERNAL_SERVER_ERROR, "Response building error")
                                .into_response()
                        });
                    }
                } else {
                    // 上游返回 429/401/5xx 错误，记录失败并故障转移
                    {
                        let mut candidates = ctx.candidates.write().await;
                        if let Some(c) = candidates.get_mut(*cand_global_idx) {
                            c.fail_count += 1;
                            c.last_failed_at = Some(Instant::now());
                        }
                    }

                    let status_code = status.as_u16();
                    let err_bytes = resp.bytes().await.unwrap_or_default();
                    let mut res_builder = Response::builder().status(status_code);
                    res_builder = res_builder.header("Content-Type", "application/json");
                    last_error_response = Some(
                        res_builder
                            .body(Body::from(err_bytes))
                            .unwrap_or_else(|_| (StatusCode::BAD_GATEWAY, "Error").into_response()),
                    );

                    // 401/403/429/5xx 触发 failover，继续尝试下一个 Key
                    if status.is_server_error()
                        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
                        || status == reqwest::StatusCode::UNAUTHORIZED
                        || status == reqwest::StatusCode::FORBIDDEN
                    {
                        continue;
                    } else {
                        break;
                    }
                }
            }
            Err(_err) => {
                // 网络连接错误 / 超时
                {
                    let mut candidates = ctx.candidates.write().await;
                    if let Some(c) = candidates.get_mut(*cand_global_idx) {
                        c.fail_count += 1;
                        c.last_failed_at = Some(Instant::now());
                    }
                }
                last_error_response = Some(
                    (
                        StatusCode::BAD_GATEWAY,
                        Json(json!({
                            "error": {
                                "message": format!("Upstream request to {} failed", candidate.site_name),
                                "type": "gateway_network_error"
                            }
                        })),
                    )
                        .into_response(),
                );
                continue;
            }
        }
    }

    ctx.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
    last_error_response.unwrap_or_else(|| {
        (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "error": {
                    "message": "All candidate keys failed in OpenHub Gateway pool",
                    "type": "gateway_error"
                }
            })),
        )
            .into_response()
    })
}

async fn chat_completions_handler(
    State(ctx): State<GatewayContext>,
    headers: HeaderMap,
    body_bytes: axum::body::Bytes,
) -> Response {
    forward_request(ctx, headers, body_bytes, "chat/completions").await
}

async fn completions_handler(
    State(ctx): State<GatewayContext>,
    headers: HeaderMap,
    body_bytes: axum::body::Bytes,
) -> Response {
    forward_request(ctx, headers, body_bytes, "completions").await
}

async fn embeddings_handler(
    State(ctx): State<GatewayContext>,
    headers: HeaderMap,
    body_bytes: axum::body::Bytes,
) -> Response {
    forward_request(ctx, headers, body_bytes, "embeddings").await
}

async fn messages_handler(
    State(ctx): State<GatewayContext>,
    headers: HeaderMap,
    body_bytes: axum::body::Bytes,
) -> Response {
    forward_request(ctx, headers, body_bytes, "messages").await
}

pub fn create_gateway_router(ctx: GatewayContext) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::<GatewayContext>::new()
        .route("/health", get(health_handler))
        .route("/v1/models", get(list_models_handler))
        .route("/v1/chat/completions", post(chat_completions_handler))
        .route("/v1/completions", post(completions_handler))
        .route("/v1/embeddings", post(embeddings_handler))
        .route("/v1/messages", post(messages_handler))
        .layer(cors)
        .with_state(ctx)
}

pub async fn start_gateway_server(
    state: &GatewayState,
    port: u16,
) -> Result<String, String> {
    let mut is_running = state.is_running.write().await;
    if *is_running {
        let current = *state.current_port.read().await;
        if current == port {
            return Ok(format!("http://127.0.0.1:{current}/v1"));
        }
        // 端口变化，先停止旧实例
        stop_gateway_server(state).await;
    }

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("绑定本地端口 {port} 失败: {e}"))?;

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::broadcast::channel::<()>(1);
    *state.shutdown_tx.lock().await = Some(shutdown_tx);
    *state.current_port.write().await = port;
    *is_running = true;

    let router = create_gateway_router(state.context.clone());

    tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.recv().await;
            })
            .await
            .ok();
    });

    Ok(format!("http://127.0.0.1:{port}/v1"))
}

pub async fn stop_gateway_server(state: &GatewayState) {
    let mut tx_guard = state.shutdown_tx.lock().await;
    if let Some(tx) = tx_guard.take() {
        let _ = tx.send(());
    }
    *state.is_running.write().await = false;
}

pub async fn get_gateway_status_impl(state: &GatewayState) -> GatewayStatus {
    let running = *state.is_running.read().await;
    let port = *state.current_port.read().await;
    let candidates = state.context.candidates.read().await;
    let mut unique_models = HashSet::new();
    for c in candidates.iter() {
        for m in &c.models {
            unique_models.insert(m.clone());
        }
    }

    GatewayStatus {
        running,
        port,
        url: format!("http://127.0.0.1:{port}/v1"),
        active_keys_count: candidates.len(),
        active_models_count: unique_models.len(),
        total_requests: state.context.metrics.total_requests.load(Ordering::Relaxed),
        successful_requests: state
            .context
            .metrics
            .successful_requests
            .load(Ordering::Relaxed),
        failed_requests: state
            .context
            .metrics
            .failed_requests
            .load(Ordering::Relaxed),
    }
}

fn get_candidates_from_db(
    database: &crate::models::Database,
    config: &GatewayConfig,
) -> Result<Vec<CandidateKey>, String> {
    let conn = database.0.lock().map_err(|_| "数据库锁定失败")?;
    Ok(load_candidates_from_connection(&conn, config))
}

#[tauri::command]
pub async fn get_gateway_status(
    state: tauri::State<'_, GatewayState>,
) -> Result<GatewayStatus, String> {
    Ok(get_gateway_status_impl(&state).await)
}

#[tauri::command]
pub async fn start_gateway(
    database: tauri::State<'_, crate::models::Database>,
    state: tauri::State<'_, GatewayState>,
    port: Option<u16>,
) -> Result<GatewayStatus, String> {
    let p = port.unwrap_or_else(|| {
        tokio::task::block_in_place(|| {
            futures_util::FutureExt::now_or_never(async { *state.current_port.read().await })
                .unwrap_or(DEFAULT_GATEWAY_PORT)
        })
    });
    let config = state.context.config.read().await.clone();
    let candidates = get_candidates_from_db(&database, &config)?;
    *state.context.candidates.write().await = candidates;
    start_gateway_server(&state, p).await?;
    Ok(get_gateway_status_impl(&state).await)
}

#[tauri::command]
pub async fn stop_gateway(
    state: tauri::State<'_, GatewayState>,
) -> Result<GatewayStatus, String> {
    stop_gateway_server(&state).await;
    Ok(get_gateway_status_impl(&state).await)
}

#[tauri::command]
pub async fn update_gateway_config(
    database: tauri::State<'_, crate::models::Database>,
    state: tauri::State<'_, GatewayState>,
    config: GatewayConfig,
) -> Result<GatewayStatus, String> {
    *state.context.config.write().await = config.clone();
    let candidates = get_candidates_from_db(&database, &config)?;
    *state.context.candidates.write().await = candidates;
    if config.enabled {
        let _ = start_gateway_server(&state, config.port).await;
    } else {
        stop_gateway_server(&state).await;
    }
    Ok(get_gateway_status_impl(&state).await)
}

#[tauri::command]
pub async fn reload_gateway_candidates(
    database: tauri::State<'_, crate::models::Database>,
    state: tauri::State<'_, GatewayState>,
) -> Result<GatewayStatus, String> {
    let config = state.context.config.read().await.clone();
    let candidates = get_candidates_from_db(&database, &config)?;
    *state.context.candidates.write().await = candidates;
    Ok(get_gateway_status_impl(&state).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_supports_model_matching() {
        let mut models = HashSet::new();
        models.insert("gpt-4o".to_string());
        models.insert("claude-3-7-sonnet".to_string());
        models.insert("vendor/deepseek-v3".to_string());

        let candidate = CandidateKey {
            site_id: "s1".to_string(),
            site_name: "Site1".to_string(),
            api_base_url: "https://api.test.com/".to_string(),
            system_type: "new-api".to_string(),
            group: "default".to_string(),
            key: "sk-test".to_string(),
            models,
            last_failed_at: None,
            fail_count: 0,
        };

        // 精确匹配
        assert!(candidate.supports_model("gpt-4o"));
        assert!(candidate.supports_model("GPT-4O"));
        // 前缀兼容匹配
        assert!(candidate.supports_model("deepseek-v3"));
        assert!(candidate.supports_model("openai/gpt-4o"));
        // 不支持的模型
        assert!(!candidate.supports_model("gemini-2.5-pro"));
    }

    #[test]
    fn builds_upstream_urls_correctly() {
        assert_eq!(
            build_upstream_url("https://api.example.com/", "chat/completions"),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            build_upstream_url("https://api.example.com/v1", "chat/completions"),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            build_upstream_url("https://api.example.com/v1/", "v1/chat/completions"),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            build_upstream_url("https://api.example.com", "models"),
            "https://api.example.com/v1/models"
        );
    }
}


