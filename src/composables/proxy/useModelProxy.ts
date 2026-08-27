import { ref, computed } from "vue";
import { isTauri, runCommand } from "../core/ipc";
import { useToast } from "../core/useToast";
import {
  API_PATH_V1,
  API_PATH_RESPONSES,
  API_PATH_GEMINI,
  API_PATH_MESSAGES,
  OPENCODE_UPSTREAM_URL,
} from "../../constants";
import type {
  ChannelConfig,
  OpencodeProxyConfig,
  OpencodeProxyStatus,
  ProxyRequestLog,
  ChannelUsageStats,
  ChannelModelUsageStats,
  ChannelModelList,
  GatewayOverviewStats,
} from "./types";

export * from "./types";

/**
 * 模型 API Origin：桌面端访问本机内嵌服务（实际端口来自后端状态），
 * 浏览器/瘦客户端与 Web 服务同源。
 */
function modelApiOrigin(): string {
  return isTauri
    ? `http://127.0.0.1:${proxyStatus.value.port || 17896}`
    : window.location.origin;
}

export function channelAlias(channel: ChannelConfig | null | undefined): string {
  const a = channel?.alias?.trim().toLowerCase();
  return a || channel?.id || "";
}

/** 渠道统计维度键：稳定数字 ID（后端日统计表维度），未分配时回退别名 */
export function channelStatsKey(channel: ChannelConfig | null | undefined): string {
  return channel?.statsId != null ? String(channel.statsId) : channelAlias(channel);
}

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

export function filterFreeModelsOnly(models: string[]): string[] {
  return models.filter((m) => {
    const lower = m.toLowerCase();
    return lower.includes("free") || lower === "big-pickle";
  });
}

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

export function isValidChannelAlias(alias: string): boolean {
  return /^[a-zA-Z0-9_-]+$/.test(alias.trim());
}

