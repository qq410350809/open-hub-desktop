import { ref, computed, type ComputedRef } from "vue";
import { runCommand, useLibrary } from "./useLibrary";
import { useToast } from "./useToast";
import { useUIState } from "./useUIState";
import type {
  ChromeSessionInfo,
  ChromeSessionValue,
  ChromeUsageScanResult,
  SyncLogEntry,
  SyncSitesProgress,
} from "../types";

const { sites, usageSites, loadLibrary } = useLibrary();
const { showToast } = useToast();
const { chromeSessionDialogOpen } = useUIState();

// Chrome 会话弹窗数据
const chromeSessionSite = ref<any>(null);
const chromeSessionTrigger = ref<HTMLElement | null>(null);
const chromeSessions = ref<ChromeSessionInfo[]>([]);
const chromeSessionsLoading = ref(false);
const chromeSessionsError = ref("");
const chromeSessionCopyingProfileId = ref("");
const chromeBrowserSyncingProfileId = ref("");
const chromeModelsSyncing = ref(false);
const chromeBrowserSyncError = ref("");
const chromeBrowserSyncLogs = ref<SyncLogEntry[]>([]);
const chromeBrowserSyncElapsedMs = ref(0);
const chromeUsageScanning = ref(false);
const chromeUsageScanResult = ref<ChromeUsageScanResult | null>(null);

let chromeSessionRequestId = 0;
let chromeBrowserSyncRunId = 0;
let chromeBrowserSyncLogId = 0;
let chromeBrowserSyncStartedAt = 0;
let chromeBrowserSyncLastLogAt = 0;
let chromeBrowserSyncTimer: number | null = null;

interface SyncedSiteModelsResult {
  models: Array<{ id: string; owned_by?: string; ownedBy?: string }>;
  source: string;
  keys: string[];
}

interface SyncedModelCacheAccount {
  profileId: string;
  profileName: string;
  accountName: string;
  username: string;
  keys: string[];
  error: string;
}

const chromeUsageAccounts: ComputedRef<Record<string, ChromeSessionInfo[]>> = computed(() =>
  Object.fromEntries(
    usageSites.value.map((site) => [site.siteId, site.sessions]),
  ),
);

function needsChromeAccountFallback(session: ChromeSessionInfo): boolean {
  return !session.isValid || Boolean(session.syncError.trim());
}

function canSyncAccountViaChrome(session: ChromeSessionInfo): boolean {
  return chromeSessionSite.value?.systemType?.toLocaleLowerCase() === "newapi"
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
  chromeBrowserSyncLastLogAt = 0;
}

function appendChromeBrowserSyncLog(progress: Omit<SyncSitesProgress, "runId">) {
  if (!chromeSessionDialogOpen.value) return;
  const now = Date.now();
  const delta = chromeBrowserSyncLastLogAt ? now - chromeBrowserSyncLastLogAt : 0;
  chromeBrowserSyncLastLogAt = now;
  if (progress.status === "success" || progress.status === "error") {
    const runningEntry = [...chromeBrowserSyncLogs.value]
      .reverse()
      .find((entry) => entry.stage === progress.stage && entry.status === "running");
    if (runningEntry) {
      runningEntry.status = progress.status;
      runningEntry.message = progress.message;
      runningEntry.elapsedMs = delta;
      if (progress.status === "error") {
        for (const entry of chromeBrowserSyncLogs.value) {
          if (entry.status === "running") entry.status = "error";
        }
      }
      return;
    }
  }
  if (progress.status === "error") {
    for (const entry of chromeBrowserSyncLogs.value) {
      if (entry.status === "running") entry.status = "error";
    }
  }
  chromeBrowserSyncLogs.value.push({
    ...progress,
    id: ++chromeBrowserSyncLogId,
    elapsedMs: delta,
  });
}

function receiveChromeBrowserSyncProgress(progress: SyncSitesProgress) {
  if (progress.runId !== chromeBrowserSyncRunId) return;
  appendChromeBrowserSyncLog(progress);
}

