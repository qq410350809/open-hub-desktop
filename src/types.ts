export interface Maintainer {
  name: string;
  id: string;
  username: string;
  profileUrl: string;
}

export interface ExtensionLink {
  label: string;
  url: string;
}

export type SiteLinkKind = "api" | "checkin" | "benefit" | "status" | "extension";

export interface AddressItem {
  label: string;
  url: string;
  note?: string;
}

export interface SiteRecord {
  id: string;
  name: string;
  description: string;
  registrationLimit: number;
  icon: string;
  apiBaseUrl: string;
  systemType: string;
  tags: string[];
  supportsImmersiveTranslation: boolean;
  supportsLdc: boolean;
  supportsCheckin: boolean;
  supportsNsfw: boolean;
  checkinUrl: string;
  checkinNote: string;
  benefitUrl: string;
  maintainers: Maintainer[];
  rateLimit: string;
  statusUrl: string;
  extensionLinks: ExtensionLink[];
  isOnlyMaintainerVisible: boolean;
  requiresInviteCode: boolean;
  isRunaway: boolean;
  isFakeCharity: boolean;
  hasPendingReport: boolean;
  isPersonal: boolean;
  isPending: boolean;
  favorite: boolean;
  hidden: boolean;
  updatedAt: string;
}

export interface LibraryData {
  sites: SiteRecord[];
  suggestedTags: string[];
  usageSites: ChromeUsageSite[];
}

export interface ChromeSessionInfo {
  profileId: string;
  domain: string;
  cookieCount: number;
  cookieNames: string[];
  profileName: string;
  accountName: string;
  username: string;
  apiKeyCount: number;
  apiModelCount: number;
  apiCountsSynced: boolean;
  apiSyncError: string;
  hasAccessToken: boolean;
  remaining: number | null;
  used: number | null;
  total: number | null;
  unit: string;
  isValid: boolean;
  syncError: string;
  checkinEnabled: boolean;
  checkedInToday: boolean;
  checkinError: string;
  accountUpdatedAt: string;
  newapiUserId?: string;
}

export interface ChromeSessionValue {
  domain: string;
  cookie: string;
  cookieCount: number;
  profileName: string;
}

export interface ChromeUsageScanResult {
  scanned: number;
  detected: number;
  accounts: number;
  warnings: number;
  newlyMarked: number;
  sites: ChromeUsageSite[];
}

export interface ChromeUsageSite {
  siteId: string;
  sessions: ChromeSessionInfo[];
}

export interface SyncSitesResult {
  added: number;
  updated: number;
  total: number;
  profileName: string;
  accountName: string;
  userName: string;
  runaway: boolean;
  siteIds: string[];
}

export type SyncProgressStatus = "running" | "success" | "error" | "info";
export type SyncRunState = "idle" | "syncing" | "detecting" | "complete" | "error";

export interface SyncSitesProgress {
  runId: number;
  stage: string;
  status: SyncProgressStatus;
  message: string;
}

export interface SyncLogEntry {
  id: number;
  elapsedMs: number;
  stage: string;
  status: SyncProgressStatus;
  message: string;
}

export interface CharityFeedItem {
  id: string;
  title: string;
  link: string;
  author: string;
  publishedAt: string;
  summary: string;
  categories: string[];
  isNew: boolean;
  replyCount: number;
  views: number;
  likeCount: number;
  lastActivityAt: string;
  pinned: boolean;
  posters: string[];
  feedIds?: string[];
  feedNames?: string[];
}

export interface CharityFeedResult {
  feedId: string;
  feedName: string;
  items: CharityFeedItem[];
  fetchedAt: string;
  changed: boolean;
  newCount: number;
  updatedCount: number;
  initialized: boolean;
  sourceProfileName: string;
  sourceAccountName: string;
  status?: string;
  message?: string;
  usedNodeId?: string;
  usedNodeName?: string;
  unreadCount?: number;
  skipped?: boolean;
  totalCount?: number;
  offset?: number;
  limit?: number;
  hasMore?: boolean;
}

export interface CharitySyncProgress {
  feedId: string;
  feedName: string;
  stage: string;
  status: string;
  message: string;
  usedNodeId: string;
  usedNodeName: string;
  newCount: number;
  updatedCount: number;
  unreadCount: number;
}

