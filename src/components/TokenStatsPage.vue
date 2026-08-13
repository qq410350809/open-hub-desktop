<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import type { EChartsOption } from "../echarts";
import EChart from "./EChart.vue";
import QuickRangeDropdown from "./QuickRangeDropdown.vue";
import AppTable, { type AppTableColumn } from "./AppTable.vue";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
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

const store = useStore();

// —— 趋势维度：根据顶部所选时间区间自动决定 X 轴粒度 ——
// < 7 天 → 逐小时；7 天 ~ 3 个月 → 逐日；≥ 3 个月 → 逐月
const trendGranularity = computed<TrendGranularity>(() => {
  const from = store.tokenStatsFrom.value;
  const to = store.tokenStatsTo.value;
  if (from && to) {
    const days = Math.round((parseLocal(to).getTime() - parseLocal(from).getTime()) / 86_400_000) + 1;
    if (days < 7) return "hour";
    if (days <= 92) return "day";
    return "month";
  }
  // 未设置范围：依据数据跨度自适应
  return "day";
});

// —— 弹窗状态：详情列表统一切到弹窗展示 ——
type ModalKind = "daily" | "projects" | "sources" | "models";
const modal = ref<ModalKind | null>(null);
const modalOpen = ref(false);
function openModal(kind: ModalKind) {
  modal.value = kind;
  detailPage.value = 1;
  modalOpen.value = true;
}
function closeModal() {
  modalOpen.value = false;
  modal.value = null;
}

// —— 明细弹窗：标签页 + 分页 ——
const detailTab = ref<"daily" | "projects">("daily");
const PAGE_SIZE = 12;
const detailPage = ref(1);
function openDetail(tab: "daily" | "projects") {
  detailTab.value = tab;
  detailPage.value = 1;
  modal.value = tab; // 复用 modal 弹窗（daily/projects）
  modalOpen.value = true;
}
function switchDetailTab(tab: "daily" | "projects") {
  detailTab.value = tab;
  detailPage.value = 1;
}

const dailyColumns: AppTableColumn[] = [
  { key: "label", title: "时间", width: "minmax(120px,1fr)" },
  { key: "total", title: "总计", width: "90px", align: "right", sortable: true },
  { key: "input", title: "输入", width: "90px", align: "right", sortable: true },
  { key: "output", title: "输出", width: "90px", align: "right", sortable: true },
  { key: "cache", title: "缓存", width: "90px", align: "right", sortable: true },
  { key: "cacheHitRate", title: "缓存命中率", width: "110px", align: "right", sortable: true },
  { key: "reasoning", title: "推理", width: "90px", align: "right", sortable: true },
  { key: "sessions", title: "对话", width: "90px", align: "right", sortable: true },
];

const projectColumns: AppTableColumn[] = [
  { key: "project", title: "项目", width: "minmax(120px,1fr)" },
  { key: "totalTokens", title: "总计", width: "90px", align: "right", sortable: true },
  { key: "input", title: "输入", width: "90px", align: "right", sortable: true },
  { key: "output", title: "输出", width: "90px", align: "right", sortable: true },
  { key: "cache", title: "缓存", width: "90px", align: "right", sortable: true },
  { key: "cacheHitRate", title: "缓存命中率", width: "110px", align: "right", sortable: true },
  { key: "reasoning", title: "推理", width: "90px", align: "right", sortable: true },
  { key: "sessions", title: "对话", width: "90px", align: "right", sortable: true },
  { key: "costUsd", title: "成本", width: "100px", align: "right", sortable: true },
];

const sourceColumns: AppTableColumn[] = [
  { key: "source", title: "工具", width: "minmax(120px,1fr)" },
  { key: "totalTokens", title: "总计", width: "90px", align: "right", sortable: true },
  { key: "inputTokens", title: "输入", width: "90px", align: "right", sortable: true },
  { key: "outputTokens", title: "输出", width: "90px", align: "right", sortable: true },
  { key: "cacheTokens", title: "缓存", width: "90px", align: "right", sortable: true },
  { key: "cacheHitRate", title: "缓存命中率", width: "110px", align: "right", sortable: true },
  { key: "reasoningTokens", title: "推理", width: "90px", align: "right", sortable: true },
  { key: "conversations", title: "对话", width: "90px", align: "right", sortable: true },
  { key: "costUsd", title: "成本", width: "100px", align: "right", sortable: true },
  { key: "share", title: "占比", width: "70px", align: "right" },
];

const modelColumns: AppTableColumn[] = sourceColumns;

// sessions 仅用于项目用量表；顶部成本改用覆盖全部工具的小时桶逐模型定价。
const sessions = computed(() => store.tokenStats.value?.sessions ?? []);

// —— 供应商品牌色 ——
const PROVIDER_COLORS: Record<string, string> = {
  claude: "#d97757",
  codex: "#3b82f6",
  opencode: "#f59e0b",
  gemini: "#2196f3",
  kiro: "#a78bfa",
  copilot: "#6b7280",
  openclaw: "#facc15",
  goose: "#ef4444",
  antigravity: "#ff6900",
  zed: "#14b8a6",
  cursor: "#8b5cf6",
  catpawai: "#ec4899",
  "command-code": "#10b981",
};
function providerColor(source: string, index: number) {
  return PROVIDER_COLORS[source.toLowerCase()] || `hsl(${150 + index * 40}, 60%, 45%)`;
}

