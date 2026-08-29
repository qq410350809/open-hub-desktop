use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Child;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

pub const RUNTIME_SECRET: &str = "openhub-local-proxy-runtime";
pub const RUNTIME_GROUP: &str = "OpenHub";
/// 下载测速并行的 lane 数：lane 间并行，lane 内串行切换节点
pub const SPEED_TEST_LANES: usize = 8;
/// 全局单实例预配的账号 lane 池上限：未绑定通道的账号各占一个 lane
pub const ACCOUNT_LANE_POOL: usize = 64;
/// 全局单实例预配的通道 lane 池上限：每个通道占一个 lane
pub const CHANNEL_LANE_POOL: usize = 16;
/// 一体探测的单次硬超时（延时+网速同一条连接）：响应头到达 = 延迟值，
/// 超时前没收到响应头 → 两指标一起判死。预算须覆盖"经节点建链 + TLS 握手 +
/// 请求头"全链路，实测 400ms 级 RTT 节点的 TTFB 可达 5.3s，5s 预算会把大量
/// 可用节点误杀，放宽到 8s。
pub const SPEED_TEST_TIMEOUT_MS: u64 = 8000;
/// 下载测速的流式采样目标：收满即停（也作为测速 URL 的 bytes 参数）。
/// 500KB 样本太小——TCP 慢启动未爬到满速就结束，实测会把 6.5MB/s 的节点
/// 测成 0.8MB/s（差 8 倍）；大样本才能测出真实吞吐。
pub const SPEED_TEST_TARGET_BYTES: u64 = 10_000_000;
/// 网速的传输窗口：从响应头到达起算，超过即停止采样。
/// 并行 lane 共享总带宽，窗口越长互相争抢越久、平均速率被压得越低；
/// 短窗口 + 峰值统计只需捕捉稳态 burst，无需下载完整大文件。
pub const SPEED_TEST_TRANSFER_WINDOW_MS: u64 = 900;
/// 峰值吞吐的时间桶宽度：100ms 足以平滑 chunk 级调度突发（10ms 桶会被
/// 单个大 chunk / 本地缓冲排空灌出虚高峰值），又能在窗口内留出多个采样桶
pub const SPEED_TEST_PEAK_BUCKET_MS: u64 = 100;
/// 峰值统计的连续桶数：取最大连续 3 桶（300ms 滑窗）平均速率，
/// 单桶突发被摊薄，峰值更接近可持续的稳态吞吐
pub const SPEED_TEST_PEAK_WINDOW_BUCKETS: u64 = 3;
/// 网速有效的最小总采样量：窗口内实收不足视为无有效吞吐（节点近乎不可用）
pub const SPEED_TEST_MIN_SAMPLE_BYTES: u64 = 32_000;
/// 网速等效换算基准：channel_latency_ms 语义保持"等效下载 500KB 耗时 ms"，
/// 前端 MB/s 换算与通道候选门槛均基于该基准，无需随采样目标变化。
pub const SPEED_TEST_REF_BYTES: u64 = 500_000;
pub const CHANNEL_SPEED_TEST_URL: &str = "https://speed.cloudflare.com/__down?bytes=10000000";
/// 通道候选节点的网速门槛：channel_latency_ms 现在是真实下载 500KB 的总耗时（ms）
pub const CHANNEL_MAX_DOWNLOAD_MS: i64 = 1500;
pub const BATCH_PROXY_TEST_CONCURRENCY: usize = 24;
#[allow(dead_code)]
pub const BATCH_PROXY_TEST_NODE_CHUNK: usize = 5000;
/// 账号出口候选的连通门槛。latency_ms 来自控制器 delay 接口
/// （unified-delay 双测取小，传统面板口径），500ms 即旧有语义。
/// 超过此门槛的候选由 channel_candidate_nodes 的 2000ms 档兜底。
pub const ACCOUNT_PROXY_MAX_LATENCY_MS: i64 = 500;
pub const ACCOUNT_PROXY_MAX_ATTEMPTS: usize = 2;
pub const ACCOUNT_PROXY_BAN_TIMEOUT: Duration = Duration::from_secs(30 * 60);
pub const ACCOUNT_PROXY_BAN_FORBIDDEN: Duration = Duration::from_secs(2 * 60 * 60);
pub const ACCOUNT_PROXY_BAN_UNREACHABLE: Duration = Duration::from_secs(2 * 60 * 60);
pub const ACCOUNT_PROXY_BAN_DEFAULT: Duration = Duration::from_secs(15 * 60);
pub const DEFAULT_PROXY_CHANNEL_ID: &str = "default";
pub const DEFAULT_PROXY_CHANNEL_NAME: &str = "默认通道";

