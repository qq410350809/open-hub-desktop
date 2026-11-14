use super::balancer::{
    build_client_for_candidate, format_upstream_error_message, get_node_display_name,
    get_sorted_egress_candidates,
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
use std::time::Instant;

/// 同一次请求内换节点的 429 退避上界。再长会让客户端以为请求卡死；
/// 跨请求的渠道冷却见 `MAX_429_COOLDOWN_MS`。
pub(super) const MAX_429_BACKOFF_MS: u64 = 30_000;
/// 无 Retry-After 时，渠道+模型冷却的起步等待。
pub(super) const DEFAULT_429_COOLDOWN_MS: u64 = 15_000;
/// 429 等待下限。500ms 级重试几乎必然落在上游窗口内，再撞一次白烧配额。
pub(super) const MIN_429_BACKOFF_MS: u64 = 5_000;
/// 跨请求冷却上界：覆盖常见「N 次 / 5 分钟」窗口的一部分，同时不把客户端挂死。
pub(super) const MAX_429_COOLDOWN_MS: u64 = 120_000;

/// 无 Retry-After 时的指数退避：5s 起，每次翻倍（5/10/20/30s…），
/// 由调用方截到 `MAX_429_BACKOFF_MS`。
pub(super) fn exponential_backoff_ms(attempt_idx: usize) -> u64 {
    5_000u64 << attempt_idx.min(4)
}

/// 跨请求冷却时长：有 Retry-After 时夹到 [5s, 120s]，否则 15s 起随连续 429 翻倍。
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
    body: bytes::Bytes,
    retry_after_ms: Option<u64>,
) -> Response {
    let mut resp = (status, [("content-type", "application/json")], body).into_response();
    if status == StatusCode::TOO_MANY_REQUESTS {
        let wait_ms = retry_after_ms
            .unwrap_or(DEFAULT_429_COOLDOWN_MS)
            .clamp(MIN_429_BACKOFF_MS, MAX_429_COOLDOWN_MS);
        attach_retry_after(&mut resp, wait_ms);
    }
    resp
}

/// 解析上游 `Retry-After`：支持「延迟秒数」形式（RFC 7231 的两种取值中实际唯一常见的一种）。
/// HTTP-date 形式不解析——需要当前时间基准，且各家限流响应几乎都用秒数。
pub(super) fn parse_retry_after_value(raw: &str) -> Option<u64> {
    let secs: f64 = raw.trim().parse().ok()?;
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    Some((secs * 1000.0) as u64)
}

pub(super) fn parse_retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    parse_retry_after_value(headers.get("retry-after")?.to_str().ok()?)
}

