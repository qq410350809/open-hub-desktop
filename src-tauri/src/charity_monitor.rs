use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio_util::sync::CancellationToken;

use crate::chrome_session;
use crate::models::*;
use crate::proxy_pool::{self, ProxyRuntime};

pub(crate) const DEFAULT_CHARITY_FEED_ID: &str = "1515";

#[derive(Debug, Clone, Copy)]
struct CharityFeedSource {
    id: &'static str,
    name: &'static str,
    json_url: &'static str,
}

const CHARITY_FEEDS: &[CharityFeedSource] = &[
    CharityFeedSource {
        id: "1515",
        name: "公益推广",
        json_url: "https://linux.do/tag/1515-tag/1515.json?order=created&ascending=false",
    },
    CharityFeedSource {
        id: "1980",
        name: "公益站",
        json_url: "https://linux.do/tag/1980-tag/1980.json?order=created&ascending=false",
    },
    CharityFeedSource {
        id: "2233",
        name: "中转站",
        json_url: "https://linux.do/tag/2233-tag/2233.json?order=created&ascending=false",
    },
    CharityFeedSource {
        id: "2234",
        name: "开源推广",
        json_url: "https://linux.do/tag/2234-tag/2234.json?order=created&ascending=false",
    },
    CharityFeedSource {
        id: "1514",
        name: "高级推广",
        json_url: "https://linux.do/tag/1514-tag/1514.json?order=created&ascending=false",
    },
    CharityFeedSource {
        id: "193",
        name: "订阅节点",
        json_url: "https://linux.do/tag/193-tag/193.json?order=created&ascending=false",
    },
];

fn charity_feed_source(feed_id: &str) -> Result<CharityFeedSource, String> {
    let feed_id = feed_id.trim();
    CHARITY_FEEDS
        .iter()
        .copied()
        .find(|source| source.id == feed_id)
        .ok_or_else(|| format!("不支持的 Linux.do 标签：{feed_id}"))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CharityFeedItem {
    id: String,
    title: String,
    link: String,
    author: String,
    published_at: String,
    summary: String,
    categories: Vec<String>,
    is_new: bool,
    #[serde(default)]
    reply_count: i64,
    #[serde(default)]
    views: i64,
    #[serde(default)]
    like_count: i64,
    #[serde(default)]
    last_activity_at: String,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    posters: Vec<String>,
    /// 该帖子归属的标签 id（同一 guid 可能命中多个标签）
    #[serde(default)]
    feed_ids: Vec<String>,
    /// 该帖子归属的标签名（供列表展示）
    #[serde(default)]
    feed_names: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CharityFeedResult {
    feed_id: String,
    feed_name: String,
    items: Vec<CharityFeedItem>,
    fetched_at: String,
    changed: bool,
    new_count: usize,
    updated_count: usize,
    initialized: bool,
    source_profile_name: String,
    source_account_name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    used_node_id: String,
    #[serde(default)]
    used_node_name: String,
    #[serde(default)]
    unread_count: usize,
    #[serde(default)]
    skipped: bool,
    #[serde(default)]
    total_count: usize,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: usize,
    #[serde(default)]
    has_more: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CharitySyncProgress {
    feed_id: String,
    feed_name: String,
    stage: String,
    status: String,
    message: String,
    used_node_id: String,
    used_node_name: String,
    new_count: usize,
    updated_count: usize,
    unread_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CharitySyncLogEntry {
    id: i64,
    at: String,
    feed_id: String,
    feed_name: String,
    stage: String,
    status: String,
    message: String,
    node_name: String,
    duration_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CharityRefreshAllResult {
    cancelled_active_round: bool,
    cancelled_log_count: usize,
    feed_count: usize,
}

/// 公益同步只使用延迟 ≤500ms 的测速成功节点作为候选。
const CHARITY_FAST_NODE_MAX_LATENCY_MS: i64 = 500;
/// 单次请求（一个节点）的最大耗时；失败/超时后换下一节点。
const CHARITY_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// 每个标签每轮最多尝试两个节点。公益监听每 5 分钟触发一次，必须给整轮设置
/// 明确上限，不能让单个标签扫完整个代理池并饿死其它标签。
const CHARITY_MAX_NODE_ATTEMPTS: usize = 2;
/// 装入 Mihomo 的候选上限，避免一次装载几百节点导致极慢。
const CHARITY_PREPARE_NODE_LIMIT: usize = 40;
/// 超时/网络失败：仅移出公益候选，短时拉黑（不删代理池）。
const CHARITY_BAN_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// 403/站点拒绝：拉黑更久。
const CHARITY_BAN_FORBIDDEN: Duration = Duration::from_secs(2 * 60 * 60);
/// 传输层（error sending request / 连接不上节点）与 403 同级拉黑，
/// 避免半小时后又把明显连不通的节点捞回来反复浪费。
const CHARITY_BAN_UNREACHABLE: Duration = Duration::from_secs(2 * 60 * 60);
/// 其它失败默认拉黑。
const CHARITY_BAN_DEFAULT: Duration = Duration::from_secs(15 * 60);
const CHARITY_PAGE_SIZE: usize = 20;
/// 列表一次可取的最大条数（渐进渲染用全量，不再后端分页）。
const CHARITY_PAGE_LIMIT_MAX: usize = 2000;
/// 定时触发：每 5 分钟一次，对齐到本地时区「分钟为 5 的整数倍、秒为 00」
/// 例如 12:00:00 / 12:05:00 / 12:10:00 …
const CHARITY_SCHEDULE_EVERY_MINUTES: u32 = 5;
/// 调度循环最小检查间隔，避免空转。
const CHARITY_SCHEDULER_TICK: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
struct CharityNodeRef {
    id: String,
    name: String,
    latency_ms: i64,
}

/// 单个标签的公益候选队列：队头取用；失败/超时不回队。
#[derive(Debug, Default)]
struct CharityNodeQueue {
    nodes: VecDeque<CharityNodeRef>,
}

impl CharityNodeQueue {
    fn from_nodes(nodes: Vec<CharityNodeRef>) -> Self {
        Self {
            nodes: VecDeque::from(nodes),
        }
    }

    fn pop_front(&mut self) -> Option<CharityNodeRef> {
        self.nodes.pop_front()
    }

    fn push_back(&mut self, node: CharityNodeRef) {
        // 已拉黑节点不允许再入队
        self.nodes.push_back(node);
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// 从本轮公益队列中剔除指定节点（可清掉残留重复项）。
    fn remove_id(&mut self, node_id: &str) -> usize {
        let before = self.nodes.len();
        self.nodes.retain(|node| node.id != node_id);
        before.saturating_sub(self.nodes.len())
    }
}

pub(crate) struct CharityMonitorRuntime {
    running: AtomicBool,
    visible: AtomicBool,
    force_round: AtomicBool,
    syncing: AtomicBool,
    node_round_robin: AtomicUsize,
    // 当前整轮同步共享的取消信号；“立即刷新”会先取消旧轮，再请求全标签新轮。
    active_sync_cancellation: Mutex<Option<CancellationToken>>,
    // 全局代理出口唯一，并行标签同步时用这把锁串行化“切换节点+请求+判定”段
    proxy_sync_lock: tokio::sync::Mutex<()>,
    last_errors: Mutex<HashMap<String, String>>,
    /// 公益候选黑名单：node_id -> 解禁时间。不删除代理池节点。
    charity_ban_until: Mutex<HashMap<String, Instant>>,
}

impl CharityMonitorRuntime {
    pub(crate) fn new() -> Self {
        Self {
            running: AtomicBool::new(false),
            visible: AtomicBool::new(true),
            force_round: AtomicBool::new(false),
            syncing: AtomicBool::new(false),
            node_round_robin: AtomicUsize::new(0),
            active_sync_cancellation: Mutex::new(None),
            proxy_sync_lock: tokio::sync::Mutex::new(()),
            last_errors: Mutex::new(HashMap::new()),
            charity_ban_until: Mutex::new(HashMap::new()),
        }
    }

    fn purge_expired_bans(&self) {
        let Ok(mut bans) = self.charity_ban_until.lock() else {
            return;
        };
        let now = Instant::now();
        bans.retain(|_, until| *until > now);
    }

    fn is_banned(&self, node_id: &str) -> bool {
        let Ok(mut bans) = self.charity_ban_until.lock() else {
            return false;
        };
        let now = Instant::now();
        match bans.get(node_id) {
            Some(until) if *until > now => true,
            Some(_) => {
                bans.remove(node_id);
                false
            }
            None => false,
        }
    }

    /// 仅移出公益候选（短 TTL），绝不删除 proxy_pool_nodes。
    fn ban_node(&self, node_id: &str, ttl: Duration) {
        if node_id.trim().is_empty() {
            return;
        }
        if let Ok(mut bans) = self.charity_ban_until.lock() {
            let until = Instant::now() + ttl;
            bans.entry(node_id.to_string())
                .and_modify(|old| {
                    if until > *old {
                        *old = until;
                    }
                })
                .or_insert(until);
        }
    }

    /// 当前仍在生效的公益黑名单/冷却节点 id（403、超时、成功冷却等）。
    fn active_banned_ids(&self) -> Vec<String> {
        let mut bans = match self.charity_ban_until.lock() {
            Ok(guard) => guard,
            Err(_) => return Vec::new(),
        };
        let now = Instant::now();
        let active = bans
            .iter()
            .filter_map(|(id, until)| if *until > now { Some(id.clone()) } else { None })
            .collect::<Vec<_>>();
        bans.retain(|_, until| *until > now);
        active
    }

    pub(crate) fn set_visible(&self, visible: bool) {
        // 仅记录前台可见性（UI/诊断用）；定时触发不再依赖可见性。
        self.visible.store(visible, Ordering::Relaxed);
    }

    pub(crate) fn request_round(&self) {
        // 仅由“立即刷新”等显式按钮触发临时同步；调度循环消费 force_round。
        self.force_round.store(true, Ordering::Relaxed);
    }

    fn try_begin_sync(&self) -> Option<CancellationToken> {
        if self
            .syncing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
            .is_err()
        {
            return None;
        }
        let cancellation = CancellationToken::new();
        if let Ok(mut active) = self.active_sync_cancellation.lock() {
            *active = Some(cancellation.clone());
        }
        Some(cancellation)
    }

    fn cancel_active_sync(&self) -> bool {
        let active = self
            .active_sync_cancellation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(cancellation) = active.as_ref() else {
            return false;
        };
        cancellation.cancel();
        true
    }

    fn end_sync(&self) {
        if let Ok(mut active) = self.active_sync_cancellation.lock() {
            *active = None;
        }
        self.syncing.store(false, Ordering::SeqCst);
    }
}

fn plain_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut inside_tag = false;
    for character in html.chars() {
        match character {
            '<' => inside_tag = true,
            '>' => {
                inside_tag = false;
                text.push(' ');
            }
            _ if !inside_tag => text.push(character),
            _ => {}
        }
    }
    let decoded = quick_xml::escape::unescape(&text)
        .map(|value| value.into_owned())
        .unwrap_or(text);
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn topic_id(value: &str) -> Option<u64> {
    value
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .and_then(|value| value.parse().ok())
}

fn items_from_topic_list(value: &str) -> Result<Vec<CharityFeedItem>, String> {
    let value: serde_json::Value =
        serde_json::from_str(value).map_err(|error| format!("标签主题数据无法解析：{error}"))?;
    let topics = value
        .pointer("/topic_list/topics")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "标签主题数据缺少 topic_list.topics".to_string())?;
    let users = value
        .get("users")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|user| {
            let id = user.get("id")?.as_u64()?;
            let name = user
                .get("username")
                .or_else(|| user.get("name"))?
                .as_str()?
                .trim();
            if name.is_empty() {
                return None;
            }
            let avatar = user
                .get("avatar_template")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|template| {
                    let filled = template.replace("{size}", "48");
                    if filled.starts_with("http://") || filled.starts_with("https://") {
                        filled
                    } else if filled.starts_with("//") {
                        format!("https:{filled}")
                    } else if filled.starts_with('/') {
                        format!("https://linux.do{filled}")
                    } else {
                        format!("https://linux.do/{filled}")
                    }
                })
                .unwrap_or_default();
            Some((id, (name.to_string(), avatar)))
        })
        .collect::<HashMap<_, _>>();
    let mut items = topics
        .iter()
        .filter_map(|topic| {
            let id = topic.get("id")?.as_u64()?;
            let created_at = topic.get("created_at")?.as_str()?.trim();
            if created_at.is_empty() {
                return None;
            }
            let title = topic
                .get("title")
                .or_else(|| topic.get("fancy_title"))
                .and_then(serde_json::Value::as_str)
                .map(plain_text)
                .filter(|value| !value.is_empty())?;
            let slug = topic
                .get("slug")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("topic");
            let poster_ids = topic
                .get("posters")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|poster| poster.get("user_id")?.as_u64())
                .collect::<Vec<_>>();
            let author = poster_ids
                .first()
                .and_then(|user_id| users.get(user_id).map(|(name, _)| name.clone()))
                .unwrap_or_default();
            let posters = poster_ids
                .iter()
                .filter_map(|user_id| {
                    let (name, avatar) = users.get(user_id)?;
                    if !avatar.is_empty() {
                        Some(avatar.clone())
                    } else if !name.is_empty() {
                        Some(name.clone())
                    } else {
                        None
                    }
                })
                .take(5)
                .collect::<Vec<_>>();
            let mut categories = topic
                .get("tags")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|tag| tag.as_str().map(str::trim))
                .filter(|tag| !tag.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();
            categories.sort();
            categories.dedup();
            let posts_count = topic
                .get("posts_count")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0)
                .max(0);
            let reply_count = topic
                .get("reply_count")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_else(|| (posts_count - 1).max(0))
                .max(0);
            let views = topic
                .get("views")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0)
                .max(0);
            let like_count = topic
                .get("like_count")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0)
                .max(0);
            let last_activity_at = topic
                .get("last_posted_at")
                .or_else(|| topic.get("bumped_at"))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(created_at)
                .to_string();
            let pinned = topic
                .get("pinned")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
                || topic
                    .get("pinned_globally")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
            Some(CharityFeedItem {
                id: format!("topic-{id}"),
                title,
                link: format!("https://linux.do/t/{slug}/{id}"),
                author,
                published_at: created_at.to_string(),
                summary: topic
                    .get("excerpt")
                    .and_then(serde_json::Value::as_str)
                    .map(plain_text)
                    .unwrap_or_default(),
                categories,
                feed_ids: Vec::new(),
                feed_names: Vec::new(),
                is_new: false,
                reply_count,
                views,
                like_count,
                last_activity_at,
                pinned,
                posters,
            })
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| {
        right
            .published_at
            .cmp(&left.published_at)
            .then_with(|| topic_id(&right.link).cmp(&topic_id(&left.link)))
    });
    items.truncate(40);
    if items.is_empty() {
        Err("标签主题列表中没有找到有效帖子".into())
    } else {
        Ok(items)
    }
}

