//! 站点自定义路由 —— 按 host 分派的站点级操作定制层。
//!
//! 分派优先级：先按 systemType 进入架构业务方法（NewAPI / Sub2API / 白与黑…），
//! 业务方法内部再按站点 host 查这里的路由：命中注册站点的专属实现走自定义
//! （不再进入该操作的降级链），未命中走通用逻辑。每个操作（获取访问令牌 /
//! 账号额度 / Key 模型 / 签到 / 健康度）各有独立的路由函数与检查点，站点只注册
//! 实际需要的操作，其余自动回落通用实现。
//!
//! 这里是全项目「按 host 特判」的唯一收口：新增站点定制只改本文件（注册表 +
//! 对应操作函数），不再往 sync / catalog 的流程代码里散落 `if host == ...`。

use url::Url;

/// 「获取访问令牌」操作的自定义实现。
#[derive(Clone, Copy)]
pub(crate) enum TokenFlow {
    /// 直连 `GET /api/user/token` 换取访问令牌：成功即用，失败原样报错。
    /// 用于令牌接口稳定、无盾的站点；不进入缓存令牌校验 / 自愈重试 /
    /// 浏览器兜底等降级链。
    DirectUserToken,
}

/// 「模型健康度」操作的自定义实现。
#[derive(Clone, Copy)]
pub(crate) enum HealthFlow {
    /// 站点自建监控站的公开嵌入健康度接口（跨域子域，匿名可读，JSON）。
    PublicEmbed(&'static str),
    /// 站点自建监控站的公开 HTML 状态页（无 JSON 接口，从页面标记里解析
    /// 模型状态；跨域子域，匿名可读）。
    PublicEmbedHtml(&'static str),
    /// 浏览器内刷新令牌后抓取：站点健康度只认短时效访问令牌，且刷新令牌
    /// 只能在 Chrome 页面里轮换（直连 refresh 会让浏览器登出）。直连拿不到
    /// 时由 Chrome 同源桥接完成「refresh → 抓健康度」。
    BrowserRefresh,
    /// Sub2API「渠道监控 v2」矩阵接口：按 platform_model 下发逐小时健康等级，
    /// 需要登录令牌（Chrome LocalStorage 的 auth_token，Bearer 鉴权）。
    Sub2ApiChannelMonitor(&'static str),
}

/// 「账号额度」操作的自定义实现。暂无站点注册——扩展示范点：
/// 有站点需要专属账号流程时在此加变体，并到 `fetch_site_account` 的
/// 检查点分派。
pub(crate) enum AccountFlow {}

/// 「Key/模型」操作的自定义实现。暂无站点注册。
pub(crate) enum KeysFlow {}

/// 「签到」操作的自定义实现。暂无站点注册。
pub(crate) enum CheckinFlow {}

/// 单站点自定义路由配置：字段按操作独立，`None` 表示该操作走通用逻辑。
struct SiteRoutes {
    /// 注册域：host 等于它或以 `.注册域` 结尾即命中（忽略 `www.`）。
    domain: &'static str,
    /// 平台探测提示：systemType 为空时探测流水线命中本表即直接采用。
    platform_hint: Option<&'static str>,
    token: Option<TokenFlow>,
    health: Option<HealthFlow>,
}

/// 站点注册表 —— 新增/调整站点定制只改这里。
const SITE_ROUTES: &[SiteRoutes] = &[
    // 42x：访问令牌由 /api/user/token 直接下发（Cookie 鉴权），令牌接口稳定、
    // 站点无需过盾，不需要缓存令牌校验与浏览器兜底；接口形态属 NewAPI 刷新
    // 令牌一族，探测提示固定为 newapi2。
    SiteRoutes {
        domain: "42x.shop",
        platform_hint: Some("newapi2"),
        token: Some(TokenFlow::DirectUserToken),
        health: None,
    },
    // x666（薄荷 API）：主域的「模型状态」接口要求登录（未登录回 401），逐模型 ×
    // 逐时段状态由独立工具子域公开下发（实测 tool.x666.me 匿名可读、11 个模型各带
    // 24 格 slot_data）。响应形状与「模型状态」增强模块一致，复用
    // `parse_model_status_health`。用 `all` 而不是 `batch`：batch 只回一个名为
    // "batch" 的聚合条目，没有逐模型数据。
    SiteRoutes {
        domain: "x666.me",
        platform_hint: None,
        token: None,
        health: Some(HealthFlow::PublicEmbed(
            "https://tool.x666.me/api/model-status/embed/status/all?window=24h",
        )),
    },
    // hyxware.com（HyxAPI）：健康度接口只认 15 分钟有效期的访问令牌；站点
    // 没有标准的 `/api/user/token`（普通账号 403），且刷新令牌只能在 Chrome
    // 页面里轮换（直连 refresh 会把浏览器登出）。直连因此永远 401、健康度
    // 全灰，注册浏览器刷新流程兜底：页面内 refresh 后用临时令牌抓取。
    SiteRoutes {
        domain: "hyxware.com",
        platform_hint: Some("newapi2"),
        token: None,
        health: Some(HealthFlow::BrowserRefresh),
    },
    // api.kunyou.asia（虾蹬王）：Sub2API 站点，健康度由站长自建的监控页
    // 公开提供，页面是 HTML（无 JSON 接口），按模型状态 chip 解析。
    SiteRoutes {
        domain: "kunyou.asia",
        platform_hint: Some("sub2api"),
        token: None,
        health: Some(HealthFlow::PublicEmbedHtml(
            "https://account-doctor.woshikunyou.workers.dev/status",
        )),
    },
    // api.daigua.icu（呆瓜 API）：Sub2API 站点的「渠道监控 v2」矩阵接口，
    // 按 platform_model 分组下发逐小时健康等级；匿名 401，需带 Chrome
    // LocalStorage 里的 auth_token（Bearer），与模型市场共用同一凭据。
    SiteRoutes {
        domain: "daigua.icu",
        platform_hint: Some("sub2api"),
        token: None,
        health: Some(HealthFlow::Sub2ApiChannelMonitor(
            "https://api.daigua.icu/api/v1/channel-monitor-v2/matrix?range=24h&group_by=platform_model&timezone=Asia%2FShanghai",
        )),
    },
];

/// 按 host 匹配注册站点：等于注册域或其子域即命中（忽略 `www.`）。
/// 后缀必须落在域名边界上——`42x.shop.evil.com`、`not42x.shop` 不命中。
fn site_routes(base_url: &Url) -> Option<&'static SiteRoutes> {
    let host = base_url.host_str()?.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    SITE_ROUTES
        .iter()
        .find(|routes| host == routes.domain || host.ends_with(&format!(".{}", routes.domain)))
}

/// 「获取访问令牌」路由：命中返回站点专属实现，未命中走通用 NewAPI 链路。
pub(crate) fn token_flow(base_url: &Url) -> Option<TokenFlow> {
    site_routes(base_url).and_then(|routes| routes.token)
}

/// 「模型健康度」路由：命中返回站点专属数据源，未命中走通用四级回退。
pub(crate) fn health_flow(base_url: &Url) -> Option<HealthFlow> {
    site_routes(base_url).and_then(|routes| routes.health)
}

/// 站点注册的架构探测提示（host 命中即生效，优先于通用域名规则）。
pub(crate) fn platform_hint(base_url: &Url) -> Option<&'static str> {
    site_routes(base_url).and_then(|routes| routes.platform_hint)
}

/// 站点 API 主机对应的公开嵌入健康度接口（`HealthFlow::PublicEmbed` 的取值封装）。
pub(crate) fn public_embed_health_url(base_url: &Url) -> Option<&'static str> {
    match health_flow(base_url) {
        Some(HealthFlow::PublicEmbed(url)) => Some(url),
        _ => None,
    }
}

