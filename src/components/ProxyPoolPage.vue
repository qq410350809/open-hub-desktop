<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, watch } from "vue";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import type { ProxyNode, ProxySubscription } from "../types";

const store = useStore();
const speedTestPresets = [
  { label: "Cloudflare · 204", value: "https://cp.cloudflare.com/generate_204" },
  { label: "Microsoft · HTTPS", value: "https://www.msftconnecttest.com/connecttest.txt" },
  { label: "Microsoft · HTTP", value: "http://www.msftconnecttest.com/connecttest.txt" },
  { label: "Apple · Captive Portal", value: "http://captive.apple.com/hotspot-detect.html" },
];
const sourceName = ref("");
const sourceLinks = ref("");
const editingId = ref("");
const selectedSource = ref("all");
// 6000+ 节点默认只展示 ≤500ms 的可用节点，避免全量渲染卡死。
const latencyFilter = ref<"500" | "1000" | "2000">("500");
const latencyFilterOptions = [
  { value: "500", label: "≤ 500ms" },
  { value: "1000", label: "≤ 1000ms" },
  { value: "2000", label: "≤ 2000ms" },
] as const;
const settingsOpen = ref(false);
const ignoreAddresses = ref("");
const speedTestUrl = ref("");
const speedTestPreset = ref("https://cp.cloudflare.com/generate_204");
const message = ref("");
const importDialogOpen = ref(false);
const deleteConfirmId = ref("");
const nodeViewMode = ref<"list" | "ip">("list");
const collapsedGroups = ref<Set<string>>(new Set());
const cancelConfirmOpen = ref(false);

const RENDER_CHUNK = 120;
const GROUP_NODE_CHUNK = 80;
const visibleNodeLimit = ref(RENDER_CHUNK);
const visibleGroupLimit = ref(12);
const expandedGroupLimits = ref<Record<string, number>>({});
let renderMoreRaf = 0;
let liveRebuildTimer = 0;

const rawNodeCount = computed(() => store.proxyPool.value.subscriptions
  .reduce((sum, item) => sum + item.nodeCount, 0));
const duplicateCount = computed(() => Math.max(0, rawNodeCount.value - store.proxyPool.value.nodeCount));

function nodeSortRank(node: ProxyNode) {
  if (node.testStatus === "error" || node.testStatus === "invalid") return 2;
  if (node.latencyMs == null) return 1;
  return 0;
}
function compareNodes(left: ProxyNode, right: ProxyNode) {
  const leftRank = nodeSortRank(left);
  const rightRank = nodeSortRank(right);
  if (leftRank !== rightRank) return leftRank - rightRank;
  if (left.latencyMs != null && right.latencyMs != null && left.latencyMs !== right.latencyMs) {
    return left.latencyMs - right.latencyMs;
  }
  return left.name.localeCompare(right.name, "zh-CN");
}

// shallowRef：测速时只改节点字段，不重建大数组，避免全表 filter/sort 卡死。
const displayNodes = shallowRef<ProxyNode[]>([]);
const displayNodeIds = shallowRef<Set<string>>(new Set());

function resetProgressiveRender() {
  visibleNodeLimit.value = RENDER_CHUNK;
  visibleGroupLimit.value = 12;
  expandedGroupLimits.value = {};
  if (renderMoreRaf) {
    cancelAnimationFrame(renderMoreRaf);
    renderMoreRaf = 0;
  }
}

function rebuildDisplayNodes() {
  const maxLatency = Number(latencyFilter.value);
  const sourceName = selectedSource.value === "all"
    ? ""
    : (store.proxyPool.value.subscriptions.find((item) => item.id === selectedSource.value)?.name ?? "");
  const next = store.proxyPool.value.nodes
    .filter((node) => {
      if (sourceName && !node.subscriptionNames.includes(sourceName)) return false;
      // 仅展示已测通且延迟在阈值内的节点。
      if (node.latencyMs == null || node.testStatus !== "success") return false;
      if (node.latencyMs > maxLatency) return false;
      return true;
    })
    .sort(compareNodes);
  displayNodes.value = next;
  displayNodeIds.value = new Set(next.map((node) => node.id));
  resetProgressiveRender();
}

const filteredNodes = computed(() => displayNodes.value);
const renderedNodes = computed(() => displayNodes.value.slice(0, visibleNodeLimit.value));
const hasMoreNodes = computed(() => displayNodes.value.length > renderedNodes.value.length);

