//! 自动会话同步调度器：让「在用」站点的账号资料（余额/签到/访问令牌）与
//! Key/模型列表在后台自动保活，会话失效（令牌被拒或直连被安全盾拦截）时
//! 自动走 Chrome 桥接恢复，不再依赖用户手动点击「Chrome 会话」。
//!
//! 每轮三个阶段：
//! 1. 直连保活：复用 chrome_usage 的扫描+账号刷新（无浏览器介入）。
//! 2. 失效恢复：对 is_valid=0 且 requires_chrome_fallback 的 NewAPI 账号，
//!    以 Auto 模式走 Chrome 静默→后台两级桥接（绝不弹前台窗口）。
//! 3. 模型刷新：本轮恢复成功的账号立即刷新 Key/模型；其余有效账号的缓存
//!    超过 24 小时也顺带刷新（每轮限量，按最旧优先）。
//!
//! 浏览器兜底失败后进入持久化指数退避冷却（见 account_sync），冷却内的
//! 账号本轮直接跳过；确需人工过盾时通过事件通知前端引导手动处理一次。

use crate::account_sync::{self, ChromeSyncMode};
use crate::chrome_usage;
use crate::models::*;
use crate::models_fetch;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

const AUTO_SYNC_FIRST_ROUND_DELAY: Duration = Duration::from_secs(15);
const AUTO_SYNC_SCHEDULER_TICK: Duration = Duration::from_secs(20);
const DEFAULT_INTERVAL_MINUTES: u64 = 30;
const MIN_INTERVAL_MINUTES: u64 = 5;
const MAX_INTERVAL_MINUTES: u64 = 360;
/// 每轮模型刷新的限量：恢复成功的账号不限量（它们必须立即拿到新 Key），
/// 仅“缓存过期”的常规刷新限量，避免一轮打太多站点接口。
const STALE_MODEL_REFRESH_LIMIT: usize = 5;
const MODEL_CACHE_STALE_SECS: i64 = 24 * 3600;

const ENABLED_KEY: &str = "autoSync.enabled";
const INTERVAL_KEY: &str = "autoSync.intervalMinutes";
const LAST_ROUND_AT_KEY: &str = "autoSync.lastRoundAt";
const LAST_SUMMARY_KEY: &str = "autoSync.lastSummary";

/// 调度器运行时：running 防重复启动，force_round 支持前端“立即同步一轮”。
pub struct AutoSyncRuntime {
    pub(crate) running: AtomicBool,
    pub(crate) force_round: AtomicBool,
}

