use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, time::Duration};
use tauri::{Manager, State};

use crate::chrome_session;
use crate::db::*;
use crate::models::*;

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
            (!name.is_empty()).then(|| (id, name.to_string()))
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
            let author = topic
                .get("posters")
                .and_then(serde_json::Value::as_array)
                .and_then(|posters| posters.first())
                .and_then(|poster| poster.get("user_id"))
                .and_then(serde_json::Value::as_u64)
                .and_then(|user_id| users.get(&user_id).cloned())
                .unwrap_or_default();
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
    let mut request = client
        .get(source.json_url)
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
    database: &Database,
    source: CharityFeedSource,
) -> Result<(String, String, String), String> {
    let client = build_http_client(database, Duration::from_secs(8), 5, "Linux.do 标签请求")?;
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
    let initialized_key = format!("charity_feed_initialized:{}", source.id);
    let source_key = format!("charity_feed_source_url:{}", source.id);
    let fetched_key = format!("charity_feed_last_fetched_at:{}", source.id);
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
                 (feed_id, guid, title, link, author, published_at, summary, categories, first_seen_at, last_seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
                 ON CONFLICT(feed_id, guid) DO UPDATE SET
                   title = excluded.title,
                   link = excluded.link,
                   author = excluded.author,
                   published_at = excluded.published_at,
                   summary = excluded.summary,
                   categories = excluded.categories,
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
                ],
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
    })
}

#[tauri::command]
pub async fn fetch_charity_feed(
    app: tauri::AppHandle,
    database: State<'_, Database>,
    feed_id: Option<String>,
) -> Result<CharityFeedResult, String> {
    let source = charity_feed_source(feed_id.as_deref().unwrap_or(DEFAULT_CHARITY_FEED_ID))?;
    let (topics_body, profile_name, account_name) =
        fetch_topic_body(&app, &database, source).await?;
    let items = items_from_topic_list(&topics_body)?;
    persist_feed(&database, source, items, profile_name, account_name)
}

#[cfg(test)]
mod tests {
    use super::*;

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
          "users":[{"id":7,"username":"user7"}],
          "topic_list":{"topics":[
            {"id":1,"title":"旧帖新回复","slug":"old","created_at":"2026-07-01T08:00:00.000Z","posters":[{"user_id":7}]},
            {"id":2,"title":"真正的新帖","slug":"new","created_at":"2026-08-03T02:00:00.000Z","excerpt":"<p>新帖摘要</p>"},
            {"id":3,"title":"最新主题","slug":"latest","created_at":"2026-08-04T01:00:00.000Z"}
          ]}
        }"#;
        let items = items_from_topic_list(topics).unwrap();
        assert_eq!(items[0].title, "最新主题");
        assert_eq!(items[1].title, "真正的新帖");
        assert_eq!(items[1].summary, "新帖摘要");
        assert_eq!(items[2].title, "旧帖新回复");
        assert_eq!(items[2].author, "user7");
    }
}