export const proxyConfig = ref<OpencodeProxyConfig>({
  enabled: true,
  listenHost: "127.0.0.1",
  port: 17896,
  apiKey: "",
  channels: [
    {
      id: "opencode",
      name: "OpenCode",
      description: "OpenCode 官方 Public 免费直连通道，免 Key 访问在线优质编码与推理模型",
      enabled: true,
      protocol: "opencode",
      upstreamUrl: OPENCODE_UPSTREAM_URL,
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

export const proxyStatus = ref<OpencodeProxyStatus>({
  running: false,
  port: 17896,
  url: "http://127.0.0.1:17896/v1",
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

export const proxyLoading = ref(false);
export const savingConfig = ref(false);
export const togglingServer = ref(false);
export const fetchingModels = ref(false);
export const channelModels = ref<Record<string, string[]>>({});
export const channelStats = ref<Record<string, ChannelUsageStats>>({});
export const gatewayOverview = ref<GatewayOverviewStats | null>(null);
export const proxyLogs = ref<ProxyRequestLog[]>([]);
export const loadingLogs = ref(false);
export const logPage = ref(1);
export const logPageSize = ref(10);
export const logTotal = ref(0);
export const logSuccessTotal = ref(0);
export const logErrorTotal = ref(0);
export const logGlobalTotal = ref(0);
export const logGlobalSuccess = ref(0);
export const logGlobalError = ref(0);
export const logPageCount = computed(() => Math.max(1, Math.ceil(logTotal.value / logPageSize.value)));
export const logRangeStart = computed(() => (logTotal.value === 0 ? 0 : (logPage.value - 1) * logPageSize.value + 1));
export const logRangeEnd = computed(() => Math.min(logPage.value * logPageSize.value, logTotal.value));
export const logSortBy = ref<"timestamp" | "status" | "tokens" | "duration" | null>(null);
export const logSortOrder = ref<"asc" | "desc">("desc");
// 日志日期区间默认「今日」
function toLocalDate(value: Date): string {
  const year = value.getFullYear();
  const month = String(value.getMonth() + 1).padStart(2, "0");
  const day = String(value.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}
const today = toLocalDate(new Date());
export const logDateFrom = ref(today);
export const logDateTo = ref(today);

// 控制台总览日期区间默认「近14天」（与原趋势图默认窗口一致）；空串 = 全部（全量累计）
function daysAgo(n: number): string {
  const d = new Date();
  d.setDate(d.getDate() - n);
  return toLocalDate(d);
}
export const overviewDateFrom = ref(daysAgo(13));
export const overviewDateTo = ref(today);

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
              upstreamUrl: OPENCODE_UPSTREAM_URL,
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
      await loadCachedModels();
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
        // 后端按渠道生效别名归集，这里同样以别名为键（channelAlias 回退 id，两端一致）
        for (const item of list) map[item.channelId] = item;
        channelStats.value = map;
      }
    } catch (e) {
      console.warn("获取渠道使用统计失败:", e);
    }
  }

  /** 「渠道 × 模型」粒度累计用量：管理可用模型弹窗行内统计展示用 */
  async function fetchChannelModelStats(channelId: string): Promise<ChannelModelUsageStats[]> {
    try {
      const list = await runCommand<ChannelModelUsageStats[]>("get_channel_model_stats", { channelId });
      return Array.isArray(list) ? list : [];
    } catch (e) {
      console.warn("获取模型用量统计失败:", e);
      return [];
    }
  }

  /** 控制台「全渠道数据总览」：按所选日期区间逐日聚合 + 区间累计（持久化日统计表）；
   *  未选区间（全部）时后端回退近 N 天窗口（90 天，受后端 1-90 钳制）+ 全量累计 */
  async function refreshGatewayOverview() {
    const payload: Record<string, unknown> = {};
    if (overviewDateFrom.value) payload.from = overviewDateFrom.value;
    if (overviewDateTo.value) payload.to = overviewDateTo.value;
    if (!payload.from && !payload.to) payload.days = 90;
    try {
      gatewayOverview.value = await runCommand<GatewayOverviewStats>("get_model_proxy_overview_stats", payload);
    } catch (e) {
      console.warn("获取全渠道数据总览失败:", e);
    }
  }

  async function saveConfig(newConfig: OpencodeProxyConfig) {
    savingConfig.value = true;
    try {
      // 明细保留天数：空值归一为 0（= 永久保留），避免空串破坏后端反序列化
      const normalized: OpencodeProxyConfig = {
        ...newConfig,
        listenHost: newConfig.listenHost?.trim() || "127.0.0.1",
        logRetentionDays: Number(newConfig.logRetentionDays ?? 0) || 0,
      };
      const status = await runCommand<OpencodeProxyStatus>("save_opencode_proxy_config_cmd", {
        config: normalized,
      });
      if (status) proxyStatus.value = status;
      proxyConfig.value = { ...normalized };
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

  async function fetchUpstreamModels(options: { setGlobalFetching?: boolean; channelId?: string } = {}): Promise<Record<string, string[]>> {
    if (options.setGlobalFetching) {
      fetchingModels.value = true;
    }
    try {
      const res = await runCommand<any>(
        "fetch_opencode_models",
        options.channelId ? { channelId: options.channelId } : {},
      );
      const list: ChannelModelList[] =
        Array.isArray(res) && Array.isArray(res[0])
          ? res[0]
          : Array.isArray(res)
          ? res
          : [];
      const map: Record<string, string[]> = {};
      if (Array.isArray(list)) {
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
      }
      // 拉取失败的渠道必须可见，否则用户只会看到"列表不变"而无从排查
      const errors: unknown[] =
        Array.isArray(res) && Array.isArray(res[1]) ? res[1] : [];
      if (errors.length > 0) {
        const names = errors
          .map((e: any) => e?.channelName || e?.channel_name || e?.channelId || e?.channel_id || "未知渠道")
          .join("、");
        showToast(`以下渠道模型拉取失败：${names}`, true);
      }
      return map;
    } catch (e) {
      console.warn("拉取模型失败:", e);
      showToast(`模型列表拉取失败：${String(e)}`, true);
      return {};
    } finally {
      if (options.setGlobalFetching) {
        fetchingModels.value = false;
      }
    }
  }

  async function refreshModels() {
    const map = await fetchUpstreamModels({ setGlobalFetching: true });
    // 以现存渠道为准重建缓存：成功渠道用新数据；
    // 本次拉取失败（不在 map 中）的渠道保留旧值，避免条目丢失导致列表闪空/回退到启动快照
    const next: Record<string, string[]> = {};
    for (const channel of proxyConfig.value.channels) {
      const fresh = map[channel.id];
      if (fresh) next[channel.id] = fresh;
      else if (channelModels.value[channel.id]?.length) {
        next[channel.id] = channelModels.value[channel.id];
      }
    }
    channelModels.value = next;
  }

  async function loadCachedModels() {
    try {
      const res = await runCommand<any>("get_opencode_cached_channel_models");
      const list: ChannelModelList[] =
        Array.isArray(res) && Array.isArray(res[0])
          ? res[0]
          : Array.isArray(res)
          ? res
          : [];
      const map: Record<string, string[]> = {};
      if (Array.isArray(list)) {
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
      }
      if (Object.keys(map).length > 0) {
        channelModels.value = map;
      }
    } catch (e) {
      console.warn("读取已缓存渠道模型失败:", e);
    }
  }

  function modelsForChannel(channelId: string): string[] {
    let list = channelModels.value[channelId] ?? [];
    const channel = proxyConfig.value.channels.find((c) => c.id === channelId);
    if (isOpenCodeFreeChannel(channel)) {
      list = filterFreeModelsOnly(list);
    }
    return list;
  }

  async function fetchLogs(options: {
    page?: number;
    pageSize?: number;
    filter?: string;
    q?: string;
    from?: string;
    to?: string;
    sortBy?: "timestamp" | "status" | "tokens" | "duration" | null;
    sortOrder?: "asc" | "desc";
  } = {}) {
    loadingLogs.value = true;
    try {
      const p = options.page ?? logPage.value;
      const ps = options.pageSize ?? logPageSize.value;
      const f = options.filter ?? "";
      const query = options.q ?? "";
      const dateFrom = options.from !== undefined ? options.from : logDateFrom.value;
      const dateTo = options.to !== undefined ? options.to : logDateTo.value;
      const sortBy = options.sortBy !== undefined ? options.sortBy : logSortBy.value;
      const sortOrder = options.sortOrder !== undefined ? options.sortOrder : logSortOrder.value;

      const payload: Record<string, unknown> = {
        page: p,
        pageSize: ps,
      };
      if (f && f !== "all") payload.filter = f;
      if (query.trim()) payload.q = query.trim();
      if (dateFrom) payload.from = dateFrom;
      if (dateTo) payload.to = dateTo;
      if (sortBy) {
        payload.sortBy = sortBy;
        payload.sortOrder = sortOrder;
      }

      const res = await runCommand<{
        items: ProxyRequestLog[];
        total: number;
        globalTotal: number;
        globalSuccess: number;
        globalError: number;
        successTotal: number;
        errorTotal: number;
      }>("get_opencode_proxy_logs", payload);

      if (res && Array.isArray(res.items)) {
        proxyLogs.value = res.items;
        logPage.value = p;
        logPageSize.value = ps;
        logTotal.value = res.total ?? res.items.length;
        logSuccessTotal.value = res.successTotal ?? 0;
        logErrorTotal.value = res.errorTotal ?? 0;
        // 顶部标签计数由后端按当前日期区间统计（不受 filter/搜索影响），每次拉取都刷新
        logGlobalTotal.value = res.globalTotal ?? logGlobalTotal.value;
        logGlobalSuccess.value = res.globalSuccess ?? logGlobalSuccess.value;
        logGlobalError.value = res.globalError ?? logGlobalError.value;
      } else if (Array.isArray(res)) {
        proxyLogs.value = res;
        logTotal.value = (res as any).length;
      }
    } catch (e) {
      console.warn("获取请求日志失败:", e);
    } finally {
      loadingLogs.value = false;
    }
  }

  function goLogPage(page: number, extraOptions: { filter?: string; q?: string } = {}) {
    const target = Math.max(1, Math.min(page, logPageCount.value || 1));
    return fetchLogs({ page: target, ...extraOptions });
  }

  function toggleLogSort(column: "timestamp" | "status" | "tokens" | "duration", extraOptions: { filter?: string; q?: string } = {}) {
    if (logSortBy.value === column) {
      if (logSortOrder.value === "desc") {
        logSortOrder.value = "asc";
      } else {
        logSortBy.value = null;
        logSortOrder.value = "desc";
      }
    } else {
      logSortBy.value = column;
      logSortOrder.value = "desc";
    }
    return fetchLogs({ page: 1, ...extraOptions });
  }

  /**
   * 清理请求明细日志。统计聚合表（渠道统计/总览/反代模式报表）持久化，不受影响。
   * @param mode  "payload_only" 仅清报文全文；"all" 删除明细行
   * @param before 可选 YYYY-MM-DD：只清理该日期之前的明细；缺省清理全部
   */
  async function clearLogs(mode: "payload_only" | "all" = "all", before?: string) {
    try {
      const removed = await runCommand<number>("clear_opencode_proxy_logs", {
        mode,
        before: before || null,
      });
      if (mode === "payload_only") {
        await fetchLogs();
        showToast(
          before
            ? `已清理 ${removed} 条该日期前日志的请求与响应报文`
            : "已清空所有日志的请求与响应报文详细内容",
        );
      } else {
        if (before) {
          await fetchLogs();
        } else {
          proxyLogs.value = [];
          logTotal.value = 0;
          logSuccessTotal.value = 0;
          logErrorTotal.value = 0;
          logGlobalTotal.value = 0;
          logGlobalSuccess.value = 0;
          logGlobalError.value = 0;
          logPage.value = 1;
        }
        await refreshStatus();
        showToast(
          before ? `已删除 ${removed} 条该日期前的明细日志（统计不受影响）` : "所有请求明细日志已清空（统计不受影响）",
        );
      }
      return true;
    } catch (e) {
      showToast(`清理日志失败: ${String(e)}`, true);
      return false;
    }
  }

  async function copyProxyUrl(alias?: any) {
    const aliasStr = typeof alias === "string" ? alias : undefined;
    const base = `${modelApiOrigin()}${API_PATH_V1}`;
    const text = aliasStr ? `${base.replace(/\/+$/, "")}/${aliasStr}` : base;
    try {
      await navigator.clipboard.writeText(text);
      showToast(`Base URL 已复制: ${text}`);
    } catch {
      showToast("复制失败，请手动复制", true);
    }
  }

  async function copyResponsesUrl(alias?: any) {
    const aliasStr = typeof alias === "string" ? alias : undefined;
    const base = `${modelApiOrigin()}${API_PATH_RESPONSES}`;
    const text = aliasStr ? `${base.replace(/\/+$/, "")}/${aliasStr}` : base;
    try {
      await navigator.clipboard.writeText(text);
      showToast(`Responses API URL 已复制: ${text}`);
    } catch {
      showToast("复制失败，请手动复制", true);
    }
  }

  async function copyGeminiUrl(alias?: any) {
    const aliasStr = typeof alias === "string" ? alias : undefined;
    const base = `${modelApiOrigin()}${API_PATH_GEMINI}`;
    const text = aliasStr ? `${base.replace(/\/+$/, "")}/${aliasStr}` : base;
    try {
      await navigator.clipboard.writeText(text);
      showToast(`Gemini Base URL 已复制: ${text}`);
    } catch {
      showToast("复制失败，请手动复制", true);
    }
  }

  async function copyGeminiV1BetaUrl(alias?: any) {
    const aliasStr = typeof alias === "string" ? alias : undefined;
    const base = `${modelApiOrigin()}/v1beta`;
    const text = aliasStr ? `${base.replace(/\/+$/, "")}/${aliasStr}` : base;
    try {
      await navigator.clipboard.writeText(text);
      showToast(`Gemini v1beta Base URL 已复制: ${text}`);
    } catch {
      showToast("复制失败，请手动复制", true);
    }
  }

  async function copyClaudeUrl(alias?: any) {
    const aliasStr = typeof alias === "string" ? alias : undefined;
    const base = `${modelApiOrigin()}${API_PATH_MESSAGES}`;
    const text = aliasStr ? `${base.replace(/\/+$/, "")}/${aliasStr}` : base;
    try {
      await navigator.clipboard.writeText(text);
      showToast(`Claude Messages URL 已复制: ${text}`);
    } catch {
      showToast("复制失败，请手动复制", true);
    }
  }

  async function copyProxyKey() {
    const key = proxyConfig.value.apiKey?.trim() || "";
    if (!key) {
      showToast("模型接口 API Key 由服务自动管理");
      return;
    }
    try {
      await navigator.clipboard.writeText(key);
      showToast("API Key 已复制");
    } catch {
      showToast("复制失败，请手动复制", true);
    }
  }

  return {
    proxyConfig,
    proxyStatus,
    proxyLoading,
    savingConfig,
    togglingServer,
    fetchingModels,
    channelModels,
    channelStats,
    gatewayOverview,
    modelsForChannel,
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
    logDateFrom,
    logDateTo,
    overviewDateFrom,
    overviewDateTo,
    loadProxyData,
    refreshStatus,
    refreshChannelStats,
    fetchChannelModelStats,
    refreshGatewayOverview,
    saveConfig,
    toggleServer,
    fetchUpstreamModels,
    refreshModels,
    loadCachedModels,
    fetchLogs,
    goLogPage,
    toggleLogSort,
    clearLogs,
    copyProxyUrl,
    copyResponsesUrl,
    copyGeminiUrl,
    copyGeminiV1BetaUrl,
    copyClaudeUrl,
    copyProxyKey,
  };
}
