//! 模型能力测试（probe）：对反代渠道的「渠道×模型」组合并发发送内置/自定义提示词，
//! 自动判分（客观题对答案）+ LLM 评审（开放题由评审模型打分），结果落库供对比。

pub mod commands;
pub mod runner;
pub mod store;
pub mod types;

#[allow(unused_imports)]
pub use commands::*;
#[allow(unused_imports)]
pub use store::*;
#[allow(unused_imports)]
pub use types::*;

#[cfg(test)]
mod tests;
