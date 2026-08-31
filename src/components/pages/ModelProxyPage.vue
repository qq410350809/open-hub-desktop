<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from "vue";
import { marked } from "marked";
import DOMPurify from "dompurify";
import { icons } from "../../icons";
import {
  useModelProxy,
  channelAlias,
  channelStatsKey,
  filterChannelModels,
  isValidChannelAlias,
  type ChannelConfig,
  type KeyGroupItem,
  type ChannelUsageStats,
  type ChannelModelUsageStats,
  type GatewayDailyPoint,
  type GatewayHourlyPoint,
  type GatewayOverviewTotals,
  type ProxyRequestLog,
} from "../../composables/useModelProxy";
import { useLibrary, runCommand } from "../../composables/useLibrary";
import { useToast } from "../../composables/useToast";
import { usePreferences } from "../../composables/usePreferences";
import EChart from "../common/EChart.vue";
import type { EChartsOption } from "../../echarts";
import {
  formatNumber as formatNumberUtil,
  formatUptime as formatUptimeUtil,
  formatLogDate,
  formatLogTime,
  formatLogFull,
} from "../../utils";
import CustomSelect from "../common/CustomSelect.vue";
import DateRangeDropdown from "../common/DateRangeDropdown.vue";
import type { SiteRecord } from "../../types";
import { API_PATH_V1, API_PATH_GEMINI, API_PATH_MESSAGES, DEFAULT_SERVICE_PORT } from "../../constants";
import { isTauri } from "../../composables/core/ipc";

/** 模型 API Origin：桌面端访问本机内嵌服务（端口随实际监听顺延），浏览器端与 Web 服务同源。 */
const serviceOrigin = computed(() =>
  isTauri
    ? `http://127.0.0.1:${proxyStatus.value.port || DEFAULT_SERVICE_PORT}`
    : window.location.origin,
);

const logPageSizeOptions = [
  { value: 10, text: "10" },
  { value: 25, text: "25" },
  { value: 50, text: "50" },
  { value: 100, text: "100" },
];

const { showToast } = useToast();
const {
  proxyConfig,
  proxyStatus,
  savingConfig,
  togglingServer,
  fetchingModels,
  channelModels,
  modelsForChannel,
  channelStats,
  proxyLogs,
  loadingLogs,
  loadProxyData,
  refreshStatus,
  refreshChannelStats,
  fetchChannelModelStats,
  refreshGatewayOverview,
  gatewayOverview,
  copyResponsesUrl,
  saveConfig,
  toggleServer,
fetchUpstreamModels,
refreshModels,
loadCachedModels,
fetchLogs,
  goLogPage,
  clearLogs,
  copyProxyUrl,
  copyGeminiUrl,
  copyGeminiV1BetaUrl,
  copyClaudeUrl,
  copyProxyKey,
  logPage,
  logPageSize,
  logDateFrom,
  logDateTo,
  overviewDateFrom,
  overviewDateTo,
  logTotal,
  logGlobalTotal,
  logGlobalSuccess,
  logGlobalError,
  logPageCount,
  logRangeStart,
  logRangeEnd,
  logSortBy,
  logSortOrder,
  toggleLogSort,
} = useModelProxy();

const showKey = ref(false);
const gatewaySearchQuery = ref("");
const channelSearchQuery = ref("");
const logSearchQuery = ref("");
const logStatusFilter = ref<"all" | "success" | "error">("all");
const configModalOpen = ref(false);
const gatewayModelsModalOpen = ref(false);
const currentMainTab = ref<"console" | "channels" | "logs">("console");
const channelModelsModalOpen = ref(false);
const channelSettingsDialogOpen = ref(false);
const clearLogsModalOpen = ref(false);
const clearingLogs = ref(false);
// 范围清理：为空时清理全部；填写日期时仅清理该日期之前的明细
const clearBeforeDate = ref("");
const selectedLogForDetail = ref<ProxyRequestLog | null>(null);
const selectedChannel = ref<ChannelConfig | null>(null);
const copiedModelId = ref<string | null>(null);

async function handleClearLogs(mode: "payload_only" | "all") {
  clearingLogs.value = true;
  try {
    await clearLogs(mode, clearBeforeDate.value || undefined);
    clearLogsModalOpen.value = false;
    clearBeforeDate.value = "";
  } finally {
    clearingLogs.value = false;
  }
}

let uptimeTicker: number | null = null;
let statusPollTimer: number | null = null;
let channelStatsTimer: number | null = null;

// 标签页切换刷新对应统计；immediate 保证首次进入控制台也必定拉取总览
// （否则页面挂载时序竞态会导致首屏空数据，需手动切一次标签才出现）
watch(
  currentMainTab,
  (tab) => {
    if (tab === "channels") {
      void refreshChannelStats();
      void refreshSiteInheritedKeyCounts();
    } else if (tab === "console") {
      void refreshGatewayOverview();
    }
  },
  { immediate: true },
);

// 服务（自启动完成）转为运行时，控制台补拉一次总览
watch(
  () => proxyStatus.value.running,
  (running) => {
    if (running && currentMainTab.value === "console") {
      void refreshGatewayOverview();
    }
  },
);

onMounted(async () => {
void refreshGatewayOverview();
await loadProxyData();
await loadCachedModels();
void refreshSiteInheritedKeyCounts();
await fetchLogs({ filter: logStatusFilter.value, q: logSearchQuery.value.trim() });

  uptimeTicker = window.setInterval(() => {
    if (proxyStatus.value.running) {
      proxyStatus.value.uptimeSeconds += 1;
    }
  }, 1000);

  statusPollTimer = window.setInterval(async () => {
    if (proxyStatus.value.running) {
      await refreshStatus();
    }
  }, 3000);

  // 每 5s 刷新当前标签页统计。总览/渠道统计读持久化日统计表，
  // 服务未运行（历史数据仍在）也需要刷新，故不再以 running 为前置条件
  channelStatsTimer = window.setInterval(async () => {
    if (currentMainTab.value === "channels") {
      await refreshChannelStats();
    } else if (currentMainTab.value === "console") {
      await refreshGatewayOverview();
    }
  }, 5000);
});

onUnmounted(() => {
  if (uptimeTicker !== null) {
    clearInterval(uptimeTicker);
    uptimeTicker = null;
  }
  if (statusPollTimer !== null) {
    clearInterval(statusPollTimer);
    statusPollTimer = null;
  }
  if (channelStatsTimer !== null) {
    clearInterval(channelStatsTimer);
    channelStatsTimer = null;
  }
});

async function switchToLogsTab() {
  currentMainTab.value = "logs";
  await fetchLogs({ filter: logStatusFilter.value, q: logSearchQuery.value.trim() });
}

async function handleSave() {
  const ok = await saveConfig(proxyConfig.value);
  if (ok) {
    configModalOpen.value = false;
  }
}

async function handleChannelSave(channel: ChannelConfig) {
  const ok = await saveConfig(proxyConfig.value);
  if (ok) {
    showToast(`已更新「${channel.name}」渠道设置`);
  }
}

function handleOpenGatewayModelsModal() {
  gatewayModelsModalOpen.value = true;
  if (Object.keys(channelModels.value).length === 0) {
    loadCachedModels();
  }
}

function closeGatewayModelsModal() {
  gatewayModelsModalOpen.value = false;
}

async function handleOpenChannelModelsModal(channel: ChannelConfig) {
  selectedChannel.value = channel;
  channelModalTab.value = "models";
  channelRawKeys.value = [];
  channelDraftKeyGroups.value = [];
  newKeyGroupName.value = "";
  channelModelsModalOpen.value = true;
  // Key 行通道绑定下拉需要通道候选；两路加载完成后归一化各 Key 的通道引用（名称 → ID）
  void Promise.all([
    loadProxyPoolOptions(),
    loadChannelKeysAndGroupsDraft(channel),
  ]).then(() => {
    for (const k of channelRawKeys.value) {
      k.fixedChannelId = normalizePoolChannelRef(k.fixedChannelId);
    }
  });
  // 模型级代理出口覆盖草稿：从渠道既有规则初始化
  {
    const draft = new Map<string, { mode: ModelProxyMode; nodeId: string }>();
    for (const r of channel.modelProxyRules ?? []) {
      draft.set(r.model.toLowerCase(), { mode: (r.mode as ModelProxyMode) || "", nodeId: r.nodeId ?? "" });
    }
    modelProxyModeDraft.value = draft;
  }
  // 模型行内统计：异步拉取，加载完成后由 Map 响应式刷新各行
  if (channel.id) {
    loadingModelStats.value = true;
    channelModelStatsMap.value = new Map();
    void fetchChannelModelStats(channel.id).then((list) => {
      const map = new Map<string, ChannelModelUsageStats>();
      for (const s of list) map.set(s.model.toLowerCase(), s);
      channelModelStatsMap.value = map;
      loadingModelStats.value = false;
    });
  }
  // 1. 初始化白名单勾选草稿：白名单为 null（全部启用）时默认全选；否则仅勾选白名单中的模型
  const allow = channel.enabledModels;
  if (allow == null) {
    channelModelAllChecked.value = true;
    channelModelSelection.value = {};
  } else {
    channelModelAllChecked.value = false;
    const map: Record<string, boolean> = {};
    for (const m of allow) map[m] = true;
    channelModelSelection.value = map;
  }
  // 2. 初始化弹窗内展示的模型草稿列表：从全局已知模型拷贝
  const existing = modelsForChannel(channel.id);
  channelDraftModels.value = [...existing];

  // 3. 若全局缓存无已知模型，尝试从库中读取缓存（不远程获取）
  if (channelDraftModels.value.length === 0) {
    fetchingDraftModels.value = true;
    try {
      if (channel.siteId) {
        try {
          const cache = await runCommand<{ models?: { id: string }[] }>("get_site_model_cache", {
            siteId: channel.siteId,
          });
          if (Array.isArray(cache?.models) && cache.models.length > 0) {
            channelDraftModels.value = cache.models.map((m) => m.id).filter(Boolean);
          }
        } catch {
          /* 忽略 */
        }
      }
      // 仍无数据时，读取后端内存缓存（可能由之前的刷新操作写入）
      if (channelDraftModels.value.length === 0) {
        await loadCachedModels();
        const cached = modelsForChannel(channel.id);
        if (cached.length > 0) {
          channelDraftModels.value = [...cached];
        }
      }
    } finally {
      fetchingDraftModels.value = false;
    }
  }

  // 打开弹窗只展示本地缓存；远程模型仅由“刷新上游模型”按钮主动拉取。
}

function closeChannelModelsModal() {
  channelModelsModalOpen.value = false;
  channelDraftModels.value = [];
  channelModelSelection.value = {};
  channelModelStatsMap.value = new Map();
  modelProxyModeDraft.value = new Map();
  fetchingDraftModels.value = false;
}

// —— 渠道「管理模型」弹窗：双视图 Tab 与 Key 分组管理 ——
const channelModalTab = ref<"models" | "keys">("models");

interface ChannelKeyDetailItem {
  key: string;
  accountLabel: string;
  profileName: string;
  groupId: string;
  enabled: boolean;
  supportedModels?: string[] | null;
  /** 渠道为固定通道模式时该 Key 绑定的代理池通道 ID；空 = 渠道默认通道 */
  fixedChannelId: string;
}

const channelRawKeys = ref<ChannelKeyDetailItem[]>([]);
const channelDraftKeyGroups = ref<KeyGroupItem[]>([]);
const newKeyGroupName = ref("");

/** 获取某 Key 的脱敏显示 */
function maskKeyStr(key: string): string {
  const value = key.trim();
  if (!value) return "—";
  if (value.length <= 8) return "••••••••";
  const prefix = value.startsWith("sk-") ? 7 : 4;
  const suffix = Math.min(4, Math.max(2, Math.floor(value.length / 8)));
  return `${value.slice(0, prefix)}••••••••${value.slice(-suffix)}`;
}

/** 为渠道初始化分组与 Key 列表 */
async function loadChannelKeysAndGroupsDraft(channel: ChannelConfig) {
  channelDraftKeyGroups.value = (channel.keyGroups ?? []).map((g: KeyGroupItem) => ({ ...g }));
  if (channelDraftKeyGroups.value.length === 0) {
    channelDraftKeyGroups.value = [
      { id: "primary", name: "主力组", enabled: true },
      { id: "backup", name: "备用组", enabled: true },
    ];
  }

  const keysList: ChannelKeyDetailItem[] = [];
  const seenKeys = new Set<string>();

  if (channel.siteId) {
    try {
      const cache = await runCommand<{
        accounts?: Array<{
          profileName?: string;
          accountName?: string;
          username?: string;
          keys?: string[];
          keyGroups?: Record<string, string>;
          keyModels?: Record<string, Array<{ id: string }>>;
        }>;
      }>("get_site_model_cache", { siteId: channel.siteId });

      for (const acc of cache?.accounts ?? []) {
        const accLabel = acc.username || acc.accountName || acc.profileName || "站点账号";
        const profName = acc.profileName || "";
        for (const k of acc.keys ?? []) {
          const trimmed = k.trim();
          if (trimmed && !seenKeys.has(trimmed)) {
            seenKeys.add(trimmed);
            const rawGroup = acc.keyGroups?.[trimmed]?.trim();
            const models = acc.keyModels?.[trimmed]?.map((m) => m.id);
            keysList.push({
              key: trimmed,
              accountLabel: accLabel,
              profileName: profName,
              groupId: rawGroup || "primary",
              enabled: true,
              supportedModels: models && models.length > 0 ? models : null,
              fixedChannelId: "",
            });
          }
        }
      }
    } catch {
      /* 忽略缓存读取错误 */
    }
  }

  // 融合自定义渠道的静态 Keys
  const staticKeys = channel.apiKeys?.length
    ? channel.apiKeys
    : channel.apiKey?.trim()
      ? [channel.apiKey]
      : [];
  for (const k of staticKeys) {
    const trimmed = k.trim();
    if (trimmed && !seenKeys.has(trimmed)) {
      seenKeys.add(trimmed);
      keysList.push({
        key: trimmed,
        accountLabel: "手动配置",
        profileName: "",
        groupId: "primary",
        enabled: true,
        supportedModels: null,
        fixedChannelId: "",
      });
    }
  }

  // 融合已有渠道配置中的 Key 规则
  if (channel.keyRules) {
    for (const rule of channel.keyRules) {
      const existing = keysList.find((k) => k.key === rule.key);
      if (existing) {
        if (rule.groupId) existing.groupId = rule.groupId;
        existing.enabled = rule.enabled;
        if (rule.supportedModels) existing.supportedModels = rule.supportedModels;
        existing.fixedChannelId = rule.fixedChannelId ?? "";
      }
    }
  }

  // 确保所有 Key 的 groupId 都存在于 channelDraftKeyGroups
  for (const k of keysList) {
    if (!channelDraftKeyGroups.value.some((g: KeyGroupItem) => g.id === k.groupId)) {
      channelDraftKeyGroups.value.push({
        id: k.groupId,
        name: k.groupId === "primary" ? "主力组" : k.groupId === "backup" ? "备用组" : k.groupId,
        enabled: true,
      });
    }
  }

  channelRawKeys.value = keysList;

  // 渠道未配置任何 Key 时不保留分组（分组调度仅对有 Key 的渠道有意义）
  if (keysList.length === 0) {
    channelDraftKeyGroups.value = [];
  }
}

/** 分组操作：添加新分组 */
function addKeyGroup() {
  const name = newKeyGroupName.value.trim();
  if (!name) return;
  const id = `grp_${Date.now().toString(36)}`;
  channelDraftKeyGroups.value.push({ id, name, enabled: true });
  newKeyGroupName.value = "";
}

/** 分组操作：上移/下移优先级 */
function moveKeyGroup(groupId: string, dir: -1 | 1) {
  const idx = channelDraftKeyGroups.value.findIndex((g) => g.id === groupId);
  const target = idx + dir;
  if (idx < 0 || target < 0 || target >= channelDraftKeyGroups.value.length) return;
  const list = [...channelDraftKeyGroups.value];
  [list[idx], list[target]] = [list[target], list[idx]];
  channelDraftKeyGroups.value = list;
}

/** 分组操作：删除分组（将组内 Key 迁移到首个可用分组） */
function deleteKeyGroup(groupId: string) {
  if (channelDraftKeyGroups.value.length <= 1) {
    showToast("至少保留一个分组", true);
    return;
  }
  channelDraftKeyGroups.value = channelDraftKeyGroups.value.filter((g: KeyGroupItem) => g.id !== groupId);
  const fallbackGroupId = channelDraftKeyGroups.value[0]?.id || "primary";
  for (const k of channelRawKeys.value) {
    if (k.groupId === groupId) {
      k.groupId = fallbackGroupId;
    }
  }
}

/** 获取某个分组内的所有 Keys */
function keysInGroup(groupId: string): ChannelKeyDetailItem[] {
  return channelRawKeys.value.filter((k) => k.groupId === groupId);
}

/** Key 操作：组内上移/下移 */
function moveKeyInGroup(keyStr: string, dir: -1 | 1) {
  const item = channelRawKeys.value.find((k) => k.key === keyStr);
  if (!item) return;
  const groupKeys = keysInGroup(item.groupId);
  const idx = groupKeys.findIndex((k) => k.key === keyStr);
  const target = idx + dir;
  if (idx < 0 || target < 0 || target >= groupKeys.length) return;
  const otherKey = groupKeys[target].key;
  const idxA = channelRawKeys.value.findIndex((k) => k.key === keyStr);
  const idxB = channelRawKeys.value.findIndex((k) => k.key === otherKey);
  if (idxA >= 0 && idxB >= 0) {
    const list = [...channelRawKeys.value];
    [list[idxA], list[idxB]] = [list[idxB], list[idxA]];
    channelRawKeys.value = list;
  }
}

// —— 渠道「管理模型」弹窗：草稿模型列表与勾选启用白名单 ——
const channelDraftModels = ref<string[]>([]);
const fetchingDraftModels = ref(false);
const channelModelSelection = ref<Record<string, boolean>>({});
/** true = 全选模式（等价未配置白名单，对外全部启用） */
const channelModelAllChecked = ref(true);

// 模型行内统计：channel_daily_stats + 明细日志聚合（打开弹窗时拉取一次）
const channelModelStatsMap = ref<Map<string, ChannelModelUsageStats>>(new Map());
const loadingModelStats = ref(false);

// 模型级代理出口覆盖草稿：model(小写) → 规则；无条目 = 跟随渠道级配置
type ModelProxyMode = "" | "direct" | "pool" | "fixed";
const modelProxyModeDraft = ref<Map<string, { mode: ModelProxyMode; nodeId: string }>>(new Map());

/** 某模型当前生效的代理模式（草稿优先，无草稿回退渠道级配置推导） */
function effectiveProxyMode(model: string): ModelProxyMode | "inherit" {
  const key = model.toLowerCase();
  const draft = modelProxyModeDraft.value.get(key);
  if (draft && draft.mode) return draft.mode;
  const ch = selectedChannel.value;
  if (!ch) return "inherit";
  const rule = ch.modelProxyRules?.find((r) => r.model.toLowerCase() === key);
  if (rule) return (rule.mode as ModelProxyMode) || "inherit";
  // 渠道级配置推导（fixed_channel/custom_node 在模型粒度均归入「固定」族）
  const mode = channelProxyModeOf(ch);
  return mode === "pool" ? "pool" : mode === "direct" ? "direct" : "fixed";
}

/** 渠道级配置的推导描述（用于下拉「跟随渠道」的提示文案） */
const channelLevelProxyLabel = computed(() => {
  const ch = selectedChannel.value;
  if (!ch) return "";
  const mode = channelProxyModeOf(ch);
  if (mode === "fixed_channel") return "渠道级：固定通道";
  if (mode === "custom_node") return "渠道级：自定义节点";
  if (mode === "pool") return "渠道级：代理池轮询";
  return "渠道级：直连";
});

/** 渠道级配置推导出的代理模式（用于「与渠道不同」角标判断；fixed_channel 在模型粒度按固定节点族展示） */
function effectiveChannelProxyMode(): Exclude<ModelProxyMode, ""> {
  const ch = selectedChannel.value;
  if (!ch) return "direct";
  const mode = channelProxyModeOf(ch);
  if (mode === "pool") return "pool";
  if (mode === "direct") return "direct";
  return "fixed";
}

function setModelProxyMode(model: string, mode: ModelProxyMode) {
  const key = model.toLowerCase();
  if (!mode) {
    // 切回「跟随渠道」：清掉该模型的草稿与既有规则（保存时落库为无覆盖）
    modelProxyModeDraft.value.delete(key);
    modelProxyModeDraft.value = new Map(modelProxyModeDraft.value);
    return;
  }
  const existing = modelProxyModeDraft.value.get(key);
  modelProxyModeDraft.value.set(key, { mode, nodeId: existing?.nodeId ?? "" });
  modelProxyModeDraft.value = new Map(modelProxyModeDraft.value);
}

/** 模型行的 Key 专属限制：仅统计显式配置了 supportedModels 的 Key，返回该模型命中的 Key 数；-1 表示不适用 */
function keySupportCountFor(model: string): number {
  const keys = channelRawKeys.value;
  if (keys.length === 0) return -1;
  let hits = 0;
  for (const k of keys) {
    if (!k.supportedModels?.length) continue;
    if (k.supportedModels.some((m) => m.toLowerCase() === model.toLowerCase())) hits += 1;
  }
  return hits;
}

/** 是否所有含专属限制的 Key 都支持该模型（无限制 Key 存在时视为自由） */
function isModelFreeForAllKeys(model: string): boolean {
  const restricted = channelRawKeys.value.filter((k) => k.supportedModels?.length);
  if (restricted.length === 0) return true;
  return restricted.every(
    (k) => k.supportedModels!.some((m) => m.toLowerCase() === model.toLowerCase()),
  );
}