#[derive(Debug, Clone)]
pub struct ParsedNode {
    pub id: String,
    pub name: String,
    pub proxy_type: String,
    pub server: String,
    pub port: i64,
    pub cipher: String,
    pub udp: bool,
    pub raw_json: JsonValue,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct RuntimeNode {
    pub id: String,
    pub config: JsonValue,
}

pub struct InstanceState {
    pub child: Option<Child>,
    pub directory: PathBuf,
    pub config_hash: String,
    pub engine_path: String,
    pub last_error: String,
    pub proxy_port: u16,
    pub controller_port: u16,
}

pub fn stop_single_instance(instance: &mut InstanceState) {
    if let Some(mut child) = instance.child.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

pub struct ActiveProxyTest {
    pub id: u64,
    pub cancellation: CancellationToken,
}

/// 全局单实例内的一个 lane：select 组 + 绑定该组的本地监听端口。
/// lane 的流量固定走组内当前选中节点，切换节点 = 控制器 PUT。
#[derive(Debug, Clone)]
pub struct LaneSlot {
    pub group_name: String,
    pub listen_port: u16,
}

pub struct ProxyRuntime {
    pub directory: PathBuf,
    pub shared_instance: Mutex<InstanceState>,
    /// 测速 lane 池（SPEED_TEST_LANES 个），批量/单节点测速独占
    pub speed_lane_slots: Mutex<Vec<LaneSlot>>,
    /// 账号 lane 池（ACCOUNT_LANE_POOL 个）：未绑定通道的账号各占一个 lane
    pub account_lane_slots: Mutex<Vec<LaneSlot>>,
    /// 通道 lane 池（CHANNEL_LANE_POOL 个）：每个通道占一个 lane
    pub channel_lane_slots: Mutex<Vec<LaneSlot>>,
    /// channel_id -> channel_lane_slots 下标
    pub channel_lane_map: Mutex<HashMap<String, usize>>,
    /// profile_id -> account_lane_slots 下标
    pub account_lane_map: Mutex<HashMap<String, usize>>,
    /// lane 组名 -> 当前选中节点 id（mihomo 代理名）。全局实例重启后清空。
    pub lane_selected: Mutex<HashMap<String, String>>,
    pub active_test: Mutex<Option<ActiveProxyTest>>,
    pub next_test_id: AtomicU64,
    pub runtime_op_lock: tokio::sync::Mutex<()>,
    #[allow(dead_code)]
    pub shared_pool_lock: tokio::sync::Mutex<()>,
    pub shared_pool_index: AtomicU64,
    /// 账号代理实例的节点分配游标：多账号按顺序轮询候选节点，避免全部集中到延迟最低的第一个
    pub account_alloc_seq: AtomicU64,
    pub account_ban_until: Mutex<HashMap<String, Instant>>,
}

pub struct ProxyTestLease<'a> {
    pub runtime: &'a ProxyRuntime,
    pub id: u64,
    pub cancellation: CancellationToken,
}

impl ProxyRuntime {
    pub fn new(directory: PathBuf) -> Self {
        Self::new_with_ports(directory, 0, 0)
    }

    pub fn new_with_ports(directory: PathBuf, proxy_port: u16, controller_port: u16) -> Self {
        let shared_dir = directory.join("shared");
        Self {
            directory,
            shared_instance: Mutex::new(InstanceState {
                child: None,
                directory: shared_dir,
                config_hash: String::new(),
                engine_path: String::new(),
                last_error: String::new(),
                proxy_port,
                controller_port,
            }),
            speed_lane_slots: Mutex::new(Vec::new()),
            account_lane_slots: Mutex::new(Vec::new()),
            channel_lane_slots: Mutex::new(Vec::new()),
            channel_lane_map: Mutex::new(HashMap::new()),
            account_lane_map: Mutex::new(HashMap::new()),
            lane_selected: Mutex::new(HashMap::new()),
            active_test: Mutex::new(None),
            next_test_id: AtomicU64::new(1),
            runtime_op_lock: tokio::sync::Mutex::new(()),
            shared_pool_lock: tokio::sync::Mutex::new(()),
            shared_pool_index: AtomicU64::new(0),
            account_alloc_seq: AtomicU64::new(0),
            account_ban_until: Mutex::new(HashMap::new()),
        }
    }

    pub fn channel_port(&self, channel_id: &str) -> Option<u16> {
        let map = self.channel_lane_map.lock().ok()?;
        let idx = *map.get(channel_id)?;
        let slots = self.channel_lane_slots.lock().ok()?;
        let lane = slots.get(idx)?;
        (lane.listen_port > 0).then_some(lane.listen_port)
    }

