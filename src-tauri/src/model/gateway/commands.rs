use super::config::{load_model_proxy_config, save_model_proxy_config};
use super::router::fetch_upstream_models_inner;
use super::server::{start_model_proxy_server, stop_model_proxy_server};
use super::stats::{
    get_channel_usage_stats_summary, get_gateway_overview_stats, get_model_proxy_status_summary,
};
use super::types::{
    ChannelConfig, ChannelModelFetchError, ChannelModelList, ChannelUsageStats, GatewayOverviewStats,
    ModelProxyConfig, ModelProxyState, ModelProxyStatus, OpencodeProxyConfig, OpencodeProxyState,
    OpencodeProxyStatus, ProxyRequestLog,
};
use crate::models::Database;
use serde_json::Value as JsonValue;
use tauri::State;

#[tauri::command]
pub async fn get_model_proxy_config(
    database: State<'_, Database>,
) -> Result<ModelProxyConfig, String> {
    let conn = database
        .0
        .lock()
        .map_err(|_| "获取数据库连接失败".to_string())?;
    Ok(load_model_proxy_config(&conn))
}

#[tauri::command]
pub async fn get_opencode_proxy_config(
    database: State<'_, Database>,
) -> Result<OpencodeProxyConfig, String> {
    get_model_proxy_config(database).await
}

#[tauri::command]
pub async fn save_model_proxy_config_cmd(
    database: State<'_, Database>,
    state: State<'_, ModelProxyState>,
    config: ModelProxyConfig,
) -> Result<ModelProxyStatus, String> {
    {
        let conn = database
            .0
            .lock()
            .map_err(|_| "获取数据库连接失败".to_string())?;
        save_model_proxy_config(&conn, &config)?;
    }
    *state.context.config.write().await = config.clone();

    let is_running = state.shutdown_sender.read().await.is_some();
    if config.enabled {
        // 端口未变化时无需重启：白名单等配置均在请求时热读取，重建 listener 反而引入端口释放竞态
        let port_changed = *state.current_port.read().await != config.port;
        if !is_running {
            start_model_proxy_server(&state).await?;
        } else if port_changed {
            stop_model_proxy_server(&state).await?;
            start_model_proxy_server(&state).await?;
        }
    } else if is_running {
        stop_model_proxy_server(&state).await?;
    }
    Ok(get_model_proxy_status_summary(&state).await)
}

#[tauri::command]
pub async fn save_opencode_proxy_config_cmd(
    database: State<'_, Database>,
    state: State<'_, OpencodeProxyState>,
    config: OpencodeProxyConfig,
) -> Result<OpencodeProxyStatus, String> {
    save_model_proxy_config_cmd(database, state, config).await
}

#[tauri::command]
pub async fn get_model_proxy_status(
    state: State<'_, ModelProxyState>,
) -> Result<ModelProxyStatus, String> {
    Ok(get_model_proxy_status_summary(&state).await)
}

#[tauri::command]
pub async fn get_opencode_proxy_status(
    state: State<'_, OpencodeProxyState>,
) -> Result<OpencodeProxyStatus, String> {
    get_model_proxy_status(state).await
}

#[tauri::command]
pub async fn start_model_proxy(
    state: State<'_, ModelProxyState>,
) -> Result<ModelProxyStatus, String> {
    start_model_proxy_server(&state).await?;
    Ok(get_model_proxy_status_summary(&state).await)
}

#[tauri::command]
pub async fn start_opencode_proxy(
    state: State<'_, OpencodeProxyState>,
) -> Result<OpencodeProxyStatus, String> {
    start_model_proxy(state).await
}

#[tauri::command]
pub async fn stop_model_proxy(
    state: State<'_, ModelProxyState>,
) -> Result<ModelProxyStatus, String> {
    stop_model_proxy_server(&state).await?;
    Ok(get_model_proxy_status_summary(&state).await)
}

#[tauri::command]
pub async fn stop_opencode_proxy(
    state: State<'_, OpencodeProxyState>,
) -> Result<OpencodeProxyStatus, String> {
    stop_model_proxy(state).await
}

