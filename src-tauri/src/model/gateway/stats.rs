use super::types::{
    ChannelUsageStats, ModelProxyContext, ModelProxyStatus, ModelProxyState,
    OpencodeProxyStatus, OpencodeProxyState, ProxyRequestLog,
};
use rusqlite::params;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use tauri::Manager;

impl ModelProxyContext {
    /// 异步记录请求日志至本地数据库
    pub async fn record_log(&self, log: ProxyRequestLog) {
        let app_handle_opt = self.app_handle.read().await.clone();
        if let Some(app) = app_handle_opt {
            let database = app.state::<crate::models::Database>();
            let now_millis = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let _ = (move || -> Result<(), rusqlite::Error> {
                let conn = database
                    .0
                    .lock()
                    .map_err(|_| rusqlite::Error::ExecuteReturnedResults)?;
                conn.execute(
                    "INSERT OR REPLACE INTO opencode_proxy_logs (
                        id, timestamp, method, path, channel_id, model, stream, status_code,
                        duration_ms, ttft_ms, prompt_tokens, prompt_cache_hit_tokens,
                        prompt_cache_miss_tokens, completion_tokens, reasoning_tokens, total_tokens,
                        error_message, request_body, response_body, node_name, created_at
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
                    params![
                        log.id,
                        log.timestamp,
                        log.method,
                        log.path,
                        log.channel_id,
                        log.model,
                        if log.stream { 1 } else { 0 },
                        log.status_code as i64,
                        log.duration_ms as i64,
                        log.ttft_ms.map(|v| v as i64),
                        log.prompt_tokens.map(|v| v as i64),
                        log.prompt_cache_hit_tokens.map(|v| v as i64),
                        log.prompt_cache_miss_tokens.map(|v| v as i64),
                        log.completion_tokens.map(|v| v as i64),
                        log.reasoning_tokens.map(|v| v as i64),
                        log.total_tokens.map(|v| v as i64),
                        log.error_message,
                        log.request_body,
                        log.response_body,
                        log.node_name,
                        now_millis,
                    ],
                )?;
                conn.execute(
                    "DELETE FROM opencode_proxy_logs WHERE id NOT IN (
                        SELECT id FROM opencode_proxy_logs ORDER BY created_at DESC, rowid DESC LIMIT 1000
                    )",
                    [],
                )?;
                Ok(())
            })();
        }
    }
}