function startChromeBrowserSyncLog() {
  resetChromeBrowserSyncLog();
  chromeBrowserSyncStartedAt = Date.now();
  chromeBrowserSyncLastLogAt = chromeBrowserSyncStartedAt;
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
    appendChromeBrowserSyncLog({
      stage: `sync-done-${session.profileId}`,
      status: "success",
      message: `${session.profileName} 账号同步完成`,
    });
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
  appendChromeBrowserSyncLog({
    stage: "fallback-start",
    status: "info",
    message: `检测到 ${sessions.length} 个需要回退同步的 NewAPI 账号`,
  });
  if (chromeBrowserSyncTimer === null) startChromeBrowserSyncLog();
  let succeeded = 0;
  try {
    for (const session of sessions) {
      if (await runChromeAccountSync(session)) succeeded += 1;
    }
  } finally {
    chromeBrowserSyncingProfileId.value = "";
  }
  const failed = sessions.length - succeeded;
  appendChromeBrowserSyncLog({
    stage: "fallback-done",
    status: failed > 0 ? "error" : "success",
    message: `回退同步完成：${succeeded} 个成功${failed > 0 ? `，${failed} 个失败` : ""}`,
  });
  if (failed > 0) {
    showToast(`Chrome 已更新 ${succeeded} 个账号，${failed} 个失败`, true);
  } else {
    showToast(`已通过 Chrome 更新 ${succeeded} 个账号`);
  }
}

async function syncChromeModelsForSessions(site: any, sessions: ChromeSessionInfo[]): Promise<boolean> {
  const targets = sessions.filter((session) => session.isValid);
  if (targets.length === 0) {
    appendChromeBrowserSyncLog({
      stage: "models-empty",
      status: "info",
      message: "没有合法账号可同步 Key 与模型",
    });
    return false;
  }
  chromeModelsSyncing.value = true;
  appendChromeBrowserSyncLog({
    stage: "models-start",
    status: "info",
    message: `开始同步 ${targets.length} 个账号的 Key 与模型`,
  });
  let succeeded = 0;
  let keyCount = 0;
  let modelCount = 0;
  const errors: string[] = [];
  let completed = false;
  try {
    await runCommand("clear_site_model_cache_for_site", { siteId: site.id });
    let nextTargetIndex = 0;
    const workerCount = Math.min(2, targets.length);
    await Promise.all(Array.from({ length: workerCount }, async () => {
      while (nextTargetIndex < targets.length) {
        const session = targets[nextTargetIndex++];
        const stage = `models-${session.profileId}`;
        const accountLabel = session.username || session.accountName || session.profileName;
        appendChromeBrowserSyncLog({
          stage,
          status: "running",
          message: `正在同步 ${accountLabel} 的 Key 与模型…`,
        });
        try {
          let baseUrl = site.apiBaseUrl.trim();
          if (!baseUrl.endsWith("/")) baseUrl += "/";
          const result = await runCommand<SyncedSiteModelsResult>("fetch_site_models_json", {
            url: baseUrl,
            siteId: site.id,
            profileId: session.profileId,
          });
          await runCommand("save_site_model_cache_for_account", {
            siteId: site.id,
            account: {
              profileId: session.profileId,
              profileName: session.profileName,
              accountName: session.accountName,
              username: session.username,
              keys: result.keys ?? [],
              error: "",
            } satisfies SyncedModelCacheAccount,
            result,
          });
          succeeded += 1;
          keyCount += result.keys?.length ?? 0;
          modelCount += result.models?.length ?? 0;
          appendChromeBrowserSyncLog({
            stage,
            status: "success",
            message: `${accountLabel}：${result.keys?.length ?? 0} 个 Key，${result.models?.length ?? 0} 个模型`,
          });
        } catch (error) {
          const message = String(error);
          errors.push(`${accountLabel}：${message}`);
          await runCommand("save_site_model_cache_for_account", {
            siteId: site.id,
            account: {
              profileId: session.profileId,
              profileName: session.profileName,
              accountName: session.accountName,
              username: session.username,
              keys: [],
              error: message,
            } satisfies SyncedModelCacheAccount,
            result: null,
          });
          appendChromeBrowserSyncLog({
            stage,
            status: "error",
            message: `${accountLabel} 同步失败：${message}`,
          });
        }
      }
    }));
    await loadLibrary();
    chromeSessions.value = chromeUsageAccounts.value[site.id] ?? chromeSessions.value;
    const failed = targets.length - succeeded;
    appendChromeBrowserSyncLog({
      stage: "models-complete",
      status: failed > 0 ? "error" : "success",
      message: failed > 0
        ? `Key 与模型同步完成：${succeeded} 个成功，${failed} 个失败，共 ${keyCount} 个 Key、${modelCount} 个模型`
        : `Key 与模型同步完成：共 ${keyCount} 个 Key、${modelCount} 个模型`,
    });
    if (errors.length > 0) {
      chromeBrowserSyncError.value = errors.join("\n");
    }
    completed = failed === 0;
  } catch (error) {
    const message = String(error);
    chromeBrowserSyncError.value = message;
    appendChromeBrowserSyncLog({
      stage: "models-failed",
      status: "error",
      message: `Key 与模型同步中断：${message}`,
    });
  } finally {
    chromeModelsSyncing.value = false;
  }
  return completed;
}

