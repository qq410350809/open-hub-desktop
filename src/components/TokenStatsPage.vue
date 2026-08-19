<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { EChartsOption } from "../echarts";
import EChart from "./EChart.vue";
import QuickRangeDropdown from "./QuickRangeDropdown.vue";
import AppTable, { type AppTableColumn } from "./AppTable.vue";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import { usePreferences } from "../composables/usePreferences";
import {
  bucketModelTotals,
  bucketSourceTotals,
  bucketTotals,
  buildDailyMapFromBuckets,
  cacheHitRateOf,
  bucketKeyFor,
  buildHealthTimeline,
  buildPrecedingKeys,
  buildTrendDetailFromBuckets,
  healthLevelOf,
  buildTrendFromBuckets,
  estimateRequestCount,
  mergeModelTotals,
  formatCompact,
  formatCost,
  formatRate,
  formatTokens,
  isKnownModel,
  isKnownSource,
  localDateOf,
  parseLocal,
  toLocalDate,
} from "../composables/tokenStatsAgg";
import type { TrendGranularity } from "../composables/tokenStatsAgg";
import { isTauri, runCommand } from "../composables/useLibrary";

const store = useStore();
const { preferences } = usePreferences();

// —— 4 大深度分析弹窗状态 ——
const toolsModalOpen = ref(false);
const modelsModalOpen = ref(false);
const projectsModalOpen = ref(false);
const auditModalOpen = ref(false);
const healthModalOpen = ref(false);

// —— 趋势图表显示维度 ——
type TrendMetric = "total" | "breakdown" | "reasoning" | "requests";
const trendMetric = ref<TrendMetric>("total");

// —— 搜索与过滤 ——
const modelSearch = ref("");
const projectSearch = ref("");
const sourceSearch = ref("");

// —— 趋势粒度：根据顶部所选时间区间自动决定 X 轴粒度 ——
const trendGranularity = computed<TrendGranularity>(() => {
  const from = store.tokenStatsFrom.value;
  const to = store.tokenStatsTo.value;
  if (from && to) {
    const days = Math.round((parseLocal(to).getTime() - parseLocal(from).getTime()) / 86_400_000) + 1;
    if (days < 7) return "hour";
    if (days <= 92) return "day";
    return "month";
  }
  return "day";
});

function trendUnitLabel(): string {
  switch (trendGranularity.value) {
    case "hour": return "逐小时";
    case "day": return "逐日";
    case "month": return "逐月";
    default: return "逐日";
  }
}

// —— 统计重建状态与阶段日志 ——
type RefreshPhase = "confirm" | "running" | "success" | "error";
type RefreshLogStatus = "running" | "success" | "error";
type TokenCollectorProgress = {
  stage: string;
  status: RefreshLogStatus;
  message: string;
};
type RefreshLogEntry = TokenCollectorProgress & {
  id: number;
  time: string;
};

const refreshDialogOpen = ref(false);
const refreshPhase = ref<RefreshPhase>("confirm");
const refreshLogs = ref<RefreshLogEntry[]>([]);
const refreshLogListRef = ref<HTMLOListElement>();
let refreshLogId = 0;
let unlistenTokenCollectorProgress: UnlistenFn | undefined;
let tokenStatsPageMounted = true;

const refreshStageLabels: Record<string, string> = {
  prepare: "准备",
  cache: "缓存",
  scan: "扫描",
  aggregate: "汇总",
  database: "数据库",
  view: "页面",
  complete: "完成",
  error: "错误",
};

const refreshStatusTitle = computed(() => {
  if (refreshPhase.value === "running") return "正在重建 Token 统计";
  if (refreshPhase.value === "success") return "统计重建完成";
  if (refreshPhase.value === "error") return "统计重建失败";
  return "重建 Token 统计";
});

const refreshStatusDescription = computed(() => {
  if (refreshPhase.value === "running") return "正在重新读取多端日志并重建本地数据库，请稍候。";
  if (refreshPhase.value === "success") return "本地数据库与当前页面快照均已更新。";
  if (refreshPhase.value === "error") return "任务未能完成，请根据下方日志检查后重试。";
  return "清除本地临时解析缓存，重新完整扫描各 AI 工具的本地日志并写入 SQLite。";
});

function appendRefreshLog(progress: TokenCollectorProgress) {
  const last = refreshLogs.value[refreshLogs.value.length - 1];
  if (last?.stage === progress.stage && last.status === progress.status && last.message === progress.message) {
    return;
  }
  refreshLogs.value.push({
    ...progress,
    id: ++refreshLogId,
    time: new Date().toLocaleTimeString("zh-CN", {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
      hour12: false,
    }),
  });
}

function openRefreshDialog() {
  refreshDialogOpen.value = true;
  if (store.tokenCollectorSyncing.value) {
    refreshPhase.value = "running";
    return;
  }
  refreshPhase.value = "confirm";
  refreshLogs.value = [];
}

function closeRefreshDialog() {
  refreshDialogOpen.value = false;
}

async function startRefresh() {
  if (store.tokenCollectorSyncing.value) return;
  refreshPhase.value = "running";
  refreshLogs.value = [];
  appendRefreshLog({ stage: "prepare", status: "running", message: "统计重建请求已提交" });

  try {
    await store.syncTokenCollector(true);
  } catch (error) {
    appendRefreshLog({
      stage: "error",
      status: "error",
      message: String(error),
    });
    refreshPhase.value = "error";
    return;
  }

  appendRefreshLog({ stage: "view", status: "running", message: "正在重新读取数据库快照并更新页面" });
  try {
    await store.refreshTokenDatabaseView(true);
    appendRefreshLog({ stage: "view", status: "success", message: "页面快照已更新" });
    appendRefreshLog({ stage: "complete", status: "success", message: "统计重建全部完成" });
    refreshPhase.value = "success";
  } catch (error) {
    appendRefreshLog({
      stage: "view",
      status: "error",
      message: `页面更新失败：${String(error)}`,
    });
    refreshPhase.value = "error";
  }
}

// —— 本地 AI Agent 路径诊断弹窗 ——
const agentDialogOpen = ref(false);
const agentKindLabels: Record<string, string> = {
  config: "配置",
  data: "数据",
  database: "数据库",
  logs: "日志",
};
const localAgents = computed(() => store.localAgentPaths.value?.agents ?? []);
const localAgentsHome = computed(() => store.localAgentPaths.value?.home ?? "");
const localAgentEnvOverrides = computed(() => store.localAgentPaths.value?.envOverrides ?? []);
const detectedAgentsCount = computed(() => localAgents.value.filter((a) => a.detected).length);
const localAgentsCollectedAt = computed(() => {
  const raw = store.localAgentPaths.value?.collectedAt ?? "";
  return raw.length >= 16 ? raw.slice(5, 16).replace("T", " ") : "";
});

function formatAgentCount(value: number): string {
  return value >= 10000 ? `${(value / 1000).toFixed(1)}k` : String(value);
}

function displayAgentPath(path: string): string {
  const home = localAgentsHome.value;
  if (home && path.startsWith(`${home}/`)) return `~${path.slice(home.length)}`;
  return path;
}

function agentPathSegments(path: string): string[] {
  return displayAgentPath(path)
    .split("/")
    .filter(Boolean)
    .map((part, index, parts) => (index < parts.length - 1 ? `${part}/` : part));
}

function openAgentDialog() {
  agentDialogOpen.value = true;
  void store.loadLocalAgentPaths();
}

function closeAgentDialog() {
  agentDialogOpen.value = false;
}

async function copyAgentPath(path: string) {
  if (!path) return;
  try {
    await navigator.clipboard.writeText(path);
    store.showToast("已复制路径至剪贴板");
  } catch {
    store.showToast("复制失败", true);
  }
}

// —— 数据导出弹窗 ——
const exportDialogOpen = ref(false);
function openExportDialog() {
  exportDialogOpen.value = true;
}
function closeExportDialog() {
  exportDialogOpen.value = false;
}

async function downloadFile(filename: string, content: string, mimeType: string) {
  if (isTauri) {
    try {
      // Base64 编码以安全传输二进制内容（如 CSV 的 BOM）
      const encoder = new TextEncoder();
      const bytes = encoder.encode(content);
      let binary = "";
      for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
      const base64 = btoa(binary);
      const result = await runCommand<{ path: string | null; cancelled: boolean }>("save_export_file", {
        args: { filename, content: base64 },
      });
      if (result.cancelled) return;
      store.showToast(`已导出到 ${result.path}`);
    } catch (e) {
      store.showToast(`导出失败: ${e}`);
    }
  } else {
    const blob = new Blob([content], { type: mimeType });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = filename;
    a.click();
    URL.revokeObjectURL(url);
    store.showToast("已导出文件");
  }
}

async function exportDataAsJson() {
  const payload = {
    exportTime: new Date().toISOString(),
    timeRange: {
      from: store.tokenStatsFrom.value || null,
      to: store.tokenStatsTo.value || null,
      label: rangeLabel.value,
    },
    summary: {
      totalTokens: bucketTotal.value.total,
      inputTokens: rangeSplits.value.input,
      outputTokens: rangeSplits.value.output,
      cacheTokens: rangeSplits.value.cache,
      cacheHitRate: cacheHitRate.value,
      dailyAverage: dailyAverage.value,
      activeDays: activeDays.value,
      streakDays: streakDays.value,
      dialogues: healthTimeline.value.totalDialogues,
      requests: healthTimeline.value.totalRequests,
      estimatedCostUsd: estimatedCost.value,
    },
    sources: bySource.value,
    models: byModel.value,
    projects: projectUsage.value,
    trendDetails: trendDetailList.value,
  };
  await downloadFile(
    `openhub-token-stats-${toLocalDate(new Date())}.json`,
    JSON.stringify(payload, null, 2),
    "application/json",
  );
  closeExportDialog();
}

async function exportDataAsCsv() {
  const rows: string[] = [];
  rows.push("时间,总计Token,输入Token,输出Token,缓存Token,缓存命中率,推理Token,对话数,请求数");
  for (const item of trendDetailList.value) {
    const hitRate = item.cacheHitRate != null ? `${(item.cacheHitRate * 100).toFixed(2)}%` : "0%";
    rows.push(`"${item.label}",${item.total},${item.input},${item.output},${item.cache},"${hitRate}",${item.reasoning},${item.sessions},${item.requests}`);
  }
  await downloadFile(
    `openhub-token-trend-${toLocalDate(new Date())}.csv`,
    "\uFEFF" + rows.join("\n"),
    "text/csv;charset=utf-8;",
  );
  closeExportDialog();
}

// —— 表格列配置（宽度总和需控制在弹窗内容宽 ~920px 内，避免横向滚动条） ——
const dailyColumns: AppTableColumn[] = [
  { key: "label", title: "时间节点", width: "minmax(120px, 1.2fr)", sortable: true },
  { key: "total", title: "总量 Tokens", width: "88px", align: "right", sortable: true },
  { key: "input", title: "输入", width: "72px", align: "right", sortable: true },
  { key: "output", title: "输出", width: "72px", align: "right", sortable: true },
  { key: "cache", title: "缓存 (读+写)", width: "76px", align: "right", sortable: true },
  { key: "cacheHitRate", title: "缓存命中率", width: "80px", align: "right", sortable: true },
  { key: "reasoning", title: "深度推理", width: "72px", align: "right", sortable: true },
  { key: "sessions", title: "对话轮次", width: "72px", align: "right", sortable: true },
  { key: "requests", title: "API 请求数", width: "80px", align: "right", sortable: true },
];

const projectColumns: AppTableColumn[] = [
  { key: "project", title: "项目 / 工作区", width: "minmax(120px, 1.4fr)", sortable: true },
  { key: "totalTokens", title: "消耗总计", width: "88px", align: "right", sortable: true },
  { key: "share", title: "占比", width: "78px", align: "right", sortable: false },
  { key: "input", title: "输入", width: "72px", align: "right", sortable: true },
  { key: "output", title: "输出", width: "72px", align: "right", sortable: true },
  { key: "cache", title: "缓存", width: "72px", align: "right", sortable: true },
  { key: "cacheHitRate", title: "缓存命中率", width: "80px", align: "right", sortable: true },
  { key: "reasoning", title: "推理", width: "72px", align: "right", sortable: true },
  { key: "sessions", title: "对话轮次", width: "72px", align: "right", sortable: true },
  { key: "requests", title: "请求数", width: "72px", align: "right", sortable: true },
  { key: "costUsd", title: "估算成本", width: "90px", align: "right", sortable: true },
];

const sourceColumns: AppTableColumn[] = [
  { key: "source", title: "工具 / 来源", width: "minmax(120px, 1.3fr)", sortable: true },
  { key: "totalTokens", title: "总量 Tokens", width: "88px", align: "right", sortable: true },
  { key: "share", title: "占比", width: "78px", align: "right", sortable: false },
  { key: "inputTokens", title: "输入", width: "72px", align: "right", sortable: true },
  { key: "outputTokens", title: "输出", width: "72px", align: "right", sortable: true },
  { key: "cacheTokens", title: "缓存", width: "72px", align: "right", sortable: true },
  { key: "cacheHitRate", title: "缓存命中率", width: "80px", align: "right", sortable: true },
  { key: "reasoningTokens", title: "推理", width: "72px", align: "right", sortable: true },
  { key: "conversations", title: "对话数", width: "72px", align: "right", sortable: true },
  { key: "requests", title: "请求数", width: "72px", align: "right", sortable: true },
  { key: "costUsd", title: "成本 (USD)", width: "90px", align: "right", sortable: true },
];