#[derive(Clone, Debug)]
pub struct EgressRequestMeta {
    pub req_id: String,
    pub path: String,
    pub channel_id: String,
    /// 统计维度稳定数字 ID（字符串形式），随失败日志落库
    pub channel_stats_id: Option<String>,
    /// 客户端原始模型名（可能带渠道别名前缀）：仅用于日志展示与统计维度
    pub model: String,
    /// 剥离前缀后的裸模型名：用于匹配「管理可用模型」里的模型级代理出口规则。
    /// 必须与 model 区分 —— 规则表的键来自上游返回的原始模型 id（无前缀），
    /// 拿带前缀的 model 去查表会 miss 并静默退回渠道级代理模式。
    pub rule_model: String,
    pub stream: bool,
    pub req_body_str: Option<String>,
    /// 发起请求的客户端标识（User-Agent / 端点推断）：失败日志也要归属到真实客户端
    pub client_name: Option<String>,
    /// 客户端原始 User-Agent 请求头（截断保存）
    pub user_agent: Option<String>,
    /// 客户端会话标识（x-session-id 等）
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

/// 构建出网请求（含鉴权头），供常规发送与重试复用，保证两次发出的请求完全一致。
fn build_egress_request(
    client: &reqwest::Client,
    upstream_url: &str,
    body: &JsonValue,
    channel_api_key: &str,
    target: TargetProtocol,
) -> reqwest::RequestBuilder {
    let mut req_builder = client
        .post(upstream_url)
        .header("Content-Type", "application/json")
        .header("User-Agent", GATEWAY_USER_AGENT)
        .json(body);

    if !channel_api_key.is_empty() {
        req_builder = req_builder.header("Authorization", format!("Bearer {channel_api_key}"));
    }
    if matches!(target, TargetProtocol::AnthropicMessages) {
        // Anthropic 原生 /v1/messages 快车道：官方 API 仅认 x-api-key，
        // 兼容站两者皆收；版本头为官方必需
        if !channel_api_key.is_empty() {
            req_builder = req_builder.header("x-api-key", channel_api_key);
        }
        req_builder = req_builder.header("anthropic-version", "2023-06-01");
    }
    req_builder
}

/// 通用弹性出网请求调度引擎
/// 统一处理：全局节点轮询、`config.max_retries` 动态重试循环、401 立即退出、429/网络异常自动切 IP 重试、Attempt 独立打点
///
/// `max_retries` 是**换出口节点**重试的预算：只有出口候选多于一个（代理池 / pool 模式）
/// 时才有意义。直连单出口的渠道同一把 Key 重发结果不会变，退化为 0 —— Key 级故障转移
/// （同组顺延 → 下一组）由 pipeline 负责。
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
    let candidates =
        get_sorted_egress_candidates(ctx, channel, &meta.rule_model, channel_api_key).await;
    let max_retries = if candidates.len() > 1 {
        config.max_retries as usize
    } else {
        0
    };
    let total_attempts_allowed = max_retries + 1;
    let node_round_robin = ctx.node_round_robin_for(&channel.id).await;
    let base_node_idx = node_round_robin.load(Ordering::Relaxed);

    let mut last_error = String::new();
    let mut last_status = StatusCode::BAD_GATEWAY;
    let mut last_err_bytes = Bytes::new();
    let mut count_429: usize = 0;
    let mut last_retry_after_ms: Option<u64> = None;
    let mut received_upstream_response = false;

