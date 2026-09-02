use super::runner;
use super::store;
use super::types::*;
use crate::context::{AppContext, Managed};
use crate::model::gateway::types::ModelProxyState;
use std::sync::Arc;

/// 启动一次模型能力测试（后台执行，进度经 `model-test-progress` 事件推送）。
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn run_model_test(
    ctx: Managed<'_, Arc<AppContext>>,
    gateway: Managed<'_, ModelProxyState>,
    params: RunParams,
) -> Result<RunStartInfo, String> {
    // runner 需要把 context 分发进多个 spawn 任务，持 Arc；此处浅克隆（字段均为 Arc）
    let gateway_ctx = Arc::new(gateway.context.clone());
    runner::start_model_test(ctx.inner().clone(), gateway_ctx, params).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn cancel_model_test(ctx: Managed<'_, Arc<AppContext>>) -> Result<(), String> {
    runner::cancel_model_test(ctx.inner())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn list_model_test_runs(
    ctx: Managed<'_, Arc<AppContext>>,
    limit: Option<u32>,
) -> Result<Vec<TestRunRecord>, String> {
    let ctx = ctx.inner().clone();
    let active_run_id = ctx
        .model_probe
        .active_run_id
        .lock()
        .ok()
        .and_then(|guard| *guard);
    crate::context::spawn_blocking(move || {
        let _ = store::reap_stale_runs(&ctx.database, active_run_id);
        store::list_runs(&ctx.database, limit.unwrap_or(50).clamp(1, 500))
    })
    .await
    .map_err(|error| format!("任务调度失败：{error}"))?
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_model_test_results(
    ctx: Managed<'_, Arc<AppContext>>,
    run_id: i64,
) -> Result<Vec<ProbeResult>, String> {
    let ctx = ctx.inner().clone();
    crate::context::spawn_blocking(move || store::get_run_results(&ctx.database, run_id))
        .await
        .map_err(|error| format!("任务调度失败：{error}"))?
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn delete_model_test_run(
    ctx: Managed<'_, Arc<AppContext>>,
    run_id: i64,
) -> Result<u64, String> {
    let ctx = ctx.inner().clone();
    crate::context::spawn_blocking(move || store::delete_run(&ctx.database, run_id))
        .await
        .map_err(|error| format!("任务调度失败：{error}"))?
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_model_test_custom_prompts(
    ctx: Managed<'_, Arc<AppContext>>,
) -> Result<Vec<ProbePrompt>, String> {
    let ctx = ctx.inner().clone();
    crate::context::spawn_blocking(move || store::get_custom_prompts(&ctx.database))
        .await
        .map_err(|error| format!("任务调度失败：{error}"))?
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn save_model_test_custom_prompts(
    ctx: Managed<'_, Arc<AppContext>>,
    prompts: Vec<ProbePrompt>,
) -> Result<(), String> {
    let ctx = ctx.inner().clone();
    crate::context::spawn_blocking(move || store::save_custom_prompts(&ctx.database, &prompts))
        .await
        .map_err(|error| format!("任务调度失败：{error}"))?
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_model_test_last_config(
    ctx: Managed<'_, Arc<AppContext>>,
) -> Result<Option<serde_json::Value>, String> {
    let ctx = ctx.inner().clone();
    store::get_last_config(&ctx)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn save_model_test_last_config(
    ctx: Managed<'_, Arc<AppContext>>,
    config: serde_json::Value,
) -> Result<(), String> {
    let ctx = ctx.inner().clone();
    store::save_last_config(&ctx, &config)
}
