//! 「白嫖」AI 公益站目录（`https://baipiao.org/charity/`）作为公共库的第二个来源。
//!
//! ## 为什么要单独抓 HTML
//!
//! 这个目录没有公开的站点 JSON 接口（`robots.txt` 只放行 `/api/relay-partner/public`，
//! 那是**中转站**榜单的可见性投影，不含公益站）。公益站列表是 Astro 服务端渲染的
//! 静态 HTML：每张卡片是一个 `<div class="ccard" data-card …>`，站点信息全在
//! `data-*` 属性里，因此这里解析属性而不是文本节点，结构变动时也更容易失败得明显。
//!
//! ## 字段口径
//!
//! - 名称 / 简介 / 标签 / 状态（`active` / `expired`）来自列表卡片，一次请求拿全。
//! - **API 基址来自详情页正文**：列表只给「注册入口」这种落地页链接，真正能接的
//!   `https://xxx/v1` 写在详情页的代码块与「base_url 填 …」的说明里。所以这里对
//!   **新增站点**逐个抓详情页提取（已有站点不重复抓，避免每次同步都打对方 60+ 次）。
//!   详情页提取失败不算同步失败——退回入口域名，并保留备注说明。
//! - 「已关停 / 避雷」映射成 `is_runaway`，与 ldoh 的跑路标记同一语义。
//!
//! ## 礼貌与边界
//!
//! - 只在用户显式点「同步公共库」并勾选该来源时抓取，不做后台轮询。
//! - 详情页并发 4 路、单个 12 秒超时、整轮有总预算；失败即降级，不重试轰炸。
//! - 同步只做「新增 + 按地址更新」，**不删除**本地已有站点。

use crate::context::EventBus;
use crate::db::build_site_http_client;
use crate::models::*;
use crate::site::library::*;
use std::collections::HashSet;
use std::time::Duration;

/// 来源标识，与 `remote_sync` 里的 `SOURCE_BAIPIAO` 同值。
pub(crate) const BAIPIAO_SOURCE_ID: &str = "baipiao";
/// 列表页地址。
pub(crate) const BAIPIAO_LIST_URL: &str = "https://baipiao.org/charity/";
pub(crate) const BAIPIAO_ORIGIN: &str = "https://baipiao.org";
/// 详情页并发上限：对方是静态站，但仍按「不打扰」的口径压到 4 路。
const BAIPIAO_DETAIL_CONCURRENCY: usize = 4;
/// 单次请求超时。
const BAIPIAO_REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
/// 详情页整轮总预算：超时后未抓到的站点按入口域名降级，不让同步无限拖长。
const BAIPIAO_DETAIL_BUDGET: Duration = Duration::from_secs(90);
/// 单页响应体读取上限，防御异常大体。
const BAIPIAO_BODY_LIMIT: usize = 2 * 1024 * 1024;

/// 列表卡片解析出的原始站点信息。
#[derive(Debug, Clone, Default)]
pub(crate) struct BaipiaoCard {
    /// 详情页 slug（`data-slug` 去掉 `gy:` 前缀），同时用作稳定站点 id 的一部分。
    pub(crate) slug: String,
    pub(crate) name: String,
    pub(crate) summary: String,
    pub(crate) tags: Vec<String>,
    pub(crate) status: String,
    /// 卡片里第一个非 baipiao 域的「入口 / 注册」链接。
    pub(crate) entry_url: String,
}

impl BaipiaoCard {
    /// 该卡片是否等同于「已关停 / 避雷」。
    pub(crate) fn is_expired(&self) -> bool {
        self.status.eq_ignore_ascii_case("expired")
    }
}

