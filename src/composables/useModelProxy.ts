import { ref, computed } from "vue";
import { runCommand } from "./useLibrary";
import { useToast } from "./useToast";

export interface ChannelConfig {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  protocol: string;
  upstreamUrl: string;
  apiKey?: string;
  /** 站点转换继承的多个原 Key，请求时自动轮换尝试；为空时回退 apiKey */
  apiKeys?: string[];
  useProxyPool: boolean;
  /** 英文别名：网关模型前缀（如 opencode/*）；全渠道唯一，含 opencode。为空回退为渠道 id。 */
  alias?: string;
  /** 通过「站点转换」创建时关联的站点库站点 id */
  siteId?: string | null;
  /** 代理池固定通道：始终经代理池出口节点转发，不优先直连 */
  useFixedProxy?: boolean;
  /**
   * 该渠道对外暴露的模型白名单：
   * - undefined / null（默认，兼容旧配置）= 全部启用
   * - [] = 不暴露任何模型
   * - 非空数组 = 仅勾选的模型体现在可用模型列表
   */
  enabledModels?: string[] | null;
}

/** 渠道的生效别名：显式配置为空时回退为渠道 id，统一小写。 */
export function channelAlias(channel: ChannelConfig | null | undefined): string {
  const a = channel?.alias?.trim().toLowerCase();
  return a || channel?.id || "";
}

/** 判断是否为 OpenCode 免 Key 免费通道（未填有效 Key）。 */
export function isOpenCodeFreeChannel(channel: ChannelConfig | null | undefined): boolean {
  if (!channel) return false;
  const isOpencode =
    channel.id === "opencode" ||
    channel.protocol === "opencode" ||
    channel.alias === "opencode" ||
    (channel.upstreamUrl && channel.upstreamUrl.includes("opencode.ai")) ||
    (channel.name && channel.name.toLowerCase().includes("opencode"));
  if (!isOpencode) return false;
  const hasKey = !!(channel.apiKey?.trim() || channel.apiKeys?.some((k) => k.trim()));
  return !hasKey;
}

/** 过滤 OpenCode 免 Key 渠道支持的模型（包含 free 关键词或 big-pickle） */
export function filterFreeModelsOnly(models: string[]): string[] {
  return models.filter((m) => {
    const lower = m.toLowerCase();
    return lower.includes("free") || lower === "big-pickle";
  });
}

/** 按渠道白名单过滤模型列表：未配置白名单时返回全量。 */
export function filterChannelModels(
  channel: ChannelConfig | null | undefined,
  models: string[],
): string[] {
  let list = models;
  if (isOpenCodeFreeChannel(channel)) {
    list = filterFreeModelsOnly(list);
  }
  const allow = channel?.enabledModels;
  if (!allow) return list;
  return list.filter((m) => allow.includes(m));
}

/** 校验英文别名合法性：仅限英文字母、数字、- 与 _。 */
export function isValidChannelAlias(alias: string): boolean {
  return /^[a-zA-Z0-9_-]+$/.test(alias.trim());
}

export interface OpencodeProxyConfig {
  enabled: boolean;
  port: number;
  apiKey: string;
  channels: ChannelConfig[];
  timeoutSeconds: number;
  recordRequestBody?: boolean;
  /** 失败重试次数，默认 0 = 失败直接返回；代理池渠道同时受可用节点数限制 */
  maxRetries?: number;
}

export interface OpencodeProxyStatus {
  running: boolean;
  port: number;
  url: string;
  totalRequests: number;
  successfulRequests: number;
  failedRequests: number;
  uptimeSeconds: number;
  modelsCount: number;
  channelsCount: number;
  totalPromptTokens?: number;
  totalCompletionTokens?: number;
  totalReasoningTokens?: number;
  totalReasoningRequests?: number;
  totalCacheHitTokens?: number;
  totalTokens?: number;
  todayTotalTokens?: number;
}

export interface ProxyRequestLog {
  id: string;
  timestamp: string;
  method: string;
  path: string;
  channelId: string;
  model: string;
  stream: boolean;
  statusCode: number;
  durationMs: number;
  ttftMs?: number;
  promptTokens?: number;
  promptCacheHitTokens?: number;
  promptCacheMissTokens?: number;
  completionTokens?: number;
  reasoningTokens?: number;
  totalTokens?: number;
  errorMessage?: string;
  requestBody?: string;
  responseBody?: string;
  nodeName?: string;
}

/** 单个渠道的累计使用统计（渠道卡片底部展示）。 */
export interface ChannelUsageStats {
  channelId: string;
  totalRequests: number;
  successfulRequests: number;
  failedRequests: number;
  totalPromptTokens?: number;
  totalCompletionTokens?: number;
  totalReasoningTokens?: number;
  totalReasoningRequests?: number;
  totalCacheHitTokens?: number;
  totalTokens?: number;
  todayTotalTokens?: number;
}

