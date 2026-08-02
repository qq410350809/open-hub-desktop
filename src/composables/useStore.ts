import { computed, ref, type ComputedRef } from "vue";
import { openUrl } from "@tauri-apps/plugin-opener";
import { runCommand, useLibrary } from "./useLibrary";
import { usePreferences } from "./usePreferences";
import { useToast } from "./useToast";
import type {
  AddressItem,
  ChromeSessionInfo,
  ChromeSessionValue,
  ChromeUsageScanResult,
  RemoteUserInfo,
  SiteLinkKind,
  SiteRecord,
  SyncLogEntry,
  SyncProgressStatus,
  SyncRunState,
  SyncSitesProgress,
  SyncSitesResult,
} from "../types";

const isTauri = "__TAURI_INTERNALS__" in window;

// ============================================================
//  全局单例状态 —— 在模块作用域定义，所有组件共享同一份
// ============================================================
const { sites, suggestedTags, usageSites, loading, loadLibrary } = useLibrary();
const { preferences, updatePreferences } = usePreferences();
const { showToast } = useToast();

// 视图状态
const runawayFilter = ref(preferences.defaultRunawayFilter);
const usageFilter = ref(preferences.defaultUsageFilter);
const query = ref("");
const tag = ref("all");
const level = ref("all");
const feature = ref("all");
const systemTypeFilter = ref("all");

// 页面状态
const page = ref<"library" | "models" | "settings">("library");
const editingId = ref<string | null>(null);
const activeTab = ref<"basic" | "features" | "maintenance">("basic");

// 弹窗状态
const modalOpen = ref(false);
const linkDialogOpen = ref(false);
const previewDialogOpen = ref(false);
const chromeSessionDialogOpen = ref(false);
const syncDialogOpen = ref(false);
const siteModelsDialogOpen = ref(false);

// 链接弹窗数据
const linkDialogKind = ref<SiteLinkKind>("api");
const linkDialogSite = ref<SiteRecord | null>(null);
const linkDialogTrigger = ref<HTMLElement | null>(null);

// 预览弹窗数据
const previewSite = ref<SiteRecord | null>(null);
const previewTrigger = ref<HTMLElement | null>(null);

// 站点模型弹窗数据
const siteModelsSite = ref<SiteRecord | null>(null);

// Chrome 会话弹窗数据
const chromeSessionSite = ref<SiteRecord | null>(null);
const chromeSessionTrigger = ref<HTMLElement | null>(null);
const chromeSessions = ref<ChromeSessionInfo[]>([]);
const chromeSessionsLoading = ref(false);
const chromeSessionsError = ref("");
const chromeSessionCopyingProfileId = ref("");
const chromeBrowserSyncingProfileId = ref("");
const chromeBrowserSyncError = ref("");
const chromeBrowserSyncLogs = ref<SyncLogEntry[]>([]);
const chromeBrowserSyncElapsedMs = ref(0);
const chromeUsageScanning = ref(false);
const chromeUsageScanResult = ref<ChromeUsageScanResult | null>(null);
const syncingSites = ref(false);
const syncingModelKeys = ref(false);
const modelKeySyncCompleted = ref(0);
const modelKeySyncTotal = ref(0);
const syncRunState = ref<SyncRunState>("idle");
const syncLogs = ref<SyncLogEntry[]>([]);
const syncElapsedMs = ref(0);
const remoteUser = ref<RemoteUserInfo | null>(null);
const remoteUserLoading = ref(false);
const remoteUserError = ref("");
const syncDialogRunaway = ref(false);
const syncDialogMode = ref<"remote" | "sessions" | "models">("remote");
const syncDialogSiteIds = ref<string[]>([]);
let chromeSessionRequestId = 0;
let chromeBrowserSyncRunId = 0;
let chromeBrowserSyncLogId = 0;
let chromeBrowserSyncStartedAt = 0;
let chromeBrowserSyncTimer: number | null = null;
let remoteUserRequestId = 0;
let syncRunId = 0;
let syncLogId = 0;
let syncStartedAt = 0;
let syncTimer: number | null = null;

const remoteLoginUrl = "https://ldoh.105117.xyz/";

// ============================================================
//  计算属性
// ============================================================
const allTags: ComputedRef<string[]> = computed(() => [
  ...new Set([...suggestedTags.value, ...sites.value.flatMap((site) => site.tags)]),
]);

const chromeUsageAccounts: ComputedRef<Record<string, ChromeSessionInfo[]>> = computed(() =>
  Object.fromEntries(
    usageSites.value.map((site) => [site.siteId, site.sessions]),
  ),
);

