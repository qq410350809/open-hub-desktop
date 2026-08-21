use super::types::{
    current_timestamp, ChannelConfig, ModelProxyConfig, ModelProxyContext, ProxyRequestLog,
};
use serde_json::Value as JsonValue;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::Manager;

pub fn strip_opencode_prefix(model: &str) -> &str {
    model.strip_prefix("opencode/").unwrap_or(model)
}

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
    if let Some(ch) = config.channels.iter().find(|c| c.id == "opencode" && c.enabled) {
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
        node_name: Some(get_node_display_name(ctx, cand_id).await),
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
    if !channel.use_proxy_pool && !channel.use_fixed_proxy {
        return vec!["__direct__".to_string()];
    }

    let mut candidates = Vec::new();
    if !channel.use_fixed_proxy {
        candidates.push("__direct__".to_string());
    }

    if let Some(app) = ctx.app_handle.read().await.as_ref() {
        let database = app.state::<crate::models::Database>();
        let nodes: Vec<String> = {
            match database.0.lock() {
                Ok(conn) => {
                    let stmt_res = conn.prepare(
                        "SELECT id FROM proxy_pool_nodes
                         WHERE (latency_ms > 0 AND latency_ms <= 1000)
                            OR (channel_latency_ms > 0 AND channel_latency_ms <= 1000)
                         ORDER BY COALESCE(NULLIF(latency_ms, 0), channel_latency_ms, 999) ASC",
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
        candidates.extend(nodes);
    }

    candidates
}

pub async fn build_client_for_candidate(
    ctx: &ModelProxyContext,
    candidate: &str,
) -> reqwest::Client {
    if candidate == "__direct__" {
        return ctx.default_http_client.clone();
    }

    if let Some(app) = ctx.app_handle.read().await.as_ref() {
        let database = app.state::<crate::models::Database>();
        let runtime = app.state::<crate::proxypool::ProxyRuntime>();

        let _ =
            crate::proxypool::select_proxy_node_transient(&database, &runtime, candidate).await;
        let proxy_url = crate::proxypool::runtime_proxy_url_pub(&runtime);
        if let Ok(proxy) = reqwest::Proxy::all(&proxy_url) {
            if let Ok(client) = reqwest::Client::builder()
                .proxy(proxy)
                .timeout(Duration::from_secs(300))
                .build()
            {
                return client;
            }
        }
    }

    ctx.default_http_client.clone()
}

pub async fn get_node_display_name(ctx: &ModelProxyContext, candidate: &str) -> String {
    if candidate == "__direct__" {
        return "直连通道".to_string();
    }

    if let Some(app) = ctx.app_handle.read().await.as_ref() {
        let database = app.state::<crate::models::Database>();
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
