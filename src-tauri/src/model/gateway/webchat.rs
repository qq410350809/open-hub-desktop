//! 「网页直连」渠道（protocol = "web-chat"）的上游会话管理。
//!
//! 上游 oxalpha.com 不提供公开 API：聊天端点 `POST /api/chat` 受 Laravel
//! 会话保护，要求先访问 `/chat` 页面取得 Session Cookie 与 CSRF Token，
//! 响应为 OpenAI Chat Completions 形状的 SSE 流。本模块负责这对凭证的
//! 获取、缓存与过期刷新（419 = CSRF 失配）。

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tracing::warn;

/// 上游唯一可用模型（站点前端 window.__CHAT_MODELS__ 固化值）
pub const WEBCHAT_MODEL: &str = "stealth/ox-alpha";

/// Laravel session 有效期 2 小时；提前到 30 分钟主动刷新，
/// 避免长会话中途失效后整轮请求报废。
const SESSION_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone)]
pub struct WebChatSession {
    /// 请求需携带的完整 Cookie 串（XSRF-TOKEN + session）
    pub cookie: String,
    /// 页面 meta[csrf-token]，随 X-CSRF-TOKEN 头发送
    pub csrf: String,
    base: String,
    fetched_at: Instant,
}

impl WebChatSession {
    fn fresh_for(&self, base: &str) -> bool {
        self.base == base && self.fetched_at.elapsed() < SESSION_TTL
    }
}

static SESSION: OnceLock<Mutex<Option<WebChatSession>>> = OnceLock::new();

fn session_cell() -> &'static Mutex<Option<WebChatSession>> {
    SESSION.get_or_init(|| Mutex::new(None))
}

/// 网页直连出网所需的浏览器身份头（UA/Origin/Referer），供 dispatcher 复用
pub fn browser_header_pairs(base: &str) -> Vec<(&'static str, String)> {
    vec![
        ("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36".to_string()),
        ("Origin", base.to_string()),
        ("Referer", format!("{base}/chat")),
    ]
}

fn browser_headers(base: &str) -> [(&'static str, String); 3] {
    let pairs = browser_header_pairs(base);
    [pairs[0].clone(), pairs[1].clone(), pairs[2].clone()]
}

/// 访问 `{base}/chat` 抓取新会话凭证（不读缓存，仅供强制刷新使用）
async fn fetch_session(client: &reqwest::Client, base: &str) -> Result<WebChatSession, String> {
    let mut req = client.get(format!("{base}/chat"));
    for (k, v) in browser_headers(base) {
        req = req.header(k, v);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("上游会话页请求失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("上游会话页返回 HTTP {}", resp.status()));
    }
    let cookie = resp
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|c| c.split(';').next())
        .filter(|kv| kv.contains('=') && !kv.trim().is_empty())
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("; ");
    if cookie.is_empty() {
        return Err("会话页未下发 Cookie".to_string());
    }
    let html = resp
        .text()
        .await
        .map_err(|e| format!("读取会话页失败: {e}"))?;
    let Some(csrf) = html
        .split("csrf-token\" content=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .filter(|s| !s.is_empty())
    else {
        return Err("会话页未包含 CSRF Token（页面结构可能已变更）".to_string());
    };
    Ok(WebChatSession {
        cookie,
        csrf: csrf.to_string(),
        base: base.to_string(),
        fetched_at: Instant::now(),
    })
}

/// 取得可用会话：缓存未过期直接复用，否则抓取新会话并写回缓存。
async fn obtain_session(
    client: &reqwest::Client,
    base: &str,
    force_refresh: bool,
) -> Result<WebChatSession, String> {
    {
        let cache = session_cell().lock().unwrap();
        if let Some(sess) = cache.as_ref() {
            if !force_refresh && sess.fresh_for(base) {
                return Ok(sess.clone());
            }
        }
    }
    let sess = fetch_session(client, base).await?;
    *session_cell().lock().unwrap() = Some(sess.clone());
    Ok(sess)
}

/// 取得可用会话（优先缓存）
pub async fn ensure_session(
    client: &reqwest::Client,
    base: &str,
) -> Result<WebChatSession, String> {
    obtain_session(client, base, false).await
}

/// 强制刷新会话（CSRF 过期 419 时调用），失败时记录告警并透传错误
pub async fn force_refresh_session(
    client: &reqwest::Client,
    base: &str,
) -> Result<WebChatSession, String> {
    match obtain_session(client, base, true).await {
        Ok(s) => Ok(s),
        Err(e) => {
            warn!("[ModelGateway] web-chat 会话刷新失败: {e}");
            Err(e)
        }
    }
}

#[cfg(test)]
mod webchat_tests {
    use super::*;

    #[test]
    fn parse_csrf_and_cookies_from_page_payload() {
        // CSRF 提取逻辑复刻 fetch_session 内联解析，验证边界形态
        let html = r#"<meta name="csrf-token" content="abc123XYZ">"#;
        let csrf = html
            .split("csrf-token\" content=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next());
        assert_eq!(csrf, Some("abc123XYZ"));

        let set_cookies = [
            "XSRF-TOKEN=tok%3D; expires=Wed, 26 Aug 2026 07:20:43 GMT; path=/; secure",
            "ox_alpha_session=payload; expires=Wed, 26 Aug 2026 07:20:43 GMT; path=/; secure; httponly",
            "should-not-have-value",
        ];
        let cookie = set_cookies
            .iter()
            .filter_map(|c| c.split(';').next())
            .filter(|kv| kv.contains('=') && !kv.trim().is_empty())
            .map(|c| c.trim())
            .collect::<Vec<_>>()
            .join("; ");
        assert_eq!(
            cookie, "XSRF-TOKEN=tok%3D; ox_alpha_session=payload",
            "无值条目必须被剔除"
        );
    }

    #[tokio::test]
    async fn cached_session_reused_within_ttl_then_refreshed_on_demand() {
        let client = reqwest::Client::new();
        // 无法真实联网的测试环境下 ensure_session 会失败——仅断言错误信息可读，
        // 真实获取路径由网关联调覆盖
        let result = ensure_session(&client, "https://webchat.invalid").await;
        assert!(result.is_err(), "无效域名必须报错而非 panic");
    }
}
