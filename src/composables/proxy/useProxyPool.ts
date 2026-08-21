import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { computed, ref } from "vue";
import type { GeoipDownloadProgress, GeoipStatus, MihomoDownloadProgress, MihomoKernelStatus, ProxyIpAnalysis, ProxyNode, ProxyNodeTestProgress, ProxyPoolRefreshResult, ProxyPoolState, ProxySourceProgress } from "../../types";
import { runCommand } from "../core/ipc";

const isTauri = "__TAURI_INTERNALS__" in window;

const emptyState = (): ProxyPoolState => ({
  subscriptions: [], nodes: [], channels: [], defaultChannelId: "default",
  activeNodeId: "", activeNode: null,
  enabled: false, ignoreAddresses: "localhost,127.0.0.1,::1,.local,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16",
  speedTestUrl: "http://www.gstatic.com/generate_204", runtimeAvailable: false,
  runtimePath: "", runtimeError: "", nodeCount: 0, subscriptionCount: 0,
  invalidNodeCount: 0,
});

const proxyPool = ref<ProxyPoolState>(emptyState());
const proxyPoolLoading = ref(false);
const proxyPoolError = ref("");
const proxyPoolBusyId = ref("");
const channelTestBusyId = ref("");
// 节点切换是独立状态，不再占用全局 busy，避免整片节点卡片变灰。
const proxyPoolSwitchingNodeId = ref("");
let desiredProxyNodeId = "";
let activationWorker: Promise<void> | null = null;
let lastActivationError: unknown = null;
const testingNodeIds = ref<Set<string>>(new Set());
const proxyTestProgress = ref({ completed: 0, total: 0 });
const proxyTestCancelling = ref(false);
const proxyTestCancelRequested = ref(false);
const proxyNodesRevision = ref(0);
const proxySourceProgress = ref<Record<string, ProxySourceProgress>>({});

// —— Mihomo 内核自管理状态 ——
const kernelStatus = ref<MihomoKernelStatus | null>(null);
const kernelLoading = ref(false);
const kernelChecking = ref(false);
const kernelDownloading = ref(false);
const kernelDownloadProgress = ref<MihomoDownloadProgress>({
  stage: "",
  progress: 0,
  message: "",
});

// —— GeoIP 数据库自管理状态 ——
const geoipStatus = ref<GeoipStatus | null>(null);
const geoipLoading = ref(false);
const geoipDownloading = ref(false);
const geoipDownloadProgress = ref<GeoipDownloadProgress>({
  stage: "",
  progress: 0,
  message: "",
});

if (isTauri) {
  listen<MihomoDownloadProgress>("mihomo-kernel-progress", (event) => {
    kernelDownloadProgress.value = event.payload;
  });
  listen<GeoipDownloadProgress>("geoip-download-progress", (event) => {
    geoipDownloadProgress.value = event.payload;
  });
}

function bumpProxyNodesRevision() {
  proxyNodesRevision.value += 1;
}

async function loadProxyPool() {
  proxyPoolLoading.value = true;
  try {
    proxyPool.value = await runCommand<ProxyPoolState>("get_proxy_pool_state");
    bumpProxyNodesRevision();
    loadMihomoKernelStatus();
    loadGeoipStatus();
  } catch (error) {
    proxyPoolError.value = String(error);
  } finally {
    proxyPoolLoading.value = false;
  }
}

async function analyzeProxyNodes() {
  proxyPoolBusyId.value = "ip-analysis";
  proxyPoolError.value = "";
  try {
    return await runCommand<ProxyIpAnalysis>("analyze_proxy_nodes");
  } catch (error) {
    proxyPoolError.value = String(error);
    throw error;
  } finally {
    proxyPoolBusyId.value = "";
  }
}