const filteredSites: ComputedRef<SiteRecord[]> = computed(() => {
  const q = query.value.trim().toLocaleLowerCase("zh-CN");
  return sites.value
    .filter((site) => {
      if (runawayFilter.value === "active" && site.isRunaway) return false;
      if (runawayFilter.value === "runaway" && !site.isRunaway) return false;
      if (usageFilter.value === "personal" && !site.isPersonal) return false;
      if (usageFilter.value === "unused" && site.isPersonal) return false;
      if (tag.value !== "all" && !site.tags.includes(tag.value)) return false;
      if (level.value !== "all" && site.registrationLimit !== Number(level.value)) return false;
      if (!matchesFeature(site, feature.value)) return false;
      const siteSystemType = site.systemType.trim().toLocaleLowerCase();
      if (
        systemTypeFilter.value === "unknown" &&
        ["newapi", "sub2api"].includes(siteSystemType)
      ) return false;
      if (
        !["all", "unknown"].includes(systemTypeFilter.value) &&
        siteSystemType !== systemTypeFilter.value
      ) return false;
      const content = [
        site.name,
        site.apiBaseUrl,
        site.description,
        site.rateLimit,
        ...site.tags,
        ...site.maintainers.map((item) => item.name),
      ]
        .join(" ")
        .toLocaleLowerCase("zh-CN");
      return !q || content.includes(q);
    })
    .sort(
      (a, b) =>
        Number(b.isPersonal) - Number(a.isPersonal) ||
        new Date(b.updatedAt).getTime() - new Date(a.updatedAt).getTime(),
    );
});

const activeCount: ComputedRef<number> = computed(() => sites.value.filter((site) => !site.isRunaway).length);
const runawayCount: ComputedRef<number> = computed(() => sites.value.filter((site) => site.isRunaway).length);
const personalCount: ComputedRef<number> = computed(() => sites.value.filter((site) => site.isPersonal).length);

const hasFilters: ComputedRef<boolean> = computed(
  () =>
    Boolean(query.value) ||
    tag.value !== "all" ||
    level.value !== "all" ||
    feature.value !== "all" ||
    systemTypeFilter.value !== "all",
);

const editingSite: ComputedRef<SiteRecord | null> = computed(() =>
  editingId.value ? sites.value.find((site) => site.id === editingId.value) ?? null : null,
);

// ============================================================
//  操作函数
// ============================================================
function clearFilters() {
  query.value = "";
  tag.value = "all";
  level.value = "all";
  feature.value = "all";
  systemTypeFilter.value = "all";
}

function setRunawayFilter(filter: string) {
  runawayFilter.value = filter;
  updatePreferences({ defaultRunawayFilter: filter });
}

function setUsageFilter(filter: string) {
  usageFilter.value = filter;
  updatePreferences({ defaultUsageFilter: filter });
}

function openSettings() {
  page.value = "settings";
}

function closeSettings() {
  page.value = "library";
}

function openModels() {
  page.value = "models";
}

function openLibrary() {
  page.value = "library";
}

function openSiteModelsDialog(site: SiteRecord) {
  siteModelsSite.value = site;
  siteModelsDialogOpen.value = true;
}

function closeSiteModelsDialog() {
  siteModelsDialogOpen.value = false;
  siteModelsSite.value = null;
}

function openModal(site?: SiteRecord) {
  editingId.value = site?.id ?? null;
  activeTab.value = "basic";
  modalOpen.value = true;
}

function closeModal() {
  modalOpen.value = false;
  editingId.value = null;
}

async function refreshRemoteUser() {
  const requestId = ++remoteUserRequestId;
  remoteUser.value = null;
  remoteUserError.value = "";
  remoteUserLoading.value = true;
  try {
    const user = await runCommand<RemoteUserInfo>("get_remote_user");
    if (requestId !== remoteUserRequestId || !syncDialogOpen.value) return;
    remoteUser.value = user;
  } catch (error) {
    if (requestId !== remoteUserRequestId || !syncDialogOpen.value) return;
    remoteUserError.value = String(error);
  } finally {
    if (requestId === remoteUserRequestId) remoteUserLoading.value = false;
  }
}

function openSyncDialog() {
  if (syncDialogOpen.value) return;
  syncRunId += 1;
  resetSyncLog();
  syncDialogRunaway.value = runawayFilter.value === "runaway";
  syncDialogMode.value = usageFilter.value === "personal" ? "sessions" : "remote";
  syncDialogSiteIds.value = filteredSites.value.map((site) => site.id);
  syncDialogOpen.value = true;
  if (syncDialogMode.value === "remote") void refreshRemoteUser();
}

function openModelSyncDialog() {
  if (syncDialogOpen.value) return;
  syncRunId += 1;
  resetSyncLog();
  syncDialogRunaway.value = false;
  syncDialogMode.value = "models";
  syncDialogSiteIds.value = filteredSites.value.map((site) => site.id);
  syncDialogOpen.value = true;
}

function closeSyncDialog() {
  if (syncingSites.value || syncingModelKeys.value) return;
  syncRunId += 1;
  stopSyncTimer();
  syncRunState.value = "idle";
  remoteUserRequestId += 1;
  syncDialogOpen.value = false;
  remoteUser.value = null;
  remoteUserError.value = "";
  remoteUserLoading.value = false;
  syncDialogSiteIds.value = [];
}

async function openRemoteLogin() {
  await openExternal(remoteLoginUrl);
}

