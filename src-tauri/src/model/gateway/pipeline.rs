//! 协议处理器公共流水线。
//!
//! 四个协议入口（OpenAI Chat / Responses / Anthropic Messages / Gemini）共享同一段骨架：
//! 渠道解析(404) → 模型兼容性校验(400) → 出网准备(目标协议) → 弹性调度 → 公共日志骨架。
//! 各入口文件只负责：入参解析、客户端协议 ↔ OpenAI 中枢转换、响应回转。

use super::balancer::{
    load_channel_all_keys_info, resolve_channel_candidates, resolve_channel_detailed,
    resolve_channel_key_groups_for_model, ChannelResolution, ResolvedKeyGroup,
};
use super::dispatcher::{
    execute_resilient_egress, parse_retry_after_value, rate_limit_backoff_ms, EgressRequestMeta,
    EgressSuccess,
};
use super::egress::{self, TargetProtocol};
use super::logger::{
    client_name_from_headers, record_attempt_failure, user_agent_from_headers, ProxyLogParams,
};
use super::policies::opencode::check_model_channel_compatibility;
use super::router::check_auth;
use super::types::{
    current_timestamp, ChannelConfig, ModelProxyConfig, ModelProxyContext, ProxyRequestLog,
};
use axum::http::{header::RETRY_AFTER, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// 单个分组在单个请求内最多顺延尝试的 Key 数量上限。
/// 组内会逐个 Key 故障转移，若组内有大量失效 Key，叠加上游超时会把单请求拖得过长。
const MAX_ATTEMPTS_PER_GROUP: usize = 8;

/// 尝试队列中的一个候选：待用的 Key + 其所属分组名（仅用于日志）
#[derive(Debug, PartialEq, Eq)]
pub struct KeyAttempt {
    pub key: String,
    pub group_label: String,
}

/// 把「分组优先级队列」展平为单个请求内的有序 Key 尝试队列。
///
/// 两种模式都会在组内**顺延全部可用 Key**（至多 `MAX_ATTEMPTS_PER_GROUP` 个），
/// 区别只在起点：
/// - 轮询组（round_robin）：起点由该组游标 `next_start` 逐请求推进，组内 Key 分摊调用量；
///   当前 Key 失败后接着试组内下一个（旋转序），而不是直接跳组；
/// - 独立组（independent）：恒从组内首个 Key 起，黏住首 Key，失败才顺延。
///
/// 队列按分组顺序拼接：同组 Key 全部失败才进入下一组；全队列耗尽即视为该渠道失败。
pub fn build_key_attempt_queue(
    groups: &[ResolvedKeyGroup],
    mut next_start: impl FnMut(&ResolvedKeyGroup) -> usize,
    chan_alias: &str,
) -> Vec<KeyAttempt> {
    let mut queue: Vec<KeyAttempt> = Vec::new();
    for group in groups {
        if group.keys.is_empty() {
            continue;
        }
        let len = group.keys.len();
        let start = if group.is_independent() {
            0
        } else {
            next_start(group) % len
        };
        let take = len.min(MAX_ATTEMPTS_PER_GROUP);
        if take < len {
            tracing::warn!(
                "[ModelGateway] 渠道「{}」分组「{}」共 {} 个 Key，本次请求最多尝试 {} 个",
                chan_alias,
                group.name,
                len,
                take
            );
        }
        for offset in 0..take {
            queue.push(KeyAttempt {
                key: group.keys[(start + offset) % len].clone(),
                group_label: group.name.clone(),
            });
        }
    }
    queue
}

/// 单个 Key 失败后是否值得换下一个 Key 再试。
///
/// 只有「与这把 Key 本身相关」的失败才换 Key：鉴权/配额/限流（401/402/403/429）、
/// 上游或网络故障（5xx、连接失败合成的 502）、超时（408）。
/// 429 先顺延同组其他 Key，再试本渠道其余外放该模型的 Key；被限流的 Key 进入冷却，
/// 后续请求跳过它，避免同一把 Key 被客户端连打。
/// 400/404/413/422 这类由请求内容决定的错误换 Key 结果不会变，
/// 逐 Key 重放只会白烧配额、拖长响应，应立即返回。
pub fn should_failover_to_next_key(status: StatusCode) -> bool {
    status.is_server_error()
        || matches!(
            status,
            StatusCode::UNAUTHORIZED
                | StatusCode::PAYMENT_REQUIRED
                | StatusCode::FORBIDDEN
                | StatusCode::REQUEST_TIMEOUT
                | StatusCode::TOO_MANY_REQUESTS
        )
}

fn attach_retry_after(resp: &mut Response, wait_ms: u64) {
    let secs = wait_ms.div_ceil(1000).max(1);
    if let Ok(value) = HeaderValue::from_str(&secs.to_string()) {
        resp.headers_mut().insert(RETRY_AFTER, value);
    }
}

fn retry_after_ms_from_response(resp: &Response) -> Option<u64> {
    parse_retry_after_value(resp.headers().get("retry-after")?.to_str().ok()?)
}

fn rate_limited_cooldown_response(style: ClientProtocol, remaining: Duration) -> Response {
    let wait_ms = remaining.as_millis() as u64;
    let secs = wait_ms.div_ceil(1000).max(1);
    let mut resp = gateway_error_response(
        style,
        StatusCode::TOO_MANY_REQUESTS,
        "RATE_LIMITED",
        format!("上游频次受限，冷却中，请 {secs} 秒后再试"),
    );
    attach_retry_after(&mut resp, wait_ms);
    resp
}

/// 逐候选累积错误响应，最终挑给客户端看的那一个：优先第一个非 401 的错误。
/// 首个 Key 的 429/5xx 才是请求失败的真实原因，不该被后面某把过期 Key 的 401 覆盖；
/// 全部都是 401 时返回最后一个。
#[derive(Default)]
pub struct ErrorTrail {
    first_non_auth: Option<Response>,
    last: Option<Response>,
}

impl ErrorTrail {
    pub fn push(&mut self, resp: Response) {
        if self.first_non_auth.is_none() && resp.status() != StatusCode::UNAUTHORIZED {
            self.first_non_auth = Some(resp);
        } else {
            self.last = Some(resp);
        }
    }

    pub fn finish(self) -> Option<Response> {
        self.first_non_auth.or(self.last)
    }
}

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
#[allow(dead_code)]
pub fn model_not_found_response(raw_model: &str, style: ClientProtocol) -> Response {
    model_not_found_response_with_message(
        format!("No available channel for model '{raw_model}'"),
        style,
    )
}

/// 自定义信息的 404 响应体（如定向渠道已禁用）
pub fn model_not_found_response_with_message(message: String, style: ClientProtocol) -> Response {
    match style {
        ClientProtocol::Gemini => (
            StatusCode::NOT_FOUND,
            axum::Json(json!({
                "error": {
                    "code": 404,
                    "message": message,
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
                    "message": message
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
                    "message": message,
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
                    "message": message,
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

/// 渠道解析：失败时记录 404 日志并返回对应协议错误体。
/// 定向指派（`alias/model`）命中已禁用渠道时同样 404，错误信息写明渠道已禁用 ——
/// 绝不兜底到其他渠道。
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
    client_name: Option<String>,
    user_agent: Option<String>,
) -> Result<(&'a ChannelConfig, String), Response> {
    // (日志归属渠道别名, 统计 ID, 错误信息)
    let (log_channel, log_stats_id, err_msg) = match resolve_channel_detailed(config, raw_model) {
        ChannelResolution::Resolved(ch, model) => return Ok((ch, model)),
        ChannelResolution::Disabled(ch, bare) => (
            ch.effective_alias(),
            ch.stats_id.map(|v| v.to_string()),
            format!(
                "渠道「{}」已禁用，模型 '{}' 的定向请求不会转发到其他渠道",
                ch.effective_alias(),
                bare
            ),
        ),
        ChannelResolution::NotFound => (
            // 404 无法归属到具体渠道，沿用既有惯例计入 opencode 通道（含其统计 ID）
            "opencode".to_string(),
            config
                .channels
                .iter()
                .find(|c| c.id == "opencode")
                .and_then(|c| c.stats_id)
                .map(|v| v.to_string()),
            format!("未找到支持模型 '{raw_model}' 的可用渠道"),
        ),
    };
    let dur = start_time.elapsed().as_millis() as u64;
    record_attempt_failure(
        ctx,
        ProxyLogParams::new_failure(
            req_id.to_string(),
            path.to_string(),
            log_channel,
            raw_model.to_string(),
            is_stream,
            404,
            dur,
            Some(err_msg.clone()),
            req_body_str.clone(),
            None,
        )
        .with_channel_stats_id(log_stats_id)
        .with_client_name(client_name)
        .with_user_agent(user_agent),
    )
    .await;
    Err(model_not_found_response_with_message(err_msg, style))
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
    client_name: Option<String>,
    user_agent: Option<String>,
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
            .with_channel_stats_id(channel.stats_id.map(|v| v.to_string()))
            .with_client_name(client_name)
            .with_user_agent(user_agent),
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
            session_id: None,
            client_name: None,
            user_agent: None,
        }
    }
}

/// 单个渠道内的完整尝试：兼容性校验 → 出网准备（含同协议快速通道）→ 弹性调度。
///
/// 渠道内的 Key 分组与出口节点全部耗尽后返回 `Err`；跨渠道故障转移由外层
/// `dispatch_protocol_egress` 负责。渠道有 Key 但无一支持该模型时同样返回
/// `Err`（越权拦截：绝不回退使用与模型不匹配的 Key）。
#[allow(clippy::too_many_arguments)]
async fn dispatch_single_channel_egress(
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
    client_name: Option<String>,
    user_agent: Option<String>,
) -> Result<EgressOutcome, Response> {
    let chan_alias = channel.effective_alias();
    let chan_stats_id = channel.stats_id;

    // 解析出按分组优先级排列的候选 Key 分组（分组身份 = Key 的 groupName）
    let key_groups = resolve_channel_key_groups_for_model(ctx, channel, model_to_send).await;

    let attempts: Vec<KeyAttempt> = if key_groups.is_empty() {
        // 空分组有两种成因，必须区分处理：
        // ① 渠道一个 Key 都没有（免 Key 渠道，如 opencode）：按原样空 Key 单次尝试；
        // ② 渠道有 Key，但没有任何 Key 声明支持该模型（或 Key/分组全部被禁用）：
        //    此时改用任意其他 Key 出网即构成越权 —— 该 Key 并未获得此模型的访问授权。
        //    必须判该渠道无候选并跳过，交由上层跨渠道故障转移，
        //    让真正有 Key 支持该模型的渠道按「分组顺序 + 组内轮询模式」编排。
        let all_keys = load_channel_all_keys_info(ctx, channel).await;
        if !all_keys.is_empty() {
            let err_msg = if all_keys.iter().all(|k| !k.enabled) {
                format!("渠道「{chan_alias}」的所有 API Key 均被禁用，已跳过该渠道")
            } else {
                format!(
                    "渠道「{chan_alias}」没有任何 API Key 支持模型「{model_to_send}」，已跳过该渠道（不越权使用其他 Key）"
                )
            };
            record_attempt_failure(
                ctx,
                ProxyLogParams::new_failure(
                    req_id.to_string(),
                    path.to_string(),
                    chan_alias.clone(),
                    raw_model.to_string(),
                    is_stream,
                    400,
                    start_time.elapsed().as_millis() as u64,
                    Some(err_msg.clone()),
                    req_body_str.clone(),
                    None,
                )
                .with_channel_stats_id(chan_stats_id.map(|v| v.to_string()))
                .with_client_name(client_name.clone())
                .with_user_agent(user_agent.clone()),
            )
            .await;
            return Err(incompatible_model_response(err_msg, style));
        }
        vec![KeyAttempt {
            key: String::new(),
            group_label: String::new(),
        }]
    } else {
        // 轮询组的起点游标按「渠道 + 分组」分片，建队前先取齐（建队本身是同步的）
        let mut cursors: HashMap<String, std::sync::Arc<std::sync::atomic::AtomicUsize>> =
            HashMap::new();
        for group in key_groups.iter().filter(|g| !g.is_independent()) {
            let cursor = ctx.key_round_robin_for(&channel.id, &group.id).await;
            cursors.insert(group.id.clone(), cursor);
        }
        build_key_attempt_queue(
            &key_groups,
            |group| {
                cursors
                    .get(&group.id)
                    .map(|c| c.fetch_add(1, Ordering::Relaxed))
                    .unwrap_or(0)
            },
            &chan_alias,
        )
    };

    // 出网协议：模型级覆盖优先（响应回转嗅探也按该协议归一）
    let target = channel.target_protocol_for(model_to_send);
    let mut errors = ErrorTrail::default();
    let candidate_keys: Vec<String> = attempts.iter().map(|a| a.key.clone()).collect();
    if let Some(remaining) = ctx
        .rate_limit_all_cooling(&channel.id, model_to_send, &candidate_keys)
        .await
    {
        tracing::warn!(
            "[ModelGateway] 渠道「{}」模型「{}」的候选 Key 均在 429 冷却（剩余 {}ms），本次不请求上游",
            chan_alias,
            model_to_send,
            remaining.as_millis()
        );
        return Err(rate_limited_cooldown_response(style, remaining));
    }

    // 顺序遍历尝试队列：同组顺延 → 下一组，均已在建队时铺平
    for (attempt_idx, attempt) in attempts.iter().enumerate() {
        let selected_key = &attempt.key;

        if let Some(remaining) = ctx
            .rate_limit_remaining(&channel.id, model_to_send, selected_key)
            .await
        {
            tracing::warn!(
                "[ModelGateway] 渠道「{}」分组「{}」模型「{}」当前 Key 仍在 429 冷却（剩余 {}ms），跳过",
                chan_alias,
                attempt.group_label,
                model_to_send,
                remaining.as_millis()
            );
            continue;
        }

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
            client_name.clone(),
            user_agent.clone(),
        )
        .await
        {
            errors.push(err_resp);
            continue;
        }

        let (upstream_url, egress_body) = egress::prepare_egress_with(
            channel,
            selected_key,
            model_to_send,
            egress_payload.clone(),
            is_stream,
        );

        let group_req_id = if attempt_idx == 0 {
            req_id.to_string()
        } else {
            format!("{req_id}-g{}", attempt_idx + 1)
        };

        let meta = EgressRequestMeta {
            req_id: group_req_id,
            path: path.to_string(),
            channel_id: chan_alias.clone(),
            channel_stats_id: chan_stats_id.map(|v| v.to_string()),
            model: raw_model.to_string(),
            rule_model: model_to_send.to_string(),
            stream: is_stream,
            req_body_str: req_body_str.clone(),
            client_name: client_name.clone(),
            user_agent: user_agent.clone(),
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
                ctx.clear_rate_limit(&channel.id, model_to_send, selected_key)
                    .await;
                return Ok(EgressOutcome {
                    success,
                    chan_alias,
                    chan_stats_id,
                    target,
                    model_to_send: model_to_send.to_string(),
                });
            }
            Err(err_resp) => {
                let status = err_resp.status();
                let mut err_resp = err_resp;
                if status == StatusCode::TOO_MANY_REQUESTS {
                    let retry_after_ms = retry_after_ms_from_response(&err_resp);
                    let consecutive = ctx
                        .rate_limit_consecutive(&channel.id, model_to_send, selected_key)
                        .await
                        .saturating_add(1);
                    let backoff_ms = rate_limit_backoff_ms(consecutive, retry_after_ms);
                    ctx.mark_rate_limited(&channel.id, model_to_send, selected_key, backoff_ms)
                        .await;
                    attach_retry_after(&mut err_resp, backoff_ms);
                    tracing::warn!(
                        "[ModelGateway] 渠道「{}」分组「{}」模型「{}」当前 Key 返回 429，冷却 {}ms 后顺延下一把",
                        chan_alias,
                        attempt.group_label,
                        model_to_send,
                        backoff_ms
                    );
                }
                // 与请求内容相关的确定性错误（400/404/413/422…）换 Key 也不会变，
                // 立即返回，不再逐 Key / 逐组重放
                if !should_failover_to_next_key(status) {
                    tracing::warn!(
                        "[ModelGateway] 渠道「{}」分组「{}」请求模型「{}」返回 {}，属请求侧错误，不再尝试其他 Key",
                        chan_alias,
                        attempt.group_label,
                        model_to_send,
                        status
                    );
                    return Err(err_resp);
                }
                // 当前 Key 失败（401/403/429/5xx/网络），顺延到队列中的下一个候选 Key
                // （同组下一个 Key，同组耗尽后进入下一个分组）
                if attempt_idx + 1 < attempts.len() {
                    tracing::warn!(
                        "[ModelGateway] 渠道「{}」分组「{}」第 {} 次候选请求模型「{}」返回 {}，自动尝试下一候选 Key...",
                        chan_alias,
                        attempt.group_label,
                        attempt_idx + 1,
                        model_to_send,
                        status
                    );
                }
                errors.push(err_resp);
            }
        }
    }

    // 所有分组均尝试失败：返回最能说明失败原因的那次错误响应。
    // 若一把都没出网（候选 Key 全在冷却中被跳过），回 429 而不是 502。
    if let Some(resp) = errors.finish() {
        return Err(resp);
    }
    if let Some(remaining) = ctx
        .rate_limit_all_cooling(&channel.id, model_to_send, &candidate_keys)
        .await
    {
        return Err(rate_limited_cooldown_response(style, remaining));
    }
    Err(gateway_error_response(
        style,
        StatusCode::BAD_GATEWAY,
        "UPSTREAM_UNAVAILABLE",
        format!("渠道「{chan_alias}」的所有可用 Key 与分组均请求失败"),
    ))
}

/// 跨渠道故障转移上限：单个请求最多尝试的渠道数。
/// 每个渠道内部已有 Key 分组 × 出口节点两层重试，叠加过多渠道会把单请求拖到不可接受的时长。
const MAX_CHANNEL_FAILOVER: usize = 3;

/// 出网调度入口：默认只在 handler 选定的渠道上完成「同组顺延 → 下一组」的 Key 故障转移，
/// 渠道耗尽即直接返回错误。
///
/// 仅当 `config.channel_failover` 开启且请求为裸模型名（非 `alias/model` 定向指派）时，
/// 才在渠道耗尽后切换到下一个同样声明提供该模型的渠道（候选见 `resolve_channel_candidates`）。
///
/// 返回的 `EgressOutcome.chan_alias` 是**实际成功**的渠道，日志与统计因此归属正确。
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
    client_name: Option<String>,
    user_agent: Option<String>,
) -> Result<EgressOutcome, Response> {
    // 候选列表：定向请求最多只有被指派的渠道自身，裸模型名才可能有后备渠道
    let fallbacks: Vec<&ChannelConfig> = if config.channel_failover {
        resolve_channel_candidates(config, raw_model)
            .iter()
            .map(|(ch, _)| *ch)
            .filter(|ch| ch.id != channel.id)
            .take(MAX_CHANNEL_FAILOVER.saturating_sub(1))
            .collect()
    } else {
        Vec::new()
    };

    let mut chain: Vec<&ChannelConfig> = vec![channel];
    chain.extend(fallbacks);
    let total = chain.len();

    let mut errors = ErrorTrail::default();
    for (idx, cand) in chain.into_iter().enumerate() {
        // 后备渠道的请求 ID 加后缀，与首选渠道的日志区分开
        let chan_req_id = if idx == 0 {
            req_id.to_string()
        } else {
            format!("{req_id}-c{}", idx + 1)
        };

        match dispatch_single_channel_egress(
            ctx,
            config,
            cand,
            model_to_send,
            raw_model,
            path,
            &chan_req_id,
            is_stream,
            start_time,
            req_body_str,
            egress_payload.clone(),
            style,
            client_name.clone(),
            user_agent.clone(),
        )
        .await
        {
            Ok(outcome) => {
                if idx > 0 {
                    tracing::info!(
                        "[ModelGateway] 模型「{}」经跨渠道故障转移由渠道「{}」成功承接（第 {}/{} 个候选）",
                        model_to_send,
                        cand.effective_alias(),
                        idx + 1,
                        total
                    );
                }
                return Ok(outcome);
            }
            Err(err_resp) => {
                if idx + 1 < total {
                    tracing::warn!(
                        "[ModelGateway] 渠道「{}」全部候选耗尽，模型「{}」转移到下一渠道（第 {}/{} 个候选）...",
                        cand.effective_alias(),
                        model_to_send,
                        idx + 2,
                        total
                    );
                }
                errors.push(err_resp);
            }
        }
    }

    // 全部候选渠道均失败：返回最能说明失败原因的错误响应
    Err(errors.finish().unwrap_or_else(|| {
        gateway_error_response(
            style,
            StatusCode::BAD_GATEWAY,
            "UPSTREAM_UNAVAILABLE",
            format!("模型「{model_to_send}」的全部候选渠道均请求失败"),
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
            user_agent_from_headers(headers),
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
    use crate::model::gateway::types::{KEY_GROUP_MODE_INDEPENDENT, KEY_GROUP_MODE_ROUND_ROBIN};
    use std::sync::atomic::AtomicUsize;

    fn group(id: &str, mode: &str, keys: &[&str]) -> ResolvedKeyGroup {
        ResolvedKeyGroup {
            id: id.to_string(),
            name: id.to_string(),
            mode: mode.to_string(),
            keys: keys.iter().map(|k| k.to_string()).collect(),
        }
    }

    /// 用一个原子计数器模拟单组游标
    fn next_from(counter: &AtomicUsize) -> impl FnMut(&ResolvedKeyGroup) -> usize + '_ {
        move |_| counter.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn round_robin_group_rotates_start_and_walks_whole_group() {
        // 轮询组：起点随游标逐请求轮转，但队列包含组内全部 Key（旋转序），
        // 当前 Key 失败后顺延同组下一个而不是直接跳组
        let groups = vec![group("g1", KEY_GROUP_MODE_ROUND_ROBIN, &["k1", "k2", "k3"])];
        let counter = AtomicUsize::new(0);

        let picked: Vec<Vec<String>> = (0..4)
            .map(|_| {
                build_key_attempt_queue(&groups, next_from(&counter), "ch")
                    .into_iter()
                    .map(|a| a.key)
                    .collect()
            })
            .collect();

        assert_eq!(picked[0], vec!["k1", "k2", "k3"]);
        assert_eq!(picked[1], vec!["k2", "k3", "k1"]);
        assert_eq!(picked[2], vec!["k3", "k1", "k2"]);
        assert_eq!(picked[3], vec!["k1", "k2", "k3"], "回绕");
    }

    #[test]
    fn round_robin_cursor_is_per_group_not_shared() {
        // 两个各 2 Key 的轮询组：各自独立游标，每组每请求都能轮转
        // （旧实现共用一个全局计数器，一次请求前进 2，`% 2` 恒等 → 永远不轮转）
        let groups = vec![
            group("g1", KEY_GROUP_MODE_ROUND_ROBIN, &["a1", "a2"]),
            group("g2", KEY_GROUP_MODE_ROUND_ROBIN, &["b1", "b2"]),
        ];
        let mut cursors: HashMap<String, AtomicUsize> = HashMap::new();
        cursors.insert("g1".into(), AtomicUsize::new(0));
        cursors.insert("g2".into(), AtomicUsize::new(0));
        let mut next = |g: &ResolvedKeyGroup| cursors[&g.id].fetch_add(1, Ordering::Relaxed);

        let heads = |q: Vec<KeyAttempt>| (q[0].key.clone(), q[2].key.clone());
        assert_eq!(
            heads(build_key_attempt_queue(&groups, &mut next, "ch")),
            ("a1".to_string(), "b1".to_string())
        );
        assert_eq!(
            heads(build_key_attempt_queue(&groups, &mut next, "ch")),
            ("a2".to_string(), "b2".to_string()),
            "两组都应轮转到第二个 Key"
        );
    }

    #[test]
    fn independent_group_is_sticky_then_walks_group() {
        // 独立组：始终从组内第一个 Key 开始，失败才顺延组内后续 Key（顺序固定，不随请求轮转）
        let groups = vec![group("g1", KEY_GROUP_MODE_INDEPENDENT, &["k1", "k2", "k3"])];
        let counter = AtomicUsize::new(0);

        for _ in 0..3 {
            let q = build_key_attempt_queue(&groups, next_from(&counter), "ch");
            let keys: Vec<&str> = q.iter().map(|a| a.key.as_str()).collect();
            assert_eq!(keys, vec!["k1", "k2", "k3"], "黏住 k1 且组内顺序转移");
        }
        assert_eq!(counter.load(Ordering::Relaxed), 0, "独立组不消耗轮询游标");
    }

    #[test]
    fn mixed_groups_chain_in_priority_order() {
        // 混合：轮询组从游标起点铺开全组，独立组铺开组内全部候选，按分组顺序拼接
        let groups = vec![
            group("rr", KEY_GROUP_MODE_ROUND_ROBIN, &["a1", "a2"]),
            group("indep", KEY_GROUP_MODE_INDEPENDENT, &["b1", "b2"]),
        ];
        let counter = AtomicUsize::new(1);

        let q = build_key_attempt_queue(&groups, next_from(&counter), "ch");
        let keys: Vec<&str> = q.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(keys, vec!["a2", "a1", "b1", "b2"]);
        assert_eq!(q[0].group_label, "rr");
        assert_eq!(q[1].group_label, "rr");
        assert_eq!(q[2].group_label, "indep");
    }

    #[test]
    fn group_caps_attempts_per_request() {
        // 组内 Key 过多时截断，避免失效 Key 叠加上游超时把单请求拖爆
        let keys: Vec<String> = (0..MAX_ATTEMPTS_PER_GROUP + 5)
            .map(|i| format!("k{i}"))
            .collect();
        let groups = vec![ResolvedKeyGroup {
            id: "big".to_string(),
            name: "big".to_string(),
            mode: KEY_GROUP_MODE_INDEPENDENT.to_string(),
            keys: keys.clone(),
        }];
        let counter = AtomicUsize::new(0);

        let q = build_key_attempt_queue(&groups, next_from(&counter), "ch");
        assert_eq!(q.len(), MAX_ATTEMPTS_PER_GROUP);
        assert_eq!(q[0].key, "k0", "截断保留最靠前的 Key");
    }

    #[test]
    fn empty_groups_are_skipped() {
        let groups = vec![
            group("empty", KEY_GROUP_MODE_ROUND_ROBIN, &[]),
            group("real", KEY_GROUP_MODE_ROUND_ROBIN, &["k1"]),
        ];
        let counter = AtomicUsize::new(0);
        let q = build_key_attempt_queue(&groups, next_from(&counter), "ch");
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].key, "k1");
    }

    #[test]
    fn key_failover_only_for_key_related_statuses() {
        for s in [401u16, 402, 403, 408, 429, 500, 502, 503, 504] {
            assert!(
                should_failover_to_next_key(StatusCode::from_u16(s).unwrap()),
                "{s} 应换 Key"
            );
        }
        for s in [400u16, 404, 413, 415, 422] {
            assert!(
                !should_failover_to_next_key(StatusCode::from_u16(s).unwrap()),
                "{s} 不应换 Key"
            );
        }
    }

    #[test]
    fn error_trail_prefers_first_non_auth_error() {
        let mk = |s: StatusCode| (s, "x").into_response();
        // 首个 429 不该被后面的 401 覆盖
        let mut t = ErrorTrail::default();
        t.push(mk(StatusCode::TOO_MANY_REQUESTS));
        t.push(mk(StatusCode::UNAUTHORIZED));
        assert_eq!(t.finish().unwrap().status(), StatusCode::TOO_MANY_REQUESTS);
        // 首个是 401、后面出现 502：取 502
        let mut t = ErrorTrail::default();
        t.push(mk(StatusCode::UNAUTHORIZED));
        t.push(mk(StatusCode::BAD_GATEWAY));
        assert_eq!(t.finish().unwrap().status(), StatusCode::BAD_GATEWAY);
        // 全是 401：取最后一个（都一样）
        let mut t = ErrorTrail::default();
        t.push(mk(StatusCode::UNAUTHORIZED));
        t.push(mk(StatusCode::UNAUTHORIZED));
        assert_eq!(t.finish().unwrap().status(), StatusCode::UNAUTHORIZED);
        assert!(ErrorTrail::default().finish().is_none());
    }

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
