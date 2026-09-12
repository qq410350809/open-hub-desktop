use super::config::load_model_proxy_config;
use super::router::fetch_upstream_models_inner;
use super::types::{
    ModelProxyConfig, ModelProxyContext, ModelProxyState, OpencodeProxyState, ProxyMetrics,
};
use crate::context::AppContext;
use reqwest::Client;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

impl ModelProxyState {
    #[allow(dead_code)]
    pub fn new_with_app(app: Option<std::sync::Arc<crate::context::AppContext>>) -> Self {
        let state = Self::new();
        if let Some(ctx) = app {
            crate::context::block_on(state.attach_ctx(ctx));
        }
        state
    }

    pub fn new() -> Self {
        let http_client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .unwrap_or_default();
        let stream_http_client = build_stream_http_client();

        let route_enabled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let context = ModelProxyContext {
            route_enabled: route_enabled.clone(),
            config: Arc::new(RwLock::new(ModelProxyConfig::default())),
            metrics: Arc::new(ProxyMetrics::default()),
            started_at: Arc::new(RwLock::new(None)),
            current_port: Arc::new(RwLock::new(0)),
            cached_channel_models: Arc::new(RwLock::new(Vec::new())),
            cached_fetch_errors: Arc::new(RwLock::new(Vec::new())),
            default_http_client: Arc::new(tokio::sync::RwLock::new(http_client)),
            default_stream_client: Arc::new(tokio::sync::RwLock::new(stream_http_client)),
            app_ctx: Arc::new(RwLock::new(None)),
            key_round_robin: Arc::new(RwLock::new(HashMap::new())),
            node_round_robin: Arc::new(RwLock::new(HashMap::new())),
            log_retention_last_run: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            rate_limit_cooldowns: Arc::new(RwLock::new(HashMap::new())),
        };

        Self { context }
    }

    /// 注入应用上下文（幂等）。
    pub async fn attach_ctx(&self, ctx: Arc<AppContext>) {
        *self.context.app_ctx.write().await = Some(ctx);
    }
}

pub async fn start_model_proxy_server(state: &ModelProxyState) -> Result<(), String> {
    if let Some(ctx) = state.context.app_ctx.read().await.as_ref() {
        let conn = ctx.database.0.lock().ok();
        if let Some(cfg) = conn.map(|c| load_model_proxy_config(&c)) {
            *state.context.config.write().await = cfg;
        }
    }

    // 配置可能带自定义 timeout_seconds：重建默认出网客户端使其生效
    refresh_default_http_client(&state.context).await;

    state.context.route_enabled.store(true, Ordering::Release);
    *state.context.started_at.write().await = Some(Instant::now());

    let ctx_for_models = state.context.clone();
    crate::context::spawn(async move {
        fetch_upstream_models_inner(&ctx_for_models, None).await;
    });

    Ok(())
}

/// 按当前配置重建默认出网客户端（timeout_seconds 生效）。
/// 配置保存/站点同步后调用，保证直连通道运行期超时配置即时生效。
pub async fn refresh_default_http_client(ctx: &ModelProxyContext) {
    let timeout = {
        let cfg = ctx.config.read().await;
        crate::model::gateway::balancer::egress_timeout(&cfg)
    };
    if let Ok(client) = Client::builder().timeout(timeout).build() {
        *ctx.default_http_client.write().await = client;
    }
    if let Ok(client) = build_stream_http_client_checked() {
        *ctx.default_stream_client.write().await = client;
    }
}

/// 流式出网客户端构造参数：连接超时见 [`crate::model::gateway::balancer::STREAM_CONNECT_TIMEOUT`]；
/// **不设总超时**——reqwest 的 `timeout()` 覆盖整个响应体读取，长流任务（agent
/// 多轮工具循环、深度思考模型）会被无差别掐断。流式存活判定由转发循环的
/// 空闲超时负责（见 `stream.rs`，窗口取 `timeout_seconds` 配置）。
fn build_stream_http_client_checked() -> Result<Client, reqwest::Error> {
    Client::builder()
        .connect_timeout(crate::model::gateway::balancer::STREAM_CONNECT_TIMEOUT)
        .build()
}

fn build_stream_http_client() -> Client {
    build_stream_http_client_checked().unwrap_or_default()
}

#[allow(dead_code)]
pub async fn start_opencode_proxy_server(state: &OpencodeProxyState) -> Result<(), String> {
    start_model_proxy_server(state).await
}

pub async fn stop_model_proxy_server(state: &ModelProxyState) -> Result<(), String> {
    state.context.route_enabled.store(false, Ordering::Release);
    *state.context.started_at.write().await = None;
    Ok(())
}

#[allow(dead_code)]
pub async fn stop_opencode_proxy_server(state: &OpencodeProxyState) -> Result<(), String> {
    stop_model_proxy_server(state).await
}
