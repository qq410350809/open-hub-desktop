use super::balancer::{
    build_client_for_candidate, format_upstream_error_message, get_node_display_name,
    get_sorted_egress_candidates,
};
use super::egress::TargetProtocol;
use super::logger::{cap_log_body, record_attempt_failure, ProxyLogParams};
use super::policies::opencode::{
    apply_cli_identity_headers, is_empty_success_payload, matches_channel_or_url,
    GATEWAY_USER_AGENT,
};
use super::pipeline::{gateway_error_response, ClientProtocol};
use super::types::{ChannelConfig, ModelProxyConfig, ModelProxyContext};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use serde_json::Value as JsonValue;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tracing::warn;

#[derive(Clone, Debug)]
pub struct EgressRequestMeta {
    pub req_id: String,
    pub path: String,
    pub channel_id: String,
    /// 统计维度稳定数字 ID（字符串形式），随失败日志落库
    pub channel_stats_id: Option<String>,
    pub model: String,
    pub stream: bool,
    pub req_body_str: Option<String>,
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

/// 构建出网请求（含 OpenCode CLI 身份头模拟与鉴权头），供常规发送与 503 原地重试复用，
/// 保证两次发出的请求完全一致。
fn build_egress_request(
    client: &reqwest::Client,
    upstream_url: &str,
    body: &JsonValue,
    channel_api_key: &str,
    is_opencode: bool,
    attempt_req_id: &str,
    session_seed: &str,
    target: TargetProtocol,
) -> reqwest::RequestBuilder {
    let mut req_builder = client
        .post(upstream_url)
        .header("Content-Type", "application/json")
        .json(body);

    if is_opencode {
        // OpenCode 渠道个性化身份策略见 policies/opencode.rs
        req_builder = apply_cli_identity_headers(req_builder, session_seed, attempt_req_id);
    } else {
        req_builder = req_builder.header("User-Agent", GATEWAY_USER_AGENT);
    }

    if !channel_api_key.is_empty() {
        req_builder = req_builder.header("Authorization", format!("Bearer {channel_api_key}"));
    }
    if matches!(target, TargetProtocol::AnthropicMessages) && !is_opencode {
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
pub async fn execute_resilient_egress(
    ctx: &ModelProxyContext,
    channel: &ChannelConfig,
    config: &ModelProxyConfig,
    meta: EgressRequestMeta,
    upstream_url: &str,
    channel_api_key: &str,
    body: &JsonValue,
    client_protocol: ClientProtocol,
) -> Result<EgressSuccess, Response> {
    let candidates = get_sorted_egress_candidates(ctx, channel).await;
    let max_retries = config.max_retries as usize;
    let total_attempts_allowed = max_retries + 1;
    let base_node_idx = ctx.node_round_robin.load(Ordering::Relaxed);

    let mut last_error = String::new();
    let mut last_status = StatusCode::BAD_GATEWAY;
    let mut last_err_bytes = Bytes::new();
    let mut count_429: usize = 0;

    for attempt_idx in 0..total_attempts_allowed {
        let cand_id = if candidates.is_empty() {
            "__direct__"
        } else {
            &candidates[(base_node_idx + attempt_idx) % candidates.len()]
        };
        let cand_start = Instant::now();
        let client = build_client_for_candidate(ctx, cand_id).await;
        let node_display = get_node_display_name(ctx, cand_id).await;

        let attempt_req_id = if attempt_idx == 0 {
            meta.req_id.clone()
        } else {
            format!("{}#{}", meta.req_id, attempt_idx + 1)
        };

        let is_opencode = matches_channel_or_url(channel, upstream_url);

        // OpenCode 渠道专属容错：遇到 502/503，或 200 但响应体为空内容（官方已知缺陷）时，
        // 在当前节点等待 1 秒后原地重试一次。
        // 该次重试不受 max_retries 名额约束、不切换节点；每个候选节点各享一次机会，
        // 重试仍失败则切换到其他节点；全部候选耗尽后空内容以 400 返回客户端。所有异常请求均记录失败日志。
        let mut inplace_retried = false;

        /// 内层发送循环的出口
        enum SendStep {
            /// 正常交由外层处理（成功响应 / 上游错误响应 / 网络错误）
            Done(Result<reqwest::Response, reqwest::Error>),
            /// 本节点判定失败（原地重试后仍为空内容），转入常规节点切换流程
            NodeFailedEmpty,
        }

        // 内层循环仅在触发原地重试时多转一圈，其余情况原样返回发送结果
        let send_step: SendStep = loop {
            let result = build_egress_request(
                &client,
                upstream_url,
                body,
                channel_api_key,
                is_opencode,
                &attempt_req_id,
                &meta.req_id,
                TargetProtocol::from_channel(channel),
            )
            .send()
            .await;

            let retryable_status = matches!(
                &result,
                Ok(resp)
                    if resp.status() == StatusCode::BAD_GATEWAY
                        || resp.status() == StatusCode::SERVICE_UNAVAILABLE
            );

            // ① 502/503：网关类临时故障，读取错误体记录日志后原地重试。
            // 对所有渠道通用 —— 站点转换渠道/转发渠道同样受益。
            if !inplace_retried && retryable_status {
                inplace_retried = true;
                let resp = match result {
                    Ok(r) => r,
                    Err(_) => unreachable!("retryable_status 仅在 result 为 Ok 时成立"),
                };
                let status = resp.status();
                let err_bytes = resp.bytes().await.unwrap_or_default();
                let err_text = String::from_utf8_lossy(&err_bytes).to_string();
                let formatted = format_upstream_error_message(status.as_u16(), &err_text);
                last_err_bytes = err_bytes.clone();

                record_attempt_failure(
                    ctx,
                    ProxyLogParams::new_failure(
                        attempt_req_id.clone(),
                        meta.path.clone(),
                        meta.channel_id.clone(),
                        meta.model.clone(),
                        meta.stream,
                        status.as_u16(),
                        cand_start.elapsed().as_millis() as u64,
                        Some(format!(
                            "上游 {status}，1 秒后在当前节点原地重试（不占重试名额）: {formatted}"
                        )),
                        meta.req_body_str.clone(),
                        Some(node_display.clone()),
                    )
                    .with_channel_stats_id(meta.channel_stats_id.clone())
                    .with_upstream_url(Some(upstream_url.to_string()))
                    .with_response_body(cap_log_body(err_text)),
                )
                .await;

                warn!("[ModelGateway] 上游 {status}，节点 {node_display} 1 秒后原地重试");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }

            // ② 200 但响应体为空内容（OpenCode 官方缺陷）：同样触发原地重试；
            // 重试后仍为空则本节点判负，最终以 400 返回客户端。
            // 仅限非流式请求 —— 流式 body 无法在不破坏转发的前提下预读判定。
            if is_opencode
                && !meta.stream
                && matches!(&result, Ok(resp) if resp.status().is_success())
            {
                let resp = match result {
                    Ok(r) => r,
                    Err(_) => unreachable!("matches! 已确保 result 为 Ok"),
                };
                let status = resp.status();
                let body_bytes = resp.bytes().await.unwrap_or_default();
                if !is_empty_success_payload(&body_bytes) {
                    // 内容有效：把预读的 body 重新打包为完整 Response 返回，上层无感知
                    let rebuilt = axum::http::Response::builder()
                        .status(axum::http::StatusCode::from_u16(status.as_u16())
                            .unwrap_or(axum::http::StatusCode::OK))
                        .header("Content-Type", "application/json")
                        .body(body_bytes)
                        .expect("固定字段合成响应必然合法");
                    break SendStep::Done(Ok(rebuilt.into()));
                }

                // 空内容一律记录失败日志（含首次与原地重试后的第二次）
                record_attempt_failure(
                    ctx,
                    ProxyLogParams::new_failure(
                        attempt_req_id.clone(),
                        meta.path.clone(),
                        meta.channel_id.clone(),
                        meta.model.clone(),
                        meta.stream,
                        200,
                        cand_start.elapsed().as_millis() as u64,
                        Some(
                            "OpenCode 上游返回 200 空内容（官方已知缺陷，按错误请求处理）"
                                .to_string(),
                        ),
                        meta.req_body_str.clone(),
                        Some(node_display.clone()),
                    )
                    .with_channel_stats_id(meta.channel_stats_id.clone())
                    .with_upstream_url(Some(upstream_url.to_string()))
                    .with_response_body(cap_log_body(
                        String::from_utf8_lossy(&body_bytes).to_string(),
                    )),
                )
                .await;

                if !inplace_retried {
                    inplace_retried = true;
                    warn!(
                        "[ModelGateway] OpenCode 上游 200 空内容，节点 {node_display} 1 秒后原地重试"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }

                warn!(
                    "[ModelGateway] OpenCode 上游原地重试仍为空内容，节点 {node_display} 判负"
                );
                break SendStep::NodeFailedEmpty;
            }

            break SendStep::Done(result);
        };

        // 空内容视为错误请求：与 5xx 同样参与节点轮换；预算耗尽后向客户端返回 400
        if matches!(send_step, SendStep::NodeFailedEmpty) {
            last_status = StatusCode::BAD_REQUEST;
            last_error = "OpenCode 上游持续返回空内容（已原地重试并轮换节点），按错误请求处理".to_string();

            if attempt_idx < max_retries {
                ctx.node_round_robin.fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                continue;
            }
            break;
        }

        let send_result = match send_step {
            SendStep::Done(r) => r,
            SendStep::NodeFailedEmpty => unreachable!("已在上方拦截"),
        };

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
                    .with_upstream_url(Some(upstream_url.to_string()))
                        .with_response_body(cap_log_body(err_text.clone())),
                    )
                    .await;

                    if count_429 <= max_retries {
                        ctx.node_round_robin.fetch_add(1, Ordering::Relaxed);
                        // 遭遇 429 时增加动态指数退避延迟（500ms ~ 1500ms），避免瞬时重试风暴击穿后续所有节点
                        let backoff_ms = 500 + 300 * (attempt_idx as u64).min(3);
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
                        ctx.node_round_robin.fetch_add(1, Ordering::Relaxed);
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
                    .with_channel_stats_id(meta.channel_stats_id.clone()),
                )
                .await;

                if attempt_idx < max_retries {
                    ctx.node_round_robin.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    continue;
                } else {
                    break;
                }
            }
        }
    }

    if !last_err_bytes.is_empty() {
        // 上游错误体原样透传（保持上游自身协议形状）
        Err((
            last_status,
            [("content-type", "application/json")],
            last_err_bytes,
        )
            .into_response())
    } else {
        Err(gateway_error_response(
            client_protocol,
            last_status,
            &last_status.as_u16().to_string(),
            last_error,
        ))
    }
}

