//! Bounded model discovery and cache persistence, independent of HTTP routing.
use super::balancer::resolve_channel_api_keys;
use super::types::{
    effective_aux_url, ChannelConfig, ChannelModelFetchError, ChannelModelList, ModelProxyContext,
    ModelProxySyncProgress, ModelProxySyncStage, GATEWAY_USER_AGENT, MODELS_FETCH_CHANNEL_TIMEOUT,
    MODELS_FETCH_TOTAL_TIMEOUT,
};
use axum::http::StatusCode;
use futures_util::{stream, StreamExt};
use serde_json::Value as JsonValue;
use std::collections::HashSet;
use std::time::Duration;
use tokio::time::{timeout_at, Instant};
use tracing::warn;

#[cfg(test)]
#[path = "tests/model_sync.rs"]
mod tests;

/// Fetch selected enabled channels. Omitted channel/request IDs retain startup/bulk behavior.
pub async fn fetch_upstream_models_inner(
    ctx: &ModelProxyContext,
    channel_id: Option<&str>,
    request_id: Option<&str>,
) -> (Vec<ChannelModelList>, Vec<ChannelModelFetchError>) {
    fetch_upstream_models_with_timeout(ctx, channel_id, request_id, MODELS_FETCH_CHANNEL_TIMEOUT)
        .await
}

/// A single deadline covers key resolution and every endpoint/key/main/aux attempt.
/// The smaller budget parameter also permits real HTTP timeout regression tests.
pub(super) async fn fetch_upstream_models_with_timeout(
    ctx: &ModelProxyContext,
    channel_id: Option<&str>,
    request_id: Option<&str>,
    channel_timeout: Duration,
) -> (Vec<ChannelModelList>, Vec<ChannelModelFetchError>) {
    let channels: Vec<_> = ctx
        .config
        .read()
        .await
        .channels
        .iter()
        .filter(|ch| ch.enabled && channel_id.is_none_or(|id| id == ch.id))
        .cloned()
        .collect();
    let bus = ctx
        .app_ctx
        .read()
        .await
        .as_ref()
        .map(|app| app.event_bus.clone());
    let mut versions = ctx.model_sync_versions.write().await;
    let jobs: Vec<_> = channels
        .into_iter()
        .enumerate()
        .map(|(index, channel)| {
            let version = versions.entry(channel.id.clone()).or_default();
            *version = version.wrapping_add(1);
            (index, channel, *version)
        })
        .collect();
    drop(versions);

    let mut completed = stream::iter(jobs)
        .map(|(index, ch, version)| {
            let bus = bus.as_ref();
            async move {
                let deadline = Instant::now() + channel_timeout;
                // Queueing counts toward the deadline, so a startup bulk refresh cannot
                // leave a concurrent single-channel IPC request waiting indefinitely.
                let permit = timeout_at(deadline, ctx.model_sync_slots.acquire()).await;
                let Ok(Ok(_permit)) = permit else {
                    return (
                        index,
                        ch,
                        version,
                        Err("渠道模型同步超时（等待同步队列）".to_string()),
                    );
                };
                let progress = SyncProgress {
                    bus,
                    request_id,
                    channel_id: &ch.id,
                };
                progress.emit(
                    ModelProxySyncStage::Resolving,
                    "正在解析渠道 API Key",
                    None,
                    None,
                );
                let outcome = match timeout_at(deadline, resolve_channel_api_keys(ctx, &ch)).await {
                    Ok(keys) => fetch_channel_models(ctx, &ch, &keys, deadline, &progress).await,
                    Err(_) => Err("渠道模型同步超时（解析 API Key）".to_string()),
                };
                (index, ch, version, outcome)
            }
        })
        .buffer_unordered(3)
        .collect::<Vec<_>>()
        .await;
    completed.sort_by_key(|(index, _, _, _)| *index);
    let mut channel_models = Vec::new();
    let mut fetch_errors = Vec::new();
    let live = ctx.config.read().await;
    let versions = ctx.model_sync_versions.read().await;
    let mut cached = ctx.cached_channel_models.write().await;
    let mut cached_errors = ctx.cached_fetch_errors.write().await;
    cached.retain(|entry| live.channels.iter().any(|ch| ch.id == entry.channel_id));
    cached_errors.retain(|entry| live.channels.iter().any(|ch| ch.id == entry.channel_id));
    for (_, ch, version, outcome) in completed {
        // Config edits/removals and newer syncs invalidate the snapshot, not the old cache.
        let current = live.channels.iter().any(|current| current == &ch);
        let latest = versions.get(&ch.id) == Some(&version);
        if !current || !latest {
            fetch_errors.push(ChannelModelFetchError {
                channel_id: ch.id.clone(),
                channel_name: ch.name.clone(),
                alias: ch.effective_alias(),
                error: "渠道配置或同步任务已更新，已丢弃过期结果".to_string(),
            });
            continue;
        }
        cached_errors.retain(|entry| entry.channel_id != ch.id);
        match outcome {
            Ok(models) => {
                let item = ChannelModelList {
                    channel_id: ch.id.clone(),
                    channel_name: ch.name.clone(),
                    alias: ch.effective_alias(),
                    models,
                };
                cached.retain(|entry| entry.channel_id != ch.id);
                cached.push(item.clone());
                channel_models.push(item);
                SyncProgress {
                    bus: bus.as_ref(),
                    request_id,
                    channel_id: &ch.id,
                }
                .emit(
                    ModelProxySyncStage::Saving,
                    "正在保存已获取的模型列表",
                    None,
                    None,
                );
            }
            Err(error) => {
                warn!(
                    "[ModelGateway] 模型列表拉取失败：channel_id={} error={}",
                    ch.id, error
                );
                let failure = ChannelModelFetchError {
                    channel_id: ch.id.clone(),
                    channel_name: ch.name.clone(),
                    alias: ch.effective_alias(),
                    error,
                };
                cached_errors.push(failure.clone());
                fetch_errors.push(failure);
            }
        }
    }
    drop(cached_errors);
    drop(cached);
    // Serialize persistence with result acceptance so an older sync cannot write after a newer one.
    // SQLite work runs in the blocking pool; it never stalls the async executor.
    if !channel_models.is_empty() {
        if let Err(error) = save_channel_models_to_db(ctx, &channel_models).await {
            warn!("[ModelGateway] 保存渠道模型到数据库失败: {error}");
        }
    }
    drop(versions);
    drop(live);
    (channel_models, fetch_errors)
}