async function saveProxySubscription(name: string, url: string, id?: string) {
  proxyPoolError.value = "";
  // 先保存来源并立刻刷新列表，让地址先出现；再异步解析节点。
  const subscription = await runCommand<{ id: string; name?: string; url?: string; nodeCount?: number; lastError?: string; createdAt?: string; updatedAt?: string }>(
    "save_proxy_subscription",
    { id: id || null, name, url },
  );
  const now = new Date().toISOString();
  const existing = proxyPool.value.subscriptions.find((item) => item.id === subscription.id);
  const nextSub = {
    id: subscription.id,
    name: subscription.name || name,
    url: subscription.url || url,
    nodeCount: subscription.nodeCount ?? existing?.nodeCount ?? 0,
    lastError: subscription.lastError ?? "",
    createdAt: subscription.createdAt || existing?.createdAt || now,
    updatedAt: subscription.updatedAt || now,
  };
  proxyPool.value = {
    ...proxyPool.value,
    subscriptions: [nextSub, ...proxyPool.value.subscriptions.filter((item) => item.id !== nextSub.id)],
    subscriptionCount: 0, // temp, fixed below
  };
  proxyPool.value.subscriptionCount = proxyPool.value.subscriptions.length;
  proxySourceProgress.value = {
    ...proxySourceProgress.value,
    [nextSub.id]: {
      sourceId: nextSub.id,
      stage: "queued",
      status: "running",
      message: "来源已保存，准备解析…",
      completed: 0,
      total: 0,
      added: 0,
      discarded: 0,
    },
  };
  const result = await refreshProxySubscription(nextSub.id);
  return result;
}

async function deleteProxySubscription(id: string) {
  proxyPoolBusyId.value = id;
  proxyPoolError.value = "";
  try {
    proxyPool.value = await runCommand<ProxyPoolState>("delete_proxy_subscription", { id });
    bumpProxyNodesRevision();
  } catch (error) {
    proxyPoolError.value = String(error);
    throw error;
  } finally {
    proxyPoolBusyId.value = "";
  }
}

async function refreshProxySubscription(id: string) {
  proxyPoolBusyId.value = id;
  proxyPoolError.value = "";
  proxySourceProgress.value = {
    ...proxySourceProgress.value,
    [id]: {
      sourceId: id,
      stage: "queued",
      status: "running",
      message: "准备刷新…",
      completed: 0,
      total: 0,
      added: 0,
      discarded: 0,
    },
  };
  let unlisten: UnlistenFn | undefined;
  if (isTauri) {
    try {
      unlisten = await listen<ProxySourceProgress>("proxy-source-progress", ({ payload }) => {
        if (payload.sourceId !== id) return;
        proxySourceProgress.value = {
          ...proxySourceProgress.value,
          [id]: payload,
        };
        // 解析过程中同步更新来源卡片上的错误/数量提示。
        const index = proxyPool.value.subscriptions.findIndex((item) => item.id === id);
        if (index >= 0) {
          const current = proxyPool.value.subscriptions[index];
          proxyPool.value.subscriptions[index] = {
            ...current,
            lastError: payload.stage === "error" ? payload.message : "",
            nodeCount: payload.stage === "done" ? payload.total : current.nodeCount,
            updatedAt: new Date().toISOString(),
          };
        }
      });
    } catch {
      /* progress is best-effort */
    }
  }
  try {
    const result = await runCommand<ProxyPoolRefreshResult>("refresh_proxy_subscription", { id });
    await loadProxyPool();
    proxySourceProgress.value = {
      ...proxySourceProgress.value,
      [id]: {
        sourceId: id,
        stage: "done",
        status: "success",
        message: `解析完成：${result.total} 个节点，新增 ${result.added}，过滤 ${result.discarded}`,
        completed: result.total,
        total: result.total,
        added: result.added,
        discarded: result.discarded,
      },
    };
    return result;
  } catch (error) {
    const message = String(error);
    await loadProxyPool();
    proxyPoolError.value = message;
    proxySourceProgress.value = {
      ...proxySourceProgress.value,
      [id]: {
        sourceId: id,
        stage: "error",
        status: "error",
        message,
        completed: 0,
        total: 0,
        added: 0,
        discarded: 0,
      },
    };
    throw error;
  } finally {
    unlisten?.();
    if (proxyPoolBusyId.value === id) proxyPoolBusyId.value = "";
  }
}

async function refreshAllProxySubscriptions() {
  const ids = proxyPool.value.subscriptions.map((item) => item.id);
  if (!ids.length) return { succeeded: 0, failed: 0, discarded: 0 };
  proxyPoolBusyId.value = "all";
  proxyPoolError.value = "";
  let failed = 0;
  let discarded = 0;
  // 串行刷新，保证每个来源卡片都能看到完整进度，也避免同时压垮网络。
  for (const id of ids) {
    try {
      const result = await refreshProxySubscription(id);
      discarded += result.discarded;
    } catch {
      failed += 1;
    }
  }
  if (failed) proxyPoolError.value = `${failed} 个导入源刷新失败，请查看来源卡片错误信息`;
  proxyPoolBusyId.value = "";
  return { succeeded: ids.length - failed, failed, discarded };
}

