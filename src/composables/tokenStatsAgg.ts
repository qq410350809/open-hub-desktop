// Token 统计页的纯聚合/绘图几何逻辑（无 Vue 依赖，可独立测试）。
// 数据来自 tokentracker CLI 的 sessions 数组。

export interface TokenTokensLike {
  inputTokens?: number;
  outputTokens?: number;
  cachedInputTokens?: number;
  cacheCreationInputTokens?: number;
  reasoningOutputTokens?: number;
}
export interface TokenSessionLike {
  startedAt: string;
  totalTokens?: number;
  costUsd?: number;
  source?: string;
  projectKey?: string;
  tokens?: TokenTokensLike;
}
export interface DailyStat {
  date: string;
  total: number;
  input: number;
  output: number;
  cache: number;
  reasoning: number;
  sessions: number;
}

export function toLocalDate(value: Date): string {
  const year = value.getFullYear();
  const month = String(value.getMonth() + 1).padStart(2, "0");
  const day = String(value.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

export function localDateOf(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso.slice(0, 10) || "";
  return toLocalDate(date);
}

export function parseLocal(dateStr: string): Date {
  return new Date(`${dateStr}T00:00:00`);
}

export function startOfWeek(date: Date): Date {
  const result = new Date(date);
  const offset = (result.getDay() + 6) % 7;
  result.setDate(result.getDate() - offset);
  return result;
}

export function buildDailyMap(sessions: TokenSessionLike[]): Map<string, DailyStat> {
  const map = new Map<string, DailyStat>();
  for (const session of sessions) {
    const date = localDateOf(session.startedAt);
    if (!date) continue;
    const current = map.get(date) || {
      date,
      total: 0,
      input: 0,
      output: 0,
      cache: 0,
      reasoning: 0,
      sessions: 0,
    };
    current.total += session.totalTokens || 0;
    current.input += session.tokens?.inputTokens || 0;
    current.output += session.tokens?.outputTokens || 0;
    // 缓存列 = 命中缓存 + 新写缓存；total_tokens 由 CLI 权威给出，组件列不一定与总计严格对账。
    current.cache +=
      (session.tokens?.cachedInputTokens || 0) + (session.tokens?.cacheCreationInputTokens || 0);
    current.reasoning += session.tokens?.reasoningOutputTokens || 0;
    current.sessions += 1;
    map.set(date, current);
  }
  return map;
}

export function buildDailyBreakdown(sessions: TokenSessionLike[]): DailyStat[] {
  return [...buildDailyMap(sessions).values()].sort((left, right) =>
    right.date.localeCompare(left.date),
  );
}

export function buildTrendSeries(
  dailyMap: Map<string, DailyStat>,
  granularity: "day" | "week" | "month",
): { label: string; value: number }[] {
  const keys = [...dailyMap.keys()].sort();
  if (granularity === "day") {
    return keys.map((key) => ({ label: key, value: dailyMap.get(key)!.total }));
  }
  const buckets = new Map<string, { label: string; value: number }>();
  for (const key of keys) {
    const date = parseLocal(key);
    const bucketKey =
      granularity === "week" ? toLocalDate(startOfWeek(date)) : key.slice(0, 7);
    const current = buckets.get(bucketKey) || { label: bucketKey, value: 0 };
    current.value += dailyMap.get(key)!.total;
    buckets.set(bucketKey, current);
  }
  return [...buckets.values()];
}

// —— 多粒度使用趋势（基于小时桶，支持 日/周/月/年/全 五档）——
export type TrendGranularity =
  | "hour"   // < 7 天 → 逐小时
  | "day"    // ≤ 92 天 → 逐日
  | "month"; // > 92 天 → 逐月

export function bucketKeyFor(granularity: TrendGranularity, iso: string): { key: string; label: string } {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return { key: iso, label: iso };
  const day = toLocalDate(date);
  switch (granularity) {
    case "hour": {
      const hh = String(date.getHours()).padStart(2, "0");
      return { key: day + "-" + hh, label: day + " " + hh + ":00" };
    }
    case "day":
      return { key: day, label: day };
    case "month":
      return { key: day.slice(0, 7), label: day.slice(0, 7) };
    default:
      return { key: day, label: day };
  }
}

/** 根据顶部时间区间 + 粒度，生成完整节点（无数据也保留空节点） */
export function buildRangeKeys(
  from: string,
  to: string,
  granularity: TrendGranularity,
): { key: string; label: string }[] {
  if (!from || !to || from > to) return [];
  const keys: { key: string; label: string }[] = [];
  if (granularity === "hour") {
    const cursor = parseLocal(from);
    const end = parseLocal(to);
    end.setHours(23, 0, 0, 0);
    while (cursor.getTime() <= end.getTime()) {
      const day = toLocalDate(cursor);
      const hh = String(cursor.getHours()).padStart(2, "0");
      keys.push({ key: `${day}-${hh}`, label: `${day} ${hh}:00` });
      cursor.setHours(cursor.getHours() + 1);
    }
    return keys;
  }
  if (granularity === "month") {
    let y = Number(from.slice(0, 4));
    let m = Number(from.slice(5, 7));
    const ey = Number(to.slice(0, 4));
    const em = Number(to.slice(5, 7));
    while (y < ey || (y === ey && m <= em)) {
      const label = `${y}-${String(m).padStart(2, "0")}`;
      keys.push({ key: label, label });
      m += 1;
      if (m > 12) {
        m = 1;
        y += 1;
      }
    }
    return keys;
  }
  // day
  const cursor = parseLocal(from);
  const end = parseLocal(to);
  while (cursor.getTime() <= end.getTime()) {
    const day = toLocalDate(cursor);
    keys.push({ key: day, label: day });
    cursor.setDate(cursor.getDate() + 1);
  }
  return keys;
}

function resolveTrendSpan(
  buckets: UsageBucketLike[],
  from?: string,
  to?: string,
): { from: string; to: string } | null {
  if (from && to) return { from, to };
  let min = "";
  let max = "";
  for (const bucket of buckets) {
    const day = localDateOf(bucket.timestamp);
    if (!day) continue;
    if (!min || day < min) min = day;
    if (!max || day > max) max = day;
  }
  if (!min || !max) return null;
  return { from: min, to: max };
}

export function buildTrendFromBuckets(
  buckets: UsageBucketLike[],
  granularity: TrendGranularity,
  from?: string,
  to?: string,
): { label: string; value: number }[] {
  const map = new Map<string, { label: string; value: number }>();
  for (const bucket of buckets) {
    const { key, label } = bucketKeyFor(granularity, bucket.timestamp);
    if (!key) continue;
    const current = map.get(key) || { label, value: 0 };
    current.value += bucket.totalTokens || 0;
    map.set(key, current);
  }
  const span = resolveTrendSpan(buckets, from, to);
  if (!span) return [];
  return buildRangeKeys(span.from, span.to, granularity).map(({ key, label }) => ({
    label,
    value: map.get(key)?.value ?? 0,
  }));
}

export interface TrendDetailItem {
  label: string;
  total: number;
  input: number;
  output: number;
  cache: number;
  reasoning: number;
  sessions: number;
}

// 按粒度聚合完整分项，用于明细列表（与图表 buildTrendFromBuckets 同粒度、同 key，保证一一对应）
export function buildTrendDetailFromBuckets(
  buckets: UsageBucketLike[],
  granularity: TrendGranularity,
  from?: string,
  to?: string,
): TrendDetailItem[] {
  const map = new Map<string, TrendDetailItem>();
  for (const bucket of buckets) {
    const { key, label } = bucketKeyFor(granularity, bucket.timestamp);
    if (!key) continue;
    const current = map.get(key) || {
      label,
      total: 0, input: 0, output: 0, cache: 0, reasoning: 0, sessions: 0,
    };
    current.total += bucket.totalTokens || 0;
    current.input += bucket.inputTokens || 0;
    current.output += bucket.outputTokens || 0;
    current.cache += (bucket.cachedInputTokens || 0) + (bucket.cacheCreationInputTokens || 0);
    current.reasoning += bucket.reasoningOutputTokens || 0;
    current.sessions += bucket.conversationCount || 0;
    map.set(key, current);
  }
  const span = resolveTrendSpan(buckets, from, to);
  if (!span) return [];
  return buildRangeKeys(span.from, span.to, granularity).map(({ key, label }) => {
    const current = map.get(key);
    return current || {
      label,
      total: 0, input: 0, output: 0, cache: 0, reasoning: 0, sessions: 0,
    };
  });
}

export function buildChartGeometry2(
  series: { label: string; value: number }[],
  width = CHART_W,
  height = CHART_H,
  pad = CHART_PAD,
): ChartGeometry {
  const max = Math.max(1, ...series.map((item) => item.value));
  const n = series.length;
  const innerW = width - 2 * pad;
  const innerH = height - 2 * pad;
  const px = (index: number) =>
    n <= 1 ? width / 2 : pad + (innerW * index) / (n - 1);
  const py = (value: number) => height - pad - (innerH * value) / max;
  const points = series.map((item, index) => ({
    x: px(index),
    y: py(item.value),
    label: item.label,
    value: item.value,
  }));
  const line = points
    .map((point, index) => `${index ? "L" : "M"}${point.x.toFixed(1)},${point.y.toFixed(1)}`)
    .join(" ");
  const area = points.length
    ? `${line} L${points[points.length - 1].x.toFixed(1)},${(height - pad).toFixed(1)} L${points[0].x.toFixed(1)},${(height - pad).toFixed(1)} Z`
    : "";
  const axis = [0, 0.5, 1].map((ratio) => ({
    y: height - pad - innerH * ratio,
    value: Math.round(max * ratio),
  }));
  const tickCount = Math.min(8, n);
  const tickIndexes: number[] = [];
  for (let i = 0; i < tickCount; i++) {
    tickIndexes.push(Math.round((i * (n - 1)) / Math.max(1, tickCount - 1)));
  }
  const uniqueLabels = new Set<string>();
  const xTicks = tickIndexes
    .map((idx) => ({ label: series[idx]?.label ?? "", x: px(idx) }))
    .filter((t) => {
      if (uniqueLabels.has(t.label)) return false;
      uniqueLabels.add(t.label);
      return true;
    });
  return { line, area, points, axis, xTicks, max, n };
}

export const CHART_W = 720;
export const CHART_H = 200;
export const CHART_PAD = 8;

export interface ChartPoint {
  x: number;
  y: number;
  label: string;
  value: number;
}
export interface ChartGeometry {
  line: string;
  area: string;
  points: ChartPoint[];
  axis: { y: number; value: number }[];
  xTicks: { label: string; x: number }[];
  max: number;
  n: number;
}

export function buildChartGeometry(
  series: { label: string; value: number }[],
): ChartGeometry {
  const max = Math.max(1, ...series.map((item) => item.value));
  const n = series.length;
  const px = (index: number) =>
    n <= 1 ? CHART_W / 2 : CHART_PAD + ((CHART_W - 2 * CHART_PAD) * index) / (n - 1);
  const py = (value: number) => CHART_H - CHART_PAD - ((CHART_H - 2 * CHART_PAD) * value) / max;
  const points = series.map((item, index) => ({
    x: px(index),
    y: py(item.value),
    label: item.label,
    value: item.value,
  }));
  const line = points
    .map((point, index) => `${index ? "L" : "M"}${point.x.toFixed(1)},${point.y.toFixed(1)}`)
    .join(" ");
  const area = points.length
    ? `${line} L${points[points.length - 1].x.toFixed(1)},${(CHART_H - CHART_PAD).toFixed(1)} L${points[0].x.toFixed(1)},${(CHART_H - CHART_PAD).toFixed(1)} Z`
    : "";
  const axis = [0, 0.5, 1].map((ratio) => ({
    y: CHART_H - CHART_PAD - (CHART_H - 2 * CHART_PAD) * ratio,
    value: Math.round(max * ratio),
  }));
  const xTicks = [
    ...new Set(
      [series[0]?.label, series[Math.floor(n / 2)]?.label, series[n - 1]?.label].filter(
        Boolean,
      ) as string[],
    ),
  ].map((label) => {
    const target = series.findIndex((item) => item.label === label);
    const x = n <= 1 ? CHART_W / 2 : CHART_PAD + ((CHART_W - 2 * CHART_PAD) * target) / (n - 1);
    return { label, x };
  });
  return { line, area, points, axis, xTicks, max, n };
}

export const WEEKS_CAP = 53;

export interface HeatmapDay {
  date: string;
  tokens: number;
  level: number;
  isFuture: boolean;
}
export interface HeatmapData {
  weeks: { days: HeatmapDay[] }[];
  months: { label: string; span: number }[];
  startLabel: string;
  endLabel: string;
}

export function buildHeatmap(dailyMap: Map<string, DailyStat>, today = new Date()): HeatmapData {
  let start: Date;
  if (dailyMap.size) {
    start = parseLocal([...dailyMap.keys()].sort()[0]);
  } else {
    start = new Date(today);
    start.setDate(start.getDate() - 84);
  }
  start = startOfWeek(start);
  const maxTokens = Math.max(1, ...[...dailyMap.values()].map((item) => item.total));
  const levelOf = (tokens: number) => {
    if (tokens <= 0) return 0;
    const ratio = Math.log(tokens) / Math.log(maxTokens);
    return 1 + Math.min(3, Math.floor(ratio * 4));
  };
  const weeks: { days: HeatmapDay[] }[] = [];
  const cursor = new Date(start);
  while (cursor <= today && weeks.length < WEEKS_CAP) {
    const days: HeatmapDay[] = [];
    for (let index = 0; index < 7; index += 1) {
      const date = toLocalDate(cursor);
      const tokens = dailyMap.get(date)?.total ?? 0;
      days.push({
        date,
        tokens,
        level: levelOf(tokens),
        isFuture: cursor.getTime() > today.getTime(),
      });
      cursor.setDate(cursor.getDate() + 1);
    }
    weeks.push({ days });
  }
  const months: { label: string; span: number }[] = [];
  for (const week of weeks) {
    const label = week.days[0].date.slice(0, 7);
    const last = months[months.length - 1];
    if (last && last.label === label) {
      last.span += 1;
    } else {
      months.push({ label, span: 1 });
    }
  }
  return {
    weeks,
    months,
    startLabel: weeks[0]?.days[0].date ?? "",
    endLabel: toLocalDate(today),
  };
}

// —— 请求健康热力图：按天聚合大模型请求失败率，用层级着色 ——
export interface HealthHeatmapDay {
  date: string;
  requests: number;
  failed: number;
  rate: number | null; // 0~1，null 表示无数据
  level: number;       // 0=无数据 1=低失败 2=中 3=高 4=极高
  isFuture: boolean;
  outOfRange?: boolean; // 周网格补齐、不在顶部区间内
}
export interface HealthHeatmapData {
  weeks: { days: HealthHeatmapDay[] }[];
  months: { label: string; span: number }[];
  startLabel: string;
  endLabel: string;
  totalRequests: number;
  totalFailed: number;
  overallRate: number | null;
  rangeDays: number;   // 顶部区间内的日历天数（含无数据日）
  activeDays: number;  // 区间内有请求的天数
}

interface HealthInput {
  hour: string;
  success: number;
  failed: number;
}

export function buildHealthHeatmap(
  buckets: HealthInput[],
  from?: string,
  to?: string,
  today = new Date(),
): HealthHeatmapData {
  // 按本地日期聚合成功/失败
  const byDay = new Map<string, { requests: number; failed: number }>();
  for (const b of buckets) {
    const day = localDateOf(b.hour);
    if (!day) continue;
    const cur = byDay.get(day) || { requests: 0, failed: 0 };
    cur.requests += (b.success || 0) + (b.failed || 0);
    cur.failed += b.failed || 0;
    byDay.set(day, cur);
  }

  // 区间起止：优先顶部选择；否则用数据跨度；都没有时回退近 90 天
  let startDay = from || "";
  let endDay = to || "";
  if (!startDay || !endDay) {
    const days = [...byDay.keys()].sort();
    if (days.length) {
      startDay = startDay || days[0];
      endDay = endDay || days[days.length - 1];
    } else {
      const d = new Date(today);
      d.setDate(d.getDate() - 89);
      startDay = startDay || toLocalDate(d);
      endDay = endDay || toLocalDate(today);
    }
  }
  if (startDay > endDay) {
    const tmp = startDay;
    startDay = endDay;
    endDay = tmp;
  }

  // 周网格仅用于排版：从区间起点所在周一开始，到区间终点所在周日结束
  // 区间内无数据的日期仍会以空格子体现（level=0），不会被省略
  let start = startOfWeek(parseLocal(startDay));
  let end = startOfWeek(parseLocal(endDay));
  end.setDate(end.getDate() + 6);

  // 限制最多 WEEKS_CAP 周（保留最近）
  const maxSpan = (WEEKS_CAP - 1) * 7 * 86_400_000;
  if (end.getTime() - start.getTime() > maxSpan) {
    start = new Date(end.getTime() - maxSpan);
    start = startOfWeek(start);
  }

  const rateOf = (requests: number, failed: number) =>
    requests > 0 ? failed / requests : null;
  const levelOf = (rate: number | null) => {
    if (rate == null) return 0;
    if (rate <= 0.02) return 1;
    if (rate <= 0.08) return 2;
    if (rate <= 0.2) return 3;
    return 4;
  };

  const weeks: { days: HealthHeatmapDay[] }[] = [];
  let totalRequests = 0;
  let totalFailed = 0;
  let rangeDays = 0;
  let activeDays = 0;
  const cursor = new Date(start);
  while (cursor.getTime() <= end.getTime() && weeks.length < WEEKS_CAP) {
    const days: HealthHeatmapDay[] = [];
    for (let index = 0; index < 7; index += 1) {
      const date = toLocalDate(cursor);
      const inRange = date >= startDay && date <= endDay;
      const isFuture = cursor.getTime() > today.getTime();
      const agg = byDay.get(date);
      const requests = inRange ? (agg?.requests ?? 0) : 0;
      const failed = inRange ? (agg?.failed ?? 0) : 0;
      const rate = inRange ? rateOf(requests, failed) : null;
      if (inRange && !isFuture) {
        rangeDays += 1;
        totalRequests += requests;
        totalFailed += failed;
        if (requests > 0) activeDays += 1;
      }
      days.push({
        date,
        requests,
        failed,
        rate,
        // 区间内无数据：level=0 灰色空格子仍然显示；区间外补齐格 / 未来日：透明
        level: inRange && !isFuture ? levelOf(rate) : 0,
        isFuture,          // 真正的未来日
        outOfRange: !inRange, // 仅用于周网格补齐
      });
      cursor.setDate(cursor.getDate() + 1);
    }
    weeks.push({ days });
  }

  const months: { label: string; span: number }[] = [];
  for (const week of weeks) {
    const label = week.days[0].date.slice(0, 7);
    const last = months[months.length - 1];
    if (last && last.label === label) last.span += 1;
    else months.push({ label, span: 1 });
  }

  return {
    weeks,
    months,
    startLabel: startDay,
    endLabel: endDay,
    totalRequests,
    totalFailed,
    overallRate: rateOf(totalRequests, totalFailed),
    rangeDays,
    activeDays,
  };
}

// —— 过滤 unknown / 空模型来源 ——
export function isKnownModel(model?: string) {
  const value = (model || "").trim().toLowerCase();
  return value !== "" && value !== "unknown";
}
export function isKnownSource(source?: string) {
  const value = (source || "").trim().toLowerCase();
  return value !== "" && value !== "unknown";
}

// —— 小时用量桶聚合（tokentracker cursors.json）——
export interface UsageBucketLike {
  source: string;
  model: string;
  timestamp: string;
  totalTokens?: number;
  billableTotalTokens?: number;
  inputTokens?: number;
  cachedInputTokens?: number;
  cacheCreationInputTokens?: number;
  outputTokens?: number;
  reasoningOutputTokens?: number;
  conversationCount?: number;
}

export function buildDailyMapFromBuckets(buckets: UsageBucketLike[]): Map<string, DailyStat> {
  const map = new Map<string, DailyStat>();
  for (const bucket of buckets) {
    const date = localDateOf(bucket.timestamp);
    if (!date) continue;
    const current = map.get(date) || {
      date,
      total: 0,
      input: 0,
      output: 0,
      cache: 0,
      reasoning: 0,
      sessions: 0,
    };
    current.total += bucket.totalTokens || 0;
    current.input += bucket.inputTokens || 0;
    current.output += bucket.outputTokens || 0;
    current.cache +=
      (bucket.cachedInputTokens || 0) + (bucket.cacheCreationInputTokens || 0);
    current.reasoning += bucket.reasoningOutputTokens || 0;
    current.sessions += bucket.conversationCount || 0;
    map.set(date, current);
  }
  return map;
}

export function bucketTotals(buckets: UsageBucketLike[]) {
  const result = { total: 0, billable: 0, conversations: 0 };
  for (const bucket of buckets) {
    result.total += bucket.totalTokens || 0;
    result.billable += bucket.billableTotalTokens || 0;
    result.conversations += bucket.conversationCount || 0;
  }
  return result;
}

export function bucketSourceTotals(buckets: UsageBucketLike[]) {
  const groups = new Map<string, { source: string; totalTokens: number; conversations: number }>();
  for (const bucket of buckets) {
    const current = groups.get(bucket.source) || { source: bucket.source, totalTokens: 0, conversations: 0 };
    current.totalTokens += bucket.totalTokens || 0;
    current.conversations += bucket.conversationCount || 0;
    groups.set(bucket.source, current);
  }
  return [...groups.values()].sort((a, b) => b.totalTokens - a.totalTokens);
}

// 归一化模型名：去掉斜杠前的厂家前缀（如 "anthropic/claude-sonnet-4" → "claude-sonnet-4"），保留完整模型名
export function normalizeModelName(name: string): string {
  const trimmed = (name || "").trim();
  if (!trimmed) return trimmed;
  const slash = trimmed.indexOf("/");
  if (slash > 0) return trimmed.slice(slash + 1).trim();
  return trimmed;
}

export interface ModelTotal {
  model: string;
  totalTokens: number;
  conversations: number;
}

// 剥离 "-small" / "-code" 等后缀：返回 [基础名, 是否命中]。
// 例：claude-sonnet-4-small → ["claude-sonnet-4", true]；claude-sonnet-4 → 原样
// 剥离尾部版本/时间戳后缀（可多次剥直到稳定）：
// - 词缀：-small / -code / -free
// - 日期：-YYYYMMDD / -YYMMDD / -MMDD（如 -20250514、-260731、-0731）
// - 版本：-ga-YYYYMMDD（如 -ga-260731）
function stripSuffix(name: string): { base: string; changed: boolean } {
  let base = name;
  let changed = false;
  for (let pass = 0; pass < 6; pass++) {
    const prev = base;
    // 词缀
    base = base.replace(/-(?:small|code|free)$/i, "");
    // ga + 日期
    base = base.replace(/-ga-(?:\d{8}|\d{6})$/i, "");
    // 纯数字日期（8 位 YYYYMMDD / 6 位 YYMMDD / 4 位 MMDD）
    base = base.replace(/-(?:\d{8}|\d{6}|\d{4})$/i, "");
    if (base !== prev) {
      changed = true;
    } else {
      break;
    }
    if (base === name) break;
  }
  // 避免剥到变成裸字母（如只剩 "gpt"），至少保留 1 个 `-` 片段
  if (!base.includes("-")) {
    return { base: name, changed };
  }
  return { base, changed };
}

// 合并模型统计：规则——
// 1) 归一化去厂家（保留完整模型名）
// 2) 剥离 -small / -code 后缀归并到对应分类
// 3) 不做"长串往短串"的子串合并
export function mergeModelTotals(items: ModelTotal[]): ModelTotal[] {
  // 第 1 步：归一化（去斜杠厂家）后按名聚合
  const byName = new Map<string, ModelTotal>();
  for (const item of items) {
    const name = normalizeModelName(item.model) || item.model;
    const current = byName.get(name) || { model: name, totalTokens: 0, conversations: 0 };
    current.totalTokens += item.totalTokens;
    current.conversations += item.conversations;
    byName.set(name, current);
  }

  const list = [...byName.values()];

  // 第 2 步：剥离 -small / -code 后缀，合并到对应基础分类
  const result = new Map<string, ModelTotal>();
  for (const item of list) {
    const { base, changed } = stripSuffix(item.model);
    const targetName = changed ? base : item.model;
    const target = result.get(targetName) || { model: targetName, totalTokens: 0, conversations: 0 };
    target.totalTokens += item.totalTokens;
    target.conversations += item.conversations;
    result.set(targetName, target);
  }

  return [...result.values()].sort((a, b) => b.totalTokens - a.totalTokens);
}

export function bucketModelTotals(buckets: UsageBucketLike[]) {
  const groups = new Map<string, { model: string; totalTokens: number; conversations: number }>();
  for (const bucket of buckets) {
    const current = groups.get(bucket.model) || { model: bucket.model, totalTokens: 0, conversations: 0 };
    current.totalTokens += bucket.totalTokens || 0;
    current.conversations += bucket.conversationCount || 0;
    groups.set(bucket.model, current);
  }
  return [...groups.values()].sort((a, b) => b.totalTokens - a.totalTokens);
}

// —— 数字格式化 ——
export function formatTokens(value?: number | null) {
  const amount = Number(value ?? 0);
  if (!Number.isFinite(amount)) return "0";
  return new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 0 }).format(Math.round(amount));
}

export function formatCompact(value?: number | null) {
  const amount = Number(value ?? 0);
  if (!Number.isFinite(amount) || amount <= 0) return "0";
  if (amount < 1000) return String(Math.round(amount));
  if (amount < 1_000_000) {
    return `${(amount / 1000).toFixed(amount < 10_000 ? 1 : 0).replace(/\.0$/, "")}k`;
  }
  if (amount < 1_000_000_000) {
    return `${(amount / 1_000_000).toFixed(amount < 10_000_000 ? 1 : 0).replace(/\.0$/, "")}M`;
  }
  return `${(amount / 1_000_000_000).toFixed(1).replace(/\.0$/, "")}B`;
}

export function formatCost(value?: number | null) {
  const amount = Number(value ?? 0);
  if (!Number.isFinite(amount)) return "$0.00";
  return `$${amount.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
}

export function formatRate(value?: number | null) {
  if (value == null) return "—";
  return `${Math.round(Number(value) * 100)}%`;
}
