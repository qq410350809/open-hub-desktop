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
import { isUnknownSystemType, supportsKeyDiscovery } from "../../types";
import type { SiteModelHealth } from "../../types";

const { sites, usageSites, loadLibrary } = useLibrary();
const { showToast } = useToast();
const { filteredSites, runawayFilter, usageFilter } = useFilterState();
const { syncDialogOpen } = useUIState();
const { analyzeChromeUsage, syncSiteAccountBundles, closeChromeSyncTabs, cancelAllChromeAccountSyncs, chromeSessionSyncActive } = useChromeSession();
const { openExternal } = useSiteActions();

/** 批量会话同步的站点级并发上限：Chrome AppleEvent 通道与可见标签焦点互相挤占，
 *  2 路是收益与稳定性的折中；站内账号始终串行。 */
const SESSION_SYNC_CONCURRENCY = 2;

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
const syncDialogMode = ref<"remote" | "quota" | "session">("remote");
const syncDialogUsage = ref<"personal" | "pending">("personal");
const syncDialogSiteIds = ref<string[]>([]);

let syncRunId = 0;
let syncLogId = 0;
let syncStartedAt = 0;
let syncLastLogAt = 0;
let syncTimer: number | null = null;
let remoteUserRequestId = 0;
/** 批量会话同步的强制停止标记：停止后不再取新站点/账号，并取消全部在途 run。 */
let sessionSyncForceStopped = false;
/** 批量额度同步的强制停止标记：与「会话同步」同款撤销语义（逐站点/逐账号边界生效）。 */
let quotaSyncForceStopped = false;

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
  // 嵌套段位由批量会话同步与批量额度同步分配（syncRunId * 10000 + 序号）。
  if (syncDialogMode.value !== "session" && syncDialogMode.value !== "quota") return;
  if (Math.floor(progress.runId / 10_000) !== syncRunId) return;
  appendSyncLog({
    ...progress,
    stage: `chrome-detail-${progress.runId}-${progress.stage}`,
    message: `Chrome：${progress.message}`,
  });
}

