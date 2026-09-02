use super::*;
use crate::charity::feed::*;
use crate::charity::fetcher::*;
use crate::charity::scheduler::*;
use crate::proxypool;
use rusqlite::Connection;
use std::time::Duration;

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
    runtime.ban_node("", Duration::from_secs(60));
    assert!(!runtime.is_banned(""));
}

#[test]
fn http_403_is_forbidden_and_ejects_from_queue() {
    assert!(proxypool::is_http_forbidden_error(
        "公益站标签请求失败（HTTP 403）"
    ));
    assert!(proxypool::is_http_forbidden_error(
        "unexpected status 403 Forbidden"
    ));
    assert!(!proxypool::is_http_forbidden_error("timeout after 8s"));
    assert_eq!(ban_ttl_for_error("HTTP 403"), CHARITY_BAN_FORBIDDEN);
    // 429 限流：短封禁，节点很快可重新入池
    assert_eq!(
        ban_ttl_for_error("公益推广标签请求失败（HTTP 429，HTTP/2.0）"),
        CHARITY_BAN_RATE_LIMITED
    );
    assert!(proxypool::is_transport_error(
        "error sending request for url (https://linux.do/tag/1980)"
    ));
    assert!(proxypool::is_transport_error("connection reset by peer"));
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
    queue.push_back_if_absent(first);
    assert_eq!(queue.pop_front().unwrap().id, "b");
    assert_eq!(queue.pop_front().unwrap().id, "a");
    assert!(queue.is_empty());
}

#[test]
fn schedules_every_five_minutes_on_the_clock() {
    assert_eq!(seconds_until_next_aligned_run(12, 3, 10, 5), 110);
    assert_eq!(seconds_until_next_aligned_run(12, 5, 0, 5), 300);
    assert_eq!(seconds_until_next_aligned_run(12, 5, 1, 5), 299);
    assert_eq!(seconds_until_next_aligned_run(12, 4, 59, 5), 1);
    assert_eq!(seconds_until_next_aligned_run(12, 0, 0, 5), 300);
    assert_eq!(seconds_until_next_aligned_run(12, 59, 50, 5), 10);
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
    assert_eq!(rotate_fast_nodes(&nodes, 3)[0].0, "a");
    assert_eq!(rotate_fast_nodes(&nodes, 4)[0].0, "b");
    assert!(rotate_fast_nodes(&[], 0).is_empty());
}

fn node_refs(ids: &[&str]) -> Vec<CharityNodeRef> {
    ids.iter()
        .map(|id| CharityNodeRef {
            id: id.to_string(),
            name: id.to_string(),
            latency_ms: 100,
        })
        .collect()
}

#[test]
fn sticky_ordering_keeps_preferred_node_first_without_rotating() {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering as AtomicOrdering;
    use std::sync::{Arc, Mutex};

    let counter = AtomicUsize::new(0);
    // 有粘性节点：排最前，不消耗轮换计数
    let ordered = order_nodes_sticky(node_refs(&["a", "b", "c"]), Some("b"), &counter);
    assert_eq!(
        ordered.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
        ["b", "a", "c"]
    );
    assert_eq!(counter.load(AtomicOrdering::Relaxed), 0);

    // 再来一轮：粘性节点仍然最前，顺序稳定（不再轮换）
    let ordered = order_nodes_sticky(node_refs(&["a", "b", "c"]), Some("b"), &counter);
    assert_eq!(ordered[0].id, "b");

    // 粘性节点不在候选（被剔除/掉出名单）：回退到轮换
    let ordered = order_nodes_sticky(node_refs(&["a", "b", "c"]), Some("gone"), &counter);
    assert_eq!(ordered[0].id, "a");
    assert_eq!(counter.load(AtomicOrdering::Relaxed), 1);

    // 无粘性节点：按轮换偏移取队首
    let ordered = order_nodes_sticky(node_refs(&["a", "b", "c"]), None, &counter);
    assert_eq!(ordered[0].id, "b");
}

#[test]
fn ejecting_node_clears_sticky_preference_and_queue() {
    use std::sync::{Arc, Mutex};

    let runtime = CharityMonitorRuntime::new();
    runtime.set_preferred_node("n1");
    let queue = Arc::new(Mutex::new(CharityNodeQueue::from_nodes(node_refs(&[
        "n1", "n2",
    ]))));
    let node = CharityNodeRef {
        id: "n1".into(),
        name: "N1".into(),
        latency_ms: 100,
    };
    eject_node_from_charity_candidate(&runtime, &queue, &node, "HTTP 403");
    assert_eq!(runtime.preferred_node(), None);
    assert!(runtime.is_banned("n1"));
    assert_eq!(queue.lock().unwrap().nodes.len(), 1);
    assert_eq!(queue.lock().unwrap().nodes[0].id, "n2");
}