const ipAnalysisByNode = computed(() => {
  const map = new Map<string, { primaryIp: string; resolvedIps: string[]; countryCode: string; countryName: string; classification: string }>();
  for (const node of store.proxyPool.value.nodes) {
    map.set(node.id, {
      primaryIp: node.primaryIp || "",
      resolvedIps: node.primaryIp ? [node.primaryIp] : [],
      countryCode: node.countryCode || "ZZ",
      countryName: node.countryName || "未知地区",
      classification: node.classification || (node.primaryIp ? "public" : "unresolved"),
    });
  }
  return map;
});

const ipGroupSummary = computed(() => {
  const codes = new Set<string>();
  const ips = new Set<string>();
  let unresolved = 0;
  for (const node of displayNodes.value) {
    codes.add(node.countryCode || "ZZ");
    if (node.primaryIp) ips.add(node.primaryIp);
    else unresolved += 1;
  }
  return {
    groupCount: codes.size,
    uniqueIps: ips.size,
    unresolvedNodes: unresolved,
    totalNodes: displayNodes.value.length,
  };
});

const ipGroups = computed(() => {
  const groups = new Map<string, {
    key: string;
    label: string;
    classification: string;
    countryCode: string;
    countryName: string;
    nodeIds: string[];
    nodes: ProxyNode[];
  }>();
  for (const node of displayNodes.value) {
    const code = node.countryCode?.trim() || "ZZ";
    const name = node.countryName?.trim() || "未知地区";
    const classification = node.classification?.trim() || (code === "LOCAL" ? "local" : code === "ZZ" ? "unknown" : "public");
    const current = groups.get(code) ?? {
      key: code,
      label: name,
      classification,
      countryCode: code,
      countryName: name,
      nodeIds: [],
      nodes: [],
    };
    current.nodeIds.push(node.id);
    current.nodes.push(node);
    groups.set(code, current);
  }
  return [...groups.values()]
    .map((group) => ({ ...group, nodes: [...group.nodes].sort(compareNodes), nodeCount: group.nodes.length }))
    .sort((left, right) => {
      const rank = (code: string) => (code === "ZZ" ? 2 : code === "LOCAL" ? 1 : 0);
      const rankDiff = rank(left.countryCode) - rank(right.countryCode);
      if (rankDiff) return rankDiff;
      if (right.nodes.length !== left.nodes.length) return right.nodes.length - left.nodes.length;
      return left.countryName.localeCompare(right.countryName, "zh-CN");
    });
});

const renderedIpGroups = computed(() => ipGroups.value.slice(0, visibleGroupLimit.value));
const hasMoreGroups = computed(() => ipGroups.value.length > renderedIpGroups.value.length);

function groupRenderedNodes(group: { key: string; nodes: ProxyNode[] }) {
  const limit = expandedGroupLimits.value[group.key] ?? GROUP_NODE_CHUNK;
  return group.nodes.slice(0, limit);
}
function groupHasMoreNodes(group: { key: string; nodes: ProxyNode[] }) {
  return group.nodes.length > (expandedGroupLimits.value[group.key] ?? GROUP_NODE_CHUNK);
}
function revealMoreGroupNodes(groupKey: string, total: number) {
  const current = expandedGroupLimits.value[groupKey] ?? GROUP_NODE_CHUNK;
  expandedGroupLimits.value = {
    ...expandedGroupLimits.value,
    [groupKey]: Math.min(total, current + GROUP_NODE_CHUNK),
  };
}
function revealMoreNodes() {
  if (!hasMoreNodes.value) return;
  visibleNodeLimit.value = Math.min(
    displayNodes.value.length,
    visibleNodeLimit.value + RENDER_CHUNK,
  );
}
function revealMoreGroups() {
  if (!hasMoreGroups.value) return;
  visibleGroupLimit.value = Math.min(
    ipGroups.value.length,
    visibleGroupLimit.value + 8,
  );
}
function scheduleInitialChunks() {
  // 仅自动扩 1 次，避免一口气把几千节点全挂到 DOM。
  if (renderMoreRaf) return;
  renderMoreRaf = requestAnimationFrame(() => {
    renderMoreRaf = 0;
    if (nodeViewMode.value === "list" && visibleNodeLimit.value < Math.min(displayNodes.value.length, RENDER_CHUNK * 2)) {
      visibleNodeLimit.value = Math.min(displayNodes.value.length, RENDER_CHUNK * 2);
    }
    if (nodeViewMode.value === "ip" && visibleGroupLimit.value < Math.min(ipGroups.value.length, 16)) {
      visibleGroupLimit.value = Math.min(ipGroups.value.length, 16);
    }
  });
}

