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
/** 从某个节点向前回溯 count 个节点（不含自身），用于网格前置自动补全 */
export function buildPrecedingKeys(
  endKeyOrLabel: string,
  count: number,
  granularity: TrendGranularity,
): { key: string; label: string }[] {
  if (count <= 0 || !endKeyOrLabel) return [];
  const out: { key: string; label: string }[] = [];

  if (granularity === "hour") {
    // endKey: YYYY-MM-DD-HH  or label YYYY-MM-DD HH:00
    let day = "";
    let hh = 0;
    const m1 = endKeyOrLabel.match(/^(\d{4}-\d{2}-\d{2})-(\d{2})$/);
    const m2 = endKeyOrLabel.match(/^(\d{4}-\d{2}-\d{2})[ T](\d{2})(?::\d{2})?$/);
    if (m1) { day = m1[1]; hh = Number(m1[2]); }
    else if (m2) { day = m2[1]; hh = Number(m2[2]); }
    else return [];
    const cursor = parseLocal(day);
    cursor.setHours(hh, 0, 0, 0);
    for (let i = 0; i < count; i++) {
      cursor.setHours(cursor.getHours() - 1);
      const d = toLocalDate(cursor);
      const h = String(cursor.getHours()).padStart(2, "0");
      out.push({ key: `${d}-${h}`, label: `${d} ${h}:00` });
    }
    return out.reverse();
  }

  if (granularity === "month") {
    // YYYY-MM
    const m = endKeyOrLabel.match(/^(\d{4})-(\d{2})/);
    if (!m) return [];
    let y = Number(m[1]);
    let mon = Number(m[2]);
    for (let i = 0; i < count; i++) {
      mon -= 1;
      if (mon < 1) { mon = 12; y -= 1; }
      const label = `${y}-${String(mon).padStart(2, "0")}`;
      out.push({ key: label, label });
    }
    return out.reverse();
  }

  // day: YYYY-MM-DD
  const day = endKeyOrLabel.slice(0, 10);
  if (!/^\d{4}-\d{2}-\d{2}$/.test(day)) return [];
  const cursor = parseLocal(day);
  for (let i = 0; i < count; i++) {
    cursor.setDate(cursor.getDate() - 1);
    const d = toLocalDate(cursor);
    out.push({ key: d, label: d });
  }
  return out.reverse();
}

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

// —— 请求健康时间线：按所选区间完整节点展示成功/失败分布 ——
export interface HealthTimelineCell {
  key: string;
  label: string;
  dialogues: number;         // 用户发起 turns
  success: number;
  failed: number;
  requests: number;          // 展示用请求量：优先后端提取，其次 usage 估算
  usageRequests: number;     // token usage 侧估算请求量（仅兜底）
  successRate: number | null; // 0~1，null=无健康样本
  level: number; // 0=无请求 1=很差 2=差 3=中 4=好 5=很好/默认健康
}

export interface HealthTimelineData {
  cells: HealthTimelineCell[];
  startLabel: string;
  endLabel: string;
  totalDialogues: number;
  totalSuccess: number;
  totalFailed: number;
  totalRequests: number;
  successRate: number | null;
  nodeCount: number;
  activeCount: number;
  /** 是否有后端真实提取数据（有则 KPI/时间线不再被 usage 估算主导） */
  hasExtracted: boolean;
}

interface HealthInput {
  hour: string;
  dialogues?: number; // 用户发起 turns
  requests?: number; // 后端提取的真实 API 请求数
  success: number;
  failed: number;
}

/** 成功率 → 色阶：0 无请求；1 红(不健康) … 5 绿(健康) */
export function healthLevelOf(successRate: number | null, hasRequests = true): number {
  if (!hasRequests) return 0;
  // 有请求但没有失败样本时，按健康展示（避免大量误报“无数据”）
  if (successRate == null) return 5;
  if (successRate < 0.5) return 1;
  if (successRate < 0.7) return 2;
  if (successRate < 0.85) return 3;
  if (successRate < 0.95) return 4;
  return 5;
}

/**
 * 估算 API 请求次数。
 * tokentracker hourly.buckets 只有 conversation_count，没有 request_count；
 * 用 output/reasoning 规模把“对话轮”放大成更接近真实请求量的估计值。
 */
export function estimateRequestCount(input: {
  conversationCount?: number;
  outputTokens?: number;
  reasoningOutputTokens?: number;
  totalTokens?: number;
}): number {
  const conversations = Math.max(0, Number(input.conversationCount || 0));
  if (conversations <= 0) {
    // 没有 conversation 时，用输出规模兜底（极少见）
    const out = Math.max(0, Number(input.outputTokens || 0) + Number(input.reasoningOutputTokens || 0));
    return out > 0 ? Math.max(1, Math.round(out / 800)) : 0;
  }
  const out = Math.max(0, Number(input.outputTokens || 0) + Number(input.reasoningOutputTokens || 0));
  // 经验：每次请求大约 600~1200 output tokens；按 800 估算每轮对话对应请求数
  const byOutput = out > 0 ? Math.round(out / 800) : 0;
  // 至少不少于对话数，避免低估；同时给一个温和上限，避免极端值爆炸
  const est = Math.max(conversations, byOutput);
  return Math.min(est, conversations * 12);
}