/// 拆分 HTML 里的所有 `data-card` 卡片块。
///
/// 卡片以 `<div class="ccard" data-card` 开头，到下一个同级卡片（或结尾）为止。
/// 不做完整 DOM 解析：页面结构由 Astro 固定输出，按块切分足够稳，也避免引入
/// 重型 HTML 解析依赖。
pub(crate) fn split_baipiao_cards(html: &str) -> Vec<String> {
    let mut cards = Vec::new();
    let rest = html;
    // 先定位所有卡片起点，再两两切片，天然处理「最后一张卡片没有后继」。
    let mut starts: Vec<usize> = Vec::new();
    let mut offset = 0usize;
    while let Some(found) = rest[offset..].find("class=\"ccard\" data-card") {
        let at = offset + found;
        // 回退到该属性所在的 `<div` 起点，保证切片完整包含开标签。
        let block_start = rest[..at].rfind('<').unwrap_or(at);
        starts.push(block_start);
        offset = at + 1;
        if offset >= rest.len() {
            break;
        }
    }
    for (index, start) in starts.iter().copied().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(rest.len());
        cards.push(rest[start..end].to_string());
    }
    cards
}

/// 从一张卡片的 HTML 块里解析站点信息；缺名称或缺标识的卡片返回 `None`。
pub(crate) fn parse_baipiao_card(block: &str) -> Option<BaipiaoCard> {
    let open_tag = card_open_tag(block)?;
    let name = html_attribute(&open_tag, "data-name")
        .map(|value| decode_basic_html_entities(&value))
        .unwrap_or_default();
    if name.trim().is_empty() {
        return None;
    }
    let slug = html_attribute(&open_tag, "data-slug")
        .unwrap_or_default()
        .trim()
        .trim_start_matches("gy:")
        .to_string();
    let summary = html_attribute(&open_tag, "data-summary")
        .map(|value| decode_basic_html_entities(&value))
        .unwrap_or_default();
    let tags = html_attribute(&open_tag, "data-tags")
        .map(|value| {
            decode_basic_html_entities(&value)
                .split(',')
                .map(|tag| tag.trim().to_string())
                .filter(|tag| !tag.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let status = html_attribute(&open_tag, "data-status").unwrap_or_default();
    Some(BaipiaoCard {
        slug,
        name: name.trim().to_string(),
        summary: summary.trim().to_string(),
        tags,
        status,
        entry_url: first_external_entry(block),
    })
}

/// 取出卡片开标签（`<div ...>`），属性都在它上面。
fn card_open_tag(block: &str) -> Option<String> {
    let start = block.find("<div")?;
    let end = block[start..].find('>')? + start;
    Some(block[start..=end].to_string())
}

/// 卡片里第一个指向外部站点的入口链接，`/go.html?u=` 跳转会先解码。
fn first_external_entry(block: &str) -> String {
    let mut offset = 0usize;
    while let Some(found) = block[offset..].find("href=") {
        let at = offset + found;
        let tag_start = block[..at].rfind('<').unwrap_or(0);
        let Some(tag_end) = block[at..].find('>') else {
            break;
        };
        let tag = &block[tag_start..at + tag_end];
        offset = at + 1;
        let Some(href) = html_attribute(tag, "href") else {
            continue;
        };
        let Some(url) = resolve_baipiao_link(&href) else {
            continue;
        };
        return url;
    }
    String::new()
}

/// 把卡片里的链接还原成真实外部地址；`baipiao.org` 自身与 `javascript:` 等一律丢弃。
pub(crate) fn resolve_baipiao_link(href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() || href.starts_with("javascript:") || href.starts_with('#') {
        return None;
    }
    let inner = if let Some(rest) = href.strip_prefix("/go.html?u=") {
        percent_decode(rest)
    } else if let Some(rest) = href
        .strip_prefix("https://baipiao.org/go.html?u=")
        .or_else(|| href.strip_prefix("http://baipiao.org/go.html?u="))
    {
        percent_decode(rest)
    } else {
        href.to_string()
    };
    let Ok(parsed) = url::Url::parse(inner.trim()) else {
        return None;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
    if host.is_empty() || host == "baipiao.org" || host.ends_with(".baipiao.org") {
        return None;
    }
    Some(parsed.to_string())
}

/// `%XX` 解码；只处理跳转参数需要的场景，非法序列原样保留。
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = &value[index + 1..index + 3];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 解析整个列表页，返回所有可用卡片（保持页面顺序）。
pub(crate) fn parse_baipiao_list(html: &str) -> Vec<BaipiaoCard> {
    split_baipiao_cards(html)
        .iter()
        .filter_map(|block| parse_baipiao_card(block))
        .collect()
}

/// 从详情页正文里提取 API 基址。
///
/// 命中优先级：
/// 1. 显式 `base_url … https://xxx/v1` 之类的说明（最准）；
/// 2. 代码块里的 `https://xxx/v1/chat/completions`、`/v1/models` 等绝对地址；
/// 3. 正文里第一个带 `/v1`、`/anthropic` 等接口段的绝对地址。
///
/// 任何命中都收敛回「协议 + 主机 + 非接口前缀路径」，即站点库用的基址形态。
pub(crate) fn extract_api_base_url(html: &str, fallback_host: &str) -> Option<String> {
    let text = strip_html_to_text(html);
    let mut candidates: Vec<(usize, String)> = Vec::new();

    for (index, raw) in absolute_urls(&text) {
        let Some(base) = normalize_api_base(&raw) else {
            continue;
        };
        if is_baipiao_own_host(&base) {
            continue;
        }
        candidates.push((index, base));
    }

    // 优先取正文里最靠前、且确实带接口段的那条。
    if let Some((_, base)) = candidates.first() {
        return Some(base.clone());
    }

    // 详情页没有可用的接口地址，但正文提到过入口主机时，退回该主机。
    let host = fallback_host.trim();
    if host.is_empty() {
        return None;
    }
    normalize_api_base(&format!("https://{host}/"))
}

/// 页面正文里出现的所有绝对 URL，附带出现位置。
fn absolute_urls(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut offset = 0usize;
    while let Some(at) = text[offset..].find("http") {
        let start = offset + at;
        let rest = &text[start..];
        if !(rest.starts_with("http://") || rest.starts_with("https://")) {
            offset = start + 4;
            continue;
        }
        let end = rest
            .find(|ch: char| {
                ch.is_whitespace()
                    || matches!(ch, '"' | '\'' | '<' | '>' | '`' | ')' | ']' | '}' | ',' | ';')
            })
            .unwrap_or(rest.len());
        let candidate = rest[..end].trim_end_matches(['.', '。', '、']).to_string();
        if candidate.len() > "https://".len() {
            found.push((start, candidate));
        }
        offset = start + end.max(4);
        if offset >= text.len() {
            break;
        }
    }
    found
}

/// 把一个可能带接口段的地址收敛成站点基址。
fn normalize_api_base(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let parsed = url::Url::parse(raw).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str()?;
    if host.is_empty() {
        return None;
    }
    // 先取出路径字符串的所有权：`parsed` 后面还要被改写成基址，不能同时持有借用。
    let path = parsed.path().to_string();
    let mut segments: Vec<&str> = path.split('/').filter(|segment| !segment.is_empty()).collect();
    // 截断到接口段之前：`/v1/chat/completions` → `/`，`/api/v1/models` → `/api`。
    if let Some(cut) = segments.iter().position(|segment| {
        matches!(
            segment.to_ascii_lowercase().as_str(),
            "v1" | "v2" | "v3" | "v1beta" | "anthropic" | "openai" | "models" | "chat"
                | "completions" | "messages" | "embeddings"
        )
    }) {
        segments.truncate(cut);
    }
    let mut url = parsed;
    let path = if segments.is_empty() {
        "/".to_string()
    } else {
        format!("/{}/", segments.join("/"))
    };
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    url.set_username("").ok()?;
    url.set_password(None).ok()?;
    Some(url.to_string())
}

fn is_baipiao_own_host(url: &str) -> bool {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| host == "baipiao.org" || host.ends_with(".baipiao.org"))
}

/// 粗暴去标签：丢弃 `<script>` / `<style>` / `<svg>`，其余标签替换为空白。
///
/// 详情页正文里的代码块与说明文字是提取目标，标签去掉后按空白切分反而更容易
/// 定位绝对地址；这里不追求排版还原。
pub(crate) fn strip_html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let lower = html.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut index = 0usize;
    while index < html.len() {
        let Some(open) = lower[index..].find('<') else {
            out.push_str(&html[index..]);
            break;
        };
        let at = index + open;
        out.push_str(&html[index..at]);

        // 标签名必须“紧跟在 < 之后”（允许一个前导 `/`），不能跨过空白往后找：
        // 属性值里常出现 `<` 之外的任意字符，跨空白匹配会把正文位置读错。
        let name_start = if bytes.get(at + 1) == Some(&b'/') {
            at + 2
        } else {
            at + 1
        };
        let name_len = bytes[name_start..]
            .iter()
            .take_while(|byte| byte.is_ascii_alphanumeric())
            .count();
        let tag_name = &lower[name_start..name_start + name_len];
        let closing = name_start > at + 1;

        // raw-text 元素整段丢弃：必须直接跳到 `</name`，不能先按 `>` 找标签结尾——
        // 脚本/样式正文里带 `>` 的代码片段极多，逐个当标签解析会把后续正文一起吃掉。
        if !closing && matches!(tag_name, "script" | "style" | "svg" | "noscript") {
            let close = format!("</{tag_name}");
            index = match lower[at..].find(&close) {
                Some(end) => at + end,
                None => html.len(),
            };
            out.push(' ');
            continue;
        }

        let Some(close) = lower[at..].find('>') else {
            break;
        };
        index = at + close + 1;
        out.push(' ');
    }
    out
}

/// 拉一次列表页并解析。
pub(crate) async fn fetch_baipiao_cards(
    client: &wreq::Client,
) -> Result<Vec<BaipiaoCard>, String> {
    let response = client
        .get(BAIPIAO_LIST_URL)
        .header(wreq::header::ACCEPT, "text/html,application/xhtml+xml")
        .header(wreq::header::USER_AGENT, "OpenHub-Desktop/0.3")
        .timeout(BAIPIAO_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|error| format!("无法连接白嫖公益站目录：{error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "白嫖公益站目录请求失败（HTTP {}）",
            response.status().as_u16()
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("读取白嫖公益站目录失败：{error}"))?;
    let body = String::from_utf8_lossy(&bytes[..bytes.len().min(BAIPIAO_BODY_LIMIT)]).into_owned();
    let cards = parse_baipiao_list(&body);
    if cards.is_empty() {
        return Err("白嫖公益站目录解析失败：未找到任何站点卡片（页面结构可能已调整）".into());
    }
    Ok(cards)
}

/// 详情页地址。
pub(crate) fn baipiao_detail_url(slug: &str) -> String {
    format!("{BAIPIAO_ORIGIN}/charity/{slug}/")
}

/// 抓单个详情页并提取接口基址。
pub(crate) async fn fetch_baipiao_api_base(
    client: &wreq::Client,
    slug: &str,
    fallback_host: &str,
) -> Option<String> {
    if slug.trim().is_empty() {
        return None;
    }
    let response = client
        .get(baipiao_detail_url(slug))
        .header(wreq::header::ACCEPT, "text/html,application/xhtml+xml")
        .header(wreq::header::USER_AGENT, "OpenHub-Desktop/0.3")
        .timeout(BAIPIAO_REQUEST_TIMEOUT)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let bytes = response.bytes().await.ok()?;
    let body = String::from_utf8_lossy(&bytes[..bytes.len().min(BAIPIAO_BODY_LIMIT)]).into_owned();
    extract_api_base_url(&body, fallback_host)
}

/// 卡片 → 站点记录（尚未补详情页基址）。
pub(crate) fn card_to_site(card: &BaipiaoCard, entry_host: &str) -> SiteRecord {
    let mut site = SiteRecord::default();
    // id 前缀带来源，避免与 ldoh 的 UUID 混用；slug 稳定，重复同步不会新增。
    site.id = format!("baipiao-{}", if card.slug.is_empty() { slugify(&card.name) } else { card.slug.clone() });
    site.name = card.name.clone();
    site.description = card.summary.clone();
    site.api_base_url = if card.entry_url.is_empty() {
        String::new()
    } else {
        card.entry_url.clone()
    };
    site.tags = card.tags.clone();
    site.is_runaway = card.is_expired();
    if !card.entry_url.is_empty() {
        site.extension_links.push(ExtensionLink {
            label: "白嫖目录".into(),
            url: format!("{BAIPIAO_ORIGIN}/charity/{}/", card.slug),
        });
    }
    if site.api_base_url.is_empty() && !entry_host.is_empty() {
        site.api_base_url = format!("https://{entry_host}/");
    }
    site
}

/// 极简 slug 化：非字母数字折叠成 `-`，用于缺 `data-slug` 的卡片。
pub(crate) fn slugify(value: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.extend(ch.to_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "site".to_string()
    } else {
        out
    }
}

/// 站点是否已存在（按来源 id 或地址键）。
fn existing_site_keys(database: &Database) -> Result<(HashSet<String>, HashSet<String>), String> {
    let connection = database.lock_conn()?;
    let mut statement = connection
        .prepare("SELECT id, api_base_url FROM directory_sites")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| error.to_string())?;
    let mut ids = HashSet::new();
    let mut keys = HashSet::new();
    for row in rows {
        let (id, url) = row.map_err(|error| error.to_string())?;
        ids.insert(id);
        let key = site_address_key(&url);
        if !key.is_empty() {
            keys.insert(key);
        }
    }
    Ok((ids, keys))
}

/// 同步「白嫖」公益站目录到站点库。
///
/// 返回与 ldoh 同步同形的 [`SyncSitesResult`]，便于前端复用同一套日志与提示。
/// 该来源不需要登录，因此 `profileName` / `userName` 留空，由前端按来源标注。
pub(crate) async fn sync_baipiao_sites(
    database: &Database,
    bus: &EventBus,
    run_id: u64,
) -> Result<SyncSitesResult, String> {
    emit_sync_progress(
        bus,
        run_id,
        "baipiao-list",
        "running",
        "正在读取白嫖公益站目录".into(),
    );
    let client = build_site_http_client(database, BAIPIAO_REQUEST_TIMEOUT, 5, "白嫖目录请求")?;
    let cards = fetch_baipiao_cards(&client).await?;
    let active = cards.iter().filter(|card| !card.is_expired()).count();
    emit_sync_progress(
        bus,
        run_id,
        "baipiao-list",
        "success",
        format!(
            "已解析 {} 个公益站（在营 {} 个，已关停 {} 个）",
            cards.len(),
            active,
            cards.len() - active
        ),
    );

    // 只有「本地还没有」的站点才值得抓详情页补 API 基址：已有站点沿用库里
    // 的地址（可能已被用户校正过），每次全量重抓会白打对方几十次请求。
    let (existing_ids, existing_keys) = existing_site_keys(database)?;
    let mut sites: Vec<SiteRecord> = Vec::with_capacity(cards.len());
    let mut pending_detail: Vec<usize> = Vec::new();
    for card in &cards {
        let entry_host = url::Url::parse(&card.entry_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string))
            .unwrap_or_default();
        let site = card_to_site(card, &entry_host);
        let known = existing_ids.contains(&site.id)
            || (!site.api_base_url.is_empty()
                && existing_keys.contains(&site_address_key(&site.api_base_url)));
        if !known {
            pending_detail.push(sites.len());
        }
        sites.push(site);
    }

    let detail_total = pending_detail.len();
    if detail_total > 0 {
        emit_sync_progress(
            bus,
            run_id,
            "baipiao-detail",
            "running",
            format!("正在补齐 {detail_total} 个新增站点的 API 基址"),
        );
        use futures_util::StreamExt;
        let resolved = tokio::time::timeout(
            BAIPIAO_DETAIL_BUDGET,
            futures_util::stream::iter(pending_detail.iter().copied().map(|index| {
                let client = client.clone();
                let slug = cards[index].slug.clone();
                let fallback = url::Url::parse(&cards[index].entry_url)
                    .ok()
                    .and_then(|url| url.host_str().map(str::to_string))
                    .unwrap_or_default();
                async move {
                    (
                        index,
                        fetch_baipiao_api_base(&client, &slug, &fallback).await,
                    )
                }
            }))
            .buffer_unordered(BAIPIAO_DETAIL_CONCURRENCY)
            .collect::<Vec<(usize, Option<String>)>>(),
        )
        .await;

        let mut filled = 0usize;
        match resolved {
            Ok(results) => {
                for (index, api_base) in results {
                    if let Some(api_base) = api_base {
                        sites[index].api_base_url = api_base;
                        filled += 1;
                    }
                }
                emit_sync_progress(
                    bus,
                    run_id,
                    "baipiao-detail",
                    "success",
                    format!(
                        "已补齐 {filled}/{detail_total} 个站点的 API 基址{}",
                        if filled < detail_total {
                            "（其余退回入口域名）"
                        } else {
                            ""
                        }
                    ),
                );
            }
            Err(_) => {
                emit_sync_progress(
                    bus,
                    run_id,
                    "baipiao-detail",
                    "error",
                    format!(
                        "补齐 API 基址超时：已处理的先落库，其余 {detail_total} 个站点退回入口域名"
                    ),
                );
            }
        }
    }

    // 地址为空的卡片无法入库（站点库要求非空 API 地址），直接跳过并计数。
    let mut skipped = 0usize;
    sites.retain(|site| {
        if site.api_base_url.trim().is_empty() {
            skipped += 1;
            false
        } else {
            true
        }
    });
    if skipped > 0 {
        emit_sync_progress(
            bus,
            run_id,
            "baipiao-filter",
            "info",
            format!("{skipped} 个站点缺少可用地址，已跳过"),
        );
    }

    emit_sync_progress(
        bus,
        run_id,
        "baipiao-save",
        "running",
        "正在写入本地数据库并保留本地类型与在用状态".into(),
    );
    let outcome = merge_remote_sites(database, sites)?;
    emit_sync_progress(
        bus,
        run_id,
        "baipiao-save",
        "success",
        format!(
            "白嫖目录写入完成：新增 {}，更新 {}",
            outcome.added, outcome.updated
        ),
    );

    Ok(SyncSitesResult {
        added: outcome.added,
        updated: outcome.updated,
        total: outcome.added + outcome.updated,
        profile_name: String::new(),
        account_name: String::new(),
        user_name: String::new(),
        runaway: false,
        site_ids: outcome.added_ids,
        sources: vec![BAIPIAO_SOURCE_ID.to_string()],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST_FIXTURE: &str = r#"
<div class="ccard" data-card data-slug="gy:chatanywhere" data-status="active" data-entry-id="chatanywhere" data-name="ChatAnywhere" data-summary="免费 GPT &amp; Claude 接口。" data-tags="免费API,GPT,Claude">
<div class="cchannels"><a class="centry" href="https://baipiao.org/go.html?u=https%3A%2F%2Fapi.chatanywhere.tech%2F" target="_blank" rel="noopener nofollow"><span class="ck">站点</span>注册入口</a></div></div>
<div class="ccard" data-card data-slug="gy:token-rhythm" data-status="expired" data-entry-id="token-rhythm" data-name="基元律动 Token Rhythm" data-summary="长期维护的公益网关。" data-tags="免费API,OpenAI兼容">
<div class="cchannels"><a class="centry" href="https://baipiao.org/go.html?u=https%3A%2F%2Ftokenrhythm.studio%2F">站点</a></div></div>
<div class="ccard" data-card data-slug="gy:no-name" data-status="active"></div>
"#;

    #[test]
    fn parses_cards_from_list_html() {
        let cards = parse_baipiao_list(LIST_FIXTURE);
        assert_eq!(cards.len(), 2, "缺名称的卡片应被丢弃");
        assert_eq!(cards[0].name, "ChatAnywhere");
        assert_eq!(cards[0].slug, "chatanywhere");
        assert_eq!(cards[0].summary, "免费 GPT & Claude 接口。");
        assert_eq!(cards[0].tags, vec!["免费API", "GPT", "Claude"]);
        assert!(!cards[0].is_expired());
        assert_eq!(cards[0].entry_url, "https://api.chatanywhere.tech/");
        assert!(cards[1].is_expired());
    }

    #[test]
    fn decodes_go_redirect_and_rejects_internal_links() {
        assert_eq!(
            resolve_baipiao_link("/go.html?u=https%3A%2F%2Fapi.demo.top%2Fv1").as_deref(),
            Some("https://api.demo.top/v1")
        );
        assert!(resolve_baipiao_link("https://baipiao.org/charity/x/").is_none());
        assert!(resolve_baipiao_link("javascript:void(0)").is_none());
    }

    #[test]
    fn extracts_api_base_from_detail_page() {
        let html = r#"<p>OpenAI 兼容：base_url 填 <code>https://fufu.iqach.top/v1</code></p>
      <pre><code>curl https://fufu.iqach.top/v1/chat/completions -H "Authorization: Bearer x"</code></pre>
      <script>var a = "https://cdn.example.com/x.js";</script>"#;
        assert_eq!(
            extract_api_base_url(html, "fufu.iqach.top").as_deref(),
            Some("https://fufu.iqach.top/")
        );
    }

    #[test]
    fn falls_back_to_entry_host_when_detail_has_no_endpoint() {
        let html = "<p>暂无接口说明</p>";
        assert_eq!(
            extract_api_base_url(html, "demo.example").as_deref(),
            Some("https://demo.example/")
        );
        assert!(extract_api_base_url(html, "").is_none());
    }

    #[test]
    fn keeps_path_prefix_before_api_segment() {
        let html = "<code>https://modeloc.com/pool/v1</code>";
        assert_eq!(
            extract_api_base_url(html, "").as_deref(),
            Some("https://modeloc.com/pool/")
        );
    }

    #[test]
    fn strip_keeps_body_after_scripts_and_styles() {
        // 真实详情页的结构特征：head 里有多个内联脚本与样式，样式与脚本正文
        // 都含 `>` 字符；正文里的接口地址必须在剥离后仍然可见。
        let html = r#"<html><head><style>.a{display:grid}.b>c{margin:0}</style>
<script>const x = a > b ? "https://cdn.ex.com/a.js" : 0;</script>
<style media="(max-width:600px)">{content: ">";}</style></head>
<body><p>base_url 填 <code>https://demo.example/v1</code></p></body></html>"#;
        let text = strip_html_to_text(html);
        assert!(text.contains("https://demo.example/v1"), "剥离后的正文被吞掉了：{text}");
        assert!(!text.contains("cdn.ex.com"), "脚本内容应被丢弃");
    }

    #[test]
    fn site_address_key_folds_api_prefix() {
        assert_eq!(
            site_address_key("https://api.demo.top/v1/"),
            site_address_key("http://www.api.demo.top/")
        );
        assert_ne!(
            site_address_key("https://api.demo.top/"),
            site_address_key("https://api.other.top/")
        );
    }
}
