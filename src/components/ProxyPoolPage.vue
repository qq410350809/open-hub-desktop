<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, watch } from "vue";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import { usePreferences } from "../composables/usePreferences";
import { KERNEL_DOWNLOAD_MIRRORS } from "../composables/useProxyPool";
import CustomSelect from "./CustomSelect.vue";
import type { ProxyChannel, ProxyNode, ProxySubscription } from "../types";

const store = useStore();
const { preferences, updatePreferences } = usePreferences();

const kernelParsedVersion = computed(() => {
  const raw = store.kernelStatus?.value?.version || "";
  if (!raw) return { tag: "未安装", arch: "" };
  const tagMatch = raw.match(/v\d+\.\d+(\.\d+)?/i);
  const tag = tagMatch ? tagMatch[0] : (raw.split(" ")[0] || raw);
  let arch = "";
  if (/darwin/i.test(raw)) arch = "macOS";
  else if (/windows/i.test(raw)) arch = "Windows";
  else if (/linux/i.test(raw)) arch = "Linux";

  if (/arm64|aarch64/i.test(raw)) arch += " ARM64";
  else if (/amd64|x86_64|x64/i.test(raw)) arch += " x64";

  return { tag, arch: arch.trim() };
});

// —— 来源与过滤状态 ——
const sourceName = ref("");
const sourceLinks = ref("");
const editingId = ref("");
const selectedSource = ref("all");
const nodeSearchQuery = ref("");

// 延迟级别过滤
const latencyFilter = ref<"500" | "1000" | "2000" | "error" | "all">("1000");
const latencyFilterOptions = [
  { value: "500", text: "≤ 500ms" },
  { value: "1000", text: "≤ 1000ms" },
  { value: "2000", text: "≤ 2000ms" },
  { value: "error", text: "失败/超时" },
  { value: "all", text: "全部节点" },
];

const sourceOptions = computed(() => [
  { value: "all", text: `全部来源 (${store.proxyPool.value.nodeCount})` },
  ...store.proxyPool.value.subscriptions.map((sub) => ({
    value: sub.id,
    text: `${sub.name} (${sub.nodeCount})`,
  })),
]);

const channels = computed(() => store.proxyPool.value.channels);
const settingsOpen = ref(false);
const ignoreAddresses = ref("");
const message = ref("");
const importDialogOpen = ref(false);
const deleteConfirmId = ref("");
const channelDialogOpen = ref(false);
const channelEditingId = ref("");
const channelName = ref("");
const channelSelectedNodeId = ref("");
const channelNodeQuery = ref("");
const channelAssignedProfileIds = ref<Set<string>>(new Set());
const deleteChannelConfirmId = ref("");
const nodeViewMode = ref<"list" | "ip">(preferences.proxyNodeViewMode === "country" ? "ip" : "list");
const collapsedGroups = ref<Set<string>>(new Set());
const cancelConfirmOpen = ref(false);

// —— 渐进式渲染分页参数 ——
const RENDER_CHUNK = 120;
const GROUP_NODE_CHUNK = 80;
const visibleNodeLimit = ref(RENDER_CHUNK);
const visibleGroupLimit = ref(12);
const expandedGroupLimits = ref<Record<string, number>>({});
let renderMoreRaf = 0;

// —— 统计衍生指标 ——
const rawNodeCount = computed(() =>
  store.proxyPool.value.subscriptions.reduce((sum, item) => sum + item.nodeCount, 0),
);
const duplicateCount = computed(() =>
  Math.max(0, rawNodeCount.value - store.proxyPool.value.nodeCount),
);
const assignedAccountCount = computed(() =>
  channels.value.reduce((sum, channel) => sum + channel.accountCount, 0),
);

const fastNodesCount = computed(() =>
  store.proxyPool.value.nodes.filter(
    (n) => n.testStatus === "success" && n.latencyMs != null && n.latencyMs <= 500,
  ).length,
);

const goodNodesCount = computed(() =>
  store.proxyPool.value.nodes.filter(
    (n) => n.testStatus === "success" && n.latencyMs != null && n.latencyMs <= 1000,
  ).length,
);

// —— 排序与节点列表构建 ——
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
  const filter = latencyFilter.value;
  const sName = selectedSource.value === "all"
    ? ""
    : (store.proxyPool.value.subscriptions.find((item) => item.id === selectedSource.value)?.name ?? "");
  const query = nodeSearchQuery.value.trim().toLowerCase();

  const next = store.proxyPool.value.nodes
    .filter((node) => {
      if (sName && !node.subscriptionNames.includes(sName)) return false;
      if (query) {
        const match = [
          node.name,
          node.server,
          String(node.port),
          node.primaryIp,
          node.countryName,
          node.countryCode,
          node.proxyType,
        ].some((val) => val && String(val).toLowerCase().includes(query));
        if (!match) return false;
      }
      if (filter === "all") return node.testStatus !== "invalid";
      if (filter === "error") {
        return (
          node.testStatus === "error" ||
          node.testStatus === "invalid" ||
          (node.testStatus === "success" && node.latencyMs == null)
        );
      }
      const maxLatency = Number(filter);
      if (node.latencyMs == null || node.testStatus !== "success") return false;
      if (node.latencyMs > maxLatency) return false;
      return true;
    })
    .sort(compareNodes);

  displayNodes.value = filter === "all" ? next.slice(0, 3000) : next;
  displayNodeIds.value = new Set(displayNodes.value.map((node) => node.id));
  resetProgressiveRender();
}

const filteredNodes = computed(() => displayNodes.value);
const renderedNodes = computed(() => displayNodes.value.slice(0, visibleNodeLimit.value));
const hasMoreNodes = computed(() => displayNodes.value.length > renderedNodes.value.length);

