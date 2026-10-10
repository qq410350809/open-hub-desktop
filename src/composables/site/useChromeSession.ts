import { ref, computed, type ComputedRef } from "vue";
import { runCommand } from "../core/ipc";
import { useLibrary } from "./useLibrary";
import { useToast } from "../core/useToast";
import { useUIState } from "../ui/useUIState";
import { useConfirm } from "../ui/useConfirm";
import type {
  ChromeSessionInfo,
  ChromeUsageScanResult,
  SyncLogEntry,
  SyncProgressStatus,
  SyncSitesProgress,
} from "../../types";
import {
  isBaiheibaiSystem,
  isNewApiCompatible,
  isSub2ApiSystem,
  isUnknownSystemType,
  normalizeSystemType,
  supportsSiteToken,
} from "../../types";

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
// 强制停止标记：用户在关闭确认里选择“强制停止并关闭”后置位。
// 账号同步循环与单账号同步在拿到结果后据此放弃后续账号与提示；
// 下一次发起同步时复位。后端通过 cancel_site_account_sync 按 runId
// 在阶段边界停止打开新的 Chrome 标签页。
let chromeSyncForceStopped = false;
// 正在进行的账号同步 runId 集合（含批量会话同步的每个站点）。
// 强制停止时逐个取消，避免批量并行下只取消了最后一个 run。
const activeAccountSyncRunIds = new Set<number>();
// 浏览器兜底冷却已改为后端持久化（site_accounts.browser_fallback_*，指数退避），
// 前端从会话信息的 browserFallbackCooldownMs 读取：重启不丢失，自动调度与
// 手动弹窗共用同一份状态；手动点击账号行的 Chrome 同步按钮不受冷却限制。
let chromeBrowserSyncTimer: number | null = null;

/** 同步日志输入口：消息可以来自本模块内部，也可以由批量池注入自己的收集器。 */
export type SyncLogSink = (entry: {
  stage: string;
  status: SyncProgressStatus;
  message: string;
}) => void;

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

function canSyncAccountViaChromeFor(site: any, session: ChromeSessionInfo): boolean {
  const systemType = site?.systemType ?? "";
  // Sub2API 与 NewAPI 一样支持浏览器兜底：直连被 WAF 拦截时，页面内同源请求能过盾。
  const supportsChromeBridge = isNewApiCompatible(systemType) || isSub2ApiSystem(systemType);
  return supportsChromeBridge && needsChromeAccountFallback(session);
}

