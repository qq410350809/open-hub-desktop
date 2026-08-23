use crate::charity::db::*;
use crate::charity::fetcher::*;
use crate::charity::types::*;
use crate::context::{spawn, AppContext};
use crate::proxypool;
use std::sync::{atomic::Ordering, Arc, Mutex};
use std::time::Duration;
use tracing::error;

pub fn local_hms() -> (u32, u32, u32) {
    if let Ok(output) = std::process::Command::new("/bin/date")
        .arg("+%H:%M:%S")
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            let parts = text.trim().split(':').collect::<Vec<_>>();
            if parts.len() == 3 {
                if let (Ok(h), Ok(m), Ok(s)) = (
                    parts[0].parse::<u32>(),
                    parts[1].parse::<u32>(),
                    parts[2].parse::<u32>(),
                ) {
                    if h < 24 && m < 60 && s < 60 {
                        return (h, m, s);
                    }
                }
            }
        }
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let tod = (secs % 86_400) as u32;
    (tod / 3600, (tod % 3600) / 60, tod % 60)
}

pub fn seconds_until_next_aligned_run(
    hour: u32,
    minute: u32,
    second: u32,
    interval_minutes: u32,
) -> u64 {
    let interval = interval_minutes.max(1) as i64;
    let interval_secs = interval * 60;
    let now_secs = hour as i64 * 3600 + minute as i64 * 60 + second as i64;
    let rem = now_secs.rem_euclid(interval_secs);
    let delta = if rem == 0 {
        interval_secs
    } else {
        interval_secs - rem
    };
    delta as u64
}

pub fn seconds_until_next_scheduled_run() -> u64 {
    let (h, m, s) = local_hms();
    seconds_until_next_aligned_run(h, m, s, CHARITY_SCHEDULE_EVERY_MINUTES).max(1)
}

pub fn start_charity_monitor(ctx: Arc<AppContext>) {
    if ctx.charity_runtime.running.swap(true, Ordering::SeqCst) {
        return;
    }
    {
        tokio::task::block_in_place(|| abandon_running_charity_sync_logs(&ctx.database));
    }
    spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        loop {
            let monitor = &ctx.charity_runtime;
            let force = monitor.force_round.load(Ordering::Relaxed);
            if !force {
                let mut wait_secs = seconds_until_next_scheduled_run();
                while wait_secs > 0 {
                    if ctx.charity_runtime.force_round.load(Ordering::Relaxed) {
                        break;
                    }
                    let step = wait_secs.min(CHARITY_SCHEDULER_TICK.as_secs().max(1));
                    tokio::time::sleep(Duration::from_secs(step)).await;
                    wait_secs = wait_secs.saturating_sub(step);
                    if wait_secs <= 5 {
                        let recomputed = seconds_until_next_scheduled_run();
                        if recomputed > 30 && wait_secs <= 5 {
                            wait_secs = 0;
                            break;
                        }
                        wait_secs = recomputed.min(wait_secs);
                    }
                }
            }

            let Some(cancellation) = (loop {
                let monitor = &ctx.charity_runtime;
                if let Some(token) = monitor.try_begin_sync() {
                    break Some(token);
                }
                if monitor.force_round.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                }
                break None;
            }) else {
                continue;
            };

            let forced = ctx
                .charity_runtime
                .force_round
                .swap(false, Ordering::Relaxed);
            let stage = if forced { "manual" } else { "poll" };
            let database = &*ctx.database;
            let runtime = &*ctx.proxy_runtime;
            let round_nodes =
                tokio::task::block_in_place(|| build_charity_node_queue(&database, &monitor))
                    .unwrap_or_default();

            if round_nodes.is_empty() {
                let sources_for_skip = load_charity_sources(&database).unwrap_or_default();
                for source in &sources_for_skip {
                    let message = format!(
                        "无 ≤{CHARITY_FAST_NODE_MAX_LATENCY_MS}ms 可用公益候选节点，本轮跳过"
                    );
                    let _ = write_feed_sync_meta(&database, &source.id, "skipped", &message, "", 0);
                    if let Ok(mut errors) = monitor.last_errors.lock() {
                        errors.insert(source.id.clone(), message);
                    }
                }
                monitor.end_sync();
                continue;
            }

            let prepare_ids = round_nodes
                .iter()
                .map(|node| node.id.clone())
                .collect::<Vec<_>>();
            if let Err(error) =
                proxypool::prepare_proxy_nodes_transient(&database, &runtime, &prepare_ids).await
            {
                let message = format!("装载公益候选节点失败：{error}");
                let sources_for_err = load_charity_sources(&database).unwrap_or_default();
                for source in &sources_for_err {
                    let _ = write_feed_sync_meta(&database, &source.id, "error", &message, "", 0);
                    if let Ok(mut errors) = monitor.last_errors.lock() {
                        errors.insert(source.id.clone(), message.clone());
                    }
                }
                monitor.end_sync();
                continue;
            }

            let sources_for_round = load_charity_sources(&database).unwrap_or_default();
            let mut handles = Vec::with_capacity(sources_for_round.len());
            let shared_queue = Arc::new(Mutex::new(CharityNodeQueue::from_nodes(round_nodes)));
            for source in sources_for_round {
                let task_ctx = ctx.clone();
                let cancellation = cancellation.clone();
                let shared_queue = shared_queue.clone();
                handles.push(spawn(async move {
                    let result = sync_feed_with_fast_nodes(
                        &task_ctx,
                        &task_ctx.database,
                        &task_ctx.proxy_runtime,
                        &source,
                        stage,
                        &cancellation,
                        Some(shared_queue),
                        true,
                    )
                    .await;
                    (source.id.to_string(), result)
                }));
            }
            for handle in handles {
                match handle.await {
                    Ok((feed_id, Ok(_))) => {
                        if let Ok(mut errors) = ctx.charity_runtime.last_errors.lock() {
                            errors.remove(&feed_id);
                        }
                    }
                    Ok((feed_id, Err(error))) => {
                        if !is_charity_sync_cancelled(&error) {
                            if let Ok(mut errors) = ctx.charity_runtime.last_errors.lock() {
                                errors.insert(feed_id, error);
                            }
                        }
                    }
                    Err(error) => error!("公益监听并行任务失败：{error}"),
                }
            }
            let _ = proxypool::restore_proxy_node_transient(&database, &runtime).await;
            ctx.charity_runtime.end_sync();
        }
    });
}