const sourceNameMap: Record<string, string> = {
  claude: "Claude",
  codex: "Codex",
  cursor: "Cursor",
  catpawai: "CatPawAI",
  gemini: "Gemini",
  opencode: "OpenCode",
  kiro: "Kiro",
  copilot: "Copilot",
  openclaw: "OpenClaw",
  goose: "Goose",
  antigravity: "Antigravity",
  zed: "Zed",
  "command-code": "Command Code",
};
function sourceLabel(source: string) {
  return sourceNameMap[source.toLowerCase()] || source || "未知来源";
}

function shareOf(value: number, total: number) {
  return total > 0 ? Math.min(100, (value / total) * 100) : 0;
}

// —— 小时用量桶（OpenHub 自有采集 + CatPawAI 本地 SQLite）——
const allBuckets = computed(() => store.tokenUsage.value?.buckets ?? []);
const filteredBuckets = computed(() => {
  const buckets = allBuckets.value;
  const from = store.tokenStatsFrom.value;
  const to = store.tokenStatsTo.value;
  return buckets.filter((bucket) => {
    // 去掉 unknown / 空 模型与来源
    if (!isKnownModel(bucket.model) || !isKnownSource(bucket.source)) return false;
    const day = localDateOf(bucket.timestamp);
    return (!from || day >= from) && (!to || day <= to);
  });
});

const dailyMap = computed(() => buildDailyMapFromBuckets(filteredBuckets.value));
const bucketTotal = computed(() => bucketTotals(filteredBuckets.value));

// —— KPI 计算 ——
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
// —— 所选日期区间统计 ——
const rangeLabel = computed(() => {
  const from = store.tokenStatsFrom.value;
  const to = store.tokenStatsTo.value;
  if (!from && !to) return "全部";
  return `${from || "…"} ~ ${to || "…"}`;
});
const rangeDays = computed(() => {
  const from = store.tokenStatsFrom.value;
  const to = store.tokenStatsTo.value;
  if (!from || !to) return activeDays.value;
  return Math.max(1, Math.round((parseLocal(to).getTime() - parseLocal(from).getTime()) / 86_400_000) + 1);
});
// 日均 Tokens（区间总量 / 区间活跃天数）
const dailyAverage = computed(() => {
  const days = Math.max(1, activeDays.value);
  return bucketTotal.value.total / days;
});

// 区间 Token 拆分（输入 / 输出 / 缓存）
const rangeSplits = computed(() => {
  let input = 0;
  let output = 0;
  let cache = 0;
  for (const stat of dailyMap.value.values()) {
    input += stat.input;
    output += stat.output;
    cache += stat.cache;
  }
  return { input, output, cache };
});

const cacheHitRate = computed(() => {
  let cacheRead = 0;
  let cacheWrite = 0;
  let fresh = 0;
  for (const bucket of filteredBuckets.value) {
    cacheRead += bucket.cachedInputTokens || 0;
    cacheWrite += bucket.cacheCreationInputTokens || 0;
    fresh += bucket.inputTokens || 0;
  }
  const total = cacheRead + cacheWrite + fresh;
  return total > 0 ? cacheRead / total : null;
});

// 成本只汇总数据源明确上报的金额，不对缺少价格的数据进行猜测。
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
  if (totalTokens <= 0) return "按模型分项估算";
  if (pricedTokens <= 0 || coverage == null) return "暂无匹配定价";
  if (coverage >= 0.9995) return "来源上报成本 · 全部覆盖";
  return `来源上报成本 · 覆盖 ${(coverage * 100).toFixed(1)}%`;
});

const totalTokensAll = computed(() => bucketTotal.value.total);
const bySource = computed(() => bucketSourceTotals(filteredBuckets.value));
const byModel = computed(() => mergeModelTotals(bucketModelTotals(filteredBuckets.value)));
const topSources = computed(() => bySource.value.slice(0, 5));
const topModels = computed(() => byModel.value.slice(0, 5));