function resetSyncLog() {
  stopSyncTimer();
  syncRunState.value = "idle";
  syncLogs.value = [];
  syncElapsedMs.value = 0;
  syncStartedAt = 0;
}

function startSyncTimer() {
  stopSyncTimer();
  syncStartedAt = Date.now();
  syncElapsedMs.value = 0;
  syncTimer = window.setInterval(() => {
    syncElapsedMs.value = Date.now() - syncStartedAt;
  }, 200);
}

function stopSyncTimer() {
  if (syncTimer !== null) {
    window.clearInterval(syncTimer);
    syncTimer = null;
  }
  if (syncStartedAt) syncElapsedMs.value = Date.now() - syncStartedAt;
}

function appendSyncLog(
  progress: Omit<SyncSitesProgress, "runId"> | { stage: string; status: SyncProgressStatus; message: string },
) {
  if (!syncDialogOpen.value || syncRunState.value === "idle") return;
  if (progress.status === "error") {
    if (progress.stage === "failed") {
      for (const entry of syncLogs.value) {
        if (entry.status === "running") entry.status = "error";
      }
    } else {
      const runningEntry = [...syncLogs.value]
        .reverse()
        .find((entry) => entry.stage === progress.stage && entry.status === "running");
      if (runningEntry) runningEntry.status = "error";
    }
  } else if (progress.status === "success") {
    const runningEntry = [...syncLogs.value]
      .reverse()
      .find((entry) => entry.stage === progress.stage && entry.status === "running");
    if (runningEntry) runningEntry.status = "success";
  }
  syncLogs.value.push({
    ...progress,
    id: ++syncLogId,
    elapsedMs: syncStartedAt ? Date.now() - syncStartedAt : 0,
  });
}

function receiveSyncProgress(progress: SyncSitesProgress) {
  if (progress.runId !== syncRunId) return;
  appendSyncLog(progress);
}

function receiveNestedChromeSyncProgress(progress: SyncSitesProgress) {
  if (Math.floor(progress.runId / 10_000) !== syncRunId) return;
  appendSyncLog({
    ...progress,
    stage: `chrome-detail-${progress.runId}-${progress.stage}`,
    message: `Chrome：${progress.message}`,
  });
}

function needsChromeAccountFallback(session: ChromeSessionInfo): boolean {
  return !session.isValid || Boolean(session.syncError.trim());
}