async function saveProxyPoolSettings(ignoreAddresses: string) {
  proxyPoolError.value = "";
  try {
    proxyPool.value = await runCommand<ProxyPoolState>("set_proxy_pool_settings", { ignoreAddresses });
    bumpProxyNodesRevision();
  } catch (error) {
    proxyPoolError.value = String(error);
    throw error;
  }
}

async function runProxyActivationQueue() {
  while (desiredProxyNodeId) {
    // latest-wins：切换过程中继续点其他节点时，只执行最后一次选择，避免并发重配 Mihomo。
    const nodeId = desiredProxyNodeId;
    desiredProxyNodeId = "";
    proxyPoolSwitchingNodeId.value = nodeId;
    proxyPoolError.value = "";
    lastActivationError = null;
    try {
      proxyPool.value = await runCommand<ProxyPoolState>("set_active_proxy_node", { nodeId });
      bumpProxyNodesRevision();
    } catch (error) {
      lastActivationError = error;
      proxyPoolError.value = String(error);
    }
  }
}

async function activateProxyNode(nodeId: string) {
  desiredProxyNodeId = nodeId;
  proxyPoolSwitchingNodeId.value = nodeId;
  if (!activationWorker) {
    activationWorker = runProxyActivationQueue().finally(() => {
      activationWorker = null;
      proxyPoolSwitchingNodeId.value = "";
    });
  }
  await activationWorker;
  if (lastActivationError) throw lastActivationError;
}

async function clearActiveProxyNode() {
  proxyPoolBusyId.value = "clear";
  proxyPoolError.value = "";
  try {
    proxyPool.value = await runCommand<ProxyPoolState>("clear_active_proxy_node");
    bumpProxyNodesRevision();
  } catch (error) {
    proxyPoolError.value = String(error);
    throw error;
  } finally {
    proxyPoolBusyId.value = "";
  }
}

async function testProxyNode(nodeId: string) {
  testingNodeIds.value = new Set(testingNodeIds.value).add(nodeId);
  try {
    const node = await runCommand<ProxyNode>("test_proxy_node", { nodeId });
    const index = proxyPool.value.nodes.findIndex((item) => item.id === node.id);
    if (index >= 0) proxyPool.value.nodes[index] = node;
    return node;
  } catch (error) {
    await loadProxyPool();
    throw error;
  } finally {
    const next = new Set(testingNodeIds.value);
    next.delete(nodeId);
    testingNodeIds.value = next;
  }
}

