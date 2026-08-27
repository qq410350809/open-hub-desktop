//! 协议处理器公共流水线。
//!
//! 四个协议入口（OpenAI Chat / Responses / Anthropic Messages / Gemini）共享同一段骨架：
//! 渠道解析(404) → 模型兼容性校验(400) → 出网准备(目标协议) → 弹性调度 → 公共日志骨架。
//! 各入口文件只负责：入参解析、客户端协议 ↔ OpenAI 中枢转换、响应回转。

use super::balancer::{
    resolve_channel, resolve_channel_key_groups_for_model, select_channel_api_key,
};
use super::dispatcher::{execute_resilient_egress, EgressRequestMeta, EgressSuccess};
use super::egress::{self, TargetProtocol};
use super::logger::{client_name_from_headers, record_attempt_failure, ProxyLogParams};
use super::policies::opencode::check_model_channel_compatibility;
use super::router::check_auth;
use super::types::{
    current_timestamp, ChannelConfig, ModelProxyConfig, ModelProxyContext, ProxyRequestLog,
};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::sync::atomic::Ordering;
use std::time::Instant;

/// 客户端入口协议，决定 404/400 错误体的 JSON 形状
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientProtocol {
    OpenAi,
    /// Responses API 客户端：错误体与 OpenAI 不同（type 在顶层、error 内嵌 code/param）
    Responses,
    Anthropic,
    Gemini,
}

/// 未找到可用渠道时，按客户端协议返回对应格式的 404 响应体
pub fn model_not_found_response(raw_model: &str, style: ClientProtocol) -> Response {
    match style {
        ClientProtocol::Gemini => (
            StatusCode::NOT_FOUND,
            axum::Json(json!({
                "error": {
                    "code": 404,
                    "message": format!("No available channel for model '{raw_model}'"),
                    "status": "NOT_FOUND"
                }
            })),
        )
            .into_response(),
        ClientProtocol::Anthropic => (
            StatusCode::NOT_FOUND,
            axum::Json(json!({
                "type": "error",
                "error": {
                    "type": "not_found_error",
                    "message": format!("No available channel for model '{raw_model}'")
                }
            })),
        )
            .into_response(),
        ClientProtocol::Responses => (
            StatusCode::NOT_FOUND,
            axum::Json(json!({
                "type": "error",
                "error": {
                    "type": "invalid_request_error",
                    "code": "model_not_found",
                    "message": format!("No available channel for model '{raw_model}'"),
                    "param": null,
                    "request_id": null
                }
            })),
        )
            .into_response(),
        ClientProtocol::OpenAi => (
            StatusCode::NOT_FOUND,
            axum::Json(json!({
                "error": {
                    "message": format!("No available channel for model '{raw_model}'"),
                    "type": "invalid_request_error",
                    "code": "model_not_found"
                }
            })),
        )
            .into_response(),
    }
}

/// 兼容性校验失败的错误响应体（同样按客户端协议区分形状）
fn incompatible_model_response(err_msg: String, style: ClientProtocol) -> Response {
    match style {
        ClientProtocol::Gemini => (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({
                "error": { "code": 400, "message": err_msg, "status": "INVALID_ARGUMENT" }
            })),
        )
            .into_response(),
        ClientProtocol::Anthropic => (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({
                "type": "error",
                "error": { "type": "invalid_request_error", "message": err_msg }
            })),
        )
            .into_response(),
        ClientProtocol::Responses => (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({
                "type": "error",
                "error": {
                    "type": "invalid_request_error",
                    "code": "unsupported_free_model",
                    "message": err_msg,
                    "param": null,
                    "request_id": null
                }
            })),
        )
            .into_response(),
        ClientProtocol::OpenAi => (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({
                "error": {
                    "message": err_msg,
                    "type": "invalid_request_error",
                    "code": "unsupported_free_model"
                }
            })),
        )
            .into_response(),
    }
}