// —— IP 与国家地区分组 ——
const ipAnalysisByNode = computed(() => {
  const map = new Map<
    string,
    {
      primaryIp: string;
      resolvedIps: string[];
      countryCode: string;
      countryName: string;
      classification: string;
    }
  >();
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
  const groups = new Map<
    string,
    {
      key: string;
      label: string;
      classification: string;
      countryCode: string;
      countryName: string;
      nodeIds: string[];
      nodes: ProxyNode[];
    }
  >();
  for (const node of displayNodes.value) {
    const code = node.countryCode?.trim() || "ZZ";
    const name = node.countryName?.trim() || "未知地区";
    const classification =
      node.classification?.trim() ||
      (code === "LOCAL" ? "local" : code === "ZZ" ? "unknown" : "public");
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
    .map((group) => ({
      ...group,
      nodes: [...group.nodes].sort(compareNodes),
      nodeCount: group.nodes.length,
    }))
    .sort((left, right) => {
      const rank = (code: string) => (code === "LOCAL" ? 1 : code === "ZZ" ? 2 : 0);
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
  visibleGroupLimit.value = Math.min(ipGroups.value.length, visibleGroupLimit.value + 8);
}
function scheduleInitialChunks() {
  if (renderMoreRaf) return;
  renderMoreRaf = requestAnimationFrame(() => {
    renderMoreRaf = 0;
    if (
      nodeViewMode.value === "list" &&
      visibleNodeLimit.value < Math.min(displayNodes.value.length, RENDER_CHUNK * 2)
    ) {
      visibleNodeLimit.value = Math.min(displayNodes.value.length, RENDER_CHUNK * 2);
    }
    if (
      nodeViewMode.value === "ip" &&
      visibleGroupLimit.value < Math.min(ipGroups.value.length, 16)
    ) {
      visibleGroupLimit.value = Math.min(ipGroups.value.length, 16);
    }
  });
}

// —— 规则设置 ——
function syncSettings() {
  ignoreAddresses.value = store.proxyPool.value.ignoreAddresses;
}
function openSettings() {
  settingsOpen.value = true;
  document.body.classList.add("modal-open");
  void store.loadMihomoKernelStatus();
}
function closeSettings() {
  settingsOpen.value = false;
  document.body.classList.remove("modal-open");
}

async function saveSettings() {
  try {
    await store.saveProxyPoolSettings(ignoreAddresses.value);
    syncSettings();
    closeSettings();
    message.value = "代理规则已保存，本地与局域网地址始终保持直连";
  } catch {
    /* error handled in store */
  }
}

// —— 导入来源管理 ——
function openImportDialog() {
  resetSource();
  importDialogOpen.value = true;
  document.body.classList.add("modal-open");
}
function openEditDialog(source: ProxySubscription) {
  editingId.value = source.id;
  sourceName.value = source.name;
  sourceLinks.value = source.url;
  importDialogOpen.value = true;
  document.body.classList.add("modal-open");
}
function closeImportDialog() {
  importDialogOpen.value = false;
  resetSource();
  document.body.classList.remove("modal-open");
}
function editSource(source: ProxySubscription) {
  openEditDialog(source);
}
function resetSource() {
  editingId.value = "";
  sourceName.value = "";
  sourceLinks.value = "";
}

async function submitSource() {
  message.value = "";
  try {
    const result = await store.saveProxySubscription(
      sourceName.value,
      sourceLinks.value,
      editingId.value || undefined,
    );
    resetSource();
    message.value =
      result.discarded > 0
        ? `导入完成：${result.total} 个节点，过滤 ${result.discarded} 个非法节点`
        : `导入完成：${result.total} 个节点，新增 ${result.added}`;
  } catch {
    /* store error */
  }
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
  } catch {
    /* store error */
  }
}
function cancelRemoveSource() {
  deleteConfirmId.value = "";
}

async function refreshSource(source: ProxySubscription) {
  message.value = "";
  try {
    const result = await store.refreshProxySubscription(source.id);
    message.value =
      result.discarded > 0
        ? `刷新完成，新增 ${result.added}，自动移除 ${result.discarded} 个非法节点`
        : `刷新完成，当前 ${result.total} 个原始节点`;
  } catch {
    /* store error */
  }
}

async function refreshAll() {
  message.value = "";
  const result = await store.refreshAllProxySubscriptions();
  if (!store.proxyPoolError.value) {
    message.value =
      result.discarded > 0
        ? `全部来源刷新完成，自动移除 ${result.discarded} 个非法节点`
        : "全部来源刷新完成";
  }
}

// —— 通道配置 ——
function channelBusyId(channel: ProxyChannel) {
  return `test-channel-${channel.id}`;
}
function isChannelTesting(channel: ProxyChannel) {
  return store.channelTestBusyId.value === channelBusyId(channel);
}
function downloadRateText(latencyMs: number | null | undefined) {
  if (latencyMs == null) return "待测速";
  const seconds = Math.max(latencyMs, 1) / 1000;
  const mbps = 500_000 / 1_000_000 / seconds;
  if (mbps >= 100) return `${Math.round(mbps)}MB/s`;
  if (mbps >= 1) return `${mbps.toFixed(1)}MB/s`;
  return `${Math.round(mbps * 1000)}KB/s`;
}

type ProxyPoolAccountOption = {
  profileId: string;
  profileName: string;
  accountName: string;
  sites: { siteId: string; siteName: string; apiBaseUrl: string }[];
};

const proxyPoolAccounts = computed<ProxyPoolAccountOption[]>(() => {
  const byProfile = new Map<string, ProxyPoolAccountOption>();
  for (const site of store.sites.value) {
    if (!site.useProxyPool) continue;
    const sessions = store.chromeUsageAccounts.value[site.id] ?? [];
    for (const session of sessions) {
      let entry = byProfile.get(session.profileId);
      if (!entry) {
        entry = {
          profileId: session.profileId,
          profileName: session.profileName,
          accountName: session.accountName,
          sites: [],
        };
        byProfile.set(session.profileId, entry);
      }
      if (!entry.sites.some((item) => item.siteId === site.id)) {
        entry.sites.push({ siteId: site.id, siteName: site.name, apiBaseUrl: site.apiBaseUrl });
      }
    }
  }
  return [...byProfile.values()].sort((left, right) =>
    (left.accountName || left.profileName).localeCompare(right.accountName || right.profileName, "zh-CN"),
  );
});

const accountChannelLabels = computed(() => {
  const map = new Map<string, string>();
  for (const channel of channels.value) {
    for (const account of channel.accounts) {
      if (!map.has(account.profileId)) map.set(account.profileId, channel.name);
    }
  }
  return map;
});

const channelCandidateNodes = computed(() => {
  const query = channelNodeQuery.value.trim().toLowerCase();
  return store.proxyPool.value.nodes
    .filter(
      (node) =>
        node.channelTestStatus === "success" &&
        node.channelLatencyMs != null &&
        node.channelLatencyMs <= 500,
    )
    .filter((node) => {
      if (!query) return true;
      return [node.name, node.countryName, node.countryCode, node.server].some((value) =>
        value.toLowerCase().includes(query),
      );
    })
    .sort(
      (left, right) =>
        (left.channelLatencyMs ?? Number.POSITIVE_INFINITY) -
        (right.channelLatencyMs ?? Number.POSITIVE_INFINITY),
    );
});

function openChannelDialog(channel?: ProxyChannel) {
  channelEditingId.value = channel?.id ?? "";
  channelName.value = channel?.name ?? "";
  channelSelectedNodeId.value = channel?.nodeId ?? "";
  channelNodeQuery.value = "";
  channelAssignedProfileIds.value = new Set((channel?.accounts ?? []).map((account) => account.profileId));
  channelDialogOpen.value = true;
  document.body.classList.add("modal-open");
}
function closeChannelDialog() {
  channelDialogOpen.value = false;
  channelEditingId.value = "";
  channelName.value = "";
  channelSelectedNodeId.value = "";
  channelNodeQuery.value = "";
  channelAssignedProfileIds.value = new Set();
  document.body.classList.remove("modal-open");
}
function addChannel() {
  openChannelDialog();
}
function toggleChannelAccount(profileId: string) {
  const next = new Set(channelAssignedProfileIds.value);
  if (next.has(profileId)) next.delete(profileId);
  else next.add(profileId);
  channelAssignedProfileIds.value = next;
}
function isChannelAccountLocked(profileId: string) {
  return (
    !channelAssignedProfileIds.value.has(profileId) && accountChannelLabels.value.has(profileId)
  );
}
function selectChannelNode(nodeId: string) {
  channelSelectedNodeId.value = nodeId;
}
async function testChannelNodes() {
  message.value = "";
  try {
    await store.testProxyChannelNodes(channelEditingId.value || "");
    message.value = "测速完成，请选择节点后保存";
  } catch {
    /* store error */
  }
}
async function saveChannel() {
  message.value = "";
  if (!channelName.value.trim()) {
    message.value = "请输入通道名称";
    return;
  }
  try {
    const state = await store.saveProxyChannel(channelName.value, channelEditingId.value || undefined);
    const channelId =
      state.channels.find((item) => item.id === channelEditingId.value)?.id ??
      state.channels.find((item) => item.name === channelName.value)?.id ??
      state.defaultChannelId;
    if (channelId && channelSelectedNodeId.value) {
      await store.setProxyChannelNode(channelId, channelSelectedNodeId.value);
    }
    if (channelId) {
      const previous = new Set(
        (state.channels.find((item) => item.id === channelId)?.accounts ?? []).map(
          (account) => account.profileId,
        ),
      );
      for (const account of proxyPoolAccounts.value) {
        if (
          channelAssignedProfileIds.value.has(account.profileId) &&
          !previous.has(account.profileId)
        ) {
          await store.assignAccountProxyChannel(account.profileId, channelId);
        } else if (
          !channelAssignedProfileIds.value.has(account.profileId) &&
          previous.has(account.profileId)
        ) {
          await store.unassignAccountProxyChannel(account.profileId);
        }
      }
    }
    closeChannelDialog();
    message.value = `通道「${channelName.value}」已保存`;
  } catch {
    /* store error */
  }
}
async function removeChannel(channel: ProxyChannel) {
  if (channels.value.length <= 1) {
    message.value = "至少保留一个代理通道";
    return;
  }
  if (deleteChannelConfirmId.value !== channel.id) {
    deleteChannelConfirmId.value = channel.id;
    return;
  }
  message.value = "";
  try {
    await store.deleteProxyChannel(channel.id);
    deleteChannelConfirmId.value = "";
    message.value = `通道「${channel.name}」已删除`;
  } catch {
    /* store error */
  }
}

// —— 节点测速与操作 ——
async function testNode(node: ProxyNode) {
  try {
    await store.testProxyNode(node.id);
  } catch {
    /* failed state is saved */
  }
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
function selectedSourceName() {
  if (selectedSource.value === "all") return "";
  return (
    store.proxyPool.value.subscriptions.find((item) => item.id === selectedSource.value)?.name ?? ""
  );
}
function selectedSourceLabel() {
  return selectedSourceName() || "全部来源";
}
function nodesForSelectedSource() {
  const sName = selectedSourceName();
  if (!sName) return store.proxyPool.value.nodes;
  return store.proxyPool.value.nodes.filter((node) => node.subscriptionNames.includes(sName));
}
function testableNodesForSelectedSource() {
  return nodesForSelectedSource();
}
function requestCancelTest() {
  if (isBatchTesting()) {
    cancelConfirmOpen.value = true;
    document.body.classList.add("modal-open");
  }
}
function closeCancelTest() {
  cancelConfirmOpen.value = false;
  document.body.classList.remove("modal-open");
}
async function confirmCancelTest() {
  closeCancelTest();
  message.value = "正在取消测速任务…";
  try {
    const cancelled = await store.cancelProxyNodeTests();
    message.value = cancelled
      ? isBatchTesting()
        ? "已请求取消，正在停止当前测速…"
        : "测速任务已取消"
      : "测速任务已经结束或不在测速中";
  } catch (error) {
    message.value = `取消请求已发送（${String(error)}）`;
  }
}
function testResultMessage(
  scope: string,
  result: Awaited<ReturnType<typeof store.testAllProxyNodes>>,
) {
  if (result.cancelled) return `${scope}已取消：完成 ${result.completed}/${result.total}`;
  return `${scope}完成：${result.succeeded} 个成功，${result.failed} 个失败`;
}
async function testAll() {
  message.value = "";
  const sName = selectedSourceName();
  if (!sName) {
    message.value = "正在装载节点并并行测速…";
    const result = await store.testAllProxyNodes();
    message.value = testResultMessage("全部来源测速", result);
    return;
  }
  const nodes = testableNodesForSelectedSource();
  if (!nodes.length) {
    message.value = `${selectedSourceLabel()}当前没有可测速节点`;
    return;
  }
  message.value = `正在装载 ${nodes.length} 个节点并并行测速…`;
  const result = await store.testProxyNodes(
    nodes.map((node) => node.id),
    `test-source-${selectedSource.value}`,
  );
  if (
    !result.cancelled &&
    result.succeeded === 0 &&
    result.failed > 0 &&
    ["500", "1000", "2000"].includes(latencyFilter.value)
  ) {
    latencyFilter.value = "error";
  }
  message.value = testResultMessage(`${selectedSourceLabel()}测速`, result);
}
async function testGroup(group: { key: string; countryName: string; nodes: ProxyNode[] }) {
  message.value = "";
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
  updatePreferences({ proxyNodeViewMode: "country" });
}
function openNormalList() {
  nodeViewMode.value = "list";
  updatePreferences({ proxyNodeViewMode: "list" });
}
function countryFlag(code?: string) {
  if (!code || !/^[A-Z]{2}$/.test(code)) return code === "LOCAL" ? "🏠" : "🌐";
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
  return latencyClassForMs(node.latencyMs, node.testStatus);
}
function channelLatencyClass(node: ProxyNode) {
  return latencyClassForMs(node.channelLatencyMs, node.channelTestStatus);
}
function latencyClassForMs(latencyMs: number | null | undefined, testStatus: string) {
  if (testStatus === "error" || testStatus === "invalid") return "bad";
  if (latencyMs == null) return "untested";
  if (latencyMs < 250) return "fast";
  if (latencyMs < 500) return "good";
  if (latencyMs < 1000) return "medium";
  return "slow";
}
function latencyText(node: ProxyNode) {
  if (store.testingNodeIds.value.has(node.id)) return "…";
  if (node.testStatus === "error" || node.testStatus === "invalid") return "Error";
  return node.latencyMs == null ? "测速" : `${node.latencyMs}ms`;
}
function protocolLabel(value: string) {
  const labels: Record<string, string> = {
    http: "HTTP",
    socks5: "Socks5",
    ss: "SS",
    ssr: "SSR",
    vmess: "VMess",
    vless: "VLess",
    trojan: "Trojan",
    hysteria: "Hy",
    hysteria2: "Hy2",
    tuic: "TUIC",
    anytls: "AnyTLS",
  };
  return labels[value] ?? value?.toUpperCase();
}
function endpoint(node: ProxyNode) {
  return `${node.server}:${node.port}`;
}
function nodeCountryLabel(node: ProxyNode) {
  if (node.countryName && node.countryName !== "未知地区") return node.countryName;
  if (node.countryCode && node.countryCode !== "ZZ") return node.countryCode;
  return "";
}
function nodeSourceLabel(node: ProxyNode) {
  if (!node.subscriptionNames?.length) return "未分来源";
  if (node.subscriptionNames.length === 1) return node.subscriptionNames[0];
  return `${node.subscriptionNames[0]} +${node.subscriptionNames.length - 1}`;
}
function nodeDetailTitle(node: ProxyNode) {
  const lines = [
    node.name,
    `协议：${protocolLabel(node.proxyType)}${node.udp ? " · UDP" : ""}`,
    `地址：${endpoint(node)}`,
    node.primaryIp ? `IP：${node.primaryIp}` : "",
    nodeCountryLabel(node)
      ? `地区：${nodeCountryLabel(node)}${node.countryCode && node.countryCode !== "ZZ" ? ` (${node.countryCode})` : ""}`
      : "",
    `来源：${node.subscriptionNames.join(" / ") || "未分来源"}`,
    node.latencyMs != null ? `延迟：${node.latencyMs}ms` : "延迟：未测速",
  ].filter(Boolean);
  return lines.join("\n");
}
function sourceProgress(sourceId: string) {
  return store.proxySourceProgress.value[sourceId] || null;
}
function sourceProgressText(sourceId: string) {
  const progress = sourceProgress(sourceId);
  if (!progress) return "";
  if (progress.stage === "saving" && progress.total > 0) {
    return `${progress.message}（${progress.completed}/${progress.total}）`;
  }
  return progress.message;
}
function isSourceParsing(sourceId: string) {
  const progress = sourceProgress(sourceId);
  return Boolean(
    progress &&
      progress.stage !== "done" &&
      progress.stage !== "error" &&
      (store.proxyPoolBusyId.value === sourceId ||
        store.proxyPoolBusyId.value === "all" ||
        progress.status === "running"),
  );
}

onMounted(() => {
  syncSettings();
  rebuildDisplayNodes();
  void store.loadProxyPool();
});
onBeforeUnmount(() => {
  if (renderMoreRaf) cancelAnimationFrame(renderMoreRaf);
});
watch(() => store.proxyPool.value, syncSettings, { deep: false });
watch(
  () => [selectedSource.value, latencyFilter.value, nodeSearchQuery.value, store.proxyNodesRevision.value] as const,
  () => {
    rebuildDisplayNodes();
    void nextTick(scheduleInitialChunks);
  },
);
watch(nodeViewMode, () => {
  resetProgressiveRender();
  void nextTick(scheduleInitialChunks);
});
</script>

<template>
  <main class="proxy-pool-page pp-dashboard">
    <!-- 顶部宏观智控驾驶舱 (Cockpit Bar) -->
    <header class="pp-cockpit-bar">
      <div class="pp-cockpit-left">
        <div class="pp-brand-section">
          <div class="pp-eyebrow-row">
            <span class="pp-live-dot" />
            <span class="pp-eyebrow-text">智能代理池管理</span>
            <span class="pp-eyebrow-badge">固定出口 · 智能代理池</span>
          </div>
          <div class="pp-title-row">
            <h1>代理池管理</h1>
          </div>
          <p class="pp-cockpit-subtitle">
            智能订阅去重 · 节点批量并发测速 · Chrome 账号固定通道出口
          </p>
        </div>
      </div>

      <div class="pp-cockpit-right">
        <button
          type="button"
          class="pp-btn-secondary"
          :class="{ active: settingsOpen }"
          title="配置直连名单与本地绕过规则"
          @click="openSettings"
        >
          <span v-html="icons.settings" />
          <span>代理规则</span>
        </button>

        <button
          type="button"
          class="pp-btn-secondary"
          title="导入与管理订阅链接及单节点"
          @click="openImportDialog"
        >
          <span v-html="icons.plus" />
          <span>导入来源</span>
          <span class="pp-count-chip">{{ store.proxyPool.value.subscriptions.length }}</span>
        </button>

        <button
          type="button"
          class="pp-btn-primary"
          :class="{ 'is-danger': isBatchTesting() }"
          :disabled="
            !testableNodesForSelectedSource().length ||
            (Boolean(store.proxyPoolBusyId.value) && !isBatchTesting()) ||
            (store.testingNodeIds.value.size > 0 && !isBatchTesting())
          "
          :title="selectedSource === 'all' ? '测速全部来源节点' : `只测速当前选中来源：${selectedSourceLabel()}`"
          @click="isBatchTesting() ? requestCancelTest() : testAll()"
        >
          <span
            :class="{ 'is-spinning': isBatchTesting() && !store.proxyTestCancelling.value }"
            v-html="isBatchTesting() ? icons.close : icons.pulse"
          />
          <span>{{
            isBatchTesting()
              ? store.proxyTestCancelling.value
                ? "正在取消…"
                : `取消测速 ${store.proxyTestProgress.value.completed}/${store.proxyTestProgress.value.total}`
              : selectedSource === "all"
                ? "批量测速"
                : "测速此来源"
          }}</span>
        </button>
      </div>
    </header>

    <!-- 状态反馈横幅 -->
    <div v-if="store.proxyPoolError.value" class="pp-error-banner" role="alert">
      <span class="pp-error-icon" v-html="icons.info" />
      <div class="pp-error-content">
        <strong>代理池状态提示</strong>
        <p>{{ store.proxyPoolError.value }}</p>
      </div>
    </div>
    <div v-else-if="message" class="pp-success-banner" role="status">
      <span class="pp-success-icon" v-html="icons.check" />
      <div class="pp-success-content">
        <p>{{ message }}</p>
      </div>
    </div>

    <!-- 核心滚动视口 -->
    <div class="pp-scroll-viewport">
      <!-- 4 大核心 KPI Bento 指标卡 (Stats Deck) -->
      <section class="pp-stats-deck" aria-label="代理池核心指标概览">
        <!-- 卡片 1: 代理通道网络 -->
        <div class="pp-stat-card">
          <div class="pp-stat-header">
            <span class="pp-stat-tag is-blue">
              <span v-html="icons.sliders" />
              <span>通道网络</span>
            </span>
            <span class="pp-stat-pill is-blue">{{ channels.length }} 个通道</span>
          </div>
          <div class="pp-stat-main">
            <strong>{{ assignedAccountCount }}</strong>
            <span class="pp-stat-unit">已绑定账号</span>
          </div>
          <div class="pp-stat-footer">
            <span>共 {{ channels.length }} 个通道可分配</span>
          </div>
        </div>

        <!-- 卡片 2: 节点总容量与去重 -->
        <div class="pp-stat-card">
          <div class="pp-stat-header">
            <span class="pp-stat-tag is-emerald">
              <span v-html="icons.database" />
              <span>节点仓库</span>
            </span>
            <span class="pp-stat-pill is-emerald">去重库</span>
          </div>
          <div class="pp-stat-main">
            <strong>{{ store.proxyPool.value.nodeCount }}</strong>
            <span class="pp-stat-unit">去重节点</span>
          </div>
          <div class="pp-stat-footer">
            <span>原始 <strong>{{ rawNodeCount }}</strong> 个 · 自动合并 <strong>{{ duplicateCount }}</strong> 重复</span>
          </div>
        </div>

        <!-- 卡片 3: 极速健康节点 -->
        <div class="pp-stat-card">
          <div class="pp-stat-header">
            <span class="pp-stat-tag is-purple">
              <span v-html="icons.pulse" />
              <span>速度与健康</span>
            </span>
            <span class="pp-stat-pill is-purple">≤500ms 极速</span>
          </div>
          <div class="pp-stat-main">
            <strong>{{ fastNodesCount }}</strong>
            <span class="pp-stat-unit">可用节点</span>
          </div>
          <div class="pp-stat-footer">
            <span>≤1000ms: <strong>{{ goodNodesCount }}</strong> 个 · Cloudflare 500KB 实测</span>
          </div>
        </div>

        <!-- 卡片 4: 地区覆盖 -->
        <div class="pp-stat-card">
          <div class="pp-stat-header">
            <span class="pp-stat-tag is-orange">
              <span v-html="icons.globe" />
              <span>地区覆盖</span>
            </span>
            <span class="pp-stat-pill is-orange">GeoIP 智能</span>
          </div>
          <div class="pp-stat-main">
            <strong>{{ ipGroupSummary.groupCount }}</strong>
            <span class="pp-stat-unit">国家/地区</span>
          </div>
          <div class="pp-stat-footer">
            <span>已解析独立已知 IP: <strong>{{ ipGroupSummary.uniqueIps }}</strong> 个</span>
          </div>
        </div>
      </section>

      <!-- 代理通道出口管理阵列 -->
      <section class="pp-channels-section" aria-label="代理通道">
        <div class="pp-channels-header">
          <div class="pp-channels-title-group">
            <h2>固定出口通道</h2>
            <p>每个 Chrome 账号归属一个通道，账号下的所有站点共享该通道固定出口与实测节点</p>
          </div>
          <button type="button" class="pp-btn-secondary pp-btn-sm" @click="addChannel">
            <span v-html="icons.plus" />
            <span>添加通道</span>
          </button>
        </div>

        <div class="pp-channels-grid">
          <article
            v-for="channel in channels"
            :key="channel.id"
            class="pp-channel-card"
            :class="{ 'is-testing': isChannelTesting(channel) }"
            @click="openChannelDialog(channel)"
          >
            <div class="pp-channel-head">
              <div class="pp-channel-name-row">
                <strong>{{ channel.name }}</strong>
              </div>
              <span class="pp-channel-account-count">
                {{ channel.accountCount }} 个账号
              </span>
            </div>

            <div class="pp-channel-node-preview">
              <span class="pp-channel-node-icon" v-html="icons.activity" />
              <div class="pp-channel-node-info">
                <span v-if="channel.node" class="pp-channel-node-name" :title="channel.node.name">
                  {{ channel.node.name }}
                </span>
                <span v-else-if="isChannelTesting(channel)" class="pp-channel-testing-text">
                  正在测速中…
                </span>
                <span v-else class="pp-channel-unset-text">未固定出口节点</span>
              </div>
              <span
                v-if="channel.node"
                class="pp-channel-rate-badge"
                :class="channelLatencyClass(channel.node)"
              >
                {{ downloadRateText(channel.node.channelLatencyMs ?? channel.node.latencyMs) }}
              </span>
            </div>

            <div class="pp-channel-footer" @click.stop>
              <button
                type="button"
                class="pp-channel-act-btn"
                @click="openChannelDialog(channel)"
              >
                配置通道
              </button>
              <button
                type="button"
                class="pp-channel-act-btn is-danger"
                :disabled="channels.length <= 1 || Boolean(store.proxyPoolBusyId.value)"
                @click="removeChannel(channel)"
              >
                {{ deleteChannelConfirmId === channel.id ? "确认删除？" : "删除" }}
              </button>
            </div>
          </article>
        </div>
      </section>

      <!-- 交互式指令工具条 (Command & Filter Strip) -->
      <section class="pp-command-strip" aria-label="节点筛选与工具条">
        <div class="pp-strip-left">
          <!-- 视图模式切换 -->
          <div class="pp-view-switcher">
            <button
              type="button"
              class="pp-view-btn"
              :class="{ active: nodeViewMode === 'list' }"
              @click="openNormalList"
            >
              <span v-html="icons.rows" />
              <span>列表展示</span>
            </button>
            <button
              type="button"
              class="pp-view-btn"
              :class="{ active: nodeViewMode === 'ip' }"
              @click="openCountryGroups"
            >
              <span v-html="icons.globe" />
              <span>国家/地区分组</span>
            </button>
          </div>

          <div class="pp-strip-divider" />

          <!-- 来源快速切换下拉框 -->
          <CustomSelect
            class="pp-strip-dropdown"
            :options="sourceOptions"
            :model-value="selectedSource"
            aria-label="来源筛选"
            @update:model-value="selectedSource = String($event)"
          />

          <div class="pp-strip-divider" />

          <!-- 延迟范围下拉框 -->
          <CustomSelect
            class="pp-strip-dropdown"
            :options="latencyFilterOptions"
            :model-value="latencyFilter"
            aria-label="延迟范围筛选"
            @update:model-value="latencyFilter = $event as any"
          />
        </div>

        <div class="pp-strip-right">
          <!-- 搜索节点输入框 -->
          <div class="pp-search-box">
            <span class="pp-search-icon" v-html="icons.search" />
            <input
              v-model="nodeSearchQuery"
              class="pp-search-input"
              type="search"
              placeholder="搜索节点名称 / IP / 地区 / 协议…"
            />
            <button
              v-if="nodeSearchQuery"
              type="button"
              class="pp-search-clear"
              aria-label="清空搜索"
              @click="nodeSearchQuery = ''"
              v-html="icons.close"
            />
          </div>

          <button
            v-if="store.proxyPool.value.invalidNodeCount > 0"
            type="button"
            class="pp-btn-secondary is-danger pp-btn-sm"
            :disabled="store.proxyPoolBusyId.value === 'delete-invalid'"
            @click="cleanInvalid"
          >
            <span v-html="icons.trash" />
            <span>清理无效 {{ store.proxyPool.value.invalidNodeCount }}</span>
          </button>
        </div>
      </section>

      <!-- 节点呈现区域 (Node Presentation Area) -->
      <section class="pp-nodes-section" aria-label="代理节点展示">
        <!-- 模式 1: 列表模式 (List Grid) -->
        <div v-if="nodeViewMode === 'list' && filteredNodes.length" class="pp-nodes-grid">
          <article
            v-for="node in renderedNodes"
            :key="node.id"
            class="pp-node-card"
            :class="{ disabled: Boolean(store.proxyPoolBusyId.value) }"
            :title="nodeDetailTitle(node)"
          >
            <div class="pp-node-head">
              <div class="pp-node-title-group">
                <span class="pp-node-flag">{{ countryFlag(node.countryCode) }}</span>
                <strong class="pp-node-name">{{ node.name }}</strong>
              </div>
              <button
                type="button"
                class="pp-node-latency-btn"
                :class="latencyClass(node)"
                :disabled="Boolean(store.proxyPoolBusyId.value) || store.testingNodeIds.value.has(node.id)"
                @click.stop="testNode(node)"
              >
                <span v-if="store.testingNodeIds.value.has(node.id)" class="pp-mini-spinner" />
                <template v-else>{{ latencyText(node) }}</template>
              </button>
            </div>

            <div class="pp-node-endpoint">
              <code>{{ endpoint(node) }}</code>
            </div>

            <div class="pp-node-meta-row">
              <span v-if="nodeCountryLabel(node)" class="pp-node-region-chip">
                {{ nodeCountryLabel(node) }}
              </span>
              <span v-if="node.primaryIp || ipAnalysisByNode.get(node.id)?.primaryIp" class="pp-node-ip-chip">
                {{ node.primaryIp || ipAnalysisByNode.get(node.id)?.primaryIp }}
              </span>
              <span class="pp-node-source-chip">{{ nodeSourceLabel(node) }}</span>
            </div>

            <div class="pp-node-tags-row">
              <span class="pp-protocol-badge">{{ protocolLabel(node.proxyType) }}</span>
              <span v-if="node.udp" class="pp-sub-badge">UDP</span>
              <span v-if="node.cipher" class="pp-sub-badge">{{ node.cipher }}</span>
            </div>
          </article>
        </div>

        <!-- 模式 1: 列表加载更多按钮 -->
        <div v-if="nodeViewMode === 'list' && hasMoreNodes" class="pp-load-more-bar">
          <button type="button" class="pp-btn-secondary pp-load-more-btn" @click="revealMoreNodes">
            继续加载 {{ Math.min(RENDER_CHUNK, filteredNodes.length - renderedNodes.length) }} 个节点
            （已显示 {{ renderedNodes.length }}/{{ filteredNodes.length }}）
          </button>
        </div>

        <!-- 模式 2: 国家/地区分组模式 (Country Accordion Groups) -->
        <div v-if="nodeViewMode === 'ip' && ipGroups.length" class="pp-country-groups-container">
          <div class="pp-country-groups-header">
            <div class="pp-country-stat-pill">
              <strong>{{ ipGroupSummary.groupCount }}</strong>
              <span>国家/地区</span>
            </div>
            <div class="pp-country-stat-pill">
              <strong>{{ ipGroupSummary.uniqueIps }}</strong>
              <span>已知独立 IP</span>
            </div>
            <div class="pp-country-stat-pill">
              <strong>{{ ipGroupSummary.unresolvedNodes }}</strong>
              <span>无 IP 节点</span>
            </div>
          </div>

          <div class="pp-groups-list">
            <section
              v-for="group in renderedIpGroups"
              :key="group.key"
              class="pp-country-group-card"
              :class="{ 'is-collapsed': isGroupCollapsed(group.key) }"
            >
              <header class="pp-group-card-header">
                <button
                  type="button"
                  class="pp-group-toggle-btn"
                  :aria-expanded="!isGroupCollapsed(group.key)"
                  @click="toggleGroup(group.key)"
                >
                  <span class="pp-group-chevron" :class="{ 'is-collapsed': isGroupCollapsed(group.key) }">▼</span>
                  <span class="pp-group-flag">{{ countryFlag(group.countryCode) }}</span>
                  <div class="pp-group-title-info">
                    <strong>{{ group.countryName }}</strong>
                    <small>
                      {{ group.countryCode === "LOCAL" ? "LOCAL" : group.countryCode }} · 已显示 {{ Math.min(groupRenderedNodes(group).length, group.nodes.length) }}/{{ group.nodes.length }} 节点 · {{ groupIpCount(group.nodes) }} 个 IP
                    </small>
                  </div>
                </button>

                <div class="pp-group-actions">
                  <button
                    type="button"
                    class="pp-btn-secondary pp-btn-sm"
                    :class="{ active: isGroupTesting(group.key) }"
                    :disabled="
                      !store.proxyPool.value.runtimeAvailable ||
                      !groupRenderedNodes(group).length ||
                      (Boolean(store.proxyPoolBusyId.value) && !isGroupTesting(group.key)) ||
                      (store.testingNodeIds.value.size > 0 && !isGroupTesting(group.key))
                    "
                    @click.stop="isGroupTesting(group.key) ? requestCancelTest() : testGroup(group)"
                  >
                    <span v-html="isGroupTesting(group.key) ? icons.close : icons.pulse" />
                    <span>{{
                      isGroupTesting(group.key)
                        ? store.proxyTestCancelling.value
                          ? "取消中…"
                          : `取消 ${store.proxyTestProgress.value.completed}/${store.proxyTestProgress.value.total}`
                        : "本组测速"
                    }}</span>
                  </button>
                </div>
              </header>

              <div v-if="!isGroupCollapsed(group.key)" class="pp-group-body">
                <div class="pp-nodes-grid is-group-nodes">
                  <article
                    v-for="node in groupRenderedNodes(group)"
                    :key="`${group.key}-${node.id}`"
                    class="pp-node-card"
                    :class="{ disabled: Boolean(store.proxyPoolBusyId.value) }"
                    :title="nodeDetailTitle(node)"
                  >
                    <div class="pp-node-head">
                      <div class="pp-node-title-group">
                        <strong class="pp-node-name">{{ node.name }}</strong>
                      </div>
                      <button
                        type="button"
                        class="pp-node-latency-btn"
                        :class="latencyClass(node)"
                        :disabled="Boolean(store.proxyPoolBusyId.value) || store.testingNodeIds.value.has(node.id)"
                        @click.stop="testNode(node)"
                      >
                        <span v-if="store.testingNodeIds.value.has(node.id)" class="pp-mini-spinner" />
                        <template v-else>{{ latencyText(node) }}</template>
                      </button>
                    </div>

                    <div class="pp-node-endpoint">
                      <code>{{ endpoint(node) }}</code>
                    </div>

                    <div class="pp-node-meta-row">
                      <span v-if="node.primaryIp || ipAnalysisByNode.get(node.id)?.primaryIp" class="pp-node-ip-chip">
                        {{ node.primaryIp || ipAnalysisByNode.get(node.id)?.primaryIp }}
                      </span>
                      <span class="pp-node-source-chip">{{ nodeSourceLabel(node) }}</span>
                    </div>

                    <div class="pp-node-tags-row">
                      <span class="pp-protocol-badge">{{ protocolLabel(node.proxyType) }}</span>
                      <span v-if="node.udp" class="pp-sub-badge">UDP</span>
                      <span v-if="node.cipher" class="pp-sub-badge">{{ node.cipher }}</span>
                    </div>
                  </article>
                </div>

                <div v-if="groupHasMoreNodes(group)" class="pp-group-more-bar">
                  <button
                    type="button"
                    class="pp-btn-secondary pp-btn-sm"
                    @click.stop="revealMoreGroupNodes(group.key, group.nodes.length)"
                  >
                    展开该组更多 {{ Math.min(GROUP_NODE_CHUNK, group.nodes.length - groupRenderedNodes(group).length) }} 个节点
                  </button>
                </div>
              </div>
            </section>
          </div>

          <div v-if="hasMoreGroups" class="pp-load-more-bar">
            <button type="button" class="pp-btn-secondary pp-load-more-btn" @click="revealMoreGroups">
              继续加载更多地区分组 （已显示 {{ renderedIpGroups.length }}/{{ ipGroups.length }}）
            </button>
          </div>
        </div>

        <!-- 空数据提示 -->
        <div
          v-if="(nodeViewMode === 'list' && !filteredNodes.length) || (nodeViewMode === 'ip' && !ipGroups.length)"
          class="pp-empty-state"
        >
          <span class="pp-empty-icon" v-html="icons.globe" />
          <strong>{{
            !store.proxyPool.value.nodes.length
              ? "暂无可用代理节点，请点击右上角「导入来源」添加订阅"
              : latencyFilter === "error"
                ? "当前来源下没有失败/超时节点"
                : latencyFilter === "all"
                  ? "当前来源下暂无节点记录"
                  : `当前 ≤${latencyFilter}ms 范围内没有节点，可切换“全部节点”或点击“批量测速”`
          }}</strong>
        </div>
      </section>
    </div>

    <!-- ============================================================
         4 大独立全功能弹窗体系 (Dedicated Modals)
         ============================================================ -->

    <!-- 1. 导入来源管理弹窗 (Import Sources Modal) -->
    <Teleport to="body">
      <Transition name="pp-modal-fade">
        <div v-if="importDialogOpen" class="pp-modal-backdrop" @click.self="closeImportDialog">
          <section class="pp-modal-card is-import" role="dialog" aria-modal="true">
            <header class="pp-modal-header">
              <div class="pp-modal-title-group">
                <div class="pp-modal-eyebrow">代理订阅与节点管理</div>
                <h2>{{ editingId ? "编辑订阅来源" : "导入代理订阅与节点" }}</h2>
              </div>
              <button type="button" class="pp-modal-close-btn" aria-label="关闭" @click="closeImportDialog">×</button>
            </header>

            <div class="pp-modal-body">
              <!-- 导入表单卡片 -->
              <form class="pp-import-form-card" @submit.prevent="submitSource">
                <div class="pp-form-row">
                  <input
                    v-model="sourceName"
                    class="pp-input"
                    type="text"
                    required
                    placeholder="来源备注名称 (如: 主力订阅源)"
                  />
                </div>
                <div class="pp-form-row">
                  <textarea
                    v-model="sourceLinks"
                    class="pp-textarea"
                    required
                    rows="3"
                    placeholder="粘贴订阅链接 (https://...) 或多行单个节点链接 (vmess://, ss://, trojan://, vless://, hysteria2://...)"
                  />
                </div>
                <div class="pp-form-actions">
                  <button
                    v-if="editingId"
                    type="button"
                    class="pp-btn-secondary"
                    @click="resetSource"
                  >
                    取消编辑
                  </button>
                  <button
                    type="submit"
                    class="pp-btn-primary"
                    :disabled="Boolean(store.proxyPoolBusyId.value)"
                  >
                    <span v-html="icons.plus" />
                    <span>{{ editingId ? "保存并解析更新" : "确认导入代理" }}</span>
                  </button>
                </div>
              </form>

              <!-- 已导入来源列表 -->
              <div v-if="store.proxyPool.value.subscriptions.length" class="pp-subs-list-container">
                <div class="pp-subs-list-header">
                  <strong>已导入来源 ({{ store.proxyPool.value.subscriptions.length }})</strong>
                  <button
                    type="button"
                    class="pp-btn-secondary pp-btn-sm"
                    :disabled="store.proxyPoolBusyId.value === 'all'"
                    @click="refreshAll"
                  >
                    <span :class="{ 'is-spinning': store.proxyPoolBusyId.value === 'all' }" v-html="icons.restore" />
                    <span>{{ store.proxyPoolBusyId.value === "all" ? "全部刷新中…" : "刷新全部来源" }}</span>
                  </button>
                </div>

                <div class="pp-subs-cards-list">
                  <article
                    v-for="source in store.proxyPool.value.subscriptions"
                    :key="source.id"
                    class="pp-sub-item-card"
                    :class="{
                      'is-selected': selectedSource === source.id,
                      'is-parsing': isSourceParsing(source.id),
                      'is-error': Boolean(source.lastError) || sourceProgress(source.id)?.stage === 'error',
                    }"
                  >
                    <div class="pp-sub-card-main">
                      <div class="pp-sub-title-row">
                        <strong>{{ source.name }}</strong>
                        <span class="pp-sub-count-badge">{{ source.nodeCount }} 节点</span>
                      </div>
                      <p class="pp-sub-url-text" :title="source.url">{{ source.url }}</p>

                      <!-- 解析进度条 -->
                      <div
                        v-if="
                          isSourceParsing(source.id) ||
                          sourceProgress(source.id)?.stage === 'done' ||
                          sourceProgress(source.id)?.stage === 'error' ||
                          source.lastError
                        "
                        class="pp-sub-progress-box"
                      >
                        <div v-if="isSourceParsing(source.id)" class="pp-sub-progress-track">
                          <i
                            :style="{
                              width: sourceProgress(source.id)?.total
                                ? `${Math.min(100, Math.round(((sourceProgress(source.id)?.completed || 0) / (sourceProgress(source.id)?.total || 1)) * 100))}%`
                                : sourceProgress(source.id)?.stage === 'fetching'
                                  ? '35%'
                                  : sourceProgress(source.id)?.stage === 'parsing'
                                    ? '60%'
                                    : '80%',
                            }"
                          />
                        </div>
                        <small :class="{ 'is-error': source.lastError || sourceProgress(source.id)?.stage === 'error' }">
                          {{ source.lastError || sourceProgressText(source.id) }}
                        </small>
                      </div>
                    </div>

                    <div class="pp-sub-card-actions">
                      <template v-if="deleteConfirmId !== source.id">
                        <button type="button" class="pp-btn-secondary pp-btn-sm" @click="editSource(source)">
                          编辑
                        </button>
                        <button
                          type="button"
                          class="pp-btn-secondary pp-btn-sm"
                          :disabled="store.proxyPoolBusyId.value === source.id || store.proxyPoolBusyId.value === 'all'"
                          @click="refreshSource(source)"
                        >
                          {{ isSourceParsing(source.id) ? "解析中" : "刷新" }}
                        </button>
                        <button
                          type="button"
                          class="pp-btn-secondary is-danger pp-btn-sm"
                          :disabled="store.proxyPoolBusyId.value === source.id || store.proxyPoolBusyId.value === 'all'"
                          @click="removeSource(source)"
                        >
                          删除
                        </button>
                      </template>
                      <template v-else>
                        <span class="pp-confirm-text">确定删除？</span>
                        <button type="button" class="pp-btn-secondary pp-btn-sm" @click="cancelRemoveSource">
                          取消
                        </button>
                        <button
                          type="button"
                          class="pp-btn-primary is-danger pp-btn-sm"
                          :disabled="store.proxyPoolBusyId.value === source.id"
                          @click="removeSource(source)"
                        >
                          确认删除
                        </button>
                      </template>
                    </div>
                  </article>
                </div>
              </div>
            </div>

            <footer class="pp-modal-footer">
              <button type="button" class="pp-btn-cancel" @click="closeImportDialog">关闭</button>
            </footer>
          </section>
        </div>
      </Transition>
    </Teleport>

    <!-- 2. 代理通道配置弹窗 (Channel Config Modal) -->
    <Teleport to="body">
      <Transition name="pp-modal-fade">
        <div v-if="channelDialogOpen" class="pp-modal-backdrop" @click.self="closeChannelDialog">
          <section class="pp-modal-card is-channel-dialog" role="dialog" aria-modal="true">
            <header class="pp-modal-header">
              <div class="pp-modal-title-group">
                <div class="pp-modal-eyebrow">出口通道路由</div>
                <h2>{{ channelEditingId ? "配置代理通道" : "新建代理通道" }}</h2>
                <p>一个 Chrome 账号只归属一个固定通道出口，账号下的所有站点共享固定节点</p>
              </div>
              <button type="button" class="pp-modal-close-btn" aria-label="关闭" @click="closeChannelDialog">×</button>
            </header>

            <div class="pp-modal-body">
              <form class="pp-channel-config-form" @submit.prevent="saveChannel">
                <!-- 通道名称 -->
                <div class="pp-form-group">
                  <label class="pp-label">通道名称</label>
                  <input
                    v-model="channelName"
                    class="pp-input"
                    type="text"
                    required
                    placeholder="例如：香港专线固定出口"
                  />
                </div>

                <!-- 固定节点选择器 -->
                <div class="pp-form-group">
                  <div class="pp-label-row">
                    <label class="pp-label">固定出口节点 (≤500ms 极速候选)</label>
                    <button
                      type="button"
                      class="pp-btn-secondary pp-btn-sm"
                      :disabled="Boolean(store.proxyPoolBusyId.value) || Boolean(store.channelTestBusyId.value)"
                      @click="testChannelNodes"
                    >
                      <span v-html="icons.pulse" />
                      <span>{{ store.channelTestBusyId.value ? "测速中…" : "刷新通道候选测速" }}</span>
                    </button>
                  </div>

                  <div class="pp-channel-candidate-box">
                    <div class="pp-candidate-search-bar">
                      <span class="pp-search-icon" v-html="icons.search" />
                      <input
                        v-model="channelNodeQuery"
                        class="pp-search-input"
                        type="search"
                        placeholder="搜索候选节点名称 / 地区…"
                      />
                      <span class="pp-candidate-count-pill">{{ channelCandidateNodes.length }} 个候选节点</span>
                    </div>

                    <div class="pp-candidate-nodes-list">
                      <label
                        v-for="node in channelCandidateNodes"
                        :key="node.id"
                        class="pp-candidate-node-item"
                        :class="{ 'is-selected': channelSelectedNodeId === node.id }"
                      >
                        <input
                          type="radio"
                          name="channel-node"
                          :value="node.id"
                          :checked="channelSelectedNodeId === node.id"
                          @change="selectChannelNode(node.id)"
                        />
                        <span class="pp-candidate-flag">{{ countryFlag(node.countryCode) }}</span>
                        <div class="pp-candidate-info">
                          <strong>{{ node.name }}</strong>
                          <small>{{ [nodeCountryLabel(node), endpoint(node)].filter(Boolean).join(" · ") }}</small>
                        </div>
                        <span class="pp-candidate-rate-badge" :class="channelLatencyClass(node)">
                          {{ downloadRateText(node.channelLatencyMs) }}
                        </span>
                      </label>

                      <div v-if="!channelCandidateNodes.length" class="pp-candidate-empty">
                        <span v-html="icons.globe" />
                        <strong>{{ channelNodeQuery ? "没有匹配的候选节点" : "暂无 ≤500ms 的候选节点" }}</strong>
                        <small>可点击上方「刷新通道候选测速」或在主界面完成测速</small>
                      </div>
                    </div>
                  </div>
                </div>

                <!-- 分配归属账号 -->
                <div v-if="proxyPoolAccounts.length" class="pp-form-group">
                  <label class="pp-label">分配使用该通道的 Chrome 账号</label>
                  <p class="pp-hint-text">每个账号只能归属一个通道，已归属其他通道的账号不可重复勾选</p>

                  <div class="pp-account-bindings-list">
                    <div
                      v-for="account in proxyPoolAccounts"
                      :key="account.profileId"
                      class="pp-account-binding-item"
                      :class="{ 'is-locked': isChannelAccountLocked(account.profileId) }"
                    >
                      <label class="pp-account-checkbox-row">
                        <input
                          type="checkbox"
                          :checked="channelAssignedProfileIds.has(account.profileId)"
                          :disabled="isChannelAccountLocked(account.profileId)"
                          @change="toggleChannelAccount(account.profileId)"
                        />
                        <div class="pp-account-details">
                          <strong>{{ account.accountName || account.profileName }}</strong>
                          <small>
                            Profile: {{ account.profileId }}
                            <template v-if="isChannelAccountLocked(account.profileId)">
                              · 已绑定通道: {{ accountChannelLabels.get(account.profileId) }}
                            </template>
                          </small>
                        </div>
                      </label>

                      <div v-if="account.sites?.length" class="pp-account-sites-tags">
                        <span v-for="site in account.sites" :key="site.siteId" class="pp-site-tag">
                          {{ site.siteName }}
                        </span>
                      </div>
                    </div>
                  </div>
                </div>

                <div class="pp-modal-footer">
                  <button type="button" class="pp-btn-cancel" @click="closeChannelDialog">取消</button>
                  <button
                    type="submit"
                    class="pp-btn-primary"
                    :disabled="Boolean(store.proxyPoolBusyId.value)"
                  >
                    保存通道配置
                  </button>
                </div>
              </form>
            </div>
          </section>
        </div>
      </Transition>
    </Teleport>

    <!-- 3. 代理规则与直连名单抽屉/弹窗 (Settings Modal) -->
    <Teleport to="body">
      <Transition name="pp-modal-fade">
        <div v-if="settingsOpen" class="pp-modal-backdrop" @click.self="closeSettings">
          <section class="pp-modal-card is-settings" role="dialog" aria-modal="true">
            <header class="pp-modal-header">
              <div class="pp-modal-title-group">
                <div class="pp-modal-eyebrow">BYPASS & ROUTING RULES</div>
                <h2>代理规则与直连名单</h2>
                <p>配置必须始终直连、不走代理池出口的目标 IP 与域名</p>
              </div>
              <button type="button" class="pp-modal-close-btn" aria-label="关闭" @click="closeSettings">×</button>
            </header>

            <div class="pp-modal-body">
              <!-- 核心组件下载加速源选择栏（共用于内核与 GeoIP） -->
              <div class="pp-mirror-banner">
                <div class="pp-mirror-header">
                  <span class="pp-mirror-title">组件下载加速源</span>
                  <small class="pp-mirror-subtitle">共用于内置 Mihomo 内核与 GeoIP 数据库的极速拉取与在线更新</small>
                </div>
                <div class="component-mirror-controls">
                  <select v-model="store.kernelSelectedMirror.value" class="kernel-mirror-select">
                    <option v-for="m in KERNEL_DOWNLOAD_MIRRORS" :key="m.value" :value="m.value">
                      {{ m.text }}
                    </option>
                  </select>
                  <input
                    v-if="store.kernelSelectedMirror?.value === 'custom'"
                    v-model="store.kernelCustomMirror.value"
                    type="text"
                    class="kernel-custom-mirror-input"
                    placeholder="https://your-mirror.com/"
                  />
                </div>
              </div>

              <!-- 专属美化版 Mihomo 内核卡片 -->
              <div class="kernel-card">
                <div class="kernel-card-header">
                  <div class="kernel-card-identity">
                    <div class="kernel-icon-badge">
                      <span v-html="icons.wifi" />
                    </div>
                    <div class="kernel-identity-text">
                      <div class="kernel-title-row">
                        <span class="kernel-title">内置 Mihomo 内核</span>
                        <span v-if="store.kernelLoading?.value" class="kernel-badge is-loading">检测中…</span>
                        <span v-else-if="store.kernelStatus?.value?.installed" class="kernel-badge is-ready">
                          <i class="dot" /> 运行就绪
                        </span>
                        <span v-else class="kernel-badge is-missing">
                          <i class="dot" /> 待安装
                        </span>
                      </div>
                      <div class="kernel-subtitle">
                        <template v-if="store.kernelStatus?.value?.installed">
                          <span>版本</span>
                          <strong class="kernel-version-tag">{{ kernelParsedVersion.tag }}</strong>
                          <span v-if="kernelParsedVersion.arch" class="kernel-arch-tag">{{ kernelParsedVersion.arch }}</span>
                          <span
                            v-if="store.kernelStatus?.value?.latestVersion && store.kernelStatus.value.latestVersion !== kernelParsedVersion.tag"
                            class="kernel-update-pill"
                          >
                            发现新版本 {{ store.kernelStatus.value.latestVersion }}
                          </span>
                        </template>
                        <span v-else class="kernel-missing-text">未检测到内置内核，点击右侧按钮一键下载</span>
                      </div>
                    </div>
                  </div>

                  <!-- 操作按钮组 -->
                  <div class="kernel-card-actions">
                    <button
                      v-if="store.kernelStatus?.value?.installed"
                      type="button"
                      class="kernel-btn-secondary"
                      :disabled="store.kernelLoading?.value || store.kernelChecking?.value || store.kernelDownloading?.value"
                      @click="store.checkMihomoKernelUpdate()"
                    >
                      <span class="btn-icon" :class="{ 'is-spinning': store.kernelChecking?.value }" v-html="icons.restore" />
                      <span>{{ store.kernelChecking?.value ? "检查中…" : "检查更新" }}</span>
                    </button>

                    <button
                      type="button"
                      class="kernel-btn-primary"
                      :class="{ 'is-accent': !store.kernelStatus?.value?.installed }"
                      :disabled="store.kernelLoading?.value || store.kernelDownloading?.value"
                      @click="store.downloadOrUpdateMihomoKernel()"
                    >
                      <span class="btn-icon" v-html="icons.download || icons.restore" />
                      <span>{{ store.kernelDownloading?.value ? "正在下载…" : (store.kernelStatus?.value?.installed ? "重新下载 / 更新" : "一键下载内核") }}</span>
                    </button>
                  </div>
                </div>

                <!-- 下载进度条 -->
                <div v-if="store.kernelDownloading?.value" class="kernel-progress-wrapper">
                  <div class="kernel-progress-track">
                    <div
                      class="kernel-progress-fill"
                      :style="{ width: `${Math.max(4, Math.round((store.kernelDownloadProgress?.value?.progress ?? 0) * 100))}%` }"
                    />
                  </div>
                    <div class="kernel-progress-meta">
                      <span class="kernel-progress-msg">{{ store.kernelDownloadProgress?.value?.message }}</span>
                      <span class="kernel-progress-pct">{{ Math.round((store.kernelDownloadProgress?.value?.progress ?? 0) * 100) }}%</span>
                    </div>
                </div>
              </div>

              <!-- 专属 GeoIP 数据库管理卡片 -->
              <div class="kernel-card geoip-card">
                <div class="kernel-card-header">
                  <div class="kernel-card-identity">
                    <div class="kernel-icon-badge is-geoip">
                      <span v-html="icons.globe" />
                    </div>
                    <div class="kernel-identity-text">
                      <div class="kernel-title-row">
                        <span class="kernel-title">GeoIP 国家与地域数据库</span>
                        <span v-if="store.geoipLoading?.value" class="kernel-badge is-loading">检测中…</span>
                        <span v-else-if="store.geoipStatus?.value?.installed" class="kernel-badge is-ready">
                          <i class="dot" /> 运行就绪
                        </span>
                        <span v-else class="kernel-badge is-missing">
                          <i class="dot" /> 待安装
                        </span>
                      </div>
                      <div class="kernel-subtitle">
                        <template v-if="store.geoipStatus?.value?.installed">
                          <span>数据库大小</span>
                          <strong class="kernel-version-tag">{{ store.geoipStatus.value.fileSizeFormatted }}</strong>
                          <span v-if="store.geoipStatus.value.updatedAt" class="kernel-arch-tag">更新于 {{ store.geoipStatus.value.updatedAt }}</span>
                        </template>
                        <span v-else class="kernel-missing-text">未检测到本地 GeoIP 数据库，建议下载以获得精准节点国旗与地域解析</span>
                      </div>
                    </div>
                  </div>

                  <!-- 操作按钮组 -->
                  <div class="kernel-card-actions">
                    <button
                      type="button"
                      class="kernel-btn-primary"
                      :class="{ 'is-accent': !store.geoipStatus?.value?.installed }"
                      :disabled="store.geoipLoading?.value || store.geoipDownloading?.value"
                      @click="store.downloadOrUpdateGeoip()"
                    >
                      <span class="btn-icon" :class="{ 'is-spinning': store.geoipDownloading?.value }" v-html="icons.download || icons.restore" />
                      <span>{{ store.geoipDownloading?.value ? "正在下载…" : (store.geoipStatus?.value?.installed ? "重新下载 / 更新" : "一键下载 GeoIP") }}</span>
                    </button>
                  </div>
                </div>

                <!-- 下载进度条 -->
                <div v-if="store.geoipDownloading?.value" class="kernel-progress-wrapper">
                  <div class="kernel-progress-track">
                    <div
                      class="kernel-progress-fill"
                      :style="{ width: `${Math.max(4, Math.round((store.geoipDownloadProgress?.value?.progress ?? 0) * 100))}%` }"
                    />
                  </div>
                  <div class="kernel-progress-meta">
                    <span class="kernel-progress-msg">{{ store.geoipDownloadProgress?.value?.message }}</span>
                    <span class="kernel-progress-pct">{{ Math.round((store.geoipDownloadProgress?.value?.progress ?? 0) * 100) }}%</span>
                  </div>
                </div>
              </div>

              <div class="pp-settings-field-group">
                <label class="pp-label">直连与忽略地址名单</label>
                <textarea
                  v-model="ignoreAddresses"
                  class="pp-textarea is-rules"
                  rows="6"
                  placeholder="127.0.0.1&#10;192.168.0.0/16&#10;localhost&#10;*.local"
                />
                <small class="pp-hint-text">
                  每行或英文逗号分隔，支持 IP、CIDR 掩码段（如 10.0.0.0/8）、通配符域名（如 *.corp.internal）。本地及回环地址始终保持直连。
                </small>
              </div>
            </div>

            <footer class="pp-modal-footer">
              <button type="button" class="pp-btn-cancel" @click="closeSettings">取消</button>
              <button type="button" class="pp-btn-primary" @click="saveSettings">保存规则</button>
            </footer>
          </section>
        </div>
      </Transition>
    </Teleport>

    <!-- 4. 测速取消确认弹窗 (Cancel Confirm Modal) -->
    <Teleport to="body">
      <Transition name="pp-modal-fade">
        <div v-if="cancelConfirmOpen" class="pp-modal-backdrop" @click.self="closeCancelTest">
          <section class="pp-modal-card is-confirm" role="alertdialog" aria-modal="true">
            <div class="pp-confirm-body">
              <span class="pp-confirm-icon" v-html="icons.alert" />
              <div class="pp-confirm-text-group">
                <h2>确定取消当前测速任务？</h2>
                <p>已完成的节点测速结果将自动保留，剩余队列中正在等待的节点将立即停止测速。</p>
              </div>
            </div>
            <footer class="pp-modal-footer">
              <button type="button" class="pp-btn-cancel" @click="closeCancelTest">继续测速</button>
              <button type="button" class="pp-btn-primary is-danger" @click="confirmCancelTest">取消任务</button>
            </footer>
          </section>
        </div>
      </Transition>
    </Teleport>
  </main>
</template>

<style scoped>
.pp-dashboard {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  background: var(--page-bg);
  color: var(--text);
  overflow: hidden;
}

/* ============================================================
   1. 顶部全景智控驾驶舱 (Cockpit Bar)
   ============================================================ */
.pp-cockpit-bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 12px 20px;
  background: var(--surface);
  border-bottom: 1px solid var(--line);
  flex-shrink: 0;
}

.pp-cockpit-left {
  display: flex;
  align-items: center;
  gap: 12px;
}

.pp-brand-section {
  display: flex;
  flex-direction: column;
  gap: 1px;
}

.pp-eyebrow-row {
  display: flex;
  align-items: center;
  gap: 6px;
}

.pp-live-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: #10b981;
  box-shadow: 0 0 8px #10b981;
  animation: ppPulse 2s infinite ease-in-out;
}

