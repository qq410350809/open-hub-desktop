use super::config::load_model_proxy_config;
use super::router::{create_model_proxy_router, fetch_upstream_models_inner};
use super::types::{
    DEFAULT_MODEL_PROXY_PORT, ModelProxyConfig, ModelProxyContext, ModelProxyState,
    OpencodeProxyState, ProxyMetrics,
};
use reqwest::Client;
use std::net::SocketAddr;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tokio::sync::{oneshot, RwLock};

impl ModelProxyState {
    pub fn new_with_app(app: Option<AppHandle>) -> Self {
        let http_client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .unwrap_or_default();

        let context = ModelProxyContext {
            config: Arc::new(RwLock::new(ModelProxyConfig::default())),
            metrics: Arc::new(ProxyMetrics::default()),
            started_at: Arc::new(RwLock::new(None)),
            cached_channel_models: Arc::new(RwLock::new(Vec::new())),
            cached_fetch_errors: Arc::new(RwLock::new(Vec::new())),
            default_http_client: http_client,
            app_handle: Arc::new(RwLock::new(app)),
            key_round_robin: Arc::new(AtomicUsize::new(0)),
        };

        Self {
            context,
            shutdown_sender: Arc::new(RwLock::new(None)),
            current_port: Arc::new(RwLock::new(DEFAULT_MODEL_PROXY_PORT)),
        }
    }
}

pub async fn start_model_proxy_server(state: &ModelProxyState) -> Result<(), String> {
    let mut sender_guard = state.shutdown_sender.write().await;
    if sender_guard.is_some() {
        return Ok(());
    }

    if let Some(app) = state.context.app_handle.read().await.as_ref() {
        let loaded = {
            let database = app.state::<crate::models::Database>();
            let conn = database.0.lock().ok();
            conn.map(|c| load_model_proxy_config(&c))
        };
        if let Some(cfg) = loaded {
            *state.context.config.write().await = cfg;
        }
    }

    let config = state.context.config.read().await.clone();
    let port = config.port;
    *state.current_port.write().await = port;

    let router = create_model_proxy_router(state.context.clone());
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("绑定端口 {port} 失败: {e}"))?;

    let (tx, rx) = oneshot::channel::<()>();
    *sender_guard = Some(tx);
    *state.context.started_at.write().await = Some(Instant::now());

    let ctx_clone = state.context.clone();
    tauri::async_runtime::spawn(async move {
        let server = axum::serve(listener, router).with_graceful_shutdown(async move {
            let _ = rx.await;
        });

        if let Err(e) = server.await {
            eprintln!("[OpenHub] 模型网关运行发生异常: {e}");
        }
        *ctx_clone.started_at.write().await = None;
    });

    let ctx_for_models = state.context.clone();
    tauri::async_runtime::spawn(async move {
        fetch_upstream_models_inner(&ctx_for_models).await;
    });

    Ok(())
}

#[allow(dead_code)]
pub async fn start_opencode_proxy_server(state: &OpencodeProxyState) -> Result<(), String> {
    start_model_proxy_server(state).await
}

pub async fn stop_model_proxy_server(state: &ModelProxyState) -> Result<(), String> {
    let mut sender_guard = state.shutdown_sender.write().await;
    if let Some(sender) = sender_guard.take() {
        let _ = sender.send(());
    }
    *state.context.started_at.write().await = None;
    Ok(())
}

#[allow(dead_code)]
pub async fn stop_opencode_proxy_server(state: &OpencodeProxyState) -> Result<(), String> {
    stop_model_proxy_server(state).await
}
