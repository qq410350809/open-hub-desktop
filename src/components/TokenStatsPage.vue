<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import type { EChartsOption } from "../echarts";
import EChart from "./EChart.vue";
import QuickRangeDropdown from "./QuickRangeDropdown.vue";
import { useStore } from "../composables/useStore";
import {
  bucketModelTotals,
  bucketSourceTotals,
  bucketTotals,
  buildDailyMapFromBuckets,
  buildHeatmap,
  buildTrendDetailFromBuckets,
  buildTrendFromBuckets,
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
function totalPages(listLength: number) {
  return Math.max(1, Math.ceil(listLength / PAGE_SIZE));
}
function paginate<T>(list: T[]): T[] {
  const start = (detailPage.value - 1) * PAGE_SIZE;
  return list.slice(start, start + PAGE_SIZE);
}
function detailPrev() { if (detailPage.value > 1) detailPage.value--; }
function detailNext(total: number) { if (detailPage.value < totalPages(total)) detailPage.value++; }
function switchDetailTab(tab: "daily" | "projects") {
  detailTab.value = tab;
  detailPage.value = 1;
}

// sessions 仅用于：项目用量表 + 会话成本估算（来自 Claude/Codex 会话日志）
const sessions = computed(() => store.tokenStats.value?.sessions ?? []);

// —— 供应商品牌色（复刻 tokentracker UsageOverview 配色）——
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
};
function providerColor(source: string, index: number) {
  return PROVIDER_COLORS[source.toLowerCase()] || `hsl(${150 + index * 40}, 60%, 45%)`;
}

const sourceNameMap: Record<string, string> = {
  claude: "Claude",
  codex: "Codex",
  cursor: "Cursor",
  gemini: "Gemini",
  opencode: "OpenCode",
  kiro: "Kiro",
  copilot: "Copilot",
  openclaw: "OpenClaw",
  goose: "Goose",
  antigravity: "Antigravity",
  zed: "Zed",
};
function sourceLabel(source: string) {
  return sourceNameMap[source] || source || "未知来源";
}

function shareOf(value: number, total: number) {
  return total > 0 ? Math.min(100, (value / total) * 100) : 0;
}

// —— 小时用量桶（覆盖所有工具，来自 cursors.json hourly.buckets）——
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

// 成本估算：用会话数据的实际单价（cost/token）外推到全部工具的 token 用量
const costPerToken = computed(() => {
  let tokens = 0;
  let cost = 0;
  for (const session of sessions.value) {
    tokens += session.totalTokens || 0;
    cost += session.costUsd || 0;
  }
  return tokens > 0 ? cost / tokens : 0;
});
const estimatedCost = computed(() => bucketTotal.value.total * costPerToken.value);

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
  reasoning: number;
  costUsd: number;
};
const projectUsage = computed<ProjectUsageItem[]>(() => {
  const groups = new Map<
    string,
    { project: string; sessions: number; totalTokens: number; input: number; output: number; cache: number; reasoning: number; costUsd: number }
  >();
  for (const session of sessions.value) {
    // UUID 形态的项目名无法识别（Claude Code 缺失 cwd 上下文时产生），归并为"其他"
    const rawKey = session.projectKey || "未知项目";
    const key = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(rawKey)
      ? "其他"
      : rawKey;
    const current =
      groups.get(key) ||
      {
        project: key, sessions: 0, totalTokens: 0,
        input: 0, output: 0, cache: 0, reasoning: 0, costUsd: 0,
      };
    current.sessions += 1;
    current.totalTokens += session.totalTokens || 0;
    current.input += session.tokens?.inputTokens || 0;
    current.output += session.tokens?.outputTokens || 0;
    current.cache += (session.tokens?.cachedInputTokens || 0) + (session.tokens?.cacheCreationInputTokens || 0);
    current.reasoning += session.tokens?.reasoningOutputTokens || 0;
    current.costUsd += session.costUsd || 0;
    groups.set(key, current);
  }
  return [...groups.values()].sort((a, b) => b.totalTokens - a.totalTokens).slice(0, 20);
});