async function syncAccountViaChrome(session: ChromeSessionInfo) {
  if (chromeBrowserSyncingProfileId.value) return;
  startChromeBrowserSyncLog();
  appendChromeBrowserSyncLog({
    stage: "manual-sync-start",
    status: "info",
    message: `手动同步 ${session.profileName} 的账号数据`,
  });
  let succeeded = false;
  let modelsSucceeded = false;
  try {
    succeeded = await runChromeAccountSync(session);
    if (succeeded && chromeSessionSite.value) {
      const refreshed = chromeSessions.value.find((item) => item.profileId === session.profileId);
      if (refreshed) {
        modelsSucceeded = await syncChromeModelsForSessions(chromeSessionSite.value, [refreshed]);
      }
    }
  } finally {
    stopChromeBrowserSyncTimer();
    chromeBrowserSyncingProfileId.value = "";
  }
  if (succeeded && modelsSucceeded) {
    appendChromeBrowserSyncLog({
      stage: "manual-sync-done",
      status: "success",
      message: `${session.profileName} 账号、Key 与模型同步完成`,
    });
  }
  if (succeeded && modelsSucceeded) {
    showToast(`已通过 Chrome 更新 ${session.accountName || session.profileName} 的账号、Key 与模型`);
  } else if (succeeded) {
    showToast(`账号已更新，但 Key 与模型同步失败：${chromeBrowserSyncError.value}`, true);
  } else {
    showToast(`Chrome 同步失败：${chromeBrowserSyncError.value}`, true);
  }
}

async function copyChromeSession(session: ChromeSessionInfo) {
  const site = chromeSessionSite.value;
  const url = site?.checkinUrl?.trim() || site?.apiBaseUrl?.trim() || "";
  if (!url) {
    showToast("该站点地址已失效", true);
    appendChromeBrowserSyncLog({
      stage: "copy-failed",
      status: "error",
      message: "站点地址已失效，无法读取会话",
    });
    return;
  }
  appendChromeBrowserSyncLog({
    stage: `copy-start-${session.profileId}`,
    status: "running",
    message: `正在从 Chrome「${session.profileName}」读取 ${session.domain} 的 Cookie…`,
  });
  chromeSessionCopyingProfileId.value = session.profileId;
  try {
    const value = await runCommand<ChromeSessionValue>("read_chrome_session", {
      url,
      profileId: session.profileId,
    });
    await navigator.clipboard.writeText(value.cookie);
    appendChromeBrowserSyncLog({
      stage: `copy-start-${session.profileId}`,
      status: "success",
      message: `已从 Chrome「${value.profileName}」读取 ${value.domain} 的 ${value.cookieCount} 个 Cookie 并复制到剪贴板`,
    });
    showToast(`已从 Chrome「${value.profileName}」读取 ${value.domain} 的 ${value.cookieCount} 个 Cookie 并复制`);
  } catch (error) {
    appendChromeBrowserSyncLog({
      stage: `copy-start-${session.profileId}`,
      status: "error",
      message: `读取 Chrome 会话失败：${String(error)}`,
    });
    showToast(`读取 Chrome 会话失败：${String(error)}`, true);
  } finally {
    chromeSessionCopyingProfileId.value = "";
  }
}

