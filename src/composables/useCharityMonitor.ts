import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { computed, ref, shallowRef } from "vue";
import { runCommand } from "./useLibrary";
import type {
  CharityFeedItem,
  CharityFeedResult,
  CharityFeedTag,
  CharitySyncLogEntry,
  CharitySyncProgress,
} from "../types";

const isTauri = "__TAURI_INTERNALS__" in window;
const PAGE_SIZE = 20;
const MAX_SYNC_LOG = 120;

const charityTags = ref<CharityFeedTag[]>([
  { id: "1515", name: "公益推广" },
  { id: "1980", name: "公益站" },
  { id: "2233", name: "中转站" },
  { id: "2234", name: "开源推广" },
  { id: "1514", name: "高级推广" },
  { id: "193", name: "订阅节点" },
]);

const selectedTagId = ref("1515");
const currentFeedName = ref("公益推广");
const items = shallowRef<CharityFeedItem[]>([]);
const loading = ref(false);
const loadingMore = ref(false);
const error = ref("");
const statusMessage = ref("");
const lastFetchedAt = ref("");
const unreadCount = ref(0);
const totalUnreadCount = ref(0);
const updatedCount = ref(0);
const usedNodeName = ref("");
const initialized = ref(false);
const totalCount = ref(0);
const hasMore = ref(false);
const nextOffset = ref(0);
const syncLog = shallowRef<CharitySyncLogEntry[]>([]);
const syncLogOpen = ref(false);
const refreshAllBusy = ref(false);
const syncLogLoading = ref(false);

let eventUnlisten: UnlistenFn | undefined;
let started = false;
let loadSeq = 0;
let reloadTimer: number | null = null;
let manualSyncing = false;

function tagMeta(feedId: string) {
  return charityTags.value.find((tag) => tag.id === feedId) ?? { id: feedId, name: feedId };
}

function applyLocalPage(result: CharityFeedResult, mode: "replace" | "append") {
  if (result.feedId !== selectedTagId.value) return;

  if (mode === "replace") items.value = result.items ?? [];
  else if (result.items?.length) {
    const seen = new Set(items.value.map((item) => item.id));
    items.value = items.value.concat(result.items.filter((item) => !seen.has(item.id)));
  }

  lastFetchedAt.value = result.fetchedAt || lastFetchedAt.value;
  currentFeedName.value = result.feedName || tagMeta(selectedTagId.value).name;
  initialized.value = result.initialized;
  usedNodeName.value = result.usedNodeName || "";
  updatedCount.value = result.updatedCount ?? 0;
  totalCount.value = result.totalCount ?? items.value.length;
  const offset = result.offset ?? 0;
  nextOffset.value = offset + (result.items?.length ?? 0);
  hasMore.value = Boolean(
    result.hasMore ?? (totalCount.value > 0 && nextOffset.value < totalCount.value),
  );
  if (typeof result.unreadCount === "number") unreadCount.value = result.unreadCount;

  if (result.status === "error" || result.skipped || result.status === "skipped") {
    error.value = result.message || "";
  } else {
    error.value = "";
  }
  statusMessage.value = "";
}

async function refreshUnreadTotal() {
  try {
    totalUnreadCount.value = await runCommand<number>("get_charity_unread_total");
  } catch {
    /* ignore */
  }
}

async function loadCharitySyncLogs() {
  syncLogLoading.value = true;
  try {
    const rows = await runCommand<CharitySyncLogEntry[]>("get_charity_sync_logs", {
      limit: MAX_SYNC_LOG,
    });
    syncLog.value = Array.isArray(rows) ? rows : [];
  } catch (cause) {
    // 读日志失败时保留现有内容，避免弹窗空白误导
    if (!syncLog.value.length) {
      syncLog.value = [
        {
          id: Date.now(),
          at: new Date().toISOString(),
          feedId: selectedTagId.value,
          feedName: currentFeedName.value,
          stage: "log",
          status: "error",
          message: `读取同步日志失败：${String(cause)}`,
          nodeName: "",
        },
      ];
    }
  } finally {
    syncLogLoading.value = false;
  }
}

