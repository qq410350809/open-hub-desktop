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
/// 三路并行探测的预算：
/// - 延迟 = 控制器 delay 接口（unified-delay 面板口径），mihomo 侧 query
///   超时同此值（客户端再放宽 2s 收响应）；
/// - 网速 = 经 lane 的真实流式下载，硬超时（含建链）须覆盖"经节点建链 +
///   TLS 握手 + 请求头 + 采样窗"全链路，实测 400ms 级 RTT 节点的响应头
///   到达需 5.3s，5s 会把大量可用节点误杀，放宽到 8s；
/// - 出口 IP 回显 = 经 lane 抓取出口公网 IP（`LANE_DELAY_TIMEOUT_MS`），
///   仅用于落库纠错国家分组，不影响节点判定。
pub const SPEED_TEST_TIMEOUT_MS: u64 = 8000;
/// 出口 IP 回显抓取（国家分组纠错）的独立预算
pub const LANE_DELAY_TIMEOUT_MS: u64 = 5000;
/// 出口 IP 回显服务（按优先级依次尝试）：全部纯 HTTP（无 TLS 握手，延迟
/// 数值明显低于 HTTPS 版本）且强制/仅回 IPv4，响应体为纯文本 IP。
/// 节点侧超时不换服务（换了也不通），服务侧异常才降级；
/// 若个别服务 301 跳转 HTTPS，reqwest 会跟随，仅该次略慢不影响结果。
pub const PROXY_IP_ECHO_URLS: [&str; 3] = [
    "http://api4.ipify.org",
    "http://ipv4.icanhazip.com",
    "http://api-ipv4.ip.sb/ip",
];
/// 下载测速的流式采样目标：收满 500KB 即停（也作为测速 URL 的 bytes 参数）。
/// 批量测速对带宽友好：8 lane 并行时每个节点最多收 500KB，全量测速的
/// 总下载量从 GB 级降到百 MB 级，不再挤占节点正常使用带宽。
pub const SPEED_TEST_TARGET_BYTES: u64 = 500_000;
/// 传输采样窗口：从首字节到达起算，1 秒内没收满 500KB 就主动断开——
/// 已收到的标本对峰值桶计量已经足够，慢节点不再拖到整体超时，
/// 批量测速的每节点下载段耗时封顶约 TTFB + 1s
pub const SPEED_TEST_TRANSFER_WINDOW_MS: u64 = 1000;
/// 吞吐计量的时间桶宽度：从首字节起按 50ms 分桶，取字节最多的单桶为峰值。
/// 50ms 桶在 10MB/s 下每桶约 500KB，既平滑 chunk 级调度突发又足够细，
/// 单桶满速即代表链路稳态能力（TCP 慢启动的低速首桶自然被峰值桶覆盖）。
pub const SPEED_TEST_PEAK_BUCKET_MS: u64 = 50;
/// 网速有效的最小总采样量：总量不足视为无有效吞吐（节点近乎不可用）
pub const SPEED_TEST_MIN_SAMPLE_BYTES: u64 = 32_000;
/// 网速换算基准：channel_latency_ms 语义保持"等效下载 500KB 耗时 ms"，
/// 由峰值桶速率外推（500KB ÷ 桶速率）；前端 MB/s 换算与通道候选门槛
/// 均基于该基准，无需随采样目标变化。
pub const SPEED_TEST_REF_BYTES: u64 = 500_000;
pub const CHANNEL_SPEED_TEST_URL: &str = "https://speed.cloudflare.com/__down?bytes=500000";
/// 通道候选节点的网速门槛：channel_latency_ms 现在是真实下载 500KB 的总耗时（ms）
pub const CHANNEL_MAX_DOWNLOAD_MS: i64 = 1500;
pub const BATCH_PROXY_TEST_CONCURRENCY: usize = 24;
#[allow(dead_code)]
pub const BATCH_PROXY_TEST_NODE_CHUNK: usize = 5000;
/// 账号出口候选的连通门槛。latency_ms 来自控制器 delay 接口
/// （unified-delay 双测取小，传统面板口径），500ms 即旧有语义。
/// 超过此门槛的候选由 channel_candidate_nodes 的 2000ms 档兜底。
pub const ACCOUNT_PROXY_MAX_LATENCY_MS: i64 = 500;
/// 单次代理池请求的最大轮候次数：必须足够让走死链节点（gstatic 通但目标站不通）
/// 也能在一次同步里触达活的节点。2 次再遇到立体感悬挂场景根本顶不住。
pub const ACCOUNT_PROXY_MAX_ATTEMPTS: usize = 4;
pub const ACCOUNT_PROXY_BAN_TIMEOUT: Duration = Duration::from_secs(30 * 60);
pub const ACCOUNT_PROXY_BAN_FORBIDDEN: Duration = Duration::from_secs(2 * 60 * 60);
pub const ACCOUNT_PROXY_BAN_UNREACHABLE: Duration = Duration::from_secs(2 * 60 * 60);
pub const ACCOUNT_PROXY_BAN_DEFAULT: Duration = Duration::from_secs(15 * 60);
/// 账号出口选点时控制器验活的探测目标与超时（对应 mihomo /delay 参数）。
pub(crate) const NODE_PROBE_URL: &str = "https://www.gstatic.com/generate_204";
pub(crate) const NODE_PROBE_TIMEOUT_MS: u64 = 3_000;
/// 探活失败的候选节点短封禁时长：避免同一死节点在每次 ensure 时反复实测，
/// 也给节点自愈留窗口（到期后重新参与验活）。
pub(crate) const ACCOUNT_NODE_PROBE_FAIL_TTL: Duration = Duration::from_secs(5 * 60);
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