function syncSettings() {
  ignoreAddresses.value = store.proxyPool.value.ignoreAddresses;
  speedTestUrl.value = store.proxyPool.value.speedTestUrl;
  speedTestPreset.value = speedTestPresets.some((item) => item.value === speedTestUrl.value)
    ? speedTestUrl.value
    : "custom";
}
function onSpeedTestPresetChange() {
  if (speedTestPreset.value === "custom") {
    if (speedTestPresets.some((item) => item.value === speedTestUrl.value)) speedTestUrl.value = "";
    return;
  }
  speedTestUrl.value = speedTestPreset.value;
}
function openImportDialog() {
  resetSource();
  importDialogOpen.value = true;
}
function openEditDialog(source: ProxySubscription) {
  editingId.value = source.id;
  sourceName.value = source.name;
  sourceLinks.value = source.url;
  importDialogOpen.value = true;
}
function closeImportDialog() {
  importDialogOpen.value = false;
  resetSource();
}
function onBackdropClick(e: MouseEvent) {
  if (e.target === e.currentTarget) closeImportDialog();
}
function editSource(source: ProxySubscription) {
  openEditDialog(source);
}
function resetSource() { editingId.value = ""; sourceName.value = ""; sourceLinks.value = ""; }
async function submitSource() {
  message.value = "";
  try {
    await store.saveProxySubscription(sourceName.value, sourceLinks.value, editingId.value || undefined);
    resetSource(); message.value = "导入完成，重复节点已自动合并";
  } catch { /* store error */ }
}
async function removeSource(source: ProxySubscription) {
  if (deleteConfirmId.value !== source.id) {
    deleteConfirmId.value = source.id;
    return;
  }
  message.value = "";
  try {
    await store.deleteProxySubscription(source.id);
    if (selectedSource.value === source.id) selectedSource.value = "all";
    deleteConfirmId.value = "";
    message.value = "来源已删除";
  } catch { /* store error */ }
}
function cancelRemoveSource() { deleteConfirmId.value = ""; }
async function refreshSource(source: ProxySubscription) {
  message.value = "";
  try {
    const result = await store.refreshProxySubscription(source.id);
    message.value = result.discarded > 0
      ? `刷新完成，新增 ${result.added}，自动移除 ${result.discarded} 个非法节点`
      : `刷新完成，当前 ${result.total} 个原始节点`;
  } catch { /* store error */ }
}
async function refreshAll() {
  message.value = "";
  const result = await store.refreshAllProxySubscriptions();
  if (!store.proxyPoolError.value) {
    message.value = result.discarded > 0
      ? `全部来源刷新完成，自动移除 ${result.discarded} 个非法节点`
      : "全部来源刷新完成";
  }
}
async function saveSettings() {
  try {
    await store.saveProxyPoolSettings(ignoreAddresses.value, speedTestUrl.value);
    syncSettings(); message.value = "代理规则已保存，本地与局域网地址始终直连";
  } catch { /* store error */ }
}
async function activate(node: ProxyNode) {
  try { await store.activateProxyNode(node.id); message.value = `应用已开始通过"${node.name}"访问外部接口`; } catch { /* store error */ }
}
function selectNode(node: ProxyNode) {
  if (
    node.id === store.proxyPool.value.activeNodeId ||
    !store.proxyPool.value.runtimeAvailable ||
    Boolean(store.proxyPoolBusyId.value)
  ) return;
  void activate(node);
}
async function direct() {
  try { await store.clearActiveProxyNode(); message.value = "代理已关闭，应用外部接口恢复直连"; } catch { /* store error */ }
}
async function testNode(node: ProxyNode) {
  try { await store.testProxyNode(node.id); } catch { /* failed state is saved */ }
}
function isGroupCollapsed(groupKey: string) {
  return collapsedGroups.value.has(groupKey);
}
function toggleGroup(groupKey: string) {
  const next = new Set(collapsedGroups.value);
  if (next.has(groupKey)) next.delete(groupKey);
  else next.add(groupKey);
  collapsedGroups.value = next;
}
function groupBusyId(groupKey: string) {
  return `test-group:${groupKey}`;
}
function isGroupTesting(groupKey: string) {
  return store.proxyPoolBusyId.value === groupBusyId(groupKey);
}
function isBatchTesting() {
  return store.proxyPoolBusyId.value.startsWith("test-");
}
function requestCancelTest() {
  if (isBatchTesting()) cancelConfirmOpen.value = true;
}
async function confirmCancelTest() {
  cancelConfirmOpen.value = false;
  message.value = "正在取消测速任务…";
  try {
    const cancelled = await store.cancelProxyNodeTests();
    if (!cancelled) message.value = "测速任务已经结束";
  } catch { /* store error */ }
}
function testResultMessage(scope: string, result: Awaited<ReturnType<typeof store.testAllProxyNodes>>) {
  if (result.cancelled) return `${scope}已取消：完成 ${result.completed}/${result.total}`;
  return `${scope}完成：${result.succeeded} 个成功，${result.failed} 个失败`;
}
async function testAll() {
  message.value = "";
  const result = await store.testAllProxyNodes();
  message.value = testResultMessage("批量测速", result);
}
async function testGroup(group: { key: string; countryName: string; nodes: ProxyNode[] }) {
  message.value = "";
  // 分组测速只测当前已展开显示的节点，不测同组里未加载出来的。
  const visibleNodes = groupRenderedNodes(group);
  if (!visibleNodes.length) {
    message.value = `${group.countryName}当前没有可测速的显示节点`;
    return;
  }
  const result = await store.testProxyNodes(
    visibleNodes.map((node) => node.id),
    groupBusyId(group.key),
  );
  message.value = testResultMessage(`${group.countryName}测速`, result);
}
async function cleanInvalid() {
  try {
    await store.deleteInvalidProxyNodes();
    message.value = "无效节点已清理";
  } catch {
    /* error shown by store */
  }
}
function openCountryGroups() {
  nodeViewMode.value = "ip";
  message.value = `已按导入时的国家信息分组：${ipGroups.value.length} 个地区`;
}
function countryFlag(code: string) {
  if (!/^[A-Z]{2}$/.test(code)) return code === "LOCAL" ? "⌂" : "🌐";
  return String.fromCodePoint(...[...code].map((character) => 127397 + character.charCodeAt(0)));
}
function groupIpCount(nodes: ProxyNode[]) {
  const ips = new Set<string>();
  for (const node of nodes) {
    const analysis = ipAnalysisByNode.value.get(node.id);
    if (!analysis) continue;
    for (const ip of analysis.resolvedIps) ips.add(ip);
  }
  return ips.size;
}
function latencyClass(node: ProxyNode) {
  if (node.testStatus === "error" || node.testStatus === "invalid") return "bad";
  if (node.latencyMs == null) return "untested";
  if (node.latencyMs < 250) return "fast";
  if (node.latencyMs < 400) return "medium";
  return "slow";
}
function latencyText(node: ProxyNode) {
  if (store.testingNodeIds.value.has(node.id)) return "…";
  if (node.testStatus === "error" || node.testStatus === "invalid") return "Error";
  return node.latencyMs == null ? "测速" : String(node.latencyMs);
}
function protocolLabel(value: string) {
  const labels: Record<string, string> = {
    http: "Http",
    socks5: "Socks5",
    ss: "SS",
    ssr: "SSR",
    vmess: "VMess",
    vless: "VLess",
    trojan: "Trojan",
    hysteria: "Hy",
    hysteria2: "Hy2",
    tuic: "TUIC",
  };
  return labels[value] ?? value;
}
function endpoint(node: ProxyNode) { return `${node.server}:${node.port}`; }

