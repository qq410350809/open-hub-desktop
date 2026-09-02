import { listen, type UnlistenFn } from "../core/events";
import { computed, ref, shallowRef } from "vue";
import { runCommand, isTauri } from "../core/ipc";
import type {
  CharityFeedItem,
  CharityFeedResult,
  CharityFeedTag,
  CharitySyncLogEntry,
  CharitySyncProgress,
} from "../../types";

const DEFAULT_PAGE_SIZE = 20;
const MAX_SYNC_LOG = 120;

export type CharityPropertyFilter = "all" | "hot" | "pinned" | "today";

const charityTags = ref<CharityFeedTag[]>([
  { id: "all", name: "全部" },
]);

const charitySourcesLoading = ref(false);

async function loadCharitySources() {
  try {
    const sources = await runCommand<CharityFeedTag[]>("list_charity_sources");
    if (Array.isArray(sources)) {
      charityTags.value = [
        { id: "all", name: "全部" },
        ...sources,
      ];
    }
  } catch {
    /* 加载失败保持现有标签不变 */
  }
}

async function addCharitySource(id: string, name: string) {
  charitySourcesLoading.value = true;
  try {
    await runCommand("add_charity_source", { id, name });
    await loadCharitySources();
  } finally {
    charitySourcesLoading.value = false;
  }
}

async function updateCharitySource(id: string, opts: { name?: string; enabled?: boolean; upstreamProtocol?: string }) {
  charitySourcesLoading.value = true;
  try {
    const params: Record<string, any> = { id };
    if (opts.name !== undefined) params.name = opts.name;
    if (opts.enabled !== undefined) params.enabled = opts.enabled;
    if (opts.upstreamProtocol !== undefined) params.upstream_protocol = opts.upstreamProtocol;
    await runCommand("update_charity_source", params);
    await loadCharitySources();
  } finally {
    charitySourcesLoading.value = false;
  }
}

async function removeCharitySource(id: string) {
  charitySourcesLoading.value = true;
  try {
    await runCommand("remove_charity_source", { id });
    if (selectedTagId.value === id) {
      selectedTagId.value = "all";
      currentFeedName.value = "全部";
      currentPage.value = 1;
    }
    await loadCharitySources();
    await queryLocalFeed(selectedTagId.value, 1);
  } finally {
    charitySourcesLoading.value = false;
  }
}

const selectedTagId = ref("all");
const currentFeedName = ref("全部");
const items = shallowRef<CharityFeedItem[]>([]);
const loading = ref(false);
const loadingMore = ref(false);
const error = ref("");
const statusMessage = ref("");
const lastFetchedAt = ref("");
const todayCount = ref(0);
const updatedCount = ref(0);
const usedNodeName = ref("");
const initialized = ref(false);
const totalCount = ref(0);
const hasMore = ref(false);
const nextOffset = ref(0);
const currentPage = ref(1);
const pageSize = ref(DEFAULT_PAGE_SIZE);
const totalPages = computed(() => Math.max(1, Math.ceil(totalCount.value / pageSize.value)));
const syncLog = shallowRef<CharitySyncLogEntry[]>([]);
const syncLogOpen = ref(false);
const refreshAllBusy = ref(false);
const syncLogLoading = ref(false);
const syncing = ref(false);
const searchKeyword = ref("");
const propertyFilter = ref<CharityPropertyFilter>("all");
const topicSorting = ref<{ id: string; desc: boolean }[]>([{ id: "publishedAt", desc: true }]);
const proxyPoolSummary = ref<{ validCount: number; candidateCount: number } | null>(null);

let eventUnlisten: UnlistenFn | undefined;
let menuRefreshUnlisten: UnlistenFn | undefined;
let started = false;
let loadSeq = 0;
let reloadTimer: number | null = null;
let manualSyncing = false;
let syncLogReloadTimer: number | null = null;
let searchDebounceTimer: number | null = null;

function scheduleSyncLogReload() {
  if (!syncLogOpen.value) return;
  if (syncLogReloadTimer != null) return;
  syncLogReloadTimer = window.setTimeout(() => {
    syncLogReloadTimer = null;
    void loadCharitySyncLogs();
  }, 400);
}

function tagMeta(feedId: string) {
  if (feedId === "all") return { id: "all", name: "全部" };
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

  if (result.status === "error" || result.skipped || result.status === "skipped") {
    error.value = result.message || "";
  } else {
    error.value = "";
  }
  statusMessage.value = "";
}

async function refreshSidebarCounts() {
  try {
    todayCount.value = await runCommand<number>("get_charity_today_count");
  } catch {
    /* ignore */
  }
}

