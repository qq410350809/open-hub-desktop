import { listen, type UnlistenFn } from "../core/events";
import { computed, ref } from "vue";
import type { ClashSubscriptionInfo, GeoipDownloadProgress, GeoipStatus, MihomoDownloadProgress, MihomoKernelStatus, ProxyIpAnalysis, ProxyNode, ProxyNodeTestProgress, ProxyPoolRefreshResult, ProxyPoolState, ProxySourceProgress } from "../../types";
import { isIntegratedClient, runCommand } from "../core/ipc";
import { createProxyTestProgress, mergeProxyTestMetadata } from "./proxyTestProgress";

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
// 通道测速实时进度（单阶段 GET：TTFB 作延迟、下载完总耗时作网速）
const channelTestProgress = ref({ completed: 0, total: 0 });
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
let componentEventsStarted = false;
const componentEventUnlisteners: UnlistenFn[] = [];

// 每个节点归属最近一次测速，旧命令、旧 RAF 和取消后的回包都不能覆盖新一轮。
let proxyTestGeneration = 0;
let proxyStateRequest = 0;
let proxyTestDataVersion = 0;
const nodeTestDataVersions = new Map<string, number>();
const latestNodeTest = new Map<string, number>();
const proxyTestSessions = new Set<ProxyTestSession>();
type ProxyTestSession = {
  generation: number;
  runId: string;
  mode: "node" | "batch" | "channel";
  tracker: ReturnType<typeof createProxyTestProgress>;
  cancelled: boolean;
  closed: boolean;
  flush: () => void;
  dispose: () => void;
  settle: (state: ProxyPoolState) => void;
};

// SSE 的 listen 先同步挂载回调，但 Promise 可能等长连接结束才返回。
// 每个事件共享一个底层监听器，避免 Web 重测积累无法及时释放的闭包。
type ProxyEventEntry = {
  handlers: Set<(event: { payload: unknown }) => void>;
  ready: Promise<void>;
  stop?: UnlistenFn;
};
const proxyEventEntries = new Map<string, ProxyEventEntry>();
async function listenProxyEvent<T>(event: string, handler: (event: { payload: T }) => void): Promise<UnlistenFn> {
  let entry = proxyEventEntries.get(event);
  if (!entry) {
    const created: ProxyEventEntry = { handlers: new Set(), ready: Promise.resolve() };
    proxyEventEntries.set(event, created);
    created.ready = listen<unknown>(event, (payload) => created.handlers.forEach((fn) => fn(payload)))
      .then((stop) => {
        created.stop = stop;
        if (!created.handlers.size) {
          stop();
          if (proxyEventEntries.get(event) === created) proxyEventEntries.delete(event);
        }
      })
      .catch(() => { if (proxyEventEntries.get(event) === created) proxyEventEntries.delete(event); });
    entry = created;
  }
  const wrapped = (payload: { payload: unknown }) => handler(payload as { payload: T });
  entry.handlers.add(wrapped);
  if (isIntegratedClient) await entry.ready;
  return () => {
    entry.handlers.delete(wrapped);
    if (!entry.handlers.size && entry.stop) {
      entry.stop();
      if (proxyEventEntries.get(event) === entry) proxyEventEntries.delete(event);
    }
  };
}

function updateNodeCopies(ids: ReadonlySet<string>, update: (node: ProxyNode) => void) {
  for (const node of proxyPool.value.nodes) if (ids.has(node.id)) update(node);
  for (const channel of proxyPool.value.channels) {
    if (channel.node && ids.has(channel.node.id)) update(channel.node);
  }
  if (proxyPool.value.activeNode && ids.has(proxyPool.value.activeNode.id)) update(proxyPool.value.activeNode);
}

function markProxyTestDataChanged(ids: Iterable<string>) {
  const version = ++proxyTestDataVersion;
  for (const id of ids) nodeTestDataVersions.set(id, version);
}

