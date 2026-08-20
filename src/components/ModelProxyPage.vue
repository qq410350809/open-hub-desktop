<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from "vue";
import { icons } from "../icons";
import {
  useModelProxy,
  channelAlias,
  isValidChannelAlias,
  filterChannelModels,
  type ChannelConfig,
  type ChannelUsageStats,
  type ProxyRequestLog,
} from "../composables/useModelProxy";
import { useLibrary, runCommand } from "../composables/useLibrary";
import { useToast } from "../composables/useToast";
import CustomSelect from "./CustomSelect.vue";
import type { SiteRecord } from "../types";

const logPageSizeOptions = [
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
  testingHealth,
  fetchingModels,
  channelModels,
  modelsForChannel,
  channelStats,
  healthResult,
  healthResultTime,
  proxyLogs,
  loadingLogs,
  loadProxyData,
  refreshStatus,
  refreshChannelStats,
  saveConfig,
  toggleServer,
  testHealth,
  refreshModels,
  fetchLogs,
  goLogPage,
  clearLogs,
  copyProxyUrl,
  copyProxyKey,
  logPage,
  logPageSize,
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
const healthModalOpen = ref(false);
const gatewayModelsModalOpen = ref(false);
const currentMainTab = ref<"console" | "channels" | "logs">("console");
const channelModelsModalOpen = ref(false);
const channelSettingsDialogOpen = ref(false);
const clearLogsModalOpen = ref(false);
const clearingLogs = ref(false);
const selectedLogForDetail = ref<ProxyRequestLog | null>(null);
const selectedChannel = ref<ChannelConfig | null>(null);
const copiedModelId = ref<string | null>(null);

async function handleClearLogs(mode: "payload_only" | "all") {
  clearingLogs.value = true;
  try {
    await clearLogs(mode);
    clearLogsModalOpen.value = false;
  } finally {
    clearingLogs.value = false;
  }
}

let uptimeTicker: number | null = null;
let statusPollTimer: number | null = null;
let channelStatsTimer: number | null = null;

// 切换到「反代渠道」标签时刷新一次渠道使用统计
watch(currentMainTab, (tab) => {
  if (tab === "channels") {
    void refreshChannelStats();
  }
});

onMounted(async () => {
  await loadProxyData();
  await refreshModels();
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

  // 反代渠道标签处于激活且服务运行时，每 5s 刷新一次渠道使用统计
  channelStatsTimer = window.setInterval(async () => {
    if (currentMainTab.value === "channels" && proxyStatus.value.running) {
      await refreshChannelStats();
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

async function handleOpenHealthModal() {
  healthModalOpen.value = true;
  await testHealth();
}

function closeHealthModal() {
  healthModalOpen.value = false;
}

function handleOpenGatewayModelsModal() {
  gatewayModelsModalOpen.value = true;
  if (Object.keys(channelModels.value).length === 0) {
    refreshModels();
  }
}

function closeGatewayModelsModal() {
  gatewayModelsModalOpen.value = false;
}

async function handleOpenChannelModelsModal(channel: ChannelConfig) {
  selectedChannel.value = channel;
  channelModelsModalOpen.value = true;
  // 初始化勾选状态：白名单为 null（全部启用）时默认全选；否则仅勾选白名单中的模型
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
  if (modelsForChannel(channel.id).length === 0) {
    if (channel.siteId) {
      try {
        const cache = await runCommand<{ models?: { id: string }[] }>("get_site_model_cache", {
          siteId: channel.siteId,
        });
        if (Array.isArray(cache?.models) && cache.models.length > 0) {
          channelModels.value[channel.id] = cache.models.map((m) => m.id).filter(Boolean);
        }
      } catch {
        /* 忽略 */
      }
    }
    if (modelsForChannel(channel.id).length === 0) {
      void refreshModels();
    }
  }
}

function closeChannelModelsModal() {
  channelModelsModalOpen.value = false;
}

// —— 渠道「管理模型」弹窗：勾选启用白名单 ——
const channelModelSelection = ref<Record<string, boolean>>({});
/** true = 全选模式（等价未配置白名单，对外全部启用） */
const channelModelAllChecked = ref(true);

function isModelChecked(model: string): boolean {
  if (channelModelAllChecked.value) return true;
  return !!channelModelSelection.value[model];
}

/** 当前选中渠道的模型列表（弹窗数据源） */
function selectedChannelModels(): string[] {
  const channel = selectedChannel.value;
  return channel ? modelsForChannel(channel.id) : [];
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

async function saveChannelModelSelection() {
  const channel = selectedChannel.value;
  if (!channel) return;
  // 全选 = 不配置白名单（全部启用）；部分勾选 = 白名单；一个不勾 = 不暴露任何模型
  if (channelModelAllChecked.value) {
    channel.enabledModels = null;
  } else {
    channel.enabledModels = selectedChannelModels().filter((m) => isModelChecked(m));
  }
  const ok = await saveConfig(proxyConfig.value);
  if (ok) {
    showToast(`已更新「${channel.name}」渠道可用模型（${channelCheckedCount.value} 个已启用）`);
    channelModelsModalOpen.value = false;
  }
}

// —— 渠道「设置」弹窗：别名 / 内部代理池轮询 / 代理池固定通道 ——
interface ChannelSettingsDraft {
  alias: string;
  useProxyPool: boolean;
  useFixedProxy: boolean;
}

const channelSettingsTarget = ref<ChannelConfig | null>(null);
const channelSettingsDraft = ref<ChannelSettingsDraft>({
  alias: "",
  useProxyPool: false,
  useFixedProxy: false,
});
const channelSettingsError = ref("");

function handleOpenChannelSettingsDialog(channel: ChannelConfig) {
  channelSettingsTarget.value = channel;
  channelSettingsDraft.value = {
    alias: channelAlias(channel),
    useProxyPool: channel.useProxyPool,
    useFixedProxy: !!channel.useFixedProxy,
  };
  channelSettingsError.value = "";
  channelSettingsDialogOpen.value = true;
}

function closeChannelSettingsDialog() {
  channelSettingsDialogOpen.value = false;
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
  const err = validateAlias(channelSettingsDraft.value.alias, channel.id);
  if (err) {
    channelSettingsError.value = err;
    return;
  }
  channel.alias = channelSettingsDraft.value.alias.trim().toLowerCase();
  // 两种渠道的设置界面不同：站点转换渠道只有「代理池固定通道」，官方通道只有「内部代理池轮询」
  if (channel.siteId) {
    channel.useProxyPool = false;
    channel.useFixedProxy = channelSettingsDraft.value.useFixedProxy;
  } else {
    channel.useProxyPool = channelSettingsDraft.value.useProxyPool;
    channel.useFixedProxy = false;
  }
  const ok = await saveConfig(proxyConfig.value);
  if (ok) {
    showToast(
      `已更新「${channel.name}」渠道设置（别名 ${channel.alias}${channel.useFixedProxy ? " · 代理池固定通道" : channel.useProxyPool ? " · 代理池轮询" : ""}）`,
    );
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
const convertKeyLoading = ref(false);
/** 站点同步数据中读取到的原 Key 列表（与站点纪录关联，全部继承） */
const convertSiteKeys = ref<{ account: string; key: string }[]>([]);
/** 未读取到站点 Key 时的手动兜底输入 */
const convertManualKey = ref("");

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

function maskApiKey(key: string): string {
  const value = key.trim();
  if (!value) return "—";
  if (value.length <= 6) return `${"•".repeat(6)}`;
  const prefixLength = value.startsWith("sk-") ? 7 : 4;
  const suffixLength = Math.min(4, Math.max(2, Math.floor(value.length / 8)));
  if (value.length <= prefixLength + suffixLength) {
    return `${value.slice(0, 4)}${"•".repeat(6)}`;
  }
  return `${value.slice(0, prefixLength)}${"•".repeat(8)}${value.slice(-suffixLength)}`;
}

function openSiteConvertDialog() {
  convertSelectedSite.value = null;
  convertAlias.value = "";
  convertApiBaseUrl.value = "";
  convertAliasError.value = "";
  convertSiteKeys.value = [];
  convertManualKey.value = "";
  siteConvertDialogOpen.value = true;
  if (librarySites.value.length === 0) {
    void loadLibrary();
  }
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
  convertSiteKeys.value = [];
  convertManualKey.value = "";
  convertKeyLoading.value = true;
  try {
    const cache = await runCommand<{
      models?: { id: string }[];
      accounts?: { username?: string; accountName?: string; profileName?: string; keys?: string[] }[];
    }>("get_site_model_cache", { siteId: site.id });
    const accounts = Array.isArray(cache?.accounts) ? cache.accounts : [];
    const keys: { account: string; key: string }[] = [];
    for (const acc of accounts) {
      const accName = acc.username || acc.accountName || acc.profileName || "账号";
      for (const k of Array.isArray(acc.keys) ? acc.keys : []) {
        if (k.trim()) keys.push({ account: accName, key: k });
      }
    }
    convertSiteKeys.value = keys;
    if (Array.isArray(cache?.models) && cache.models.length > 0) {
      const modelIds = cache.models.map((m) => m.id).filter(Boolean);
      channelModels.value[`site_${site.id}`] = modelIds;
    }
  } catch {
    /* 忽略：无本地缓存时由用户手动填写 */
  } finally {
    convertKeyLoading.value = false;
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
  // 继承站点同步的全部原 Key（请求时自动轮换尝试）；未读取到用手动兜底输入
  const keys = convertSiteKeys.value.map((item) => item.key.trim()).filter(Boolean);
  const manualKey = convertManualKey.value.trim();
  const apiKeys = keys.length > 0 ? keys : manualKey ? [manualKey] : [];
  const channel: ChannelConfig = {
    id: `site_${site.id}`,
    name: site.name,
    description: `由站点「${site.name}」转换而来的反代渠道（继承站点原 Key ×${apiKeys.length || 1}）`,
    enabled: true,
    protocol: "openai",
    upstreamUrl: convertApiBaseUrl.value.trim(),
    apiKey: apiKeys[0] ?? "",
    apiKeys,
    // 站点转换渠道不支持「内部代理池轮询」，仅可在渠道设置中开启「代理池固定通道」
    useProxyPool: false,
    alias: convertAlias.value.trim().toLowerCase(),
    siteId: site.id,
    useFixedProxy: false,
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
  return channelStats.value[channel.id] ?? emptyChannelStats;
}

function channelSuccessRate(channel: ChannelConfig): string {
  const s = channelStatsFor(channel);
  if (s.totalRequests <= 0) return "—";
  return `${((s.successfulRequests / s.totalRequests) * 100).toFixed(1)}%`;
}

/** 有请求但成功率低于 90% 时标红提示 */
function channelSuccessRateBad(channel: ChannelConfig): boolean {
  const s = channelStatsFor(channel);
  return s.totalRequests > 0 && s.successfulRequests / s.totalRequests < 0.9;
}

const detailActiveTab = ref<"tokens" | "request" | "response" | "reasoning" | "meta" | "error">("tokens");

const parsedResponseBody = computed(() => parseResponseBody(selectedLogForDetail.value?.responseBody));

function openLogDetail(log: ProxyRequestLog) {
  selectedLogForDetail.value = log;
  if (log.statusCode >= 400) {
    detailActiveTab.value = "error";
  } else {
    detailActiveTab.value = "tokens";
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

function getEstimatedTps(log: ProxyRequestLog): string {
  const tokens = getOutputTextTokens(log);
  if (tokens <= 0) return "0.0";
  const genDur = log.ttftMs && log.durationMs > log.ttftMs ? log.durationMs - log.ttftMs : log.durationMs;
  if (genDur <= 0) return "0.0";
  return ((tokens / genDur) * 1000).toFixed(1);
}

/** 从任意文本中提取 <think> / <thought> / <thinking> / 流式前缀标签，分离出思考过程与干净正文 */
function extractThinkFromText(rawText: string): { thinking: string; content: string } {
  if (!rawText) return { thinking: "", content: "" };

  let text = rawText;
  const thinkingParts: string[] = [];

  // 1. 匹配后端流式旧标记：^\s*thinking\n([\s\S]*?)\n\s*response\s*\n\n([\s\S]*)$
  const markMatch = text.match(/^\s*thinking\n([\s\S]*?)\n\s*response\s*\n\n([\s\S]*)$/);
  if (markMatch) {
    if (markMatch[1].trim()) thinkingParts.push(markMatch[1].trim());
    text = markMatch[2].trim();
  }

  // 2. 匹配并提取所有 <think>...</think>, <thought>...</thought>, <thinking>...</thinking> 标签块（支持未闭合到结尾）
  const thinkTagRegex = /<(?:think|thought|thinking)>([\s\S]*?)(?:<\/(?:think|thought|thinking)>|$)/gi;
  let match: RegExpExecArray | null;
  while ((match = thinkTagRegex.exec(text)) !== null) {
    if (match[1] && match[1].trim()) {
      thinkingParts.push(match[1].trim());
    }
  }

  // 3. 移除文本中所有的思考标签及内容
  const cleanContent = text
    .replace(/<(?:think|thought|thinking)>[\s\S]*?(?:<\/(?:think|thought|thinking)>|$)/gi, "")
    .trim();

  return {
    thinking: thinkingParts.join("\n\n").trim(),
    content: cleanContent,
  };
}

/** 解析后端保存的响应正文，彻底剥离思考过程（reasoning / <think>）与最终正文（含工具调用） */
function parseResponseBody(body: string | undefined | null): { thinking: string; content: string } {
  if (!body) return { thinking: "", content: "" };

  // 先尝试 JSON 解析（针对非流式 JSON 响应）
  try {
    const parsed = JSON.parse(body);

    // 1. OpenAI 格式 (choices[0].message)
    const message = parsed?.choices?.[0]?.message;
    if (message && typeof message === "object") {
      const thinking = typeof message.reasoning_content === "string"
        ? message.reasoning_content
        : typeof message.reasoning === "string"
          ? message.reasoning
          : "";
      let rawContent = typeof message.content === "string" ? message.content : "";
      if (!rawContent && Array.isArray(message.content)) {
        rawContent = message.content
          .map((part: any) => (typeof part === "string" ? part : part?.text ?? ""))
          .join("");
      }

      // 提取工具调用
      const toolCalls = Array.isArray(message.tool_calls)
        ? message.tool_calls
            .map((tc: any) => {
              const name = tc?.function?.name || tc?.name || "未知工具";
              const args = tc?.function?.arguments || tc?.arguments || "{}";
              return `[工具调用] ${name}(${typeof args === "string" ? args : JSON.stringify(args)})`;
            })
            .join("\n\n")
        : "";

      const extracted = extractThinkFromText(rawContent);
      const combinedThinking = [thinking.trim(), extracted.thinking.trim()]
        .filter(Boolean)
        .join("\n\n");
      const combinedContent = [extracted.content.trim(), toolCalls.trim()]
        .filter(Boolean)
        .join("\n\n");

      return {
        thinking: combinedThinking,
        content: combinedContent,
      };
    }

    // 2. Anthropic messages 格式 (content: [{type: "thinking", ...}, {type: "text", ...}, {type: "tool_use", ...}])
    if (Array.isArray(parsed?.content)) {
      const parts = parsed.content.map((part: any) =>
        typeof part === "string" ? { type: "text", text: part } : part
      );
      const thinkingFromBlocks = parts
        .filter((p: any) => p?.type === "thinking" || p?.type === "redacted_thinking")
        .map((p: any) => p?.thinking ?? "")
        .join("\n\n");
      const rawText = parts
        .filter((p: any) => p?.type === "text")
        .map((p: any) => p?.text ?? "")
        .join("");
      const toolUses = parts
        .filter((p: any) => p?.type === "tool_use")
        .map((p: any) => `[工具调用] ${p?.name || "工具"}(${JSON.stringify(p?.input || {})})`)
        .join("\n\n");
      const topThinking = typeof parsed.thinking === "string" ? parsed.thinking : "";

      const extracted = extractThinkFromText(rawText);
      const combinedThinking = [topThinking.trim(), thinkingFromBlocks.trim(), extracted.thinking.trim()]
        .filter(Boolean)
        .join("\n\n");
      const combinedContent = [extracted.content.trim(), toolUses.trim()]
        .filter(Boolean)
        .join("\n\n");

      return {
        thinking: combinedThinking,
        content: combinedContent,
      };
    }

    // 3. Responses API 格式 (output: [{type: "reasoning", ...}, {type: "message", ...}, {type: "function_call", ...}])
    if (Array.isArray(parsed?.output)) {
      const thinkingFromOutput = parsed.output
        .filter((o: any) => o?.type === "reasoning")
        .map((o: any) =>
          Array.isArray(o?.summary)
            ? o.summary.map((s: any) => s?.text ?? "").join("")
            : typeof o?.summary === "string"
              ? o.summary
              : ""
        )
        .join("\n\n");
      const rawContent = parsed.output
        .filter((o: any) => o?.type === "message")
        .flatMap((o: any) => (Array.isArray(o?.content) ? o.content : []))
        .map((p: any) => (typeof p === "string" ? p : p?.text ?? ""))
        .join("");
      const functionCalls = parsed.output
        .filter((o: any) => o?.type === "function_call")
        .map((o: any) => `[工具调用] ${o?.name || "工具"}(${typeof o?.arguments === "string" ? o.arguments : JSON.stringify(o?.arguments || {})})`)
        .join("\n\n");

      const extracted = extractThinkFromText(rawContent);
      const combinedThinking = [thinkingFromOutput.trim(), extracted.thinking.trim()]
        .filter(Boolean)
        .join("\n\n");
      const combinedContent = [extracted.content.trim(), functionCalls.trim()]
        .filter(Boolean)
        .join("\n\n");

      return {
        thinking: combinedThinking,
        content: combinedContent,
      };
    }
  } catch {
    // 非 JSON 文本（例如流式拼接文本），进入通用文本提取
  }

  // 流式拼接文本或普通文本，统一运行提取函数剥离 <think> 等标签
  return extractThinkFromText(body);
}

async function copyText(text: string, label = "内容") {
  try {
    await navigator.clipboard.writeText(text);
    showToast(`已复制 ${label}`);
  } catch {
    showToast("复制失败", true);
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
    return "OpenCode Public 免费通道对单出口 IP 存在频次限制 (Rate limit exceeded)。解决方案：① 在对应渠道卡片开启「内部代理池轮询」，网关将自动通过系统代理池中 ≤1000ms 的多个健康节点轮询出口 IP，并在遭遇频次限制时自动秒级重试切换；② 切换其他免费模型（如 deepseek-v4-flash-free / nemotron-3-ultra-free / mimo-v2.5-free / laguna-s-2.1-free）；③ 稍候 30 秒后自动恢复。";
  }
  return "请根据下方原始错误响应体排查上游返回的具体原因。";
}

function formatCompactToken(val?: number | null): string {
  const num = Number(val ?? 0);
  if (!Number.isFinite(num) || num <= 0) return "0";
  if (num < 1000) return String(Math.round(num));
  if (num < 1_000_000) {
    const k = (num / 1000).toFixed(num < 100_000 ? 1 : 0).replace(/\.0$/, "");
    return `${k}k`;
  }
  const m = (num / 1_000_000).toFixed(num < 100_000_000 ? 1 : 0).replace(/\.0$/, "");
  return `${m}m`;
}

function formatSec(ms?: number | null): string {
  if (ms == null || !Number.isFinite(ms)) return "--";
  const sec = ms / 1000;
  if (sec < 0.01 && ms > 0) return "<0.01s";
  return `${sec.toFixed(2)}s`;
}

interface HealthCheckItem {
  name: string;
  endpoint: string;
  status: string;
  message: string;
  auth?: string;
}

const healthCheckList = computed<HealthCheckItem[]>(() => {
  if (!healthResult.value) return [];
  if (Array.isArray(healthResult.value)) {
    return healthResult.value as HealthCheckItem[];
  }
  if (healthResult.value.checks && Array.isArray(healthResult.value.checks)) {
    return healthResult.value.checks as HealthCheckItem[];
  }
  if (healthResult.value.error) {
    return [
      {
        name: "健康检查连接",
        endpoint: "/healthz",
        status: "error",
        message: String(healthResult.value.error),
        auth: "公开",
      },
    ];
  }
  return [
    {
      name: "本地服务",
      endpoint: "/healthz",
      status: "ok",
      message: JSON.stringify(healthResult.value),
      auth: "公开",
    },
  ];
});

const healthAllPassed = computed(() => {
  if (healthCheckList.value.length === 0) return false;
  return healthCheckList.value.every((item: HealthCheckItem) => item.status === "ok");
});

export interface ChannelModelGroup {
  channel: ChannelConfig;
  models: string[];
}

const gatewayGroupedModels = computed<ChannelModelGroup[]>(() => {
  const q = gatewaySearchQuery.value.trim().toLowerCase();
  return proxyConfig.value.channels.map((channel) => {
    // 该渠道对外可见的模型：按渠道拉取的模型再经白名单勾选结果过滤
    let models = filterChannelModels(channel, modelsForChannel(channel.id));
    const alias = channelAlias(channel);
    if (q) {
      models = models.filter(
        (m) => m.toLowerCase().includes(q) || `${alias}/${m}`.toLowerCase().includes(q)
      );
    }
    return {
      channel,
      models,
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

const filteredChannelModels = computed(() => {
  const q = channelSearchQuery.value.trim().toLowerCase();
  const list = selectedChannelModels();
  const alias = channelAlias(selectedChannel.value);
  if (!q) return list;
  return list.filter((m) => m.toLowerCase().includes(q) || `${alias}/${m}`.toLowerCase().includes(q));
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

/** 全量计数：来自后端全库计数，不随 filter/搜索变化，供标签固定显示 */
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
  if (num === undefined || num === null) return "0";
  return num.toLocaleString();
}

function formatUptime(seconds: number) {
  if (seconds < 60) return `${seconds} 秒`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分钟`;
  const hours = Math.floor(seconds / 3600);
  const mins = Math.floor((seconds % 3600) / 60);
  return `${hours} 小时 ${mins} 分`;
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
        <div class="mp-header-title-row">
          <span class="mp-icon" v-html="icons.repeat" />
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
          本地模型反向代理网关 · 对外提供标准兼容的 OpenAI 和 Anthropic API
        </p>
      </div>

      <div class="mp-header-actions">
        <!-- 服务配置弹窗触发按钮 -->
        <button
          type="button"
          class="mp-btn mp-btn-ghost"
          title="修改反代端口与访问密钥"
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

        <!-- 健康检查弹窗触发按钮 -->
        <button
          type="button"
          class="mp-btn mp-btn-ghost"
          :disabled="testingHealth"
          title="打开健康检查报告弹窗"
          @click="handleOpenHealthModal"
        >
          <span v-html="icons.pulse" />
          <span>{{ testingHealth ? "测试中…" : "健康检查" }}</span>
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

    <!-- 选项卡 1: 反代控制台 (Console / Overview) -->
    <div v-if="currentMainTab === 'console'" class="mp-tab-pane">
      <!-- 运行概览与连接参数 -->
      <section class="mp-card mp-overview-card">
        <div class="mp-card-header">
          <div class="mp-card-title-group">
            <span class="mp-card-icon" v-html="icons.activity" />
            <h2>服务连接与状态</h2>
          </div>
          <span class="mp-tag font-mono">http://127.0.0.1:{{ proxyStatus.port || proxyConfig.port }}</span>
        </div>

        <div class="mp-metrics-row">
          <div class="mp-metric-box">
            <label>服务端口</label>
            <strong class="font-mono text-brand">{{ proxyStatus.port || proxyConfig.port }}</strong>
          </div>
          <div class="mp-metric-box">
            <label>累计请求</label>
            <strong class="font-mono">{{ proxyStatus.totalRequests }}</strong>
          </div>
          <div class="mp-metric-box">
            <label>成功 / 失败</label>
            <strong class="font-mono text-success">{{ proxyStatus.successfulRequests }} <span class="text-muted">/</span> <span class="text-danger">{{ proxyStatus.failedRequests }}</span></strong>
          </div>
          <div class="mp-metric-box">
            <label>运行时长</label>
            <strong>{{ proxyStatus.running ? formatUptime(proxyStatus.uptimeSeconds) : '--' }}</strong>
          </div>
        </div>

        <!-- 累计 Token 使用统计 (累加总量) -->
        <div class="mp-metrics-tokens-panel">
          <div class="mp-mtp-head">
            <div class="mp-mtp-title-wrap">
              <span class="mp-mtp-icon">⚡</span>
              <span class="mp-mtp-title">累计 Token 使用统计 (累加总量)</span>
            </div>
            <div class="mp-mtp-total-badge" title="累计所有请求消耗的 Token 总量">
              <span class="text-muted">总计消耗:</span>
              <strong class="font-mono text-brand">{{ formatNumber(proxyStatus.totalTokens || 0) }}</strong>
              <span class="text-muted text-xs">Tokens</span>
            </div>
          </div>

          <div class="mp-mtp-grid">
            <div class="mp-mtp-card is-in" title="累计实际新增的输入 Token（总输入 − 缓存命中）">
              <div class="mp-mtp-card-head">
                <span class="mp-mtp-dot is-in" />
                <span class="mp-mtp-card-label">累计输入</span>
              </div>
              <strong class="mp-mtp-card-val font-mono">{{ formatNumber(Math.max(0, (proxyStatus.totalPromptTokens || 0) - (proxyStatus.totalCacheHitTokens || 0))) }}</strong>
              <small class="mp-mtp-card-sub text-muted">新增输入（已扣除缓存命中）</small>
            </div>

            <div class="mp-mtp-card is-hit" title="累计命中的前缀缓存 Token（极大节省延迟与算力）">
              <div class="mp-mtp-card-head">
                <span class="mp-mtp-dot is-hit" />
                <span class="mp-mtp-card-label">累计缓存命中</span>
              </div>
              <strong class="mp-mtp-card-val font-mono text-brand">{{ formatNumber(proxyStatus.totalCacheHitTokens || 0) }}</strong>
              <small class="mp-mtp-card-sub">
                <span v-if="(proxyStatus.totalPromptTokens || 0) > 0" class="text-brand font-semibold font-mono">
                  命中率 {{ Math.round(((proxyStatus.totalCacheHitTokens || 0) / (proxyStatus.totalPromptTokens || 1)) * 100) }}%
                </span>
                <span v-else class="text-muted">前缀缓存极速复用</span>
              </small>
            </div>

            <div class="mp-mtp-card is-think" title="累计模型深度思考与思维链 Token">
              <div class="mp-mtp-card-head">
                <span class="mp-mtp-dot is-think" />
                <span class="mp-mtp-card-label">累计思考推理</span>
              </div>
              <strong class="mp-mtp-card-val font-mono">{{ formatNumber(proxyStatus.totalReasoningTokens || 0) }}</strong>
              <small class="mp-mtp-card-sub text-muted">
                <span v-if="proxyStatus.totalRequests > 0" class="font-mono">
                  触发思维 {{ proxyStatus.totalReasoningRequests ?? 0 }} / {{ proxyStatus.totalRequests }} 次 · 占比 {{ Math.round(((proxyStatus.totalReasoningRequests || 0) / proxyStatus.totalRequests) * 100) }}%
                </span>
                <span v-else>深度思考推理消耗</span>
              </small>
            </div>

            <div class="mp-mtp-card is-out" title="累计纯文本输出 Token = 生成输出 − 思考推理（已剥离重复计数）">
              <div class="mp-mtp-card-head">
                <span class="mp-mtp-dot is-out" />
                <span class="mp-mtp-card-label">累计生成输出</span>
              </div>
              <strong class="mp-mtp-card-val font-mono">{{ formatNumber(Math.max(0, (proxyStatus.totalCompletionTokens || 0) - (proxyStatus.totalReasoningTokens || 0))) }}</strong>
              <small class="mp-mtp-card-sub text-muted">纯文本输出（已剥离思考推理）</small>
            </div>
          </div>
        </div>

        <div class="mp-endpoint-list">
          <div class="mp-endpoint-item">
            <span class="mp-ep-label">Base URL</span>
            <code class="mp-ep-code">{{ proxyStatus.url || `http://127.0.0.1:${proxyConfig.port}/v1` }}</code>
            <button
              type="button"
              class="mp-action-btn"
              title="复制 Base URL"
              @click="copyProxyUrl"
            >
              <span v-html="icons.copy" />
              <span>复制</span>
            </button>
          </div>

          <div class="mp-endpoint-item">
            <span class="mp-ep-label">API Key</span>
            <code class="mp-ep-code">
              {{ showKey ? (proxyConfig.apiKey || '(未配置密钥，免密直接访问)') : (proxyConfig.apiKey ? '••••••••••••••••••••' : '(未配置密钥，免密直接访问)') }}
            </code>
            <div class="mp-ep-btns">
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
                <span>复制</span>
              </button>
            </div>
          </div>
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
                <h3>{{ channel.name }}</h3>
                <span class="mp-card-tags">
                  <span class="mp-proto-tag">{{ channel.protocol.toUpperCase() }} 协议</span>
                  <span class="mp-alias-tag" :title="`英文别名：${channelAlias(channel)}（作为网关模型前缀）`">{{ channelAlias(channel) }}</span>
                  <span
                    v-if="channel.siteId"
                    class="mp-alias-tag is-site"
                    title="与站点库原纪录关联，使用该站点同步的原 Key"
                  >站点关联</span>
                  <span
                    v-if="(channel.apiKeys?.length ?? 0) > 1"
                    class="mp-alias-tag"
                    :title="`继承该站点 ${channel.apiKeys!.length} 个原 Key，请求时自动轮换`"
                  >Key ×{{ channel.apiKeys!.length }}</span>
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

          <p class="mp-channel-desc">
            {{ channel.description }}
          </p>

          <!-- 渠道快捷状态：网络模式 / 在线模型数（点击进入对应弹窗） -->
          <div class="mp-channel-meta-row">
            <button
              type="button"
              class="mp-channel-meta-chip"
              :class="{ 'is-active': channel.useProxyPool || channel.useFixedProxy }"
              title="点击打开渠道设置：网络模式与英文别名"
              @click="handleOpenChannelSettingsDialog(channel)"
            >
              <span v-html="icons.repeat" />
              <span>{{ channel.useFixedProxy ? "固定通道" : (channel.useProxyPool ? "代理池轮询" : "直连上游") }}</span>
            </button>
            <button
              type="button"
              class="mp-channel-meta-chip"
              :class="{ 'is-active': channel.enabledModels != null }"
              title="管理此渠道对外暴露的可用模型"
              @click="handleOpenChannelModelsModal(channel)"
            >
              <span v-html="icons.cpu" />
              <span>模型 {{ channelEnabledModelsCount(channel) }}</span>
            </button>
          </div>

          <div class="mp-channel-card-footer">
            <!-- 该渠道使用统计 -->
            <div class="mp-channel-stats">
              <div class="mp-channel-stat" title="该渠道累计请求次数">
                <span>累计请求</span>
                <strong class="font-mono">{{ channelStatsFor(channel).totalRequests }}</strong>
              </div>
              <div class="mp-channel-stat" title="该渠道累计成功请求占比">
                <span>成功率</span>
                <strong :class="{ 'is-bad': channelSuccessRateBad(channel) }">{{ channelSuccessRate(channel) }}</strong>
              </div>
              <div class="mp-channel-stat" title="该渠道累计消耗 Token（含缓存命中）">
                <span>累计 Token</span>
                <strong class="font-mono text-brand">{{ formatCompactToken(channelStatsFor(channel).totalTokens) }}</strong>
              </div>
            </div>

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

          <div class="mp-search-box flex-1">
            <span v-html="icons.search" />
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
                <th style="width: 135px;">方法与路径</th>
                <th style="width: 180px;">渠道 / 模型</th>
                <th style="width: 125px;">出网节点</th>
                <th style="width: 82px;">模式 / 速率</th>
                <th style="width: 70px;" class="mp-th-sortable" title="点击切换：升序 / 降序 / 默认排序" :class="{ 'is-sorted': logSortBy === 'status' }" @click="sortLogsBy('status')">状态<span class="mp-sort-arrow">{{ logSortIndicator('status') }}</span></th>
                <th style="min-width: 175px;" class="mp-th-sortable" title="点击切换：升序 / 降序 / 默认排序" :class="{ 'is-sorted': logSortBy === 'tokens' }" @click="sortLogsBy('tokens')">Token 统计<span class="mp-sort-arrow">{{ logSortIndicator('tokens') }}</span></th>
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
                    <span class="mp-log-date">{{ log.timestamp ? log.timestamp.split(' ')[0] : '--' }}</span>
                    <strong class="mp-log-time">{{ log.timestamp && log.timestamp.split(' ')[1] ? log.timestamp.split(' ')[1] : log.timestamp }}</strong>
                  </div>
                </td>
                <td>
                  <div class="mp-log-method-path">
                    <span class="mp-method-tag" :class="`method-${log.method.toLowerCase()}`">{{ log.method }}</span>
                    <code class="mp-path-code">{{ log.path }}</code>
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
                  <!-- Token 统计列：输入 / 缓存 / 输出 / 思考，两行两列等宽卡片 -->
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
      @click.self="configModalOpen = false"
    >
      <div class="mp-modal-box mp-modal-box-sm">
        <div class="mp-modal-header">
          <div class="mp-modal-title-group">
            <span class="mp-modal-icon" v-html="icons.settings" />
            <div>
              <h3 id="mp-config-modal-title">反代服务配置</h3>
              <small class="text-muted">设置本地反代端口与访问密钥</small>
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
            <label for="mp-port">监听端口</label>
            <input
              id="mp-port"
              v-model.number="proxyConfig.port"
              type="number"
              min="1024"
              max="65535"
              class="mp-input font-mono"
              placeholder="8088"
              required
            />
            <small>本地独立绑定的 HTTP 服务端口</small>
          </div>

          <div class="mp-field">
            <label for="mp-apikey">访问密钥</label>
            <input
              id="mp-apikey"
              v-model="proxyConfig.apiKey"
              type="text"
              class="mp-input font-mono"
              placeholder="留空表示免密访问，或自定义如 sk-proxy"
            />
            <small>客户端调用反代服务时的本地 Bearer Key 校验（留空则免密）</small>
          </div>

          <!-- 记录请求全文开关 (默认关闭) -->
          <div class="mp-field">
            <div class="mp-proxy-pool-row" style="padding: 0;">
              <label for="mp-record-body" style="font-size: 13px; font-weight: 600; color: var(--text); cursor: pointer;">记录请求全文</label>
              <label class="mp-switch-wrap" :title="proxyConfig.recordRequestBody ? '点击关闭请求全文记录' : '点击开启请求全文记录'">
                <input
                  id="mp-record-body"
                  v-model="proxyConfig.recordRequestBody"
                  type="checkbox"
                />
                <span class="mp-switch-round" />
              </label>
            </div>
            <small>开启后在请求日志中保存客户端传入的完整 JSON 报文（默认关闭以节省内存）</small>
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
      @click.self="closeLogDetail"
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
              <small class="text-muted">请求 ID: {{ selectedLogForDetail.id }} · 记录时间: {{ selectedLogForDetail.timestamp }}</small>
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
          <!-- 4 宫格 Token 消耗与缓存命中仪表盘 -->
          <div class="mp-token-dashboard-grid">
            <!-- 卡片 1: 输入 Tokens（新增 = 总输入 − 缓存命中） -->
            <div class="mp-token-card is-in" title="新增输入 = 总输入 − 缓存命中">
              <div class="mp-tc-head">
                <span class="mp-tc-label">📥 新增输入</span>
                <span class="mp-tc-badge" :class="(selectedLogForDetail.promptCacheHitTokens || 0) > 0 ? 'badge-hit' : ''">
                  {{ (selectedLogForDetail.promptCacheHitTokens || 0) > 0 ? `⚡ 命中率 ${getCacheHitRate(selectedLogForDetail)}%` : '未命中前缀' }}
                </span>
              </div>
              <div class="mp-tc-value font-mono">
                {{ getNewInputTokens(selectedLogForDetail) }}
              </div>
              <div class="mp-tc-foot font-mono">
                <span>⚡ 命中: <strong>{{ selectedLogForDetail.promptCacheHitTokens ?? 0 }}</strong></span>
                <span class="mp-tc-divider">·</span>
                <span>总输入: <strong>{{ selectedLogForDetail.promptTokens ?? 0 }}</strong></span>
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
              <div class="mp-tc-foot">
                <span v-if="(selectedLogForDetail.promptCacheHitTokens || 0) > 0" class="text-success font-semibold">
                  ✓ 已复用前缀缓存，节省计算
                </span>
                <span v-else class="text-muted">首轮请求或上下文前缀未命中</span>
              </div>
            </div>

            <!-- 卡片 3: 思考/推理 Tokens -->
            <div class="mp-token-card is-think">
              <div class="mp-tc-head">
                <span class="mp-tc-label">🧠 思考推理</span>
                <span class="mp-tc-badge">深度思考</span>
              </div>
              <div class="mp-tc-value font-mono" style="color: #8b5cf6;">
                {{ selectedLogForDetail.reasoningTokens ?? 0 }}
              </div>
              <div class="mp-tc-foot">
                <span v-if="(selectedLogForDetail.reasoningTokens || 0) > 0" class="text-success font-semibold">
                  ✓ 已触发深度思考
                </span>
                <span v-else class="text-muted">本轮未触发思考</span>
              </div>
            </div>

            <!-- 卡片 4: 输出 Tokens（纯文本 = 总输出 − 思考推理） -->
            <div class="mp-token-card is-out" title="生成输出（纯文本）= 总输出 − 思考推理，避免重复计数">
              <div class="mp-tc-head">
                <span class="mp-tc-label">📤 生成输出</span>
                <span class="mp-tc-badge" v-if="(selectedLogForDetail.reasoningTokens || 0) > 0">已剥离思考 {{ selectedLogForDetail.reasoningTokens }} Token</span>
                <span class="mp-tc-badge" v-else>纯文本输出</span>
              </div>
              <div class="mp-tc-value font-mono" style="color: #10b981;">
                {{ getOutputTextTokens(selectedLogForDetail) }}
              </div>
              <div class="mp-tc-foot font-mono">
                <span>生成速率: <strong>~{{ getEstimatedTps(selectedLogForDetail) }}</strong> Token/秒</span>
                <span v-if="selectedLogForDetail.ttftMs" class="mp-tc-divider">·</span>
                <span v-if="selectedLogForDetail.ttftMs">首字 <strong>{{ selectedLogForDetail.ttftMs }}ms</strong></span>
              </div>
            </div>

            <!-- 卡片 5: 总 Token（全宽汇总） -->
            <div class="mp-token-card is-total" title="本次请求全部 Token 消耗 = 新增输入 + 缓存命中 + 思考推理 + 生成输出（各分项去重后相加）">
              <div class="mp-tc-head">
                <span class="mp-tc-label">🧮 总 Token 消耗</span>
                <span class="mp-tc-badge badge-total">全部用量汇总</span>
              </div>
              <div class="mp-tc-value font-mono text-brand">
                {{ selectedLogForDetail.totalTokens ?? ((selectedLogForDetail.promptTokens || 0) + (selectedLogForDetail.completionTokens || 0)) }}
              </div>
              <div class="mp-tc-foot font-mono">
                <span>= 新增输入 <strong>{{ getNewInputTokens(selectedLogForDetail) }}</strong></span>
                <span class="mp-tc-divider">+</span>
                <span>缓存命中 <strong>{{ selectedLogForDetail.promptCacheHitTokens ?? 0 }}</strong></span>
                <span class="mp-tc-divider">+</span>
                <span>思考推理 <strong>{{ selectedLogForDetail.reasoningTokens ?? 0 }}</strong></span>
                <span class="mp-tc-divider">+</span>
                <span>输出生成 <strong>{{ getOutputTextTokens(selectedLogForDetail) }}</strong></span>
              </div>
            </div>
          </div>

          <!-- 选项卡导航栏 (Tabs) -->
          <div class="mp-detail-tabs-bar">
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
              type="button"
              class="mp-detail-tab-btn"
              :class="{ active: detailActiveTab === 'tokens' }"
              @click="detailActiveTab = 'tokens'"
            >
              <span v-html="icons.chart" />
              <span>📊 Token 与网络概览</span>
            </button>

            <button
              type="button"
              class="mp-detail-tab-btn"
              :class="{ active: detailActiveTab === 'request' }"
              @click="detailActiveTab = 'request'"
            >
              <span v-html="icons.code" />
              <span>📝 客户端请求全文</span>
            </button>

            <button
              type="button"
              class="mp-detail-tab-btn"
              :class="{ active: detailActiveTab === 'response' }"
              @click="detailActiveTab = 'response'"
            >
              <span v-html="icons.message" />
              <span>💬 响应全文</span>
            </button>

            <button
              type="button"
              class="mp-detail-tab-btn"
              :class="{ active: detailActiveTab === 'reasoning' }"
              @click="detailActiveTab = 'reasoning'"
            >
              <span v-html="icons.message" />
              <span>🧠 思考过程</span>
            </button>

            <button
              type="button"
              class="mp-detail-tab-btn"
              :class="{ active: detailActiveTab === 'meta' }"
              @click="detailActiveTab = 'meta'"
            >
              <span v-html="icons.settings" />
              <span>🔍 路由与调用元数据</span>
            </button>
          </div>

          <!-- 选项卡内容 1: 错误诊断 -->
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
                <button
                  type="button"
                  class="mp-action-btn"
                  title="复制原始错误报文"
                  @click="copyText(selectedLogForDetail.responseBody, '错误响应报文')"
                >
                  <span v-html="icons.copy" />
                  <span>复制</span>
                </button>
              </div>
              <pre class="mp-lrb-pre font-mono">{{ selectedLogForDetail.responseBody }}</pre>
            </div>
          </div>

          <!-- 选项卡内容 2: Token 与网络概览 -->
          <div v-if="detailActiveTab === 'tokens'" class="mp-detail-tab-content">
            <div class="mp-log-detail-grid">
              <div class="mp-ld-item">
                <label>📥 新增输入 Token</label>
                <div class="mp-ld-val font-mono">
                  <span>{{ getNewInputTokens(selectedLogForDetail) }} Tokens</span>
                  <small class="text-muted font-mono" style="display: block;">总输入 {{ selectedLogForDetail.promptTokens ?? 0 }} − 缓存命中 {{ selectedLogForDetail.promptCacheHitTokens ?? 0 }}</small>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>⚡ 缓存命中 Token</label>
                <div class="mp-ld-val font-mono text-brand">
                  <span>{{ selectedLogForDetail.promptCacheHitTokens ?? 0 }} Tokens ({{ getCacheHitRate(selectedLogForDetail) }}%)</span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>🧠 思考推理 Token</label>
                <div class="mp-ld-val font-mono" style="color: #8b5cf6;">
                  <span>{{ selectedLogForDetail.reasoningTokens ?? 0 }} Tokens{{ (selectedLogForDetail.reasoningTokens || 0) > 0 ? '（已触发深度思考）' : '（未触发）' }}</span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>📤 输出生成 Token</label>
                <div class="mp-ld-val font-mono text-success">
                  <span>{{ getOutputTextTokens(selectedLogForDetail) }} Tokens</span>
                  <small class="text-muted font-mono" style="display: block;">总输出 {{ selectedLogForDetail.completionTokens ?? 0 }} − 思考推理 {{ selectedLogForDetail.reasoningTokens ?? 0 }}（纯文本，已剥离重复计数）</small>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>首字响应耗时</label>
                <div class="mp-ld-val font-mono text-brand">
                  <span>{{ selectedLogForDetail.ttftMs ? `${selectedLogForDetail.ttftMs} ms` : '同步即达' }}</span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>响应总耗时</label>
                <div class="mp-ld-val font-mono">
                  <span>{{ selectedLogForDetail.durationMs }} ms</span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>总 Token 消耗</label>
                <div class="mp-ld-val font-mono">
                  <strong>{{ selectedLogForDetail.totalTokens ?? ((selectedLogForDetail.promptTokens || 0) + (selectedLogForDetail.completionTokens || 0)) }} Tokens</strong>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>平均生成速率</label>
                <div class="mp-ld-val font-mono">
                  <span>~{{ getEstimatedTps(selectedLogForDetail) }} Token/秒</span>
                </div>
              </div>
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
                <button
                  v-if="selectedLogForDetail.requestBody"
                  type="button"
                  class="mp-action-btn"
                  title="一键复制客户端完整请求报文"
                  @click="copyText(selectedLogForDetail.requestBody, '请求全文')"
                >
                  <span v-html="icons.copy" />
                  <span>复制全文</span>
                </button>
              </div>
              <pre class="mp-lrb-pre font-mono">{{ selectedLogForDetail.requestBody || '// 未开启「记录请求全文」开关\n// 默认不保存请求全文以节省内存与存储空间。如需在日志中查看客户端发送的完整请求 JSON，请在右上角「服务配置」中开启「记录请求全文」开关。' }}</pre>
            </div>
          </div>

          <!-- 选项卡内容 4: 响应全文 -->
          <div v-if="detailActiveTab === 'response'" class="mp-detail-tab-content">
            <div class="mp-log-raw-box">
              <div class="mp-lrb-header">
                <label>服务端最终响应正文</label>
                <button
                  v-if="parsedResponseBody.content"
                  type="button"
                  class="mp-action-btn"
                  title="一键复制服务端响应全文"
                  @click="copyText(parsedResponseBody.content, '响应全文')"
                >
                  <span v-html="icons.copy" />
                  <span>复制全文</span>
                </button>
              </div>
              <pre class="mp-lrb-pre font-mono">{{ parsedResponseBody.content || '未捕获或流式尚未产生正文内容' }}</pre>
            </div>
          </div>

          <!-- 选项卡内容 5: 思考过程 -->
          <div v-if="detailActiveTab === 'reasoning'" class="mp-detail-tab-content">
            <div class="mp-log-raw-box">
              <div class="mp-lrb-header">
                <label>模型思考推理过程（reasoning）</label>
                <button
                  v-if="parsedResponseBody.thinking"
                  type="button"
                  class="mp-action-btn"
                  title="一键复制思考过程"
                  @click="copyText(parsedResponseBody.thinking, '思考过程')"
                >
                  <span v-html="icons.copy" />
                  <span>复制全文</span>
                </button>
              </div>
              <pre class="mp-lrb-pre font-mono">{{ parsedResponseBody.thinking || '该请求未产生思考过程（非推理模型或无 reasoning 输出）' }}</pre>
            </div>
          </div>

          <!-- 选项卡内容 6: 元数据与参数 -->
          <div v-if="detailActiveTab === 'meta'" class="mp-detail-tab-content">
            <div class="mp-log-detail-grid">
              <div class="mp-ld-item">
                <label>请求方法与路径</label>
                <div class="mp-ld-val">
                  <span class="mp-method-tag" :class="`method-${selectedLogForDetail.method.toLowerCase()}`">{{ selectedLogForDetail.method }}</span>
                  <code class="font-mono">{{ selectedLogForDetail.path }}</code>
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
                <label>响应耗时</label>
                <div class="mp-ld-val font-mono">
                  <span>{{ selectedLogForDetail.durationMs }} ms</span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>传输协议模式</label>
                <div class="mp-ld-val">
                  <span>{{ selectedLogForDetail.stream ? "流式实时传输" : "同步响应" }}</span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>请求唯一标识</label>
                <div class="mp-ld-val font-mono text-muted text-xs">
                  <span>{{ selectedLogForDetail.id }}</span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>请求时间戳</label>
                <div class="mp-ld-val font-mono text-muted">
                  <span>{{ selectedLogForDetail.timestamp }}</span>
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
      @click.self="closeGatewayModelsModal"
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
              <span v-html="icons.search" />
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
                    <span class="mp-proto-tag">{{ group.channel.protocol.toUpperCase() }} 协议</span>
                    <span class="mp-group-count-badge">{{ group.models.length }} 个模型</span>
                    <span
                      v-if="group.channel.enabledModels != null"
                      class="mp-group-count-badge is-filtered"
                      title="该渠道已在「管理模型」中勾选白名单，未勾选的模型不在此展示"
                    >已管理</span>
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
                    </div>
                    <div class="mp-mec-id-row">
                      <span class="mp-mec-id-label">调用 ID:</span>
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
      @click.self="closeChannelModelsModal"
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
          <div class="mp-models-modal-toolbar">
            <div class="mp-search-box flex-1">
              <span v-html="icons.search" />
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
              :disabled="fetchingModels"
              title="从该上游渠道重新获取模型列表"
              @click="refreshModels"
            >
              <span :class="{ 'mp-spin': fetchingModels }" v-html="icons.restore" />
              <span>{{ fetchingModels ? "正在拉取…" : "刷新上游模型" }}</span>
            </button>
          </div>

          <!-- 可勾选的模型卡片矩阵 -->
          <div class="mp-model-cards-grid">
            <div
              v-for="model in filteredChannelModels"
              :key="model"
              class="mp-model-elegant-card"
              :class="{ 'is-selected': isModelChecked(model) }"
              role="checkbox"
              :aria-checked="isModelChecked(model)"
              :tabindex="0"
              :title="isModelChecked(model) ? `已启用：${model}` : `未启用：${model}`"
              @click="toggleModel(model)"
              @keydown.enter.space.prevent="toggleModel(model)"
            >
              <div class="mp-mec-check" aria-hidden="true">
                <span v-html="isModelChecked(model) ? icons.check : ''" />
              </div>
              <div class="mp-mec-left">
                <div class="mp-mec-title-row">
                  <span class="mp-model-free-badge">{{ channelAlias(selectedChannel) }}</span>
                  <span class="mp-model-name-title">{{ model }}</span>
                </div>
                <div class="mp-mec-id-row">
                  <span class="mp-mec-id-label">网关 ID:</span>
                  <code class="mp-mec-id-code">{{ channelAlias(selectedChannel) }}/{{ model }}</code>
                </div>
              </div>
            </div>
          </div>

          <div v-if="filteredChannelModels.length === 0" class="mp-empty-box">
            <div class="mp-empty-icon" v-html="icons.shield" />
            <p v-if="fetchingModels">正在从上游渠道拉取最新模型…</p>
            <p v-else-if="channelSearchQuery">未检索到匹配的模型</p>
            <p v-else>暂无模型数据，请先点击「刷新上游模型」</p>
          </div>
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
      @click.self="closeChannelSettingsDialog"
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
              @input="channelSettingsError = validateAlias(channelSettingsDraft.alias, channelSettingsTarget?.id)"
            />
            <p v-if="channelSettingsError" class="mp-settings-error">{{ channelSettingsError }}</p>
            <p v-else class="mp-settings-hint">所有渠道别名不能重复（含 opencode）</p>
          </div>

          <!-- 内部代理池轮询（仅官方免费通道，如 OpenCode） -->
          <div v-if="!channelSettingsTarget?.siteId" class="mp-proxy-pool-box">
            <div class="mp-proxy-pool-row">
              <div class="mp-proxy-pool-label">
                <span class="mp-pp-icon" v-html="icons.repeat" />
                <span>内部代理池轮询</span>
              </div>
              <label class="mp-switch-wrap" :title="channelSettingsDraft.useProxyPool ? '点击关闭代理池轮询' : '点击开启内部代理池轮询'">
                <input
                  v-model="channelSettingsDraft.useProxyPool"
                  type="checkbox"
                />
                <span class="mp-switch-round" />
              </label>
            </div>

            <div v-if="channelSettingsDraft.useProxyPool" class="mp-proxy-pool-status is-active">
              <span class="mp-status-dot-sm" />
              <span>优先直连，报错自动按速度切换至代理池 <strong>≤ 1000ms</strong> 节点（粘性保持）</span>
            </div>
            <div v-else class="mp-proxy-pool-status is-inactive">
              <span>当前网络模式：直接连接 (直连上游通道)</span>
            </div>
          </div>

          <!-- 代理池固定通道（仅站点转换渠道） -->
          <div v-if="channelSettingsTarget?.siteId" class="mp-proxy-pool-box">
            <div class="mp-proxy-pool-row">
              <div class="mp-proxy-pool-label">
                <span class="mp-pp-icon" v-html="icons.shield" />
                <span>代理池固定通道</span>
              </div>
              <label class="mp-switch-wrap" :title="channelSettingsDraft.useFixedProxy ? '点击关闭固定通道' : '点击开启代理池固定通道'">
                <input
                  v-model="channelSettingsDraft.useFixedProxy"
                  type="checkbox"
                />
                <span class="mp-switch-round" />
              </label>
            </div>

            <div v-if="channelSettingsDraft.useFixedProxy" class="mp-proxy-pool-status is-active">
              <span class="mp-status-dot-sm" />
              <span>始终经代理池出口节点转发（不直连），适合直连被限制的站点渠道</span>
            </div>
            <div v-else class="mp-proxy-pool-status is-inactive">
              <span>默认出口：直连上游通道</span>
            </div>
          </div>
        </div>

        <div class="mp-modal-footer">
          <div class="mp-modal-footer-hint text-muted text-xs">
            <span>💡 遇到上游频次限制或连接错误时，网关会自动切换代理池出口节点重试</span>
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
      @click.self="closeDeleteChannelModal"
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
      @click.self="closeSiteConvertDialog"
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
          <!-- 站点列表 -->
          <div class="mp-site-list">
            <button
              v-for="site in convertibleSites"
              :key="site.id"
              type="button"
              class="mp-site-item"
              :class="{ 'is-selected': convertSelectedSite?.id === site.id }"
              @click="selectConvertSite(site)"
            >
              <span class="mp-site-item-name">{{ site.name }}</span>
              <span class="mp-site-item-url font-mono">{{ site.apiBaseUrl }}</span>
            </button>
            <div v-if="convertibleSites.length === 0" class="mp-empty-box">
              <div class="mp-empty-icon" v-html="icons.globe" />
              <p>暂无「在用且存活」的站点可转换</p>
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

            <!-- 站点原 Key：全部继承，无需选择 -->
            <div class="mp-settings-field">
              <div class="mp-settings-field-head">
                <span>站点 API Key（继承全部）</span>
                <small v-if="convertKeyLoading" class="text-muted">正在读取站点 Key…</small>
              </div>
              <div v-if="convertSiteKeys.length > 0" class="mp-convert-keys">
                <span
                  v-for="(item, i) in convertSiteKeys"
                  :key="i"
                  class="mp-convert-key-chip"
                  :title="`${item.account} 的原 Key`"
                >{{ maskApiKey(item.key) }}</span>
              </div>
              <input
                v-else
                v-model="convertManualKey"
                type="password"
                class="mp-settings-input"
                placeholder="未读取到站点 Key，可手动填写（留空则发送 Bearer public）"
              />
              <p v-if="convertSiteKeys.length > 0" class="mp-settings-hint">已继承该站点 {{ convertSiteKeys.length }} 个原 Key，请求时自动轮换尝试</p>
              <p v-else-if="convertKeyLoading" class="mp-settings-hint">正在从本地同步数据读取站点原 Key…</p>
              <p v-else class="mp-settings-hint">未在本地同步数据中找到该站点的 Key</p>
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

    <!-- 健康检查与服务状态弹出框 (宽屏合并弹窗) -->
    <div
      v-if="healthModalOpen"
      class="mp-modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-labelledby="mp-health-modal-title"
      @click.self="closeHealthModal"
    >
      <div class="mp-modal-box mp-modal-box-extra-wide">
        <div class="mp-modal-header">
          <div class="mp-modal-title-group">
            <span class="mp-modal-icon" v-html="icons.pulse" />
            <div>
              <h3 id="mp-health-modal-title">服务连接状态与健康检查报告</h3>
              <small class="text-muted">检测时间：{{ healthResultTime || '刚刚' }} · 端点：http://127.0.0.1:{{ proxyStatus.port || proxyConfig.port }}</small>
            </div>
          </div>
          <button
            type="button"
            class="mp-modal-close"
            title="关闭弹窗 (Esc)"
            @click="closeHealthModal"
          >
            <span v-html="icons.close" />
          </button>
        </div>

        <div class="mp-modal-body">
          <!-- 运行概览指标块 -->
          <div class="mp-metrics-row">
            <div class="mp-metric-box">
              <label>服务端口</label>
              <strong class="font-mono text-brand">{{ proxyStatus.port || proxyConfig.port }}</strong>
            </div>
            <div class="mp-metric-box">
              <label>累计请求</label>
              <strong class="font-mono">{{ proxyStatus.totalRequests }}</strong>
            </div>
            <div class="mp-metric-box">
              <label>成功 / 失败</label>
              <strong class="font-mono text-success">{{ proxyStatus.successfulRequests }} <span class="text-muted">/</span> <span class="text-danger">{{ proxyStatus.failedRequests }}</span></strong>
            </div>
            <div class="mp-metric-box">
              <label>运行时长</label>
              <strong>{{ proxyStatus.running ? formatUptime(proxyStatus.uptimeSeconds) : '--' }}</strong>
            </div>
          </div>

          <!-- Base URL & API Key 快捷条目 -->
          <div class="mp-endpoint-list">
            <div class="mp-endpoint-item">
              <span class="mp-ep-label">Base URL</span>
              <code class="mp-ep-code">{{ proxyStatus.url || `http://127.0.0.1:${proxyConfig.port}/v1` }}</code>
              <button
                type="button"
                class="mp-action-btn"
                title="复制 Base URL"
                @click="copyProxyUrl"
              >
                <span v-html="icons.copy" />
                <span>复制</span>
              </button>
            </div>

            <div class="mp-endpoint-item">
              <span class="mp-ep-label">API Key</span>
              <code class="mp-ep-code">
                {{ showKey ? (proxyConfig.apiKey || '(未配置密钥，免密直接访问)') : (proxyConfig.apiKey ? '••••••••••••••••••••' : '(未配置密钥，免密直接访问)') }}
              </code>
              <div class="mp-ep-btns">
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
                  <span>复制</span>
                </button>
              </div>
            </div>
          </div>

          <!-- 诊断状态横幅 -->
          <div
            class="mp-health-summary-banner"
            :class="healthAllPassed ? 'is-success' : 'is-warning'"
          >
            <span
              class="mp-summary-icon"
              v-html="healthAllPassed ? icons.check : icons.alert"
            />
            <div class="mp-summary-text">
              <strong>{{ healthAllPassed ? '所有端点与通道运行正常' : '部分端点存在提示或异常' }}</strong>
              <p v-if="healthCheckList.length > 0">共完成 {{ healthCheckList.length }} 项端点与通道连通性检测</p>
              <p v-else>正在连接服务并获取健康检查数据…</p>
            </div>
          </div>

          <!-- 详细检测数组表格 -->
          <div class="mp-health-table-wrap">
            <table class="mp-health-table">
              <thead>
                <tr>
                  <th style="width: 220px;">检测项</th>
                  <th style="width: 200px;">端点 / 路径</th>
                  <th style="width: 90px;">状态</th>
                  <th style="width: 100px;">鉴权</th>
                  <th>检测详情与协议说明</th>
                </tr>
              </thead>
              <tbody>
                <tr
                  v-for="(item, idx) in healthCheckList"
                  :key="idx"
                  class="mp-health-row"
                >
                  <td class="font-medium text-text">{{ item.name }}</td>
                  <td><code class="mp-ep-code-sm">{{ item.endpoint }}</code></td>
                  <td>
                    <span
                      class="mp-status-tag"
                      :class="{
                        'tag-ok': item.status === 'ok',
                        'tag-warn': item.status === 'warning' || item.status === 'disabled',
                        'tag-err': item.status === 'error',
                      }"
                    >
                      <span class="mp-status-dot-sm" />
                      <span>{{ item.status === 'ok' ? '正常' : (item.status === 'warning' ? '提示' : '异常') }}</span>
                    </span>
                  </td>
                  <td>
                    <span class="mp-auth-tag">{{ item.auth || '--' }}</span>
                  </td>
                  <td class="text-muted text-sm">{{ item.message }}</td>
                </tr>
                <tr v-if="healthCheckList.length === 0">
                  <td colspan="5" class="text-center py-6 text-muted">
                    {{ testingHealth ? '正在执行健康检查…' : '暂无检查数据' }}
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        </div>

        <div class="mp-modal-footer">
          <button
            type="button"
            class="mp-btn mp-btn-ghost"
            :disabled="testingHealth"
            @click="testHealth"
          >
            <span v-html="icons.restore" />
            <span>{{ testingHealth ? "正在重新测试…" : "重新检测" }}</span>
          </button>
          <button
            type="button"
            class="mp-btn mp-btn-primary"
            @click="closeHealthModal"
          >
            确定
          </button>
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
      @click.self="clearLogsModalOpen = false"
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
                    <strong class="text-danger">彻底清空全部记录与统计</strong>
                    <span class="mp-coc-badge is-danger">全部删除</span>
                  </div>
                </div>
              </div>
              <p class="mp-coc-desc">
                从本地 SQLite 数据库中永久删除所有反代调用日志记录，并将控制台运行时计数器与 Token 统计归零。
              </p>
              <div class="mp-coc-action">
                <button
                  type="button"
                  class="mp-btn mp-btn-danger mp-btn-sm"
                  :disabled="clearingLogs"
                  @click="handleClearLogs('all')"
                >
                  <span>{{ clearingLogs ? "清空中…" : "彻底清空所有记录" }}</span>
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
  padding: 20px 24px 40px;
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

/* 顶栏 */
.mp-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 16px;
  width: 100%;
  box-sizing: border-box;
  flex-wrap: wrap;
}

.mp-header-title-row {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

.mp-header-title-row .mp-icon {
  width: 28px;
  height: 28px;
  color: var(--brand);
  display: inline-flex;
}

.mp-header-title-row .mp-icon :deep(svg) {
  width: 100%;
  height: 100%;
}

.mp-header h1 {
  font-size: 24px;
  font-weight: 800;
  color: var(--text);
  margin: 0;
  letter-spacing: -0.02em;
}

.mp-subtitle {
  margin: 6px 0 0;
  font-size: 13px;
  color: var(--muted);
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

.mp-btn-badge-danger {
  background: color-mix(in srgb, var(--danger) 15%, transparent);
  color: var(--danger);
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
  /* 抵消 .mp-page 底部留白，让分页条真正贴到视口底边 */
  margin-bottom: -40px;
}

.mp-logs-page-view .mp-logs-summary-bar {
  flex-shrink: 0;
}

.mp-logs-main-card .app-table-pagination {
  flex-shrink: 0;
}

/* 请求日志全屏视图样式 */
.mp-logs-summary-bar {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
  width: 100%;
  box-sizing: border-box;
  flex-wrap: wrap;
}

.mp-lsb-left {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

.mp-lsb-badge-group {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.mp-lsb-pill {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  font-weight: 600;
  color: var(--brand-deep);
  background: var(--brand-soft);
  padding: 3px 10px;
  border-radius: 999px;
  border: 1px solid color-mix(in srgb, var(--brand) 25%, transparent);
}

.mp-lsb-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--brand);
}

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

.mp-tag {
  font-size: 11.5px;
  padding: 2px 8px;
  border-radius: var(--r-xs);
  background: var(--surface-soft);
  color: var(--muted);
  border: 1px solid var(--line);
  font-weight: 600;
}

/* 指标卡片 */
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

/* 累计 Token 消耗与缓存统计面板 */
.mp-metrics-tokens-panel {
  background: var(--surface-soft);
  border: 1px solid color-mix(in srgb, var(--brand) 20%, var(--line));
  border-radius: var(--r-md);
  padding: 12px 14px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.mp-mtp-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}

.mp-mtp-title-wrap {
  display: flex;
  align-items: center;
  gap: 6px;
}

.mp-mtp-icon {
  color: var(--brand);
  font-size: 14px;
}

.mp-mtp-title {
  font-size: 12px;
  font-weight: 700;
  color: var(--text);
}

.mp-mtp-total-badge {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  background: var(--brand-soft);
  border: 1px solid color-mix(in srgb, var(--brand) 25%, transparent);
  padding: 2px 8px;
  border-radius: 999px;
  font-size: 11.5px;
}

.mp-mtp-total-badge strong {
  font-size: 13px;
}

.mp-mtp-grid {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 8px;
}

.mp-mtp-card {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-sm, 6px);
  padding: 8px 10px;
  display: flex;
  flex-direction: column;
  gap: 2px;
  transition: all 0.15s ease;
}

.mp-mtp-card:hover {
  border-color: color-mix(in srgb, var(--brand) 40%, var(--line));
  transform: translateY(-1px);
}

.mp-mtp-card-head {
  display: flex;
  align-items: center;
  gap: 5px;
}

.mp-mtp-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  flex-shrink: 0;
}

.mp-mtp-dot.is-in {
  background: #3b82f6;
}

.mp-mtp-dot.is-hit {
  background: var(--brand);
}

.mp-mtp-dot.is-think {
  background: #8b5cf6;
}

.mp-mtp-dot.is-out {
  background: #10b981;
}

.mp-mtp-card-label {
  font-size: 10.5px;
  font-weight: 650;
  color: var(--muted);
}

.mp-mtp-card-val {
  font-size: 15px;
  font-weight: 750;
  color: var(--text);
}

.mp-mtp-card.is-hit .mp-mtp-card-val {
  color: var(--brand-deep);
}

.mp-mtp-card-sub {
  font-size: 10.5px;
  line-height: 1.2;
}

@media (max-width: 860px) {
  .mp-mtp-grid {
    grid-template-columns: repeat(2, 1fr);
  }
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
  justify-content: space-between;
  gap: 10px;
  padding: 10px 12px;
  border-radius: var(--r-md, 8px);
  background: var(--surface-soft);
  border: 1px solid var(--line);
  cursor: pointer;
  transition: all 0.15s ease;
}

.mp-site-item:hover {
  border-color: var(--line-strong);
  background: var(--surface);
}

.mp-site-item.is-selected {
  border-color: var(--brand);
  background: var(--brand-soft);
}

.mp-site-item-name {
  font-size: 13px;
  font-weight: 650;
  color: var(--text);
}

.mp-site-item-url {
  font-size: 11.5px;
  color: var(--muted);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 55%;
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

.mp-channel-desc {
  font-size: 12.5px;
  color: var(--muted);
  margin: 0;
  line-height: 1.45;
  min-height: 36px;
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

/* 渠道快捷状态行 */
.mp-channel-meta-row {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
}

.mp-channel-meta-chip {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 4px 10px;
  border-radius: 999px;
  background: var(--surface-soft);
  border: 1px solid var(--line);
  color: var(--muted);
  font-size: 11.5px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.15s ease;
}

.mp-channel-meta-chip :deep(svg) {
  width: 12px;
  height: 12px;
}

.mp-channel-meta-chip:hover {
  color: var(--text);
  border-color: var(--line-strong);
}

.mp-channel-meta-chip.is-active {
  color: var(--brand-deep);
  border-color: var(--brand);
  background: var(--brand-soft);
}

.mp-channel-card-footer {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding-top: 12px;
  border-top: 1px solid var(--line);
}

/* 渠道使用统计：三格均分 */
.mp-channel-stats {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 8px;
}

.mp-channel-stat {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.mp-channel-stat span {
  font-size: 11px;
  color: var(--muted);
}

.mp-channel-stat strong {
  font-size: 13px;
  color: var(--text);
}

.mp-channel-stat strong.is-bad {
  color: var(--danger, #e5484d);
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
}

.mp-search-box :deep(svg) {
  position: absolute;
  left: 12px;
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
  position: static;
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

/* 健康检查弹窗样式 */
.mp-health-summary-banner {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px 16px;
  border-radius: var(--r-md);
  border: 1px solid transparent;
}

.mp-health-summary-banner.is-success {
  background: var(--brand-soft);
  border-color: color-mix(in srgb, var(--brand) 40%, transparent);
}

.mp-health-summary-banner.is-warning {
  background: color-mix(in srgb, var(--danger) 10%, transparent);
  border-color: color-mix(in srgb, var(--danger) 40%, transparent);
}

.mp-summary-icon {
  width: 24px;
  height: 24px;
  display: inline-flex;
  flex-shrink: 0;
}

.mp-health-summary-banner.is-success .mp-summary-icon {
  color: var(--brand);
}

.mp-health-summary-banner.is-warning .mp-summary-icon {
  color: var(--danger);
}

.mp-summary-text strong {
  font-size: 13.5px;
  color: var(--text);
  display: block;
}

.mp-summary-text p {
  margin: 2px 0 0;
  font-size: 12px;
  color: var(--muted);
}

.mp-health-table-wrap,
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

.mp-health-table,
.mp-logs-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 12.5px;
  text-align: left;
}

.mp-health-table th,
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

.mp-health-table td,
.mp-logs-table td {
  padding: 10px 12px;
  border-bottom: 1px solid var(--line);
  vertical-align: middle;
}

.mp-health-row:last-child td {
  border-bottom: none;
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
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
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
  align-items: center;
  gap: 6px;
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
  font-size: 11.5px;
  color: var(--text);
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
  word-break: break-all;
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

.mp-log-err-preview {
  display: flex;
  align-items: center;
  gap: 6px;
  color: var(--danger);
  font-size: 11.5px;
  max-width: 320px;
}

.mp-err-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--danger);
  flex-shrink: 0;
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

.mp-log-success-banner {
  display: flex;
  align-items: center;
  gap: 12px;
  background: var(--brand-soft);
  border: 1px solid color-mix(in srgb, var(--brand) 40%, transparent);
  border-radius: var(--r-md);
  padding: 12px 16px;
}

.mp-lsb-icon {
  width: 22px;
  height: 22px;
  color: var(--brand);
  display: inline-flex;
  flex-shrink: 0;
}

.mp-lsb-icon :deep(svg) {
  width: 100%;
  height: 100%;
}

.mp-lsb-content strong {
  font-size: 13.5px;
  color: var(--brand-deep);
  display: block;
}

.mp-lsb-content p {
  margin: 2px 0 0;
  font-size: 12px;
  color: var(--muted);
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