onMounted(() => {
  syncSettings();
  rebuildDisplayNodes();
  void store.loadProxyPool();
});
onBeforeUnmount(() => {
  if (renderMoreRaf) cancelAnimationFrame(renderMoreRaf);
  if (liveRebuildTimer) window.clearTimeout(liveRebuildTimer);
});
watch(() => store.proxyPool.value, syncSettings, { deep: false });
watch(
  () => [selectedSource.value, latencyFilter.value, store.proxyNodesRevision.value] as const,
  () => {
    rebuildDisplayNodes();
    void nextTick(scheduleInitialChunks);
  },
);
watch(nodeViewMode, () => {
  resetProgressiveRender();
  void nextTick(scheduleInitialChunks);
});

// 测速进行中若已选择延迟阈值，则低频重建显示列表，
// 让达标节点逐步出现，又不会每帧全量 filter 6000+ 节点。
watch(
  () => [store.proxyTestProgress.value.completed, store.proxyPoolBusyId.value, latencyFilter.value] as const,
  () => {
    if (!store.proxyPoolBusyId.value.startsWith("test-")) return;
    if (liveRebuildTimer) return;
    liveRebuildTimer = window.setTimeout(() => {
      liveRebuildTimer = 0;
      const prevLimit = visibleNodeLimit.value;
      rebuildDisplayNodes();
      visibleNodeLimit.value = Math.max(prevLimit, RENDER_CHUNK);
    }, 400);
  },
);
</script>

