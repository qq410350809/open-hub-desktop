import type {
  DailyStat,
  TokenSessionLike,
  TrendDetailItem,
  TrendGranularity,
  UsageBucketLike,
} from "./types";
import { cacheHitRateOf } from "./tokenFormatters";

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

export function buildPrecedingKeys(
  endKeyOrLabel: string,
  count: number,
  granularity: TrendGranularity,
): { key: string; label: string }[] {
  if (count <= 0 || !endKeyOrLabel) return [];
  const out: { key: string; label: string }[] = [];

  if (granularity === "hour") {
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
    let [y1, m1] = from.slice(0, 7).split("-").map(Number);
    const [y2, m2] = to.slice(0, 7).split("-").map(Number);
    if (!y1 || !m1 || !y2 || !m2) return [];
    while (y1 < y2 || (y1 === y2 && m1 <= m2)) {
      const label = `${y1}-${String(m1).padStart(2, "0")}`;
      keys.push({ key: label, label });
      m1 += 1;
      if (m1 > 12) {
        m1 = 1;
        y1 += 1;
      }
    }
    return keys;
  }
  const cursor = parseLocal(from);
  const end = parseLocal(to);
  while (cursor.getTime() <= end.getTime()) {
    const day = toLocalDate(cursor);
    keys.push({ key: day, label: day });
    cursor.setDate(cursor.getDate() + 1);
  }
  return keys;
}

export function resolveTrendSpan(
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
      key,
      label,
      total: 0, input: 0, output: 0, cache: 0, cacheRead: 0, cacheWrite: 0, cacheHitRate: null,
      reasoning: 0, sessions: 0, estimatedInput: 0,
    };
    current.total += bucket.totalTokens || 0;
    current.input += bucket.inputTokens || 0;
    current.output += bucket.outputTokens || 0;
    current.cacheRead += bucket.cachedInputTokens || 0;
    current.cacheWrite += bucket.cacheCreationInputTokens || 0;
    current.cache += (bucket.cachedInputTokens || 0) + (bucket.cacheCreationInputTokens || 0);
    current.reasoning += bucket.reasoningOutputTokens || 0;
    current.sessions += bucket.conversationCount || 0;
    current.estimatedInput += bucket.estimatedInputTokens || 0;
    map.set(key, current);
  }
  const span = resolveTrendSpan(buckets, from, to);
  if (!span) return [];
  return buildRangeKeys(span.from, span.to, granularity).map(({ key, label }) => {
    const current = map.get(key);
    const item = current || {
      key,
      label,
      total: 0, input: 0, output: 0, cache: 0, cacheRead: 0, cacheWrite: 0, cacheHitRate: null,
      reasoning: 0, sessions: 0, estimatedInput: 0,
    };
    item.cacheHitRate = cacheHitRateOf(item.cacheRead, item.cacheWrite, item.input, item.estimatedInput);
    return item;
  });
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