const healthColumns: AppTableColumn[] = [
  { key: "label", title: "时段", width: "minmax(120px, 1.2fr)", sortable: true },
  { key: "dialogues", title: "对话数", width: "80px", align: "right", sortable: true },
  { key: "requests", title: "请求数", width: "82px", align: "right", sortable: true },
  { key: "success", title: "成功", width: "78px", align: "right", sortable: true },
  { key: "failed", title: "失败", width: "78px", align: "right", sortable: true },
  { key: "successRate", title: "成功率", width: "84px", align: "right", sortable: true },
  { key: "level", title: "健康等级", width: "86px", align: "center", sortable: true },
];

const healthTableRows = computed(() =>
  healthDisplayCells.value
    .map((c) => ({
      ...c,
      successRate: c.successRate != null ? (c.successRate * 100).toFixed(1) + "%" : "—",
    })),
);

const modelColumns: AppTableColumn[] = [
  { key: "model", title: "模型名称 / 家族", width: "minmax(130px, 1.5fr)", sortable: true },
  { key: "totalTokens", title: "总量 Tokens", width: "88px", align: "right", sortable: true },
  { key: "share", title: "占比", width: "78px", align: "right", sortable: false },
  { key: "inputTokens", title: "输入", width: "72px", align: "right", sortable: true },
  { key: "outputTokens", title: "输出", width: "72px", align: "right", sortable: true },
  { key: "cacheTokens", title: "缓存", width: "72px", align: "right", sortable: true },
  { key: "cacheHitRate", title: "缓存命中率", width: "80px", align: "right", sortable: true },
  { key: "reasoningTokens", title: "推理", width: "72px", align: "right", sortable: true },
  { key: "conversations", title: "对话", width: "72px", align: "right", sortable: true },
  { key: "requests", title: "请求数", width: "72px", align: "right", sortable: true },
  { key: "costUsd", title: "成本 (USD)", width: "90px", align: "right", sortable: true },
];

const sessions = computed(() => store.tokenStats.value?.sessions ?? []);

// —— 供应商品牌色与配置 ——
const PROVIDER_COLORS: Record<string, string> = {
  claude: "#d97757",
  codex: "#3b82f6",
  cursor: "#8b5cf6",
  opencode: "#f59e0b",
  gemini: "#2196f3",
  antigravity: "#ff6900",
  kiro: "#a78bfa",
  copilot: "#6b7280",
  openclaw: "#facc15",
  goose: "#ef4444",
  zed: "#14b8a6",
  catpawai: "#ec4899",
  "command-code": "#10b981",
  dsh: "#1e88e5",
};

function providerColor(source: string, index = 0): string {
  return PROVIDER_COLORS[source.toLowerCase()] || `hsl(${150 + index * 40}, 60%, 45%)`;
}

const HEALTH_LEVEL_COLORS = ["rgba(148,163,184,0.4)", "#ef4444", "#f97316", "#eab308", "#84cc16", "#10b981"];
function healthLevelColor(level: number): string {
  return HEALTH_LEVEL_COLORS[level] ?? HEALTH_LEVEL_COLORS[0];
}

const sourceNameMap: Record<string, string> = {
  claude: "Claude Code",
  codex: "Codex CLI",
  cursor: "Cursor",
  catpawai: "CatPawAI",
  gemini: "Gemini CLI",
  opencode: "OpenCode",
  kiro: "Kiro",
  copilot: "GitHub Copilot",
  openclaw: "OpenClaw",
  goose: "Goose AI",
  antigravity: "Google Antigravity",
  zed: "Zed Editor",
  "command-code": "Command Code",
  dsh: "DeepSeek CLI (DSH)",
};

function sourceLabel(source: string): string {
  return sourceNameMap[source.toLowerCase()] || source || "未知来源";
}

function shareOf(value: number, total: number): number {
  return total > 0 ? Math.min(100, (value / total) * 100) : 0;
}

// —— 小时用量桶过滤 ——
const allBuckets = computed(() => store.tokenUsage.value?.buckets ?? []);
const filteredBuckets = computed(() => {
  const buckets = allBuckets.value;
  const from = store.tokenStatsFrom.value;
  const to = store.tokenStatsTo.value;
  return buckets.filter((bucket) => {
    if (!isKnownModel(bucket.model) || !isKnownSource(bucket.source)) return false;
    const day = localDateOf(bucket.timestamp);
    return (!from || day >= from) && (!to || day <= to);
  });
});

const dailyMap = computed(() => buildDailyMapFromBuckets(filteredBuckets.value));
const bucketTotal = computed(() => bucketTotals(filteredBuckets.value));

// —— KPI 指标计算 ——
const activeDays = computed(() => dailyMap.value.size);
const streakDays = computed(() => {
  const keys = [...dailyMap.value.keys()].sort();
  if (!keys.length) return 0;
  const cursor = parseLocal(keys[keys.length - 1]);
  let streak = 0;
  while (dailyMap.value.has(toLocalDate(cursor))) {
    streak += 1;
    cursor.setDate(cursor.getDate() - 1);
  }
  return streak;
});

const rangeLabel = computed(() => {
  const from = store.tokenStatsFrom.value;
  const to = store.tokenStatsTo.value;
  if (!from && !to) return "全部时间";
  if (from === to) return from;
  return `${from || "…"} ~ ${to || "…"}`;
});

const rangeDays = computed(() => {
  const from = store.tokenStatsFrom.value;
  const to = store.tokenStatsTo.value;
  if (!from || !to) return activeDays.value || 1;
  return Math.max(1, Math.round((parseLocal(to).getTime() - parseLocal(from).getTime()) / 86_400_000) + 1);
});

const dailyAverage = computed(() => {
  const days = Math.max(1, activeDays.value);
  return bucketTotal.value.total / days;
});

const rangeSplits = computed(() => {
  let input = 0;
  let output = 0;
  let cache = 0;
  let reasoning = 0;
  for (const stat of dailyMap.value.values()) {
    input += stat.input;
    output += stat.output;
    cache += stat.cache;
    reasoning += stat.reasoning;
  }
  return { input, output, cache, reasoning };
});

const cacheHitRate = computed(() => {
  let cacheRead = 0;
  let cacheWrite = 0;
  let fresh = 0;
  let estimatedInput = 0;
  for (const bucket of filteredBuckets.value) {
    cacheRead += bucket.cachedInputTokens || 0;
    cacheWrite += bucket.cacheCreationInputTokens || 0;
    fresh += bucket.inputTokens || 0;
    estimatedInput += bucket.estimatedInputTokens || 0;
  }
  return cacheHitRateOf(cacheRead, cacheWrite, fresh, estimatedInput);
});

// 缓存命中率评级
const cacheHitRateRating = computed(() => {
  const rate = cacheHitRate.value;
  if (rate == null || rate <= 0) return { label: "暂无缓存", class: "is-none" };
  if (rate >= 0.7) return { label: "极高效率 ⚡", class: "is-excellent" };
  if (rate >= 0.4) return { label: "良好 ✦", class: "is-good" };
  return { label: "偏低 · 可优化", class: "is-fair" };
});

// Prompt Caching 节约估算（Prompt 缓存平均每 100 万 Token 节约 ~$2.70）
const estimatedCacheSavings = computed(() => {
  const cachedTokens = rangeSplits.value.cache;
  if (cachedTokens <= 0) return 0;
  return (cachedTokens / 1_000_000) * 2.70;
});

// 成本汇总
const costSummary = computed(() => {
  let costUsd = 0;
  let pricedTokens = 0;
  let totalTokens = 0;
  for (const bucket of filteredBuckets.value) {
    const tokens = Math.max(0, bucket.totalTokens || 0);
    totalTokens += tokens;
    costUsd += Math.max(0, bucket.costUsd || 0);
    if (bucket.pricingAvailable) pricedTokens += tokens;
  }
  return {
    costUsd,
    pricedTokens,
    totalTokens,
    coverage: totalTokens > 0 ? pricedTokens / totalTokens : null,
  };
});

const estimatedCost = computed(() => costSummary.value.costUsd);
const costCaption = computed(() => {
  const { coverage, pricedTokens, totalTokens } = costSummary.value;
  if (totalTokens <= 0) return "按模型标准价估算";
  if (pricedTokens <= 0 || coverage == null) return "暂无定价上报";
  if (coverage >= 0.9995) return "多端日志实际账单 · 100% 覆盖";
  return `来源上报覆盖率 ${(coverage * 100).toFixed(1)}%`;
});

const totalTokensAll = computed(() => bucketTotal.value.total);

// 工具分布
const bySource = computed(() =>
  bucketSourceTotals(filteredBuckets.value).filter((item) => item.totalTokens > 0),
);
const filteredSources = computed(() => {
  const q = sourceSearch.value.trim().toLowerCase();
  if (!q) return bySource.value;
  return bySource.value.filter((s) =>
    s.source.toLowerCase().includes(q) || sourceLabel(s.source).toLowerCase().includes(q),
  );
});

// 模型分布
const byModel = computed(() =>
  mergeModelTotals(bucketModelTotals(filteredBuckets.value)).filter((item) => item.totalTokens > 0),
);
const filteredModels = computed(() => {
  const q = modelSearch.value.trim().toLowerCase();
  if (!q) return byModel.value;
  return byModel.value.filter((m) => m.model.toLowerCase().includes(q));
});

const topSources = computed(() => bySource.value.slice(0, 5));
const topModels = computed(() => byModel.value.slice(0, 5));

// 项目用量
type ProjectUsageItem = {
  project: string;
  sessions: number;
  requests: number;
  requestsEstimated: boolean;
  totalTokens: number;
  input: number;
  output: number;
  cache: number;
  cacheRead: number;
  cacheWrite: number;
  cacheHitRate: number | null;
  reasoning: number;
  costUsd: number;
  estimatedTokens: number;
};

const projectUsage = computed<ProjectUsageItem[]>(() => {
  const groups = new Map<
    string,
    {
      project: string;
      sessions: number;
      requests: number;
      requestsEstimated: boolean;
      totalTokens: number;
      input: number;
      output: number;
      cache: number;
      cacheRead: number;
      cacheWrite: number;
      cacheHitRate: number | null;
      reasoning: number;
      costUsd: number;
      estimatedTokens: number;
      estimatedInput: number;
    }
  >();

  const normalizeProject = (rawKey?: string) => {
    const value = rawKey?.trim() || "默认工作区";
    if (value.toLowerCase().includes("books")) return "books";
    return /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value)
      ? "其他临时任务"
      : value;
  };

  const ensureGroup = (rawKey?: string) => {
    const key = normalizeProject(rawKey);
    const current = groups.get(key) || {
      project: key,
      sessions: 0,
      requests: 0,
      requestsEstimated: false,
      totalTokens: 0,
      input: 0,
      output: 0,
      cache: 0,
      cacheRead: 0,
      cacheWrite: 0,
      cacheHitRate: null,
      reasoning: 0,
      costUsd: 0,
      estimatedTokens: 0,
      estimatedInput: 0,
    };
    groups.set(key, current);
    return current;
  };

  const projectBucketSources = new Set<string>();
  for (const bucket of filteredBuckets.value) {
    if (!bucket.projectKey?.trim()) continue;
    projectBucketSources.add(bucket.source.toLowerCase());
    const current = ensureGroup(bucket.projectKey);
    current.sessions += bucket.conversationCount || 0;
    if (bucket.requestCount != null) {
      current.requests += bucket.requestCount || 0;
    } else {
      current.requests += estimateRequestCount({
        conversationCount: bucket.conversationCount,
        outputTokens: bucket.outputTokens,
        reasoningOutputTokens: bucket.reasoningOutputTokens,
        totalTokens: bucket.totalTokens,
      });
      current.requestsEstimated = true;
    }
    current.totalTokens += bucket.totalTokens || 0;
    current.input += bucket.inputTokens || 0;
    current.output += bucket.outputTokens || 0;
    current.cache += (bucket.cachedInputTokens || 0) + (bucket.cacheCreationInputTokens || 0);
    current.cacheRead += bucket.cachedInputTokens || 0;
    current.cacheWrite += bucket.cacheCreationInputTokens || 0;
    current.reasoning += bucket.reasoningOutputTokens || 0;
    current.costUsd += bucket.costUsd || 0;
    current.estimatedTokens += bucket.estimatedTokens || 0;
    current.estimatedInput += bucket.estimatedInputTokens || 0;
  }

  for (const session of sessions.value) {
    if (projectBucketSources.has((session.source || "").toLowerCase())) continue;
    const current = ensureGroup(session.projectKey);
    const sessionTurns = session.turns || 0;
    current.sessions += sessionTurns;
    current.requests += estimateRequestCount({
      conversationCount: sessionTurns,
      outputTokens: session.tokens?.outputTokens,
      reasoningOutputTokens: session.tokens?.reasoningOutputTokens,
    });
    current.requestsEstimated = true;
    current.totalTokens += session.totalTokens || 0;
    current.input += session.tokens?.inputTokens || 0;
    current.output += session.tokens?.outputTokens || 0;
    current.cache += (session.tokens?.cachedInputTokens || 0) + (session.tokens?.cacheCreationInputTokens || 0);
    current.cacheRead += session.tokens?.cachedInputTokens || 0;
    current.cacheWrite += session.tokens?.cacheCreationInputTokens || 0;
    current.reasoning += session.tokens?.reasoningOutputTokens || 0;
    current.costUsd += session.costUsd || 0;
    const usageKind = String(session.provenance?.tokenUsage || "");
    if (usageKind.includes("estimated")) {
      current.estimatedTokens += session.totalTokens || 0;
      current.estimatedInput += session.tokens?.inputTokens || 0;
    }
  }

  return [...groups.values()]
    .map((item) => ({
      ...item,
      cacheHitRate: cacheHitRateOf(item.cacheRead, item.cacheWrite, item.input, item.estimatedInput),
    }))
    .filter((item) => item.totalTokens > 0)
    .sort((a, b) => b.totalTokens - a.totalTokens);
});

