//! 一次性工具：对 OpenHub 全库站点重跑平台类型检测。
//!
//! 用法（在 src-tauri 目录下）：
//!   cargo run --example detect_all
//!
//! 流程：先用 `VACUUM INTO` 把数据库备份成 sites.sqlite3.bak-<时间戳>，
//! 再对每个 api_base_url 非空的站点跑新的检测流水线并回写 system_type。

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

fn default_db_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    format!("{home}/Library/Application Support/com.dfeer.openhub.desktop/sites.sqlite3")
}

fn backup_database(path: &Path) -> Result<String, String> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_secs();
    let backup_path = format!("{}.bak-{stamp}", path.display());
    let connection = rusqlite::Connection::open(path).map_err(|error| error.to_string())?;
    // VACUUM INTO 生成一致快照（含 WAL 中的未提交数据）。
    connection
        .execute_batch(&format!("VACUUM INTO '{}'", backup_path))
        .map_err(|error| error.to_string())?;
    Ok(backup_path)
}

fn main() {
    let db_path = std::env::args().nth(1).unwrap_or_else(default_db_path);
    let path = Path::new(&db_path);
    println!("数据库: {}", path.display());
    match backup_database(path) {
        Ok(backup_path) => println!("已备份到: {backup_path}"),
        Err(error) => {
            eprintln!("备份失败，中止：{error}");
            std::process::exit(1);
        }
    }
    match open_hub_desktop_lib::run_library_detect(path) {
        Ok(report) => {
            println!(
                "检测完成：共 {} 个站点，识别出类型 {}，其中类型发生变化 {}，未识别 {}",
                report.total, report.detected, report.changed, report.unknown
            );
        }
        Err(error) => {
            eprintln!("检测失败：{error}");
            std::process::exit(1);
        }
    }
}
