//! 渠道个性化策略层：每个特殊渠道（如 OpenCode 官方通道）的行为定制
//! 集中在独立文件，避免散落在 balancer/dispatcher/router 通用逻辑中。
//!
//! 新增特殊渠道时：新建同级策略文件 → 在此声明并 re-export。

pub mod opencode;

#[allow(unused_imports)]
pub use opencode::*;
