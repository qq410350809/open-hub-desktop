use super::balancer::{
    build_client_for_candidate, format_upstream_error_message, get_node_display_name,
    get_sorted_egress_candidates, is_opencode_channel,
};
use super::logger::{cap_log_body, record_attempt_failure, ProxyLogParams};
use super::types::{ChannelConfig, ModelProxyConfig, ModelProxyContext};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use bytes::Bytes;
use serde_json::{json, Value as JsonValue};
use std::sync::atomic::Ordering;
use std::time::Instant;

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

        let is_opencode = is_opencode_channel(channel) || upstream_url.contains("opencode.ai");
        let mut req_builder = client
            .post(upstream_url)
            .header("Content-Type", "application/json")
            .json(body);

        if is_opencode {
            // OpenCode 官方 CLI 身份与会话请求头模拟：抹平 CLI 与反代差异，享受官方正常会话配额
            let session_id = format!("sess_{}", meta.req_id.replace('-', ""));
            let project_id = "proj_openhub_gateway".to_string();
            req_builder = req_builder
                .header("User-Agent", "opencode/1.18.18/cli")
                .header("x-opencode-client", "cli")
                .header("x-opencode-session", session_id)
                .header("x-opencode-project", project_id)
                .header("x-opencode-request", attempt_req_id.clone());
        } else {
            req_builder = req_builder.header("User-Agent", "OpenHub-Gateway/0.3.0");
        }

        if !channel_api_key.is_empty() {
            req_builder = req_builder.header("Authorization", format!("Bearer {channel_api_key}"));
        }

        match req_builder.send().await {
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
        Err((
            last_status,
            [("content-type", "application/json")],
            last_err_bytes,
        )
            .into_response())
    } else {
        Err((
            last_status,
            Json(json!({
                "error": {
                    "message": last_error,
                    "type": "upstream_error",
                    "code": last_status.as_u16(),
                    "status": "UNAVAILABLE"
                }
            })),
        )
            .into_response())
    }
}
