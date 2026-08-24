use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::fs;
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
pub const BATCH_PROXY_TEST_TIMEOUT_MS: &str = "5000";
pub const BATCH_PROXY_TEST_CONCURRENCY: usize = 24;
#[allow(dead_code)]
pub const BATCH_PROXY_TEST_NODE_CHUNK: usize = 5000;
pub const ACCOUNT_PROXY_MAX_LATENCY_MS: i64 = 500;
pub const ACCOUNT_PROXY_MAX_ATTEMPTS: usize = 2;
pub const ACCOUNT_PROXY_BAN_TIMEOUT: Duration = Duration::from_secs(30 * 60);
pub const ACCOUNT_PROXY_BAN_FORBIDDEN: Duration = Duration::from_secs(2 * 60 * 60);
pub const ACCOUNT_PROXY_BAN_UNREACHABLE: Duration = Duration::from_secs(2 * 60 * 60);
pub const ACCOUNT_PROXY_BAN_DEFAULT: Duration = Duration::from_secs(15 * 60);
pub const DEFAULT_PROXY_CHANNEL_ID: &str = "default";
pub const DEFAULT_PROXY_CHANNEL_NAME: &str = "默认通道";
pub const CHANNEL_SPEED_TEST_URL: &str = "https://speed.cloudflare.com/__down?bytes=500000";

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

pub struct ProxyRuntime {
    pub directory: PathBuf,
    pub shared_instance: Mutex<InstanceState>,
    pub channel_instances: Mutex<HashMap<String, InstanceState>>,
    pub account_instances: Mutex<HashMap<String, InstanceState>>,
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

pub struct TemporaryRuntimeDirectory(pub PathBuf);

impl Drop for TemporaryRuntimeDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
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
            channel_instances: Mutex::new(HashMap::new()),
            account_instances: Mutex::new(HashMap::new()),
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
        let instances = self.channel_instances.lock().ok()?;
        let inst = instances.get(channel_id)?;
        (inst.proxy_port > 0).then_some(inst.proxy_port)
    }

    pub fn channel_proxy_url(&self, channel_id: &str) -> Option<String> {
        let port = self.channel_port(channel_id)?;
        Some(format!("http://127.0.0.1:{port}"))
    }

    #[allow(dead_code)]
    pub fn account_port(&self, profile_id: &str) -> Option<u16> {
        let instances = self.account_instances.lock().ok()?;
        let inst = instances.get(profile_id)?;
        (inst.proxy_port > 0).then_some(inst.proxy_port)
    }

    #[allow(dead_code)]
    pub fn account_proxy_url(&self, profile_id: &str) -> Option<String> {
        let port = self.account_port(profile_id)?;
        Some(format!("http://127.0.0.1:{port}"))
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
        if let Ok(mut map) = self.channel_instances.lock() {
            for (_, mut inst) in map.drain() {
                stop_single_instance(&mut inst);
            }
        }
        if let Ok(mut map) = self.account_instances.lock() {
            for (_, mut inst) in map.drain() {
                stop_single_instance(&mut inst);
            }
        }
    }
}