async function runProxyNodeBatch(nodeIds: string[] | null, busyId: string) {
  const requestedIds = nodeIds ? new Set(nodeIds) : null;
  const candidates = proxyPool.value.nodes.filter((node) => (
    node.testStatus !== "invalid" && (!requestedIds || requestedIds.has(node.id))
  ));
  if (!candidates.length) return { succeeded: 0, failed: 0, cancelled: false, completed: 0, total: 0 };

  // 节点索引表：避免每个进度事件都全表 findIndex。
  const nodeIndex = new Map(proxyPool.value.nodes.map((node, index) => [node.id, index]));
  proxyPoolBusyId.value = busyId;
  proxyPoolError.value = "";
  proxyTestCancelling.value = false;
  proxyTestCancelRequested.value = false;
  proxyTestProgress.value = { completed: 0, total: candidates.length };

  let batchSucceeded = 0;
  let batchFailed = 0;
  let receivedProgress = false;
  let commandFailed = false;
  let unlisten: UnlistenFn | undefined;
  let rafId = 0;
  const pendingStarts = new Set<string>();
  const pendingStops = new Set<string>();
  let pendingProgress: { completed: number; total: number } | null = null;
  const pendingResults = new Map<string, { latencyMs: number | null; status: string }>();

  const flushProgress = () => {
    rafId = 0;
    if (pendingStarts.size || pendingStops.size) {
      const testing = new Set(testingNodeIds.value);
      pendingStarts.forEach((id) => testing.add(id));
      pendingStops.forEach((id) => testing.delete(id));
      pendingStarts.clear();
      pendingStops.clear();
      testingNodeIds.value = testing;
    }
    if (pendingResults.size) {
      const testedAt = new Date().toISOString();
      pendingResults.forEach((result, nodeId) => {
        const index = nodeIndex.get(nodeId);
        if (index == null) return;
        const node = proxyPool.value.nodes[index];
        if (!node) return;
        // 原地更新，避免替换整个数组元素触发大列表 diff。
        node.latencyMs = result.latencyMs;
        node.testStatus = result.status;
        node.testedAt = testedAt;
      });
      pendingResults.clear();
    }
    if (pendingProgress) {
      proxyTestProgress.value = pendingProgress;
      pendingProgress = null;
    }
  };

  const scheduleFlush = () => {
    if (rafId) return;
    rafId = window.requestAnimationFrame(flushProgress);
  };

  if (isTauri) {
    try {
      unlisten = await listen<ProxyNodeTestProgress>("proxy-node-test-progress", ({ payload }) => {
        receivedProgress = true;
        if (payload.phase === "started") {
          pendingStops.delete(payload.nodeId);
          pendingStarts.add(payload.nodeId);
        } else {
          pendingStarts.delete(payload.nodeId);
          pendingStops.add(payload.nodeId);
          if (payload.status !== "cancelled") {
            if (payload.status === "success") batchSucceeded += 1;
            else batchFailed += 1;
            pendingResults.set(payload.nodeId, {
              latencyMs: payload.latencyMs,
              status: payload.status,
            });
          }
          pendingProgress = { completed: payload.completed, total: payload.total };
        }
        scheduleFlush();
      });
    } catch {
      /* final state still refreshes even when event listening is unavailable */
    }
  }
  try {
    const runBatch = async () => nodeIds
      ? runCommand<ProxyPoolState>("test_proxy_nodes", { nodeIds })
      : runCommand<ProxyPoolState>("test_all_proxy_nodes");
    try {
      proxyPool.value = await runBatch();
    } catch (error) {
      const msg = String(error);
      // 上一轮取消后 lease 可能尚未完全释放，短暂重试一次。
      if (msg.includes("已有代理测速任务正在进行")) {
        await new Promise((r) => setTimeout(r, 300));
        proxyPool.value = await runBatch();
      } else {
        throw error;
      }
    }
    // 最终整表替换后再重建一次列表；测速过程中只做原地字段更新。
    bumpProxyNodesRevision();
  } catch (error) {
    commandFailed = true;
    const errorMessage = String(error);
    // 用户已取消时，不要把取消过程中的内核中断当红色失败。
    if (proxyTestCancelRequested.value || errorMessage.includes("测速已取消")) {
      commandFailed = false;
    } else {
      await loadProxyPool();
      proxyPoolError.value = errorMessage;
    }
  } finally {
    if (rafId) {
      window.cancelAnimationFrame(rafId);
      rafId = 0;
    }
    flushProgress();
    unlisten?.();
    testingNodeIds.value = new Set();
    proxyPoolBusyId.value = "";
    proxyTestCancelling.value = false;
  }
  const progress = proxyTestProgress.value;
  const cancelled = !commandFailed && (
    proxyTestCancelRequested.value || (receivedProgress && progress.completed < progress.total)
  );
  proxyTestCancelRequested.value = false;
  if (!receivedProgress) {
    const resultNodes = proxyPool.value.nodes.filter((node) => !requestedIds || requestedIds.has(node.id));
    batchSucceeded = resultNodes.filter((node) => node.testStatus === "success").length;
    batchFailed = resultNodes.filter((node) => node.testStatus === "error" || node.testStatus === "invalid").length;
  }
  return {
    succeeded: batchSucceeded,
    failed: batchFailed,
    cancelled,
    completed: progress.completed,
    total: progress.total,
  };
}

async function testAllProxyNodes() {
  return runProxyNodeBatch(null, "test-all");
}

async function testProxyNodes(nodeIds: string[], busyId = "test-selection") {
  return runProxyNodeBatch(nodeIds, busyId);
}

async function saveProxyChannel(name: string, id?: string) {
  proxyPoolBusyId.value = "channel-save";
  proxyPoolError.value = "";
  try {
    proxyPool.value = await runCommand<ProxyPoolState>("save_proxy_channel", {
      id: id || null,
      name,
    });
    bumpProxyNodesRevision();
    return proxyPool.value;
  } catch (error) {
    proxyPoolError.value = String(error);
    throw error;
  } finally {
    if (proxyPoolBusyId.value === "channel-save") proxyPoolBusyId.value = "";
  }
}

async function deleteProxyChannel(id: string) {
  proxyPoolBusyId.value = "channel-delete";
  proxyPoolError.value = "";
  try {
    proxyPool.value = await runCommand<ProxyPoolState>("delete_proxy_channel", { id });
    bumpProxyNodesRevision();
    return proxyPool.value;
  } catch (error) {
    proxyPoolError.value = String(error);
    throw error;
  } finally {
    if (proxyPoolBusyId.value === "channel-delete") proxyPoolBusyId.value = "";
  }
}