async function syncSites() {
  if (
    syncingSites.value ||
    syncingModelKeys.value ||
    (syncDialogMode.value === "remote" && !remoteUser.value)
  ) return;
  const runaway = syncDialogRunaway.value;
  const mode = syncDialogMode.value;
  const siteIds = [...syncDialogSiteIds.value];
  const runId = ++syncRunId;
  resetSyncLog();
  syncRunState.value = "syncing";
  startSyncTimer();
  appendSyncLog({ stage: "start", status: "info", message: "同步任务已开始" });
  remoteUserError.value = "";
  if (mode === "models") {
    await syncAllModelKeys(siteIds);
    return;
  }
  syncingSites.value = true;
  try {
    if (mode === "sessions") {
      appendSyncLog({
        stage: "scope",
        status: "info",
        message: `已锁定当前列表中的 ${siteIds.length} 个在用站点`,
      });
      if (siteIds.length === 0) {
        appendSyncLog({ stage: "accounts", status: "info", message: "当前列表没有需要同步的在用站点" });
        syncRunState.value = "complete";
        stopSyncTimer();
        return;
      }
      const accountResult = await analyzeChromeUsage(false, undefined, runId, siteIds);
      if (!accountResult) throw new Error("当前列表的 Chrome 会话同步失败");
      const siteMap = new Map(sites.value.map((site) => [site.id, site]));
      const browserCandidates = accountResult.sites.flatMap((usageSite) => {
        const site = siteMap.get(usageSite.siteId);
        if (site?.systemType.toLocaleLowerCase() !== "newapi") return [];
        return usageSite.sessions
          .filter(needsChromeAccountFallback)
          .map((session) => ({ site, session }));
      });
      let browserSucceeded = 0;
      let newlyValidated = 0;
      const validSiteIds = new Set(
        accountResult.sites
          .filter((usageSite) => usageSite.sessions.some((session) => session.isValid))
          .map((usageSite) => usageSite.siteId),
      );
      const candidateGroups = new Map<string, Array<(typeof browserCandidates)[number] & { childRunId: number }>>();
      browserCandidates.forEach((candidate, index) => {
        const group = candidateGroups.get(candidate.site.id) ?? [];
        group.push({ ...candidate, childRunId: runId * 10_000 + index + 1 });
        candidateGroups.set(candidate.site.id, group);
      });
      const groupedCandidates = [...candidateGroups.values()];
      const workerCount = Math.min(3, groupedCandidates.length);
      let nextGroupIndex = 0;
      if (browserCandidates.length > 0) {
        appendSyncLog({
          stage: "chrome-parallel",
          status: "info",
          message: `需要 Chrome 回退 ${browserCandidates.length} 个账号，按站点并行处理（并发 ${workerCount}）`,
        });
      }
      const workerResults = await Promise.all(Array.from({ length: workerCount }, async () => {
        let succeeded = 0;
        let validated = 0;
        while (nextGroupIndex < groupedCandidates.length) {
          const group = groupedCandidates[nextGroupIndex++];
          for (const candidate of group) {
            const stage = `chrome-profile-${candidate.site.id}-${candidate.session.profileId}`;
            appendSyncLog({
              stage,
              status: "running",
              message: `正在通过 Chrome ${candidate.session.profileName} 同步 ${candidate.site.name}`,
            });
            try {
              await runCommand<ChromeSessionInfo>("sync_site_account_via_chrome", {
                siteId: candidate.site.id,
                profileId: candidate.session.profileId,
                runId: candidate.childRunId,
              });
              succeeded += 1;
              if (!candidate.session.isValid) validated += 1;
              validSiteIds.add(candidate.site.id);
              appendSyncLog({
                stage,
                status: "success",
                message: `${candidate.site.name} · Chrome ${candidate.session.profileName} 同步成功`,
              });
            } catch (error) {
              appendSyncLog({
                stage,
                status: "error",
                message: `${candidate.site.name} · Chrome ${candidate.session.profileName} 同步失败：${String(error)}`,
              });
            }
          }
        }
        return { succeeded, validated };
      }));
      for (const result of workerResults) {
        browserSucceeded += result.succeeded;
        newlyValidated += result.validated;
      }
      if (browserCandidates.length > 0) await loadLibrary();
      syncRunState.value = "complete";
      appendSyncLog({
        stage: "accounts",
        status: browserSucceeded === browserCandidates.length ? "success" : "error",
        message: `会话同步完成：${validSiteIds.size} 个站点、${accountResult.accounts + newlyValidated} 个账号${browserCandidates.length ? `，Chrome 验证 ${browserSucceeded}/${browserCandidates.length}` : accountResult.warnings ? `，${accountResult.warnings} 个警告` : ""}`,
      });
      stopSyncTimer();
      showToast(`已同步当前列表中的 ${siteIds.length} 个在用站点`);
      return;
    }
    const result = await runCommand<SyncSitesResult>("sync_remote_sites", { runaway, runId });
    await loadLibrary();
    const scope = result.runaway ? "跑路站点" : "存活站点";
    const account = result.userName ? `账号 ${result.userName}` : `Chrome ${result.profileName}`;
    showToast(`${account} 已同步 ${result.total} 个${scope}（新增 ${result.added}，更新 ${result.updated}）`);
    syncRunState.value = "detecting";
    appendSyncLog({
      stage: "available",
      status: "success",
      message: `站点数据已可用，共 ${result.total} 条；类型检测将在后台继续`,
    });
    void detectSyncedSiteTypes(result.siteIds, runId);
  } catch (error) {
    remoteUserError.value = `同步失败：${String(error)}`;
    syncRunState.value = "error";
    appendSyncLog({ stage: "failed", status: "error", message: remoteUserError.value });
    stopSyncTimer();
    showToast(remoteUserError.value, true);
  } finally {
    syncingSites.value = false;
  }
}

async function detectSyncedSiteTypes(siteIds: string[], runId: number) {
  if (siteIds.length === 0) {
    if (runId === syncRunId && syncDialogOpen.value) {
      appendSyncLog({ stage: "detect", status: "info", message: "本批没有需要检测的站点" });
      syncRunState.value = "complete";
      stopSyncTimer();
    }
    return;
  }
  try {
    const detected = await runCommand<number>("detect_site_system_types", { siteIds, runId });
    await loadLibrary();
    if (runId === syncRunId && syncDialogOpen.value) {
      syncRunState.value = "complete";
      stopSyncTimer();
    }
    showToast(`站点类型检测完成，已处理 ${detected} 个站点`);
  } catch (error) {
    if (runId === syncRunId && syncDialogOpen.value) {
      appendSyncLog({ stage: "detect", status: "error", message: `类型检测失败：${String(error)}` });
      syncRunState.value = "complete";
      stopSyncTimer();
    }
    showToast(`站点已同步，类型检测失败：${String(error)}`, true);
  }
}

async function saveSite(input: SiteRecord): Promise<boolean> {
  try {
    if (editingId.value) {
      await runCommand<SiteRecord>("update_site", { id: editingId.value, input });
    } else {
      await runCommand<SiteRecord>("create_site", { input });
    }
    closeModal();
    await loadLibrary();
    showToast(editingId.value ? "站点已更新" : "站点已添加");
    return true;
  } catch (error) {
    showToast(String(error), true);
    return false;
  }
}

async function importSite(siteUrl: string): Promise<SiteRecord> {
  try {
    const site = await runCommand<SiteRecord>("import_site", { siteUrl });
    closeModal();
    await loadLibrary();
    showToast(`已导入「${site.name}」${site.systemType ? `（${site.systemType}）` : ""}`);
    return site;
  } catch (error) {
    showToast(`导入失败：${String(error)}`, true);
    throw error;
  }
}

