use super::config::{load_model_proxy_config, save_model_proxy_config};
use super::router::fetch_upstream_models_inner;
use super::server::{start_model_proxy_server, stop_model_proxy_server};
use super::stats::{get_channel_usage_stats_summary, get_model_proxy_status_summary};
use super::types::{
    ChannelConfig, ChannelModelFetchError, ChannelModelList, ChannelUsageStats, ModelProxyConfig,
    ModelProxyState, ModelProxyStatus, OpencodeProxyConfig, OpencodeProxyState, OpencodeProxyStatus,
    ProxyRequestLog,
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
        if is_running {
            stop_model_proxy_server(&state).await?;
            start_model_proxy_server(&state).await?;
        } else {
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

#[tauri::command]
pub async fn fetch_opencode_models(
    state: State<'_, OpencodeProxyState>,
) -> Result<(Vec<ChannelModelList>, Vec<ChannelModelFetchError>), String> {
    fetch_model_proxy_models(state).await
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
) -> Result<super::types::ProxyLogsResponse, String> {
    let p = page.unwrap_or(1).max(1);
    let ps = page_size.unwrap_or(50).clamp(1, 200);
    let offset = (p - 1) * ps;
    let filter_str = filter.unwrap_or_else(|| "all".to_string());
    let query_str = q.unwrap_or_default().trim().to_string();

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
        let sql = format!("SELECT COUNT(*) FROM opencode_proxy_logs {where_sql}");
        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
        conn.query_row(&sql, params_refs.as_slice(), |r| r.get(0))
            .unwrap_or(0)
    };

    let count_succ: usize = {
        let sql = format!("SELECT COUNT(*) FROM opencode_proxy_logs {where_sql} AND status_code >= 200 AND status_code < 300");
        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
        conn.query_row(&sql, params_refs.as_slice(), |r| r.get(0))
            .unwrap_or(0)
    };

    let count_err: usize = {
        let sql = format!("SELECT COUNT(*) FROM opencode_proxy_logs {where_sql} AND status_code >= 400");
        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
        conn.query_row(&sql, params_refs.as_slice(), |r| r.get(0))
            .unwrap_or(0)
    };

    let count_all: usize = conn
        .query_row("SELECT COUNT(*) FROM opencode_proxy_logs", [], |r| r.get(0))
        .unwrap_or(0);

    let query_sql = format!(
        "SELECT id, timestamp, method, path, channel_id, model, stream, status_code,
                duration_ms, ttft_ms, prompt_tokens, prompt_cache_hit_tokens,
                prompt_cache_miss_tokens, completion_tokens, reasoning_tokens, total_tokens,
                error_message, request_body, response_body, node_name
         FROM opencode_proxy_logs
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
                model: row.get(5)?,
                stream: stream_int == 1,
                status_code: status_int as u16,
                duration_ms: dur_int as u64,
                ttft_ms: row.get::<_, Option<i64>>(9)?.map(|v| v as u64),
                prompt_tokens: row.get::<_, Option<i64>>(10)?.map(|v| v as u64),
                prompt_cache_hit_tokens: row.get::<_, Option<i64>>(11)?.map(|v| v as u64),
                prompt_cache_miss_tokens: row.get::<_, Option<i64>>(12)?.map(|v| v as u64),
                completion_tokens: row.get::<_, Option<i64>>(13)?.map(|v| v as u64),
                reasoning_tokens: row.get::<_, Option<i64>>(14)?.map(|v| v as u64),
                total_tokens: row.get::<_, Option<i64>>(15)?.map(|v| v as u64),
                error_message: row.get(16)?,
                request_body: row.get(17)?,
                response_body: row.get(18)?,
                node_name: row.get(19)?,
            })
        })
        .map_err(|e| format!("解析日志失败: {e}"))?;

    let logs = rows.filter_map(Result::ok).collect();
    Ok(super::types::ProxyLogsResponse {
        items: logs,
        total: count_filtered,
        global_total: count_all,
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
) -> Result<super::types::ProxyLogsResponse, String> {
    get_model_proxy_logs(database, page, page_size, filter, q, sort_by, sort_order).await
}

#[tauri::command]
pub async fn get_model_proxy_channel_stats(
    state: State<'_, ModelProxyState>,
) -> Result<Vec<ChannelUsageStats>, String> {
    get_channel_usage_stats_summary(&state).await
}

#[tauri::command]
pub async fn get_opencode_channel_stats(
    state: State<'_, OpencodeProxyState>,
) -> Result<Vec<ChannelUsageStats>, String> {
    get_model_proxy_channel_stats(state).await
}

#[tauri::command]
pub async fn clear_model_proxy_logs(
    database: State<'_, Database>,
    mode: Option<String>,
) -> Result<(), String> {
    let conn = database
        .0
        .lock()
        .map_err(|_| "获取数据库连接失败".to_string())?;
    let clear_mode = mode.as_deref().unwrap_or("all");

    if clear_mode == "payload_only" {
        conn.execute(
            "UPDATE opencode_proxy_logs SET request_body = NULL, response_body = NULL",
            [],
        )
        .map_err(|e| format!("清理日志载荷失败: {e}"))?;
    } else {
        conn.execute("DELETE FROM opencode_proxy_logs", [])
            .map_err(|e| format!("清空日志记录失败: {e}"))?;
    }
    Ok(())
}

#[tauri::command]
pub async fn clear_opencode_proxy_logs(
    database: State<'_, Database>,
    mode: Option<String>,
) -> Result<(), String> {
    clear_model_proxy_logs(database, mode).await
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

