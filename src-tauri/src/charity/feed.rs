use crate::charity::types::CharityFeedItem;
use std::collections::HashMap;

pub fn plain_text(html: &str) -> String {
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

pub fn topic_id(value: &str) -> Option<u64> {
    value
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .and_then(|value| value.parse().ok())
}

pub fn charity_tag_json_url(tag_id: &str) -> String {
    format!("https://linux.do/tag/{tag_id}-tag/{tag_id}.json?order=created&ascending=false")
}

pub fn items_from_topic_list(value: &str) -> Result<Vec<CharityFeedItem>, String> {
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
