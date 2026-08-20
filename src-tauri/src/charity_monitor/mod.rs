pub mod commands;
pub mod db;
pub mod feed;
pub mod fetcher;
pub mod scheduler;
pub mod types;

pub use commands::*;
pub use scheduler::start_charity_monitor;
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::charity_monitor::feed::*;
    use crate::charity_monitor::fetcher::*;
    use crate::charity_monitor::scheduler::*;
    use crate::proxy_pool;
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
        assert!(proxy_pool::is_http_forbidden_error("公益站标签请求失败（HTTP 403）"));
        assert!(proxy_pool::is_http_forbidden_error("unexpected status 403 Forbidden"));
        assert!(!proxy_pool::is_http_forbidden_error("timeout after 8s"));
        assert_eq!(ban_ttl_for_error("HTTP 403"), CHARITY_BAN_FORBIDDEN);
        assert!(proxy_pool::is_transport_error(
            "error sending request for url (https://linux.do/tag/1980)"
        ));
        assert!(proxy_pool::is_transport_error("connection reset by peer"));
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

    #[test]
    fn charity_feed_source_url_pattern() {
        let id = "1515";
        let url = format!("https://linux.do/tag/{id}-tag/{id}.json?order=created&ascending=false");
        assert!(url.contains(id));
        assert_eq!(charity_tag_json_url("1980"), url.replace("1515", "1980"));
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