#[tauri::command]
pub async fn fetch_model_proxy_models(
    state: State<'_, ModelProxyState>,
) -> Result<(Vec<ChannelModelList>, Vec<ChannelModelFetchError>), String> {
    fetch_upstream_models_inner(&state.context).await;
    let models = state.context.cached_channel_models.read().await.clone();
    let errors = state.context.cached_fetch_errors.read().await.clone();
    Ok((models, errors))
}

/// 只读取内存中已缓存的渠道模型列表，不发起远程请求。
/// 用于页面加载时默认展示已有数据，而非每次打开都远程拉取。
#[tauri::command]
pub async fn get_cached_channel_models(
    state: State<'_, ModelProxyState>,
) -> Result<Vec<ChannelModelList>, String> {
    let models = state.context.cached_channel_models.read().await.clone();
    Ok(models)
}

/// 只读取内存中已缓存的渠道模型拉取错误，不发起远程请求。
#[tauri::command]
pub async fn get_cached_channel_errors(
    state: State<'_, ModelProxyState>,
) -> Result<Vec<ChannelModelFetchError>, String> {
    let errors = state.context.cached_fetch_errors.read().await.clone();
    Ok(errors)
}

#[tauri::command]
pub async fn fetch_opencode_models(
    state: State<'_, OpencodeProxyState>,
) -> Result<(Vec<ChannelModelList>, Vec<ChannelModelFetchError>), String> {
    fetch_model_proxy_models(state).await
}

/// opencode 别名：只读取内存中已缓存的渠道模型列表，不发起远程请求。
#[tauri::command]
pub async fn get_opencode_cached_channel_models(
    state: State<'_, OpencodeProxyState>,
) -> Result<Vec<ChannelModelList>, String> {
    get_cached_channel_models(state).await
}

/// opencode 别名：只读取内存中已缓存的渠道模型拉取错误，不发起远程请求。
#[tauri::command]
pub async fn get_opencode_cached_channel_errors(
    state: State<'_, OpencodeProxyState>,
) -> Result<Vec<ChannelModelFetchError>, String> {
    get_cached_channel_errors(state).await
}

#[tauri::command]
pub async fn test_model_proxy_health(
    state: State<'_, ModelProxyState>,
) -> Result<serde_json::Value, String> {
    let port = *state.current_port.read().await;
    let url = format!("http://127.0.0.1:{port}/healthz");
    let resp = state
        .context
        .default_http_client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("健康检测请求失败: {e}"))?;

    let json_val = resp
        .json::<serde_json::Value>()
        .await
        .map_err(|e| format!("解析健康检测响应失败: {e}"))?;
    Ok(json_val)
}

#[tauri::command]
pub async fn test_opencode_proxy_health(
    state: State<'_, OpencodeProxyState>,
) -> Result<serde_json::Value, String> {
    test_model_proxy_health(state).await
}