/// 站点 API 主机对应的公开 HTML 健康度页（`HealthFlow::PublicEmbedHtml` 的取值封装）。
pub(crate) fn public_embed_html_health_url(base_url: &Url) -> Option<&'static str> {
    match health_flow(base_url) {
        Some(HealthFlow::PublicEmbedHtml(url)) => Some(url),
        _ => None,
    }
}

/// 站点健康度是否需要 Chrome 页面内刷新令牌后抓取（`HealthFlow::BrowserRefresh`）。
pub(crate) fn health_flow_requires_browser_refresh(base_url: &Url) -> bool {
    matches!(health_flow(base_url), Some(HealthFlow::BrowserRefresh))
}

/// Sub2API「渠道监控 v2」矩阵健康度接口（`HealthFlow::Sub2ApiChannelMonitor`）。
pub(crate) fn sub2api_channel_monitor_health_url(base_url: &Url) -> Option<&'static str> {
    match health_flow(base_url) {
        Some(HealthFlow::Sub2ApiChannelMonitor(url)) => Some(url),
        _ => None,
    }
}

/// 「账号额度」路由：暂无站点注册，全部走通用逻辑。
pub(crate) fn account_flow(_base_url: &Url) -> Option<AccountFlow> {
    None
}

/// 「Key/模型」路由：暂无站点注册，全部走通用逻辑。
pub(crate) fn keys_flow(_base_url: &Url) -> Option<KeysFlow> {
    None
}

/// 「签到」路由：暂无站点注册，全部走通用逻辑。
pub(crate) fn checkin_flow(_base_url: &Url) -> Option<CheckinFlow> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daigua_health_flow_uses_channel_monitor_matrix() {
        let base = Url::parse("https://api.daigua.icu/").unwrap();
        assert_eq!(platform_hint(&base), Some("sub2api"));
        assert!(sub2api_channel_monitor_health_url(&base).is_some());
        assert!(public_embed_health_url(&base).is_none());
        assert!(public_embed_html_health_url(&base).is_none());
        assert!(!health_flow_requires_browser_refresh(&base));

        let evil = Url::parse("https://daigua.icu.evil.com/").unwrap();
        assert!(sub2api_channel_monitor_health_url(&evil).is_none());
    }

    #[test]
    fn hyxware_health_flow_uses_browser_refresh() {
        let base = Url::parse("https://hyxware.com/").unwrap();
        assert_eq!(platform_hint(&base), Some("newapi2"));
        assert!(health_flow_requires_browser_refresh(&base));
        assert!(public_embed_health_url(&base).is_none());

        // 子域命中；伪造后缀不得命中。
        let sub = Url::parse("https://api.hyxware.com/").unwrap();
        assert!(health_flow_requires_browser_refresh(&sub));
        let evil = Url::parse("https://hyxware.com.evil.com/").unwrap();
        assert!(!health_flow_requires_browser_refresh(&evil));
    }

    #[test]
    fn x666_health_flow_uses_public_embed() {
        let base = Url::parse("https://x666.me/").unwrap();
        assert!(public_embed_health_url(&base).is_some());
        assert!(!health_flow_requires_browser_refresh(&base));
    }

    #[test]
    fn kunyou_health_flow_uses_public_html_embed() {
        let base = Url::parse("https://api.kunyou.asia/").unwrap();
        assert_eq!(platform_hint(&base), Some("sub2api"));
        assert_eq!(
            public_embed_html_health_url(&base),
            Some("https://account-doctor.woshikunyou.workers.dev/status")
        );
        assert!(public_embed_health_url(&base).is_none());
        assert!(!health_flow_requires_browser_refresh(&base));
    }
}
