export interface ChannelConfig {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  protocol: string;
  upstreamUrl: string;
  apiKey?: string;
  apiKeys?: string[];
  useProxyPool: boolean;
  alias?: string;
  siteId?: string | null;
  useFixedProxy?: boolean;
  enabledModels?: string[] | null;
  /** 统计维度稳定数字 ID：内置渠道占 1-100（opencode=1），动态渠道从 101 起；与别名解耦 */
  statsId?: number;
}

export interface OpencodeProxyConfig {
  enabled: boolean;
  listenHost: string;
  port: number;
  apiKey: string;
  channels: ChannelConfig[];
  timeoutSeconds: number;
  recordRequestBody?: boolean;
  maxRetries?: number;
  /** 动态渠道统计 ID 分配计数器（101 起，1-100 预留内置渠道） */
  nextChannelStatsId?: number;
  /** 请求明细保留天数：超期自动清理明细（统计聚合不受影响）；0 或缺省 = 永久保留 */
  logRetentionDays?: number;
}

export interface OpencodeProxyStatus {
  running: boolean;
  port: number;
  url: string;
  totalRequests: number;
  successfulRequests: number;
  failedRequests: number;
  uptimeSeconds: number;
  modelsCount: number;
  channelsCount: number;
  totalPromptTokens?: number;
  totalCompletionTokens?: number;
  totalReasoningTokens?: number;
  totalReasoningRequests?: number;
  totalCacheHitTokens?: number;
  totalTokens?: number;
  todayTotalTokens?: number;
}

export interface ProxyRequestLog {
  id: string;
  timestamp: string;
  method: string;
  path: string;
  channelId: string;
  model: string;
  stream: boolean;
  statusCode: number;
  durationMs: number;
  ttftMs?: number;
  promptTokens?: number;
  promptCacheHitTokens?: number;
  promptCacheMissTokens?: number;
  /** Prompt 缓存写入量（Anthropic cache_creation_input_tokens） */
  cacheCreationTokens?: number;
  completionTokens?: number;
  reasoningTokens?: number;
  totalTokens?: number;
  errorMessage?: string;
  requestBody?: string;
  responseBody?: string;
  nodeName?: string;
  /** 发起请求的客户端标识（User-Agent / 端点推断，如 claude / codex） */
  clientName?: string | null;
  /** 出网上游地址（完整 URL，含 path），日志展示「入->出」双地址 */
  upstreamUrl?: string | null;
}

export interface ChannelUsageStats {
  channelId: string;
  totalRequests: number;
  successfulRequests: number;
  failedRequests: number;
  avgDurationMs?: number;
  avgTtftMs?: number | null;
  totalPromptTokens?: number;
  totalCompletionTokens?: number;
  totalReasoningTokens?: number;
  totalReasoningRequests?: number;
  totalCacheHitTokens?: number;
  totalTokens?: number;
  todayTotalTokens?: number;
  /** 今日（本地时区）双通道数据 */
  todayRequests?: number;
  todaySuccessfulRequests?: number;
  todayFailedRequests?: number;
  todayAvgDurationMs?: number;
  todayAvgTtftMs?: number | null;
  todayPromptTokens?: number;
  todayCompletionTokens?: number;
  todayCacheHitTokens?: number;
}

export interface ChannelModelList {
  channelId: string;
  models: string[];
}

/** 「日 × 全渠道」聚合数据点（后端 channel_daily_stats 跨渠道求和） */
export interface GatewayDailyPoint {
  date: string;
  totalRequests: number;
  successfulRequests: number;
  failedRequests: number;
  promptTokens: number;
  completionTokens: number;
  reasoningTokens: number;
  cacheHitTokens: number;
  totalTokens: number;
}

/** 「时 × 全渠道」聚合数据点（≤3 天区间趋势用，后端 channel_hourly_stats 跨渠道求和） */
export interface GatewayHourlyPoint {
  /** 本地日期 YYYY-MM-DD（多天区间时区分小时桶归属） */
  date: string;
  /** 0-23（本地时间） */
  hour: number;
  totalRequests: number;
  successfulRequests: number;
  failedRequests: number;
  promptTokens: number;
  completionTokens: number;
  reasoningTokens: number;
  cacheHitTokens: number;
  totalTokens: number;
}

/** 全渠道累计汇总（含平均耗时 / 平均首 Token 时延） */
export interface GatewayOverviewTotals {
  totalRequests: number;
  successfulRequests: number;
  failedRequests: number;
  avgDurationMs: number;
  avgTtftMs?: number | null;
  promptTokens: number;
  completionTokens: number;
  reasoningTokens: number;
  cacheHitTokens: number;
  totalTokens: number;
}

/** 控制台「全渠道数据总览」：区间逐日数据（缺日补零）+ 区间累计 + 今日聚合 */
export interface GatewayOverviewStats {
  days: number;
  daily: GatewayDailyPoint[];
  totals: GatewayOverviewTotals;
  /** 今日（本地时区）全渠道聚合，与所选区间解耦，供 KPI「今日」角标使用 */
  today: GatewayDailyPoint;
  /** ≤3 天区间的小时级趋势（每天 24 点）；不满足条件或无小时数据时不返回，前端回退日视图 */
  hourly?: GatewayHourlyPoint[] | null;
  /** 长区间（>92 天）的月级趋势；date 为 YYYY-MM；区间内有数据时才返回，前端回退日视图 */
  monthly?: GatewayDailyPoint[] | null;
}