const trendSeries = computed(() => buildTrendFromBuckets(filteredBuckets.value, trendGranularity.value));
// 明细列表与图表同粒度、同 key，保证一一对应
const trendDetail = computed(() => buildTrendDetailFromBuckets(filteredBuckets.value, trendGranularity.value));
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

const heatmap = computed(() => buildHeatmap(dailyMap.value));

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
    grid: { left: 8, right: 14, top: 12, bottom: 6, containLabel: true },
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

// —— ECharts 热力图 option（活跃热力图）——
const heatmapOption = computed<EChartsOption>(() => {
  const dark = document.documentElement.dataset.theme === "dark";
  const world = heatmap.value;
  const cells: number[] = [];
  for (const week of world.weeks) {
    for (const day of week.days) {
      cells.push(day.tokens);
    }
  }
  const maxTokens = Math.max(1, ...cells);
  // 构造热力图数据 [x, y, value]：x=第几周，y=星期几(0=一 ~ 6=日)，value=tokens
  const data: [number, number, number][] = [];
  const days = ["一", "二", "三", "四", "五", "六", "日"];
  world.weeks.forEach((week, wIndex) => {
    week.days.forEach((day, dIndex) => {
      data.push([wIndex, dIndex, day.tokens]);
    });
  });
  const colors: [number, string][] = [
    [0, dark ? "#1f1f1f" : "#f0f0f0"],
    [0.2, dark ? "#064e3b" : "#d1fae5"],
    [0.45, dark ? "#065f46" : "#6ee7b7"],
    [0.7, dark ? "#059669" : "#10b981"],
    [1, dark ? "#34d399" : "#047857"],
  ];
  return {
    grid: { left: 6, right: 6, top: 6, bottom: 16 },
    tooltip: {
      trigger: "item",
      backgroundColor: dark ? "#1f1f1f" : "#ffffff",
      borderColor: dark ? "#333333" : "#e5e5e5",
      textStyle: { color: dark ? "#d4d4d4" : "#737373", fontSize: 12 },
      formatter: (p: unknown) => {
        const item = p as { value: [number, number, number] };
        const [x, y, tokens] = item.value;
        const day = world.weeks[x]?.days[y];
        const label = day ? `${day.date} · 周${days[y]}` : "—";
        return `${label}<br/><b style="color:#10b981">${formatTokens(tokens)}</b> Tokens`;
      },
    },
    xAxis: {
      type: "category",
      data: world.weeks.map((_, i) => String(i)),
      splitArea: { show: false },
      axisLine: { show: false },
      axisTick: { show: false },
      axisLabel: { show: false },
    },
    yAxis: {
      type: "category",
      data: days,
      splitArea: { show: false },
      axisLine: { show: false },
      axisTick: { show: false },
      axisLabel: { color: dark ? "#a3a3a3" : "#737373", fontSize: 10 },
    },
    visualMap: {
      min: 0,
      max: maxTokens,
      calculable: false,
      orient: "horizontal",
      left: "center",
      bottom: 0,
      itemWidth: 12,
      itemHeight: 8,
      text: ["多", "少"],
      textStyle: { color: dark ? "#a3a3a3" : "#808080", fontSize: 10 },
      inRange: { color: colors.map((c) => c[1]) },
    },
    series: [
      {
        type: "heatmap",
        data,
        itemStyle: { borderColor: dark ? "#0a0a0a" : "#ffffff", borderWidth: 2, borderRadius: 2 },
        emphasis: { itemStyle: { shadowBlur: 6, shadowColor: "rgba(16,185,129,0.5)" } },
      },
    ],
  };
});
const modalTitle = computed(() => "用量明细");

function refreshAll() {
  store.refreshTokenStats();
  void store.loadTokenUsage();
}

onMounted(() => {
  void Promise.all([store.loadTokenUsage(), store.loadTokenStats()]);
});
</script>

