//! 协议处理器公共流水线。
//!
//! 四个协议入口（OpenAI Chat / Responses / Anthropic Messages / Gemini）共享同一段骨架：
//! 渠道解析(404) → 模型兼容性校验(400) → 出网准备(目标协议) → 弹性调度 → 公共日志骨架。
//! 各入口文件只负责：入参解析、客户端协议 ↔ OpenAI 中枢转换、响应回转。

use super::balancer::{
    resolve_channel, resolve_channel_candidates, resolve_channel_key_groups_for_model,
    select_channel_api_key, ResolvedKeyGroup,
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

/// 独立（黏性）分组在单个请求内最多顺延尝试的 Key 数量上限。
/// 独立组会在组内逐个 Key 故障转移，若组内有大量失效 Key，叠加上游超时会把单请求拖得过长。
const MAX_INDEPENDENT_ATTEMPTS_PER_GROUP: usize = 8;

/// 尝试队列中的一个候选：待用的 Key + 其所属分组名（仅用于日志）
#[derive(Debug, PartialEq, Eq)]
pub struct KeyAttempt {
    pub key: String,
    pub group_label: String,
}

/// 把「分组优先级队列」展平为单个请求内的有序 Key 尝试队列。
///
/// - 轮询组（round_robin）贡献 1 次尝试，Key 由全局计数器在组内轮转选出（逐请求分摊调用量）；
/// - 独立组（independent）贡献组内前若干个 Key 的有序尝试：黏住首个 Key，
///   仅在其失败时顺延到组内下一个，至多 `MAX_INDEPENDENT_ATTEMPTS_PER_GROUP` 个。
///
/// 队列按分组顺序拼接，耗尽即视为该渠道全部候选失败。
pub fn build_key_attempt_queue(
    groups: &[ResolvedKeyGroup],
    round_robin: &std::sync::atomic::AtomicUsize,
    chan_alias: &str,
) -> Vec<KeyAttempt> {
    let mut queue: Vec<KeyAttempt> = Vec::new();
    for group in groups {
        if group.keys.is_empty() {
            continue;
        }
        if group.is_independent() {
            // 独立组：组内顺序故障转移。限制单组最多尝试的 Key 数，避免大量失效 Key
            // 叠加上游超时把单个请求拖到不可接受的时长。
            let take = group.keys.len().min(MAX_INDEPENDENT_ATTEMPTS_PER_GROUP);
            if take < group.keys.len() {
                tracing::warn!(
                    "[ModelGateway] 渠道「{}」独立分组「{}」共 {} 个 Key，本次请求最多尝试前 {} 个",
                    chan_alias,
                    group.name,
                    group.keys.len(),
                    take
                );
            }
            for key in group.keys.iter().take(take) {
                queue.push(KeyAttempt {
                    key: key.clone(),
                    group_label: group.name.clone(),
                });
            }
        } else {
            let idx = round_robin.fetch_add(1, Ordering::Relaxed) % group.keys.len();
            queue.push(KeyAttempt {
                key: group.keys[idx].clone(),
                group_label: group.name.clone(),
            });
        }
    }
    queue
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

/// 单个渠道内的完整尝试：兼容性校验 → 出网准备（含同协议快速通道）→ 弹性调度。
///
/// 渠道内的 Key 分组与出口节点全部耗尽后返回 `Err`；跨渠道故障转移由外层
/// `dispatch_protocol_egress` 负责。
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
) -> Result<EgressOutcome, Response> {
    let chan_alias = channel.effective_alias();
    let chan_stats_id = channel.stats_id;

    // 解析出按分组优先级排列的候选 Key 分组（分组身份 = Key 的 groupName）
    let key_groups = resolve_channel_key_groups_for_model(ctx, channel, model_to_send).await;

    let attempts: Vec<KeyAttempt> = if key_groups.is_empty() {
        // 无任何可用 Key：单次尝试（空 Key，用于免 Key 渠道）
        vec![KeyAttempt {
            key: select_channel_api_key(ctx, channel).await,
            group_label: String::new(),
        }]
    } else {
        build_key_attempt_queue(&key_groups, &ctx.key_round_robin, &chan_alias)
    };

    // 出网协议：模型级覆盖优先（响应回转嗅探也按该协议归一）
    let target = channel.target_protocol_for(model_to_send);
    let mut last_error_response: Option<Response> = None;

    // 顺序遍历尝试队列：组内（独立组）与组间故障转移已在建队时铺平
    for (attempt_idx, attempt) in attempts.iter().enumerate() {
        let selected_key = &attempt.key;

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
                // 当前 Key 请求失败（例如 401 鉴权失败、429 频次限制或上游故障），
                // 记录错误并顺延到队列中的下一个候选 Key（独立组内下一个 Key，或下一个分组）
                tracing::warn!(
                    "[ModelGateway] 渠道「{}」分组「{}」第 {} 次候选请求模型「{}」失败，自动尝试下一候选 Key...",
                    chan_alias,
                    attempt.group_label,
                    attempt_idx + 1,
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

/// 跨渠道故障转移上限：单个请求最多尝试的渠道数。
/// 每个渠道内部已有 Key 分组 × 出口节点两层重试，叠加过多渠道会把单请求拖到不可接受的时长。
const MAX_CHANNEL_FAILOVER: usize = 3;

/// 出网调度入口：在候选渠道列表上做故障转移。
///
/// 首选渠道的 Key 分组与出口节点全部耗尽后，自动切换到下一个同样提供该模型的渠道
/// （候选顺序见 `resolve_channel_candidates`，首项即原 `resolve_channel` 的结果，
/// 故单渠道场景下行为与改动前完全一致）。
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
) -> Result<EgressOutcome, Response> {
    // 候选列表的首项恒为传入的 channel（handler 已用 resolve_channel 选定）；
    // 其后为后备渠道。带别名前缀的定向请求只会得到单项列表，不做转移。
    let candidates = resolve_channel_candidates(config, raw_model);
    let fallbacks: Vec<&ChannelConfig> = candidates
        .iter()
        .map(|(ch, _)| *ch)
        .filter(|ch| ch.id != channel.id)
        .take(MAX_CHANNEL_FAILOVER.saturating_sub(1))
        .collect();

    let mut chain: Vec<&ChannelConfig> = vec![channel];
    chain.extend(fallbacks);
    let total = chain.len();

    let mut last_error_response: Option<Response> = None;
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
                last_error_response = Some(err_resp);
            }
        }
    }

    // 全部候选渠道均失败：返回最后一个渠道的错误响应
    Err(last_error_response.unwrap_or_else(|| {
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

    #[test]
    fn round_robin_group_rotates_one_key_per_request() {
        // 轮询组：每次请求只贡献 1 次尝试，Key 随计数器在组内轮转
        let groups = vec![group("g1", KEY_GROUP_MODE_ROUND_ROBIN, &["k1", "k2", "k3"])];
        let counter = AtomicUsize::new(0);

        let picked: Vec<String> = (0..4)
            .map(|_| {
                let q = build_key_attempt_queue(&groups, &counter, "ch");
                assert_eq!(q.len(), 1, "轮询组单请求只尝试 1 个 Key");
                q[0].key.clone()
            })
            .collect();

        assert_eq!(picked, vec!["k1", "k2", "k3", "k1"], "组内均匀轮转并回绕");
    }

    #[test]
    fn independent_group_is_sticky_then_walks_group() {
        // 独立组：始终从组内第一个 Key 开始，失败才顺延组内后续 Key（顺序固定，不随请求轮转）
        let groups = vec![group("g1", KEY_GROUP_MODE_INDEPENDENT, &["k1", "k2", "k3"])];
        let counter = AtomicUsize::new(0);

        for _ in 0..3 {
            let q = build_key_attempt_queue(&groups, &counter, "ch");
            let keys: Vec<&str> = q.iter().map(|a| a.key.as_str()).collect();
            assert_eq!(keys, vec!["k1", "k2", "k3"], "黏住 k1 且组内顺序转移");
        }
        assert_eq!(counter.load(Ordering::Relaxed), 0, "独立组不消耗轮询计数器");
    }

    #[test]
    fn mixed_groups_chain_in_priority_order() {
        // 混合：轮询组贡献 1 个候选，独立组铺开组内全部候选，按分组顺序拼接
        let groups = vec![
            group("rr", KEY_GROUP_MODE_ROUND_ROBIN, &["a1", "a2"]),
            group("indep", KEY_GROUP_MODE_INDEPENDENT, &["b1", "b2"]),
        ];
        let counter = AtomicUsize::new(1);

        let q = build_key_attempt_queue(&groups, &counter, "ch");
        let keys: Vec<&str> = q.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(keys, vec!["a2", "b1", "b2"]);
        assert_eq!(q[0].group_label, "rr");
        assert_eq!(q[1].group_label, "indep");
    }

    #[test]
    fn independent_group_caps_attempts_per_request() {
        // 独立组内 Key 过多时截断，避免失效 Key 叠加上游超时把单请求拖爆
        let keys: Vec<String> = (0..MAX_INDEPENDENT_ATTEMPTS_PER_GROUP + 5)
            .map(|i| format!("k{i}"))
            .collect();
        let groups = vec![ResolvedKeyGroup {
            id: "big".to_string(),
            name: "big".to_string(),
            mode: KEY_GROUP_MODE_INDEPENDENT.to_string(),
            keys: keys.clone(),
        }];
        let counter = AtomicUsize::new(0);

        let q = build_key_attempt_queue(&groups, &counter, "ch");
        assert_eq!(q.len(), MAX_INDEPENDENT_ATTEMPTS_PER_GROUP);
        assert_eq!(q[0].key, "k0", "截断保留最靠前的 Key");
    }

    #[test]
    fn empty_groups_are_skipped() {
        let groups = vec![
            group("empty", KEY_GROUP_MODE_ROUND_ROBIN, &[]),
            group("real", KEY_GROUP_MODE_ROUND_ROBIN, &["k1"]),
        ];
        let counter = AtomicUsize::new(0);
        let q = build_key_attempt_queue(&groups, &counter, "ch");
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].key, "k1");
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