/// 重试耗尽/网络错误的合成错误体（按客户端协议成形）。
/// 与 `model_not_found_response`/`incompatible_model_response` 共用一套形状。
pub fn gateway_error_response(
    style: ClientProtocol,
    status: StatusCode,
    code: &str,
    message: String,
) -> Response {
    let body = match style {
        ClientProtocol::Gemini => json!({
            "error": { "code": status.as_u16(), "message": message, "status": "UNAVAILABLE" }
        }),
        ClientProtocol::Anthropic => json!({
            "type": "error",
            "error": { "type": "api_error", "message": message }
        }),
        ClientProtocol::Responses => json!({
            "type": "error",
            "error": {
                "type": "api_error",
                "code": code,
                "message": message,
                "param": null,
                "request_id": null
            }
        }),
        ClientProtocol::OpenAi => json!({
            "error": {
                "message": message,
                "type": "upstream_error",
                "code": code,
                "status": "UNAVAILABLE"
            }
        }),
    };
    (status, axum::Json(body)).into_response()
}

/// 渠道解析：失败时记录 404 日志并返回对应协议错误体
pub async fn resolve_channel_or_404<'a>(
    ctx: &ModelProxyContext,
    config: &'a ModelProxyConfig,
    raw_model: &str,
    path: &str,
    req_id: &str,
    is_stream: bool,
    start_time: Instant,
    req_body_str: &Option<String>,
    style: ClientProtocol,
) -> Result<(&'a ChannelConfig, String), Response> {
    match resolve_channel(config, raw_model) {
        Some(pair) => Ok(pair),
        None => {
            let dur = start_time.elapsed().as_millis() as u64;
            // 404 无法归属到具体渠道，沿用既有惯例计入 opencode 通道（含其统计 ID）
            let opencode_stats_id = config
                .channels
                .iter()
                .find(|c| c.id == "opencode")
                .and_then(|c| c.stats_id)
                .map(|v| v.to_string());
            record_attempt_failure(
                ctx,
                ProxyLogParams::new_failure(
                    req_id.to_string(),
                    path.to_string(),
                    "opencode".to_string(),
                    raw_model.to_string(),
                    is_stream,
                    404,
                    dur,
                    Some(format!("未找到支持模型 '{raw_model}' 的可用渠道")),
                    req_body_str.clone(),
                    None,
                )
                .with_channel_stats_id(opencode_stats_id),
            )
            .await;
            Err(model_not_found_response(raw_model, style))
        }
    }
}

/// 统一校验渠道与模型兼容性，未通过时记录日志并返回对应协议错误响应
pub async fn validate_model_channel_request(
    ctx: &ModelProxyContext,
    channel: &ChannelConfig,
    model_to_send: &str,
    raw_model: &str,
    channel_api_key: &str,
    path: &str,
    style: ClientProtocol,
    req_id: &str,
    is_stream: bool,
    start_time: Instant,
    req_body_str: &Option<String>,
) -> Result<(), Response> {
    if let Err(err_msg) = check_model_channel_compatibility(channel, model_to_send, channel_api_key)
    {
        let dur = start_time.elapsed().as_millis() as u64;
        record_attempt_failure(
            ctx,
            ProxyLogParams::new_failure(
                req_id.to_string(),
                path.to_string(),
                channel.effective_alias(),
                raw_model.to_string(),
                is_stream,
                400,
                dur,
                Some(err_msg.clone()),
                req_body_str.clone(),
                None,
            )
            .with_channel_stats_id(channel.stats_id.map(|v| v.to_string())),
        )
        .await;
        return Err(incompatible_model_response(err_msg, style));
    }
    Ok(())
}

/// 一次成功出网的全部产物
pub struct EgressOutcome {
    pub success: EgressSuccess,
    pub chan_alias: String,
    /// 统计维度稳定数字 ID，随日志落库
    pub chan_stats_id: Option<u32>,
    pub target: TargetProtocol,
    pub model_to_send: String,
}