/** 单个渠道拉取到的模型列表（含别名前缀）。 */
export interface ChannelModelList {
  channelId: string;
  alias: string;
  models: string[];
}

const proxyConfig = ref<OpencodeProxyConfig>({
  enabled: true,
  port: 8088,
  apiKey: "",
  channels: [
    {
      id: "opencode",
      name: "OpenCode",
      description: "OpenCode 官方 Public 免费直连通道，免 Key 访问在线优质编码与推理模型",
      enabled: true,
      protocol: "opencode",
      upstreamUrl: "https://opencode.ai/zen/v1",
      apiKey: "public",
      useProxyPool: false,
      alias: "opencode",
      siteId: null,
      useFixedProxy: false,
      enabledModels: null,
    },
  ],
  timeoutSeconds: 300,
  maxRetries: 0,
});

const proxyStatus = ref<OpencodeProxyStatus>({
  running: false,
  port: 8088,
  url: "http://127.0.0.1:8088/v1",
  totalRequests: 0,
  successfulRequests: 0,
  failedRequests: 0,
  uptimeSeconds: 0,
  modelsCount: 0,
  channelsCount: 1,
  totalPromptTokens: 0,
  totalCompletionTokens: 0,
  totalReasoningTokens: 0,
  totalReasoningRequests: 0,
  totalCacheHitTokens: 0,
  totalTokens: 0,
  todayTotalTokens: 0,
});

const proxyLoading = ref(false);
const savingConfig = ref(false);
const togglingServer = ref(false);
const testingHealth = ref(false);
const fetchingModels = ref(false);
/** 各渠道拉取到的模型列表（key = channelId，值为裸模型名数组） */
const channelModels = ref<Record<string, string[]>>({});
/** 按渠道 id 索引的累计使用统计 */
const channelStats = ref<Record<string, ChannelUsageStats>>({});
const healthResult = ref<any>(null);
const healthResultTime = ref<string>("");
const proxyLogs = ref<ProxyRequestLog[]>([]);
const loadingLogs = ref(false);
/** 分页状态：当前页 / 每页条数 / 后端返回的总数与成功异常计数 */
const logPage = ref(1);
const logPageSize = ref(50);
const logTotal = ref(0);
const logSuccessTotal = ref(0);
const logErrorTotal = ref(0);
/** 全库固定计数（不随 filter/搜索变化），用于标签页显示 */
const logGlobalTotal = ref(0);
const logGlobalSuccess = ref(0);
const logGlobalError = ref(0);
const logPageCount = computed(() => Math.max(1, Math.ceil(logTotal.value / logPageSize.value)));
const logRangeStart = computed(() => (logTotal.value === 0 ? 0 : (logPage.value - 1) * logPageSize.value + 1));
const logRangeEnd = computed(() => Math.min(logPage.value * logPageSize.value, logTotal.value));
/** 排序：三态（升序 → 降序 → 默认）。null 表示不排序，由后端按默认顺序返回 */
const logSortBy = ref<"timestamp" | "status" | "tokens" | "duration" | null>(null);
const logSortOrder = ref<"asc" | "desc">("desc");