export interface CharitySyncLogEntry {
  id: number;
  at: string;
  feedId: string;
  feedName: string;
  stage: string;
  status: string;
  message: string;
  nodeName: string;
  durationMs?: number;
}

export interface CharityFeedTag {
  id: string;
  name: string;
}

export interface RemoteUserInfo {
  name: string;
  username: string;
  avatarUrl: string;
  profileName: string;
  accountName: string;
}

export type ModelCategory = "all" | "openai" | "claude" | "deepseek" | "gemini" | "grok" | "domestic" | "other";

export interface ModelItem {
  id: string;
  name: string;
  category: ModelCategory;
  vendorName: string;
  sites: SiteRecord[];
}

export type ThemePreference = "system" | "light" | "dark";
export type ProxyNodeViewModePreference = "list" | "country";

export interface Preferences {
  theme: ThemePreference;
  defaultRunawayFilter: string;
  defaultUsageFilter: string;
  proxyNodeViewMode: ProxyNodeViewModePreference;
  sidebarCollapsed: boolean;
}

export const emptySite = (): SiteRecord => ({
  id: "",
  name: "",
  description: "",
  registrationLimit: 0,
  icon: "",
  apiBaseUrl: "",
  systemType: "",
  tags: [],
  supportsImmersiveTranslation: false,
  supportsLdc: false,
  supportsCheckin: false,
  supportsNsfw: false,
  checkinUrl: "",
  checkinNote: "",
  benefitUrl: "",
  maintainers: [],
  rateLimit: "",
  statusUrl: "",
  extensionLinks: [],
  isOnlyMaintainerVisible: false,
  requiresInviteCode: false,
  isRunaway: false,
  isFakeCharity: false,
  hasPendingReport: false,
  isPersonal: false,
  isPending: false,
  favorite: false,
  hidden: false,
  updatedAt: "",
});

export interface ProxySubscription {
  id: string;
  name: string;
  url: string;
  nodeCount: number;
  lastError: string;
  createdAt: string;
  updatedAt: string;
}

export interface ProxyNode {
  id: string;
  subscriptionNames: string[];
  name: string;
  proxyType: string;
  server: string;
  port: number;
  cipher: string;
  udp: boolean;
  latencyMs: number | null;
  testStatus: string;
  testedAt: string;
  countryCode: string;
  countryName: string;
  classification: string;
  primaryIp: string;
  updatedAt: string;
}

export interface ProxyPoolState {
  subscriptions: ProxySubscription[];
  nodes: ProxyNode[];
  activeNodeId: string;
  activeNode: ProxyNode | null;
  enabled: boolean;
  ignoreAddresses: string;
  speedTestUrl: string;
  runtimeAvailable: boolean;
  runtimePath: string;
  runtimeError: string;
  nodeCount: number;
  subscriptionCount: number;
  invalidNodeCount: number;
}

export interface ProxyPoolRefreshResult {
  subscription: ProxySubscription;
  added: number;
  total: number;
  discarded: number;
}

export interface ProxySourceProgress {
  sourceId: string;
  stage: "queued" | "fetching" | "parsing" | "saving" | "done" | "error";
  status: string;
  message: string;
  completed: number;
  total: number;
  added: number;
  discarded: number;
}

export interface ProxyNodeTestProgress {
  nodeId: string;
  phase: "started" | "completed";
  latencyMs: number | null;
  status: string;
  completed: number;
  total: number;
}

export interface ProxyIpNodeAnalysis {
  nodeId: string;
  nodeName: string;
  server: string;
  resolvedIps: string[];
  primaryIp: string;
  classification: string;
  countryCode: string;
  countryName: string;
  error: string;
}

export interface ProxyIpGroup {
  key: string;
  label: string;
  classification: string;
  countryCode: string;
  countryName: string;
  nodeIds: string[];
  nodeCount: number;
}

export interface ProxyIpAnalysis {
  analyzedAt: string;
  geoipAvailable: boolean;
  geoipDatabasePath: string;
  totalNodes: number;
  resolvedNodes: number;
  unresolvedNodes: number;
  uniqueIps: number;
  nodes: ProxyIpNodeAnalysis[];
  groups: ProxyIpGroup[];
}