async function setProxyChannelNode(channelId: string, nodeId: string) {
  proxyPoolBusyId.value = "channel-node";
  proxyPoolError.value = "";
  try {
    proxyPool.value = await runCommand<ProxyPoolState>("set_proxy_channel_node", { channelId, nodeId });
    bumpProxyNodesRevision();
    return proxyPool.value;
  } catch (error) {
    proxyPoolError.value = String(error);
    throw error;
  } finally {
    if (proxyPoolBusyId.value === "channel-node") proxyPoolBusyId.value = "";
  }
}

async function assignAccountProxyChannel(profileId: string, channelId: string) {
  proxyPoolBusyId.value = "channel-assign";
  proxyPoolError.value = "";
  try {
    proxyPool.value = await runCommand<ProxyPoolState>("assign_account_proxy_channel", {
      profileId,
      channelId,
    });
    bumpProxyNodesRevision();
    return proxyPool.value;
  } catch (error) {
    proxyPoolError.value = String(error);
    throw error;
  } finally {
    if (proxyPoolBusyId.value === "channel-assign") proxyPoolBusyId.value = "";
  }
}

async function unassignAccountProxyChannel(profileId: string) {
  proxyPoolBusyId.value = "channel-assign";
  proxyPoolError.value = "";
  try {
    proxyPool.value = await runCommand<ProxyPoolState>("unassign_account_proxy_channel", {
      profileId,
    });
    bumpProxyNodesRevision();
    return proxyPool.value;
  } catch (error) {
    proxyPoolError.value = String(error);
    throw error;
  } finally {
    if (proxyPoolBusyId.value === "channel-assign") proxyPoolBusyId.value = "";
  }
}

async function testProxyChannelNodes(channelId?: string, nodeIds?: string[]) {
  const busyId = `test-channel-${channelId || "all"}`;
  channelTestBusyId.value = busyId;
  proxyPoolError.value = "";
  try {
    proxyPool.value = await runCommand<ProxyPoolState>("test_proxy_channel_nodes", {
      channelId: channelId || undefined,
      nodeIds: nodeIds && nodeIds.length > 0 ? nodeIds : undefined,
    });
    bumpProxyNodesRevision();
    return proxyPool.value;
  } catch (error) {
    proxyPoolError.value = String(error);
    throw error;
  } finally {
    if (channelTestBusyId.value === busyId) channelTestBusyId.value = "";
  }
}