function openSyncDialog(
  explicitMode?: "remote" | "quota" | "session",
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
    // 额度同步只针对可识别架构的站点；未知站点无签到/额度能力，直接排除。
    // 会话同步保留全部选中站点：未知架构站点同样能建立 Chrome 账号关联。
    syncDialogSiteIds.value = syncDialogMode.value === "quota"
      ? explicitSiteIds.filter((id) => {
          const site = sites.value.find((s) => s.id === id);
          return site ? !isUnknownSystemType(site.systemType) : false;
        })
      : [...explicitSiteIds];
  } else if (syncDialogMode.value === "quota") {
    const targetUsage = syncDialogUsage.value;
    const targetSites = sites.value.filter((site) =>
      (targetUsage === "pending" ? site.isPending : site.isPersonal) &&
      !isUnknownSystemType(site.systemType),
    );
    syncDialogSiteIds.value = targetSites.length > 0
      ? targetSites.map((s) => s.id)
      : filteredSites.value
          .filter((s) => !isUnknownSystemType(s.systemType))
          .map((s) => s.id);
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
      appendSyncLog({ stage: "detect", status: "info", message: "本批没有新增站点，无需类型检测" });
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
  /** 全站模型健康度（模型 ID → 健康度）；站点无该接口时为空。 */
  modelHealth?: Record<string, SiteModelHealth>;
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
        .filter((session) => {
          if (!session.isValid) return false;
          // 已有 Key 的账号照旧刷新。
          if (session.apiKeyCount > 0) return true;
          // 其余情况只在支持 Key 接口的架构上处理：未知平台纳入只会换来错误。
          if (!supportsKeyDiscovery(site.systemType)) return false;
          // 从未同步过 → 首次发现（漏掉它，账号的 Key 缓存永远是空的，Sub2API 余额
          // 只能退回会话令牌，令牌一过期整站就报 401）；上次同步失败 → 重试
          // （卡片上写着“同步失败，点击重试”，批量同步不该当它不存在）。
          // 已经成功同步过、站点确实没有 Key 的账号仍然跳过，避免反复打扰站点。
          return !session.apiCountsSynced || Boolean(session.apiSyncError);
        })
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
    // 先清空本次要同步站点的旧 Key/模型缓存，再开始逐账号拉取。
    // 必须集中在并发池之前做：clear_site_model_cache_for_site 按 site_id 删除
    // 该站点【全部账号】的缓存行，如果放进池里"每个站点清一次"，同一站点的两个
    // 账号被不同 worker 并发处理时就会互相踩：后一个 worker 的 clear 会把前一个
    // 已经写入的账号行删掉，表现为"每次同步总有一个账号成功、另一个失败/为空"。
    const targetSiteIds = [...new Set(targets.map((target) => target.site.id))];
    for (const siteId of targetSiteIds) {
      await runCommand("clear_site_model_cache_for_site", { siteId });
    }
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

/**
 * 批量会话同步：一次扫描全部目标站点，然后站点级并发池（站内账号串行）。
 *
 * - 站点之间最多 {@link SESSION_SYNC_CONCURRENCY} 路并行；同一站点的账号永远顺序处理，
 *   避免同源桥接标签互相抢占（wait_for_new_chrome_tab 按 origin 匹配）。
 * - 每个账号的后端 runId 按 `syncRunId * 10000 + 序号` 分配，让嵌套的
 *   chrome-account-sync-progress 事件能按 floor(runId / 10000) === syncRunId
 *   路由回本弹窗日志（ChromeSessionDialog 的进度通道不受影响）。
 * - 停止走 stopBatchSync（sessionSyncForceStopped + cancelAllChromeAccountSyncs()）：
 *   不再取新站点/新账号，并在途请求在后端阶段边界立即失败。
 */
async function runSessionSyncBatch(runId: number) {
  // 与单站点弹窗同步共用 Chrome 桥接通道：一方在跑时另一方必须拒绝，
  // 否则同源桥接标签会互相抢占。
  if (chromeSessionSyncActive.value) {
    appendSyncLog({ stage: "session-busy", status: "error", message: "已有 Chrome 会话同步正在进行，请等待结束后重试" });
    showToast("已有 Chrome 会话同步正在进行", true);
    syncRunState.value = "error";
    stopSyncTimer();
    return;
  }
  chromeSessionSyncActive.value = true;
  try {
    await runSessionSyncBatchInner(runId);
  } finally {
    chromeSessionSyncActive.value = false;
    sessionSyncForceStopped = false;
  }
}

async function runSessionSyncBatchInner(runId: number) {
  const siteIds = [...syncDialogSiteIds.value];
  appendSyncLog({
    stage: "session-scope",
    status: "info",
    message: `会话同步范围：已选 ${siteIds.length} 个站点；站点之间最多 ${SESSION_SYNC_CONCURRENCY} 路并行，站内账号按顺序处理`,
  });
  if (siteIds.length === 0) {
    appendSyncLog({ stage: "session-empty", status: "info", message: "当前没有可同步会话的站点" });
    syncRunState.value = "complete";
    stopSyncTimer();
    showToast("当前没有可同步会话的站点", true);
    return;
  }
  const siteMap = new Map(sites.value.map((site) => [site.id, site]));
  const targets = siteIds
    .map((id) => siteMap.get(id))
    .filter((site): site is NonNullable<typeof site> => Boolean(site));
  if (targets.length === 0) {
    appendSyncLog({ stage: "session-empty", status: "info", message: "选中的站点已不在当前库中" });
    syncRunState.value = "complete";
    stopSyncTimer();
    return;
  }

  appendSyncLog({ stage: "session-scan", status: "running", message: "正在扫描 Chrome 配置并刷新目标站点账号" });
  // 站点 id 是显式传入的：后端总会把它们并入账号刷新集合（refresh_pending
  // 只影响未显式指定的站点），因此这里固定 false 即可，归类保持不变。
  const scan = await analyzeChromeUsage(false, undefined, runId, siteIds, false, false);
  if (!scan) throw new Error("Chrome 会话扫描失败");
  if (sessionSyncForceStopped) {
    appendSyncLog({ stage: "session-stopped", status: "error", message: "已强制停止：扫描完成后不再处理账号" });
    syncRunState.value = "complete";
    stopSyncTimer();
    return;
  }
  appendSyncLog({
    stage: "session-scan",
    status: "success",
    message: `会话扫描完成：${scan.detected} 个站点有会话，${scan.accounts} 个合法账号`,
  });

  const sessionsBySite = new Map(scan.sites.map((item) => [item.siteId, item.sessions]));
  const totals = { total: 0, completed: 0, failed: 0, refreshed: 0, reused: 0 };
  let skippedSites = 0;
  let runSeq = 0;

  // 同源站点必须串行：桥接标签按 origin 匹配（wait_for_new_chrome_tab /
  // run_javascript_in_marked_chrome_tab），两个记录指向同一 origin 时并行会互相抢占。
  const originOf = (raw: string): string => {
    try {
      return new URL(raw).origin;
    } catch {
      return raw.trim();
    }
  };
  const originOrder: string[] = [];
  const groupsByOrigin = new Map<string, typeof targets>();
  for (const site of targets) {
    const key = originOf(site.apiBaseUrl || site.checkinUrl || site.id);
    const group = groupsByOrigin.get(key);
    if (group) {
      group.push(site);
    } else {
      groupsByOrigin.set(key, [site]);
      originOrder.push(key);
    }
  }
  let nextGroupIndex = 0;
  const workerCount = Math.min(SESSION_SYNC_CONCURRENCY, originOrder.length);
  await Promise.all(
    Array.from({ length: workerCount }, async () => {
      while (!sessionSyncForceStopped) {
        const groupIndex = nextGroupIndex++;
        if (groupIndex >= originOrder.length) return;
        // 同一 origin 组内按顺序处理，保证桥接标签不被并发复用。
        for (const site of groupsByOrigin.get(originOrder[groupIndex])!) {
          if (sessionSyncForceStopped) return;
          const sessions = sessionsBySite.get(site.id) ?? [];
          if (sessions.length === 0) {
            skippedSites += 1;
            appendSyncLog({
              stage: `session-site-${site.id}`,
              status: "info",
              message: `${site.name}：未检测到 Chrome 账号会话，已跳过`,
            });
            continue;
          }
          appendSyncLog({
            stage: `session-site-${site.id}`,
            status: "running",
            message: `${site.name}：开始同步 ${sessions.length} 个账号`,
          });
          try {
            const summary = await syncSiteAccountBundles(site, sessions, {
              log: (entry) =>
                appendSyncLog({
                  ...entry,
                  stage: `session-${site.id}-${entry.stage}`,
                  message: `${site.name}｜${entry.message}`,
                }),
              shouldStop: () => sessionSyncForceStopped,
              allocateRunId: () => runId * 10_000 + (++runSeq),
            });
            totals.total += summary.total;
            totals.completed += summary.completed;
            totals.failed += summary.failed;
            totals.refreshed += summary.refreshed;
            totals.reused += summary.reused;
            appendSyncLog({
              stage: `session-site-${site.id}`,
              status: summary.failed > 0 ? "error" : "success",
              message: `${site.name}：${summary.completed}/${summary.total} 个账号完成${summary.failed > 0 ? `，${summary.failed} 个失败` : ""}`,
            });
          } catch (error) {
            appendSyncLog({
              stage: `session-site-${site.id}`,
              status: "error",
              message: `${site.name} 同步失败：${String(error)}`,
            });
          }
        }
      }
    }),
  );

  await loadLibrary();
  if (sessionSyncForceStopped) {
    appendSyncLog({
      stage: "session-stopped",
      status: "error",
      message: `已强制停止：${totals.completed}/${totals.total} 个账号完成，剩余站点未处理`,
    });
    showToast(`会话同步已强制停止：完成 ${totals.completed} 个账号`, true);
  } else {
    appendSyncLog({
      stage: "session-complete",
      status: totals.failed > 0 ? "error" : "success",
      message: `会话同步完成：${targets.length - skippedSites} 个站点、${totals.total} 个账号；完成 ${totals.completed} 个${totals.failed > 0 ? `，失败 ${totals.failed} 个` : ""}${totals.refreshed > 0 ? `，浏览器刷新 ${totals.refreshed} 个` : ""}${skippedSites > 0 ? `；${skippedSites} 个站点无会话已跳过` : ""}`,
    });
    showToast(
      totals.failed > 0
        ? `会话同步完成：${totals.completed} 个账号成功，${totals.failed} 个失败`
        : `会话同步完成：${totals.completed} 个账号已更新`,
      totals.failed > 0,
    );
  }
  syncRunState.value = "complete";
  stopSyncTimer();
}

/** 强制停止批量同步（会话 / 额度共用）：不再取新站点/账号，并取消全部在途 run（后端阶段边界失败）。 */
async function stopBatchSync() {
  const mode = syncDialogMode.value;
  if ((mode !== "session" && mode !== "quota") || !syncingSites.value) return;
  if (mode === "quota") {
    quotaSyncForceStopped = true;
  } else {
    sessionSyncForceStopped = true;
  }
  appendSyncLog({
    stage: `${mode}-stop`,
    status: "error",
    message: "已请求强制停止：不再开始新的站点/账号，正在取消在途请求",
  });
  await cancelAllChromeAccountSyncs();
  try {
    await runCommand("close_chrome_sync_tabs");
  } catch {
    // 标签清理失败不阻塞停止流程
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
    if (mode === "session") {
      await runSessionSyncBatch(runId);
      return;
    }
    if (mode === "quota") {
      const usageLabel = syncDialogUsage.value === "pending" ? "待定" : "在用";
      const siteIds = [...syncDialogSiteIds.value];
      appendSyncLog({
        stage: "scope",
        status: "info",
        message: `额度同步范围：当前 ${siteIds.length} 个${usageLabel}站点；按站点顺序逐个同步，站内账号按顺序处理，归类保持不变`,
      });
      if (siteIds.length === 0) {
        appendSyncLog({ stage: "quota-empty", status: "info", message: `当前没有可同步额度的${usageLabel}站点` });
        syncRunState.value = "complete";
        stopSyncTimer();
        showToast(`当前没有可同步额度的${usageLabel}站点`, true);
        return;
      }
      // 与「提取 / 同步站点会话和额度」共用同一套管线：按选中站点顺序，
      // 逐个站点执行「扫描提取会话 + syncSiteAccountBundles 逐账号同步额度（浏览器兜底）」，
      // 站点之间不并发，避免桥接标签与站点接口互相抢占。
      if (chromeSessionSyncActive.value) {
        appendSyncLog({ stage: "quota-busy", status: "error", message: "已有 Chrome 会话同步正在进行，请等待结束后重试" });
        showToast("已有 Chrome 会话同步正在进行", true);
        syncRunState.value = "error";
        stopSyncTimer();
        return;
      }
      chromeSessionSyncActive.value = true;
      try {
        const siteMap = new Map(sites.value.map((site) => [site.id, site]));
        const targets = siteIds
          .map((id) => siteMap.get(id))
          .filter((site): site is NonNullable<typeof site> => Boolean(site));
        const totals = { total: 0, completed: 0, failed: 0, refreshed: 0, reused: 0 };
        let skippedSites = 0;
        let totalWarnings = 0;
        let runSeq = 0;

        for (const [siteIndex, site] of targets.entries()) {
          if (quotaSyncForceStopped) break;
          const progressLabel = `站点 ${siteIndex + 1}/${targets.length}`;
          appendSyncLog({
            stage: `quota-site-${site.id}`,
            status: "running",
            message: `${progressLabel}｜${site.name}：正在提取会话并同步额度`,
          });
          try {
            const scan = await analyzeChromeUsage(false, site.id, runId, undefined, false, Boolean(site.isPending));
            if (quotaSyncForceStopped) break;
            if (!scan) throw new Error("会话扫描失败");
            totalWarnings += scan.warnings;
            const sessions = scan.sites.find((item) => item.siteId === site.id)?.sessions ?? [];
            if (sessions.length === 0) {
              skippedSites += 1;
              appendSyncLog({
                stage: `quota-site-${site.id}`,
                status: "info",
                message: `${progressLabel}｜${site.name}：未检测到 Chrome 账号会话，已跳过`,
              });
              continue;
            }
            const summary = await syncSiteAccountBundles(site, sessions, {
              log: (entry) =>
                appendSyncLog({
                  ...entry,
                  stage: `quota-${site.id}-${entry.stage}`,
                  message: `${site.name}｜${entry.message}`,
                }),
              shouldStop: () => quotaSyncForceStopped,
              allocateRunId: () => runId * 10_000 + (++runSeq),
            });
            totals.total += summary.total;
            totals.completed += summary.completed;
            totals.failed += summary.failed;
            totals.refreshed += summary.refreshed;
            totals.reused += summary.reused;
            appendSyncLog({
              stage: `quota-site-${site.id}`,
              status: summary.failed > 0 ? "error" : "success",
              message: `${progressLabel}｜${site.name}：${summary.completed}/${summary.total} 个账号完成${summary.failed > 0 ? `，${summary.failed} 个失败` : ""}`,
            });
          } catch (error) {
            appendSyncLog({
              stage: `quota-site-${site.id}`,
              status: "error",
              message: `${progressLabel}｜${site.name} 同步失败：${String(error)}`,
            });
          } finally {
            // 与单站点流程一致：站点收尾再清理一次桥接标签，覆盖账号循环提前退出的残留。
            await closeChromeSyncTabs("站点收尾", `site-${site.id}`, site.apiBaseUrl, (entry) =>
              appendSyncLog({
                ...entry,
                stage: `quota-${site.id}-${entry.stage}`,
                message: `${site.name}｜${entry.message}`,
              }),
            );
          }
        }

        await loadLibrary();
        if (quotaSyncForceStopped) {
          appendSyncLog({
            stage: "quota-stopped",
            status: "error",
            message: `已强制停止：${totals.completed}/${totals.total} 个账号完成，剩余站点未处理`,
          });
          showToast(`额度同步已强制停止：完成 ${totals.completed} 个账号`, true);
        } else {
          appendSyncLog({
            stage: "quota-complete",
            status: totals.failed > 0 ? "error" : "success",
            message: `额度同步完成：${targets.length - skippedSites} 个站点、${totals.total} 个账号；完成 ${totals.completed}${totals.failed > 0 ? `，失败 ${totals.failed} 个` : ""}${totals.refreshed > 0 ? `，浏览器刷新 ${totals.refreshed} 个` : ""}${totalWarnings > 0 ? `；${totalWarnings} 个警告` : ""}`,
          });
          showToast(
            totals.failed > 0
              ? `额度同步完成：${totals.completed} 个账号成功，${totals.failed} 个失败`
              : `已同步 ${targets.length - skippedSites} 个${usageLabel}站点额度：${totals.total} 个账号`,
            totals.failed > 0,
          );
        }
        syncRunState.value = "complete";
        stopSyncTimer();
        return;
      } finally {
        chromeSessionSyncActive.value = false;
        quotaSyncForceStopped = false;
      }
    }
    const result = await runCommand<SyncSitesResult>("sync_remote_sites", { runId });
    await loadLibrary();
    const account = result.userName ? `账号 ${result.userName}` : `Chrome ${result.profileName}`;
    showToast(`${account} 已同步 ${result.total} 个公共站点（新增 ${result.added}，更新 ${result.updated}）`);
    syncRunState.value = "detecting";
    appendSyncLog({ stage: "available", status: "success", message: `站点数据已可用，共 ${result.total} 条（存活与跑路全量同步）；仅新增 ${result.added} 个站点进入类型检测` });
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
    stopBatchSync,
    detectSyncedSiteTypes,
    syncAllModelKeys,
  };
}
