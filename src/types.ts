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
  useProxyPool: boolean;
  favorite: boolean;
  hidden: boolean;
  updatedAt: string;
}

export type SiteUsageState = "all" | "personal" | "pending";

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
  /** 浏览器兜底剩余冷却毫秒（后端持久化指数退避），0 表示不在冷却。 */
  browserFallbackCooldownMs?: number;
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

export interface AutoSyncSettings {
  enabled: boolean;
  intervalMinutes: number;
}

export interface AutoSyncAccountChange {
  siteId: string;
  siteName: string;
  profileId: string;
  accountLabel: string;
  error: string;
}

export interface AutoSyncRoundSummary {
  startedAt: number;
  finishedAt: number;
  refreshedAccounts: number;
  recovered: AutoSyncAccountChange[];
  pendingManual: AutoSyncAccountChange[];
  modelsRefreshed: number;
  modelsFailed: number;
  error: string;
}

export interface AutoSyncStatus {
  enabled: boolean;
  intervalMinutes: number;
  lastRoundAt: number;
  lastSummary: AutoSyncRoundSummary | null;
}

export interface AutoSyncProgress {
  stage: string;
  status: SyncProgressStatus;
  message: string;
  at: number;
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
  jsonUrl?: string;
  enabled?: boolean;
  sortOrder?: number;
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

/** 模型聚合页：同名分组的展示模式（聚合 = 跨站合并，独立 = 各站分开）。 */
export type ModelAggGroupMode = "aggregate" | "independent";

export interface Preferences {
  theme: ThemePreference;
  defaultRunawayFilter: string;
  defaultUsageFilter: string;
  proxyNodeViewMode: ProxyNodeViewModePreference;
  sidebarCollapsed: boolean;
  /** 模型聚合页右侧条目顺序（独立站点 = site id，聚合块 = `group:<分组名>`）。 */
  modelAggSiteOrder: string[];
  /** 模型聚合页：分组名 → 聚合/独立模式。 */
  modelAggGroupModes: Record<string, ModelAggGroupMode>;
  /** 模型聚合页：手动取消勾选（隐藏）的树节点键；默认空 = 全部选中。 */
  modelAggHiddenModels: string[];
}

// —— 站点模型缓存（与 Rust 侧 models.rs 的同名结构对齐，camelCase）——

export interface SiteModelItem {
  id: string;
  ownedBy?: string;
}

export interface SiteModelCacheAccount {
  profileId: string;
  profileName: string;
  accountName: string;
  username: string;
  keys: string[];
  keyGroups?: Record<string, string>;
  keyModels?: Record<string, SiteModelItem[]>;
  error?: string;
}

export interface SiteModelCache {
  models: SiteModelItem[];
  apiSource?: string;
  accounts: SiteModelCacheAccount[];
}

export interface SiteModelCacheEntry {
  siteId: string;
  cache: SiteModelCache;
}

// —— 系统类型（与 Rust 侧 platform_detect::canonical_platform 保持一致）——
export const SYSTEM_TYPES: { value: string; text: string }[] = [
  { value: "openai", text: "OpenAI" },
  { value: "codex", text: "Codex" },
  { value: "claude", text: "Claude" },
  { value: "gemini", text: "Gemini" },
  { value: "gemini-cli", text: "Gemini CLI" },
  { value: "antigravity", text: "Antigravity" },
  { value: "cliproxyapi", text: "CliproxyAPI" },
  { value: "anyrouter", text: "AnyRouter" },
  { value: "done-hub", text: "Done Hub" },
  { value: "one-hub", text: "One Hub" },
  { value: "veloera", text: "Veloera" },
  { value: "new-api", text: "NewAPI · Cookie" },
  { value: "newapi2", text: "NewAPI · 刷新令牌" },
  { value: "sub2api", text: "Sub2API" },
  { value: "one-api", text: "One API" },
];

/** 去除空白/中划线/下划线并转小写，用于跨新旧命名比较。 */
export function normalizeSystemType(raw: string): string {
  return raw.trim().toLocaleLowerCase().replace(/[\s_\-]/g, "");
}

/** 系统类型的友好展示名（如 newapi2 → NewAPI · 刷新令牌）；未知类型回退为原始值。 */
export function systemTypeLabel(raw: string): string {
  const normalized = normalizeSystemType(raw);
  const match = SYSTEM_TYPES.find(
    (item) => normalizeSystemType(item.value) === normalized,
  );
  return match?.text ?? raw;
}

/** 已知系统类型的规范化值集合（用于"未知类型"过滤）。 */
export const KNOWN_SYSTEM_TYPES: ReadonlySet<string> = new Set(
  SYSTEM_TYPES.map((item) => normalizeSystemType(item.value)),
);

/** 判断系统类型是否属于/兼容 NewAPI 架构（NewAPI / AnyRouter / One API / One Hub / Done Hub / Veloera）。 */
export function isNewApiCompatible(raw: string): boolean {
  const normalized = normalizeSystemType(raw);
  return [
    "newapi",
    "newapi2",
    "anyrouter",
    "oneapi",
    "onehub",
    "donehub",
    "veloera",
  ].includes(normalized);
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
  useProxyPool: false,
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
  channelLatencyMs: number | null;
  channelTestStatus: string;
  countryCode: string;
  countryName: string;
  classification: string;
  primaryIp: string;
  updatedAt: string;
}

export interface ProxyChannelAccount {
  profileId: string;
}

export interface ProxyChannel {
  id: string;
  name: string;
  nodeId: string;
  node: ProxyNode | null;
  testUrl: string;
  accountCount: number;
  accounts: ProxyChannelAccount[];
  createdAt: string;
  updatedAt: string;
}

export interface ProxyPoolState {
  subscriptions: ProxySubscription[];
  nodes: ProxyNode[];
  channels: ProxyChannel[];
  defaultChannelId: string;
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

// —— Token 统计（OpenHub 自有本地采集器）——
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

// —— Token 用量半小时桶（OpenHub 直接读取各工具本地日志）——
export interface TokenUsageBucket {
  source: string;
  model: string;
  /** 支持项目维度的数据源由 OpenHub 直接填充。 */
  projectKey?: string;
  timestamp: string;
  totalTokens: number;
  billableTotalTokens: number;
  inputTokens: number;
  cachedInputTokens: number;
  cacheCreationInputTokens: number;
  outputTokens: number;
  reasoningOutputTokens: number;
  conversationCount: number;
  /** 桶内真实 API 请求数（一次模型调用 = 一次请求，含子代理/工具循环触发）。旧快照可能缺失。 */
  requestCount?: number;
  costUsd: number;
  pricingAvailable: boolean;
  /** 根据本地可见会话上下文估算、而非来源直接上报的 Token 数。 */
  estimatedTokens: number;
  /** 输入 Token 中来自无缓存字段来源的估算部分；用于区分 0% 命中与无缓存数据。 */
  estimatedInputTokens: number;
}

export interface TokenUsageReport {
  available: boolean;
  buckets: TokenUsageBucket[];
  startDate: string;
  endDate: string;
  pricingSource: string;
}

export interface TokenCollectorSyncReport {
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

// —— 本地 AI Agent 路径诊断 ——
export interface LocalAgentPathEntry {
  kind: string;   // config / data / database
  label: string;
  path: string;
  exists: boolean;
  /** 文件大小（如 38 MB）或目录直属条目数（如 12 项）。 */
  detail: string;
}

export interface LocalAgentPaths {
  source: string;
  name: string;
  root: string;
  detected: boolean;
  paths: LocalAgentPathEntry[];
  /** 最近一次采集中该来源的会话数 / 用量事件数。 */
  collectedSessions: number;
  collectedEvents: number;
}

export interface LocalAgentEnvOverride {
  key: string;
  value: string;
}

export interface LocalAgentPathsReport {
  available: boolean;
  home: string;
  agents: LocalAgentPaths[];
  /** 当前生效的路径重定向环境变量。 */
  envOverrides: LocalAgentEnvOverride[];
  /** 采集缓存的最近更新时间（ISO），空表示尚无采集缓存。 */
  collectedAt: string;
}

export interface LightweightState {
  running: boolean;
  port: number;
  enabled: boolean;
  url: string;
}

export interface ModelCatalogSourceStatus {
  source: string;
  url: string;
  fetchedAt: string;
  recordCount: number;
}

export interface ModelCatalogItem {
  canonicalKey: string;
  displayName: string;
  manufacturer: string;
  mode: string;
  contextLength: number;
  maxInputTokens: number;
  maxOutputTokens: number;
  inputCostPerToken: number;
  outputCostPerToken: number;
  cacheReadCostPerToken: number;
  cacheWriteCostPerToken: number;
  imageCost: number;
  audioInputCostPerToken: number;
  audioOutputCostPerToken: number;
  requestCost: number;
  capabilities: string[];
}

export interface ModelCatalogSnapshot {
  models: ModelCatalogItem[];
  total: number;
  lastSyncedAt: string;
  syncedToday: boolean;
  sources: ModelCatalogSourceStatus[];
}

export interface ModelCatalogEntryDetail {
  source: string;
  sourceModelId: string;
  channel: string;
  mode: string;
  displayName: string;
  contextLength: number;
  maxInputTokens: number;
  maxOutputTokens: number;
  inputCostPerToken: number;
  outputCostPerToken: number;
  cacheReadCostPerToken: number;
  cacheWriteCostPerToken: number;
  imageCost: number;
  audioInputCostPerToken: number;
  audioOutputCostPerToken: number;
  requestCost: number;
  raw: unknown;
}

export interface ModelCatalogDetail {
  model: ModelCatalogItem;
  pricing: Record<string, number>;
  entries: ModelCatalogEntryDetail[];
}

export interface ModelCatalogSyncResult {
  synced: boolean;
  skipped: boolean;
  message: string;
  snapshot: ModelCatalogSnapshot;
}
