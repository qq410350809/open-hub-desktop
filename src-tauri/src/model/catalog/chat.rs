//! 站点模型对话测试：以站点 Key 直连 `/v1/chat/completions`（SSE 流式），
//! 逐增量经 Tauri Channel 推送到前端「模型测试」对话弹窗。
//! 仅桌面形态提供：Web/瘦客户端走 HTTP RPC，不支持流式推送。

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures_util::stream::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;

use crate::context::{AppContext, Managed};
use crate::db::build_site_http_client;
use crate::model::gateway::stream::SseLineReader;
use crate::site::sync::{chrome_request_headers, chrome_user_agent};

/// 对话测试总超时：覆盖建连 + 全部流式输出；测试会话通常远短于该值。
const CHAT_TEST_TIMEOUT: Duration = Duration::from_secs(300);
/// 非 2xx 响应体错误说明的最大保留字符数。
const ERROR_BODY_CHARS: usize = 400;

/// 一条对话消息（前端按多轮历史整体下发）。
#[derive(Debug, Deserialize)]
pub(crate) struct ChatTurnMessage {
    pub(crate) role: String,
    pub(crate) content: String,
}

/// 流式事件负载：kind = delta（增量）| done（正常结束）| error（失败）| cancelled（已中止）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatStreamEvent {
    pub(crate) kind: String,
    /// delta：本轮新增正文增量
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) content: Option<String>,
    /// delta：本轮新增思考增量（DeepSeek reasoning_content 等）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning: Option<String>,
    /// error：失败说明；cancelled：固定提示
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) message: Option<String>,
    /// done：首字延迟（毫秒）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) first_token_ms: Option<u128>,
    /// done：总耗时（毫秒）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) total_ms: Option<u128>,
    /// done：正文累计字符数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) chars: Option<usize>,
}

impl ChatStreamEvent {
    fn delta(content: Option<String>, reasoning: Option<String>) -> Self {
        Self { kind: "delta".into(), content, reasoning, message: None, first_token_ms: None, total_ms: None, chars: None }
    }

    fn done(first_token_ms: Option<u128>, total_ms: u128, chars: usize) -> Self {
        Self { kind: "done".into(), content: None, reasoning: None, message: None, first_token_ms, total_ms: Some(total_ms), chars: Some(chars) }
    }

    fn error(message: String) -> Self {
        Self { kind: "error".into(), content: None, reasoning: None, message: Some(message), first_token_ms: None, total_ms: None, chars: None }
    }

    fn cancelled() -> Self {
        Self { kind: "cancelled".into(), content: None, reasoning: None, message: Some("已中止".into()), first_token_ms: None, total_ms: None, chars: None }
    }
}

/// 用户请求中止的流式请求 ID 集合（一次取消只生效一次，取用即移除）。
fn chat_cancelled() -> &'static Mutex<HashSet<String>> {
    static CANCELLED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    CANCELLED.get_or_init(|| Mutex::new(HashSet::new()))
}

fn take_chat_cancelled(request_id: &str) -> bool {
    chat_cancelled()
        .lock()
        .map(|mut set| set.remove(request_id))
        .unwrap_or(false)
}

