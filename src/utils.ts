export function escapeHtml(value: unknown): string {
  return String(value ?? "").replace(
    /[&<>'"]/g,
    (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "'": "&#39;", '"': "&quot;" })[character]!,
  );
}

export function formatDate(value: string): string {
  if (!value) return "未知";
  const normalized = /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/.test(value)
    ? `${value.replace(" ", "T")}Z`
    : value;
  const date = new Date(normalized);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  })
    .format(date)
    .replace(/\//g, "-");
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
