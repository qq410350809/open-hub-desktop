use super::policies::opencode::strip_opencode_prefix;
use super::types::{
    current_timestamp, ChannelConfig, ModelProxyConfig, ModelProxyContext, ProxyRequestLog,
};
use serde_json::Value as JsonValue;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tracing::warn;

/// 根据请求模型名解析目标渠道与发送给上游的裸模型名。
/// 规则：
/// 1. `alias/裸模型` 优先按别名前缀精确匹配启用渠道；
/// 2. 无前缀时，若某个启用渠道的白名单（enabled_models）中包含该模型，则优先分发给该渠道；
/// 3. 若无匹配，回退至启用的默认 opencode 渠道；
/// 4. 若默认 opencode 未启用，回退至首个已启用的自定义渠道。
pub fn resolve_channel<'a>(
    config: &'a ModelProxyConfig,
    raw_model: &str,
) -> Option<(&'a ChannelConfig, String)> {
    // 1. 带前缀别名匹配 (如 x666/claude-sonnet-5)
    if let Some((prefix, rest)) = raw_model.split_once('/') {
        if let Some(ch) = config
            .channels
            .iter()
            .find(|c| c.enabled && c.effective_alias().eq_ignore_ascii_case(prefix))
        {
            return Some((ch, rest.to_string()));
        }
    }

    let stripped = strip_opencode_prefix(raw_model);

    // 2. 检查是否有启用渠道显式在 enabled_models 中勾选/包含了该模型
    if let Some(ch) = config.channels.iter().find(|c| {
        c.enabled
            && c.enabled_models.as_ref().map_or(false, |models| {
                models
                    .iter()
                    .any(|m| m.eq_ignore_ascii_case(stripped) || m.eq_ignore_ascii_case(raw_model))
            })
    }) {
        return Some((ch, stripped.to_string()));
    }

    // 3. 回退默认 opencode 渠道（如果已启用）
    if let Some(ch) = config
        .channels
        .iter()
        .find(|c| c.id == "opencode" && c.enabled)
    {
        return Some((ch, stripped.to_string()));
    }

    // 4. 若 opencode 渠道未启用，回退到首个已启用的自定义渠道
    if let Some(ch) = config.channels.iter().find(|c| c.enabled) {
        return Some((ch, stripped.to_string()));
    }

    None
}

/// 渠道多 Key 轮询选择器：原子递增索引并在有效 API Keys 中轮流选取
pub fn select_channel_api_key(ctx: &ModelProxyContext, channel: &ChannelConfig) -> String {
    let keys = channel.get_effective_keys();
    if keys.is_empty() {
        return String::new();
    }
    if keys.len() == 1 {
        return keys[0].clone();
    }
    let idx = ctx.key_round_robin.fetch_add(1, Ordering::Relaxed) % keys.len();
    keys[idx].clone()
}

pub fn format_upstream_error_message(status: u16, error_body: &str) -> String {
    if let Ok(jv) = serde_json::from_str::<JsonValue>(error_body) {
        if let Some(msg) = jv.pointer("/error/message").and_then(JsonValue::as_str) {
            return format!("HTTP {status} 接口错误: {msg}");
        }
        if let Some(msg) = jv.pointer("/message").and_then(JsonValue::as_str) {
            return format!("HTTP {status} 接口错误: {msg}");
        }
    }

    if status == 429 || error_body.contains("Rate limit exceeded") {
        return "HTTP 429 频次受限: 上游接口触发了请求频次限制。已自动尝试切换下一个健康节点或重试。".to_string();
    }
    if error_body.contains("400 Bad Request") && error_body.contains("cloudflare") {
        return "HTTP 400 Cloudflare 拦截: 上游网关拒绝请求（请检查模型名称是否支持，或尝试开启/关闭代理池轮询）".to_string();
    }
    if error_body.contains("502 Bad Gateway") && error_body.contains("cloudflare") {
        return "HTTP 502 Cloudflare 上游不可达: 当前节点连接服务器超时".to_string();
    }
    if error_body.contains("503 Service Temporarily Unavailable") {
        return "HTTP 503 上游服务繁忙".to_string();
    }
    if error_body.contains("<html>") {
        return format!("HTTP {status} 上游返回 HTML 错误页面");
    }

    format!("HTTP {status}: {error_body}")
}

/// 记录节点/渠道自动切换事件
#[allow(dead_code)]
pub async fn record_failover_event(
    ctx: &ModelProxyContext,
    req_id: &str,
    path: &str,
    channel_id: &str,
    model: &str,
    is_stream: bool,
    status_code: u16,
    error_message: String,
    duration_ms: u64,
    req_body_str: Option<String>,
    cand_id: &str,
) {
    ctx.record_log(ProxyRequestLog {
        id: req_id.to_string(),
        timestamp: current_timestamp(),
        method: "POST".to_string(),
        path: path.to_string(),
        channel_id: channel_id.to_string(),
        model: model.to_string(),
        stream: is_stream,
        status_code,
        duration_ms,
        ttft_ms: None,
        prompt_tokens: None,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
        completion_tokens: None,
        reasoning_tokens: None,
        total_tokens: None,
        error_message: Some(error_message),
        request_body: req_body_str,
        response_body: None,
        channel_stats_id: None,
        node_name: Some(get_node_display_name(ctx, cand_id).await),
        cache_creation_tokens: None,
        client_name: None,
        upstream_url: None,
    })
    .await;
}