async function queryLocalFeed(feedId = selectedTagId.value, offset = 0, append = false) {
  const seq = ++loadSeq;
  if (append) loadingMore.value = true;
  else loading.value = true;
  try {
    const result = await runCommand<CharityFeedResult>("get_charity_feed", {
      feedId,
      offset,
      limit: PAGE_SIZE,
    });
    if (seq !== loadSeq || feedId !== selectedTagId.value) return result;
    applyLocalPage(result, append ? "append" : "replace");
    void refreshUnreadTotal();
    return result;
  } catch (cause) {
    if (seq === loadSeq && feedId === selectedTagId.value) error.value = String(cause);
    throw cause;
  } finally {
    if (seq === loadSeq) {
      loading.value = false;
      loadingMore.value = false;
    }
  }
}

function scheduleLocalReload(feedId = selectedTagId.value) {
  if (feedId !== selectedTagId.value) {
    void refreshUnreadTotal();
    return;
  }
  if (reloadTimer != null) window.clearTimeout(reloadTimer);
  reloadTimer = window.setTimeout(() => {
    reloadTimer = null;
    void queryLocalFeed(selectedTagId.value, 0, false);
  }, 300);
}

async function loadMoreCharityFeed() {
  if (!hasMore.value || loading.value || loadingMore.value) return;
  await queryLocalFeed(selectedTagId.value, nextOffset.value, true);
}

/**
 * “立即刷新”刷新全部标签：后端先取消当前轮及所有残留 running 历史任务，
 * 然后由独立调度器马上启动六个标签的新一轮；UI 始终只读本地库。
 */
async function refreshCharityFeed() {
  if (manualSyncing) return;
  manualSyncing = true;
  refreshAllBusy.value = true;
  try {
    const result = await runCommand<{
      cancelledActiveRound: boolean;
      cancelledLogCount: number;
      feedCount: number;
    }>("refresh_all_charity_feeds");
    const cancelled = Math.max(
      result.cancelledLogCount || 0,
      result.cancelledActiveRound ? 1 : 0,
    );
    statusMessage.value = cancelled > 0
      ? `已取消 ${cancelled} 个未完成任务，正在刷新全部 ${result.feedCount} 个标签`
      : `正在刷新全部 ${result.feedCount} 个标签`;
  } catch (cause) {
    error.value = `提交全部标签刷新失败：${String(cause)}`;
  } finally {
    manualSyncing = false;
    refreshAllBusy.value = false;
    await queryLocalFeed(selectedTagId.value, 0, false);
    if (syncLogOpen.value) void loadCharitySyncLogs();
  }
}

async function selectTag(tagId: string) {
  if (tagId === selectedTagId.value) return;
  selectedTagId.value = tagId;
  currentFeedName.value = tagMeta(tagId).name;
  items.value = [];
  totalCount.value = 0;
  hasMore.value = false;
  nextOffset.value = 0;
  unreadCount.value = 0;
  updatedCount.value = 0;
  statusMessage.value = "";
  error.value = "";
  usedNodeName.value = "";
  loadingMore.value = false;
  await queryLocalFeed(tagId, 0, false);
  void markCharityFeedRead();
}

async function markCharityFeedRead() {
  try {
    unreadCount.value = await runCommand<number>("mark_charity_feed_read", {
      feedId: selectedTagId.value,
    });
    void refreshUnreadTotal();
    if (items.value.some((item) => item.isNew)) {
      items.value = items.value.map((item) => (item.isNew ? { ...item, isNew: false } : item));
    }
  } catch {
    unreadCount.value = 0;
  }
}

async function ensureEventBridge() {
  if (!isTauri || eventUnlisten) return;
  try {
    eventUnlisten = await listen<CharitySyncProgress>("charity-sync-progress", ({ payload }) => {
      // 同步过程只写后端日志；弹窗打开时刷新日志，主界面不展示“同步中”
      if (syncLogOpen.value) {
        void loadCharitySyncLogs();
      }
      if (payload.status !== "running") {
        scheduleLocalReload(payload.feedId);
        void refreshUnreadTotal();
      }
    });
  } catch {
    /* ignore */
  }
}