impl EgressOutcome {
    /// 公共日志骨架：各协议入口在此基础上补全 token / 响应正文等字段
    pub fn base_log(
        &self,
        path: &str,
        raw_model: &str,
        is_stream: bool,
        req_body_str: Option<String>,
    ) -> ProxyRequestLog {
        ProxyRequestLog {
            id: self.success.attempt_req_id.clone(),
            timestamp: current_timestamp(),
            method: "POST".to_string(),
            path: path.to_string(),
            channel_id: self.chan_alias.clone(),
            channel_stats_id: self.chan_stats_id.map(|v| v.to_string()),
            model: raw_model.to_string(),
            stream: is_stream,
            status_code: self.success.status.as_u16(),
            duration_ms: self.success.cand_start.elapsed().as_millis() as u64,
            ttft_ms: None,
            prompt_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
            cache_creation_tokens: None,
            completion_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            error_message: None,
            request_body: req_body_str,
            response_body: None,
            node_name: Some(self.success.node_display.clone()),
            upstream_url: Some(self.success.upstream_url.clone()),
            client_name: None,
        }
    }
}

/// 兼容性校验 → 出网准备（含同协议快速通道）→ 弹性调度。
///
/// `convert = false` 表示传入的 body 已是目标协议原生格式（同协议快速通道），
/// 仅构建 URL 不做请求体转换。
#[allow(clippy::too_many_arguments)]
pub async fn dispatch_protocol_egress(
    ctx: &ModelProxyContext,
    config: &ModelProxyConfig,
    channel: &ChannelConfig,
    model_to_send: &str,
    raw_model: &str,
    path: &str,
    req_id: &str,
    is_stream: bool,
    start_time: Instant,
    req_body_str: &Option<String>,
    egress_payload: egress::EgressBody,
    style: ClientProtocol,
) -> Result<EgressOutcome, Response> {
    let chan_alias = channel.effective_alias();
    let chan_stats_id = channel.stats_id;

    // 解析出按分组优先级排列的候选 Key 队列：Vec<Vec<String>>
    // 外层为分组（优先级顺序），内层为该组内支持该模型的可用 Key
    let key_groups = resolve_channel_key_groups_for_model(ctx, channel, model_to_send).await;

    // 若无配置任何可用 Key，构建一个单次尝试队列（包含一个空 Key，用于免 Key 渠道）
    let candidate_groups: Vec<Vec<String>> = if key_groups.is_empty() {
        vec![vec![select_channel_api_key(ctx, channel).await]]
    } else {
        key_groups
    };

    let target = TargetProtocol::from_channel(channel);
    let mut last_error_response: Option<Response> = None;

    // 外层循环：分组优先级队列遍历（组间故障转移 Failover）
    for (group_idx, group_keys) in candidate_groups.iter().enumerate() {
        if group_keys.is_empty() {
            continue;
        }

        // 组内轮询：根据原子计数器在组内可用 Key 间均匀 Round-Robin 选取起始 Key
        let start_key_idx = ctx.key_round_robin.fetch_add(1, Ordering::Relaxed) % group_keys.len();
        let selected_key = &group_keys[start_key_idx];

        if let Err(err_resp) = validate_model_channel_request(
            ctx,
            channel,
            model_to_send,
            raw_model,
            selected_key,
            path,
            style,
            req_id,
            is_stream,
            start_time,
            req_body_str,
        )
        .await
        {
            last_error_response = Some(err_resp);
            continue;
        }

        let (upstream_url, egress_body) = egress::prepare_egress_with(
            channel,
            selected_key,
            model_to_send,
            egress_payload.clone(),
            is_stream,
        );

        let group_req_id = if group_idx == 0 {
            req_id.to_string()
        } else {
            format!("{req_id}-g{}", group_idx + 1)
        };

        let meta = EgressRequestMeta {
            req_id: group_req_id,
            path: path.to_string(),
            channel_id: chan_alias.clone(),
            channel_stats_id: chan_stats_id.map(|v| v.to_string()),
            model: raw_model.to_string(),
            stream: is_stream,
            req_body_str: req_body_str.clone(),
        };

        match execute_resilient_egress(
            ctx,
            channel,
            config,
            meta,
            &upstream_url,
            selected_key,
            &egress_body,
            style,
        )
        .await
        {
            Ok(success) => {
                return Ok(EgressOutcome {
                    success,
                    chan_alias,
                    chan_stats_id,
                    target,
                    model_to_send: model_to_send.to_string(),
                });
            }
            Err(err_resp) => {
                // 当前分组请求失败（例如 401 鉴权失败、429 频次限制或上游故障），记录错误并尝试切换到下一个优先级分组
                tracing::warn!(
                    "[ModelGateway] 渠道「{}」第 {} 分组请求模型「{}」失败，自动尝试下一优先级分组...",
                    chan_alias,
                    group_idx + 1,
                    model_to_send
                );
                last_error_response = Some(err_resp);
            }
        }
    }

    // 所有分组均尝试失败，返回最后一个分组的错误响应（或兜底 502）
    Err(last_error_response.unwrap_or_else(|| {
        gateway_error_response(
            style,
            StatusCode::BAD_GATEWAY,
            "UPSTREAM_UNAVAILABLE",
            format!("渠道「{chan_alias}」的所有可用 Key 与分组均请求失败"),
        )
    }))
}