<template>
  <main class="token-stats-page tt-dash">
    <header class="token-stats-header">
      <div>
        <span class="token-stats-eyebrow">TOKEN TRACKER</span>
        <h1>Token 统计</h1>
        <p>数据来自本机 tokentracker CLI 的本地会话日志。</p>
      </div>
      <div class="token-stats-actions">
        <QuickRangeDropdown />
        <button
          class="tt-btn"
          type="button"
          :disabled="store.tokenStatsLoading.value || store.tokenUsageLoading.value"
          title="强制重读本地用量数据后刷新"
          @click="refreshAll"
        >
          <span :class="{ 'is-spinning': store.tokenStatsLoading.value }">↻</span>
          <span>{{ store.tokenStatsLoading.value ? "读取中…" : "刷新" }}</span>
        </button>
      </div>
    </header>

    <div class="token-stats-scroll">
      <div v-if="store.tokenUsageError.value" class="tt-error" role="alert">
        <strong>无法读取 Token 统计</strong>
        <p>{{ store.tokenUsageError.value }}</p>
        <p>依赖本机 tokentracker CLI：<code>npm i -g tokentracker-cli</code>，或设置 <code>OPENHUB_TOKENTRACKER_PATH</code>。</p>
      </div>

      <template v-if="store.tokenUsageLoading.value && !store.tokenUsage.value">
        <div class="tt-empty"><span class="is-spinning">↻</span><strong>正在读取本地用量数据…</strong></div>
      </template>

      <template v-else-if="store.tokenUsage.value">
        <!-- KPI 指标卡 -->
        <div class="tt-kpis">
          <div class="tt-kpi tt-kpi-hero">
            <span class="tt-kpi-label">TOKEN 总数</span>
            <strong class="tt-kpi-value">{{ formatCompact(bucketTotal.total) }}</strong>
            <span class="tt-kpi-sub">输入 {{ formatCompact(rangeSplits.input) }} · 输出 {{ formatCompact(rangeSplits.output) }}</span>
          </div>
          <div class="tt-kpi">
            <span class="tt-kpi-label">对话数</span>
            <strong class="tt-kpi-value">{{ formatTokens(bucketTotal.conversations) }}</strong>
            <span class="tt-kpi-sub">区间内对话轮次</span>
          </div>
          <div class="tt-kpi">
            <span class="tt-kpi-label">估算成本</span>
            <strong class="tt-kpi-value">{{ estimatedCost > 0 ? formatCost(estimatedCost) : "—" }}</strong>
            <span class="tt-kpi-sub">基于会话单价外推</span>
          </div>
          <div class="tt-kpi">
            <span class="tt-kpi-label">日均 Tokens</span>
            <strong class="tt-kpi-value">{{ formatCompact(dailyAverage) }}</strong>
            <span class="tt-kpi-sub">区间总量 / {{ formatTokens(activeDays) }} 活跃天</span>
          </div>
          <div class="tt-kpi">
            <span class="tt-kpi-label">缓存命中率</span>
            <strong class="tt-kpi-value">{{ formatRate(cacheHitRate) }}</strong>
            <span class="tt-kpi-sub">输入缓存占比 · 缓存 {{ formatCompact(rangeSplits.cache) }}</span>
          </div>
          <div class="tt-kpi">
            <span class="tt-kpi-label">活跃天数</span>
            <strong class="tt-kpi-value">{{ formatTokens(activeDays) }}<small>天</small></strong>
            <span class="tt-kpi-sub">区间跨度 {{ formatTokens(rangeDays) }} 天 · 连续 {{ formatTokens(streakDays) }} 天</span>
          </div>
        </div>

                <!-- 图表区：趋势 + 热力图 -->
        <div class="tt-charts">
          <section class="tt-card tt-card-chart">
            <header class="tt-card-head">
              <div>
                <h2>使用趋势</h2>
                <p>TREND · 按{{ trendUnitLabel() }}罗列 · 已按时间区间自适应</p>
              </div>
              <div class="tt-head-actions">
                <button type="button" class="tt-link-btn" @click="openDetail('daily')">明细 ▾</button>
              </div>
            </header>
            <div class="tt-card-body tt-chart-body">
              <EChart v-if="trendSeries.length" :option="trendChartOption" height="240px" />
              <div v-else class="tt-table-empty">该范围内没有趋势数据</div>
            </div>
          </section>

          <section class="tt-card tt-card-heat">
            <header class="tt-card-head">
              <div>
                <h2>活跃热力图</h2>
                <p>ACTIVITY · {{ heatmap.startLabel }} ~ {{ heatmap.endLabel }}</p>
              </div>
            </header>
            <div class="tt-card-body tt-chart-body">
              <EChart :option="heatmapOption" height="150px" />
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
        <div class="tt-modal" role="dialog" aria-modal="true" :aria-label="modalTitle">
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
                <div class="tt-table tt-daily" role="table">
                  <div class="tt-table-row tt-table-head" role="row">
                    <span class="tt-col-date">时间</span>
                    <span class="tt-col-num">总计</span>
                    <span class="tt-col-num">输入</span>
                    <span class="tt-col-num">输出</span>
                    <span class="tt-col-num">缓存</span>
                    <span class="tt-col-num">推理</span>
                    <span class="tt-col-num">对话</span>
                  </div>
                  <div v-for="(item, idx) in paginate(trendDetail)" :key="item.label + idx" class="tt-table-row" role="row">
                    <span class="tt-col-date">{{ formatDetailTime(item.label) }}</span>
                    <span class="tt-col-num">{{ formatCompact(item.total) }}</span>
                    <span class="tt-col-num">{{ formatCompact(item.input) }}</span>
                    <span class="tt-col-num">{{ formatCompact(item.output) }}</span>
                    <span class="tt-col-num">{{ formatCompact(item.cache) }}</span>
                    <span class="tt-col-num">{{ formatCompact(item.reasoning) }}</span>
                    <span class="tt-col-num">{{ formatTokens(item.sessions) }}</span>
                  </div>
                  <div v-if="!trendDetail.length" class="tt-table-empty">该范围内没有明细数据</div>
                </div>
                <div class="tt-pager">
                  <button type="button" :disabled="detailPage <= 1" @click="detailPrev">‹ 上一页</button>
                  <span>第 {{ detailPage }} / {{ totalPages(trendDetail.length) }} 页</span>
                  <button type="button" :disabled="detailPage >= totalPages(trendDetail.length)" @click="detailNext(trendDetail.length)">下一页 ›</button>
                </div>
              </div>

              <div v-else>
                <div class="tt-table tt-projects" role="table">
                  <div class="tt-table-row tt-table-head" role="row">
                    <span>项目</span>
                    <span class="tt-col-num">总计</span>
                    <span class="tt-col-num">输入</span>
                    <span class="tt-col-num">输出</span>
                    <span class="tt-col-num">缓存</span>
                    <span class="tt-col-num">推理</span>
                    <span class="tt-col-num">对话</span>
                    <span class="tt-col-num">成本</span>
                  </div>
                  <div v-for="item in paginate(projectUsage)" :key="item.project" class="tt-table-row" role="row">
                    <span :title="item.project">{{ item.project }}</span>
                    <span class="tt-col-num">{{ formatCompact(item.totalTokens) }}</span>
                    <span class="tt-col-num">{{ formatCompact(item.input) }}</span>
                    <span class="tt-col-num">{{ formatCompact(item.output) }}</span>
                    <span class="tt-col-num">{{ formatCompact(item.cache) }}</span>
                    <span class="tt-col-num">{{ formatCompact(item.reasoning) }}</span>
                    <span class="tt-col-num">{{ formatTokens(item.sessions) }}</span>
                    <span class="tt-col-num">{{ formatCost(item.costUsd) }}</span>
                  </div>
                  <div v-if="!projectUsage.length" class="tt-table-empty">该范围内没有项目数据</div>
                </div>
                <div class="tt-pager">
                  <button type="button" :disabled="detailPage <= 1" @click="detailPrev">‹ 上一页</button>
                  <span>第 {{ detailPage }} / {{ totalPages(projectUsage.length) }} 页</span>
                  <button type="button" :disabled="detailPage >= totalPages(projectUsage.length)" @click="detailNext(projectUsage.length)">下一页 ›</button>
                </div>
              </div>
            </div>

            <!-- 工具明细 -->
            <div v-else-if="modal === 'sources'">
              <div class="tt-provider-list">
                <div v-for="(item, index) in bySource" :key="item.source" class="tt-provider">
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
            </div>

            <!-- 模型明细 -->
            <div v-else-if="modal === 'models'">
              <div class="tt-provider-list">
                <div v-for="(model, index) in byModel" :key="model.model" class="tt-provider">
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
            </div>
          </div>
        </div>
      </div>
    </Transition>
  </main>
</template>