async fn request_topic_list(
    client: &reqwest::Client,
    source: CharityFeedSource,
    cookie_header: Option<&str>,
) -> Result<(String, String), String> {
    // 追加当前时间戳 t，穿透 CDN / 服务端缓存，避免拿到旧数据。
    let request_url = {
        let mut url = url::Url::parse(source.json_url)
            .map_err(|error| format!("标签地址解析失败：{error}"))?;
        url.query_pairs_mut().append_pair(
            "t",
            &std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis().to_string())
                .unwrap_or_default(),
        );
        url
    };
    let mut request = client
        .get(request_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
        .header(
            reqwest::header::USER_AGENT,
            chrome_session::chrome_user_agent(),
        )
        // 模拟真实浏览器的 fetch 上下文，降低被 Cloudflare 判为脚本的风险。
        .header("Sec-Fetch-Dest", "empty")
        .header("Sec-Fetch-Mode", "cors")
        .header("Sec-Fetch-Site", "same-origin")
        .header(
            "Referer",
            format!("https://linux.do/tag/{}-tag/{}", source.id, source.id),
        );
    if let Some(cookie_header) = cookie_header.filter(|value| !value.trim().is_empty()) {
        request = request.header(reqwest::header::COOKIE, cookie_header);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("{}标签请求失败：{error}", source.name))?;
    let protocol = format!("{:?}", response.version());
    if !response.status().is_success() {
        return Err(format!(
            "{}标签请求失败（HTTP {}，{protocol}）",
            source.name,
            response.status().as_u16()
        ));
    }
    let body = response
        .text()
        .await
        .map_err(|error| format!("{}标签内容读取失败：{error}", source.name))?;
    if !body.trim_start().starts_with('{') {
        return Err(format!(
            "{}标签返回了非 JSON 内容，可能需要完成 Linux.do 安全验证",
            source.name
        ));
    }
    Ok((body, protocol))
}

async fn fetch_topic_body(
    app: &tauri::AppHandle,
    client: reqwest::Client,
    source: CharityFeedSource,
) -> Result<(String, String, String, String), String> {
    // 静默方案：纯 HTTP 请求（走代理出口），不打开/控制浏览器。
    // 403 通常是节点出口 IP 被 Cloudflare 临时风控，换节点即可解决；
    // 带登录 Cookie 可进一步降低被质询概率。
    if let Ok((body, protocol)) = request_topic_list(&client, source, None).await {
        return Ok((body, String::new(), String::new(), protocol));
    }

    let home_dir = app
        .path()
        .home_dir()
        .map_err(|error| format!("无法定位用户目录：{error}"))?;

    // 用各 Chrome Profile 的登录 Cookie 走代理重试（不同账号 cookie 更“干净”，
    // 且已登录会话通常不会触发 Cloudflare 登录墙）。
    let profiles = tauri::async_runtime::spawn_blocking({
        let home_dir = home_dir.clone();
        move || chrome_session::profile_identities_from_home(&home_dir)
    })
    .await
    .map_err(|error| format!("读取 Chrome Profile 任务失败：{error}"))??;

    let jobs = profiles
        .into_iter()
        .map(|profile| {
            let client = client.clone();
            let home_dir = home_dir.clone();
            tauri::async_runtime::spawn(async move {
                let profile_id = profile.id.clone();
                let cookie_header = tauri::async_runtime::spawn_blocking(move || {
                    chrome_session::read_chrome_cookie_header_from_home(
                        &home_dir,
                        source.json_url,
                        &profile_id,
                    )
                })
                .await
                .map_err(|error| format!("读取 Chrome Cookie 任务失败：{error}"))??;
                let (body, protocol) =
                    request_topic_list(&client, source, Some(&cookie_header)).await?;
                Ok::<_, String>((body, profile.name, profile.account_name, protocol))
            })
        })
        .collect::<Vec<_>>();
    let mut errors = Vec::new();
    for job in jobs {
        match job.await {
            Ok(Ok(result)) => return Ok(result),
            Ok(Err(error)) => errors.push(error),
            Err(error) => errors.push(format!("Chrome 标签请求任务失败：{error}")),
        }
    }
    Err(errors
        .last()
        .cloned()
        .unwrap_or_else(|| format!("无法读取 Linux.do {}标签", source.name)))
}

fn persist_feed(
    database: &Database,
    source: CharityFeedSource,
    mut items: Vec<CharityFeedItem>,
    source_profile_name: String,
    source_account_name: String,
) -> Result<CharityFeedResult, String> {
    let keys = feed_meta_keys(source.id);
    let initialized_key = keys.initialized.clone();
    let source_key = keys.source_url.clone();
    let fetched_key = keys.fetched_at.clone();
    let mut connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let initialized = connection
        .query_row(
            "SELECT value FROM app_meta WHERE key = ?1",
            [&initialized_key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| error.to_string())?
        .is_some();
    let existing = {
        let mut statement = connection
            .prepare(
                "SELECT guid, title, link, published_at
                 FROM charity_feed_items WHERE feed_id = ?1",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([source.id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    (
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ),
                ))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<HashMap<_, _>, _>>()
            .map_err(|error| error.to_string())?;
        rows
    };
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    let mut new_count = 0;
    let mut updated_count = 0;
    for item in &mut items {
        if let Some((title, link, published_at)) = existing.get(&item.id) {
            if title != &item.title || link != &item.link || published_at != &item.published_at {
                updated_count += 1;
            }
        } else if initialized {
            item.is_new = true;
            new_count += 1;
        }
        transaction
            .execute(
                "INSERT INTO charity_feed_items
                 (feed_id, guid, title, link, author, published_at, summary, categories,
                  reply_count, views, like_count, last_activity_at, pinned, posters,
                  first_seen_at, last_seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
                 ON CONFLICT(feed_id, guid) DO UPDATE SET
                   title = excluded.title,
                   link = excluded.link,
                   author = excluded.author,
                   published_at = excluded.published_at,
                   summary = excluded.summary,
                   categories = excluded.categories,
                   reply_count = excluded.reply_count,
                   views = excluded.views,
                   like_count = excluded.like_count,
                   last_activity_at = excluded.last_activity_at,
                   pinned = excluded.pinned,
                   posters = excluded.posters,
                   last_seen_at = CURRENT_TIMESTAMP",
                params![
                    source.id,
                    item.id,
                    item.title,
                    item.link,
                    item.author,
                    item.published_at,
                    item.summary,
                    serde_json::to_string(&item.categories).map_err(|error| error.to_string())?,
                    item.reply_count,
                    item.views,
                    item.like_count,
                    item.last_activity_at,
                    item.pinned,
                    serde_json::to_string(&item.posters).map_err(|error| error.to_string())?,
                ],
            )
            .map_err(|error| error.to_string())?;
    }
    let read_key = keys.read_at.clone();
    if !initialized {
        // 首次初始化：建立已读水位，避免历史帖子全部变成未读角标。
        transaction
            .execute(
                "INSERT OR REPLACE INTO app_meta (key, value) VALUES (?1, CURRENT_TIMESTAMP)",
                params![read_key],
            )
            .map_err(|error| error.to_string())?;
    }
    transaction
        .execute(
            "INSERT OR REPLACE INTO app_meta (key, value) VALUES
             (?1, '1'), (?2, ?3), (?4, CURRENT_TIMESTAMP)",
            params![initialized_key, source_key, source.json_url, fetched_key],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "DELETE FROM charity_feed_items
             WHERE feed_id = ?1 AND guid NOT IN (
               SELECT guid FROM charity_feed_items
               WHERE feed_id = ?1
               ORDER BY last_seen_at DESC, rowid DESC LIMIT 120
             )",
            [source.id],
        )
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())?;
    let fetched_at = connection
        .query_row(
            "SELECT value FROM app_meta WHERE key = ?1",
            [&fetched_key],
            |row| row.get::<_, String>(0),
        )
        .map_err(|error| error.to_string())?;
    // 注意：connection 仍持有数据库锁，不能调用 unread_count_for_feed（会再次加锁造成自锁死锁）。
    // 直接在当前连接内计算未读数。
    let unread_count = {
        let read_at: String = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key = ?1",
                [&keys.read_at],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .unwrap_or_default();
        if read_at.trim().is_empty() {
            0usize
        } else {
            connection
                .query_row(
                    "SELECT COUNT(*) FROM charity_feed_items
                     WHERE feed_id = ?1 AND first_seen_at > ?2",
                    params![source.id, read_at],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| error.to_string())?
                .max(0) as usize
        }
    };
    Ok(CharityFeedResult {
        feed_id: source.id.into(),
        feed_name: source.name.into(),
        items,
        fetched_at,
        changed: new_count > 0 || updated_count > 0,
        new_count,
        updated_count,
        initialized,
        source_profile_name,
        source_account_name,
        status: "success".into(),
        message: String::new(),
        used_node_id: String::new(),
        used_node_name: String::new(),
        unread_count,
        skipped: false,
        total_count: 0,
        offset: 0,
        limit: CHARITY_PAGE_SIZE,
        has_more: false,
    })
}

