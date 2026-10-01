import type { SiteModelHealth } from "./types";

export function escapeHtml(value: unknown): string {
  return String(value ?? "").replace(
    /[&<>'"]/g,
    (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "'": "&#39;", '"': "&quot;" })[character]!,
  );
}

export function parseTimestampToDate(value: string | number | Date | null | undefined): Date | null {
  if (value === null || value === undefined || value === "") return null;
  if (value instanceof Date) {
    return Number.isNaN(value.getTime()) ? null : value;
  }
  if (typeof value === "number") {
    const ms = value < 100_000_000_000 ? value * 1000 : value;
    const d = new Date(ms);
    return Number.isNaN(d.getTime()) ? null : d;
  }
  const str = String(value).trim();
  if (/^\d{9,11}$/.test(str)) {
    const d = new Date(Number(str) * 1000);
    return Number.isNaN(d.getTime()) ? null : d;
  }
  if (/^\d{12,14}$/.test(str)) {
    const d = new Date(Number(str));
    return Number.isNaN(d.getTime()) ? null : d;
  }
  if (/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/.test(str)) {
    const d = new Date(str.replace(" ", "T"));
    return Number.isNaN(d.getTime()) ? null : d;
  }
  const d = new Date(str);
  return Number.isNaN(d.getTime()) ? null : d;
}

export function formatDate(value: string | number | Date | null | undefined): string {
  if (!value) return "未知";
  const d = parseTimestampToDate(value);
  if (!d) return String(value);
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  const h = String(d.getHours()).padStart(2, "0");
  const min = String(d.getMinutes()).padStart(2, "0");
  return `${y}-${m}-${day} ${h}:${min}`;
}

export function formatLogDate(value: string | number | Date | null | undefined): string {
  const d = parseTimestampToDate(value);
  if (!d) return typeof value === "string" && value ? value.split(" ")[0] : "--";
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

export function formatLogTime(value: string | number | Date | null | undefined): string {
  const d = parseTimestampToDate(value);
  if (!d) return "--";
  const h = String(d.getHours()).padStart(2, "0");
  const min = String(d.getMinutes()).padStart(2, "0");
  const sec = String(d.getSeconds()).padStart(2, "0");
  return `${h}:${min}:${sec}`;
}

export function formatLogFull(value: string | number | Date | null | undefined): string {
  const d = parseTimestampToDate(value);
  if (!d) return typeof value === "string" && value ? value : "未知时间";
  const dateStr = formatLogDate(d);
  const timeStr = formatLogTime(d);
  return `${dateStr} ${timeStr}`;
}

export function formatRateLimit(value: string): string {
  let formatted = value.trim().replace(/\s+/g, " ");
  if (!formatted) return "";
  const compact = formatted.toLocaleLowerCase().replace(/\s+/g, "");
  if (["unknown", "未知"].includes(compact)) return "";
  if (
    ["0", "∞", "无", "无限制", "不限制", "不限制rpm", "不限", "不限速", "unlimit", "unlimited"].includes(compact)
  )
    return "不限速";

  const rateNumber = (raw: string) => {
    const numeric = Number(raw.replace(/,/g, ""));
    return Number.isFinite(numeric)
      ? new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 2 }).format(numeric)
      : raw;
  };
  const duration = (amount: string | undefined, unit: string) => {
    const count = amount ? Number(amount) : 1;
    const normalizedUnit = unit.toLocaleLowerCase();
    const label = /^(?:s|sec|secs|second|seconds|秒)$/.test(normalizedUnit)
      ? "秒"
      : /^(?:h|hr|hrs|hour|hours|时|小时)$/.test(normalizedUnit)
        ? "小时"
        : /^(?:d|day|days|天)$/.test(normalizedUnit)
          ? "天"
          : "分钟";
    return count === 1 ? label : `${rateNumber(String(count))}${label}`;
  };

  formatted = formatted
    .replace(/\brpm\s*(\d[\d,]*(?:\.\d+)?)\b/gi, (_, count: string) => `${rateNumber(count)}次/分钟`)
    .replace(/(\d[\d,]*(?:\.\d+)?)\s*rpm\b/gi, (_, count: string) => `${rateNumber(count)}次/分钟`)
    .replace(/(\d[\d,]*(?:\.\d+)?)\s*(?:次)?\s*\/\s*一分(?:钟)?/g, (_, count: string) => `${rateNumber(count)}次/分钟`)
    .replace(
      /(\d[\d,]*(?:\.\d+)?)\s*(?:次)?\s*\/\s*(?:(\d+(?:\.\d+)?)\s*)?(seconds?|secs?|sec|s|minutes?|mins?|min|m|hours?|hrs?|hr|h|days?|day|d|秒|分钟|分|小时|时|天)/gi,
      (_, count: string, amount: string | undefined, unit: string) =>
        `${rateNumber(count)}次/${duration(amount, unit)}`,
    )
    .replace(/\bgpt\b/gi, "GPT")
    .replace(/:/g, "：")
    .replace(/(?:默认|翻译)\s*(?=\d[\d,]*(?:\.\d+)?次\/)/g, (label) => `${label.trim()}：`);
  if (/^\d[\d,]*(?:\.\d+)?$/.test(formatted)) return `${rateNumber(formatted)}次/分钟`;
  return formatted;
}

