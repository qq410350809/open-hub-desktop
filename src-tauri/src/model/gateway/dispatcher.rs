use super::balancer::{
    build_client_for_candidate, egress_timeout, format_upstream_error_message,
    get_node_display_name, get_sorted_egress_candidates,
};
use super::egress::TargetProtocol;
use super::logger::{cap_log_body, record_attempt_failure, ProxyLogParams};
use super::pipeline::{gateway_error_response, ClientProtocol};
use super::types::{ChannelConfig, ModelProxyConfig, ModelProxyContext, GATEWAY_USER_AGENT};
use axum::{
    http::{header::RETRY_AFTER, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use serde_json::Value as JsonValue;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

pub(super) const MAX_429_BACKOFF_MS: u64 = 30_000;
pub(super) const DEFAULT_429_COOLDOWN_MS: u64 = 15_000;
pub(super) const MIN_429_BACKOFF_MS: u64 = 5_000;
pub(super) const MAX_429_COOLDOWN_MS: u64 = 120_000;
/// Error pages must not consume unbounded memory while retries are pending.
const MAX_UPSTREAM_ERROR_BYTES: usize = 1024 * 1024;

pub(super) fn exponential_backoff_ms(attempt_idx: usize) -> u64 {
    5_000u64 << attempt_idx.min(4)
}

pub(super) fn rate_limit_backoff_ms(consecutive: u32, retry_after_ms: Option<u64>) -> u64 {
    if let Some(ms) = retry_after_ms {
        return ms.clamp(MIN_429_BACKOFF_MS, MAX_429_COOLDOWN_MS);
    }
    let shift = consecutive.saturating_sub(1).min(3);
    (DEFAULT_429_COOLDOWN_MS << shift).min(MAX_429_COOLDOWN_MS)
}

fn attach_retry_after(resp: &mut Response, wait_ms: u64) {
    let secs = wait_ms.div_ceil(1000).max(1);
    if let Ok(value) = HeaderValue::from_str(&secs.to_string()) {
        resp.headers_mut().insert(RETRY_AFTER, value);
    }
}

fn upstream_error_response(
    status: StatusCode,
    body: Bytes,
    retry_after_ms: Option<u64>,
) -> Response {
    let mut resp = (status, [("content-type", "application/json")], body).into_response();
    if status == StatusCode::TOO_MANY_REQUESTS {
        attach_retry_after(
            &mut resp,
            retry_after_ms
                .unwrap_or(DEFAULT_429_COOLDOWN_MS)
                .clamp(MIN_429_BACKOFF_MS, MAX_429_COOLDOWN_MS),
        );
    }
    resp
}

/// Fractional seconds are accepted as well as the commonly used whole-second form.
pub(super) fn parse_retry_after_value(raw: &str) -> Option<u64> {
    let secs: f64 = raw.trim().parse().ok()?;
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    Some((secs * 1000.0).ceil() as u64)
}

pub(super) fn parse_retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    parse_retry_after_value(headers.get("retry-after")?.to_str().ok()?)
}

#[derive(Clone, Debug)]
pub struct EgressRequestMeta {
    pub req_id: String,
    pub path: String,
    pub channel_id: String,
    pub channel_stats_id: Option<String>,
    /// Original client model name, retained for logging.
    pub model: String,
    /// Bare upstream model name used for model-level protocol and egress rules.
    pub rule_model: String,
    pub stream: bool,
    pub req_body_str: Option<String>,
    pub client_name: Option<String>,
    pub user_agent: Option<String>,
    pub session_id: Option<String>,
}

#[allow(dead_code)]
pub struct EgressSuccess {
    pub status: StatusCode,
    pub response: reqwest::Response,
    pub cand_id: String,
    pub node_display: String,
    pub cand_start: Instant,
    pub attempt_req_id: String,
    pub attempt_idx: usize,
    pub upstream_url: String,
}

fn build_egress_request(
    client: &reqwest::Client,
    upstream_url: &str,
    body: &JsonValue,
    channel_api_key: &str,
    target: TargetProtocol,
) -> reqwest::RequestBuilder {
    let mut request = client
        .post(upstream_url)
        .header("Content-Type", "application/json")
        .header("User-Agent", GATEWAY_USER_AGENT)
        .json(body);
    if !channel_api_key.is_empty() {
        request = request.header("Authorization", format!("Bearer {channel_api_key}"));
    }
    if matches!(target, TargetProtocol::AnthropicMessages) {
        if !channel_api_key.is_empty() {
            request = request.header("x-api-key", channel_api_key);
        }
        request = request.header("anthropic-version", "2023-06-01");
    }
    request
}

struct AttemptFailure {
    status: StatusCode,
    message: String,
    body: Bytes,
    retry_after_ms: Option<u64>,
    /// Only a transport failure before HTTP headers permits mirror failover.
    received_response: bool,
}

/// One bounded attempt for either the primary or mirror. The deadline covers client
/// preparation, headers and error-body reads, but never a successful streaming body.
async fn send_attempt(
    ctx: &ModelProxyContext,
    candidate: &str,
    streaming: bool,
    request: (&str, &JsonValue, &str, TargetProtocol),
    budget: Duration,
) -> Result<reqwest::Response, AttemptFailure> {
    let operation = async {
        let client = build_client_for_candidate(ctx, candidate, streaming).await;
        let (url, body, key, target) = request;
        build_egress_request(&client, url, body, key, target)
            .send()
            .await
    };
    let deadline = tokio::time::Instant::now() + budget;
    let response = match tokio::time::timeout_at(deadline, operation).await {
        Ok(Ok(response)) => response,
        result => {
            let (status, message) = match result {
                Ok(Err(error)) => {
                    let status = if error.is_timeout() {
                        StatusCode::GATEWAY_TIMEOUT
                    } else {
                        StatusCode::BAD_GATEWAY
                    };
                    (status, format!("连接上游失败: {}", error.without_url()))
                }
                _ => (StatusCode::GATEWAY_TIMEOUT, "等待上游响应超时".to_string()),
            };
            return Err(AttemptFailure {
                status,
                message,
                body: Bytes::new(),
                retry_after_ms: None,
                received_response: false,
            });
        }
    };
    if response.status().is_success() {
        return Ok(response);
    }

    let status = response.status();
    let retry_after_ms = parse_retry_after_ms(response.headers());
    let body_result = tokio::time::timeout_at(deadline, read_error_body(response)).await;
    let (body, detail) = match body_result {
        Ok(Ok(body)) => (body, None),
        Ok(Err(detail)) => (Bytes::new(), Some(detail)),
        Err(_) => (Bytes::new(), Some("读取上游错误响应超时".to_string())),
    };
    let message = detail.unwrap_or_else(|| {
        format_upstream_error_message(status.as_u16(), &String::from_utf8_lossy(&body))
    });
    Err(AttemptFailure {
        status,
        message,
        body,
        retry_after_ms,
        received_response: true,
    })
}

async fn read_error_body(mut response: reqwest::Response) -> Result<Bytes, String> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("读取上游错误响应失败: {}", e.without_url()))?
    {
        if chunk.len() > MAX_UPSTREAM_ERROR_BYTES.saturating_sub(body.len()) {
            return Err("上游错误响应超过 1 MiB，已停止读取".to_string());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body.into())
}