function mergeStateSince(state: ProxyPoolState, version: number) {
  const changed = new Set<string>();
  nodeTestDataVersions.forEach((updated, id) => { if (updated > version) changed.add(id); });
  return mergeProxyTestMetadata(state, proxyPool.value, changed);
}

// 配置/删除/激活等命令仍以响应中的拓扑与配置为准，只防止覆盖在途测速数据。
async function runProxyPoolCommand(command: string, args: Record<string, unknown> = {}) {
  const version = proxyTestDataVersion;
  const state = await runCommand<ProxyPoolState>(command, args);
  // 先落同帧事件，覆盖请求期间收到但尚未 RAF flush 的最新结果。
  proxyTestSessions.forEach((session) => session.flush());
  proxyPool.value = mergeStateSince(state, version);
  bumpProxyNodesRevision();
  return proxyPool.value;
}

function createProxyTestRunId() {
  if (globalThis.crypto?.randomUUID) return globalThis.crypto.randomUUID();
  if (globalThis.crypto?.getRandomValues) {
    return Array.from(globalThis.crypto.getRandomValues(new Uint8Array(16)), (byte) => byte.toString(16).padStart(2, "0")).join("");
  }
  // 老旧非安全上下文的相关性标识，不用作鉴权或秘密。
  return `${Date.now().toString(36)}-${proxyTestGeneration}-${Math.random().toString(36).slice(2)}-${Math.random().toString(36).slice(2)}`;
}

async function beginProxyTest(
  ids: string[],
  mode: "node" | "batch" | "channel",
): Promise<ProxyTestSession> {
  const generation = ++proxyTestGeneration;
  const runId = createProxyTestRunId();
  const tracker = createProxyTestProgress(ids, runId);
  ids.forEach((id) => latestNodeTest.set(id, generation));
  // 已被完全接管的旧监听器不再保留；部分重测时仍允许未重测节点完成富化。
  for (const old of proxyTestSessions) {
    const supersededBatch = !old.tracker.settled && (old.mode !== "node" || mode !== "node");
    if (supersededBatch || [...old.tracker.ids].every((id) => latestNodeTest.get(id) !== old.generation)) old.dispose();
  }
  updateNodeCopies(tracker.ids, (node) => { node.ipInfo = null; });
  markProxyTestDataChanged(ids);
  bumpProxyNodesRevision();
  let rafId = 0;
  let unlisten: UnlistenFn | undefined;
  let rebuildTimer = 0;
  let lastRebuildAt = 0;
  const rebuild = () => {
    rebuildTimer = 0;
    if (session.closed) return;
    lastRebuildAt = Date.now();
    bumpProxyNodesRevision();
  };
  let indexedNodes = proxyPool.value.nodes;
  let nodeIndex = new Map(indexedNodes.map((node) => [node.id, node]));
  const owns = (id: string) => latestNodeTest.get(id) === generation;
  const session: ProxyTestSession = {
    generation, runId, mode, tracker, cancelled: false, closed: false,
    flush() {
      if (rafId) window.cancelAnimationFrame(rafId);
      rafId = 0;
      if (session.closed) { tracker.clearPending(); return; }
      if (indexedNodes !== proxyPool.value.nodes) {
        indexedNodes = proxyPool.value.nodes;
        nodeIndex = new Map(indexedNodes.map((node) => [node.id, node]));
      }
      const testing = new Set(testingNodeIds.value);
      tracker.starts.forEach((id) => { if (owns(id)) testing.add(id); });
      tracker.stops.forEach((id) => { if (owns(id)) testing.delete(id); });
      if (tracker.starts.size || tracker.stops.size) testingNodeIds.value = testing;
      let changed = false;
      const update = (node: ProxyNode) => {
        if (!owns(node.id) || !tracker.dirty.has(node.id)) return;
        tracker.apply(node);
        markProxyTestDataChanged([node.id]);
        changed = true;
      };
      tracker.dirty.forEach((id) => { const node = nodeIndex.get(id); if (node) update(node); });
      proxyPool.value.channels.forEach((channel) => { if (channel.node) update(channel.node); });
      if (proxyPool.value.activeNode) update(proxyPool.value.activeNode);
      if (!tracker.settled && generation === proxyTestGeneration) {
        if (mode === "batch") proxyTestProgress.value = tracker.progress;
        if (mode === "channel") channelTestProgress.value = tracker.progress;
      }
      tracker.clearPending();
      // 保持原大列表节流；尾帧也必须重过滤，不能漏掉最后的类型查询结果。
      if (changed && !rebuildTimer) {
        const wait = 1000 - (Date.now() - lastRebuildAt);
        if (wait <= 0) rebuild();
        else rebuildTimer = window.setTimeout(rebuild, wait);
      }
    },
    dispose() {
      session.closed = true;
      if (rafId) window.cancelAnimationFrame(rafId);
      rafId = 0;
      tracker.clearPending();
      if (rebuildTimer) window.clearTimeout(rebuildTimer);
      rebuildTimer = 0;
      unlisten?.();
      proxyTestSessions.delete(session);
    },
    settle(state) {
      if (rafId) window.cancelAnimationFrame(rafId);
      rafId = 0;
      if (rebuildTimer) window.clearTimeout(rebuildTimer);
      rebuildTimer = 0;
      tracker.settle(state);
    },
  };
  proxyTestSessions.add(session);
  unlisten = await listenProxyEvent<ProxyNodeTestProgress>(
    mode === "channel" ? "proxy-channel-test-progress" : "proxy-node-test-progress",
    ({ payload }) => {
      if (session.closed || !owns(payload.nodeId) || (session.cancelled && payload.phase === "ip-info")) return;
      if (!tracker.accept(payload)) return;
      if (!rafId) rafId = window.requestAnimationFrame(session.flush);
    },
  );
  if (session.closed) unlisten();
  return session;
}