/// 鉴权失败 + 总请求数计数的公共入口封装；失败时已记录日志并返回错误响应
pub async fn auth_and_count(
    ctx: &ModelProxyContext,
    headers: &axum::http::HeaderMap,
    uri: &axum::http::Uri,
    config: &ModelProxyConfig,
    req_id: &str,
    path: &str,
    raw_model: &str,
    is_stream: bool,
    start_time: Instant,
    req_body_str: &Option<String>,
) -> Result<(), Response> {
    if !ctx.route_enabled.load(Ordering::Acquire) {
        return Err(super::router::gateway_disabled_response());
    }
    if let Err(res) = check_auth(headers, uri, config).await {
        super::logger::record_auth_failure_log(
            ctx,
            req_id,
            path,
            raw_model,
            is_stream,
            start_time.elapsed().as_millis() as u64,
            req_body_str.clone(),
            Some(client_name_from_headers(headers, path)),
        )
        .await;
        return Err(res);
    }
    ctx.metrics.total_requests.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

#[cfg(test)]
mod pipeline_tests {
    use super::*;

    #[tokio::test]
    async fn responses_error_body_uses_responses_shape() {
        // P1-5：Responses 客户端收到的不再是 OpenAI Chat 形状错误体
        let resp = model_not_found_response("no-such-model", ClientProtocol::Responses);
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let bytes = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let jv: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(jv["type"], "error");
        assert_eq!(
            jv.pointer("/error/code")
                .and_then(serde_json::Value::as_str),
            Some("model_not_found")
        );
        assert!(
            jv.pointer("/error/param").is_some(),
            "Responses 形状带 param 字段"
        );
        assert!(jv.pointer("/error/request_id").is_some());
    }

    #[tokio::test]
    async fn gateway_error_respects_client_protocol_shape() {
        // P1-5：重试耗尽/网络错误的合成错误体按客户端协议成形
        let resp = gateway_error_response(
            ClientProtocol::Anthropic,
            StatusCode::BAD_GATEWAY,
            "502",
            "boom".into(),
        );
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let bytes = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let jv: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(jv["type"], "error");
        assert_eq!(
            jv.pointer("/error/type")
                .and_then(serde_json::Value::as_str),
            Some("api_error")
        );

        let resp = gateway_error_response(
            ClientProtocol::Responses,
            StatusCode::BAD_GATEWAY,
            "502",
            "boom".into(),
        );
        let bytes = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let jv: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(jv["type"], "error");
        assert!(jv.pointer("/error/param").is_some());

        let resp = gateway_error_response(
            ClientProtocol::Gemini,
            StatusCode::BAD_GATEWAY,
            "502",
            "boom".into(),
        );
        let bytes = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let jv: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            jv.pointer("/error/code").is_some(),
            "Gemini 形状带 code 字段"
        );
    }
}
