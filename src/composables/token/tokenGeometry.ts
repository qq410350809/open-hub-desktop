import type { ChartGeometry, HeatmapData, HeatmapDay, DailyStat, HealthTimelineData, HealthInput } from "./types";
import { parseLocal, startOfWeek, toLocalDate } from "./tokenTrendAgg";
import { buildHealthTimeline } from "./tokenBreakdownAgg";

export const CHART_W = 720;
export const CHART_H = 200;
export const CHART_PAD = 8;
export const WEEKS_CAP = 53;

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

// 兼容旧名（若仍有引用）
export type HealthHeatmapDay = import("./types").HealthTimelineCell;
export type HealthHeatmapData = HealthTimelineData;

export function buildHealthHeatmap(
  buckets: HealthInput[],
  from?: string,
  to?: string,
  _today = new Date(),
): HealthTimelineData {
  return buildHealthTimeline(buckets, "day", from, to);
}