/** Token 数格式化：12.3k / 4.5M / 860 */
function fmtCompactTokens(n?: number | null): string {
  if (!n || n <= 0) return "0";
  if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(1)}B`;
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

/** 最近使用时间的人性化展示：刚刚/N 分钟前/N 小时前/昨天/N 天前/具体日期 */
function fmtLastUsed(ts?: string | null): string {
  if (!ts) return "从未调用";
  const t = new Date(ts.replace(" ", "T"));
  if (Number.isNaN(t.getTime())) return ts;
  const diffMs = Date.now() - t.getTime();
  const min = Math.floor(diffMs / 60_000);
  if (min < 1) return "刚刚";
  if (min < 60) return `${min} 分钟前`;
  const hour = Math.floor(min / 60);
  if (hour < 24) return `${hour} 小时前`;
  const day = Math.floor(hour / 24);
  if (day === 1) return "昨天";
  if (day < 30) return `${day} 天前`;
  return ts.slice(0, 10);
}

function isModelChecked(model: string): boolean {
  if (channelModelAllChecked.value) return true;
  return !!channelModelSelection.value[model];
}

/** 当前选中渠道的模型列表（弹窗数据源：优先取草稿） */
function selectedChannelModels(): string[] {
  return channelDraftModels.value;
}

/** 当前勾选数量（按现有模型列表计算），用于头部计数与保存结果 */
const channelCheckedCount = computed(
  () => selectedChannelModels().filter((m) => isModelChecked(m)).length,
);

function toggleModel(model: string) {
  const list = selectedChannelModels();
  if (channelModelAllChecked.value) {
    // 首次取消勾选：从全选模式切换为显式列表，仅取消当前这一个
    const map: Record<string, boolean> = {};
    for (const m of list) map[m] = true;
    map[model] = false;
    channelModelSelection.value = map;
    channelModelAllChecked.value = false;
    return;
  }
  const map = { ...channelModelSelection.value };
  if (map[model]) delete map[model];
  else map[model] = true;
  channelModelSelection.value = map;
}

function selectAllChannelModels() {
  channelModelAllChecked.value = true;
  channelModelSelection.value = {};
}

function clearChannelModels() {
  channelModelAllChecked.value = false;
  channelModelSelection.value = {};
}

// —— 多渠道共同提供的模型：倒排索引 + 路由顺序草稿 ——

/** model(小写) → 提供该模型的启用渠道列表（对外可见口径，白名单过滤后）；仅保留 ≥2 个渠道的条目 */
const channelOverlapByModel = computed<Map<string, ChannelConfig[]>>(() => {
  const byModel = new Map<string, ChannelConfig[]>();
  for (const channel of proxyConfig.value.channels) {
    if (!channel.enabled) continue;
    const models = [...new Set(filterChannelModels(channel, modelsForChannel(channel.id)))];
    for (const model of models) {
      const list = byModel.get(model.toLowerCase()) ?? [];
      list.push(channel);
      byModel.set(model.toLowerCase(), list);
    }
  }
  const overlaps = new Map<string, ChannelConfig[]>();
  for (const [model, channels] of byModel) {
    if (channels.length >= 2) overlaps.set(model, channels);
  }
  return overlaps;
});

async function refreshChannelDraftModels() {
  const channel = selectedChannel.value;
  if (!channel) return;
  fetchingDraftModels.value = true;
  try {
    // 刷新 = 始终远程获取，替换原有模型列表
    const fetchedMap = await fetchUpstreamModels({
      setGlobalFetching: false,
      channelId: channel.id,
    });
    if (fetchedMap[channel.id]?.length) {
      // 更新弹窗草稿列表为远程获取的最新模型
      channelDraftModels.value = [...fetchedMap[channel.id]];
      // 同步到全局渠道模型缓存
      channelModels.value = {
        ...channelModels.value,
        [channel.id]: [...fetchedMap[channel.id]],
      };
    }
  } finally {
    fetchingDraftModels.value = false;
  }
}

async function saveChannelModelSelection() {
  const channel = selectedChannel.value;
  if (!channel) return;

  // 1. 同步弹窗草稿模型到全局渠道模型缓存
  if (channelDraftModels.value.length > 0) {
    channelModels.value = {
      ...channelModels.value,
      [channel.id]: [...channelDraftModels.value],
    };
  }

  // 2. 全选 = 不配置白名单（全部启用）；部分勾选 = 白名单；一个不勾 = 不暴露任何模型
  if (channelModelAllChecked.value) {
    channel.enabledModels = null;
  } else {
    channel.enabledModels = selectedChannelModels().filter((m) => isModelChecked(m));
  }

  // 3. 保存该渠道的 Key 分组与规则设置（无 Key 的渠道不落库任何分组配置）
  if (channelRawKeys.value.length > 0) {
    channel.keyGroups = channelDraftKeyGroups.value.map((g: KeyGroupItem) => ({
      id: g.id.trim(),
      name: g.name.trim() || g.id.trim(),
      enabled: g.enabled,
    }));
    channel.keyRules = channelRawKeys.value.map((k) => ({
      key: k.key,
      groupId: k.groupId,
      enabled: k.enabled,
      supportedModels: k.supportedModels,
      fixedChannelId: k.fixedChannelId || null,
    }));
  }

  // 3.5 模型级代理出口覆盖：本弹窗内出现过的模型以草稿为准（无草稿 = 移除覆盖），
  // 其余模型的既有规则原样保留；空列表归一为 null
  {
    const draft = modelProxyModeDraft.value;
    const kept = (channel.modelProxyRules ?? []).filter(
      (r) => !draft.has(r.model.toLowerCase()),
    );
    const added = selectedChannelModels().flatMap((m) => {
      const d = draft.get(m.toLowerCase());
      if (!d?.mode) return [];
      return [{
        model: m,
        mode: d.mode,
        nodeId: d.mode === "fixed" && d.nodeId.trim() ? d.nodeId.trim() : null,
      }];
    });
    const merged = [...kept, ...added];
    channel.modelProxyRules = merged.length > 0 ? merged : null;
  }

  // 4. 只有内部保存时才触发全局「可用模型」加载状态
  fetchingModels.value = true;
  try {
    const ok = await saveConfig(proxyConfig.value);
    if (ok) {
      showToast(`已更新「${channel.name}」渠道可用模型（${channelCheckedCount.value} 个已启用）`);
      closeChannelModelsModal();
    }
  } finally {
    fetchingModels.value = false;
  }
}

// —— 渠道「设置」弹窗：别名 / 代理设置（四模式合一） ——
type ChannelProxyMode = "direct" | "pool" | "fixed_channel" | "custom_node";
interface ChannelSettingsDraft {
  alias: string;
  proxyMode: ChannelProxyMode;
  /** custom_node 模式锁定的代理池节点 ID */
  fixedProxyNode: string;
  /** fixed_channel 模式的渠道默认固定通道 ID */
  proxyFixedChannel: string;
}

const channelSettingsTarget = ref<ChannelConfig | null>(null);
const channelSettingsDraft = ref<ChannelSettingsDraft>({
  alias: "",
  proxyMode: "direct",
  fixedProxyNode: "",
  proxyFixedChannel: "",
});
const channelSettingsError = ref("");
const channelSettingsTargetIsBuiltin = computed(
  () => channelSettingsTarget.value != null && isBuiltinChannel(channelSettingsTarget.value),
);

/** 代理池节点候选（自定义节点模式选择用，测速成功者优先按延迟升序） */
const proxyPoolNodeOptions = ref<{ id: string; name: string; latencyMs: number | null }[]>([]);
/** 代理池固定通道候选（固定通道模式选择用） */
const proxyPoolChannelOptions = ref<{ id: string; name: string }[]>([]);

async function loadProxyPoolOptions() {
  try {
    const state = await runCommand<Record<string, any>>("get_proxy_pool_state");
    proxyPoolNodeOptions.value = ((state?.nodes as any[]) ?? [])
      .filter((n) => n.testStatus === "success")
      .map((n) => ({ id: n.id, name: n.name, latencyMs: n.latencyMs ?? null }))
      .sort((a, b) => (a.latencyMs ?? 99999) - (b.latencyMs ?? 99999));
    proxyPoolChannelOptions.value = ((state?.channels as any[]) ?? []).map((c) => ({
      id: c.id,
      name: c.name,
    }));
  } catch {
    /* 加载失败保持空列表，下拉展示空态 */
  }
}

/** 通道引用归一化：存量数据若以通道名称存库则转换为通道 ID，绑定与名称解耦；
 * 候选未加载或无法识别时原样返回（由「已删除」标记兜底展示） */
function normalizePoolChannelRef(value: string): string {
  const v = value.trim();
  if (!v || proxyPoolChannelOptions.value.length === 0) return v;
  if (proxyPoolChannelOptions.value.some((c) => c.id === v)) return v;
  return proxyPoolChannelOptions.value.find((c) => c.name === v)?.id ?? v;
}

/** 通道引用已失联（绑定后被删除）：候选加载完成且无匹配时下拉展示「已删除」标记 */
function isDeletedPoolChannelRef(value: string): boolean {
  const v = value.trim();
  if (!v || proxyPoolChannelOptions.value.length === 0) return false;
  return !proxyPoolChannelOptions.value.some((c) => c.id === v);
}

/** 渠道配置 → 四模式的归一化推导（兼容旧布尔字段） */
function channelProxyModeOf(ch: ChannelConfig | null): ChannelProxyMode {
  if (!ch) return "direct";
  const mode = String(ch.proxyMode ?? "").trim().toLowerCase();
  if (mode === "pool" || mode === "fixed_channel" || mode === "custom_node" || mode === "direct") {
    return mode as ChannelProxyMode;
  }
  if (ch.useFixedProxy) return "custom_node";
  if (ch.useProxyPool) return "pool";
  return "direct";
}

/** 管理模型弹窗当前渠道是否为固定通道模式（Key 行展示通道绑定下拉） */
const selectedChannelIsFixedChannel = computed(
  () => channelProxyModeOf(selectedChannel.value) === "fixed_channel",
);

function handleOpenChannelSettingsDialog(channel: ChannelConfig) {
  channelSettingsTarget.value = channel;
  channelSettingsDraft.value = {
    alias: channelAlias(channel),
    proxyMode: channelProxyModeOf(channel),
    fixedProxyNode: channel.fixedProxyNode ?? "",
    proxyFixedChannel: channel.proxyFixedChannel ?? "",
  };
  channelSettingsError.value = "";
  channelSettingsDialogOpen.value = true;
  // 通道候选加载完成后归一化已绑定通道引用（名称 → ID）；期间用户改动则跳过
  const original = channelSettingsDraft.value.proxyFixedChannel;
  void loadProxyPoolOptions().then(() => {
    if (
      channelSettingsDialogOpen.value &&
      channelSettingsDraft.value.proxyFixedChannel === original
    ) {
      channelSettingsDraft.value.proxyFixedChannel = normalizePoolChannelRef(original);
    }
  });
}

function closeChannelSettingsDialog() {
  channelSettingsDialogOpen.value = false;
}

/** 内置固化渠道（后端保留 statsId 1-100，opencode=1；动态渠道从 101 起分配） */
/** 出口路径是否与入口不同（相同则无需展示"出"行，默认都是 chat 路径） */
const STANDARD_EGRESS_PATHS = ["/v1/chat/completions", "/v1/messages", "/v1/responses"];
function upstreamPathDiffers(path: string, upstreamUrl: string | null | undefined): boolean {
  if (!upstreamUrl) return false;
  try {
    const u = new URL(upstreamUrl);
    const p = u.pathname;
    // 标准协议路径（chat/messages/responses/gemini）一律视为"默认"，不展示"出"行
    if (STANDARD_EGRESS_PATHS.includes(p) || p.startsWith("/v1/gemini")) return false;
    return p !== path;
  } catch {
    return upstreamUrl !== path;
  }
}

/** 格式化出网上游地址：去掉 scheme 前缀只保留 host+path，超长截断 */
function formatUpstreamUrl(url: string | null | undefined): string {
  if (!url) return "";
  try {
    const u = new URL(url);
    const display = (u.host + u.pathname + u.search).replace(/\/+$/, "");
    return display.length > 42 ? display.slice(0, 41) + "…" : display;
  } catch {
    return url.length > 42 ? url.slice(0, 41) + "…" : url;
  }
}

function isBuiltinChannel(channel: ChannelConfig): boolean {
  return channel.statsId != null && channel.statsId > 0 && channel.statsId < 101;
}

/**
 * 渠道有效 Key 数量：与后端 get_effective_keys 同口径——
 * apiKeys 非空时以其为准（多 Key 自动轮换），否则回退单 apiKey；空串不计。
 * opencode 匿名模式无 Key 时为 0。
 */
function channelKeyCount(channel: ChannelConfig): number {
  const list = channel.apiKeys?.length
    ? channel.apiKeys
    : channel.apiKey?.trim()
      ? [channel.apiKey]
      : [];
  return new Set(list.map((k) => k.trim()).filter(Boolean)).size;
}

// —— 站点关联渠道继承的 Key 数量（运行时从站点模型缓存读取，跨账号去重）——
const siteInheritedKeyCounts = ref<Record<string, number>>({});

/** 按后端 resolve_channel_api_keys 同口径统计：汇总 site_model_cache 各账号 keys 并全局去重 */
async function refreshSiteInheritedKeyCounts() {
  const siteIds = [
    ...new Set(
      proxyConfig.value.channels
        .map((c) => c.siteId)
        .filter((v): v is string => !!v && v.trim() !== ""),
    ),
  ];
  const counts: Record<string, number> = {};
  await Promise.all(
    siteIds.map(async (siteId) => {
      try {
        const cache = await runCommand<{ accounts?: { keys?: string[] }[] }>(
          "get_site_model_cache",
          { siteId },
        );
        const seen = new Set<string>();
        for (const account of cache?.accounts ?? []) {
          for (const key of account.keys ?? []) {
            const trimmed = key.trim();
            if (trimmed) seen.add(trimmed);
          }
        }
        counts[siteId] = seen.size;
      } catch {
        /* 缓存读取失败时保持上次结果 */
      }
    }),
  );
  if (Object.keys(counts).length > 0) {
    siteInheritedKeyCounts.value = { ...siteInheritedKeyCounts.value, ...counts };
  }
}

function channelInheritedKeyCount(channel: ChannelConfig): number {
  if (!channel.siteId) return 0;
  return siteInheritedKeyCounts.value[channel.siteId] ?? 0;
}

/** 校验别名：合法字符 + 全渠道唯一（含 opencode）。返回错误信息，空串表示通过。 */
function validateAlias(alias: string, excludeId?: string): string {
  const a = alias.trim().toLowerCase();
  if (!a) return "请填写英文别名";
  if (!isValidChannelAlias(a)) return "英文别名只能包含英文字母、数字、- 与 _";
  const conflict = proxyConfig.value.channels.find(
    (c) => c.id !== excludeId && channelAlias(c) === a,
  );
  if (conflict) return `别名「${a}」已存在（${conflict.name}），所有渠道别名不能重复`;
  return "";
}

async function saveChannelSettings() {
  const channel = channelSettingsTarget.value;
  if (!channel) return;
  // 固化渠道别名固定（网关模型前缀依赖它），无论草稿值如何都保持原样
  const nextAlias = isBuiltinChannel(channel)
    ? channelAlias(channel)
    : channelSettingsDraft.value.alias.trim().toLowerCase();
  const err = validateAlias(nextAlias, channel.id);
  if (err) {
    channelSettingsError.value = err;
    return;
  }
  channel.alias = nextAlias;
  // 四模式合一落库；旧布尔字段同步维护，兼容旧版读取方
  const mode = channelSettingsDraft.value.proxyMode;
  channel.proxyMode = mode;
  channel.proxyFixedChannel =
    mode === "fixed_channel" ? channelSettingsDraft.value.proxyFixedChannel || null : null;
  channel.fixedProxyNode =
    mode === "custom_node" ? channelSettingsDraft.value.fixedProxyNode || null : null;
  channel.useProxyPool = mode === "pool";
  channel.useFixedProxy = mode === "custom_node" || mode === "fixed_channel";
  const ok = await saveConfig(proxyConfig.value);
  if (ok) {
    const modeLabel =
      mode === "pool"
        ? "代理池轮询"
        : mode === "fixed_channel"
          ? "代理池固定通道"
          : mode === "custom_node"
            ? "自定义节点"
            : "强制直连";
    showToast(`已更新「${channel.name}」渠道设置（别名 ${channel.alias} · ${modeLabel}）`);
    channelSettingsDialogOpen.value = false;
  }
}

// —— 渠道删除 ——
const deleteChannelModalOpen = ref(false);
const deletingChannel = ref<ChannelConfig | null>(null);

function handleOpenDeleteChannelModal(channel: ChannelConfig) {
  deletingChannel.value = channel;
  deleteChannelModalOpen.value = true;
}

function closeDeleteChannelModal() {
  deleteChannelModalOpen.value = false;
  deletingChannel.value = null;
}

async function confirmDeleteChannel() {
  if (!deletingChannel.value) return;
  const channel = deletingChannel.value;
  const targetId = channel.id;
  const name = channel.name;

  proxyConfig.value.channels = proxyConfig.value.channels.filter((c) => c.id !== targetId);
  if (channelModels.value[targetId]) {
    delete channelModels.value[targetId];
  }

  const ok = await saveConfig(proxyConfig.value);
  if (ok) {
    showToast(`已删除反代渠道「${name}」`);
    closeDeleteChannelModal();
  }
}

// —— 站点转换：从站点库「在用且存活」的站点创建反代渠道 ——
const { sites: librarySites, loadLibrary } = useLibrary();
const siteConvertDialogOpen = ref(false);
const convertSelectedSite = ref<SiteRecord | null>(null);
const convertAlias = ref("");
const convertApiBaseUrl = ref("");
const convertAliasError = ref("");
const convertModelLoading = ref(false);
const convertSiteModelCount = ref(0);
const convertSiteSearch = ref("");

/** 各站点模型缓存摘要：Key/模型/账号数，供转换列表展示 */
interface SiteCacheSummary {
  keyCount: number;
  modelCount: number;
  accountCount: number;
}
const siteCacheSummaries = ref<Record<string, SiteCacheSummary>>({});

function siteCacheSummary(siteId: string): SiteCacheSummary | undefined {
  return siteCacheSummaries.value[siteId];
}

/** 一次拉取全部站点缓存，统计各站点的 Key（去重）、模型、账号数 */
async function refreshSiteCacheSummaries() {
  try {
    const entries = await runCommand<
      { siteId: string; cache: { models?: unknown[]; accounts?: { keys?: string[] }[] } }[]
    >("get_all_site_model_caches");
    const next: Record<string, SiteCacheSummary> = {};
    for (const entry of entries ?? []) {
      const keySet = new Set<string>();
      for (const account of entry.cache?.accounts ?? []) {
        for (const key of account.keys ?? []) {
          const trimmed = key.trim();
          if (trimmed) keySet.add(trimmed);
        }
      }
      next[entry.siteId] = {
        keyCount: keySet.size,
        modelCount: entry.cache?.models?.length ?? 0,
        accountCount: entry.cache?.accounts?.length ?? 0,
      };
    }
    siteCacheSummaries.value = next;
  } catch {
    /* 忽略：摘要缺失时按 0 展示 */
  }
}

/** 在用且存活（未标记跑路）的站点；已转换为渠道的排除在外 */
const convertibleSites = computed(() => {
  const convertedIds = new Set(
    proxyConfig.value.channels.map((c) => c.siteId).filter((v): v is string => !!v),
  );
  return librarySites.value
    .filter((s) => s.isPersonal && !s.isRunaway)
    .filter((s) => !convertedIds.has(s.id))
    .slice()
    .sort((a, b) => a.name.localeCompare(b.name, "zh-CN"));
});

/** 转换弹窗内搜索：匹配站点名称或 API 地址 */
const filteredConvertibleSites = computed(() => {
  const q = convertSiteSearch.value.trim().toLowerCase();
  if (!q) return convertibleSites.value;
  return convertibleSites.value.filter(
    (s) =>
      s.name.toLowerCase().includes(q) || s.apiBaseUrl.toLowerCase().includes(q),
  );
});

/** 从站点名生成英文别名（中文名回退为 site），并保证与现有渠道别名不冲突 */
function slugifySiteName(name: string): string {
  const base = (name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 24)) || "site";
  return base;
}

function uniqueChannelAlias(base: string): string {
  const existing = new Set(proxyConfig.value.channels.map((c) => channelAlias(c)));
  if (!existing.has(base)) return base;
  let i = 2;
  while (existing.has(`${base}-${i}`)) i += 1;
  return `${base}-${i}`;
}

function openSiteConvertDialog() {
  convertSelectedSite.value = null;
  convertAlias.value = "";
  convertApiBaseUrl.value = "";
  convertAliasError.value = "";
  convertSiteModelCount.value = 0;
  convertSiteSearch.value = "";
  siteConvertDialogOpen.value = true;
  if (librarySites.value.length === 0) {
    void loadLibrary();
  }
  void refreshSiteCacheSummaries();
}

function closeSiteConvertDialog() {
  siteConvertDialogOpen.value = false;
}

/** 选择站点：预填 API 地址、自动生成唯一别名，并读取站点同步的全部原 Key（继承，无需选择） */
async function selectConvertSite(site: SiteRecord) {
  convertSelectedSite.value = site;
  convertApiBaseUrl.value = site.apiBaseUrl.trim();
  convertAlias.value = uniqueChannelAlias(slugifySiteName(site.name));
  convertAliasError.value = "";
  convertSiteModelCount.value = 0;
  convertModelLoading.value = true;
  try {
    const cache = await runCommand<{ models?: { id: string }[] }>("get_site_model_cache", {
      siteId: site.id,
    });
    const modelIds = Array.isArray(cache?.models) ? cache.models.map((m) => m.id).filter(Boolean) : [];
    convertSiteModelCount.value = modelIds.length;
    if (modelIds.length > 0) {
      channelModels.value[`site_${site.id}`] = modelIds;
    }
  } catch {
    /* 忽略：模型缓存由网关运行时按 siteId 读取 */
  } finally {
    convertModelLoading.value = false;
  }
}

watch(convertAlias, (val) => {
  convertAliasError.value = validateAlias(val);
});

async function confirmConvertSite() {
  const site = convertSelectedSite.value;
  if (!site) return;
  const err = validateAlias(convertAlias.value);
  if (err) {
    convertAliasError.value = err;
    return;
  }
  if (!convertApiBaseUrl.value.trim()) {
    convertAliasError.value = "请填写 API 地址";
    return;
  }
  const channel: ChannelConfig = {
    id: `site_${site.id}`,
    name: site.name,
    description: `由站点「${site.name}」转换而来的反代渠道（运行时使用关联站点 Key）`,
    enabled: true,
    protocol: "openai",
    upstreamUrl: convertApiBaseUrl.value.trim(),
    apiKey: "",
    apiKeys: [],
    // 站点转换渠道默认强制直连，代理策略可在渠道设置的四模式中选择
    useProxyPool: false,
    proxyMode: "direct",
    alias: convertAlias.value.trim().toLowerCase(),
    siteId: site.id,
    useFixedProxy: false,
    proxyFixedChannel: null,
    enabledModels: null,
  };
  proxyConfig.value.channels.push(channel);
  const ok = await saveConfig(proxyConfig.value);
  if (ok) {
    showToast(`已将「${site.name}」转换为反代渠道（别名 ${channel.alias}）`);
    siteConvertDialogOpen.value = false;
    void refreshModels();
  } else {
    proxyConfig.value.channels = proxyConfig.value.channels.filter((c) => c.id !== channel.id);
  }
}

// —— 渠道卡片底部使用统计 ——
const emptyChannelStats: ChannelUsageStats = {
  channelId: "",
  totalRequests: 0,
  successfulRequests: 0,
  failedRequests: 0,
  totalTokens: 0,
  todayTotalTokens: 0,
};

function channelStatsFor(channel: ChannelConfig): ChannelUsageStats {
  const key = channelStatsKey(channel);
  return channelStats.value[key]
    ?? channelStats.value[channelAlias(channel)]
    ?? channelStats.value[channel.id]
    ?? emptyChannelStats;
}

function channelSuccessRate(channel: ChannelConfig): string {
  const s = channelStatsFor(channel);
  if (s.totalRequests <= 0) return "—";
  return `${((s.successfulRequests / s.totalRequests) * 100).toFixed(1)}%`;
}

function channelTodaySuccessRate(channel: ChannelConfig): string {
  const s = channelStatsFor(channel);
  const today = s.todayRequests ?? 0;
  if (today <= 0) return "-";
  return `${(((s.todaySuccessfulRequests ?? 0) / today) * 100).toFixed(1)}%`;
}

/** 有请求但成功率低于 90% 时标红提示 */
function channelSuccessRateBad(channel: ChannelConfig): boolean {
  const s = channelStatsFor(channel);
  return s.totalRequests > 0 && s.successfulRequests / s.totalRequests < 0.9;
}

const detailActiveTab = ref<"overview" | "request" | "response" | "reasoning" | "error">("overview");

function openLogDetail(log: ProxyRequestLog) {
  selectedLogForDetail.value = log;
  if (log.statusCode >= 400) {
    detailActiveTab.value = "error";
  } else {
    detailActiveTab.value = "overview";
  }
}

function closeLogDetail() {
  selectedLogForDetail.value = null;
}

function getCacheHitRate(log: ProxyRequestLog): string {
  const prompt = log.promptTokens || 0;
  const hit = log.promptCacheHitTokens || 0;
  if (prompt <= 0) return "0.0";
  return ((hit / prompt) * 100).toFixed(1);
}

/** 新增输入 = 总输入 - 缓存命中（扣掉复用部分才是真实新开销） */
function getNewInputTokens(log: ProxyRequestLog): number {
  return Math.max(0, (log.promptTokens || 0) - (log.promptCacheHitTokens || 0));
}

/** 生成输出（纯文本）= 总输出 − 思考推理（上游 usage.completion_tokens 通常已含 reasoning） */
function getOutputTextTokens(log: ProxyRequestLog): number {
  return Math.max(0, (log.completionTokens || 0) - (log.reasoningTokens || 0));
}

/** 总消耗：优先上游回传 total，缺省时按 输入+输出 合成 */
function getTokenTotal(log: ProxyRequestLog): number {
  return log.totalTokens ?? ((log.promptTokens || 0) + (log.completionTokens || 0));
}

/** 分项占总消耗百分比（与四卡片加法口径一致：新增输入 + 缓存命中 + 思考 + 输出 = 总量） */
function getTokenSharePct(log: ProxyRequestLog, part: number): string {
  const total = getTokenTotal(log);
  if (!total || total <= 0) return "--";
  return `${Math.round((Math.max(0, part) / total) * 100)}%`;
}

function getEstimatedTps(log: ProxyRequestLog): string {
  const tokens = getOutputTextTokens(log);
  if (tokens <= 0) return "0.0";
  const genDur = log.ttftMs && log.durationMs > log.ttftMs ? log.durationMs - log.ttftMs : log.durationMs;
  if (genDur <= 0) return "0.0";
  return ((tokens / genDur) * 1000).toFixed(1);
}

async function copyText(text: string, label = "内容") {
  try {
    await navigator.clipboard.writeText(text);
    showToast(`已复制 ${label}`);
  } catch {
    showToast("复制失败", true);
  }
}

// —— 响应报文结构化解析：按出现顺序拆出 正文 / 思考 / 工具调用 片段 ——
const responseViewMode = ref<"structured" | "raw">("structured");
const responseSegments = computed(() => parseResponseSegments(selectedLogForDetail.value?.responseBody));

type ResponseSegment =
  | { kind: "text"; text: string }
  | { kind: "reasoning"; text: string }
  | { kind: "tool"; name: string; args: string; callId?: string; segIndex?: number };;

function appendSegment(list: ResponseSegment[], seg: ResponseSegment) {
  const last = list[list.length - 1];
  // 相邻同类文本/思考段合并；工具段各自独立
  if (
    last &&
    ((last.kind === "text" && seg.kind === "text") ||
      (last.kind === "reasoning" && seg.kind === "reasoning"))
  ) {
    last.text += seg.text;
    return;
  }
  list.push(seg);
}

/** 把上游响应（SSE 原文或非流式 JSON）解析为顺序片段：正文 / 思考 / 工具调用 */
function parseResponseSegments(body: string | undefined | null): ResponseSegment[] {
  const segments: ResponseSegment[] = [];
  if (!body) return segments;
  const text = body.trim();

  const feedChatDelta = (delta: any) => {
    if (!delta || typeof delta !== "object") return;
    const reasoning =
      typeof delta.reasoning_content === "string"
        ? delta.reasoning_content
        : typeof delta.reasoning === "string"
          ? delta.reasoning
          : "";
    if (reasoning) appendSegment(segments, { kind: "reasoning", text: reasoning });
    if (typeof delta.content === "string" && delta.content) {
      appendSegment(segments, { kind: "text", text: delta.content });
    }
    for (const tc of Array.isArray(delta.tool_calls) ? delta.tool_calls : []) {
      const name = tc?.function?.name || "";
      const frag = typeof tc?.function?.arguments === "string" ? tc.function.arguments : "";
      const idx = typeof tc?.index === "number" ? tc.index : -1;
      // 同一 index 的分片追加到既有工具段（流式参数是切片下发）
      const existing = [...segments].reverse().find(
        (s) => s.kind === "tool" && (idx >= 0 ? (s as any).segIndex === idx : !s.name),
      ) as Extract<ResponseSegment, { kind: "tool" }> | undefined;
      if (existing) {
        if (name) existing.name = name;
        existing.args += frag;
      } else {
        segments.push({ kind: "tool", name, args: frag, callId: tc?.id, segIndex: idx });
      }
    }
  };

  const feedAnthropicEvent = (frame: any) => {
    if (!frame || typeof frame !== "object") return;
    if (frame.type === "content_block_start") {
      const block = frame.content_block ?? {};
      if (block.type === "tool_use") {
        segments.push({ kind: "tool", name: block.name ?? "", args: "", callId: block.id, segIndex: frame.index });
      }
      return;
    }
    if (frame.type === "content_block_delta") {
      const d = frame.delta ?? {};
      if (d.type === "text_delta" && typeof d.text === "string") {
        appendSegment(segments, { kind: "text", text: d.text });
      } else if (d.type === "thinking_delta" && typeof d.thinking === "string") {
        appendSegment(segments, { kind: "reasoning", text: d.thinking });
      } else if (d.type === "input_json_delta" && typeof d.partial_json === "string") {
        const existing = [...segments].reverse().find(
          (s) => s.kind === "tool" && (s as any).segIndex === frame.index,
        ) as Extract<ResponseSegment, { kind: "tool" }> | undefined;
        if (existing) existing.args += d.partial_json;
      }
    }
  };

  const ingestFrame = (frame: any) => {
    if (!frame || typeof frame !== "object") return;
    if (frame.choices?.[0]?.delta) return feedChatDelta(frame.choices[0].delta);
    if (frame.type?.startsWith("content_block")) return feedAnthropicEvent(frame);
  };

  // 形态 A：SSE 原文（流式日志保存形态）
  if (text.includes("data:") || text.includes("event:")) {
    for (const line of text.split("\n")) {
      const m = line.match(/^\s*data:\s*(.+)$/);
      if (!m || m[1] === "[DONE]") continue;
      try {
        ingestFrame(JSON.parse(m[1]));
      } catch {
        // 跳过心跳/坏帧
      }
    }
    if (segments.length) return segments;
  }

  // 形态 B：非流式 JSON —— OpenAI Chat / Anthropic Messages / Gemini
  try {
    const jv = JSON.parse(text);
    const msg = jv?.choices?.[0]?.message;
    if (msg) {
      if (typeof msg.reasoning_content === "string" && msg.reasoning_content)
        appendSegment(segments, { kind: "reasoning", text: msg.reasoning_content });
      else if (typeof msg.reasoning === "string" && msg.reasoning)
        appendSegment(segments, { kind: "reasoning", text: msg.reasoning });
      if (typeof msg.content === "string" && msg.content)
        appendSegment(segments, { kind: "text", text: msg.content });
      for (const tc of Array.isArray(msg.tool_calls) ? msg.tool_calls : []) {
        segments.push({
          kind: "tool",
          name: tc?.function?.name ?? "",
          args: typeof tc?.function?.arguments === "string" ? tc.function.arguments : JSON.stringify(tc?.function?.arguments ?? {}),
          callId: tc?.id,
        });
      }
      if (segments.length) return segments;
    }
    if (Array.isArray(jv?.content)) {
      for (const block of jv.content) {
        if (block?.type === "text" && block.text) appendSegment(segments, { kind: "text", text: block.text });
        else if ((block?.type === "thinking" || block?.type === "redacted_thinking") && block.thinking)
          appendSegment(segments, { kind: "reasoning", text: block.thinking });
        else if (block?.type === "tool_use")
          segments.push({ kind: "tool", name: block.name ?? "", args: JSON.stringify(block.input ?? {}), callId: block.id });
      }
      if (segments.length) return segments;
    }
    const parts = jv?.candidates?.[0]?.content?.parts;
    if (Array.isArray(parts)) {
      for (const p of parts) {
        if (typeof p?.text === "string") {
          appendSegment(segments, p.thought ? { kind: "reasoning", text: p.text } : { kind: "text", text: p.text });
        } else if (p?.functionCall) {
          segments.push({ kind: "tool", name: p.functionCall.name ?? "", args: JSON.stringify(p.functionCall.args ?? {}) });
        }
      }
      if (segments.length) return segments;
    }
  } catch {
    // 非 JSON，落到 raw
  }

  // 无法识别形态：以纯文本段兜底展示
  return [{ kind: "text", text }];
}

/** 工具参数美化：合法 JSON 缩进，否则原样 */
function prettyToolArgs(args: string): string {
  try {
    return JSON.stringify(JSON.parse(args || "{}"), null, 2);
  } catch {
    return args || "{}";
  }
}

/** 正文片段 Markdown 渲染（marked + DOMPurify 消毒，防上游注入） */
function renderSegmentMarkdown(text: string): string {
  try {
    return DOMPurify.sanitize(marked.parse(text, { async: false }) as string);
  } catch {
    return `<pre>${text.replace(/</g, "&lt;")}</pre>`;
  }
}

// —— 报文一键格式化：日志展示上游原文，格式化仅作为查看辅助（可还原） ——
const bodyFormatOverride = ref<{ request?: string; response?: string }>({});

watch(selectedLogForDetail, () => {
  bodyFormatOverride.value = {};
});

/** 详情报文展示文本：未格式化时即后端保存的原文 */
function displayBody(kind: "request" | "response"): string {
  const raw =
    kind === "request"
      ? selectedLogForDetail.value?.requestBody
      : selectedLogForDetail.value?.responseBody;
  return bodyFormatOverride.value[kind] ?? raw ?? "";
}

/** 一键格式化：单一 JSON 直接美化；SSE 流逐帧美化每个 data 行（失败保持原行） */
function formatLogBody(kind: "request" | "response") {
  const src = displayBody(kind).trim();
  if (!src) {
    showToast("没有可格式化的内容", true);
    return;
  }
  try {
    bodyFormatOverride.value[kind] = JSON.stringify(JSON.parse(src), null, 2);
    showToast("已格式化为缩进 JSON");
    return;
  } catch {
    // 非单一 JSON，继续尝试 SSE 逐帧
  }
  const pretty = src
    .split("\n")
    .map((line) => {
      const m = line.match(/^data:\s*(.+)$/);
      if (!m || m[1] === "[DONE]") return line;
      try {
        return `data: ${JSON.stringify(JSON.parse(m[1]), null, 2).replace(/\n/g, "\n  ")}`;
      } catch {
        return line;
      }
    })
    .join("\n");
  if (pretty !== src) {
    bodyFormatOverride.value[kind] = pretty;
    showToast("已按 SSE 帧逐条格式化");
  } else {
    showToast("内容不是可解析的 JSON / SSE 报文", true);
  }
}

function getErrorSuggestion(log: ProxyRequestLog): string {
  if (log.statusCode === 401) {
    return "本地 Bearer API Key 校验未通过。请检查调用工具中的 Authorization Header 是否与服务配置中的访问密钥一致。";
  }
  if (log.statusCode === 502 || log.statusCode === 504) {
    return "上游网关连接超时或连接中断。若开启了代理池，请检查代理节点是否通畅；若未开启，请检查上游端点连通性。";
  }
  if (log.statusCode === 503) {
    return "该渠道当前在系统配置中处于「已禁用」状态，请在主界面开启对应渠道开关。";
  }
  if (log.statusCode === 400 || log.statusCode === 422) {
    return "请求体参数格式不正确，或传入了上游不支持的模型名称与特殊参数。";
  }
  if (log.statusCode === 429) {
    return "OpenCode 免费通道对未认证单出口 IP 存在频次限制。网关已自动注入官方 CLI 指纹并支持本地凭据探测。若仍遇频次限制：① 在渠道设置中配置 OpenCode API Key；② 开启「内部代理池轮询」自动多节点切换与动态退避重试；③ 切换其他可用免费模型（如 mimo-v2.5-free / deepseek-v4-flash-free / big-pickle / nemotron-3-ultra-free）；④ 稍候 30 秒后自动恢复。";
  }
  return "请根据下方原始错误响应体排查上游返回的具体原因。";
}

function formatTokenBMK(val?: number | null): string {
  const num = Number(val ?? 0);
  if (!Number.isFinite(num) || num <= 0) return "0";
  if (num < 1000) return String(Math.round(num));
  if (num < 1_000_000) {
    const k = (num / 1000).toFixed(num < 100_000 ? 1 : 0).replace(/\.0$/, "");
    return `${k}K`;
  }
  if (num < 1_000_000_000) {
    const m = (num / 1_000_000).toFixed(num < 100_000_000 ? 1 : 0).replace(/\.0$/, "");
    return `${m}M`;
  }
  const b = (num / 1_000_000_000).toFixed(num < 100_000_000_000 ? 2 : 1).replace(/\.0+$/, "");
  return `${b}B`;
}

function formatCompactToken(val?: number | null): string {
  return formatTokenBMK(val);
}

function formatSec(ms?: number | null): string {
  if (ms == null || !Number.isFinite(ms)) return "--";
  const sec = ms / 1000;
  if (sec < 0.01 && ms > 0) return "<0.01s";
  return `${sec.toFixed(2)}s`;
}

// —— 控制台端点快速复制与指标聚合 ——
const openAiBaseUrl = computed(
  () => (isTauri && proxyStatus.value.url) || `${serviceOrigin.value}${API_PATH_V1}`
);
const claudeMessagesUrl = computed(
  () => `${serviceOrigin.value}${API_PATH_MESSAGES}`
);
const geminiV1Url = computed(
  () => `${serviceOrigin.value}${API_PATH_GEMINI}`
);
const geminiV1BetaUrl = computed(
  () => `${serviceOrigin.value}/v1beta`
);

const responsesApiUrl = computed(
  () => `${openAiBaseUrl.value.replace(/\/+$/, "")}/responses`
);
const chatCompletionsUrl = computed(
  () => `${openAiBaseUrl.value.replace(/\/+$/, "")}/chat/completions`
);

// 四个对话端点在卡片里只展示路径，完整地址悬停可见、复制时带上
const stripOrigin = (url: string) => url.replace(/^https?:\/\/[^/]+/, "");

async function copyChatCompletionsUrl() {
  await copyText(chatCompletionsUrl.value, "Chat Completions URL");
}

// —— 统计展示口径：优先持久化全渠道累计（日统计表汇总，重启不丢、不受日志裁剪影响），
//    概览未加载完成时回退本次运行计数器 ——
const statTotals = computed<GatewayOverviewTotals>(
  () =>
    gatewayOverview.value?.totals ?? {
      totalRequests: proxyStatus.value.totalRequests,
      successfulRequests: proxyStatus.value.successfulRequests,
      failedRequests: proxyStatus.value.failedRequests,
      avgDurationMs: 0,
      avgTtftMs: null,
      promptTokens: proxyStatus.value.totalPromptTokens ?? 0,
      completionTokens: proxyStatus.value.totalCompletionTokens ?? 0,
      reasoningTokens: proxyStatus.value.totalReasoningTokens ?? 0,
      cacheHitTokens: proxyStatus.value.totalCacheHitTokens ?? 0,
      totalTokens: proxyStatus.value.totalTokens ?? 0,
    },
);

/** 今日（本地时区）全渠道聚合：后端与所选区间解耦单独返回，切走今日区间时角标仍有数据 */
const todayStats = computed(() => gatewayOverview.value?.today ?? null);

async function copyAuthHeader() {
  const key = proxyConfig.value.apiKey?.trim() || "";
  if (!key) {
    showToast("API Key 尚未生成，请先保存网关配置或重启服务", true);
    return;
  }
  await copyText(`Authorization: Bearer ${key}`, "Authorization Header");
}

const liveSuccessRate = computed(() => {
  const total = statTotals.value.totalRequests;
  if (!total) return "100%";
  return `${((statTotals.value.successfulRequests / total) * 100).toFixed(1)}%`;
});

const liveCacheHitRate = computed(() => {
  const prompt = statTotals.value.promptTokens || 0;
  const hit = statTotals.value.cacheHitTokens || 0;
  if (!prompt) return "0%";
  return `${Math.round((hit / prompt) * 100)}%`;
});

// —— 全渠道趋势图表（持久化日统计 · 跟随亮暗主题）——
const { preferences } = usePreferences();

const overviewDays = computed(() => gatewayOverview.value?.days ?? 14);
const overviewHasData = computed(() =>
  (gatewayOverview.value?.daily ?? []).some((d) => d.totalRequests > 0),
);

/** 小时级趋势：区间 ≤3 天且有小时数据时后端返回，否则回退按日视图 */
const overviewHourly = computed<GatewayHourlyPoint[] | null>(
  () => gatewayOverview.value?.hourly ?? null,
);
const overviewIsHourly = computed(() => (overviewHourly.value?.length ?? 0) > 0);

/** 月级趋势：区间总天数 > 92（超过一个季度，如「今年」）时后端返回，否则按日 */
const overviewMonthly = computed<GatewayDailyPoint[] | null>(
  () => gatewayOverview.value?.monthly ?? null,
);
const overviewIsMonthly = computed(() => (overviewMonthly.value?.length ?? 0) > 0);

function localTodayStr(): string {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

const overviewIsAll = computed(() => !overviewDateFrom.value && !overviewDateTo.value);
const overviewIsToday = computed(
  () => overviewDateFrom.value === localTodayStr() && overviewDateTo.value === localTodayStr(),
);

/** 总览卡片头部的统计范围徽章：全部 / 今日 / 日期（区间），KPI 数字均按此口径 */
const overviewScopeLabel = computed(() => {
  if (overviewIsAll.value) return "全部";
  const f = overviewDateFrom.value;
  const t = overviewDateTo.value;
  if (f && t && f === t) return f === localTodayStr() ? "今日" : f;
  return `${f || "…"} ~ ${t || "…"}`;
});

/** 趋势卡聚合口径文案：区间模式显示日期区间，「全部」显示近 N 天窗口 */
const overviewRangeText = computed(() => {
  const f = overviewDateFrom.value;
  const t = overviewDateTo.value;
  if (!f && !t) return `近 ${overviewDays.value} 天按日聚合`;
  if (overviewIsHourly.value) {
    return f && t && f === t ? `${f} 按小时聚合` : `${f || "…"} ~ ${t || "…"} 按小时聚合`;
  }
  if (overviewIsMonthly.value) return `${f || "…"} ~ ${t || "…"} 按月聚合`;
  if (f && t && f === t) return `${f} 按日聚合`;
  return `${f || "…"} ~ ${t || "…"} 按日聚合`;
});

function overviewChartTheme() {
  const isDark =
    preferences.theme === "dark" ||
    (preferences.theme === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  return {
    textColor: isDark ? "#94a3b8" : "#64748b",
    gridLineColor: isDark ? "rgba(255, 255, 255, 0.06)" : "rgba(0, 0, 0, 0.06)",
    tooltipBg: isDark ? "rgba(15, 23, 42, 0.95)" : "rgba(255, 255, 255, 0.95)",
    tooltipBorder: isDark ? "rgba(255, 255, 255, 0.15)" : "rgba(0, 0, 0, 0.1)",
    tooltipText: isDark ? "#f8fafc" : "#0f172a",
    brand: isDark ? "#10b981" : "#059669",
  };
}

function overviewAxisCompact(v: number): string {
  if (v >= 1_000_000) return `${Number((v / 1_000_000).toFixed(1))}M`;
  if (v >= 10_000) return `${Number((v / 1000).toFixed(1))}k`;
  return String(v);
}

/** 趋势图数据点：日粒度标签 MM-DD，小时粒度标签 HH 时，字段与图表系列一一对应 */
interface OverviewChartPoint {
  label: string;
  successfulRequests: number;
  failedRequests: number;
  promptTokens: number;
  completionTokens: number;
  reasoningTokens: number;
  cacheHitTokens: number;
}

function toDailyChartPoints(points: GatewayDailyPoint[]): OverviewChartPoint[] {
  return points.map((p) => ({
    label: p.date.slice(5),
    successfulRequests: p.successfulRequests,
    failedRequests: p.failedRequests,
    promptTokens: p.promptTokens,
    completionTokens: p.completionTokens,
    reasoningTokens: p.reasoningTokens,
    cacheHitTokens: p.cacheHitTokens,
  }));
}

/** 小时粒度标签：单日只显「14时」；多天（≤3 天）带日期前缀「08-21 14时」 */
function toHourlyChartPoints(points: GatewayHourlyPoint[]): OverviewChartPoint[] {
  const dates = new Set(points.map((p) => p.date));
  const multiDay = dates.size > 1;
  return points.map((p) => ({
    label: multiDay
      ? `${p.date.slice(5)} ${String(p.hour).padStart(2, "0")}时`
      : `${String(p.hour).padStart(2, "0")}时`,
    successfulRequests: p.successfulRequests,
    failedRequests: p.failedRequests,
    promptTokens: p.promptTokens,
    completionTokens: p.completionTokens,
    reasoningTokens: p.reasoningTokens,
    cacheHitTokens: p.cacheHitTokens,
  }));
}

/** 月粒度标签：同一年用「8月」，跨年用「25-12」避免歧义 */
function toMonthlyChartPoints(points: GatewayDailyPoint[]): OverviewChartPoint[] {
  const years = new Set(points.map((p) => p.date.slice(0, 4)));
  const sameYear = years.size === 1;
  return points.map((p) => ({
    label: sameYear ? `${parseInt(p.date.slice(5, 7), 10)}月` : p.date.slice(2),
    successfulRequests: p.successfulRequests,
    failedRequests: p.failedRequests,
    promptTokens: p.promptTokens,
    completionTokens: p.completionTokens,
    reasoningTokens: p.reasoningTokens,
    cacheHitTokens: p.cacheHitTokens,
  }));
}

function buildRequestsChartOption(points: OverviewChartPoint[]): EChartsOption {
  const th = overviewChartTheme();
  return {
    tooltip: {
      trigger: "axis",
      axisPointer: { type: "shadow" },
      backgroundColor: th.tooltipBg,
      borderColor: th.tooltipBorder,
      textStyle: { color: th.tooltipText, fontSize: 12 },
    },
    legend: {
      data: ["成功", "失败"],
      textStyle: { color: th.textColor, fontSize: 11 },
      top: 0,
      right: 4,
      itemWidth: 10,
      itemHeight: 10,
    },
    grid: { left: 40, right: 8, top: 28, bottom: 22 },
    xAxis: {
      type: "category",
      data: points.map((p) => p.label),
      axisLine: { lineStyle: { color: th.gridLineColor } },
      axisLabel: { color: th.textColor, fontSize: 10 },
      axisTick: { show: false },
    },
    yAxis: {
      type: "value",
      minInterval: 1,
      axisLabel: { color: th.textColor, fontSize: 10, formatter: (v: number) => overviewAxisCompact(v) },
      splitLine: { lineStyle: { color: th.gridLineColor, type: "dashed" } },
    },
    series: [
      {
        name: "成功",
        type: "bar",
        stack: "requests",
        barMaxWidth: 18,
        data: points.map((p) => p.successfulRequests),
        itemStyle: { color: th.brand },
      },
      {
        name: "失败",
        type: "bar",
        stack: "requests",
        barMaxWidth: 18,
        data: points.map((p) => p.failedRequests),
        itemStyle: { color: "#f43f5e", borderRadius: [3, 3, 0, 0] },
      },
    ],
  };
}

function buildTokensChartOption(points: OverviewChartPoint[]): EChartsOption {
  const th = overviewChartTheme();
  const series = [
    { name: "净增输入", color: "#3b82f6", data: points.map((p) => Math.max(0, p.promptTokens - p.cacheHitTokens)) },
    { name: "缓存命中", color: "#f59e0b", data: points.map((p) => p.cacheHitTokens) },
    { name: "思考推理", color: "#8b5cf6", data: points.map((p) => p.reasoningTokens) },
    { name: "纯文本输出", color: th.brand, data: points.map((p) => Math.max(0, p.completionTokens - p.reasoningTokens)) },
  ];
  return {
    tooltip: {
      trigger: "axis",
      axisPointer: { type: "shadow" },
      backgroundColor: th.tooltipBg,
      borderColor: th.tooltipBorder,
      textStyle: { color: th.tooltipText, fontSize: 12 },
      valueFormatter: (v) => formatNumberUtil(Number(v ?? 0)),
    },
    legend: {
      data: series.map((s) => s.name),
      textStyle: { color: th.textColor, fontSize: 10.5 },
      top: 0,
      right: 4,
      itemWidth: 10,
      itemHeight: 10,
    },
    grid: { left: 46, right: 8, top: 28, bottom: 22 },
    xAxis: {
      type: "category",
      data: points.map((p) => p.label),
      axisLine: { lineStyle: { color: th.gridLineColor } },
      axisLabel: { color: th.textColor, fontSize: 10 },
      axisTick: { show: false },
    },
    yAxis: {
      type: "value",
      axisLabel: { color: th.textColor, fontSize: 10, formatter: (v: number) => overviewAxisCompact(v) },
      splitLine: { lineStyle: { color: th.gridLineColor, type: "dashed" } },
    },
    series: series.map((s, idx) => ({
      name: s.name,
      type: "bar" as const,
      stack: "tokens",
      barMaxWidth: 18,
      data: s.data,
      itemStyle: { color: s.color, borderRadius: idx === series.length - 1 ? [3, 3, 0, 0] : undefined },
    })),
  };
}

const overviewRequestsChart = computed(() =>
  buildRequestsChartOption(toDailyChartPoints(gatewayOverview.value?.daily ?? [])),
);
const overviewTokensChart = computed(() =>
  buildTokensChartOption(toDailyChartPoints(gatewayOverview.value?.daily ?? [])),
);
const overviewHourlyRequestsChart = computed(() =>
  buildRequestsChartOption(toHourlyChartPoints(overviewHourly.value ?? [])),
);
const overviewHourlyTokensChart = computed(() =>
  buildTokensChartOption(toHourlyChartPoints(overviewHourly.value ?? [])),
);
const overviewMonthlyRequestsChart = computed(() =>
  buildRequestsChartOption(toMonthlyChartPoints(overviewMonthly.value ?? [])),
);
const overviewMonthlyTokensChart = computed(() =>
  buildTokensChartOption(toMonthlyChartPoints(overviewMonthly.value ?? [])),
);

/** 趋势图粒度自动选择：单日 → 小时；>92 天 → 月；其余 → 日 */
const overviewGranularityLabel = computed(() =>
  overviewIsHourly.value ? "· 按小时" : overviewIsMonthly.value ? "· 按月" : "",
);
const overviewActiveRequestsChart = computed(() =>
  overviewIsHourly.value
    ? overviewHourlyRequestsChart.value
    : overviewIsMonthly.value
      ? overviewMonthlyRequestsChart.value
      : overviewRequestsChart.value,
);
const overviewActiveTokensChart = computed(() =>
  overviewIsHourly.value
    ? overviewHourlyTokensChart.value
    : overviewIsMonthly.value
      ? overviewMonthlyTokensChart.value
      : overviewTokensChart.value,
);

export interface ChannelModelGroup {
  channel: ChannelConfig;
  models: string[];
  /** 该渠道已知模型总数（未经白名单过滤），供「白名单 N/M」角标展示 */
  totalKnown: number;
}

const gatewayGroupedModels = computed<ChannelModelGroup[]>(() => {
  const q = gatewaySearchQuery.value.trim().toLowerCase();
  return proxyConfig.value.channels.map((channel) => {
    // 该渠道对外可见的模型：按渠道拉取的模型再经白名单勾选结果过滤
    const known = modelsForChannel(channel.id);
    let models = filterChannelModels(channel, known);
    const alias = channelAlias(channel);
    if (q) {
      models = models.filter(
        (m) => m.toLowerCase().includes(q) || `${alias}/${m}`.toLowerCase().includes(q)
      );
    }
    return {
      channel,
      models,
      totalKnown: known.length,
    };
  });
});

const totalGatewayModelsCount = computed(() => {
  return gatewayGroupedModels.value.reduce((acc, g) => acc + g.models.length, 0);
});

/** 渠道卡片上对外可见（白名单内）的模型数量 */
function channelEnabledModelsCount(channel: ChannelConfig): number {
  return filterChannelModels(channel, modelsForChannel(channel.id)).length;
}

/** 顶栏「可用模型」徽标数量：优先取后端已按白名单过滤的计数，其次按前端白名单过滤结果兜底 */
const availableModelsCount = computed(() => {
  if (proxyStatus.value.modelsCount > 0) return proxyStatus.value.modelsCount;
  return proxyConfig.value.channels.reduce((acc, c) => acc + channelEnabledModelsCount(c), 0);
});

/** 模型列表排序：discovery = 上游顺序；usage = 按累计调用量降序 */
const channelModelSortMode = ref<"discovery" | "usage">("discovery");
/** 仅看已启用的模型 */
const channelModelEnabledOnly = ref(false);

const filteredChannelModels = computed(() => {
  const q = channelSearchQuery.value.trim().toLowerCase();
  const alias = channelAlias(selectedChannel.value);
  let list = selectedChannelModels();
  if (q) list = list.filter((m) => m.toLowerCase().includes(q) || `${alias}/${m}`.toLowerCase().includes(q));
  if (channelModelEnabledOnly.value) {
    list = list.filter((m) => isModelChecked(m));
  }
  if (channelModelSortMode.value === "usage") {
    const stats = channelModelStatsMap.value;
    return [...list].sort(
      (a, b) => (stats.get(b.toLowerCase())?.totalRequests ?? 0) - (stats.get(a.toLowerCase())?.totalRequests ?? 0),
    );
  }
  return list;
});

/** 当前页数据：筛选/搜索/排序已由后端 SQL 处理，前端仅透传展示 */
const filteredLogs = computed<ProxyRequestLog[]>(() => proxyLogs.value);

/** 排序指示符：未排序 ↕ / 升序 ▲ / 降序 ▼ */
function logSortIndicator(by: "timestamp" | "status" | "tokens" | "duration") {
  if (logSortBy.value !== by) return "↕";
  return logSortOrder.value === "asc" ? "▲" : "▼";
}

/** 点击列头排序：沿用当前筛选与关键词，回到第一页由后端重新查询 */
function sortLogsBy(by: "timestamp" | "status" | "tokens" | "duration") {
  toggleLogSort(by, { filter: logStatusFilter.value, q: logSearchQuery.value.trim() });
}

/** 顶部标签计数：后端按所选日期区间统计，不随状态筛选/搜索变化 */
const logCounts = computed(() => ({
  all: logGlobalTotal.value,
  success: logGlobalSuccess.value,
  error: logGlobalError.value,
}));

/** 切换状态筛选：回到第一页并按新条件重新拉取 */
function switchLogFilter(filter: "all" | "success" | "error") {
  logStatusFilter.value = filter;
  goLogPage(1, { filter, q: logSearchQuery.value.trim() });
}

/** 时间范围筛选变更后：回到第一页并按新日期重新拉取 */
async function applyLogRange() {
  await goLogPage(1, { filter: logStatusFilter.value, q: logSearchQuery.value.trim() });
}

/** 控制台总览时间范围变更后：按新日期区间重拉 KPI 与趋势图 */
async function applyOverviewRange() {
  await refreshGatewayOverview();
}

/** 搜索防抖：停笔 350ms 后回到第一页重查 */
let searchTimer: number | undefined;
watch(logSearchQuery, (val) => {
  window.clearTimeout(searchTimer);
  searchTimer = window.setTimeout(() => {
    goLogPage(1, { filter: logStatusFilter.value, q: val.trim() });
  }, 350);
});

/** 分页按钮序列：总页数 ≤7 全显，否则首尾 + 当前附近 + 省略号 */
const logPageNumbers = computed<Array<number | "…">>(() => {
  const total = logPageCount.value;
  const current = logPage.value;
  if (total <= 7) return Array.from({ length: total }, (_, index) => index + 1);
  if (current <= 4) return [1, 2, 3, 4, 5, "…", total];
  if (current >= total - 3) return [1, "…", total - 4, total - 3, total - 2, total - 1, total];
  return [1, "…", current - 1, current, current + 1, "…", total];
});

function formatNumber(num: number | undefined | null): string {
  return formatNumberUtil(num);
}

function formatUptime(seconds: number) {
  return formatUptimeUtil(seconds);
}

async function copyModel(modelId: string, channel: ChannelConfig) {
  const fullId = `${channelAlias(channel)}/${modelId}`;
  try {
    await navigator.clipboard.writeText(fullId);
    copiedModelId.value = modelId;
    showToast(`已复制模型 ID: ${fullId}`);
    setTimeout(() => {
      if (copiedModelId.value === modelId) {
        copiedModelId.value = null;
      }
    }, 1500);
  } catch {
    showToast("复制失败", true);
  }
}
</script>

<template>
  <div class="mp-page" role="main">
    <!-- 顶栏标题与快捷控制 -->
    <header class="mp-header">
      <div class="mp-header-left">
        <div class="mp-brand-section">
          <div class="mp-eyebrow-row">
            <span class="mp-live-dot" :class="{ 'is-off': !proxyStatus.running }" />
            <span class="mp-eyebrow-text">本地模型反向代理网关</span>
          </div>
          <div class="mp-header-title-row">
            <h1>模型反代</h1>
            <span
              class="mp-status-pill"
              :class="{ active: proxyStatus.running }"
            >
              <span class="mp-status-dot" />
              <span>{{ proxyStatus.running ? "运行中" : "已停止" }}</span>
            </span>
            <span class="mp-channel-pill">多渠道架构</span>
          </div>
          <p class="mp-subtitle">
            对外提供标准兼容的 OpenAI 和 Anthropic API · 共享服务端口 <strong>{{ proxyStatus.port || "启动后确定" }}</strong>
          </p>
        </div>
      </div>

      <div class="mp-header-actions">
        <!-- 服务配置弹窗触发按钮 -->
        <button
          type="button"
          class="mp-btn mp-btn-ghost"
          title="管理模型接口 API Key 与网关选项"
          @click="configModalOpen = true"
        >
          <span v-html="icons.settings" />
          <span>服务配置</span>
        </button>

        <!-- 网关可用模型总览弹窗触发按钮 -->
        <button
          type="button"
          class="mp-btn mp-btn-ghost"
          :disabled="fetchingModels"
          title="查看网关聚合的可用模型目录与快速复制"
          @click="handleOpenGatewayModelsModal"
        >
          <span v-html="icons.cpu" />
          <span>{{ fetchingModels ? "加载中…" : "可用模型" }}</span>
          <span v-if="availableModelsCount > 0" class="mp-btn-badge">
            {{ availableModelsCount }}
          </span>
        </button>

        <!-- 启动/停止服务按钮 -->
        <button
          type="button"
          class="mp-btn"
          :class="proxyStatus.running ? 'mp-btn-danger' : 'mp-btn-primary'"
          :disabled="togglingServer"
          @click="toggleServer"
        >
          <span v-html="proxyStatus.running ? icons.pause : icons.play" />
          <span>{{ togglingServer ? "操作中…" : (proxyStatus.running ? "停止服务" : "启动服务") }}</span>
        </button>
      </div>
    </header>

    <!-- 一级页面标签导航切换 (Tab Switcher) -->
    <div class="mp-main-tab-nav">
      <button
        type="button"
        class="mp-main-nav-btn"
        :class="{ active: currentMainTab === 'console' }"
        @click="currentMainTab = 'console'"
      >
        <span v-html="icons.activity" />
        <span>反代控制台</span>
      </button>

      <button
        type="button"
        class="mp-main-nav-btn"
        :class="{ active: currentMainTab === 'channels' }"
        @click="currentMainTab = 'channels'"
      >
        <span v-html="icons.shield" />
        <span>反代渠道</span>
        <span class="mp-tab-count-pill font-mono">
          {{ proxyConfig.channels.length }}
        </span>
      </button>

      <button
        type="button"
        class="mp-main-nav-btn"
        :class="{ active: currentMainTab === 'logs' }"
        @click="switchToLogsTab"
      >
        <span v-html="icons.rows" />
        <span>请求调用日志</span>
        <span v-if="logCounts.all > 0" class="mp-tab-count-pill font-mono">
          {{ logCounts.all }}
        </span>
        <span v-if="logCounts.error > 0" class="mp-tab-count-pill is-err font-mono" title="有异常/失败请求">
          {{ logCounts.error }} 异常
        </span>
      </button>
    </div>

    <!-- 选项卡 1: 反代控制台 (Gateway Console) -->
    <div v-if="currentMainTab === 'console'" class="mp-tab-pane mp-console-hub">
      <!-- 1. 网关端点与访问密钥 (Connection Matrix) -->
      <section class="mp-card mp-endpoints-card">
        <div class="mp-card-header">
          <div class="mp-card-title-group">
            <span class="mp-card-icon" v-html="icons.activity" />
            <h2>网关连接端点与鉴权</h2>
          </div>
          <div class="mp-endpoints-summary-chips">
            <span class="mp-ep-chip font-mono">
              <span class="text-muted">端口</span>
              <strong class="text-brand">{{ proxyStatus.port || "启动后确定" }}</strong>
            </span>
            <span class="mp-ep-chip font-mono">
              <span class="text-muted">运行</span>
              <strong>{{ proxyStatus.running ? formatUptime(proxyStatus.uptimeSeconds) : '已停止' }}</strong>
            </span>
            <span class="mp-ep-chip font-mono">
              <span class="text-muted">渠道</span>
              <strong>{{ proxyConfig.channels.length }} 个</strong>
            </span>
            <span class="mp-ep-chip font-mono">
              <span class="text-muted">模型</span>
              <strong>{{ availableModelsCount }} 个就绪</strong>
            </span>
          </div>
        </div>

        <div class="mp-endpoint-rows">
          <!-- 合并行：OpenAI Base URL（客户端最常填）与 API Key 并排 -->
          <div class="mp-endpoint-row mp-epr-merged">
            <div class="mp-epr-half is-gw">
              <div class="mp-epr-key-head">
                <div class="mp-epr-label">
                  <span class="mp-proto-badge is-key">Gateway</span>
                  <span>Base URL</span>
                </div>
                <span
                  class="mp-epr-key-state is-on"
                  title="同一端口支持 OpenAI / Responses / Claude / Gemini 四种协议请求"
                >
                  四协议入口
                </span>
              </div>
              <div class="mp-epr-key-line">
                <code class="mp-epr-code font-mono" :title="openAiBaseUrl">{{ openAiBaseUrl }}</code>
                <button
                  type="button"
                  class="mp-action-btn mp-btn-icon-only"
                  title="复制网关 Base URL"
                  @click="copyProxyUrl"
                >
                  <span v-html="icons.copy" />
                </button>
              </div>
            </div>

            <div class="mp-epr-half is-key">
              <div class="mp-epr-key-head">
                <div class="mp-epr-label">
                  <span class="mp-proto-badge is-key">API Key</span>
                  <span>访问密钥</span>
                </div>
                <span class="mp-epr-key-state" :class="proxyConfig.apiKey ? 'is-on' : 'is-off'">
                  {{ proxyConfig.apiKey ? '模型接口 API Key 已配置' : '等待生成模型接口 API Key' }}
                </span>
              </div>
              <div class="mp-epr-key-line">
                <code
                  class="mp-epr-code font-mono"
                  :title="proxyConfig.apiKey || '等待服务生成 API Key'"
                >
                  {{ showKey ? (proxyConfig.apiKey || '(等待服务生成 API Key)') : (proxyConfig.apiKey ? '••••••••••••••••••••' : '(等待服务生成 API Key)') }}
                </code>
                <div class="mp-epr-btns">
                  <button
                    v-if="proxyConfig.apiKey"
                    type="button"
                    class="mp-action-btn mp-btn-icon-only"
                    :title="showKey ? '隐藏密钥' : '显示密钥'"
                    @click="showKey = !showKey"
                  >
                    <span v-html="showKey ? icons.eyeOff : icons.eye" />
                  </button>
                  <button
                    type="button"
                    class="mp-action-btn"
                    title="复制 API Key"
                    @click="copyProxyKey"
                  >
                    <span v-html="icons.copy" />
                    <span>复制 Key</span>
                  </button>
                  <button
                    v-if="proxyConfig.apiKey"
                    type="button"
                    class="mp-action-btn"
                    title="复制标准 Authorization Header"
                    @click="copyAuthHeader"
                  >
                    <span>复制 Header</span>
                  </button>
                </div>
              </div>
            </div>
          </div>

          <!-- 四个对话端点：2×2 紧凑网格 -->
          <div class="mp-epr-grid">
            <!-- OpenAI Chat Completions（完整请求地址） -->
            <div class="mp-endpoint-row mp-epr-cell">
              <div class="mp-epr-label">
                <span class="mp-proto-badge is-openai">Chat</span>
                <span>Completions URL</span>
              </div>
              <code class="mp-epr-code font-mono" :title="chatCompletionsUrl">{{ stripOrigin(chatCompletionsUrl) }}</code>
              <button
                type="button"
                class="mp-action-btn mp-btn-icon-only"
                title="复制 OpenAI Chat Completions 完整请求地址"
                @click="copyChatCompletionsUrl"
              >
                <span v-html="icons.copy" />
              </button>
            </div>

            <!-- OpenAI Responses (Codex 等) -->
            <div class="mp-endpoint-row mp-epr-cell">
              <div class="mp-epr-label">
                <span class="mp-proto-badge is-openai">Responses</span>
                <span>API URL</span>
              </div>
              <code class="mp-epr-code font-mono" :title="responsesApiUrl">{{ stripOrigin(responsesApiUrl) }}</code>
              <button
                type="button"
                class="mp-action-btn mp-btn-icon-only"
                title="复制 OpenAI Responses API 端点 URL（Codex 等使用）"
                @click="copyResponsesUrl()"
              >
                <span v-html="icons.copy" />
              </button>
            </div>

            <!-- Claude Messages -->
            <div class="mp-endpoint-row mp-epr-cell">
              <div class="mp-epr-label">
                <span class="mp-proto-badge is-claude">Claude</span>
                <span>Messages URL</span>
              </div>
              <code class="mp-epr-code font-mono" :title="claudeMessagesUrl">{{ stripOrigin(claudeMessagesUrl) }}</code>
              <button
                type="button"
                class="mp-action-btn mp-btn-icon-only"
                title="复制 Claude Messages URL"
                @click="copyClaudeUrl"
              >
                <span v-html="icons.copy" />
              </button>
            </div>

            <!-- Gemini (兼容入口 v1 / 原生 SDK v1beta) -->
            <div class="mp-endpoint-row mp-epr-cell">
              <div class="mp-epr-label">
                <span class="mp-proto-badge is-gemini">Gemini</span>
                <span>Base URL</span>
              </div>
              <div class="mp-gemini-dual">
                <div class="mp-gemini-item">
                  <code class="mp-epr-code font-mono" :title="geminiV1Url">{{ stripOrigin(geminiV1Url) }}</code>
                  <button
                    type="button"
                    class="mp-action-btn mp-btn-icon-only"
                    title="复制 Gemini 兼容入口 Base URL (/v1/gemini)"
                    @click="copyGeminiUrl"
                  >
                    <span v-html="icons.copy" />
                  </button>
                  <span class="mp-gemini-tag">兼容入口</span>
                </div>
                <div class="mp-gemini-item">
                  <code class="mp-epr-code font-mono" :title="geminiV1BetaUrl">{{ stripOrigin(geminiV1BetaUrl) }}</code>
                  <button
                    type="button"
                    class="mp-action-btn mp-btn-icon-only"
                    title="复制 Google Gemini 原生 SDK Base URL (/v1beta)"
                    @click="copyGeminiV1BetaUrl"
                  >
                    <span v-html="icons.copy" />
                  </button>
                  <span class="mp-gemini-tag">原生 SDK</span>
                </div>
              </div>
            </div>
          </div>
        </div>
      </section>

      <!-- 2. 全渠道数据总览（KPI 四宫格 + 时间范围切换，卡片内所有数字按所选范围统计） -->
      <section class="mp-card mp-overview-card">
        <div class="mp-card-header">
          <div class="mp-card-title-group">
            <span class="mp-card-icon">📊</span>
            <h2>全渠道数据总览</h2>
            <span
              class="mp-overview-scope font-mono"
              :title="`当前统计范围：${overviewScopeLabel}，下方 KPI 与趋势图均按此范围统计`"
            >{{ overviewScopeLabel }}</span>
          </div>
          <DateRangeDropdown
            v-model:from="overviewDateFrom"
            v-model:to="overviewDateTo"
            @apply="applyOverviewRange"
          />
        </div>

        <section class="mp-kpi-matrix-grid">
          <!-- KPI 1: 请求量与成功率（按所选范围） -->
          <div class="mp-kpi-card is-traffic">
            <div class="mp-kpi-top">
              <span class="mp-kpi-label">请求量 / 成功率</span>
              <span class="mp-kpi-badge" :class="liveSuccessRate.startsWith('100') || parseFloat(liveSuccessRate) >= 95 ? 'is-good' : 'is-warn'">
                {{ liveSuccessRate }} 成功率
              </span>
            </div>
            <div class="mp-kpi-main">
              <strong class="mp-kpi-number font-mono">{{ formatNumber(statTotals.totalRequests) }}</strong>
              <span class="mp-kpi-unit">次调用</span>
            </div>
            <div class="mp-kpi-footer font-mono">
              <span class="text-success font-semibold">✓ {{ formatNumber(statTotals.successfulRequests) }} 成功</span>
              <span class="mp-kpi-sep">·</span>
              <span :class="statTotals.failedRequests > 0 ? 'text-danger font-semibold' : 'text-muted'">✗ {{ formatNumber(statTotals.failedRequests) }} 异常</span>
              <!-- 区间即今日时主数字已含今日，避免重复；其余范围附今日快照（与区间解耦） -->
              <template v-if="!overviewIsToday">
                <span class="mp-kpi-sep">·</span>
                <span class="text-brand">今日 {{ formatNumber(todayStats?.totalRequests ?? 0) }} 次</span>
              </template>
            </div>
          </div>

          <!-- KPI 2: 平均响应耗时 / 首字 TTFT（所选范围全渠道均值） -->
          <div class="mp-kpi-card is-latency" title="所选时间范围内已完结请求的端到端平均耗时；TTFT 为流式请求首个 Token 时延均值">
            <div class="mp-kpi-top">
              <span class="mp-kpi-label">平均响应耗时 / TTFT</span>
              <span class="mp-kpi-badge is-good">全渠道均值</span>
            </div>
            <div class="mp-kpi-main">
              <strong class="mp-kpi-number font-mono">{{ formatSec(statTotals.avgDurationMs) }}</strong>
              <span class="mp-kpi-unit">端到端</span>
            </div>
            <div class="mp-kpi-footer font-mono">
              <span class="text-brand">⚡ 首字 TTFT {{ formatSec(statTotals.avgTtftMs) }}</span>
              <span class="mp-kpi-sep">·</span>
              <span class="text-muted">样本 {{ formatNumber(statTotals.totalRequests) }} 次</span>
            </div>
          </div>

          <!-- KPI 3: Token 消耗总量 (BMK 格式化，按所选范围) -->
          <div class="mp-kpi-card is-tokens" :title="`当前范围精确计费: ${formatNumber(statTotals.totalTokens)} Tokens`">
            <div class="mp-kpi-top">
              <span class="mp-kpi-label">Token 消耗总量</span>
              <span class="mp-kpi-badge is-brand">聚合计费</span>
            </div>
            <div class="mp-kpi-main">
              <strong class="mp-kpi-number font-mono text-brand">{{ formatTokenBMK(statTotals.totalTokens) }}</strong>
              <span class="mp-kpi-unit">Tokens</span>
            </div>
            <div class="mp-kpi-footer font-mono">
              <span class="text-muted">精确计费 {{ formatNumber(statTotals.totalTokens) }}</span>
              <template v-if="!overviewIsToday">
                <span class="mp-kpi-sep">·</span>
                <span class="text-brand">今日 {{ formatTokenBMK(todayStats?.totalTokens ?? 0) }}</span>
              </template>
            </div>
          </div>

          <!-- KPI 4: 前缀缓存与算力节省 (BMK 格式化，按所选范围) -->
          <div class="mp-kpi-card is-cache" :title="`当前范围缓存命中: ${formatNumber(statTotals.cacheHitTokens)} Tokens`">
            <div class="mp-kpi-top">
              <span class="mp-kpi-label">KV Cache 缓存复用</span>
              <span class="mp-kpi-badge is-hit">命中率 {{ liveCacheHitRate }}</span>
            </div>
            <div class="mp-kpi-main">
              <strong class="mp-kpi-number font-mono" style="color: #f59e0b;">{{ formatTokenBMK(statTotals.cacheHitTokens) }}</strong>
              <span class="mp-kpi-unit">Tokens</span>
            </div>
            <div class="mp-kpi-footer font-mono">
              <span style="color: #f59e0b;" class="font-semibold">⚡ 节省重复上下文计算</span>
            </div>
          </div>
        </section>
      </section>

      <!-- 4. 趋势图表（持久化统计 · 跨渠道汇总 · 单日区间自动切换小时粒度） -->
      <section class="mp-card mp-trend-card">
        <div class="mp-card-header">
          <div class="mp-card-title-group">
            <span class="mp-card-icon">📈</span>
            <h2>请求与 Token 趋势</h2>
          </div>
          <div class="mp-trend-badges">
            <span class="mp-deck-badge font-mono" title="数据来自持久化统计表，重启不丢失、不受日志裁剪影响">
              <span class="text-muted">{{ overviewIsAll ? "总消耗" : "区间消耗" }}</span>
              <strong class="text-brand">{{ formatTokenBMK(statTotals.totalTokens) }}</strong>
              <span class="text-muted text-xs">Tokens</span>
              <span class="text-muted">· {{ overviewRangeText }}</span>
            </span>
            <span
              v-if="proxyStatus.totalRequests > 0"
              class="mp-deck-badge font-mono"
              title="本次进程运行以来的实时计数，与所选时间范围无关"
            >
              <span class="text-muted">本次运行思维触发</span>
              <strong>{{ proxyStatus.totalReasoningRequests ?? 0 }}/{{ proxyStatus.totalRequests }}</strong>
            </span>
          </div>
        </div>
        <div v-if="overviewHasData || overviewIsHourly" class="mp-trend-grid">
          <div class="mp-trend-box">
            <div class="mp-trend-title">请求量（成功 / 失败）{{ overviewGranularityLabel }}</div>
            <EChart :option="overviewActiveRequestsChart" height="150px" />
          </div>
          <div class="mp-trend-box">
            <div class="mp-trend-title">Token 构成消耗{{ overviewGranularityLabel }}</div>
            <EChart :option="overviewActiveTokensChart" height="150px" />
          </div>
        </div>
        <div v-else class="mp-trend-empty">
          所选时间范围内暂无请求数据 · 可切换更长时间范围，或通过网关发起请求后查看全渠道请求量与 Token 消耗趋势
        </div>
      </section>
    </div>

    <!-- 选项卡 2: 反代上游渠道 (Channels Matrix) -->
    <div v-else-if="currentMainTab === 'channels'" class="mp-tab-pane">
      <div class="mp-section-head">
        <div class="mp-card-title-group">
          <span class="mp-card-icon" v-html="icons.shield" />
          <h2>反代上游渠道 ({{ proxyConfig.channels.length }})</h2>
        </div>
        <div class="mp-section-actions">
          <small class="text-muted">独立管理各个上游反代通道与内部代理池轮询</small>
          <button
            type="button"
            class="mp-btn mp-btn-ghost mp-btn-sm"
            title="从站点库「在用且存活」的站点创建反代渠道"
            @click="openSiteConvertDialog"
          >
            <span v-html="icons.globe" />
            <span>站点转换</span>
          </button>
        </div>
      </div>

      <div class="mp-channels-grid">
        <!-- 渠道卡片 -->
        <div
          v-for="channel in proxyConfig.channels"
          :key="channel.id"
          class="mp-channel-card"
          :class="{ 'is-disabled': !channel.enabled }"
        >
          <div class="mp-channel-card-head">
            <div class="mp-channel-card-title">
              <div class="mp-channel-badge-icon">
                <span v-html="icons.cpu" />
              </div>
              <div>
                <h3>{{ channel.name }}<span class="mp-title-alias">（{{ channelAlias(channel) }}）</span></h3>
                <span class="mp-card-tags">
                  <span
                    v-if="isBuiltinChannel(channel)"
                    class="mp-alias-tag is-builtin"
                    title="内置固化渠道：官方维护，别名固定不可修改"
                  >固化渠道</span>
                  <span
                    v-if="channel.siteId"
                    class="mp-alias-tag is-site"
                    title="与站点库原纪录关联，使用该站点同步的原 Key"
                  >站点关联</span>
                  <span
                    v-if="channel.siteId"
                    class="mp-alias-tag"
                    :title="
                      channelInheritedKeyCount(channel) > 1
                        ? `从站点缓存继承 ${channelInheritedKeyCount(channel)} 个 Key，请求时自动轮换`
                        : channelInheritedKeyCount(channel) === 1
                          ? '从站点缓存继承 1 个 Key'
                          : '站点缓存中暂无可用 Key，请先在站点库同步该站点的 Key'
                    "
                  >Key ×{{ channelInheritedKeyCount(channel) }}</span>
                  <span
                    v-else-if="channelKeyCount(channel) > 0"
                    class="mp-alias-tag"
                    :title="
                      channelKeyCount(channel) > 1
                        ? `配置了 ${channelKeyCount(channel)} 个 Key，请求时自动轮换`
                        : '已配置 1 个 Key'
                    "
                  >Key ×{{ channelKeyCount(channel) }}</span>
                  <span
                    v-else-if="isBuiltinChannel(channel)"
                    class="mp-alias-tag"
                    title="未配置 Key：以匿名模式访问 OpenCode 免费模型"
                  >免 Key</span>
                </span>
              </div>
            </div>

            <label class="mp-switch-wrap" :title="channel.enabled ? '点击禁用该渠道' : '点击启用该渠道'">
              <input
                v-model="channel.enabled"
                type="checkbox"
                @change="handleChannelSave(channel)"
              />
              <span class="mp-switch-round" />
            </label>
          </div>

          <!-- 渠道统计摘要：累计与今日双层对照 -->
          <div class="mp-channel-summary" aria-label="渠道使用统计">
            <div class="mp-channel-summary-row is-total">
              <span class="mp-channel-summary-label">累计</span>
              <div class="mp-channel-summary-metrics">
                <span class="mp-channel-summary-metric" :title="`累计请求 ${channelStatsFor(channel).totalRequests} 次`">
                  <small>请求</small>
                  <strong class="font-mono">{{ formatNumber(channelStatsFor(channel).totalRequests) }}</strong>
                </span>
                <span
                  class="mp-channel-summary-metric"
                  :class="{ 'is-bad': channelSuccessRateBad(channel) }"
                  :title="`累计成功率 ${channelSuccessRate(channel)}`"
                >
                  <small>成功率</small>
                  <strong>{{ channelSuccessRate(channel) }}</strong>
                </span>
                <span class="mp-channel-summary-metric" :title="`累计 Token ${formatNumber(channelStatsFor(channel).totalTokens)}`">
                  <small>Token</small>
                  <strong class="font-mono text-brand">{{ formatCompactToken(channelStatsFor(channel).totalTokens) }}</strong>
                </span>
              </div>
            </div>
            <div class="mp-channel-summary-row is-today">
              <span class="mp-channel-summary-label">今日</span>
              <div class="mp-channel-summary-metrics">
                <span class="mp-channel-summary-metric" :title="`今日请求 ${channelStatsFor(channel).todayRequests ?? 0} 次`">
                  <small>请求</small>
                  <strong class="font-mono">{{ formatNumber(channelStatsFor(channel).todayRequests ?? 0) }}</strong>
                </span>
                <span
                  class="mp-channel-summary-metric"
                  :class="{ 'is-bad': (channelStatsFor(channel).todayRequests ?? 0) > 0 && (channelStatsFor(channel).todaySuccessfulRequests ?? 0) / (channelStatsFor(channel).todayRequests ?? 1) < 0.9 }"
                  :title="`今日成功率 ${channelTodaySuccessRate(channel)}`"
                >
                  <small>成功率</small>
                  <strong>{{ channelTodaySuccessRate(channel) }}</strong>
                </span>
                <span class="mp-channel-summary-metric" :title="`今日 Token ${formatNumber(channelStatsFor(channel).todayTotalTokens ?? 0)}`">
                  <small>Token</small>
                  <strong class="font-mono text-brand">{{ formatCompactToken(channelStatsFor(channel).todayTotalTokens ?? 0) }}</strong>
                </span>
              </div>
            </div>
          </div>

          <div class="mp-channel-card-footer">
            <div class="mp-channel-actions">
              <button
                type="button"
                class="mp-action-btn"
                :class="{ 'is-active': channel.enabledModels != null }"
                title="勾选此渠道对外暴露的模型，选中的体现在可用模型列表"
                @click="handleOpenChannelModelsModal(channel)"
              >
                <span v-html="icons.edit" />
                <span>管理模型</span>
              </button>
              <button
                type="button"
                class="mp-action-btn"
                title="打开渠道设置：内部代理池轮询开关"
                @click="handleOpenChannelSettingsDialog(channel)"
              >
                <span v-html="icons.settings" />
                <span>设置</span>
              </button>
              <button
                v-if="channel.siteId || channel.id !== 'opencode'"
                type="button"
                class="mp-action-btn is-danger"
                title="删除此反代渠道"
                @click="handleOpenDeleteChannelModal(channel)"
              >
                <span v-html="icons.trash" />
                <span>删除</span>
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>

    <!-- 选项卡 3: 请求调用日志全景 (In-page Full View) -->
    <div v-else-if="currentMainTab === 'logs'" class="mp-tab-pane mp-logs-page-view">

      <!-- 请求日志主卡片 -->
      <div class="mp-card mp-logs-main-card">
        <!-- 筛选与操作工具栏 -->
        <div class="mp-logs-toolbar">
          <div class="mp-log-filter-tabs">
            <button
              type="button"
              class="mp-log-tab-btn"
              :class="{ active: logStatusFilter === 'all' }"
              @click="switchLogFilter('all')"
            >
              全部 ({{ logCounts.all }})
            </button>
            <button
              type="button"
              class="mp-log-tab-btn"
              :class="{ active: logStatusFilter === 'success' }"
              @click="switchLogFilter('success')"
            >
              成功 ({{ logCounts.success }})
            </button>
            <button
              type="button"
              class="mp-log-tab-btn"
              :class="{ active: logStatusFilter === 'error' }"
              @click="switchLogFilter('error')"
            >
              异常/失败 ({{ logCounts.error }})
            </button>
          </div>

          <div class="mp-logs-range">
            <DateRangeDropdown
              v-model:from="logDateFrom"
              v-model:to="logDateTo"
              @apply="applyLogRange"
            />
          </div>

          <div class="mp-search-box flex-1">
            <span class="mp-search-icon" v-html="icons.search" />
            <input
              v-model="logSearchQuery"
              type="search"
              placeholder="搜索模型名称、请求路径、HTTP状态码或错误提示…"
              class="mp-search-input-lg"
            />
            <button
              v-if="logSearchQuery"
              type="button"
              class="mp-search-clear-btn"
              title="清空搜索"
              @click="logSearchQuery = ''"
            >
              <span v-html="icons.close" />
            </button>
          </div>

          <div class="mp-logs-actions">
            <button
              type="button"
              class="mp-btn mp-btn-ghost mp-btn-sm"
              :disabled="loadingLogs"
              title="从本地数据库刷新最新日志"
              @click="fetchLogs({ filter: logStatusFilter, q: logSearchQuery.trim() })"
            >
              <span :class="{ 'mp-spin': loadingLogs }" v-html="icons.restore" />
              <span>{{ loadingLogs ? "刷新中…" : "刷新日志" }}</span>
            </button>
            <button
              type="button"
              class="mp-btn mp-btn-ghost mp-btn-sm text-danger"
              :disabled="proxyLogs.length === 0"
              title="清理本地 SQLite 数据库中的反代请求日志"
              @click="clearLogsModalOpen = true"
            >
              <span v-html="icons.trash" />
              <span>清空数据库日志</span>
            </button>
          </div>
        </div>

        <!-- 请求日志表格 -->
        <div class="mp-logs-table-wrap" :class="{ 'is-loading': loadingLogs }">
          <!-- loading 遮罩 -->
          <div v-if="loadingLogs" class="mp-table-loading-overlay">
            <span class="mp-spin" v-html="icons.restore" />
            <span>加载中…</span>
          </div>
          <table class="mp-logs-table">
            <thead>
              <tr>
                <th style="width: 105px;" class="mp-th-sortable" title="点击切换：升序 / 降序 / 默认排序" :class="{ 'is-sorted': logSortBy === 'timestamp' }" @click="sortLogsBy('timestamp')">请求时间<span class="mp-sort-arrow">{{ logSortIndicator('timestamp') }}</span></th>
                <th style="width: 230px;">入网 -> 出网</th>
                <th>渠道 / 模型</th>
                <th style="width: 125px;">出网节点</th>
                <th style="width: 82px;">模式 / 速率</th>
                <th style="width: 70px;" class="mp-th-sortable" title="点击切换：升序 / 降序 / 默认排序" :class="{ 'is-sorted': logSortBy === 'status' }" @click="sortLogsBy('status')">状态<span class="mp-sort-arrow">{{ logSortIndicator('status') }}</span></th>
                <th style="width: 175px;" class="mp-th-sortable" title="点击切换：升序 / 降序 / 默认排序" :class="{ 'is-sorted': logSortBy === 'tokens' }" @click="sortLogsBy('tokens')">Token 分布<span class="mp-sort-arrow">{{ logSortIndicator('tokens') }}</span></th>
                <th style="width: 90px;" class="mp-th-sortable" title="点击切换：升序 / 降序 / 默认排序" :class="{ 'is-sorted': logSortBy === 'duration' }" @click="sortLogsBy('duration')">耗时<span class="mp-sort-arrow">{{ logSortIndicator('duration') }}</span></th>
                <th style="width: 65px; text-align: center;">操作</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="log in filteredLogs"
                :key="log.id"
                class="mp-log-row"
                :class="{ 'has-error': log.statusCode >= 400 }"
                @click="openLogDetail(log)"
              >
                <td>
                  <div class="mp-log-time-col font-mono">
                    <span class="mp-log-date">{{ formatLogDate(log.timestamp) }}</span>
                    <strong class="mp-log-time">{{ formatLogTime(log.timestamp) }}</strong>
                  </div>
                </td>
                <td>
                  <div class="mp-log-method-path">
                    <div class="mp-path-row">
                      <span class="mp-path-label">入</span>
                      <code class="mp-path-code" :title="`入口路径：${log.path}`">{{ log.path }}</code>
                    </div>
                    <div v-if="upstreamPathDiffers(log.path, log.upstreamUrl)" class="mp-path-row">
                      <span class="mp-path-label">出</span>
                      <code class="mp-upstream-code" :title="`出网地址：${log.upstreamUrl}`">{{ formatUpstreamUrl(log.upstreamUrl) }}</code>
                    </div>
                  </div>
                </td>
                <td>
                  <div class="mp-log-model-col">
                    <div class="mp-log-chan-row">
                      <span class="mp-proto-tag">{{ log.channelId.toUpperCase() }}</span>
                    </div>
                    <strong class="mp-log-model-name font-mono" :title="log.model">{{ log.model }}</strong>
                  </div>
                </td>
                <td>
                  <div class="mp-log-node-wrap">
                    <span
                      class="mp-node-pill font-mono"
                      :class="log.nodeName && log.nodeName !== '直连通道' ? 'is-proxy' : 'is-direct'"
                      :title="log.nodeName ? `出网网络节点：${log.nodeName}` : '出网网络：直连通道'"
                    >
                      <span class="mp-node-dot" />
                      <span class="truncate">{{ log.nodeName || '直连通道' }}</span>
                    </span>
                  </div>
                </td>
                <td>
                  <div class="mp-mode-cell">
                    <span class="mp-stream-tag" :class="{ 'is-stream': log.stream }">
                      {{ log.stream ? "流式" : "非流式" }}
                    </span>
                    <span class="mp-mode-tps font-mono" :title="`生成速率（输出 Token / 生成耗时）`">
                      {{ getEstimatedTps(log) }} tok/s
                    </span>
                  </div>
                </td>
                <td>
                  <span
                    class="mp-status-tag"
                    :class="log.statusCode >= 200 && log.statusCode < 300 ? 'tag-ok' : 'tag-err'"
                  >
                    <span class="mp-status-dot-sm" />
                    <span>{{ log.statusCode }}</span>
                  </span>
                </td>
                <td>
                  <!-- Token 分布列：输入 / 缓存 / 输出 / 思考，两行两列等宽卡片 -->
                  <div v-if="log.promptTokens !== undefined || log.completionTokens !== undefined" class="mp-log-tokens-cell font-mono">
                    <div class="mp-token-pill-row">
                      <span class="mp-token-tag is-in" :title="`新增输入 Token: ${getNewInputTokens(log)}（总输入 ${log.promptTokens ?? 0} − 缓存命中 ${log.promptCacheHitTokens ?? 0}）`">
                        <span>输入</span>
                        <strong>{{ formatCompactToken(getNewInputTokens(log)) }}</strong>
                      </span>
                      <span v-if="log.promptCacheHitTokens" class="mp-token-tag is-hit" :title="`缓存命中: ${log.promptCacheHitTokens}`">
                        <span>缓存</span>
                        <strong>{{ formatCompactToken(log.promptCacheHitTokens) }}</strong>
                      </span>
                    </div>
                    <div class="mp-token-pill-row">
                      <span class="mp-token-tag is-out" :title="`输出 Token（纯文本，已剥离思考）: ${getOutputTextTokens(log)}（总输出 ${log.completionTokens ?? 0} − 思考 ${log.reasoningTokens ?? 0}）`">
                        <span>输出</span>
                        <strong>{{ formatCompactToken(getOutputTextTokens(log)) }}</strong>
                      </span>
                      <span v-if="log.reasoningTokens" class="mp-token-tag is-think" :title="`思考推理: ${log.reasoningTokens}`">
                        <span>思考</span>
                        <strong>{{ formatCompactToken(log.reasoningTokens) }}</strong>
                      </span>
                    </div>
                  </div>
                  <span v-else class="text-muted text-xs font-mono">--</span>
                </td>
                <td>
                  <div class="mp-dur-col font-mono text-xs">
                    <div class="mp-dur-row">
                      <span class="mp-dur-label">首:</span>
                      <span class="mp-dur-val text-brand">{{ log.ttftMs ? formatSec(log.ttftMs) : '--' }}</span>
                    </div>
                    <div class="mp-dur-row">
                      <span class="mp-dur-label">总:</span>
                      <span class="mp-dur-val" :class="log.durationMs > 2000 ? 'text-warn font-semibold' : 'text-text font-semibold'">{{ formatSec(log.durationMs) }}</span>
                    </div>
                  </div>
                </td>
                <td style="text-align: center;">
                  <button
                    type="button"
                    class="mp-btn-text"
                    title="查看该条请求详情与全文报文"
                    @click.stop="openLogDetail(log)"
                  >
                    详情
                  </button>
                </td>
              </tr>

              <tr v-if="filteredLogs.length === 0">
                <td colspan="9" class="text-center py-10 text-muted">
                  <div class="mp-empty-box">
                    <div class="mp-empty-icon" v-html="icons.rows" />
                    <p v-if="loadingLogs">正在读取本地数据库请求日志…</p>
                    <p v-else-if="proxyLogs.length > 0">未检索到匹配的请求记录</p>
                    <p v-else>暂无反代调用记录 · 当有客户端发起 API 请求并完结后将持久化保存并显示在这里</p>
                  </div>
                </td>
              </tr>
            </tbody>
          </table>
        </div>

        <!-- 分页：仅当前页数据渲染，翻页时向后端分页查询 -->
        <footer v-if="logTotal > 0" class="app-table-pagination">
          <label>
            <span>每页</span>
            <CustomSelect
              class="app-table-page-size"
              placement="top"
              :options="logPageSizeOptions"
              :model-value="logPageSize"
              aria-label="每页条数"
              @update:model-value="logPageSize = Number($event); goLogPage(1, { filter: logStatusFilter, q: logSearchQuery.trim() })"
            />
            <span>条</span>
          </label>
          <div class="app-table-page-buttons">
            <button type="button" :disabled="logPage <= 1" @click="goLogPage(1, { filter: logStatusFilter, q: logSearchQuery.trim() })">首页</button>
            <button type="button" :disabled="logPage <= 1" @click="goLogPage(logPage - 1, { filter: logStatusFilter, q: logSearchQuery.trim() })">上一页</button>
            <button
              v-for="pageNumber in logPageNumbers"
              :key="String(pageNumber)"
              type="button"
              :class="{ active: pageNumber === logPage }"
              :disabled="pageNumber === '…'"
              @click="typeof pageNumber === 'number' && goLogPage(pageNumber, { filter: logStatusFilter, q: logSearchQuery.trim() })"
            >{{ pageNumber }}</button>
            <button type="button" :disabled="logPage >= logPageCount" @click="goLogPage(logPage + 1, { filter: logStatusFilter, q: logSearchQuery.trim() })">下一页</button>
            <button type="button" :disabled="logPage >= logPageCount" @click="goLogPage(logPageCount, { filter: logStatusFilter, q: logSearchQuery.trim() })">末页</button>
          </div>
          <span class="app-table-page-total">{{ logRangeStart.toLocaleString() }}–{{ logRangeEnd.toLocaleString() }} / {{ logTotal.toLocaleString() }}</span>
        </footer>
      </div>
    </div>

    <!-- 服务配置弹出框 (Modal Dialog) -->
    <div
      v-if="configModalOpen"
      class="mp-modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-labelledby="mp-config-modal-title"
     
    >
      <div class="mp-modal-box mp-modal-box-sm">
        <div class="mp-modal-header">
          <div class="mp-modal-title-group">
            <span class="mp-modal-icon" v-html="icons.settings" />
            <div>
              <h3 id="mp-config-modal-title">反代服务配置</h3>
              <small class="text-muted">管理模型接口 API Key 与渠道行为</small>
            </div>
          </div>
          <button
            type="button"
            class="mp-modal-close"
            title="关闭弹窗 (Esc)"
            @click="configModalOpen = false"
          >
            <span v-html="icons.close" />
          </button>
        </div>

        <form class="mp-modal-body" @submit.prevent="handleSave">
          <div class="mp-field">
            <label>服务端口</label>
            <div class="mp-input font-mono" aria-readonly="true">{{ proxyStatus.port || "启动后确定" }}</div>
            <small>模型接口与 Web UI/API 共用该端口，不能单独修改</small>
          </div>

          <div class="mp-field">
            <label for="mp-apikey">访问密钥</label>
            <input
              id="mp-apikey"
              v-model="proxyConfig.apiKey"
              type="text"
              class="mp-input font-mono"
              placeholder="由服务自动生成，如 sk-openhub-…"
            />
            <small>所有 /v1 模型接口必须携带 API Key；API Key 不放入 URL</small>
          </div>

          <!-- 记录请求/响应全文开关 (默认关闭) -->
          <div class="mp-field">
            <div class="mp-proxy-pool-row" style="padding: 0;">
              <label for="mp-record-body" style="font-size: 13px; font-weight: 600; color: var(--text); cursor: pointer;">记录请求 / 响应全文</label>
              <label class="mp-switch-wrap" :title="proxyConfig.recordRequestBody ? '点击关闭全文记录' : '点击开启全文记录'">
                <input
                  id="mp-record-body"
                  v-model="proxyConfig.recordRequestBody"
                  type="checkbox"
                />
                <span class="mp-switch-round" />
              </label>
            </div>
            <small>开启后同时保存客户端请求原文与上游响应原文（两者同开同关，默认关闭以节省存储）</small>
          </div>

          <!-- 明细日志保留天数（0 = 永久保留） -->
          <div class="mp-field">
            <label for="mp-log-retention">明细日志保留天数</label>
            <input
              id="mp-log-retention"
              v-model.number="proxyConfig.logRetentionDays"
              type="number"
              min="0"
              max="3650"
              class="mp-input font-mono"
              placeholder="0"
            />
            <small>超过保留期的请求明细由网关自动删除并同步清空更早日志的报文全文（0 = 永久保留，靠手动范围清理管理）；渠道统计与长期聚合数据不受清理影响</small>
          </div>

          <!-- 失败重试次数 -->
          <div class="mp-field">
            <label for="mp-max-retries">失败重试次数</label>
            <input
              id="mp-max-retries"
              v-model.number="proxyConfig.maxRetries"
              type="number"
              min="0"
              max="10"
              class="mp-input font-mono"
              placeholder="0"
            />
            <small>请求失败后最多重试几次（默认 0 = 失败直接返回）；开启代理池的渠道同时受可用节点数限制，失败节点自动移至队尾</small>
          </div>
        </form>

        <div class="mp-modal-footer">
          <div class="mp-modal-footer-buttons">
            <button
              type="button"
              class="mp-btn mp-btn-ghost"
              @click="configModalOpen = false"
            >
              取消
            </button>
            <button
              type="button"
              class="mp-btn mp-btn-primary"
              :disabled="savingConfig"
              @click="handleSave"
            >
              <span v-html="icons.check" />
              <span>{{ savingConfig ? "保存中…" : "保存并应用" }}</span>
            </button>
          </div>
        </div>
      </div>
    </div>

    <!-- 请求日志详情弹窗 (Request Log Detail Modal with Token Analytics & Full Payloads) -->
    <div
      v-if="selectedLogForDetail"
      class="mp-modal-backdrop mp-sub-backdrop"
      role="dialog"
      aria-modal="true"
      aria-labelledby="mp-log-detail-title"
     
    >
      <div class="mp-modal-box mp-modal-box-extra-wide mp-log-detail-box">
        <div class="mp-modal-header">
          <div class="mp-modal-title-group">
            <div class="mp-modal-badge-icon" :class="selectedLogForDetail.statusCode >= 400 ? 'is-error' : 'is-success'">
              <span v-html="selectedLogForDetail.statusCode >= 400 ? icons.alert : icons.check" />
            </div>
            <div>
              <div class="mp-modal-title-wrap">
                <h3 id="mp-log-detail-title">请求详情与 Token 全面分析</h3>
                <span
                  class="mp-status-tag"
                  :class="selectedLogForDetail.statusCode >= 200 && selectedLogForDetail.statusCode < 300 ? 'tag-ok' : 'tag-err'"
                >
                  <span class="mp-status-dot-sm" />
                  <span>HTTP {{ selectedLogForDetail.statusCode }}</span>
                </span>
                <span class="mp-header-chip font-mono">{{ selectedLogForDetail.durationMs }}ms</span>
                <span v-if="selectedLogForDetail.ttftMs" class="mp-header-chip font-mono">首字 {{ selectedLogForDetail.ttftMs }}ms</span>
                <span class="mp-header-chip font-mono" :class="selectedLogForDetail.nodeName && selectedLogForDetail.nodeName !== '直连通道' ? 'is-proxy-chip' : ''">🌐 {{ selectedLogForDetail.nodeName || '直连通道' }}</span>
                <span v-if="selectedLogForDetail.stream" class="mp-stream-tag is-stream">流式实时流</span>
              </div>
              <small class="text-muted">请求 ID {{ selectedLogForDetail.id }} · 记录时间 {{ formatLogFull(selectedLogForDetail.timestamp) }}</small>
            </div>
          </div>
          <button
            type="button"
            class="mp-modal-close"
            title="关闭详情 (Esc)"
            @click="closeLogDetail"
          >
            <span v-html="icons.close" />
          </button>
        </div>

        <div class="mp-modal-body">
          <!-- 4 宫格 Token 仪表盘：承载本次请求全部 Token 信息（总量以「/ 总 N · 占比」融入各卡） -->
          <div class="mp-token-dashboard-grid">
            <!-- 卡片 1: 输入侧（新增 = 总输入 − 缓存命中；含缓存写入） -->
            <div class="mp-token-card is-in" title="新增输入 = 总输入 − 缓存命中；缓存写入已计入新增输入">
              <div class="mp-tc-head">
                <span class="mp-tc-label">📥 输入 Tokens</span>
                <span class="mp-tc-badge" :class="(selectedLogForDetail.promptCacheHitTokens || 0) > 0 ? 'badge-hit' : ''">
                  {{ (selectedLogForDetail.promptCacheHitTokens || 0) > 0 ? `⚡ 命中率 ${getCacheHitRate(selectedLogForDetail)}%` : '未命中前缀' }}
                </span>
              </div>
              <div class="mp-tc-value font-mono">
                {{ getNewInputTokens(selectedLogForDetail) }}
              </div>
              <div class="mp-tc-foot font-mono">
                <span>总输入 <strong>{{ selectedLogForDetail.promptTokens ?? 0 }}</strong></span>
                <span class="mp-tc-divider">·</span>
                <span v-if="(selectedLogForDetail.cacheCreationTokens ?? 0) > 0">缓存写入 <strong>{{ selectedLogForDetail.cacheCreationTokens }}</strong></span>
                <span v-if="(selectedLogForDetail.cacheCreationTokens ?? 0) > 0" class="mp-tc-divider">·</span>
                <span>占总 {{ getTokenSharePct(selectedLogForDetail, getNewInputTokens(selectedLogForDetail)) }}</span>
                <span class="mp-tc-divider">·</span>
                <span>合计 <strong>{{ getTokenTotal(selectedLogForDetail) }}</strong></span>
              </div>
            </div>

            <!-- 卡片 2: 缓存命中 Tokens -->
            <div class="mp-token-card is-hit">
              <div class="mp-tc-head">
                <span class="mp-tc-label">⚡ 缓存命中</span>
                <span class="mp-tc-badge badge-hit">前缀缓存复用</span>
              </div>
              <div class="mp-tc-value font-mono text-brand">
                {{ selectedLogForDetail.promptCacheHitTokens ?? 0 }}
              </div>
              <div class="mp-tc-foot font-mono">
                <span v-if="(selectedLogForDetail.promptCacheHitTokens || 0) > 0" class="text-success font-semibold">
                  ✓ 已复用，节省计算
                </span>
                <span v-else class="text-muted">首轮请求或前缀未命中</span>
                <span class="mp-tc-divider">·</span>
                <span>占总 {{ getTokenSharePct(selectedLogForDetail, selectedLogForDetail.promptCacheHitTokens ?? 0) }}</span>
                <span class="mp-tc-divider">·</span>
                <span>合计 <strong>{{ getTokenTotal(selectedLogForDetail) }}</strong></span>
              </div>
            </div>

            <!-- 卡片 3: 思考/推理 Tokens -->
            <div class="mp-token-card is-think">
              <div class="mp-tc-head">
                <span class="mp-tc-label">🧠 思考推理</span>
                <span class="mp-tc-badge">{{ (selectedLogForDetail.reasoningTokens || 0) > 0 ? '深度思考' : '未触发' }}</span>
              </div>
              <div class="mp-tc-value font-mono" style="color: #8b5cf6;">
                {{ selectedLogForDetail.reasoningTokens ?? 0 }}
              </div>
              <div class="mp-tc-foot font-mono">
                <span>占总 {{ getTokenSharePct(selectedLogForDetail, selectedLogForDetail.reasoningTokens ?? 0) }}</span>
                <span class="mp-tc-divider">·</span>
                <span>合计 <strong>{{ getTokenTotal(selectedLogForDetail) }}</strong></span>
              </div>
            </div>

            <!-- 卡片 4: 输出侧（纯文本 = 总输出 − 思考推理） -->
            <div class="mp-token-card is-out" title="生成输出（纯文本）= 总输出 − 思考推理，避免重复计数">
              <div class="mp-tc-head">
                <span class="mp-tc-label">📤 输出 Tokens</span>
                <span class="mp-tc-badge" v-if="(selectedLogForDetail.reasoningTokens || 0) > 0">已剥离思考 {{ selectedLogForDetail.reasoningTokens }} Token</span>
                <span class="mp-tc-badge" v-else>纯文本输出</span>
              </div>
              <div class="mp-tc-value font-mono" style="color: #10b981;">
                {{ getOutputTextTokens(selectedLogForDetail) }}
              </div>
              <div class="mp-tc-foot font-mono">
                <span>总输出 <strong>{{ selectedLogForDetail.completionTokens ?? 0 }}</strong></span>
                <span class="mp-tc-divider">·</span>
                <span>~{{ getEstimatedTps(selectedLogForDetail) }} tok/s</span>
                <span class="mp-tc-divider">·</span>
                <span>占总 {{ getTokenSharePct(selectedLogForDetail, getOutputTextTokens(selectedLogForDetail)) }}</span>
                <span class="mp-tc-divider">·</span>
                <span>合计 <strong>{{ getTokenTotal(selectedLogForDetail) }}</strong></span>
              </div>
            </div>

            <!-- 卡片 5 已移除：总量以「/ 总 N · 占比」融入上方四张分项卡 -->
          </div>

          <!-- 选项卡导航栏 (Tabs) -->
          <div class="mp-detail-tabs-bar">
            <button
              type="button"
              class="mp-detail-tab-btn"
              :class="{ active: detailActiveTab === 'overview' }"
              @click="detailActiveTab = 'overview'"
            >
              <span v-html="icons.chart" />
              <span>📋 请求概览</span>
            </button>

            <button
              v-if="selectedLogForDetail.statusCode >= 400 || selectedLogForDetail.errorMessage"
              type="button"
              class="mp-detail-tab-btn is-error"
              :class="{ active: detailActiveTab === 'error' }"
              @click="detailActiveTab = 'error'"
            >
              <span v-html="icons.alert" />
              <span>⚠️ 错误诊断</span>
            </button>

            <button
              v-if="selectedLogForDetail.requestBody"
              type="button"
              class="mp-detail-tab-btn"
              :class="{ active: detailActiveTab === 'request' }"
              @click="detailActiveTab = 'request'"
            >
              <span v-html="icons.code" />
              <span>📝 客户端请求全文</span>
            </button>

            <button
              v-if="selectedLogForDetail.responseBody"
              type="button"
              class="mp-detail-tab-btn"
              :class="{ active: detailActiveTab === 'response' }"
              @click="detailActiveTab = 'response'"
            >
              <span v-html="icons.message" />
              <span>💬 响应全文</span>
            </button>
          </div>

          <!-- 选项卡内容: 错误诊断 -->
          <div v-if="detailActiveTab === 'error'" class="mp-detail-tab-content">
            <div class="mp-log-error-banner">
              <div class="mp-leb-icon" v-html="icons.alert" />
              <div class="mp-leb-content">
                <div class="mp-leb-title">
                  <strong>错误原因分析 (HTTP {{ selectedLogForDetail.statusCode }})</strong>
                </div>
                <p class="mp-leb-reason">{{ selectedLogForDetail.errorMessage || '上游返回非 200 异常状态' }}</p>
                <div class="mp-leb-suggestion">
                  <span class="mp-leb-tag">💡 排查建议</span>
                  <span>{{ getErrorSuggestion(selectedLogForDetail) }}</span>
                </div>
              </div>
            </div>

            <div v-if="selectedLogForDetail.responseBody" class="mp-log-raw-box">
              <div class="mp-lrb-header">
                <label>上游原始错误响应报文</label>
                <div class="flex-center-start gap-2">
                  <button
                    type="button"
                    class="mp-action-btn"
                    title="一键缩进格式化 JSON"
                    @click="formatLogBody('response')"
                  >
                    <span class="font-mono">{ }</span>
                    <span>格式化</span>
                  </button>
                  <button
                    type="button"
                    class="mp-action-btn"
                    title="复制原始错误报文"
                    @click="copyText(displayBody('response'), '错误响应报文')"
                  >
                    <span v-html="icons.copy" />
                    <span>复制</span>
                  </button>
                </div>
              </div>
              <pre class="mp-lrb-pre font-mono">{{ displayBody('response') }}</pre>
            </div>
          </div>

          <!-- 选项卡内容 3: 请求全文 -->
          <div v-if="detailActiveTab === 'request'" class="mp-detail-tab-content">
            <div class="mp-log-raw-box">
              <div class="mp-lrb-header">
                <div class="flex-center-start gap-2">
                  <label>客户端完整请求报文</label>
                  <span class="mp-stream-tag" :class="{ 'is-stream': selectedLogForDetail.stream }">
                    {{ selectedLogForDetail.stream ? "流式传输" : "同步响应" }}
                  </span>
                </div>
                <div class="flex-center-start gap-2">
                  <button
                    v-if="selectedLogForDetail.requestBody"
                    type="button"
                    class="mp-action-btn"
                    title="一键缩进格式化 JSON"
                    @click="formatLogBody('request')"
                  >
                    <span class="font-mono">{ }</span>
                    <span>格式化</span>
                  </button>
                  <button
                    v-if="selectedLogForDetail.requestBody"
                    type="button"
                    class="mp-action-btn"
                    title="一键复制客户端完整请求报文"
                    @click="copyText(displayBody('request'), '请求全文')"
                  >
                    <span v-html="icons.copy" />
                    <span>复制全文</span>
                  </button>
                </div>
              </div>
              <pre class="mp-lrb-pre font-mono">{{ displayBody('request') || '// 未开启「记录请求 / 响应全文」开关\n// 请求与响应全文同开同关。如需查看完整报文，请在右上角「服务配置」中开启该开关。' }}</pre>
            </div>
          </div>

          <!-- 选项卡内容 4: 响应全文（结构化渲染 + 原文切换） -->
          <div v-if="detailActiveTab === 'response'" class="mp-detail-tab-content">
            <div class="mp-log-raw-box">
              <div class="mp-lrb-header">
                <div class="flex-center-start gap-2">
                  <label>上游响应</label>
                  <div class="mp-view-toggle">
                    <button
                      type="button"
                      :class="{ active: responseViewMode === 'structured' }"
                      @click="responseViewMode = 'structured'"
                    >语义视图</button>
                    <button
                      type="button"
                      :class="{ active: responseViewMode === 'raw' }"
                      @click="responseViewMode = 'raw'"
                    >原文</button>
                  </div>
                </div>
                <div class="flex-center-start gap-2">
                  <button
                    v-if="selectedLogForDetail.responseBody && responseViewMode === 'raw'"
                    type="button"
                    class="mp-action-btn"
                    title="一键缩进格式化（JSON 美化 / SSE 逐帧展开）"
                    @click="formatLogBody('response')"
                  >
                    <span class="font-mono">{ }</span>
                    <span>格式化</span>
                  </button>
                  <button
                    v-if="selectedLogForDetail.responseBody"
                    type="button"
                    class="mp-action-btn"
                    title="一键复制上游响应原文"
                    @click="copyText(displayBody('response'), '响应原文')"
                  >
                    <span v-html="icons.copy" />
                    <span>复制全文</span>
                  </button>
                </div>
              </div>

              <!-- 语义视图：按出现顺序渲染 工具调用 / 正文(Markdown) / 思考 片段 -->
              <div v-if="responseViewMode === 'structured'" class="mp-seg-list">
                <div v-if="!responseSegments.length" class="text-muted text-xs" style="padding: 12px;">
                  未捕获到上游响应报文
                </div>
                <template v-for="(seg, i) in responseSegments" :key="i">
                  <div v-if="seg.kind === 'tool'" class="mp-seg-card mp-seg-tool">
                    <div class="mp-seg-tool-head">
                      <span class="mp-seg-kind-tag is-tool">🛠️ 工具调用</span>
                      <strong class="font-mono">{{ seg.name || '未知工具' }}</strong>
                      <span v-if="seg.callId" class="mp-seg-callid font-mono">{{ seg.callId }}</span>
                    </div>
                    <pre class="mp-seg-args font-mono">{{ prettyToolArgs(seg.args) }}</pre>
                  </div>

                  <div v-else-if="seg.kind === 'text' && seg.text.trim()" class="mp-seg-card mp-seg-text">
                    <div class="mp-seg-tool-head">
                      <span class="mp-seg-kind-tag is-text">📄 内容输出</span>
                    </div>
                    <!-- 上游模型输出，marked 解析后经 DOMPurify 消毒 -->
                    <div class="mp-seg-markdown" v-html="renderSegmentMarkdown(seg.text)" />
                  </div>

                  <div v-else-if="seg.kind === 'reasoning' && seg.text.trim()" class="mp-seg-card mp-seg-reason">
                    <div class="mp-seg-tool-head">
                      <span class="mp-seg-kind-tag is-reason">💭 思考</span>
                    </div>
                    <pre class="mp-seg-reason-body font-mono">{{ seg.text }}</pre>
                  </div>
                </template>
              </div>

              <!-- 原文视图 -->
              <pre
                v-else
                class="mp-lrb-pre font-mono"
              >{{ displayBody('response') || '未捕获到上游响应报文' }}</pre>
            </div>
          </div>

          <!-- 选项卡内容: 请求概览（路由 / 状态 / 耗时等调用元数据） -->
          <div v-if="detailActiveTab === 'overview'" class="mp-detail-tab-content">
            <div class="mp-log-detail-grid">
              <div class="mp-ld-item">
                <label>HTTP 状态码</label>
                <div class="mp-ld-val">
                  <span
                    class="mp-status-tag"
                    :class="selectedLogForDetail.statusCode >= 200 && selectedLogForDetail.statusCode < 300 ? 'tag-ok' : 'tag-err'"
                  >
                    <span class="mp-status-dot-sm" />
                    <span>{{ selectedLogForDetail.statusCode }} {{ selectedLogForDetail.statusCode >= 200 && selectedLogForDetail.statusCode < 300 ? '成功' : '异常' }}</span>
                  </span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>调用客户端</label>
                <div class="mp-ld-val font-mono">
                  <span>{{ selectedLogForDetail.clientName || '--' }}</span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>入网 -> 出网</label>
                <div class="mp-ld-val" style="flex-direction: column; align-items: flex-start; gap: 4px;">
                  <div style="display: flex; align-items: center; gap: 6px;">
                    <span class="mp-path-label">入</span>
                    <code class="font-mono">{{ selectedLogForDetail.path }}</code>
                  </div>
                  <div v-if="upstreamPathDiffers(selectedLogForDetail.path, selectedLogForDetail.upstreamUrl)" style="display: flex; align-items: center; gap: 6px;">
                    <span class="mp-path-label">出</span>
                    <code class="font-mono" style="font-size: 11px; color: var(--text-muted, #888);">{{ selectedLogForDetail.upstreamUrl }}</code>
                  </div>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>上游渠道与模型</label>
                <div class="mp-ld-val">
                  <span class="mp-proto-tag">{{ selectedLogForDetail.channelId.toUpperCase() }}</span>
                  <strong class="font-mono">{{ selectedLogForDetail.model }}</strong>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>出网通道 / 节点</label>
                <div class="mp-ld-val">
                  <span
                    class="mp-node-pill font-mono"
                    :class="selectedLogForDetail.nodeName && selectedLogForDetail.nodeName !== '直连通道' ? 'is-proxy' : 'is-direct'"
                  >
                    <span class="mp-node-dot" />
                    <span>{{ selectedLogForDetail.nodeName || '直连通道' }}</span>
                  </span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>响应耗时（首字 / 总）</label>
                <div class="mp-ld-val font-mono">
                  <span>
                    首 {{ selectedLogForDetail.ttftMs ? formatSec(selectedLogForDetail.ttftMs) : (selectedLogForDetail.stream ? '--' : '同步即达') }}
                    · 总 {{ formatSec(selectedLogForDetail.durationMs) }}
                  </span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>平均生成速率</label>
                <div class="mp-ld-val font-mono">
                  <span>~{{ getEstimatedTps(selectedLogForDetail) }} Token/秒</span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>传输协议模式</label>
                <div class="mp-ld-val">
                  <span>{{ selectedLogForDetail.stream ? "流式实时传输" : "同步响应" }}</span>
                </div>
              </div>

              <div v-if="selectedLogForDetail.errorMessage" class="mp-ld-item" style="grid-column: 1 / -1;">
                <label>错误信息</label>
                <div class="mp-ld-val" style="color: var(--danger, #e5484d); word-break: break-all; white-space: pre-wrap;">
                  <span>{{ selectedLogForDetail.errorMessage }}</span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>请求唯一标识</label>
                <div class="mp-ld-val font-mono text-muted text-xs">
                  <span>{{ selectedLogForDetail.id }}</span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>请求时间</label>
                <div class="mp-ld-val font-mono text-muted">
                  <span>{{ formatLogFull(selectedLogForDetail.timestamp) }}</span>
                </div>
              </div>
            </div>
          </div>
        </div>

        <div class="mp-modal-footer">
          <div class="mp-modal-footer-hint">
            <span>支持使用键盘 <kbd>Esc</kbd> 快速关闭详情</span>
          </div>
          <button
            type="button"
            class="mp-btn mp-btn-primary"
            @click="closeLogDetail"
          >
            确定
          </button>
        </div>
      </div>
    </div>

    <!-- 弹窗 1: 顶栏「可用模型」- 反代网关对外聚合可用模型总览 (Gateway Models Modal) -->
    <div
      v-if="gatewayModelsModalOpen"
      class="mp-modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-labelledby="mp-gateway-models-title"
     
    >
      <div class="mp-modal-box mp-modal-box-wide">
        <div class="mp-modal-header">
          <div class="mp-modal-title-group">
            <div class="mp-modal-badge-icon">
              <span v-html="icons.cpu" />
            </div>
            <div>
              <div class="mp-modal-title-wrap">
                <h3 id="mp-gateway-models-title">反代网关可用模型总览</h3>
                <span class="mp-header-chip">{{ totalGatewayModelsCount }} 个就绪模型</span>
              </div>
              <small class="text-muted">网关聚合对外提供的模型目录 · 点击模型或复制按钮即可一键复制调用 ID</small>
            </div>
          </div>
          <button
            type="button"
            class="mp-modal-close"
            title="关闭弹窗 (Esc)"
            @click="closeGatewayModelsModal"
          >
            <span v-html="icons.close" />
          </button>
        </div>

        <div class="mp-modal-body">
          <div class="mp-models-modal-toolbar">
            <div class="mp-search-box flex-1">
              <span class="mp-search-icon" v-html="icons.search" />
              <input
                v-model="gatewaySearchQuery"
                type="search"
                placeholder="搜索模型名称 (如 deepseek, nemotron, mimo, laguna)…"
                class="mp-search-input-lg"
              />
              <button
                v-if="gatewaySearchQuery"
                type="button"
                class="mp-search-clear-btn"
                title="清空搜索"
                @click="gatewaySearchQuery = ''"
              >
                <span v-html="icons.close" />
              </button>
            </div>
            <button
              type="button"
              class="mp-btn mp-btn-ghost"
              :disabled="fetchingModels"
              title="重新拉取并更新网关模型列表"
              @click="refreshModels"
            >
              <span :class="{ 'mp-spin': fetchingModels }" v-html="icons.restore" />
              <span>{{ fetchingModels ? "正在拉取…" : "刷新列表" }}</span>
            </button>
          </div>

          <!-- 按渠道分组展示模型矩阵 -->
          <div class="mp-channel-groups-container">
            <div
              v-for="group in gatewayGroupedModels"
              :key="group.channel.id"
              class="mp-channel-group-block"
            >
              <!-- 分组头部栏 -->
              <div class="mp-group-header">
                <div class="mp-group-title-row">
                  <div class="mp-group-icon">
                    <span v-html="icons.shield" />
                  </div>
                  <div class="mp-group-name-wrap">
                    <h4 class="mp-group-name">{{ group.channel.name }} 渠道</h4>
                    <span
                      v-if="group.channel.enabledModels != null"
                      class="mp-group-count-badge is-filtered"
                      :title="`已启用白名单：仅对外暴露 ${group.models.length}/${group.totalKnown} 个已知模型；上游新增的模型需在「管理模型」中勾选后才会出现在此处`"
                    >白名单 {{ group.models.length }}/{{ group.totalKnown }}</span>
                    <span class="mp-group-count-badge">{{ group.models.length }} 个模型</span>
                    <span
                      v-if="group.channel.enabledModels != null && group.totalKnown > group.models.length"
                      class="mp-group-count-badge is-filtered"
                      :title="`上游有 ${group.totalKnown - group.models.length} 个模型不在白名单内，总览不展示；可在「管理模型」中调整`"
                    >+{{ group.totalKnown - group.models.length }} 未纳入</span>
                  </div>
                </div>
                <div class="mp-group-meta">
                  <span class="mp-group-endpoint font-mono">{{ group.channel.upstreamUrl }}</span>
                  <span
                    class="mp-status-pill mp-status-pill-xs"
                    :class="{ active: group.channel.enabled }"
                  >
                    <span class="mp-status-dot" />
                    <span>{{ group.channel.enabled ? '已启用' : '已禁用' }}</span>
                  </span>
                </div>
              </div>

              <!-- 属于该渠道的模型卡片矩阵 -->
              <div v-if="group.models.length > 0" class="mp-model-cards-grid">
                <div
                  v-for="model in group.models"
                  :key="model"
                  class="mp-model-elegant-card"
                  :class="{ 'is-copied': copiedModelId === model }"
                  @click="copyModel(model, group.channel)"
                >
                  <div class="mp-mec-left">
                    <div class="mp-mec-title-row">
                      <span class="mp-model-free-badge">{{ channelAlias(group.channel) }}</span>
                      <span class="mp-model-name-title">{{ model }}</span>
                      <span
                        v-if="(channelOverlapByModel.get(model.toLowerCase())?.length ?? 0) >= 2"
                        class="mp-overlap-badge"
                        :title="`该模型由 ${channelOverlapByModel.get(model.toLowerCase())!.length} 个渠道共同提供；不带别名前缀调用时按「管理模型」中配置的顺序路由`"
                      >{{ channelOverlapByModel.get(model.toLowerCase())!.length }} 渠道共供</span>
                    </div>
                    <div class="mp-mec-id-row">
                      <span class="mp-mec-id-label">调用 ID</span>
                      <code class="mp-mec-id-code">{{ channelAlias(group.channel) }}/{{ model }}</code>
                    </div>
                  </div>

                  <div class="mp-mec-right">
                    <button
                      type="button"
                      class="mp-copy-action-btn"
                      :class="{ 'copied': copiedModelId === model }"
                      :title="`复制 ${channelAlias(group.channel)}/${model}`"
                      @click.stop="copyModel(model, group.channel)"
                    >
                      <span v-html="copiedModelId === model ? icons.check : icons.copy" />
                      <span>{{ copiedModelId === model ? '已复制' : '复制 ID' }}</span>
                    </button>
                  </div>
                </div>
              </div>

              <div v-else class="mp-group-empty-note text-muted text-xs">
                <span v-if="!group.channel.enabled">该渠道当前已被禁用</span>
                <span v-else-if="gatewaySearchQuery">未检索到匹配的模型</span>
                <span v-else>暂无就绪模型</span>
              </div>
            </div>
          </div>

          <div v-if="totalGatewayModelsCount === 0" class="mp-empty-box">
            <div class="mp-empty-icon" v-html="icons.cpu" />
            <p v-if="fetchingModels">正在从各渠道上游拉取可用模型列表…</p>
            <p v-else>暂无匹配的可用模型</p>
          </div>
        </div>

        <div class="mp-modal-footer">
          <div class="mp-modal-footer-hint text-muted text-xs">
            <span>💡 提示：在客户端工具中输入调用 ID 即可发起调用</span>
          </div>
          <button
            type="button"
            class="mp-btn mp-btn-primary"
            @click="closeGatewayModelsModal"
          >
            完成
          </button>
        </div>
      </div>
    </div>

    <!-- 弹窗 2: 渠道卡片「管理模型」- 勾选该渠道对外暴露的可用模型 (Channel Models Modal) -->
    <div
      v-if="channelModelsModalOpen"
      class="mp-modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-labelledby="mp-channel-models-title"
     
    >
      <div class="mp-modal-box mp-modal-box-wide">
        <div class="mp-modal-header">
          <div class="mp-modal-title-group">
            <div class="mp-modal-badge-icon">
              <span v-html="icons.shield" />
            </div>
            <div>
              <div class="mp-modal-title-wrap">
                <h3 id="mp-channel-models-title">{{ selectedChannel?.name || 'OpenCode' }} · 管理可用模型</h3>
                <span class="mp-header-chip">{{ channelCheckedCount }} / {{ selectedChannelModels().length }} 个已启用</span>
                <span class="mp-header-endpoint-chip font-mono">{{ selectedChannel?.upstreamUrl }}</span>
              </div>
              <small class="text-muted">勾选需要对外暴露的模型 · 未勾选的模型将不在可用模型列表与网关目录中展示</small>
            </div>
          </div>
          <button
            type="button"
            class="mp-modal-close"
            title="关闭弹窗 (Esc)"
            @click="closeChannelModelsModal"
          >
            <span v-html="icons.close" />
          </button>
        </div>

        <div class="mp-modal-body">
          <!-- 弹窗内部 Tab 切换（渠道未配置任何 Key 时不提供分组调度操作） -->
          <div v-if="channelRawKeys.length > 0" class="mp-inner-tab-nav">
            <button
              type="button"
              class="mp-inner-tab-btn"
              :class="{ active: channelModalTab === 'models' }"
              @click="channelModalTab = 'models'"
            >
              <span v-html="icons.cpu" />
              <span>可用模型管理</span>
              <span class="mp-inner-tab-badge font-mono">{{ channelCheckedCount }}/{{ selectedChannelModels().length }}</span>
            </button>
            <button
              type="button"
              class="mp-inner-tab-btn"
              :class="{ active: channelModalTab === 'keys' }"
              @click="channelModalTab = 'keys'"
            >
              <span v-html="icons.key" />
              <span>Key 分组与轮询调度</span>
              <span class="mp-inner-tab-badge font-mono">{{ channelRawKeys.length }} Key · {{ channelDraftKeyGroups.length }} 组</span>
            </button>
          </div>

          <!-- 视图 1: 模型管理与重叠模型优先级 -->
          <template v-if="channelModalTab === 'models'">
            <div class="mp-models-modal-toolbar">
              <div class="mp-search-box flex-1">
                <span class="mp-search-icon" v-html="icons.search" />
                <input
                  v-model="channelSearchQuery"
                  type="search"
                  placeholder="搜索此渠道下的模型 (如 deepseek, nemotron, mimo, laguna)…"
                  class="mp-search-input-lg"
                />
                <button
                  v-if="channelSearchQuery"
                  type="button"
                  class="mp-search-clear-btn"
                  title="清空搜索"
                  @click="channelSearchQuery = ''"
                >
                  <span v-html="icons.close" />
                </button>
              </div>
              <button
                type="button"
                class="mp-btn mp-btn-ghost"
                :class="{ 'is-active': channelModelAllChecked }"
                title="勾选全部模型（对外暴露全部）"
                @click="selectAllChannelModels"
              >
                <span v-html="icons.check" />
                <span>全选</span>
              </button>
              <button
                type="button"
                class="mp-btn mp-btn-ghost"
                :class="{ 'is-active': !channelModelAllChecked && channelCheckedCount === 0 }"
                title="取消全部勾选（不对外暴露任何模型）"
                @click="clearChannelModels"
              >
                <span v-html="icons.close" />
                <span>清空</span>
              </button>
              <button
                type="button"
                class="mp-btn mp-btn-ghost"
                :disabled="fetchingDraftModels"
                title="从该上游渠道重新获取模型列表"
                @click="refreshChannelDraftModels"
              >
                <span :class="{ 'mp-spin': fetchingDraftModels }" v-html="icons.restore" />
                <span>{{ fetchingDraftModels ? "正在拉取…" : "刷新上游模型" }}</span>
              </button>
              <label class="mp-toolbar-switch" title="只显示已勾选启用的模型">
                <input v-model="channelModelEnabledOnly" type="checkbox" />
                <span>仅看已启用</span>
              </label>
              <div class="mp-toolbar-sort">
                <select v-model="channelModelSortMode" class="mp-sort-select" title="列表排序方式">
                  <option value="discovery">上游顺序</option>
                  <option value="usage">按调用量</option>
                </select>
              </div>
            </div>

            <!-- 可勾选的模型列表：卡片行 = 勾选框 + 名称/状态 + 用量统计 + 调用 ID 复制 -->
            <div class="mp-model-check-list">
              <div
                v-for="model in filteredChannelModels"
                :key="model"
                class="mp-mcm-card"
                :class="{ 'is-selected': isModelChecked(model) }"
                role="checkbox"
                :aria-checked="isModelChecked(model)"
                :tabindex="0"
                @click="toggleModel(model)"
                @keydown.enter.space.prevent="toggleModel(model)"
              >
                <span class="mp-mec-check" aria-hidden="true">
                  <span v-html="isModelChecked(model) ? icons.check : ''" />
                </span>

                <!-- 主体：模型名 + 启用状态 + Key 专属提示 -->
                <div class="mp-mcm-main">
                  <div class="mp-mcm-title-row">
                    <span class="mp-model-name-title">{{ model }}</span>
                    <span class="mp-status-pill mp-status-pill-xs" :class="{ active: isModelChecked(model) }">
                      <span class="mp-status-dot" />
                      <span>{{ isModelChecked(model) ? '已启用' : '未启用' }}</span>
                    </span>
                    <span
                      v-if="channelModelStatsMap.get(model.toLowerCase())?.todayRequests"
                      class="mp-mcm-today-chip font-mono"
                      :title="`今日已调用 ${channelModelStatsMap.get(model.toLowerCase())!.todayRequests} 次 / ${fmtCompactTokens(channelModelStatsMap.get(model.toLowerCase())!.todayTokens)} tokens`"
                    >今日 {{ channelModelStatsMap.get(model.toLowerCase())!.todayRequests }} 次</span>
                  </div>
                  <code class="mp-mcm-id-code font-mono" :title="'点击复制调用 ID'" @click.stop="selectedChannel && copyModel(model, selectedChannel)">
                    {{ channelAlias(selectedChannel) }}/{{ model }}
                    <span class="mp-mcm-copy-icon" v-html="copiedModelId === model ? icons.check : icons.copy" />
                    <span v-if="copiedModelId === model" class="mp-mcm-copy-ok">已复制</span>
                  </code>
                </div>

                <!-- 右侧用量统计区 -->
                <div class="mp-mcm-stats">
                  <template v-if="channelModelStatsMap.get(model.toLowerCase())">
                    <div class="mp-mcm-stat" title="累计调用次数（含失败）">
                      <span class="mp-mcm-stat-val font-mono">{{ channelModelStatsMap.get(model.toLowerCase())!.totalRequests }}</span>
                      <span class="mp-mcm-stat-label">次调用</span>
                    </div>
                    <div
                      class="mp-mcm-stat"
                      :class="{ 'is-bad': channelModelStatsMap.get(model.toLowerCase())!.failedRequests > 0 }"
                      :title="`失败 ${channelModelStatsMap.get(model.toLowerCase())!.failedRequests} 次，成功率 ${
                        channelModelStatsMap.get(model.toLowerCase())!.totalRequests
                          ? Math.round((1 - channelModelStatsMap.get(model.toLowerCase())!.failedRequests / channelModelStatsMap.get(model.toLowerCase())!.totalRequests) * 100)
                          : 100
                      }%`"
                    >
                      <span class="mp-mcm-stat-val font-mono">{{ fmtCompactTokens(channelModelStatsMap.get(model.toLowerCase())!.totalTokens) }}</span>
                      <span class="mp-mcm-stat-label">tokens</span>
                    </div>
                    <div class="mp-mcm-stat" title="平均响应耗时（含首字）">
                      <span class="mp-mcm-stat-val font-mono">{{ formatSec(channelModelStatsMap.get(model.toLowerCase())!.avgDurationMs) }}</span>
                      <span class="mp-mcm-stat-label">均耗</span>
                    </div>
                    <div class="mp-mcm-stat mp-mcm-stat-wide" title="最近一次调用时间">
                      <span class="mp-mcm-stat-val">{{ fmtLastUsed(channelModelStatsMap.get(model.toLowerCase())!.lastUsedAt) }}</span>
                      <span class="mp-mcm-stat-label">最近使用</span>
                    </div>
                  </template>
                  <template v-else>
                    <div class="mp-mcm-stat mp-mcm-stat-empty">
                      <span
                        class="mp-mcm-stat-label"
                        :class="{ 'mp-spin-icon': loadingModelStats }"
                      >{{ loadingModelStats ? '统计加载中…' : '暂无调用记录' }}</span>
                    </div>
                  </template>
                </div>

                <!-- 底部第四行：Key 专属限制提示 -->
                <span
                  v-if="keySupportCountFor(model) >= 0 && !isModelFreeForAllKeys(model)"
                  class="mp-kg-model-tag mp-mcm-key-tag"
                  :title="`该渠道有专属 Key 限制：仅 ${keySupportCountFor(model)} 个 Key 支持此模型，调度时可能受 Key 分组配置影响`"
                >Key 专属 · {{ keySupportCountFor(model) }}</span>

                <!-- 行尾：该模型的代理出口策略（默认跟随渠道级配置） -->
                <div class="mp-mcm-proxy" @click.stop>
                  <span
                    class="mp-mcm-proxy-label"
                    :title="`出网代理策略：默认${channelLevelProxyLabel}；可为本模型单独指定`"
                  >代理</span>
                  <select
                    class="mp-mcm-proxy-select"
                    :value="effectiveProxyMode(model)"
                    title="为本模型单独选择出网代理策略"
                    @change="setModelProxyMode(model, ($event.target as HTMLSelectElement).value as ModelProxyMode)"
                  >
                    <option value="inherit">跟随渠道</option>
                    <option value="direct">强制直连</option>
                    <option value="pool">代理池</option>
                    <option value="fixed">固定节点</option>
                  </select>
                  <span
                    v-if="effectiveProxyMode(model) !== 'inherit' && effectiveProxyMode(model) !== effectiveChannelProxyMode()"
                    class="mp-mcm-proxy-dot"
                    title="本模型代理策略与渠道级配置不同"
                  />
                </div>
              </div>
            </div>

            <div v-if="filteredChannelModels.length === 0" class="mp-empty-box">
              <div class="mp-empty-icon" v-html="icons.shield" />
              <p v-if="fetchingDraftModels">正在从上游渠道拉取最新模型…</p>
              <p v-else-if="channelSearchQuery">未检索到匹配的模型</p>
              <p v-else>暂无模型数据，请先点击「刷新上游模型」</p>
            </div>
          </template>

          <!-- 视图 2: Key 分组调度与故障转移 -->
          <template v-else>
            <div class="mp-key-groups-toolbar">
              <div class="mp-kg-intro text-xs text-muted">
                <span>💡 <strong>调度策略</strong>：同组内 Key 循环轮询；前一组 Key 全部请求失败时，自动平滑切换到下一优先级分组重试。</span>
              </div>
              <div class="mp-add-group-form">
                <input
                  v-model="newKeyGroupName"
                  type="text"
                  placeholder="新分组名称（如 备用通道2）"
                  class="mp-add-group-input"
                  @keydown.enter.prevent="addKeyGroup"
                />
                <button
                  type="button"
                  class="mp-btn mp-btn-primary mp-btn-sm"
                  :disabled="!newKeyGroupName.trim()"
                  @click="addKeyGroup"
                >
                  <span v-html="icons.plus" />
                  <span>添加分组</span>
                </button>
              </div>
            </div>

            <div class="mp-key-groups-container">
              <div
                v-for="(grp, gIdx) in channelDraftKeyGroups"
                :key="grp.id"
                class="mp-key-group-card"
                :class="{ 'is-disabled': !grp.enabled }"
              >
                <!-- 分组头部 -->
                <div class="mp-key-group-head">
                  <div class="mp-kg-head-left">
                    <span class="mp-kg-priority-badge font-mono" :class="{ 'is-top': gIdx === 0 }">
                      #{{ gIdx + 1 }} {{ gIdx === 0 ? "主力优先" : "故障后备" }}
                    </span>
                    <input
                      v-model="grp.name"
                      type="text"
                      class="mp-kg-name-input"
                      title="点击直接修改分组名称"
                    />
                    <span class="mp-group-count-badge font-mono">{{ keysInGroup(grp.id).length }} 个 Key</span>
                  </div>

                  <div class="mp-kg-head-actions">
                    <button
                      type="button"
                      class="mp-reorder-btn"
                      :disabled="gIdx === 0"
                      title="提升该分组优先级"
                      @click="moveKeyGroup(grp.id, -1)"
                    >
                      <span v-html="icons.arrowUp" />
                    </button>
                    <button
                      type="button"
                      class="mp-reorder-btn"
                      :disabled="gIdx === channelDraftKeyGroups.length - 1"
                      title="降低该分组优先级"
                      @click="moveKeyGroup(grp.id, 1)"
                    >
                      <span style="transform: rotate(180deg); display: inline-flex;" v-html="icons.arrowUp" />
                    </button>
                    <button
                      type="button"
                      class="mp-reorder-btn text-danger"
                      title="删除该分组（组内 Key 将归入首个可用组）"
                      @click="deleteKeyGroup(grp.id)"
                    >
                      <span v-html="icons.trash" />
                    </button>
                    <label class="mp-switch-wrap" :title="grp.enabled ? '点击禁用该分组' : '点击启用该分组'">
                      <input
                        v-model="grp.enabled"
                        type="checkbox"
                      />
                      <span class="mp-switch-round" />
                    </label>
                  </div>
                </div>

                <!-- 分组内部 Key 列表 -->
                <div class="mp-kg-keys-list">
                  <div
                    v-for="(kItem, kIdx) in keysInGroup(grp.id)"
                    :key="kItem.key"
                    class="mp-kg-key-row"
                    :class="{ 'is-disabled': !kItem.enabled }"
                  >
                    <div class="mp-kg-key-main">
                      <span class="mp-kg-key-acc">{{ kItem.accountLabel }}</span>
                      <code class="mp-kg-key-code font-mono">{{ maskKeyStr(kItem.key) }}</code>
                      <span
                        v-if="kItem.supportedModels?.length"
                        class="mp-kg-model-tag"
                        :title="`该 Key 仅支持：${kItem.supportedModels.join(', ')}`"
                      >专属 {{ kItem.supportedModels.length }} 模型</span>
                      <span v-else class="mp-kg-model-tag is-all">全量模型</span>
                    </div>

                    <div class="mp-kg-key-controls">
                      <!-- 固定通道模式：为该 Key 绑定代理池通道（不同账号走不同通道） -->
                      <select
                        v-if="selectedChannelIsFixedChannel"
                        v-model="kItem.fixedChannelId"
                        class="mp-kg-group-select"
                        title="该 Key 绑定的代理池固定通道；留空使用渠道默认通道"
                      >
                        <option value="">默认通道</option>
                        <option
                          v-if="isDeletedPoolChannelRef(kItem.fixedChannelId)"
                          :value="kItem.fixedChannelId"
                        >
                          原通道已删除
                        </option>
                        <option
                          v-for="pc in proxyPoolChannelOptions"
                          :key="pc.id"
                          :value="pc.id"
                        >
                          {{ pc.name }}
                        </option>
                      </select>

                      <!-- 移动所属分组下拉框 -->
                      <select
                        v-model="kItem.groupId"
                        class="mp-kg-group-select"
                        title="变更该 Key 所属分组"
                      >
                        <option
                          v-for="targetGrp in channelDraftKeyGroups"
                          :key="targetGrp.id"
                          :value="targetGrp.id"
                        >
                          移动至 {{ targetGrp.name }}
                        </option>
                      </select>

                      <!-- 组内排序按钮 -->
                      <button
                        type="button"
                        class="mp-reorder-btn"
                        :disabled="kIdx === 0"
                        title="组内上移（优先轮询）"
                        @click="moveKeyInGroup(kItem.key, -1)"
                      >
                        <span v-html="icons.arrowUp" />
                      </button>
                      <button
                        type="button"
                        class="mp-reorder-btn"
                        :disabled="kIdx === keysInGroup(grp.id).length - 1"
                        title="组内下移"
                        @click="moveKeyInGroup(kItem.key, 1)"
                      >
                        <span style="transform: rotate(180deg); display: inline-flex;" v-html="icons.arrowUp" />
                      </button>

                      <!-- 单 Key 启用/禁用开关 -->
                      <label class="mp-switch-wrap" :title="kItem.enabled ? '点击禁用此 Key' : '点击启用此 Key'">
                        <input
                          v-model="kItem.enabled"
                          type="checkbox"
                        />
                        <span class="mp-switch-round" />
                      </label>
                    </div>
                  </div>

                  <div v-if="keysInGroup(grp.id).length === 0" class="mp-kg-empty-note text-muted text-xs">
                    该分组内暂无 Key，可从其他分组将 Key 移入此处
                  </div>
                </div>
              </div>
            </div>
          </template>
        </div>

        <div class="mp-modal-footer">
          <div class="mp-modal-footer-hint text-muted text-xs">
            <span v-if="channelModelAllChecked">💡 当前为全选状态：该渠道全部模型对外可用</span>
            <span v-else-if="channelCheckedCount > 0">💡 已勾选 {{ channelCheckedCount }} 个模型，仅这些会出现在可用模型列表</span>
            <span v-else>⚠️ 未勾选任何模型：保存后该渠道将不对外暴露模型</span>
          </div>
          <div class="mp-modal-footer-buttons">
            <button
              type="button"
              class="mp-btn mp-btn-ghost"
              @click="closeChannelModelsModal"
            >
              取消
            </button>
            <button
              type="button"
              class="mp-btn mp-btn-primary"
              :disabled="savingConfig"
              title="保存当前勾选结果到渠道配置"
              @click="saveChannelModelSelection"
            >
              <span v-html="icons.check" />
              <span>{{ savingConfig ? "保存中…" : "保存所选模型" }}</span>
            </button>
          </div>
        </div>
      </div>
    </div>

    <!-- 弹窗 3: 渠道设置 - 内部代理池轮询开关 (Channel Settings Modal) -->
    <div
      v-if="channelSettingsDialogOpen"
      class="mp-modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-labelledby="mp-channel-settings-title"
     
    >
      <div class="mp-modal-box">
        <div class="mp-modal-header">
          <div class="mp-modal-title-group">
            <div class="mp-modal-badge-icon">
              <span v-html="icons.settings" />
            </div>
            <div>
              <div class="mp-modal-title-wrap">
                <h3 id="mp-channel-settings-title">{{ channelSettingsTarget?.name || 'OpenCode' }} · 渠道设置</h3>
                <span class="mp-header-endpoint-chip font-mono">{{ channelSettingsTarget?.upstreamUrl }}</span>
              </div>
              <small class="text-muted">{{ channelSettingsTarget?.siteId ? "站点转换渠道 · 配置代理池固定通道" : "官方免费通道 · 配置内部代理池轮询" }}</small>
            </div>
          </div>
          <button
            type="button"
            class="mp-modal-close"
            title="关闭弹窗 (Esc)"
            @click="closeChannelSettingsDialog"
          >
            <span v-html="icons.close" />
          </button>
        </div>

        <div class="mp-modal-body">
          <!-- 英文别名 -->
          <div class="mp-settings-field">
            <div class="mp-settings-field-head">
              <div class="mp-proxy-pool-label">
                <span class="mp-pp-icon" v-html="icons.globe" />
                <span>英文别名</span>
              </div>
              <small class="text-muted">网关模型前缀，如 {{ channelSettingsDraft.alias || "alias" }}/model</small>
            </div>
            <input
              v-model="channelSettingsDraft.alias"
              type="text"
              class="mp-settings-input"
              :class="{ 'has-error': channelSettingsError }"
              placeholder="仅限英文、数字、- 与 _"
              :disabled="channelSettingsTargetIsBuiltin"
              title="固化渠道的英文别名固定为 opencode，不可修改"
              @input="channelSettingsError = validateAlias(channelSettingsDraft.alias, channelSettingsTarget?.id)"
            />
            <p v-if="channelSettingsError" class="mp-settings-error">{{ channelSettingsError }}</p>
            <p v-else-if="channelSettingsTargetIsBuiltin" class="mp-settings-hint">固化渠道的别名固定为 opencode（网关模型前缀依赖它），不可修改</p>
            <p v-else class="mp-settings-hint">所有渠道别名不能重复（含 opencode）</p>
          </div>

          <!-- 代理设置（四模式合一，合并旧「内部代理池轮询 / 代理池固定通道」双开关） -->
          <div class="mp-proxy-pool-box">
            <div class="mp-proxy-pool-row">
              <div class="mp-proxy-pool-label">
                <span class="mp-pp-icon" v-html="icons.shield" />
                <span>代理设置</span>
              </div>
              <select
                v-model="channelSettingsDraft.proxyMode"
                class="mp-settings-input"
                title="渠道出网代理策略，默认强制直连"
              >
                <option value="direct">强制直连（默认）</option>
                <option value="pool">代理池（轮询 + 失败切换）</option>
                <option value="fixed_channel">固定通道（代理池）</option>
                <option value="custom_node">自定义节点（代理池）</option>
              </select>
            </div>

            <div v-if="channelSettingsDraft.proxyMode === 'direct'" class="mp-proxy-pool-status is-inactive">
              <span>不走任何代理，直连上游通道（默认）</span>
            </div>
            <div v-else-if="channelSettingsDraft.proxyMode === 'pool'" class="mp-proxy-pool-status is-active">
              <span class="mp-status-dot-sm" />
              <span>优先直连，报错自动按速度切换至代理池 <strong>≤ 1000ms</strong> 节点（粘性保持）</span>
            </div>
            <div v-else-if="channelSettingsDraft.proxyMode === 'fixed_channel'" class="mp-proxy-pool-status is-active">
              <span class="mp-status-dot-sm" />
              <span>固定经代理池通道出口转发；可在「管理可用模型 → Key 分组」为不同 Key 绑定不同通道</span>
            </div>
            <div v-else class="mp-proxy-pool-status is-active">
              <span class="mp-status-dot-sm" />
              <span>恒定使用所选单一节点出口（不直连、不轮换）</span>
            </div>

            <!-- 固定通道：选择渠道默认通道（Key 可在 Key 分组中按 Key 覆盖） -->
            <div v-if="channelSettingsDraft.proxyMode === 'fixed_channel'" class="mp-proxy-pool-row" style="margin-top: 10px;">
              <div class="mp-proxy-pool-label">
                <span class="mp-pp-icon" v-html="icons.globe" />
                <span>默认固定通道</span>
              </div>
              <select
                v-model="channelSettingsDraft.proxyFixedChannel"
                class="mp-settings-input"
                title="未单独绑定通道的 Key 使用该通道"
              >
                <option value="">请选择通道</option>
                <option
                  v-if="isDeletedPoolChannelRef(channelSettingsDraft.proxyFixedChannel)"
                  :value="channelSettingsDraft.proxyFixedChannel"
                >
                  原通道已删除（请重新选择）
                </option>
                <option v-for="pc in proxyPoolChannelOptions" :key="pc.id" :value="pc.id">
                  {{ pc.name }}
                </option>
              </select>
            </div>

            <!-- 自定义节点：选择代理池单一节点（参照固定通道设置的候选口径） -->
            <div v-if="channelSettingsDraft.proxyMode === 'custom_node'" class="mp-proxy-pool-row" style="margin-top: 10px;">
              <div class="mp-proxy-pool-label">
                <span class="mp-pp-icon" v-html="icons.activity" />
                <span>固定出口节点</span>
              </div>
              <select
                v-model="channelSettingsDraft.fixedProxyNode"
                class="mp-settings-input"
                title="恒定使用该节点出网；留空锁定池内首个启用节点"
              >
                <option value="">请选择节点</option>
                <option v-for="pn in proxyPoolNodeOptions" :key="pn.id" :value="pn.id">
                  {{ pn.name }}{{ pn.latencyMs != null ? ` · ${pn.latencyMs}ms` : "" }}
                </option>
              </select>
            </div>
          </div>
        </div>

        <div class="mp-modal-footer">
          <div class="mp-modal-footer-hint text-muted text-xs">
            <span>💡 遇到上游频次限制或连接错误时，代理池轮询渠道会自动切换出口节点重试；固定通道始终锁定同一节点</span>
          </div>
          <div class="mp-modal-footer-buttons">
            <button
              type="button"
              class="mp-btn mp-btn-ghost"
              @click="closeChannelSettingsDialog"
            >
              取消
            </button>
            <button
              type="button"
              class="mp-btn mp-btn-primary"
              :disabled="savingConfig"
              title="保存渠道设置"
              @click="saveChannelSettings"
            >
              <span v-html="icons.check" />
              <span>{{ savingConfig ? "保存中…" : "保存设置" }}</span>
            </button>
          </div>
        </div>
      </div>
    </div>

    <!-- 弹窗: 删除反代渠道确认 (Delete Channel Confirmation Modal) -->
    <div
      v-if="deleteChannelModalOpen && deletingChannel"
      class="mp-modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-labelledby="mp-delete-channel-title"
     
    >
      <div class="mp-modal-box mp-modal-box-sm">
        <div class="mp-modal-header">
          <div class="mp-modal-title-group">
            <div class="mp-modal-badge-icon is-error">
              <span v-html="icons.trash" />
            </div>
            <div>
              <h3 id="mp-delete-channel-title">删除反代渠道</h3>
              <small class="text-muted">移除该渠道及对外暴露的模型路由</small>
            </div>
          </div>
          <button
            type="button"
            class="mp-modal-close"
            title="关闭弹窗 (Esc)"
            @click="closeDeleteChannelModal"
          >
            <span v-html="icons.close" />
          </button>
        </div>

        <div class="mp-modal-body">
          <p class="text-sm">
            确定要删除反代渠道<strong>「{{ deletingChannel.name }}」</strong>（别名 <code>{{ channelAlias(deletingChannel) }}</code>）吗？
          </p>
          <p v-if="deletingChannel.siteId" class="text-xs text-muted" style="margin-top: 8px;">
            💡 该渠道是由站点库转换生成的，删除后不会影响站点库中的站点记录，您后续可随时通过「站点转换」重新添加。
          </p>
        </div>

        <div class="mp-modal-footer">
          <div class="mp-modal-footer-buttons">
            <button
              type="button"
              class="mp-btn mp-btn-ghost"
              @click="closeDeleteChannelModal"
            >
              取消
            </button>
            <button
              type="button"
              class="mp-btn mp-btn-danger"
              :disabled="savingConfig"
              @click="confirmDeleteChannel"
            >
              <span v-html="icons.trash" />
              <span>{{ savingConfig ? "正在删除…" : "确认删除" }}</span>
            </button>
          </div>
        </div>
      </div>
    </div>

    <!-- 弹窗 4: 站点转换 - 从站点库「在用且存活」站点创建反代渠道 (Site Convert Modal) -->
    <div
      v-if="siteConvertDialogOpen"
      class="mp-modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-labelledby="mp-site-convert-title"
     
    >
      <div class="mp-modal-box mp-modal-box-wide">
        <div class="mp-modal-header">
          <div class="mp-modal-title-group">
            <div class="mp-modal-badge-icon">
              <span v-html="icons.globe" />
            </div>
            <div>
              <div class="mp-modal-title-wrap">
                <h3 id="mp-site-convert-title">站点转换</h3>
                <span class="mp-header-chip">{{ convertibleSites.length }} 个可用站点</span>
              </div>
              <small class="text-muted">从站点库「在用且存活」的站点创建反代渠道 · 所有渠道英文别名不能重复（含 opencode）</small>
            </div>
          </div>
          <button
            type="button"
            class="mp-modal-close"
            title="关闭弹窗 (Esc)"
            @click="closeSiteConvertDialog"
          >
            <span v-html="icons.close" />
          </button>
        </div>

        <div class="mp-modal-body">
          <!-- 站点搜索框 -->
          <div class="mp-search-box">
            <span class="mp-search-icon" v-html="icons.search" />
            <input
              v-model="convertSiteSearch"
              type="search"
              placeholder="搜索站点名称或 API 地址…"
            />
          </div>

          <!-- 站点列表：图标 + 名称/主机 + Key·模型·账号 统计徽标 -->
          <div class="mp-site-list">
            <button
              v-for="site in filteredConvertibleSites"
              :key="site.id"
              type="button"
              class="mp-site-item"
              :class="{ 'is-selected': convertSelectedSite?.id === site.id }"
              @click="selectConvertSite(site)"
            >
              <img v-if="site.icon" class="mp-site-item-icon" :src="site.icon" alt="" />
              <span v-else class="mp-site-item-icon mp-site-item-icon-fallback">{{
                site.name.slice(0, 1)
              }}</span>
              <span class="mp-site-item-main">
                <span class="mp-site-item-name">{{ site.name }}</span>
                <span class="mp-site-item-url font-mono">{{ formatUpstreamUrl(site.apiBaseUrl) }}</span>
              </span>
              <span class="mp-site-item-stats font-mono">
                <span
                  class="mp-sis-badge"
                  :class="{ 'is-empty': (siteCacheSummary(site.id)?.keyCount ?? 0) === 0 }"
                  :title="`从站点缓存继承 ${siteCacheSummary(site.id)?.keyCount ?? 0} 个 Key`"
                >Key {{ siteCacheSummary(site.id)?.keyCount ?? 0 }}</span>
                <span
                  class="mp-sis-badge"
                  :class="{ 'is-empty': (siteCacheSummary(site.id)?.modelCount ?? 0) === 0 }"
                  :title="`已同步 ${siteCacheSummary(site.id)?.modelCount ?? 0} 个模型`"
                >模型 {{ siteCacheSummary(site.id)?.modelCount ?? 0 }}</span>
                <span
                  v-if="(siteCacheSummary(site.id)?.accountCount ?? 0) > 1"
                  class="mp-sis-badge"
                  :title="`覆盖 ${siteCacheSummary(site.id)?.accountCount} 个浏览器账号`"
                >{{ siteCacheSummary(site.id)?.accountCount }} 账号</span>
              </span>
            </button>
            <div v-if="filteredConvertibleSites.length === 0" class="mp-empty-box">
              <div class="mp-empty-icon" v-html="icons.globe" />
              <p v-if="convertSiteSearch">未检索到匹配的站点</p>
              <p v-else>暂无「在用且存活」的站点可转换</p>
            </div>
          </div>

          <!-- 转换配置 -->
          <div v-if="convertSelectedSite" class="mp-convert-config">
            <div class="mp-settings-field">
              <div class="mp-settings-field-head">
                <div class="mp-proxy-pool-label">
                  <span class="mp-pp-icon" v-html="icons.globe" />
                  <span>英文别名（唯一）</span>
                </div>
                <small class="text-muted">网关模型前缀，如 {{ convertAlias || "alias" }}/model</small>
              </div>
              <input
                v-model="convertAlias"
                type="text"
                class="mp-settings-input"
                :class="{ 'has-error': convertAliasError }"
                placeholder="仅限英文、数字、- 与 _"
              />
              <p v-if="convertAliasError" class="mp-settings-error">{{ convertAliasError }}</p>
            </div>
            <div class="mp-settings-field">
              <div class="mp-settings-field-head">
                <span>API 地址</span>
              </div>
              <input
                v-model="convertApiBaseUrl"
                type="text"
                class="mp-settings-input"
                placeholder="https://example.com/v1"
              />
            </div>

            <!-- 站点凭证由网关运行时按 siteId 读取，不复制到渠道配置 -->
            <div class="mp-settings-field">
              <div class="mp-settings-field-head">
                <span>站点凭证</span>
                <small v-if="convertModelLoading" class="text-muted">正在读取站点模型…</small>
              </div>
              <p class="mp-settings-hint">
                该渠道只保存站点关联关系；请求和模型拉取时会直接使用站点当前同步的 Key，不在反代配置中保存副本。
                <span v-if="convertModelLoading">正在读取站点缓存…</span>
                <template v-else>
                  <span v-if="convertSiteModelCount > 0">当前已同步 {{ convertSiteModelCount }} 个模型。</span>
                  <span
                    v-if="(siteCacheSummary(convertSelectedSite.id)?.keyCount ?? 0) > 0"
                  >将继承 {{ siteCacheSummary(convertSelectedSite.id)?.keyCount }} 个 Key（多 Key 自动轮换）。</span>
                  <span v-else class="mp-convert-key-missing">⚠️ 该站点暂无已同步的 Key，转换后需先在站点库同步 Key 才能调用。</span>
                </template>
              </p>
            </div>
          </div>
        </div>

        <div class="mp-modal-footer">
          <div class="mp-modal-footer-hint text-muted text-xs">
            <span v-if="convertSelectedSite">💡 转换后可在「管理模型」中勾选该渠道对外暴露的模型</span>
            <span v-else>💡 选择上方一个在用且存活的站点开始转换</span>
          </div>
          <div class="mp-modal-footer-buttons">
            <button
              type="button"
              class="mp-btn mp-btn-ghost"
              @click="closeSiteConvertDialog"
            >
              取消
            </button>
            <button
              type="button"
              class="mp-btn mp-btn-primary"
              :disabled="savingConfig || !convertSelectedSite"
              title="将该站点转换为一个反代渠道"
              @click="confirmConvertSite"
            >
              <span v-html="icons.plus" />
              <span>{{ savingConfig ? "转换中…" : "转换为渠道" }}</span>
            </button>
          </div>
        </div>
      </div>
    </div>

    <!-- 数据库日志清理选项弹窗 (Clear Logs Options Modal) -->
    <div
      v-if="clearLogsModalOpen"
      class="mp-modal-backdrop mp-sub-backdrop"
      role="dialog"
      aria-modal="true"
      aria-labelledby="mp-clear-modal-title"
     
    >
      <div class="mp-modal-box mp-modal-box-sm mp-clear-logs-box">
        <div class="mp-modal-header">
          <div class="mp-card-title-group">
            <span class="mp-modal-icon text-danger" v-html="icons.trash" />
            <h3 id="mp-clear-modal-title">清理数据库日志</h3>
          </div>
          <button
            type="button"
            class="mp-modal-close"
            title="关闭 (Esc)"
            @click="clearLogsModalOpen = false"
          >
            <span v-html="icons.close" />
          </button>
        </div>

        <div class="mp-modal-body">
          <p class="mp-clear-modal-desc">
            请选择要执行的本地 SQLite 数据库日志清理模式：
          </p>

          <!-- 范围清理：可选日期，仅清理该日期之前的明细 -->
          <div class="mp-field" style="margin-bottom: 14px;">
            <label for="mp-clear-before">清理范围（可选）</label>
            <input
              id="mp-clear-before"
              v-model="clearBeforeDate"
              type="date"
              class="mp-input"
            />
            <small>填写日期时只清理该日期之前（不含当日）的明细；留空则作用于全部明细。渠道统计、全渠道总览与 Token 统计中心的反代模式报表均来自独立聚合表，<strong>不受清理影响</strong>。</small>
          </div>

          <div class="mp-clear-options-grid">
            <!-- 选项 1: 仅清空请求/响应报文 -->
            <div class="mp-clear-option-card">
              <div class="mp-coc-head">
                <div class="mp-coc-title-wrap">
                  <span class="mp-coc-icon">📝</span>
                  <div>
                    <strong>清理请求与响应详细内容</strong>
                    <span class="mp-coc-badge">推荐（大幅节省磁盘空间）</span>
                  </div>
                </div>
              </div>
              <p class="mp-coc-desc">
                仅清空日志中保存的客户端请求全文与服务端响应内容（释放大体积存储），<strong>保留</strong>历史调用时间、状态码、模型、节点、耗时与 Token 统计等列表索引。
              </p>
              <div class="mp-coc-action">
                <button
                  type="button"
                  class="mp-btn mp-btn-ghost mp-btn-sm"
                  :disabled="clearingLogs"
                  @click="handleClearLogs('payload_only')"
                >
                  <span>{{ clearingLogs ? "清理中…" : "仅清理报文全文" }}</span>
                </button>
              </div>
            </div>

            <!-- 选项 2: 完全清空所有记录 -->
            <div class="mp-clear-option-card is-danger">
              <div class="mp-coc-head">
                <div class="mp-coc-title-wrap">
                  <span class="mp-coc-icon text-danger">🗑️</span>
                  <div>
                    <strong class="text-danger">删除明细记录</strong>
                    <span class="mp-coc-badge is-danger">{{ clearBeforeDate ? '按日期删除' : '全部删除' }}</span>
                  </div>
                </div>
              </div>
              <p class="mp-coc-desc">
                {{ clearBeforeDate
                  ? `永久删除 ${clearBeforeDate} 之前的反代请求明细日志。`
                  : '从本地 SQLite 数据库中删除所有反代请求明细日志。' }}
                渠道统计与长期聚合数据持久保存在统计表中，<strong>不会被清空</strong>；运行时计数器不受影响。
              </p>
              <div class="mp-coc-action">
                <button
                  type="button"
                  class="mp-btn mp-btn-danger mp-btn-sm"
                  :disabled="clearingLogs"
                  @click="handleClearLogs('all')"
                >
                  <span>{{ clearingLogs ? "清理中…" : (clearBeforeDate ? "删除该日期前明细" : "删除全部明细") }}</span>
                </button>
              </div>
            </div>
          </div>
        </div>

        <div class="mp-modal-footer">
          <button
            type="button"
            class="mp-btn mp-btn-ghost"
            @click="clearLogsModalOpen = false"
          >
            取消
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.mp-page {
  padding: 16px 20px 40px;
  width: 100%;
  max-width: 100%;
  box-sizing: border-box;
  margin: 0 auto;
  display: flex;
  flex-direction: column;
  gap: 16px;
  min-width: 0;
  flex: 1 1 auto;
  min-height: 0;
}

/* 顶栏（与其他页面统一的驾驶舱横条） */
.mp-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 16px;
  width: 100%;
  box-sizing: border-box;
  flex-wrap: wrap;
  margin: -16px -20px 0;
  padding: 12px 20px 14px;
  background: var(--surface);
  border-bottom: 1px solid var(--line);
}

/* 左侧三行：眉标 / 标题 / 副标题 */
.mp-brand-section {
  display: flex;
  flex-direction: column;
  gap: 1px;
  min-width: 0;
}

.mp-eyebrow-row {
  display: flex;
  align-items: center;
  gap: 6px;
}

.mp-live-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: #10b981;
  box-shadow: 0 0 8px #10b981;
  animation: mp-pulse 2s infinite ease-in-out;
}

.mp-live-dot.is-off {
  background: var(--muted);
  box-shadow: none;
  animation: none;
}

.mp-eyebrow-text {
  font-size: 10px;
  font-weight: 750;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--brand);
}

.mp-header-title-row {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

.mp-header h1 {
  font-size: 18px;
  font-weight: 750;
  color: var(--text);
  margin: 0;
  line-height: 1.2;
  letter-spacing: -0.01em;
}

.mp-subtitle {
  margin: 0;
  font-size: 11px;
  color: var(--muted);
}

.mp-subtitle strong {
  color: var(--text);
  font-weight: 600;
}

.mp-status-pill {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  font-weight: 600;
  padding: 3px 10px;
  border-radius: 999px;
  background: var(--surface-soft);
  color: var(--muted);
  border: 1px solid var(--line);
}

.mp-status-pill.active {
  background: var(--brand-soft);
  color: var(--brand-deep);
  border-color: color-mix(in srgb, var(--brand) 40%, transparent);
}

.mp-status-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--muted);
}

.mp-status-pill.active .mp-status-dot {
  background: var(--brand);
  box-shadow: 0 0 8px var(--brand);
  animation: mp-pulse 2s infinite;
}

.mp-channel-pill {
  font-size: 11.5px;
  font-weight: 600;
  padding: 2px 8px;
  border-radius: var(--r-xs);
  background: var(--surface-soft);
  color: var(--brand-deep);
  border: 1px solid color-mix(in srgb, var(--brand) 30%, transparent);
}

@keyframes mp-pulse {
  0%, 100% { opacity: 1; transform: scale(1); }
  50% { opacity: 0.5; transform: scale(1.15); }
}

.mp-header-actions {
  display: flex;
  align-items: center;
  gap: 10px;
}

/* 头部按钮与其他页面驾驶舱横条对齐（32px 高度） */
.mp-header-actions .mp-btn {
  height: 32px;
  padding: 0 12px;
  font-size: 12px;
}

/* 按钮规范 */
.mp-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 36px;
  padding: 0 14px;
  border-radius: var(--r-md, 8px);
  font-size: 13px;
  font-weight: 600;
  cursor: pointer;
  border: 1px solid transparent;
  transition: all 0.18s var(--ease);
}

.mp-btn-sm {
  height: 30px;
  padding: 0 10px;
  font-size: 12px;
}

.mp-btn :deep(svg) {
  width: 15px;
  height: 15px;
}

.mp-btn-badge {
  font-size: 11px;
  padding: 1px 6px;
  border-radius: 999px;
  background: var(--brand-soft);
  color: var(--brand-deep);
  font-weight: 700;
  margin-left: 2px;
}

.mp-btn-primary {
  background: var(--brand);
  color: #fff;
  border-color: var(--brand);
}

.mp-btn-primary:hover {
  background: var(--brand-deep);
  transform: translateY(-1px);
}

.mp-btn-danger {
  background: var(--danger);
  color: #fff;
  border-color: var(--danger);
}

.mp-btn-danger:hover {
  opacity: 0.9;
}

.mp-btn-ghost {
  background: var(--surface);
  border-color: var(--line);
  color: var(--text);
}

.mp-btn-ghost:hover {
  background: var(--surface-hover);
  border-color: var(--line-strong);
}

/* 一级页面 Tab 切换导航条 */
.mp-main-tab-nav {
  display: flex;
  align-items: center;
  gap: 8px;
  background: var(--surface);
  padding: 5px;
  border-radius: var(--r-lg, 12px);
  border: 1px solid var(--line);
  box-shadow: 0 1px 4px rgba(0, 0, 0, 0.02);
  width: fit-content;
}

.mp-main-nav-btn {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 8px 16px;
  border-radius: var(--r-md, 8px);
  border: none;
  background: transparent;
  color: var(--muted);
  font-size: 13.5px;
  font-weight: 650;
  cursor: pointer;
  transition: all 0.18s ease;
}

.mp-main-nav-btn :deep(svg) {
  width: 16px;
  height: 16px;
}

.mp-main-nav-btn:hover {
  color: var(--text);
  background: var(--surface-hover);
}

.mp-main-nav-btn.active {
  background: var(--brand);
  color: #fff;
  box-shadow: 0 2px 8px color-mix(in srgb, var(--brand) 35%, transparent);
}

.mp-tab-count-pill {
  font-size: 11px;
  font-weight: 700;
  padding: 1px 7px;
  border-radius: 999px;
  background: var(--surface-soft);
  color: var(--text);
  border: 1px solid var(--line);
}

.mp-main-nav-btn.active .mp-tab-count-pill {
  background: rgba(255, 255, 255, 0.25);
  color: #fff;
  border-color: transparent;
}

.mp-tab-count-pill.is-err {
  background: color-mix(in srgb, var(--danger) 15%, transparent);
  color: var(--danger);
  border-color: color-mix(in srgb, var(--danger) 30%, transparent);
}

.mp-main-nav-btn.active .mp-tab-count-pill.is-err {
  background: var(--danger);
  color: #fff;
  border-color: transparent;
}

.mp-tab-pane {
  display: flex;
  flex-direction: column;
  gap: 16px;
  width: 100%;
  max-width: 100%;
  box-sizing: border-box;
  min-width: 0;
  animation: fadeInTab 0.18s ease;
}

@keyframes fadeInTab {
  from { opacity: 0; transform: translateY(3px); }
  to { opacity: 1; transform: translateY(0); }
}

/* 日志页视图：在 header + 主 tab 条之下撑满剩余高度，表格区自适应、分页条固定在底部可见 */
.mp-logs-page-view {
  flex: 1 1 auto;
  min-height: 0;
  overflow: hidden;
  /* .mp-page 底边距 40px，这里收回 24px，给分页条留 16px 呼吸空间 */
  margin-bottom: -24px;
}

.mp-logs-main-card .app-table-pagination {
  flex-shrink: 0;
}

/* 请求日志全屏视图样式 */
.mp-card.mp-logs-main-card {
  padding: 0;
  gap: 0;
  overflow: hidden;
  width: 100%;
  max-width: 100%;
  box-sizing: border-box;
  display: flex;
  flex-direction: column;
  flex: 1 1 auto;
  min-height: 0;
}

.mp-logs-main-card .mp-logs-toolbar {
  padding: 16px 18px 12px;
  width: 100%;
  box-sizing: border-box;
  flex-shrink: 0;
}

.mp-logs-main-card .mp-logs-table-wrap {
  border-top: 1px solid var(--line);
  flex: 1 1 auto;
  min-height: 0;
  max-height: none;
  overflow-y: auto;
  overflow-x: auto;
  width: 100%;
  max-width: 100%;
  box-sizing: border-box;
}

.mp-btn-text {
  background: transparent;
  border: 1px solid transparent;
  color: var(--brand);
  font-size: 11.5px;
  font-weight: 600;
  padding: 2px 8px;
  border-radius: 4px;
  cursor: pointer;
  transition: all 0.15s ease;
}

.mp-btn-text:hover {
  background: var(--brand-soft);
  border-color: var(--brand);
}

/* 卡片基类 */
.mp-card {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 12px);
  padding: 20px;
  display: flex;
  flex-direction: column;
  gap: 16px;
  box-shadow: 0 2px 8px rgba(16, 35, 25, 0.04);
}

.mp-card-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
}

.mp-card-title-group {
  display: flex;
  align-items: center;
  gap: 8px;
}

.mp-card-icon {
  width: 20px;
  height: 20px;
  color: var(--brand);
  display: inline-flex;
}

.mp-card-icon :deep(svg) {
  width: 100%;
  height: 100%;
}

.mp-card-header h2 {
  font-size: 15px;
  font-weight: 750;
  color: var(--text);
  margin: 0;
}

/* 指标卡片 */
/* ==========================================================================
   控制台重构样式 (mp-console-hub / 端点鉴权卡 / KPI 4宫格 / Token深度洞察)
   ========================================================================== */
.mp-console-hub {
  display: flex;
  flex-direction: column;
  gap: 10px;
  min-width: 0;
  max-width: 100%;
}

.mp-console-hub .mp-card {
  padding: 13px 14px;
  gap: 10px;
}

/* 1. 网关连接端点卡 */
.mp-endpoints-card {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.mp-endpoints-summary-chips {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.mp-ep-chip {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  background: var(--surface-soft);
  border: 1px solid var(--line);
  padding: 3px 8px;
  border-radius: 999px;
  font-size: 11.5px;
}

.mp-endpoint-rows {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

/* 四协议端点双列网格：短 URL 不再独占整行留白 */
.mp-epr-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 8px;
  min-width: 0;
}

/* 合并行：Base URL 与 API Key 并排，中缝分隔 */
.mp-epr-merged {
  display: flex;
  align-items: stretch;
  padding: 0;
  gap: 0;
}

.mp-epr-half {
  flex: 1;
  min-width: 0;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 9px;
}

.mp-epr-half.is-gw,
.mp-epr-half.is-key {
  flex-direction: column;
  align-items: stretch;
  gap: 4px;
}

.mp-epr-half.is-key {
  border-left: 1px solid var(--line);
}

.mp-epr-key-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  min-width: 0;
}

.mp-epr-key-line {
  display: flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
}

.mp-epr-key-line .mp-epr-code {
  flex: 1;
}

@media (max-width: 1080px) {
  .mp-epr-grid {
    grid-template-columns: 1fr;
  }

  .mp-epr-merged {
    flex-direction: column;
  }

  .mp-epr-half.is-key {
    border-left: none;
    border-top: 1px solid var(--line);
  }
}

.mp-endpoint-row.mp-epr-cell {
  padding: 6px 9px;
  gap: 8px;
}

.mp-epr-cell .mp-epr-label {
  min-width: 0;
}

.mp-epr-cell .mp-epr-code {
  font-size: 12px;
}

/* Gemini 合并行：双版本并排 */
.mp-gemini-dual {
  display: flex;
  align-items: center;
  gap: 10px;
  flex: 1;
  min-width: 0;
}

.mp-gemini-item {
  display: flex;
  align-items: center;
  gap: 6px;
  flex: 1;
  min-width: 0;
}

.mp-gemini-item .mp-epr-code {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.mp-gemini-tag {
  font-size: 10px;
  font-weight: 600;
  color: var(--muted);
  background: var(--bg-soft);
  padding: 1px 5px;
  border-radius: 3px;
  white-space: nowrap;
  flex-shrink: 0;
}

@media (max-width: 1080px) {
  .mp-epr-grid {
    grid-template-columns: 1fr;
  }
}

.mp-endpoint-row {
  display: flex;
  align-items: center;
  gap: 12px;
  background: var(--surface-soft);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  padding: 8px 12px;
  min-width: 0;
  transition: border-color 0.15s ease;
}

.mp-endpoint-row:hover {
  border-color: color-mix(in srgb, var(--brand) 40%, var(--line));
}

.mp-epr-label {
  display: flex;
  align-items: center;
  gap: 6px;
  min-width: 130px;
  font-size: 12px;
  font-weight: 700;
  color: var(--text);
  flex-shrink: 0;
}

.mp-proto-badge {
  font-size: 10.5px;
  font-weight: 700;
  padding: 1px 6px;
  border-radius: 4px;
  white-space: nowrap;
}

.mp-proto-badge.is-openai {
  background: rgba(16, 185, 129, 0.12);
  color: #10b981;
}

.mp-proto-badge.is-claude {
  background: rgba(217, 119, 6, 0.12);
  color: #d97706;
}

.mp-proto-badge.is-gemini {
  background: rgba(59, 130, 246, 0.12);
  color: #3b82f6;
}

.mp-proto-badge.is-key {
  background: rgba(139, 92, 246, 0.12);
  color: #8b5cf6;
}

.mp-epr-code {
  flex: 1;
  font-size: 12.5px;
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  min-width: 0;
  background: transparent;
  padding: 0;
}

.mp-epr-btns {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-shrink: 0;
}

/* 密钥行鉴权状态徽标：填充长行留白并即时反映鉴权模式 */
.mp-epr-key-state {
  flex-shrink: 0;
  font-size: 10.5px;
  font-weight: 700;
  padding: 2px 8px;
  border-radius: 999px;
}

.mp-epr-key-state.is-on {
  color: var(--brand);
  background: color-mix(in srgb, var(--brand) 12%, transparent);
  border: 1px solid color-mix(in srgb, var(--brand) 30%, transparent);
}

.mp-epr-key-state.is-off {
  color: var(--muted);
  background: var(--surface-soft);
  border: 1px solid var(--line);
}

/* 全渠道数据总览卡：卡片头（标题 + 范围徽章 + 时间下拉）+ KPI 四宫格同容器 */
.mp-overview-card {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

/* 统计范围徽章：明示 KPI 数字的统计口径 */
.mp-overview-scope {
  padding: 2px 9px;
  border-radius: 999px;
  background: var(--brand-soft);
  color: var(--brand-deep);
  font-size: 11px;
  font-weight: 700;
  white-space: nowrap;
}

/* 嵌入总览卡后的 KPI 砖块改用页面底色，与卡片表面区分层次 */
.mp-overview-card .mp-kpi-card {
  background: var(--page-bg);
}

/* 趋势卡双徽章：区间统计与本次运行计数分列，避免口径混淆 */
.mp-trend-badges {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

/* 2. 关键性能 KPI 仪表盘 (4 宫格) */
.mp-kpi-matrix-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
  gap: 12px;
  min-width: 0;
}

.mp-kpi-card {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 12px);
  padding: 10px 12px;
  display: flex;
  flex-direction: column;
  gap: 5px;
  transition: all 0.2s var(--ease);
  box-shadow: 0 2px 8px rgba(16, 35, 25, 0.02);
  min-width: 0;
}

.mp-kpi-card:hover {
  border-color: color-mix(in srgb, var(--brand) 40%, var(--line));
  transform: translateY(-2px);
  box-shadow: 0 6px 16px color-mix(in srgb, var(--brand) 8%, transparent);
}

.mp-kpi-top {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}

.mp-kpi-label {
  font-size: 12px;
  font-weight: 650;
  color: var(--muted);
  white-space: nowrap;
}

.mp-kpi-badge {
  font-size: 10.5px;
  font-weight: 700;
  padding: 1px 6px;
  border-radius: 999px;
  white-space: nowrap;
}

.mp-kpi-badge.is-good {
  background: rgba(16, 185, 129, 0.12);
  color: #10b981;
}

.mp-kpi-badge.is-warn {
  background: rgba(245, 158, 11, 0.12);
  color: #f59e0b;
}

.mp-kpi-badge.is-brand {
  background: var(--brand-soft);
  color: var(--brand-deep);
}

.mp-kpi-badge.is-hit {
  background: rgba(245, 158, 11, 0.12);
  color: #f59e0b;
}

.mp-kpi-main {
  display: flex;
  align-items: baseline;
  gap: 6px;
}

.mp-kpi-number {
  font-size: 21px;
  font-weight: 800;
  color: var(--text);
  line-height: 1.1;
  letter-spacing: -0.02em;
}

.mp-kpi-unit {
  font-size: 11px;
  color: var(--muted);
  font-weight: 600;
}

.mp-kpi-footer {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 11px;
  color: var(--muted);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.mp-kpi-sep {
  opacity: 0.4;
}

/* 3. Token 深度洞察卡 */
/* Token 深度洞察精简行 */
/* —— 全渠道趋势图表卡 —— */
.mp-trend-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 10px;
  min-width: 0;
}

.mp-trend-box {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-sm, 6px);
  padding: 6px 8px 4px;
  min-width: 0;
}

.mp-trend-title {
  font-size: 11px;
  font-weight: 700;
  color: var(--muted);
  margin-bottom: 2px;
}

.mp-trend-empty {
  border: 1px dashed var(--line);
  border-radius: var(--r-sm, 6px);
  padding: 16px 12px;
  text-align: center;
  font-size: 12px;
  color: var(--muted);
}

@media (max-width: 1080px) {
  .mp-trend-grid {
    grid-template-columns: 1fr;
  }
}

/* 兼容弹窗内部旧通用样式 */
.mp-metrics-row {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 10px;
}

.mp-metric-box {
  background: var(--surface-soft);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 10px 12px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.mp-metric-box label {
  font-size: 11px;
  color: var(--muted);
}

.mp-metric-box strong {
  font-size: 16px;
  font-weight: 750;
  color: var(--text);
}

/* 端点展示条目 */
.mp-endpoint-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.mp-endpoint-item {
  display: flex;
  align-items: center;
  gap: 10px;
  background: var(--surface-soft);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 8px 12px;
}

.mp-ep-label {
  font-size: 12px;
  font-weight: 700;
  color: var(--muted);
  min-width: 68px;
}

.mp-ep-code {
  flex: 1;
  font-family: var(--font-mono, monospace);
  font-size: 12.5px;
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.mp-ep-btns {
  display: flex;
  align-items: center;
  gap: 6px;
}

.mp-action-btn {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  height: 26px;
  padding: 0 8px;
  border-radius: var(--r-xs);
  background: var(--surface);
  border: 1px solid var(--line);
  color: var(--muted);
  font-size: 11.5px;
  cursor: pointer;
  transition: all 0.15s ease;
}

.mp-action-btn :deep(svg) {
  width: 12px;
  height: 12px;
}

.mp-action-btn:hover {
  background: var(--surface-hover);
  color: var(--text);
  border-color: var(--line-strong);
}

.mp-action-btn.is-danger {
  color: var(--color-danger, #ef4444);
}

.mp-action-btn.is-danger:hover {
  background: rgba(239, 68, 68, 0.1);
  border-color: var(--color-danger, #ef4444);
  color: var(--color-danger, #ef4444);
}

/* 已配置模型白名单的渠道：管理模型按钮高亮提示 */
.mp-action-btn.is-active {
  color: var(--brand-deep);
  border-color: var(--brand);
  background: var(--brand-soft);
}

.mp-btn-icon-only {
  width: 26px;
  padding: 0;
  justify-content: center;
}

/* 渠道分区标题 */
.mp-section-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-top: 6px;
}

.mp-section-head h2 {
  font-size: 15px;
  font-weight: 750;
  color: var(--text);
  margin: 0;
}

/* 渠道卡片网格 (三列卡片布局) */
.mp-channels-grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 16px;
}

@media (max-width: 1100px) {
  .mp-channels-grid {
    grid-template-columns: repeat(2, 1fr);
  }
}

@media (max-width: 720px) {
  .mp-channels-grid {
    grid-template-columns: 1fr;
  }
}

.mp-channel-card {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 12px);
  padding: 18px;
  display: flex;
  flex-direction: column;
  gap: 14px;
  transition: all 0.2s ease;
  box-shadow: 0 2px 8px rgba(16, 35, 25, 0.04);
}

.mp-channel-card:hover {
  border-color: color-mix(in srgb, var(--brand) 40%, transparent);
  box-shadow: 0 4px 16px rgba(16, 35, 25, 0.08);
}

.mp-channel-card.is-disabled {
  opacity: 0.65;
  background: var(--surface-soft);
}

.mp-channel-card-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.mp-channel-card-title {
  display: flex;
  align-items: center;
  gap: 10px;
}

.mp-channel-badge-icon {
  width: 32px;
  height: 32px;
  border-radius: var(--r-md);
  background: var(--brand-soft);
  color: var(--brand);
  display: flex;
  align-items: center;
  justify-content: center;
}

.mp-channel-badge-icon :deep(svg) {
  width: 18px;
  height: 18px;
}

.mp-channel-card-title h3 {
  font-size: 15px;
  font-weight: 750;
  color: var(--text);
  margin: 0;
}

/* 标题后括号内的英文别名：弱化展示，不喧宾夺主 */
.mp-title-alias {
  font-size: 12px;
  font-weight: 500;
  color: var(--text-muted);
}

.mp-proto-tag {
  font-size: 10.5px;
  font-weight: 600;
  color: var(--brand-deep);
  background: var(--brand-soft);
  padding: 1px 6px;
  border-radius: 4px;
  display: inline-block;
}

/* 渠道卡片标题标签行 */
.mp-card-tags {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.mp-alias-tag {
  font-size: 10.5px;
  font-weight: 700;
  font-family: var(--font-mono, monospace);
  color: var(--text);
  background: var(--surface-soft);
  border: 1px solid var(--line);
  padding: 1px 6px;
  border-radius: 4px;
  display: inline-block;
}

/* 站点转换渠道标记：与站点库原纪录关联 */
.mp-alias-tag.is-site {
  color: var(--brand-deep);
  background: var(--brand-soft);
  border-color: var(--brand);
}

/* 内置固化渠道标记：官方维护、别名固定 */
.mp-alias-tag.is-builtin {
  color: #8a6d1a;
  background: rgba(212, 167, 44, 0.12);
  border-color: rgba(212, 167, 44, 0.45);
}

:global(:root[data-theme="dark"]) .mp-alias-tag.is-builtin {
  color: #e2c258;
  background: rgba(212, 167, 44, 0.16);
  border-color: rgba(212, 167, 44, 0.4);
}

/* 分区头部操作区 */
.mp-section-actions {
  display: flex;
  align-items: center;
  gap: 12px;
}

/* 设置弹窗表单字段 */
.mp-settings-field {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.mp-settings-field-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  font-size: 12px;
  font-weight: 650;
  color: var(--text);
}

.mp-settings-input {
  height: 36px;
  padding: 0 12px;
  border-radius: var(--r-md, 8px);
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--text);
  font-size: 13px;
  font-family: var(--font-mono, monospace);
  outline: none;
  transition: border-color 0.15s ease, box-shadow 0.15s ease;
}

.mp-settings-input:focus {
  border-color: var(--brand);
  box-shadow: 0 0 0 3px color-mix(in srgb, var(--brand) 15%, transparent);
}

.mp-settings-input.has-error {
  border-color: var(--danger, #e5484d);
}

.mp-settings-error {
  font-size: 11.5px;
  color: var(--danger, #e5484d);
  margin: 0;
}

.mp-settings-hint {
  font-size: 11.5px;
  color: var(--muted);
  margin: 0;
}

/* 站点转换：站点列表 */
.mp-site-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
  max-height: 260px;
  overflow-y: auto;
}

.mp-site-item {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 9px 12px;
  border-radius: var(--r-md, 8px);
  background: var(--surface-soft);
  border: 1px solid var(--line);
  cursor: pointer;
  transition: all 0.15s ease;
  text-align: left;
}

.mp-site-item:hover {
  border-color: var(--line-strong);
  background: var(--surface);
}

.mp-site-item.is-selected {
  border-color: var(--brand);
  background: var(--brand-soft);
}

.mp-site-item-icon {
  width: 26px;
  height: 26px;
  border-radius: 6px;
  object-fit: cover;
  flex-shrink: 0;
  background: var(--surface);
  border: 1px solid var(--line);
}

.mp-site-item-icon-fallback {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  font-size: 12px;
  font-weight: 700;
  color: var(--brand-deep);
}

.mp-site-item-main {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
  flex: 1;
}

.mp-site-item-name {
  font-size: 13px;
  font-weight: 650;
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.mp-site-item-url {
  font-size: 11px;
  color: var(--muted);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.mp-site-item-stats {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  flex-shrink: 0;
}

.mp-sis-badge {
  font-family: var(--font-mono, monospace);
  font-size: 10.5px;
  font-weight: 600;
  padding: 1px 7px;
  border-radius: 999px;
  background: var(--brand-soft);
  border: 1px solid color-mix(in srgb, var(--brand) 45%, transparent);
  color: var(--brand-deep);
  white-space: nowrap;
}

.mp-sis-badge.is-empty {
  background: var(--surface);
  border-color: var(--line);
  color: var(--muted);
}

/* 转换弹窗：站点无 Key 的警示文案 */
.mp-convert-key-missing {
  color: var(--warning, #d97706);
  font-weight: 600;
}

.mp-convert-config {
  display: flex;
  flex-direction: column;
  gap: 14px;
  padding-top: 4px;
}

/* 转换弹窗：继承的站点 Key 列表 */
.mp-convert-keys {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}

.mp-convert-key-chip {
  font-family: var(--font-mono, monospace);
  font-size: 12px;
  padding: 4px 10px;
  border-radius: 6px;
  background: var(--brand-soft);
  border: 1px solid var(--brand);
  color: var(--brand-deep);
}

/* 代理池配置块 */
.mp-proxy-pool-box {
  background: var(--surface-soft);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 12px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.mp-proxy-pool-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.mp-proxy-pool-label {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  font-weight: 650;
  color: var(--text);
}

.mp-pp-icon {
  width: 14px;
  height: 14px;
  color: var(--brand);
  display: inline-flex;
}

.mp-pp-icon :deep(svg) {
  width: 100%;
  height: 100%;
}

.mp-proxy-pool-status {
  font-size: 11.5px;
  line-height: 1.4;
  padding: 6px 8px;
  border-radius: var(--r-xs);
}

.mp-proxy-pool-status.is-active {
  background: var(--brand-soft);
  color: var(--brand-deep);
  display: flex;
  align-items: center;
  gap: 6px;
}

.mp-proxy-pool-status.is-active strong {
  color: var(--brand);
}

.mp-proxy-pool-status.is-inactive {
  color: var(--muted);
}

/* 渠道统计摘要：累计与今日双层对照 */
.mp-channel-summary {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 8px 10px;
  border-radius: var(--r-md, 10px);
  background: var(--surface-soft);
  border: 1px solid var(--line);
}

.mp-channel-summary-row {
  display: flex;
  align-items: center;
  gap: 10px;
}

.mp-channel-summary-row.is-today {
  border-top: 1px dashed var(--line);
  padding-top: 6px;
}

.mp-channel-summary-label {
  font-size: 11px;
  font-weight: 700;
  color: var(--muted);
  width: 28px;
  flex-shrink: 0;
  text-align: center;
  letter-spacing: 0.04em;
}

.mp-channel-summary-row.is-today .mp-channel-summary-label {
  color: var(--brand-deep);
}

.mp-channel-summary-metrics {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 6px;
  flex: 1;
  min-width: 0;
}

.mp-channel-summary-metric {
  display: flex;
  flex-direction: column;
  gap: 1px;
  min-width: 0;
}

.mp-channel-summary-metric small {
  font-size: 10.5px;
  color: var(--muted);
  line-height: 1;
}

.mp-channel-summary-metric strong {
  font-size: 13.5px;
  font-weight: 700;
  color: var(--text);
  line-height: 1.2;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.mp-channel-summary-metric.is-bad strong {
  color: var(--danger, #e5484d);
}

@media (max-width: 480px) {
  .mp-channel-summary-metrics {
    gap: 4px;
  }
  .mp-channel-summary-metric strong {
    font-size: 12.5px;
  }
}

.mp-channel-card-footer {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding-top: 12px;
  border-top: 1px solid var(--line);
}

.mp-channel-actions {
  display: flex;
  gap: 8px;
}

.mp-channel-actions .mp-action-btn {
  flex: 1;
  justify-content: center;
}

/* 开关组件 */
.mp-switch-wrap {
  position: relative;
  display: inline-block;
  width: 38px;
  height: 20px;
  cursor: pointer;
}

.mp-switch-wrap input {
  opacity: 0;
  width: 0;
  height: 0;
}

.mp-switch-round {
  position: absolute;
  inset: 0;
  background-color: var(--line);
  border-radius: 999px;
  transition: 0.2s;
}

.mp-switch-round:before {
  position: absolute;
  content: "";
  height: 14px;
  width: 14px;
  left: 3px;
  bottom: 3px;
  background-color: #fff;
  border-radius: 50%;
  transition: 0.2s;
}

.mp-switch-wrap input:checked + .mp-switch-round {
  background-color: var(--brand);
}

.mp-switch-wrap input:checked + .mp-switch-round:before {
  transform: translateX(18px);
}

/* 表单控件 */
.mp-field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  width: 100%;
}

.mp-field label {
  font-size: 12px;
  font-weight: 600;
  color: var(--text);
  display: flex;
  justify-content: space-between;
}

.mp-field small {
  font-size: 11px;
  color: var(--muted);
}

.mp-input {
  width: 100%;
  padding: 8px 10px;
  border-radius: var(--r-md);
  border: 1px solid var(--line);
  background: var(--surface-soft);
  color: var(--text);
  font-size: 13px;
  box-sizing: border-box;
}

.mp-input:focus {
  outline: none;
  border-color: var(--brand);
  box-shadow: 0 0 0 2px var(--brand-glow);
}

/* —— 弹窗内部 Tab 导航 —— */
.mp-inner-tab-nav {
  display: flex;
  gap: 8px;
  border-bottom: 1px solid var(--line);
  padding-bottom: 10px;
}

.mp-inner-tab-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 6px 12px;
  border-radius: var(--r-md, 8px);
  background: var(--surface-soft);
  border: 1px solid var(--line);
  color: var(--muted);
  font-size: 13px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.15s ease;
}

.mp-inner-tab-btn :deep(svg) {
  width: 14px;
  height: 14px;
}

.mp-inner-tab-btn:hover {
  background: var(--surface);
  color: var(--text);
}

.mp-inner-tab-btn.active {
  background: var(--brand-soft);
  border-color: var(--brand);
  color: var(--brand-deep);
  font-weight: 700;
}

.mp-inner-tab-badge {
  font-size: 11px;
  padding: 1px 6px;
  border-radius: 999px;
  background: var(--surface);
  border: 1px solid var(--line);
  color: var(--muted);
}

.mp-inner-tab-btn.active .mp-inner-tab-badge {
  background: var(--brand);
  border-color: var(--brand);
  color: #fff;
}

/* —— Key 分组调度与轮询面板样式 —— */
.mp-key-groups-toolbar {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
  padding-bottom: 4px;
}

.mp-kg-intro {
  flex: 1;
  min-width: 240px;
}

.mp-add-group-form {
  display: flex;
  align-items: center;
  gap: 6px;
}

.mp-add-group-input {
  height: 28px;
  padding: 0 10px;
  border-radius: var(--r-xs, 6px);
  border: 1px solid var(--line);
  background: var(--surface-soft);
  font-size: 12px;
  color: var(--text);
}

.mp-key-groups-container {
  display: flex;
  flex-direction: column;
  gap: 12px;
  max-height: 480px;
  overflow-y: auto;
  padding-right: 4px;
}

.mp-key-group-card {
  display: flex;
  flex-direction: column;
  gap: 8px;
  background: var(--surface-soft);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 10px);
  padding: 12px;
  transition: all 0.2s ease;
}

.mp-key-group-card.is-disabled {
  opacity: 0.55;
  background: var(--surface);
}

.mp-key-group-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 10px;
  padding-bottom: 8px;
  border-bottom: 1px solid var(--line);
}

.mp-kg-head-left {
  display: flex;
  align-items: center;
  gap: 8px;
  flex: 1;
  min-width: 0;
}

.mp-kg-priority-badge {
  font-size: 10.5px;
  font-weight: 800;
  padding: 1px 6px;
  border-radius: 4px;
  background: var(--surface);
  border: 1px solid var(--line-strong);
  color: var(--muted);
  flex-shrink: 0;
}

.mp-kg-priority-badge.is-top {
  background: var(--brand);
  border-color: var(--brand);
  color: #fff;
}

.mp-kg-name-input {
  font-size: 13.5px;
  font-weight: 750;
  color: var(--text);
  background: transparent;
  border: 1px solid transparent;
  border-radius: 4px;
  padding: 2px 6px;
  max-width: 180px;
  transition: all 0.15s ease;
}

.mp-kg-name-input:hover {
  border-color: var(--line);
  background: var(--surface);
}

.mp-kg-name-input:focus {
  outline: none;
  border-color: var(--brand);
  background: var(--surface);
}

.mp-kg-head-actions {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-shrink: 0;
}

.mp-kg-keys-list {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.mp-kg-key-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 10px;
  padding: 6px 10px;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: 6px;
  transition: all 0.15s ease;
}

.mp-kg-key-row.is-disabled {
  opacity: 0.45;
}

.mp-kg-key-main {
  display: flex;
  align-items: center;
  gap: 8px;
  flex: 1;
  min-width: 0;
}

.mp-kg-key-acc {
  font-size: 12px;
  font-weight: 600;
  color: var(--text);
  max-width: 140px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  flex-shrink: 0;
}

.mp-kg-key-code {
  font-size: 11.5px;
  color: var(--brand-deep);
  background: color-mix(in srgb, var(--brand) 10%, transparent);
  padding: 1px 6px;
  border-radius: 4px;
  flex-shrink: 0;
}

.mp-kg-model-tag {
  font-size: 10.5px;
  font-weight: 600;
  padding: 1px 6px;
  border-radius: 4px;
  background: var(--surface-soft);
  border: 1px solid var(--line);
  color: var(--muted);
}

.mp-kg-model-tag.is-all {
  color: var(--text-soft);
}

.mp-kg-key-controls {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-shrink: 0;
}

.mp-kg-group-select {
  height: 24px;
  padding: 0 6px;
  font-size: 11px;
  border-radius: 4px;
  border: 1px solid var(--line);
  background: var(--surface-soft);
  color: var(--text);
  cursor: pointer;
}

.mp-kg-empty-note {
  padding: 12px 0;
  text-align: center;
}

/* 弹窗模态框通用 */
.mp-modal-backdrop {
  position: fixed;
  inset: 0;
  z-index: 9000;
  background: rgba(10, 20, 15, 0.45);
  backdrop-filter: blur(4px);
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
}

.mp-sub-backdrop {
  z-index: 9100;
  background: rgba(10, 20, 15, 0.65);
}

.mp-modal-box {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 12px);
  width: 100%;
  max-width: 520px;
  max-height: 88vh;
  display: flex;
  flex-direction: column;
  box-shadow: 0 16px 40px rgba(0, 0, 0, 0.2);
  overflow: hidden;
  animation: mp-modal-fade 0.2s ease;
}

.mp-modal-box-sm {
  max-width: 480px;
}

.mp-modal-box-wide {
  max-width: 900px;
  width: 90vw;
}

.mp-modal-box-extra-wide {
  max-width: 1160px;
  width: 95vw;
}

@keyframes mp-modal-fade {
  from { opacity: 0; transform: scale(0.97); }
  to { opacity: 1; transform: scale(1); }
}

.mp-modal-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 16px 20px;
  border-bottom: 1px solid var(--line);
}

.mp-modal-title-group {
  display: flex;
  align-items: center;
  gap: 12px;
}

.mp-modal-badge-icon {
  width: 36px;
  height: 36px;
  border-radius: var(--r-md);
  background: var(--brand-soft);
  color: var(--brand);
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
}

.mp-modal-badge-icon.is-error {
  background: color-mix(in srgb, var(--danger) 15%, transparent);
  color: var(--danger);
}

.mp-modal-badge-icon.is-success {
  background: var(--brand-soft);
  color: var(--brand);
}

.mp-modal-badge-icon :deep(svg) {
  width: 20px;
  height: 20px;
}

.mp-modal-icon {
  width: 24px;
  height: 24px;
  color: var(--brand);
  display: inline-flex;
}

.mp-modal-icon :deep(svg) {
  width: 100%;
  height: 100%;
}

.mp-modal-title-wrap {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.mp-modal-header h3 {
  margin: 0;
  font-size: 16px;
  font-weight: 750;
  color: var(--text);
}

.mp-header-chip {
  font-size: 11px;
  font-weight: 700;
  padding: 1px 7px;
  border-radius: 999px;
  background: var(--brand-soft);
  color: var(--brand-deep);
}

.mp-header-endpoint-chip {
  font-size: 11px;
  padding: 1px 7px;
  border-radius: var(--r-xs);
  background: var(--surface-soft);
  color: var(--muted);
  border: 1px solid var(--line);
}

.mp-modal-close {
  width: 32px;
  height: 32px;
  border-radius: var(--r-xs);
  border: none;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  display: inline-flex;
  align-items: center;
  justify-content: center;
}

.mp-modal-close :deep(svg) {
  width: 18px;
  height: 18px;
}

.mp-modal-close:hover {
  background: var(--surface-hover);
  color: var(--text);
}

.mp-modal-body {
  padding: 20px;
  display: flex;
  flex-direction: column;
  gap: 16px;
  overflow-y: auto;
}

.mp-modal-footer {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 10px;
  padding: 14px 20px;
  border-top: 1px solid var(--line);
  background: var(--surface-soft);
}

.mp-modal-footer-hint {
  font-size: 12px;
  color: var(--muted);
}

/* 底部操作按钮组：靠右相邻排布，避免被 space-between 撑散 */
.mp-modal-footer-buttons {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-shrink: 0;
}

/* 模型弹窗高级工具栏 */
.mp-models-modal-toolbar {
  display: flex;
  align-items: center;
  gap: 12px;
}

.mp-search-box {
  position: relative;
  display: flex;
  align-items: center;
}

.mp-search-box.flex-1 {
  flex: 1;
  min-width: 200px;
}

/* 放大镜图标：以包裹 span 定位并精确垂直居中 */
.mp-search-icon {
  position: absolute;
  left: 12px;
  top: 50%;
  transform: translateY(-50%);
  display: inline-flex;
  pointer-events: none;
}

.mp-search-icon :deep(svg) {
  width: 16px;
  height: 16px;
  color: var(--muted);
}

.mp-search-input-lg {
  width: 100%;
  height: 38px;
  padding: 0 32px 0 36px;
  border-radius: var(--r-md);
  border: 1px solid var(--line);
  background: var(--surface-soft);
  color: var(--text);
  font-size: 13px;
  box-sizing: border-box;
  transition: all 0.18s ease;
}

.mp-search-input-lg:focus {
  outline: none;
  border-color: var(--brand);
  box-shadow: 0 0 0 2px var(--brand-glow);
  background: var(--surface);
}

.mp-search-clear-btn {
  position: absolute;
  right: 8px;
  width: 20px;
  height: 20px;
  border-radius: 50%;
  border: none;
  background: var(--surface-hover);
  color: var(--muted);
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 0;
}

.mp-search-clear-btn :deep(svg) {
  width: 12px;
  height: 12px;
}

.mp-spin {
  animation: mp-spin-anim 0.8s linear infinite;
}

@keyframes mp-spin-anim {
  from { transform: rotate(0deg); }
  to { transform: rotate(360deg); }
}

/* 渠道分组展示容器 */
.mp-channel-groups-container {
  display: flex;
  flex-direction: column;
  gap: 20px;
  max-height: 480px;
  overflow-y: auto;
  padding-right: 4px;
}

.mp-channel-group-block {
  display: flex;
  flex-direction: column;
  gap: 10px;
  background: var(--surface-soft);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 12px);
  padding: 14px 16px;
}

.mp-group-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
  padding-bottom: 8px;
  border-bottom: 1px solid var(--line);
}

.mp-group-title-row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.mp-group-icon {
  width: 22px;
  height: 22px;
  color: var(--brand);
  display: inline-flex;
}

.mp-group-icon :deep(svg) {
  width: 100%;
  height: 100%;
}

.mp-group-name-wrap {
  display: flex;
  align-items: center;
  gap: 8px;
}

.mp-group-name {
  margin: 0;
  font-size: 14px;
  font-weight: 750;
  color: var(--text);
}

.mp-group-count-badge {
  font-size: 11px;
  font-weight: 600;
  padding: 1px 6px;
  border-radius: 999px;
  background: var(--surface);
  border: 1px solid var(--line);
  color: var(--muted);
}

/* 已配置模型白名单的渠道标记 */
.mp-group-count-badge.is-filtered {
  background: var(--brand-soft);
  border-color: var(--brand);
  color: var(--brand-deep);
}

.mp-group-meta {
  display: flex;
  align-items: center;
  gap: 8px;
}

.mp-group-endpoint {
  font-size: 11px;
  color: var(--muted);
  background: var(--surface);
  padding: 2px 6px;
  border-radius: 4px;
  border: 1px solid var(--line);
}

.mp-status-pill-xs {
  font-size: 10.5px;
  padding: 1px 7px;
}

.mp-group-empty-note {
  padding: 16px 0;
  text-align: center;
}

/* 优雅模型卡片双列矩阵 */
.mp-model-cards-grid {
  display: grid;
  grid-template-columns: repeat(2, 1fr);
  gap: 12px;
}

@media (max-width: 680px) {
  .mp-model-cards-grid {
    grid-template-columns: 1fr;
  }
}

.mp-model-elegant-card {
  background: var(--surface-soft);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 10px);
  padding: 12px 14px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  cursor: pointer;
  transition: all 0.2s cubic-bezier(0.4, 0, 0.2, 1);
  position: relative;
}

.mp-model-elegant-card:hover {
  background: var(--surface);
  border-color: color-mix(in srgb, var(--brand) 50%, transparent);
  transform: translateY(-1px);
  box-shadow: 0 4px 14px rgba(0, 0, 0, 0.06);
}

.mp-model-elegant-card.is-copied {
  border-color: var(--brand);
  background: var(--brand-soft);
}

/* 管理模型：勾选态高亮，未勾选态降低透明度 */
.mp-model-elegant-card.is-selected {
  border-color: var(--brand);
  background: var(--brand-soft);
}

.mp-model-elegant-card:not(.is-selected) {
  opacity: 0.68;
}

.mp-model-elegant-card:not(.is-selected):hover {
  opacity: 0.9;
}

.mp-mec-check {
  flex-shrink: 0;
  width: 20px;
  height: 20px;
  border-radius: 6px;
  border: 1.5px solid var(--line-strong);
  background: var(--surface);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  transition: all 0.18s ease;
}

.mp-mec-check :deep(svg) {
  width: 12px;
  height: 12px;
  color: #fff;
}

.mp-model-elegant-card.is-selected .mp-mec-check {
  background: var(--brand);
  border-color: var(--brand);
}

.mp-mec-left {
  display: flex;
  flex-direction: column;
  gap: 5px;
  min-width: 0;
  flex: 1;
}

.mp-mec-title-row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.mp-model-free-badge {
  font-size: 10px;
  font-weight: 800;
  letter-spacing: 0.04em;
  padding: 1px 5px;
  border-radius: 4px;
  background: color-mix(in srgb, var(--brand) 20%, transparent);
  color: var(--brand-deep);
  flex-shrink: 0;
}

.mp-model-name-title {
  font-family: var(--font-mono, monospace);
  font-size: 13.5px;
  font-weight: 700;
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.mp-mec-id-row {
  display: flex;
  align-items: center;
  gap: 6px;
}

.mp-mec-id-label {
  font-size: 11px;
  color: var(--muted);
  flex-shrink: 0;
}

.mp-mec-id-code {
  font-family: var(--font-mono, monospace);
  font-size: 12px;
  color: var(--brand-deep);
  background: color-mix(in srgb, var(--brand) 10%, transparent);
  padding: 1px 6px;
  border-radius: 4px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.mp-mec-right {
  flex-shrink: 0;
}

.mp-copy-action-btn {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  height: 28px;
  padding: 0 10px;
  border-radius: var(--r-xs, 6px);
  background: var(--surface);
  border: 1px solid var(--line);
  color: var(--text);
  font-size: 11.5px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.18s ease;
}

.mp-copy-action-btn :deep(svg) {
  width: 12px;
  height: 12px;
  color: var(--muted);
}

.mp-copy-action-btn:hover {
  background: var(--brand-soft);
  border-color: var(--brand);
  color: var(--brand-deep);
}

.mp-copy-action-btn:hover :deep(svg) {
  color: var(--brand);
}

.mp-copy-action-btn.copied {
  background: var(--brand);
  border-color: var(--brand);
  color: #fff;
}

.mp-copy-action-btn.copied :deep(svg) {
  color: #fff;
}

/* 管理模型弹窗：富信息卡片行 */
.mp-model-check-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.mp-mcm-card {
  position: relative;
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 14px;
  background: var(--surface-soft);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 10px);
  cursor: pointer;
  transition: all 0.18s cubic-bezier(0.4, 0, 0.2, 1);
}

.mp-mcm-card:hover {
  border-color: color-mix(in srgb, var(--brand) 50%, transparent);
  box-shadow: 0 2px 8px color-mix(in srgb, var(--brand) 10%, transparent);
}

/* 勾选态高亮，未勾选态降低透明度 */
.mp-mcm-card.is-selected {
  border-color: var(--brand);
  background: var(--brand-soft);
}

.mp-mcm-card:not(.is-selected) {
  opacity: 0.72;
}

.mp-mcm-card:not(.is-selected):hover {
  opacity: 1;
}

.mp-mcm-card.is-selected .mp-mec-check {
  background: var(--brand);
  border-color: var(--brand);
}

/* —— 主体区（名称 + 状态 + 今日 + 调用 ID） —— */
.mp-mcm-main {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-width: 220px;
  flex-shrink: 0;
}

.mp-mcm-title-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.mp-mcm-today-chip {
  font-size: 10.5px;
  font-weight: 700;
  padding: 1px 7px;
  border-radius: 999px;
  background: color-mix(in srgb, var(--warning) 16%, transparent);
  color: var(--warning);
}

.mp-mcm-id-code {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  align-self: flex-start;
  font-size: 11.5px;
  color: var(--brand-deep);
  background: color-mix(in srgb, var(--surface) 80%, transparent);
  border: 1px dashed color-mix(in srgb, var(--brand) 35%, transparent);
  padding: 2px 8px;
  border-radius: 6px;
  cursor: copy;
  transition: all 0.15s ease;
  max-width: 100%;
  overflow: hidden;
  white-space: nowrap;
}

.mp-mcm-id-code:hover {
  border-color: var(--brand);
  background: color-mix(in srgb, var(--brand) 12%, transparent);
}

.mp-mcm-copy-icon :deep(svg) {
  width: 11px;
  height: 11px;
  vertical-align: -1px;
}

.mp-mcm-copy-ok {
  font-size: 10.5px;
  color: var(--brand);
  font-weight: 700;
}

/* —— 右侧统计区 —— */
.mp-mcm-stats {
  margin-left: auto;
  display: flex;
  align-items: stretch;
  gap: 0;
  flex-shrink: 0;
}

.mp-mcm-stat {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  justify-content: center;
  gap: 1px;
  padding: 0 14px;
  border-left: 1px solid var(--line);
  min-width: 64px;
}

.mp-mcm-stat-val {
  font-size: 13px;
  font-weight: 700;
  color: var(--text);
  line-height: 1.25;
  white-space: nowrap;
}

.mp-mcm-stat-label {
  font-size: 10px;
  color: var(--muted);
  letter-spacing: 0.04em;
  white-space: nowrap;
}

.mp-mcm-stat.is-bad .mp-mcm-stat-val {
  color: var(--danger, #e5484d);
}

.mp-mcm-stat-wide {
  min-width: 88px;
}

.mp-mcm-stat-wide .mp-mcm-stat-val {
  font-size: 11.5px;
  font-weight: 600;
  color: var(--muted);
}

.mp-mcm-stat-empty {
  align-items: flex-end;
}

.mp-mcm-stat-empty .mp-mcm-stat-label {
  font-style: normal;
}

/* Key 专属提示：绝对定位贴右下角，不占布局 */
.mp-mcm-key-tag {
  position: absolute;
  right: 10px;
  bottom: -6px;
  font-size: 9.5px;
  padding: 0 6px;
  border-radius: 999px;
  background: var(--surface);
  border: 1px solid color-mix(in srgb, var(--warning) 45%, transparent);
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.08);
}

/* 行尾模型级代理策略选择 */
.mp-mcm-proxy {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  flex-shrink: 0;
  margin-left: 8px;
  position: relative;
}

.mp-mcm-proxy-label {
  font-size: 11px;
  color: var(--muted);
  white-space: nowrap;
}

.mp-mcm-proxy-select {
  height: 26px;
  padding: 0 6px;
  border-radius: 6px;
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--text);
  font-size: 11.5px;
  cursor: pointer;
  max-width: 92px;
}

.mp-mcm-proxy-select:hover {
  border-color: color-mix(in srgb, var(--brand) 55%, transparent);
}

/* 覆盖与渠道级配置不同时的提示圆点 */
.mp-mcm-proxy-dot {
  position: absolute;
  top: -3px;
  right: -3px;
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--warning);
  border: 1.5px solid var(--surface-soft);
}

/* 工具栏扩展控件 */
.mp-toolbar-switch {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 12.5px;
  color: var(--muted);
  cursor: pointer;
  white-space: nowrap;
  user-select: none;
}

.mp-toolbar-switch input {
  accent-color: var(--brand);
  cursor: pointer;
}

.mp-toolbar-sort {
  display: inline-flex;
  align-items: center;
}

.mp-sort-select {
  height: 32px;
  padding: 0 8px;
  border-radius: 8px;
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--text);
  font-size: 12.5px;
  cursor: pointer;
}

@keyframes mp-spin-rotate {
  from { transform: rotate(360deg); }
  to { transform: rotate(0deg); }
}

.mp-spin-icon {
  animation: mp-spin-rotate 1.2s linear infinite;
  display: inline-block;
}

.mp-reorder-btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 18px;
  height: 18px;
  border-radius: 4px;
  border: none;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  transition: all 0.15s ease;
  padding: 0;
}

.mp-reorder-btn :deep(svg) {
  width: 11px;
  height: 11px;
}

.mp-reorder-btn:hover:not(:disabled) {
  background: var(--brand-soft);
  color: var(--brand-deep);
}

.mp-reorder-btn:disabled {
  opacity: 0.3;
  cursor: default;
}

/* 模型行内的「N 渠道共供」角标 */
.mp-overlap-badge {
  flex-shrink: 0;
  font-size: 10px;
  font-weight: 700;
  padding: 1px 6px;
  border-radius: 999px;
  background: color-mix(in srgb, #f59e0b 16%, transparent);
  border: 1px solid color-mix(in srgb, #f59e0b 55%, transparent);
  color: #b45309;
}

.mp-empty-box {
  padding: 48px 0;
  text-align: center;
  color: var(--muted);
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 10px;
}

.mp-empty-icon {
  width: 36px;
  height: 36px;
  color: var(--line-strong);
  display: inline-flex;
}

.mp-empty-icon :deep(svg) {
  width: 100%;
  height: 100%;
}

.mp-empty-box p {
  margin: 0;
  font-size: 13px;
}


.mp-logs-table-wrap {
  position: relative;
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  overflow: hidden;
  max-height: 480px;
  overflow-y: auto;
  overflow-x: auto;
  width: 100%;
  box-sizing: border-box;
}

.mp-logs-table-wrap.is-loading {
  pointer-events: none;
}

.mp-table-loading-overlay {
  position: absolute;
  inset: 0;
  z-index: 10;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  background: color-mix(in srgb, var(--surface) 80%, transparent);
  backdrop-filter: blur(2px);
  font-size: 13px;
  color: var(--muted);
  font-weight: 500;
}

.mp-logs-table {
  width: 100%;
  border-collapse: collapse;
  /* 固定布局：其余列精确定宽，渠道/模型列（未设宽）弹性吃满剩余空间 */
  table-layout: fixed;
  font-size: 12.5px;
  text-align: left;
}

.mp-logs-table th {
  background: var(--surface-soft);
  color: var(--muted);
  font-weight: 650;
  padding: 8px 12px;
  border-bottom: 1px solid var(--line);
  font-size: 11.5px;
  position: sticky;
  top: 0;
  z-index: 1;
  white-space: nowrap;
}

.mp-logs-table .mp-th-sortable {
  cursor: pointer;
  user-select: none;
  transition: color 0.15s;
}

.mp-logs-table .mp-th-sortable:hover {
  color: var(--brand);
}

.mp-logs-table .mp-th-sortable.is-sorted {
  color: var(--brand);
}

.mp-logs-table .mp-sort-arrow {
  margin-left: 4px;
  font-size: 10px;
  opacity: 0.75;
}

.mp-logs-table td {
  padding: 10px 12px;
  border-bottom: 1px solid var(--line);
  vertical-align: middle;
}


.mp-ep-code-sm {
  font-family: var(--font-mono, monospace);
  font-size: 11.5px;
  color: var(--brand);
  background: var(--surface-soft);
  padding: 2px 6px;
  border-radius: 4px;
}

.mp-status-tag {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 2px 8px;
  border-radius: 999px;
  font-size: 11px;
  font-weight: 600;
}

.mp-status-dot-sm {
  width: 6px;
  height: 6px;
  border-radius: 50%;
}

.tag-ok {
  background: var(--brand-soft);
  color: var(--brand-deep);
}

.tag-ok .mp-status-dot-sm {
  background: var(--brand);
}

.tag-warn {
  background: color-mix(in srgb, #f59e0b 15%, transparent);
  color: #b45309;
}

.tag-warn .mp-status-dot-sm {
  background: #f59e0b;
}

.tag-err {
  background: color-mix(in srgb, var(--danger) 15%, transparent);
  color: var(--danger);
}

.tag-err .mp-status-dot-sm {
  background: var(--danger);
}

.mp-auth-tag {
  font-size: 11px;
  padding: 2px 6px;
  border-radius: 4px;
  background: var(--surface-soft);
  color: var(--muted);
  border: 1px solid var(--line);
}

/* 请求日志专用样式 */
.mp-logs-toolbar {
  display: flex;
  /* 工具栏控件高度不一（筛选页签/搜索框/按钮），统一以底边对齐 */
  align-items: flex-end;
  gap: 12px;
  flex-wrap: wrap;
}

.mp-logs-range {
  display: inline-flex;
  flex-shrink: 0;
}

.mp-log-filter-tabs {
  display: flex;
  align-items: center;
  gap: 4px;
  background: var(--surface-soft);
  padding: 3px;
  border-radius: var(--r-md);
  border: 1px solid var(--line);
}

.mp-log-tab-btn {
  border: none;
  background: transparent;
  color: var(--muted);
  font-size: 12px;
  font-weight: 600;
  padding: 4px 10px;
  border-radius: var(--r-xs);
  cursor: pointer;
  transition: all 0.15s ease;
}

.mp-log-tab-btn:hover {
  color: var(--text);
}

.mp-log-tab-btn.active {
  background: var(--surface);
  color: var(--brand);
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.08);
}

.mp-logs-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}

.mp-log-row {
  cursor: pointer;
  transition: background 0.15s ease;
}

.mp-log-row:hover {
  background: var(--surface-hover);
}

.mp-log-row.has-error {
  background: color-mix(in srgb, var(--danger) 4%, transparent);
}

.mp-log-method-path {
  display: flex;
  flex-direction: column;
  gap: 3px;
  line-height: 1.25;
  min-width: 0;
}

.mp-path-row {
  display: flex;
  align-items: center;
  gap: 4px;
  min-width: 0;
}

.mp-path-label {
  font-size: 9px;
  font-weight: 800;
  color: var(--text-muted, #999);
  background: color-mix(in srgb, var(--text-muted, #999) 12%, transparent);
  padding: 0 3px;
  border-radius: 2px;
  flex-shrink: 0;
}

.mp-method-tag {
  font-size: 10px;
  font-weight: 800;
  padding: 1px 5px;
  border-radius: 3px;
}

.method-post {
  background: color-mix(in srgb, var(--brand) 15%, transparent);
  color: var(--brand-deep);
}

.method-get {
  background: color-mix(in srgb, #3b82f6 15%, transparent);
  color: #2563eb;
}

.mp-path-code {
  font-family: var(--font-mono, monospace);
  font-size: 11px;
  color: var(--text);
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.mp-upstream-code {
  font-family: var(--font-mono, monospace);
  font-size: 11px;
  color: var(--text-muted, #888);
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.mp-log-time-col {
  display: flex;
  flex-direction: column;
  gap: 2px;
  line-height: 1.25;
}

.mp-log-time-col .mp-log-date {
  font-size: 11px;
  color: var(--muted);
}

.mp-log-time-col .mp-log-time {
  font-size: 12px;
  font-weight: 700;
  color: var(--text);
}

.mp-log-model-col {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 3px;
  min-width: 0;
  line-height: 1.25;
}

.mp-log-chan-row {
  display: flex;
  align-items: center;
}

.mp-log-model-col .mp-proto-tag {
  font-size: 9.5px;
  padding: 1px 5px;
  border-radius: 3px;
}

.mp-log-model-col .mp-log-model-name {
  font-size: 12px;
  font-weight: 650;
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 100%;
}

.mp-log-node-wrap {
  display: flex;
  align-items: center;
  max-width: 120px;
}

.mp-node-pill {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  font-size: 11px;
  padding: 1.5px 7px;
  border-radius: 999px;
  white-space: nowrap;
  max-width: 100%;
}

.mp-node-pill.is-direct {
  background: color-mix(in srgb, #3b82f6 12%, transparent);
  color: #2563eb;
  border: 1px solid color-mix(in srgb, #3b82f6 25%, transparent);
}

.mp-node-pill.is-direct .mp-node-dot {
  background: #3b82f6;
}

.mp-node-pill.is-proxy {
  background: color-mix(in srgb, #8b5cf6 15%, transparent);
  color: #7c3aed;
  border: 1px solid color-mix(in srgb, #8b5cf6 30%, transparent);
}

.mp-node-pill.is-proxy .mp-node-dot {
  background: #8b5cf6;
}

.mp-node-dot {
  width: 5px;
  height: 5px;
  border-radius: 50%;
  flex-shrink: 0;
}

.is-proxy-chip {
  background: color-mix(in srgb, #8b5cf6 15%, transparent) !important;
  color: #7c3aed !important;
  border-color: color-mix(in srgb, #8b5cf6 30%, transparent) !important;
}

.mp-stream-tag {
  font-size: 10px;
  padding: 1px 5px;
  border-radius: 4px;
  background: var(--surface-soft);
  color: var(--muted);
  border: 1px solid var(--line);
}

.mp-stream-tag.is-stream {
  color: var(--brand-deep);
  background: var(--brand-soft);
  border-color: color-mix(in srgb, var(--brand) 25%, transparent);
}

.mp-mode-cell {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 3px;
}

.mp-mode-tps {
  font-size: 10px;
  color: var(--faint);
  white-space: nowrap;
}

.truncate {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.flex-center-start {
  display: flex;
  align-items: center;
}

.ml-1 {
  margin-left: 4px;
}

/* 请求详情弹窗专属样式 */
.mp-log-detail-box {
  max-width: 1180px;
  width: 94vw;
}

.mp-log-detail-box .mp-modal-header {
  padding: 16px 20px;
}

.mp-log-detail-box .mp-modal-title-group {
  flex: 1;
  min-width: 0;
}

.mp-log-detail-box .mp-modal-title-wrap {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: nowrap;
  white-space: nowrap;
}

.mp-log-detail-box .mp-modal-title-wrap > * {
  flex-shrink: 0;
}

.mp-log-error-banner {
  display: flex;
  align-items: flex-start;
  gap: 12px;
  background: color-mix(in srgb, var(--danger) 10%, transparent);
  border: 1px solid color-mix(in srgb, var(--danger) 35%, transparent);
  border-radius: var(--r-md);
  padding: 14px 16px;
}

.mp-leb-icon {
  width: 22px;
  height: 22px;
  color: var(--danger);
  display: inline-flex;
  flex-shrink: 0;
  margin-top: 1px;
}

.mp-leb-icon :deep(svg) {
  width: 100%;
  height: 100%;
}

.mp-leb-content {
  display: flex;
  flex-direction: column;
  gap: 6px;
  min-width: 0;
  flex: 1;
}

.mp-leb-title strong {
  font-size: 13.5px;
  color: var(--danger);
}

.mp-leb-reason {
  margin: 0;
  font-size: 12.5px;
  color: var(--text);
  font-weight: 550;
  word-break: break-all;
}

.mp-leb-suggestion {
  display: flex;
  align-items: flex-start;
  gap: 6px;
  font-size: 12px;
  color: var(--muted);
  background: var(--surface);
  padding: 6px 10px;
  border-radius: var(--r-xs);
  border: 1px solid color-mix(in srgb, var(--danger) 20%, transparent);
  margin-top: 4px;
}

.mp-leb-tag {
  font-weight: 700;
  color: var(--brand-deep);
  flex-shrink: 0;
}

.mp-log-detail-grid {
  display: grid;
  grid-template-columns: repeat(2, 1fr);
  gap: 12px;
}

@media (max-width: 600px) {
  .mp-log-detail-grid {
    grid-template-columns: 1fr;
  }
}

.mp-ld-item {
  background: var(--surface-soft);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 10px 12px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.mp-ld-item label {
  font-size: 11px;
  color: var(--muted);
  font-weight: 600;
}

.mp-ld-val {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12.5px;
  color: var(--text);
}

.mp-dur-col {
  display: flex;
  flex-direction: column;
  gap: 2px;
  line-height: 1.25;
}

.mp-dur-row {
  display: flex;
  align-items: center;
  gap: 4px;
}

.mp-dur-label {
  font-size: 11px;
  color: var(--muted);
  width: 18px;
  flex-shrink: 0;
}

.mp-dur-val {
  font-size: 11.5px;
}

.mp-log-tokens-cell {
  display: flex;
  flex-direction: column;
  gap: 3px;
}

.mp-token-pill-row {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 4px;
}

.mp-token-tag {
  font-size: 10px;
  font-weight: 600;
  padding: 1px 5px;
  border-radius: 3px;
  display: inline-flex;
  align-items: center;
  justify-content: space-between;
  gap: 6px;
  min-width: 0;
}

.mp-token-tag > strong {
  font-weight: 700;
  font-variant-numeric: tabular-nums;
}

.mp-token-tag.is-in {
  background: var(--surface-soft);
  color: var(--muted);
  border: 1px solid var(--line);
}

.mp-token-tag.is-hit {
  background: var(--brand-soft);
  color: var(--brand-deep);
  border: 1px solid color-mix(in srgb, var(--brand) 30%, transparent);
  font-weight: 700;
}

.mp-token-tag.is-out {
  background: color-mix(in srgb, #10b981 12%, transparent);
  color: #059669;
  border: 1px solid color-mix(in srgb, #10b981 25%, transparent);
}

.mp-token-tag.is-think {
  background: color-mix(in srgb, #8b5cf6 12%, transparent);
  color: #7c3aed;
  border: 1px solid color-mix(in srgb, #8b5cf6 25%, transparent);
  font-weight: 700;
}

/* 4 宫格 Token 仪表盘 */
.mp-token-dashboard-grid {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 12px;
}

@media (max-width: 768px) {
  .mp-token-dashboard-grid {
    grid-template-columns: repeat(2, 1fr);
  }
}

.mp-token-card {
  background: var(--surface-soft);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 12px 14px;
  display: flex;
  flex-direction: column;
  gap: 6px;
  transition: all 0.15s ease;
}

.mp-token-card.is-in {
  border-left: 3px solid var(--muted);
}

.mp-token-card.is-hit {
  border-left: 3px solid var(--brand);
  background: color-mix(in srgb, var(--brand) 4%, var(--surface-soft));
}

.mp-token-card.is-think {
  border-left: 3px solid #8b5cf6;
  background: color-mix(in srgb, #8b5cf6 4%, var(--surface-soft));
}

.mp-token-card.is-out {
  border-left: 3px solid #10b981;
  background: color-mix(in srgb, #10b981 4%, var(--surface-soft));
}

.mp-tc-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 4px;
}

.mp-tc-label {
  font-size: 11px;
  font-weight: 700;
  color: var(--muted);
}

.mp-tc-badge {
  font-size: 10px;
  font-weight: 600;
  padding: 1px 5px;
  border-radius: 3px;
  background: var(--surface);
  color: var(--muted);
  border: 1px solid var(--line);
}

.mp-tc-badge.badge-hit {
  background: var(--brand-soft);
  color: var(--brand-deep);
  border-color: color-mix(in srgb, var(--brand) 30%, transparent);
}

/* 总 Token 汇总卡：全宽横贯，视觉重量突出 */
.mp-token-card.is-total {
  grid-column: 1 / -1;
  border-left: 3px solid var(--brand);
  background: color-mix(in srgb, var(--brand) 6%, var(--surface-soft));
}

.mp-tc-badge.badge-total {
  background: var(--brand-soft);
  color: var(--brand-deep);
  border-color: color-mix(in srgb, var(--brand) 30%, transparent);
}

.mp-tc-value {
  font-size: 22px;
  font-weight: 800;
  color: var(--text);
  line-height: 1.2;
}

.mp-tc-foot {
  font-size: 11px;
  color: var(--muted);
  display: flex;
  align-items: center;
  gap: 4px;
  flex-wrap: wrap;
}

.mp-tc-divider {
  color: var(--line);
}

/* 选项卡导航栏 */
.mp-detail-tabs-bar {
  display: flex;
  align-items: center;
  gap: 6px;
  border-bottom: 1px solid var(--line);
  padding-bottom: 8px;
  flex-wrap: wrap;
}

.mp-detail-tab-btn {
  border: 1px solid transparent;
  background: var(--surface-soft);
  color: var(--muted);
  font-size: 12px;
  font-weight: 650;
  padding: 6px 12px;
  border-radius: var(--r-md);
  cursor: pointer;
  display: inline-flex;
  align-items: center;
  gap: 6px;
  transition: all 0.15s ease;
}

.mp-detail-tab-btn :deep(svg) {
  width: 14px;
  height: 14px;
}

.mp-detail-tab-btn:hover {
  color: var(--text);
  background: var(--surface-hover);
}

.mp-detail-tab-btn.active {
  background: var(--brand-soft);
  color: var(--brand-deep);
  border-color: color-mix(in srgb, var(--brand) 30%, transparent);
}

.mp-detail-tab-btn.is-error {
  color: var(--danger);
  background: color-mix(in srgb, var(--danger) 8%, transparent);
}

.mp-detail-tab-btn.is-error.active {
  background: color-mix(in srgb, var(--danger) 18%, transparent);
  border-color: color-mix(in srgb, var(--danger) 40%, transparent);
}

.mp-detail-tab-content {
  display: flex;
  flex-direction: column;
  gap: 14px;
  animation: mp-fade-in 0.15s ease-out;
}

@keyframes mp-fade-in {
  from { opacity: 0.5; transform: translateY(2px); }
  to { opacity: 1; transform: translateY(0); }
}

.mp-log-raw-box {
  background: var(--surface-soft);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 12px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.mp-lrb-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.mp-lrb-header label {
  font-size: 11.5px;
  font-weight: 700;
  color: var(--muted);
}

/* 响应语义视图：视图切换 + 片段卡片 */
.mp-view-toggle {
  display: inline-flex;
  border: 1px solid var(--border, #ddd);
  border-radius: 8px;
  overflow: hidden;
}
.mp-view-toggle button {
  border: none;
  background: transparent;
  padding: 3px 12px;
  font-size: 12px;
  cursor: pointer;
  color: var(--text-muted, #888);
}
.mp-view-toggle button.active {
  background: var(--brand, #4a6cf7);
  color: #fff;
}
.mp-seg-list {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 10px;
}
.mp-seg-card {
  border: 1px solid var(--border, #e3e6ee);
  border-radius: 10px;
  overflow: hidden;
  background: var(--bg-primary, #fff);
}
.mp-seg-tool-head {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 7px 12px;
  background: rgba(0, 0, 0, 0.03);
  border-bottom: 1px solid var(--border, #e3e6ee);
  flex-wrap: wrap;
}
.mp-seg-kind-tag {
  font-size: 11px;
  padding: 2px 8px;
  border-radius: 999px;
  white-space: nowrap;
}
.mp-seg-kind-tag.is-tool { background: rgba(255, 159, 10, 0.14); color: #b06000; }
.mp-seg-kind-tag.is-text { background: rgba(16, 185, 129, 0.13); color: #0b8a5c; }
.mp-seg-kind-tag.is-reason { background: rgba(139, 92, 246, 0.13); color: #6d3fd4; }
.mp-seg-callid { font-size: 11px; color: var(--text-muted, #999); }
.mp-seg-args {
  margin: 0;
  padding: 10px 12px;
  font-size: 12px;
  line-height: 1.55;
  max-height: 260px;
  overflow: auto;
  white-space: pre-wrap;
  word-break: break-all;
  background: rgba(0, 0, 0, 0.02);
}
.mp-seg-markdown {
  padding: 10px 14px;
  font-size: 13px;
  line-height: 1.7;
  word-break: break-word;
}
.mp-seg-markdown :deep(pre) {
  background: #1f2430;
  color: #e6e9f2;
  padding: 10px 12px;
  border-radius: 8px;
  overflow: auto;
  font-size: 12px;
}
.mp-seg-markdown :deep(code) {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 12px;
}
.mp-seg-markdown :deep(p) { margin: 6px 0; }
.mp-seg-markdown :deep(h1),
.mp-seg-markdown :deep(h2),
.mp-seg-markdown :deep(h3),
.mp-seg-markdown :deep(h4) { margin: 10px 0 6px; }
.mp-seg-markdown :deep(table) { border-collapse: collapse; margin: 8px 0; }
.mp-seg-markdown :deep(th),
.mp-seg-markdown :deep(td) {
  border: 1px solid var(--border, #ddd);
  padding: 4px 8px;
  font-size: 12px;
}
.mp-seg-reason-body {
  margin: 0;
  padding: 10px 12px;
  font-size: 12px;
  line-height: 1.65;
  color: #6d3fd4;
  background: rgba(139, 92, 246, 0.05);
  white-space: pre-wrap;
  word-break: break-word;
  max-height: 240px;
  overflow: auto;
}

.mp-lrb-pre {
  margin: 0;
  font-family: var(--font-mono, monospace);
  font-size: 11.5px;
  color: var(--text);
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-xs);
  padding: 12px 14px;
  white-space: pre-wrap;
  word-break: break-all;
  max-height: 360px;
  overflow-y: auto;
  line-height: 1.5;
}

/* 清理数据库日志弹窗样式 */
.mp-clear-logs-box {
  max-width: 540px;
}

.mp-clear-modal-desc {
  margin: 0 0 14px;
  font-size: 13px;
  color: var(--muted);
}

.mp-clear-options-grid {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.mp-clear-option-card {
  background: var(--surface-soft);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 14px;
  display: flex;
  flex-direction: column;
  gap: 8px;
  transition: all 0.18s ease;
}

.mp-clear-option-card:hover {
  border-color: var(--line-strong);
  background: var(--surface-hover);
}

.mp-clear-option-card.is-danger {
  border-color: color-mix(in srgb, var(--danger) 25%, transparent);
  background: color-mix(in srgb, var(--danger) 4%, var(--surface-soft));
}

.mp-clear-option-card.is-danger:hover {
  border-color: color-mix(in srgb, var(--danger) 45%, transparent);
  background: color-mix(in srgb, var(--danger) 8%, var(--surface-soft));
}

.mp-coc-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.mp-coc-title-wrap {
  display: flex;
  align-items: center;
  gap: 10px;
}

.mp-coc-icon {
  font-size: 18px;
  line-height: 1;
}

.mp-coc-badge {
  font-size: 10px;
  font-weight: 700;
  padding: 1px 6px;
  border-radius: 999px;
  background: var(--brand-soft);
  color: var(--brand-deep);
  margin-left: 6px;
}

.mp-coc-badge.is-danger {
  background: color-mix(in srgb, var(--danger) 15%, transparent);
  color: var(--danger);
}

.mp-coc-desc {
  margin: 0;
  font-size: 12px;
  color: var(--muted);
  line-height: 1.5;
}

.mp-coc-action {
  display: flex;
  justify-content: flex-end;
  margin-top: 4px;
}
</style>