/// 前端「停止」按钮：登记请求 ID，流式循环在每个分块间隙检查并退出。
#[tauri::command]
pub fn site_model_chat_cancel(request_id: String) -> Result<(), String> {
    match chat_cancelled().lock() {
        Ok(mut set) => {
            set.insert(request_id);
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    }
}

/// 与站点模型流式对话：SSE 每个增量即时经 Channel 推送，最终回 done/error。
#[tauri::command]
pub async fn site_model_chat_stream(
    ctx: Managed<'_, Arc<AppContext>>,
    channel: Channel<ChatStreamEvent>,
    request_id: String,
    url: String,
    api_key: String,
    model: String,
    thinking_level: Option<String>,
    thinking_budget: Option<u64>,
    messages: Vec<ChatTurnMessage>,
) -> Result<(), String> {
    // 走站点的代理池出口设置，与站点模型同步共用同一客户端构造逻辑。
    let client = build_site_http_client(&*ctx.database, CHAT_TEST_TIMEOUT, 3, "模型对话测试")?;
    // 思考预算下限对齐 Anthropic（min 1024）
    let thinking_budget = thinking_budget.map(|budget| budget.max(1024));
    run_chat_stream(
        client,
        channel,
        request_id,
        url,
        api_key,
        model,
        thinking_level,
        thinking_budget,
        messages,
    )
    .await
}

async fn run_chat_stream(
    client: wreq::Client,
    channel: Channel<ChatStreamEvent>,
    request_id: String,
    url: String,
    api_key: String,
    model: String,
    thinking_level: Option<String>,
    thinking_budget: Option<u64>,
    messages: Vec<ChatTurnMessage>,
) -> Result<(), String> {
    let base_url = normalize_test_base_url(&url)?;
    let endpoint = base_url
        .join("/v1/chat/completions")
        .map_err(|_| "无法生成 /v1/chat/completions 地址".to_string())?;
    let mut payload = serde_json::json!({
        "model": model,
        "messages": messages
            .iter()
            .map(|turn| serde_json::json!({ "role": turn.role, "content": turn.content }))
            .collect::<Vec<_>>(),
        "stream": true,
    });
    // 思考级别：各家参数约定不一（GLM/Qwen 用 thinking/enable_thinking，
    // OpenAI o 系用 reasoning_effort，Claude/Gemini/Qwen 支持预算），
    // 这里按意图携带对应参数以扩大兼容面；站点不认时报错会原样回显到对话里，
    // 本身就是测试信息。
    match (thinking_level.as_deref(), thinking_budget) {
        (Some("off"), _) => {
            payload["thinking"] = serde_json::json!({ "type": "disabled" });
            payload["enable_thinking"] = serde_json::json!(false);
        }
        (Some("minimal"), _) => {
            // GPT-5 系：几乎不推理但非完全关闭，独立于关闭档
            payload["reasoning_effort"] = serde_json::json!("minimal");
        }
        (Some("budget"), Some(budget)) => {
            // Anthropic 原生形状 + Qwen thinking_budget，聚合站会自行转换
            payload["thinking"] =
                serde_json::json!({ "type": "enabled", "budget_tokens": budget });
            payload["thinking_budget"] = serde_json::json!(budget);
        }
        (Some(level), _) if matches!(level, "low" | "medium" | "high" | "xhigh" | "max") => {
            payload["reasoning_effort"] = serde_json::json!(level);
            payload["thinking"] = serde_json::json!({ "type": "enabled" });
        }
        _ => {}
    }
    let user_agent = chrome_user_agent();
    let request = chrome_request_headers(client.post(endpoint), base_url.as_str(), &user_agent)
        .bearer_auth(&api_key)
        .json(&payload);

    let started = Instant::now();
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            let _ = channel.send(ChatStreamEvent::error(format!("请求失败：{error}")));
            return Ok(());
        }
    };

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let excerpt: String = body.chars().take(ERROR_BODY_CHARS).collect();
        let _ = channel.send(ChatStreamEvent::error(format!(
            "HTTP {status}：{excerpt}"
        )));
        return Ok(());
    }

    let mut stream = response.bytes_stream();
    // 跨 chunk 安全的 SSE 行读取（字节级缓冲，见 gateway::stream 注释）
    let mut reader = SseLineReader::new();
    let mut splitter = ThinkSplitter::default();
    let mut first_token_ms: Option<u128> = None;
    let mut chars = 0usize;

    loop {
        if take_chat_cancelled(&request_id) {
            let _ = channel.send(ChatStreamEvent::cancelled());
            return Ok(());
        }
        let Some(chunk) = stream.next().await else { break };
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                let _ = channel.send(ChatStreamEvent::error(format!("读取流式响应失败：{error}")));
                return Ok(());
            }
        };
        for line in reader.push(&chunk) {
            let Some(rest) = line.strip_prefix("data:") else { continue };
            let data = rest.strip_prefix(' ').unwrap_or(rest);
            if data == "[DONE]" {
                flush_think_tail(&mut splitter, &channel, &mut chars);
                let _ = channel.send(ChatStreamEvent::done(
                    first_token_ms,
                    started.elapsed().as_millis(),
                    chars,
                ));
                return Ok(());
            }
            if data.trim().is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
                continue;
            };
            // 站点可能以 SSE 帧内嵌 error 对象表达失败
            if let Some(error) = value.get("error") {
                let message = error
                    .get("message")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| error.to_string());
                let _ = channel.send(ChatStreamEvent::error(message));
                return Ok(());
            }
            let Some(delta) = value
                .get("choices")
                .and_then(|choices| choices.get(0))
                .and_then(|choice| choice.get("delta"))
            else {
                continue;
            };
            let reasoning = delta
                .get("reasoning_content")
                .or_else(|| delta.get("reasoning"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let content = delta.get("content").and_then(|v| v.as_str()).unwrap_or_default();
            if reasoning.is_empty() && content.is_empty() {
                continue;
            }
            // <think> 内嵌思考改道：部分模型把思考过程混在 content 里下发
            let (think_delta, content_delta) = splitter.feed(content);
            let reasoning_out = format!("{reasoning}{think_delta}");
            let content_out = content_delta;
            if reasoning_out.is_empty() && content_out.is_empty() {
                continue;
            }
            if first_token_ms.is_none() {
                first_token_ms = Some(started.elapsed().as_millis());
            }
            chars += content_out.chars().count();
            let _ = channel.send(ChatStreamEvent::delta(
                (!content_out.is_empty()).then_some(content_out),
                (!reasoning_out.is_empty()).then_some(reasoning_out),
            ));
        }
    }

    // 服务端未发 [DONE] 即断流：按正常结束收尾
    flush_think_tail(&mut splitter, &channel, &mut chars);
    let _ = channel.send(ChatStreamEvent::done(
        first_token_ms,
        started.elapsed().as_millis(),
        chars,
    ));
    Ok(())
}

