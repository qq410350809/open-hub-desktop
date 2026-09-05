use super::fingerprints;
use super::runner;
use super::store;
use super::types::*;
use crate::context::{AppContext, Managed};
use crate::model::gateway::types::ModelProxyState;
use std::sync::Arc;

/// 启动一次模型验真检测（后台执行，进度经 `model-test-progress` 事件推送）。
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

/// 内置探测目录（前端展示与勾选用，题库由后端维护）。
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_detection_suites() -> Result<Vec<DetectionProbe>, String> {
    Ok(fingerprints::builtin_probes())
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

/// 某次检测的按目标验真结论（含全部探测明细）。
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_model_test_results(
    ctx: Managed<'_, Arc<AppContext>>,
    run_id: i64,
) -> Result<Vec<TargetVerdict>, String> {
    let ctx = ctx.inner().clone();
    crate::context::spawn_blocking(move || {
        let results = store::get_run_results(&ctx.database, run_id)?;
        Ok(runner::build_verdicts(&results))
    })
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
