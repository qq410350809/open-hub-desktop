//! 四个协议入口处理器：每个文件负责一种客户端协议入口的
//! 入参解析、客户端协议 ↔ OpenAI 中枢转换、响应回转；
//! 公共骨架（鉴权/渠道解析/校验/出网调度/日志）统一走 pipeline。

pub mod anthropic;
pub mod chat;
pub mod gemini;
pub mod responses;

pub use anthropic::handle_messages;
pub use chat::handle_chat_completions;
pub use gemini::handle_gemini_generate;
pub use responses::handle_responses;
