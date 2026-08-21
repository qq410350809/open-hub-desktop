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

export type TrendGranularity =
  | "hour"   // < 7 天 → 逐小时
  | "day"    // ≤ 92 天 → 逐日
  | "month"; // > 92 天 → 逐月

export interface UsageBucketLike {
  timestamp: string;
  source?: string;
  model?: string;
  projectKey?: string;
  totalTokens?: number;
  inputTokens?: number;
  cachedInputTokens?: number;
  cacheCreationInputTokens?: number;
  outputTokens?: number;
  reasoningOutputTokens?: number;
  conversationCount?: number;
  requestCount?: number;
  costUsd?: number;
  estimatedTokens?: number;
  estimatedInputTokens?: number;
}

export interface TrendDetailItem {
  key?: string;
  label: string;
  total: number;
  input: number;
  output: number;
  cache: number;
  cacheRead: number;
  cacheWrite: number;
  cacheHitRate: number | null;
  reasoning: number;
  sessions: number;
  estimatedInput: number;
}

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

export interface HealthTimelineCell {
  key: string;
  label: string;
  dialogues: number;
  success: number;
  failed: number;
  requests: number;
  usageRequests: number;
  requestsEstimated: boolean;
  successRate: number | null;
  level: number;
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
  hasExtracted: boolean;
}

export interface HealthInput {
  hour: string;
  dialogues?: number;
  requests?: number;
  success: number;
  failed: number;
}

export interface HealthUsageInput {
  timestamp: string;
  conversationCount?: number;
  outputTokens?: number;
  reasoningOutputTokens?: number;
  totalTokens?: number;
  requests?: number;
}

export interface BucketBreakdownTotal {
  totalTokens: number;
  inputTokens: number;
  outputTokens: number;
  cacheTokens: number;
  cacheReadTokens: number;
  cacheWriteTokens: number;
  cacheHitRate: number | null;
  reasoningTokens: number;
  conversations: number;
  requests: number;
  requestsEstimated: boolean;
  costUsd: number;
  estimatedTokens: number;
  estimatedInputTokens: number;
}

export interface SourceTotal extends BucketBreakdownTotal {
  source: string;
}

export interface ModelTotal extends BucketBreakdownTotal {
  model: string;
}
