import { ref, computed, type ComputedRef } from "vue";
import { runCommand, useLibrary } from "./useLibrary";
import { useToast } from "./useToast";
import { useUIState } from "./useUIState";
import { useConfirm } from "./useConfirm";
import type {
  ChromeSessionInfo,
  ChromeSessionValue,
  ChromeUsageScanResult,
  SyncLogEntry,
  SyncSitesProgress,
} from "../types";
import { isNewApiCompatible, normalizeSystemType } from "../types";

const { sites, usageSites, loadLibrary } = useLibrary();
const { showToast } = useToast();
const { chromeSessionDialogOpen } = useUIState();
const { confirm } = useConfirm();

// Chrome 会话弹窗数据
const chromeSessionSite = ref<any>(null);
const chromeSessionTrigger = ref<HTMLElement | null>(null);
const chromeSessions = ref<ChromeSessionInfo[]>([]);
const chromeSessionsLoading = ref(false);
const chromeSessionsError = ref("");
const chromeSessionCopyingProfileId = ref("");
const chromeBrowserSyncingProfileId = ref("");
const chromeModelsSyncing = ref(false);
// 覆盖整轮同步（扫描 → 逐账号资料/Key/模型 → 清理标签页）的总开关。
// 账号之间的衔接间隙（如等待关闭临时 Chrome 标签页）没有其他在跑标志，
// 没有它的话状态标签会短暂误显示“已完成”。
const chromeSessionSyncActive = ref(false);
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
// 浏览器兜底冷却已改为后端持久化（site_accounts.browser_fallback_*，指数退避），
// 前端从会话信息的 browserFallbackCooldownMs 读取：重启不丢失，自动调度与
// 手动弹窗共用同一份状态；手动点击账号行的 Chrome 同步按钮不受冷却限制。
let chromeBrowserSyncTimer: number | null = null;

const chromeUsageAccounts: ComputedRef<Record<string, ChromeSessionInfo[]>> = computed(() =>
  Object.fromEntries(
    usageSites.value.map((site) => [
      site.siteId,
      (site.sessions ?? []).slice().sort((a, b) =>
        (a.username || a.accountName || a.profileName || "").localeCompare(
          b.username || b.accountName || b.profileName || "",
          undefined,
          { numeric: true, sensitivity: "base" }
        )
      ),
    ]),
  ),
);

function needsChromeAccountFallback(session: ChromeSessionInfo): boolean {
  // 账号数据无效或存在同步错误时，按站点配置进入 Cookie 或 refresh token 回退。
  return !session.isValid || Boolean(session.syncError);
}

function canSyncAccountViaChrome(session: ChromeSessionInfo): boolean {
  return isNewApiCompatible(chromeSessionSite.value?.systemType ?? "")
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
    // 失败冷却与失败原因由后端写入 site_accounts（sync_error / browser_fallback_*），
    // 返回的 refreshed 会话已带最新的 browserFallbackCooldownMs。
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
  chromeSessionSyncActive.value = true;
  const accountLabel = session.username || session.accountName || session.profileName;
  const stage = `manual-sync-${session.profileId}`;
  appendChromeBrowserSyncLog({
    stage,
    status: "running",
    message: `单账户同步｜${accountLabel}｜开始同步额度与会话资料`,
  });
  let accountSucceeded = false;
  try {
    accountSucceeded = await runChromeAccountSync(session);
  } finally {
    await closeChromeSyncTabs(accountLabel, session.profileId);
    chromeSessionSyncActive.value = false;
    stopChromeBrowserSyncTimer();
    chromeBrowserSyncingProfileId.value = "";
  }
  appendChromeBrowserSyncLog({
    stage,
    status: accountSucceeded ? "success" : "error",
    message: accountSucceeded
      ? `单账户同步｜${accountLabel}｜额度与会话资料同步完成`
      : `单账户同步｜${accountLabel}｜未完成，请查看上方失败步骤`,
  });
  if (accountSucceeded) {
    showToast(`已更新 ${accountLabel} 额度与账号资料`);
  } else {
    showToast(`Chrome 同步失败：${chromeBrowserSyncError.value}`, true);
  }
}