<template>
  <main class="proxy-pool-page">
    <header class="proxy-pool-header">
      <div>
        <span class="proxy-pool-eyebrow">NETWORK ROUTING</span>
        <h1>代理池</h1>
        <p>选择代理节点后，应用的外部接口请求将统一通过该节点。</p>
      </div>
      <div class="proxy-header-actions">
        <div
          class="proxy-runtime-status"
          :class="{ active: store.proxyPool.value.enabled }"
          :title="store.proxyPool.value.activeNode?.name || (store.proxyPool.value.runtimeError || '当前直连')"
        >
          <i />
          <span>{{ store.proxyPool.value.activeNode?.name || (store.proxyPool.value.runtimeAvailable ? "当前直连" : "代理核心不可用") }}</span>
        </div>
        <button
          v-if="store.proxyPool.value.activeNodeId"
          class="secondary-button proxy-direct-button"
          type="button"
          :disabled="store.proxyPoolBusyId.value === 'clear'"
          @click="direct"
        >
          <span v-html="icons.wifiOff" />
          <span>关闭代理</span>
        </button>
        <button class="secondary-button proxy-settings-button" type="button" :aria-expanded="settingsOpen" @click="settingsOpen = !settingsOpen">
          <span v-html="icons.settings" />
          <span>代理规则</span>
        </button>
      </div>
    </header>

    <div class="proxy-pool-scroll">
      <section class="proxy-summary-grid" aria-label="代理池概览">
        <div><strong>{{ store.proxyPool.value.subscriptionCount }}</strong><span>导入来源</span></div>
        <div><strong>{{ store.proxyPool.value.nodeCount }}</strong><span>去重节点</span></div>
        <div><strong>{{ duplicateCount }}</strong><span>已合并重复</span></div>
        <div class="proxy-summary-endpoint">
          <strong>{{ store.proxyPool.value.activeNode?.name || "直连" }}</strong>
          <span>{{ store.proxyPool.value.activeNode ? endpoint(store.proxyPool.value.activeNode) : "当前外部请求不使用代理节点" }}</span>
        </div>
      </section>

      <section v-if="settingsOpen" class="proxy-settings-panel">
        <div class="proxy-section-title">
          <div><strong>请求规则</strong><span>配置代理测速地址与必须保持直连的目标。</span></div>
        </div>
        <div class="proxy-connection-grid">
          <label class="proxy-speed-field">
            <span>测速地址</span>
            <select class="proxy-speed-select" v-model="speedTestPreset" @change="onSpeedTestPresetChange">
              <option v-for="preset in speedTestPresets" :key="preset.value" :value="preset.value">{{ preset.label }}</option>
              <option value="custom">自定义地址</option>
            </select>
            <input v-if="speedTestPreset === 'custom'" v-model="speedTestUrl" placeholder="输入 http(s) 测速地址" />
          </label>
          <label class="proxy-ignore-field">
            <span>忽略地址</span>
            <textarea v-model="ignoreAddresses" rows="4" placeholder="127.0.0.1&#10;192.168.0.0/16&#10;localhost" />
            <small>每行或逗号分隔，支持域名、通配符和 CIDR；本地地址始终直连。</small>
          </label>
        </div>
        <div class="proxy-connection-actions">
          <button class="primary-button" type="button" @click="saveSettings">保存规则</button>
        </div>
      </section>

      <div v-if="store.proxyPoolError.value" class="proxy-alert is-error" role="alert">
        <span v-html="icons.info" /><p>{{ store.proxyPoolError.value }}</p>
      </div>
      <div v-else-if="message" class="proxy-alert is-success" role="status">
        <span v-html="icons.check" /><p>{{ message }}</p>
      </div>

      <section class="proxy-nodes-panel">
        <div class="proxy-node-toolbar">
          <div class="proxy-node-heading">
            <strong>代理节点</strong>
            <span>显示 {{ renderedNodes.length }}/{{ filteredNodes.length }} · ≤{{ latencyFilter }}ms · 共 {{ store.proxyPool.value.nodeCount }}</span>
          </div>
          <div class="proxy-node-filters">
            <select class="proxy-source-select" v-model="selectedSource" aria-label="筛选导入来源">
              <option value="all">全部来源</option>
              <option v-for="sub in store.proxyPool.value.subscriptions" :key="sub.id" :value="sub.id">{{ sub.name }}</option>
            </select>
            <select class="proxy-latency-select" v-model="latencyFilter" aria-label="按延迟显示节点">
              <option v-for="option in latencyFilterOptions" :key="option.value" :value="option.value">{{ option.label }}</option>
            </select>
          </div>
          <div class="proxy-node-actions">
            <button class="secondary-button proxy-import-button" type="button" @click="openImportDialog">
              <span v-html="icons.plus" /><span>导入来源</span>
            </button>
            <button
              class="secondary-button"
              :class="{ danger: isBatchTesting() }"
              type="button"
              :disabled="!store.proxyPool.value.nodes.length || (Boolean(store.proxyPoolBusyId.value) && !isBatchTesting()) || (store.testingNodeIds.value.size > 0 && !isBatchTesting())"
              @click="isBatchTesting() ? requestCancelTest() : testAll()"
            >
              <span v-html="isBatchTesting() ? icons.close : icons.pulse" />
              <span>{{ isBatchTesting() ? (store.proxyTestCancelling.value ? "正在取消…" : `取消测速 ${store.proxyTestProgress.value.completed}/${store.proxyTestProgress.value.total}`) : "批量测速" }}</span>
            </button>
            <button class="secondary-button" type="button" :disabled="store.proxyPoolBusyId.value === 'all' || !store.proxyPool.value.subscriptions.length" @click="refreshAll">
              <span v-html="icons.restore" /><span>刷新来源</span>
            </button>
            <button class="secondary-button" type="button" :disabled="!store.proxyPool.value.nodes.length" @click="openCountryGroups">
              <span v-html="icons.globe" /><span>{{ nodeViewMode === "ip" ? "刷新分组" : "国家分组" }}</span>
            </button>
            <button v-if="nodeViewMode === 'ip'" class="secondary-button" type="button" @click="nodeViewMode = 'list'">
              <span v-html="icons.rows" /><span>普通列表</span>
            </button>
            <button v-if="store.proxyPool.value.invalidNodeCount > 0" class="secondary-button danger" type="button" :disabled="store.proxyPoolBusyId.value === 'delete-invalid'" @click="cleanInvalid">
              <span v-html="icons.trash" /><span>清理无效 {{ store.proxyPool.value.invalidNodeCount }}</span>
            </button>
          </div>
        </div>
        <div v-if="nodeViewMode === 'list' && filteredNodes.length" class="proxy-node-grid">
          <article
            v-for="node in renderedNodes"
            :key="node.id"
            class="proxy-node-tile"
            :class="{
              active: node.id === store.proxyPool.value.activeNodeId,
              disabled: !store.proxyPool.value.runtimeAvailable || Boolean(store.proxyPoolBusyId.value),
            }"
            role="button"
            :tabindex="store.proxyPool.value.runtimeAvailable ? 0 : -1"
            :title="`${node.subscriptionNames.join(' / ')} · ${endpoint(node)}`"
            @click="selectNode(node)"
            @keydown.enter.prevent="selectNode(node)"
            @keydown.space.prevent="selectNode(node)"
          >
            <div class="proxy-node-tile-head">
              <strong>{{ node.name }}</strong>
              <button
                class="proxy-tile-latency"
                :class="latencyClass(node)"
                type="button"
                :disabled="Boolean(store.proxyPoolBusyId.value) || store.testingNodeIds.value.has(node.id)"
                title="重新测速"
                @click.stop="testNode(node)"
              ><span v-if="store.testingNodeIds.value.has(node.id)" class="proxy-node-loading" v-html="icons.restore" /><template v-else>{{ latencyText(node) }}</template></button>
            </div>
            <div class="proxy-node-tile-tags">
              <span>{{ protocolLabel(node.proxyType) }}</span>
              <span v-if="node.udp">UDP</span>
              <i v-if="node.id === store.proxyPool.value.activeNodeId">使用中</i>
            </div>
          </article>
        </div>
        <div v-if="nodeViewMode === 'list' && hasMoreNodes" class="proxy-render-more">
          <button class="secondary-button" type="button" @click="revealMoreNodes">
            继续加载 {{ Math.min(RENDER_CHUNK, filteredNodes.length - renderedNodes.length) }} 个节点
            （已显示 {{ renderedNodes.length }}/{{ filteredNodes.length }}）
          </button>
        </div>
        <div v-if="nodeViewMode === 'ip' && ipGroups.length" class="proxy-ip-groups">
          <div class="proxy-ip-summary" title="国家信息在导入/刷新时写入，分组时不再重新分析">
            <div><strong>{{ ipGroupSummary.groupCount }}</strong><span>国家/地区</span></div>
            <div><strong>{{ ipGroupSummary.uniqueIps }}</strong><span>已知 IP</span></div>
            <div><strong>{{ ipGroupSummary.unresolvedNodes }}</strong><span>无 IP 节点</span></div>
            <p><i class="active" />导入时已确定国家</p>
          </div>
          <section v-for="group in renderedIpGroups" :key="group.key" class="proxy-ip-group" :class="{ collapsed: isGroupCollapsed(group.key) }">
            <header class="proxy-ip-group-header">
              <button class="proxy-ip-group-toggle" type="button" :aria-expanded="!isGroupCollapsed(group.key)" @click="toggleGroup(group.key)">
                <span class="proxy-ip-group-chevron" :class="{ collapsed: isGroupCollapsed(group.key) }" v-html="icons.chevron" />
                <span class="proxy-ip-group-title">
                  <strong><b>{{ countryFlag(group.countryCode) }}</b>{{ group.countryName }}</strong>
                  <small>{{ group.countryCode === "LOCAL" ? "LOCAL" : group.countryCode }} · 显示 {{ Math.min(groupRenderedNodes(group).length, group.nodes.length) }}/{{ group.nodes.length }} · {{ groupIpCount(group.nodes) }} 个 IP</small>
                </span>
              </button>
              <div class="proxy-ip-group-actions">
                <button
                  class="proxy-group-test-button"
                  :class="{ active: isGroupTesting(group.key) }"
                  type="button"
                  :disabled="!store.proxyPool.value.runtimeAvailable || !groupRenderedNodes(group).length || (Boolean(store.proxyPoolBusyId.value) && !isGroupTesting(group.key)) || (store.testingNodeIds.value.size > 0 && !isGroupTesting(group.key))"
                  @click.stop="isGroupTesting(group.key) ? requestCancelTest() : testGroup(group)"
                  :title="groupHasMoreNodes(group) ? `仅测当前显示的 ${groupRenderedNodes(group).length} 个节点` : `测速该组 ${groupRenderedNodes(group).length} 个节点`"
                >
                  <span v-html="isGroupTesting(group.key) ? icons.close : icons.pulse" />
                  <span>{{ isGroupTesting(group.key) ? (store.proxyTestCancelling.value ? "取消中…" : `取消 ${store.proxyTestProgress.value.completed}/${store.proxyTestProgress.value.total}`) : "测速" }}</span>
                </button>
                <i :class="`is-${group.classification}`" />
              </div>
            </header>
            <div v-if="!isGroupCollapsed(group.key)" class="proxy-node-grid">
              <article
                v-for="node in groupRenderedNodes(group)"
                :key="`${group.key}-${node.id}`"
                class="proxy-node-tile"
                :class="{
                  active: node.id === store.proxyPool.value.activeNodeId,
                  disabled: !store.proxyPool.value.runtimeAvailable || Boolean(store.proxyPoolBusyId.value),
                }"
                role="button"
                :tabindex="store.proxyPool.value.runtimeAvailable ? 0 : -1"
                :title="`${node.subscriptionNames.join(' / ')} · ${endpoint(node)}`"
                @click="selectNode(node)"
                @keydown.enter.prevent="selectNode(node)"
                @keydown.space.prevent="selectNode(node)"
              >
                <div class="proxy-node-tile-head">
                  <strong>{{ node.name }}</strong>
                  <button class="proxy-tile-latency" :class="latencyClass(node)" type="button" :disabled="Boolean(store.proxyPoolBusyId.value) || store.testingNodeIds.value.has(node.id)" title="重新测速" @click.stop="testNode(node)"><span v-if="store.testingNodeIds.value.has(node.id)" class="proxy-node-loading" v-html="icons.restore" /><template v-else>{{ latencyText(node) }}</template></button>
                </div>
                <div class="proxy-node-tile-tags">
                  <span>{{ protocolLabel(node.proxyType) }}</span>
                  <span v-if="node.udp">UDP</span>
                  <span v-if="ipAnalysisByNode.get(node.id)?.primaryIp" class="proxy-node-ip">{{ ipAnalysisByNode.get(node.id)?.primaryIp }}</span>
                  <i v-if="node.id === store.proxyPool.value.activeNodeId">使用中</i>
                </div>
              </article>
            </div>
            <div v-if="!isGroupCollapsed(group.key) && groupHasMoreNodes(group)" class="proxy-render-more is-inline">
              <button class="secondary-button" type="button" @click.stop="revealMoreGroupNodes(group.key, group.nodes.length)">
                继续加载该组 {{ Math.min(GROUP_NODE_CHUNK, group.nodes.length - groupRenderedNodes(group).length) }} 个
                （{{ groupRenderedNodes(group).length }}/{{ group.nodes.length }}）
              </button>
            </div>
          </section>
        </div>
        <div v-if="nodeViewMode === 'ip' && hasMoreGroups" class="proxy-render-more">
          <button class="secondary-button" type="button" @click="revealMoreGroups">
            继续加载分组 （已显示 {{ renderedIpGroups.length }}/{{ ipGroups.length }}）
          </button>
        </div>
        <div v-if="(nodeViewMode === 'list' && !filteredNodes.length) || (nodeViewMode === 'ip' && !ipGroups.length)" class="proxy-node-empty">
          <span v-html="icons.globe" />
          <strong>{{
            !store.proxyPool.value.nodes.length
              ? "导入链接后，去重节点会显示在这里"
              : "当前 ≤" + latencyFilter + "ms 范围内没有节点，可先批量测速或放宽阈值"
          }}</strong>
        </div>
      </section>
    </div>
  </main>

  <Teleport to="body">
    <div v-if="cancelConfirmOpen" class="proxy-test-cancel-backdrop" @click.self="cancelConfirmOpen = false">
      <section class="proxy-test-cancel-dialog" role="alertdialog" aria-modal="true" aria-labelledby="proxy-test-cancel-title">
        <span class="proxy-test-cancel-icon" v-html="icons.info" />
        <div>
          <strong id="proxy-test-cancel-title">取消当前测速任务？</strong>
          <p>已完成的节点结果会保留，正在请求及等待队列中的节点将立即停止。</p>
        </div>
        <footer>
          <button class="secondary-button" type="button" @click="cancelConfirmOpen = false">继续测速</button>
          <button class="primary-button danger" type="button" @click="confirmCancelTest">取消任务</button>
        </footer>
      </section>
    </div>
  </Teleport>

  <!-- 导入来源弹窗 -->
  <Teleport to="body">
    <div v-if="importDialogOpen" class="proxy-import-backdrop" @click="onBackdropClick">
      <section class="proxy-import-dialog" role="dialog" aria-modal="true">
        <header class="proxy-import-dialog-header">
          <div>
            <strong>{{ editingId ? "编辑来源" : "导入来源" }}</strong>
            <span>粘贴订阅链接或节点链接</span>
          </div>
          <button class="icon-button proxy-import-close" type="button" title="关闭" @click="closeImportDialog" v-html="icons.close" />
        </header>
        <div class="proxy-import-dialog-body">
          <form class="proxy-subscription-form" @submit.prevent="submitSource">
            <input v-model="sourceName" required placeholder="来源名称" />
            <textarea v-model="sourceLinks" required rows="4" placeholder="https://example.com/sub&#10;或粘贴多行 vmess://、ss://、trojan://…" />
            <div><button v-if="editingId" class="secondary-button" type="button" @click="resetSource">取消编辑</button><button class="primary-button" type="submit" :disabled="Boolean(store.proxyPoolBusyId.value)">{{ editingId ? "保存并导入" : "导入代理" }}</button></div>
          </form>
          <div v-if="store.proxyPool.value.subscriptions.length" class="proxy-import-list-heading">
            <strong>已导入来源</strong><span>{{ store.proxyPool.value.subscriptions.length }} 个</span>
          </div>
          <div class="proxy-subscription-list">
            <article v-for="source in store.proxyPool.value.subscriptions" :key="source.id" class="proxy-subscription-card" :class="{ selected: selectedSource === source.id }" @click="selectedSource = selectedSource === source.id ? 'all' : source.id">
              <header><div><strong>{{ source.name }}</strong><span>{{ source.nodeCount }} 个原始节点</span></div><i :class="{ error: source.lastError }" /></header>
              <p>{{ source.url }}</p><small v-if="source.lastError">{{ source.lastError }}</small>
              <footer v-if="deleteConfirmId !== source.id"><button class="text-button" type="button" @click.stop="editSource(source)">编辑</button><button class="text-button" type="button" :disabled="store.proxyPoolBusyId.value === source.id" @click.stop="refreshSource(source)">刷新</button><button class="text-button danger" type="button" @click.stop="removeSource(source)">删除</button></footer>
              <footer v-else class="proxy-delete-confirm"><span>确定删除？</span><button class="text-button" type="button" @click.stop="cancelRemoveSource">取消</button><button class="text-button danger" type="button" :disabled="store.proxyPoolBusyId.value === source.id" @click.stop="removeSource(source)">确认删除</button></footer>
            </article>
            <div v-if="!store.proxyPool.value.subscriptions.length" class="proxy-side-empty">粘贴订阅地址或节点链接开始导入</div>
          </div>
        </div>
      </section>
    </div>
  </Teleport>
</template>
