import { ref } from "vue";
import type {
  RawLogReport,
  RequestHealthReport,
  TokenStatsReport,
  TokenTrackerSyncReport,
  TokenUsageReport,
} from "../types";
import { runCommand } from "./useLibrary";

const tokenStats = ref<TokenStatsReport | null>(null);
const tokenStatsLoading = ref(false);
const tokenStatsError = ref("");
const tokenStatsFrom = ref("");
const tokenStatsTo = ref("");

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

// Tokentracker 本地增量同步状态。同步只更新本地 cursor，不参与云端上传。
const tokenTrackerSyncing = ref(false);
const tokenTrackerSyncError = ref("");
const tokenTrackerSyncReport = ref<TokenTrackerSyncReport | null>(null);
let tokenTrackerSyncPromise: Promise<TokenTrackerSyncReport> | null = null;

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

async function loadTokenStats(from?: string, to?: string, refresh = false) {
  tokenStatsLoading.value = true;
  tokenStatsError.value = "";
  try {
    tokenStats.value = await runCommand<TokenStatsReport>("get_token_stats", {
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

const quickRanges = [
  { label: "今日", days: -2 },
  { label: "昨日", days: -3 },
  { label: "近7天", days: 7 },
  { label: "近14天", days: 14 },
  { label: "近30天", days: 30 },
  { label: "近90天", days: 90 },
  { label: "本月", days: 0 },
  { label: "本季度", days: -4 },
  { label: "今年", days: -5 },
  { label: "全部", days: -1 },
] as const;

function isCurrentRange(days: number) {
  if (days === -1) return !tokenStatsFrom.value && !tokenStatsTo.value;

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

  return (
    tokenStatsFrom.value === toLocalDate(from) &&
    tokenStatsTo.value === toLocalDate(to)
  );
}

function applyQuickRange(days: number) {
  setQuickRange(days);
  void loadTokenStats();
}

function onRangeChange() {
  void loadTokenStats();
}

function refreshTokenStats() {
  void loadTokenStats(tokenStatsFrom.value, tokenStatsTo.value, true);
}

async function syncTokenTracker(force = false): Promise<TokenTrackerSyncReport> {
  if (tokenTrackerSyncPromise) {
    return tokenTrackerSyncPromise;
  }
  tokenTrackerSyncing.value = true;
  tokenTrackerSyncError.value = "";
  const promise = runCommand<TokenTrackerSyncReport>("sync_token_tracker", { force })
    .then((report) => {
      tokenTrackerSyncReport.value = report;
      return report;
    })
    .catch((error) => {
      tokenTrackerSyncError.value = String(error);
      throw error;
    })
    .finally(() => {
      tokenTrackerSyncing.value = false;
      tokenTrackerSyncPromise = null;
    });
  tokenTrackerSyncPromise = promise;
  return promise;
}

async function loadTokenUsage() {
  tokenUsageLoading.value = true;
  tokenUsageError.value = "";
  try {
    tokenUsage.value = await runCommand<TokenUsageReport>("get_token_usage");
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
    rawLogs.value = await runCommand<RawLogReport>("get_token_raw_logs");
  } catch (error) {
    rawLogsError.value = String(error);
    rawLogs.value = null;
  } finally {
    rawLogsLoading.value = false;
  }
}

async function loadRequestHealth(refresh = false) {
  // 已有数据时后台刷新，不清空，避免对话数闪回 0
  const keepExisting = !!requestHealth.value && !refresh;
  if (!keepExisting) {
    requestHealthLoading.value = true;
  }
  requestHealthError.value = "";
  try {
    requestHealth.value = await runCommand<RequestHealthReport>("get_token_request_health", {
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
    tokenTrackerSyncing,
    tokenTrackerSyncError,
    tokenTrackerSyncReport,
    syncTokenTracker,
    quickRanges,
    isCurrentRange,
    applyQuickRange,
    onRangeChange,
    refreshTokenStats,
    loadTokenStats,
    loadTokenUsage,
    loadTokenRawLogs,
    loadRequestHealth,
    setQuickRange,
  };
}