pub async fn get_model_proxy_status_summary(state: &ModelProxyState) -> ModelProxyStatus {
    let running = state.shutdown_sender.read().await.is_some();
    let port = *state.current_port.read().await;
    let url = format!("http://127.0.0.1:{port}/v1");
    let uptime_seconds = state
        .context
        .started_at
        .read()
        .await
        .map(|t| t.elapsed().as_secs())
        .unwrap_or(0);

    let config = state.context.config.read().await;
    let channels_count = config.channels.len();
    let models_count = {
        let models = state.context.cached_channel_models.read().await;
        models
            .iter()
            .map(|entry| {
                let channel = config.channels.iter().find(|c| c.id == entry.channel_id);
                let allowed = channel.and_then(|c| c.enabled_models.as_ref());
                match allowed {
                    None => entry.models.len(),
                    Some(allowed) => entry.models.iter().filter(|m| allowed.contains(m)).count(),
                }
            })
            .sum::<usize>()
    };

    let total_reqs = state.context.metrics.total_requests.load(Ordering::Relaxed);
    let succ_reqs = state
        .context
        .metrics
        .successful_requests
        .load(Ordering::Relaxed);
    let fail_reqs = state
        .context
        .metrics
        .failed_requests
        .load(Ordering::Relaxed);
    let total_prompt = state
        .context
        .metrics
        .total_prompt_tokens
        .load(Ordering::Relaxed);
    let total_completion = state
        .context
        .metrics
        .total_completion_tokens
        .load(Ordering::Relaxed);
    let total_reasoning = state
        .context
        .metrics
        .total_reasoning_tokens
        .load(Ordering::Relaxed);
    let total_reasoning_reqs = state
        .context
        .metrics
        .total_reasoning_requests
        .load(Ordering::Relaxed);
    let total_cache_hit = state
        .context
        .metrics
        .total_cache_hit_tokens
        .load(Ordering::Relaxed);
    let total_toks = state.context.metrics.total_tokens.load(Ordering::Relaxed);

    let (db_total_prompt, db_total_comp, db_total_reas, db_total_cache, db_total_all, db_today_tokens) = {
        let app_handle_opt = state.context.app_handle.read().await.clone();
        if let Some(app) = app_handle_opt {
            let database = app.state::<crate::models::Database>();
            let res: Result<(i64, i64, i64, i64, i64, i64), rusqlite::Error> = (|| {
                let conn = database.0.lock().map_err(|_| rusqlite::Error::ExecuteReturnedResults)?;
                let total_row = conn.query_row(
                    "SELECT 
                        COALESCE(SUM(prompt_tokens), 0),
                        COALESCE(SUM(completion_tokens), 0),
                        COALESCE(SUM(reasoning_tokens), 0),
                        COALESCE(SUM(prompt_cache_hit_tokens), 0),
                        COALESCE(SUM(total_tokens), 0)
                     FROM opencode_proxy_logs",
                    [],
                    |r| Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                ).unwrap_or((0, 0, 0, 0, 0));

                let today_start_secs = {
                    let now_secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let local_offset_secs = if let Ok(output) = std::process::Command::new("/bin/date").arg("+%z").output() {
                        if output.status.success() {
                            let text = String::from_utf8_lossy(&output.stdout);
                            let value = text.trim();
                            if value.len() == 5 && (value.starts_with('+') || value.starts_with('-')) {
                                if let (Ok(h), Ok(m)) = (value[1..3].parse::<i64>(), value[3..5].parse::<i64>()) {
                                    let sign = if value.starts_with('-') { -1 } else { 1 };
                                    sign * (h * 3600 + m * 60)
                                } else { 28800 }
                            } else { 28800 }
                        } else { 28800 }
                    } else { 28800 };
                    let local_secs = now_secs as i64 + local_offset_secs;
                    let local_today_start = local_secs - local_secs.rem_euclid(86400);
                    (local_today_start - local_offset_secs).max(0)
                };

                let today_tokens = conn.query_row(
                    "SELECT COALESCE(SUM(total_tokens), 0)
                     FROM opencode_proxy_logs
                     WHERE CAST(timestamp AS INTEGER) >= ?1",
                    [today_start_secs],
                    |r| r.get::<_, i64>(0)
                ).unwrap_or(0);

                Ok((total_row.0, total_row.1, total_row.2, total_row.3, total_row.4, today_tokens))
            })();
            res.unwrap_or((0, 0, 0, 0, 0, 0))
        } else {
            (0, 0, 0, 0, 0, 0)
        }
    };

    ModelProxyStatus {
        running,
        port,
        url,
        total_requests: total_reqs,
        successful_requests: succ_reqs,
        failed_requests: fail_reqs,
        uptime_seconds,
        models_count,
        channels_count,
        total_prompt_tokens: total_prompt.max(db_total_prompt as u64),
        total_completion_tokens: total_completion.max(db_total_comp as u64),
        total_reasoning_tokens: total_reasoning.max(db_total_reas as u64),
        total_reasoning_requests: total_reasoning_reqs,
        total_cache_hit_tokens: total_cache_hit.max(db_total_cache as u64),
        total_tokens: total_toks.max(db_total_all as u64),
        today_total_tokens: db_today_tokens as u64,
    }
}

#[allow(dead_code)]
pub async fn get_opencode_proxy_status_summary(state: &OpencodeProxyState) -> OpencodeProxyStatus {
    get_model_proxy_status_summary(state).await
}

