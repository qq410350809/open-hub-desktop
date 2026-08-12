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
import { normalizeSystemType } from "../types";

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

interface ChromeModelsSyncSummary {
  success: boolean;
  succeeded: number;
  failed: number;
  keyCount: number;
  modelCount: number;
  error: string;
}

const chromeUsageAccounts: ComputedRef<Record<string, ChromeSessionInfo[]>> = computed(() =>
  Object.fromEntries(
    usageSites.value.map((site) => [site.siteId, site.sessions]),
  ),
);

function needsChromeAccountFallback(session: ChromeSessionInfo): boolean {
  // 只有访问令牌缺失或被服务端拒绝时才进入 Cookie/refresh token/Chrome 回退。
  // 普通网络错误、self 解析错误等保留日志，但不应额外弹出浏览器。
  return !session.isValid;
}

function canSyncAccountViaChrome(session: ChromeSessionInfo): boolean {
  return normalizeSystemType(chromeSessionSite.value?.systemType ?? "") === "newapi"
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
      return;
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
  appendChromeBrowserSyncLog({
    ...progress,
    stage: `browser-account-${progress.runId}-${progress.stage}`,
    message: `浏览器账号请求｜${progress.message}`,
  });
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

async function runChromeAccountSync(
  session: ChromeSessionInfo,
  options: { reloadLibrary?: boolean } = {},
): Promise<boolean> {
  const reloadLibrary = options.reloadLibrary ?? true;
  const site = chromeSessionSite.value;
  if (!site) return false;
  const runId = ++chromeBrowserSyncRunId;
  const accountLabel = session.username || session.accountName || session.profileName;
  const stage = `account-refresh-${session.profileId}`;
  appendChromeBrowserSyncLog({
    stage,
    status: "running",
    message: `账号资料｜${accountLabel}｜正在刷新余额、签到和认证信息`,
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
    if (reloadLibrary) await loadLibrary();
    appendChromeBrowserSyncLog({
      stage,
      status: "success",
      message: `账号资料｜${accountLabel}｜完成：余额、签到和认证信息已更新`,
    });
    return true;
  } catch (error) {
    const message = String(error);
    chromeBrowserSyncError.value = chromeBrowserSyncError.value
      ? `${chromeBrowserSyncError.value}\n${accountLabel}：${message}`
      : `${accountLabel}：${message}`;
    appendChromeBrowserSyncLog({
      stage,
      status: "error",
      message: `账号资料｜${accountLabel}｜失败：${message}`,
    });
    return false;
  } finally {
    if (chromeBrowserSyncingProfileId.value === session.profileId) {
      chromeBrowserSyncingProfileId.value = "";
    }
  }
}

async function syncChromeModelsForSessions(
  site: any,
  sessions: ChromeSessionInfo[],
  options: { clearCache?: boolean; reloadLibrary?: boolean } = {},
): Promise<ChromeModelsSyncSummary> {
  // 保留已有 Key，先验证缓存 Key；成功后按账号覆盖缓存，失败才进入访问令牌回退。
  const clearCache = options.clearCache ?? false;
  const reloadLibrary = options.reloadLibrary ?? true;
  const targets = sessions.filter((session) => session.isValid);
  if (targets.length === 0) {
    const error = "没有合法账号可同步 Key 与模型";
    appendChromeBrowserSyncLog({
      stage: `models-empty-${site.id}`,
      status: "info",
      message: `Key/模型｜${error}`,
    });
    return { success: false, succeeded: 0, failed: 0, keyCount: 0, modelCount: 0, error };
  }
  chromeModelsSyncing.value = true;
  let succeeded = 0;
  let keyCount = 0;
  let modelCount = 0;
  const errors: string[] = [];
  try {
    if (clearCache) {
      await runCommand("clear_site_model_cache_for_site", { siteId: site.id });
    }
    for (const session of targets) {
      const stage = `models-${site.id}-${session.profileId}`;
      const accountLabel = session.username || session.accountName || session.profileName;
      appendChromeBrowserSyncLog({
        stage,
        status: "running",
        message: `Key/模型｜${accountLabel}｜正在读取 Key 并验证模型列表`,
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
        const accountKeyCount = result.keys?.length ?? 0;
        const accountModelCount = result.models?.length ?? 0;
        succeeded += 1;
        keyCount += accountKeyCount;
        modelCount += accountModelCount;
        appendChromeBrowserSyncLog({
          stage,
          status: "success",
          message: `Key/模型｜${accountLabel}｜完成：${accountKeyCount} 个 Key，${accountModelCount} 个模型`,
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
          message: `Key/模型｜${accountLabel}｜失败：${message}`,
        });
      }
    }
    if (reloadLibrary) {
      await loadLibrary();
      chromeSessions.value = chromeUsageAccounts.value[site.id] ?? chromeSessions.value;
    }
    const failed = targets.length - succeeded;
    if (targets.length > 1) {
      appendChromeBrowserSyncLog({
        stage: `models-complete-${site.id}`,
        status: failed > 0 ? "error" : "success",
        message: failed > 0
          ? `Key/模型汇总｜${succeeded} 个成功，${failed} 个失败，共 ${keyCount} 个 Key、${modelCount} 个模型`
          : `Key/模型汇总｜${succeeded} 个账号完成，共 ${keyCount} 个 Key、${modelCount} 个模型`,
      });
    }
    const error = errors.join("\n");
    if (error) {
      chromeBrowserSyncError.value = chromeBrowserSyncError.value
        ? `${chromeBrowserSyncError.value}\n${error}`
        : error;
    }
    return {
      success: failed === 0,
      succeeded,
      failed,
      keyCount,
      modelCount,
      error,
    };
  } catch (error) {
    const message = String(error);
    chromeBrowserSyncError.value = chromeBrowserSyncError.value
      ? `${chromeBrowserSyncError.value}\n${message}`
      : message;
    appendChromeBrowserSyncLog({
      stage: `models-failed-${site.id}`,
      status: "error",
      message: `Key/模型｜流程中断：${message}`,
    });
    return {
      success: false,
      succeeded,
      failed: Math.max(1, targets.length - succeeded),
      keyCount,
      modelCount,
      error: message,
    };
  } finally {
    chromeModelsSyncing.value = false;
  }
}

async function closeChromeSyncTabs(accountLabel = "当前账号", profileId = "current") {
  const stage = `chrome-cleanup-${profileId}`;
  try {
    await runCommand("close_chrome_sync_tabs");
    appendChromeBrowserSyncLog({
      stage,
      status: "success",
      message: `浏览器清理｜${accountLabel}｜临时 Chrome 标签已关闭`,
    });
  } catch (error) {
    appendChromeBrowserSyncLog({
      stage,
      status: "error",
      message: `浏览器清理｜${accountLabel}｜失败：${String(error)}`,
    });
  }
}

async function syncAccountViaChrome(session: ChromeSessionInfo) {
  if (chromeBrowserSyncingProfileId.value) return;
  startChromeBrowserSyncLog();
  const accountLabel = session.username || session.accountName || session.profileName;
  const stage = `manual-sync-${session.profileId}`;
  appendChromeBrowserSyncLog({
    stage,
    status: "running",
    message: `单账户同步｜${accountLabel}｜开始处理账号资料、Key 和模型`,
  });
  let accountSucceeded = false;
  let modelsSummary: ChromeModelsSyncSummary = {
    success: false,
    succeeded: 0,
    failed: 0,
    keyCount: 0,
    modelCount: 0,
    error: "",
  };
  try {
    accountSucceeded = await runChromeAccountSync(session);
    if (accountSucceeded && chromeSessionSite.value) {
      const refreshed = chromeSessions.value.find((item) => item.profileId === session.profileId);
      if (refreshed) {
        modelsSummary = await syncChromeModelsForSessions(chromeSessionSite.value, [refreshed]);
      }
    }
  } finally {
    await closeChromeSyncTabs(accountLabel, session.profileId);
    stopChromeBrowserSyncTimer();
    chromeBrowserSyncingProfileId.value = "";
  }
  const succeeded = accountSucceeded && modelsSummary.success;
  appendChromeBrowserSyncLog({
    stage,
    status: succeeded ? "success" : "error",
    message: succeeded
      ? `单账户同步｜${accountLabel}｜完成：${modelsSummary.keyCount} 个 Key，${modelsSummary.modelCount} 个模型`
      : `单账户同步｜${accountLabel}｜未完成，请查看上方失败步骤`,
  });
  if (succeeded) {
    showToast(`已更新 ${accountLabel}：${modelsSummary.keyCount} 个 Key、${modelsSummary.modelCount} 个模型`);
  } else if (accountSucceeded) {
    showToast(`账号资料已更新，但 Key/模型同步失败：${chromeBrowserSyncError.value}`, true);
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
  extractOnly = false,
  refreshPending = false,
): Promise<ChromeUsageScanResult | null> {
  if (chromeUsageScanning.value) return chromeUsageScanResult.value;
  if (chromeSessionDialogOpen.value) {
    appendChromeBrowserSyncLog({
      stage: "scan-running",
      status: "running",
      message: "会话扫描｜正在扫描 Chrome 配置并检测账号会话",
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
        extractOnly,
        refreshPending,
      },
    );
    chromeUsageScanResult.value = result;
    await loadLibrary();
    if (chromeSessionDialogOpen.value) {
      appendChromeBrowserSyncLog({
        stage: "scan-running",
        status: "success",
        message: `会话扫描｜完成：${result.detected} 个站点、${result.accounts} 个合法账号${result.newlyMarked ? `，新待定 ${result.newlyMarked} 个` : ""}${result.warnings ? `，${result.warnings} 个警告` : ""}`,
      });
    }
    if (notify) {
      showToast(
        `账号缓存已更新：${result.detected} 个站点、${result.accounts} 个合法账号${result.newlyMarked ? `，新待定 ${result.newlyMarked} 个` : ""}${result.warnings ? `，${result.warnings} 个警告` : ""}`,
      );
    }
    return result;
  } catch (error) {
    if (chromeSessionDialogOpen.value) {
      appendChromeBrowserSyncLog({
        stage: "scan-running",
        status: "error",
        message: `会话扫描｜失败：${String(error)}`,
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
    message: "会话扫描｜正在读取 Chrome 配置和站点账号会话",
  });
  try {
    const result = await analyzeChromeUsage(
      true,
      site.id,
      chromeBrowserSyncRunId,
      undefined,
      false,
      Boolean(site.isPending),
    );
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
      message: `会话扫描｜完成：检测到 ${chromeSessions.value.length} 个 Chrome 账号会话`,
    });
    const sessionsToProcess = [...chromeSessions.value];
    let refreshedAccounts = 0;
    let reusedAccounts = 0;
    let completedAccounts = 0;
    let failedAccounts = 0;
    let totalKeyCount = 0;
    let totalModelCount = 0;
    appendChromeBrowserSyncLog({
      stage: "account-bundles",
      status: "info",
      message: `同步计划｜共 ${sessionsToProcess.length} 个账号，将按顺序完成账号资料、Key、模型和标签清理`,
    });

    for (const [index, initialSession] of sessionsToProcess.entries()) {
      let session = initialSession;
      const accountLabel = session.username || session.accountName || session.profileName;
      const progressLabel = `账户 ${index + 1}/${sessionsToProcess.length}`;
      const stage = `account-bundle-${session.profileId}`;
      let accountReady = true;
      let accountMode = "复用已有账号资料";
      let modelsSummary: ChromeModelsSyncSummary = {
        success: false,
        succeeded: 0,
        failed: 0,
        keyCount: 0,
        modelCount: 0,
        error: "",
      };
      appendChromeBrowserSyncLog({
        stage,
        status: "running",
        message: `${progressLabel}｜${accountLabel}｜开始`,
      });
      try {
        if (canSyncAccountViaChrome(session)) {
          accountMode = "通过 Cookie/refresh token 取得访问令牌并刷新账号资料";
          appendChromeBrowserSyncLog({
            stage: `${stage}-strategy`,
            status: "info",
            message: `账号资料｜${accountLabel}｜本地数据不可用，进入 Cookie/refresh token 回退流程`,
          });
          accountReady = await runChromeAccountSync(session, { reloadLibrary: false });
          const refreshed = chromeSessions.value.find((item) => item.profileId === session.profileId);
          if (refreshed) session = refreshed;
          if (accountReady) refreshedAccounts += 1;
        } else if (session.isValid) {
          reusedAccounts += 1;
          appendChromeBrowserSyncLog({
            stage: `${stage}-strategy`,
            status: "info",
            message: `账号资料｜${accountLabel}｜本地数据有效，跳过浏览器刷新`,
          });
        } else {
          accountReady = false;
          accountMode = "账号访问令牌不可用";
          appendChromeBrowserSyncLog({
            stage: `${stage}-strategy`,
            status: "error",
            message: `账号资料｜${accountLabel}｜访问令牌不可用，且当前类型不支持认证回退`,
          });
        }

        if (accountReady && session.isValid) {
          modelsSummary = await syncChromeModelsForSessions(chromeSessionSite.value, [session], {
            clearCache: false,
            reloadLibrary: false,
          });
          totalKeyCount += modelsSummary.keyCount;
          totalModelCount += modelsSummary.modelCount;
        }

        const bundleSucceeded = accountReady && session.isValid && modelsSummary.success;
        if (bundleSucceeded) {
          completedAccounts += 1;
        } else {
          failedAccounts += 1;
        }
        appendChromeBrowserSyncLog({
          stage,
          status: bundleSucceeded ? "success" : "error",
          message: bundleSucceeded
            ? `${progressLabel}｜${accountLabel}｜完成：${accountMode}，${modelsSummary.keyCount} 个 Key，${modelsSummary.modelCount} 个模型`
            : `${progressLabel}｜${accountLabel}｜未完成：${accountReady ? "Key/模型同步失败" : accountMode}`,
        });
      } finally {
        await closeChromeSyncTabs(accountLabel, session.profileId);
      }
    }
    await loadLibrary();
    chromeSessionSite.value = sites.value.find((item: any) => item.id === site.id) ?? chromeSessionSite.value;
    chromeSessions.value = chromeUsageAccounts.value[site.id] ?? chromeSessions.value;
    appendChromeBrowserSyncLog({
      stage: "scan-complete",
      status: failedAccounts > 0 ? "error" : "success",
      message: failedAccounts > 0
        ? `同步汇总｜${completedAccounts}/${sessionsToProcess.length} 个账号完成，${failedAccounts} 个失败；共 ${totalKeyCount} 个 Key、${totalModelCount} 个模型`
        : `同步汇总｜${completedAccounts}/${sessionsToProcess.length} 个账号全部完成；账号资料刷新 ${refreshedAccounts} 个，复用 ${reusedAccounts} 个；共 ${totalKeyCount} 个 Key、${totalModelCount} 个模型`,
    });
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
    syncAccountViaChrome,
    copyChromeSession,
    closeChromeSessionDialog,
    analyzeChromeUsage,
    syncChromeSession,
  };
}