export function hostname(url: string): string {
  try {
    return new URL(url).hostname;
  } catch {
    return url;
  }
}

export function logoText(apiBaseUrl: string, name: string): string {
  const host = hostname(apiBaseUrl).replace(/^www\./, "");
  return (host.split(".")[0] || name).slice(0, 6);
}

/** 格式化毫秒时长：<1s 显示 ms，否则显示秒（带一位小数）。 */
export function formatDuration(ms?: number | null): string {
  if (ms == null || ms < 0) return "—";
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

/** 带正号前缀的时长格式化，用于显示耗时增量。 */
export function formatElapsed(milliseconds: number): string {
  if (milliseconds < 1000) return `+${milliseconds}ms`;
  return `+${(milliseconds / 1000).toFixed(1)}s`;
}

/** 数字本地化（千分位）。null/undefined 返回 "0"。 */
export function formatNumber(num: number | undefined | null): string {
  if (num === undefined || num === null) return "0";
  return num.toLocaleString();
}

/** 将秒数格式化为可读的运行时间。 */
export function formatUptime(seconds: number): string {
  if (seconds < 60) return `${seconds} 秒`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分钟`;
  const hours = Math.floor(seconds / 3600);
  const mins = Math.floor((seconds % 3600) / 60);
  return `${hours} 小时 ${mins} 分`;
}

/** 紧凑数字格式：<1k 显示原数，<10k 显示如 1.2k，<1M 显示如 12k，≥1M 显示如 1.5m。 */
export function formatCompactCount(value?: number | null): string {
  const amount = Number(value ?? 0);
  if (!Number.isFinite(amount) || amount <= 0) return "0";
  if (amount < 1000) return String(Math.round(amount));
  if (amount < 10000) {
    const text = (amount / 1000).toFixed(1).replace(/\.0$/, "");
    return `${text}k`;
  }
  if (amount < 1000000) return `${Math.round(amount / 1000)}k`;
  return `${(amount / 1000000).toFixed(1).replace(/\.0$/, "")}m`;
}

/** Token 数格式化：≥1M 显示 M，≥1K 显示 K。 */
export function formatTokens(value: number): string {
  if (!value) return "—";
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(value % 1_000_000 ? 1 : 0)}M`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(value % 1_000 ? 1 : 0)}K`;
  return String(value);
}

/** Token 数完整格式化（千分位）。 */
export function formatTokensFull(value: number): string {
  if (!value) return "—";
  return value.toLocaleString("zh-CN");
}

/** 价格格式化：<$0.01 显示4位小数，<$1 显示3位，否则2位。 */
export function formatPrice(cost: number | undefined | null): string {
  if (cost === undefined || cost === null || cost <= 0) return "—";
  if (cost < 0.01) return `$${cost.toFixed(4)}`;
  if (cost < 1) return `$${cost.toFixed(3)}`;
  return `$${cost.toFixed(2)}`;
}

/**
 * 模型健康度的展示等级。
 * - `healthy` 成功率 ≥ 90%
 * - `degraded` 成功率在 50%~90%
 * - `down` 成功率 < 50%
 *
 * 没有「无流量」这一档：站点只为窗口内确有请求的模型下发条目，
 * 模型不在健康度表里就是没有数据，界面直接不显示徽标，而不是显示 0%。
 */
export type ModelHealthLevel = "healthy" | "degraded" | "down";

/**
 * 成功率 → 展示等级的唯一阈值来源（0~1 入参，越界自动钳位）。
 * 徽标、进度条与逐时状态条必须共用这一处，否则同一模型会出现
 * 「标签写橙色 60%、条子画红色」这类自相矛盾的展示。
 */
export function modelHealthLevelOf(successRate: number): ModelHealthLevel {
  const clamped = Math.min(1, Math.max(0, successRate));
  return clamped >= 0.9 ? "healthy" : clamped >= 0.5 ? "degraded" : "down";
}

export interface ModelHealthBadge {
  level: ModelHealthLevel;
  /** 卡片上的一行短标签，如 `99.9%`。 */
  label: string;
  /** 进度条填充比例（0~100 的百分数），直接绑到 style.width。 */
  width: number;
  /** 悬浮提示，包含延迟/出字速度与「全站口径」免责说明。 */
  title: string;
}

/** 状态条槽位数：与站点窗口小时数一致（同步固定请求 hours=24），逐格一个整点。 */
export const MODEL_STATUS_SLOT_COUNT = 24;

const STATUS_SLOT_SECONDS = 3600;

/** 状态条上的一个槽位，对应窗口内一个整点。 */
export interface ModelStatusSlot {
  /** 该槽位对应的整点（Unix 秒）。 */
  ts: number;
  /** 成功率等级；该整点无流量（站点未下发数据点）时为 null，界面留灰槽。 */
  level: ModelHealthLevel | null;
  /** 成功率（0~1）；无流量时缺省。 */
  successRate?: number;
  /** 单格悬浮提示：整点 + 成功率 / 无流量说明。 */
  title: string;
}

/** 整点时间戳格式化成 `MM-DD HH:00`（本地时区），跨天时仍能分辨。 */
function formatHourLabel(ts: number): string {
  const date = new Date(ts * 1000);
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  const hour = String(date.getHours()).padStart(2, "0");
  return `${month}-${day} ${hour}:00`;
}

/**
 * 把逐时健康度翻成固定 24 格的竖条状态条（图片样式：绿 = 成功率达标，
 * 橙/红逐级下降，灰 = 该整点无流量）。
 *
 * 站点只为**有流量**的整点下发数据点，所以这里按 `windowStart` 对号入座：
 * 没有数据点的格子留 null（灰槽）。不允许用 0 填补——「无流量」和
 * 「成功率 0%」是完全不同的结论，前者是没数据、后者是全线失败。
 *
 * 窗口起点的取值顺序：站点给的 `windowStart` → 最后一个数据点倒推
 * （老版本/魔改站点不给窗口时保证条带仍按整点对齐）→ 调用方给的
 * `fallbackWindowStart`（同站点其它模型的窗口，让无数据模型的全灰条带
 * 与有数据条带对齐）→ 当前整点。
 *
 * **永远返回 24 格**，没有序列数据时整条全灰：模型在窗口内没有流量也是
 * 一种状态，界面要能一眼看出「这条是空的」，而不是干脆不渲染。
 */
export function describeModelStatusStrip(
  health?: SiteModelHealth | null,
  fallbackWindowStart?: number,
): ModelStatusSlot[] {
  const points = Array.isArray(health?.series) ? health.series : [];
  const rates = new Map<number, number>();
  let lastTs = 0;
  for (const point of points) {
    const rawTs = point?.ts;
    if (typeof rawTs !== "number" || !Number.isFinite(rawTs) || rawTs <= 0) continue;
    const ts = Math.floor(rawTs / STATUS_SLOT_SECONDS) * STATUS_SLOT_SECONDS;
    lastTs = Math.max(lastTs, ts);
    const rate = point.successRate;
    if (typeof rate === "number" && Number.isFinite(rate)) {
      rates.set(ts, Math.min(1, Math.max(0, rate)));
    }
  }
  const windowStart = (() => {
    const start = health?.windowStart;
    if (typeof start === "number" && Number.isFinite(start) && start > 0) {
      return Math.floor(start / STATUS_SLOT_SECONDS) * STATUS_SLOT_SECONDS;
    }
    if (lastTs > 0) {
      return lastTs - (MODEL_STATUS_SLOT_COUNT - 1) * STATUS_SLOT_SECONDS;
    }
    if (
      typeof fallbackWindowStart === "number" &&
      Number.isFinite(fallbackWindowStart) &&
      fallbackWindowStart > 0
    ) {
      return Math.floor(fallbackWindowStart / STATUS_SLOT_SECONDS) * STATUS_SLOT_SECONDS;
    }
    const nowHour = Math.floor(Date.now() / 1000 / STATUS_SLOT_SECONDS) * STATUS_SLOT_SECONDS;
    return nowHour - (MODEL_STATUS_SLOT_COUNT - 1) * STATUS_SLOT_SECONDS;
  })();
  return Array.from({ length: MODEL_STATUS_SLOT_COUNT }, (_, index) => {
    const ts = windowStart + index * STATUS_SLOT_SECONDS;
    const label = formatHourLabel(ts);
    const rate = rates.get(ts);
    if (rate === undefined) {
      return { ts, level: null, title: `${label} 无流量数据` };
    }
    return {
      ts,
      level: modelHealthLevelOf(rate),
      successRate: rate,
      title: `${label} 成功率 ${formatSuccessRate(rate)}`,
    };
  });
}

/** 状态条容器的整体说明（口径 + 配色含义），绑到容器的 title/aria-label。 */
export function describeModelStatusStripTitle(health?: SiteModelHealth | null): string {
  const hours = health?.windowHours ?? 24;
  return `近 ${hours} 小时逐时成功率（站点全站口径，与当前 Key 无关）：绿色 ≥90%，橙色 50%~90%，红色 <50%，灰色为该时段无流量。悬停各格查看具体时段。`;
}

/** 毫秒转人类可读时长：<1s 毫秒，<1min 秒，其余分钟。 */
function formatLatency(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(ms < 10_000 ? 1 : 0)}s`;
  return `${(ms / 60_000).toFixed(1)}min`;
}