async function deleteSite(site: SiteRecord) {
  if (!window.confirm(`确定删除"${site.name}"吗？此操作会永久移除本地记录。`)) return;
  try {
    await runCommand<void>("delete_site", { id: site.id });
    await loadLibrary();
    showToast("站点已删除");
  } catch (error) {
    showToast(String(error), true);
  }
}

async function togglePersonal(site: SiteRecord) {
  await runCommand<SiteRecord>("toggle_personal", { id: site.id });
  await loadLibrary();
}

async function syncChromeSession(site: SiteRecord, trigger: HTMLElement) {
  const requestId = ++chromeSessionRequestId;
  chromeSessionSite.value = site;
  chromeSessionTrigger.value = trigger;
  chromeSessions.value = [];
  chromeSessionsError.value = "";
  resetChromeBrowserSyncLog();
  chromeSessionsLoading.value = true;
  chromeSessionDialogOpen.value = true;
  const result = await analyzeChromeUsage(true, site.id);
  if (requestId !== chromeSessionRequestId) return;
  chromeSessionsLoading.value = false;
  if (!result) {
    chromeSessionsError.value = "账号缓存刷新失败，请稍后重试";
    return;
  }
  chromeSessionSite.value = sites.value.find((item) => item.id === site.id) ?? site;
  chromeSessions.value = result.sites.find((item) => item.siteId === site.id)?.sessions ?? [];
  if (chromeSessions.value.length === 0) {
    chromeSessionsError.value = "未检测到该站点的 Chrome 账户会话";
    return;
  }
  const chromeFallbackAccounts = chromeSessions.value.filter((session) =>
    canSyncAccountViaChrome(session),
  );
  if (chromeFallbackAccounts.length > 0) {
    await syncCloudflareAccountsViaChrome(chromeFallbackAccounts);
  }
}

interface SyncedSiteModelsResult {
  models: Array<{ id: string; owned_by?: string; ownedBy?: string }>;
  source: string;
  keys: string[];
}

async function syncAllModelKeys(siteIds = filteredSites.value.map((site) => site.id)) {
  if (syncingModelKeys.value || syncingSites.value) return;
  const visibleSiteIds = new Set(siteIds);
  const siteMap = new Map(sites.value.map((site) => [site.id, site]));
  const targets = usageSites.value
    .flatMap((usageSite) => {
      if (!visibleSiteIds.has(usageSite.siteId)) return [];
      const site = siteMap.get(usageSite.siteId);
      if (!site) return [];
      return usageSite.sessions
        .filter((session) => session.isValid)
        .map((session) => ({ site, session }));
    })
    .filter((target, index, items) =>
      items.findIndex((candidate) =>
        candidate.site.id === target.site.id &&
        candidate.session.profileId === target.session.profileId,
      ) === index,
    );
  if (targets.length === 0) {
    appendSyncLog({
      stage: "models-empty",
      status: "info",
      message: "当前列表没有可同步 Key 与模型的合法账号",
    });
    syncRunState.value = "complete";
    stopSyncTimer();
    showToast("当前列表没有可同步 Key 的账号", true);
    return;
  }

  appendSyncLog({
    stage: "models-scope",
    status: "info",
    message: `已锁定当前列表中的 ${siteIds.length} 个在用存活站点，共 ${targets.length} 个账号`,
  });
  syncingModelKeys.value = true;
  modelKeySyncCompleted.value = 0;
  modelKeySyncTotal.value = targets.length;
  let succeeded = 0;
  let failed = 0;
  let keyCount = 0;
  let modelCount = 0;
  const modelsBySite = new Map<string, SyncedSiteModelsResult[]>();
  try {
    for (const { site, session } of targets) {
      const stage = `models-${site.id}-${session.profileId}`;
      const accountLabel = session.username || session.accountName || session.profileName;
      appendSyncLog({
        stage,
        status: "running",
        message: `正在同步 ${site.name} · ${accountLabel} 的 Key 与模型`,
      });
      try {
        let baseUrl = site.apiBaseUrl.trim();
        if (!baseUrl.endsWith("/")) baseUrl += "/";
        const result = await runCommand<SyncedSiteModelsResult>("fetch_site_models_json", {
          url: baseUrl,
          siteId: site.id,
          profileId: session.profileId,
        });
        const siteResults = modelsBySite.get(site.id) ?? [];
        siteResults.push(result);
        modelsBySite.set(site.id, siteResults);
        keyCount += result.keys?.length ?? 0;
        modelCount += result.models?.length ?? 0;
        succeeded += 1;
        appendSyncLog({
          stage,
          status: "success",
          message: `${site.name} · ${accountLabel} 同步成功：${result.keys?.length ?? 0} 个 Key，${result.models?.length ?? 0} 个模型`,
        });
      } catch (error) {
        failed += 1;
        appendSyncLog({
          stage,
          status: "error",
          message: `${site.name} · ${accountLabel} 同步失败：${String(error)}`,
        });
      } finally {
        modelKeySyncCompleted.value += 1;
      }
    }

    for (const [siteId, results] of modelsBySite) {
      const models = results
        .flatMap((result) => result.models ?? [])
        .filter((model, index, items) =>
          items.findIndex((candidate) => candidate.id === model.id) === index,
        );
      if (models.length === 0) continue;
      const apiSource = results.find((result) =>
        ["newapi-key", "sub2api-key"].includes(result.source),
      )?.source ?? results[0]?.source ?? "models";
      localStorage.setItem(`openhub_models_${siteId}`, JSON.stringify({ models, apiSource }));
    }
    await loadLibrary();
    syncRunState.value = "complete";
    appendSyncLog({
      stage: "models-complete",
      status: failed > 0 ? "error" : "success",
      message: failed > 0
        ? `模型同步完成：${succeeded} 个账号成功，${failed} 个失败，共 ${keyCount} 个 Key、${modelCount} 个模型`
        : `模型同步完成：${succeeded} 个账号，共 ${keyCount} 个 Key、${modelCount} 个模型`,
    });
    stopSyncTimer();
    showToast(
      failed > 0
        ? `模型同步完成：${succeeded} 个账号成功，${failed} 个失败，共 ${keyCount} 个 Key、${modelCount} 个模型`
        : `模型同步完成：${succeeded} 个账号，共 ${keyCount} 个 Key、${modelCount} 个模型`,
      failed > 0,
    );
  } catch (error) {
    syncRunState.value = "error";
    appendSyncLog({
      stage: "models-failed",
      status: "error",
      message: `模型同步失败：${String(error)}`,
    });
    stopSyncTimer();
    showToast(`模型同步失败：${String(error)}`, true);
  } finally {
    syncingModelKeys.value = false;
  }
}

