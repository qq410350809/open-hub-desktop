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
export type FontFamilyPreference = string;
export type FontSizePreference = "small" | "medium" | "large";

export interface Preferences {
  theme: ThemePreference;
  fontFamily: FontFamilyPreference;
  fontSize: FontSizePreference;
  defaultRunawayFilter: string;
  defaultUsageFilter: string;
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
  favorite: false,
  hidden: false,
  updatedAt: "",
});