function canSyncAccountViaChrome(session: ChromeSessionInfo): boolean {
  return canSyncAccountViaChromeFor(chromeSessionSite.value, session);
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

interface ChromeAccountSyncOptions {
  /** 目标站点；默认取当前弹窗站点。 */
  site?: any;
  reloadLibrary?: boolean;
  /** 日志收集器；批量池注入自己的收集器后不会再写弹窗日志。 */
  log?: SyncLogSink;
  /** 停止检查；批量池注入自己的强制停止标记。 */
  shouldStop?: () => boolean;
  /** 显式指定后端 runId（批量会话同步按 syncRunId 分配 10000 段位，供弹窗路由细节日志）。 */
  runIdOverride?: number;
}

/**
 * 刷新单个账号的余额和认证信息（签到仅对有签到集成的架构执行）。
 * 成功返回后端回传的会话（已含最新冷却与错误状态），失败返回 null。
 */
async function runChromeAccountSync(
  session: ChromeSessionInfo,
  options: ChromeAccountSyncOptions = {},
): Promise<ChromeSessionInfo | null> {
  const site = options.site ?? chromeSessionSite.value;
  const log = options.log ?? appendChromeBrowserSyncLog;
  const shouldStop = options.shouldStop ?? (() => chromeSyncForceStopped);
  if (shouldStop()) return null;
  if (!site) return null;
  // 「白与黑」没有签到集成，日志文案不提签到，避免误导。
  const accountScopeLabel = isBaiheibaiSystem(site.systemType)
    ? "余额与认证信息"
    : "余额、签到和认证信息";
  const reloadLibrary = options.reloadLibrary ?? true;
  // 只有属于当前弹窗站点的请求才更新弹窗状态（批量并行时其它站点不污染弹窗）。
  const dialogScoped = chromeSessionDialogOpen.value && chromeSessionSite.value?.id === site.id;
  const runId = options.runIdOverride ?? ++chromeBrowserSyncRunId;
  activeAccountSyncRunIds.add(runId);
  const accountLabel = session.username || session.accountName || session.profileName;
  const stage = `account-refresh-${session.profileId}`;
  log({
    stage,
    status: "running",
    message: `账号资料｜${accountLabel}｜正在刷新${accountScopeLabel}`,
  });
  if (dialogScoped) chromeBrowserSyncingProfileId.value = session.profileId;
  try {
    const refreshed = await runCommand<ChromeSessionInfo>("sync_site_account_via_chrome", {
      siteId: site.id,
      profileId: session.profileId,
      runId,
    });
    // 强制停止后放弃本次结果（后端已在阶段边界中断，结果不可信）
    if (shouldStop()) return null;
    if (dialogScoped) {
      const index = chromeSessions.value.findIndex((item) => item.profileId === session.profileId);
      if (index >= 0) chromeSessions.value[index] = refreshed;
    }
    // 失败冷却与失败原因由后端写入 site_accounts（sync_error / browser_fallback_*），
    // 返回的 refreshed 会话已带最新的 browserFallbackCooldownMs。
    if (reloadLibrary) await loadLibrary();
    log({
      stage,
      status: "success",
      message: `账号资料｜${accountLabel}｜完成：${accountScopeLabel}已更新`,
    });
    return refreshed;
  } catch (error) {
    const message = String(error);
    if (dialogScoped) {
      chromeBrowserSyncError.value = chromeBrowserSyncError.value
        ? `${chromeBrowserSyncError.value}\n${accountLabel}：${message}`
        : `${accountLabel}：${message}`;
    }
    log({
      stage,
      status: "error",
      message: `账号资料｜${accountLabel}｜失败：${message}`,
    });
    return null;
  } finally {
    activeAccountSyncRunIds.delete(runId);
    if (dialogScoped && chromeBrowserSyncingProfileId.value === session.profileId) {
      chromeBrowserSyncingProfileId.value = "";
    }
  }
}

async function closeChromeSyncTabs(
  accountLabel = "当前账号",
  profileId = "current",
  scopeUrl?: string,
  log: SyncLogSink = appendChromeBrowserSyncLog,
) {
  const scope = (scopeUrl ?? "").trim();
  // 无站点地址就无法限定清理范围：宁可不清理，也不能退化成全局关闭——
  // 批量会话同步期间全局清理会误关其它站点正在使用的桥接标签。
  // （强制停止的全局清理由调用方直接走 close_chrome_sync_tabs。）
  if (!scope) return;
  const stage = `chrome-cleanup-${profileId}`;
  try {
    await runCommand("close_chrome_sync_tabs", { scopeUrl: scope });
    log({
      stage,
      status: "success",
      message: `浏览器清理｜${accountLabel}｜临时 Chrome 标签已关闭`,
    });
  } catch (error) {
    log({
      stage,
      status: "error",
      message: `浏览器清理｜${accountLabel}｜失败：${String(error)}`,
    });
  }
}

/** 取消所有正在进行的账号同步 run（批量并行下逐个取消，而非只取消最后一个）。 */
async function cancelAllChromeAccountSyncs() {
  const runIds = [...activeAccountSyncRunIds];
  await Promise.all(
    runIds.map((runId) =>
      runCommand<boolean>("cancel_site_account_sync", { runId }).catch(() => false),
    ),
  );
}

async function syncAccountViaChrome(session: ChromeSessionInfo) {
  if (chromeBrowserSyncingProfileId.value) return;
  // 批量会话同步期间拒绝单账号手动同步：共用桥接通道，交错会互相抢占。
  if (chromeSessionSyncActive.value) {
    showToast("已有 Chrome 会话同步正在进行，请等待结束后重试", true);
    return;
  }
  chromeSyncForceStopped = false;
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
    accountSucceeded = (await runChromeAccountSync(session)) !== null;
  } finally {
    await closeChromeSyncTabs(accountLabel, session.profileId, chromeSessionSite.value?.apiBaseUrl);
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
  } else if (!chromeSyncForceStopped) {
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


async function closeChromeSessionDialog() {
  if (chromeBrowserSyncingProfileId.value || chromeModelsSyncing.value) {
    const accepted = await confirm({
      title: "强制停止同步",
      message: "正在同步账号额度与会话资料。强制停止后本次同步立即中断，当前账号数据可能不完整，临时打开的 Chrome 标签页会被清理。确定停止并关闭吗？",
      confirmText: "强制停止并关闭",
      cancelText: "继续同步",
      danger: true,
    });
    if (!accepted) return;
    chromeSyncForceStopped = true;
    chromeSessionRequestId += 1;
    chromeBrowserSyncRunId += 1;
    // 逐个取消全部活跃 run：批量并行时只取消最后一个会漏掉其它站点。
    await cancelAllChromeAccountSyncs();
    try {
      await runCommand("close_chrome_sync_tabs");
    } catch {
      // 标签清理失败同样不阻塞关闭
    }
    chromeBrowserSyncingProfileId.value = "";
    chromeSessionSyncActive.value = false;
    stopChromeBrowserSyncTimer();
  }
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

export interface SiteBundleSyncSummary {
  /** 站点账号总数。 */
  total: number;
  /** 成功完成的账号数。 */
  completed: number;
  /** 未完成的账号数。 */
  failed: number;
  /** 通过浏览器刷新成功的账号数。 */
  refreshed: number;
  /** 本地数据有效、跳过浏览器刷新的账号数。 */
  reused: number;
}

interface SiteBundleSyncOptions {
  log: SyncLogSink;
  shouldStop: () => boolean;
  /**
   * 分配后端 runId。批量会话同步按 `syncRunId * 10000 + 序号` 分配，
   * 让 SyncSitesDialog 的嵌套进度过滤（floor(runId / 10000) === syncRunId）
   * 把每个账号的浏览器阶段细节路由回批量弹窗日志。
   */
  allocateRunId?: () => number;
}

/**
 * 单站点全部账号的会话同步核心：站内账号严格顺序处理，
 * 逐账号刷新（浏览器兜底）并在每个账号后清理该站点的桥接标签。
 * 弹窗（单站点）与批量池（多站点并行）共用此实现，仅注入不同的
 * 日志收集器、停止检查与 runId 登记回调。
 */
async function syncSiteAccountBundles(
  site: any,
  sessions: ChromeSessionInfo[],
  options: SiteBundleSyncOptions,
): Promise<SiteBundleSyncSummary> {
  const { log, shouldStop } = options;
  const accountOnly = isUnknownSystemType(site.systemType);
  const sessionsToProcess = [...sessions];
  let refreshedAccounts = 0;
  let reusedAccounts = 0;
  let completedAccounts = 0;
  let failedAccounts = 0;
  log({
    stage: `site-plan-${site.id}`,
    status: "info",
    message: accountOnly
      ? `同步计划｜${site.name} 共 ${sessionsToProcess.length} 个账号，未知架构站点仅同步账号会话，不查询签到与余额`
      : `同步计划｜${site.name} 共 ${sessionsToProcess.length} 个账号，将按顺序同步额度与会话资料`,
  });

  for (const [index, initialSession] of sessionsToProcess.entries()) {
    // 强制停止后终止剩余账号；当前账号的结果由 runChromeAccountSync 放弃
    if (shouldStop()) break;
    let session = initialSession;
    const accountLabel = session.username || session.accountName || session.profileName;
    const progressLabel = `账户 ${index + 1}/${sessionsToProcess.length}`;
    const stage = `account-bundle-${session.profileId}`;
    let accountReady = true;
    let accountMode = "复用已有账号资料";
    log({
      stage,
      status: "running",
      message: accountOnly
        ? `${progressLabel}｜${accountLabel}｜开始同步账号`
        : `${progressLabel}｜${accountLabel}｜开始同步额度`,
    });
    try {
      if (accountOnly) {
        accountMode = "仅同步账号（未知架构：不查询签到与余额）";
        log({
          stage: `${stage}-strategy`,
          status: "info",
          message: `账号关联｜${accountLabel}｜已检测到 Chrome 登录会话，仅建立账号关联`,
        });
      } else if (canSyncAccountViaChromeFor(site, session)) {
        const useRefreshAuth = normalizeSystemType(site.systemType) === "newapi2";
        accountMode = useRefreshAuth
          ? "通过 refresh token 取得访问令牌并刷新额度"
          : "通过 Cookie 刷新额度";
        log({
          stage: `${stage}-strategy`,
          status: "info",
          message: `账号额度｜${accountLabel}｜进入${useRefreshAuth ? " refresh token" : " Cookie"}同步流程`,
        });
        const refreshed = await runChromeAccountSync(session, {
          site,
          reloadLibrary: false,
          log,
          shouldStop,
          runIdOverride: options.allocateRunId?.(),
        });
        accountReady = refreshed !== null;
        if (refreshed) {
          session = refreshed;
          refreshedAccounts += 1;
        }
      } else if (supportsSiteToken(site.systemType)) {
        // 「令牌由用户维护」的架构（白与黑）没有浏览器凭据通道：后端会用账号行里
        // 保存的站点令牌调站点自己的额度接口，全程不拉起 Chrome。
        accountMode = "用站点访问令牌刷新额度";
        log({
          stage: `${stage}-strategy`,
          status: "info",
          message: `账号额度｜${accountLabel}｜使用站点访问令牌刷新额度`,
        });
        const refreshed = await runChromeAccountSync(session, {
          site,
          reloadLibrary: false,
          log,
          shouldStop,
          runIdOverride: options.allocateRunId?.(),
        });
        accountReady = refreshed !== null;
        if (refreshed) {
          session = refreshed;
          refreshedAccounts += 1;
        }
      } else if (session.isValid && session.syncError) {
        // 缓存数据仍在，但最近一次额度刷新失败：不能当作同步成功。
        accountReady = false;
        accountMode = "沿用本地缓存（额度刷新失败）";
        log({
          stage: `${stage}-strategy`,
          status: "error",
          message: `账号额度｜${accountLabel}｜额度刷新失败，沿用本地缓存：${session.syncError}`,
        });
      } else if (session.isValid) {
        reusedAccounts += 1;
        log({
          stage: `${stage}-strategy`,
          status: "info",
          message: `账号额度｜${accountLabel}｜本地数据有效，跳过浏览器刷新`,
        });
      } else {
        accountReady = false;
        accountMode = "账号认证不可用";
        log({
          stage: `${stage}-strategy`,
          status: "error",
          message: `账号额度｜${accountLabel}｜认证不可用，且当前类型不支持认证回退`,
        });
      }

      const bundleSucceeded = accountOnly ? accountReady : accountReady && session.isValid;
      if (bundleSucceeded) {
        completedAccounts += 1;
      } else {
        failedAccounts += 1;
      }
      log({
        stage,
        status: bundleSucceeded ? "success" : "error",
        message: bundleSucceeded
          ? `${progressLabel}｜${accountLabel}｜完成：${accountMode}`
          : `${progressLabel}｜${accountLabel}｜未完成：${accountMode}`,
      });
    } finally {
      await closeChromeSyncTabs(accountLabel, session.profileId, site.apiBaseUrl, log);
    }
  }

  return {
    total: sessionsToProcess.length,
    completed: completedAccounts,
    failed: failedAccounts,
    refreshed: refreshedAccounts,
    reused: reusedAccounts,
  };
}

async function syncChromeSession(site: any, trigger: HTMLElement) {
  // 与批量会话同步共用 Chrome 桥接通道：批量在跑时拒绝打开单站点同步，
  // 避免同源桥接标签互相抢占。
  if (chromeSessionSyncActive.value) {
    showToast("已有 Chrome 会话同步正在进行，请等待结束后重试", true);
    return;
  }
  // 未知架构站点没有可识别的签到/余额接口：同步只建立 Chrome 账号会话关联，
  // 后端扫描会跳过账号接口探测，前端也不做单账号额度刷新。
  const accountOnly = isUnknownSystemType(site.systemType);
  const requestId = ++chromeSessionRequestId;
  chromeSyncForceStopped = false;
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

    // 扫描后站点记录可能已更新（如新标注的待定状态），用库中最新记录驱动同步。
    const summary = await syncSiteAccountBundles(chromeSessionSite.value ?? site, chromeSessions.value, {
      log: appendChromeBrowserSyncLog,
      shouldStop: () => chromeSyncForceStopped,
    });
    if (chromeSyncForceStopped) return;
    // 收尾再按站点作用域清理一次：整轮中断或账号循环提前退出时，
    // 仍可能残留该站点的桥接标签（逐账号清理只覆盖已完成账号）。
    await closeChromeSyncTabs("站点收尾", `site-${site.id}`, site.apiBaseUrl);

    await loadLibrary();
    chromeSessionSite.value = sites.value.find((item: any) => item.id === site.id) ?? chromeSessionSite.value;
    chromeSessions.value = chromeUsageAccounts.value[site.id] ?? chromeSessions.value;
    appendChromeBrowserSyncLog({
      stage: "scan-complete",
      status: summary.failed > 0 ? "error" : "success",
      message: summary.failed > 0
        ? `同步汇总｜${summary.completed}/${summary.total} 个账号完成，${summary.failed} 个失败`
        : accountOnly
          ? `同步汇总｜${summary.completed}/${summary.total} 个账号已关联 Chrome 会话；未知架构站点不查询签到与余额`
          : `同步汇总｜${summary.completed}/${summary.total} 个账号全部完成；额度刷新 ${summary.refreshed} 个，有效 ${summary.reused} 个`,
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
    syncSiteAccountBundles,
    closeChromeSyncTabs,
    cancelAllChromeAccountSyncs,
    syncAccountViaChrome,
    deleteSiteAccount,
    closeChromeSessionDialog,
    analyzeChromeUsage,
    syncChromeSession,
  };
}