export function useModelProxy() {
  const { showToast } = useToast();

  async function refreshStatus() {
    try {
      const status = await runCommand<OpencodeProxyStatus>("get_opencode_proxy_status");
      if (status) proxyStatus.value = status;
    } catch {
      // background polling
    }
  }

  async function loadProxyData() {
    proxyLoading.value = true;
    try {
      const [cfg, status] = await Promise.all([
        runCommand<OpencodeProxyConfig>("get_opencode_proxy_config"),
        runCommand<OpencodeProxyStatus>("get_opencode_proxy_status"),
      ]);
      if (cfg) {
        if (!cfg.channels || cfg.channels.length === 0) {
          cfg.channels = [
            {
              id: "opencode",
              name: "OpenCode",
              description: "OpenCode 官方 Public 免费直连通道，免 Key 访问在线优质编码与推理模型",
              enabled: true,
              protocol: "opencode",
              upstreamUrl: "https://opencode.ai/zen/v1",
              apiKey: "public",
              useProxyPool: false,
              alias: "opencode",
              siteId: null,
              useFixedProxy: false,
              enabledModels: null,
            },
          ];
        }
        proxyConfig.value = cfg;
      }
      if (status) proxyStatus.value = status;
      await refreshChannelStats();
    } catch (e) {
      console.error("加载模型反代配置失败:", e);
    } finally {
      proxyLoading.value = false;
    }
  }

  async function refreshChannelStats() {
    try {
      const list = await runCommand<ChannelUsageStats[]>("get_opencode_channel_stats");
      if (Array.isArray(list)) {
        const map: Record<string, ChannelUsageStats> = {};
        for (const item of list) map[item.channelId] = item;
        channelStats.value = map;
      }
    } catch (e) {
      console.warn("获取渠道使用统计失败:", e);
    }
  }

  async function saveConfig(newConfig: OpencodeProxyConfig) {
    savingConfig.value = true;
    try {
      const status = await runCommand<OpencodeProxyStatus>("save_opencode_proxy_config_cmd", {
        config: newConfig,
      });
      if (status) proxyStatus.value = status;
      proxyConfig.value = { ...newConfig };
      showToast("反代配置与渠道设置已保存");
      return true;
    } catch (e) {
      showToast(`保存配置失败: ${String(e)}`, true);
      return false;
    } finally {
      savingConfig.value = false;
    }
  }

  async function toggleServer() {
    togglingServer.value = true;
    try {
      if (proxyStatus.value.running) {
        const status = await runCommand<OpencodeProxyStatus>("stop_opencode_proxy");
        if (status) proxyStatus.value = status;
        showToast("模型反代服务已停止");
      } else {
        const status = await runCommand<OpencodeProxyStatus>("start_opencode_proxy");
        if (status) proxyStatus.value = status;
        showToast(`模型反代服务已在端口 ${proxyStatus.value.port} 启动`);
      }
    } catch (e) {
      showToast(`操作服务失败: ${String(e)}`, true);
    } finally {
      togglingServer.value = false;
    }
  }

  async function testHealth() {
    testingHealth.value = true;
    healthResult.value = null;
    try {
      const res = await runCommand<any>("test_opencode_proxy_health");
      healthResult.value = res;
      healthResultTime.value = new Date().toLocaleTimeString();
      showToast("健康检查通过 (200 OK)");
      return res;
    } catch (e) {
      healthResult.value = { error: String(e) };
      healthResultTime.value = new Date().toLocaleTimeString();
      showToast(`健康检查未通过: ${String(e)}`, true);
      return null;
    } finally {
      testingHealth.value = false;
    }
  }

  async function refreshModels() {
    fetchingModels.value = true;
    try {
      const res = await runCommand<any>("fetch_opencode_models");
      const list: ChannelModelList[] =
        Array.isArray(res) && Array.isArray(res[0])
          ? res[0]
          : Array.isArray(res)
          ? res
          : [];
      if (Array.isArray(list)) {
        const map: Record<string, string[]> = {};
        for (const item of list) {
          const cid = item.channelId || (item as any).channel_id;
          if (cid) {
            let mList = item.models || [];
            const channel = proxyConfig.value.channels.find((c) => c.id === cid);
            if (isOpenCodeFreeChannel(channel)) {
              mList = filterFreeModelsOnly(mList);
            }
            map[cid] = mList;
          }
        }
        channelModels.value = map;
      }
    } catch (e) {
      console.warn("拉取模型失败:", e);
    } finally {
      fetchingModels.value = false;
    }
  }

  /** 某渠道的模型列表（未拉取到时为空数组）。 */
  function modelsForChannel(channelId: string): string[] {
    let list = channelModels.value[channelId] ?? [];
    const channel = proxyConfig.value.channels.find((c) => c.id === channelId);
    if (isOpenCodeFreeChannel(channel)) {
      list = filterFreeModelsOnly(list);
    }
    return list;
  }

  async function fetchLogs(options: { filter?: string; q?: string } = {}) {
    loadingLogs.value = true;
    try {
      const filter = options.filter ?? "";
      const q = (options.q ?? "").trim().toLowerCase();
      const data = await runCommand<
        | {
            items: ProxyRequestLog[];
            total: number;
            successTotal: number;
            errorTotal: number;
            globalTotal?: number;
            globalSuccessTotal?: number;
            globalErrorTotal?: number;
          }
        | ProxyRequestLog[]
      >("get_opencode_proxy_logs", {
        page: logPage.value,
        pageSize: logPageSize.value,
        filter,
        q: options.q ?? "",
        sortBy: logSortBy.value,
        sortOrder: logSortOrder.value,
      });
      if (Array.isArray(data)) {
        // 旧内核响应：不支持分页/过滤，返回全量。本地按当前筛选与关键词过滤、切片兜底，
        // 保证"异常/失败"列表只出现失败记录、计数与列表一致，不混入正常数据。
        let list = data.filter((l) => {
          if (filter === "success") return l.statusCode >= 200 && l.statusCode < 300;
          if (filter === "error") return l.statusCode >= 400;
          return true;
        });
        if (q) {
          list = list.filter(
            (l) =>
              l.model.toLowerCase().includes(q) ||
              l.path.toLowerCase().includes(q) ||
              String(l.statusCode).includes(q) ||
              (l.errorMessage ?? "").toLowerCase().includes(q)
          );
        }
        logTotal.value = list.length;
        logSuccessTotal.value = list.filter((l) => l.statusCode >= 200 && l.statusCode < 300).length;
        logErrorTotal.value = list.filter((l) => l.statusCode >= 400).length;
        logGlobalTotal.value = data.length;
        logGlobalSuccess.value = data.filter((l) => l.statusCode >= 200 && l.statusCode < 300).length;
        logGlobalError.value = data.filter((l) => l.statusCode >= 400).length;
        proxyLogs.value = list.slice((logPage.value - 1) * logPageSize.value, logPage.value * logPageSize.value);
      } else if (data && Array.isArray(data.items)) {
        proxyLogs.value = data.items;
        logTotal.value = data.total ?? proxyLogs.value.length;
        logSuccessTotal.value = data.successTotal ?? 0;
        logErrorTotal.value = data.errorTotal ?? 0;
        logGlobalTotal.value = data.globalTotal ?? logTotal.value;
        logGlobalSuccess.value = data.globalSuccessTotal ?? logSuccessTotal.value;
        logGlobalError.value = data.globalErrorTotal ?? logErrorTotal.value;
      }
    } catch (e) {
      console.warn("获取请求日志失败:", e);
    } finally {
      loadingLogs.value = false;
    }
  }

  /** 切换排序三态：异列设升序；同列 升序 → 降序 → 取消排序（回到后端默认顺序）；沿用调用方当前的筛选与关键词重新拉取 */
  function toggleLogSort(by: "timestamp" | "status" | "tokens" | "duration", options: { filter?: string; q?: string } = {}) {
    if (logSortBy.value === by) {
      if (logSortOrder.value === "asc") {
        logSortOrder.value = "desc";
      } else {
        logSortBy.value = null;
      }
    } else {
      logSortBy.value = by;
      logSortOrder.value = "asc";
    }
    goLogPage(1, options);
  }
  async function goLogPage(page: number, options: { filter?: string; q?: string } = {}) {
    const target = Math.min(Math.max(1, page), logPageCount.value);
    logPage.value = target;
    await fetchLogs(options);
  }

  async function clearLogs(mode: "payload_only" | "all" = "all") {
    try {
      await runCommand("clear_opencode_proxy_logs", { mode });
      if (mode === "payload_only") {
        await fetchLogs();
        showToast("已清空所有日志的请求与响应报文详细内容");
      } else {
        proxyLogs.value = [];
        logTotal.value = 0;
        logSuccessTotal.value = 0;
        logErrorTotal.value = 0;
        logPage.value = 1;
        await refreshStatus();
        showToast("已清空所有历史日志与统计数据");
      }
    } catch (e) {
      showToast(`清空日志失败: ${String(e)}`, true);
    }
  }

  async function copyProxyUrl() {
    const url = proxyStatus.value.url || `http://127.0.0.1:${proxyConfig.value.port}/v1`;
    try {
      await navigator.clipboard.writeText(url);
      showToast(`已复制 Base URL: ${url}`);
    } catch {
      showToast("复制失败", true);
    }
  }
  async function copyGeminiUrl() {
    const port = proxyStatus.value.port || proxyConfig.value.port;
    const url = `http://127.0.0.1:${port}/v1/gemini`;
    try {
      await navigator.clipboard.writeText(url);
      showToast(`已复制 Google Gemini Base URL: ${url}`);
    } catch {
      showToast("复制失败", true);
    }
  }

  async function copyClaudeUrl() {
    const port = proxyStatus.value.port || proxyConfig.value.port;
    const url = `http://127.0.0.1:${port}/v1/messages`;
    try {
      await navigator.clipboard.writeText(url);
      showToast(`已复制 Claude Messages URL: ${url}`);
    } catch {
      showToast("复制失败", true);
    }
  }

  async function copyProxyKey() {
    const key = proxyConfig.value.apiKey || "";
    if (!key) {
      showToast("当前未配置访问密钥（免密直连）");
      return;
    }
    try {
      await navigator.clipboard.writeText(key);
      showToast("已复制访问密钥到剪贴板");
    } catch {
      showToast("复制失败", true);
    }
  }

  return {
    proxyConfig,
    proxyStatus,
    proxyLoading,
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
    logPage,
    logPageSize,
    logTotal,
    logSuccessTotal,
    logErrorTotal,
    logGlobalTotal,
    logGlobalSuccess,
    logGlobalError,
    logPageCount,
    logRangeStart,
    logRangeEnd,
    logSortBy,
    logSortOrder,
    loadProxyData,
    refreshStatus,
    refreshChannelStats,
    saveConfig,
    toggleServer,
    testHealth,
    refreshModels,
    fetchLogs,
    goLogPage,
    toggleLogSort,
    clearLogs,
    copyProxyUrl,
    copyGeminiUrl,
    copyClaudeUrl,
    copyProxyKey,
  };
}
