import type {
  BucketBreakdownTotal,
  HealthInput,
  HealthTimelineCell,
  HealthTimelineData,
  HealthUsageInput,
  ModelTotal,
  SourceTotal,
  TrendGranularity,
  UsageBucketLike,
} from "./types";
import { cacheHitRateOf, healthLevelOf, normalizeModelName } from "./tokenFormatters";
import { bucketKeyFor, buildRangeKeys, localDateOf } from "./tokenTrendAgg";

export function bucketTotals(buckets: UsageBucketLike[]) {
  const result = { total: 0, billable: 0, conversations: 0 };
  for (const bucket of buckets) {
    result.total += bucket.totalTokens || 0;
    result.billable += (bucket as any).billableTotalTokens || bucket.totalTokens || 0;
    result.conversations += bucket.conversationCount || 0;
  }
  return result;
}

export type { BucketBreakdownTotal };

/**
 * 查表键：复刻后端 raw_key（小写、取最后一段 "/" 前缀），
 * 保证前端展示归组与 token_model_mappings 表的主键一致。
 */
export function modelRawKey(name: string): string {
  const trimmed = (name || "").trim();
  if (!trimmed) return "";
  const tail = trimmed.split("/").pop() ?? trimmed;
  return tail.trim().toLowerCase();
}

/** raw_key → 正式模型名；未命中的原始名回退现有 normalize + stripSuffix 归组。 */
export type ModelMappingLookup = Map<string, string>;

/** 由映射表构建归组查表（只收已确定正式名的行）。 */
export function buildModelMappingLookup(
  mappings: { rawKey?: string; rawModel?: string; officialModel?: string; reviewStatus?: string; confirmed?: boolean }[] | null | undefined,
): ModelMappingLookup | undefined {
  if (!mappings?.length) return undefined;
  const lookup: ModelMappingLookup = new Map();
  for (const item of mappings) {
    const official = (item.officialModel || "").trim();
    // AI 建议尚未经过人工审核，不能改变统计归组口径。
    if (!official || (item.reviewStatus && item.reviewStatus !== "approved") || (!item.reviewStatus && !item.confirmed)) continue;
    const key = item.rawKey?.trim() || modelRawKey(item.rawModel || "");
    if (key) lookup.set(key, official);
  }
  return lookup.size ? lookup : undefined;
}

function emptyBucketBreakdown(): BucketBreakdownTotal {
  return {
    totalTokens: 0,
    inputTokens: 0,
    outputTokens: 0,
    cacheTokens: 0,
    cacheReadTokens: 0,
    cacheWriteTokens: 0,
    cacheHitRate: null,
    reasoningTokens: 0,
    conversations: 0,
    requests: 0,
    requestsEstimated: false,
    costUsd: 0,
    estimatedTokens: 0,
    estimatedInputTokens: 0,
  };
}

function addBucketBreakdown(target: BucketBreakdownTotal, bucket: UsageBucketLike) {
  target.totalTokens += bucket.totalTokens || 0;
  target.inputTokens += bucket.inputTokens || 0;
  target.outputTokens += bucket.outputTokens || 0;
  target.cacheTokens += (bucket.cachedInputTokens || 0) + (bucket.cacheCreationInputTokens || 0);
  target.cacheReadTokens += bucket.cachedInputTokens || 0;
  target.cacheWriteTokens += bucket.cacheCreationInputTokens || 0;
  target.reasoningTokens += bucket.reasoningOutputTokens || 0;
  target.conversations += bucket.conversationCount || 0;
  if (bucket.requestCount != null) {
    target.requests += bucket.requestCount || 0;
  } else {
    target.requests += estimateRequestCount({
      conversationCount: bucket.conversationCount,
      outputTokens: bucket.outputTokens,
      reasoningOutputTokens: bucket.reasoningOutputTokens,
      totalTokens: bucket.totalTokens,
    });
    target.requestsEstimated = true;
  }
  target.costUsd += bucket.costUsd || 0;
  target.estimatedTokens += bucket.estimatedTokens || 0;
  target.estimatedInputTokens += bucket.estimatedInputTokens || 0;
}