const CHARITY_SYNC_LOG_LIMIT: usize = 300;

fn append_charity_sync_log(
    database: &Database,
    feed_id: &str,
    feed_name: &str,
    stage: &str,
    status: &str,
    message: &str,
    node_name: &str,
) -> Option<i64> {
    let Ok(connection) = database.0.lock() else {
        return None;
    };
    if connection
        .execute(
            "INSERT INTO charity_sync_logs
             (feed_id, feed_name, stage, status, message, node_name, duration_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
            params![feed_id, feed_name, stage, status, message, node_name],
        )
        .is_err()
    {
        return None;
    }
    let id = connection.last_insert_rowid();
    let _ = connection.execute(
        "DELETE FROM charity_sync_logs
         WHERE id NOT IN (
           SELECT id FROM charity_sync_logs
           ORDER BY created_at DESC, id DESC
           LIMIT ?1
         )",
        params![CHARITY_SYNC_LOG_LIMIT as i64],
    );
    Some(id)
}

fn update_charity_sync_log(
    database: &Database,
    id: i64,
    status: &str,
    message: &str,
    node_name: &str,
    duration_ms: i64,
) {
    let Ok(connection) = database.0.lock() else {
        return;
    };
    let _ = connection.execute(
        "UPDATE charity_sync_logs
         SET status = ?1, message = ?2, node_name = ?3, duration_ms = ?4
         WHERE id = ?5 AND status = 'running'",
        params![status, message, node_name, duration_ms, id],
    );
}

/// 进行中任务心跳：刷新 duration / 文案 / 节点名，供前端看到进度跳动。
fn touch_running_charity_sync_log(
    database: &Database,
    id: i64,
    message: &str,
    node_name: &str,
    duration_ms: i64,
) {
    let Ok(connection) = database.0.lock() else {
        return;
    };
    let _ = connection.execute(
        "UPDATE charity_sync_logs
         SET message = ?1, node_name = ?2, duration_ms = ?3
         WHERE id = ?4 AND status = 'running'",
        params![message, node_name, duration_ms, id],
    );
}

fn emit_running_progress(
    app: &AppHandle,
    source: CharityFeedSource,
    stage: &str,
    message: &str,
    node_name: &str,
) {
    emit_charity_progress(
        app,
        CharitySyncProgress {
            feed_id: source.id.into(),
            feed_name: source.name.into(),
            stage: stage.into(),
            status: "running".into(),
            message: message.into(),
            used_node_id: String::new(),
            used_node_name: node_name.into(),
            new_count: 0,
            updated_count: 0,
            unread_count: 0,
        },
    );
}

fn emit_charity_progress(app: &AppHandle, progress: CharitySyncProgress) {
    let _ = app.emit("charity-sync-progress", progress);
}

fn finish_charity_sync_log(
    app: &AppHandle,
    database: &Database,
    log_id: Option<i64>,
    source: CharityFeedSource,
    stage: &str,
    status: &str,
    message: &str,
    node_name: &str,
    duration_ms: i64,
    new_count: usize,
    updated_count: usize,
    unread_count: usize,
) {
    if let Some(id) = log_id {
        update_charity_sync_log(database, id, status, message, node_name, duration_ms);
    }
    emit_charity_progress(
        app,
        CharitySyncProgress {
            feed_id: source.id.into(),
            feed_name: source.name.into(),
            stage: stage.into(),
            status: status.into(),
            message: message.into(),
            used_node_id: String::new(),
            used_node_name: node_name.into(),
            new_count,
            updated_count,
            unread_count,
        },
    );
}

fn list_charity_sync_logs(
    database: &Database,
    limit: usize,
) -> Result<Vec<CharitySyncLogEntry>, String> {
    let limit = limit.clamp(1, CHARITY_SYNC_LOG_LIMIT);
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let mut statement = connection
        .prepare(
            "SELECT id, created_at, feed_id, feed_name, stage, status, message, node_name, duration_ms
             FROM charity_sync_logs
             ORDER BY created_at DESC, id DESC
             LIMIT ?1",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![limit as i64], |row| {
            Ok(CharitySyncLogEntry {
                id: row.get(0)?,
                at: row.get(1)?,
                feed_id: row.get(2)?,
                feed_name: row.get(3)?,
                stage: row.get(4)?,
                status: row.get(5)?,
                message: row.get(6)?,
                node_name: row.get(7)?,
                duration_ms: row.get(8)?,
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    Ok(rows)
}

/// 全部标签聚合视图：同一 guid 帖子按最新行合并，并列出其归属的标签。
fn load_all_feed_items_from_db(
    database: &Database,
    offset: usize,
    limit: usize,
    keyword: &str,
) -> Result<CharityFeedResult, String> {
    let limit = limit.clamp(1, CHARITY_PAGE_LIMIT_MAX);
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let key_pat = format!("%{}%", keyword.trim());
    let has_key = !keyword.trim().is_empty();
    let total_count = if has_key {
        connection
            .query_row(
                "SELECT COUNT(DISTINCT guid) FROM charity_feed_items
                 WHERE title LIKE ?1 OR author LIKE ?1 OR categories LIKE ?1",
                [&key_pat],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            .max(0) as usize
    } else {
        connection
            .query_row(
                "SELECT COUNT(DISTINCT guid) FROM charity_feed_items",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            .max(0) as usize
    };

    let mut statement = connection
        .prepare(
            "SELECT guid, title, link, author, published_at, summary, categories, first_seen_at,
                    reply_count, views, like_count, last_activity_at, pinned, posters, feed_id
             FROM charity_feed_items
             WHERE (?3 = '' OR title LIKE ?3 OR author LIKE ?3 OR categories LIKE ?3)
             ORDER BY published_at DESC, rowid DESC
             LIMIT ?1 OFFSET ?2",
        )
        .map_err(|error| error.to_string())?;
    let all = statement
        .query_map(
            params![
                (limit * 8) as i64, // 多标签可能重复占用行，放大取数后在内存聚合
                offset as i64,
                key_pat
            ],
            |row| {
                let categories: String = row.get(6)?;
                let posters_raw: String = row.get(13)?;
                Ok((
                    row.get::<_, String>(0)?, // guid
                    row.get::<_, String>(1)?, // title
                    row.get::<_, String>(2)?, // link
                    row.get::<_, String>(3)?, // author
                    row.get::<_, String>(4)?, // published_at
                    row.get::<_, String>(5)?, // summary
                    serde_json::from_str::<Vec<String>>(&categories).unwrap_or_default(),
                    row.get::<_, String>(7)?, // first_seen_at
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, i64>(12)?,
                    serde_json::from_str::<Vec<String>>(&posters_raw).unwrap_or_default(),
                    row.get::<_, String>(14)?, // feed_id
                ))
            },
        )
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    drop(statement);
    drop(connection);

    // 同一 guid 聚合归属标签
    let mut merged: Vec<CharityFeedItem> = Vec::new();
    for (
        guid,
        title,
        link,
        author,
        published_at,
        summary,
        categories,
        _first_seen_at,
        reply,
        views,
        likes,
        last_activity,
        pinned,
        posters,
        feed_id,
    ) in all
    {
        let feed_name = CHARITY_FEEDS
            .iter()
            .find(|f| f.id == feed_id)
            .map(|f| f.name.to_string())
            .unwrap_or_default();
        if let Some(item) = merged.iter_mut().find(|existing| existing.id == guid) {
            if !item.feed_ids.iter().any(|id| id == &feed_id) {
                item.feed_ids.push(feed_id);
                item.feed_names.push(feed_name);
            }
            item.views = item.views.max(views);
            item.reply_count = item.reply_count.max(reply);
            item.like_count = item.like_count.max(likes);
            continue;
        }
        merged.push(CharityFeedItem {
            id: guid,
            title,
            link,
            author,
            published_at,
            summary,
            categories,
            is_new: false,
            reply_count: reply,
            views,
            like_count: likes,
            last_activity_at: last_activity,
            pinned: pinned != 0,
            posters,
            feed_ids: vec![feed_id],
            feed_names: vec![feed_name],
        });
    }

    // 多取的行要塞回 offset 语义：由于我们 LIMIT offset*8 起查，直接从头保留即可满足“全部视图分页”
    let items = merged.into_iter().take(limit).collect::<Vec<_>>();
    let item_count = items.len();

    Ok(CharityFeedResult {
        feed_id: "all".into(),
        feed_name: "全部".into(),
        items,
        fetched_at: String::new(),
        changed: false,
        new_count: 0,
        updated_count: 0,
        initialized: true,
        source_profile_name: String::new(),
        source_account_name: String::new(),
        status: "local".into(),
        message: String::new(),
        used_node_id: String::new(),
        used_node_name: String::new(),
        unread_count: 0,
        skipped: false,
        total_count,
        offset,
        limit,
        has_more: offset + item_count < total_count,
    })
}

fn cancel_running_charity_sync_logs(database: &Database, reason: &str) -> Result<usize, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    connection
        .execute(
            "UPDATE charity_sync_logs
             SET status = 'cancelled',
                 message = ?1,
                 duration_ms = MAX(
                   duration_ms,
                   CAST((julianday('now') - julianday(created_at)) * 86400000 AS INTEGER)
                 )
             WHERE status = 'running'",
            [reason],
        )
        .map_err(|error| error.to_string())
}

fn clear_charity_sync_logs_db(database: &Database) -> Result<(), String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    connection
        .execute("DELETE FROM charity_sync_logs", [])
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// 应用重启后，把上次未完成的 running 日志标记为失败，避免“进行中”永久残留。
fn abandon_running_charity_sync_logs(database: &Database) {
    let Ok(connection) = database.0.lock() else {
        return;
    };
    let _ = connection.execute(
        "UPDATE charity_sync_logs
         SET status = 'failed',
             message = CASE
               WHEN trim(message) = '' THEN '应用重启，任务已中断'
               ELSE message || '（应用重启，任务已中断）'
             END,
             duration_ms = CASE WHEN duration_ms > 0 THEN duration_ms ELSE 0 END
         WHERE status = 'running'",
        [],
    );
}

struct CharityFeedMetaKeys {
    initialized: String,
    source_url: String,
    fetched_at: String,
    read_at: String,
    last_status: String,
    last_message: String,
    last_node: String,
    last_updated: String,
}

fn feed_meta_keys(feed_id: &str) -> CharityFeedMetaKeys {
    CharityFeedMetaKeys {
        initialized: format!("charity_feed_initialized:{feed_id}"),
        source_url: format!("charity_feed_source_url:{feed_id}"),
        fetched_at: format!("charity_feed_last_fetched_at:{feed_id}"),
        read_at: format!("charity_feed_last_read_at:{feed_id}"),
        last_status: format!("charity_feed_last_status:{feed_id}"),
        last_message: format!("charity_feed_last_message:{feed_id}"),
        last_node: format!("charity_feed_last_node:{feed_id}"),
        last_updated: format!("charity_feed_last_updated_count:{feed_id}"),
    }
}

fn write_feed_sync_meta(
    database: &Database,
    feed_id: &str,
    status: &str,
    message: &str,
    node_name: &str,
    updated_count: usize,
) -> Result<(), String> {
    let keys = feed_meta_keys(feed_id);
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    for (key, value) in [
        (keys.last_status, status.to_string()),
        (keys.last_message, message.to_string()),
        (keys.last_node, node_name.to_string()),
        (keys.last_updated, updated_count.to_string()),
    ] {
        connection
            .execute(
                "INSERT INTO app_meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn read_app_meta(database: &Database, key: &str) -> Result<String, String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    connection
        .query_row("SELECT value FROM app_meta WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()
        .map(|value| value.unwrap_or_default())
        .map_err(|error| error.to_string())
}

fn write_app_meta(database: &Database, key: &str, value: &str) -> Result<(), String> {
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    connection
        .execute(
            "INSERT INTO app_meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn unread_count_for_feed(database: &Database, feed_id: &str) -> Result<usize, String> {
    let keys = feed_meta_keys(feed_id);
    let read_at = read_app_meta(database, &keys.read_at)?;
    if read_at.trim().is_empty() {
        // 尚未建立已读水位时不制造角标噪音；进入页面 mark read 后才统计增量。
        return Ok(0);
    }
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let count = connection
        .query_row(
            "SELECT COUNT(*) FROM charity_feed_items
             WHERE feed_id = ?1 AND first_seen_at > ?2",
            params![feed_id, read_at],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| error.to_string())?;
    Ok(count.max(0) as usize)
}

fn load_feed_items_from_db(
    database: &Database,
    source: CharityFeedSource,
    offset: usize,
    limit: usize,
    keyword: &str,
) -> Result<CharityFeedResult, String> {
    let limit = limit.clamp(1, CHARITY_PAGE_LIMIT_MAX);
    let keys = feed_meta_keys(source.id);
    // 单次加锁完成 meta + count + page，避免切标签时多次抢锁卡死。
    let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
    let read_meta = |key: &str| -> Result<String, String> {
        connection
            .query_row("SELECT value FROM app_meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map(|value| value.unwrap_or_default())
            .map_err(|error| error.to_string())
    };
    let initialized = !read_meta(&keys.initialized)?.is_empty();
    let fetched_at = read_meta(&keys.fetched_at)?;
    let read_at = read_meta(&keys.read_at)?;
    let last_status = read_meta(&keys.last_status)?;
    let last_message = read_meta(&keys.last_message)?;
    let last_node = read_meta(&keys.last_node)?;
    let last_updated = read_meta(&keys.last_updated)?.parse::<usize>().unwrap_or(0);
    let key_pat = format!("%{}%", keyword.trim());
    let has_key = !keyword.trim().is_empty();
    let total_count = if has_key {
        connection
            .query_row(
                "SELECT COUNT(*) FROM charity_feed_items
                 WHERE feed_id = ?1 AND (title LIKE ?2 OR author LIKE ?2 OR categories LIKE ?2)",
                params![source.id, key_pat],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            .max(0) as usize
    } else {
        connection
            .query_row(
                "SELECT COUNT(*) FROM charity_feed_items WHERE feed_id = ?1",
                [source.id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            .max(0) as usize
    };
    let unread_count = if read_at.trim().is_empty() {
        0usize
    } else {
        connection
            .query_row(
                "SELECT COUNT(*) FROM charity_feed_items
                 WHERE feed_id = ?1 AND first_seen_at > ?2",
                params![source.id, read_at],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            .max(0) as usize
    };
    let mut statement = connection
        .prepare(
            "SELECT guid, title, link, author, published_at, summary, categories, first_seen_at,
                    reply_count, views, like_count, last_activity_at, pinned, posters
             FROM charity_feed_items
             WHERE feed_id = ?1 AND (?4 = '' OR title LIKE ?4 OR author LIKE ?4 OR categories LIKE ?4)
             ORDER BY published_at DESC, rowid DESC
             LIMIT ?2 OFFSET ?3",
        )
        .map_err(|error| error.to_string())?;
    let items = statement
        .query_map(
            params![source.id, limit as i64, offset as i64, key_pat],
            |row| {
                let categories: String = row.get(6)?;
                let first_seen_at: String = row.get(7)?;
                let posters_raw: String = row.get(13)?;
                let parsed_categories = if categories.is_empty() || categories == "[]" {
                    Vec::new()
                } else {
                    serde_json::from_str::<Vec<String>>(&categories).unwrap_or_default()
                };
                let parsed_posters = if posters_raw.is_empty() || posters_raw == "[]" {
                    Vec::new()
                } else {
                    serde_json::from_str::<Vec<String>>(&posters_raw).unwrap_or_default()
                };
                let is_new = initialized && !read_at.trim().is_empty() && first_seen_at > read_at;
                Ok(CharityFeedItem {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    link: row.get(2)?,
                    author: row.get(3)?,
                    published_at: row.get(4)?,
                    summary: row.get(5)?,
                    categories: parsed_categories,
                    feed_ids: vec![source.id.to_string()],
                    feed_names: vec![source.name.to_string()],
                    is_new,
                    reply_count: row.get(8)?,
                    views: row.get(9)?,
                    like_count: row.get(10)?,
                    last_activity_at: row.get(11)?,
                    pinned: row.get::<_, i64>(12)? != 0,
                    posters: parsed_posters,
                })
            },
        )
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    drop(statement);
    drop(connection);
    let has_more = offset + items.len() < total_count;
    let status = if last_status.is_empty() {
        "local".to_string()
    } else {
        last_status
    };
    let skipped = status == "skipped";
    Ok(CharityFeedResult {
        feed_id: source.id.into(),
        feed_name: source.name.into(),
        items,
        fetched_at,
        changed: false,
        new_count: 0,
        updated_count: last_updated,
        initialized,
        source_profile_name: String::new(),
        source_account_name: String::new(),
        status,
        message: last_message,
        used_node_id: String::new(),
        used_node_name: last_node,
        unread_count,
        skipped,
        total_count,
        offset,
        limit,
        has_more,
    })
}

#[allow(dead_code)]
fn rotate_fast_nodes(nodes: &[(String, String, i64)], offset: usize) -> Vec<(String, String, i64)> {
    if nodes.is_empty() {
        return Vec::new();
    }
    let offset = offset % nodes.len();
    let mut rotated = Vec::with_capacity(nodes.len());
    rotated.extend_from_slice(&nodes[offset..]);
    rotated.extend_from_slice(&nodes[..offset]);
    rotated
}

fn build_mihomo_client(proxy_url: &str) -> Result<reqwest::Client, String> {
    let proxy = reqwest::Proxy::all(proxy_url).map_err(|_| "Mihomo 混合端口地址无效")?;
    // Linux.do 当前的 Cloudflare 路径对 HTTP/1.1 出口会高概率返回 403；
    // Cargo 已显式启用 reqwest/http2，这里保留 ALPN 协商并复用连接。
    reqwest::Client::builder()
        .timeout(CHARITY_REQUEST_TIMEOUT)
        .connect_timeout(Duration::from_secs(8))
        .http2_adaptive_window(true)
        .pool_idle_timeout(Duration::from_secs(60))
        .tcp_keepalive(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .proxy(proxy)
        .build()
        .map_err(|error| format!("无法初始化代理请求客户端：{error}"))
}

const CHARITY_SYNC_CANCELLED_PREFIX: &str = "同步任务已取消";

fn is_charity_sync_cancelled(error: &str) -> bool {
    error.starts_with(CHARITY_SYNC_CANCELLED_PREFIX)
}

#[allow(dead_code)]
fn finish_cancelled_feed_sync(
    app: &AppHandle,
    database: &Database,
    log_id: Option<i64>,
    source: CharityFeedSource,
    stage: &str,
    duration_ms: i64,
) -> String {
    let message = format!("{CHARITY_SYNC_CANCELLED_PREFIX}：{}", source.name);
    let unread_count = unread_count_for_feed(database, source.id).unwrap_or(0);
    finish_charity_sync_log(
        app,
        database,
        log_id,
        source,
        stage,
        "cancelled",
        &message,
        "",
        duration_ms,
        0,
        0,
        unread_count,
    );
    message
}

fn is_http_forbidden_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("http 403")
        || lower.contains("status: 403")
        || lower.contains("status 403")
        || lower.contains("(403)")
        || lower.contains(" 403 ")
        || lower.contains("403 forbidden")
        || lower.ends_with("403")
        || lower.contains("error code: 403")
}

fn is_transport_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("error sending request")
        || lower.contains("i/o timeout")
        || lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("deadline")
        || lower.contains("connection")
        || lower.contains("connection reset")
        || lower.contains("connect error")
        || lower.contains("连接失败")
        || lower.contains("无法建立连接")
        || lower.contains("连接被重置")
}

fn ban_ttl_for_error(error: &str) -> Duration {
    let lower = error.to_ascii_lowercase();
    if is_http_forbidden_error(error) {
        CHARITY_BAN_FORBIDDEN
    } else if is_transport_error(error) {
        // 连节点/代理出口都不可达：预计短期不会恢复，提高拉黑时长。
        CHARITY_BAN_UNREACHABLE
    } else if lower.contains("超时") {
        CHARITY_BAN_TIMEOUT
    } else {
        CHARITY_BAN_DEFAULT
    }
}

/// 失败节点：公益黑名单 + 从共享/本地队列剔除（不删代理池）。
fn eject_node_from_charity_candidate(
    monitor: &CharityMonitorRuntime,
    queue: &Arc<Mutex<CharityNodeQueue>>,
    node: &CharityNodeRef,
    error: &str,
) {
    let ttl = ban_ttl_for_error(error);
    monitor.ban_node(&node.id, ttl);
    if let Ok(mut q) = queue.lock() {
        let _ = q.remove_id(&node.id);
    }
}

fn build_charity_node_queue(
    database: &Database,
    monitor: &CharityMonitorRuntime,
) -> Result<Vec<CharityNodeRef>, String> {
    monitor.purge_expired_bans();
    // 优先使用有订阅来源的节点（igi 专线等，IP 信誉好），避免一头扎进
    // 大量免费公共坏节点（罗马尼亚/美国等常被 Cloudflare 风控导致 403）。
    let raw =
        proxy_pool::list_prioritized_fast_proxy_nodes(database, CHARITY_FAST_NODE_MAX_LATENCY_MS)?;
    let mut nodes = raw
        .into_iter()
        .filter(|(id, _, _)| !monitor.is_banned(id))
        .map(|(id, name, latency_ms)| CharityNodeRef {
            id,
            name,
            latency_ms,
        })
        .collect::<Vec<_>>();
    if nodes.is_empty() {
        return Ok(nodes);
    }
    // 轮询起点：避免每轮总从同一个最快节点开始。
    let offset = monitor.node_round_robin.fetch_add(1, Ordering::Relaxed) % nodes.len();
    if offset > 0 {
        nodes.rotate_left(offset);
    }
    // 装载/尝试都有上限：队头优先，多了的仍可通过轮转在后续轮次用到。
    if nodes.len() > CHARITY_PREPARE_NODE_LIMIT {
        nodes.truncate(CHARITY_PREPARE_NODE_LIMIT);
    }
    Ok(nodes)
}

async fn sync_feed_with_fast_nodes(
    app: &AppHandle,
    database: &Database,
    runtime: &ProxyRuntime,
    source: CharityFeedSource,
    stage: &str,
    cancellation: &CancellationToken,
    shared_queue: Option<Arc<Mutex<CharityNodeQueue>>>,
    nodes_prepared: bool,
) -> Result<CharityFeedResult, String> {
    let feed_started_at = Instant::now();
    let stage_label = if stage == "manual" {
        "手动刷新"
    } else {
        "后台轮询"
    };
    let feed_duration_ms = || feed_started_at.elapsed().as_millis() as i64;

    if cancellation.is_cancelled() {
        return Err(format!("{CHARITY_SYNC_CANCELLED_PREFIX}：{}", source.name));
    }

    // 代理轮询：linux.do 必须走代理。出口唯一。
    // 注意：不在这里长期持锁，改为每个节点尝试前独立拿锁，
    // 让多个标签的尝试交错推进（伪并行），避免一个标签独占整轮时间。
    let monitor_state = app.state::<CharityMonitorRuntime>();

    if cancellation.is_cancelled() {
        return Err(format!("{CHARITY_SYNC_CANCELLED_PREFIX}：{}", source.name));
    }

    let proxy_url = proxy_pool::runtime_proxy_url_pub(runtime);
    let client = match build_mihomo_client(&proxy_url) {
        Ok(client) => client,
        Err(error) => {
            // 无可用客户端：记一条失败任务
            let log_id = append_charity_sync_log(
                database,
                source.id,
                source.name,
                stage,
                "running",
                &format!("{stage_label}失败：无法初始化代理客户端"),
                "",
            );
            finish_charity_sync_log(
                app,
                database,
                log_id,
                source,
                stage,
                "failed",
                &error,
                "",
                feed_duration_ms(),
                0,
                0,
                0,
            );
            return Err(error);
        }
    };

    let queue = if let Some(shared) = shared_queue.clone() {
        shared
    } else {
        let nodes = match tokio::task::block_in_place(|| {
            build_charity_node_queue(database, &monitor_state)
        }) {
            Ok(nodes) => nodes,
            Err(error) => {
                let log_id = append_charity_sync_log(
                    database,
                    source.id,
                    source.name,
                    stage,
                    "running",
                    &format!("{stage_label}失败：读取候选节点"),
                    "",
                );
                finish_charity_sync_log(
                    app,
                    database,
                    log_id,
                    source,
                    stage,
                    "failed",
                    &error,
                    "",
                    feed_duration_ms(),
                    0,
                    0,
                    0,
                );
                return Err(error);
            }
        };
        if nodes.is_empty() {
            let message =
                format!("无 ≤{CHARITY_FAST_NODE_MAX_LATENCY_MS}ms 可用公益候选节点，本轮跳过");
            let log_id = append_charity_sync_log(
                database,
                source.id,
                source.name,
                stage,
                "running",
                &format!("{stage_label}跳过：{}", source.name),
                "",
            );
            let local = tokio::task::block_in_place(|| {
                load_feed_items_from_db(database, source, 0, CHARITY_PAGE_SIZE, "")
            })?;
            let _ = write_feed_sync_meta(database, source.id, "skipped", &message, "", 0);
            finish_charity_sync_log(
                app,
                database,
                log_id,
                source,
                stage,
                "skipped",
                &message,
                "",
                feed_duration_ms(),
                0,
                0,
                local.unread_count,
            );
            let mut local = local;
            local.status = "skipped".into();
            local.skipped = true;
            local.message = message;
            return Ok(local);
        }
        if !nodes_prepared {
            let ids = nodes.iter().map(|n| n.id.clone()).collect::<Vec<_>>();
            if let Err(error) =
                proxy_pool::prepare_proxy_nodes_transient(database, runtime, &ids).await
            {
                let message = format!("装载快节点失败：{error}");
                let log_id = append_charity_sync_log(
                    database,
                    source.id,
                    source.name,
                    stage,
                    "running",
                    &format!("{stage_label}失败：装载节点"),
                    "",
                );
                finish_charity_sync_log(
                    app,
                    database,
                    log_id,
                    source,
                    stage,
                    "failed",
                    &message,
                    "",
                    feed_duration_ms(),
                    0,
                    0,
                    0,
                );
                return Err(message);
            }
        }
        Arc::new(Mutex::new(CharityNodeQueue::from_nodes(nodes)))
    };

    let mut last_error = String::new();
    let mut attempts = 0usize;
    // 关键：Mihomo 对外只有一个出口（OpenHub 组当前选中节点）。
    // 每次尝试必须把「切节点 + 完整请求」视为原子区间，否则其它标签会在
    // CONNECT/TLS 建连前切走出口，导致日志节点与真实出口不一致。
    // 锁只覆盖一次尝试，失败后立即让出，避免单个标签独占整轮。
    let parallel_mode = shared_queue.is_some();

    while attempts < CHARITY_MAX_NODE_ATTEMPTS {
        if cancellation.is_cancelled() {
            if !parallel_mode {
                let _ = proxy_pool::restore_proxy_node_transient(database, runtime).await;
            }
            return Err(format!("{CHARITY_SYNC_CANCELLED_PREFIX}：{}", source.name));
        }

        let Some(node) = queue.lock().ok().and_then(|mut q| q.pop_front()) else {
            break;
        };
        if monitor_state.is_banned(&node.id) {
            continue;
        }

        let attempt_guard = if parallel_mode {
            Some(tokio::select! {
                _ = cancellation.cancelled() => {
                    return Err(format!("{CHARITY_SYNC_CANCELLED_PREFIX}：{}", source.name));
                }
                guard = monitor_state.proxy_sync_lock.lock() => guard,
            })
        } else {
            None
        };
        // 其它标签可能在等待锁期间已判定该节点失败；拿到锁后必须复查。
        if monitor_state.is_banned(&node.id) {
            drop(attempt_guard);
            continue;
        }
        attempts += 1;

        // —— 关键：选定节点后立刻新建「进行中」任务，节点名立刻可见 ——
        let attempt_started = Instant::now();
        let running_msg = format!(
            "{stage_label}进行中：{} · {}（{}ms）· 第{}次",
            source.name, node.name, node.latency_ms, attempts
        );
        let log_id = append_charity_sync_log(
            database,
            source.id,
            source.name,
            stage,
            "running",
            &running_msg,
            &node.name,
        );
        emit_running_progress(app, source, stage, &running_msg, &node.name);

        if cancellation.is_cancelled() {
            if !parallel_mode {
                let _ = proxy_pool::restore_proxy_node_transient(database, runtime).await;
            }
            let dur = attempt_started.elapsed().as_millis() as i64;
            let message = format!("{CHARITY_SYNC_CANCELLED_PREFIX}：{}", source.name);
            finish_charity_sync_log(
                app,
                database,
                log_id,
                source,
                stage,
                "cancelled",
                &message,
                &node.name,
                dur,
                0,
                0,
                0,
            );
            return Err(message);
        }

        if let Err(error) =
            proxy_pool::select_proxy_node_transient(database, runtime, &node.id).await
        {
            let message = format!("{}: 切换代理失败：{error}", node.name);
            eject_node_from_charity_candidate(&monitor_state, &queue, &node, &message);
            let dur = attempt_started.elapsed().as_millis() as i64;
            if !parallel_mode {
                let _ = proxy_pool::restore_proxy_node_transient(database, runtime).await;
            }
            finish_charity_sync_log(
                app, database, log_id, source, stage, "failed", &message, &node.name, dur, 0, 0, 0,
            );
            last_error = message;
            continue; // 失败任务已结束，下一节点开新任务
        }

        // 出口切换后给内核一点生效时间；attempt_guard 仍持有，出口不会漂移。
        let mut waited = Duration::from_millis(0);
        while waited < Duration::from_millis(60) {
            if cancellation.is_cancelled() {
                break;
            }
            let slice = Duration::from_millis(30);
            tokio::time::sleep(slice).await;
            waited += slice;
        }

        // 请求阶段：单次发起 fetch；单次请求最多 CHARITY_REQUEST_TIMEOUT，
        // 剩余预算真正递减，到 0 即判超时（此前每次循环重置导致 60s 从未生效）。
        let request_deadline = Instant::now() + CHARITY_REQUEST_TIMEOUT;
        let fetch_future = fetch_topic_body(app, client.clone(), source);
        tokio::pin!(fetch_future);
        let fetch_result = loop {
            if cancellation.is_cancelled() {
                break Err(format!("{CHARITY_SYNC_CANCELLED_PREFIX}：{}", source.name));
            }
            let left = request_deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break Err(format!(
                    "{}: 请求超时（单次请求 {}s）",
                    node.name,
                    CHARITY_REQUEST_TIMEOUT.as_secs()
                ));
            }
            let tick = left.min(Duration::from_millis(500));
            tokio::select! {
                _ = cancellation.cancelled() => {
                    break Err(format!("{CHARITY_SYNC_CANCELLED_PREFIX}：{}", source.name));
                }
                result = &mut fetch_future => {
                    break result;
                }
                _ = tokio::time::sleep(tick) => {
                    let dur = attempt_started.elapsed().as_millis() as i64;
                    let msg = format!(
                        "{stage_label}进行中：{} · {} · 已用 {:.1}s",
                        source.name,
                        node.name,
                        dur as f64 / 1000.0
                    );
                    if let Some(id) = log_id {
                        touch_running_charity_sync_log(database, id, &msg, &node.name, dur);
                    }
                    emit_running_progress(app, source, stage, &msg, &node.name);
                }
            }
        };
        // 网络响应已完整拿到，后续解析/入库不再依赖当前代理出口。
        drop(attempt_guard);

        match fetch_result {
            Ok((body, profile_name, account_name, protocol)) => {
                match items_from_topic_list(&body) {
                    Ok(items) => {
                        let persist_result = tokio::task::block_in_place(|| {
                            persist_feed(database, source, items, profile_name, account_name)
                        });
                        match persist_result {
                            Ok(mut result) => {
                                result.used_node_id = node.id.clone();
                                result.used_node_name = node.name.clone();
                                result.status = "success".into();
                                result.message = format!(
                                    "已通过 {}（{}ms，{}，第{}次尝试）同步",
                                    node.name, node.latency_ms, protocol, attempts
                                );
                                let _ = write_feed_sync_meta(
                                    database,
                                    source.id,
                                    "success",
                                    &result.message,
                                    &node.name,
                                    result.updated_count + result.new_count,
                                );
                                // 成功节点回队尾：下一次请求从队列弹出另一个节点，
                                // 同一节点不会被 6 个标签连续请求（本轮内基本不会轮回到）。
                                if let Ok(mut q) = queue.lock() {
                                    q.push_back(node.clone());
                                }
                                let _ = proxy_pool::restore_proxy_node_transient(database, runtime)
                                    .await;
                                let dur = attempt_started.elapsed().as_millis() as i64;
                                finish_charity_sync_log(
                                    app,
                                    database,
                                    log_id,
                                    source,
                                    stage,
                                    "success",
                                    &result.message,
                                    &node.name,
                                    dur,
                                    result.new_count,
                                    result.updated_count,
                                    result.unread_count,
                                );
                                return Ok(result);
                            }
                            Err(error) => {
                                // 入库失败：节点未必坏，回队尾；本任务标失败，再开新任务
                                if let Ok(mut q) = queue.lock() {
                                    q.push_back(node.clone());
                                }
                                let message = format!("{}: 入库失败：{error}", node.name);
                                let dur = attempt_started.elapsed().as_millis() as i64;
                                if !parallel_mode {
                                    let _ =
                                        proxy_pool::restore_proxy_node_transient(database, runtime)
                                            .await;
                                }
                                finish_charity_sync_log(
                                    app, database, log_id, source, stage, "failed", &message,
                                    &node.name, dur, 0, 0, 0,
                                );
                                last_error = message;
                            }
                        }
                    }
                    Err(error) => {
                        let raw = format!("{}: {error}", node.name);
                        eject_node_from_charity_candidate(&monitor_state, &queue, &node, &raw);
                        let message = if is_http_forbidden_error(&raw) {
                            format!("{}: HTTP 403，已从公益队列剔除", node.name)
                        } else if is_transport_error(&raw) {
                            format!("{}: 节点连接失败（已从公益队列剔除）：{}", node.name, raw)
                        } else {
                            raw
                        };
                        let dur = attempt_started.elapsed().as_millis() as i64;
                        if !parallel_mode {
                            if !parallel_mode {
                                let _ = proxy_pool::restore_proxy_node_transient(database, runtime)
                                    .await;
                            }
                        }
                        finish_charity_sync_log(
                            app, database, log_id, source, stage, "failed", &message, &node.name,
                            dur, 0, 0, 0,
                        );
                        last_error = message;
                    }
                }
            }
            Err(error) => {
                let cancelled = is_charity_sync_cancelled(&error);
                let raw = if error.contains(':') {
                    error
                } else {
                    format!("{}: {error}", node.name)
                };
                if !cancelled {
                    eject_node_from_charity_candidate(&monitor_state, &queue, &node, &raw);
                }
                let message = if !cancelled && is_http_forbidden_error(&raw) {
                    format!("{}: HTTP 403，已从公益队列剔除", node.name)
                } else if !cancelled && is_transport_error(&raw) {
                    format!("{}: 节点连接失败（已从公益队列剔除）：{}", node.name, raw)
                } else {
                    raw
                };
                let dur = attempt_started.elapsed().as_millis() as i64;
                let status = if cancelled { "cancelled" } else { "failed" };
                if !cancelled && !parallel_mode {
                    let _ = proxy_pool::restore_proxy_node_transient(database, runtime).await;
                }
                finish_charity_sync_log(
                    app, database, log_id, source, stage, status, &message, &node.name, dur, 0, 0,
                    0,
                );
                if cancelled {
                    if !parallel_mode {
                        let _ = proxy_pool::restore_proxy_node_transient(database, runtime).await;
                    }
                    return Err(message);
                }
                last_error = message;
            }
        }
        // 失败已完结；循环继续 = 自动起新任务试下一节点
    }

    if !parallel_mode {
        let _ = proxy_pool::restore_proxy_node_transient(database, runtime).await;
    }
    let mut local = tokio::task::block_in_place(|| {
        load_feed_items_from_db(database, source, 0, CHARITY_PAGE_SIZE, "")
    })
    .unwrap_or(CharityFeedResult {
        feed_id: source.id.into(),
        feed_name: source.name.into(),
        items: Vec::new(),
        fetched_at: String::new(),
        changed: false,
        new_count: 0,
        updated_count: 0,
        initialized: false,
        source_profile_name: String::new(),
        source_account_name: String::new(),
        status: "error".into(),
        message: String::new(),
        used_node_id: String::new(),
        used_node_name: String::new(),
        unread_count: 0,
        skipped: false,
        total_count: 0,
        offset: 0,
        limit: CHARITY_PAGE_SIZE,
        has_more: false,
    });
    local.status = "error".into();
    local.message = if last_error.is_empty() {
        format!(
            "{} 同步失败：本轮没有可尝试的 ≤{}ms 候选节点",
            source.name, CHARITY_FAST_NODE_MAX_LATENCY_MS,
        )
    } else {
        format!(
            "{}（本轮已尝试 {attempts}/{} 个节点）",
            last_error, CHARITY_MAX_NODE_ATTEMPTS
        )
    };
    let _ = write_feed_sync_meta(database, source.id, "error", &local.message, "", 0);
    // 各节点失败已分别记日志；这里不再追加空节点总失败行，避免“节点—”污染。
    Err(local.message.clone())
}

#[tauri::command]
pub async fn get_charity_feed(
    database: State<'_, Database>,
    runtime: State<'_, CharityMonitorRuntime>,
    feed_id: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
    keyword: Option<String>,
) -> Result<CharityFeedResult, String> {
    let requested = feed_id.as_deref().unwrap_or(DEFAULT_CHARITY_FEED_ID);
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(CHARITY_PAGE_SIZE);
    let keyword = keyword.unwrap_or_default();
    if requested == "all" {
        return tokio::task::block_in_place(|| {
            load_all_feed_items_from_db(&database, offset, limit, &keyword)
        });
    }
    let source = charity_feed_source(requested)?;
    // 读库离开 async worker，避免同步命令/锁拖住运行时。
    let mut result = tokio::task::block_in_place(|| {
        load_feed_items_from_db(&database, source, offset, limit, &keyword)
    })?;
    if let Ok(errors) = runtime.last_errors.lock() {
        if let Some(message) = errors.get(source.id) {
            if result.message.is_empty() {
                result.message = message.clone();
                if result.status == "local" {
                    result.status = "error".into();
                }
            }
        }
    }
    Ok(result)
}

#[tauri::command]
pub async fn mark_charity_feed_read(
    database: State<'_, Database>,
    feed_id: Option<String>,
) -> Result<usize, String> {
    let requested = feed_id.as_deref().unwrap_or(DEFAULT_CHARITY_FEED_ID);
    tokio::task::block_in_place(|| {
        let now = {
            let connection = database
                .0
                .lock()
                .map_err(|_| "本地数据库锁定失败".to_string())?;
            connection
                .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|error| error.to_string())?
        };
        if requested == "all" {
            // 全部视图：一次性把 6 个标签的已读水位都推进
            let mut total = 0usize;
            for source in CHARITY_FEEDS {
                write_app_meta(&database, &feed_meta_keys(source.id).read_at, &now)?;
                total += unread_count_for_feed(&database, source.id)?;
            }
            return Ok(total);
        }
        let source = charity_feed_source(requested)?;
        write_app_meta(&database, &feed_meta_keys(source.id).read_at, &now)?;
        unread_count_for_feed(&database, source.id)
    })
}

/// 今天发布的公益帖子数 = published_at 落在「本地时区今天 00:00 ~ 明天 00:00」。
/// published_at 为 UTC ISO 字符串，统一用 unixepoch() 解析后按 UTC 秒区间比较，避免 T/空格格式差异。
#[tauri::command]
pub async fn get_charity_today_count(database: State<'_, Database>) -> Result<usize, String> {
    tokio::task::block_in_place(|| {
        let (utc_start, utc_end) = local_day_utc_range_secs();
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        let count = connection
            .query_row(
                "SELECT COUNT(*) FROM charity_feed_items
                 WHERE published_at IS NOT NULL
                   AND unixepoch(published_at) >= ?1
                   AND unixepoch(published_at) < ?2",
                params![utc_start, utc_end],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?;
        Ok(count.max(0) as usize)
    })
}

/// 本地时区今天 00:00 与明天 00:00 对应的 UTC 秒区间。
/// 不新增 chrono 依赖：用 /bin/date 读取时区偏移 ±HHMM，再以 UTC epoch 换算。
fn local_day_utc_range_secs() -> (i64, i64) {
    let offset = local_utc_offset_secs();
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|dur| dur.as_secs() as i64)
        .unwrap_or(0);
    let local_now = now_secs + offset;
    let local_today_start = local_now - local_now.rem_euclid(86_400);
    let utc_start = local_today_start - offset;
    (utc_start, utc_start + 86_400)
}

/// 读取本地时区相对 UTC 的偏移秒数（如 +0800 → 28800；失败回退 0）。
fn local_utc_offset_secs() -> i64 {
    if let Ok(output) = std::process::Command::new("/bin/date").arg("+%z").output() {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            let value = text.trim();
            if value.len() == 5 && (value.starts_with('+') || value.starts_with('-')) {
                if let (Ok(hour), Ok(minute)) =
                    (value[1..3].parse::<i64>(), value[3..5].parse::<i64>())
                {
                    let sign = if value.starts_with('-') { -1 } else { 1 };
                    if hour < 24 && minute < 60 {
                        return sign * (hour * 3600 + minute * 60);
                    }
                }
            }
        }
    }
    0
}

#[tauri::command]
pub async fn get_charity_unread_total(database: State<'_, Database>) -> Result<usize, String> {
    tokio::task::block_in_place(|| {
        let mut total = 0usize;
        for source in CHARITY_FEEDS {
            total += unread_count_for_feed(&database, source.id)?;
        }
        Ok(total)
    })
}

#[tauri::command]
pub async fn fetch_charity_feed(
    app: AppHandle,
    database: State<'_, Database>,
    runtime: State<'_, ProxyRuntime>,
    monitor: State<'_, CharityMonitorRuntime>,
    feed_id: Option<String>,
) -> Result<CharityFeedResult, String> {
    let source = charity_feed_source(feed_id.as_deref().unwrap_or(DEFAULT_CHARITY_FEED_ID))?;
    // 手动刷新与后台轮询互斥，避免打开页面时和定时任务抢锁卡死。
    let Some(cancellation) = monitor.try_begin_sync() else {
        let mut local = tokio::task::block_in_place(|| {
            load_feed_items_from_db(&database, source, 0, CHARITY_PAGE_SIZE, "")
        })?;
        local.message = "后台同步进行中，已返回本地数据".into();
        local.status = if local.status.is_empty() {
            "local".into()
        } else {
            local.status
        };
        emit_charity_progress(
            &app,
            CharitySyncProgress {
                feed_id: source.id.into(),
                feed_name: source.name.into(),
                stage: "manual".into(),
                status: "skipped".into(),
                message: local.message.clone(),
                used_node_id: String::new(),
                used_node_name: String::new(),
                new_count: 0,
                updated_count: 0,
                unread_count: local.unread_count,
            },
        );
        return Ok(local);
    };
    // 同步只负责写库；返回值也从本地库再读一遍，保证与 UI 查询同源。
    let sync_result = sync_feed_with_fast_nodes(
        &app,
        &database,
        &runtime,
        source,
        "manual",
        &cancellation,
        None,
        false,
    )
    .await;
    monitor.end_sync();
    match &sync_result {
        Ok(_) => {
            if let Ok(mut errors) = monitor.last_errors.lock() {
                errors.remove(source.id);
            }
        }
        Err(error) => {
            if !is_charity_sync_cancelled(error) {
                if let Ok(mut errors) = monitor.last_errors.lock() {
                    errors.insert(source.id.to_string(), error.clone());
                }
            }
        }
    }
    // 无论成功失败，UI 应以本地库为准；同步错误通过 status/message 元数据体现。
    let mut local = tokio::task::block_in_place(|| {
        load_feed_items_from_db(&database, source, 0, CHARITY_PAGE_SIZE, "")
    })?;
    if let Err(error) = sync_result {
        if local.message.is_empty() {
            local.message = error;
            local.status = "error".into();
        }
    }
    Ok(local)
}

/// 公益同步用代理池概览：有效节点数 / ≤500ms 候选数。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CharityProxyPoolSummary {
    /// 测速成功且延迟有效的节点数
    pub(crate) valid_count: usize,
    /// 当前公益候选阈值（≤500ms）内的节点数
    pub(crate) candidate_count: usize,
}

#[tauri::command]
pub async fn get_charity_proxy_pool_summary(
    database: State<'_, Database>,
    monitor: State<'_, CharityMonitorRuntime>,
) -> Result<CharityProxyPoolSummary, String> {
    tokio::task::block_in_place(|| {
        let connection = database.0.lock().map_err(|_| "本地数据库锁定失败")?;
        let valid_count = connection
            .query_row(
                "SELECT COUNT(*) FROM proxy_pool_nodes
                 WHERE test_status = 'success' AND latency_ms IS NOT NULL AND latency_ms > 0",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            .max(0) as usize;
        let candidate_count = connection
            .query_row(
                "SELECT COUNT(*) FROM proxy_pool_nodes
                 WHERE test_status = 'success' AND latency_ms IS NOT NULL AND latency_ms > 0
                   AND latency_ms <= ?1",
                [CHARITY_FAST_NODE_MAX_LATENCY_MS],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            .max(0) as usize;

        // 扣除当前仍被公益拉黑/冷却中的节点（403、超时、成功冷却）。
        // 只影响公益候选展示，不修改代理池本身。
        let banned = monitor.active_banned_ids();
        if banned.is_empty() {
            return Ok(CharityProxyPoolSummary {
                valid_count,
                candidate_count,
            });
        }
        let placeholders = banned.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let mut params = banned
            .iter()
            .map(|id| rusqlite::types::Value::Text(id.clone()))
            .collect::<Vec<_>>();
        let valid_after = connection
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM proxy_pool_nodes
                     WHERE test_status = 'success' AND latency_ms IS NOT NULL AND latency_ms > 0
                       AND id IN ({placeholders})"
                ),
                rusqlite::params_from_iter(params.iter()),
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            .max(0) as usize;
        let mut candidate_params = Vec::with_capacity(params.len() + 1);
        candidate_params.push(rusqlite::types::Value::Integer(
            CHARITY_FAST_NODE_MAX_LATENCY_MS,
        ));
        candidate_params.append(&mut params);
        let candidate_after = connection
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM proxy_pool_nodes
                     WHERE test_status = 'success' AND latency_ms IS NOT NULL AND latency_ms > 0
                       AND latency_ms <= ?1
                       AND id IN ({placeholders})"
                ),
                rusqlite::params_from_iter(candidate_params.iter()),
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?
            .max(0) as usize;
        Ok(CharityProxyPoolSummary {
            valid_count: valid_count.saturating_sub(valid_after),
            candidate_count: candidate_count.saturating_sub(candidate_after),
        })
    })
}

#[tauri::command]
pub async fn get_charity_sync_logs(
    database: State<'_, Database>,
    limit: Option<usize>,
) -> Result<Vec<CharitySyncLogEntry>, String> {
    tokio::task::block_in_place(|| list_charity_sync_logs(&database, limit.unwrap_or(120)))
}

#[tauri::command]
pub async fn clear_charity_sync_logs(database: State<'_, Database>) -> Result<(), String> {
    tokio::task::block_in_place(|| clear_charity_sync_logs_db(&database))
}

#[tauri::command]
pub fn set_charity_monitor_visible(
    monitor: State<'_, CharityMonitorRuntime>,
    visible: bool,
) -> Result<(), String> {
    monitor.set_visible(visible);
    Ok(())
}

#[tauri::command]
pub fn request_charity_round(monitor: State<'_, CharityMonitorRuntime>) -> Result<(), String> {
    monitor.request_round();
    Ok(())
}

#[tauri::command]
pub async fn refresh_all_charity_feeds(
    database: State<'_, Database>,
    monitor: State<'_, CharityMonitorRuntime>,
) -> Result<CharityRefreshAllResult, String> {
    // “立即刷新”具有替换语义：先取消当前整轮和数据库中所有未结束历史任务，
    // 再让后台调度器立刻启动包含全部标签的新一轮。
    let cancelled_active_round = monitor.cancel_active_sync();
    let cancelled_log_count = tokio::task::block_in_place(|| {
        cancel_running_charity_sync_logs(&database, "已被新的“立即刷新全部标签”任务取消")
    })?;
    monitor.request_round();
    Ok(CharityRefreshAllResult {
        cancelled_active_round,
        cancelled_log_count,
        feed_count: CHARITY_FEEDS.len(),
    })
}

/// 读取本地时分秒（macOS/Linux 用 /bin/date；失败时退回 UTC 近似，保证调度不中断）。
fn local_hms() -> (u32, u32, u32) {
    if let Ok(output) = std::process::Command::new("/bin/date")
        .arg("+%H:%M:%S")
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            let parts = text.trim().split(':').collect::<Vec<_>>();
            if parts.len() == 3 {
                if let (Ok(h), Ok(m), Ok(s)) = (
                    parts[0].parse::<u32>(),
                    parts[1].parse::<u32>(),
                    parts[2].parse::<u32>(),
                ) {
                    if h < 24 && m < 60 && s < 60 {
                        return (h, m, s);
                    }
                }
            }
        }
    }
    // fallback: UTC
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let tod = (secs % 86_400) as u32;
    (tod / 3600, (tod % 3600) / 60, tod % 60)
}

/// 距离下一个「分钟 % interval == 0 且秒为 00」还有多少秒。
/// 例如 interval=5：
/// - 12:03:10 → 110s 后到 12:05:00
/// - 12:05:00 已到点 → 300s 后到 12:10:00（当前点由调度循环消费后才会再算）
/// - 12:05:01 → 299s 后到 12:10:00
/// - 12:59:50 → 10s 后到 13:00:00
fn seconds_until_next_aligned_run(
    hour: u32,
    minute: u32,
    second: u32,
    interval_minutes: u32,
) -> u64 {
    let interval = interval_minutes.max(1) as i64;
    let interval_secs = interval * 60;
    let now_secs = hour as i64 * 3600 + minute as i64 * 60 + second as i64;
    // 下一个对齐点：ceil 到 interval 边界；若刚好在边界（秒=0 且分钟对齐）则跳到下一格，
    // 避免同一触发点被重复消费。调度侧在到点后会先跑一轮再重新计算。
    let rem = now_secs.rem_euclid(interval_secs);
    let delta = if rem == 0 {
        // 整点秒：若 second==0 表示正好在边界；调度等待时用 >0 推到下一格。
        // 但 start 循环在 force 为空时会 sleep delta，若 delta=interval 合理。
        interval_secs
    } else {
        interval_secs - rem
    };
    delta as u64
}

fn seconds_until_next_scheduled_run() -> u64 {
    let (h, m, s) = local_hms();
    seconds_until_next_aligned_run(h, m, s, CHARITY_SCHEDULE_EVERY_MINUTES).max(1)
}

pub(crate) fn start_charity_monitor(app: AppHandle) {
    let monitor = app.state::<CharityMonitorRuntime>();
    if monitor.running.swap(true, Ordering::SeqCst) {
        return;
    }
    // 启动时清理上次异常退出留下的 running 日志。
    {
        let database = app.state::<Database>();
        tokio::task::block_in_place(|| abandon_running_charity_sync_logs(&database));
    }
    tauri::async_runtime::spawn(async move {
        // 启动后稍等：给代理 restore 一点时间；不自动跑首轮，等下一个 :00/:05/:10… 或按钮临时触发。
        tokio::time::sleep(Duration::from_secs(1)).await;
        loop {
            let monitor = app.state::<CharityMonitorRuntime>();
            // 临时触发（立即刷新按钮）优先，否则等到下一个 5 分钟对齐点（秒 00）。
            let force = monitor.force_round.load(Ordering::Relaxed);
            if !force {
                let mut wait_secs = seconds_until_next_scheduled_run();
                while wait_secs > 0 {
                    if app
                        .state::<CharityMonitorRuntime>()
                        .force_round
                        .load(Ordering::Relaxed)
                    {
                        break;
                    }
                    let step = wait_secs.min(CHARITY_SCHEDULER_TICK.as_secs().max(1));
                    tokio::time::sleep(Duration::from_secs(step)).await;
                    wait_secs = wait_secs.saturating_sub(step);
                    // 接近触发点时用实时时钟重算，避免 sleep 漂移跨过对齐点。
                    if wait_secs <= 5 {
                        let recomputed = seconds_until_next_scheduled_run();
                        // 若重算突然变大，说明已跨过对齐点，应立刻进入一轮。
                        if recomputed > 30 && wait_secs <= 5 {
                            wait_secs = 0;
                            break;
                        }
                        wait_secs = recomputed.min(wait_secs);
                    }
                }
            }

            // 到点或按钮触发：若上一轮仍在跑，等它结束（按钮 cancel 会释放）。
            let Some(cancellation) = (loop {
                let monitor = app.state::<CharityMonitorRuntime>();
                if let Some(token) = monitor.try_begin_sync() {
                    break Some(token);
                }
                if monitor.force_round.load(Ordering::Relaxed) {
                    // 立即刷新会 cancel 旧轮；稍等后重试拿锁。
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                }
                // 定时到点但上轮未结束：跳过本点，等下一个 5 分钟对齐点，避免堆积。
                break None;
            }) else {
                continue;
            };

            let forced = app
                .state::<CharityMonitorRuntime>()
                .force_round
                .swap(false, Ordering::Relaxed);
            let stage = if forced { "manual" } else { "poll" };
            // 每轮：重建 ≤500ms 候选 → 一次装载 → 整轮共享一个队列。
            // 每次请求弹出不同的节点（成功节点回队尾，本轮内不重复使用），
            // 避免同一节点被 6 个标签连续请求触发 Cloudflare 403。
            // 网络尝试通过 proxy_sync_lock 公平串行，配合每标签尝试上限防止饿死。
            let database = app.state::<Database>();
            let runtime = app.state::<ProxyRuntime>();
            let round_nodes =
                tokio::task::block_in_place(|| build_charity_node_queue(&database, &monitor))
                    .unwrap_or_default();

            if round_nodes.is_empty() {
                for source in CHARITY_FEEDS {
                    let message = format!(
                        "无 ≤{CHARITY_FAST_NODE_MAX_LATENCY_MS}ms 可用公益候选节点，本轮跳过"
                    );
                    let _ = write_feed_sync_meta(&database, source.id, "skipped", &message, "", 0);
                    if let Ok(mut errors) = monitor.last_errors.lock() {
                        errors.insert(source.id.to_string(), message);
                    }
                }
                monitor.end_sync();
                continue;
            }

            let prepare_ids = round_nodes
                .iter()
                .map(|node| node.id.clone())
                .collect::<Vec<_>>();
            if let Err(error) =
                proxy_pool::prepare_proxy_nodes_transient(&database, &runtime, &prepare_ids).await
            {
                let message = format!("装载公益候选节点失败：{error}");
                for source in CHARITY_FEEDS {
                    let _ = write_feed_sync_meta(&database, source.id, "error", &message, "", 0);
                    if let Ok(mut errors) = monitor.last_errors.lock() {
                        errors.insert(source.id.to_string(), message.clone());
                    }
                }
                monitor.end_sync();
                continue;
            }

            let mut handles = Vec::with_capacity(CHARITY_FEEDS.len());
            let shared_queue = Arc::new(Mutex::new(CharityNodeQueue::from_nodes(round_nodes)));
            for source in CHARITY_FEEDS {
                let app = app.clone();
                let source = *source;
                let cancellation = cancellation.clone();
                let shared_queue = shared_queue.clone();
                handles.push(tauri::async_runtime::spawn(async move {
                    let database = app.state::<Database>();
                    let runtime = app.state::<ProxyRuntime>();
                    let result = sync_feed_with_fast_nodes(
                        &app,
                        &database,
                        &runtime,
                        source,
                        stage,
                        &cancellation,
                        Some(shared_queue),
                        true,
                    )
                    .await;
                    (source.id.to_string(), result)
                }));
            }
            for handle in handles {
                match handle.await {
                    Ok((feed_id, Ok(_))) => {
                        if let Ok(mut errors) =
                            app.state::<CharityMonitorRuntime>().last_errors.lock()
                        {
                            errors.remove(&feed_id);
                        }
                    }
                    Ok((feed_id, Err(error))) => {
                        if !is_charity_sync_cancelled(&error) {
                            if let Ok(mut errors) =
                                app.state::<CharityMonitorRuntime>().last_errors.lock()
                            {
                                errors.insert(feed_id, error);
                            }
                        }
                    }
                    Err(error) => eprintln!("公益监听并行任务失败：{error}"),
                }
            }
            // 整轮结束：恢复用户全局代理出口；队列自然丢弃，下轮重建。
            let _ = proxy_pool::restore_proxy_node_transient(&database, &runtime).await;
            app.state::<CharityMonitorRuntime>().end_sync();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_charity_round_can_be_cancelled_and_released() {
        let runtime = CharityMonitorRuntime::new();
        let cancellation = runtime.try_begin_sync().expect("first round should start");
        assert!(runtime.try_begin_sync().is_none());
        assert!(runtime.cancel_active_sync());
        assert!(cancellation.is_cancelled());
        runtime.end_sync();
        assert!(!runtime.cancel_active_sync());
        assert!(runtime.try_begin_sync().is_some());
    }

    #[test]
    fn bans_node_only_in_charity_candidate_set() {
        let runtime = CharityMonitorRuntime::new();
        assert!(!runtime.is_banned("n1"));
        runtime.ban_node("n1", Duration::from_secs(60));
        assert!(runtime.is_banned("n1"));
        // 空 id 忽略，避免误伤
        runtime.ban_node("", Duration::from_secs(60));
        assert!(!runtime.is_banned(""));
    }

    #[test]
    fn http_403_is_forbidden_and_ejects_from_queue() {
        assert!(is_http_forbidden_error("公益站标签请求失败（HTTP 403）"));
        assert!(is_http_forbidden_error("unexpected status 403 Forbidden"));
        assert!(!is_http_forbidden_error("timeout after 8s"));
        assert_eq!(ban_ttl_for_error("HTTP 403"), CHARITY_BAN_FORBIDDEN);
        assert!(is_transport_error(
            "error sending request for url (https://linux.do/tag/1980)"
        ));
        assert!(is_transport_error("connection reset by peer"));
        assert_eq!(
            ban_ttl_for_error("error sending request for url (https://linux.do)"),
            CHARITY_BAN_UNREACHABLE
        );

        let mut queue = CharityNodeQueue::from_nodes(vec![
            CharityNodeRef {
                id: "a".into(),
                name: "A".into(),
                latency_ms: 10,
            },
            CharityNodeRef {
                id: "b".into(),
                name: "B".into(),
                latency_ms: 20,
            },
            CharityNodeRef {
                id: "a".into(),
                name: "A2".into(),
                latency_ms: 11,
            },
        ]);
        assert_eq!(queue.remove_id("a"), 2);
        assert_eq!(queue.pop_front().unwrap().id, "b");
        assert!(queue.is_empty());
    }

    #[test]
    fn node_queue_pops_front_and_push_back() {
        let mut queue = CharityNodeQueue::from_nodes(vec![
            CharityNodeRef {
                id: "a".into(),
                name: "A".into(),
                latency_ms: 10,
            },
            CharityNodeRef {
                id: "b".into(),
                name: "B".into(),
                latency_ms: 20,
            },
        ]);
        let first = queue.pop_front().expect("a");
        assert_eq!(first.id, "a");
        queue.push_back(first);
        assert_eq!(queue.pop_front().unwrap().id, "b");
        assert_eq!(queue.pop_front().unwrap().id, "a");
        assert!(queue.is_empty());
    }

    #[test]
    fn schedules_every_five_minutes_on_the_clock() {
        // 12:03:10 → 1 分 50 秒后到 12:05:00
        assert_eq!(seconds_until_next_aligned_run(12, 3, 10, 5), 110);
        // 12:05:00 正好对齐 → 下一格 12:10:00
        assert_eq!(seconds_until_next_aligned_run(12, 5, 0, 5), 300);
        // 12:05:01 → 12:10:00
        assert_eq!(seconds_until_next_aligned_run(12, 5, 1, 5), 299);
        // 12:04:59 → 1 秒到 12:05:00
        assert_eq!(seconds_until_next_aligned_run(12, 4, 59, 5), 1);
        // 12:00:00 → 12:05:00
        assert_eq!(seconds_until_next_aligned_run(12, 0, 0, 5), 300);
        // 12:59:50 → 10 秒到 13:00:00
        assert_eq!(seconds_until_next_aligned_run(12, 59, 50, 5), 10);
        // 23:58:00 → 2 分钟到 00:00:00
        assert_eq!(seconds_until_next_aligned_run(23, 58, 0, 5), 120);
    }

    #[test]
    fn rotates_fast_nodes_for_round_robin() {
        let nodes = vec![
            ("a".to_string(), "A".to_string(), 100),
            ("b".to_string(), "B".to_string(), 200),
            ("c".to_string(), "C".to_string(), 300),
        ];
        assert_eq!(rotate_fast_nodes(&nodes, 0)[0].0, "a");
        assert_eq!(rotate_fast_nodes(&nodes, 1)[0].0, "b");
        assert_eq!(rotate_fast_nodes(&nodes, 2)[0].0, "c");
        // 环绕：偏移超过长度时回到开头
        assert_eq!(rotate_fast_nodes(&nodes, 3)[0].0, "a");
        assert_eq!(rotate_fast_nodes(&nodes, 4)[0].0, "b");
        assert!(rotate_fast_nodes(&[], 0).is_empty());
    }

    #[test]
    fn recognizes_configured_linux_do_tags() {
        assert_eq!(charity_feed_source("1515").unwrap().name, "公益推广");
        assert_eq!(charity_feed_source("1980").unwrap().name, "公益站");
        assert_eq!(charity_feed_source("2233").unwrap().name, "中转站");
        assert_eq!(charity_feed_source("2234").unwrap().name, "开源推广");
        assert_eq!(charity_feed_source("1514").unwrap().name, "高级推广");
        assert_eq!(charity_feed_source("193").unwrap().name, "订阅节点");
        assert!(charity_feed_source("unknown").is_err());
    }

    #[test]
    fn sorts_by_topic_creation_time_instead_of_activity_time() {
        let topics = r#"{
          "users":[{"id":7,"username":"user7","avatar_template":"/user_avatar/linux.do/user7/{size}/1_2.png"}],
          "topic_list":{"topics":[
            {"id":1,"title":"旧帖新回复","slug":"old","created_at":"2026-07-01T08:00:00.000Z","posts_count":12,"reply_count":11,"views":23500,"like_count":3,"last_posted_at":"2026-08-04T10:00:00.000Z","pinned":true,"tags":["运营反馈","公告"],"posters":[{"user_id":7}]},
            {"id":2,"title":"真正的新帖","slug":"new","created_at":"2026-08-03T02:00:00.000Z","excerpt":"<p>新帖摘要</p>","posts_count":2,"views":100},
            {"id":3,"title":"最新主题","slug":"latest","created_at":"2026-08-04T01:00:00.000Z","posts_count":1,"views":8}
          ]}
        }"#;
        let items = items_from_topic_list(topics).unwrap();
        assert_eq!(items[0].title, "最新主题");
        assert_eq!(items[1].title, "真正的新帖");
        assert_eq!(items[1].summary, "新帖摘要");
        assert_eq!(items[2].title, "旧帖新回复");
        assert_eq!(items[2].author, "user7");
        assert_eq!(items[2].reply_count, 11);
        assert_eq!(items[2].views, 23500);
        assert!(items[2].pinned);
        assert_eq!(
            items[2].posters[0],
            "https://linux.do/user_avatar/linux.do/user7/48/1_2.png"
        );
    }
}
