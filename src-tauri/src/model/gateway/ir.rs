//! 网关通用中间对象（IR, Intermediate Representation）。
//!
//! 所有协议的流式响应先被各自的 Parser 解析为本模块定义的
//! [`UniversalStreamEvent`] 事件流，再由目标协议的 Emitter 消费重建为客户端
//! 协议的 SSE。以类型化事件取代「OpenAI Chat JSON」作为中枢格式，
//! 使 reasoning / tool_calls / 缓存计数等元素在全链路无损传递。

use serde_json::{json, Value as JsonValue};

/// 统一 token 用量口径。
///
/// `input_tokens` 为**总输入**（含缓存命中与缓存写入部分），明细单独携带；
/// 各协议出口按自身语义换算（例如 Anthropic 出口的 input_tokens 需扣除缓存，
/// Responses/Chat 出口直接使用总量）。
#[derive(Debug, Default, Clone, PartialEq)]
pub struct UniversalUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub reasoning_tokens: u64,
}

impl UniversalUsage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

}

/// 统一的停止原因；各协议字符串 ↔ 枚举的双向映射集中在此。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    ToolUse,
    ContentFilter,
}

impl StopReason {
    /// 从 OpenAI Chat finish_reason 解析（含方言兜底）
    pub fn from_chat(finish_reason: &str) -> Self {
        match finish_reason {
            "length" => Self::MaxTokens,
            "tool_calls" | "function_call" => Self::ToolUse,
            "content_filter" => Self::ContentFilter,
            _ => Self::EndTurn,
        }
    }

    /// 从 Anthropic stop_reason 解析
    pub fn from_anthropic(stop_reason: &str) -> Self {
        match stop_reason {
            "max_tokens" => Self::MaxTokens,
            "tool_use" => Self::ToolUse,
            "refusal" => Self::ContentFilter,
            _ => Self::EndTurn,
        }
    }

    /// 从 Gemini finishReason 解析
    pub fn from_gemini(finish_reason: &str) -> Self {
        match finish_reason {
            "MAX_TOKENS" => Self::MaxTokens,
            "SAFETY" | "RECITATION" | "PROHIBITED_CONTENT" | "BLOCKLIST" => Self::ContentFilter,
            _ => Self::EndTurn,
        }
    }

    pub fn to_chat(self) -> &'static str {
        match self {
            Self::EndTurn => "stop",
            Self::MaxTokens => "length",
            Self::ToolUse => "tool_calls",
            Self::ContentFilter => "content_filter",
        }
    }

    pub fn to_anthropic(self) -> &'static str {
        match self {
            Self::EndTurn => "end_turn",
            Self::MaxTokens => "max_tokens",
            Self::ToolUse => "tool_use",
            // Anthropic 无 content_filter 终态，最接近的安全终态为 refusal 之外的 end_turn
            Self::ContentFilter => "end_turn",
        }
    }

    pub fn to_gemini(self) -> &'static str {
        match self {
            Self::EndTurn => "STOP",
            Self::MaxTokens => "MAX_TOKENS",
            Self::ToolUse => "STOP",
            Self::ContentFilter => "SAFETY",
        }
    }
}

/// 跨协议统一的流式事件。
///
/// Parser 把上游 SSE 压缩为此枚举序列；Emitter 按客户端协议重建。
#[derive(Debug, Clone)]
pub enum UniversalStreamEvent {
    /// 思考/推理增量（Anthropic thinking、DeepSeek reasoning_content、Gemini thought）
    ReasoningDelta(String),
    /// 正文文本增量
    TextDelta(String),
    /// 工具调用声明：首个可见分片时发出一次，携带完整元数据
    ToolCallStart {
        /// 工具调用序号（协议内递增，从 0 开始）
        index: u64,
        call_id: String,
        name: String,
    },
    /// 工具调用参数 JSON 片段
    ToolCallDelta { index: u64, fragment: String },
    /// 流终止：停止原因 + 全量用量（各 parser 在 finish 时合成一次）
    Finish {
        reason: StopReason,
        usage: UniversalUsage,
    },
}

/// 把 IR 用量换算为 OpenAI Chat usage JSON（prompt 含缓存，明细放 details）
pub fn usage_to_chat_json(usage: &UniversalUsage) -> JsonValue {
    json!({
        "prompt_tokens": usage.input_tokens,
        "completion_tokens": usage.output_tokens,
        "total_tokens": usage.total(),
        "prompt_tokens_details": {
            "cached_tokens": usage.cache_read_tokens,
            "cache_creation_tokens": usage.cache_creation_tokens,
        },
        "completion_tokens_details": {
            "reasoning_tokens": usage.reasoning_tokens,
        },
    })
}