export interface HealthUsageInput {
  timestamp: string;
  conversationCount?: number;
  outputTokens?: number;
  reasoningOutputTokens?: number;
  totalTokens?: number;
  requests?: number; // 若外部已估算可直接传
}

/**
 * 按趋势粒度生成与左侧图表一一对应的完整节点。
 * - 请求量优先取后端多工具提取（Codex/Claude/OpenCode/Mimo/Zcode/Antigravity）
 * - 仅当完全没有提取数据时，才用 token usage 估算兜底
 * - 对话数取后端 dialogues（用户发起 turns）
 * - 成功率只在有 health 样本时计算；有请求但无样本 => 视为健康
 */
export function buildHealthTimeline(
  buckets: HealthInput[],
  granularity: TrendGranularity,
  from?: string,
  to?: string,
  usageBuckets: HealthUsageInput[] = [],
): HealthTimelineData {
  // health: extracted dialogues/requests + success/failed samples
  const healthMap = new Map<string, { label: string; dialogues: number; requests: number; success: number; failed: number }>();
  let extractedTotal = 0;
  for (const b of buckets) {
    const { key, label } = bucketKeyFor(granularity, b.hour);
    if (!key) continue;
    const cur = healthMap.get(key) || { label, dialogues: 0, requests: 0, success: 0, failed: 0 };
    cur.dialogues += Number(b.dialogues || 0);
    cur.requests += Number(b.requests || 0);
    cur.success += b.success || 0;
    cur.failed += b.failed || 0;
    healthMap.set(key, cur);
    extractedTotal += Number(b.requests || 0) + Number(b.dialogues || 0);
  }
  const hasExtracted = extractedTotal > 0;

  // usage activity: estimated request count from token usage
  const usageMap = new Map<string, number>();
  for (const b of usageBuckets) {
    const { key } = bucketKeyFor(granularity, b.timestamp);
    if (!key) continue;
    const req = b.requests != null
      ? Number(b.requests || 0)
      : estimateRequestCount(b);
    usageMap.set(key, (usageMap.get(key) || 0) + req);
  }

  // 区间：优先顶部选择；否则取 usage/health 并集跨度
  let startDay = from || "";
  let endDay = to || "";
  if (!startDay || !endDay) {
    let min = "";
    let max = "";
    const consider = (iso: string) => {
      const day = localDateOf(iso);
      if (!day) return;
      if (!min || day < min) min = day;
      if (!max || day > max) max = day;
    };
    for (const b of usageBuckets) consider(b.timestamp);
    for (const b of buckets) consider(b.hour);
    if (min && max) {
      startDay = startDay || min;
      endDay = endDay || max;
    }
  }
  if (!startDay || !endDay) {
    return {
      cells: [],
      startLabel: "",
      endLabel: "",
      totalDialogues: 0,
      totalSuccess: 0,
      totalFailed: 0,
      totalRequests: 0,
      successRate: null,
      nodeCount: 0,
      activeCount: 0,
      hasExtracted: false,
    };
  }
  if (startDay > endDay) {
    const tmp = startDay;
    startDay = endDay;
    endDay = tmp;
  }

  const keys = buildRangeKeys(startDay, endDay, granularity);
  let totalDialogues = 0;
  let totalSuccess = 0;
  let totalFailed = 0;
  let totalRequests = 0;
  let activeCount = 0;
  const cells: HealthTimelineCell[] = keys.map(({ key, label }) => {
    const health = healthMap.get(key);
    const dialogues = health?.dialogues ?? 0;
    const success = health?.success ?? 0;
    const failed = health?.failed ?? 0;
    const extractedRequests = health?.requests ?? 0;
    const sampleRequests = success + failed;
    const usageRequests = usageMap.get(key) || 0;
    // 展示请求量优先级：
    // 1) 后端多工具提取的真实 API 请求数
    // 2) 仅当完全没有提取数据时，才用 usage 估算兜底
    // 3) 成功失败样本合计
    const requests = extractedRequests > 0
      ? extractedRequests
      : (!hasExtracted && usageRequests > 0 ? usageRequests : sampleRequests);
    const successRate = sampleRequests > 0 ? success / sampleRequests : null;
    if (requests > 0) activeCount += 1;
    totalDialogues += dialogues;
    totalSuccess += success;
    totalFailed += failed;
    totalRequests += requests;
    return {
      key,
      label,
      dialogues,
      success,
      failed,
      requests,
      usageRequests,
      successRate,
      level: healthLevelOf(successRate, requests > 0),
    };
  });

  const healthTotal = totalSuccess + totalFailed;
  return {
    cells,
    startLabel: startDay,
    endLabel: endDay,
    totalDialogues,
    totalSuccess,
    totalFailed,
    totalRequests,
    successRate: healthTotal > 0 ? totalSuccess / healthTotal : null,
    nodeCount: cells.length,
    activeCount,
    hasExtracted,
  };
}

// 兼容旧名（若仍有引用）
export type HealthHeatmapDay = HealthTimelineCell;
export type HealthHeatmapData = HealthTimelineData;
export function buildHealthHeatmap(
  buckets: HealthInput[],
  from?: string,
  to?: string,
  _today = new Date(),
): HealthTimelineData {
  // 旧签名无粒度：默认按日
  return buildHealthTimeline(buckets, "day", from, to);
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