// —— Token 统计（数据来源：tokentracker CLI）——
export interface TokenSessionTokens {
  inputTokens: number;
  cachedInputTokens: number;
  cacheCreationInputTokens: number;
  outputTokens: number;
  reasoningOutputTokens: number;
  totalTokens: number;
}

export interface TokenSession {
  version: number;
  sessionHash: string;
  source: string;
  projectKey: string;
  model: string;
  startedAt: string;
  endedAt: string;
  activeMs: number;
  turns: number;
  editTurns: number;
  retryTurns: number;
  subagentCalls: number;
  subagentTypes: Record<string, number>;
  tokens: TokenSessionTokens;
  provenance: Record<string, unknown>;
  durationMs: number;
  totalTokens: number;
  costUsd: number;
  productive: boolean;
  firstPass: boolean;
  oneShot: boolean;
  tokensPerEdit: number | null;
  costPerEdit: number | null;
}

export interface TokenSummary {
  sessions: number;
  productiveSessions: number;
  oneShotSessions: number;
  editTurns: number;
  retries: number;
  totalTokens: number;
  costUsd: number;
  editTokens: number;
  editCostUsd: number;
  productiveRate: number;
  oneShotRate: number | null;
  editSessions: number;
  firstPassSessions: number;
  editSessionRate: number;
  firstPassRate: number | null;
  tokensPerEdit: number | null;
  costPerEdit: number | null;
}

export interface TokenModelStat extends TokenSummary {
  model: string;
}

export interface TokenSubagentStat {
  name: string;
  calls: number;
  sessions: number;
  totalTokens: number;
  costUsd: number;
}

export interface TokenStatsReport {
  available: boolean;
  sessions: TokenSession[];
  sessionCount: number;
  summary: TokenSummary;
  byModel: TokenModelStat[];
  subagents: TokenSubagentStat[];
  provenance: Record<string, unknown>;
}

// —— Token 用量小时桶（tokentracker cursors.json hourly.buckets）——
export interface TokenUsageBucket {
  source: string;
  model: string;
  timestamp: string;
  totalTokens: number;
  billableTotalTokens: number;
  inputTokens: number;
  cachedInputTokens: number;
  cacheCreationInputTokens: number;
  outputTokens: number;
  reasoningOutputTokens: number;
  conversationCount: number;
}

export interface TokenUsageReport {
  available: boolean;
  buckets: TokenUsageBucket[];
  startDate: string;
  endDate: string;
}

export interface TokenTrackerSyncReport {
  available: boolean;
  changed: boolean;
  skipped: boolean;
  elapsedMs: number;
  updatedAt: string;
  message: string;
}

// —— 请求/对话活动：多工具直读后的小时桶 ——
export interface RequestHealthBucket {
  hour: string;          // ISO 小时 (YYYY-MM-DDTHH:00:00.000Z)
  dialogues: number;     // 用户发起 turns（排除 tool_result / 自动触发）
  requests: number;      // 真实 API 请求数（多工具）
  success: number;       // 可观测成功样本
  failed: number;        // 可观测失败样本
}

export interface RequestHealthSourceSummary {
  source: string;
  dialogues: number;
  requests: number;
  success: number;
  failed: number;
}

export interface RequestHealthReport {
  available: boolean;
  buckets: RequestHealthBucket[];
  bySource?: RequestHealthSourceSummary[];
}

// —— 原始日志解析：会话 / 对话 / 请求 ——
export interface RawSession {
  id: string;
  source: string;
  project: string;
  startedAt: string;
  endedAt: string;
  messageCount: number;
  conversationCount: number;
  model: string;
  totalTokens: number;
}
export interface RawConversation {
  id: string;
  sessionId: string;
  source: string;
  project: string;
  index: number;
  startedAt: string;
  endedAt: string;
  requestCount: number;
  model: string;
  totalTokens: number;
}
export interface RawRequest {
  id: string;
  sessionId: string;
  conversationId: string;
  source: string;
  timestamp: string;
  role: string;
  model: string;
  inputTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
  outputTokens: number;
  totalTokens: number;
}
export interface RawLogReport {
  available: boolean;
  sessions: RawSession[];
  conversations: RawConversation[];
  requests: RawRequest[];
}
