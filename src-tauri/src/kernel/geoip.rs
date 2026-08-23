use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use crate::context::{AppContext, Managed};
use crate::kernel::{download_bytes_with_dynamic_racing, get_app_bin_dir};

const GEOIP_PRIMARY_URL: &str = "https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest/country.mmdb";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoipStatus {
    pub installed: bool,
    pub path: String,
    pub file_size: u64,
    pub file_size_formatted: String,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoipDownloadProgress {
    pub stage: String,
    pub progress: f64,
    pub message: String,
}

pub fn get_app_geoip_path(ctx: &AppContext) -> PathBuf {
    let _ = fs::create_dir_all(&ctx.data_dir);
    ctx.geoip_path()
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn geoip_status_from_path(target_path: &PathBuf) -> GeoipStatus {
    if target_path.is_file() {
        if let Ok(meta) = fs::metadata(target_path) {
            let size = meta.len();
            let updated_at = meta.modified().ok().map(|st| {
                let dt: DateTime<Local> = st.into();
                dt.format("%Y-%m-%d %H:%M").to_string()
            });

            return GeoipStatus {
                installed: true,
                path: target_path.display().to_string(),
                file_size: size,
                file_size_formatted: format_bytes(size),
                updated_at,
            };
        }
    }

    GeoipStatus {
        installed: false,
        path: target_path.display().to_string(),
        file_size: 0,
        file_size_formatted: "0 B".to_string(),
        updated_at: None,
    }
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_geoip_status(
    ctx: Managed<'_, Arc<AppContext>>,
) -> Result<GeoipStatus, String> {
    Ok(geoip_status_from_path(&get_app_geoip_path(&ctx)))
}

/// 下载并更新 GeoIP 数据库；完成后基于新库修复已有节点的国家/地区映射。
/// 桌面端由命令注入托管状态；server 端由 RPC 分发器直接传入。
pub async fn download_or_update_geoip_inner(
    ctx: &Arc<AppContext>,
    database: Option<&crate::models::Database>,
    proxy_runtime: Option<&crate::proxypool::ProxyRuntime>,
    mirror: Option<String>,
) -> Result<GeoipStatus, String> {
    let bus = ctx.event_bus.clone();
    let emit_progress = |stage: &str, progress: f64, message: &str| {
        bus.emit(
            "geoip-download-progress",
            GeoipDownloadProgress {
                stage: stage.to_string(),
                progress,
                message: message.to_string(),
            },
        );
    };

    emit_progress("checking", 0.05, "正在检测最新 GeoIP 数据库…");

    let raw_download_url = GEOIP_PRIMARY_URL;
    let target_path = get_app_geoip_path(ctx);
    let temp_download_path = target_path.with_extension("tmp");

    let mmdb_bytes = download_bytes_with_dynamic_racing(
        raw_download_url,
        mirror.as_deref(),
        &bus,
        "geoip-download-progress",
        "GeoIP 数据库",
    ).await?;

    emit_progress("verifying", 0.92, "正在校验 GeoIP MMDB 数据库有效性…");

    // 校验 MMDB 结构有效性
    let _reader = maxminddb::Reader::from_source(&mmdb_bytes)
        .map_err(|e| format!("校验下载的 GeoIP 数据库格式失败：{e}"))?;

    // 写入临时文件并原子覆盖
    let mut temp_file = File::create(&temp_download_path).map_err(|e| format!("创建临时文件失败：{e}"))?;
    temp_file.write_all(&mmdb_bytes).map_err(|e| format!("写入 GeoIP 数据库失败：{e}"))?;
    drop(temp_file);

    let _ = fs::remove_file(&target_path);
    fs::rename(&temp_download_path, &target_path).map_err(|e| format!("保存 GeoIP 数据库失败：{e}"))?;

    // 如果 bin 目录存在，也同步备份一份到 bin/ 目录
    let bin_dir = get_app_bin_dir(ctx);
    let _ = fs::copy(&target_path, bin_dir.join("Country.mmdb"));

    // 自动使用新下载的 GeoIP 数据库重新解析并修复已有节点的国家与地域映射
    if let (Some(db), Some(runtime)) = (database, proxy_runtime) {
        let _ = crate::proxypool::repair_node_locations_with_geoip(db, runtime);
        bus.emit("proxy-nodes-updated", ());
    }

    emit_progress("completed", 1.0, "GeoIP 国家地理数据库更新成功！");

    Ok(geoip_status_from_path(&target_path))
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn download_or_update_geoip(
    ctx: Managed<'_, Arc<AppContext>>,
    mirror: Option<String>,
) -> Result<GeoipStatus, String> {
    download_or_update_geoip_inner(&ctx, Some(&ctx.database), Some(&ctx.proxy_runtime), mirror).await
}