/// 流结束时冲刷 <think> 拆分器残留（思考尾段或半个标签的正文尾巴）
fn flush_think_tail(
    splitter: &mut ThinkSplitter,
    channel: &Channel<ChatStreamEvent>,
    chars: &mut usize,
) {
    let (think_tail, content_tail) = splitter.flush();
    if !think_tail.is_empty() {
        let _ = channel.send(ChatStreamEvent::delta(None, Some(think_tail)));
    }
    if !content_tail.is_empty() {
        *chars += content_tail.chars().count();
        let _ = channel.send(ChatStreamEvent::delta(Some(content_tail), None));
    }
}

/// `<think>` 标签拆分器：部分推理模型把思考过程以 `<think>…</think>` 内嵌在
/// content 增量里下发，这段内容需改道到 reasoning 展示，否则会被 Markdown
/// 渲染吞掉或与正文混排。仅在正文开头出现标签时启用，正文中间的同类字样
/// 不受影响；标签可能被网络分块切断，靠 pending 残留缓冲跨片识别。
#[derive(Default)]
struct ThinkSplitter {
    in_think: bool,
    /// 跨 chunk 的未完整标签 / 思考尾段残留
    pending: String,
    /// 是否已过开头判定（首段非空白内容消费后，不再尝试进入思考块）
    header_checked: bool,
}

const THINK_OPEN: &str = "<think>";
const THINK_CLOSE: &str = "</think>";

impl ThinkSplitter {
    fn feed(&mut self, text: &str) -> (String, String) {
        let mut reasoning = String::new();
        let mut content = String::new();
        let mut buf = std::mem::take(&mut self.pending) + text;

        // 开头判定：允许先跳过空白再识别 <think>；判定过后正文按普通内容处理
        if !self.in_think && !self.header_checked {
            let trimmed = buf.trim_start();
            if trimmed.is_empty() {
                content.push_str(&buf);
                return (reasoning, content);
            }
            if let Some(rest) = trimmed.strip_prefix(THINK_OPEN) {
                self.header_checked = true;
                self.in_think = true;
                buf = rest.to_string();
            } else if THINK_OPEN.starts_with(trimmed) {
                // 可能是被切断的标签开头：整体留到下一片再判定
                self.pending = buf;
                return (reasoning, content);
            } else {
                self.header_checked = true;
                content.push_str(&buf);
                return (reasoning, content);
            }
        }

        loop {
            if self.in_think {
                match buf.find(THINK_CLOSE) {
                    Some(pos) => {
                        reasoning.push_str(&buf[..pos]);
                        buf = buf[pos + THINK_CLOSE.len()..].to_string();
                        self.in_think = false;
                    }
                    None => {
                        let keep = partial_tag_tail_len(&buf, THINK_CLOSE);
                        let split_at = buf.len() - keep;
                        reasoning.push_str(&buf[..split_at]);
                        self.pending = buf[split_at..].to_string();
                        return (reasoning, content);
                    }
                }
            } else {
                // 已进入过思考块或开头非 <think>：剩余内容全部是正文
                content.push_str(&buf);
                self.pending.clear();
                return (reasoning, content);
            }
        }
    }