#[tauri::command]
pub async fn get_model_proxy_logs(
    database: State<'_, Database>,
    page: Option<usize>,
    page_size: Option<usize>,
    filter: Option<String>,
    q: Option<String>,
    sort_by: Option<String>,
    sort_order: Option<String>,
    from: Option<String>,
    to: Option<String>,
) -> Result<super::types::ProxyLogsResponse, String> {
    let p = page.unwrap_or(1).max(1);
    let ps = page_size.unwrap_or(50).clamp(1, 200);
    let offset = (p - 1) * ps;
    let filter_str = filter.unwrap_or_else(|| "all".to_string());
    let query_str = q.unwrap_or_default().trim().to_string();
    let date_from_str = from.unwrap_or_default().trim().to_string();
    let date_to_str = to.unwrap_or_default().trim().to_string();

    let sort_col = match sort_by.as_deref() {
        Some("status_code") | Some("statusCode") => "status_code",
        Some("duration_ms") | Some("durationMs") => "duration_ms",
        Some("total_tokens") | Some("totalTokens") => "COALESCE(total_tokens, 0)",
        Some("channel_id") | Some("channelId") => "channel_id",
        Some("model") => "model",
        _ => "timestamp",
    };
    let sort_dir = match sort_order.as_deref() {
        Some("asc") | Some("ASC") => "ASC",
        _ => "DESC",
    };

    let conn = database
        .0
        .lock()
        .map_err(|_| "获取数据库连接失败".to_string())?;

    let mut where_clauses = Vec::new();
    let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if filter_str == "success" {
        where_clauses.push("status_code >= 200 AND status_code < 300".to_string());
    } else if filter_str == "error" {
        where_clauses.push("status_code >= 400".to_string());
    }

    // 时间范围过滤：from/to 均为 YYYY-MM-DD 日期，精确到日，闭区间。
    // 纯日期条件单独留存一份，供全局计数使用（顶部标签数随所选区间变化）
    let mut date_conds: Vec<&'static str> = Vec::new();
    let mut date_only_params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if !date_from_str.is_empty() {
        date_conds.push("timestamp >= ?");
        where_clauses.push("timestamp >= ?".to_string());
        let bound = format!("{date_from_str} 00:00:00");
        params_vec.push(Box::new(bound.clone()));
        date_only_params.push(Box::new(bound));
    }
    if !date_to_str.is_empty() {
        date_conds.push("timestamp <= ?");
        where_clauses.push("timestamp <= ?".to_string());
        let bound = format!("{date_to_str} 23:59:59");
        params_vec.push(Box::new(bound.clone()));
        date_only_params.push(Box::new(bound));
    }
    let date_where_sql = if date_conds.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", date_conds.join(" AND "))
    };

    if !query_str.is_empty() {
        where_clauses.push("(model LIKE ? OR channel_id LIKE ? OR node_name LIKE ? OR error_message LIKE ?)".to_string());
        let pat = format!("%{query_str}%");
        params_vec.push(Box::new(pat.clone()));
        params_vec.push(Box::new(pat.clone()));
        params_vec.push(Box::new(pat.clone()));
        params_vec.push(Box::new(pat));
    }

    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_clauses.join(" AND "))
    };

    let count_filtered: usize = {
        let sql = format!("SELECT COUNT(*) FROM model_proxy_logs {where_sql}");
        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
        conn.query_row(&sql, params_refs.as_slice(), |r| r.get(0))
            .unwrap_or(0)
    };

    // 当前筛选条件下的成功/错误计数（供表格底部/当前视图使用）
    let count_succ: usize = {
        let status_sql = "status_code >= 200 AND status_code < 300";
        let sql = if where_sql.is_empty() {
            format!("SELECT COUNT(*) FROM model_proxy_logs WHERE {status_sql}")
        } else {
            format!("SELECT COUNT(*) FROM model_proxy_logs {where_sql} AND {status_sql}")
        };
        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
        conn.query_row(&sql, params_refs.as_slice(), |r| r.get(0))
            .unwrap_or(0)
    };

    // 当前筛选条件下的错误计数
    let count_err: usize = {
        let status_sql = "status_code >= 400";
        let sql = if where_sql.is_empty() {
            format!("SELECT COUNT(*) FROM model_proxy_logs WHERE {status_sql}")
        } else {
            format!("SELECT COUNT(*) FROM model_proxy_logs {where_sql} AND {status_sql}")
        };
        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
        conn.query_row(&sql, params_refs.as_slice(), |r| r.get(0))
            .unwrap_or(0)
    };

    // 顶部标签统计：不受状态筛选与搜索关键词影响，但随所选日期区间变化
    let global_total: usize = {
        let sql = format!("SELECT COUNT(*) FROM model_proxy_logs {date_where_sql}");
        let params_refs: Vec<&dyn rusqlite::ToSql> = date_only_params.iter().map(|b| b.as_ref()).collect();
        conn.query_row(&sql, params_refs.as_slice(), |r| r.get(0))
            .unwrap_or(0)
    };
    let global_succ: usize = {
        let status_sql = "status_code >= 200 AND status_code < 300";
        let sql = if date_where_sql.is_empty() {
            format!("SELECT COUNT(*) FROM model_proxy_logs WHERE {status_sql}")
        } else {
            format!("SELECT COUNT(*) FROM model_proxy_logs {date_where_sql} AND {status_sql}")
        };
        let params_refs: Vec<&dyn rusqlite::ToSql> = date_only_params.iter().map(|b| b.as_ref()).collect();
        conn.query_row(&sql, params_refs.as_slice(), |r| r.get(0))
            .unwrap_or(0)
    };
    let global_err: usize = {
        let status_sql = "status_code >= 400";
        let sql = if date_where_sql.is_empty() {
            format!("SELECT COUNT(*) FROM model_proxy_logs WHERE {status_sql}")
        } else {
            format!("SELECT COUNT(*) FROM model_proxy_logs {date_where_sql} AND {status_sql}")
        };
        let params_refs: Vec<&dyn rusqlite::ToSql> = date_only_params.iter().map(|b| b.as_ref()).collect();
        conn.query_row(&sql, params_refs.as_slice(), |r| r.get(0))
            .unwrap_or(0)
    };

    let query_sql = format!(
        "SELECT id, timestamp, method, path, channel_id, model, stream, status_code,
                duration_ms, ttft_ms, prompt_tokens, prompt_cache_hit_tokens,
                prompt_cache_miss_tokens, cache_creation_tokens, completion_tokens,
                reasoning_tokens, total_tokens,
                error_message, request_body, response_body, node_name, client_name
         FROM model_proxy_logs
         {where_sql}
         ORDER BY {sort_col} {sort_dir}, rowid DESC
         LIMIT ? OFFSET ?",
    );

    let mut stmt = conn
        .prepare(&query_sql)
        .map_err(|e| format!("查询日志失败: {e}"))?;

    let mut query_params: Vec<Box<dyn rusqlite::ToSql>> = params_vec;
    query_params.push(Box::new(ps as i64));
    query_params.push(Box::new(offset as i64));

    let query_refs: Vec<&dyn rusqlite::ToSql> = query_params.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(query_refs.as_slice(), |row| {
            let stream_int: i64 = row.get(6)?;
            let status_int: i64 = row.get(7)?;
            let dur_int: i64 = row.get(8)?;

            Ok(ProxyRequestLog {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                method: row.get(2)?,
                path: row.get(3)?,
                channel_id: row.get(4)?,
                // 日志表不落统计 ID：该结构仅供前端展示，不再回写日统计
                channel_stats_id: None,
                model: row.get(5)?,
                stream: stream_int == 1,
                status_code: status_int as u16,
                duration_ms: dur_int as u64,
                ttft_ms: row.get::<_, Option<i64>>(9)?.map(|v| v as u64),
                prompt_tokens: row.get::<_, Option<i64>>(10)?.map(|v| v as u64),
                prompt_cache_hit_tokens: row.get::<_, Option<i64>>(11)?.map(|v| v as u64),
                prompt_cache_miss_tokens: row.get::<_, Option<i64>>(12)?.map(|v| v as u64),
                cache_creation_tokens: row.get::<_, Option<i64>>(13)?.map(|v| v as u64),
                completion_tokens: row.get::<_, Option<i64>>(14)?.map(|v| v as u64),
                reasoning_tokens: row.get::<_, Option<i64>>(15)?.map(|v| v as u64),
                total_tokens: row.get::<_, Option<i64>>(16)?.map(|v| v as u64),
                error_message: row.get(17)?,
                request_body: row.get(18)?,
                response_body: row.get(19)?,
                node_name: row.get(20)?,
                client_name: row.get(21)?,
            })
        })
        .map_err(|e| format!("解析日志失败: {e}"))?;

    let logs = rows.filter_map(Result::ok).collect();
    Ok(super::types::ProxyLogsResponse {
        items: logs,
        total: count_filtered,
        global_total,
        global_success: global_succ,
        global_error: global_err,
        success_total: count_succ,
        error_total: count_err,
    })
}

