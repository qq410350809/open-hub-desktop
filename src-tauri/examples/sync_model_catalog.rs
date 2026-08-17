//! 一次性工具：对本地数据库强制重跑模型参数同步，验证 LLMPricing 数据收录逻辑。
//!
//! 用法（在 src-tauri 目录下）：
//!   cargo run --example sync_model_catalog -- [db_path]

use open_hub_desktop_lib::sync_model_catalog_once;

fn default_db_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    format!("{home}/Library/Application Support/com.dfeer.openhub.desktop/sites.sqlite3")
}

fn main() {
    let db_path = std::env::args().nth(1).unwrap_or_else(default_db_path);
    println!("数据库: {db_path}");
    match sync_model_catalog_once(&db_path) {
        Ok(report) => {
            println!(
                "同步完成：LLMPricing 收录 {} 个供应商，{} 个模型（共 {} 个分片）",
                report.provider_count, report.model_count, report.shard_count
            );
        }
        Err(error) => {
            eprintln!("同步失败：{error}");
            std::process::exit(1);
        }
    }
}
