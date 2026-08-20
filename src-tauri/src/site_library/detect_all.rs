//! 全库站点平台类型检测 —— 供 `examples/detect_all.rs` 一次性重跑。
//!
//! 与 `system_detect::detect_site_system_types`（只重测空值/与 URL 提示不一致的站点）
//! 不同，这里对**所有**非空 `api_base_url` 的站点强制执行新的检测流水线
//! （`platform_detect::detect_platform`），用于把旧库里误判的类型一次性纠正过来。

use crate::db::build_http_client_with_proxy;
use crate::models::Database;
use crate::platform_detect::detect_platform;
use rusqlite::params;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub struct LibraryDetectReport {
    pub total: usize,
    pub detected: usize,
    pub changed: usize,
    pub unknown: usize,
}

fn load_targets(
    connection: &rusqlite::Connection,
) -> Result<Vec<(String, String, String)>, String> {
    let mut statement = connection
        .prepare(
            "SELECT id, api_base_url, system_type FROM directory_sites \
             WHERE TRIM(api_base_url) <> ''",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    let targets = rows.filter_map(|row| row.ok()).collect::<Vec<_>>();
    Ok(targets)
}

/// 对全库站点重跑一次平台检测并回写 system_type。
pub fn run_library_detect(path: &Path) -> Result<LibraryDetectReport, String> {
    let database = Database::open(path)?;
    let client =
        build_http_client_with_proxy(&database, Duration::from_secs(8), 3, "全库类型检测")?;

    let targets = {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        load_targets(&connection)?
    };

    let results = tauri::async_runtime::block_on(async {
        let semaphore = Arc::new(tokio::sync::Semaphore::new(8));
        let mut handles = Vec::with_capacity(targets.len());
        for (site_id, base_url, old_type) in targets {
            let permit = semaphore
                .clone()
                .acquire_owned()
                .await
                .unwrap_or_else(|_| unreachable!("信号量未关闭"));
            let client = client.clone();
            handles.push(tauri::async_runtime::spawn(async move {
                let detection = detect_platform(&client, &base_url).await;
                drop(permit);
                (site_id, old_type, detection.platform)
            }));
        }
        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            if let Ok(completed) = handle.await {
                results.push(completed);
            }
        }
        results
    });

    let total = results.len();
    let mut detected = 0usize;
    let mut changed = 0usize;
    let mut unknown = 0usize;
    let mut updates = Vec::new();
    for (site_id, old_type, platform) in &results {
        if let Some(new_type) = platform {
            if !new_type.is_empty() {
                detected += 1;
                if !old_type.eq_ignore_ascii_case(new_type) {
                    changed += 1;
                    updates.push((site_id.clone(), new_type.clone()));
                }
                continue;
            }
        }
        unknown += 1;
    }

    if !updates.is_empty() {
        let mut connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        let transaction = connection
            .transaction()
            .map_err(|error| error.to_string())?;
        for (site_id, system_type) in &updates {
            transaction
                .execute(
                    "UPDATE directory_sites SET system_type = ?2 WHERE id = ?1",
                    params![site_id, system_type],
                )
                .map_err(|error| error.to_string())?;
        }
        transaction.commit().map_err(|error| error.to_string())?;
    }

    Ok(LibraryDetectReport {
        total,
        detected,
        changed,
        unknown,
    })
}