// ===========================================================================
// 请求侧 IR：UniversalRequest
//
// 四个入口各自把原生请求体解析为本结构，出网侧由 Serializer 展开为目标协议
// 原生体。字段完整性由编译期保证——此前「JsonValue 中枢漏字段」类缺陷的根治。
// 协议独占元素（cache_control / thinking signature）在此一等公民建模；
// 跨协议序列化时无对应物的字段按目标协议语义静默降级。
// ===========================================================================

use std::collections::BTreeMap;

/// 图片来源
#[derive(Debug, Clone)]
pub enum ImageSource {
    Base64 { media_type: String, data: String },
    Url(String),
}

/// Anthropic prompt caching 标记（cache_control: {"type":"ephemeral"}）
#[derive(Debug, Clone)]
pub struct CacheControl {
    pub kind: String,
}

/// 消息内容部件：跨协议统一的内容原子
#[derive(Debug, Clone)]
pub enum PartKind {
    Text { text: String },
    Image(ImageSource),
    /// 思考块；signature 为 Anthropic 思考链连续性凭证，同协议回写时必须携带
    Thinking { text: String, signature: Option<String> },
    ToolUse { call_id: String, name: String, input: JsonValue },
    ToolResult { call_id: String, content: String, is_error: bool },
    /// 当前版本无法结构化表达的输入（如音频），以文本提示形式跨协议传递
    Unsupported { hint: String },
}

#[derive(Debug, Clone)]
pub struct ContentPart {
    pub kind: PartKind,
    pub cache_control: Option<CacheControl>,
}

impl ContentPart {
    pub fn text(text: impl Into<String>) -> Self {
        Self { kind: PartKind::Text { text: text.into() }, cache_control: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone)]
pub struct UniversalMessage {
    pub role: Role,
    pub parts: Vec<ContentPart>,
}

#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema 对象
    pub input_schema: JsonValue,
}

#[derive(Debug, Clone)]
pub enum ToolChoice {
    Auto,
    Required,
    None_,
    Tool { name: String },
}

/// 推理控制：effort 为离散档位（Responses/OpenAI 方言），budget 为 token 预算
/// （Anthropic thinking.budget_tokens）；两者不互译，序列化器各取所需。
#[derive(Debug, Clone, Default)]
pub struct ReasoningConfig {
    pub effort: Option<String>,
    pub budget_tokens: Option<u64>,
}

/// 请求侧通用对象：所有入口协议的唯一规范化表示
#[derive(Debug, Clone, Default)]
pub struct UniversalRequest {
    pub model: String,
    /// system 提示（chat 的 role:system、anthropic 顶层 system、
    /// gemini systemInstruction、responses instructions 的统一归宿）
    pub system: Vec<ContentPart>,
    pub messages: Vec<UniversalMessage>,
    pub tools: Vec<ToolDef>,
    pub tool_choice: Option<ToolChoice>,
    pub parallel_tool_calls: Option<bool>,
    pub reasoning: Option<ReasoningConfig>,
    /// Anthropic 必填；缺省时由 anthropic 序列化器补默认值
    pub max_tokens: Option<u64>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    /// Anthropic/Gemini 支持；Chat 序列化器忽略
    pub top_k: Option<u32>,
    pub stop_sequences: Vec<String>,
    /// Chat 方言的 response_format（json_object / json_schema），原样透传
    pub response_format: Option<JsonValue>,
    pub stream: bool,
    /// Anthropic metadata 等协议级附属信息
    pub metadata: Option<JsonValue>,
    /// 未识别字段的保真通道：目标协议认识时由序列化器回填
    pub extra: BTreeMap<String, JsonValue>,
    /// 入站协议标记（"chat"/"anthropic"/"responses"/"gemini"），
    /// 供序列化器判断 extra 键是否源自同协议请求体：
    /// chat→chat 可全量回填，跨协议仅回填目标协议认识的白名单。
    pub source: Option<&'static str>,
}

impl UniversalRequest {
    pub fn new(model: impl Into<String>) -> Self {
        Self { model: model.into(), ..Default::default() }
    }
}