const filteredProjects = computed(() => {
  const q = projectSearch.value.trim().toLowerCase();
  if (!q) return projectUsage.value;
  return projectUsage.value.filter((p) => p.project.toLowerCase().includes(q));
});

// —— 趋势与明细数据 ——
const trendSeries = computed(() =>
  buildTrendFromBuckets(
    filteredBuckets.value,
    trendGranularity.value,
    store.tokenStatsFrom.value || undefined,
    store.tokenStatsTo.value || undefined,
  ),
);

const trendDetail = computed(() =>
  buildTrendDetailFromBuckets(
    filteredBuckets.value,
    trendGranularity.value,
    store.tokenStatsFrom.value || undefined,
    store.tokenStatsTo.value || undefined,
  ),
);

function formatDetailTime(label: string): string {
  switch (trendGranularity.value) {
    case "hour": {
      const m = label.match(/^(\d{4})-(\d{2}-\d{2}) (\d{2}:\d{2})$/);
      return m ? `${m[2]} ${m[3]}` : label;
    }
    case "day": {
      const m = label.match(/^\d{4}-(\d{2}-\d{2})$/);
      return m ? m[1] : label;
    }
    default:
      return label;
  }
}

// —— 请求健康时间线 ——
const healthTimeline = computed(() =>
  buildHealthTimeline(
    store.requestHealth.value?.buckets ?? [],
    trendGranularity.value,
    store.tokenStatsFrom.value || undefined,
    store.tokenStatsTo.value || undefined,
    (store.tokenUsage.value?.buckets ?? []).map((b) => ({
      timestamp: b.timestamp,
      conversationCount: b.conversationCount || 0,
      outputTokens: b.outputTokens || 0,
      reasoningOutputTokens: b.reasoningOutputTokens || 0,
      totalTokens: b.totalTokens || 0,
      requests: b.requestCount ?? undefined,
    })),
  ),
);

const requestsPerTurnLabel = computed(() => {
  const dialogues = healthTimeline.value.totalDialogues;
  if (!dialogues) return "—";
  const avg = healthTimeline.value.totalRequests / dialogues;
  return avg >= 10 ? String(Math.round(avg)) : avg.toFixed(1);
});

const requestsByLabel = computed(() => {
  const map = new Map<string, { requests: number; requestsEstimated: boolean }>();
  for (const cell of healthTimeline.value.cells) {
    map.set(cell.label, {
      requests: Math.max(0, cell.requests || 0),
      requestsEstimated: cell.requestsEstimated || false,
    });
  }
  return map;
});

const trendDetailList = computed(() =>
  trendDetail.value
    .filter((item) => item.total > 0)
    .map((item) => {
      const hit = requestsByLabel.value.get(item.label);
      return {
        ...item,
        requests: hit?.requests ?? 0,
        requestsEstimated: hit?.requestsEstimated ?? false,
      };
    }),
);

// —— 健康时间线网格测量 ——
const HEALTH_ROWS = 8;
const HEALTH_CELL = 12;
const HEALTH_GAP = 3;
const healthGridRef = ref<HTMLElement | null>(null);
const healthCols = ref(24);
let healthRo: ResizeObserver | null = null;

const healthStatusInfo = computed(() => {
  const rate = healthTimeline.value.successRate;
  if (rate == null) return { label: "状态正常", class: "is-excellent" };
  if (rate >= 0.98) return { label: "极佳", class: "is-excellent" };
  if (rate >= 0.90) return { label: "良好", class: "is-good" };
  if (rate >= 0.80) return { label: "波动", class: "is-fair" };
  return { label: "异常", class: "is-bad" };
});

function measureHealthGrid() {
  const el = healthGridRef.value;
  if (!el) return;
  const width = el.clientWidth || el.getBoundingClientRect().width;
  if (width <= 0) return;
  const targetColWidth = HEALTH_CELL + HEALTH_GAP;
  const cols = Math.max(12, Math.round((width + HEALTH_GAP) / targetColWidth));
  if (cols !== healthCols.value) healthCols.value = cols;
}

type HealthDisplayCell = {
  key: string;
  label: string;
  dialogues: number;
  success: number;
  failed: number;
  requests: number;
  requestsEstimated?: boolean;
  successRate: number | null;
  level: number;
  pad?: boolean;
};

const healthBucketMap = computed(() => {
  const map = new Map<string, { dialogues: number; requests: number; success: number; failed: number; usage: number; usageEstimated: boolean }>();
  for (const b of store.requestHealth.value?.buckets ?? []) {
    const { key } = bucketKeyFor(trendGranularity.value, b.hour);
    if (!key) continue;
    const cur = map.get(key) || { dialogues: 0, requests: 0, success: 0, failed: 0, usage: 0, usageEstimated: false };
    cur.dialogues += Number(b.dialogues || 0);
    cur.requests += b.requests || 0;
    cur.success += b.success || 0;
    cur.failed += b.failed || 0;
    map.set(key, cur);
  }
  for (const b of store.tokenUsage.value?.buckets ?? []) {
    const { key } = bucketKeyFor(trendGranularity.value, b.timestamp);
    if (!key) continue;
    const cur = map.get(key) || { dialogues: 0, requests: 0, success: 0, failed: 0, usage: 0, usageEstimated: false };
    if (b.requestCount != null) {
      cur.usage += b.requestCount || 0;
    } else {
      cur.usage += estimateRequestCount({
        conversationCount: b.conversationCount || 0,
        outputTokens: b.outputTokens || 0,
        reasoningOutputTokens: b.reasoningOutputTokens || 0,
        totalTokens: b.totalTokens || 0,
      });
      cur.usageEstimated = true;
    }
    map.set(key, cur);
  }
  return map;
});

const healthDisplayCells = computed<HealthDisplayCell[]>(() => {
  const source = healthTimeline.value.cells;
  const capacity = HEALTH_ROWS * Math.max(1, healthCols.value);
  let body: HealthDisplayCell[] = source.map((c) => ({ ...c, pad: false }));
  if (body.length > capacity) {
    body = body.slice(body.length - capacity);
  }

  const padCount = Math.max(0, capacity - body.length);
  if (padCount > 0) {
    const anchor = body[0]?.key || body[0]?.label || healthTimeline.value.startLabel || "";
    const preceding = buildPrecedingKeys(anchor, padCount, trendGranularity.value);
    const map = healthBucketMap.value;
    const mapped = preceding.map((p) => {
      const hit = map.get(p.key);
      const dialogues = hit?.dialogues ?? 0;
      const rawSuccess = hit?.success ?? 0;
      const rawFailed = hit?.failed ?? 0;
      const extractedRequests = hit?.requests ?? 0;
      const sampleRequests = rawSuccess + rawFailed;
      const usageRequests = hit?.usage ?? 0;
      const requests = extractedRequests > 0 ? extractedRequests : (usageRequests > 0 ? usageRequests : sampleRequests);
      const requestsEstimated = extractedRequests <= 0 && usageRequests > 0 && (hit?.usageEstimated ?? false);
      const failed = Math.max(0, rawFailed);
      const success = requests > 0
        ? Math.max(0, requests - Math.min(failed, requests))
        : rawSuccess;
      const successRate = requests > 0
        ? success / requests
        : (failed > 0 ? 0 : (sampleRequests > 0 ? rawSuccess / sampleRequests : null));
      return {
        key: `pre-${p.key}`,
        label: p.label,
        dialogues,
        success,
        failed,
        requests,
        requestsEstimated,
        successRate,
        level: healthLevelOf(successRate, requests > 0 || failed > 0, failed, requests),
        pad: false,
      };
    });
    body = [...mapped, ...body];
  }
  return body;
});

function healthCellTitle(cell: HealthDisplayCell): string {
  if (!cell.label && cell.pad) return "空档";
  if (!cell.label) return "—";
  const dialogues = cell.dialogues || 0;
  const dialoguePart = dialogues > 0 ? ` · 对话 ${formatTokens(dialogues)}` : "";
  if (cell.requests <= 0 && cell.failed <= 0) {
    return dialogues > 0
      ? `${cell.label}${dialoguePart} · 无请求`
      : `${cell.label} · 无请求`;
  }
  const reqPart = cell.requestsEstimated ? `≈${formatTokens(cell.requests)}` : formatTokens(cell.requests);
  const rateTxt = cell.successRate == null ? "—" : `${(cell.successRate * 100).toFixed(1)}%`;
  const failPart = cell.failed > 0
    ? ` · ⚠ 失败 ${formatTokens(cell.failed)}`
    : ` · 失败 0`;
  return `${cell.label}${dialoguePart} · 请求 ${reqPart} · 成功 ${formatTokens(cell.success)}${failPart} · 成功率 ${rateTxt}`;
}