struct SyncProgress<'a> {
    bus: Option<&'a crate::context::EventBus>,
    request_id: Option<&'a str>,
    channel_id: &'a str,
}

impl SyncProgress<'_> {
    fn emit(
        &self,
        stage: ModelProxySyncStage,
        message: &str,
        endpoint: Option<&str>,
        key: Option<(usize, usize)>,
    ) {
        if let Some(bus) = self.bus {
            // Configured URLs may include userinfo/query credentials. reqwest errors and
            // upstream bodies are deliberately never interpolated into public messages.
            let endpoint = endpoint.and_then(safe_model_fetch_endpoint);
            bus.emit(
                "model-proxy-sync-progress",
                ModelProxySyncProgress {
                    request_id: self.request_id,
                    channel_id: self.channel_id,
                    stage,
                    message,
                    endpoint: endpoint.as_deref(),
                    key_index: key.map(|(index, _)| index),
                    key_total: key.map(|(_, total)| total),
                },
            );
        }
    }
}

fn safe_model_fetch_endpoint(raw: &str) -> Option<String> {
    let mut url = url::Url::parse(raw).ok()?;
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

async fn fetch_channel_models(
    ctx: &ModelProxyContext,
    ch: &ChannelConfig,
    api_keys: &[String],
    deadline: Instant,
    progress: &SyncProgress<'_>,
) -> Result<Vec<String>, String> {
    let base = ch.base_url.trim().trim_end_matches('/');
    let aux = effective_aux_url(ch)
        .map(|aux| aux.trim_end_matches('/').to_string())
        .filter(|aux| aux != base);
    // Reserve half the remaining channel budget for aux even if main keeps responding
    // slowly for many keys. A transport failure ends that base immediately.
    let main_deadline = if aux.is_some() {
        let now = Instant::now();
        now + deadline.saturating_duration_since(now) / 2
    } else {
        deadline
    };
    let main = fetch_models_from_base(ctx, api_keys, base, main_deadline, progress).await;
    if main.is_ok() {
        return main;
    }
    if let Some(aux) = aux {
        progress.emit(
            ModelProxySyncStage::Fallback,
            "主地址失败，正在尝试辅助地址",
            Some(&aux),
            None,
        );
        return fetch_models_from_base(ctx, api_keys, &aux, deadline, progress)
            .await
            .map_err(|aux_error| {
                format!("主地址：{}；辅助地址：{}", main.unwrap_err(), aux_error)
            });
    }
    main
}

/// Probe endpoint candidates and union every successful key. Deadline/transport failures
/// return any models already collected rather than discarding partial success.
async fn fetch_models_from_base(
    ctx: &ModelProxyContext,
    api_keys: &[String],
    base: &str,
    deadline: Instant,
    progress: &SyncProgress<'_>,
) -> Result<Vec<String>, String> {
    let candidates = if base.ends_with("/v1") || base.ends_with("/vbeta") {
        vec![format!("{base}/models")]
    } else {
        vec![format!("{base}/v1/models"), format!("{base}/models")]
    };
    let key_total = api_keys.len().max(1);
    let mut models = Vec::new();
    let mut seen = HashSet::new();
    let mut last_error = "上游未返回可用模型".to_string();
    for (endpoint_index, models_url) in candidates.iter().enumerate() {
        if endpoint_index > 0 {
            progress.emit(
                ModelProxySyncStage::Fallback,
                "正在尝试备用模型端点",
                Some(models_url),
                None,
            );
        }
        let mut auth_failed = false;
        for key_index in 0..key_total {
            if Instant::now() >= deadline {
                return partial_models(models, "渠道模型同步超时".to_string());
            }
            let key = Some((key_index + 1, key_total));
            progress.emit(
                ModelProxySyncStage::Request,
                "正在请求上游模型列表",
                Some(models_url),
                key,
            );
            let mut req = ctx
                .models_fetch_client
                .get(models_url)
                .header("User-Agent", GATEWAY_USER_AGENT);
            if let Some(api_key) = api_keys.get(key_index).filter(|key| !key.is_empty()) {
                req = req.header("Authorization", format!("Bearer {api_key}"));
            }
            let request_deadline = deadline.min(Instant::now() + MODELS_FETCH_TOTAL_TIMEOUT);
            let response = timeout_at(request_deadline, async {
                let resp = req.send().await?;
                let status = resp.status();
                progress.emit(
                    ModelProxySyncStage::Response,
                    &format!("已收到 HTTP {}，正在读取并解析模型列表", status.as_u16()),
                    Some(models_url),
                    key,
                );
                // Error bodies can echo credentials and are not needed for status diagnostics.
                let bytes = if status.is_success() {
                    resp.bytes().await?
                } else {
                    bytes::Bytes::new()
                };
                Ok::<_, reqwest::Error>((status, bytes))
            })
            .await;
            let (status, bytes) = match response {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => {
                    let message = if error.is_timeout() {
                        "上游模型请求超时"
                    } else {
                        "连接上游失败"
                    };
                    progress.emit(
                        ModelProxySyncStage::Response,
                        message,
                        Some(models_url),
                        key,
                    );
                    return partial_models(models, message.to_string());
                }
                Err(_) => {
                    progress.emit(
                        ModelProxySyncStage::Response,
                        "上游模型请求超时",
                        Some(models_url),
                        key,
                    );
                    return partial_models(models, "上游模型请求超时".to_string());
                }
            };
            if !status.is_success() {
                last_error = format!("上游返回 HTTP 状态码: {}", status.as_u16());
                auth_failed |= matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN);
                continue;
            }
            match parse_models_payload(&bytes) {
                Some(list) if !list.is_empty() => {
                    for model in list {
                        if seen.insert(model.clone()) {
                            models.push(model);
                        }
                    }
                    progress.emit(
                        ModelProxySyncStage::Response,
                        "模型列表解析成功",
                        Some(models_url),
                        key,
                    );
                }
                _ => {
                    let prefix = String::from_utf8_lossy(&bytes[..bytes.len().min(80)])
                        .trim()
                        .to_lowercase();
                    let html = prefix.starts_with("<!doctype") || prefix.starts_with("<html");
                    last_error = if html {
                        "上游返回 HTML 页面而非模型列表（请检查模型接口路径）".to_string()
                    } else {
                        "上游响应中未解析到任何模型".to_string()
                    };
                    progress.emit(
                        ModelProxySyncStage::Response,
                        &last_error,
                        Some(models_url),
                        key,
                    );
                    if html {
                        break;
                    }
                }
            }
        }
        if !models.is_empty() {
            return Ok(models);
        }
        if auth_failed {
            break;
        }
    }
    partial_models(models, last_error)
}