#[tauri::command]
pub async fn get_opencode_proxy_logs(
    database: State<'_, Database>,
    page: Option<usize>,
    page_size: Option<usize>,
    filter: Option<String>,
    q: Option<String>,
    sort_by: Option<String>,
    sort_order: Option<String>,
    from: Option<String>,
    to: Option<String>,
) -> Result<super::types::ProxyLogsResponse, String> {
    get_model_proxy_logs(database, page, page_size, filter, q, sort_by, sort_order, from, to).await
}

#[tauri::command]
pub async fn get_model_proxy_channel_stats(
    state: State<'_, ModelProxyState>,
) -> Result<Vec<ChannelUsageStats>, String> {
    get_channel_usage_stats_summary(&state).await
}

/// Token 统计中心「反代模式」数据源：与本地模式同构的用量桶 + 请求健康报表。
/// from/to 均为 YYYY-MM-DD（可省略，默认全部已聚合数据）。
#[tauri::command]
pub async fn get_proxy_token_usage(
    state: State<'_, ModelProxyState>,
    from: Option<String>,
    to: Option<String>,
) -> Result<super::types::ProxyTokenUsageReport, String> {
    super::stats::get_proxy_token_usage_report(&state, from, to).await
}

#[tauri::command]
pub async fn get_opencode_channel_stats(
    state: State<'_, OpencodeProxyState>,
) -> Result<Vec<ChannelUsageStats>, String> {
    get_model_proxy_channel_stats(state).await
}