function installTestState(session: ProxyTestSession, state: ProxyPoolState): boolean {
  if (session.closed || session.generation !== proxyTestGeneration) return false;
  session.settle(state);
  // 非本批节点可能仍有富化在途；仅合并仍归属旧会话的 IP，不恢复其测速指标。
  for (const other of proxyTestSessions) {
    if (other === session) continue;
    const merge = (node: ProxyNode) => {
      if (latestNodeTest.get(node.id) === other.generation) other.tracker.applyIpInfo(node);
    };
    state.nodes.forEach(merge);
    state.channels.forEach((channel) => { if (channel.node) merge(channel.node); });
    if (state.activeNode) merge(state.activeNode);
  }
  proxyPool.value = state;
  markProxyTestDataChanged(session.tracker.ids);
  bumpProxyNodesRevision();
  return true;
}

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

async function startComponentEvents() {
  if (componentEventsStarted) return;
  componentEventsStarted = true;
  try {
    componentEventUnlisteners.push(
      await listenProxyEvent<MihomoDownloadProgress>("mihomo-kernel-progress", (event) => {
        kernelDownloadProgress.value = event.payload;
      }),
      await listenProxyEvent<GeoipDownloadProgress>("geoip-download-progress", (event) => {
        geoipDownloadProgress.value = event.payload;
      }),
      await listenProxyEvent("proxy-nodes-updated", () => {
        void loadProxyPool();
      }),
    );
  } catch {
    // 组件进度监听失败不阻止主界面和手动下载。
  }
}

function stopComponentEvents() {
  componentEventUnlisteners.splice(0).forEach((unlisten) => unlisten());
  proxyTestSessions.forEach((session) => session.dispose());
  componentEventsStarted = false;
}

function bumpProxyNodesRevision() {
  proxyNodesRevision.value += 1;
}

