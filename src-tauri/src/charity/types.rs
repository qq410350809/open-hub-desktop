use serde::Serialize;
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

pub const DEFAULT_CHARITY_FEED_ID: &str = "1515";
pub const CHARITY_FAST_NODE_MAX_LATENCY_MS: i64 = 500;
pub const CHARITY_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
pub const CHARITY_MAX_NODE_ATTEMPTS: usize = 5;
pub const CHARITY_PREPARE_NODE_LIMIT: usize = 40;
pub const CHARITY_BAN_TIMEOUT: Duration = Duration::from_secs(30 * 60);
pub const CHARITY_BAN_FORBIDDEN: Duration = Duration::from_secs(2 * 60 * 60);
pub const CHARITY_BAN_UNREACHABLE: Duration = Duration::from_secs(2 * 60 * 60);
pub const CHARITY_BAN_DEFAULT: Duration = Duration::from_secs(15 * 60);
pub const CHARITY_PAGE_SIZE: usize = 20;
pub const CHARITY_PAGE_LIMIT_MAX: usize = 2000;
pub const CHARITY_SCHEDULE_EVERY_MINUTES: u32 = 5;
pub const CHARITY_SCHEDULER_TICK: Duration = Duration::from_secs(1);
pub const CHARITY_SYNC_LOG_LIMIT: usize = 300;
pub const CHARITY_SYNC_CANCELLED_PREFIX: &str = "同步任务已取消";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CharityFeedSource {
    pub id: String,
    pub name: String,
    pub json_url: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub sort_order: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CharityFeedItem {
    pub id: String,
    pub title: String,
    pub link: String,
    pub author: String,
    pub published_at: String,
    pub summary: String,
    pub categories: Vec<String>,
    pub is_new: bool,
    #[serde(default)]
    pub reply_count: i64,
    #[serde(default)]
    pub views: i64,
    #[serde(default)]
    pub like_count: i64,
    #[serde(default)]
    pub last_activity_at: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub posters: Vec<String>,
    #[serde(default)]
    pub feed_ids: Vec<String>,
    #[serde(default)]
    pub feed_names: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CharityFeedResult {
    pub feed_id: String,
    pub feed_name: String,
    pub items: Vec<CharityFeedItem>,
    pub fetched_at: String,
    pub changed: bool,
    pub new_count: usize,
    pub updated_count: usize,
    pub initialized: bool,
    pub source_profile_name: String,
    pub source_account_name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub used_node_id: String,
    #[serde(default)]
    pub used_node_name: String,
    #[serde(default)]
    pub unread_count: usize,
    #[serde(default)]
    pub skipped: bool,
    #[serde(default)]
    pub total_count: usize,
    #[serde(default)]
    pub offset: usize,
    #[serde(default)]
    pub limit: usize,
    #[serde(default)]
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CharitySyncProgress {
    pub feed_id: String,
    pub feed_name: String,
    pub stage: String,
    pub status: String,
    pub message: String,
    pub used_node_id: String,
    pub used_node_name: String,
    pub new_count: usize,
    pub updated_count: usize,
    pub unread_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CharitySyncLogEntry {
    pub id: i64,
    pub at: String,
    pub feed_id: String,
    pub feed_name: String,
    pub stage: String,
    pub status: String,
    pub message: String,
    pub node_name: String,
    pub duration_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CharityRefreshAllResult {
    pub cancelled_active_round: bool,
    pub cancelled_log_count: usize,
    pub feed_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CharityProxyPoolSummary {
    pub valid_count: usize,
    pub candidate_count: usize,
}

#[derive(Debug, Clone)]
pub struct CharityNodeRef {
    pub id: String,
    pub name: String,
    pub latency_ms: i64,
}

#[derive(Debug, Default)]
pub struct CharityNodeQueue {
    pub nodes: VecDeque<CharityNodeRef>,
}

impl CharityNodeQueue {
    pub fn from_nodes(nodes: Vec<CharityNodeRef>) -> Self {
        Self {
            nodes: VecDeque::from(nodes),
        }
    }

    pub fn pop_front(&mut self) -> Option<CharityNodeRef> {
        self.nodes.pop_front()
    }

    pub fn push_back(&mut self, node: CharityNodeRef) {
        self.nodes.push_back(node);
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn remove_id(&mut self, node_id: &str) -> usize {
        let before = self.nodes.len();
        self.nodes.retain(|node| node.id != node_id);
        before.saturating_sub(self.nodes.len())
    }
}

pub struct CharityMonitorRuntime {
    pub running: AtomicBool,
    pub visible: AtomicBool,
    pub force_round: AtomicBool,
    pub syncing: AtomicBool,
    pub node_round_robin: AtomicUsize,
    pub active_sync_cancellation: Mutex<Option<CancellationToken>>,
    pub proxy_sync_lock: tokio::sync::Mutex<()>,
    pub last_errors: Mutex<HashMap<String, String>>,
    pub charity_ban_until: Mutex<HashMap<String, Instant>>,
}

impl CharityMonitorRuntime {
    pub fn new() -> Self {
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

    pub fn purge_expired_bans(&self) {
        let Ok(mut bans) = self.charity_ban_until.lock() else {
            return;
        };
        let now = Instant::now();
        bans.retain(|_, until| *until > now);
    }

    pub fn is_banned(&self, node_id: &str) -> bool {
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

    pub fn ban_node(&self, node_id: &str, ttl: Duration) {
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

    pub fn active_banned_ids(&self) -> Vec<String> {
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

    pub fn set_visible(&self, visible: bool) {
        self.visible.store(visible, Ordering::Relaxed);
    }

    pub fn request_round(&self) {
        self.force_round.store(true, Ordering::Relaxed);
    }

    pub fn try_begin_sync(&self) -> Option<CancellationToken> {
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

    pub fn cancel_active_sync(&self) -> bool {
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

    pub fn end_sync(&self) {
        if let Ok(mut active) = self.active_sync_cancellation.lock() {
            *active = None;
        }
        self.syncing.store(false, Ordering::SeqCst);
    }
}

pub struct CharityFeedMetaKeys {
    pub initialized: String,
    pub source_url: String,
    pub fetched_at: String,
    pub read_at: String,
    pub last_status: String,
    pub last_message: String,
    pub last_node: String,
    pub last_updated: String,
}

pub fn feed_meta_keys(feed_id: &str) -> CharityFeedMetaKeys {
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