async function toggleRunaway(site: SiteRecord) {
  const wasRunaway = site.isRunaway;
  try {
    await runCommand<SiteRecord>("toggle_runaway", { id: site.id });
    await loadLibrary();
    showToast(wasRunaway ? "已恢复为存活站点" : "已移入跑路列表");
  } catch (error) {
    showToast(`状态更新失败：${String(error)}`, true);
  }
}

async function openExternal(url: string) {
  if (!url) return;
  try {
    if (isTauri) await openUrl(url);
    else window.open(url, "_blank", "noopener");
  } catch (error) {
    showToast(`无法打开链接：${String(error)}`, true);
  }
}

async function openExternalInChromeProfile(url: string, profileId: string) {
  if (!url) return;
  if (!profileId || !isTauri) {
    await openExternal(url);
    return;
  }
  try {
    await runCommand<void>("open_url_in_chrome_profile", { url, profileId });
  } catch (error) {
    showToast(`无法使用所选 Chrome 账户打开链接：${String(error)}`, true);
  }
}

function openLinkDialog(site: SiteRecord, kind: SiteLinkKind, trigger: HTMLElement) {
  linkDialogSite.value = site;
  linkDialogKind.value = kind;
  linkDialogTrigger.value = trigger;
  linkDialogOpen.value = true;
}

function closeLinkDialog() {
  linkDialogOpen.value = false;
  linkDialogSite.value = null;
  linkDialogTrigger.value?.focus();
  linkDialogTrigger.value = null;
}

function openPreview(site: SiteRecord, trigger: HTMLElement) {
  previewSite.value = site;
  previewTrigger.value = trigger;
  previewDialogOpen.value = true;
}

function closePreview() {
  previewDialogOpen.value = false;
  previewSite.value = null;
  previewTrigger.value?.focus();
  previewTrigger.value = null;
}

async function copyAddress(url: string, label: string) {
  try {
    await navigator.clipboard.writeText(url);
    showToast(`${label}已复制`);
  } catch {
    showToast("复制失败，请手动复制", true);
  }
}

async function analyzeChromeUsage(
  notify = false,
  siteId?: string,
  runId?: number,
  siteIds?: string[],
): Promise<ChromeUsageScanResult | null> {
  if (chromeUsageScanning.value) return chromeUsageScanResult.value;
  chromeUsageScanning.value = true;
  try {
    const result = await runCommand<ChromeUsageScanResult>(
      "mark_sites_with_chrome_sessions",
      {
        ...(siteId ? { siteId } : {}),
        ...(siteIds ? { siteIds } : {}),
        ...(runId ? { runId } : {}),
      },
    );
    chromeUsageScanResult.value = result;
    await loadLibrary();
    if (notify) {
      showToast(
        `账号缓存已更新：${result.detected} 个站点、${result.accounts} 个合法账号${result.warnings ? `，${result.warnings} 个警告` : ""}`,
      );
    }
    return result;
  } catch (error) {
    if (notify) showToast(`分析 Chrome 会话失败：${String(error)}`, true);
    return null;
  } finally {
    chromeUsageScanning.value = false;
  }
}

function closeChromeSessionDialog() {
  if (chromeBrowserSyncingProfileId.value) return;
  chromeSessionRequestId += 1;
  chromeBrowserSyncRunId += 1;
  resetChromeBrowserSyncLog();
  chromeSessionDialogOpen.value = false;
  chromeSessionSite.value = null;
  chromeSessions.value = [];
  chromeSessionsError.value = "";
  chromeBrowserSyncError.value = "";
  chromeSessionTrigger.value?.focus();
  chromeSessionTrigger.value = null;
}