export function estimateRequestCount(input: {
  conversationCount?: number;
  outputTokens?: number;
  reasoningOutputTokens?: number;
  totalTokens?: number;
}): number {
  const conversations = Math.max(0, Number(input.conversationCount || 0));
  if (conversations <= 0) {
    const out = Math.max(0, Number(input.outputTokens || 0) + Number(input.reasoningOutputTokens || 0));
    return out > 0 ? Math.max(1, Math.round(out / 800)) : 0;
  }
  const out = Math.max(0, Number(input.outputTokens || 0) + Number(input.reasoningOutputTokens || 0));
  const byOutput = out > 0 ? Math.round(out / 800) : 0;
  const est = Math.max(conversations, byOutput);
  return Math.min(est, conversations * 12);
}

export function bucketSourceTotals(buckets: UsageBucketLike[]): SourceTotal[] {
  const groups = new Map<string, SourceTotal>();
  for (const bucket of buckets) {
    const sourceKey = bucket.source || "unknown";
    const current = groups.get(sourceKey) || { source: sourceKey, ...emptyBucketBreakdown() };
    addBucketBreakdown(current, bucket);
    groups.set(sourceKey, current);
  }
  return [...groups.values()]
    .map((item) => ({
      ...item,
      cacheHitRate: cacheHitRateOf(
        item.cacheReadTokens,
        item.cacheWriteTokens,
        item.inputTokens,
        item.estimatedInputTokens,
      ),
    }))
    .sort((a, b) => b.totalTokens - a.totalTokens);
}

function stripSuffix(name: string): { base: string; changed: boolean } {
  let base = name;
  let changed = false;
  for (let pass = 0; pass < 6; pass++) {
    const prev = base;
    base = base.replace(/-(?:small|code|free)$/i, "");
    base = base.replace(/-ga-(?:\d{8}|\d{6})$/i, "");
    base = base.replace(/-(?:\d{8}|\d{6}|\d{4})$/i, "");
    if (base !== prev) {
      changed = true;
    } else {
      break;
    }
    if (base === name) break;
  }
  if (!base.includes("-")) {
    return { base: name, changed };
  }
  return { base, changed };
}

export function mergeModelTotals(items: ModelTotal[], mapping?: ModelMappingLookup): ModelTotal[] {
  // 映射表命中的名字是目录正式名，不能再被 stripSuffix 二次削尾。
  const mappedNames = new Set<string>();
  const byName = new Map<string, ModelTotal>();
  for (const item of items) {
    let name: string;
    const official = mapping?.get(modelRawKey(item.model)) || "";
    if (official) {
      name = official;
      mappedNames.add(name);
    } else {
      name = normalizeModelName(item.model) || item.model;
    }
    const current = byName.get(name) || { model: name, ...emptyBucketBreakdown() };
    current.totalTokens += item.totalTokens;
    current.inputTokens += item.inputTokens;
    current.outputTokens += item.outputTokens;
    current.cacheTokens += item.cacheTokens;
    current.cacheReadTokens += item.cacheReadTokens;
    current.cacheWriteTokens += item.cacheWriteTokens;
    current.reasoningTokens += item.reasoningTokens;
    current.conversations += item.conversations;
    current.requests += item.requests;
    current.requestsEstimated = current.requestsEstimated || item.requestsEstimated;
    current.costUsd += item.costUsd;
    current.estimatedTokens += item.estimatedTokens;
    current.estimatedInputTokens += item.estimatedInputTokens;
    byName.set(name, current);
  }

  const list = [...byName.values()];
  const result = new Map<string, ModelTotal>();
  for (const item of list) {
    let targetName: string;
    if (mappedNames.has(item.model)) {
      targetName = item.model;
    } else {
      const { base, changed } = stripSuffix(item.model);
      targetName = changed ? base : item.model;
    }
    const target = result.get(targetName) || { model: targetName, ...emptyBucketBreakdown() };
    target.totalTokens += item.totalTokens;
    target.inputTokens += item.inputTokens;
    target.outputTokens += item.outputTokens;
    target.cacheTokens += item.cacheTokens;
    target.cacheReadTokens += item.cacheReadTokens;
    target.cacheWriteTokens += item.cacheWriteTokens;
    target.reasoningTokens += item.reasoningTokens;
    target.conversations += item.conversations;
    target.requests += item.requests;
    target.requestsEstimated = target.requestsEstimated || item.requestsEstimated;
    target.costUsd += item.costUsd;
    target.estimatedTokens += item.estimatedTokens;
    target.estimatedInputTokens += item.estimatedInputTokens;
    result.set(targetName, target);
  }

  return [...result.values()]
    .map((item) => ({
      ...item,
      cacheHitRate: cacheHitRateOf(
        item.cacheReadTokens,
        item.cacheWriteTokens,
        item.inputTokens,
        item.estimatedInputTokens,
      ),
    }))
    .sort((a, b) => b.totalTokens - a.totalTokens);
}

