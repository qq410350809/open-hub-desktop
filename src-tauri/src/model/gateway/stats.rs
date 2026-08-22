use super::types::{
    ChannelUsageStats, GatewayDailyPoint, GatewayHourlyPoint, GatewayOverviewStats,
    GatewayOverviewTotals, ModelProxyContext, ModelProxyStatus, ModelProxyState,
    OpencodeProxyState, OpencodeProxyStatus, ProxyRequestLog, ProxyTokenUsageReport,
};
use chrono::Datelike;
use rusqlite::params;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use tauri::Manager;

impl ModelProxyContext {
    /// 异步记录请求日志至本地数据库，并同步累加「渠道 × 模型 × 客户端 × 日/时」聚合统计。
    /// 明细日志按保留天数节流清理（默认永久保留）；统计聚合表独立持久化，不受清理影响。
    pub async fn record_log(&self, log: ProxyRequestLog) {
        let app_handle_opt = self.app_handle.read().await.clone();
        if let Some(app) = app_handle_opt {
            let database = app.state::<crate::models::Database>();
            let now_millis = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);

            // 保留期清理节流：至多每小时执行一次，避免每次写入都全表扫描
            const RETENTION_CHECK_INTERVAL_MS: u64 = 3_600_000;
            let retention_cutoff: Option<String> = {
                let days = self.config.read().await.effective_log_retention_days();
                if days == 0 {
                    None
                } else {
                    let last_run = self.log_retention_last_run.load(Ordering::Relaxed);
                    let due = now_millis.max(0) as u64 >= last_run
                        && (now_millis.max(0) as u64).saturating_sub(last_run) >= RETENTION_CHECK_INTERVAL_MS;
                    if due {
                        let cutoff = chrono::Local::now().date_naive()
                            - chrono::Duration::days(days as i64);
                        Some(cutoff.format("%Y-%m-%d").to_string())
                    } else {
                        None
                    }
                }
            };

            let retention_clock = self.log_retention_last_run.clone();
            let _ = (move || -> Result<(), rusqlite::Error> {
                let conn = database.lock_db();
                conn.execute(
                    "INSERT OR REPLACE INTO model_proxy_logs (
                        id, timestamp, method, path, channel_id, model, stream, status_code,
                        duration_ms, ttft_ms, prompt_tokens, prompt_cache_hit_tokens,
                        prompt_cache_miss_tokens, cache_creation_tokens, completion_tokens,
                        reasoning_tokens, total_tokens,
                        error_message, request_body, response_body, node_name, client_name, created_at
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
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
                        log.cache_creation_tokens.map(|v| v as i64),
                        log.completion_tokens.map(|v| v as i64),
                        log.reasoning_tokens.map(|v| v as i64),
                        log.total_tokens.map(|v| v as i64),
                        log.error_message,
                        log.request_body,
                        log.response_body,
                        log.node_name,
                        log.client_name,
                        now_millis,
                    ],
                )?;

                // 明细保留期清理：删除保留窗口之外的明细（统计聚合表不受影响）
                if let Some(cutoff) = &retention_cutoff {
                    conn.execute(
                        "DELETE FROM model_proxy_logs WHERE timestamp < ?1",
                        [cutoff.as_str()],
                    )?;
                    retention_clock.store(now_millis.max(0) as u64, Ordering::Relaxed);
                }

                // 「渠道 × 模型 × 客户端 × 日」与「× 时」聚合：长期统计依赖这两表。
                // 渠道维度为稳定数字 ID（channel_stats_id），改别名不错位；缺省回退日志渠道列
                let today = chrono::Local::now().format("%Y-%m-%d").to_string();
                let is_success = if log.status_code < 400 { 1 } else { 0 };
                let is_failure = 1 - is_success;
                let stats_key = log
                    .channel_stats_id
                    .clone()
                    .unwrap_or_else(|| log.channel_id.clone());
                let model_dim = if log.model.trim().is_empty() { "" } else { log.model.as_str() };
                let client_dim = log
                    .client_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("");
                conn.execute(
                    "INSERT INTO channel_daily_stats (
                        date, channel_id, model, client_name, total_requests, successful_requests, failed_requests,
                        duration_ms_total, ttft_ms_total, ttft_count,
                        prompt_tokens, completion_tokens, reasoning_tokens, cache_hit_tokens, cache_creation_tokens, total_tokens
                    ) VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                    ON CONFLICT(date, channel_id, model, client_name) DO UPDATE SET
                        total_requests = total_requests + 1,
                        successful_requests = successful_requests + excluded.successful_requests,
                        failed_requests = failed_requests + excluded.failed_requests,
                        duration_ms_total = duration_ms_total + excluded.duration_ms_total,
                        ttft_ms_total = ttft_ms_total + excluded.ttft_ms_total,
                        ttft_count = ttft_count + excluded.ttft_count,
                        prompt_tokens = prompt_tokens + excluded.prompt_tokens,
                        completion_tokens = completion_tokens + excluded.completion_tokens,
                        reasoning_tokens = reasoning_tokens + excluded.reasoning_tokens,
                        cache_hit_tokens = cache_hit_tokens + excluded.cache_hit_tokens,
                        cache_creation_tokens = cache_creation_tokens + excluded.cache_creation_tokens,
                        total_tokens = total_tokens + excluded.total_tokens",
                    params![
                        today,
                        stats_key.clone(),
                        model_dim,
                        client_dim,
                        is_success,
                        is_failure,
                        log.duration_ms as i64,
                        log.ttft_ms.unwrap_or(0) as i64,
                        if log.ttft_ms.is_some() { 1 } else { 0 },
                        log.prompt_tokens.unwrap_or(0) as i64,
                        log.completion_tokens.unwrap_or(0) as i64,
                        log.reasoning_tokens.unwrap_or(0) as i64,
                        log.prompt_cache_hit_tokens.unwrap_or(0) as i64,
                        log.cache_creation_tokens.unwrap_or(0) as i64,
                        log.total_tokens.unwrap_or(0) as i64,
                    ],
                )?;
                // 小时粒度：供小时趋势。小时取自日志时间戳（本地时间文本）
                let hour: i64 = log
                    .timestamp
                    .get(11..13)
                    .and_then(|h| h.parse::<i64>().ok())
                    .unwrap_or(0);
                conn.execute(
                    "INSERT INTO channel_hourly_stats (
                        date, hour, channel_id, model, client_name, total_requests, successful_requests, failed_requests,
                        prompt_tokens, completion_tokens, reasoning_tokens, cache_hit_tokens, cache_creation_tokens, total_tokens
                    ) VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                    ON CONFLICT(date, hour, channel_id, model, client_name) DO UPDATE SET
                        total_requests = total_requests + 1,
                        successful_requests = successful_requests + excluded.successful_requests,
                        failed_requests = failed_requests + excluded.failed_requests,
                        prompt_tokens = prompt_tokens + excluded.prompt_tokens,
                        completion_tokens = completion_tokens + excluded.completion_tokens,
                        reasoning_tokens = reasoning_tokens + excluded.reasoning_tokens,
                        cache_hit_tokens = cache_hit_tokens + excluded.cache_hit_tokens,
                        cache_creation_tokens = cache_creation_tokens + excluded.cache_creation_tokens,
                        total_tokens = total_tokens + excluded.total_tokens",
                    params![
                        today,
                        hour,
                        stats_key,
                        model_dim,
                        client_dim,
                        is_success,
                        is_failure,
                        log.prompt_tokens.unwrap_or(0) as i64,
                        log.completion_tokens.unwrap_or(0) as i64,
                        log.reasoning_tokens.unwrap_or(0) as i64,
                        log.prompt_cache_hit_tokens.unwrap_or(0) as i64,
                        log.cache_creation_tokens.unwrap_or(0) as i64,
                        log.total_tokens.unwrap_or(0) as i64,
                    ],
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
                // 累计与今日 Token 均取自持久化日统计表。
                // 不能走明细日志表——明细按保留天数清理，且历史上限 1000 条，
                // 清理/裁剪都会让「累计」缩水；日统计表才是长期统计的唯一事实来源。
                let total_row = conn.query_row(
                    "SELECT
                        COALESCE(SUM(prompt_tokens), 0),
                        COALESCE(SUM(completion_tokens), 0),
                        COALESCE(SUM(reasoning_tokens), 0),
                        COALESCE(SUM(cache_hit_tokens), 0),
                        COALESCE(SUM(total_tokens), 0)
                     FROM channel_daily_stats",
                    [],
                    |r| Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                ).unwrap_or((0, 0, 0, 0, 0));

                let today = chrono::Local::now().format("%Y-%m-%d").to_string();
                let today_tokens = conn
                    .query_row(
                        "SELECT COALESCE(SUM(total_tokens), 0)
                         FROM channel_daily_stats
                         WHERE date = ?1",
                        [&today],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap_or(0);

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
        let conn = database.lock_db();

        let today = chrono::Local::now().format("%Y-%m-%d").to_string();

        // 一次性迁移旧数据：日统计早期以渠道别名为维度，改为稳定数字 ID。
        // 幂等——迁移后 WHERE 不再命中，仅多付出若干次主键探测
        for ch in &channels {
            let key = ch.stats_key();
            let alias = ch.effective_alias();
            if key != alias {
                let _ = conn.execute(
                    "UPDATE channel_daily_stats SET channel_id = ?1 WHERE channel_id = ?2",
                    params![key, alias],
                );
            }
        }

        // 总量 = 全部日数据汇总；今日行单独取出，双通道一次查询带回
        let mut stmt = conn
            .prepare(
                "SELECT
                    t.channel_id,
                    t.total_requests, t.successful_requests, t.failed_requests,
                    t.duration_ms_total, t.ttft_ms_total, t.ttft_count,
                    t.prompt_tokens, t.completion_tokens, t.reasoning_tokens, t.cache_hit_tokens, t.total_tokens,
                    COALESCE(d.total_requests, 0),
                    COALESCE(d.successful_requests, 0),
                    COALESCE(d.failed_requests, 0),
                    COALESCE(d.duration_ms_total, 0),
                    COALESCE(d.ttft_ms_total, 0),
                    COALESCE(d.ttft_count, 0),
                    COALESCE(d.prompt_tokens, 0),
                    COALESCE(d.completion_tokens, 0),
                    COALESCE(d.cache_hit_tokens, 0),
                    COALESCE(d.total_tokens, 0)
                 FROM (
                    SELECT channel_id,
                        SUM(total_requests) as total_requests,
                        SUM(successful_requests) as successful_requests,
                        SUM(failed_requests) as failed_requests,
                        SUM(duration_ms_total) as duration_ms_total,
                        SUM(ttft_ms_total) as ttft_ms_total,
                        SUM(ttft_count) as ttft_count,
                        SUM(prompt_tokens) as prompt_tokens,
                        SUM(completion_tokens) as completion_tokens,
                        SUM(reasoning_tokens) as reasoning_tokens,
                        SUM(cache_hit_tokens) as cache_hit_tokens,
                        SUM(total_tokens) as total_tokens
                    FROM channel_daily_stats GROUP BY channel_id
                 ) t
                 LEFT JOIN channel_daily_stats d
                    ON d.channel_id = t.channel_id AND d.date = ?1",
            )
            .map_err(|e| format!("查询渠道统计失败: {e}"))?;

        let rows = stmt
            .query_map([&today], |row| {
                let total_requests: i64 = row.get(1)?;
                let duration_total: i64 = row.get(4)?;
                let ttft_total: i64 = row.get(5)?;
                let ttft_count: i64 = row.get(6)?;
                let today_requests: i64 = row.get(12)?;
                let today_duration_total: i64 = row.get(15)?;
                let today_ttft_total: i64 = row.get(16)?;
                let today_ttft_count: i64 = row.get(17)?;

                Ok(ChannelUsageStats {
                    channel_id: row.get(0)?,
                    total_requests: total_requests as u64,
                    successful_requests: row.get::<_, i64>(2)? as u64,
                    failed_requests: row.get::<_, i64>(3)? as u64,
                    avg_duration_ms: (duration_total / total_requests.max(1)) as u64,
                    avg_ttft_ms: avg_or_none(ttft_total as i64, ttft_count as i64),
                    total_prompt_tokens: row.get::<_, i64>(7)? as u64,
                    total_completion_tokens: row.get::<_, i64>(8)? as u64,
                    total_reasoning_tokens: row.get::<_, i64>(9)? as u64,
                    total_cache_hit_tokens: row.get::<_, i64>(10)? as u64,
                    total_tokens: row.get::<_, i64>(11)? as u64,
                    today_requests: today_requests as u64,
                    today_successful_requests: row.get::<_, i64>(13)? as u64,
                    today_failed_requests: row.get::<_, i64>(14)? as u64,
                    today_avg_duration_ms: (today_duration_total / today_requests.max(1)) as u64,
                    today_avg_ttft_ms: avg_or_none(today_ttft_total, today_ttft_count),
                    today_prompt_tokens: row.get::<_, i64>(18)? as u64,
                    today_completion_tokens: row.get::<_, i64>(19)? as u64,
                    today_cache_hit_tokens: row.get::<_, i64>(20)? as u64,
                    today_total_tokens: row.get::<_, i64>(21)? as u64,
                })
            })
            .map_err(|e| format!("解析统计数据失败: {e}"))?;

        let mut result_map = HashMap::new();
        for r in rows.flatten() {
            result_map.insert(r.channel_id.clone(), r);
        }

        let empty_stats = |id: String| ChannelUsageStats {
            channel_id: id,
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
            today_requests: 0,
            today_successful_requests: 0,
            today_failed_requests: 0,
            today_avg_duration_ms: 0,
            today_avg_ttft_ms: None,
            today_prompt_tokens: 0,
            today_completion_tokens: 0,
            today_cache_hit_tokens: 0,
            today_total_tokens: 0,
        };

        let mut stats_list = Vec::new();
        for ch in &channels {
            // 日统计维度为渠道稳定数字 ID（stats_key），与可修改的别名解耦
            if let Some(st) = result_map.remove(&ch.stats_key()) {
                stats_list.push(st);
            } else {
                stats_list.push(empty_stats(ch.stats_key()));
            }
        }

        for (_, st) in result_map {
            stats_list.push(st);
        }

        Ok(stats_list)
    })
}