fn partial_models(models: Vec<String>, error: String) -> Result<Vec<String>, String> {
    if models.is_empty() {
        Err(error)
    } else {
        Ok(models)
    }
}

/// 解析模型列表响应体：兼容 OpenAI（data[].id）与 Gemini（models[].name）两种格式。
/// 返回 None 表示 body 不是合法 JSON 或结构完全不匹配（如 HTML 页面）。
fn parse_models_payload(bytes: &[u8]) -> Option<Vec<String>> {
    let jv = serde_json::from_slice::<JsonValue>(bytes).ok()?;
    let items = jv
        .get("data")
        .and_then(JsonValue::as_array)
        .or_else(|| jv.get("models").and_then(JsonValue::as_array))?;
    let mut list = Vec::new();
    for item in items {
        // OpenAI 条目用 id，Gemini 条目用 name（可带 models/ 前缀）
        let key = item
            .get("id")
            .and_then(JsonValue::as_str)
            .or_else(|| item.get("name").and_then(JsonValue::as_str));
        if let Some(id) = key {
            list.push(id.trim_start_matches("models/").to_string());
        }
    }
    Some(list)
}

/// 保存渠道模型列表到数据库，用于持久化缓存
async fn save_channel_models_to_db(
    ctx: &ModelProxyContext,
    channel_models: &[ChannelModelList],
) -> Result<(), String> {
    let Some(app) = ctx.app_ctx.read().await.clone() else {
        return Ok(());
    };
    let items = channel_models.to_vec();
    tokio::task::spawn_blocking(move || {
        let mut conn = app.database.0.lock().map_err(|e| format!("获取数据库锁失败: {e}"))?;
        let transaction = conn.transaction().map_err(|e| e.to_string())?;
        {
            let mut statement = transaction.prepare_cached(
                "INSERT OR REPLACE INTO channel_model_cache (channel_id, channel_name, alias, models_json, updated_at) VALUES (?1, ?2, ?3, ?4, datetime('now'))"
            ).map_err(|e| e.to_string())?;
            for item in items {
                let models = serde_json::to_string(&item.models).map_err(|e| e.to_string())?;
                statement.execute(rusqlite::params![item.channel_id, item.channel_name, item.alias, models]).map_err(|e| e.to_string())?;
            }
        }
        transaction.commit().map_err(|e| e.to_string())
    }).await.map_err(|e| format!("缓存保存任务失败: {e}"))?
}