async function refreshTodayCount() {
  try {
    todayCount.value = await runCommand<number>("get_charity_today_count");
  } catch {
    /* ignore */
  }
}

async function refreshProxyPoolSummary() {
  try {
    proxyPoolSummary.value = await runCommand<{
      validCount: number;
      candidateCount: number;
    }>("get_charity_proxy_pool_summary");
  } catch {
    /* ignore */
  }
}

async function loadCharitySyncLogs() {
  void refreshProxyPoolSummary();
  syncLogLoading.value = true;
  try {
    const rows = await runCommand<CharitySyncLogEntry[]>("get_charity_sync_logs", {
      limit: MAX_SYNC_LOG,
    });
    syncLog.value = Array.isArray(rows) ? rows : [];
  } catch (cause) {
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

async function queryLocalFeed(feedId = selectedTagId.value, page = currentPage.value) {
  const seq = ++loadSeq;
  loading.value = true;
  try {
    const offset = Math.max(0, (page - 1) * pageSize.value);
    const sort = topicSorting.value[0] ?? { id: "publishedAt", desc: true };
    const result = await runCommand<CharityFeedResult>("get_charity_feed", {
      feedId,
      offset,
      limit: pageSize.value,
      keyword: searchKeyword.value.trim() || undefined,
      filter: propertyFilter.value === "all" ? undefined : propertyFilter.value,
      sortBy: sort.id,
      sortOrder: sort.desc ? "desc" : "asc",
    });
    if (seq !== loadSeq || feedId !== selectedTagId.value) return result;
    applyLocalPage(result, "replace");
    const pageCount = Math.max(1, Math.ceil(totalCount.value / pageSize.value));
    currentPage.value = Math.min(page, pageCount);
    void refreshSidebarCounts();
    return result;
  } catch (cause) {
    if (seq === loadSeq && feedId === selectedTagId.value) error.value = String(cause);
    throw cause;
  } finally {
    if (seq === loadSeq) loading.value = false;
  }
}

function goCharityPage(page: number) {
  const next = Math.min(Math.max(1, page), totalPages.value);
  if (next === currentPage.value || loading.value) return;
  void queryLocalFeed(selectedTagId.value, next);
}

function scheduleLocalReload(feedId = selectedTagId.value) {
  if (feedId !== selectedTagId.value) {
    void refreshSidebarCounts();
    return;
  }
  if (reloadTimer != null) window.clearTimeout(reloadTimer);
  reloadTimer = window.setTimeout(() => {
    reloadTimer = null;
    void queryLocalFeed(selectedTagId.value, currentPage.value);
  }, 300);
}

function setSearchKeyword(value: string) {
  searchKeyword.value = value;
  if (searchDebounceTimer != null) window.clearTimeout(searchDebounceTimer);
  searchDebounceTimer = window.setTimeout(() => {
    searchDebounceTimer = null;
    currentPage.value = 1;
    void queryLocalFeed(selectedTagId.value, 1);
  }, 260);
}

function setCharityPropertyFilter(filter: CharityPropertyFilter) {
  if (propertyFilter.value === filter) return;
  propertyFilter.value = filter;
  currentPage.value = 1;
  void queryLocalFeed(selectedTagId.value, 1);
}

function setTopicSorting(sorting: { id: string; desc: boolean }[]) {
  topicSorting.value = sorting.length ? [...sorting] : [{ id: "publishedAt", desc: true }];
  currentPage.value = 1;
  void queryLocalFeed(selectedTagId.value, 1);
}

async function refreshCharityFeed() {
  if (manualSyncing) return;
  manualSyncing = true;
  refreshAllBusy.value = true;
  syncing.value = true;
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
    await queryLocalFeed(selectedTagId.value, currentPage.value);
    if (syncLogOpen.value) void loadCharitySyncLogs();
  }
}

async function selectTag(tagId: string) {
  if (tagId === selectedTagId.value) return;
  if (searchKeyword.value.trim()) {
    searchKeyword.value = "";
  }
  selectedTagId.value = tagId;
  currentFeedName.value = tagMeta(tagId).name;
  currentPage.value = 1;
  statusMessage.value = "";
  error.value = "";
  await queryLocalFeed(tagId, 1);
}

async function ensureEventBridge() {
  if (!isTauri || eventUnlisten) return;
  try {
    menuRefreshUnlisten = await listen("menu-refresh-requested", () => {
      void refreshCharityFeed();
    });
  } catch {
    menuRefreshUnlisten = undefined;
  }
  try {
    eventUnlisten = await listen<CharitySyncProgress>("charity-sync-progress", ({ payload }) => {
      syncing.value = payload.status === "running";
      if (payload.status === "running" && syncLogOpen.value) {
        const node = payload.usedNodeName || "";
        const rows = syncLog.value.slice();
        let hit = false;
        for (let i = 0; i < rows.length; i++) {
          const row = rows[i];
          if (row.status === "running" && row.feedId === payload.feedId) {
            rows[i] = {
              ...row,
              message: payload.message || row.message,
              nodeName: node || row.nodeName,
              stage: payload.stage || row.stage,
            };
            hit = true;
            break;
          }
        }
        if (hit) syncLog.value = rows;
        scheduleSyncLogReload();
      } else if (syncLogOpen.value) {
        scheduleSyncLogReload();
      }
      if (payload.status !== "running") {
        scheduleLocalReload(payload.feedId);
        void refreshSidebarCounts();
      }
    });
  } catch {
    /* ignore */
  }
}

function requestCharityRound() {
  void runCommand("request_charity_round").catch(() => undefined);
}

function onVisibilityChange() {
  const visible = document.visibilityState === "visible";
  void runCommand("set_charity_monitor_visible", { visible }).catch(() => undefined);
  if (visible) {
    scheduleLocalReload(selectedTagId.value);
    void refreshSidebarCounts();
    if (syncLogOpen.value) void loadCharitySyncLogs();
  }
}

async function startCharityMonitor() {
  if (started) return;
  started = true;
  await loadCharitySources();
  await ensureEventBridge();
  document.addEventListener("visibilitychange", onVisibilityChange);
  const visible = document.visibilityState === "visible";
  void runCommand("set_charity_monitor_visible", { visible }).catch(() => undefined);
  void queryLocalFeed(selectedTagId.value, 1);
  void refreshSidebarCounts();
}

function stopCharityMonitor() {
  document.removeEventListener("visibilitychange", onVisibilityChange);
  if (reloadTimer != null) {
    window.clearTimeout(reloadTimer);
    reloadTimer = null;
  }
  eventUnlisten?.();
  eventUnlisten = undefined;
  menuRefreshUnlisten?.();
  menuRefreshUnlisten = undefined;
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

const displayedCount = computed(() => items.value.length);

function setCharityPageSize(size: number) {
  if (!Number.isFinite(size) || size <= 0 || size === pageSize.value) return;
  pageSize.value = size;
  currentPage.value = 1;
  void queryLocalFeed(selectedTagId.value, 1);
}

export function useCharityMonitor() {
  return {
    charityTags,
    charitySourcesLoading,
    selectedTagId,
    searchKeyword,
    charityPropertyFilter: propertyFilter,
    setCharityPropertyFilter,
    topicSorting,
    setTopicSorting,
    currentFeedName,
    charityFeedItems: items,
    charityFeedLoading: loading,
    charityFeedLoadingMore: loadingMore,
    charityFeedSyncing: syncing,
    charityFeedRefreshAllBusy: refreshAllBusy,
    charityFeedError: error,
    charityFeedStatusMessage: statusMessage,
    charityFeedLastFetchedAt: lastFetchedAt,
    charityFeedTodayCount: todayCount,
    refreshTodayCount,
    charityProxyPoolSummary: proxyPoolSummary,
    refreshCharityProxyPoolSummary: refreshProxyPoolSummary,
    charityFeedUpdatedCount: updatedCount,
    charityFeedSourceProfileName: ref(""),
    charityFeedSourceAccountName: ref(""),
    charityFeedUsedNodeName: usedNodeName,
    charityFeedInitialized: initialized,
    charityFeedTotalCount: totalCount,
    charityFeedDisplayedCount: displayedCount,
    charityFeedHasMore: hasMore,
    charityFeedCurrentPage: currentPage,
    charityFeedPageSize: pageSize,
    charityFeedTotalPages: totalPages,
    charitySyncLog: syncLog,
    charitySyncLogOpen: syncLogOpen,
    charitySyncLogLoading: syncLogLoading,
    goCharityPage,
    setCharityPageSize,
    loadCharityFeedLocal: queryLocalFeed,
    refreshCharityFeed,
    selectTag,
    setSearchKeyword,
    startCharityMonitor,
    stopCharityMonitor,
    loadCharitySyncLogs,
    clearCharitySyncLog,
    toggleCharitySyncLog,
    closeCharitySyncLog,
    requestCharityRound,
    loadCharitySources,
    addCharitySource,
    updateCharitySource,
    removeCharitySource,
  };
}