    /// 流结束时冲刷残留：仍在思考块内 → 归思考；否则归正文
    fn flush(&mut self) -> (String, String) {
        let pending = std::mem::take(&mut self.pending);
        if self.in_think {
            (pending, String::new())
        } else {
            (String::new(), pending)
        }
    }
}

/// buf 末尾恰好是 tag 前缀的长度（防止关闭标签被网络分块切断而漏判）
fn partial_tag_tail_len(buf: &str, tag: &str) -> usize {
    let max = tag.len().saturating_sub(1).min(buf.len());
    for k in (1..=max).rev() {
        if buf.ends_with(&tag[..k]) {
            return k;
        }
    }
    0
}

/// 归一化站点 API 地址：补协议头并解析（补尾斜杠交给 Url::join 的根路径语义）。
fn normalize_test_base_url(raw: &str) -> Result<url::Url, String> {
    let base = raw.trim();
    let with_scheme = if base.starts_with("http://") || base.starts_with("https://") {
        base.to_string()
    } else {
        format!("https://{base}")
    };
    url::Url::parse(&with_scheme).map_err(|_| "站点 API 地址无效".to_string())
}

#[cfg(test)]
mod tests {
    use super::ThinkSplitter;

    fn feed_all(parts: &[&str]) -> (String, String) {
        let mut splitter = ThinkSplitter::default();
        let mut reasoning = String::new();
        let mut content = String::new();
        for part in parts {
            let (r, c) = splitter.feed(part);
            reasoning.push_str(&r);
            content.push_str(&c);
        }
        let (r, c) = splitter.flush();
        reasoning.push_str(&r);
        content.push_str(&c);
        (reasoning, content)
    }

    #[test]
    fn plain_content_passes_through() {
        assert_eq!(feed_all(&["你好，", "世界！"]), (String::new(), "你好，世界！".into()));
    }

    #[test]
    fn inline_think_block_is_split() {
        let (r, c) = feed_all(&["<think>先分析一下</think>答案是 6。"]);
        assert_eq!(r, "先分析一下");
        assert_eq!(c, "答案是 6。");
    }

    #[test]
    fn tags_split_across_chunks() {
        let (r, c) = feed_all(&["<th", "ink>思考中，含中文与</thi", "nk>正文开始"]);
        assert_eq!(r, "思考中，含中文与");
        assert_eq!(c, "正文开始");
    }

    #[test]
    fn leading_whitespace_before_think() {
        let (r, c) = feed_all(&["\n\n<think>", "思考</think>", "正文"]);
        assert_eq!(r, "思考");
        assert_eq!(c, "正文");
    }

    #[test]
    fn unclosed_think_flushes_to_reasoning() {
        let (r, c) = feed_all(&["<think>思考到一半就结束了"]);
        assert_eq!(r, "思考到一半就结束了");
        assert_eq!(c, "");
    }

    #[test]
    fn mid_content_think_stays_in_content() {
        let (r, c) = feed_all(&["先说结论。", "中间出现<think>不算思考块"]);
        assert_eq!(r, "");
        assert_eq!(c, "先说结论。中间出现<think>不算思考块");
    }

    #[test]
    fn empty_reasoning_deltas_with_think_only_model() {
        // 常见节奏：思考以 <think> 增量混在 content 里逐片下发（reasoning_content 字段为空）
        let mut splitter = ThinkSplitter::default();
        let mut reasoning = String::new();
        let mut content = String::new();
        for text in ["<think>第一段。", "第二段。", "</think>", "结论。"] {
            let (tr, tc) = splitter.feed(text);
            reasoning.push_str(&tr);
            content.push_str(&tc);
        }
        let (tr, tc) = splitter.flush();
        reasoning.push_str(&tr);
        content.push_str(&tc);
        assert_eq!(reasoning, "第一段。第二段。");
        assert_eq!(content, "结论。");
    }
}