async function cancelProxyNodeTests() {
  // 取消必须瞬时响应：只发信号，不阻塞在测速主命令上。
  proxyTestCancelRequested.value = true;
  proxyTestCancelling.value = true;
  try {
    await runCommand<boolean>("cancel_proxy_node_tests");
  } catch (error) {
    console.error("cancel_proxy_node_tests failed", error);
  }

  // 最多等 1.5s 看 busy 是否被 finally 清掉；超时强制解锁 UI。
  const deadline = Date.now() + 1500;
  while (Date.now() < deadline && proxyPoolBusyId.value.startsWith("test-")) {
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  if (proxyPoolBusyId.value.startsWith("test-")) {
    testingNodeIds.value = new Set();
    proxyPoolBusyId.value = "";
  }
  proxyTestCancelling.value = false;
  return true;
}

const proxyPoolActive = computed(() => proxyPool.value.enabled);

async function deleteInvalidProxyNodes() {
  proxyPoolBusyId.value = "delete-invalid";
  proxyPoolError.value = "";
  try {
    proxyPool.value = await runCommand<ProxyPoolState>("delete_invalid_proxy_nodes");
    bumpProxyNodesRevision();
  } catch (error) {
    proxyPoolError.value = String(error);
    throw error;
  } finally {
    proxyPoolBusyId.value = "";
  }
}

export const KERNEL_DOWNLOAD_MIRRORS = [
  { value: "auto", text: "⚡ 智能全网竞速 (推荐 · 4线程并发)" },
  { value: "https://gh-proxy.com", text: "🚀 gh-proxy.com (亚太 CDN)" },
  { value: "https://ghfast.top", text: "🚀 ghfast.top (Cloudflare 边缘加速)" },
  { value: "https://gh.ddlc.top", text: "🚀 gh.ddlc.top (国内边缘加速)" },
  { value: "https://ghps.cc", text: "🚀 ghps.cc (国内镜像)" },
  { value: "https://github.boki.moe", text: "🚀 github.boki.moe (镜像加速)" },
  { value: "https://ghproxy.net", text: "🚀 ghproxy.net (备用镜像)" },
  { value: "direct", text: "🌐 GitHub 官方直连 (适合 VPN/代理)" },
  { value: "custom", text: "⚙️ 自定义镜像源前缀" },
] as const;

const kernelSelectedMirror = ref<string>("auto");
const kernelCustomMirror = ref<string>("");

async function loadMihomoKernelStatus() {
  kernelLoading.value = true;
  try {
    kernelStatus.value = await runCommand<MihomoKernelStatus>("get_mihomo_kernel_status");
  } catch (err) {
    console.error("读取 Mihomo 内核状态失败", err);
  } finally {
    kernelLoading.value = false;
  }
}

async function checkMihomoKernelUpdate(mirror?: string) {
  const m = mirror ?? (kernelSelectedMirror.value === "custom" ? kernelCustomMirror.value : kernelSelectedMirror.value);
  kernelChecking.value = true;
  try {
    kernelStatus.value = await runCommand<MihomoKernelStatus>("check_mihomo_kernel_update", { mirror: m || null });
    return kernelStatus.value;
  } catch (err) {
    proxyPoolError.value = String(err);
    throw err;
  } finally {
    kernelChecking.value = false;
  }
}

async function downloadOrUpdateMihomoKernel(mirror?: string) {
  const m = mirror ?? (kernelSelectedMirror.value === "custom" ? kernelCustomMirror.value : kernelSelectedMirror.value);
  kernelDownloading.value = true;
  kernelDownloadProgress.value = { stage: "starting", progress: 0, message: "准备下载…" };
  try {
    kernelStatus.value = await runCommand<MihomoKernelStatus>("download_or_update_mihomo_kernel", { mirror: m || null });
    await loadProxyPool();
    return kernelStatus.value;
  } catch (err) {
    proxyPoolError.value = String(err);
    throw err;
  } finally {
    kernelDownloading.value = false;
  }
}

async function loadGeoipStatus() {
  geoipLoading.value = true;
  try {
    geoipStatus.value = await runCommand<GeoipStatus>("get_geoip_status");
  } catch (err) {
    console.error("读取 GeoIP 状态失败", err);
  } finally {
    geoipLoading.value = false;
  }
}

async function downloadOrUpdateGeoip(mirror?: string) {
  const m = mirror ?? (kernelSelectedMirror.value === "custom" ? kernelCustomMirror.value : kernelSelectedMirror.value);
  geoipDownloading.value = true;
  geoipDownloadProgress.value = { stage: "starting", progress: 0, message: "准备下载 GeoIP 数据库…" };
  try {
    geoipStatus.value = await runCommand<GeoipStatus>("download_or_update_geoip", { mirror: m || null });
    await loadProxyPool();
    return geoipStatus.value;
  } catch (err) {
    proxyPoolError.value = String(err);
    throw err;
  } finally {
    geoipDownloading.value = false;
  }
}

export function useProxyPool() {
  return {
    proxyPool, proxyPoolLoading, proxyPoolError, proxyPoolBusyId, channelTestBusyId, proxyPoolSwitchingNodeId, testingNodeIds, proxyTestProgress, proxyTestCancelling, proxyNodesRevision, proxySourceProgress, proxyPoolActive,
    kernelStatus, kernelLoading, kernelChecking, kernelDownloading, kernelDownloadProgress,
    geoipStatus, geoipLoading, geoipDownloading, geoipDownloadProgress,
    kernelSelectedMirror, kernelCustomMirror,
    loadProxyPool, saveProxySubscription, deleteProxySubscription, refreshProxySubscription,
    refreshAllProxySubscriptions, saveProxyPoolSettings, activateProxyNode, clearActiveProxyNode,
    analyzeProxyNodes,
    deleteInvalidProxyNodes,
    testProxyNode, testProxyNodes, testAllProxyNodes, cancelProxyNodeTests,
    saveProxyChannel, deleteProxyChannel, setProxyChannelNode,
    assignAccountProxyChannel, unassignAccountProxyChannel, testProxyChannelNodes,
    loadMihomoKernelStatus, checkMihomoKernelUpdate, downloadOrUpdateMihomoKernel,
    loadGeoipStatus, downloadOrUpdateGeoip,
  };
}