/// 控制台「全渠道数据总览」：日期区间逐日聚合 + 区间累计（未传区间时为近 N 天 + 全量，默认 14 天）
#[tauri::command]
pub async fn get_model_proxy_overview_stats(
    state: State<'_, ModelProxyState>,
    days: Option<u32>,
    from: Option<String>,
    to: Option<String>,
) -> Result<GatewayOverviewStats, String> {
    get_gateway_overview_stats(&state, days, from, to).await
}

/// 清理请求明细日志。统计聚合表（channel_daily_stats / channel_hourly_stats）独立持久化，不受影响。
/// - mode="all"（默认）：删除明细行；mode="payload_only"：仅置空请求/响应正文，保留元数据
/// - before（YYYY-MM-DD）：只清理该日期之前（不含当日）的明细；未提供时清理全部
/// 返回受影响的行数。
#[tauri::command]
pub async fn clear_model_proxy_logs(
    database: State<'_, Database>,
    mode: Option<String>,
    before: Option<String>,
) -> Result<u64, String> {
    let conn = database
        .0
        .lock()
        .map_err(|_| "获取数据库连接失败".to_string())?;
    let clear_mode = mode.as_deref().unwrap_or("all");

    // 归一化边界：before 为纯日期，统一转成「当日 00:00:00」开区间（清理该日之前的内容）
    let cutoff = before
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!("{s} 00:00:00"));

    let (sql, params): (&str, Vec<Box<dyn rusqlite::ToSql>>) = match (clear_mode, &cutoff) {
        ("payload_only", Some(bound)) => (
            "UPDATE model_proxy_logs SET request_body = NULL, response_body = NULL WHERE timestamp < ?",
            vec![Box::new(bound.clone())],
        ),
        ("payload_only", None) => (
            "UPDATE model_proxy_logs SET request_body = NULL, response_body = NULL",
            vec![],
        ),
        (_, Some(bound)) => (
            "DELETE FROM model_proxy_logs WHERE timestamp < ?",
            vec![Box::new(bound.clone())],
        ),
        (_, None) => ("DELETE FROM model_proxy_logs", vec![]),
    };

    let params_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let affected = conn
        .execute(sql, params_refs.as_slice())
        .map_err(|e| format!("清理日志记录失败: {e}"))?;
    Ok(affected as u64)
}

#[tauri::command]
pub async fn clear_opencode_proxy_logs(
    database: State<'_, Database>,
    mode: Option<String>,
    before: Option<String>,
) -> Result<u64, String> {
    clear_model_proxy_logs(database, mode, before).await
}