// 项目用量按项目聚合（含各分项；buckets 无项目维度故用会话日志）
type ProjectUsageItem = {
  project: string;
  sessions: number;
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
    { project: string; sessions: number; totalTokens: number; input: number; output: number; cache: number; cacheRead: number; cacheWrite: number; cacheHitRate: number | null; reasoning: number; costUsd: number; estimatedTokens: number }
  >();
  const normalizeProject = (rawKey?: string) => {
    const value = rawKey?.trim() || "未知项目";
    // UUID 形态的项目名无法识别（Claude Code 缺失 cwd 上下文时产生），归并为“其他”。
    return /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value)
      ? "其他"
      : value;
  };
  const ensureGroup = (rawKey?: string) => {
    const key = normalizeProject(rawKey);
    const current = groups.get(key) || {
      project: key, sessions: 0, totalTokens: 0,
      input: 0, output: 0, cache: 0, cacheRead: 0, cacheWrite: 0, cacheHitRate: null,
      reasoning: 0, costUsd: 0, estimatedTokens: 0,
    };
    groups.set(key, current);
    return current;
  };

  // CatPawAI 桶自带 workspace 项目维度，直接按当前前端日期范围精确聚合。
  // 记录这些来源后，跳过同来源 sessions，避免项目维度重复统计。
  const projectBucketSources = new Set<string>();
  for (const bucket of filteredBuckets.value) {
    if (!bucket.projectKey?.trim()) continue;
    projectBucketSources.add(bucket.source.toLowerCase());
    const current = ensureGroup(bucket.projectKey);
    current.sessions += bucket.conversationCount || 0;
    current.totalTokens += bucket.totalTokens || 0;
    current.input += bucket.inputTokens || 0;
    current.output += bucket.outputTokens || 0;
    current.cache += (bucket.cachedInputTokens || 0) + (bucket.cacheCreationInputTokens || 0);
    current.cacheRead += bucket.cachedInputTokens || 0;
    current.cacheWrite += bucket.cacheCreationInputTokens || 0;
    current.reasoning += bucket.reasoningOutputTokens || 0;
    current.costUsd += bucket.costUsd || 0;
    current.estimatedTokens += bucket.estimatedTokens || 0;
  }

  // 对没有项目桶的兼容来源，使用会话数据补充项目维度。
  for (const session of sessions.value) {
    if (projectBucketSources.has((session.source || "").toLowerCase())) continue;
    const current = ensureGroup(session.projectKey);
    current.sessions += 1;
    current.totalTokens += session.totalTokens || 0;
    current.input += session.tokens?.inputTokens || 0;
    current.output += session.tokens?.outputTokens || 0;
    current.cache += (session.tokens?.cachedInputTokens || 0) + (session.tokens?.cacheCreationInputTokens || 0);
    current.cacheRead += session.tokens?.cachedInputTokens || 0;
    current.cacheWrite += session.tokens?.cacheCreationInputTokens || 0;
    current.reasoning += session.tokens?.reasoningOutputTokens || 0;
    current.costUsd += session.costUsd || 0;
    const usageKind = String(session.provenance?.tokenUsage || "");
    if (usageKind.includes("estimated")) current.estimatedTokens += session.totalTokens || 0;
  }
  return [...groups.values()]
    .map((item) => ({ ...item, cacheHitRate: cacheHitRateOf(item.cacheRead, item.cacheWrite, item.input) }))
    .sort((a, b) => b.totalTokens - a.totalTokens)
    .slice(0, 20);
});

const trendSeries = computed(() =>
  buildTrendFromBuckets(
    filteredBuckets.value,
    trendGranularity.value,
    store.tokenStatsFrom.value || undefined,
    store.tokenStatsTo.value || undefined,
  ),
);
// 明细列表与图表同粒度、同 key，保证一一对应（含空节点）
const trendDetail = computed(() =>
  buildTrendDetailFromBuckets(
    filteredBuckets.value,
    trendGranularity.value,
    store.tokenStatsFrom.value || undefined,
    store.tokenStatsTo.value || undefined,
  ),
);
function trendUnitLabel() {
  switch (trendGranularity.value) {
    case "hour": return "逐小时";
    case "day": return "逐日";
    case "month": return "逐月";
    default: return "逐日";
  }
}

// 明细列表时间标签：逐小时/逐日去掉年份，逐月保留
function formatDetailTime(label: string): string {
  switch (trendGranularity.value) {
    case "hour": {
      // "2026-08-08 00:00" → "08-08 00:00"
      const m = label.match(/^(\d{4})-(\d{2}-\d{2}) (\d{2}:\d{2})$/);
      return m ? `${m[2]} ${m[3]}` : label;
    }
    case "day": {
      // "2026-08-08" → "08-08"
      const m = label.match(/^\d{4}-(\d{2}-\d{2})$/);
      return m ? m[1] : label;
    }
    default:
      return label;
  }
}

// 请求健康时间线 cell tooltip
function healthCellTitle(cell: {
  label: string;
  dialogues?: number;
  requests: number;
  success: number;
  failed: number;
  successRate: number | null;
  pad?: boolean;
}): string {
  if (!cell.label && cell.pad) return "空档";
  if (!cell.label) return "—";
  const dialogues = cell.dialogues || 0;
  const dialoguePart = dialogues > 0 ? ` · 对话 ${formatTokens(dialogues)}` : "";
  if (cell.requests <= 0 && cell.failed <= 0) {
    return dialogues > 0
      ? `${cell.label}${dialoguePart} · 无请求`
      : `${cell.label} · 无请求`;
  }
  const rateTxt = cell.successRate == null ? "—" : `${(cell.successRate * 100).toFixed(1)}%`;
  const failPart = cell.failed > 0
    ? ` · ⚠ 失败 ${formatTokens(cell.failed)}`
    : ` · 失败 ${formatTokens(cell.failed)}`;
  return `${cell.label}${dialoguePart} · 请求 ${formatTokens(cell.requests)} · 成功 ${formatTokens(cell.success)}${failPart} · 成功率 ${rateTxt}`;
}

// —— 请求健康时间线：与左侧趋势同粒度完整节点 ——
const healthTimeline = computed(() =>
  buildHealthTimeline(
    store.requestHealth.value?.buckets ?? [],
    trendGranularity.value,
    store.tokenStatsFrom.value || undefined,
    store.tokenStatsTo.value || undefined,
    // 用全量 usage（含区间外）作为请求活跃度底图，避免大量误显示“无数据”
    (store.tokenUsage.value?.buckets ?? []).map((b) => ({
      timestamp: b.timestamp,
      conversationCount: b.conversationCount || 0,
      outputTokens: b.outputTokens || 0,
      reasoningOutputTokens: b.reasoningOutputTokens || 0,
      totalTokens: b.totalTokens || 0,
    })),
  ),
);