/// 删除站点下指定 Chrome 配置账号的关联记录（额度、令牌与模型缓存一并移除）。
/// 仅解除本机关联，不影响 Chrome 配置本身；重新同步会再次建立关联。
async function deleteSiteAccount(site: any, session: ChromeSessionInfo) {
  const accountLabel = session.username || session.accountName || session.profileName;
  const accepted = await confirm({
    title: "删除会话账号",
    message: `确定删除「${site?.name ?? "该站点"}」下账号「${accountLabel}」（Chrome 配置：${session.profileName}）吗？将同时移除该账号的额度、令牌与模型缓存，且不会影响 Chrome 配置本身。`,
    confirmText: "删除",
    danger: true,
  });
  if (!accepted) return;
  try {
    await runCommand<void>("delete_site_account", {
      siteId: site.id,
      profileId: session.profileId,
    });
    await loadLibrary();
    showToast(`已删除账号「${accountLabel}」`);
  } catch (error) {
    showToast(`删除账号失败：${String(error)}`, true);
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
  chromeSessionSyncActive.value = true;
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
    appendChromeBrowserSyncLog({
      stage: "account-bundles",
      status: "info",
      message: `同步计划｜共 ${sessionsToProcess.length} 个账号，将按顺序同步额度与会话资料`,
    });

    for (const [index, initialSession] of sessionsToProcess.entries()) {
      let session = initialSession;
      const accountLabel = session.username || session.accountName || session.profileName;
      const progressLabel = `账户 ${index + 1}/${sessionsToProcess.length}`;
      const stage = `account-bundle-${session.profileId}`;
      let accountReady = true;
      let accountMode = "复用已有账号资料";
      appendChromeBrowserSyncLog({
        stage,
        status: "running",
        message: `${progressLabel}｜${accountLabel}｜开始同步额度`,
      });
      try {
        if (canSyncAccountViaChrome(session)) {
          const useRefreshAuth = normalizeSystemType(chromeSessionSite.value?.systemType ?? "") === "newapi2";
          accountMode = useRefreshAuth
            ? "通过 refresh token 取得访问令牌并刷新额度"
            : "通过 Cookie 刷新额度";
          appendChromeBrowserSyncLog({
            stage: `${stage}-strategy`,
            status: "info",
            message: `账号额度｜${accountLabel}｜进入${useRefreshAuth ? " refresh token" : " Cookie"}同步流程`,
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
            message: `账号额度｜${accountLabel}｜本地数据有效，跳过浏览器刷新`,
          });
        } else {
          accountReady = false;
          accountMode = "账号认证不可用";
          appendChromeBrowserSyncLog({
            stage: `${stage}-strategy`,
            status: "error",
            message: `账号额度｜${accountLabel}｜认证不可用，且当前类型不支持认证回退`,
          });
        }

        const bundleSucceeded = accountReady && session.isValid;
        if (bundleSucceeded) {
          completedAccounts += 1;
        } else {
          failedAccounts += 1;
        }
        appendChromeBrowserSyncLog({
          stage,
          status: bundleSucceeded ? "success" : "error",
          message: bundleSucceeded
            ? `${progressLabel}｜${accountLabel}｜完成：${accountMode}`
            : `${progressLabel}｜${accountLabel}｜未完成：${accountMode}`,
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
        ? `同步汇总｜${completedAccounts}/${sessionsToProcess.length} 个账号完成，${failedAccounts} 个失败`
        : `同步汇总｜${completedAccounts}/${sessionsToProcess.length} 个账号全部完成；额度刷新 ${refreshedAccounts} 个，有效 ${reusedAccounts} 个`,
    });
  } finally {
    if (requestId === chromeSessionRequestId) {
      chromeSessionSyncActive.value = false;
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
    chromeSessionSyncActive,
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
    deleteSiteAccount,
    closeChromeSessionDialog,
    analyzeChromeUsage,
    syncChromeSession,
  };
}
