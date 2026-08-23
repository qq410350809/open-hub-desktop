use crate::context::{spawn, spawn_blocking, AppContext};
use crate::token::stats::db::collect_token_data;
use crate::token::stats::types::{local_timestamp, TOKEN_COLLECT_INTERVAL};
use std::sync::Arc;
use tracing::{error, info};

pub fn start_token_collector(ctx: Arc<AppContext>) {
    spawn(async move {
        let mut interval = tokio::time::interval(TOKEN_COLLECT_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let ctx = ctx.clone();
            let result = spawn_blocking(move || {
                collect_token_data(&ctx.database, false, Some(&ctx.event_bus))
            })
            .await;
            match result {
                Ok(Ok(report)) => {
                    if report.changed {
                        info!(
                            target: "openhub::token",
                            "[OpenHub] {} Token 后台采集完成：{}",
                            local_timestamp(),
                            report.message
                        );
                    }
                }
                Ok(Err(error)) => error!(
                    target: "openhub::token",
                    "[OpenHub] {} Token 后台采集失败：{error}",
                    local_timestamp()
                ),
                Err(error) => error!(
                    target: "openhub::token",
                    "[OpenHub] {} Token 后台任务异常：{error}",
                    local_timestamp()
                ),
            }
        }
    });
}
