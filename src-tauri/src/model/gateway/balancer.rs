use super::policies::opencode::strip_opencode_prefix;
use super::types::{
    current_timestamp, ChannelConfig, ModelProxyConfig, ModelProxyContext, ProxyRequestLog,
};
use serde_json::Value as JsonValue;
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tracing::warn;

/// 渠道是否对外暴露指定模型：白名单为空(None) = 全部暴露，否则须包含该模型（大小写不敏感）
fn channel_exposes_model(channel: &ChannelConfig, model: &str) -> bool {
    channel
        .enabled_models
        .as_ref()
        .map_or(true, |models| {
            models.iter().any(|m| m.eq_ignore_ascii_case(model))
        })
}

/// 根据请求模型名解析目标渠道与发送给上游的裸模型名。
/// 规则：
/// 1. `alias/裸模型` 优先按别名前缀精确匹配启用渠道；
/// 2. 「多渠道共同提供的模型」按用户配置的路由顺序（model_channel_order）选取首个可用渠道；
/// 3. 未配置顺序时，若某个启用渠道的白名单（enabled_models）中包含该模型，则优先分发给该渠道；
/// 4. 若无匹配，回退至启用的默认 opencode 渠道；
/// 5. 若默认 opencode 未启用，回退至首个已启用的自定义渠道。
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

    // 2. 用户配置的重叠模型路由顺序：按列表序找首个启用且暴露该模型的渠道。
    //    raw 与 stripped 双查，兼容白名单里同时存在带/不带前缀写法的情况。
    if let Some(order) = &config.model_channel_order {
        let lookup = |key: &str| -> Option<&'a ChannelConfig> {
            order
                .get(&key.to_lowercase())?
                .iter()
                .find_map(|channel_id| {
                    config.channels.iter().find(|c| {
                        c.enabled && &c.id == channel_id && channel_exposes_model(c, stripped)
                    })
                })
        };
        if let Some(ch) = lookup(raw_model).or_else(|| lookup(stripped)) {
            return Some((ch, stripped.to_string()));
        }
    }

    // 3. 检查是否有启用渠道显式在 enabled_models 中勾选/包含了该模型
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

/// Key 的元数据描述，用于分组和模型支持度匹配
#[derive(Debug, Clone)]
pub struct ChannelKeyInfo {
    pub key: String,
    pub group_id: String,
    pub enabled: bool,
    pub supported_models: Option<Vec<String>>,
}

