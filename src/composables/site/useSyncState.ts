import { ref } from "vue";
import { runCommand } from "../core/ipc";
import { useLibrary } from "./useLibrary";
import { useToast } from "../core/useToast";
import { useFilterState } from "./useFilterState";
import { useUIState } from "../ui/useUIState";
import { useChromeSession } from "./useChromeSession";
import { useSiteActions } from "./useSiteActions";
import { REMOTE_LOGIN_URL } from "../../constants";
import type {
  RemoteUserInfo,
  SyncLogEntry,
  SyncProgressStatus,
  SyncRunState,
  SyncSitesProgress,
  SyncSitesResult,
} from "../../types";

const { sites, usageSites, loadLibrary } = useLibrary();
const { showToast } = useToast();
const { filteredSites, runawayFilter, usageFilter } = useFilterState();
const { syncDialogOpen } = useUIState();
const { analyzeChromeUsage } = useChromeSession();
const { openExternal } = useSiteActions();

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
const syncDialogMode = ref<"remote" | "quota">("remote");
const syncDialogUsage = ref<"personal" | "pending">("personal");
const syncDialogSiteIds = ref<string[]>([]);

let syncRunId = 0;
let syncLogId = 0;
let syncStartedAt = 0;
let syncLastLogAt = 0;
let syncTimer: number | null = null;
let remoteUserRequestId = 0;

const remoteLoginUrl = REMOTE_LOGIN_URL;

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

function resetSyncLog() {
  stopSyncTimer();
  syncRunState.value = "idle";
  syncLogs.value = [];
  syncElapsedMs.value = 0;
  syncStartedAt = 0;
  syncLastLogAt = 0;
}