// 网格：上→下、左→右（列优先），排满容器；所选区间落在末尾
const HEALTH_ROWS = 8;
const HEALTH_CELL = 11; // px
const HEALTH_GAP = 3;   // px
const healthGridRef = ref<HTMLElement | null>(null);
const healthCols = ref(24);
let healthRo: ResizeObserver | null = null;

function measureHealthGrid() {
  const el = healthGridRef.value;
  if (!el) return;
  const width = el.clientWidth || el.getBoundingClientRect().width;
  if (width <= 0) return;
  const cols = Math.max(1, Math.floor((width + HEALTH_GAP) / (HEALTH_CELL + HEALTH_GAP)));
  if (cols !== healthCols.value) healthCols.value = cols;
}

type HealthDisplayCell = {
  key: string;
  label: string;
  dialogues: number;
  success: number;
  failed: number;
  requests: number;
  successRate: number | null;
  level: number;
  pad?: boolean;
};

// 列优先展示：先填满一列（上→下），再下一列（左→右）
// capacity = rows * cols 排满；若节点不足则前置占位，保证所选区间在末尾
// 若节点过多则只保留末尾 capacity 个（仍保证选中区间的最后部分在网格末端）
// 全量健康桶按当前粒度聚合（含所选区间之外），供前置补全取值
const healthBucketMap = computed(() => {
  const map = new Map<string, { dialogues: number; requests: number; success: number; failed: number; usage: number }>();
  for (const b of store.requestHealth.value?.buckets ?? []) {
    const { key } = bucketKeyFor(trendGranularity.value, b.hour);
    if (!key) continue;
    const cur = map.get(key) || { dialogues: 0, requests: 0, success: 0, failed: 0, usage: 0 };
    cur.dialogues += Number(b.dialogues || 0);
    cur.requests += b.requests || 0;
    cur.success += b.success || 0;
    cur.failed += b.failed || 0;
    map.set(key, cur);
  }
  // usage 对每个时间节点独立兜底。即使其他日期已有真实请求数据，
  // 当前日期只要存在 Token 活动，也不应该被显示成“无数据”。
  for (const b of store.tokenUsage.value?.buckets ?? []) {
    const { key } = bucketKeyFor(trendGranularity.value, b.timestamp);
    if (!key) continue;
    const cur = map.get(key) || { dialogues: 0, requests: 0, success: 0, failed: 0, usage: 0 };
    cur.usage += estimateRequestCount({
      conversationCount: b.conversationCount || 0,
      outputTokens: b.outputTokens || 0,
      reasoningOutputTokens: b.reasoningOutputTokens || 0,
      totalTokens: b.totalTokens || 0,
    });
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

  // 前置自动补全：向前延伸真实时间节点，并填入对应时间点的真实成功/失败值
  const padCount = Math.max(0, capacity - body.length);
  let pads: HealthDisplayCell[] = [];
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
        successRate,
        level: healthLevelOf(successRate, requests > 0 || failed > 0, failed, requests),
        pad: false,
      };
    });
    const lack = padCount - mapped.length;
    const extras = Array.from({ length: Math.max(0, lack) }, (_, i) => ({
      key: `pad-${i}`,
      label: "",
      dialogues: 0,
      success: 0,
      failed: 0,
      requests: 0,
      successRate: null as number | null,
      level: 0,
      pad: true,
    }));
    pads = [...extras, ...mapped];
  }
  return [...pads, ...body];
});