async function loadProxyPool() {
  // 测速中的权威整表由该命令返回；避免后台刷新抢先替换流式进度。
  if ([...proxyTestSessions].some((session) => !session.closed && !session.tracker.settled)) return;
  const generation = proxyTestGeneration;
  const request = ++proxyStateRequest;
  const version = proxyTestDataVersion;
  proxyPoolLoading.value = true;
  try {
    const state = await runCommand<ProxyPoolState>("get_proxy_pool_state");
    if (generation !== proxyTestGeneration || request !== proxyStateRequest) return;
    for (const session of proxyTestSessions) {
      const merge = (node: ProxyNode) => {
        if (latestNodeTest.get(node.id) === session.generation) session.tracker.applyIpInfo(node);
      };
      state.nodes.forEach(merge);
      state.channels.forEach((channel) => { if (channel.node) merge(channel.node); });
      if (state.activeNode) merge(state.activeNode);
    }
    proxyTestSessions.forEach((session) => session.flush());
    proxyPool.value = mergeStateSince(state, version);
    bumpProxyNodesRevision();
    await Promise.all([loadMihomoKernelStatus(), loadGeoipStatus()]);
  } catch (error) {
    if (generation === proxyTestGeneration && request === proxyStateRequest) proxyPoolError.value = String(error);
  } finally {
    if (request === proxyStateRequest) proxyPoolLoading.value = false;
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
    await runProxyPoolCommand("delete_proxy_subscription", { id });
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
    await runProxyPoolCommand("set_proxy_pool_settings", { ignoreAddresses });
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
      await runProxyPoolCommand("set_active_proxy_node", { nodeId });
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
    await runProxyPoolCommand("clear_active_proxy_node");
  } catch (error) {
    proxyPoolError.value = String(error);
    throw error;
  } finally {
    proxyPoolBusyId.value = "";
  }
}

async function testProxyNode(nodeId: string) {
  testingNodeIds.value = new Set(testingNodeIds.value).add(nodeId);
  const session = await beginProxyTest([nodeId], "node");
  try {
    const node = await runCommand<ProxyNode>("test_proxy_node", { nodeId, runId: session.runId });
    if (!session.closed && latestNodeTest.get(nodeId) === session.generation) {
      session.settle({ ...proxyPool.value, nodes: [node], channels: [], activeNode: null });
      updateNodeCopies(session.tracker.ids, (current) => Object.assign(current, node));
      markProxyTestDataChanged(session.tracker.ids);
      bumpProxyNodesRevision();
    }
    return node;
  } catch (error) {
    session.dispose();
    if (latestNodeTest.get(nodeId) === session.generation) await loadProxyPool();
    throw error;
  } finally {
    if (latestNodeTest.get(nodeId) === session.generation) {
      const next = new Set(testingNodeIds.value);
      next.delete(nodeId);
      testingNodeIds.value = next;
    }
  }
}

