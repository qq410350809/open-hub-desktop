import { ref } from "vue";
import { listen, type UnlistenFn } from "../core/events";
import type {
  RawLogReport,
  RequestHealthReport,
  TokenStatsReport,
  TokenCollectorSyncReport,
  TokenUsageReport,
  LocalAgentPathsReport,
  TokenModelMapping,
  TokenOfficialModel,
  TokenMappingAnalyzeReport,
  TokenMappingAnalyzeProgress,
  TokenInsightReport,
  InsightEvidencePacket,
} from "../../types";
import { localTokenStatsAvailable, runLocalCommand } from "../core/ipc";

const tokenStats = ref<TokenStatsReport | null>(null);
const tokenStatsLoading = ref(false);
const tokenStatsError = ref("");
const tokenStatsFrom = ref("");
const tokenStatsTo = ref("");

// 本地 AI Agent 路径诊断（只读扫描配置 / 数据 / 数据库目录）
const localAgentPaths = ref<LocalAgentPathsReport | null>(null);
const localAgentPathsLoading = ref(false);
const localAgentPathsError = ref("");

// 小时用量桶（Token 统计页概览数据源，覆盖所有工具）
const tokenUsage = ref<TokenUsageReport | null>(null);
const tokenUsageLoading = ref(false);
const tokenUsageError = ref("");

// 原始日志解析（Token 明细页：会话/对话/请求三级）
const rawLogs = ref<RawLogReport | null>(null);
const rawLogsLoading = ref(false);
const rawLogsError = ref("");

// 请求健康（大模型请求成功/失败）
const requestHealth = ref<RequestHealthReport | null>(null);
const requestHealthLoading = ref(false);
const requestHealthError = ref("");

// 模型映射（本地 Token 统计原始名 → 正式模型；AI 分析 + 手工确认）
const tokenModelMappings = ref<TokenModelMapping[]>([]);
const tokenModelMappingsLoading = ref(false);
const tokenModelMappingsError = ref("");
const tokenModelAnalyzeReport = ref<TokenMappingAnalyzeReport | null>(null);
const tokenModelAnalyzeProgress = ref<TokenMappingAnalyzeProgress | null>(null);
const tokenModelAnalyzing = ref(false);
const tokenModelAnalyzeError = ref("");
let unlistenTokenModelAnalyzeProgress: UnlistenFn | null = null;

// AI 用量洞察（证据包由页面按当前时间范围确定性构建，AI 只做可追溯解读）
const tokenInsightReport = ref<TokenInsightReport | null>(null);
const tokenInsightLoading = ref(false);
const tokenInsightError = ref("");
const tokenInsightAnalyzing = ref(false);

// OpenHub 自有 Token 采集状态；只读取本机日志并维护本地缓存。
const tokenCollectorSyncing = ref(false);
const tokenCollectorSyncError = ref("");
const tokenCollectorSyncReport = ref<TokenCollectorSyncReport | null>(null);
let tokenCollectorSyncPromise: Promise<TokenCollectorSyncReport> | null = null;
let tokenDatabaseRefreshTimer: number | null = null;
let tokenDatabaseRefreshPromise: Promise<void> | null = null;
const TOKEN_DATABASE_REFRESH_MS = 5_000;