function closeChromeSessionDialog() {
  if (chromeBrowserSyncingProfileId.value || chromeModelsSyncing.value) return;
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

async function analyzeChromeUsage(
  notify = false,
  siteId?: string,
  runId?: number,
  siteIds?: string[],
): Promise<ChromeUsageScanResult | null> {
  if (chromeUsageScanning.value) return chromeUsageScanResult.value;
  if (chromeSessionDialogOpen.value) {
    appendChromeBrowserSyncLog({
      stage: "scan-running",
      status: "running",
      message: "正在扫描 Chrome 配置并检测账号会话…",
    });
  }
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
    if (chromeSessionDialogOpen.value) {
      appendChromeBrowserSyncLog({
        stage: "scan-running",
        status: "success",
        message: `扫描完成：${result.detected} 个站点、${result.accounts} 个合法账号${result.warnings ? `，${result.warnings} 个警告` : ""}`,
      });
    }
    if (notify) {
      showToast(
        `账号缓存已更新：${result.detected} 个站点、${result.accounts} 个合法账号${result.warnings ? `，${result.warnings} 个警告` : ""}`,
      );
    }
    return result;
  } catch (error) {
    if (chromeSessionDialogOpen.value) {
      appendChromeBrowserSyncLog({
        stage: "scan-running",
        status: "error",
        message: `扫描失败：${String(error)}`,
      });
    }
    if (notify) showToast(`分析 Chrome 会话失败：${String(error)}`, true);
    return null;
  } finally {
    chromeUsageScanning.value = false;
  }
}

async function syncChromeSession(site: any, trigger: HTMLElement) {
  const requestId = ++chromeSessionRequestId;
  chromeSessionSite.value = site;
  chromeSessionTrigger.value = trigger;
  chromeSessions.value = [];
  chromeSessionsError.value = "";
  startChromeBrowserSyncLog();
  chromeSessionsLoading.value = true;
  chromeSessionDialogOpen.value = true;
  appendChromeBrowserSyncLog({
    stage: "scan-start",
    status: "info",
    message: "开始扫描 Chrome 账号会话…",
  });
  try {
    const result = await analyzeChromeUsage(true, site.id, chromeBrowserSyncRunId);
    if (requestId !== chromeSessionRequestId) return;
    chromeSessionsLoading.value = false;
    if (!result) {
      chromeSessionsError.value = "账号缓存刷新失败，请稍后重试";
      return;
    }
    chromeSessionSite.value = sites.value.find((item: any) => item.id === site.id) ?? site;
    chromeSessions.value = result.sites.find((item: any) => item.siteId === site.id)?.sessions ?? [];
    if (chromeSessions.value.length === 0) {
      appendChromeBrowserSyncLog({
        stage: "scan-empty",
        status: "info",
        message: `未检测到「${site.name}」的 Chrome 账户会话`,
      });
      chromeSessionsError.value = "未检测到该站点的 Chrome 账户会话";
      return;
    }
    appendChromeBrowserSyncLog({
      stage: "sessions-found",
      status: "info",
      message: `检测到 ${chromeSessions.value.length} 个 Chrome 账号会话`,
    });
    const chromeFallbackAccounts = chromeSessions.value.filter((session) =>
      canSyncAccountViaChrome(session),
    );
    if (chromeFallbackAccounts.length > 0) {
      await syncCloudflareAccountsViaChrome(chromeFallbackAccounts);
    } else {
      appendChromeBrowserSyncLog({
        stage: "scan-complete",
        status: "success",
        message: "所有账号会话状态正常，无需回退同步",
      });
    }
    await syncChromeModelsForSessions(chromeSessionSite.value, chromeSessions.value);
  } finally {
    if (requestId === chromeSessionRequestId) {
      stopChromeBrowserSyncTimer();
    }
  }
}

export function useChromeSession() {
  return {
    chromeSessionSite,
    chromeSessionTrigger,
    chromeSessions,
    chromeSessionsLoading,
    chromeSessionsError,
    chromeSessionCopyingProfileId,
    chromeBrowserSyncingProfileId,
    chromeModelsSyncing,
    chromeBrowserSyncError,
    chromeBrowserSyncLogs,
    chromeBrowserSyncElapsedMs,
    chromeUsageScanning,
    chromeUsageScanResult,
    chromeUsageAccounts,
    needsChromeAccountFallback,
    canSyncAccountViaChrome,
    stopChromeBrowserSyncTimer,
    resetChromeBrowserSyncLog,
    appendChromeBrowserSyncLog,
    receiveChromeBrowserSyncProgress,
    startChromeBrowserSyncLog,
    runChromeAccountSync,
    syncCloudflareAccountsViaChrome,
    syncAccountViaChrome,
    copyChromeSession,
    closeChromeSessionDialog,
    analyzeChromeUsage,
    syncChromeSession,
  };
}