/// 从 site_model_cache 数据库或自定义配置中提取渠道拥有的所有 Key 及其原始分组、模型支持元数据
pub async fn load_channel_all_keys_info(
    ctx: &ModelProxyContext,
    channel: &ChannelConfig,
) -> Vec<ChannelKeyInfo> {
    let mut raw_keys = Vec::new();
    let mut seen = HashSet::new();

    if let Some(site_id) = channel
        .site_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
    {
        if let Some(app_ctx) = ctx.app_ctx.read().await.as_ref().cloned() {
            if let Ok(connection) = app_ctx.database.0.lock() {
                if let Ok(mut statement) = connection.prepare(
                    "SELECT keys_json, groups_json, key_models_json FROM site_model_cache WHERE site_id = ?1 ORDER BY profile_id",
                ) {
                    let rows = statement.query_map([site_id], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    });
                    if let Ok(rows) = rows {
                        for (keys_json, groups_json, key_models_json) in rows.flatten() {
                            let keys_val: Vec<JsonValue> =
                                serde_json::from_str(&keys_json).unwrap_or_default();
                            let groups_map: std::collections::HashMap<String, String> =
                                serde_json::from_str(&groups_json).unwrap_or_default();
                            let key_models_map: std::collections::HashMap<String, Vec<JsonValue>> =
                                serde_json::from_str(&key_models_json).unwrap_or_default();

                            for item in keys_val {
                                let key = item
                                    .as_str()
                                    .or_else(|| item.get("key").and_then(JsonValue::as_str))
                                    .map(str::trim)
                                    .filter(|k| !k.is_empty());
                                if let Some(k) = key {
                                    if seen.insert(k.to_string()) {
                                        let group = groups_map
                                            .get(k)
                                            .map(|s| s.trim())
                                            .filter(|s| !s.is_empty())
                                            .unwrap_or("default")
                                            .to_string();
                                        let supported_models = key_models_map.get(k).map(|models| {
                                            models
                                                .iter()
                                                .filter_map(|m| {
                                                    m.as_str()
                                                        .or_else(|| m.get("id").and_then(JsonValue::as_str))
                                                        .map(|s| s.trim().to_string())
                                                })
                                                .collect::<Vec<_>>()
                                        });
                                        raw_keys.push(ChannelKeyInfo {
                                            key: k.to_string(),
                                            group_id: group,
                                            enabled: true,
                                            supported_models,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    } else {
        for k in channel.get_effective_keys() {
            if seen.insert(k.clone()) {
                raw_keys.push(ChannelKeyInfo {
                    key: k,
                    group_id: "default".to_string(),
                    enabled: true,
                    supported_models: None,
                });
            }
        }
    }

    // 将用户在渠道中配置的自定义 KeyRules（分组重写、启用/禁用、支持模型覆盖）融合应用
    if let Some(rules) = &channel.key_rules {
        for rule in rules {
            if let Some(info) = raw_keys.iter_mut().find(|k| k.key == rule.key) {
                if !rule.group_id.trim().is_empty() {
                    info.group_id = rule.group_id.trim().to_string();
                }
                info.enabled = rule.enabled;
                if rule.supported_models.is_some() {
                    info.supported_models = rule.supported_models.clone();
                }
            } else if rule.enabled {
                // 如果是用户手动新增但不在同步缓存里的 Key，也予以纳入
                raw_keys.push(ChannelKeyInfo {
                    key: rule.key.clone(),
                    group_id: if rule.group_id.trim().is_empty() {
                        "default".to_string()
                    } else {
                        rule.group_id.trim().to_string()
                    },
                    enabled: rule.enabled,
                    supported_models: rule.supported_models.clone(),
                });
            }
        }
    }

    raw_keys
}

/// 根据请求的模型名称，按「分组优先级」解析出候选 Key 分组队列。
/// 返回：`Vec<Vec<String>>`，外层为分组优先级序列（按顺序故障转移 Failover），
/// 内层为该组内支持该模型的可用 Key 列表（组内 Round-Robin 轮询）。
pub async fn resolve_channel_key_groups_for_model(
    ctx: &ModelProxyContext,
    channel: &ChannelConfig,
    model: &str,
) -> Vec<Vec<String>> {
    let all_keys = load_channel_all_keys_info(ctx, channel).await;
    if all_keys.is_empty() {
        return Vec::new();
    }

    // 1. 整理已定义的分组顺序及其启用状态
    let defined_groups = channel.key_groups.as_deref().unwrap_or(&[]);
    let disabled_group_ids: HashSet<&str> = defined_groups
        .iter()
        .filter(|g| !g.enabled)
        .map(|g| g.id.as_str())
        .collect();

    // 2. 筛选出支持该模型且处于启用状态的 Key
    let model_lower = model.trim().to_lowercase();
    let valid_keys: Vec<&ChannelKeyInfo> = all_keys
        .iter()
        .filter(|k| {
            if !k.enabled {
                return false;
            }
            if disabled_group_ids.contains(k.group_id.as_str()) {
                return false;
            }
            // 检查模型支持度：None 表示全部支持
            if let Some(supported) = &k.supported_models {
                if !supported.is_empty()
                    && !supported.iter().any(|m| m.trim().eq_ignore_ascii_case(&model_lower))
                {
                    return false;
                }
            }
            true
        })
        .collect();

    if valid_keys.is_empty() {
        return Vec::new();
    }

    // 3. 按分组优先级对 Key 进行归类
    // 已定义的分组按定义的顺序排在前面，未在 key_groups 中显式声明的分组按发现顺序追加在后
    let mut ordered_groups: Vec<String> = defined_groups
        .iter()
        .filter(|g| g.enabled)
        .map(|g| g.id.clone())
        .collect();

    for key_info in &valid_keys {
        if !ordered_groups.contains(&key_info.group_id) {
            ordered_groups.push(key_info.group_id.clone());
        }
    }

    // 4. 构建外层为分组、内层为 Key 列表的二层结构
    let mut result: Vec<Vec<String>> = Vec::new();
    for gid in ordered_groups {
        let group_keys: Vec<String> = valid_keys
            .iter()
            .filter(|k| k.group_id == gid)
            .map(|k| k.key.clone())
            .collect();
        if !group_keys.is_empty() {
            result.push(group_keys);
        }
    }

    result
}

/// 解析渠道当前可用的 API Keys（扁平化全量列表，用于无需区分模型/分组的通用场景）。
pub async fn resolve_channel_api_keys(
    ctx: &ModelProxyContext,
    channel: &ChannelConfig,
) -> Vec<String> {
    let all = load_channel_all_keys_info(ctx, channel).await;
    let defined_groups = channel.key_groups.as_deref().unwrap_or(&[]);
    let disabled_group_ids: HashSet<&str> = defined_groups
        .iter()
        .filter(|g| !g.enabled)
        .map(|g| g.id.as_str())
        .collect();

    all.into_iter()
        .filter(|k| k.enabled && !disabled_group_ids.contains(k.group_id.as_str()))
        .map(|k| k.key)
        .collect()
}

/// 选择渠道当前可用的 API Key（全局通用轮询，无模型上下文时的兼容回退）。
pub async fn select_channel_api_key(ctx: &ModelProxyContext, channel: &ChannelConfig) -> String {
    let keys = resolve_channel_api_keys(ctx, channel).await;
    if keys.is_empty() {
        return String::new();
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
    model: &str,
) -> Vec<String> {
    // 模型级覆盖优先于渠道级配置：「管理可用模型」中为该模型单独选择的代理策略
    if let Some(rule) = channel.model_proxy_rule(model) {
        let mode = rule.mode.trim().to_lowercase();
        if mode == "direct" {
            return vec!["__direct__".to_string()];
        }
        if mode == "fixed" {
            if let Some(ref node) = rule.node_id {
                let node = node.trim();
                if !node.is_empty() {
                    return vec![node.to_string()];
                }
            }
            // 未指定节点时锁定池内首个启用节点（与渠道级固定通道语义一致）
            if let Some(first) = first_enabled_pool_node(ctx).await {
                return vec![first];
            }
            return vec!["__direct__".to_string()];
        }
        if mode == "pool" {
            let mut candidates = vec!["__direct__".to_string()];
            candidates.extend(enabled_pool_nodes(ctx).await);
            return candidates;
        }
        // 未知模式回退渠道级配置
    }

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

    if channel.use_fixed_proxy {
        // 固定通道语义：恒定使用单一出口节点。未手动指定节点时按 rowid 锁定
        // 首个启用节点（绝对稳定，不随延迟测量值漂移），绝不进入多节点轮换，
        // 否则重试/轮询游标会让流量在池内漂移，「固定」名存实亡。
        if let Some(first) = first_enabled_pool_node(ctx).await {
            return vec![first];
        }
    } else {
        candidates.extend(enabled_pool_nodes(ctx).await);
    }

    if candidates.is_empty() {
        candidates.push("__direct__".to_string());
    }

    candidates
}

/// 池内启用节点 ID 列表：测活成功者优先，其后按延迟升序，最后按入库序
async fn enabled_pool_nodes(ctx: &ModelProxyContext) -> Vec<String> {
    let app_ctx_arc = ctx.app_ctx.read().await.clone();
    let Some(app_ctx) = app_ctx_arc.as_ref() else {
        return Vec::new();
    };
    query_enabled_pool_nodes(&app_ctx.database)
}

/// 在已持有数据库引用的前提下查询启用节点（避免跨 await 持锁借用）
fn query_enabled_pool_nodes(database: &std::sync::Arc<crate::models::Database>) -> Vec<String> {
    match database.0.lock() {
        Ok(conn) => {
            let Ok(mut stmt) = conn.prepare(
                "SELECT id FROM proxy_pool_nodes
                 WHERE (is_enabled IS NULL OR is_enabled = 1)
                 ORDER BY
                   (CASE WHEN test_status = 'success' OR channel_test_status = 'success' THEN 0 ELSE 1 END) ASC,
                   COALESCE(NULLIF(channel_latency_ms, 0), NULLIF(latency_ms, 0), 99999) ASC,
                   rowid ASC",
            ) else {
                return Vec::new();
            };
            stmt.query_map([], |r| r.get::<_, String>(0))
                .map(|rows| rows.filter_map(Result::ok).collect())
                .unwrap_or_default()
        }
        Err(_) => Vec::new(),
    }
}

/// 池内首个启用节点（按 rowid 稳定排序），供「固定」语义锁定
async fn first_enabled_pool_node(ctx: &ModelProxyContext) -> Option<String> {
    enabled_pool_nodes(ctx).await.into_iter().next()
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