function canSyncAccountViaChrome(session: ChromeSessionInfo): boolean {
  return chromeSessionSite.value?.systemType.toLocaleLowerCase() === "newapi"
    && needsChromeAccountFallback(session);
}

function stopChromeBrowserSyncTimer() {
  if (chromeBrowserSyncTimer !== null) {
    window.clearInterval(chromeBrowserSyncTimer);
    chromeBrowserSyncTimer = null;
  }
  if (chromeBrowserSyncStartedAt) {
    chromeBrowserSyncElapsedMs.value = Date.now() - chromeBrowserSyncStartedAt;
  }
}

function resetChromeBrowserSyncLog() {
  stopChromeBrowserSyncTimer();
  chromeBrowserSyncLogs.value = [];
  chromeBrowserSyncElapsedMs.value = 0;
  chromeBrowserSyncStartedAt = 0;
}

function appendChromeBrowserSyncLog(
  progress: Omit<SyncSitesProgress, "runId">,
) {
  if (!chromeSessionDialogOpen.value) return;
  if (progress.status === "success" || progress.status === "error") {
    const runningEntry = [...chromeBrowserSyncLogs.value]
      .reverse()
      .find((entry) => entry.stage === progress.stage && entry.status === "running");
    if (runningEntry) runningEntry.status = progress.status;
  }
  if (progress.status === "error") {
    for (const entry of chromeBrowserSyncLogs.value) {
      if (entry.status === "running") entry.status = "error";
    }
  }
  chromeBrowserSyncLogs.value.push({
    ...progress,
    id: ++chromeBrowserSyncLogId,
    elapsedMs: chromeBrowserSyncStartedAt ? Date.now() - chromeBrowserSyncStartedAt : 0,
  });
}

function receiveChromeBrowserSyncProgress(progress: SyncSitesProgress) {
  if (progress.runId !== chromeBrowserSyncRunId) return;
  appendChromeBrowserSyncLog(progress);
}

function startChromeBrowserSyncLog() {
  resetChromeBrowserSyncLog();
  chromeBrowserSyncStartedAt = Date.now();
  chromeBrowserSyncTimer = window.setInterval(() => {
    chromeBrowserSyncElapsedMs.value = Date.now() - chromeBrowserSyncStartedAt;
  }, 200);
  chromeBrowserSyncError.value = "";
}

async function runChromeAccountSync(session: ChromeSessionInfo): Promise<boolean> {
  const site = chromeSessionSite.value;
  if (!site) return false;
  const runId = ++chromeBrowserSyncRunId;
  appendChromeBrowserSyncLog({
    stage: "start",
    status: "info",
    message: `开始同步 Chrome ${session.profileName} 的账号数据`,
  });
  chromeBrowserSyncingProfileId.value = session.profileId;
  try {
    const refreshed = await runCommand<ChromeSessionInfo>("sync_site_account_via_chrome", {
      siteId: site.id,
      profileId: session.profileId,
      runId,
    });
    const index = chromeSessions.value.findIndex((item) => item.profileId === session.profileId);
    if (index >= 0) chromeSessions.value[index] = refreshed;
    await loadLibrary();
    return true;
  } catch (error) {
    const message = String(error);
    chromeBrowserSyncError.value = chromeBrowserSyncError.value
      ? `${chromeBrowserSyncError.value}\n${session.profileName}：${message}`
      : `${session.profileName}：${message}`;
    appendChromeBrowserSyncLog({
      stage: "failed",
      status: "error",
      message: `${session.profileName} 同步失败：${message}`,
    });
    return false;
  }
}

async function syncCloudflareAccountsViaChrome(sessions: ChromeSessionInfo[]) {
  if (chromeBrowserSyncingProfileId.value || sessions.length === 0) return;
  startChromeBrowserSyncLog();
  let succeeded = 0;
  try {
    for (const session of sessions) {
      if (await runChromeAccountSync(session)) succeeded += 1;
    }
  } finally {
    stopChromeBrowserSyncTimer();
    chromeBrowserSyncingProfileId.value = "";
  }
  const failed = sessions.length - succeeded;
  if (failed > 0) {
    showToast(`Chrome 已更新 ${succeeded} 个账号，${failed} 个失败`, true);
  } else {
    showToast(`已通过 Chrome 更新 ${succeeded} 个账号`);
  }
}

async function syncAccountViaChrome(session: ChromeSessionInfo) {
  if (chromeBrowserSyncingProfileId.value) return;
  startChromeBrowserSyncLog();
  let succeeded = false;
  try {
    succeeded = await runChromeAccountSync(session);
  } finally {
    stopChromeBrowserSyncTimer();
    chromeBrowserSyncingProfileId.value = "";
  }
  if (succeeded) {
    showToast(`已通过 Chrome 更新 ${session.accountName || session.profileName} 的账号数据`);
  } else {
    showToast(`Chrome 同步失败：${chromeBrowserSyncError.value}`, true);
  }
}