export function bucketModelTotals(buckets: UsageBucketLike[]): ModelTotal[] {
  const groups = new Map<string, ModelTotal>();
  for (const bucket of buckets) {
    const modelKey = bucket.model || "unknown";
    const current = groups.get(modelKey) || { model: modelKey, ...emptyBucketBreakdown() };
    addBucketBreakdown(current, bucket);
    groups.set(modelKey, current);
  }
  return [...groups.values()]
    .map((item) => ({
      ...item,
      cacheHitRate: cacheHitRateOf(
        item.cacheReadTokens,
        item.cacheWriteTokens,
        item.inputTokens,
        item.estimatedInputTokens,
      ),
    }))
    .sort((a, b) => b.totalTokens - a.totalTokens);
}

export function buildHealthTimeline(
  buckets: HealthInput[],
  granularity: TrendGranularity,
  from?: string,
  to?: string,
  usageBuckets: HealthUsageInput[] = [],
): HealthTimelineData {
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

  const usageMap = new Map<string, { count: number; estimated: boolean }>();
  for (const b of usageBuckets) {
    const { key } = bucketKeyFor(granularity, b.timestamp);
    if (!key) continue;
    const current = usageMap.get(key) || { count: 0, estimated: false };
    if (b.requests != null) {
      current.count += Number(b.requests || 0);
    } else {
      current.count += estimateRequestCount(b);
      current.estimated = true;
    }
    usageMap.set(key, current);
  }

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
    const rawSuccess = health?.success ?? 0;
    const rawFailed = health?.failed ?? 0;
    const extractedRequests = health?.requests ?? 0;
    const sampleRequests = rawSuccess + rawFailed;
    const usage = usageMap.get(key);
    const usageRequests = usage?.count ?? 0;
    let requests = extractedRequests;
    let requestsEstimated = false;
    if (requests <= 0) {
      if (usageRequests > 0) {
        requests = usageRequests;
        requestsEstimated = usage?.estimated ?? false;
      } else {
        requests = sampleRequests;
      }
    }

    const failed = Math.max(0, rawFailed);
    const success = requests > 0
      ? Math.max(0, requests - Math.min(failed, requests))
      : rawSuccess;
    const successRate = requests > 0
      ? success / requests
      : (failed > 0 ? 0 : (sampleRequests > 0 ? rawSuccess / sampleRequests : null));

    if (requests > 0 || failed > 0) activeCount += 1;
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
      requestsEstimated,
      successRate,
      level: healthLevelOf(successRate, requests > 0 || failed > 0, failed, requests),
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
