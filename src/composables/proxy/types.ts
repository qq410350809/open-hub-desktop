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
}

export interface OpencodeProxyConfig {
  enabled: boolean;
  port: number;
  apiKey: string;
  channels: ChannelConfig[];
  timeoutSeconds: number;
  recordRequestBody?: boolean;
  maxRetries?: number;
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
  completionTokens?: number;
  reasoningTokens?: number;
  totalTokens?: number;
  errorMessage?: string;
  requestBody?: string;
  responseBody?: string;
  nodeName?: string;
}

export interface ChannelUsageStats {
  channelId: string;
  totalRequests: number;
  successfulRequests: number;
  failedRequests: number;
  totalPromptTokens?: number;
  totalCompletionTokens?: number;
  totalReasoningTokens?: number;
  totalReasoningRequests?: number;
  totalCacheHitTokens?: number;
  totalTokens?: number;
  todayTotalTokens?: number;
}

export interface ChannelModelList {
  channelId: string;
  models: string[];
}
