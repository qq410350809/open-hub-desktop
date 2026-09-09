//! 本地 AI 编程工具的模型配置管理。
//!
//! 页面清单以 token 采集记录为准（`collected_stats_by_source`），
//! 管理能力按各工具配置文件结构分级：
//! - A 级：存在明文配置文件，可结构化管理供应商 / 模型 / 上下文 / 思考级别；
//! - B 级：模型路由存于 sqlite 或云端，仅做安装检测与统计展示。
//!
//! 所有写入实时读盘、原子落盘、写前自动备份；只合并本模块管辖的字段，
//! 工具自身的其他配置（MCP、权限、项目信任等）永不触碰。

pub(crate) mod adapters;
pub(crate) mod backup;
pub mod commands;
pub(crate) mod fsutil;
pub(crate) mod mark;
pub(crate) mod types;

// tauri::command 宏生成的 __cmd__ 条目必须随 glob 再导出，
// generate_handler 才能在模块路径下找到它们。
pub use commands::*;
