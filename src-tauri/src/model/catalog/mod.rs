pub mod catalog;
#[cfg(feature = "desktop")]
pub mod chat;
pub mod channel_builder;
pub mod fetcher;
pub mod lab_registry;
pub mod models_dev;
/// 保留模型身份、拒绝歧义的能力默认值匹配。
pub mod nearest;

pub use catalog::*;
#[cfg(feature = "desktop")]
pub use chat::*;
pub use fetcher::*;