pub async fn get_channel_usage_stats_summary(
    state: &ModelProxyState,
) -> Result<Vec<ChannelUsageStats>, String> {
    let app_handle_opt = state.context.app_handle.read().await.clone();
    let Some(app) = app_handle_opt else {
        return Ok(Vec::new());
    };

    let channels = {
        let cfg = state.context.config.read().await;
        cfg.channels.clone()
    };

    tokio::task::block_in_place(move || {
        let database = app.state::<crate::models::Database>();
        let conn = database
            .0
            .lock()
            .map_err(|_| "获取数据库连接失败".to_string())?;

        let mut stmt = conn
            .prepare(
                "SELECT 
                    channel_id,
                    COUNT(*) as total_requests,
                    COALESCE(SUM(CASE WHEN status_code >= 200 AND status_code < 300 THEN 1 ELSE 0 END), 0) as succ_requests,
                    COALESCE(SUM(CASE WHEN status_code >= 400 THEN 1 ELSE 0 END), 0) as fail_requests,
                    COALESCE(AVG(duration_ms), 0) as avg_duration,
                    COALESCE(AVG(CASE WHEN ttft_ms IS NOT NULL THEN ttft_ms ELSE NULL END), 0) as avg_ttft,
                    COALESCE(SUM(prompt_tokens), 0) as sum_prompt,
                    COALESCE(SUM(completion_tokens), 0) as sum_comp,
                    COALESCE(SUM(reasoning_tokens), 0) as sum_reasoning,
                    COALESCE(SUM(prompt_cache_hit_tokens), 0) as sum_cache_hit,
                    COALESCE(SUM(total_tokens), 0) as sum_total
                 FROM opencode_proxy_logs GROUP BY channel_id",
            )
            .map_err(|e| format!("查询渠道统计失败: {e}"))?;

        let mut result_map = HashMap::new();
        let rows = stmt
            .query_map([], |row| {
                let ch_id: String = row.get(0)?;
                let total_req: i64 = row.get(1)?;
                let succ_req: i64 = row.get(2)?;
                let fail_req: i64 = row.get(3)?;
                let avg_dur: f64 = row.get(4)?;
                let avg_ttft: f64 = row.get(5)?;
                let prompt_tok: i64 = row.get(6)?;
                let comp_tok: i64 = row.get(7)?;
                let reas_tok: i64 = row.get(8)?;
                let cache_tok: i64 = row.get(9)?;
                let total_tok: i64 = row.get(10)?;

                Ok(ChannelUsageStats {
                    channel_id: ch_id,
                    total_requests: total_req as u64,
                    successful_requests: succ_req as u64,
                    failed_requests: fail_req as u64,
                    avg_duration_ms: avg_dur as u64,
                    avg_ttft_ms: if avg_ttft > 0.0 {
                        Some(avg_ttft as u64)
                    } else {
                        None
                    },
                    total_prompt_tokens: prompt_tok as u64,
                    total_completion_tokens: comp_tok as u64,
                    total_reasoning_tokens: reas_tok as u64,
                    total_cache_hit_tokens: cache_tok as u64,
                    total_tokens: total_tok as u64,
                })
            })
            .map_err(|e| format!("解析统计数据失败: {e}"))?;

        for r in rows.flatten() {
            result_map.insert(r.channel_id.clone(), r);
        }

        let mut stats_list = Vec::new();
        for ch in &channels {
            if let Some(st) = result_map.remove(&ch.id) {
                stats_list.push(st);
            } else {
                stats_list.push(ChannelUsageStats {
                    channel_id: ch.id.clone(),
                    total_requests: 0,
                    successful_requests: 0,
                    failed_requests: 0,
                    avg_duration_ms: 0,
                    avg_ttft_ms: None,
                    total_prompt_tokens: 0,
                    total_completion_tokens: 0,
                    total_reasoning_tokens: 0,
                    total_cache_hit_tokens: 0,
                    total_tokens: 0,
                });
            }
        }

        for (_, st) in result_map {
            stats_list.push(st);
        }

        Ok(stats_list)
    })
}
