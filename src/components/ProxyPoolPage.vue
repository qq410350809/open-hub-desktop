<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, watch } from "vue";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import { usePreferences } from "../composables/usePreferences";
import type { ProxyChannel, ProxyNode, ProxySubscription } from "../types";
import CustomSelect from "./CustomSelect.vue";

const store = useStore();
const { preferences, updatePreferences } = usePreferences();
const sourceName = ref("");
const sourceLinks = ref("");
const editingId = ref("");
const selectedSource = ref("all");
// 6000+ 节点默认只展示 ≤1000ms 的可用节点，避免全量渲染卡死。
const latencyFilter = ref<"500" | "1000" | "2000" | "error" | "all">("1000");
const latencyFilterOptions = [
  { value: "500", label: "≤ 500ms" },
  { value: "1000", label: "≤ 1000ms" },
  { value: "2000", label: "≤ 2000ms" },
  { value: "error", label: "失败/超时" },
  { value: "all", label: "全部(限流显示)" },
] as const;
const latencySelectOptions = latencyFilterOptions.map((opt) => ({ value: opt.value, text: opt.label }));
const sourceSelectOptions = computed(() => [
  { value: "all", text: "全部来源" },
  ...store.proxyPool.value.subscriptions.map((sub) => ({ value: sub.id, text: sub.name })),
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
const assignedAccountCount = computed(() => channels.value.reduce(
  (sum, channel) => sum + channel.accountCount, 0,
));
const defaultChannel = computed(() => channels.value.find(
  (channel) => channel.id === store.proxyPool.value.defaultChannelId,
) ?? channels.value[0] ?? null);

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
  const filter = latencyFilter.value;
  const sourceName = selectedSource.value === "all"
    ? ""
    : (store.proxyPool.value.subscriptions.find((item) => item.id === selectedSource.value)?.name ?? "");
  const next = store.proxyPool.value.nodes
    .filter((node) => {
      if (sourceName && !node.subscriptionNames.includes(sourceName)) return false;
      if (filter === "all") return node.testStatus !== "invalid";
      if (filter === "error") {
        return node.testStatus === "error" || node.testStatus === "invalid" || (node.testStatus === "success" && (node.latencyMs == null));
      }
      const maxLatency = Number(filter);
      // 默认只展示已测通且延迟在阈值内的节点。
      if (node.latencyMs == null || node.testStatus !== "success") return false;
      if (node.latencyMs > maxLatency) return false;
      return true;
    })
    .sort(compareNodes);
  // all 模式也做硬上限，避免 6000+ 一次渲染卡死；靠“继续加载”翻阅。
  displayNodes.value = filter === "all" ? next.slice(0, 3000) : next;
  displayNodeIds.value = new Set(displayNodes.value.map((node) => node.id));
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
    // 先写入来源地址并显示在列表，再看解析进度。
    importDialogOpen.value = true;
    const result = await store.saveProxySubscription(sourceName.value, sourceLinks.value, editingId.value || undefined);
    resetSource();
    message.value = result.discarded > 0
      ? `导入完成：${result.total} 个节点，过滤 ${result.discarded} 个非法节点`
      : `导入完成：${result.total} 个节点，新增 ${result.added}`;
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
function isDefaultChannel(channel: ProxyChannel) {
  return channel.id === store.proxyPool.value.defaultChannelId;
}
function channelBusyId(channel: ProxyChannel) {
  return `test-channel-${channel.id}`;
}
function isChannelTesting(channel: ProxyChannel) {
  return store.channelTestBusyId.value === channelBusyId(channel);
}
function channelNodeLabel(channel: ProxyChannel) {
  if (!channel.node) return "未固定节点";
  return `${channel.node.name} · ${downloadRateText(channel.node.channelLatencyMs ?? channel.node.latencyMs)}`;
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
    .filter((node) => node.channelTestStatus === "success" && node.channelLatencyMs != null && node.channelLatencyMs <= 500)
    .filter((node) => {
      if (!query) return true;
      return [
        node.name,
        node.countryName,
        node.countryCode,
        node.server,
      ].some((value) => value.toLowerCase().includes(query));
    })
    .sort((left, right) => (left.channelLatencyMs ?? Number.POSITIVE_INFINITY) - (right.channelLatencyMs ?? Number.POSITIVE_INFINITY));
});
function openChannelDialog(channel?: ProxyChannel) {
  channelEditingId.value = channel?.id ?? "";
  channelName.value = channel?.name ?? "";
  channelSelectedNodeId.value = channel?.nodeId ?? "";
  channelNodeQuery.value = "";
  channelAssignedProfileIds.value = new Set((channel?.accounts ?? []).map((account) => account.profileId));
  channelDialogOpen.value = true;
}
function closeChannelDialog() {
  channelDialogOpen.value = false;
  channelEditingId.value = "";
  channelName.value = "";
  channelSelectedNodeId.value = "";
  channelNodeQuery.value = "";
  channelAssignedProfileIds.value = new Set();
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
  return !channelAssignedProfileIds.value.has(profileId) && accountChannelLabels.value.has(profileId);
}
function selectChannelNode(nodeId: string) {
  channelSelectedNodeId.value = nodeId;
}
async function testChannelNodes() {
  message.value = "";
  try {
    await store.testProxyChannelNodes(channelEditingId.value || "");
    message.value = "测速完成，请选择节点后保存";
  } catch { /* store error */ }
}
async function saveChannel() {
  message.value = "";
  if (!channelName.value.trim()) {
    message.value = "请输入通道名称";
    return;
  }
  try {
    const state = await store.saveProxyChannel(channelName.value, channelEditingId.value || undefined);
    const channelId = state.channels.find((item) => item.id === channelEditingId.value)?.id
      ?? state.channels.find((item) => item.name === channelName.value)?.id
      ?? state.defaultChannelId;
    if (channelId && channelSelectedNodeId.value) {
      await store.setProxyChannelNode(channelId, channelSelectedNodeId.value);
    }
    if (channelId) {
      const previous = new Set(
        (state.channels.find((item) => item.id === channelId)?.accounts ?? [])
          .map((account) => account.profileId),
      );
      for (const account of proxyPoolAccounts.value) {
        if (channelAssignedProfileIds.value.has(account.profileId) && !previous.has(account.profileId)) {
          await store.assignAccountProxyChannel(account.profileId, channelId);
        } else if (!channelAssignedProfileIds.value.has(account.profileId) && previous.has(account.profileId)) {
          await store.unassignAccountProxyChannel(account.profileId);
        }
      }
    }
    closeChannelDialog();
    message.value = `通道「${channelName.value}」已保存`;
  } catch { /* store error */ }
}
async function removeChannel(channel: ProxyChannel) {
  if (isDefaultChannel(channel)) {
    message.value = "默认通道不能删除";
    return;
  }
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
  } catch { /* store error */ }
}
async function saveSettings() {
  try {
    await store.saveProxyPoolSettings(ignoreAddresses.value);
    syncSettings(); message.value = "代理规则已保存，本地与局域网地址始终直连";
  } catch { /* store error */ }
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
function selectedSourceName() {
  if (selectedSource.value === "all") return "";
  return store.proxyPool.value.subscriptions.find((item) => item.id === selectedSource.value)?.name ?? "";
}
function selectedSourceLabel() {
  return selectedSourceName() || "全部来源";
}
function nodesForSelectedSource() {
  const sourceName = selectedSourceName();
  if (!sourceName) return store.proxyPool.value.nodes;
  return store.proxyPool.value.nodes.filter((node) => node.subscriptionNames.includes(sourceName));
}
function testableNodesForSelectedSource() {
  // 测速按“选中来源的全部节点”执行，不限制当前延迟显示阈值。
  return nodesForSelectedSource();
}
function requestCancelTest() {
  if (isBatchTesting()) cancelConfirmOpen.value = true;
}
async function confirmCancelTest() {
  cancelConfirmOpen.value = false;
  message.value = "正在取消测速任务…";
  try {
    const cancelled = await store.cancelProxyNodeTests();
    message.value = cancelled
      ? (isBatchTesting() ? "已请求取消，正在停止当前测速…" : "测速任务已取消")
      : "测速任务已经结束或不在测速中";
  } catch (error) {
    message.value = `取消请求已发送（${String(error)}）`;
  }
}
function testResultMessage(scope: string, result: Awaited<ReturnType<typeof store.testAllProxyNodes>>) {
  if (result.cancelled) return `${scope}已取消：完成 ${result.completed}/${result.total}`;
  return `${scope}完成：${result.succeeded} 个成功，${result.failed} 个失败`;
}
async function testAll() {
  message.value = "";
  const sourceName = selectedSourceName();
  if (!sourceName) {
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
  // 全失败时自动切到“失败/超时”，避免 ≤1000ms 过滤把结果藏成空白列表。
  if (!result.cancelled && result.succeeded === 0 && result.failed > 0 && ["500","1000","2000"].includes(latencyFilter.value)) {
    latencyFilter.value = "error";
  }
  message.value = testResultMessage(`${selectedSourceLabel()}测速`, result);
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
  updatePreferences({ proxyNodeViewMode: "country" });
  message.value = `已按导入时的国家信息分组：${ipGroups.value.length} 个地区`;
}
function openNormalList() {
  nodeViewMode.value = "list";
  updatePreferences({ proxyNodeViewMode: "list" });
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
  return latencyClassForMs(node.latencyMs, node.testStatus);
}
function channelLatencyClass(node: ProxyNode) {
  return latencyClassForMs(node.channelLatencyMs, node.channelTestStatus);
}
function latencyClassForMs(latencyMs: number | null | undefined, testStatus: string) {
  if (testStatus === "error" || testStatus === "invalid") return "bad";
  if (latencyMs == null) return "untested";
  if (latencyMs < 250) return "fast";
  if (latencyMs < 400) return "medium";
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
    anytls: "AnyTLS",
  };
  return labels[value] ?? value;
}
function endpoint(node: ProxyNode) { return `${node.server}:${node.port}`; }
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
    nodeCountryLabel(node) ? `地区：${nodeCountryLabel(node)}${node.countryCode && node.countryCode !== "ZZ" ? ` (${node.countryCode})` : ""}` : "",
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
  return Boolean(progress && progress.stage !== "done" && progress.stage !== "error" && (
    store.proxyPoolBusyId.value === sourceId || store.proxyPoolBusyId.value === "all" || progress.status === "running"
  ));
}

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
        <p>代理池轮询使用节点，通道为 Chrome 账号固定出口，账号下的站点共享固定节点。</p>
      </div>
      <div class="proxy-header-actions">
        <div
          class="proxy-runtime-status"
          :class="{ active: Boolean(defaultChannel?.node) }"
          :title="defaultChannel ? channelNodeLabel(defaultChannel) : '暂无通道'"
        >
          <i />
          <span>{{ defaultChannel ? `默认通道 · ${channelNodeLabel(defaultChannel)}` : "未配置通道" }}</span>
        </div>
        <button class="secondary-button proxy-settings-button" type="button" :aria-expanded="settingsOpen" @click="settingsOpen = !settingsOpen">
          <span v-html="icons.settings" />
          <span>代理规则</span>
        </button>
      </div>
    </header>

    <div class="proxy-pool-scroll">
      <section class="proxy-summary-grid" aria-label="代理池概览">
        <div><strong>{{ channels.length }}</strong><span>代理通道</span></div>
        <div><strong>{{ store.proxyPool.value.nodeCount }}</strong><span>去重节点</span></div>
        <div><strong>{{ duplicateCount }}</strong><span>已合并重复</span></div>
        <div class="proxy-summary-endpoint">
          <strong>{{ assignedAccountCount }}</strong>
          <span>已固定通道的账号</span>
        </div>
      </section>

      <section class="proxy-channels-panel" aria-label="代理通道">
        <div class="proxy-channels-heading">
          <div>
            <strong>代理通道</strong>
            <span>每个 Chrome 账号只归属一个通道，账号下的所有站点共享该通道固定出口。</span>
          </div>
          <button class="secondary-button" type="button" @click="addChannel">
            <span v-html="icons.plus" />
            <span>添加通道</span>
          </button>
        </div>
        <div class="proxy-channel-grid">
          <article
            v-for="channel in channels"
            :key="channel.id"
            class="proxy-channel-card"
            :class="{ default: isDefaultChannel(channel), testing: isChannelTesting(channel) }"
            @click="openChannelDialog(channel)"
          >
            <header>
              <div class="proxy-channel-title">
                <strong>{{ channel.name }}</strong>
                <span v-if="isDefaultChannel(channel)">默认通道</span>
                <span>{{ channel.accountCount }} 个账号使用</span>
              </div>
              <i :class="{ muted: !channel.node }" />
            </header>
            <p class="proxy-channel-url">Cloudflare 500KB 下载测速</p>
            <div class="proxy-channel-meta">
              <span v-if="channel.node" class="proxy-channel-fastest">
                <span v-html="icons.activity" />
                {{ channelNodeLabel(channel) }}
              </span>
              <span v-else-if="isChannelTesting(channel)" class="proxy-channel-testing">正在测速…</span>
              <span v-else>未固定节点</span>
            </div>
            <footer>
              <button class="text-button" type="button" @click.stop="openChannelDialog(channel)">
                配置
              </button>
              <button
                v-if="!isDefaultChannel(channel)"
                class="text-button danger"
                type="button"
                :disabled="channels.length <= 1 || Boolean(store.proxyPoolBusyId.value)"
                @click.stop="removeChannel(channel)"
              >
                {{ deleteChannelConfirmId === channel.id ? "确认删除" : "删除" }}
              </button>
            </footer>
          </article>
        </div>
      </section>

      <section v-if="settingsOpen" class="proxy-settings-panel">
        <div class="proxy-section-title">
          <div><strong>请求规则</strong><span>测速固定使用 Cloudflare 500KB 下载地址，这里只配置必须保持直连的目标。</span></div>
        </div>
        <div class="proxy-connection-grid">
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
            <span>
              显示 {{ renderedNodes.length }}/{{ filteredNodes.length }}
              · {{ latencyFilter === 'all' ? '全部' : (latencyFilter === 'error' ? '失败/超时' : ('≤' + latencyFilter + 'ms')) }}
              · 共 {{ store.proxyPool.value.nodeCount }}
            </span>
          </div>
         <div class="proxy-node-filters">
            <CustomSelect
              class="proxy-source-select"
              :options="sourceSelectOptions"
              :model-value="selectedSource"
              aria-label="筛选导入来源"
              @update:model-value="selectedSource = String($event)"
            />
            <CustomSelect
              class="proxy-latency-select"
              :options="latencySelectOptions"
              :model-value="latencyFilter"
              aria-label="按延迟显示节点"
              @update:model-value="latencyFilter = String($event) as typeof latencyFilter"
            />
          </div>
          <div class="proxy-node-actions">
            <button class="secondary-button proxy-import-button" type="button" @click="openImportDialog">
              <span v-html="icons.plus" /><span>导入来源</span>
            </button>
            <button
              class="secondary-button"
              :class="{ danger: isBatchTesting() }"
              type="button"
              :disabled="!testableNodesForSelectedSource().length || (Boolean(store.proxyPoolBusyId.value) && !isBatchTesting()) || (store.testingNodeIds.value.size > 0 && !isBatchTesting())"
              @click="isBatchTesting() ? requestCancelTest() : testAll()"
              :title="selectedSource === 'all' ? '测速全部来源节点' : `只测速当前选中来源：${selectedSourceLabel()}`"
            >
              <span v-html="isBatchTesting() ? icons.close : icons.pulse" />
              <span>{{
                isBatchTesting()
                  ? (store.proxyTestCancelling.value
                    ? "正在取消…"
                    : `取消测速 ${store.proxyTestProgress.value.completed}/${store.proxyTestProgress.value.total}`)
                  : (selectedSource === "all" ? "批量测速" : "测速此来源")
              }}</span>
            </button>
            <button class="secondary-button" type="button" :disabled="!store.proxyPool.value.nodes.length" @click="openCountryGroups">
              <span v-html="icons.globe" /><span>{{ nodeViewMode === "ip" ? "刷新分组" : "国家分组" }}</span>
            </button>
            <button v-if="nodeViewMode === 'ip'" class="secondary-button" type="button" @click="openNormalList()">
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
            :class="{ disabled: Boolean(store.proxyPoolBusyId.value) }"
            :title="nodeDetailTitle(node)"
          >
            <div class="proxy-node-tile-head">
              <div class="proxy-node-tile-title">
                <strong>{{ node.name }}</strong>
                <small>{{ endpoint(node) }}</small>
              </div>
              <button
                class="proxy-tile-latency"
                :class="latencyClass(node)"
                type="button"
                :disabled="Boolean(store.proxyPoolBusyId.value) || store.testingNodeIds.value.has(node.id)"
                @click.stop="testNode(node)"
              ><span v-if="store.testingNodeIds.value.has(node.id)" class="proxy-node-loading" v-html="icons.restore" /><template v-else>{{ latencyText(node) }}</template></button>
            </div>
            <div class="proxy-node-tile-meta">
              <span v-if="nodeCountryLabel(node)" class="proxy-node-region">
                {{ countryFlag(node.countryCode) }} {{ nodeCountryLabel(node) }}
              </span>
              <span class="proxy-node-source">{{ nodeSourceLabel(node) }}</span>
              <span v-if="node.primaryIp || ipAnalysisByNode.get(node.id)?.primaryIp" class="proxy-node-ip">
                {{ node.primaryIp || ipAnalysisByNode.get(node.id)?.primaryIp }}
              </span>
            </div>
            <div class="proxy-node-tile-tags">
              <span>{{ protocolLabel(node.proxyType) }}</span>
              <span v-if="node.udp">UDP</span>
              <span v-if="node.cipher">{{ node.cipher }}</span>
              <i v-if="!node.latencyMs && node.testStatus !== 'success'">未测速</i>
              <i v-else-if="node.testStatus === 'error'">失败</i>
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
                :class="{ disabled: Boolean(store.proxyPoolBusyId.value) }"
                :title="nodeDetailTitle(node)"
              >
                <div class="proxy-node-tile-head">
                  <div class="proxy-node-tile-title">
                    <strong>{{ node.name }}</strong>
                    <small>{{ endpoint(node) }}</small>
                  </div>
                  <button class="proxy-tile-latency" :class="latencyClass(node)" type="button" :disabled="Boolean(store.proxyPoolBusyId.value) || store.testingNodeIds.value.has(node.id)" @click.stop="testNode(node)"><span v-if="store.testingNodeIds.value.has(node.id)" class="proxy-node-loading" v-html="icons.restore" /><template v-else>{{ latencyText(node) }}</template></button>
                </div>
                <div class="proxy-node-tile-meta">
                  <span v-if="nodeCountryLabel(node)" class="proxy-node-region">
                    {{ countryFlag(node.countryCode) }} {{ nodeCountryLabel(node) }}
                  </span>
                  <span class="proxy-node-source">{{ nodeSourceLabel(node) }}</span>
                  <span v-if="node.primaryIp || ipAnalysisByNode.get(node.id)?.primaryIp" class="proxy-node-ip">
                    {{ node.primaryIp || ipAnalysisByNode.get(node.id)?.primaryIp }}
                  </span>
                </div>
                <div class="proxy-node-tile-tags">
                  <span>{{ protocolLabel(node.proxyType) }}</span>
                  <span v-if="node.udp">UDP</span>
                  <span v-if="node.cipher">{{ node.cipher }}</span>
                  <i v-if="!node.latencyMs && node.testStatus !== 'success'">未测速</i>
                  <i v-else-if="node.testStatus === 'error'">失败</i>
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
              : (latencyFilter === 'error'
                ? '当前来源没有失败节点'
                : (latencyFilter === 'all'
                  ? '当前来源没有节点'
                  : ('当前 ≤' + latencyFilter + 'ms 范围内没有节点，可切换“失败/超时”或放宽阈值')))
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
            <div class="proxy-import-list-title">
              <strong>已导入来源</strong>
              <span>{{ store.proxyPool.value.subscriptions.length }} 个</span>
            </div>
            <button
              class="secondary-button proxy-import-refresh-all"
              type="button"
              :disabled="store.proxyPoolBusyId.value === 'all' || !store.proxyPool.value.subscriptions.length"
              @click="refreshAll"
            >
              <span :class="{ 'is-spinning': store.proxyPoolBusyId.value === 'all' }" v-html="icons.restore" />
              <span>{{ store.proxyPoolBusyId.value === 'all' ? '刷新中…' : '刷新全部' }}</span>
            </button>
          </div>
          <div class="proxy-subscription-list">
            <article
              v-for="source in store.proxyPool.value.subscriptions"
              :key="source.id"
              class="proxy-subscription-card"
              :class="{
                selected: selectedSource === source.id,
                parsing: isSourceParsing(source.id),
                error: Boolean(source.lastError) || sourceProgress(source.id)?.stage === 'error',
              }"
              @click="selectedSource = selectedSource === source.id ? 'all' : source.id"
            >
              <header>
                <div>
                  <strong>{{ source.name }}</strong>
                  <span>{{ source.nodeCount }} 个原始节点</span>
                </div>
                <i :class="{ error: source.lastError || sourceProgress(source.id)?.stage === 'error', spin: isSourceParsing(source.id) }" />
              </header>
              <p>{{ source.url }}</p>
              <div v-if="isSourceParsing(source.id) || sourceProgress(source.id)?.stage === 'done' || sourceProgress(source.id)?.stage === 'error' || source.lastError" class="proxy-source-progress">
                <div class="proxy-source-progress-track" v-if="isSourceParsing(source.id)">
                  <i :style="{ width: sourceProgress(source.id)?.total ? `${Math.min(100, Math.round(((sourceProgress(source.id)?.completed || 0) / (sourceProgress(source.id)?.total || 1)) * 100))}%` : (sourceProgress(source.id)?.stage === 'fetching' ? '35%' : sourceProgress(source.id)?.stage === 'parsing' ? '60%' : '80%') }" />
                </div>
                <small :class="{ error: source.lastError || sourceProgress(source.id)?.stage === 'error' }">
                  {{ source.lastError || sourceProgressText(source.id) }}
                </small>
              </div>
              <footer v-if="deleteConfirmId !== source.id">
                <button class="text-button" type="button" @click.stop="editSource(source)">编辑</button>
                <button class="text-button" type="button" :disabled="store.proxyPoolBusyId.value === source.id || store.proxyPoolBusyId.value === 'all'" @click.stop="refreshSource(source)">
                  {{ isSourceParsing(source.id) ? "解析中" : "刷新" }}
                </button>
                <button
                  class="text-button danger"
                  type="button"
                  :disabled="store.proxyPoolBusyId.value === source.id || store.proxyPoolBusyId.value === 'all'"
                  @click.stop="removeSource(source)"
                >删除</button>
              </footer>
              <footer v-else class="proxy-delete-confirm">
                <span>确定删除？</span>
                <button class="text-button" type="button" @click.stop="cancelRemoveSource">取消</button>
                <button class="text-button danger" type="button" :disabled="store.proxyPoolBusyId.value === source.id" @click.stop="removeSource(source)">确认删除</button>
              </footer>
            </article>
            <div v-if="!store.proxyPool.value.subscriptions.length" class="proxy-side-empty">粘贴订阅地址或节点链接开始导入</div>
          </div>
        </div>
      </section>
    </div>
  </Teleport>

  <!-- 代理通道配置弹窗 -->
  <Teleport to="body">
    <div v-if="channelDialogOpen" class="proxy-import-backdrop" @click.self="closeChannelDialog">
      <section class="proxy-channel-dialog" role="dialog" aria-modal="true">
        <header class="proxy-import-dialog-header">
          <div>
            <strong>{{ channelEditingId ? "配置通道" : "添加通道" }}</strong>
            <span>测速不会固定节点，选中节点保存后才固定；一个账号只能归属一个通道。</span>
          </div>
          <button class="icon-button proxy-import-close" type="button" title="关闭" @click="closeChannelDialog" v-html="icons.close" />
        </header>
        <div class="proxy-channel-dialog-body">
          <form class="proxy-channel-form" @submit.prevent="saveChannel()">
            <label>
              <span>通道名称</span>
              <input v-model="channelName" required placeholder="例如：香港固定出口" />
            </label>
            <div class="proxy-channel-node-field">
              <span>固定节点</span>
              <div class="proxy-channel-node-panel">
                <div class="proxy-channel-node-toolbar">
                  <label class="proxy-channel-node-search">
                    <span v-html="icons.search" />
                    <input v-model="channelNodeQuery" placeholder="搜索节点 / 地区" />
                  </label>
                  <span class="proxy-channel-node-count">{{ channelCandidateNodes.length }} 个候选</span>
                </div>
                <div class="proxy-channel-node-list">
                  <label
                    v-for="node in channelCandidateNodes"
                    :key="node.id"
                    class="proxy-channel-node-card"
                    :class="{ selected: channelSelectedNodeId === node.id }"
                  >
                    <input
                      type="radio"
                      name="channel-node"
                      :value="node.id"
                      :checked="channelSelectedNodeId === node.id"
                      @change="selectChannelNode(node.id)"
                    />
                    <span v-if="channelSelectedNodeId === node.id" class="proxy-channel-node-card-check" v-html="icons.check" />
                    <span class="proxy-channel-node-card-flag">{{ countryFlag(node.countryCode) }}</span>
                    <span class="proxy-channel-node-card-body">
                      <strong>{{ node.name }}</strong>
                      <small>{{ [nodeCountryLabel(node), endpoint(node)].filter(Boolean).join(" · ") }}</small>
                    </span>
                    <span class="proxy-channel-node-card-latency" :class="channelLatencyClass(node)">
                      <template v-if="node.channelLatencyMs != null">{{ downloadRateText(node.channelLatencyMs) }}</template>
                      <template v-else>待测速</template>
                    </span>
                  </label>
                  <div v-if="!channelCandidateNodes.length" class="proxy-channel-node-empty">
                    <span v-html="icons.globe" />
                    <strong>{{ channelNodeQuery ? "没有匹配的候选节点" : "没有 ≤500ms 的候选节点" }}</strong>
                    <small>先在节点列表完成测速，再回来选择</small>
                  </div>
                </div>
                <div class="proxy-channel-node-footer">
                  <span v-html="icons.activity" />
                  <span>测速只刷新下载速率，保存通道后才固定节点</span>
                </div>
              </div>
            </div>
            <div class="proxy-channel-form-actions">
              <button class="secondary-button" type="button" @click="closeChannelDialog">取消</button>
              <button
                class="secondary-button"
                type="button"
                :disabled="Boolean(store.proxyPoolBusyId.value) || Boolean(store.channelTestBusyId.value)"
                @click="testChannelNodes"
              >{{ store.channelTestBusyId.value ? "测速中…" : "测速" }}</button>
              <button class="primary-button" type="submit" :disabled="Boolean(store.proxyPoolBusyId.value)">保存通道</button>
            </div>
          </form>
          <section v-if="proxyPoolAccounts.length" class="proxy-channel-sites">
            <div>
              <strong>分配给该通道的账号</strong>
              <span>按 Chrome 账号选择，账号级固定出口；每个账号只能归属一个通道，已归属其他通道的账号不可重复选择。</span>
            </div>
            <div
              v-for="account in proxyPoolAccounts"
              :key="account.profileId"
              class="proxy-channel-account"
              :class="{ locked: isChannelAccountLocked(account.profileId) }"
            >
              <label class="proxy-channel-site-row">
                <input
                  type="checkbox"
                  :checked="channelAssignedProfileIds.has(account.profileId)"
                  :disabled="isChannelAccountLocked(account.profileId)"
                  @change="toggleChannelAccount(account.profileId)"
                />
                <span>
                  <strong>{{ account.accountName || account.profileName }}</strong>
                  <small>
                    {{ account.profileId }}
                    <template v-if="isChannelAccountLocked(account.profileId)">
                      · 已归属其他通道：{{ accountChannelLabels.get(account.profileId) }}
                    </template>
                  </small>
                </span>
              </label>
              <ul class="proxy-channel-account-sites">
                <li v-for="site in account.sites" :key="site.siteId">
                  <span>{{ site.siteName }}</span>
                  <small>{{ site.apiBaseUrl }}</small>
                </li>
              </ul>
            </div>
          </section>
        </div>
      </section>
    </div>
  </Teleport>
</template>