// ---------------------------------------------------------------------------
// 代理池按延迟升序与直连候选列表构建
// ---------------------------------------------------------------------------

pub async fn get_sorted_egress_candidates(
    ctx: &ModelProxyContext,
    channel: &ChannelConfig,
) -> Vec<String> {
    if channel.use_fixed_proxy {
        if let Some(ref node) = channel.fixed_proxy_node {
            if !node.trim().is_empty() {
                return vec![node.trim().to_string()];
            }
        }
    }

    if !channel.use_proxy_pool && !channel.use_fixed_proxy {
        return vec!["__direct__".to_string()];
    }

    let mut candidates = Vec::new();
    if !channel.use_fixed_proxy {
        candidates.push("__direct__".to_string());
    }

    if let Some(ctx) = ctx.app_ctx.read().await.as_ref() {
        let database = &ctx.database;
        let nodes: Vec<String> = {
            match database.0.lock() {
                Ok(conn) => {
                    let stmt_res = conn.prepare(
                        "SELECT id FROM proxy_pool_nodes
                         WHERE (is_enabled IS NULL OR is_enabled = 1)
                         ORDER BY
                           (CASE WHEN test_status = 'success' OR channel_test_status = 'success' THEN 0 ELSE 1 END) ASC,
                           COALESCE(NULLIF(channel_latency_ms, 0), NULLIF(latency_ms, 0), 99999) ASC,
                           rowid ASC",
                    );
                    if let Ok(mut stmt) = stmt_res {
                        if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                            rows.filter_map(Result::ok).collect()
                        } else {
                            Vec::new()
                        }
                    } else {
                        Vec::new()
                    }
                }
                Err(_) => Vec::new(),
            }
        };
        if channel.use_fixed_proxy {
            // 固定通道语义：恒定使用单一出口节点。未手动指定节点时按 rowid 锁定
            // 首个启用节点（绝对稳定，不随延迟测量值漂移），绝不进入多节点轮换，
            // 否则重试/轮询游标会让流量在池内漂移，「固定」名存实亡。
            if let Some(first) = nodes.into_iter().next() {
                return vec![first];
            }
        } else {
            candidates.extend(nodes);
        }
    }

    if candidates.is_empty() {
        candidates.push("__direct__".to_string());
    }

    candidates
}

/// 出网请求超时：取配置 `timeout_seconds`（clamp 10..=600，缺省 300）。
/// 该配置此前从未被读取、出网恒为 300s 硬超时，长流任务（agent 多轮工具循环）
/// 会被无差别掐断；统一经此函数换算。
pub fn egress_timeout(config: &ModelProxyConfig) -> Duration {
    let secs = config.timeout_seconds.clamp(10, 600);
    Duration::from_secs(secs)
}

pub async fn build_client_for_candidate(
    ctx: &ModelProxyContext,
    candidate: &str,
) -> reqwest::Client {
    let timeout = {
        let cfg = ctx.config.read().await;
        egress_timeout(&cfg)
    };
    if candidate == "__direct__" {
        return ctx.default_http_client.read().await.clone();
    }

    if let Some(ctx) = ctx.app_ctx.read().await.as_ref() {
        let database = &ctx.database;
        let runtime = &ctx.proxy_runtime;

        if let Err(e) =
            crate::proxypool::select_proxy_node_transient(&database, &runtime, candidate).await
        {
            warn!("[ModelGateway] 切换代理节点 {candidate} 失败: {e}");
        }
        let proxy_url = crate::proxypool::runtime_proxy_url_pub(&runtime);
        if !proxy_url.is_empty() {
            if let Ok(proxy) = reqwest::Proxy::all(&proxy_url) {
                if let Ok(client) = reqwest::Client::builder()
                    .proxy(proxy)
                    .pool_max_idle_per_host(0)
                    .timeout(timeout)
                    .build()
                {
                    return client;
                }
            }
        }
    }

    ctx.default_http_client.read().await.clone()
}

pub async fn get_node_display_name(ctx: &ModelProxyContext, candidate: &str) -> String {
    if candidate == "__direct__" {
        return "直连通道".to_string();
    }

    if let Some(ctx) = ctx.app_ctx.read().await.as_ref() {
        let database = &ctx.database;
        let name_opt: Option<String> = {
            match database.0.lock() {
                Ok(conn) => {
                    let res: Result<String, _> = conn.query_row(
                        "SELECT name FROM proxy_pool_nodes WHERE id = ?1",
                        [candidate],
                        |row| row.get(0),
                    );
                    res.ok()
                }
                Err(_) => None,
            }
        };
        if let Some(name) = name_opt {
            if !name.trim().is_empty() {
                return name;
            }
        }
    }

    candidate.to_string()
}

#[cfg(test)]
mod balancer_tests {
    use super::*;

    #[test]
    fn egress_timeout_clamps_and_defaults() {
        // P1-7：timeout_seconds 缺省 300s，越界 clamp 到 [10, 600]
        let cfg = ModelProxyConfig::default();
        assert_eq!(egress_timeout(&cfg).as_secs(), 300, "缺省 300s");
        let mut cfg = ModelProxyConfig::default();
        cfg.timeout_seconds = 5;
        assert_eq!(egress_timeout(&cfg).as_secs(), 10, "下限 clamp 10s");
        cfg.timeout_seconds = 9999;
        assert_eq!(egress_timeout(&cfg).as_secs(), 600, "上限 clamp 600s");
    }
}