// —— ECharts 交互配置 ——
const trendChartOption = computed<EChartsOption>(() => {
  const isDark = preferences.theme === "dark" || (preferences.theme === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  const textColor = isDark ? "#94a3b8" : "#64748b";
  const gridLineColor = isDark ? "rgba(255, 255, 255, 0.06)" : "rgba(0, 0, 0, 0.06)";
  const labels = trendDetail.value.map((item) => formatDetailTime(item.label));

  if (trendMetric.value === "breakdown") {
    return {
      tooltip: {
        trigger: "axis",
        axisPointer: { type: "shadow" },
        backgroundColor: isDark ? "rgba(15, 23, 42, 0.95)" : "rgba(255, 255, 255, 0.95)",
        borderColor: isDark ? "rgba(255, 255, 255, 0.15)" : "rgba(0, 0, 0, 0.1)",
        textStyle: { color: isDark ? "#f8fafc" : "#0f172a", fontSize: 12 },
      },
      legend: {
        data: ["输入 Tokens", "输出 Tokens", "Prompt 缓存", "推理 Tokens"],
        textStyle: { color: textColor, fontSize: 11 },
        top: 0,
        right: 10,
      },
      grid: { left: 45, right: 15, top: 35, bottom: 25 },
      xAxis: {
        type: "category",
        data: labels,
        axisLine: { lineStyle: { color: gridLineColor } },
        axisLabel: { color: textColor, fontSize: 10 },
      },
      yAxis: {
        type: "value",
        axisLabel: { color: textColor, fontSize: 10, formatter: (v: number) => formatCompact(v) },
        splitLine: { lineStyle: { color: gridLineColor, type: "dashed" } },
      },
      series: [
        {
          name: "输入 Tokens",
          type: "bar",
          stack: "total",
          data: trendDetail.value.map((i) => i.input),
          itemStyle: { color: "#0284c7" },
        },
        {
          name: "输出 Tokens",
          type: "bar",
          stack: "total",
          data: trendDetail.value.map((i) => i.output),
          itemStyle: { color: "#10b981" },
        },
        {
          name: "Prompt 缓存",
          type: "bar",
          stack: "total",
          data: trendDetail.value.map((i) => i.cache),
          itemStyle: { color: "#8b5cf6" },
        },
        {
          name: "推理 Tokens",
          type: "bar",
          stack: "total",
          data: trendDetail.value.map((i) => i.reasoning),
          itemStyle: { color: "#f59e0b" },
        },
      ],
    };
  }

  if (trendMetric.value === "requests") {
    const reqData = trendDetailList.value.map((i) => i.requests);
    return {
      tooltip: {
        trigger: "axis",
        backgroundColor: isDark ? "rgba(15, 23, 42, 0.95)" : "rgba(255, 255, 255, 0.95)",
        borderColor: isDark ? "rgba(255, 255, 255, 0.15)" : "rgba(0, 0, 0, 0.1)",
        textStyle: { color: isDark ? "#f8fafc" : "#0f172a", fontSize: 12 },
      },
      grid: { left: 45, right: 15, top: 20, bottom: 25 },
      xAxis: {
        type: "category",
        data: trendDetailList.value.map((i) => formatDetailTime(i.label)),
        axisLine: { lineStyle: { color: gridLineColor } },
        axisLabel: { color: textColor, fontSize: 10 },
      },
      yAxis: {
        type: "value",
        axisLabel: { color: textColor, fontSize: 10, formatter: (v: number) => formatTokens(v) },
        splitLine: { lineStyle: { color: gridLineColor, type: "dashed" } },
      },
      series: [
        {
          name: "API 请求数",
          type: "line",
          smooth: 0.3,
          symbol: "circle",
          symbolSize: 6,
          data: reqData,
          lineStyle: { width: 2.5, color: "#06b6d4" },
          itemStyle: { color: "#06b6d4" },
          areaStyle: {
            color: {
              type: "linear",
              x: 0,
              y: 0,
              x2: 0,
              y2: 1,
              colorStops: [
                { offset: 0, color: "rgba(6, 182, 212, 0.35)" },
                { offset: 1, color: "rgba(6, 182, 212, 0.0)" },
              ],
            },
          },
        },
      ],
    };
  }

  // 默认总用量折线面积图
  const values = trendSeries.value.map((i) => i.value);
  return {
    tooltip: {
      trigger: "axis",
      backgroundColor: isDark ? "rgba(15, 23, 42, 0.95)" : "rgba(255, 255, 255, 0.95)",
      borderColor: isDark ? "rgba(255, 255, 255, 0.15)" : "rgba(0, 0, 0, 0.1)",
      textStyle: { color: isDark ? "#f8fafc" : "#0f172a", fontSize: 12 },
      formatter: (params: any) => {
        const p = Array.isArray(params) ? params[0] : params;
        const index = p.dataIndex;
        const detail = trendDetail.value[index];
        if (!detail) return `${p.name}: ${formatCompact(p.value)}`;
        const hitRateStr = detail.cacheHitRate != null ? `${(detail.cacheHitRate * 100).toFixed(1)}%` : "—";
        return `
          <div style="font-weight: 600; margin-bottom: 4px;">${detail.label}</div>
          <div>总计: <strong>${formatCompact(detail.total)}</strong> (${formatTokens(detail.total)})</div>
          <div style="color: #0284c7;">输入: ${formatCompact(detail.input)}</div>
          <div style="color: #10b981;">输出: ${formatCompact(detail.output)}</div>
          <div style="color: #8b5cf6;">缓存: ${formatCompact(detail.cache)} (命中率 ${hitRateStr})</div>
          ${detail.reasoning > 0 ? `<div style="color: #f59e0b;">推理: ${formatCompact(detail.reasoning)}</div>` : ""}
          <div style="color: #06b6d4;">对话: ${detail.sessions} 轮</div>
        `;
      },
    },
    grid: { left: 45, right: 15, top: 20, bottom: 25 },
    xAxis: {
      type: "category",
      data: labels,
      axisLine: { lineStyle: { color: gridLineColor } },
      axisLabel: { color: textColor, fontSize: 10 },
    },
    yAxis: {
      type: "value",
      axisLabel: { color: textColor, fontSize: 10, formatter: (v: number) => formatCompact(v) },
      splitLine: { lineStyle: { color: gridLineColor, type: "dashed" } },
    },
    series: [
      {
        name: "Tokens",
        type: "line",
        data: values,
        smooth: 0.35,
        symbol: "circle",
        symbolSize: 6,
        lineStyle: { width: 3, color: "#10b981" },
        itemStyle: { color: "#10b981", borderColor: isDark ? "#0f172a" : "#ffffff", borderWidth: 2 },
        areaStyle: {
          color: {
            type: "linear",
            x: 0,
            y: 0,
            x2: 0,
            y2: 1,
            colorStops: [
              { offset: 0, color: "rgba(16, 185, 129, 0.4)" },
              { offset: 1, color: "rgba(16, 185, 129, 0.0)" },
            ],
          },
        },
      },
    ],
  };
});

watch(
  () => [healthTimeline.value.nodeCount, store.tokenStatsFrom.value, store.tokenStatsTo.value, trendGranularity.value],
  () => nextTick(() => measureHealthGrid()),
);

watch(
  () => refreshLogs.value.length,
  () => nextTick(() => {
    if (refreshLogListRef.value) {
      refreshLogListRef.value.scrollTop = refreshLogListRef.value.scrollHeight;
    }
  }),
);

onMounted(() => {
  tokenStatsPageMounted = true;
  if (isTauri) {
    listen<TokenCollectorProgress>("token-collector-progress", ({ payload }) => {
      appendRefreshLog(payload);
    }).then((unlisten) => {
      if (!tokenStatsPageMounted) unlisten();
      else unlistenTokenCollectorProgress = unlisten;
    });
  }

  nextTick(() => {
    measureHealthGrid();
    if (typeof ResizeObserver !== "undefined" && healthGridRef.value) {
      healthRo = new ResizeObserver(() => measureHealthGrid());
      healthRo.observe(healthGridRef.value);
    }
    window.addEventListener("resize", measureHealthGrid);
  });
});

onBeforeUnmount(() => {
  tokenStatsPageMounted = false;
  unlistenTokenCollectorProgress?.();
  unlistenTokenCollectorProgress = undefined;
  healthRo?.disconnect();
  healthRo = null;
  window.removeEventListener("resize", measureHealthGrid);
});
</script>

<template>
  <main class="token-stats-page tt-dashboard">
    <!-- 顶部宏观智控驾驶舱 (Macro Cockpit Bar) -->
    <header class="tt-cockpit-bar">
      <div class="tt-cockpit-left">
        <div class="tt-brand-section">
          <div class="tt-eyebrow-row">
            <span class="tt-live-dot" />
            <span class="tt-eyebrow-text">Token 用量分析中心</span>
          </div>
          <div class="tt-title-row">
            <h1>Token 统计中心</h1>
          </div>
          <p class="tt-cockpit-subtitle">
            全端本地日志采集 · SQLite 快照 · 覆盖 <strong>{{ bySource.length }}</strong> 款 AI 工具与 <strong>{{ byModel.length }}</strong> 个模型
          </p>
        </div>
      </div>

      <div class="tt-cockpit-right">
        <button
          type="button"
          class="tt-btn-rebuild"
          :disabled="!store.tokenCollectorSyncing.value && (store.tokenStatsLoading.value || store.tokenUsageLoading.value)"
          @click="openRefreshDialog"
        >
          <span :class="{ 'is-spinning': store.tokenStatsLoading.value || store.tokenCollectorSyncing.value }" v-html="icons.restore" />
          <span>{{ store.tokenCollectorSyncing.value ? "查看重建日志" : "重建统计" }}</span>
        </button>

        <button
          type="button"
          class="tt-btn-secondary"
          @click="openAgentDialog"
        >
          <span v-html="icons.cpu" />
          <span>本地 Agent</span>
          <span v-if="detectedAgentsCount > 0" class="tt-agent-count-chip">{{ detectedAgentsCount }} 在线</span>
        </button>

        <button
          type="button"
          class="tt-btn-secondary"
          @click="openExportDialog"
        >
          <span v-html="icons.download" />
          <span>导出报表</span>
        </button>
      </div>
    </header>

    <!-- 标题下方的筛选工具条：日期选择 + 维度弹窗按钮 -->
    <div class="tt-filter-toolbar">
      <QuickRangeDropdown />

      <div class="tt-cockpit-pills-group">
        <button
          type="button"
          class="tt-pill-btn"
          title="查看工具明细"
          @click="toolsModalOpen = true"
        >
          <span v-html="icons.cpu" />
          <span>工具 ({{ bySource.length }})</span>
        </button>

        <button
          type="button"
          class="tt-pill-btn"
          title="查看模型排行榜"
          @click="modelsModalOpen = true"
        >
          <span v-html="icons.database" />
          <span>模型 ({{ byModel.length }})</span>
        </button>

        <button
          type="button"
          class="tt-pill-btn"
          title="查看各项目与工作区用量透视"
          @click="projectsModalOpen = true"
        >
          <span v-html="icons.folder" />
          <span>项目 ({{ projectUsage.length }})</span>
        </button>

        <button
          type="button"
          class="tt-pill-btn"
          title="查看逐日逐时时序总账"
          @click="auditModalOpen = true"
        >
          <span v-html="icons.sliders" />
          <span>明细总账</span>
        </button>

        <button
          type="button"
          class="tt-pill-btn"
          title="查看请求健康矩阵明细"
          @click="healthModalOpen = true"
        >
          <span v-html="icons.activity" />
          <span>健康矩阵</span>
        </button>
      </div>
    </div>

    <!-- 首页零滚动条主视口 (No-Scroll Viewport Layout) -->
    <div class="tt-dashboard-body">
      <!-- 错误提示 -->
      <div v-if="store.tokenUsageError.value" class="tt-error-banner" role="alert">
        <span class="tt-error-icon" v-html="icons.alert" />
        <div class="tt-error-content">
          <strong>读取 Token 数据异常</strong>
          <p>{{ store.tokenUsageError.value }}</p>
          <small>OpenHub 会直接读取 Codex, Claude, Cursor, Antigravity, OpenCode, Kiro, Goose, Zed, Copilot 与 CatPawAI 的本地记录。</small>
        </div>
      </div>

      <!-- 加载中 -->
      <div v-if="store.tokenUsageLoading.value && !store.tokenUsage.value" class="tt-loading-card">
        <div class="tt-loading-spinner" />
        <p>正在读取本地 SQLite 数据库用量快照…</p>
      </div>

      <template v-else-if="store.tokenUsage.value">
        <!-- ROW 1: 4 大核心 KPI 指标卡 (Compact Bento Deck) -->
        <section class="tt-kpi-deck" aria-label="核心指标大盘">
          <!-- KPI 1: Token 消耗大盘 -->
          <div class="tt-kpi-card tt-kpi-total">
            <div class="tt-kpi-card-inner">
              <div class="tt-kpi-header">
                <span class="tt-kpi-tag is-emerald">
                  <span v-html="icons.chart" />
                  <span>总用量</span>
                </span>
                <span class="tt-kpi-badge-hit" :class="cacheHitRateRating.class">
                  ⚡ 缓存命中率 {{ formatRate(cacheHitRate) }}
                </span>
              </div>
              <div class="tt-kpi-main-val">
                <strong>{{ formatCompact(bucketTotal.total) }}</strong>
                <span class="tt-kpi-unit">Tokens</span>
              </div>
              <div class="tt-kpi-progress-bar">
                <div
                  class="tt-prog-seg is-in"
                  :style="{ width: `${shareOf(rangeSplits.input, bucketTotal.total)}%` }"
                  :title="`输入: ${formatCompact(rangeSplits.input)} (${shareOf(rangeSplits.input, bucketTotal.total).toFixed(1)}%)`"
                />
                <div
                  class="tt-prog-seg is-out"
                  :style="{ width: `${shareOf(rangeSplits.output, bucketTotal.total)}%` }"
                  :title="`输出: ${formatCompact(rangeSplits.output)} (${shareOf(rangeSplits.output, bucketTotal.total).toFixed(1)}%)`"
                />
                <div
                  class="tt-prog-seg is-cache"
                  :style="{ width: `${shareOf(rangeSplits.cache, bucketTotal.total)}%` }"
                  :title="`缓存: ${formatCompact(rangeSplits.cache)} (${shareOf(rangeSplits.cache, bucketTotal.total).toFixed(1)}%)`"
                />
              </div>
              <div class="tt-kpi-sub-pills">
                <span class="tt-sub-pill in"><i></i>输入 {{ formatCompact(rangeSplits.input) }}</span>
                <span class="tt-sub-pill out"><i></i>输出 {{ formatCompact(rangeSplits.output) }}</span>
                <span class="tt-sub-pill cache"><i></i>缓存 {{ formatCompact(rangeSplits.cache) }}</span>
                <span v-if="rangeSplits.reasoning > 0" class="tt-sub-pill reasoning"><i></i>推理 {{ formatCompact(rangeSplits.reasoning) }}</span>
              </div>
            </div>
          </div>

          <!-- KPI 2: 日均与连击活跃 -->
          <div class="tt-kpi-card">
            <div class="tt-kpi-card-inner">
              <div class="tt-kpi-header">
                <span class="tt-kpi-tag is-orange">
                  <span v-html="icons.flame" />
                  <span>消耗速率与连续</span>
                </span>
                <span v-if="streakDays > 1" class="tt-kpi-streak-pill">
                  🔥 连续 {{ streakDays }} 天
                </span>
              </div>
              <div class="tt-kpi-main-val">
                <strong>{{ formatCompact(dailyAverage) }}</strong>
                <span class="tt-kpi-unit">/ 活跃日均</span>
              </div>
              <div class="tt-kpi-meta-text">
                <span>跨度 <strong>{{ rangeDays }}</strong> 天 · 活跃 <strong>{{ activeDays }}</strong> 天</span>
                <span v-if="rangeDays > 0" class="tt-active-rate-badge">活跃率 {{ ((activeDays / rangeDays) * 100).toFixed(0) }}%</span>
              </div>
              <div class="tt-kpi-footer-note">
                统计区间: <code>{{ rangeLabel }}</code>
              </div>
            </div>
          </div>

          <!-- KPI 3: 会话与并发 API 调用 -->
          <div class="tt-kpi-card">
            <div class="tt-kpi-card-inner">
              <div class="tt-kpi-header">
                <span class="tt-kpi-tag is-blue">
                  <span v-html="icons.activity" />
                  <span>对话与请求</span>
                </span>
                <span class="tt-kpi-badge-rate">
                  {{ healthTimeline.successRate != null ? (healthTimeline.successRate * 100).toFixed(1) + "% 成功率" : "—" }}
                </span>
              </div>
              <div class="tt-kpi-main-val">
                <strong>{{ formatTokens(healthTimeline.totalDialogues) }}</strong>
                <span class="tt-kpi-unit">轮对话</span>
              </div>
              <div class="tt-kpi-meta-text">
                <span>真实 API 调用 <strong>{{ formatTokens(healthTimeline.totalRequests) }}</strong> 次</span>
              </div>
              <div class="tt-kpi-multiplier-pill">
                <span>平均每轮触发 <strong>{{ requestsPerTurnLabel }}</strong> 次模型调用</span>
              </div>
            </div>
          </div>

          <!-- KPI 4: 成本估算与价值洞察 -->
          <div class="tt-kpi-card">
            <div class="tt-kpi-card-inner">
              <div class="tt-kpi-header">
                <span class="tt-kpi-tag is-purple">
                  <span v-html="icons.card" />
                  <span>经济价值</span>
                </span>
                <span v-if="estimatedCacheSavings > 0" class="tt-kpi-savings-pill">
                  ⚡ 缓存节约 ≈ ${{ estimatedCacheSavings.toFixed(2) }}
                </span>
              </div>
              <div class="tt-kpi-main-val">
                <template v-if="costSummary.pricedTokens > 0">
                  <strong>{{ formatCost(estimatedCost) }}</strong>
                  <span class="tt-kpi-unit">USD</span>
                </template>
                <template v-else>
                  <strong class="tt-val-unpriced">未定价 / 来源未上报</strong>
                </template>
              </div>
              <div class="tt-kpi-meta-text">
                <span>{{ costCaption }}</span>
              </div>
              <div class="tt-kpi-footer-note">
                <span v-if="bucketTotal.total > 0 && costSummary.costUsd > 0">
                  均价 ≈ ${{ ((costSummary.costUsd / bucketTotal.total) * 1_000_000).toFixed(3) }} / 1M Tokens
                </span>
                <span v-else>多端日志自动提取实际账单金额</span>
              </div>
            </div>
          </div>
        </section>

        <!-- 4 大核心全景图表与分布四等分大盘 (Equal 4-Quadrant Grid) -->
        <section class="tt-quad-grid">
          <!-- 1. 趋势图卡片 -->
          <div class="tt-card tt-chart-card">
            <header class="tt-card-header">
              <div class="tt-card-title-wrap">
                <h2>Token 消耗趋势</h2>
                <p>按 {{ trendUnitLabel() }} 聚合 · 共 {{ trendSeries.length }} 个时序节点</p>
              </div>
              <div class="tt-metric-switches">
                <button
                  type="button"
                  class="tt-metric-btn"
                  :class="{ active: trendMetric === 'total' }"
                  @click="trendMetric = 'total'"
                >总用量</button>
                <button
                  type="button"
                  class="tt-metric-btn"
                  :class="{ active: trendMetric === 'breakdown' }"
                  @click="trendMetric = 'breakdown'"
                >分项堆叠</button>
                <button
                  type="button"
                  class="tt-metric-btn"
                  :class="{ active: trendMetric === 'requests' }"
                  @click="trendMetric = 'requests'"
                >API 请求数</button>
              </div>
            </header>
            <div class="tt-card-body tt-chart-body">
              <EChart v-if="trendSeries.length" :option="trendChartOption" height="100%" />
              <div v-else class="tt-empty-state">当前时间区间内暂无时序记录</div>
            </div>
          </div>

          <!-- 2. 请求健康热力时间线 -->
          <div class="tt-card tt-health-card">
            <header class="tt-card-header">
              <div class="tt-card-title-wrap">
                <h2>请求健康矩阵</h2>
                <p>色阶按成功率：≥99% 绿 · 95–99% 浅绿 · 85–95% 黄 · 70–85% 橙 · &lt;70% 红</p>
              </div>
              <button type="button" class="tt-text-btn" @click="healthModalOpen = true">
                查看明细 ➔
              </button>
            </header>
            <div class="tt-card-body tt-health-body">
              <div v-if="healthTimeline.cells.length" class="tt-health-wrapper">
                <!-- 顶部 4 大健康遥测微指标 -->
                <div class="tt-health-kpi-bar">
                  <div class="tt-hk-card">
                    <span class="tt-hk-lbl">综合成功率</span>
                    <div class="tt-hk-val-box">
                      <strong class="tt-hk-num" :class="healthTimeline.successRate != null && healthTimeline.successRate < 0.95 ? 'text-warning' : 'text-success'">
                        {{ healthTimeline.successRate != null ? (healthTimeline.successRate * 100).toFixed(1) + '%' : '100%' }}
                      </strong>
                      <span class="tt-hk-badge" :class="healthStatusInfo.class">{{ healthStatusInfo.label }}</span>
                    </div>
                  </div>
                  <div class="tt-hk-card">
                    <span class="tt-hk-lbl">请求吞吐量</span>
                    <div class="tt-hk-val-box">
                      <strong class="tt-hk-num">{{ formatTokens(healthTimeline.totalRequests) }}</strong>
                      <small class="tt-hk-unit">次</small>
                    </div>
                  </div>
                  <div class="tt-hk-card">
                    <span class="tt-hk-lbl">异常/失败</span>
                    <div class="tt-hk-val-box">
                      <strong class="tt-hk-num" :class="{ 'text-danger': healthTimeline.totalFailed > 0 }">
                        {{ formatTokens(healthTimeline.totalFailed) }}
                      </strong>
                      <small class="tt-hk-unit">次</small>
                    </div>
                  </div>
                  <div class="tt-hk-card">
                    <span class="tt-hk-lbl">活跃监测时段</span>
                    <div class="tt-hk-val-box">
                      <strong class="tt-hk-num">{{ healthTimeline.activeCount }}</strong>
                      <small class="tt-hk-unit">/ {{ healthTimeline.nodeCount }}</small>
                    </div>
                  </div>
                </div>

                <!-- 热力矩阵主体 -->
                <div
                  ref="healthGridRef"
                  class="tt-health-grid"
                  :style="{ gridTemplateRows: `repeat(${HEALTH_ROWS}, 1fr)` }"
                >
                  <div
                    v-for="cell in healthDisplayCells"
                    :key="cell.key"
                    class="tt-health-cell"
                    :class="['lv' + cell.level, { 'is-pad': cell.pad }]"
                    :title="healthCellTitle(cell)"
                  />
                </div>

                <!-- 底部图例 -->
                <div class="tt-health-legend">
                  <span>故障频发</span>
                  <span class="tt-health-cell lv1" title="成功率 < 70%" />
                  <span class="tt-health-cell lv2" title="70% ~ 85%" />
                  <span class="tt-health-cell lv3" title="85% ~ 95%" />
                  <span class="tt-health-cell lv4" title="95% ~ 99%" />
                  <span class="tt-health-cell lv5" title="≥ 99%" />
                  <span>极度健康</span>
                  <span class="tt-health-cell lv0" />
                  <span class="muted">无请求</span>
                  <span class="tt-legend-meta">· 共 {{ healthTimeline.nodeCount }} 节点 · 活跃 {{ healthTimeline.activeCount }}</span>
                </div>
              </div>
              <div v-else class="tt-empty-state">当前时间区间内暂无请求健康记录</div>
            </div>
          </div>

          <!-- 3. 工具消耗分布 -->
          <div class="tt-card tt-preview-card">
            <header class="tt-card-header">
              <div>
                <h3>主要工具消耗分布</h3>
                <p>Top 5 客户端用量占比</p>
              </div>
              <button type="button" class="tt-text-btn" @click="toolsModalOpen = true">
                查看明细 ➔
              </button>
            </header>
            <div class="tt-card-body tt-preview-body">
              <div v-for="(item, idx) in topSources.slice(0, 5)" :key="item.source" class="tt-bar-row">
                <span class="tt-bar-dot" :style="{ background: providerColor(item.source, idx) }" />
                <span class="tt-bar-label">{{ sourceLabel(item.source) }}</span>
                <div class="tt-bar-track">
                  <div class="tt-bar-fill" :style="{ width: `${shareOf(item.totalTokens, totalTokensAll)}%`, background: providerColor(item.source, idx) }" />
                </div>
                <span class="tt-bar-pct">{{ shareOf(item.totalTokens, totalTokensAll).toFixed(1) }}%</span>
                <strong class="tt-bar-val">{{ formatCompact(item.totalTokens) }}</strong>
              </div>
            </div>
          </div>

          <!-- 4. 模型消耗排行 -->
          <div class="tt-card tt-preview-card">
            <header class="tt-card-header">
              <div>
                <h3>主要模型消耗排行</h3>
                <p>Top 5 旗舰模型用量占比</p>
              </div>
              <button type="button" class="tt-text-btn" @click="modelsModalOpen = true">
                查看排行榜 ➔
              </button>
            </header>
            <div class="tt-card-body tt-preview-body">
              <div v-for="(model, idx) in topModels.slice(0, 5)" :key="model.model" class="tt-bar-row">
                <span class="tt-bar-dot" :style="{ background: providerColor(model.model, idx) }" />
                <span class="tt-bar-label font-mono" :title="model.model">{{ model.model }}</span>
                <div class="tt-bar-track">
                  <div class="tt-bar-fill" :style="{ width: `${shareOf(model.totalTokens, totalTokensAll)}%`, background: providerColor(model.model, idx) }" />
                </div>
                <span class="tt-bar-pct">{{ shareOf(model.totalTokens, totalTokensAll).toFixed(1) }}%</span>
                <strong class="tt-bar-val">{{ formatCompact(model.totalTokens) }}</strong>
              </div>
            </div>
          </div>
        </section>
      </template>
    </div>

    <!-- ============================================================
         4 大深度分析弹窗 (Popups / Modal Drawers)
         ============================================================ -->

    <!-- 1. 工具与来源全景分析弹窗 (Tools Modal) -->
    <Transition name="tt-modal-fade">
      <div v-if="toolsModalOpen" class="tt-modal-backdrop" @click.self="toolsModalOpen = false">
        <section class="tt-modal-card is-wide" role="dialog" aria-modal="true">
          <header class="tt-modal-header">
            <div>
              <h2>工具与来源全景分析</h2>
              <p>覆盖本机探测到的所有 AI 编程工具与编辑器客户端用量</p>
            </div>
            <button type="button" class="tt-modal-close-btn" aria-label="关闭" @click="toolsModalOpen = false">×</button>
          </header>

          <div class="tt-modal-body">
            <div class="tt-filter-bar">
              <label class="tt-search-input">
                <span v-html="icons.search" />
                <input v-model="sourceSearch" type="search" placeholder="搜索 AI 工具、CLI 或编辑器…" />
              </label>
              <span class="tt-filter-count">共 {{ filteredSources.length }} 款工具</span>
            </div>

            <!-- 完整工具数据表 -->
            <div class="tt-table-wrap">
              <AppTable
                :rows="filteredSources"
                :columns="sourceColumns"
                :row-key="(item: any) => item.source"
                :page-size="10"
                empty-text="没有匹配的工具数据"
              >
                <template #cell-source="{ row }">
                  <div class="tt-cell-with-dot">
                    <span class="tt-bar-dot" :style="{ background: providerColor(row.source) }" />
                    <strong>{{ sourceLabel(row.source) }}</strong>
                    <code class="tt-muted-code">({{ row.source }})</code>
                  </div>
                </template>
                <template #cell-totalTokens="{ row }"><strong>{{ formatCompact(row.totalTokens) }}</strong></template>
                <template #cell-share="{ row }">{{ shareOf(row.totalTokens, totalTokensAll).toFixed(2) }}%</template>
                <template #cell-inputTokens="{ row }">{{ formatCompact(row.inputTokens) }}</template>
                <template #cell-outputTokens="{ row }">{{ formatCompact(row.outputTokens) }}</template>
                <template #cell-cacheTokens="{ row }">{{ formatCompact(row.cacheTokens) }}</template>
                <template #cell-cacheHitRate="{ row }">{{ formatRate(row.cacheHitRate) }}</template>
                <template #cell-reasoningTokens="{ row }">{{ formatCompact(row.reasoningTokens) }}</template>
                <template #cell-conversations="{ row }">{{ formatTokens(row.conversations) }}</template>
                <template #cell-requests="{ row }">{{ formatTokens(row.requests) }}</template>
                <template #cell-costUsd="{ row }">{{ row.costUsd > 0 ? formatCost(row.costUsd) : "—" }}</template>
              </AppTable>
            </div>
          </div>
        </section>
      </div>
    </Transition>

    <!-- 2. 模型排行榜与家族透视弹窗 (Models Modal) -->
    <Transition name="tt-modal-fade">
      <div v-if="modelsModalOpen" class="tt-modal-backdrop" @click.self="modelsModalOpen = false">
        <section class="tt-modal-card is-wide" role="dialog" aria-modal="true">
          <header class="tt-modal-header">
            <div>
              <h2>模型排行榜与家族透视</h2>
              <p>按 Token 消耗总量倒序排列 · 同系列模型智能归并</p>
            </div>
            <button type="button" class="tt-modal-close-btn" aria-label="关闭" @click="modelsModalOpen = false">×</button>
          </header>

          <div class="tt-modal-body">
            <div class="tt-filter-bar">
              <label class="tt-search-input">
                <span v-html="icons.search" />
                <input v-model="modelSearch" type="search" placeholder="搜索模型名称（如 claude-3-7, gpt-4o, r1）…" />
              </label>
              <span class="tt-filter-count">共 {{ filteredModels.length }} 款模型</span>
            </div>

            <div class="tt-table-wrap">
              <AppTable
                :rows="filteredModels"
                :columns="modelColumns"
                :row-key="(item: any) => item.model"
                :page-size="15"
                empty-text="没有匹配的模型数据"
              >
                <template #cell-model="{ row }">
                  <div class="tt-cell-with-dot">
                    <span class="tt-bar-dot" :style="{ background: providerColor(row.model) }" />
                    <strong class="font-mono">{{ row.model }}</strong>
                  </div>
                </template>
                <template #cell-totalTokens="{ row }"><strong>{{ formatCompact(row.totalTokens) }}</strong></template>
                <template #cell-share="{ row }">{{ shareOf(row.totalTokens, totalTokensAll).toFixed(2) }}%</template>
                <template #cell-inputTokens="{ row }">{{ formatCompact(row.inputTokens) }}</template>
                <template #cell-outputTokens="{ row }">{{ formatCompact(row.outputTokens) }}</template>
                <template #cell-cacheTokens="{ row }">{{ formatCompact(row.cacheTokens) }}</template>
                <template #cell-cacheHitRate="{ row }">{{ formatRate(row.cacheHitRate) }}</template>
                <template #cell-reasoningTokens="{ row }">{{ formatCompact(row.reasoningTokens) }}</template>
                <template #cell-conversations="{ row }">{{ formatTokens(row.conversations) }}</template>
                <template #cell-requests="{ row }">{{ formatTokens(row.requests) }}</template>
                <template #cell-costUsd="{ row }">{{ row.costUsd > 0 ? formatCost(row.costUsd) : "—" }}</template>
              </AppTable>
            </div>
          </div>
        </section>
      </div>
    </Transition>

    <!-- 3. 项目与工作区透视弹窗 (Projects Modal) -->
    <Transition name="tt-modal-fade">
      <div v-if="projectsModalOpen" class="tt-modal-backdrop" @click.self="projectsModalOpen = false">
        <section class="tt-modal-card is-wide" role="dialog" aria-modal="true">
          <header class="tt-modal-header">
            <div>
              <h2>项目与工作区透视</h2>
              <p>从本地日志中自动提取的项目目录与工作区用量</p>
            </div>
            <button type="button" class="tt-modal-close-btn" aria-label="关闭" @click="projectsModalOpen = false">×</button>
          </header>

          <div class="tt-modal-body">
            <div class="tt-filter-bar">
              <label class="tt-search-input">
                <span v-html="icons.search" />
                <input v-model="projectSearch" type="search" placeholder="按项目名称或路径过滤…" />
              </label>
              <span class="tt-filter-count">共 {{ filteredProjects.length }} 个工作区</span>
            </div>

            <div class="tt-table-wrap">
              <AppTable
                :rows="filteredProjects"
                :columns="projectColumns"
                :row-key="(item: any) => item.project"
                :page-size="15"
                empty-text="没有匹配的项目记录"
              >
                <template #cell-project="{ row }">
                  <div class="tt-project-cell" :title="row.project">
                    <span v-html="icons.folder" />
                    <strong>{{ row.project }}</strong>
                  </div>
                </template>
                <template #cell-totalTokens="{ row }"><strong>{{ formatCompact(row.totalTokens) }}</strong></template>
                <template #cell-share="{ row }">{{ shareOf(row.totalTokens, totalTokensAll).toFixed(2) }}%</template>
                <template #cell-input="{ row }">{{ formatCompact(row.input) }}</template>
                <template #cell-output="{ row }">{{ formatCompact(row.output) }}</template>
                <template #cell-cache="{ row }">{{ formatCompact(row.cache) }}</template>
                <template #cell-cacheHitRate="{ row }">{{ formatRate(row.cacheHitRate) }}</template>
                <template #cell-reasoning="{ row }">{{ formatCompact(row.reasoning) }}</template>
                <template #cell-sessions="{ row }">{{ formatTokens(row.sessions) }}</template>
                <template #cell-requests="{ row }">{{ row.requestsEstimated ? "≈" : "" }}{{ formatTokens(row.requests) }}</template>
                <template #cell-costUsd="{ row }">{{ row.costUsd > 0 ? formatCost(row.costUsd) : "—" }}</template>
              </AppTable>
            </div>
          </div>
        </section>
      </div>
    </Transition>

    <!-- 4. 逐日/逐时明细总账弹窗 (Audit Modal) -->
    <Transition name="tt-modal-fade">
      <div v-if="auditModalOpen" class="tt-modal-backdrop" @click.self="auditModalOpen = false">
        <section class="tt-modal-card is-wide" role="dialog" aria-modal="true">
          <header class="tt-modal-header">
            <div>
              <h2>时序明细总账 (Granular Audit Ledger)</h2>
              <p>按当前所选时间跨度 · {{ trendUnitLabel() }} · 已过滤零用量空节点</p>
            </div>
            <button type="button" class="tt-modal-close-btn" aria-label="关闭" @click="auditModalOpen = false">×</button>
          </header>

          <div class="tt-modal-body">
            <div class="flex justify-between items-center">
              <span class="tt-filter-count">共 {{ trendDetailList.length }} 个时序节点</span>
              <button type="button" class="tt-btn-secondary" @click="exportDataAsCsv">
                <span v-html="icons.download" />
                <span>导出 CSV 表格</span>
              </button>
            </div>

            <div class="tt-table-wrap">
              <AppTable
                :rows="trendDetailList"
                :columns="dailyColumns"
                :row-key="(item: any) => item.label"
                :page-size="15"
                empty-text="该时间范围内没有时序记录"
              >
                <template #cell-label="{ row }">
                  <code>{{ row.label }}</code>
                </template>
                <template #cell-total="{ row }"><strong>{{ formatCompact(row.total) }}</strong></template>
                <template #cell-input="{ row }">{{ formatCompact(row.input) }}</template>
                <template #cell-output="{ row }">{{ formatCompact(row.output) }}</template>
                <template #cell-cache="{ row }">{{ formatCompact(row.cache) }}</template>
                <template #cell-cacheHitRate="{ row }">{{ formatRate(row.cacheHitRate) }}</template>
                <template #cell-reasoning="{ row }">{{ formatCompact(row.reasoning) }}</template>
                <template #cell-sessions="{ row }">{{ formatTokens(row.sessions) }}</template>
                <template #cell-requests="{ row }">{{ row.requestsEstimated ? "≈" : "" }}{{ formatTokens(row.requests) }}</template>
              </AppTable>
            </div>
          </div>
        </section>
      </div>
    </Transition>

    <!-- 5. 请求健康矩阵明细弹窗 (Health Modal) -->
    <Transition name="tt-modal-fade">
      <div v-if="healthModalOpen" class="tt-modal-backdrop" @click.self="healthModalOpen = false">
        <section class="tt-modal-card is-wide" role="dialog" aria-modal="true">
          <header class="tt-modal-header">
            <div>
              <h2>请求健康矩阵明细</h2>
              <p>每个时段的成功率、对话数与请求健康状态</p>
            </div>
            <button type="button" class="tt-modal-close-btn" aria-label="关闭" @click="healthModalOpen = false">×</button>
          </header>

          <div class="tt-modal-body">
            <span class="tt-filter-count">共 {{ healthTableRows.length }} 个时段</span>

            <div class="tt-table-wrap">
              <AppTable
                :rows="healthTableRows"
                :columns="healthColumns"
                :row-key="(item: any) => item.key || item.label"
                :page-size="15"
                empty-text="当前时间区间内暂无请求健康记录"
              >
                <template #cell-label="{ row }">
                  <div class="tt-cell-with-dot">
                    <span class="tt-bar-dot" :style="{ background: healthLevelColor(row.level) }" />
                    <code v-if="row.label">{{ row.label }}</code>
                    <span v-else class="muted">空档</span>
                  </div>
                </template>
                <template #cell-dialogues="{ row }">{{ formatTokens(row.dialogues) }}</template>
                <template #cell-requests="{ row }">{{ row.requestsEstimated ? "≈" : "" }}{{ formatTokens(row.requests) }}</template>
                <template #cell-success="{ row }">{{ formatTokens(row.success) }}</template>
                <template #cell-failed="{ row }">
                  <span :class="{ 'text-danger': row.failed > 0 }">{{ formatTokens(row.failed) }}</span>
                </template>
                <template #cell-successRate="{ row }">
                  <span :class="{ 'text-success': row.successRate !== '—' && Number(row.successRate) >= 99, 'text-warning': row.successRate !== '—' && Number(row.successRate) < 95 && Number(row.successRate) >= 70, 'text-danger': row.successRate !== '—' && Number(row.successRate) < 70 }">
                    {{ row.successRate }}
                  </span>
                </template>
                <template #cell-level="{ row }">
                  <span class="tt-health-cell" :class="'lv' + row.level" :title="'等级 ' + row.level" />
                </template>
              </AppTable>
            </div>
          </div>
        </section>
      </div>
    </Transition>

    <!-- 统计重建控制台弹窗 (Reconstruction Console Modal) -->
    <Transition name="tt-modal-fade">
      <div v-if="refreshDialogOpen" class="tt-modal-backdrop" @click.self="closeRefreshDialog">
        <section class="tt-modal-card" role="dialog" aria-modal="true">
          <header class="tt-modal-header">
            <div>
              <h2>{{ refreshStatusTitle }}</h2>
              <p>{{ refreshStatusDescription }}</p>
            </div>
            <button type="button" class="tt-modal-close-btn" aria-label="关闭" @click="closeRefreshDialog">×</button>
          </header>

          <div class="tt-modal-body">
            <template v-if="refreshPhase === 'confirm'">
              <div class="tt-refresh-workflow">
                <div class="tt-wf-step">
                  <div class="tt-wf-num">1</div>
                  <div class="tt-wf-info">
                    <strong>清除本地快照与解析缓存</strong>
                    <small>删除旧解析索引与临时计算缓存，确保数据纯净</small>
                  </div>
                </div>
                <div class="tt-wf-step">
                  <div class="tt-wf-num">2</div>
                  <div class="tt-wf-info">
                    <strong>多端 AI 工具日志重新扫描</strong>
                    <small>完整读取 Codex, Claude, Cursor, Antigravity, OpenCode 等本地记录</small>
                  </div>
                </div>
                <div class="tt-wf-step">
                  <div class="tt-wf-num">3</div>
                  <div class="tt-wf-info">
                    <strong>重构 SQLite 数据库与前端快照</strong>
                    <small>建立小时/日/月多维聚合表，毫秒级即时呈现大盘</small>
                  </div>
                </div>
              </div>
              <div class="tt-refresh-tips">
                <span v-html="icons.info" />
                <p>重建过程仅读取本机日志，<strong>不会修改或删除任何外部 AI 工具的原始会话数据</strong>。</p>
              </div>
            </template>

            <template v-else>
              <div class="tt-refresh-running-bar" :class="`is-${refreshPhase}`">
                <span class="tt-state-icon" :class="{ 'is-spinning': refreshPhase === 'running' }">
                  {{ refreshPhase === "running" ? "↻" : (refreshPhase === "success" ? "✓" : "!") }}
                </span>
                <div>
                  <strong>{{ refreshStatusTitle }}</strong>
                  <p>{{ refreshStatusDescription }}</p>
                </div>
              </div>
              <div class="tt-log-terminal">
                <div class="tt-log-header">
                  <span>实时执行日志</span>
                  <span>{{ refreshLogs.length }} 条记录</span>
                </div>
                <ol ref="refreshLogListRef" class="tt-log-list">
                  <li v-for="entry in refreshLogs" :key="entry.id" :class="`is-${entry.status}`">
                    <time>{{ entry.time }}</time>
                    <span class="tt-log-stage">{{ refreshStageLabels[entry.stage] || entry.stage }}</span>
                    <p>{{ entry.message }}</p>
                    <i>{{ entry.status === "running" ? "…" : (entry.status === "success" ? "✓" : "!") }}</i>
                  </li>
                </ol>
              </div>
            </template>
          </div>

          <footer class="tt-modal-footer">
            <template v-if="refreshPhase === 'confirm'">
              <button type="button" class="tt-btn-cancel" @click="closeRefreshDialog">取消</button>
              <button type="button" class="tt-btn-primary" @click="startRefresh">
                <span v-html="icons.restore" />
                <span>开始完整重建</span>
              </button>
            </template>
            <template v-else>
              <span class="tt-footer-hint">
                {{ refreshPhase === "running" ? "后台运行中，可随时关闭此窗口。" : "重建已写入 SQLite 数据库。" }}
              </span>
              <button type="button" class="tt-btn-cancel" @click="closeRefreshDialog">
                {{ refreshPhase === "running" ? "后台运行" : "完成并关闭" }}
              </button>
            </template>
          </footer>
        </section>
      </div>
    </Transition>

    <!-- 本地 AI Agent 路径诊断弹窗 (Local Agent Inspector Modal) -->
    <Transition name="tt-modal-fade">
      <div v-if="agentDialogOpen" class="tt-modal-backdrop" @click.self="closeAgentDialog">
        <section class="tt-modal-card is-wide" role="dialog" aria-modal="true">
          <header class="tt-modal-header">
            <div>
              <h2>本机 AI Agent 诊断终端</h2>
              <p>只读探测当前 macOS 系统中各 AI 编程工具的配置、数据库与日志目录</p>
            </div>
            <button type="button" class="tt-modal-close-btn" aria-label="关闭" @click="closeAgentDialog">×</button>
          </header>

          <div class="tt-modal-body">
            <div v-if="store.localAgentPathsLoading.value && !store.localAgentPaths.value" class="tt-loading-card">
              <div class="tt-loading-spinner" />
              <p>正在扫描本机 AI Agent 路径…</p>
            </div>
            <template v-else-if="store.localAgentPaths.value">
              <div class="tt-agent-overview-bar">
                <span class="tt-agent-meta-chip">系统根路径: <code>{{ localAgentsHome }}</code></span>
                <span class="tt-agent-meta-chip">共 <strong>{{ localAgents.length }}</strong> 款 Agent</span>
                <span class="tt-agent-meta-chip is-success">已检测 <strong>{{ detectedAgentsCount }}</strong> 款活跃</span>
                <span v-if="localAgentsCollectedAt" class="tt-agent-meta-chip">采集时间: {{ localAgentsCollectedAt }}</span>
              </div>

              <div v-if="localAgentEnvOverrides.length" class="tt-agent-env-row">
                <span
                  v-for="override in localAgentEnvOverrides"
                  :key="override.key"
                  class="tt-agent-env-chip"
                  :title="override.value"
                >{{ override.key }} → {{ override.value }}</span>
              </div>

              <div class="tt-agent-cards-grid">
                <div
                  v-for="agent in localAgents"
                  :key="agent.source"
                  class="tt-agent-diag-card"
                  :class="{ 'is-detected': agent.detected }"
                >
                  <header class="tt-agent-diag-header">
                    <span class="tt-agent-dot" :class="{ on: agent.detected }" />
                    <strong>{{ agent.name }}</strong>
                    <span
                      v-if="agent.collectedEvents > 0 || agent.collectedSessions > 0"
                      class="tt-agent-stat-badge"
                    >
                      {{ formatAgentCount(agent.collectedSessions) }} 会话 · {{ formatAgentCount(agent.collectedEvents) }} 请求
                    </span>
                    <span class="tt-agent-status-tag" :class="{ on: agent.detected }">
                      {{ agent.detected ? "已检测" : "未安装/未激活" }}
                    </span>
                  </header>

                  <div class="tt-agent-root-row">
                    <span class="label">根目录:</span>
                    <code :title="agent.root">{{ displayAgentPath(agent.root) }}</code>
                  </div>

                  <ul class="tt-agent-path-list">
                    <li
                      v-for="(entry, eIdx) in agent.paths"
                      :key="eIdx"
                      :title="`点击复制路径: ${entry.path}`"
                      @click="copyAgentPath(entry.path)"
                    >
                      <span class="tt-path-kind-badge" :data-kind="entry.kind">{{ agentKindLabels[entry.kind] || entry.kind }}</span>
                      <span class="tt-path-text">
                        <span class="tt-path-label">{{ entry.label }}</span>
                        <code :class="{ missing: !entry.exists }"><span
                          v-for="(segment, segmentIndex) in agentPathSegments(entry.path)"
                          :key="segmentIndex"
                          class="tt-path-seg"
                        >{{ segment }}</span></code>
                      </span>
                      <span class="tt-path-status-icon" :class="{ exists: entry.exists }" />
                    </li>
                  </ul>
                </div>
              </div>
            </template>

            <span class="tt-footer-hint">点击任意路径行可直接复制完整路径至系统剪贴板。</span>
          </div>
        </section>
      </div>
    </Transition>

    <!-- 导出数据弹窗 (Export Modal) -->
    <Transition name="tt-modal-fade">
      <div v-if="exportDialogOpen" class="tt-modal-backdrop" @click.self="closeExportDialog">
        <section class="tt-modal-card is-wide" role="dialog" aria-modal="true">
          <header class="tt-modal-header">
            <div>
              <h2>导出 Token 数据分析报表</h2>
              <p>导出当前所选时间范围 ({{ rangeLabel }}) 内的多维分析指标</p>
            </div>
            <button type="button" class="tt-modal-close-btn" aria-label="关闭" @click="closeExportDialog">×</button>
          </header>

          <div class="tt-modal-body">
            <div class="tt-export-options-grid">
              <div class="tt-export-option-card" @click="exportDataAsJson">
                <span class="tt-export-icon" v-html="icons.database" />
                <strong>导出完整 JSON 结构化报表</strong>
                <p>包含大盘概览、工具分布、模型排行榜、项目用量与完整时序明细，适合二次分析与归档。</p>
                <button type="button" class="tt-btn-primary">下载 JSON 文件</button>
              </div>

              <div class="tt-export-option-card" @click="exportDataAsCsv">
                <span class="tt-export-icon" v-html="icons.download" />
                <strong>导出时序明细 CSV 表格</strong>
                <p>导出逐日/逐小时 Token 消耗明细（含输入、输出、缓存、命中率与请求数），适合 Excel / Numbers 打开。</p>
                <button type="button" class="tt-btn-primary">下载 CSV 表格</button>
              </div>
            </div>
          </div>
        </section>
      </div>
    </Transition>
  </main>
</template>

<style scoped>
.tt-dashboard {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  background: var(--page-bg);
  color: var(--text);
  overflow: hidden;
}

/* ============================================================
   1. 顶部宏观智控驾驶舱 (Macro Cockpit Bar)
   ============================================================ */
.tt-cockpit-bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 12px 20px;
  background: var(--surface);
  border-bottom: 1px solid var(--line);
  flex-shrink: 0;
}

