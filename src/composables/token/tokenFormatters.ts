// 格式化与基础清洗工具函数

export function isKnownModel(model?: string) {
  if (!model) return false;
  return !model.toLowerCase().includes("unknown");
}

export function isKnownSource(source?: string) {
  if (!source) return false;
  return !source.toLowerCase().includes("unknown");
}

export function normalizeModelName(name: string): string {
  const trimmed = (name || "").trim();
  if (!trimmed) return trimmed;
  const slash = trimmed.indexOf("/");
  if (slash > 0) return trimmed.slice(slash + 1).trim();
  return trimmed;
}

export function formatTokens(value?: number | null) {
  if (value == null) return "—";
  const num = Number(value);
  if (!Number.isFinite(num)) return "—";
  return num.toLocaleString();
}

export function formatCompact(value?: number | null) {
  if (value == null) return "—";
  const num = Number(value);
  if (!Number.isFinite(num)) return "—";
  if (num === 0) return "0";
  if (Math.abs(num) >= 1_000_000_000) {
    return `${(num / 1_000_000_000).toFixed(1).replace(/\.0$/, "")}B`;
  }
  if (Math.abs(num) >= 1_000_000) {
    return `${(num / 1_000_000).toFixed(1).replace(/\.0$/, "")}M`;
  }
  if (Math.abs(num) >= 1_000) {
    return `${(num / 1_000).toFixed(1).replace(/\.0$/, "")}k`;
  }
  return num.toLocaleString();
}

export function formatCost(value?: number | null) {
  if (value == null) return "—";
  const num = Number(value);
  if (!Number.isFinite(num)) return "—";
  return `$${num.toFixed(2)}`;
}

// 缓存命中率等比率显示：保留小数点后 2 位（如 45.67%、3.45%、0.46%、100.00%）。
export function formatRate(value?: number | null) {
  if (value == null) return "—";
  const pct = Number(value) * 100;
  if (!Number.isFinite(pct)) return "—";
  return `${pct.toFixed(2)}%`;
}

// 缓存命中率：缓存读取 token 占（缓存读取 + 缓存写入 + 全新输入）的比例。
// freshInput 是各采集器已归一化的全新输入（Fresh Input）。
export function cacheHitRateOf(
  cacheRead: number,
  cacheWrite: number,
  freshInput: number,
  estimatedInput = 0,
): number | null {
  const read = Math.max(0, cacheRead || 0);
  const write = Math.max(0, cacheWrite || 0);
  const fresh = Math.max(0, freshInput || 0);
  const total = read + write + fresh;
  if (total <= 0) return null;
  if (read + write === 0 && estimatedInput >= fresh) return null;
  const rate = read / total;
  return Math.min(1, Math.max(0, rate));
}

/**
 * 请求健康色阶（以失败率为主，绝对失败数为辅）：
 * 0 无活动
 * 1 红  严重（成功率 < 70%，或只有失败）
 * 2 橙  较差（70% ~ 85%）
 * 3 黄  亚健康（85% ~ 95%）
 * 4 浅绿 轻微异常（95% ~ 99%，或成功率很高但仍有失败）
 * 5 绿  健康（≥ 99%，且失败可忽略）
 */
export function healthLevelOf(
  successRate: number | null,
  hasActivity = true,
  failed = 0,
  requests = 0,
): number {
  const failedCount = Math.max(0, Number(failed || 0));
  const requestCount = Math.max(0, Number(requests || 0));
  const active = hasActivity || requestCount > 0 || failedCount > 0;

  if (!active) return 0;

  if (requestCount <= 0 && failedCount > 0) return 1;
  if (requestCount <= 0) return failedCount > 0 ? 1 : 0;

  let rate =
    successRate == null
      ? Math.max(0, Math.min(1, (requestCount - Math.min(failedCount, requestCount)) / requestCount))
      : Math.max(0, Math.min(1, successRate));

  if (failedCount <= 0) return 5;

  if (rate < 0.7) return 1;
  if (rate < 0.85) return 2;
  if (rate < 0.95) return 3;
  if (rate < 0.99) return 4;

  if (failedCount >= 20) return 4;
  return 5;
}