async function runProxyNodeBatch(nodeIds: string[] | null, busyId: string) {
  const requestedIds = nodeIds ? new Set(nodeIds) : null;
  const candidates = proxyPool.value.nodes.filter((node) => (
    node.testStatus !== "invalid" && (!requestedIds || requestedIds.has(node.id))
  ));
  if (!candidates.length) return { succeeded: 0, failed: 0, cancelled: false, completed: 0, total: 0 };

  // 开测清空旧网速；IP 富化由 beginProxyTest 同时清空（包括通道副本）。
  for (const node of candidates) {
    node.channelLatencyMs = null;
    node.channelTestStatus = "";
  }
  proxyPoolBusyId.value = busyId;
  proxyPoolError.value = "";
  proxyTestCancelling.value = false;
  proxyTestCancelRequested.value = false;
  proxyTestProgress.value = { completed: 0, total: candidates.length };
  const session = await beginProxyTest(candidates.map((node) => node.id), "batch");
  const { tracker } = session;
  let finalState: ProxyPoolState | undefined;
  let commandFailed = false;
  try {
    const runBatch = () => nodeIds
      ? runCommand<ProxyPoolState>("test_proxy_nodes", { nodeIds, runId: session.runId })
      : runCommand<ProxyPoolState>("test_all_proxy_nodes", { runId: session.runId });
    try {
      finalState = await runBatch();
    } catch (error) {
      // 上一轮取消后的 lease 可能尚未释放；旧批次/已取消批次不得自行重启。
      if (String(error).includes("已有代理测速任务正在进行") && !session.cancelled && !session.closed) {
        await new Promise((resolve) => setTimeout(resolve, 300));
        if (session.cancelled || session.closed || session.generation !== proxyTestGeneration) throw error;
        finalState = await runBatch();
      } else throw error;
    }
    installTestState(session, finalState);
  } catch (error) {
    const errorMessage = String(error);
    if (errorMessage.includes("测速已取消")) session.cancelled = true;
    commandFailed = !session.cancelled;
    if (commandFailed && !session.closed && session.generation === proxyTestGeneration) {
      session.flush();
      try {
        const state = await runCommand<ProxyPoolState>("get_proxy_pool_state");
        installTestState(session, state);
      } catch { /* 保留已经收到的进度 */ }
      if (!session.closed && session.generation === proxyTestGeneration) proxyPoolError.value = errorMessage;
    }
  } finally {
    session.flush();
    if (session.cancelled || commandFailed || !tracker.settled) session.dispose();
    const testing = new Set(testingNodeIds.value);
    tracker.ids.forEach((id) => { if (latestNodeTest.get(id) === session.generation) testing.delete(id); });
    testingNodeIds.value = testing;
    if (session.generation === proxyTestGeneration) {
      if (proxyPoolBusyId.value === busyId) proxyPoolBusyId.value = "";
      proxyTestCancelling.value = false;
      proxyTestCancelRequested.value = false;
    }
  }
  const progress = tracker.progress;
  const cancelled = !commandFailed && session.cancelled;
  let succeeded = tracker.succeeded;
  let failed = tracker.failed;
  let completed = progress.completed;
  if (finalState && !cancelled) {
    const results = finalState.nodes.filter((node) => tracker.ids.has(node.id));
    succeeded = results.filter((node) => node.testStatus === "success").length;
    failed = results.filter((node) => node.testStatus === "error" || node.testStatus === "invalid").length;
    completed = succeeded + failed;
  }
  if (session.generation === proxyTestGeneration) proxyTestProgress.value = { completed, total: progress.total };
  return { succeeded, failed, cancelled, completed, total: progress.total };
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
    await runProxyPoolCommand("save_proxy_channel", {
      id: id || null,
      name,
    });
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
    await runProxyPoolCommand("delete_proxy_channel", { id });
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
    await runProxyPoolCommand("set_proxy_channel_node", { channelId, nodeId });
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
    await runProxyPoolCommand("assign_account_proxy_channel", {
      profileId,
      channelId,
    });
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
    await runProxyPoolCommand("unassign_account_proxy_channel", {
      profileId,
    });
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
  channelTestProgress.value = { completed: 0, total: 0 };
  const requested = nodeIds?.length ? new Set(nodeIds) : null;
  const candidates = proxyPool.value.nodes.filter((node) => node.testStatus !== "invalid" && (!requested || requested.has(node.id)));
  const session = await beginProxyTest(candidates.map((node) => node.id), "channel");
  try {
    const state = await runCommand<ProxyPoolState>("test_proxy_channel_nodes", {
      runId: session.runId,
      channelId: channelId || undefined,
      nodeIds: nodeIds && nodeIds.length > 0 ? nodeIds : undefined,
    });
    installTestState(session, state);
    return proxyPool.value;
  } catch (error) {
    session.dispose();
    if (session.generation === proxyTestGeneration) proxyPoolError.value = String(error);
    throw error;
  } finally {
    session.flush();
    if (!session.tracker.settled) session.dispose();
    const testing = new Set(testingNodeIds.value);
    session.tracker.ids.forEach((id) => { if (latestNodeTest.get(id) === session.generation) testing.delete(id); });
    testingNodeIds.value = testing;
    if (session.generation === proxyTestGeneration) {
      channelTestProgress.value = { completed: 0, total: 0 };
      if (channelTestBusyId.value === busyId) channelTestBusyId.value = "";
    }
  }
}