.tt-cockpit-left {
  display: flex;
  align-items: center;
  gap: 16px;
  flex-shrink: 0;
}

.tt-brand-section {
  display: flex;
  flex-direction: column;
  gap: 1px;
}

.tt-eyebrow-row {
  display: flex;
  align-items: center;
  gap: 6px;
}

.tt-live-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: #10b981;
  box-shadow: 0 0 8px #10b981;
  animation: ttPulse 2s infinite ease-in-out;
}

@keyframes ttPulse {
  0%, 100% { opacity: 1; transform: scale(1); }
  50% { opacity: 0.4; transform: scale(1.25); }
}

.tt-eyebrow-text {
  font-size: 10px;
  font-weight: 750;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--brand);
}

.tt-title-row h1 {
  font-size: 18px;
  font-weight: 750;
  color: var(--text);
  margin: 0;
  line-height: 1.2;
}

.tt-cockpit-subtitle {
  font-size: 11px;
  color: var(--muted);
  margin: 0;
}

.tt-cockpit-subtitle strong {
  color: var(--text);
  font-weight: 600;
}

.tt-cockpit-right {
  display: flex;
  align-items: center;
  gap: 8px;
}

/* 标题下方筛选工具条：日期选择 + 维度按钮 */
.tt-filter-toolbar {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 20px;
  background: var(--surface);
  border-bottom: 1px solid var(--line);
  flex-shrink: 0;
}

