use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
    time::Duration,
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

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BrowserFeedResult {
    ok: bool,
    #[serde(default)]
    body: String,
    #[serde(default)]
    error: String,
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

const CHARITY_FAST_NODE_MAX_LATENCY_MS: i64 = 1000;
const CHARITY_PAGE_SIZE: usize = 20;
const CHARITY_POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);
const CHARITY_HIDDEN_POLL_INTERVAL: Duration = Duration::from_secs(15 * 60);

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
        }
    }

    pub(crate) fn set_visible(&self, visible: bool) {
        // 仅记录可见性，用于前台 5 分钟 / 后台 15 分钟降频。
        self.visible.store(visible, Ordering::Relaxed);
    }

    pub(crate) fn request_round(&self) {
        // 打开应用或回到前台时请求立刻补一轮；由调度循环消费。
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
) -> Result<String, String> {
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
        .header(
            reqwest::header::USER_AGENT,
            chrome_session::chrome_user_agent(),
        );
    if let Some(cookie_header) = cookie_header.filter(|value| !value.trim().is_empty()) {
        request = request.header(reqwest::header::COOKIE, cookie_header);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("{}标签请求失败：{error}", source.name))?;
    if !response.status().is_success() {
        return Err(format!(
            "{}标签请求失败（HTTP {}）",
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
    Ok(body)
}

async fn fetch_topic_body(
    app: &tauri::AppHandle,
    client: reqwest::Client,
    source: CharityFeedSource,
) -> Result<(String, String, String), String> {
    // 请求必须走代理（Mihomo 混合端口），不做直连。
    if let Ok(body) = request_topic_list(&client, source, None).await {
        return Ok((body, String::new(), String::new()));
    }

    let home_dir = app
        .path()
        .home_dir()
        .map_err(|error| format!("无法定位用户目录：{error}"))?;
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
                let body = request_topic_list(&client, source, Some(&cookie_header)).await?;
                Ok::<_, String>((body, profile.name, profile.account_name))
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

    let state_key = format!("__openhub_charity_feed_v4_{}", source.id);
    let script = format!(
        r#"(() => {{
  const stateKey = {state_key};
  const existing = window[stateKey];
  if (existing && existing.done) return JSON.stringify(existing);
  if (!existing) {{
    window[stateKey] = {{ done: false }};
    fetch({source_url}, {{ credentials: "include", cache: "no-store", headers: {{ Accept: "application/json" }} }})
      .then(async response => {{
        const body = await response.text();
        window[stateKey] = response.ok
          ? {{ done: true, ok: true, body }}
          : {{ done: true, ok: false, error: `{source_name} HTTP ${{response.status}}` }};
      }})
      .catch(error => {{ window[stateKey] = {{ done: true, ok: false, error: String(error) }}; }});
  }}
  return "__OPENHUB_PENDING__";
}})()"#,
        state_key = serde_json::to_string(&state_key).map_err(|error| error.to_string())?,
        source_url = serde_json::to_string(source.json_url).map_err(|error| error.to_string())?,
        source_name = source.name,
    );
    let browser_result = tauri::async_runtime::spawn_blocking(move || {
        chrome_session::run_javascript_in_existing_chrome_tab(
            source.json_url,
            &script,
            Duration::from_secs(8),
        )
    })
    .await
    .map_err(|error| format!("Linux.do 标签页请求任务失败：{error}"))?;
    if let Ok(Some(value)) = browser_result {
        let result: BrowserFeedResult = serde_json::from_str(&value)
            .map_err(|error| format!("Linux.do 标签页返回格式错误：{error}"))?;
        if result.ok && result.body.trim_start().starts_with('{') {
            return Ok((
                result.body,
                "已打开的 Linux.do 标签页".into(),
                String::new(),
            ));
        }
        if !result.error.is_empty() {
            errors.push(result.error);
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
) -> Result<CharityFeedResult, String> {
    let limit = limit.clamp(1, 50);
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
    let total_count = connection
        .query_row(
            "SELECT COUNT(*) FROM charity_feed_items WHERE feed_id = ?1",
            [source.id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| error.to_string())?
        .max(0) as usize;
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
             WHERE feed_id = ?1
             ORDER BY published_at DESC, rowid DESC
             LIMIT ?2 OFFSET ?3",
        )
        .map_err(|error| error.to_string())?;
    let items = statement
        .query_map(params![source.id, limit as i64, offset as i64], |row| {
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
                is_new,
                reply_count: row.get(8)?,
                views: row.get(9)?,
                like_count: row.get(10)?,
                last_activity_at: row.get(11)?,
                pinned: row.get::<_, i64>(12)? != 0,
                posters: parsed_posters,
            })
        })
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
    reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::limited(5))
        .proxy(proxy)
        .build()
        .map_err(|error| format!("无法初始化代理请求客户端：{error}"))
}