/// 从站点库 (Site Library) 深度同步渠道配置：自动读取已存活站点的 Base URL、API Keys 与可用模型
#[tauri::command]
pub async fn sync_model_proxy_site_channels(
    database: State<'_, Database>,
    state: State<'_, ModelProxyState>,
    site_ids: Option<Vec<String>>,
) -> Result<ModelProxyConfig, String> {
    let updated_config = tokio::task::block_in_place(|| -> Result<ModelProxyConfig, String> {
        let conn = database
            .0
            .lock()
            .map_err(|_| "获取数据库连接失败".to_string())?;

        let mut current_config = load_model_proxy_config(&conn);

        // 查询站点库数据
        let mut stmt = conn
            .prepare(
                "SELECT ds.id, ds.name, ds.api_base_url, smc.keys_json, smc.models_json
                 FROM directory_sites ds
                 LEFT JOIN site_model_cache smc ON smc.site_id = ds.id",
            )
            .map_err(|e| format!("查询站点库失败: {e}"))?;

        let rows = stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let name: String = row.get(1)?;
                let api_base_url: String = row.get(2)?;
                let keys_json: String = row.get::<_, Option<String>>(3)?.unwrap_or_default();
                let models_json: String = row.get::<_, Option<String>>(4)?.unwrap_or_default();
                Ok((id, name, api_base_url, keys_json, models_json))
            })
            .map_err(|e| format!("读取站点记录失败: {e}"))?;

        let filter_set: Option<std::collections::HashSet<String>> = site_ids.map(|s| s.into_iter().collect());

        for item in rows.flatten() {
            let (site_id, site_name, base_url, keys_raw, models_raw) = item;
            if base_url.trim().is_empty() {
                continue;
            }
            if let Some(ref set) = filter_set {
                if !set.contains(&site_id) {
                    continue;
                }
            }

            // 解析 API Keys
            let mut parsed_keys = Vec::new();
            if let Ok(jv) = serde_json::from_str::<JsonValue>(&keys_raw) {
                if let Some(arr) = jv.as_array() {
                    for k in arr {
                        if let Some(s) = k.as_str() {
                            if !s.trim().is_empty() {
                                parsed_keys.push(s.trim().to_string());
                            }
                        } else if let Some(key_val) = k.get("key").and_then(JsonValue::as_str) {
                            if !key_val.trim().is_empty() {
                                parsed_keys.push(key_val.trim().to_string());
                            }
                        }
                    }
                }
            }

            // 解析 Models
            let mut parsed_models = Vec::new();
            if let Ok(jv) = serde_json::from_str::<JsonValue>(&models_raw) {
                if let Some(arr) = jv.as_array() {
                    for m in arr {
                        if let Some(s) = m.as_str() {
                            parsed_models.push(s.to_string());
                        } else if let Some(id_val) = m.get("id").and_then(JsonValue::as_str) {
                            parsed_models.push(id_val.to_string());
                        }
                    }
                }
            }

            // 查找或更新对应 channel
            if let Some(existing) = current_config.channels.iter_mut().find(|c| c.site_id.as_deref() == Some(&site_id) || c.id == site_id) {
                existing.base_url = base_url;
                if !parsed_keys.is_empty() {
                    existing.api_key = parsed_keys[0].clone();
                    existing.api_keys = Some(parsed_keys);
                }
                if !parsed_models.is_empty() && existing.enabled_models.is_none() {
                    existing.enabled_models = Some(parsed_models);
                }
            } else if filter_set.is_some() {
                // 用户主动要求同步该站点，自动新增 channel
                let channel_id = format!("site_{site_id}");
                let api_key = parsed_keys.first().cloned().unwrap_or_default();
                current_config.channels.push(ChannelConfig {
                    id: channel_id.clone(),
                    name: site_name,
                    description: format!("从站点库自动同步关联通道"),
                    enabled: true,
                    protocol: "openai".to_string(),
                    base_url,
                    api_key,
                    api_keys: if parsed_keys.is_empty() { None } else { Some(parsed_keys) },
                    use_proxy_pool: false,
                    alias: Some(site_id.clone()),
                    site_id: Some(site_id),
                    use_fixed_proxy: false,
                    fixed_proxy_node: None,
                    priority: Some(5),
                    weight: Some(100),
                    enabled_models: if parsed_models.is_empty() { None } else { Some(parsed_models) },
                    model_redirects: None,
                    rate_limit_rpm: None,
                    stats_id: None,
                });
            }
        }

        save_model_proxy_config(&conn, &current_config)?;
        Ok(current_config)
    })?;

    *state.context.config.write().await = updated_config.clone();

    Ok(updated_config)
}

#[tauri::command]
pub async fn sync_opencode_site_channels(
    database: State<'_, Database>,
    state: State<'_, OpencodeProxyState>,
    site_ids: Option<Vec<String>>,
) -> Result<OpencodeProxyConfig, String> {
    sync_model_proxy_site_channels(database, state, site_ids).await
}