/* 让 QuickRangeDropdown 在工具条中不被压缩 */
.tt-filter-toolbar :deep(.tt-range-dd) {
  flex-shrink: 0;
}

/* 顶部快速视角气泡群组 */
.tt-cockpit-pills-group {
  display: inline-flex;
  align-items: center;
  gap: 2px;
  background: var(--page-bg);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  padding: 2px;
  flex-shrink: 0;
}

.tt-pill-btn {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  height: 28px;
  padding: 0 10px;
  border-radius: 6px;
  border: none;
  background: transparent;
  color: var(--muted);
  font-size: 12px;
  font-weight: 550;
  cursor: pointer;
  transition: all 0.12s ease;
  white-space: nowrap;
  flex-shrink: 0;
}

.tt-pill-btn:hover {
  background: var(--surface);
  color: var(--text);
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.04);
}

.tt-pill-btn :deep(svg) {
  width: 13px;
  height: 13px;
}

.tt-cockpit-divider {
  width: 1px;
  height: 20px;
  background: var(--line);
  margin: 0 2px;
  flex-shrink: 0;
}

/* 让 QuickRangeDropdown 在 cockpit-right 中不被压缩 */
.tt-cockpit-right :deep(.tt-range-dd) {
  flex-shrink: 0;
}

/* 按钮规范 */
.tt-btn-rebuild {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 32px;
  padding: 0 12px;
  border-radius: var(--r-md, 8px);
  border: 1px solid color-mix(in srgb, var(--brand, #388bfd) 35%, transparent);
  background: color-mix(in srgb, var(--brand, #388bfd) 10%, var(--surface));
  color: var(--brand-deep, var(--brand, #388bfd));
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.15s ease;
  white-space: nowrap;
  flex-shrink: 0;
}

.tt-btn-rebuild:hover:not(:disabled) {
  background: color-mix(in srgb, var(--brand, #388bfd) 18%, var(--surface));
  border-color: var(--brand);
  transform: translateY(-1px);
}

.tt-btn-rebuild:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.tt-btn-rebuild :deep(svg) {
  width: 13px;
  height: 13px;
}

.tt-btn-secondary {
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
  font-weight: 550;
  cursor: pointer;
  transition: all 0.15s ease;
  white-space: nowrap;
  flex-shrink: 0;
}

.tt-btn-secondary:hover {
  background: var(--surface-hover);
  border-color: var(--line-hover);
  transform: translateY(-1px);
}

.tt-btn-secondary :deep(svg) {
  width: 13px;
  height: 13px;
  color: var(--muted);
}

.tt-agent-count-chip {
  padding: 1px 5px;
  border-radius: var(--r-full);
  background: rgba(16, 185, 129, 0.12);
  color: #10b981;
  font-size: 9.5px;
  font-weight: 700;
}

.is-spinning {
  animation: ttSpin 1s infinite linear;
}

@keyframes ttSpin {
  100% { transform: rotate(360deg); }
}

/* ============================================================
   2. 首页主视口 (No-Scroll Viewport Layout)
   ============================================================ */
.tt-dashboard-body {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  padding: 12px 18px;
  gap: 10px;
}

/* ROW 1: 4 KPI Cards (Compact) */
.tt-kpi-deck {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 10px;
  flex-shrink: 0;
}

@media (max-width: 1200px) {
  .tt-kpi-deck {
    grid-template-columns: repeat(2, 1fr);
  }
}

.tt-kpi-card {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 10px);
  padding: 10px 14px;
  display: flex;
  flex-direction: column;
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.02);
  transition: all 0.15s ease;
}

.tt-kpi-card:hover {
  border-color: var(--line-hover);
}

.tt-kpi-card-inner {
  display: flex;
  flex-direction: column;
  height: 100%;
}

.tt-kpi-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 6px;
  margin-bottom: 4px;
}

.tt-kpi-tag {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 10px;
  font-weight: 750;
  letter-spacing: 0.05em;
  text-transform: uppercase;
}

.tt-kpi-tag :deep(svg) {
  width: 12px;
  height: 12px;
}

.tt-kpi-tag.is-emerald { color: #10b981; }
.tt-kpi-tag.is-orange { color: #f97316; }
.tt-kpi-tag.is-blue { color: #3b82f6; }
.tt-kpi-tag.is-purple { color: #a855f7; }

.tt-kpi-badge-hit {
  padding: 1px 6px;
  border-radius: var(--r-full);
  font-size: 10px;
  font-weight: 700;
}
.tt-kpi-badge-hit.is-excellent { background: rgba(16, 185, 129, 0.12); color: #10b981; }
.tt-kpi-badge-hit.is-good { background: rgba(59, 130, 246, 0.12); color: #3b82f6; }
.tt-kpi-badge-hit.is-fair { background: rgba(245, 158, 11, 0.12); color: #f59e0b; }
.tt-kpi-badge-hit.is-none { background: rgba(148, 163, 184, 0.12); color: #94a3b8; }

.tt-kpi-streak-pill,
.tt-kpi-savings-pill {
  padding: 1px 6px;
  border-radius: var(--r-full);
  font-size: 10px;
  font-weight: 700;
  background: rgba(249, 115, 22, 0.12);
  color: #f97316;
}

.tt-kpi-savings-pill {
  background: rgba(168, 85, 247, 0.12);
  color: #a855f7;
}

.tt-kpi-badge-rate {
  padding: 1px 6px;
  border-radius: var(--r-full);
  font-size: 10px;
  font-weight: 700;
  background: rgba(59, 130, 246, 0.12);
  color: #3b82f6;
}

.tt-kpi-main-val {
  display: flex;
  align-items: baseline;
  gap: 5px;
  margin-bottom: 4px;
}

.tt-kpi-main-val strong {
  font-size: 22px;
  font-weight: 800;
  line-height: 1;
  font-variant-numeric: tabular-nums;
  letter-spacing: -0.02em;
}

.tt-kpi-main-val strong.tt-val-unpriced {
  font-size: 14px;
  font-weight: 600;
  color: var(--muted);
}

.tt-kpi-unit {
  font-size: 11px;
  color: var(--muted);
  font-weight: 600;
}

/* 进度条 */
.tt-kpi-progress-bar {
  display: flex;
  height: 4px;
  border-radius: var(--r-full);
  background: var(--page-bg);
  overflow: hidden;
  margin-bottom: 6px;
}

.tt-prog-seg {
  height: 100%;
  transition: width 0.3s ease;
}
.tt-prog-seg.is-in { background: #3b82f6; }
.tt-prog-seg.is-out { background: #10b981; }
.tt-prog-seg.is-cache { background: #8b5cf6; }

.tt-kpi-sub-pills {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  font-size: 10.5px;
}

.tt-sub-pill {
  display: inline-flex;
  align-items: center;
  gap: 3px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
}

.tt-sub-pill i {
  width: 5px;
  height: 5px;
  border-radius: 50%;
}
.tt-sub-pill.in i { background: #3b82f6; }
.tt-sub-pill.out i { background: #10b981; }
.tt-sub-pill.cache i { background: #8b5cf6; }
.tt-sub-pill.reasoning i { background: #f59e0b; }

.tt-kpi-meta-text {
  font-size: 11px;
  color: var(--muted);
  margin-bottom: 4px;
}

.tt-kpi-meta-text strong {
  color: var(--text);
}

.tt-active-rate-badge {
  display: inline-block;
  margin-left: 4px;
  padding: 1px 4px;
  border-radius: 3px;
  background: var(--surface-hover);
  font-size: 9.5px;
  font-weight: 600;
}

.tt-kpi-footer-note {
  margin-top: auto;
  font-size: 10.5px;
  color: var(--muted);
}

.tt-kpi-multiplier-pill {
  margin-top: auto;
  padding: 2px 6px;
  border-radius: var(--r-md, 4px);
  background: var(--surface-hover);
  font-size: 10.5px;
  color: var(--muted);
}

.tt-kpi-multiplier-pill strong {
  color: #3b82f6;
  font-weight: 700;
}

/* 4 大核心全景图表与分布四等分大盘 (Equal 4-Quadrant 2x2 Grid) */
.tt-quad-grid {
  flex: 1;
  min-height: 0;
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  grid-template-rows: repeat(2, minmax(0, 1fr));
  gap: 10px;
  overflow: hidden;
}

@media (max-width: 1100px) {
  .tt-quad-grid {
    grid-template-columns: 1fr;
    grid-template-rows: auto;
    overflow-y: auto;
  }
}

.tt-card {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 10px);
  padding: 12px 14px;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.tt-card.tt-health-card {
  padding: 8px 6px;
}

.tt-card-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  margin-bottom: 8px;
  flex-shrink: 0;
}

.tt-card-title-wrap h2,
.tt-card-title-wrap h3 {
  font-size: 13.5px;
  font-weight: 700;
  margin: 0;
}

.tt-card-title-wrap p {
  font-size: 10.5px;
  color: var(--muted);
  margin: 1px 0 0;
}

.tt-metric-switches {
  display: flex;
  gap: 2px;
  background: var(--page-bg);
  padding: 2px;
  border-radius: var(--r-md, 6px);
  border: 1px solid var(--line);
}

.tt-metric-btn {
  height: 22px;
  padding: 0 8px;
  border-radius: 4px;
  border: none;
  background: transparent;
  color: var(--muted);
  font-size: 10.5px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.12s ease;
}

.tt-metric-btn.active {
  background: var(--surface);
  color: var(--text);
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.08);
}

.tt-chart-body,
.tt-health-body {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.tt-health-kpi-bar {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 6px;
  margin-bottom: 8px;
  flex-shrink: 0;
}

.tt-hk-card {
  padding: 5px 8px;
  border-radius: var(--r-sm, 6px);
  background: var(--page-bg);
  border: 1px solid var(--line-soft);
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.tt-hk-lbl {
  font-size: 10px;
  color: var(--muted);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.tt-hk-val-box {
  display: flex;
  align-items: baseline;
  gap: 4px;
}

.tt-hk-num {
  font-size: 13.5px;
  font-weight: 750;
  font-variant-numeric: tabular-nums;
  line-height: 1;
}

.tt-hk-unit {
  font-size: 9.5px;
  color: var(--muted);
}

.tt-hk-badge {
  font-size: 9px;
  font-weight: 700;
  padding: 1px 4px;
  border-radius: 3px;
  line-height: 1;
}
.tt-hk-badge.is-excellent { background: rgba(16, 185, 129, 0.15); color: #10b981; }
.tt-hk-badge.is-good { background: rgba(59, 130, 246, 0.15); color: #3b82f6; }
.tt-hk-badge.is-fair { background: rgba(245, 158, 11, 0.15); color: #f59e0b; }
.tt-hk-badge.is-bad { background: rgba(239, 68, 68, 0.15); color: #ef4444; }

.tt-health-wrapper {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  justify-content: space-between;
  gap: 6px;
}

.tt-health-grid {
  display: grid;
  grid-auto-flow: column;
  grid-auto-columns: minmax(0, 1fr);
  column-gap: 3px;
  row-gap: 4px;
  width: 100%;
  flex: 1;
  min-height: 0;
  align-content: center;
}

.tt-health-cell {
  width: 100%;
  height: 100%;
  border-radius: 2.5px;
  transition: transform 0.1s ease, box-shadow 0.1s ease;
}
.tt-health-cell:hover {
  transform: scale(1.35);
  z-index: 10;
  box-shadow: 0 2px 6px rgba(0, 0, 0, 0.2);
}

.tt-health-cell.lv0 { background: rgba(148, 163, 184, 0.15); }
.tt-health-cell.lv1 { background: #ef4444; }
.tt-health-cell.lv2 { background: #f97316; }
.tt-health-cell.lv3 { background: #eab308; }
.tt-health-cell.lv4 { background: #84cc16; }
.tt-health-cell.lv5 { background: #10b981; }

.tt-health-legend {
  display: flex;
  align-items: center;
  gap: 5px;
  font-size: 10px;
  color: var(--muted);
  flex-shrink: 0;
  padding-top: 4px;
}
.tt-health-legend .tt-health-cell {
  width: 8px;
  height: 8px;
}
.tt-legend-meta {
  margin-left: auto;
}

.tt-preview-card {
  padding: 12px 14px;
}

.tt-preview-body {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  justify-content: flex-start;
  gap: 4px;
}

.tt-bar-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 3px 0;
}

.tt-bar-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  flex-shrink: 0;
}

.tt-bar-label {
  font-size: 11.5px;
  font-weight: 600;
  width: 130px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.tt-bar-track {
  flex: 1;
  height: 5px;
  background: var(--page-bg);
  border-radius: var(--r-full);
  overflow: hidden;
}

.tt-bar-fill {
  height: 100%;
  border-radius: var(--r-full);
  transition: width 0.3s ease;
}

.tt-bar-pct {
  font-size: 10.5px;
  color: var(--muted);
  width: 40px;
  text-align: right;
  font-variant-numeric: tabular-nums;
}

.tt-bar-val {
  font-size: 11.5px;
  width: 60px;
  text-align: right;
  font-variant-numeric: tabular-nums;
}

.tt-text-btn {
  background: transparent;
  border: none;
  color: var(--brand);
  font-size: 11.5px;
  font-weight: 600;
  cursor: pointer;
  padding: 0;
}
.tt-text-btn:hover {
  text-decoration: underline;
}

/* ============================================================
   3. 弹窗对话框体系 (Modal Dialogs)
   ============================================================ */
.tt-modal-backdrop {
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

.tt-modal-card {
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

.tt-modal-card.is-wide {
  max-width: 960px;
}

.tt-modal-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 14px 18px;
  border-bottom: 1px solid var(--line);
  flex-shrink: 0;
}

.tt-modal-header h2 {
  font-size: 15px;
  font-weight: 750;
  margin: 0;
}

.tt-modal-header p {
  font-size: 11px;
  color: var(--muted);
  margin: 2px 0 0;
}

.tt-modal-close-btn {
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

.tt-modal-close-btn:hover {
  background: var(--surface-hover);
  color: var(--text);
}

.tt-modal-body {
  padding: 16px 18px;
  overflow-y: auto;
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.tt-modal-footer {
  padding: 10px 18px;
  border-top: 1px solid var(--line);
  display: flex;
  justify-content: flex-end;
  align-items: center;
  gap: 8px;
  background: var(--page-bg);
  flex-shrink: 0;
}

.tt-table-wrap {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  overflow: hidden;
  display: flex;
  flex-direction: column;
  flex: 1 1 auto;
  min-height: 0;
}

/* 过滤搜索栏 */
.tt-filter-bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  flex-shrink: 0;
}

.tt-search-input {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  height: 34px;
  padding: 0 12px;
  border-radius: var(--r-md, 8px);
  border: 1px solid var(--line);
  background: var(--page-bg);
  width: 320px;
}

.tt-search-input input {
  border: none;
  background: transparent;
  color: var(--text);
  font-size: 12.5px;
  width: 100%;
  outline: none;
}

.tt-filter-count {
  font-size: 11.5px;
  color: var(--muted);
}

.tt-cell-with-dot {
  display: flex;
  align-items: center;
  gap: 6px;
}

.tt-muted-code {
  font-size: 10px;
  color: var(--muted);
}

.tt-project-cell {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.tt-project-cell :deep(svg) {
  width: 14px;
  height: 14px;
  color: var(--muted);
  flex-shrink: 0;
}

.tt-btn-cancel {
  height: 32px;
  padding: 0 14px;
  border-radius: var(--r-md, 6px);
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--text);
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.15s ease;
}

.tt-btn-cancel:hover {
  background: var(--surface-hover);
}

.tt-btn-primary {
  height: 32px;
  padding: 0 16px;
  border-radius: var(--r-md, 6px);
  border: none;
  background: var(--brand);
  color: #fff;
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  display: inline-flex;
  align-items: center;
  gap: 6px;
  transition: all 0.15s ease;
}

.tt-btn-primary :deep(svg) {
  width: 14px;
  height: 14px;
}

.tt-btn-primary:hover {
  background: var(--brand-deep);
  transform: translateY(-1px);
}

/* 步骤指示器 */
.tt-refresh-workflow {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.tt-wf-step {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 12px;
  border-radius: var(--r-md, 8px);
  background: var(--page-bg);
  border: 1px solid var(--line);
}

.tt-wf-num {
  width: 24px;
  height: 24px;
  border-radius: 50%;
  background: var(--brand);
  color: #fff;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 12px;
  font-weight: 700;
  flex-shrink: 0;
}

.tt-wf-info {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.tt-wf-info strong {
  font-size: 12.5px;
}

.tt-wf-info small {
  font-size: 11px;
  color: var(--muted);
}

.tt-refresh-tips {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 12px;
  border-radius: 6px;
  background: color-mix(in srgb, var(--brand) 10%, transparent);
  color: var(--text);
  font-size: 11.5px;
}

.tt-refresh-tips :deep(svg) {
  width: 14px;
  height: 14px;
  color: var(--brand);
  flex-shrink: 0;
}

.tt-refresh-tips p {
  margin: 0;
}

.tt-refresh-running-bar {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px;
  border-radius: var(--r-md, 8px);
  background: var(--page-bg);
  border: 1px solid var(--line);
}

.tt-state-icon {
  width: 28px;
  height: 28px;
  border-radius: 50%;
  background: var(--brand-soft);
  color: var(--brand);
  display: flex;
  align-items: center;
  justify-content: center;
  font-weight: 800;
  font-size: 13px;
}

.tt-log-terminal {
  background: #090d16;
  border-radius: var(--r-md, 8px);
  border: 1px solid rgba(255, 255, 255, 0.1);
  padding: 10px 12px;
  color: #f8fafc;
  font-family: monospace;
}

.tt-log-header {
  display: flex;
  justify-content: space-between;
  font-size: 10.5px;
  color: #64748b;
  padding-bottom: 6px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
  margin-bottom: 6px;
}

.tt-log-list {
  list-style: none;
  padding: 0;
  margin: 0;
  max-height: 160px;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 10.5px;
}

.tt-log-list li {
  display: flex;
  align-items: center;
  gap: 6px;
}

.tt-log-list time { color: #64748b; }
.tt-log-stage {
  padding: 1px 4px;
  border-radius: 3px;
  background: rgba(255, 255, 255, 0.1);
  color: #38bdf8;
}
.tt-log-list p { margin: 0; flex: 1; }

.tt-footer-hint {
  font-size: 11px;
  color: var(--muted);
  margin-right: auto;
}

/* Agent 诊断弹窗 */
.tt-agent-overview-bar {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  flex-shrink: 0;
}

.tt-agent-meta-chip {
  padding: 3px 8px;
  border-radius: var(--r-md, 6px);
  background: var(--page-bg);
  border: 1px solid var(--line);
  font-size: 11px;
  color: var(--muted);
}

.tt-agent-meta-chip.is-success {
  background: rgba(16, 185, 129, 0.12);
  color: #10b981;
  border-color: rgba(16, 185, 129, 0.3);
}

.tt-agent-env-row {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  flex-shrink: 0;
}

.tt-agent-env-chip {
  padding: 2px 6px;
  border-radius: 4px;
  background: var(--surface-hover);
  font-size: 10.5px;
  font-family: monospace;
}

.tt-agent-cards-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(360px, 1fr));
  gap: 10px;
}

.tt-agent-diag-card {
  background: var(--page-bg);
  border: 1px solid var(--line);
  border-radius: var(--r-lg);
  padding: 14px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.tt-agent-diag-card.is-detected {
  border-color: rgba(16, 185, 129, 0.4);
  background: var(--surface);
}

.tt-agent-diag-header {
  display: flex;
  align-items: center;
  gap: 8px;
}

.tt-agent-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: #94a3b8;
}
.tt-agent-dot.on {
  background: #10b981;
  box-shadow: 0 0 6px #10b981;
}

.tt-agent-stat-badge {
  padding: 1px 6px;
  border-radius: var(--r-full);
  background: var(--surface-hover);
  font-size: 10px;
  color: var(--muted);
}

.tt-agent-status-tag {
  margin-left: auto;
  font-size: 11px;
  color: #94a3b8;
}
.tt-agent-status-tag.on {
  color: #10b981;
  font-weight: 700;
}

.tt-agent-root-row {
  font-size: 11px;
  color: var(--muted);
  display: flex;
  align-items: center;
  gap: 6px;
}

.tt-agent-path-list {
  list-style: none;
  padding: 0;
  margin: 0;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.tt-agent-path-list li {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 8px;
  border-radius: 4px;
  background: var(--surface);
  cursor: pointer;
  transition: background 0.15s ease;
}

.tt-agent-path-list li:hover {
  background: var(--surface-hover);
}

.tt-path-kind-badge {
  padding: 1px 4px;
  border-radius: 3px;
  background: var(--brand-soft);
  color: var(--brand-deep);
  font-size: 9px;
  font-weight: 700;
}

.tt-path-text {
  flex: 1;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.tt-path-label {
  font-size: 10px;
  color: var(--muted);
}

.tt-path-text code {
  font-size: 11px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.tt-path-text code.missing {
  color: var(--muted);
  text-decoration: line-through;
}

.tt-path-status-icon {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: #94a3b8;
}
.tt-path-status-icon.exists {
  background: #10b981;
}

/* 导出选项 */
.tt-export-options-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 16px;
}

.tt-export-option-card {
  background: var(--page-bg);
  border: 1px solid var(--line);
  border-radius: var(--r-xl);
  padding: 20px;
  display: flex;
  flex-direction: column;
  align-items: center;
  text-align: center;
  gap: 12px;
  cursor: pointer;
  transition: all 0.15s ease;
}

.tt-export-option-card:hover {
  border-color: var(--brand);
  transform: translateY(-2px);
  box-shadow: 0 8px 20px rgba(0, 0, 0, 0.06);
}

.tt-export-icon {
  width: 44px;
  height: 44px;
  border-radius: 50%;
  background: var(--brand-soft);
  color: var(--brand);
  display: flex;
  align-items: center;
  justify-content: center;
}

.tt-export-icon :deep(svg) {
  width: 22px;
  height: 22px;
}

.tt-export-option-card strong {
  font-size: 14px;
}

.tt-export-option-card p {
  font-size: 12px;
  color: var(--muted);
  margin: 0;
  flex: 1;
}

/* 状态提示 */
.tt-error-banner {
  display: flex;
  align-items: flex-start;
  gap: 12px;
  padding: 14px 18px;
  border-radius: var(--r-xl);
  background: rgba(239, 68, 68, 0.1);
  border: 1px solid rgba(239, 68, 68, 0.3);
  color: #ef4444;
}

.tt-error-icon :deep(svg) {
  width: 20px;
  height: 20px;
}

.tt-error-content strong {
  display: block;
  font-size: 14px;
}

.tt-error-content p {
  margin: 4px 0 2px;
  font-size: 12px;
}

.tt-error-content small {
  color: var(--muted);
  font-size: 11px;
}

.tt-loading-card,
.tt-empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 40px;
  color: var(--muted);
  font-size: 13px;
  gap: 12px;
}

.tt-loading-spinner {
  width: 28px;
  height: 28px;
  border: 3px solid var(--line);
  border-top-color: var(--brand);
  border-radius: 50%;
  animation: ttSpin 0.8s infinite linear;
}

.tt-modal-fade-enter-active,
.tt-modal-fade-leave-active {
  transition: opacity 0.2s ease;
}

.tt-modal-fade-enter-from,
.tt-modal-fade-leave-to {
  opacity: 0;
}
</style>
