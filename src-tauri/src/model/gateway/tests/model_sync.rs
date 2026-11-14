use super::*;
use crate::model::gateway::types::ModelProxyState;
use std::sync::{Arc, Mutex};

type MockReply = (StatusCode, JsonValue, Duration);

struct MockUpstream {
    url: String,
    requests: Arc<Mutex<Vec<(String, String)>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for MockUpstream {
    fn drop(&mut self) { self.task.abort(); }
}

async fn upstream(reply: impl Fn(&str, &str) -> MockReply + Send + Sync + 'static) -> MockUpstream {
    let reply = Arc::new(reply);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let capture = requests.clone();
    let app = Router::new().fallback(get(move |uri: Uri, headers: HeaderMap| {
        let reply = reply.clone();
        let capture = capture.clone();
        async move {
            let key = headers.get("authorization").and_then(|value| value.to_str().ok()).unwrap_or_default();
            capture.lock().unwrap().push((uri.path().to_string(), key.to_string()));
            let (status, body, delay) = reply(uri.path(), key);
            tokio::time::sleep(delay).await;
            (status, Json(body))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
    MockUpstream { url, requests, task }
}

fn channel(id: &str, base: &str) -> ChannelConfig {
    serde_json::from_value(json!({"id": id, "name": id, "enabled": true, "upstreamUrl": base})).unwrap()
}

async fn context(channels: Vec<ChannelConfig>) -> ModelProxyContext {
    let mut ctx = ModelProxyState::new().context;
    ctx.models_fetch_client = reqwest::Client::builder().no_proxy().build().unwrap();
    ctx.config.write().await.channels = channels;
    ctx
}

#[tokio::test]
async fn model_sync_selected_channel_never_fetches_other_channels() {
    let first = upstream(|_, _| (StatusCode::OK, json!({"data":[{"id":"first"}]}), Duration::ZERO)).await;
    let other = upstream(|_, _| (StatusCode::OK, json!({"data":[{"id":"other"}]}), Duration::ZERO)).await;
    let ctx = context(vec![channel("first", &first.url), channel("other", &other.url)]).await;
    let (models, errors) = fetch_upstream_models_inner(&ctx, Some("first"), Some("run-1")).await;
    assert!(errors.is_empty());
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].models, vec!["first"]);
    assert_eq!(first.requests.lock().unwrap().len(), 1);
    assert!(other.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn model_sync_unions_distinct_models_for_each_key() {
    let mock = upstream(|_, key| {
        let model = if key == "Bearer key-a" { "model-a" } else { "model-b" };
        (StatusCode::OK, json!({"data":[{"id":model},{"id":"shared"}]}), Duration::ZERO)
    }).await;
    let mut ch = channel("keys", &mock.url);
    ch.api_keys = Some(vec!["key-a".to_string(), "key-b".to_string()]);
    let ctx = context(vec![ch]).await;
    let (models, errors) = fetch_upstream_models_inner(&ctx, Some("keys"), None).await;
    assert!(errors.is_empty());
    let mut models = models[0].models.clone();
    models.sort();
    assert_eq!(models, vec!["model-a", "model-b", "shared"]);
    assert_eq!(mock.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn model_sync_timeout_preserves_previous_cache() {
    let mock = upstream(|_, _| (StatusCode::OK, json!({"data":[{"id":"new"}]}), Duration::from_secs(1))).await;
    let ctx = context(vec![channel("slow", &mock.url)]).await;
    ctx.cached_channel_models.write().await.push(ChannelModelList {
        channel_id: "slow".to_string(), channel_name: "slow".to_string(), alias: "slow".to_string(), models: vec!["cached".to_string()],
    });
    let (models, errors) = fetch_upstream_models_with_timeout(&ctx, Some("slow"), None, Duration::from_millis(80)).await;
    assert!(models.is_empty());
    assert_eq!(errors.len(), 1);
    assert!(errors[0].error.contains("超时"));
    assert_eq!(ctx.cached_channel_models.read().await[0].models, vec!["cached"]);
}

#[tokio::test]
async fn model_sync_reserves_time_for_aux_when_primary_stalls() {
    let main = upstream(|_, _| (StatusCode::OK, json!({"data":[]}), Duration::from_secs(2))).await;
    let aux = upstream(|_, _| (StatusCode::OK, json!({"data":[{"id":"backup"}]}), Duration::ZERO)).await;
    let mut ch = channel("fallback", &main.url);
    ch.aux_url = Some(aux.url.clone());
    let ctx = context(vec![ch]).await;
    let (models, errors) = fetch_upstream_models_with_timeout(&ctx, Some("fallback"), None, Duration::from_millis(400)).await;
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(models[0].models, vec!["backup"]);
    assert_eq!(aux.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn model_sync_tries_legacy_endpoint_and_redacts_error_body() {
    let mock = upstream(|path, _| {
        if path == "/v1/models" {
            (StatusCode::NOT_FOUND, json!({"error":"secret-token"}), Duration::ZERO)
        } else {
            (StatusCode::OK, json!({"models":[{"name":"models/gemini"}]}), Duration::ZERO)
        }
    }).await;
    let ctx = context(vec![channel("legacy", &mock.url)]).await;
    let (models, errors) = fetch_upstream_models_inner(&ctx, Some("legacy"), None).await;
    assert!(errors.is_empty());
    assert_eq!(models[0].models, vec!["gemini"]);
    assert_eq!(mock.requests.lock().unwrap().iter().map(|item| item.0.as_str()).collect::<Vec<_>>(), vec!["/v1/models", "/models"]);

    let failed = upstream(|_, _| (StatusCode::UNAUTHORIZED, json!({"error":"secret-token"}), Duration::ZERO)).await;
    let ctx = context(vec![channel("invalid", &failed.url)]).await;
    let (_, errors) = fetch_upstream_models_inner(&ctx, Some("invalid"), None).await;
    assert!(errors[0].error.contains("401"));
    assert!(!errors[0].error.contains("secret-token"));
}

#[test]
fn model_sync_aux_mirror_keeps_version_and_query_without_duplication() {
    use crate::model::gateway::egress::aux_upstream_for;
    let cases = [
        ("https://main.example/v1", "https://aux.example", "https://main.example/v1/chat/completions", "https://aux.example/v1/chat/completions"),
        ("https://main.example", "https://aux.example/v1", "https://main.example/v1/chat/completions", "https://aux.example/v1/chat/completions"),
        ("https://main.example/v1", "https://aux.example/proxy/v1", "https://main.example/v1/messages", "https://aux.example/proxy/v1/messages"),
        ("https://main.example/v1", "https://aux.example/proxy", "https://main.example/v1/responses", "https://aux.example/proxy/v1/responses"),
        ("https://main.example", "https://aux.example", "https://main.example/v1beta/models/gemini:generateContent?key=test", "https://aux.example/v1beta/models/gemini:generateContent?key=test"),
    ];
    for (main, aux, upstream, expected) in cases {
        let mut ch = channel("aux", main);
        ch.aux_url = Some(aux.to_string());
        assert_eq!(aux_upstream_for(&ch, upstream).as_deref(), Some(expected));
        assert!(aux_upstream_for(&ch, "https://unrelated.example/v1/messages").is_none());
    }
}

#[test]
fn model_sync_progress_correlates_requests_without_url_credentials() {
    let bus = crate::context::EventBus::new();
    let mut events = bus.subscribe();
    let progress = SyncProgress { bus: Some(&bus), request_id: Some("run-123"), channel_id: "channel-1" };
    progress.emit(ModelProxySyncStage::Request, "request", Some("https://user:pass@example.com/v1/models?key=secret#token"), Some((2, 3)));
    let event = events.try_recv().unwrap();
    assert_eq!(event.event, "model-proxy-sync-progress");
    assert_eq!(event.payload["requestId"], "run-123");
    assert_eq!(event.payload["channelId"], "channel-1");
    assert_eq!(event.payload["keyIndex"], 2);
    assert_eq!(event.payload["endpoint"], "https://example.com/v1/models");
}