#[test]
fn sticky_node_is_reused_without_leaving_the_queue() {
    use std::sync::{Arc, Mutex};

    let runtime = CharityMonitorRuntime::new();
    runtime.set_preferred_node("n1");
    let queue = Arc::new(Mutex::new(CharityNodeQueue::from_nodes(node_refs(&[
        "n1", "n2",
    ]))));
    let first = take_attempt_node(&runtime, &queue).unwrap();
    assert_eq!(first.id, "n1");
    // 粘性节点不出队，可被下一轮/并行 feed 继续复用
    assert_eq!(queue.lock().unwrap().nodes.len(), 2);
    let again = take_attempt_node(&runtime, &queue).unwrap();
    assert_eq!(again.id, "n1");

    // 归还不重复入队
    queue.lock().unwrap().push_back_if_absent(first);
    assert_eq!(queue.lock().unwrap().nodes.len(), 2);

    // 粘性节点失效后：取队首新节点
    runtime.clear_preferred_node("n1");
    let next = take_attempt_node(&runtime, &queue).unwrap();
    assert_eq!(next.id, "n1"); // 队首仍是 n1（未被出队）
    assert_eq!(queue.lock().unwrap().nodes.len(), 1);
}

#[test]
fn charity_feed_source_url_pattern() {
    assert_eq!(
        charity_tag_json_url("1515"),
        "https://linux.do/tag/1515/l/latest.json?order=created"
    );
}

#[test]
fn removes_source_with_orphan_posts_and_keeps_shared_ones() {
    let connection = Connection::open_in_memory().unwrap();
    crate::db::ensure_charity_feed_sources_table(&connection).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE app_meta (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE charity_feed_items (
                 feed_id TEXT NOT NULL,
                 guid TEXT NOT NULL,
                 title TEXT NOT NULL,
                 link TEXT NOT NULL,
                 PRIMARY KEY (feed_id, guid)
             );
             INSERT INTO charity_feed_items (feed_id, guid, title, link) VALUES
               ('1515', 'topic-1', '仅公益推广', 'https://linux.do/t/a/1'),
               ('1515', 'topic-2', '双标签共有', 'https://linux.do/t/b/2'),
               ('1980', 'topic-2', '双标签共有', 'https://linux.do/t/b/2');
             INSERT INTO app_meta (key, value) VALUES
               ('charity_feed_initialized:1515', '1'),
               ('charity_feed_last_read_at:1515', '2026-01-01T00:00:00Z');",
        )
        .unwrap();
    let removed = db::remove_charity_source_db(&connection, "1515").unwrap();
    assert_eq!(removed, 1);
    let source_gone: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM charity_feed_sources WHERE id = '1515'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(source_gone, 0);
    let orphan_gone: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM charity_feed_items WHERE guid = 'topic-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(orphan_gone, 0);
    let shared_kept: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM charity_feed_items WHERE guid = 'topic-2' AND feed_id = '1980'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(shared_kept, 1);
    let meta_cleaned: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM app_meta WHERE key LIKE 'charity_feed_%:1515'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(meta_cleaned, 0);
}

