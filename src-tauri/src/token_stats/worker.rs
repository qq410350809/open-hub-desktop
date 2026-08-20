use crate::models::Database;
use crate::token_stats::db::collect_token_data;
use crate::token_stats::types::{local_timestamp, TOKEN_COLLECT_INTERVAL};
use tauri::{AppHandle, Manager};

pub fn start_token_collector(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(TOKEN_COLLECT_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let handle = app.clone();
            let result = tauri::async_runtime::spawn_blocking(move || {
                let database = handle.state::<Database>();
                collect_token_data(&database, false, None)
            })
            .await;
            match result {
                Ok(Ok(report)) => {
                    if report.changed {
                        eprintln!(
                            "[OpenHub] {} Token 后台采集完成：{}",
                            local_timestamp(),
                            report.message
                        );
                    }
                }
                Ok(Err(error)) => eprintln!(
                    "[OpenHub] {} Token 后台采集失败：{error}",
                    local_timestamp()
                ),
                Err(error) => eprintln!(
                    "[OpenHub] {} Token 后台任务异常：{error}",
                    local_timestamp()
                ),
            }
        }
    });
}
