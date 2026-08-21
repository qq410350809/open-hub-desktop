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