async function copyChromeSession(session: ChromeSessionInfo) {
  const site = chromeSessionSite.value;
  const url = site?.checkinUrl.trim() || site?.apiBaseUrl.trim() || "";
  if (!url) {
    showToast("该站点地址已失效", true);
    return;
  }
  chromeSessionCopyingProfileId.value = session.profileId;
  try {
    const value = await runCommand<ChromeSessionValue>("read_chrome_session", {
      url,
      profileId: session.profileId,
    });
    await navigator.clipboard.writeText(value.cookie);
    showToast(
      `已从 Chrome「${value.profileName}」读取 ${value.domain} 的 ${value.cookieCount} 个 Cookie 并复制`,
    );
  } catch (error) {
    showToast(`读取 Chrome 会话失败：${String(error)}`, true);
  } finally {
    chromeSessionCopyingProfileId.value = "";
  }
}

function addressItems(site: SiteRecord, kind: SiteLinkKind): AddressItem[] {
  if (kind === "api") return [{ label: "API 地址", url: site.apiBaseUrl }].filter((item) => item.url.trim());
  if (kind === "checkin")
    return [{ label: "签到地址", url: site.checkinUrl, note: site.checkinNote }].filter((item) => item.url.trim());
  if (kind === "benefit") return [{ label: "福利站地址", url: site.benefitUrl }].filter((item) => item.url.trim());
  if (kind === "status") return [{ label: "状态页地址", url: site.statusUrl }].filter((item) => item.url.trim());
  return site.extensionLinks
    .filter((item) => item.url.trim())
    .map((item) => ({ label: item.label.trim() || "扩展链接", url: item.url.trim() }));
}

function allAddressItems(site: SiteRecord): AddressItem[] {
  return (["api", "checkin", "benefit", "status", "extension"] as SiteLinkKind[]).flatMap((kind) =>
    addressItems(site, kind),
  );
}

function matchesFeature(site: SiteRecord, feature: string): boolean {
  switch (feature) {
    case "checkin": return site.supportsCheckin;
    case "translation": return site.supportsImmersiveTranslation;
    case "ldc": return site.supportsLdc;
    case "nsfw": return site.supportsNsfw;
    case "invite": return site.requiresInviteCode;
    default: return true;
  }
}

// ============================================================
//  useStore — 返回全局单例的引用
// ============================================================
export function useStore() {
  return {
    // 数据 (Ref)
    sites,
    suggestedTags,
    loading,
    preferences,
    // 视图状态 (Ref)
    runawayFilter,
    usageFilter,
    query,
    tag,
    level,
    feature,
    systemTypeFilter,
    page,
    editingId,
    activeTab,
    modalOpen,
    linkDialogOpen,
    previewDialogOpen,
    chromeSessionDialogOpen,
    syncDialogOpen,
    siteModelsDialogOpen,
    linkDialogKind,
    linkDialogSite,
    previewSite,
    siteModelsSite,
    chromeSessionSite,
    chromeSessions,
    chromeSessionsLoading,
    chromeSessionsError,
    chromeSessionCopyingProfileId,
    chromeBrowserSyncingProfileId,
    chromeBrowserSyncError,
    chromeBrowserSyncLogs,
    chromeBrowserSyncElapsedMs,
    syncingSites,
    syncingModelKeys,
    modelKeySyncCompleted,
    modelKeySyncTotal,
    syncRunState,
    syncLogs,
    syncElapsedMs,
    remoteUser,
    remoteUserLoading,
    remoteUserError,
    syncDialogRunaway,
    syncDialogMode,
    syncDialogSiteIds,
    // 计算属性 (ComputedRef)
    allTags,
    chromeUsageAccounts,
    filteredSites,
    activeCount,
    runawayCount,
    personalCount,
    hasFilters,
    editingSite,
    // 操作
    loadLibrary,
    clearFilters,
    setRunawayFilter,
    setUsageFilter,
    openSettings,
    closeSettings,
    openModels,
    openLibrary,
    openModal,
    closeModal,
    openSyncDialog,
    openModelSyncDialog,
    closeSyncDialog,
    refreshRemoteUser,
    openRemoteLogin,
    syncSites,
    receiveSyncProgress,
    receiveNestedChromeSyncProgress,
    saveSite,
    importSite,
    deleteSite,
    togglePersonal,
    syncChromeSession,
    syncAllModelKeys,
    toggleRunaway,
    openExternal,
    openExternalInChromeProfile,
    openLinkDialog,
    closeLinkDialog,
    openPreview,
    closePreview,
    openSiteModelsDialog,
    closeSiteModelsDialog,
    copyAddress,
    analyzeChromeUsage,
    closeChromeSessionDialog,
    canSyncAccountViaChrome,
    syncAccountViaChrome,
    receiveChromeBrowserSyncProgress,
    copyChromeSession,
    addressItems,
    allAddressItems,
    updatePreferences,
    showToast,
  };
}