#[test]
fn seeds_charity_feed_sources_table_once() {
    let connection = Connection::open_in_memory().unwrap();
    crate::db::ensure_charity_feed_sources_table(&connection).unwrap();
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM charity_feed_sources", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 6);
    let first: String = connection
        .query_row(
            "SELECT id FROM charity_feed_sources ORDER BY sort_order LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(first, "1515");
    crate::db::ensure_charity_feed_sources_table(&connection).unwrap();
    let again: i64 = connection
        .query_row("SELECT COUNT(*) FROM charity_feed_sources", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(again, 6);
}

#[test]
fn migrates_legacy_tag_json_urls_only_for_generated_addresses() {
    let connection = Connection::open_in_memory().unwrap();
    crate::db::ensure_charity_feed_sources_table(&connection).unwrap();
    connection
        .execute_batch(
            "UPDATE charity_feed_sources
             SET json_url = 'https://linux.do/tag/' || id || '-tag/' || id
                 || '.json?order=created&ascending=false'
             WHERE id IN ('1515', '1980');
             UPDATE charity_feed_sources
             SET json_url = 'https://linux.do/tag/custom-tag/custom.json?order=created&ascending=false'
             WHERE id = '2233';",
        )
        .unwrap();
    crate::db::ensure_charity_feed_sources_table(&connection).unwrap();
    let migrated: String = connection
        .query_row(
            "SELECT json_url FROM charity_feed_sources WHERE id = '1515'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        migrated,
        "https://linux.do/tag/1515/l/latest.json?order=created"
    );
    let migrated_b: String = connection
        .query_row(
            "SELECT json_url FROM charity_feed_sources WHERE id = '1980'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        migrated_b,
        "https://linux.do/tag/1980/l/latest.json?order=created"
    );
    let untouched: String = connection
        .query_row(
            "SELECT json_url FROM charity_feed_sources WHERE id = '2233'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        untouched,
        "https://linux.do/tag/custom-tag/custom.json?order=created&ascending=false"
    );
}

#[test]
fn combined_filter_url_encodes_tag_names_as_or_query() {
    let url = combined_filter_json_url(&["公益推广".into(), "公益站".into(), "中转站".into()]);
    assert!(url.starts_with("https://linux.do/filter.json?q=tag%3A"));
    assert!(url.contains("%E5%85%AC%E7%9B%8A%E6%8E%A8%E5%B9%BF"));
    assert!(url.contains("%2C"));
    assert!(url.contains("order%3Acreated"));
    // 时间戳防缓存参数由 request_topic_list 追加，不在这里
    assert!(!url.contains("&t="));
}

#[test]
fn split_items_attributes_topics_by_tag_name() {
    let sources = vec![
        CharityFeedSource {
            id: "1515".into(),
            name: "公益推广".into(),
            json_url: charity_tag_json_url("1515"),
            enabled: true,
            sort_order: 1,
        },
        CharityFeedSource {
            id: "1980".into(),
            name: "公益站".into(),
            json_url: charity_tag_json_url("1980"),
            enabled: true,
            sort_order: 2,
        },
    ];
    let make_item = |id: &str, tags: &[&str]| CharityFeedItem {
        id: id.into(),
        title: id.into(),
        link: String::new(),
        author: String::new(),
        published_at: String::new(),
        summary: String::new(),
        categories: tags.iter().map(|t| t.to_string()).collect(),
        feed_ids: Vec::new(),
        feed_names: Vec::new(),
        is_new: false,
        reply_count: 0,
        views: 0,
        like_count: 0,
        last_activity_at: String::new(),
        pinned: false,
        posters: Vec::new(),
    };
    let items = vec![
        make_item("a", &["公益推广", "ChatGPT"]),
        make_item("b", &["公益站"]),
        make_item("c", &["中转站"]),
    ];
    let split = split_items_by_feed(&items, &sources);
    assert_eq!(split.len(), 2);
    assert_eq!(split[0].0, "1515");
    assert_eq!(
        split[0].1.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
        ["a"]
    );
    assert_eq!(split[1].0, "1980");
    assert_eq!(
        split[1].1.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
        ["b"]
    );
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

#[test]
fn parses_object_shaped_tags_from_latest_json() {
    let topics = r#"{
          "users":[
            {"id":44468,"username":"poster44468","avatar_template":"/user_avatar/linux.do/poster44468/{size}/1_2.png"},
            {"id":367752,"username":"spikevision","avatar_template":"/user_avatar/linux.do/spikevision/{size}/2_3.png"}
          ],
          "topic_list":{"topics":[
            {
              "fancy_title": "应该是最后一次送plus额度了",
              "id": 2830779,
              "title": "应该是最后一次送plus额度了",
              "slug": "topic",
              "posts_count": 7,
              "reply_count": 3,
              "created_at": "2026-08-30T02:50:30.193Z",
              "last_posted_at": "2026-08-30T03:01:15.977Z",
              "bumped_at": "2026-08-30T03:01:15.977Z",
              "pinned": false,
              "pinned_globally": false,
              "views": 296,
              "like_count": 7,
              "tags": [
                {"id": 3, "name": "ChatGPT", "slug": "chatgpt"},
                {"id": 1515, "name": "公益推广", "slug": "1515-tag"},
                {"id": 2725, "name": "福利羊毛", "slug": "2725-tag"},
                {"id": 2567, "name": "PLUS", "slug": "2567-tag"}
              ],
              "posters": [
                {"extras": null, "description": "原始发帖人", "user_id": 44468},
                {"extras": "latest", "description": "最新发帖人", "user_id": 367752}
              ]
            }
          ]}
        }"#;
    let items = items_from_topic_list(topics).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "应该是最后一次送plus额度了");
    assert_eq!(
        items[0].categories,
        vec!["ChatGPT", "PLUS", "公益推广", "福利羊毛"]
    );
    assert_eq!(items[0].author, "poster44468");
    assert_eq!(items[0].like_count, 7);
    assert_eq!(items[0].views, 296);
    // 逐字段核对：除 tags 为对象数组外，其余字段照常提取
    assert_eq!(items[0].id, "topic-2830779");
    assert_eq!(items[0].link, "https://linux.do/t/topic/2830779");
    assert_eq!(items[0].published_at, "2026-08-30T02:50:30.193Z");
    assert_eq!(items[0].last_activity_at, "2026-08-30T03:01:15.977Z");
    assert_eq!(items[0].reply_count, 3);
    assert!(!items[0].pinned);
    assert_eq!(items[0].posters.len(), 2);
}