function startSyncTimer() {
  stopSyncTimer();
  syncStartedAt = Date.now();
  syncLastLogAt = syncStartedAt;
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
  const now = Date.now();
  const delta = syncLastLogAt ? now - syncLastLogAt : 0;
  syncLastLogAt = now;

  if (progress.status === "error") {
    if (progress.stage === "failed") {
      for (const entry of syncLogs.value) {
        if (entry.status === "running") entry.status = "error";
      }
    } else {
      const existingEntry = [...syncLogs.value]
        .reverse()
        .find((entry) => entry.stage === progress.stage);
      if (existingEntry) {
        if (existingEntry.status === "running") {
          existingEntry.status = "error";
          existingEntry.message = progress.message;
          existingEntry.elapsedMs = delta;
          return;
        }
        if (existingEntry.message === progress.message) {
          return;
        }
      }
    }
  } else if (progress.status === "success") {
    const existingEntry = [...syncLogs.value]
      .reverse()
      .find((entry) => entry.stage === progress.stage);
    if (existingEntry) {
      if (existingEntry.status === "running") {
        existingEntry.status = "success";
        existingEntry.message = progress.message;
        existingEntry.elapsedMs = delta;
        return;
      }
      if (existingEntry.message === progress.message) {
        return;
      }
    }
  } else if (progress.status === "running") {
    const runningEntry = [...syncLogs.value]
      .reverse()
      .find((entry) => entry.stage === progress.stage);
    if (runningEntry) {
      if (runningEntry.status === "running") {
        runningEntry.message = progress.message;
        runningEntry.elapsedMs = delta;
        return;
      }
      if (runningEntry.message === progress.message) {
        return;
      }
    }
  } else if (progress.status === "info") {
    const lastEntry = syncLogs.value[syncLogs.value.length - 1];
    if (lastEntry && lastEntry.stage === progress.stage && lastEntry.message === progress.message) {
      return;
    }
  }

  syncLogs.value.push({
    ...progress,
    id: ++syncLogId,
    elapsedMs: delta,
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

function openSyncDialog(
  explicitMode?: "remote" | "quota",
  explicitUsage?: "personal" | "pending",
  explicitSiteIds?: string[],
) {
  if (syncDialogOpen.value) return;
  syncRunId += 1;
  resetSyncLog();
  syncDialogRunaway.value = runawayFilter.value === "runaway";
  const quotaMode = explicitMode
    ? explicitMode === "quota"
    : (usageFilter.value === "personal" || usageFilter.value === "pending");
  syncDialogMode.value = explicitMode ?? (quotaMode ? "quota" : "remote");
  syncDialogUsage.value = explicitUsage ?? (usageFilter.value === "pending" ? "pending" : "personal");
  if (explicitSiteIds && explicitSiteIds.length > 0) {
    syncDialogSiteIds.value = [...explicitSiteIds];
  } else if (syncDialogMode.value === "quota") {
    const targetUsage = syncDialogUsage.value;
    const targetSites = sites.value.filter((site) =>
      targetUsage === "pending" ? site.isPending : site.isPersonal,
    );
    syncDialogSiteIds.value = targetSites.length > 0
      ? targetSites.map((s) => s.id)
      : filteredSites.value.map((s) => s.id);
  } else {
    syncDialogSiteIds.value = filteredSites.value.map((site) => site.id);
  }
  syncDialogOpen.value = true;
  if (syncDialogMode.value === "remote") void refreshRemoteUser();
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

interface SyncedSiteModelsResult {
  models: Array<{ id: string; owned_by?: string; ownedBy?: string }>;
  source: string;
  keys: string[];
  keyGroups?: Record<string, string>;
}

interface SyncedModelCacheAccount {
  profileId: string;
  profileName: string;
  accountName: string;
  username: string;
  keys: string[];
  keyGroups?: Record<string, string>;
  error: string;
}

interface ModelSyncSummary {
  succeeded: number;
  failed: number;
  keyCount: number;
  modelCount: number;
}

async function syncAllModelKeys(
  siteIds = filteredSites.value.map((site) => site.id),
  options: { allowDuringSiteSync?: boolean; finalize?: boolean } = {},
): Promise<ModelSyncSummary> {
  const finalize = options.finalize ?? true;
  const emptySummary = { succeeded: 0, failed: 0, keyCount: 0, modelCount: 0 };
  if (syncingModelKeys.value || (syncingSites.value && !options.allowDuringSiteSync)) {
    return emptySummary;
  }
  const visibleSiteIds = new Set(siteIds);
  const siteMap = new Map(sites.value.map((site) => [site.id, site]));
  const targets = usageSites.value
    .flatMap((usageSite) => {
      if (!visibleSiteIds.has(usageSite.siteId)) return [];
      const site = siteMap.get(usageSite.siteId);
      if (!site) return [];
      return usageSite.sessions
        .filter((session) => session.isValid && session.apiKeyCount > 0)
        .map((session) => ({ site, session }));
    })
    .filter((target, index, items) =>
      items.findIndex((candidate) =>
        candidate.site.id === target.site.id &&
        candidate.session.profileId === target.session.profileId,
      ) === index,
    );
  if (targets.length === 0) {
    for (const siteId of siteIds) {
      await runCommand("clear_site_model_cache_for_site", { siteId });
    }
    appendSyncLog({ stage: "models-empty", status: "info", message: "当前列表没有可同步 Key 与模型的合法账号" });
    if (finalize) {
      syncRunState.value = "complete";
      stopSyncTimer();
      showToast("当前列表没有可同步 Key 的账号", true);
    }
    return emptySummary;
  }

  appendSyncLog({ stage: "models-scope", status: "info", message: `已锁定当前列表中的 ${siteIds.length} 个在用存活站点，共 ${targets.length} 个账号` });
  syncingModelKeys.value = true;
  modelKeySyncCompleted.value = 0;
  modelKeySyncTotal.value = targets.length;
  let succeeded = 0;
  let failed = 0;
  let keyCount = 0;
  let modelCount = 0;
  try {
    const clearedSites = new Set<string>();
    let nextTargetIndex = 0;
    const workerCount = Math.min(3, targets.length);
    await Promise.all(Array.from({ length: workerCount }, async () => {
      while (nextTargetIndex < targets.length) {
        const { site, session } = targets[nextTargetIndex++];
        const stage = `models-${site.id}-${session.profileId}`;
        const accountLabel = session.username || session.accountName || session.profileName;
        appendSyncLog({ stage, status: "running", message: `正在同步 ${site.name} · ${accountLabel} 的 Key 与模型` });
        try {
          let baseUrl = site.apiBaseUrl.trim();
          if (!baseUrl.endsWith("/")) baseUrl += "/";
          const result = await runCommand<SyncedSiteModelsResult>("fetch_site_models_json", { url: baseUrl, siteId: site.id, profileId: session.profileId });
          if (!clearedSites.has(site.id)) {
            await runCommand("clear_site_model_cache_for_site", { siteId: site.id });
            clearedSites.add(site.id);
          }
          await runCommand("save_site_model_cache_for_account", {
            siteId: site.id,
            account: {
              profileId: session.profileId,
              profileName: session.profileName,
              accountName: session.accountName,
              username: session.username,
              keys: result.keys ?? [],
              keyGroups: result.keyGroups ?? {},
              error: "",
            } satisfies SyncedModelCacheAccount,
            result,
          });
          keyCount += result.keys?.length ?? 0;
          modelCount += result.models?.length ?? 0;
          succeeded += 1;
          appendSyncLog({ stage, status: "success", message: `${site.name} · ${accountLabel} 同步成功：${result.keys?.length ?? 0} 个 Key，${result.models?.length ?? 0} 个模型` });
        } catch (error) {
          failed += 1;
          await runCommand("save_site_model_cache_for_account", {
            siteId: site.id,
            account: {
              profileId: session.profileId,
              profileName: session.profileName,
              accountName: session.accountName,
              username: session.username,
              keys: [],
              keyGroups: {},
              error: String(error),
            } satisfies SyncedModelCacheAccount,
            result: null,
          });
          appendSyncLog({ stage, status: "error", message: `${site.name} · ${accountLabel} 同步失败：${String(error)}` });
        } finally {
          modelKeySyncCompleted.value += 1;
        }
      }
    }));

    await loadLibrary();
    appendSyncLog({ stage: "models-complete", status: failed > 0 ? "error" : "success", message: failed > 0 ? `模型同步完成：${succeeded} 个账号成功，${failed} 个失败，共 ${keyCount} 个 Key、${modelCount} 个模型` : `模型同步完成：${succeeded} 个账号，共 ${keyCount} 个 Key、${modelCount} 个模型` });
    const summary = { succeeded, failed, keyCount, modelCount };
    if (finalize) {
      syncRunState.value = "complete";
      stopSyncTimer();
    }
    return summary;
  } catch (error) {
    appendSyncLog({ stage: "models-failed", status: "error", message: `模型同步失败：${String(error)}` });
    if (finalize) {
      syncRunState.value = "error";
      stopSyncTimer();
      showToast(`模型同步失败：${String(error)}`, true);
    }
    return { succeeded, failed: failed + 1, keyCount, modelCount };
  } finally {
    syncingModelKeys.value = false;
  }
}

async function syncSites() {
  if (syncingSites.value || syncingModelKeys.value || (syncDialogMode.value === "remote" && !remoteUser.value)) return;
  const mode = syncDialogMode.value;
  const runId = ++syncRunId;
  resetSyncLog();
  syncRunState.value = "syncing";
  startSyncTimer();
  appendSyncLog({ stage: "start", status: "info", message: "同步任务已开始" });
  remoteUserError.value = "";
  syncingSites.value = true;
  try {
    if (mode === "quota") {
      const usageLabel = syncDialogUsage.value === "pending" ? "待定" : "在用";
      const siteIds = [...syncDialogSiteIds.value];
      appendSyncLog({
        stage: "scope",
        status: "info",
        message: `额度同步范围：当前 ${siteIds.length} 个${usageLabel}站点；保持全部/在用/待定归类不变`,
      });
      if (siteIds.length === 0) {
        appendSyncLog({ stage: "quota-empty", status: "info", message: `当前没有可同步额度的${usageLabel}站点` });
        syncRunState.value = "complete";
        stopSyncTimer();
        showToast(`当前没有可同步额度的${usageLabel}站点`, true);
        return;
      }
      const accountResult = await analyzeChromeUsage(
        false,
        undefined,
        runId,
        siteIds,
        false,
        syncDialogUsage.value === "pending",
      );
      if (!accountResult) throw new Error("额度同步失败");
      appendSyncLog({
        stage: "quota-complete",
        status: accountResult.warnings > 0 ? "error" : "success",
        message: `额度同步完成：${accountResult.accounts} 个合法账号${accountResult.warnings ? `，${accountResult.warnings} 个警告` : ""}`,
      });
      syncRunState.value = "complete";
      stopSyncTimer();
      showToast(`已同步 ${usageLabel}站点额度：${accountResult.accounts} 个账号${accountResult.warnings ? `，${accountResult.warnings} 个警告` : ""}`);
      return;
    }
    const result = await runCommand<SyncSitesResult>("sync_remote_sites", { runId });
    await loadLibrary();
    const account = result.userName ? `账号 ${result.userName}` : `Chrome ${result.profileName}`;
    showToast(`${account} 已同步 ${result.total} 个公共站点（新增 ${result.added}，更新 ${result.updated}）`);
    syncRunState.value = "detecting";
    appendSyncLog({ stage: "available", status: "success", message: `站点数据已可用，共 ${result.total} 条（存活与跑路全量同步）；类型检测将在后台继续` });
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

export function useSyncState() {
  return {
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
    syncDialogUsage,
    syncDialogSiteIds,
    refreshRemoteUser,
    openSyncDialog,
    closeSyncDialog,
    openRemoteLogin,
    resetSyncLog,
    startSyncTimer,
    stopSyncTimer,
    appendSyncLog,
    receiveSyncProgress,
    receiveNestedChromeSyncProgress,
    syncSites,
    detectSyncedSiteTypes,
    syncAllModelKeys,
  };
}