@keyframes ppPulse {
  0%, 100% { opacity: 1; transform: scale(1); }
  50% { opacity: 0.4; transform: scale(1.25); }
}

.pp-eyebrow-text {
  font-size: 10px;
  font-weight: 750;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--brand);
}

.pp-eyebrow-badge {
  padding: 1px 6px;
  border-radius: var(--r-full);
  background: color-mix(in srgb, var(--brand) 12%, transparent);
  color: var(--brand);
  font-size: 9.5px;
  font-weight: 700;
}

.pp-title-row h1 {
  font-size: 18px;
  font-weight: 750;
  color: var(--text);
  margin: 0;
  line-height: 1.2;
}

.pp-cockpit-subtitle {
  font-size: 11px;
  color: var(--muted);
  margin: 0;
}

.pp-cockpit-subtitle strong {
  color: var(--text);
  font-weight: 600;
}

.pp-cockpit-right {
  display: flex;
  align-items: center;
  gap: 8px;
}

.pp-btn-primary {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 32px;
  padding: 0 12px;
  border-radius: var(--r-md, 8px);
  border: 1px solid color-mix(in srgb, var(--brand, #388bfd) 35%, transparent);
  background: color-mix(in srgb, var(--brand, #388bfd) 12%, var(--surface));
  color: var(--brand-deep, var(--brand, #388bfd));
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.15s ease;
  white-space: nowrap;
}

.pp-btn-primary:hover:not(:disabled) {
  background: color-mix(in srgb, var(--brand, #388bfd) 20%, var(--surface));
  border-color: var(--brand);
  transform: translateY(-1px);
}

.pp-btn-primary.is-danger {
  border-color: rgba(239, 68, 68, 0.4);
  background: rgba(239, 68, 68, 0.12);
  color: #ef4444;
}

.pp-btn-primary.is-danger:hover:not(:disabled) {
  background: rgba(239, 68, 68, 0.2);
  border-color: #ef4444;
}

.pp-btn-primary:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.pp-btn-primary :deep(svg) {
  width: 13px;
  height: 13px;
}

.pp-btn-secondary {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 32px;
  padding: 0 11px;
  border-radius: var(--r-md, 8px);
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--text);
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.15s ease;
  white-space: nowrap;
}

.pp-btn-secondary:hover {
  background: var(--surface-hover);
  border-color: var(--line-hover);
  transform: translateY(-1px);
}

.pp-btn-secondary.active {
  background: var(--brand-soft);
  border-color: var(--brand);
  color: var(--brand-deep);
}

.pp-btn-secondary.is-danger {
  color: #ef4444;
}

.pp-btn-secondary.is-danger:hover {
  background: rgba(239, 68, 68, 0.1);
  border-color: rgba(239, 68, 68, 0.3);
}

.pp-btn-secondary :deep(svg) {
  width: 13px;
  height: 13px;
}

.pp-btn-sm {
  height: 26px;
  padding: 0 8px;
  font-size: 11px;
}

.pp-count-chip {
  padding: 1px 5px;
  border-radius: var(--r-full);
  background: var(--page-bg);
  color: var(--muted);
  font-size: 9.5px;
  font-weight: 700;
}

.pp-mini-spinner {
  width: 10px;
  height: 10px;
  border: 2px solid currentColor;
  border-top-color: transparent;
  border-radius: 50%;
  animation: ppSpin 0.8s infinite linear;
}

.is-spinning {
  animation: ppSpin 1s infinite linear;
}

@keyframes ppSpin {
  100% { transform: rotate(360deg); }
}

.pp-error-banner,
.pp-success-banner {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 20px;
  font-size: 11.5px;
  flex-shrink: 0;
}

.pp-error-banner {
  background: rgba(239, 68, 68, 0.1);
  border-bottom: 1px solid rgba(239, 68, 68, 0.2);
  color: #ef4444;
}

.pp-success-banner {
  background: rgba(16, 185, 129, 0.1);
  border-bottom: 1px solid rgba(16, 185, 129, 0.2);
  color: #10b981;
}

/* ============================================================
   2. 滚动视口与内容布局 (Scroll Viewport)
   ============================================================ */
.pp-scroll-viewport {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  overflow-x: hidden;
  padding: 12px 18px 24px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

/* 4 Bento KPI Cards */
.pp-stats-deck {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 10px;
  flex-shrink: 0;
}

@media (max-width: 1100px) {
  .pp-stats-deck {
    grid-template-columns: repeat(2, 1fr);
  }
}

.pp-stat-card {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 10px);
  padding: 10px 14px;
  display: flex;
  flex-direction: column;
  min-height: 82px;
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.02);
  transition: all 0.15s ease;
}

.pp-stat-card:hover {
  border-color: var(--line-hover);
}

.pp-stat-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 6px;
  margin-bottom: 4px;
}

.pp-stat-tag {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 10px;
  font-weight: 750;
  letter-spacing: 0.05em;
  text-transform: uppercase;
}

.pp-stat-tag :deep(svg) {
  width: 12px;
  height: 12px;
}

.pp-stat-tag.is-blue { color: #3b82f6; }
.pp-stat-tag.is-emerald { color: #10b981; }
.pp-stat-tag.is-purple { color: #a855f7; }
.pp-stat-tag.is-orange { color: #f97316; }

.pp-stat-pill {
  padding: 1px 6px;
  border-radius: var(--r-full);
  font-size: 9.5px;
  font-weight: 700;
}

.pp-stat-pill.is-blue { background: rgba(59, 130, 246, 0.12); color: #3b82f6; }
.pp-stat-pill.is-emerald { background: rgba(16, 185, 129, 0.12); color: #10b981; }
.pp-stat-pill.is-purple { background: rgba(168, 85, 247, 0.12); color: #a855f7; }
.pp-stat-pill.is-orange { background: rgba(249, 115, 22, 0.12); color: #f97316; }

.pp-stat-main {
  display: flex;
  align-items: baseline;
  gap: 5px;
  margin-bottom: 4px;
}

.pp-stat-main strong {
  font-size: 22px;
  font-weight: 800;
  line-height: 1;
  font-variant-numeric: tabular-nums;
  letter-spacing: -0.02em;
}

.pp-stat-unit {
  font-size: 11px;
  color: var(--muted);
  font-weight: 600;
}

.pp-stat-footer {
  font-size: 10.5px;
  color: var(--muted);
  margin-top: auto;
}

.pp-stat-footer strong {
  color: var(--text);
}

/* ============================================================
   3. 通道管理阵列 (Channels Section)
   ============================================================ */
.pp-channels-section {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 10px);
  padding: 12px 14px;
  display: flex;
  flex-direction: column;
  gap: 10px;
  flex-shrink: 0;
}

.pp-channels-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
}

.pp-channels-title-group h2 {
  font-size: 13px;
  font-weight: 750;
  margin: 0;
}

.pp-channels-title-group p {
  font-size: 11px;
  color: var(--muted);
  margin: 2px 0 0;
}

.pp-channels-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
  gap: 10px;
}

.pp-channel-card {
  background: var(--page-bg);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  padding: 10px 12px;
  display: flex;
  flex-direction: column;
  gap: 8px;
  cursor: pointer;
  transition: all 0.15s ease;
}

.pp-channel-card:hover {
  border-color: var(--brand);
  transform: translateY(-1px);
}

.pp-channel-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 6px;
}

.pp-channel-name-row {
  display: flex;
  align-items: center;
  gap: 6px;
}

.pp-channel-name-row strong {
  font-size: 13px;
}

.pp-channel-account-count {
  font-size: 10.5px;
  color: var(--muted);
}

.pp-channel-node-preview {
  display: flex;
  align-items: center;
  gap: 8px;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: 6px;
  padding: 6px 10px;
}

.pp-channel-node-icon :deep(svg) {
  width: 14px;
  height: 14px;
  color: var(--brand);
}

.pp-channel-node-info {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
}

.pp-channel-node-name {
  font-size: 11.5px;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.pp-channel-testing-text {
  font-size: 11px;
  color: var(--brand);
}

.pp-channel-unset-text {
  font-size: 11px;
  color: var(--muted);
}

.pp-channel-rate-badge {
  font-size: 10.5px;
  font-weight: 750;
  padding: 2px 6px;
  border-radius: 4px;
  white-space: nowrap;
}

.pp-channel-rate-badge.fast { background: rgba(16, 185, 129, 0.15); color: #10b981; }
.pp-channel-rate-badge.good { background: rgba(59, 130, 246, 0.15); color: #3b82f6; }
.pp-channel-rate-badge.medium { background: rgba(245, 158, 11, 0.15); color: #f59e0b; }
.pp-channel-rate-badge.slow { background: rgba(239, 68, 68, 0.15); color: #ef4444; }
.pp-channel-rate-badge.bad { background: rgba(239, 68, 68, 0.15); color: #ef4444; }
.pp-channel-rate-badge.untested { background: var(--surface-hover); color: var(--muted); }

.pp-channel-footer {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 8px;
  border-top: 1px dashed var(--line);
  padding-top: 6px;
}

.pp-channel-act-btn {
  background: transparent;
  border: none;
  color: var(--brand);
  font-size: 11px;
  font-weight: 600;
  cursor: pointer;
  padding: 2px 6px;
  border-radius: 4px;
}

.pp-channel-act-btn:hover {
  background: var(--surface-hover);
}

.pp-channel-act-btn.is-danger {
  color: #ef4444;
}

/* ============================================================
   4. 指令工具条 (Command Strip)
   ============================================================ */
.pp-command-strip {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 10px);
  padding: 6px 10px;
  flex-shrink: 0;
}

.pp-strip-left {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
  flex: 1;
  flex-wrap: wrap;
}

.pp-view-switcher {
  display: inline-flex;
  align-items: center;
  gap: 2px;
  background: var(--page-bg);
  padding: 2px;
  border-radius: var(--r-md, 7px);
  border: 1px solid var(--line);
  flex-shrink: 0;
}

.pp-view-btn {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  height: 26px;
  padding: 0 9px;
  border-radius: 5px;
  border: none;
  background: transparent;
  color: var(--muted);
  font-size: 11px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.12s ease;
  white-space: nowrap;
}

.pp-view-btn:hover {
  color: var(--text);
}

.pp-view-btn.active {
  background: var(--surface);
  color: var(--brand);
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.06);
}

.pp-view-btn :deep(svg) {
  width: 12px;
  height: 12px;
}

.pp-strip-divider {
  width: 1px;
  height: 18px;
  background: var(--line);
  flex-shrink: 0;
}

.pp-strip-dropdown {
  min-width: 130px;
  flex-shrink: 0;
}

.pp-strip-dropdown.select-box {
  height: 30px;
}

.pp-strip-dropdown .select-trigger {
  padding: 0 8px;
  font-size: 11px;
  font-weight: 600;
}

.pp-strip-dropdown .select-trigger svg {
  width: 12px;
}

.pp-strip-dropdown .select-menu {
  z-index: 200;
}

.pp-strip-dropdown .select-option {
  min-height: 30px;
  padding: 5px 8px;
  font-size: 11px;
}

.pp-strip-right {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-shrink: 0;
}

.pp-search-box {
  position: relative;
  display: inline-flex;
  align-items: center;
  width: 240px;
}

.pp-search-icon {
  position: absolute;
  left: 8px;
  width: 13px;
  height: 13px;
  color: var(--muted);
  display: flex;
  align-items: center;
  justify-content: center;
  pointer-events: none;
}

.pp-search-icon :deep(svg) {
  width: 13px;
  height: 13px;
}

.pp-search-input {
  width: 100%;
  height: 28px;
  padding: 0 26px 0 26px;
  border-radius: 6px;
  border: 1px solid var(--line);
  background: var(--page-bg);
  color: var(--text);
  font-size: 11.5px;
  outline: none;
  transition: all 0.15s ease;
}

.pp-search-input:focus {
  border-color: var(--brand);
  background: var(--surface);
  box-shadow: 0 0 0 2px var(--brand-soft);
}

.pp-search-clear {
  position: absolute;
  right: 6px;
  width: 16px;
  height: 16px;
  border-radius: 50%;
  border: none;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 0;
}

.pp-search-clear:hover {
  color: var(--text);
}

.pp-search-clear :deep(svg) {
  width: 10px;
  height: 10px;
}

/* ============================================================
   5. 节点卡片与网格 (Nodes Presentation)
   ============================================================ */
.pp-nodes-section {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.pp-nodes-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
  gap: 10px;
}

.pp-node-card {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  padding: 10px 12px;
  display: flex;
  flex-direction: column;
  gap: 6px;
  transition: all 0.15s ease;
}

.pp-node-card:hover {
  border-color: var(--line-hover);
  box-shadow: 0 2px 8px rgba(0, 0, 0, 0.04);
}

.pp-node-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 6px;
}

.pp-node-title-group {
  display: flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
  flex: 1;
}

.pp-node-flag {
  font-size: 15px;
  line-height: 1;
  flex-shrink: 0;
}

.pp-node-name {
  font-size: 12.5px;
  font-weight: 650;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.pp-node-latency-btn {
  height: 22px;
  padding: 0 7px;
  border-radius: 4px;
  border: 1px solid transparent;
  font-size: 10.5px;
  font-weight: 750;
  cursor: pointer;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  transition: all 0.12s ease;
  font-variant-numeric: tabular-nums;
  flex-shrink: 0;
}

.pp-node-latency-btn.fast {
  background: rgba(16, 185, 129, 0.15);
  color: #10b981;
  border-color: rgba(16, 185, 129, 0.3);
}

.pp-node-latency-btn.good {
  background: rgba(59, 130, 246, 0.15);
  color: #3b82f6;
  border-color: rgba(59, 130, 246, 0.3);
}

.pp-node-latency-btn.medium {
  background: rgba(245, 158, 11, 0.15);
  color: #f59e0b;
  border-color: rgba(245, 158, 11, 0.3);
}

.pp-node-latency-btn.slow,
.pp-node-latency-btn.bad {
  background: rgba(239, 68, 68, 0.15);
  color: #ef4444;
  border-color: rgba(239, 68, 68, 0.3);
}

.pp-node-latency-btn.untested {
  background: var(--page-bg);
  border-color: var(--line);
  color: var(--muted);
}

.pp-node-latency-btn:hover:not(:disabled) {
  filter: brightness(1.1);
  transform: scale(1.03);
}

.pp-node-endpoint code {
  font-size: 10.5px;
  color: var(--muted);
  background: var(--page-bg);
  padding: 1px 4px;
  border-radius: 3px;
  display: inline-block;
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.pp-node-meta-row {
  display: flex;
  align-items: center;
  gap: 4px;
  flex-wrap: wrap;
  font-size: 10px;
}

.pp-node-region-chip {
  padding: 0 4px;
  border-radius: 3px;
  background: rgba(249, 115, 22, 0.1);
  color: #f97316;
  font-weight: 600;
}

.pp-node-ip-chip {
  padding: 0 4px;
  border-radius: 3px;
  background: var(--page-bg);
  color: var(--muted);
}

.pp-node-source-chip {
  padding: 0 4px;
  border-radius: 3px;
  background: var(--page-bg);
  color: var(--muted);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  max-width: 110px;
}

.pp-node-tags-row {
  display: flex;
  align-items: center;
  gap: 4px;
  margin-top: 2px;
}

.pp-protocol-badge {
  padding: 1px 5px;
  border-radius: 3px;
  background: var(--brand-soft);
  color: var(--brand-deep);
  font-size: 9.5px;
  font-weight: 750;
  letter-spacing: 0.02em;
}

.pp-sub-badge {
  padding: 1px 4px;
  border-radius: 3px;
  background: var(--page-bg);
  border: 1px solid var(--line);
  color: var(--muted);
  font-size: 9.5px;
}

.pp-load-more-bar {
  display: flex;
  justify-content: center;
  padding: 10px 0;
}

.pp-load-more-btn {
  height: 32px;
  padding: 0 20px;
}

/* ============================================================
   6. 国家/地区分组手风琴 (Country Groups)
   ============================================================ */
.pp-country-groups-container {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.pp-country-groups-header {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 12px;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
}

.pp-country-stat-pill {
  display: flex;
  align-items: baseline;
  gap: 4px;
  font-size: 11px;
}

.pp-country-stat-pill strong {
  color: var(--brand);
  font-size: 14px;
  font-weight: 750;
}

.pp-country-stat-pill span {
  color: var(--muted);
}

.pp-groups-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.pp-country-group-card {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  overflow: hidden;
  transition: all 0.15s ease;
}

.pp-group-card-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  padding: 10px 14px;
  background: var(--page-bg);
  border-bottom: 1px solid var(--line);
}

.pp-country-group-card.is-collapsed .pp-group-card-header {
  border-bottom-color: transparent;
}

.pp-group-toggle-btn {
  display: flex;
  align-items: center;
  gap: 8px;
  border: none;
  background: transparent;
  color: var(--text);
  cursor: pointer;
  padding: 0;
  text-align: left;
  flex: 1;
}

.pp-group-chevron {
  font-size: 9px;
  color: var(--muted);
  transition: transform 0.15s ease;
  width: 10px;
}

.pp-group-chevron.is-collapsed {
  transform: rotate(-90deg);
}

.pp-group-flag {
  font-size: 18px;
}

.pp-group-title-info strong {
  font-size: 13px;
  margin-right: 6px;
}

.pp-group-title-info small {
  font-size: 11px;
  color: var(--muted);
}

.pp-group-body {
  padding: 12px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.pp-group-more-bar {
  display: flex;
  justify-content: center;
  padding-top: 4px;
}

.pp-empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 50px 20px;
  color: var(--muted);
  gap: 10px;
  font-size: 12.5px;
}

.pp-empty-icon :deep(svg) {
  width: 36px;
  height: 36px;
  color: var(--muted);
}

/* ============================================================
   7. 弹窗体系 (Modal Dialogs)
   ============================================================ */
.pp-modal-backdrop {
  position: fixed;
  inset: 0;
  z-index: 2000;
  background: rgba(0, 0, 0, 0.5);
  backdrop-filter: blur(4px);
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 20px;
}

.pp-modal-card {
  width: 100%;
  max-width: 640px;
  max-height: 85vh;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-xl, 14px);
  box-shadow: 0 20px 48px rgba(0, 0, 0, 0.25);
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.pp-modal-card.is-import {
  max-width: 700px;
}

.pp-modal-card.is-channel-dialog {
  max-width: 680px;
}

.pp-modal-card.is-settings {
  max-width: 580px;
}

.pp-modal-card.is-confirm {
  max-width: 440px;
}

.pp-modal-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  padding: 14px 18px;
  border-bottom: 1px solid var(--line);
  flex-shrink: 0;
}

.pp-modal-title-group h2 {
  font-size: 15px;
  font-weight: 750;
  margin: 2px 0 0;
}

.pp-modal-eyebrow {
  font-size: 9.5px;
  font-weight: 750;
  letter-spacing: 0.05em;
  color: var(--brand);
  text-transform: uppercase;
}

.pp-modal-header p {
  font-size: 11px;
  color: var(--muted);
  margin: 2px 0 0;
}

.pp-modal-close-btn {
  width: 28px;
  height: 28px;
  border-radius: 6px;
  border: none;
  background: transparent;
  color: var(--muted);
  font-size: 16px;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
}

.pp-modal-close-btn:hover {
  background: var(--surface-hover);
  color: var(--text);
}

.pp-modal-body {
  padding: 16px 18px;
  overflow-y: auto;
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.pp-modal-footer {
  padding: 10px 18px;
  border-top: 1px solid var(--line);
  display: flex;
  justify-content: flex-end;
  align-items: center;
  gap: 8px;
  background: var(--page-bg);
  flex-shrink: 0;
}

.pp-btn-cancel {
  height: 30px;
  padding: 0 14px;
  border-radius: var(--r-md, 6px);
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--text);
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
}

.pp-btn-cancel:hover {
  background: var(--surface-hover);
}

/* Import Modal Elements */
.pp-import-form-card {
  background: var(--page-bg);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  padding: 12px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.pp-form-row {
  width: 100%;
}

.pp-input,
.pp-textarea {
  width: 100%;
  padding: 6px 10px;
  border-radius: 6px;
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--text);
  font-size: 12px;
  outline: none;
  box-sizing: border-box;
}

.pp-input:focus,
.pp-textarea:focus {
  border-color: var(--brand);
}

.pp-textarea {
  font-family: inherit;
  resize: vertical;
}

.pp-textarea.is-rules {
  font-family: monospace;
  font-size: 11px;
}

.pp-form-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}

.pp-subs-list-container {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.pp-subs-list-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-size: 12px;
}

.pp-subs-cards-list {
  display: flex;
  flex-direction: column;
  gap: 6px;
  max-height: 240px;
  overflow-y: auto;
}

.pp-sub-item-card {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  padding: 8px 12px;
  background: var(--page-bg);
  border: 1px solid var(--line);
  border-radius: 6px;
}

.pp-sub-card-main {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.pp-sub-title-row {
  display: flex;
  align-items: center;
  gap: 6px;
}

.pp-sub-count-badge {
  padding: 0 4px;
  border-radius: 3px;
  background: var(--surface);
  border: 1px solid var(--line);
  font-size: 9.5px;
  color: var(--muted);
}

.pp-sub-url-text {
  font-size: 10.5px;
  color: var(--muted);
  margin: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.pp-sub-progress-box {
  margin-top: 3px;
}

.pp-sub-progress-track {
  height: 3px;
  background: var(--line);
  border-radius: 2px;
  overflow: hidden;
  margin-bottom: 2px;
}

.pp-sub-progress-track i {
  display: block;
  height: 100%;
  background: var(--brand);
  transition: width 0.2s ease;
}

.pp-sub-card-actions {
  display: flex;
  align-items: center;
  gap: 6px;
}

.pp-confirm-text {
  font-size: 11px;
  color: #ef4444;
}

/* Channel Dialog Elements */
.pp-channel-config-form {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.pp-form-group {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.pp-label-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.pp-label {
  font-size: 11.5px;
  font-weight: 700;
  color: var(--text);
}

.pp-hint-text {
  font-size: 10.5px;
  color: var(--muted);
  margin: 0;
}

.pp-channel-candidate-box {
  background: var(--page-bg);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  padding: 8px;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.pp-candidate-search-bar {
  position: relative;
  display: flex;
  align-items: center;
  gap: 6px;
}

.pp-candidate-count-pill {
  font-size: 10px;
  color: var(--muted);
  white-space: nowrap;
}

.pp-candidate-nodes-list {
  display: flex;
  flex-direction: column;
  gap: 4px;
  max-height: 180px;
  overflow-y: auto;
}

.pp-candidate-node-item {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 8px;
  border-radius: 5px;
  border: 1px solid transparent;
  background: var(--surface);
  cursor: pointer;
  transition: all 0.1s ease;
}

.pp-candidate-node-item:hover {
  background: var(--surface-hover);
}

.pp-candidate-node-item.is-selected {
  border-color: var(--brand);
  background: var(--brand-soft);
}

.pp-candidate-flag {
  font-size: 14px;
}

.pp-candidate-info {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
}

.pp-candidate-info strong {
  font-size: 11.5px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.pp-candidate-info small {
  font-size: 10px;
  color: var(--muted);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.pp-candidate-rate-badge {
  font-size: 10px;
  font-weight: 750;
  padding: 1px 5px;
  border-radius: 3px;
}

.pp-candidate-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 20px;
  color: var(--muted);
  font-size: 11px;
}

.pp-account-bindings-list {
  display: flex;
  flex-direction: column;
  gap: 6px;
  max-height: 160px;
  overflow-y: auto;
}

.pp-account-binding-item {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 6px 10px;
  border-radius: 6px;
  background: var(--page-bg);
  border: 1px solid var(--line);
}

.pp-account-binding-item.is-locked {
  opacity: 0.6;
}

.pp-account-checkbox-row {
  display: flex;
  align-items: center;
  gap: 8px;
  cursor: pointer;
}

.pp-account-details {
  display: flex;
  flex-direction: column;
}

.pp-account-details strong {
  font-size: 11.5px;
}

.pp-account-details small {
  font-size: 10px;
  color: var(--muted);
}

.pp-account-sites-tags {
  display: flex;
  gap: 3px;
}

.pp-site-tag {
  font-size: 9.5px;
  padding: 1px 4px;
  border-radius: 3px;
  background: var(--surface);
  color: var(--muted);
}

/* Confirm Dialog */
.pp-confirm-body {
  padding: 20px;
  display: flex;
  align-items: flex-start;
  gap: 12px;
}

.pp-confirm-icon :deep(svg) {
  width: 24px;
  height: 24px;
  color: #ef4444;
}

.pp-confirm-text-group h2 {
  font-size: 14px;
  font-weight: 750;
  margin: 0;
}

.pp-confirm-text-group p {
  font-size: 11.5px;
  color: var(--muted);
  margin: 4px 0 0;
}

.pp-modal-fade-enter-active,
.pp-modal-fade-leave-active {
  transition: opacity 0.2s ease;
}

.pp-modal-fade-enter-from,
.pp-modal-fade-leave-to {
  opacity: 0;
}

/* Kernel Card Styling */
.kernel-card {
  margin-bottom: 16px;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg);
  padding: 16px 18px;
  box-shadow: var(--shadow-xs);
  display: flex;
  flex-direction: column;
  gap: 12px;
  transition: border-color 0.2s, box-shadow 0.2s;
}

.kernel-card:hover {
  border-color: var(--line-strong);
  box-shadow: var(--shadow-sm);
}

.kernel-card-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
}

.kernel-card-identity {
  display: flex;
  align-items: center;
  gap: 12px;
  min-width: 0;
}

.kernel-icon-badge {
  width: 38px;
  height: 38px;
  border-radius: var(--r-md);
  background: linear-gradient(135deg, rgba(59, 130, 246, 0.14), rgba(99, 102, 241, 0.08));
  border: 1px solid rgba(59, 130, 246, 0.22);
  color: #3b82f6;
  display: grid;
  place-items: center;
  flex-shrink: 0;
}

.kernel-icon-badge.is-geoip {
  background: linear-gradient(135deg, rgba(16, 185, 129, 0.14), rgba(6, 182, 212, 0.08));
  border-color: rgba(16, 185, 129, 0.25);
  color: #10b981;
}

.kernel-icon-badge :deep(svg) {
  width: 18px;
  height: 18px;
}

.kernel-identity-text {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-width: 0;
}

.kernel-title-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.kernel-title {
  font-size: 13.5px;
  font-weight: 700;
  color: var(--text);
}

.kernel-badge {
  font-size: 11px;
  padding: 2px 8px;
  border-radius: 12px;
  font-weight: 600;
  display: inline-flex;
  align-items: center;
  gap: 5px;
  line-height: 1.2;
}

.kernel-badge.is-ready {
  background: rgba(16, 185, 129, 0.12);
  color: #10b981;
  border: 1px solid rgba(16, 185, 129, 0.25);
}

.kernel-badge.is-ready .dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: #10b981;
  box-shadow: 0 0 6px rgba(16, 185, 129, 0.6);
}

.kernel-badge.is-missing {
  background: rgba(239, 68, 68, 0.12);
  color: #ef4444;
  border: 1px solid rgba(239, 68, 68, 0.25);
}

.kernel-badge.is-missing .dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: #ef4444;
  box-shadow: 0 0 6px rgba(239, 68, 68, 0.6);
}

.kernel-badge.is-loading {
  background: var(--surface-soft);
  color: var(--muted);
  border: 1px solid var(--line);
}

.kernel-subtitle {
  font-size: 12px;
  color: var(--muted);
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.kernel-version-tag {
  background: var(--surface-soft);
  padding: 1px 6px;
  border-radius: 4px;
  border: 1px solid var(--line);
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  font-size: 11.5px;
  font-weight: 600;
  color: var(--text);
}

.kernel-arch-tag {
  font-size: 10.5px;
  font-weight: 600;
  color: var(--faint);
  background: var(--surface-soft);
  padding: 1px 5px;
  border-radius: 3px;
  border: 1px solid var(--line-soft);
}

.kernel-update-pill {
  background: linear-gradient(135deg, #3b82f6, #6366f1);
  color: #fff;
  font-size: 10.5px;
  font-weight: 600;
  padding: 2px 8px;
  border-radius: 10px;
  box-shadow: 0 2px 8px rgba(59, 130, 246, 0.35);
  animation: pulse-subtle 2s infinite;
}

.kernel-missing-text {
  color: var(--danger, #ef4444);
  font-size: 11.5px;
}

.kernel-card-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-shrink: 0;
}

.kernel-btn-secondary,
.kernel-btn-primary {
  height: 32px;
  padding: 0 13px;
  font-size: 12px;
  font-weight: 600;
  border-radius: var(--r-md);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  cursor: pointer;
  white-space: nowrap;
  flex-shrink: 0;
  transition: all 0.15s var(--ease);
}

.kernel-btn-secondary {
  background: var(--surface-soft);
  border: 1px solid var(--line);
  color: var(--text);
}

.kernel-btn-secondary:hover:not(:disabled) {
  background: var(--surface-hover);
  border-color: var(--line-strong);
  box-shadow: var(--shadow-xs);
}

.kernel-btn-primary {
  background: var(--surface-soft);
  border: 1px solid var(--line);
  color: var(--text);
}

.kernel-btn-primary:hover:not(:disabled) {
  background: var(--surface-hover);
  border-color: var(--line-strong);
}

.kernel-btn-primary.is-accent {
  background: linear-gradient(135deg, #3b82f6, #2563eb);
  border: 1px solid #2563eb;
  color: #fff;
  box-shadow: 0 2px 8px rgba(37, 99, 235, 0.3);
}

.kernel-btn-primary.is-accent:hover:not(:disabled) {
  background: linear-gradient(135deg, #2563eb, #1d4ed8);
  box-shadow: 0 4px 12px rgba(37, 99, 235, 0.4);
}

.kernel-btn-secondary:disabled,
.kernel-btn-primary:disabled {
  opacity: 0.55;
  cursor: not-allowed;
}

.btn-icon {
  display: inline-flex;
  align-items: center;
  justify-content: center;
}

.btn-icon :deep(svg) {
  width: 14px;
  height: 14px;
}

.btn-icon.is-spinning :deep(svg) {
  animation: spin 1s linear infinite;
}

.kernel-progress-wrapper {
  background: var(--surface-soft);
  border: 1px solid var(--line-soft);
  border-radius: var(--r-md);
  padding: 10px 12px;
}

.kernel-progress-track {
  height: 6px;
  border-radius: 3px;
  background: rgba(0, 0, 0, 0.08);
  overflow: hidden;
}

.kernel-progress-fill {
  height: 100%;
  border-radius: 3px;
  background: linear-gradient(90deg, #3b82f6, #6366f1);
  transition: width 0.2s cubic-bezier(0.4, 0, 0.2, 1);
}

.kernel-progress-meta {
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-size: 11px;
  color: var(--muted);
  margin-top: 6px;
}

.kernel-progress-pct {
  font-weight: 700;
  color: #3b82f6;
  font-family: ui-monospace, monospace;
}

.kernel-path-footer {
  display: flex;
  align-items: center;
  gap: 8px;
  padding-top: 10px;
  border-top: 1px dashed var(--line-soft);
  font-size: 11px;
  color: var(--faint);
}

.kernel-path-label {
  flex-shrink: 0;
  font-weight: 500;
}

.kernel-path-value {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--muted);
  background: var(--surface-soft);
  padding: 2px 6px;
  border-radius: 4px;
  border: 1px solid var(--line-soft);
  max-width: 100%;
}

.pp-mirror-banner {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  background: var(--surface-soft);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 10px 14px;
  margin-bottom: 2px;
  flex-wrap: wrap;
}

.pp-mirror-header {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.pp-mirror-title {
  font-size: 12.5px;
  font-weight: 650;
  color: var(--text);
}

.pp-mirror-subtitle {
  font-size: 11px;
  color: var(--muted);
}

.component-mirror-controls {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.kernel-mirror-select {
  height: 28px;
  padding: 0 8px 0 10px;
  border-radius: var(--r-sm);
  background: var(--surface-soft);
  border: 1px solid var(--line);
  color: var(--text);
  font-size: 11.5px;
  font-weight: 500;
  outline: none;
  cursor: pointer;
  transition: border-color 0.15s;
}

.kernel-mirror-select:hover {
  border-color: var(--line-strong);
}

.kernel-custom-mirror-input {
  height: 28px;
  padding: 0 10px;
  border-radius: var(--r-sm);
  background: var(--surface-soft);
  border: 1px solid var(--line);
  color: var(--text);
  font-size: 11.5px;
  outline: none;
  width: 220px;
  transition: border-color 0.15s;
}

.kernel-custom-mirror-input:focus {
  border-color: var(--primary, #3b82f6);
}

@keyframes spin {
  from { transform: rotate(0deg); }
  to { transform: rotate(360deg); }
}

@keyframes pulse-subtle {
  0%, 100% { opacity: 1; transform: scale(1); }
  50% { opacity: 0.88; transform: scale(1.03); }
}
</style>