/// 控制台「全渠道数据总览」。
/// 提供 from/to 时按日期区间逐日聚合（缺日补零）+ 区间内汇总；
/// 未提供时回退旧行为：近 N 天窗口（默认 14，1-90 钳制）+ 全量累计。
/// 无论何种模式都附带今日聚合点（供 KPI「今日」角标）。
/// 数据来自 channel_daily_stats（长期持久化），与运行时计数器互补。
pub async fn get_gateway_overview_stats(
    state: &ModelProxyState,
    days: Option<u32>,
    from: Option<String>,
    to: Option<String>,
) -> Result<GatewayOverviewStats, String> {
    let app_handle_opt = state.context.app_handle.read().await.clone();
    let Some(app) = app_handle_opt else {
        return Ok(GatewayOverviewStats {
            days: 14,
            daily: Vec::new(),
            totals: GatewayOverviewTotals::default(),
            today: GatewayDailyPoint::default(),
            hourly: None,
            monthly: None,
        });
    };

    tokio::task::block_in_place(move || {
        let database = app.state::<crate::models::Database>();
        let conn = database.lock_db();

        let today = chrono::Local::now().date_naive();
        let today_str = today.format("%Y-%m-%d").to_string();

        // 区间钳制：上界不超过今天（不生成未来补零日），跨度最大 366 天（防超长区间拖垮图表）
        const MAX_RANGE_DAYS: i64 = 366;
        let (start, end, range_mode) = if from.is_some() || to.is_some() {
            let parse = |s: &str| {
                chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                    .map_err(|e| format!("日期格式无效（应为 YYYY-MM-DD）: {s}, {e}"))
            };
            let f = match from.as_deref() {
                Some(s) => parse(s)?,
                None => today - chrono::Duration::days(MAX_RANGE_DAYS - 1),
            };
            let t = match to.as_deref() {
                Some(s) => parse(s)?,
                None => today,
            };
            let end = t.min(today);
            let start = f.min(end);
            let span = (end - start).num_days() + 1;
            let start = if span > MAX_RANGE_DAYS {
                end - chrono::Duration::days(MAX_RANGE_DAYS - 1)
            } else {
                start
            };
            (start, end, true)
        } else {
            let days = days.unwrap_or(14).clamp(1, 90);
            (today - chrono::Duration::days(days as i64 - 1), today, false)
        };

        let start_str = start.format("%Y-%m-%d").to_string();
        let end_str = end.format("%Y-%m-%d").to_string();

        let mut by_date = HashMap::new();
        {
            let mut stmt = conn
                .prepare(
                    "SELECT date,
                        SUM(total_requests), SUM(successful_requests), SUM(failed_requests),
                        SUM(prompt_tokens), SUM(completion_tokens),
                        SUM(reasoning_tokens), SUM(cache_hit_tokens), SUM(total_tokens)
                     FROM channel_daily_stats
                     WHERE date >= ?1 AND date <= ?2
                     GROUP BY date",
                )
                .map_err(|e| format!("查询日统计失败: {e}"))?;
            let rows = stmt
                .query_map([&start_str, &end_str], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        GatewayDailyPoint {
                            date: String::new(),
                            total_requests: row.get::<_, i64>(1)?.max(0) as u64,
                            successful_requests: row.get::<_, i64>(2)?.max(0) as u64,
                            failed_requests: row.get::<_, i64>(3)?.max(0) as u64,
                            prompt_tokens: row.get::<_, i64>(4)?.max(0) as u64,
                            completion_tokens: row.get::<_, i64>(5)?.max(0) as u64,
                            reasoning_tokens: row.get::<_, i64>(6)?.max(0) as u64,
                            cache_hit_tokens: row.get::<_, i64>(7)?.max(0) as u64,
                            total_tokens: row.get::<_, i64>(8)?.max(0) as u64,
                        },
                    ))
                })
                .map_err(|e| format!("解析日统计失败: {e}"))?;
            for (date, mut point) in rows.flatten() {
                point.date = date.clone();
                by_date.insert(date, point);
            }
        }

        // 今日聚合：单独查询，与所选区间解耦——区间不含今天时（如「昨日」）KPI「今日」角标仍有数据
        let today_point = conn
            .query_row(
                "SELECT
                    COALESCE(SUM(total_requests), 0),
                    COALESCE(SUM(successful_requests), 0),
                    COALESCE(SUM(failed_requests), 0),
                    COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0),
                    COALESCE(SUM(reasoning_tokens), 0),
                    COALESCE(SUM(cache_hit_tokens), 0),
                    COALESCE(SUM(total_tokens), 0)
                 FROM channel_daily_stats WHERE date = ?1",
                [&today_str],
                |row| {
                    Ok(GatewayDailyPoint {
                        date: today_str.clone(),
                        total_requests: row.get::<_, i64>(0)?.max(0) as u64,
                        successful_requests: row.get::<_, i64>(1)?.max(0) as u64,
                        failed_requests: row.get::<_, i64>(2)?.max(0) as u64,
                        prompt_tokens: row.get::<_, i64>(3)?.max(0) as u64,
                        completion_tokens: row.get::<_, i64>(4)?.max(0) as u64,
                        reasoning_tokens: row.get::<_, i64>(5)?.max(0) as u64,
                        cache_hit_tokens: row.get::<_, i64>(6)?.max(0) as u64,
                        total_tokens: row.get::<_, i64>(7)?.max(0) as u64,
                    })
                },
            )
            .unwrap_or_default();

        // 补齐窗口内没有流量的日期，保证图表时间轴连续
        let mut daily = Vec::with_capacity((end - start).num_days() as usize + 1);
        let mut cursor = start;
        while cursor <= end {
            let key = cursor.format("%Y-%m-%d").to_string();
            daily.push(by_date.remove(&key).unwrap_or_default());
            cursor += chrono::Duration::days(1);
        }

        // 短区间（总天数 ≤ 3）：附带小时粒度趋势（每天 24 点缺时补零，多天时点带日期）。
        // 区间内无小时级数据时返回 None，前端回退到日视图（小时表自本版本起才开始记录/回填）
        let hourly = if range_mode && (end - start).num_days() + 1 <= 3 {
            let mut stmt = conn
                .prepare(
                    "SELECT date, hour,
                        SUM(total_requests), SUM(successful_requests), SUM(failed_requests),
                        SUM(prompt_tokens), SUM(completion_tokens),
                        SUM(reasoning_tokens), SUM(cache_hit_tokens), SUM(total_tokens)
                     FROM channel_hourly_stats
                     WHERE date >= ?1 AND date <= ?2
                     GROUP BY date, hour",
                )
                .map_err(|e| format!("查询小时统计失败: {e}"))?;
            let rows = stmt
                .query_map([&start_str, &end_str], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        GatewayHourlyPoint {
                            date: String::new(),
                            hour: 0,
                            total_requests: row.get::<_, i64>(2)?.max(0) as u64,
                            successful_requests: row.get::<_, i64>(3)?.max(0) as u64,
                            failed_requests: row.get::<_, i64>(4)?.max(0) as u64,
                            prompt_tokens: row.get::<_, i64>(5)?.max(0) as u64,
                            completion_tokens: row.get::<_, i64>(6)?.max(0) as u64,
                            reasoning_tokens: row.get::<_, i64>(7)?.max(0) as u64,
                            cache_hit_tokens: row.get::<_, i64>(8)?.max(0) as u64,
                            total_tokens: row.get::<_, i64>(9)?.max(0) as u64,
                        },
                    ))
                })
                .map_err(|e| format!("解析小时统计失败: {e}"))?;
            let mut by_slot = HashMap::new();
            for (date, hour, mut point) in rows.flatten() {
                point.date = date.clone();
                point.hour = hour.max(0).min(23) as u32;
                by_slot.insert((date, point.hour), point);
            }
            let mut points: Vec<GatewayHourlyPoint> = Vec::new();
            let mut cursor = start;
            while cursor <= end {
                let date_key = cursor.format("%Y-%m-%d").to_string();
                for h in 0..24u32 {
                    let mut point = by_slot.remove(&(date_key.clone(), h)).unwrap_or_default();
                    point.date = date_key.clone();
                    point.hour = h;
                    points.push(point);
                }
                cursor += chrono::Duration::days(1);
            }
            (points.iter().any(|p| p.total_requests > 0)).then_some(points)
        } else {
            None
        };

        // 长区间（总天数 > 92，即超过一个季度）：按月聚合，避免日粒度柱子过密不可读。
        // 月维度从日统计表 substr(date,1,7) 分组汇总，缺月补零；区间内有数据时才返回
        let monthly = if range_mode && (end - start).num_days() + 1 > 92 {
            let mut stmt = conn
                .prepare(
                    "SELECT substr(date, 1, 7),
                        SUM(total_requests), SUM(successful_requests), SUM(failed_requests),
                        SUM(prompt_tokens), SUM(completion_tokens),
                        SUM(reasoning_tokens), SUM(cache_hit_tokens), SUM(total_tokens)
                     FROM channel_daily_stats
                     WHERE date >= ?1 AND date <= ?2
                     GROUP BY substr(date, 1, 7)",
                )
                .map_err(|e| format!("查询月统计失败: {e}"))?;
            let rows = stmt
                .query_map([&start_str, &end_str], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        GatewayDailyPoint {
                            date: String::new(),
                            total_requests: row.get::<_, i64>(1)?.max(0) as u64,
                            successful_requests: row.get::<_, i64>(2)?.max(0) as u64,
                            failed_requests: row.get::<_, i64>(3)?.max(0) as u64,
                            prompt_tokens: row.get::<_, i64>(4)?.max(0) as u64,
                            completion_tokens: row.get::<_, i64>(5)?.max(0) as u64,
                            reasoning_tokens: row.get::<_, i64>(6)?.max(0) as u64,
                            cache_hit_tokens: row.get::<_, i64>(7)?.max(0) as u64,
                            total_tokens: row.get::<_, i64>(8)?.max(0) as u64,
                        },
                    ))
                })
                .map_err(|e| format!("解析月统计失败: {e}"))?;
            let mut by_month = HashMap::new();
            for (ym, mut point) in rows.flatten() {
                point.date = ym.clone();
                by_month.insert(ym, point);
            }
            // 从起始月到结束月逐月补零（含跨年）
            let mut points: Vec<GatewayDailyPoint> = Vec::new();
            let mut y = start.year();
            let mut m = start.month();
            loop {
                let key = format!("{y:04}-{m:02}");
                let mut point = by_month.remove(&key).unwrap_or_default();
                point.date = key;
                points.push(point);
                if y == end.year() && m == end.month() {
                    break;
                }
                m += 1;
                if m > 12 {
                    m = 1;
                    y += 1;
                }
            }
            (points.iter().any(|p| p.total_requests > 0)).then_some(points)
        } else {
            None
        };

        // 汇总口径：区间模式仅累加所选区间；未选区间（全部）保持全量累计
        let totals = {
            let sql = format!(
                "SELECT
                    COALESCE(SUM(total_requests), 0),
                    COALESCE(SUM(successful_requests), 0),
                    COALESCE(SUM(failed_requests), 0),
                    COALESCE(SUM(duration_ms_total), 0),
                    COALESCE(SUM(ttft_ms_total), 0),
                    COALESCE(SUM(ttft_count), 0),
                    COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0),
                    COALESCE(SUM(reasoning_tokens), 0),
                    COALESCE(SUM(cache_hit_tokens), 0),
                    COALESCE(SUM(total_tokens), 0)
                 FROM channel_daily_stats {}",
                if range_mode {
                    "WHERE date >= ?1 AND date <= ?2"
                } else {
                    ""
                }
            );
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| format!("查询汇总失败: {e}"))?;
            let map_row = |row: &rusqlite::Row<'_>| {
                Ok((
                    row.get::<_, i64>(0)?.max(0) as u64,
                    row.get::<_, i64>(1)?.max(0) as u64,
                    row.get::<_, i64>(2)?.max(0) as u64,
                    row.get::<_, i64>(3)?.max(0) as u64,
                    row.get::<_, i64>(4)?.max(0) as u64,
                    row.get::<_, i64>(5)?.max(0) as u64,
                    row.get::<_, i64>(6)?.max(0) as u64,
                    row.get::<_, i64>(7)?.max(0) as u64,
                    row.get::<_, i64>(8)?.max(0) as u64,
                    row.get::<_, i64>(9)?.max(0) as u64,
                    row.get::<_, i64>(10)?.max(0) as u64,
                ))
            };
            let row = if range_mode {
                stmt.query_row(params![&start_str, &end_str], map_row)
                    .map_err(|e| format!("解析汇总失败: {e}"))?
            } else {
                stmt.query_row([], map_row)
                    .map_err(|e| format!("解析汇总失败: {e}"))?
            };
            let (reqs, succ, fail, dur_total, ttft_total, ttft_count, prompt, comp, reasoning, cache, tokens) = row;
            GatewayOverviewTotals {
                total_requests: reqs,
                successful_requests: succ,
                failed_requests: fail,
                avg_duration_ms: dur_total / reqs.max(1),
                avg_ttft_ms: avg_or_none(ttft_total as i64, ttft_count as i64),
                prompt_tokens: prompt,
                completion_tokens: comp,
                reasoning_tokens: reasoning,
                cache_hit_tokens: cache,
                total_tokens: tokens,
            }
        };

        Ok(GatewayOverviewStats {
            days: daily.len() as u32,
            daily,
            totals,
            today: today_point,
            hourly,
            monthly,
        })
    })
}