function requestCharityRound() {
  // 仅显式临时触发（立即刷新按钮 / 调试），不在打开应用或回前台时自动跑。
  void runCommand("request_charity_round").catch(() => undefined);
}

function onVisibilityChange() {
  const visible = document.visibilityState === "visible";
  void runCommand("set_charity_monitor_visible", { visible }).catch(() => undefined);
  if (visible) {
    // 回前台只刷新本地展示，不触发同步；定时由后端每 5 分钟对齐点负责。
    scheduleLocalReload(selectedTagId.value);
    void refreshUnreadTotal();
    if (syncLogOpen.value) void loadCharitySyncLogs();
  }
}

async function startCharityMonitor() {
  if (started) return;
  started = true;
  await ensureEventBridge();
  document.addEventListener("visibilitychange", onVisibilityChange);
  const visible = document.visibilityState === "visible";
  void runCommand("set_charity_monitor_visible", { visible }).catch(() => undefined);
  void queryLocalFeed(selectedTagId.value, 0, false);
  void refreshUnreadTotal();
  // 不在启动时 request_charity_round：定时 = 每 5 分钟整点秒；临时 = 按钮。
}

function stopCharityMonitor() {
  document.removeEventListener("visibilitychange", onVisibilityChange);
  if (reloadTimer != null) {
    window.clearTimeout(reloadTimer);
    reloadTimer = null;
  }
  eventUnlisten?.();
  eventUnlisten = undefined;
  started = false;
  void runCommand("set_charity_monitor_visible", { visible: false }).catch(() => undefined);
}

async function clearCharitySyncLog() {
  try {
    await runCommand("clear_charity_sync_logs");
    syncLog.value = [];
  } catch (cause) {
    syncLog.value = [
      {
        id: Date.now(),
        at: new Date().toISOString(),
        feedId: selectedTagId.value,
        feedName: currentFeedName.value,
        stage: "log",
        status: "error",
        message: `清空同步日志失败：${String(cause)}`,
        nodeName: "",
      },
    ];
  }
}

async function toggleCharitySyncLog(open?: boolean) {
  const next = typeof open === "boolean" ? open : !syncLogOpen.value;
  syncLogOpen.value = next;
  if (next) {
    document.body.classList.add("modal-open");
    await loadCharitySyncLogs();
  } else {
    document.body.classList.remove("modal-open");
  }
}

function closeCharitySyncLog() {
  void toggleCharitySyncLog(false);
}

const sidebarUnread = computed(() => totalUnreadCount.value || unreadCount.value);
const displayedCount = computed(() => items.value.length);

export function useCharityMonitor() {
  return {
    charityTags,
    selectedTagId,
    currentFeedName,
    charityFeedItems: items,
    charityFeedLoading: loading,
    charityFeedLoadingMore: loadingMore,
    charityFeedSyncing: computed(() => false),
    charityFeedRefreshAllBusy: refreshAllBusy,
    charityFeedError: error,
    charityFeedStatusMessage: statusMessage,
    charityFeedLastFetchedAt: lastFetchedAt,
    charityFeedUnreadCount: sidebarUnread,
    charityFeedSelectedUnreadCount: unreadCount,
    charityFeedUpdatedCount: updatedCount,
    charityFeedSourceProfileName: ref(""),
    charityFeedSourceAccountName: ref(""),
    charityFeedUsedNodeName: usedNodeName,
    charityFeedInitialized: initialized,
    charityFeedTotalCount: totalCount,
    charityFeedDisplayedCount: displayedCount,
    charityFeedHasMore: hasMore,
    charitySyncLog: syncLog,
    charitySyncLogOpen: syncLogOpen,
    charitySyncLogLoading: syncLogLoading,
    loadCharityFeedLocal: queryLocalFeed,
    loadMoreCharityFeed,
    refreshCharityFeed,
    selectTag,
    startCharityMonitor,
    stopCharityMonitor,
    markCharityFeedRead,
    loadCharitySyncLogs,
    clearCharitySyncLog,
    toggleCharitySyncLog,
    closeCharitySyncLog,
    requestCharityRound,
  };
}