function toLocalDate(value: Date): string {
  const year = value.getFullYear();
  const month = String(value.getMonth() + 1).padStart(2, "0");
  const day = String(value.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

/**
 * 快捷范围编码：
 *  -2 今日（仅今天）
 *  -3 昨日（仅昨天）
 *   0 本月（1 号至今）
 *  -1 全部（不限日期）
 *  >0 近 N 天（含今天，共 N 天，即今天往前 N-1 天）
 */
function setQuickRange(days: number) {
  if (days === -1) {
    tokenStatsFrom.value = "";
    tokenStatsTo.value = "";
    return;
  }

  const today = new Date();
  const from = new Date(today);
  const to = new Date(today);

  if (days === -2) {
    // 今日
  } else if (days === -3) {
    from.setDate(from.getDate() - 1);
    to.setDate(to.getDate() - 1);
  } else if (days === 0) {
    from.setDate(1);
  } else if (days === -4) {
    // 本季度（季度首日至今）
    from.setDate(1);
    from.setMonth(Math.floor(from.getMonth() / 3) * 3);
  } else if (days === -5) {
    // 今年（1月1日至今）
    from.setDate(1);
    from.setMonth(0);
  } else if (days === -6) {
    // 前一天（今天 - 2 天）
    from.setDate(from.getDate() - 2);
    to.setDate(to.getDate() - 2);
  } else if (days === -8) {
    // 后一天（明天）
    from.setDate(from.getDate() + 1);
    to.setDate(to.getDate() + 1);
  } else if (days === -10) {
    // 本周（周一起至今）
    const offset = (from.getDay() + 6) % 7;
    from.setDate(from.getDate() - offset);
  } else {
    from.setDate(from.getDate() - (days - 1));
  }

  tokenStatsFrom.value = toLocalDate(from);
  tokenStatsTo.value = toLocalDate(to);
}

// 默认「今日」
setQuickRange(-2);

function localTokenUnavailable(): Error {
  return new Error("本地 Token 统计只在客户端本地可用；当前浏览器服务仅提供反代统计");
}

async function localCommand<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  if (!localTokenStatsAvailable) throw localTokenUnavailable();
  return runLocalCommand<T>(command, args);
}

async function loadTokenStats(from?: string, to?: string, refresh = false) {
  tokenStatsLoading.value = true;
  tokenStatsError.value = "";
  try {
    tokenStats.value = await localCommand<TokenStatsReport>("get_token_stats", {
      from: (from ?? tokenStatsFrom.value) || null,
      to: (to ?? tokenStatsTo.value) || null,
      refresh,
    });
  } catch (error) {
    tokenStatsError.value = String(error);
    tokenStats.value = null;
  } finally {
    tokenStatsLoading.value = false;
  }
}

function onRangeChange() {
  void loadTokenStats();
}

function refreshTokenStats() {
  void loadTokenStats(tokenStatsFrom.value, tokenStatsTo.value);
}

async function syncTokenCollector(force = false): Promise<TokenCollectorSyncReport> {
  if (tokenCollectorSyncPromise) {
    return tokenCollectorSyncPromise;
  }
  tokenCollectorSyncing.value = true;
  tokenCollectorSyncError.value = "";
  const promise = localCommand<TokenCollectorSyncReport>("sync_token_data", { force })
    .then((report) => {
      tokenCollectorSyncReport.value = report;
      return report;
    })
    .catch((error) => {
      tokenCollectorSyncError.value = String(error);
      throw error;
    })
    .finally(() => {
      tokenCollectorSyncing.value = false;
      tokenCollectorSyncPromise = null;
    });
  tokenCollectorSyncPromise = promise;
  return promise;
}

async function loadTokenUsage() {
  tokenUsageLoading.value = true;
  tokenUsageError.value = "";
  try {
    tokenUsage.value = await localCommand<TokenUsageReport>("get_token_usage");
  } catch (error) {
    tokenUsageError.value = String(error);
    tokenUsage.value = null;
  } finally {
    tokenUsageLoading.value = false;
  }
}

async function loadTokenRawLogs() {
  rawLogsLoading.value = true;
  rawLogsError.value = "";
  try {
    rawLogs.value = await localCommand<RawLogReport>("get_token_raw_logs");
  } catch (error) {
    rawLogsError.value = String(error);
    rawLogs.value = null;
  } finally {
    rawLogsLoading.value = false;
  }
}

async function loadRequestHealth(refresh = false) {
  const keepExisting = !!requestHealth.value && !refresh;
  if (!keepExisting) {
    requestHealthLoading.value = true;
  }
  requestHealthError.value = "";
  try {
    requestHealth.value = await localCommand<RequestHealthReport>("get_token_request_health", {
      refresh,
    });
  } catch (error) {
    requestHealthError.value = String(error);
    if (!keepExisting) {
      requestHealth.value = null;
    }
  } finally {
    requestHealthLoading.value = false;
  }
}

async function loadLocalAgentPaths() {
  localAgentPathsLoading.value = true;
  localAgentPathsError.value = "";
  try {
    localAgentPaths.value = await localCommand<LocalAgentPathsReport>("get_local_agent_paths");
  } catch (error) {
    localAgentPathsError.value = String(error);
    localAgentPaths.value = null;
  } finally {
    localAgentPathsLoading.value = false;
  }
}

async function loadTokenModelMappings() {
  tokenModelMappingsLoading.value = true;
  tokenModelMappingsError.value = "";
  try {
    tokenModelMappings.value = await localCommand<TokenModelMapping[]>("get_token_model_mappings");
  } catch (error) {
    tokenModelMappingsError.value = String(error);
    tokenModelMappings.value = [];
  } finally {
    tokenModelMappingsLoading.value = false;
  }
}

/** 把统计里出现的原始模型名登记进映射表（INSERT OR IGNORE，已有判定不被覆盖）。 */
async function registerTokenModelNames(names: string[]): Promise<number> {
  if (!names.length) return 0;
  try {
    return await localCommand<number>("register_token_model_names", { names });
  } catch (error) {
    tokenModelMappingsError.value = String(error);
    return 0;
  }
}

/** 打开弹窗时的引导：先登记当前统计里的模型名，再拉取映射表。 */
async function bootstrapTokenModelMappings(names: string[]) {
  await registerTokenModelNames(names);
  await loadTokenModelMappings();
}

export interface TokenMappingAnalyzeOptions {
  channelId?: string | null;
  model?: string | null;
  force?: boolean;
}

/** 调 AI 生成审核建议；已批准和手工映射永远不会被自动覆盖。 */
async function analyzeTokenModelMappings(
  options: TokenMappingAnalyzeOptions = {},
): Promise<TokenMappingAnalyzeReport | null> {
  if (tokenModelAnalyzing.value) return null;
  tokenModelAnalyzing.value = true;
  tokenModelAnalyzeError.value = "";
  tokenModelAnalyzeReport.value = null;
  tokenModelAnalyzeProgress.value = {
    stage: "prepare",
    processed: 0,
    total: 0,
    message: "正在准备 AI 辅助识别",
  };
  unlistenTokenModelAnalyzeProgress?.();
  unlistenTokenModelAnalyzeProgress = await listen<TokenMappingAnalyzeProgress>(
    "token-mapping-analysis-progress",
    ({ payload }) => {
      tokenModelAnalyzeProgress.value = payload;
    },
    { local: true },
  );
  try {
    const report = await localCommand<TokenMappingAnalyzeReport>("analyze_token_model_mappings", {
      channelId: options.channelId || null,
      model: options.model || null,
      force: options.force ?? false,
    });
    tokenModelAnalyzeReport.value = report;
    await loadTokenModelMappings();
    return report;
  } catch (error) {
    tokenModelAnalyzeError.value = String(error);
    return null;
  } finally {
    tokenModelAnalyzing.value = false;
    unlistenTokenModelAnalyzeProgress?.();
    unlistenTokenModelAnalyzeProgress = null;
  }
}

/** 手工修改单条映射；officialModel 传空串表示清除映射（回到待识别）。 */
async function setTokenModelMapping(rawModel: string, officialModel: string): Promise<boolean> {
  try {
    await localCommand<TokenModelMapping>("set_token_model_mapping", {
      rawModel,
      officialModel,
    });
    await loadTokenModelMappings();
    return true;
  } catch (error) {
    tokenModelMappingsError.value = String(error);
    return false;
  }
}

async function reviewTokenModelMapping(
  command: "approve_token_model_mapping" | "reject_token_model_mapping" | "reopen_token_model_mapping",
  rawModel: string,
): Promise<boolean> {
  try {
    await localCommand<TokenModelMapping>(command, { rawModel });
    await loadTokenModelMappings();
    return true;
  } catch (error) {
    tokenModelMappingsError.value = String(error);
    return false;
  }
}

function approveTokenModelMapping(rawModel: string) {
  return reviewTokenModelMapping("approve_token_model_mapping", rawModel);
}

function rejectTokenModelMapping(rawModel: string) {
  return reviewTokenModelMapping("reject_token_model_mapping", rawModel);
}

function reopenTokenModelMapping(rawModel: string) {
  return reviewTokenModelMapping("reopen_token_model_mapping", rawModel);
}

/** 生成 AI 用量洞察。报告会记录范围与模型快照，避免与后续区间混淆。 */
async function analyzeTokenInsights(packet: InsightEvidencePacket): Promise<TokenInsightReport | null> {
  if (tokenInsightAnalyzing.value) return null;
  tokenInsightAnalyzing.value = true;
  tokenInsightLoading.value = true;
  tokenInsightError.value = "";
  try {
    const report = await localCommand<TokenInsightReport>("analyze_token_insights", { packet });
    tokenInsightReport.value = report;
    return report;
  } catch (error) {
    tokenInsightError.value = String(error);
    return null;
  } finally {
    tokenInsightAnalyzing.value = false;
    tokenInsightLoading.value = false;
  }
}

function clearTokenInsightReport() {
  tokenInsightReport.value = null;
  tokenInsightError.value = "";
}

/** 获取 Token 统计的正式模型清单（含目录导入 / user 手工 / AI 学习来源）。 */
async function loadTokenOfficialModels(): Promise<TokenOfficialModel[]> {
  return localCommand<TokenOfficialModel[]>("get_token_official_models");
}

/** 添加自定义正式模型（source=user）；已存在时更新名称与分组。 */
async function addTokenOfficialModel(name: string, lab = "自定义"): Promise<boolean> {
  try {
    await localCommand("add_token_official_model", { id: name, name, lab });
    return true;
  } catch (error) {
    tokenModelMappingsError.value = String(error);
    return false;
  }
}

/** 删除自定义/AI 学习来源的正式模型；目录导入的模型删除会被后端拒绝。 */
async function removeTokenOfficialModel(id: string): Promise<string> {
  try {
    await localCommand("remove_token_official_model", { id });
    return "";
  } catch (error) {
    return String(error);
  }
}

async function refreshTokenDatabaseView(showLoading = false) {
  if (tokenDatabaseRefreshPromise) {
    try {
      await tokenDatabaseRefreshPromise;
    } catch {
      // 错误已由原任务记录
    }
    if (!showLoading) return;
  }

  const refreshPromise = (async () => {
    if (showLoading) {
      await Promise.all([loadTokenUsage(), loadTokenStats(), loadRequestHealth(false)]);
      const errors = [tokenUsageError.value, tokenStatsError.value, requestHealthError.value]
        .filter((message) => message.trim());
      if (errors.length) {
        throw new Error(errors.join("；"));
      }
      return;
    }

    await Promise.all([
      localCommand<TokenUsageReport>("get_token_usage").then((value) => { tokenUsage.value = value; }),
      localCommand<TokenStatsReport>("get_token_stats", {
        from: tokenStatsFrom.value || null,
        to: tokenStatsTo.value || null,
        refresh: false,
      }).then((value) => { tokenStats.value = value; }),
      localCommand<RequestHealthReport>("get_token_request_health", { refresh: false })
        .then((value) => { requestHealth.value = value; }),
    ]);
  })();
  tokenDatabaseRefreshPromise = refreshPromise;

  try {
    await refreshPromise;
  } catch (error) {
    tokenUsageError.value = String(error);
    throw error;
  } finally {
    if (tokenDatabaseRefreshPromise === refreshPromise) {
      tokenDatabaseRefreshPromise = null;
    }
  }
}

function startTokenDatabaseRefresh() {
  if (!localTokenStatsAvailable || tokenDatabaseRefreshTimer != null) return;
  void refreshTokenDatabaseView(true).catch(() => {});
  tokenDatabaseRefreshTimer = window.setInterval(() => {
    void refreshTokenDatabaseView(false).catch(() => {});
  }, TOKEN_DATABASE_REFRESH_MS);
}

function stopTokenDatabaseRefresh() {
  if (tokenDatabaseRefreshTimer != null) {
    window.clearInterval(tokenDatabaseRefreshTimer);
    tokenDatabaseRefreshTimer = null;
  }
}

export function useTokenStats() {
  return {
    tokenStats,
    tokenStatsLoading,
    tokenStatsError,
    tokenStatsFrom,
    tokenStatsTo,
    tokenUsage,
    tokenUsageLoading,
    tokenUsageError,
    rawLogs,
    rawLogsLoading,
    rawLogsError,
    requestHealth,
    requestHealthLoading,
    requestHealthError,
    tokenCollectorSyncing,
    tokenCollectorSyncError,
    tokenCollectorSyncReport,
    localAgentPaths,
    localAgentPathsLoading,
    localAgentPathsError,
    tokenModelMappings,
    tokenModelMappingsLoading,
    tokenModelMappingsError,
    tokenModelAnalyzeReport,
    tokenModelAnalyzeProgress,
    tokenModelAnalyzing,
    tokenModelAnalyzeError,
    loadLocalAgentPaths,
    loadTokenModelMappings,
    loadTokenOfficialModels,
    addTokenOfficialModel,
    removeTokenOfficialModel,
    bootstrapTokenModelMappings,
    analyzeTokenModelMappings,
    setTokenModelMapping,
    approveTokenModelMapping,
    rejectTokenModelMapping,
    reopenTokenModelMapping,
    tokenInsightReport,
    tokenInsightLoading,
    tokenInsightError,
    tokenInsightAnalyzing,
    analyzeTokenInsights,
    clearTokenInsightReport,
    syncTokenCollector,
    onRangeChange,
    refreshTokenStats,
    loadTokenStats,
    loadTokenUsage,
    loadTokenRawLogs,
    loadRequestHealth,
    refreshTokenDatabaseView,
    startTokenDatabaseRefresh,
    stopTokenDatabaseRefresh,
  };
}
