use crate::context::{AppContext, EventBus, Managed};
use crate::db::*;
use crate::models::*;
use crate::site::library::*;
use rusqlite::params;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn detect_site_system_types(
    ctx: Managed<'_, Arc<AppContext>>,
    site_ids: Vec<String>,
    run_id: u64,
) -> Result<usize, String> {
    let database = &*ctx.database;
    let bus: EventBus = ctx.event_bus.clone();
    let site_ids = site_ids.into_iter().collect::<HashSet<_>>();
    if site_ids.is_empty() {
        return Ok(0);
    }
    let targets = {
        let connection = database.lock_conn()?;
        let mut statement = connection
            .prepare(
                "SELECT id, api_base_url, system_type FROM directory_sites
                 WHERE TRIM(api_base_url) <> ''",
            )
            .map_err(|error| error.to_string())?;
        let targets = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|error| error.to_string())?
            .filter_map(|row| row.ok())
            .filter(|(site_id, _api_base_url, stored_type)| {
                // 站点类型冻结：只探测“类型为空”的站点（通常是刚新增、远端
                // 没给类型的站点）。已入库站点的类型严禁被探测改写——探测结果
                // 与库中值不一致时以库中值为准，不再触发任何纠正。
                site_ids.contains(site_id) && stored_type.trim().is_empty()
            })
            .map(|(site_id, api_base_url, _)| (site_id, api_base_url))
            .collect::<Vec<_>>();
        targets
    };
    emit_sync_progress(
        &bus,
        run_id,
        "detect",
        "running",
        format!("已转入后台，并发检测 {} 个站点类型", targets.len()),
    );
    let client = build_site_http_client(database, Duration::from_secs(8), 3, "站点类型探测")?;
    let target_site_ids = targets
        .iter()
        .map(|(site_id, _)| site_id.clone())
        .collect::<HashSet<_>>();
    let profile_ids = cached_profile_ids_for_sites(database, &target_site_ids)?;
    let detected = probe_site_system_types(&client, targets, profile_ids).await;
    let detected_count = detected.len();
    let mut connection = database.lock_conn()?;
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    for (site_id, system_type) in detected {
        transaction
            .execute(
                "UPDATE directory_sites SET system_type = ?2 WHERE id = ?1",
                params![site_id, system_type],
            )
            .map_err(|error| error.to_string())?;
    }
    transaction.commit().map_err(|error| error.to_string())?;
    emit_sync_progress(
        &bus,
        run_id,
        "detect",
        "success",
        format!("后台类型检测完成，已处理 {detected_count} 个站点"),
    );
    Ok(detected_count)
}