    for attempt_idx in 0..total_attempts_allowed {
        let cand_id = if candidates.is_empty() {
            "__direct__"
        } else {
            &candidates[(base_node_idx + attempt_idx) % candidates.len()]
        };
        let cand_start = Instant::now();
        let client = build_client_for_candidate(ctx, cand_id, meta.stream).await;
        let node_display = get_node_display_name(ctx, cand_id).await;

        let attempt_req_id = if attempt_idx == 0 {
            meta.req_id.clone()
        } else {
            format!("{}#{}", meta.req_id, attempt_idx + 1)
        };

        let send_result = build_egress_request(
            &client,
            upstream_url,
            body,
            channel_api_key,
            TargetProtocol::from_channel(channel),
        )
        .send()
        .await;

        match send_result {
            Ok(resp) => {
                received_upstream_response = true;
                let status = resp.status();
                if status.is_success() {
                    ctx.metrics
                        .successful_requests
                        .fetch_add(1, Ordering::Relaxed);
                    return Ok(EgressSuccess {
                        status,
                        response: resp,
                        cand_id: cand_id.to_string(),
                        node_display,
                        cand_start,
                        attempt_req_id,
                        attempt_idx,
                        upstream_url: upstream_url.to_string(),
                    });
                } else if status == StatusCode::UNAUTHORIZED {
                    let err_bytes = resp.bytes().await.unwrap_or_default();
                    let err_text = String::from_utf8_lossy(&err_bytes).to_string();
                    let formatted = format_upstream_error_message(status.as_u16(), &err_text);

                    record_attempt_failure(
                        ctx,
                        ProxyLogParams::new_failure(
                            attempt_req_id,
                            meta.path.clone(),
                            meta.channel_id.clone(),
                            meta.model.clone(),
                            meta.stream,
                            401,
                            cand_start.elapsed().as_millis() as u64,
                            Some(formatted),
                            meta.req_body_str.clone(),
                            Some(node_display),
                        )
                    .with_channel_stats_id(meta.channel_stats_id.clone())
                    .with_client_name(meta.client_name.clone())
                    .with_user_agent(meta.user_agent.clone())
                    .with_session_id(meta.session_id.clone())
                    .with_upstream_url(Some(upstream_url.to_string()))
                    .with_response_body(cap_log_body(err_text)),
                )
                .await;

                return Err((
                    StatusCode::UNAUTHORIZED,
                        [("content-type", "application/json")],
                        err_bytes,
                    )
                        .into_response());
                } else if status == StatusCode::TOO_MANY_REQUESTS {
                    // Retry-After 必须在 resp.bytes() 消费掉响应前读取
                    let retry_after = parse_retry_after_ms(resp.headers());
                    last_retry_after_ms = retry_after;
                    let err_bytes = resp.bytes().await.unwrap_or_default();
                    let err_text = String::from_utf8_lossy(&err_bytes).to_string();
                    let formatted = format_upstream_error_message(status.as_u16(), &err_text);
                    last_status = status;
                    last_error = formatted.clone();
                    last_err_bytes = err_bytes;
                    count_429 += 1;

                    record_attempt_failure(
                        ctx,
                        ProxyLogParams::new_failure(
                            attempt_req_id,
                            meta.path.clone(),
                            meta.channel_id.clone(),
                            meta.model.clone(),
                            meta.stream,
                            429,
                            cand_start.elapsed().as_millis() as u64,
                            Some(formatted),
                            meta.req_body_str.clone(),
                            Some(node_display),
                        )
                    .with_channel_stats_id(meta.channel_stats_id.clone())
                    .with_client_name(meta.client_name.clone())
                    .with_user_agent(meta.user_agent.clone())
                    .with_session_id(meta.session_id.clone())
                    .with_upstream_url(Some(upstream_url.to_string()))
                    .with_response_body(cap_log_body(err_text.clone())),
                )
                .await;

                if count_429 <= max_retries {
                        node_round_robin.fetch_add(1, Ordering::Relaxed);
                        // 上游明确给出 Retry-After 时以它为准（截到上界），否则退回指数退避。
                        // 旧实现是线性 500+300*n（上界仅 1.4s），对上游按秒计的限流窗口太短，
                        // 重试往往落在窗口内再次撞 429，白耗一个重试名额。
                        let backoff_ms = retry_after
                            .unwrap_or_else(|| exponential_backoff_ms(attempt_idx))
                            .clamp(MIN_429_BACKOFF_MS, MAX_429_BACKOFF_MS);
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                        continue;
                    } else {
                        break;
                    }
                } else {
                    let err_bytes = resp.bytes().await.unwrap_or_default();
                    let err_text = String::from_utf8_lossy(&err_bytes).to_string();
                    let formatted = format_upstream_error_message(status.as_u16(), &err_text);
                    last_status = status;
                    last_error = formatted.clone();
                    last_err_bytes = err_bytes;

                    record_attempt_failure(
                        ctx,
                        ProxyLogParams::new_failure(
                            attempt_req_id,
                            meta.path.clone(),
                            meta.channel_id.clone(),
                            meta.model.clone(),
                            meta.stream,
                            status.as_u16(),
                            cand_start.elapsed().as_millis() as u64,
                            Some(formatted),
                            meta.req_body_str.clone(),
                            Some(node_display),
                        )
                    .with_channel_stats_id(meta.channel_stats_id.clone())
                    .with_client_name(meta.client_name.clone())
                    .with_user_agent(meta.user_agent.clone())
                    .with_session_id(meta.session_id.clone())
                    .with_upstream_url(Some(upstream_url.to_string()))
                    .with_response_body(cap_log_body(err_text)),
                )
                .await;

                if status.is_client_error() {
                        return Err((
                            status,
                            [("content-type", "application/json")],
                            last_err_bytes,
                        )
                            .into_response());
                    }

                    if attempt_idx < max_retries {
                        node_round_robin.fetch_add(1, Ordering::Relaxed);
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        continue;
                    } else {
                        break;
                    }
                }
            }
            Err(e) => {
                let formatted = format!("连接节点失败: {e}");
                last_error = formatted.clone();
                last_status = StatusCode::BAD_GATEWAY;

                record_attempt_failure(
                    ctx,
                    ProxyLogParams::new_failure(
                        attempt_req_id,
                        meta.path.clone(),
                        meta.channel_id.clone(),
                        meta.model.clone(),
                        meta.stream,
                        502,
                        cand_start.elapsed().as_millis() as u64,
                        Some(formatted),
                        meta.req_body_str.clone(),
                        Some(node_display),
                    )
                    .with_channel_stats_id(meta.channel_stats_id.clone())
                    .with_client_name(meta.client_name.clone())
                    .with_user_agent(meta.user_agent.clone())
                    .with_session_id(meta.session_id.clone()),
                )
                .await;

                if attempt_idx < max_retries {
                    node_round_robin.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    continue;
                } else {
                    break;
                }
            }
        }
    }

