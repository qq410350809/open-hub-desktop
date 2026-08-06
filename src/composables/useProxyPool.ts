import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { computed, ref } from "vue";
import type { ProxyIpAnalysis, ProxyNode, ProxyNodeTestProgress, ProxyPoolRefreshResult, ProxyPoolState } from "../types";
import { runCommand } from "./useLibrary";

const isTauri = "__TAURI_INTERNALS__" in window;

const emptyState = (): ProxyPoolState => ({
  subscriptions: [], nodes: [], activeNodeId: "", activeNode: null,
  enabled: false, ignoreAddresses: "localhost,127.0.0.1,::1,.local,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16",
  speedTestUrl: "https://cp.cloudflare.com/generate_204", runtimeAvailable: false,
  runtimePath: "", runtimeError: "", nodeCount: 0, subscriptionCount: 0,
  invalidNodeCount: 0,
});

const proxyPool = ref<ProxyPoolState>(emptyState());
const proxyPoolLoading = ref(false);
const proxyPoolError = ref("");
const proxyPoolBusyId = ref("");
const testingNodeIds = ref<Set<string>>(new Set());
const proxyTestProgress = ref({ completed: 0, total: 0 });
const proxyTestCancelling = ref(false);
const proxyTestCancelRequested = ref(false);
const proxyNodesRevision = ref(0);

function bumpProxyNodesRevision() {
  proxyNodesRevision.value += 1;
}

async function loadProxyPool() {
  proxyPoolLoading.value = true;
  try {
    proxyPool.value = await runCommand<ProxyPoolState>("get_proxy_pool_state");
    bumpProxyNodesRevision();
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
  proxyPoolBusyId.value = id || "new";
  proxyPoolError.value = "";
  try {
    const subscription = await runCommand<{ id: string }>("save_proxy_subscription", { id: id || null, name, url });
    await refreshProxySubscription(subscription.id);
    return subscription;
  } catch (error) {
    proxyPoolError.value = String(error);
    throw error;
  } finally {
    proxyPoolBusyId.value = "";
  }
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
  try {
    const result = await runCommand<ProxyPoolRefreshResult>("refresh_proxy_subscription", { id });
    await loadProxyPool();
    return result;
  } catch (error) {
    const message = String(error);
    await loadProxyPool();
    proxyPoolError.value = message;
    throw error;
  } finally {
    proxyPoolBusyId.value = "";
  }
}

async function refreshAllProxySubscriptions() {
  const ids = proxyPool.value.subscriptions.map((item) => item.id);
  if (!ids.length) return { succeeded: 0, failed: 0, discarded: 0 };
  proxyPoolBusyId.value = "all";
  proxyPoolError.value = "";
  const results = await Promise.allSettled(ids.map((id) => runCommand<ProxyPoolRefreshResult>("refresh_proxy_subscription", { id })));
  await loadProxyPool();
  const failed = results.filter((result) => result.status === "rejected").length;
  const discarded = results.reduce((sum, result) => sum + (result.status === "fulfilled" ? result.value.discarded : 0), 0);
  if (failed) proxyPoolError.value = `${failed} 个导入源刷新失败，请查看左侧错误信息`;
  proxyPoolBusyId.value = "";
  return { succeeded: ids.length - failed, failed, discarded };
}

async function saveProxyPoolSettings(ignoreAddresses: string, speedTestUrl: string) {
  proxyPoolError.value = "";
  try {
    proxyPool.value = await runCommand<ProxyPoolState>("set_proxy_pool_settings", { ignoreAddresses, speedTestUrl });
    bumpProxyNodesRevision();
  } catch (error) {
    proxyPoolError.value = String(error);
    throw error;
  }
}

async function activateProxyNode(nodeId: string) {
  proxyPoolBusyId.value = nodeId;
  proxyPoolError.value = "";
  try {
    proxyPool.value = await runCommand<ProxyPoolState>("set_active_proxy_node", { nodeId });
    bumpProxyNodesRevision();
  } catch (error) {
    proxyPoolError.value = String(error);
    throw error;
  } finally {
    proxyPoolBusyId.value = "";
  }
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
    proxyPool.value = nodeIds
      ? await runCommand<ProxyPoolState>("test_proxy_nodes", { nodeIds })
      : await runCommand<ProxyPoolState>("test_all_proxy_nodes");
    // 最终整表替换后再重建一次列表；测速过程中只做原地字段更新。
    bumpProxyNodesRevision();
  } catch (error) {
    commandFailed = true;
    const errorMessage = String(error);
    await loadProxyPool();
    proxyPoolError.value = errorMessage;
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

async function cancelProxyNodeTests() {
  if (!proxyPoolBusyId.value.startsWith("test-")) return false;
  proxyTestCancelRequested.value = true;
  proxyTestCancelling.value = true;
  try {
    const cancelled = await runCommand<boolean>("cancel_proxy_node_tests");
    if (!cancelled) proxyTestCancelRequested.value = false;
    return cancelled;
  } catch (error) {
    proxyTestCancelRequested.value = false;
    proxyPoolError.value = String(error);
    throw error;
  } finally {
    proxyTestCancelling.value = false;
  }
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

export function useProxyPool() {
  return {
    proxyPool, proxyPoolLoading, proxyPoolError, proxyPoolBusyId, testingNodeIds, proxyTestProgress, proxyTestCancelling, proxyNodesRevision, proxyPoolActive,
    loadProxyPool, saveProxySubscription, deleteProxySubscription, refreshProxySubscription,
    refreshAllProxySubscriptions, saveProxyPoolSettings, activateProxyNode, clearActiveProxyNode,
    analyzeProxyNodes,
    deleteInvalidProxyNodes,
    testProxyNode, testProxyNodes, testAllProxyNodes, cancelProxyNodeTests,
  };
}