/// 均值计算：计数为 0 时返回 None。
/// 必须用惰性闭包 `then`——`then_some` 的参数是急切求值，除零会在条件判断前就 panic。
pub(crate) fn avg_or_none(total: i64, count: i64) -> Option<u64> {
    (count > 0).then(|| (total / count) as u64)
}

/// 反代模式 Token 报表：从日/时聚合表生成与本地模式同构的用量桶 + 请求健康，
/// 供 Token 统计中心「反代模式」标签直接复用本地模式的前端聚合层。
///
/// 维度映射（对齐本地模式语义）：
/// - source     = client_name（User-Agent 推断的客户端标识，空 → "其他客户端"）
/// - model      = 模型名（旧版聚合行无模型维度 → "历史聚合"）
/// - projectKey = 渠道显示名（本地模式为项目工作区，反代以渠道维度替代）
/// - input      = prompt - 缓存读 - 缓存写（对齐 Anthropic 本地日志的「未缓存输入」口径）
/// 区间 ≤ 7 天用小时表（逐时桶），更长区间用日表（逐日桶），与前端粒度自动切换一致。
///
/// 前置补偿：本地模式的健康报表是全量历史扫描，健康矩阵在所选区间起点前补位时
/// 仍能取到历史数据；反代模式按区间裁剪后需通过 preceding_buckets 单独带回
/// 区间之前（回看 ≤ 366 天）的请求健康聚合。区间内「有日行但无小时行」的日期
/// （小时表晚于日表启用）以当日 00:00 单桶回退，避免矩阵/趋势前段整体空白。
pub async fn get_proxy_token_usage_report(
    state: &ModelProxyState,
    from: Option<String>,
    to: Option<String>,
) -> Result<ProxyTokenUsageReport, String> {
    use crate::models::{
        RequestHealthBucket, RequestHealthReport, RequestHealthSourceSummary, TokenUsageBucket,
        TokenUsageReport,
    };

    let empty_report = || ProxyTokenUsageReport {
        usage: TokenUsageReport {
            available: false,
            buckets: Vec::new(),
            start_date: String::new(),
            end_date: String::new(),
            pricing_source: String::new(),
        },
        health: RequestHealthReport {
            available: false,
            buckets: Vec::new(),
            preceding_buckets: Vec::new(),
            by_source: Vec::new(),
        },
    };

    let app_handle_opt = state.context.app_handle.read().await.clone();
    let Some(app) = app_handle_opt else {
        return Ok(empty_report());
    };

    // 渠道稳定统计 ID → 展示名映射（projectKey 维度）
    let channel_names: HashMap<String, String> = {
        let cfg = state.context.config.read().await;
        cfg.channels
            .iter()
            .map(|c| (c.stats_key(), c.name.clone()))
            .collect()
    };

    tokio::task::block_in_place(move || {
        let database = app.state::<crate::models::Database>();
        let conn = database.lock_db();

        let today = chrono::Local::now().date_naive();
        let parse = |s: &str| {
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map_err(|e| format!("日期格式无效（应为 YYYY-MM-DD）: {s}, {e}"))
        };
        let end = match to.as_deref() {
            Some(s) => parse(s)?.min(today),
            None => today,
        };
        let start = match from.as_deref() {
            Some(s) => parse(s)?.min(end),
            None => conn
                .query_row(
                    "SELECT COALESCE(MIN(date), '') FROM channel_daily_stats",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .ok()
                .and_then(|d| chrono::NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok())
                .unwrap_or(today),
        };
        let start_str = start.format("%Y-%m-%d").to_string();
        let end_str = end.format("%Y-%m-%d").to_string();
        // 与前端粒度切换一致：< 7 天逐小时，≥ 7 天逐日
        let hourly_mode = (end - start).num_days() + 1 < 7;

        let mut buckets: Vec<TokenUsageBucket> = Vec::new();
        let mut health_map: std::collections::BTreeMap<String, RequestHealthBucket> =
            std::collections::BTreeMap::new();
        let mut by_client: HashMap<String, RequestHealthSourceSummary> = HashMap::new();

        fn ensure_client_summary<'a>(
            map: &'a mut HashMap<String, RequestHealthSourceSummary>,
            client: &str,
        ) -> &'a mut RequestHealthSourceSummary {
            map.entry(client.to_string())
                .or_insert_with(|| RequestHealthSourceSummary {
                    source: client.to_string(),
                    dialogues: 0,
                    requests: 0,
                    success: 0,
                    failed: 0,
                })
        }

        if hourly_mode {
            let mut stmt = conn
                .prepare(
                    "SELECT date, hour, channel_id, model, client_name,
                        SUM(total_requests), SUM(successful_requests), SUM(failed_requests),
                        SUM(prompt_tokens), SUM(completion_tokens), SUM(reasoning_tokens),
                        SUM(cache_hit_tokens), SUM(cache_creation_tokens), SUM(total_tokens)
                     FROM channel_hourly_stats
                     WHERE date >= ?1 AND date <= ?2
                     GROUP BY date, hour, channel_id, model, client_name",
                )
                .map_err(|e| format!("查询反代小时统计失败: {e}"))?;
            let rows = stmt
                .query_map([&start_str, &end_str], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, i64>(9)?,
                        row.get::<_, i64>(10)?,
                        row.get::<_, i64>(11)?,
                        row.get::<_, i64>(12)?,
                        row.get::<_, i64>(13)?,
                    ))
                })
                .map_err(|e| format!("解析反代小时统计失败: {e}"))?;

            // 区间内出现过小时行的日期（供下方日表回退判断）
            let mut hourly_covered: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            for (date, hour, channel, model, client, reqs, succ, fail, prompt, comp, reasoning, hit, creation, total) in
                rows.flatten()
            {
                let client_key = if client.trim().is_empty() { "其他客户端".to_string() } else { client };
                let model_key = if model.trim().is_empty() { "历史聚合".to_string() } else { model };
                let input = (prompt - hit - creation).max(0);
                buckets.push(TokenUsageBucket {
                    source: client_key.clone(),
                    model: model_key,
                    project_key: channel_names
                        .get(&channel)
                        .cloned()
                        .unwrap_or(channel),
                    timestamp: format!("{date}T{hour:02}:00:00"),
                    total_tokens: total,
                    billable_total_tokens: total,
                    input_tokens: input,
                    cached_input_tokens: hit,
                    cache_creation_input_tokens: creation,
                    output_tokens: comp,
                    reasoning_output_tokens: reasoning,
                    conversation_count: 0,
                    request_count: reqs,
                    ..Default::default()
                });

                let hour_key = format!("{date}T{hour:02}:00:00");
                let hb = health_map.entry(hour_key).or_insert_with(|| RequestHealthBucket {
                    hour: String::new(),
                    dialogues: 0,
                    requests: 0,
                    success: 0,
                    failed: 0,
                });
                hb.requests += reqs;
                hb.success += succ;
                hb.failed += fail;

                let summary = ensure_client_summary(&mut by_client, &client_key);
                summary.requests += reqs;
                summary.success += succ;
                summary.failed += fail;
                hourly_covered.insert(date);
            }

            // 区间内补偿：日表有聚合但小时表无行的日期（小时表晚于日表启用），
            // 以当日 00:00 单桶回退，避免区间前段在健康矩阵/趋势中整体空白
            let mut fallback_stmt = conn
                .prepare(
                    "SELECT date, channel_id, model, client_name,
                        SUM(total_requests), SUM(successful_requests), SUM(failed_requests),
                        SUM(prompt_tokens), SUM(completion_tokens), SUM(reasoning_tokens),
                        SUM(cache_hit_tokens), SUM(cache_creation_tokens), SUM(total_tokens)
                     FROM channel_daily_stats
                     WHERE date >= ?1 AND date <= ?2
                     GROUP BY date, channel_id, model, client_name",
                )
                .map_err(|e| format!("查询反代日统计失败: {e}"))?;
            let fallback_rows = fallback_stmt
                .query_map([&start_str, &end_str], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, i64>(9)?,
                        row.get::<_, i64>(10)?,
                        row.get::<_, i64>(11)?,
                        row.get::<_, i64>(12)?,
                    ))
                })
                .map_err(|e| format!("解析反代日统计失败: {e}"))?;

            for (date, channel, model, client, reqs, succ, fail, prompt, comp, reasoning, hit, creation, total) in
                fallback_rows.flatten()
            {
                if hourly_covered.contains(&date) {
                    continue;
                }
                let client_key = if client.trim().is_empty() { "其他客户端".to_string() } else { client };
                let model_key = if model.trim().is_empty() { "历史聚合".to_string() } else { model };
                let input = (prompt - hit - creation).max(0);
                buckets.push(TokenUsageBucket {
                    source: client_key.clone(),
                    model: model_key,
                    project_key: channel_names
                        .get(&channel)
                        .cloned()
                        .unwrap_or(channel),
                    timestamp: format!("{date}T00:00:00"),
                    total_tokens: total,
                    billable_total_tokens: total,
                    input_tokens: input,
                    cached_input_tokens: hit,
                    cache_creation_input_tokens: creation,
                    output_tokens: comp,
                    reasoning_output_tokens: reasoning,
                    conversation_count: 0,
                    request_count: reqs,
                    ..Default::default()
                });

                let hb = health_map.entry(format!("{date}T00:00:00")).or_insert_with(|| RequestHealthBucket {
                    hour: String::new(),
                    dialogues: 0,
                    requests: 0,
                    success: 0,
                    failed: 0,
                });
                hb.requests += reqs;
                hb.success += succ;
                hb.failed += fail;

                let summary = ensure_client_summary(&mut by_client, &client_key);
                summary.requests += reqs;
                summary.success += succ;
                summary.failed += fail;
            }
        } else {
            let mut stmt = conn
                .prepare(
                    "SELECT date, channel_id, model, client_name,
                        SUM(total_requests), SUM(successful_requests), SUM(failed_requests),
                        SUM(prompt_tokens), SUM(completion_tokens), SUM(reasoning_tokens),
                        SUM(cache_hit_tokens), SUM(cache_creation_tokens), SUM(total_tokens)
                     FROM channel_daily_stats
                     WHERE date >= ?1 AND date <= ?2
                     GROUP BY date, channel_id, model, client_name",
                )
                .map_err(|e| format!("查询反代日统计失败: {e}"))?;
            let rows = stmt
                .query_map([&start_str, &end_str], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, i64>(9)?,
                        row.get::<_, i64>(10)?,
                        row.get::<_, i64>(11)?,
                        row.get::<_, i64>(12)?,
                    ))
                })
                .map_err(|e| format!("解析反代日统计失败: {e}"))?;

            for (date, channel, model, client, reqs, succ, fail, prompt, comp, reasoning, hit, creation, total) in
                rows.flatten()
            {
                let client_key = if client.trim().is_empty() { "其他客户端".to_string() } else { client };
                let model_key = if model.trim().is_empty() { "历史聚合".to_string() } else { model };
                let input = (prompt - hit - creation).max(0);
                buckets.push(TokenUsageBucket {
                    source: client_key.clone(),
                    model: model_key,
                    project_key: channel_names
                        .get(&channel)
                        .cloned()
                        .unwrap_or(channel),
                    timestamp: format!("{date}T00:00:00"),
                    total_tokens: total,
                    billable_total_tokens: total,
                    input_tokens: input,
                    cached_input_tokens: hit,
                    cache_creation_input_tokens: creation,
                    output_tokens: comp,
                    reasoning_output_tokens: reasoning,
                    conversation_count: 0,
                    request_count: reqs,
                    ..Default::default()
                });

                let hb = health_map.entry(format!("{date}T00:00:00")).or_insert_with(|| RequestHealthBucket {
                    hour: String::new(),
                    dialogues: 0,
                    requests: 0,
                    success: 0,
                    failed: 0,
                });
                hb.requests += reqs;
                hb.success += succ;
                hb.failed += fail;

                let summary = ensure_client_summary(&mut by_client, &client_key);
                summary.requests += reqs;
                summary.success += succ;
                summary.failed += fail;
            }
        }

        // —— 前置补偿：所选区间之前的请求健康聚合（preceding_buckets）——
        // 健康矩阵在区间起点前补位时由此取数（对齐本地模式的全量历史行为）。
        // 回看上限 366 天，远超矩阵可视容量；小时粒度优先取小时表，
        // 无小时行的日期（含小时表启用前）以日表 00:00 单桶回退。
        let lookback_str = (start - chrono::Duration::days(366))
            .format("%Y-%m-%d")
            .to_string();
        let mut preceding_map: std::collections::BTreeMap<String, RequestHealthBucket> =
            std::collections::BTreeMap::new();
        let mut preceding_hourly_covered: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        if hourly_mode {
            let mut stmt = conn
                .prepare(
                    "SELECT date, hour,
                        SUM(total_requests), SUM(successful_requests), SUM(failed_requests)
                     FROM channel_hourly_stats
                     WHERE date >= ?1 AND date < ?2
                     GROUP BY date, hour",
                )
                .map_err(|e| format!("查询前置小时统计失败: {e}"))?;
            let rows = stmt
                .query_map([&lookback_str, &start_str], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                })
                .map_err(|e| format!("解析前置小时统计失败: {e}"))?;
            for (date, hour, reqs, succ, fail) in rows.flatten() {
                preceding_hourly_covered.insert(date.clone());
                let hb = preceding_map
                    .entry(format!("{date}T{hour:02}:00:00"))
                    .or_insert_with(|| RequestHealthBucket {
                        hour: String::new(),
                        dialogues: 0,
                        requests: 0,
                        success: 0,
                        failed: 0,
                    });
                hb.requests += reqs;
                hb.success += succ;
                hb.failed += fail;
            }
        }
        let mut stmt = conn
            .prepare(
                "SELECT date,
                    SUM(total_requests), SUM(successful_requests), SUM(failed_requests)
                 FROM channel_daily_stats
                 WHERE date >= ?1 AND date < ?2
                 GROUP BY date",
            )
            .map_err(|e| format!("查询前置日统计失败: {e}"))?;
        let rows = stmt
            .query_map([&lookback_str, &start_str], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(|e| format!("解析前置日统计失败: {e}"))?;
        for (date, reqs, succ, fail) in rows.flatten() {
            if preceding_hourly_covered.contains(&date) {
                continue;
            }
            let hb = preceding_map
                .entry(format!("{date}T00:00:00"))
                .or_insert_with(|| RequestHealthBucket {
                    hour: String::new(),
                    dialogues: 0,
                    requests: 0,
                    success: 0,
                    failed: 0,
                });
            hb.requests += reqs;
            hb.success += succ;
            hb.failed += fail;
        }

        let health_buckets: Vec<RequestHealthBucket> = health_map
            .into_iter()
            .map(|(key, mut b)| {
                b.hour = key;
                b
            })
            .collect();
        let has_data = !buckets.is_empty() || health_buckets.iter().any(|b| b.requests > 0);

        Ok(ProxyTokenUsageReport {
            usage: TokenUsageReport {
                available: has_data,
                buckets,
                start_date: start_str,
                end_date: end_str,
                pricing_source: String::new(),
            },
            health: RequestHealthReport {
                available: has_data,
                buckets: health_buckets,
                preceding_buckets: preceding_map
                    .into_iter()
                    .map(|(key, mut b)| {
                        b.hour = key;
                        b
                    })
                    .collect(),
                by_source: by_client.into_values().collect(),
            },
        })
    })
}

#[cfg(test)]
mod stats_tests {
    use super::avg_or_none;

    #[test]
    fn avg_or_none_handles_zero_count_without_panicking() {
        assert_eq!(avg_or_none(1234, 0), None);
        assert_eq!(avg_or_none(0, 0), None);
        assert_eq!(avg_or_none(1200, 3), Some(400));
    }
}
