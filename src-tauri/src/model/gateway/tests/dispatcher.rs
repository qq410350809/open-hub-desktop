use super::*;
use axum::{routing::post, Json, Router};
use futures_util::StreamExt;
use std::sync::{Arc, Mutex};

struct Upstream {
    url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Upstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn serve(router: Router) -> Upstream {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Upstream { url, task }
}
async fn context() -> ModelProxyContext {
    let ctx = super::super::types::ModelProxyState::new().context;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    *ctx.default_http_client.write().await = client.clone();
    *ctx.default_stream_client.write().await = client;
    ctx
}
fn channel(url: &str) -> ChannelConfig {
    serde_json::from_value(
        serde_json::json!({"id":"test", "name":"test", "enabled":true, "upstreamUrl":url}),
    )
    .unwrap()
}
fn meta() -> EgressRequestMeta {
    EgressRequestMeta {
        req_id: "test-request".into(),
        path: "/v1/chat/completions".into(),
        channel_id: "test".into(),
        channel_stats_id: None,
        model: "test-model".into(),
        rule_model: "test-model".into(),
        stream: true,
        req_body_str: None,
        client_name: None,
        user_agent: None,
        session_id: None,
    }
}

#[tokio::test]
async fn successful_stream_body_can_outlive_the_header_deadline() {
    let upstream = serve(Router::new().fallback(post(|| async {
        let chunks =
            futures_util::stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(b"start"))])
                .chain(futures_util::stream::once(async {
                    tokio::time::sleep(Duration::from_millis(350)).await;
                    Ok::<_, std::io::Error>(Bytes::from_static(b"end"))
                }));
        axum::body::Body::from_stream(chunks)
    })))
    .await;
    let ctx = context().await;
    let response = send_attempt(
        &ctx,
        "__direct__",
        true,
        (
            &upstream.url,
            &serde_json::json!({}),
            "",
            TargetProtocol::OpenAiChat,
        ),
        Duration::from_millis(200),
    )
    .await
    .unwrap_or_else(|_| panic!("headers should arrive within the deadline"));
    assert_eq!(
        response.bytes().await.unwrap(),
        Bytes::from_static(b"startend")
    );
}

#[tokio::test]
async fn primary_http_error_does_not_replay_the_request_on_its_mirror() {
    let primary = serve(Router::new().fallback(post(|| async {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "{\"error\":\"primary failure\"}",
        )
    })))
    .await;
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let capture = requests.clone();
    let mirror = serve(Router::new().fallback(post(move || {
        capture.fetch_add(1, Ordering::SeqCst);
        async { "unexpected" }
    })))
    .await;
    let ctx = context().await;
    let result = execute_resilient_egress(
        &ctx,
        &channel(&primary.url),
        &ModelProxyConfig::default(),
        meta(),
        &primary.url,
        Some(&mirror.url),
        "",
        &serde_json::json!({}),
        ClientProtocol::OpenAi,
    )
    .await;
    assert_eq!(
        result.err().unwrap().status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn streaming_request_without_response_headers_times_out() {
    let upstream = serve(Router::new().fallback(post(|| async {
        std::future::pending::<()>().await;
        "never"
    })))
    .await;
    let ctx = context().await;
    let result = send_attempt(
        &ctx,
        "__direct__",
        true,
        (
            &upstream.url,
            &serde_json::json!({}),
            "",
            TargetProtocol::OpenAiChat,
        ),
        Duration::from_millis(80),
    )
    .await;
    let failure = result.unwrap_err();
    assert_eq!(failure.status, StatusCode::GATEWAY_TIMEOUT);
    assert!(!failure.received_response);
}

#[tokio::test]
async fn model_protocol_override_controls_authentication_headers() {
    let capture = Arc::new(Mutex::new(None));
    let headers = capture.clone();
    let upstream = serve(
        Router::new().fallback(post(move |h: axum::http::HeaderMap| {
            *headers.lock().unwrap() = Some(h);
            async { Json(serde_json::json!({"ok":true})) }
        })),
    )
    .await;
    let mut ch = channel(&upstream.url);
    ch.model_proxy_rules = Some(
        serde_json::from_value(serde_json::json!([
            {"model":"test-model", "mode":"follow", "protocol":"anthropic"}
        ]))
        .unwrap(),
    );
    let ctx = context().await;
    execute_resilient_egress(
        &ctx,
        &ch,
        &ModelProxyConfig::default(),
        meta(),
        &upstream.url,
        None,
        "test-key",
        &serde_json::json!({}),
        ClientProtocol::OpenAi,
    )
    .await
    .unwrap_or_else(|_| panic!("request failed"));
    let captured = capture.lock().unwrap();
    let h = captured.as_ref().unwrap();
    assert_eq!(h["x-api-key"], "test-key");
    assert_eq!(h["anthropic-version"], "2023-06-01");
}

#[tokio::test]
async fn mirror_rate_limit_status_body_and_retry_after_reach_the_client() {
    let upstream = serve(Router::new().fallback(post(|| async {
        (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "17")],
            "{\"error\":\"mirror quota\"}",
        )
    })))
    .await;
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let ctx = context().await;
    let ch = channel(&primary);
    let result = execute_resilient_egress(
        &ctx,
        &ch,
        &ModelProxyConfig::default(),
        meta(),
        &primary,
        Some(&upstream.url),
        "",
        &serde_json::json!({}),
        ClientProtocol::OpenAi,
    )
    .await;
    let response = result.err().expect("mirror must fail with its own status");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()["retry-after"], "17");
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&body).contains("mirror quota"));
}