async function cancelProxyNodeTests() {
  const generation = proxyTestGeneration;
  const sessions = [...proxyTestSessions].filter((session) => !session.tracker.settled);
  sessions.forEach((session) => { session.cancelled = true; });
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
  while (generation === proxyTestGeneration && Date.now() < deadline && proxyPoolBusyId.value.startsWith("test-")) {
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  if (generation === proxyTestGeneration && proxyPoolBusyId.value.startsWith("test-")) {
    sessions.forEach((session) => { session.flush(); session.dispose(); });
    testingNodeIds.value = new Set();
    proxyPoolBusyId.value = "";
  }
  if (generation === proxyTestGeneration) proxyTestCancelling.value = false;
  return true;
}

const proxyPoolActive = computed(() => proxyPool.value.enabled);

// —— Clash 订阅分享 ——
const clashSubInfo = ref<ClashSubscriptionInfo | null>(null);
const clashSubLoading = ref(false);

async function loadClashSubscriptionInfo() {
  clashSubLoading.value = true;
  try {
    clashSubInfo.value = await runCommand<ClashSubscriptionInfo>("get_clash_subscription_info");
  } catch (error) {
    console.error("读取 Clash 订阅信息失败", error);
  } finally {
    clashSubLoading.value = false;
  }
}

async function regenerateClashSubscriptionToken() {
  proxyPoolError.value = "";
  try {
    clashSubInfo.value = await runCommand<ClashSubscriptionInfo>(
      "regenerate_clash_subscription_token",
    );
    return clashSubInfo.value;
  } catch (error) {
    proxyPoolError.value = String(error);
    throw error;
  }
}

async function deleteInvalidProxyNodes() {
  proxyPoolBusyId.value = "delete-invalid";
  proxyPoolError.value = "";
  try {
    await runProxyPoolCommand("delete_invalid_proxy_nodes");
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
    proxyPool, proxyPoolLoading, proxyPoolError, proxyPoolBusyId, channelTestBusyId, channelTestProgress, proxyPoolSwitchingNodeId, testingNodeIds, proxyTestProgress, proxyTestCancelling, proxyNodesRevision, proxySourceProgress, proxyPoolActive,
    kernelStatus, kernelLoading, kernelChecking, kernelDownloading, kernelDownloadProgress,
    geoipStatus, geoipLoading, geoipDownloading, geoipDownloadProgress,
    kernelSelectedMirror, kernelCustomMirror,
    loadProxyPool, startComponentEvents, stopComponentEvents, saveProxySubscription, deleteProxySubscription, refreshProxySubscription,
    refreshAllProxySubscriptions, saveProxyPoolSettings, activateProxyNode, clearActiveProxyNode,
    analyzeProxyNodes,
    deleteInvalidProxyNodes,
    testProxyNode, testProxyNodes, testAllProxyNodes, cancelProxyNodeTests,
    saveProxyChannel, deleteProxyChannel, setProxyChannelNode,
    assignAccountProxyChannel, unassignAccountProxyChannel, testProxyChannelNodes,
    loadMihomoKernelStatus, checkMihomoKernelUpdate, downloadOrUpdateMihomoKernel,
    loadGeoipStatus, downloadOrUpdateGeoip,
    clashSubInfo, clashSubLoading, loadClashSubscriptionInfo, regenerateClashSubscriptionToken,
  };
}