    /// 释放通道占用的 lane（删除通道时调用）；lane 槽位可被后续通道复用。
    pub fn release_channel_lane(&self, channel_id: &str) {
        let idx = self
            .channel_lane_map
            .lock()
            .ok()
            .and_then(|mut map| map.remove(channel_id));
        let Some(idx) = idx else { return };
        let group = self
            .channel_lane_slots
            .lock()
            .ok()
            .and_then(|slots| slots.get(idx).map(|lane| lane.group_name.clone()));
        if let Some(group) = group {
            if let Ok(mut selected) = self.lane_selected.lock() {
                selected.remove(&group);
            }
        }
    }

    /// 释放账号占用的 lane（账号删除/解绑通道且无其他引用时调用）。
    pub fn release_account_lane(&self, profile_id: &str) {
        let idx = self
            .account_lane_map
            .lock()
            .ok()
            .and_then(|mut map| map.remove(profile_id));
        let Some(idx) = idx else { return };
        let group = self
            .account_lane_slots
            .lock()
            .ok()
            .and_then(|slots| slots.get(idx).map(|lane| lane.group_name.clone()));
        if let Some(group) = group {
            if let Ok(mut selected) = self.lane_selected.lock() {
                selected.remove(&group);
            }
        }
    }

    /// 账号 lane 当前选中的节点 id（mihomo 代理名），用于轮换时排除当前节点。
    pub fn account_lane_current_node(&self, profile_id: &str) -> Option<String> {
        let idx = *self
            .account_lane_map
            .lock()
            .ok()?
            .get(profile_id)?;
        let group = self
            .account_lane_slots
            .lock()
            .ok()?
            .get(idx)?
            .group_name
            .clone();
        self.lane_selected.lock().ok()?.get(&group).cloned()
    }

    /// 第 i 个测速 lane（测速与批量测速经 active_test 互斥，可安全独占）。
    pub fn speed_lane_slot(&self, index: usize) -> Result<LaneSlot, String> {
        let slots = self
            .speed_lane_slots
            .lock()
            .map_err(|_| "测速 lane 状态锁定失败".to_string())?;
        slots
            .get(index)
            .cloned()
            .ok_or_else(|| "测速 lane 尚未就绪".to_string())
    }

    pub fn shared_proxy_url(&self) -> Option<String> {
        let state = self.shared_instance.lock().ok()?;
        if state.proxy_port > 0 {
            Some(format!("http://127.0.0.1:{}", state.proxy_port))
        } else {
            None
        }
    }

    pub fn start_proxy_test(&self) -> Result<ProxyTestLease<'_>, String> {
        let mut active = self
            .active_test
            .lock()
            .map_err(|_| "测速任务状态锁定失败")?;
        if active.is_some() {
            return Err("已有代理测速任务正在进行，请等待上一任务结束".into());
        }
        let id = self.next_test_id.fetch_add(1, Ordering::Relaxed);
        let cancellation = CancellationToken::new();
        *active = Some(ActiveProxyTest {
            id,
            cancellation: cancellation.clone(),
        });
        Ok(ProxyTestLease {
            runtime: self,
            id,
            cancellation,
        })
    }

    pub fn cancel_proxy_test(&self) -> Result<bool, String> {
        let active = self
            .active_test
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(test) = active.as_ref() else {
            return Ok(false);
        };
        test.cancellation.cancel();
        Ok(true)
    }

    pub fn purge_account_bans(&self) {
        let Ok(mut bans) = self.account_ban_until.lock() else {
            return;
        };
        let now = Instant::now();
        bans.retain(|_, until| *until > now);
    }

    pub fn account_node_is_banned(&self, node_id: &str) -> bool {
        if node_id.trim().is_empty() {
            return false;
        }
        let Ok(mut bans) = self.account_ban_until.lock() else {
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

    pub fn account_ban_node(&self, node_id: &str, ttl: Duration) {
        if node_id.trim().is_empty() {
            return;
        }
        if let Ok(mut bans) = self.account_ban_until.lock() {
            let until = Instant::now() + ttl;
            bans.insert(node_id.to_string(), until);
        }
    }
}

impl Drop for ProxyTestLease<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.runtime.active_test.lock() {
            if active.as_ref().is_some_and(|test| test.id == self.id) {
                *active = None;
            }
        }
    }
}

impl Drop for ProxyRuntime {
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared_instance.lock() {
            stop_single_instance(&mut state);
        }
    }
}