// —— ECharts 折线图 option（使用趋势）——
const trendChartOption = computed<EChartsOption>(() => {
  const isHourly = trendGranularity.value === "hour";
  const labels = trendSeries.value.map((item) => {
    if (isHourly) {
      // 逐小时：拆分日期和小时，仅取小时做标签，完整放 tooltip
      const m = item.label.match(/^(\d{4}-\d{2}-\d{2}) (\d{2}:\d{2})$/);
      return m ? m[2] : item.label;
    }
    return item.label;
  });
  const values = trendSeries.value.map((item) => item.value);
  const dark = document.documentElement.dataset.theme === "dark";
  const textColor = dark ? "#d4d4d4" : "#737373";
  const faintColor = dark ? "#737373" : "#a3a3a3";
  const splitColor = dark ? "#262626" : "#e5e5e5";
  const areaTop = dark ? "rgba(52,211,153,0.22)" : "rgba(16,185,129,0.18)";

  return {
    grid: { left: 6, right: 10, top: 8, bottom: 2, containLabel: true },
    tooltip: {
      trigger: "axis",
      backgroundColor: dark ? "#1f1f1f" : "#ffffff",
      borderColor: dark ? "#333333" : "#e5e5e5",
      textStyle: { color: textColor, fontSize: 12 },
      formatter: (params: unknown) => {
        const list = params as { axisValue: string; value: number; dataIndex: number }[];
        const first = list[0];
        const raw = trendSeries.value[first.dataIndex]?.label ?? "";
        const date = isHourly ? raw : raw;
        return `<div style="font-weight:600;margin-bottom:4px">${date}</div><div style="display:flex;align-items:center;gap:6px"><span style="display:inline-block;width:10px;height:10px;border-radius:2px;background:#10b981"></span>Tokens <b>${formatTokens(first.value)}</b></div>`;
      },
    },
    xAxis: {
      type: "category",
      data: labels,
      boundaryGap: isHourly ? false : true,
      axisLine: { lineStyle: { color: splitColor } },
      axisTick: { show: false },
      axisLabel: {
        color: faintColor,
        fontSize: 10,
        interval: "auto",
        hideOverlap: true,
        formatter: (v: string) => v,
      },
    },
    yAxis: {
      type: "value",
      axisLabel: { color: faintColor, fontSize: 10, formatter: (v: number) => formatCompact(v) },
      splitLine: { lineStyle: { type: "dashed", color: splitColor } },
    },
    series: [
      {
        name: "Tokens",
        type: "line",
        data: values,
        smooth: 0.35,
        symbol: "circle",
        symbolSize: isHourly ? 4 : 5,
        showSymbol: values.length <= 40,
        lineStyle: { width: 2.5, color: "#10b981" },
        itemStyle: { color: "#10b981", borderColor: dark ? "#171717" : "#ffffff", borderWidth: 1.5 },
        areaStyle: { color: { type: "linear", x: 0, y: 0, x2: 0, y2: 1, colorStops: [{ offset: 0, color: areaTop }, { offset: 1, color: "rgba(16,185,129,0)" }] } },
        emphasis: { focus: "series" },
      },
    ],
  };
});

const modalTitle = computed(() => "用量明细");

async function refreshAll() {
  try {
    await store.syncTokenCollector(true);
  } catch {
    // 保留现有缓存数据，错误由同步状态区域展示。
  }
  await store.refreshTokenDatabaseView(true);
}


watch(
  () => [healthTimeline.value.nodeCount, store.tokenStatsFrom.value, store.tokenStatsTo.value, trendGranularity.value],
  () => nextTick(() => measureHealthGrid()),
);

onMounted(() => {
  // 全局查询定时器已维护数据库快照；页面挂载只负责图表布局。
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
  healthRo?.disconnect();
  healthRo = null;
  window.removeEventListener("resize", measureHealthGrid);
});
</script>

