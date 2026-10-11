//! 「获取访问令牌」操作的独立实现与自定义路由。
//!
//! 入口 `acquire_site_access_token` 是各业务方法（账号同步 / Key 模型同步）
//! 取令牌的唯一通道，方法头部按 host 查自定义路由：
//! - 命中（api.42x.shop）：直连 `GET /api/user/token`，成功即用，任何失败
//!   原样返回 `Err`——不做缓存令牌校验、自愈重试或浏览器兜底（零降级）；
//! - 未命中：通用 NewAPI 链路（Cookie / 现有凭据换取访问令牌）。

use super::sync::{apply_newapi_auth, chrome_request_headers, request_json};
use crate::models::NewApiAuth;
use crate::site::library::custom::{self, TokenFlow};
use url::Url;

/// 从 `/api/user/token` 一类的响应解析访问令牌，兼容各发行版的大小写与包裹形态。
pub(crate) fn parse_newapi_access_token(value: &serde_json::Value) -> String {
    value
        .pointer("/data/token")
        .or_else(|| value.pointer("/data/access_token"))
        .or_else(|| value.pointer("/data/accessToken"))
        .or_else(|| value.pointer("/token"))
        .or_else(|| value.pointer("/access_token"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| value.pointer("/data").and_then(serde_json::Value::as_str))
        .unwrap_or("")
        .trim()
        .to_string()
}

/// 「获取访问令牌」操作入口 —— 独立方法，头部按 host 走自定义路由。
///
/// 命中自定义实现（api.42x.shop）时直接换取令牌并原样返回失败；
/// 未命中走通用链路：非盾失败返回 `Ok(None)`，由调用方决定后续处理。
pub(crate) async fn acquire_site_access_token(
    client: &wreq::Client,
    base_url: &Url,
    legacy: &NewApiAuth,
    user_agent: &str,
) -> Result<Option<NewApiAuth>, String> {
    match custom::token_flow(base_url) {
        Some(TokenFlow::DirectUserToken) => {
            fetch_direct_user_token(client, base_url, legacy, user_agent)
                .await
                .map(Some)
        }
        None => acquire_newapi_session_token(client, base_url, legacy, user_agent).await,
    }
}

/// 站点直连令牌实现（api.42x.shop）：`GET /api/user/token`，
/// 成功即用、失败原样报错（零降级）。
///
/// 与通用链的差别：不把非盾失败翻译成 `Ok(None)` 交给上层继续降级，
/// 也不留给 Chrome 桥接——端点返回什么就报什么。
async fn fetch_direct_user_token(
    client: &wreq::Client,
    base_url: &Url,
    auth: &NewApiAuth,
    user_agent: &str,
) -> Result<NewApiAuth, String> {
    let endpoint = base_url
        .join("/api/user/token")
        .map_err(|_| "无法生成令牌接口地址".to_string())?;
    let request = apply_newapi_auth(
        chrome_request_headers(client.get(endpoint), base_url.as_str(), user_agent),
        auth,
    );
    let value = request_json(request, "NewAPI Token 接口").await?;
    let token = parse_newapi_access_token(&value);
    if token.is_empty() {
        return Err("NewAPI Token 接口未返回访问令牌".to_string());
    }
    let user_id = match auth {
        NewApiAuth::Legacy { user_id, .. } | NewApiAuth::Token { user_id, .. } => user_id.clone(),
    };
    Ok(NewApiAuth::Token {
        access_token: token,
        user_id,
    })
}

/// 刷新令牌模式下尝试调用 /api/user/token 获取可持久化的访问令牌。
/// 传统 Cookie 模式没有访问令牌机制，调用方不得走到这里。
/// 成功返回 `Some(token_string)`，遇盾返回 `Err(shield_error)`，其他失败返回 `None`。
pub(crate) async fn try_acquire_newapi_token(
    client: &wreq::Client,
    base_url: &Url,
    auth: &NewApiAuth,
    user_agent: &str,
) -> Result<Option<String>, String> {
    let endpoint = match base_url.join("/api/user/token") {
        Ok(url) => url,
        Err(_) => return Ok(None),
    };
    let request = apply_newapi_auth(
        chrome_request_headers(client.get(endpoint), base_url.as_str(), user_agent),
        auth,
    );
    match request_json(request, "NewAPI Token 接口").await {
        Ok(value) => {
            let token = parse_newapi_access_token(&value);
            if token.is_empty() {
                Ok(None)
            } else {
                Ok(Some(token))
            }
        }
        Err(error) => {
            if error.contains("返回 HTML") || error.contains("Cloudflare") {
                Err(error)
            } else {
                Ok(None)
            }
        }
    }
}

/// 刷新令牌模式下用不轮换会话的方式获取 NewAPI 访问令牌。刻意不在浏览器外
/// 调用 /api/user/auth/refresh——refresh 会轮换
/// HttpOnly 刷新令牌，浏览器里的旧令牌随即作废，用户会被登出；旧会话失效时
/// 返回 Ok(None)，由调用方转 Chrome 桥接在浏览器内完成刷新。
/// 返回 Some(NewApiAuth::Token) 表示拿到了可用的访问令牌；
/// Ok(None) 表示本地无可用令牌；Err 表示遇盾需要浏览器验证。
pub(crate) async fn acquire_newapi_session_token(
    client: &wreq::Client,
    base_url: &Url,
    legacy: &NewApiAuth,
    user_agent: &str,
) -> Result<Option<NewApiAuth>, String> {
    let user_id = match legacy {
        NewApiAuth::Legacy { user_id, .. } | NewApiAuth::Token { user_id, .. } => user_id.clone(),
    };
    match try_acquire_newapi_token(client, base_url, legacy, user_agent).await {
        Ok(Some(token)) => Ok(Some(NewApiAuth::Token {
            access_token: token,
            user_id,
        })),
        Ok(None) => Ok(None),
        Err(shield_error) => Err(shield_error),
    }
}
