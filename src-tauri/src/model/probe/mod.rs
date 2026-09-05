//! 模型验真（probe）：对反代渠道的「渠道×模型」组合并发发送内置探测题，
//! 通过身份自述、判别指纹、降智能力与一致性采样四个维度，检测渠道是否
//! 冒名/降智交付（用便宜模型冒充旗舰、量化蒸馏版冒充原版）。结果落库供历史对比。

pub mod commands;
pub mod fingerprints;
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
