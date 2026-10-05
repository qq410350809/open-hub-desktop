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

export interface OpenedChromeSession {
  profileId: string;
  profileName: string;
  accountName: string;
}

export interface OpenUrlInChromeSessionsResult {
  opened: number;
  attempted: number;
  profiles: OpenedChromeSession[];
  errors: string[];
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
  /** 仅包含本次新增的站点 id；类型检测只针对新增站点 */
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
  /** 首次入库时间（后端 CURRENT_TIMESTAMP，UTC 无时区） */
  firstSeenAt?: string;
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

export interface CharitySyncLogFeedDetail {
  id: string;
  name: string;
  status: string;
  new: number;
  updated: number;
}

/** 单标签行：new/updated/unread；汇总行：totalNew/totalUpdated/feeds；请求行（最新话题/最新帖子）：kind/items + new/updated */
export interface CharitySyncLogDetail {
  kind?: string;
  items?: number;
  new?: number;
  updated?: number;
  unread?: number;
  /** 合计新增/更新帖数（同帖命中多标签去重后） */
  totalNew?: number;
  totalUpdated?: number;
  /** 按标签行计的原始合计（不去重），供明细表合计行展示 */
  totalNewRows?: number;
  totalUpdatedRows?: number;
  feeds?: CharitySyncLogFeedDetail[];
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
  detail?: CharitySyncLogDetail | null;
}

export interface CharityFeedTag {
  id: string;
  name: string;
  enabled?: boolean;
  sortOrder?: number;
  upstreamProtocol?: string;
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
export type ProxySortMode = "latency" | "speed" | "name";

export interface Preferences {
  theme: ThemePreference;
  defaultRunawayFilter: string;
  defaultUsageFilter: string;
  proxyNodeViewMode: ProxyNodeViewModePreference;
  proxyNodeSortMode: ProxySortMode;
  sidebarCollapsed: boolean;
  /** 站点账号别名:键 = 站点ID:profileId(或账号名),值 = 用户设置的别名 */
  accountAliases: Record<string, string>;
  /** Agent 配置页的身份模式:按工具记(Missing = 常规模式) */
  agentIdentityModes: Record<string, LocalToolIdentityMode>;
  /** Agent 配置页模型级与父级参数持久化配置 */
  agentModelConfigs?: Record<string, {
    parentConfigs?: Record<string, {
      contextWindow?: number | null;
      maxOutput?: number | null;
      defaultReasoningEffort?: string;
      efforts?: string[];
    }>;
    childOverrides?: Record<string, {
      contextWindow?: number | null;
      maxOutput?: number | null;
      defaultReasoningEffort?: string;
      efforts?: string[];
    }>;
  }>;
}

export interface MihomoKernelStatus {
  installed: boolean;
  path: string;
  version: string;
  isCustom: boolean;
  latestVersion?: string | null;
}

export interface MihomoDownloadProgress {
  stage: string;
  progress: number;
  message: string;
}

// —— 站点模型缓存（与 Rust 侧 models.rs 的同名结构对齐，camelCase）——

export interface SiteModelItem {
  id: string;
  ownedBy?: string;
}

/**
 * 模型健康度（NewAPI `/api/perf-metrics/summary`）。
 *
 * 注意两点口径：
 * 1. 这是**全站聚合**数据，按模型统计所有用户近 windowHours 小时的
 *    成功/失败、延迟与出字速度，与当前用户、当前 Key 无关。它只是健康度
 *    标签，不代表「这个 Key 能用这个模型」——那要看该 Key 的 `/v1/models` 列表。
 * 2. 站点**只为窗口内确有请求的模型**下发条目，所以「有这条记录」本身就等于
 *    「近 windowHours 小时有流量」。模型不在这张表里就是没有数据，界面不显示徽标。
 */
export interface SiteModelHealth {
  /** 近窗口请求成功率，0~1（已从站点的 0~100 归一化）。 */
  successRate?: number;
  /** 平均响应延迟（毫秒）。 */
  avgLatencyMs?: number;
  /** 平均出字速度（tokens/s）。 */
  avgTps?: number;
  /** 统计窗口小时数。 */
  windowHours?: number;
  /** 近窗口请求总数。用于提示「成功率的分母是多少」，站点没下发时缺省。 */
  requests?: number;
  /**
   * 窗口起点（Unix 秒）。界面按它与 `windowHours` 把窗口等分成
   * `MODEL_STATUS_SLOT_COUNT` 格，对号入座后缺槽代表无流量。
   */
  windowStart?: number;
  /** 逐时段成功率（0~1）。只含窗口内确有流量的时段，缺槽代表无流量。 */
  series?: SiteModelHealthPoint[];
}

/** 模型健康度逐时段采样点：一个时间格一条。 */
export interface SiteModelHealthPoint {
  /** 该采样点所属时段起点的 Unix 秒。 */
  ts: number;
  /** 该时段请求成功率，0~1（已从站点的 0~100 归一化）。 */
  successRate?: number;
  /** 该时段请求总数；站点没下发请求数时缺省。 */
  requests?: number;
}

export interface SiteModelCacheAccount {
  profileId: string;
  profileName: string;
  accountName: string;
  username: string;
  keys: string[];
  keyGroups?: Record<string, string>;
  keyModels?: Record<string, SiteModelItem[]>;
  /** 本账号同步时取到的全站模型健康度（模型 ID → 健康度）。 */
  modelHealth?: Record<string, SiteModelHealth>;
  error?: string;
}

export interface SiteModelCache {
  models: SiteModelItem[];
  apiSource?: string;
  accounts: SiteModelCacheAccount[];
  /** 各账号行合并后的站点级健康度视图（模型 ID → 健康度）。 */
  modelHealth?: Record<string, SiteModelHealth>;
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
  { value: "baiheibai", text: "白与黑" },
];

/** 去除空白/中划线/下划线并转小写，用于跨新旧命名比较。 */
export function normalizeSystemType(raw: string): string {
  return raw.trim().toLocaleLowerCase().replace(/[\s_\-]/g, "");
}

/** 系统类型的友好展示名（如 newapi2 → NewAPI · 刷新令牌）；未知类型回退为原始值。 */
export function systemTypeLabel(raw: string): string {
  if (isExplicitUnknownSystemType(raw)) return "未知类型";
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

/**
 * 未知架构站点：systemType 为空或不在已知集合。
 * 这类站点无法识别签到/额度接口，会话同步仅建立 Chrome 账号关联，不查询签到与余额。
 */
export function isUnknownSystemType(raw: string): boolean {
  const normalized = normalizeSystemType(raw);
  return !normalized || !KNOWN_SYSTEM_TYPES.has(normalized);
}

/**
 * 用户在站点表单里显式选的「未知类型」写入值。
 *
 * 与空串（从未设置）是两种意图：空串留给程序按浏览器里的痕迹自动识别架构，
 * 显式未知则是用户声明「这不是任何已知架构」，后端据此跳过所有架构推断，
 * 不会拿 NewAPI 的流程和报错去套一个用户已否认的站点。
 */
export const EXPLICIT_UNKNOWN_SYSTEM_TYPE = "unknown";

/** 是否为显式选择的「未知类型」（区别于空串＝未设置、允许自动识别）。 */
export function isExplicitUnknownSystemType(raw: string): boolean {
  return raw.trim().toLowerCase() === EXPLICIT_UNKNOWN_SYSTEM_TYPE;
}

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

/**
 * 是否支持账号级「站点令牌」维护入口。
 *
 * 现阶段只有「白与黑」这一站点类型有：它的登录凭据不来自浏览器会话，
 * 由用户自己把站点令牌贴进来，之后账号与 Key 同步都用它鉴权。其它架构
 * 的凭据一律来自 Chrome 会话（Cookie / Local Storage），挂这个入口只会
 * 让用户以为填了就能生效。
 */
export function supportsSiteToken(raw: string): boolean {
  return normalizeSystemType(raw) === "baiheibai";
}

/**
 * 是否为「白与黑」架构。
 *
 * 该架构没有签到集成：自动签到、签到状态展示、签到地址等一律不适用，
 * 表单与界面据此隐藏签到相关内容（后端 normalize 也会清零对应字段）。
 */
export function isBaiheibaiSystem(raw: string): boolean {
  return normalizeSystemType(raw) === "baiheibai";
}

/**
 * 是否支持通过 Chrome 会话发现并同步 API Key 与模型（NewAPI 系、Sub2API 与「白与黑」）。
 *
 * 只有这类站点的余额/用量能走 Key 接口，所以“从未同步过 Key 的有效账号”必须纳入
 * 首次同步，否则它的 Key 缓存永远是空的：Sub2API 只能退回会话令牌，
 * 令牌一过期整站就报 401（同一站点两个 Chrome 账号一个成功、一个失败）。
 * 未知架构没有这些接口，纳入只会换来一串错误，因此排除。
 */
export function supportsKeyDiscovery(raw: string): boolean {
  return isNewApiCompatible(raw) || normalizeSystemType(raw) === "sub2api" || isBaiheibaiSystem(raw);
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

export interface ProxyIpInfo {
  ip: string;
  kind: "hosting" | "residential" | "mobile" | "unknown";
  isp: string;
  organization: string;
  asn: string;
  source: string;
  checkedAt: string;
  status: "success" | "error";
  error: string;
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
  ipInfo?: ProxyIpInfo | null;
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
  port?: number;
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

/** Clash 订阅分享信息：本机订阅链接 + 达标节点统计 */
export interface ClashSubscriptionInfo {
  token: string;
  port: number;
  url: string;
  eligibleCount: number;
  totalCount: number;
  maxLatencyMs: number;
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
  /** 请求轮次标识；旧服务可缺失，但新测速会话只接收完全匹配的事件。 */
  runId?: string;
  phase: "started" | "completed" | "ip-info";
  /** 连通指标：GET 响应头到达耗时（TTFB） */
  latencyMs: number | null;
  /** 网速指标：body 下载完成的总耗时；未下载完（超时/失败）为 null */
  speedMs?: number | null;
  primaryIp?: string | null;
  ipInfo?: ProxyIpInfo | null;
  status: string;
  /** latency=普通延迟测速；connectivity=通道测速连通阶段；speed=通道测速网速阶段 */
  stage?: "latency" | "connectivity" | "speed" | string;
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
  /** 项目键：最近一层项目根的绝对路径，或来源标签。 */
  projectKey: string;
  /** 项目根之上最外层标记目录（Maven 聚合父目录 / 外层仓库）；无则为空。 */
  workspaceRoot?: string;
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
  /** 支持项目维度的数据源由 OpenHub 直接填充：本地为项目根绝对路径或来源标签，反代为渠道名。 */
  projectKey?: string;
  /** 项目根之上最外层标记目录；无则为空。反代模式恒为空。 */
  workspaceRoot?: string;
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

// —— 模型映射：Token 统计原始名 → 正式模型（AI 分析 / 手工确认） ——
export interface TokenModelMapping {
  rawKey: string;        // 小写、去厂商前缀后的原始名（与后端 raw_key 一致）
  rawModel: string;      // 首次见到的原始模型名
  officialModel: string; // 正式模型名；空串表示尚未确定
  officialSlug: string | null;
  lab: string | null;
  origin: "rule" | "ai" | "manual";
  confidence: number;
  reason: string | null;
  reviewStatus: "pending" | "suggested" | "approved" | "rejected";
  /** 兼容旧接口；仅 reviewStatus 为 approved 时为 true。 */
  confirmed: boolean;
  updatedAt: string;
}

export interface TokenMappingAnalyzeReport {
  analyzed: number;         // 本次实际送给 AI 的条目数
  skippedConfirmed: number; // 因已确认而跳过的条目数
  resolved: number;         // 通过本地校验、等待人工审核的建议数
  rejectedInvalid: number;  // 越批、候选不合法或置信度不合法而被拒绝的条目数
  standardsUsed: number;    // 注入提示词的已批准标准映射条数
  unresolved: string[];     // AI 未能给出正式模型的原始名
  warnings: string[];
}

export interface TokenMappingAnalyzeProgress {
  stage: string;
  processed: number;
  total: number;
  message: string;
}

/** Token 统计的正式模型清单（用户手工添加 + AI 自动学习 + 数据迁移）。 */
export interface TokenOfficialModel {
  id: string;
  name: string;
  lab: string;
  aliases: string[];
  source: string;
  confidence: number;
  createdAt: string;
  updatedAt: string;
  /**
   * 是否为「原厂模型」：模型目录中存在该模型的官方（原厂）渠道
   * （目录 `officialHostCount > 0`，按注册表 id 或 name 匹配）。
   *
   * 目录未同步时为 `false`。映射目标下拉据此过滤纯三方/转售变体；
   * 用户手工添加的条目恒为 `true`。
   */
  firstParty: boolean;
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
  /** 反代模式：所选区间之前的历史健康桶（健康矩阵前置补位取数）；本地模式为空 */
  precedingBuckets?: RequestHealthBucket[];
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

export interface ModelCatalogProvider {
  id: string;
  name: string;
  npm?: string | null;
  api?: string | null;
  doc?: string | null;
  tier?: string | null;
  subscription: boolean;
  count: number;
  dateModified?: string | null;
  /**
   * 该渠道是否为某个 lab 的自营（原厂）渠道。
   *
   * ⚠️ 与 `tier` 无关：`tier === 'lab'` 只说明渠道自身是模型厂商/自营云，
   * 不代表它是任意模型的原厂渠道。例如 `nvidia`（tier=lab）代售 30+ 个 lab 的模型，
   * `azure`（tier=lab）对上架 microsoft 模型是原厂、对上架 openai 模型却不是。
   */
  isFirstParty: boolean;
  /** 该渠道要求的 API Key 环境变量名（models.dev `providers[*].env`），如 `["OPENAI_API_KEY"]`。 */
  env?: string[];
}

export interface ModelCatalogSourceStatus {
  source: string;
  url: string;
  fetchedAt: string;
  recordCount: number;
}

export interface ModelCatalogItem {
  id: string;
  slug: string;
  name: string;
  lab: string;
  kind: string;
  family?: string | null;
  knowledge?: string | null;
  status: string;
  openWeights: boolean;
  reasoning: boolean;
  toolCall: boolean;
  attachment: boolean;
  structured: boolean;
  temperature: boolean;
  inputModalities: string[];
  contextLength: number;
  contextMin: number;
  contextMax: number;
  maxOutputTokens: number;
  refProvider?: string | null;
  refOfficial: boolean;
  refInputCost: number;
  refOutputCost: number;
  refCacheReadCost: number;
  minProvider?: string | null;
  minInputCost: number;
  minOutputCost: number;
  minCacheReadCost: number;
  priceSpread: number;
  blendedMin?: number | null;
  blendedTrusted?: number | null;
  blendedRef?: number | null;
  hostCount: number;
  pricedHostCount: number;
  freeHostCount: number;
  subHostCount: number;
  hostProviders: string[];
  aaIdx?: number | null;
  aaCoding?: number | null;
  aaAgentic?: number | null;
  aaSpeed?: number | null;
  aaTtft?: number | null;
  aaTaskCost?: number | null;
  benchmarkCount: number;
  releaseDate?: string | null;
  lastUpdated?: string | null;

  // ───────── 模型身份：原始 lab / 原始 modelId ─────────
  /**
   * 归一化后的**原始 lab**。来自 models.dev canonical 层、llmpricing 的 `lab`，
   * 或关键词/同名继承推断。**未确定时保持 `'misc'`**，绝不臆造。
   *
   * 与 `lab` 的区别：`lab` 是 llmpricing 原始字段（1941 个模型中有 175 个是 `misc`），
   * `officialLab` 在此基础上补全了 103 个；剩余 72 个是白牌/路由名（`auto`、`model-router`
   * 等），保持 `misc` 并置 `identityResolved = false`。
   */
  officialLab: string;
  /** 原始 modelId（**保留上游原拼写**，如 `z-ai/glm-5.2`、`MiniMax-M3`）。 */
  officialModelId: string;
  /** models.dev canonical id（如 `zhipuai/glm-5.2`），未定位到时为 null。 */
  canonicalId?: string | null;
  /**
   * 身份来源。`canonical` / `llmpricing_lab` 为上游直接给出；
   * `keyword_rule` / `inherited` / `canonical_reverse` 为本地推断；`unknown` 为未确定。
   */
  identitySource:
    | 'canonical'
    | 'llmpricing_lab'
    | 'keyword_rule'
    | 'inherited'
    | 'canonical_reverse'
    | 'unknown';
  /** 身份是否已确定。`false` 表示 `officialLab` 仍是 `'misc'` 占位。 */
  identityResolved: boolean;

  // ───────── 官网 / 三方渠道分层 ─────────
  /**
   * 官方（原厂）渠道数量。
   *
   * 判定：`provider` 属于该 lab 的原厂别名表 **且** 该渠道确实上架了此模型。
   * 两个条件缺一不可——`openai/gpt-oss-120b` 不在 `openai` 渠道上架，官方渠道数即为 0。
   * 实测与 llmpricing 详情页 `official` 标记 23/23 一致。
   */
  officialHostCount: number;
  /**
   * 官方（原厂）渠道 id 列表（已去重、已排序）。
   *
   * 前端据此即可在**不拉取详情**的情况下做「仅官网渠道」筛选与价格对比。
   */
  officialChannelProviders: string[];
  /** `tier === 'lab'` 的渠道数量（描述渠道自身性质，不等于官方渠道）。 */
  labTierHostCount: number;
  /** `tier === 'cloud'` 的渠道数量。 */
  cloudTierHostCount: number;
  /** `tier === 'gateway'` 的渠道数量。 */
  gatewayTierHostCount: number;

  // ───────── 免费渠道（本地推导，替代 HTML 爬取）─────────
  /**
   * 本地推导的免费渠道数量（已剔除订阅制渠道）。
   *
   * 判定：`input === 0 && output === 0 && !subscription`，其中「零价」取
   * **任一变体零价即算**（同一渠道可能同时登记付费与 `:free` 两个条目，
   * 实测 `unorouter` 对 glm-5.2 即如此）。
   *
   * 与 `freeHostCount` 的关系：`freeHostCount` 是 llmpricing 上游聚合值（权威），
   * 本字段是本地逐渠道推导值，实测一致率 97.4%。
   */
  freeChannelCount: number;
  /** 本地推导的免费渠道 id 列表（已去重）。 */
  freeChannelProviders: string[];
  /**
   * 订阅制渠道 id 列表（已去重）。
   *
   * 订阅渠道「用时不另计费」，与免费渠道语义不同，**不计入** `freeChannelCount`。
   */
  subscriptionChannelProviders: string[];
  /** 免费渠道数据来源：`derived`（本地推导）/ `upstream`（仅上游聚合值，无明细）。 */
  freeChannelSource: 'derived' | 'upstream';
  /**
   * 本地推导的免费渠道数是否与上游 `freeHostCount` 一致。
   * 不一致时以 `freeHostCount` 为准，本字段供 UI 提示口径差异。
   */
  freeChannelCountMatches: boolean;

  // ───────── 思考级别与输出能力（models.dev 渠道层聚合）─────────
  /**
   * 模型级思考级别选项（跨全部渠道取并集）。
   *
   * 两种形态：`{kind:'toggle'}`（只能开/关）与
   * `{kind:'effort', values:['none','low','medium','high','xhigh','max']}`（多档位）。
   */
  reasoningOptions?: ReasoningOption[];
  /** 模型支持的最高思考档位；无 effort 档位时为 null。 */
  reasoningEffortMax?: string | null;
  /** 模型级输出模态（跨渠道取并集），可能是 `image` / `video` / `audio`。 */
  outputModalities?: string[];
  /** 模型级最大输入上限（`limit.input`），总上下文可能大于单次可输入量。 */
  maxInputTokens?: number | null;
  /** 支持交错推理的读取字段名（跨渠道去重，如 `reasoning_content`）。 */
  interleavedFields?: string[];
  /** 是否有渠道提供「快速模式」（额外计费档）。 */
  hasFastMode?: boolean;
  /** models.dev canonical 层附加信息（描述 / 许可证 / 链接 / 权重 / 基准明细）。 */
  modelsDevExtras?: ModelsDevModelExtras;
}

/** models.dev `reasoning_options` 的元素。 */
export interface ReasoningOption {
  /** `toggle`（开/关）或 `effort`（多档位）。 */
  kind: 'toggle' | 'effort' | string;
  /** `effort` 形态下的可选档位；`toggle` 形态为空。 */
  values: string[];
}

/** models.dev canonical 层附加信息（仅详情页使用，打包为 JSON 列）。 */
export interface ModelsDevModelExtras {
  description?: string | null;
  license?: string | null;
  /** 官方链接：`[{label, url, type}]`（type 如 `paper` / `model_card`）。 */
  links: any[];
  /** 权重下载：`[{label, url, quantization?}]`。 */
  weights: any[];
  /** 基准测试明细：`[{name, score, metric, source?, date?, harness?}]`。 */
  benchmarks: any[];
}

/**
 * 单个模型的 models.dev 能力（`get_model_capabilities` 的返回单元）。
 *
 * 供「模型参数」按模型呈现思考档位与上下文/输出上限等属性，
 * 字段与 [`ModelCatalogItem`] 的富化子集一致。
 */
/** 目录匹配强度（与 Rust `nearest::MatchKind` 的 `as_str()` 一一对应）。 */
export type CapabilityMatchKind = "exact" | "variant" | "alias" | "nearest";

export interface ModelCapabilities {
  reasoningOptions: ReasoningOption[];
  reasoningEffortMax?: string | null;
  contextLength: number;
  maxOutputTokens: number;
  maxInputTokens?: number | null;
  outputModalities: string[];
  interleavedFields: string[];
  hasFastMode: boolean;
  supportsTemperature: boolean;
  supportsToolCall: boolean;
  supportsStructuredOutput: boolean;
  supportsReasoning: boolean;
  openWeights: boolean;

  /** 命中的目录条目 id（`lab/modelId`）。 */
  matchedId?: string;
  /** 该条目声明的原始 model id（上游原拼写）。 */
  matchedModelId?: string;
  /** 匹配强度；非 exact 时 UI 加「≈」前缀披露这是近似命中。 */
  matchKind?: CapabilityMatchKind;
  /** 强度指示（全等类恒 1.0，nearest 为 Dice 系数）。不是概率，仅用于提示。 */
  matchScore?: number;
  /**
   * 匹配算法版本。缓存值版本不符时必须作废重取：
   * miss 的 key 会被缓存为 null，不作废就永远看不到新算法的改善。
   */
  matchVersion?: number;
}

export interface ModelCatalogSnapshot {
  models: ModelCatalogItem[];
  providers: ModelCatalogProvider[];
  total: number;
  lastSyncedAt: string;
  syncedToday: boolean;
  sources: ModelCatalogSourceStatus[];
  meta: Record<string, any>;
}

export interface ModelCatalogHostItem {
  provider: string;
  name: string;
  modelId?: string | null;
  tier?: string | null;
  subscription: boolean;
  input?: number | null;
  output?: number | null;
  cacheRead?: number | null;
  cacheWrite?: number | null;
  context?: number | null;
  outputLimit?: number | null;
  /** 独立输入上限（models.dev `limit.input`）。总上下文可能大于单次可输入量。 */
  inputLimit?: number | null;
  /** 输出模态（models.dev `modalities.output`），可能是 `image` / `video` / `audio`。 */
  outputModalities?: string[];
  /** 该渠道声明的思考级别选项（可能与模型级聚合声明不一致，分开展示）。 */
  reasoningOptions?: ReasoningOption[];
  /** 交错推理的读取字段名（如 `reasoning_content`）。 */
  interleavedField?: string | null;
  /** 快速模式（`experimental.modes.fast`），含额外计费与请求覆盖。 */
  fastMode?: any | null;
  status?: string | null;
  official: boolean;
  doc?: string | null;
  isFree: boolean;
  isMin: boolean;
  isRef: boolean;
}

export interface ModelCatalogDetail {
  model: ModelCatalogItem;
  providers: ModelCatalogProvider[];
  hosts: ModelCatalogHostItem[];
  raw: any;
}

export interface ModelCatalogSyncResult {
  synced: boolean;
  skipped: boolean;
  message: string;
  snapshot: ModelCatalogSnapshot;
}

export interface GeoipStatus {
  installed: boolean;
  path: string;
  fileSize: number;
  fileSizeFormatted: string;
  updatedAt?: string | null;
}

export interface GeoipDownloadProgress {
  stage: string;
  progress: number;
  message: string;
}


// —— 本地 AI 编程工具模型配置管理（local_tools）——
export type LocalToolId =
  | "claude"
  | "codex"
  | "opencode"
  | "zcode"
  | "antigravity"
  | "dsh";

/** 工具如何使用供应商：一路接入 / 一次切一家 / 一次加载全部。 */
export type LocalToolProviderMode = "single" | "switch" | "all";

export interface LocalToolProviderEntry {
  id: string;
  name: string;
  baseUrl: string;
  apiKey?: string;
  protocol: string;
  models: string[];
}

export interface LocalToolModelEntry {
  id: string;
  name: string;
  provider: string;
  contextWindow: number;
  maxOutput: number;
  /** 逐模型默认思考级别（目录给「该模型最高可配置档位」）；缺省表示不指定。 */
  reasoningEffort?: string;
}

export interface LocalToolDefaultsSection {
  model: string;
  provider: string;
  reasoningEffort: string;
  reasoningEffortOptions: string[];
  perModelEffort: Record<string, string>;
}

export interface LocalToolContextSection {
  contextWindow: number | null;
  autoCompactTokenLimit: number | null;
  maxOutputTokens: number | null;
  maxThinkingTokens: number | null;
}

export interface LocalToolThinkingSection {
  effortLevel: string;
  effortLevelOptions: string[];
  maxThinkingTokens: number | null;
}

/** 配置文件相对本软件上次写入的状态：无标识 / 指纹吻合（未被动过）/ 被外部改过 */
export type LocalToolManagedState = "unmanaged" | "intact" | "modified";

export interface LocalToolConfigFile {
  kind: string;
  label: string;
  path: string;
  exists: boolean;
  managed: LocalToolManagedState;
}

export interface LocalToolConfigSnapshot {
  tool: string;
  toolName: string;
  files: LocalToolConfigFile[];
  providers: LocalToolProviderEntry[];
  models: LocalToolModelEntry[];
  defaults: LocalToolDefaultsSection;
  context: LocalToolContextSection;
  thinking: LocalToolThinkingSection;
  contentHash: string;
  effectNote: string;
  warning: string;
  providerMode: LocalToolProviderMode;
}

export interface LocalToolOverview {
  tool: string;
  toolName: string;
  detected: boolean;
  hasTokenRecords: boolean;
  collectedSessions: number;
  collectedEvents: number;
  root: string;
  providerCount: number;
  defaultModel: string;
  effectNote: string;
  providerMode: LocalToolProviderMode;
}

export interface LocalToolListReport {
  available: boolean;
  home: string;
  tools: LocalToolOverview[];
  collectedAt: string;
}

export interface LocalToolConfigPatch {
  baseHash: string;
  providers: LocalToolProviderEntry[];
  models: LocalToolModelEntry[];
  defaults: LocalToolDefaultsSection;
  context: LocalToolContextSection;
  thinking: LocalToolThinkingSection;
}

export interface LocalToolConfigSaveResult {
  snapshot: LocalToolConfigSnapshot;
  /** 本次创建的备份名；全部文件指纹吻合（直接覆盖）时为空 */
  backupName: string;
  /** 本次被备份的文件（无标识或被外部改过） */
  backedUp: string[];
}

export interface LocalToolBackupEntry {
  name: string;
  fileLabel: string;
  size: number;
  createdAt: string;
}

/** 一致性比对目标：key 为行标识（或整单标识），patch 为该行组装出的配置。 */
export interface LocalToolDiffTarget {
  key: string;
  patch: LocalToolConfigPatch;
}

/** 单行比对结果。 */
export interface LocalToolDiffEntry {
  key: string;
  /** 磁盘现状与这份配置的预期结果一致（= 该行已生效） */
  consistent: boolean;
  /** 差异说明；一致时为空 */
  differences: string[];
}

/** 比对报告。 */
export interface LocalToolDiffReport {
  tool: string;
  toolName: string;
  /** 磁盘上现有的受管供应商标识 */
  managedProviders: string[];
  /** 磁盘上现有的受管供应商名下模型 ID */
  managedModels: string[];
  entries: LocalToolDiffEntry[];
}

/** 反代清单的身份模式：按「渠道·账号·Key」逐条，或按「模型 × 站点」。 */
export type LocalToolIdentityMode = "channel" | "model";
