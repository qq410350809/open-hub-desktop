use crate::db::*;
use crate::models::*;
use crate::site_ops::*;
use rusqlite::params;
use std::collections::HashSet;
use std::time::Duration;
use tauri::State;

#[tauri::command]
pub async fn detect_site_system_types(
    app: tauri::AppHandle,
    database: State<'_, Database>,
    site_ids: Vec<String>,
    run_id: u64,
) -> Result<usize, String> {
    let site_ids = site_ids.into_iter().collect::<HashSet<_>>();
    if site_ids.is_empty() {
        return Ok(0);
    }
    let targets = {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
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
            .filter(|(site_id, api_base_url, stored_type)| {
                site_ids.contains(site_id)
                    && (stored_type.trim().is_empty()
                        || system_type_hint_from_url(api_base_url)
                            .is_some_and(|hint| !stored_type.eq_ignore_ascii_case(hint)))
            })
            .map(|(site_id, api_base_url, _)| (site_id, api_base_url))
            .collect::<Vec<_>>();
        targets
    };
    emit_sync_progress(
        &app,
        run_id,
        "detect",
        "running",
        format!("已转入后台，并发检测 {} 个站点类型", targets.len()),
    );
    let client = build_http_client(&database, Duration::from_secs(8), 3, "站点类型探测")?;
    let target_site_ids = targets
        .iter()
        .map(|(site_id, _)| site_id.clone())
        .collect::<HashSet<_>>();
    let profile_ids = cached_profile_ids_for_sites(&database, &target_site_ids)?;
    let detected = probe_site_system_types(&client, targets, profile_ids).await;
    let detected_count = detected.len();
    let mut connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
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
        &app,
        run_id,
        "detect",
        "success",
        format!("后台类型检测完成，已处理 {detected_count} 个站点"),
    );
    Ok(detected_count)
}