    // 主地址重试预算耗尽且从未收到任何上游 HTTP 响应（传输层失败：DNS 解析
    // 失败 / 连接超时 / 建连被拒），配置了辅助站点地址时改试一次。
    // 只兜传输层失败：若上游已返回 4xx/5xx，说明地址本身是通的，
    // 换镜像域名改变不了请求侧问题，保留原始错误更可诊断。
    // 只试一次：辅助地址也不通时立即把主地址的错误返回给客户端，不放大等待。
    if !received_upstream_response {
        if let Some(aux_url) = aux_upstream_url {
            let cand_id = if candidates.is_empty() {
                "__direct__"
            } else {
                &candidates[base_node_idx % candidates.len()]
            };
            let cand_start = Instant::now();
            let client = build_client_for_candidate(ctx, cand_id, meta.stream).await;
            let node_display = get_node_display_name(ctx, cand_id).await;
            let attempt_req_id = format!("{}#aux", meta.req_id);
            tracing::warn!(
                "[ModelGateway] 渠道「{}」主地址不可达，改试辅助站点地址：{}",
                channel.effective_alias(),
                aux_url
            );
            let send_result = build_egress_request(
                &client,
                aux_url,
                body,
                channel_api_key,
                TargetProtocol::from_channel(channel),
            )
            .send()
            .await;
            match send_result {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        ctx.metrics
                            .successful_requests
                            .fetch_add(1, Ordering::Relaxed);
                        return Ok(EgressSuccess {
                            status,
                            response: resp,
                            cand_id: cand_id.to_string(),
                            node_display,
                            cand_start,
                            attempt_req_id,
                            attempt_idx: total_attempts_allowed,
                            upstream_url: aux_url.to_string(),
                        });
                    }
                    tracing::warn!(
                        "[ModelGateway] 渠道「{}」辅助站点地址返回 {}，保留主地址错误响应",
                        channel.effective_alias(),
                        status
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "[ModelGateway] 渠道「{}」辅助站点地址连接失败：{}",
                        channel.effective_alias(),
                        e
                    );
                }
            }
        }
    }

    if !last_err_bytes.is_empty() {
        // 上游错误体原样透传（保持上游自身协议形状）；429 补 Retry-After 给客户端退避。
        Err(upstream_error_response(
            last_status,
            last_err_bytes,
            last_retry_after_ms,
        ))
    } else {
        Err(gateway_error_response(
            client_protocol,
            last_status,
            &last_status.as_u16().to_string(),
            last_error,
        ))
    }
}