<template>
  <main class="token-stats-page tt-dash">
    <header class="token-stats-header">
      <div>
        <span class="token-stats-eyebrow">TOKEN TRACKER</span>
        <h1>Token 统计</h1>
        <p>后台增量采集本地日志写入数据库，页面只查询数据库快照。</p>
      </div>
      <div class="token-stats-actions">
        <QuickRangeDropdown />
        <button
          class="tt-btn"
          type="button"
          :disabled="store.tokenStatsLoading.value || store.tokenUsageLoading.value || store.tokenCollectorSyncing.value"
          title="立即采集本地 Token 日志并刷新数据库"
          @click="refreshAll"
        >
          <span :class="{ 'is-spinning': store.tokenStatsLoading.value || store.tokenCollectorSyncing.value }">↻</span>
          <span>{{ store.tokenCollectorSyncing.value ? "同步中…" : (store.tokenStatsLoading.value ? "读取中…" : "刷新") }}</span>
        </button>
      </div>
      <span
        v-if="store.tokenCollectorSyncing.value"
        class="tt-sync-status"
        role="status"
      >正在采集本地日志并写入数据库…</span>
      <span
        v-else-if="store.tokenCollectorSyncError.value"
        class="tt-sync-status is-error"
        :title="store.tokenCollectorSyncError.value"
      >同步失败，显示本地缓存</span>
    </header>

    <div class="token-stats-scroll">
      <div v-if="store.tokenUsageError.value" class="tt-error" role="alert">
        <strong>无法读取 Token 统计</strong>
        <p>{{ store.tokenUsageError.value }}</p>
        <p>OpenHub 会直接读取 Codex、Claude、Command Code、Antigravity、OpenCode、MiMo、ZCode 与 CatPawAI 的本地日志；请确认相关工具已产生可读取记录。</p>
      </div>

      <template v-if="store.tokenUsageLoading.value && !store.tokenUsage.value">
        <div class="tt-empty"><span class="is-spinning">↻</span><strong>正在读取本地用量数据…</strong></div>
      </template>

      <template v-else-if="store.tokenUsage.value">
        <!-- KPI 指标卡 -->
        <div class="tt-kpi-head">
          <span class="tt-kpi-range">统计区间</span>
          <strong class="tt-kpi-range-val">{{ rangeLabel }}</strong>
        </div>
        <div class="tt-kpis">
          <div class="tt-kpi tt-kpi-total">
            <div class="tt-kpi-total-main">
              <div class="tt-kpi-top">
                <span class="tt-kpi-ic ic-indigo" v-html="icons.chart"></span>
                <span class="tt-kpi-label">TOKEN 总数</span>
              </div>
              <strong class="tt-kpi-value tt-kpi-value-lg">{{ formatCompact(bucketTotal.total) }}</strong>
              <div class="tt-kpi-splits">
                <span><i class="dot in"></i>输入 {{ formatCompact(rangeSplits.input) }}</span>
                <span><i class="dot out"></i>输出 {{ formatCompact(rangeSplits.output) }}</span>
              </div>
              <span class="tt-kpi-sub">本地日志统计</span>
            </div>
            <div class="tt-kpi-total-side">
              <div class="tt-kpi-top">
                <span class="tt-kpi-ic ic-teal" v-html="icons.database"></span>
                <span class="tt-kpi-label">缓存命中率</span>
              </div>
              <strong class="tt-kpi-value tt-kpi-value-md">{{ formatRate(cacheHitRate) }}</strong>
              <span class="tt-kpi-sub">缓存 {{ formatCompact(rangeSplits.cache) }}</span>
            </div>
          </div>
          <div class="tt-kpi">
            <div class="tt-kpi-top">
              <span class="tt-kpi-ic ic-orange" v-html="icons.flame"></span>
              <span class="tt-kpi-label">日均 Tokens</span>
            </div>
            <strong class="tt-kpi-value">{{ formatCompact(dailyAverage) }}</strong>
            <span class="tt-kpi-sub">跨度 {{ formatTokens(rangeDays) }} 天 · 连续 {{ formatTokens(streakDays) }} 天</span>
          </div>
          <div class="tt-kpi">
            <div class="tt-kpi-top">
              <span class="tt-kpi-ic ic-blue" v-html="icons.activity"></span>
              <span class="tt-kpi-label">对话数</span>
            </div>
            <strong class="tt-kpi-value">
              <template v-if="store.requestHealthLoading.value && !store.requestHealth.value">…</template>
              <template v-else>{{ formatTokens(healthTimeline.totalDialogues) }}</template>
            </strong>
            <span class="tt-kpi-sub">
              <template v-if="store.requestHealthLoading.value && !store.requestHealth.value">
                正在扫描多工具日志…
              </template>
              <template v-else>
                请求 {{ formatTokens(healthTimeline.totalRequests) }}
                <template v-if="healthTimeline.totalSuccess + healthTimeline.totalFailed > 0">
                  · 失败 {{ formatTokens(healthTimeline.totalFailed) }}
                </template>
                <template v-else>
                  · 用户发起
                </template>
              </template>
            </span>
          </div>
          <div class="tt-kpi">
            <div class="tt-kpi-top">
              <span class="tt-kpi-ic ic-purple" v-html="icons.card"></span>
              <span class="tt-kpi-label">估算成本</span>
            </div>
            <strong class="tt-kpi-value">{{ costSummary.pricedTokens > 0 ? formatCost(estimatedCost) : "—" }}</strong>
            <span class="tt-kpi-sub">{{ costCaption }}</span>
          </div>
        </div>

                        <!-- 图表区：趋势 + 热力图 -->
        <div class="tt-charts">
          <section class="tt-card tt-card-chart">
            <header class="tt-card-head">
              <div>
                <h2>使用趋势</h2>
                <p>TREND · 按{{ trendUnitLabel() }} · 区间完整节点 {{ trendSeries.length }} 个（无数据也保留）</p>
              </div>
              <div class="tt-head-actions">
                <button type="button" class="tt-link-btn" @click="openDetail('daily')">明细 ▾</button>
              </div>
            </header>
            <div class="tt-card-body tt-chart-body">
              <EChart v-if="trendSeries.length" :option="trendChartOption" height="180px" />
              <div v-else class="tt-table-empty">该范围内没有趋势数据</div>
            </div>
          </section>

          <section class="tt-card tt-card-heat">
            <header class="tt-card-head tt-health-head">
              <div>
                <h2>请求健康时间线</h2>
                <p>色阶按成功率：≥99% 绿 · 95–99% 浅绿 · 85–95% 黄 · 70–85% 橙 · &lt;70% 红（用户取消不计失败）。</p>
              </div>
              <div class="tt-health-summary">
                <div class="tt-health-rate-label">成功率</div>
                <div class="tt-health-rate-value">
                  {{ healthTimeline.successRate != null ? (healthTimeline.successRate * 100).toFixed(1) + "%" : "—" }}
                </div>
                <div class="tt-health-counts">
                  <span class="ok">● 成功 {{ formatTokens(healthTimeline.totalSuccess) }}</span>
                  <span class="bad">● 失败 {{ formatTokens(healthTimeline.totalFailed) }}</span>
                </div>
              </div>
            </header>
            <div class="tt-card-body tt-health-body">
              <div v-if="healthTimeline.cells.length" class="tt-health-timeline">
                <div
                  ref="healthGridRef"
                  class="tt-health-grid"
                  :style="{ gridTemplateRows: `repeat(${HEALTH_ROWS}, ${HEALTH_CELL}px)` }"
                >
                  <div
                    v-for="cell in healthDisplayCells"
                    :key="cell.key"
                    class="tt-health-cell"
                    :class="[
                      'lv' + cell.level,
                      { 'is-pad': cell.pad },
                    ]"
                    :title="healthCellTitle(cell)"
                  ></div>
                </div>
                <div class="tt-health-legend">
                  <span>差</span>
                  <span class="tt-health-cell lv1" title="成功率 &lt; 70%"></span>
                  <span class="tt-health-cell lv2" title="70% ~ 85%"></span>
                  <span class="tt-health-cell lv3" title="85% ~ 95%"></span>
                  <span class="tt-health-cell lv4" title="95% ~ 99%"></span>
                  <span class="tt-health-cell lv5" title="≥ 99%"></span>
                  <span>优</span>
                  <span class="tt-health-cell lv0"></span>
                  <span>无数据</span>
                  <span class="tt-health-meta">
                    · 区间节点 {{ healthTimeline.nodeCount }}
                    · 网格 {{ HEALTH_ROWS }}×{{ healthCols }}
                    · 有数据 {{ healthTimeline.activeCount }}
                  </span>
                </div>
              </div>
              <div v-else class="tt-table-empty">该范围内没有请求健康数据</div>
            </div>
          </section>
        </div>

        <!-- 分布区：工具 + 模型（紧凑展示，完整列表进弹窗） -->
        <div class="tt-dist">
          <section class="tt-card">
            <header class="tt-card-head">
              <div>
                <h2>按工具分布</h2>
                <p>BY SOURCE</p>
              </div>
              <button type="button" class="tt-link-btn" @click="openModal('sources')">查看全部 · {{ bySource.length }}</button>
            </header>
            <div class="tt-card-body">
              <div v-for="(item, index) in topSources" :key="item.source" class="tt-provider">
                <span class="tt-provider-dot" :style="{ background: providerColor(item.source, index) }"></span>
                <span class="tt-provider-name">{{ sourceLabel(item.source) }}</span>
                <span class="tt-provider-bar">
                  <i :style="{ width: `${shareOf(item.totalTokens, totalTokensAll)}%`, background: providerColor(item.source, index) }"></i>
                </span>
                <span class="tt-provider-pct">{{ shareOf(item.totalTokens, totalTokensAll).toFixed(2) }}%</span>
                <span class="tt-provider-val">{{ formatCompact(item.totalTokens) }}</span>
              </div>
              <div v-if="!bySource.length" class="tt-muted">该范围内没有数据</div>
            </div>
          </section>

          <section class="tt-card">
            <header class="tt-card-head">
              <div>
                <h2>主要模型</h2>
                <p>BY MODEL</p>
              </div>
              <button type="button" class="tt-link-btn" @click="openModal('models')">查看全部 · {{ byModel.length }}</button>
            </header>
            <div class="tt-card-body">
              <div v-for="(model, index) in topModels" :key="model.model" class="tt-provider">
                <span class="tt-provider-dot" :style="{ background: providerColor(model.model, index) }"></span>
                <span class="tt-provider-name" :title="model.model">{{ model.model || "未知模型" }}</span>
                <span class="tt-provider-bar">
                  <i :style="{ width: `${shareOf(model.totalTokens, totalTokensAll)}%`, background: providerColor(model.model, index) }"></i>
                </span>
                <span class="tt-provider-pct">{{ shareOf(model.totalTokens, totalTokensAll).toFixed(2) }}%</span>
                <span class="tt-provider-val">{{ formatCompact(model.totalTokens) }}</span>
              </div>
              <div v-if="!byModel.length" class="tt-muted">暂无模型数据</div>
            </div>
          </section>
        </div>

      </template>
    </div>

    <!-- 详情弹窗 -->
    <Transition name="tt-modal-fade">
      <div v-if="modalOpen" class="tt-modal-backdrop" @click.self="closeModal">
        <div
          class="tt-modal"
          :class="{ 'tt-modal-wide': modal === 'sources' || modal === 'models' }"
          role="dialog"
          aria-modal="true"
          :aria-label="modalTitle"
        >
          <header class="tt-modal-head">
            <div>
              <h2>{{ modalTitle }}</h2>
              <p v-if="modal === 'sources'">按工具汇总 · 共 {{ bySource.length }} 项</p>
              <p v-else-if="modal === 'models'">按模型汇总 · 共 {{ byModel.length }} 项</p>
              <p v-else>当前时间区间内的用量明细</p>
            </div>
            <button type="button" class="tt-modal-close" aria-label="关闭" @click="closeModal">×</button>
          </header>
          <div class="tt-modal-body">
            <!-- 明细：两个标签 + 分页 -->
            <div v-if="modal === 'daily' || modal === 'projects'">
              <div class="tt-modal-tabs">
                <button type="button" :class="{ active: detailTab === 'daily' }" @click="switchDetailTab('daily')">趋势明细 · {{ trendDetail.length }}</button>
                <button type="button" :class="{ active: detailTab === 'projects' }" @click="switchDetailTab('projects')">项目用量 · {{ projectUsage.length }}</button>
              </div>

              <div v-if="detailTab === 'daily'">
                <div class="tt-list-meta">按当前趋势粒度 · {{ trendUnitLabel() }}</div>
                <AppTable
                  :rows="trendDetail"
                  :columns="dailyColumns"
                  :row-key="(item: any) => item.label"
                  :page="detailPage"
                  :page-size="PAGE_SIZE"
                  empty-text="该范围内没有明细数据"
                  @update:page="detailPage = $event"
                >
                  <template #cell-label="{ row }">{{ formatDetailTime(row.label) }}</template>
                  <template #cell-total="{ row }">{{ formatCompact(row.total) }}</template>
                  <template #cell-input="{ row }">{{ formatCompact(row.input) }}</template>
                  <template #cell-output="{ row }">{{ formatCompact(row.output) }}</template>
                  <template #cell-cache="{ row }">{{ formatCompact(row.cache) }}</template>
                  <template #cell-cacheHitRate="{ row }">{{ formatRate(row.cacheHitRate) }}</template>
                  <template #cell-reasoning="{ row }">{{ formatCompact(row.reasoning) }}</template>
                  <template #cell-sessions="{ row }">{{ formatTokens(row.sessions) }}</template>
                </AppTable>
              </div>

              <div v-else>
                <AppTable
                  :rows="projectUsage"
                  :columns="projectColumns"
                  :row-key="(item: any) => item.project"
                  :page="detailPage"
                  :page-size="PAGE_SIZE"
                  empty-text="该范围内没有项目数据"
                  @update:page="detailPage = $event"
                >
                  <template #cell-project="{ row }" :title="row.project">{{ row.project }}</template>
                  <template #cell-totalTokens="{ row }">{{ formatCompact(row.totalTokens) }}</template>
                  <template #cell-input="{ row }">{{ formatCompact(row.input) }}</template>
                  <template #cell-output="{ row }">{{ formatCompact(row.output) }}</template>
                  <template #cell-cache="{ row }">{{ formatCompact(row.cache) }}</template>
                  <template #cell-cacheHitRate="{ row }">{{ formatRate(row.cacheHitRate) }}</template>
                  <template #cell-reasoning="{ row }">{{ formatCompact(row.reasoning) }}</template>
                  <template #cell-sessions="{ row }">{{ formatTokens(row.sessions) }}</template>
                  <template #cell-costUsd="{ row }">{{ formatCost(row.costUsd) }}</template>
                </AppTable>
              </div>
            </div>

            <!-- 工具明细：完整 Token 用量列表 -->
            <div v-else-if="modal === 'sources'">
              <div class="tt-list-meta">所选时间区间 · 点击列头排序</div>
              <AppTable
                :rows="bySource"
                :columns="sourceColumns"
                :row-key="(item: any) => item.source"
                :page="detailPage"
                :page-size="PAGE_SIZE"
                empty-text="该范围内没有工具数据"
                @update:page="detailPage = $event"
              >
                <template #cell-source="{ row }">
                  <span class="tt-dimension-name" :title="sourceLabel(row.source)">
                    <i class="tt-provider-dot" :style="{ background: providerColor(row.source, 0) }"></i>
                    <b>{{ sourceLabel(row.source) }}</b>
                  </span>
                </template>
                <template #cell-totalTokens="{ row }">{{ formatCompact(row.totalTokens) }}</template>
                <template #cell-inputTokens="{ row }">{{ formatCompact(row.inputTokens) }}</template>
                <template #cell-outputTokens="{ row }">{{ formatCompact(row.outputTokens) }}</template>
                <template #cell-cacheTokens="{ row }">{{ formatCompact(row.cacheTokens) }}</template>
                <template #cell-cacheHitRate="{ row }">{{ formatRate(row.cacheHitRate) }}</template>
                <template #cell-reasoningTokens="{ row }">{{ formatCompact(row.reasoningTokens) }}</template>
                <template #cell-conversations="{ row }">{{ formatTokens(row.conversations) }}</template>
                <template #cell-costUsd="{ row }">{{ row.costUsd > 0 ? formatCost(row.costUsd) : "—" }}</template>
                <template #cell-share="{ row }">{{ shareOf(row.totalTokens, totalTokensAll).toFixed(2) }}%</template>
              </AppTable>
            </div>

            <!-- 模型明细：完整 Token 用量列表 -->
            <div v-else-if="modal === 'models'">
              <div class="tt-list-meta">所选时间区间 · 同系列模型归并 · 点击列头排序</div>
              <AppTable
                :rows="byModel"
                :columns="modelColumns"
                :row-key="(item: any) => item.model"
                :page="detailPage"
                :page-size="PAGE_SIZE"
                empty-text="该范围内没有模型数据"
                @update:page="detailPage = $event"
              >
                <template #cell-source="{ row }">
                  <span class="tt-dimension-name" :title="row.model || '未知模型'">
                    <i class="tt-provider-dot" :style="{ background: providerColor(row.model, 0) }"></i>
                    <b>{{ row.model || "未知模型" }}</b>
                  </span>
                </template>
                <template #cell-totalTokens="{ row }">{{ formatCompact(row.totalTokens) }}</template>
                <template #cell-inputTokens="{ row }">{{ formatCompact(row.inputTokens) }}</template>
                <template #cell-outputTokens="{ row }">{{ formatCompact(row.outputTokens) }}</template>
                <template #cell-cacheTokens="{ row }">{{ formatCompact(row.cacheTokens) }}</template>
                <template #cell-cacheHitRate="{ row }">{{ formatRate(row.cacheHitRate) }}</template>
                <template #cell-reasoningTokens="{ row }">{{ formatCompact(row.reasoningTokens) }}</template>
                <template #cell-conversations="{ row }">{{ formatTokens(row.conversations) }}</template>
                <template #cell-costUsd="{ row }">{{ row.costUsd > 0 ? formatCost(row.costUsd) : "—" }}</template>
                <template #cell-share="{ row }">{{ shareOf(row.totalTokens, totalTokensAll).toFixed(2) }}%</template>
              </AppTable>
            </div>
          </div>
        </div>
      </div>
    </Transition>
  </main>
</template>