impl Default for AutoSyncRuntime {
    fn default() -> Self {
        Self {
            running: AtomicBool::new(false),
            force_round: AtomicBool::new(false),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoSyncSettings {
    pub enabled: bool,
    pub interval_minutes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoSyncAccountChange {
    pub site_id: String,
    pub site_name: String,
    pub profile_id: String,
    pub account_label: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AutoSyncRoundSummary {
    pub started_at: i64,
    pub finished_at: i64,
    /// 直连保活阶段刷新成功的账号数（chrome_usage 扫描结果）。
    pub refreshed_accounts: usize,
    /// 自动恢复成功（失效 → 有效）的账号列表。
    pub recovered: Vec<AutoSyncAccountChange>,
    /// 自动恢复失败（多为需要人工完成 Cloudflare 验证）的账号列表。
    pub pending_manual: Vec<AutoSyncAccountChange>,
    /// 本轮刷新 Key/模型成功 / 失败的账号数。
    pub models_refreshed: usize,
    pub models_failed: usize,
    pub error: String,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0)
}

fn read_meta_string(connection: &rusqlite::Connection, key: &str) -> Option<String> {
    connection
        .query_row("SELECT value FROM app_meta WHERE key = ?1", [key], |row| {
            row.get::<_, String>(0)
        })
        .optional()
        .ok()
        .flatten()
}

fn write_meta_string(
    connection: &rusqlite::Connection,
    key: &str,
    value: &str,
) -> Result<(), String> {
    connection
        .execute(
            "INSERT INTO app_meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn read_settings(connection: &rusqlite::Connection) -> AutoSyncSettings {
    let enabled = read_meta_string(connection, ENABLED_KEY)
        .map(|value| value.trim() == "1")
        .unwrap_or(true);
    let interval_minutes = read_meta_string(connection, INTERVAL_KEY)
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|value| value.clamp(MIN_INTERVAL_MINUTES, MAX_INTERVAL_MINUTES))
        .unwrap_or(DEFAULT_INTERVAL_MINUTES);
    AutoSyncSettings {
        enabled,
        interval_minutes,
    }
}

#[tauri::command]
pub fn get_auto_sync_settings(database: State<'_, Database>) -> Result<AutoSyncSettings, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    Ok(read_settings(&connection))
}

#[tauri::command]
pub fn set_auto_sync_settings(
    app: AppHandle,
    database: State<'_, Database>,
    enabled: Option<bool>,
    interval_minutes: Option<u64>,
) -> Result<AutoSyncSettings, String> {
    let settings = {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        if let Some(enabled) = enabled {
            write_meta_string(&connection, ENABLED_KEY, if enabled { "1" } else { "0" })?;
        }
        if let Some(interval) = interval_minutes {
            let interval = interval.clamp(MIN_INTERVAL_MINUTES, MAX_INTERVAL_MINUTES);
            write_meta_string(&connection, INTERVAL_KEY, &interval.to_string())?;
        }
        read_settings(&connection)
    };
    // 设置变化即通知前端刷新倒计时展示；调度循环每 tick 重读设置，改频率尽快生效。
    let _ = app.emit("auto-sync-settings-changed", &settings);
    Ok(settings)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoSyncStatus {
    pub enabled: bool,
    pub interval_minutes: u64,
    pub last_round_at: i64,
    pub last_summary: Option<AutoSyncRoundSummary>,
}

#[tauri::command]
pub fn get_auto_sync_status(database: State<'_, Database>) -> Result<AutoSyncStatus, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let settings = read_settings(&connection);
    let last_round_at = read_meta_string(&connection, LAST_ROUND_AT_KEY)
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(0);
    let last_summary = read_meta_string(&connection, LAST_SUMMARY_KEY)
        .and_then(|value| serde_json::from_str::<AutoSyncRoundSummary>(&value).ok());
    Ok(AutoSyncStatus {
        enabled: settings.enabled,
        interval_minutes: settings.interval_minutes,
        last_round_at,
        last_summary,
    })
}

/// 前端“立即同步一轮”按钮：置位 force 标记，调度循环在 tick 内消费。
#[tauri::command]
pub fn request_auto_sync_round(app: AppHandle) -> Result<(), String> {
    let runtime = app.state::<AutoSyncRuntime>();
    runtime.force_round.store(true, Ordering::Relaxed);
    Ok(())
}

fn emit_round_event(app: &AppHandle, summary: &AutoSyncRoundSummary) {
    let _ = app.emit("auto-sync-round", summary);
}

fn emit_progress(app: &AppHandle, stage: &str, status: &str, message: String) {
    #[derive(Serialize, Clone)]
    #[serde(rename_all = "camelCase")]
    struct Progress<'a> {
        stage: &'a str,
        status: &'a str,
        message: String,
        at: i64,
    }
    let _ = app.emit(
        "auto-sync-progress",
        Progress {
            stage,
            status,
            message,
            at: now_secs(),
        },
    );
}

/// 需要 Chrome 桥接恢复的失效账号（在用站点、NewAPI 系、非冷却中）。
struct RecoveryTarget {
    site_id: String,
    site_name: String,
    profile_id: String,
    account_label: String,
}

fn load_recovery_targets(connection: &rusqlite::Connection) -> Result<Vec<RecoveryTarget>, String> {
    let mut statement = connection
        .prepare(
            "SELECT sa.site_id, ds.name, sa.profile_id, sa.profile_name, sa.account_name,
                sa.username, ds.api_base_url, sa.sync_error,
                sa.browser_fallback_failed_at, sa.browser_fallback_fail_count
         FROM site_accounts sa
         JOIN directory_sites ds ON ds.id = sa.site_id
         WHERE sa.is_valid = 0 AND ds.is_personal = 1
           AND ds.system_type IN ('new-api', 'newapi2')
         ORDER BY sa.updated_at DESC",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let mut targets = Vec::new();
    for (
        site_id,
        site_name,
        profile_id,
        profile_name,
        account_name,
        username,
        _api_base_url,
        sync_error,
        failed_at,
        fail_count,
    ) in rows
    {
        // 只有“直连通道确实死了”（令牌被拒/安全盾）才动浏览器；
        // 普通网络抖动保留缓存等下一轮直连重试。
        if !account_sync::requires_chrome_fallback(&sync_error) {
            continue;
        }
        if account_sync::browser_fallback_cooldown_remaining_ms(failed_at, fail_count) > 0 {
            continue;
        }
        let account_label = if username.is_empty() {
            if account_name.is_empty() {
                profile_name.clone()
            } else {
                account_name.clone()
            }
        } else {
            username.clone()
        };
        targets.push(RecoveryTarget {
            site_id,
            site_name,
            profile_id,
            account_label,
        });
    }
    Ok(targets)
}

/// Key/模型缓存过期（或本轮刚恢复）的有效账号。
struct ModelRefreshTarget {
    site_id: String,
    site_name: String,
    profile_id: String,
    profile_name: String,
    account_name: String,
    username: String,
    api_base_url: String,
}

fn load_stale_model_targets(
    connection: &rusqlite::Connection,
    recovered_keys: &[(String, String)],
) -> Result<Vec<ModelRefreshTarget>, String> {
    let mut statement = connection
        .prepare(
            "SELECT sa.site_id, ds.name, sa.profile_id, sa.profile_name, sa.account_name,
                sa.username, ds.api_base_url
         FROM site_accounts sa
         JOIN directory_sites ds ON ds.id = sa.site_id
         LEFT JOIN site_model_cache smc
           ON smc.site_id = sa.site_id AND smc.profile_id = sa.profile_id
          WHERE sa.is_valid = 1 AND ds.is_personal = 1 AND sa.api_key_count > 0
            AND (smc.profile_id IS NULL
                OR strftime('%s', smc.updated_at) IS NULL
                OR strftime('%s', 'now') - strftime('%s', smc.updated_at) >= ?1)
         ORDER BY smc.updated_at IS NOT NULL, smc.updated_at ASC
         LIMIT ?2",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(
            params![
                MODEL_CACHE_STALE_SECS,
                (STALE_MODEL_REFRESH_LIMIT + recovered_keys.len()) as i64
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let mut targets: Vec<ModelRefreshTarget> = rows
        .into_iter()
        .map(
            |(
                site_id,
                site_name,
                profile_id,
                profile_name,
                account_name,
                username,
                api_base_url,
            )| {
                ModelRefreshTarget {
                    site_id,
                    site_name,
                    profile_id,
                    profile_name,
                    account_name,
                    username,
                    api_base_url,
                }
            },
        )
        .collect();
    // 本轮恢复成功的账号必须立即刷新（Key 可能已随令牌轮换变化），
    // 它们在上面 SQL 里可能因缓存还“新鲜”而被排除，这里补齐。
    for (site_id, profile_id) in recovered_keys {
        if targets
            .iter()
            .any(|target| target.site_id == *site_id && target.profile_id == *profile_id)
        {
            continue;
        }
        let row = connection
            .query_row(
                "SELECT ds.name, sa.profile_name, sa.account_name, sa.username, ds.api_base_url
                 FROM site_accounts sa JOIN directory_sites ds ON ds.id = sa.site_id
                 WHERE sa.site_id = ?1 AND sa.profile_id = ?2 AND sa.is_valid = 1",
                params![site_id, profile_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if let Some((site_name, profile_name, account_name, username, api_base_url)) = row {
            targets.push(ModelRefreshTarget {
                site_id: site_id.clone(),
                site_name,
                profile_id: profile_id.clone(),
                profile_name,
                account_name,
                username,
                api_base_url,
            });
        }
    }
    Ok(targets)
}

async fn run_auto_sync_round(app: &AppHandle) -> AutoSyncRoundSummary {
    let database = app.state::<Database>();
    let mut summary = AutoSyncRoundSummary {
        started_at: now_secs(),
        ..Default::default()
    };

    // 阶段 1：直连保活（扫描 Chrome 会话 + 刷新在用站点账号）。
    emit_progress(
        app,
        "keepalive",
        "running",
        "自动同步：正在刷新在用站点的账号资料".into(),
    );
    let scan_result = chrome_usage::mark_sites_with_chrome_sessions(
        app.clone(),
        database.clone(),
        None,
        None,
        None,
        Some(false),
        Some(false),
    )
    .await;
    match scan_result {
        Ok(result) => {
            summary.refreshed_accounts = result.accounts;
            emit_progress(
                app,
                "keepalive",
                "success",
                format!(
                    "自动同步：直连保活完成，{} 个站点、{} 个合法账号",
                    result.detected, result.accounts
                ),
            );
        }
        Err(error) => {
            summary.error = format!("直连保活失败：{error}");
            emit_progress(app, "keepalive", "error", summary.error.clone());
        }
    }

    // 阶段 2：失效账号自动恢复（Chrome 静默→后台，绝不弹前台）。
    let recovery_targets = {
        let Ok(connection) = database.0.lock() else {
            summary.error.push_str("；读取恢复队列时数据库锁定失败");
            return finish_round(app, &database, summary);
        };
        match load_recovery_targets(&connection) {
            Ok(targets) => targets,
            Err(error) => {
                summary
                    .error
                    .push_str(&format!("；读取恢复队列失败：{error}"));
                return finish_round(app, &database, summary);
            }
        }
    };
    if !recovery_targets.is_empty() {
        emit_progress(
            app,
            "recovery",
            "running",
            format!("自动同步：{} 个失效账号待恢复", recovery_targets.len()),
        );
    }
    let mut recovered_keys: Vec<(String, String)> = Vec::new();
    for target in &recovery_targets {
        let label = format!("{} · {}", target.site_name, target.account_label);
        emit_progress(
            app,
            "recovery",
            "running",
            format!("自动同步：正在通过 Chrome 恢复 {label}"),
        );
        match account_sync::sync_site_account_via_chrome_command(
            app.clone(),
            &database,
            target.site_id.clone(),
            target.profile_id.clone(),
            0,
            ChromeSyncMode::Auto,
        )
        .await
        {
            Ok(_) => {
                recovered_keys.push((target.site_id.clone(), target.profile_id.clone()));
                summary.recovered.push(AutoSyncAccountChange {
                    site_id: target.site_id.clone(),
                    site_name: target.site_name.clone(),
                    profile_id: target.profile_id.clone(),
                    account_label: target.account_label.clone(),
                    error: String::new(),
                });
            }
            Err(error) => {
                summary.pending_manual.push(AutoSyncAccountChange {
                    site_id: target.site_id.clone(),
                    site_name: target.site_name.clone(),
                    profile_id: target.profile_id.clone(),
                    account_label: target.account_label.clone(),
                    error,
                });
            }
        }
    }
    if !recovery_targets.is_empty() {
        emit_progress(
            app,
            "recovery",
            if summary.recovered.is_empty() {
                "error"
            } else {
                "success"
            },
            format!(
                "自动同步：恢复完成，{} 个成功、{} 个待人工处理",
                summary.recovered.len(),
                summary.pending_manual.len()
            ),
        );
    }

    // 阶段 3：Key/模型刷新（恢复成功的立即刷；其余过期缓存按最旧限量刷）。
    let model_targets = {
        let Ok(connection) = database.0.lock() else {
            summary.error.push_str("；读取模型刷新队列时数据库锁定失败");
            return finish_round(app, &database, summary);
        };
        match load_stale_model_targets(&connection, &recovered_keys) {
            Ok(targets) => targets,
            Err(error) => {
                summary
                    .error
                    .push_str(&format!("；读取模型刷新队列失败：{error}"));
                return finish_round(app, &database, summary);
            }
        }
    };
    if !model_targets.is_empty() {
        emit_progress(
            app,
            "models",
            "running",
            format!(
                "自动同步：正在刷新 {} 个账号的 Key/模型",
                model_targets.len()
            ),
        );
    }
    let mut cleared_sites = std::collections::HashSet::new();
    for target in &model_targets {
        let mut base_url = target.api_base_url.trim().to_string();
        if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
            base_url = format!("https://{base_url}");
        }
        if !base_url.ends_with('/') {
            base_url.push('/');
        }
        match models_fetch::auto_fetch_site_models_json(
            app,
            &database,
            base_url,
            target.site_id.clone(),
            target.profile_id.clone(),
        )
        .await
        {
            Ok(result) => {
                // 同步 Key 成功获取数据后，首次保存前清理掉这个站点原来的对应旧数据，避免数据冲突与旧 Key 残留
                if !cleared_sites.contains(&target.site_id) {
                    let _ = models_fetch::clear_site_model_cache(&database, &target.site_id);
                    cleared_sites.insert(target.site_id.clone());
                }
                let account = SiteModelCacheAccount {
                    profile_id: target.profile_id.clone(),
                    profile_name: target.profile_name.clone(),
                    account_name: target.account_name.clone(),
                    username: target.username.clone(),
                    keys: result.keys.clone(),
                    key_groups: result.key_groups.clone(),
                    key_models: result.key_models.clone(),
                    error: String::new(),
                };
                match models_fetch::save_site_model_cache(
                    &database,
                    &target.site_id,
                    &account,
                    Some(&result),
                    false,
                ) {
                    Ok(()) => summary.models_refreshed += 1,
                    Err(error) => {
                        summary.models_failed += 1;
                        summary
                            .error
                            .push_str(&format!("；{} 模型缓存写入失败：{error}", target.site_name));
                    }
                }
            }
            Err(error) => {
                summary.models_failed += 1;
                emit_progress(
                    app,
                    "models",
                    "error",
                    format!(
                        "自动同步：{} 的 Key/模型刷新失败：{error}",
                        target.site_name
                    ),
                );
            }
        }
    }
    if !model_targets.is_empty() {
        emit_progress(
            app,
            "models",
            if summary.models_failed == 0 {
                "success"
            } else {
                "error"
            },
            format!(
                "自动同步：Key/模型刷新完成，{} 个成功、{} 个失败",
                summary.models_refreshed, summary.models_failed
            ),
        );
    }

    finish_round(app, &database, summary)
}

fn finish_round(
    app: &AppHandle,
    database: &State<'_, Database>,
    mut summary: AutoSyncRoundSummary,
) -> AutoSyncRoundSummary {
    summary.finished_at = now_secs();
    if let Ok(connection) = database.0.lock() {
        let _ = write_meta_string(
            &connection,
            LAST_ROUND_AT_KEY,
            &summary.finished_at.to_string(),
        );
        if let Ok(serialized) = serde_json::to_string(&summary) {
            let _ = write_meta_string(&connection, LAST_SUMMARY_KEY, &serialized);
        }
    }
    emit_round_event(app, &summary);
    summary
}

pub(crate) fn start_auto_sync(app: AppHandle) {
    let runtime = app.state::<AutoSyncRuntime>();
    if runtime.running.swap(true, Ordering::SeqCst) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        // 启动稍等：避开应用启动高峰（代理恢复、公益监听首轮等），
        // 之后立刻跑首轮——刚开应用时最需要把隔夜的失效会话救回来。
        tokio::time::sleep(AUTO_SYNC_FIRST_ROUND_DELAY).await;
        let mut first_round = true;
        loop {
            let (enabled, interval_minutes) = {
                let database = app.state::<Database>();
                let Ok(connection) = database.0.lock() else {
                    tokio::time::sleep(AUTO_SYNC_SCHEDULER_TICK).await;
                    continue;
                };
                let settings = read_settings(&connection);
                (settings.enabled, settings.interval_minutes)
            };
            let wait_secs = if first_round {
                first_round = false;
                0
            } else if enabled {
                interval_minutes * 60
            } else {
                60
            };
            let deadline = tokio::time::Instant::now() + Duration::from_secs(wait_secs);
            let forced = loop {
                if app
                    .state::<AutoSyncRuntime>()
                    .force_round
                    .swap(false, Ordering::Relaxed)
                {
                    break true;
                }
                let now = tokio::time::Instant::now();
                if now >= deadline {
                    break false;
                }
                let step = (deadline - now)
                    .min(AUTO_SYNC_SCHEDULER_TICK)
                    .max(Duration::from_millis(500));
                tokio::time::sleep(step).await;
            };
            // 关闭状态下 force 也不跑（开关是最强语义），只消费标记。
            if !enabled {
                continue;
            }
            let _ = forced; // 到点与手动触发走同一条轮次逻辑
            let summary = run_auto_sync_round(&app).await;
            if !summary.error.is_empty() {
                eprintln!("[OpenHub] 自动会话同步：{}", summary.error);
            }
        }
    });
}