const CHARITY_SYNC_CANCELLED_PREFIX: &str = "同步任务已取消";

fn is_charity_sync_cancelled(error: &str) -> bool {
    error.starts_with(CHARITY_SYNC_CANCELLED_PREFIX)
}

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

async fn sync_feed_with_fast_nodes(
    app: &AppHandle,
    database: &Database,
    runtime: &ProxyRuntime,
    source: CharityFeedSource,
    stage: &str,
    cancellation: &CancellationToken,
) -> Result<CharityFeedResult, String> {
    let started_at = std::time::Instant::now();
    let stage_label = if stage == "manual" {
        "手动刷新"
    } else {
        "后台轮询"
    };
    // 每个标签每次同步只落一条日志：开始插入 running，结束时更新同一条记录。
    let log_id = append_charity_sync_log(
        database,
        source.id,
        source.name,
        stage,
        "running",
        &format!("{stage_label}开始：{}", source.name),
        "",
    );
    emit_charity_progress(
        app,
        CharitySyncProgress {
            feed_id: source.id.into(),
            feed_name: source.name.into(),
            stage: stage.into(),
            status: "running".into(),
            message: format!("{stage_label}开始：{}", source.name),
            used_node_id: String::new(),
            used_node_name: String::new(),
            new_count: 0,
            updated_count: 0,
            unread_count: 0,
        },
    );
    let duration_ms = || started_at.elapsed().as_millis() as i64;

    if cancellation.is_cancelled() {
        return Err(finish_cancelled_feed_sync(
            app,
            database,
            log_id,
            source,
            stage,
            duration_ms(),
        ));
    }

    // 代理轮询：linux.do 必须走代理，不做直连。
    // 全局代理出口唯一，用锁把“切换+请求+恢复”整段串行化，避免并行标签互相覆盖出口。
    // 等待代理锁和网络请求都响应取消，避免旧任务阻塞“立即刷新全部标签”。
    let monitor_state = app.state::<CharityMonitorRuntime>();
    let _guard = tokio::select! {
        _ = cancellation.cancelled() => {
            return Err(finish_cancelled_feed_sync(
                app,
                database,
                log_id,
                source,
                stage,
                duration_ms(),
            ));
        }
        guard = monitor_state.proxy_sync_lock.lock() => guard,
    };
    let proxy_url = proxy_pool::runtime_proxy_url_pub(runtime);
    let client = match build_mihomo_client(&proxy_url) {
        Ok(client) => client,
        Err(error) => {
            finish_charity_sync_log(
                app,
                database,
                log_id,
                source,
                stage,
                "failed",
                &error,
                "",
                duration_ms(),
                0,
                0,
                0,
            );
            return Err(error);
        }
    };
    let fast_nodes = match tokio::task::block_in_place(|| {
        proxy_pool::list_fast_proxy_nodes(database, CHARITY_FAST_NODE_MAX_LATENCY_MS)
    }) {
        Ok(nodes) => nodes,
        Err(error) => {
            finish_charity_sync_log(
                app,
                database,
                log_id,
                source,
                stage,
                "failed",
                &error,
                "",
                duration_ms(),
                0,
                0,
                0,
            );
            return Err(error);
        }
    };
    if fast_nodes.is_empty() {
        let mut local = tokio::task::block_in_place(|| {
            load_feed_items_from_db(database, source, 0, CHARITY_PAGE_SIZE)
        })?;
        local.status = "skipped".into();
        local.skipped = true;
        local.message = format!("无 ≤{CHARITY_FAST_NODE_MAX_LATENCY_MS}ms 可用代理节点，本轮跳过");
        let _ = write_feed_sync_meta(database, source.id, "skipped", &local.message, "", 0);
        finish_charity_sync_log(
            app,
            database,
            log_id,
            source,
            stage,
            "skipped",
            &local.message,
            "",
            duration_ms(),
            0,
            0,
            local.unread_count,
        );
        return Ok(local);
    }

    // 节点轮询：全局游标每次同步递增，从不同偏移开始尝试，避免连续同步总命中同一个最快节点。
    let fast_nodes = {
        let monitor = app.state::<CharityMonitorRuntime>();
        let offset = monitor.node_round_robin.fetch_add(1, Ordering::Relaxed);
        rotate_fast_nodes(&fast_nodes, offset)
    };

    // 一次性把全部快节点装入内核，之后每个节点只做 API 出口切换，
    // 不再“每试一个节点就重启一次 Mihomo”，避免长时间占用代理内核。
    let fast_node_ids = fast_nodes
        .iter()
        .map(|(id, _, _)| id.clone())
        .collect::<Vec<_>>();
    if let Err(error) =
        proxy_pool::prepare_proxy_nodes_transient(database, runtime, &fast_node_ids).await
    {
        let message = format!("装载快节点失败：{error}");
        finish_charity_sync_log(
            app,
            database,
            log_id,
            source,
            stage,
            "failed",
            &message,
            "",
            duration_ms(),
            0,
            0,
            0,
        );
        return Err(message);
    }

    let mut errors = Vec::new();
    for (node_id, node_name, latency_ms) in &fast_nodes {
        if cancellation.is_cancelled() {
            let _ = proxy_pool::restore_proxy_node_transient(database, runtime).await;
            return Err(finish_cancelled_feed_sync(
                app,
                database,
                log_id,
                source,
                stage,
                duration_ms(),
            ));
        }
        if let Err(error) =
            proxy_pool::select_proxy_node_transient(database, runtime, node_id).await
        {
            errors.push(format!("{node_name}: 切换代理失败：{error}"));
            continue;
        }
        // 给运行时一点时间应用出口；等待期间也要响应取消。
        tokio::select! {
            _ = cancellation.cancelled() => {
                let _ = proxy_pool::restore_proxy_node_transient(database, runtime).await;
                return Err(finish_cancelled_feed_sync(
                    app, database, log_id, source, stage, duration_ms(),
                ));
            }
            _ = tokio::time::sleep(Duration::from_millis(120)) => {}
        }
        let fetch_result = tokio::select! {
            _ = cancellation.cancelled() => {
                let _ = proxy_pool::restore_proxy_node_transient(database, runtime).await;
                return Err(finish_cancelled_feed_sync(
                    app, database, log_id, source, stage, duration_ms(),
                ));
            }
            result = fetch_topic_body(app, client.clone(), source) => result,
        };
        match fetch_result {
            Ok((body, profile_name, account_name)) => match items_from_topic_list(&body) {
                Ok(items) => {
                    let persist_result = tokio::task::block_in_place(|| {
                        persist_feed(database, source, items, profile_name, account_name)
                    });
                    match persist_result {
                        Ok(mut result) => {
                            result.used_node_id = node_id.clone();
                            result.used_node_name = node_name.clone();
                            result.status = "success".into();
                            result.message = format!("已通过 {node_name}（{latency_ms}ms）同步");
                            let _ = write_feed_sync_meta(
                                database,
                                source.id,
                                "success",
                                &result.message,
                                node_name,
                                result.updated_count + result.new_count,
                            );
                            let _ =
                                proxy_pool::restore_proxy_node_transient(database, runtime).await;
                            finish_charity_sync_log(
                                app,
                                database,
                                log_id,
                                source,
                                stage,
                                "success",
                                &result.message,
                                node_name,
                                duration_ms(),
                                result.new_count,
                                result.updated_count,
                                result.unread_count,
                            );
                            return Ok(result);
                        }
                        Err(error) => errors.push(format!("{node_name}: 入库失败：{error}")),
                    }
                }
                Err(error) => errors.push(format!("{node_name}: {error}")),
            },
            Err(error) => errors.push(format!("{node_name}: {error}")),
        }
    }

    let _ = proxy_pool::restore_proxy_node_transient(database, runtime).await;
    let mut local = tokio::task::block_in_place(|| {
        load_feed_items_from_db(database, source, 0, CHARITY_PAGE_SIZE)
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
    local.message = errors
        .last()
        .cloned()
        .unwrap_or_else(|| format!("{} 同步失败：快节点均不可用", source.name));
    let last_node = errors
        .iter()
        .rev()
        .find_map(|error| {
            error
                .split_once(':')
                .map(|(node, _)| node.trim().to_string())
        })
        .unwrap_or_default();
    let _ = write_feed_sync_meta(database, source.id, "error", &local.message, "", 0);
    finish_charity_sync_log(
        app,
        database,
        log_id,
        source,
        stage,
        "failed",
        &local.message,
        &last_node,
        duration_ms(),
        0,
        0,
        local.unread_count,
    );
    Err(local.message.clone())
}

#[tauri::command]
pub async fn get_charity_feed(
    database: State<'_, Database>,
    runtime: State<'_, CharityMonitorRuntime>,
    feed_id: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<CharityFeedResult, String> {
    let source = charity_feed_source(feed_id.as_deref().unwrap_or(DEFAULT_CHARITY_FEED_ID))?;
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(CHARITY_PAGE_SIZE);
    // 读库离开 async worker，避免同步命令/锁拖住运行时。
    let mut result =
        tokio::task::block_in_place(|| load_feed_items_from_db(&database, source, offset, limit))?;
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
    let source = charity_feed_source(feed_id.as_deref().unwrap_or(DEFAULT_CHARITY_FEED_ID))?;
    tokio::task::block_in_place(|| {
        let keys = feed_meta_keys(source.id);
        let read_key = keys.read_at;
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
        write_app_meta(&database, &read_key, &now)?;
        unread_count_for_feed(&database, source.id)
    })
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
            load_feed_items_from_db(&database, source, 0, CHARITY_PAGE_SIZE)
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
    let sync_result =
        sync_feed_with_fast_nodes(&app, &database, &runtime, source, "manual", &cancellation).await;
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
        load_feed_items_from_db(&database, source, 0, CHARITY_PAGE_SIZE)
    })?;
    if let Err(error) = sync_result {
        if local.message.is_empty() {
            local.message = error;
            local.status = "error".into();
        }
    }
    Ok(local)
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
        // 短延迟：给代理 restore / 前端 force_round 一点时间；真正首轮由 force 或可见性触发。
        tokio::time::sleep(Duration::from_secs(1)).await;
        loop {
            let monitor = app.state::<CharityMonitorRuntime>();
            let visible = monitor.visible.load(Ordering::Relaxed);
            // 注意：只有真正开始同步才消费 force_round，避免同步中请求被 swap 掉后丢失。
            let force = monitor.force_round.load(Ordering::Relaxed);
            if visible || force {
                let Some(cancellation) = monitor.try_begin_sync() else {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                };
                let _ = monitor.force_round.swap(false, Ordering::Relaxed);
                // 并行拉起全部标签任务；因 Mihomo 只有一个混合出口，
                // 各标签内部用 proxy_sync_lock 串行“切换节点 + 请求 + 恢复”，
                // 全程不写全局代理状态，也不做直连。
                let mut handles = Vec::with_capacity(CHARITY_FEEDS.len());
                for source in CHARITY_FEEDS {
                    let app = app.clone();
                    let source = *source;
                    let cancellation = cancellation.clone();
                    handles.push(tauri::async_runtime::spawn(async move {
                        let database = app.state::<Database>();
                        let runtime = app.state::<ProxyRuntime>();
                        let result = sync_feed_with_fast_nodes(
                            &app,
                            &database,
                            &runtime,
                            source,
                            "poll",
                            &cancellation,
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
                app.state::<CharityMonitorRuntime>().end_sync();
            }

            let wait = if app
                .state::<CharityMonitorRuntime>()
                .visible
                .load(Ordering::Relaxed)
            {
                CHARITY_POLL_INTERVAL
            } else {
                CHARITY_HIDDEN_POLL_INTERVAL
            };
            let steps = (wait.as_secs().max(1)) as usize;
            for _ in 0..steps {
                tokio::time::sleep(Duration::from_secs(1)).await;
                if app
                    .state::<CharityMonitorRuntime>()
                    .force_round
                    .load(Ordering::Relaxed)
                {
                    break;
                }
            }
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
