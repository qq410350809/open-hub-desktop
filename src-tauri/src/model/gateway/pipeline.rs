//! 协议处理器公共流水线。
//!
//! 四个协议入口（OpenAI Chat / Responses / Anthropic Messages / Gemini）共享同一段骨架：
//! 渠道解析(404) → 模型兼容性校验(400) → 出网准备(目标协议) → 弹性调度 → 公共日志骨架。
//! 各入口文件只负责：入参解析、客户端协议 ↔ OpenAI 中枢转换、响应回转。

use super::balancer::{check_model_channel_compatibility, resolve_channel, select_channel_api_key};
use super::dispatcher::{execute_resilient_egress, EgressRequestMeta, EgressSuccess};
use super::egress::{self, TargetProtocol};
use super::logger::{client_name_from_headers, record_attempt_failure, ProxyLogParams};
use super::router::check_auth;
use super::types::{
    current_timestamp, ChannelConfig, ModelProxyConfig, ModelProxyContext, ProxyRequestLog,
};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value as JsonValue};
use std::sync::atomic::Ordering;
use std::time::Instant;

/// 客户端入口协议，决定 404/400 错误体的 JSON 形状
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientProtocol {
    OpenAi,
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
    if let Err(err_msg) = check_model_channel_compatibility(channel, model_to_send, channel_api_key) {
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
    egress_body: &JsonValue,
    convert: bool,
    style: ClientProtocol,
) -> Result<EgressOutcome, Response> {
    let channel_api_key = select_channel_api_key(ctx, channel);
    let chan_alias = channel.effective_alias();
    let chan_stats_id = channel.stats_id;

    if let Err(err_resp) = validate_model_channel_request(
        ctx,
        channel,
        model_to_send,
        raw_model,
        &channel_api_key,
        path,
        style,
        req_id,
        is_stream,
        start_time,
        req_body_str,
    )
    .await
    {
        return Err(err_resp);
    }

    let target = TargetProtocol::from_channel(channel);
    let (upstream_url, egress_body) = egress::prepare_egress_with(
        channel,
        &channel_api_key,
        model_to_send,
        egress_body,
        is_stream,
        convert,
    );

    let meta = EgressRequestMeta {
        req_id: req_id.to_string(),
        path: path.to_string(),
        channel_id: chan_alias.clone(),
        channel_stats_id: chan_stats_id.map(|v| v.to_string()),
        model: raw_model.to_string(),
        stream: is_stream,
        req_body_str: req_body_str.clone(),
    };

    let success = match execute_resilient_egress(
        ctx,
        channel,
        config,
        meta,
        &upstream_url,
        &channel_api_key,
        &egress_body,
    )
    .await
    {
        Ok(s) => s,
        Err(err_resp) => return Err(err_resp),
    };

    Ok(EgressOutcome {
        success,
        chan_alias,
        chan_stats_id,
        target,
        model_to_send: model_to_send.to_string(),
    })
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