#[tokio::test]
async fn error_body_read_is_bounded_and_keeps_http_status() {
    let upstream = serve(Router::new().fallback(post(|| async {
        (
            StatusCode::BAD_GATEWAY,
            "x".repeat(MAX_UPSTREAM_ERROR_BYTES + 1),
        )
    })))
    .await;
    let ctx = context().await;
    let failure = send_attempt(
        &ctx,
        "__direct__",
        true,
        (
            &upstream.url,
            &serde_json::json!({}),
            "",
            TargetProtocol::OpenAiChat,
        ),
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.status, StatusCode::BAD_GATEWAY);
    assert!(failure.received_response);
    assert!(failure.body.is_empty());
    assert!(failure.message.contains("1 MiB"));
}

#[tokio::test]
async fn stalled_error_body_has_a_deadline_without_losing_rate_limit() {
    let upstream = serve(Router::new().fallback(post(|| async {
        let pending = futures_util::stream::pending::<Result<Bytes, std::io::Error>>();
        (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "12")],
            axum::body::Body::from_stream(pending),
        )
    })))
    .await;
    let ctx = context().await;
    let failure = send_attempt(
        &ctx,
        "__direct__",
        true,
        (
            &upstream.url,
            &serde_json::json!({}),
            "",
            TargetProtocol::OpenAiChat,
        ),
        Duration::from_millis(200),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(failure.retry_after_ms, Some(12_000));
    assert!(failure.received_response);
}

#[tokio::test]
async fn exhausted_dispatch_budget_does_not_start_another_key_or_channel() {
    let ctx = context().await;
    let ch = channel("http://127.0.0.1:1");
    let cfg = ModelProxyConfig {
        timeout_seconds: 10,
        ..ModelProxyConfig::default()
    };
    let result = super::super::pipeline::dispatch_protocol_egress(
        &ctx,
        &cfg,
        &ch,
        "test-model",
        "test-model",
        "/v1/chat/completions",
        "deadline",
        true,
        Instant::now() - Duration::from_secs(11),
        &None,
        super::super::egress::EgressBody::native(serde_json::json!({})),
        ClientProtocol::OpenAi,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(result.err().unwrap().status(), StatusCode::GATEWAY_TIMEOUT);
}