async fn log_attempt_failure(
    ctx: &ModelProxyContext,
    meta: &EgressRequestMeta,
    attempt_id: String,
    node_display: String,
    started: Instant,
    upstream_url: &str,
    failure: &AttemptFailure,
) {
    let endpoint = url::Url::parse(upstream_url).ok().map(|mut url| {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        url.to_string()
    });
    let body = if failure.body.is_empty() {
        None
    } else {
        cap_log_body(String::from_utf8_lossy(&failure.body).into_owned())
    };
    record_attempt_failure(
        ctx,
        ProxyLogParams::new_failure(
            attempt_id,
            meta.path.clone(),
            meta.channel_id.clone(),
            meta.model.clone(),
            meta.stream,
            failure.status.as_u16(),
            started.elapsed().as_millis() as u64,
            Some(failure.message.clone()),
            meta.req_body_str.clone(),
            Some(node_display),
        )
        .with_channel_stats_id(meta.channel_stats_id.clone())
        .with_client_name(meta.client_name.clone())
        .with_user_agent(meta.user_agent.clone())
        .with_session_id(meta.session_id.clone())
        .with_upstream_url(endpoint)
        .with_response_body(body),
    )
    .await;
}

/// Node retry budget is capped by distinct candidates: never replay a Key on the same
/// node in one round. Key/group/channel failover is owned by the pipeline.
pub async fn execute_resilient_egress(
    ctx: &ModelProxyContext,
    channel: &ChannelConfig,
    config: &ModelProxyConfig,
    meta: EgressRequestMeta,
    upstream_url: &str,
    aux_upstream_url: Option<&str>,
    channel_api_key: &str,
    body: &JsonValue,
    client_protocol: ClientProtocol,
) -> Result<EgressSuccess, Response> {
    let mut candidates =
        get_sorted_egress_candidates(ctx, channel, &meta.rule_model, channel_api_key).await;
    if candidates.is_empty() {
        candidates.push("__direct__".to_string());
    }
    let attempts = (config.max_retries as usize)
        .saturating_add(1)
        .min(candidates.len());
    let cursor = ctx.node_round_robin_for(&channel.id).await;
    let base = cursor.load(Ordering::Relaxed) % candidates.len();
    let target = channel.target_protocol_for(&meta.rule_model);
    let timeout = egress_timeout(config);
    // A stalled main origin must leave time to reach its mirror.
    let attempt_budget = timeout / (attempts as u32 + u32::from(aux_upstream_url.is_some()));
    let mut received_response = false;
    let mut last_failure = None;

    for index in 0..(attempts + usize::from(aux_upstream_url.is_some())) {
        let mirror = index == attempts;
        if mirror && received_response {
            break;
        }
        let url = if mirror {
            aux_upstream_url.unwrap()
        } else {
            upstream_url
        };
        let candidate = &candidates[(base + if mirror { 0 } else { index }) % candidates.len()];
        let started = Instant::now();
        let node_display = get_node_display_name(ctx, candidate).await;
        let attempt_id = if mirror {
            format!("{}#aux", meta.req_id)
        } else if index == 0 {
            meta.req_id.clone()
        } else {
            format!("{}#{}", meta.req_id, index + 1)
        };
        match send_attempt(
            ctx,
            candidate,
            meta.stream,
            (url, body, channel_api_key, target),
            attempt_budget,
        )
        .await
        {
            Ok(response) => {
                ctx.metrics
                    .successful_requests
                    .fetch_add(1, Ordering::Relaxed);
                return Ok(EgressSuccess {
                    status: response.status(),
                    response,
                    cand_id: candidate.clone(),
                    node_display,
                    cand_start: started,
                    attempt_req_id: attempt_id,
                    attempt_idx: index,
                    upstream_url: url.to_string(),
                });
            }
            Err(failure) => {
                received_response |= failure.received_response;
                log_attempt_failure(ctx, &meta, attempt_id, node_display, started, url, &failure)
                    .await;
                let retry = !mirror
                    && index + 1 < attempts
                    && (failure.status.is_server_error()
                        || failure.status == StatusCode::TOO_MANY_REQUESTS);
                let backoff = if failure.status == StatusCode::TOO_MANY_REQUESTS {
                    failure
                        .retry_after_ms
                        .unwrap_or_else(|| exponential_backoff_ms(index))
                        .clamp(MIN_429_BACKOFF_MS, MAX_429_BACKOFF_MS)
                } else {
                    200
                };
                last_failure = Some(failure);
                if retry {
                    cursor.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(backoff)).await;
                } else if !mirror && index + 1 < attempts {
                    break;
                }
            }
        }
    }
    let failure = last_failure.expect("at least one egress candidate");
    if !failure.body.is_empty() {
        Err(upstream_error_response(
            failure.status,
            failure.body,
            failure.retry_after_ms,
        ))
    } else {
        let mut response = gateway_error_response(
            client_protocol,
            failure.status,
            &failure.status.as_u16().to_string(),
            failure.message,
        );
        if failure.status == StatusCode::TOO_MANY_REQUESTS {
            attach_retry_after(
                &mut response,
                failure.retry_after_ms.unwrap_or(DEFAULT_429_COOLDOWN_MS),
            );
        }
        Err(response)
    }
}

#[cfg(test)]
#[path = "tests/dispatcher.rs"]
mod dispatcher_tests;
