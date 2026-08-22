use super::types::{
    ChannelUsageStats, GatewayDailyPoint, GatewayHourlyPoint, GatewayOverviewStats,
    GatewayOverviewTotals, ModelProxyContext, ModelProxyStatus, ModelProxyState,
    OpencodeProxyState, OpencodeProxyStatus, ProxyRequestLog,
};
use chrono::Datelike;
use rusqlite::params;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use tauri::Manager;

impl ModelProxyContext {
    /// 异步记录请求日志至本地数据库，并同步累加「渠道 × 日」聚合统计
    pub async fn record_log(&self, log: ProxyRequestLog) {
        let app_handle_opt = self.app_handle.read().await.clone();
        if let Some(app) = app_handle_opt {
            let database = app.state::<crate::models::Database>();
            let now_millis = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let _ = (move || -> Result<(), rusqlite::Error> {
                let conn = database.lock_db();
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

                // 「渠道 × 日」与「渠道 × 时」聚合：日志表会裁剪，长期统计依赖这两表。
                // 维度为渠道稳定数字 ID（channel_stats_id），改别名不错位；缺省回退日志渠道列
                let today = chrono::Local::now().format("%Y-%m-%d").to_string();
                let is_success = if log.status_code < 400 { 1 } else { 0 };
                let is_failure = 1 - is_success;
                let stats_key = log
                    .channel_stats_id
                    .clone()
                    .unwrap_or_else(|| log.channel_id.clone());
                conn.execute(
                    "INSERT INTO channel_daily_stats (
                        date, channel_id, total_requests, successful_requests, failed_requests,
                        duration_ms_total, ttft_ms_total, ttft_count,
                        prompt_tokens, completion_tokens, reasoning_tokens, cache_hit_tokens, total_tokens
                    ) VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                    ON CONFLICT(date, channel_id) DO UPDATE SET
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
                        total_tokens = total_tokens + excluded.total_tokens",
                    params![
                        today,
                        stats_key.clone(),
                        is_success,
                        is_failure,
                        log.duration_ms as i64,
                        log.ttft_ms.unwrap_or(0) as i64,
                        if log.ttft_ms.is_some() { 1 } else { 0 },
                        log.prompt_tokens.unwrap_or(0) as i64,
                        log.completion_tokens.unwrap_or(0) as i64,
                        log.reasoning_tokens.unwrap_or(0) as i64,
                        log.prompt_cache_hit_tokens.unwrap_or(0) as i64,
                        log.total_tokens.unwrap_or(0) as i64,
                    ],
                )?;
                // 小时粒度：供单日区间 24 小时趋势。小时取自日志时间戳（本地时间文本）
                let hour: i64 = log
                    .timestamp
                    .get(11..13)
                    .and_then(|h| h.parse::<i64>().ok())
                    .unwrap_or(0);
                conn.execute(
                    "INSERT INTO channel_hourly_stats (
                        date, hour, channel_id, total_requests, successful_requests, failed_requests,
                        prompt_tokens, completion_tokens, reasoning_tokens, cache_hit_tokens, total_tokens
                    ) VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                    ON CONFLICT(date, hour, channel_id) DO UPDATE SET
                        total_requests = total_requests + 1,
                        successful_requests = successful_requests + excluded.successful_requests,
                        failed_requests = failed_requests + excluded.failed_requests,
                        prompt_tokens = prompt_tokens + excluded.prompt_tokens,
                        completion_tokens = completion_tokens + excluded.completion_tokens,
                        reasoning_tokens = reasoning_tokens + excluded.reasoning_tokens,
                        cache_hit_tokens = cache_hit_tokens + excluded.cache_hit_tokens,
                        total_tokens = total_tokens + excluded.total_tokens",
                    params![
                        today,
                        hour,
                        stats_key,
                        is_success,
                        is_failure,
                        log.prompt_tokens.unwrap_or(0) as i64,
                        log.completion_tokens.unwrap_or(0) as i64,
                        log.reasoning_tokens.unwrap_or(0) as i64,
                        log.prompt_cache_hit_tokens.unwrap_or(0) as i64,
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

                // 今日 Token：从持久化日统计表取「今日 × 全渠道」汇总。
                // 不能走日志表——timestamp 为本地时间文本，按 epoch 秒比较会恒为 0，
                // 且日志表会裁剪到最近 1000 条，长期数据不可靠。
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