/** 成功率（0~1）显示成百分比文本。满分不写 100.0%，0% 不写 0.0%。 */
function formatSuccessRate(rate: number): string {
  if (rate >= 1) return "100%";
  if (rate <= 0) return "0%";
  const percent = rate * 100;
  return `${percent >= 99.95 ? Math.round(percent) : percent.toFixed(1)}%`;
}

/**
 * 把 NewAPI `/api/perf-metrics/summary` 的全站模型健康度翻成一条进度条 +
 * 百分比。这是**全站聚合**口径（所有用户的流量），与当前用户/当前 Key 无关，
 * 所以悬浮提示里必须点明，避免被当成「我的 Key 能不能用」的结论。
 *
 * 站点只返回 0~100 的百分数，Rust 侧已归一化到 0~1；这里不再做二次换算。
 */
export function describeModelHealth(health?: SiteModelHealth | null): ModelHealthBadge | null {
  if (!health) return null;
  const hours = health.windowHours ?? 24;
  const scope = `站点全站口径（所有用户的流量），近 ${hours} 小时`;
  const rate = health.successRate;
  if (typeof rate !== "number" || !Number.isFinite(rate)) {
    return {
      level: "degraded",
      label: "成功率未知",
      width: 0,
      title: `${scope}有请求记录，但站点未返回成功率。`,
    };
  }
  const clamped = Math.min(1, Math.max(0, rate));
  const level: ModelHealthLevel = modelHealthLevelOf(clamped);
  const details = [`成功率 ${formatSuccessRate(clamped)}`];
  if (typeof health.avgLatencyMs === "number" && health.avgLatencyMs > 0) {
    details.push(`平均延迟 ${formatLatency(health.avgLatencyMs)}`);
  }
  if (typeof health.avgTps === "number" && health.avgTps > 0) {
    details.push(`平均 ${health.avgTps.toFixed(1)} tok/s`);
  }
  return {
    level,
    label: formatSuccessRate(clamped),
    // 留 1% 底色，最差也看得见有一条槽。
    width: Math.max(1, clamped * 100),
    title: `${scope}：${details.join("，")}。该数据与当前 Key 无关，不能据此判断本 Key 是否可用。`,
  };
}
