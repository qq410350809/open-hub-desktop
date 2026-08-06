import { ref } from "vue";
import { runCommand } from "./useLibrary";
import type { CharityFeedItem, CharityFeedResult, CharityFeedTag } from "../types";

const charityTags = ref<CharityFeedTag[]>([
  { id: "1515", name: "公益推广" },
  { id: "1980", name: "公益站" },
  { id: "2233", name: "中转站" },
  { id: "2234", name: "开源推广" },
  { id: "1514", name: "高级推广" },
  { id: "193", name: "订阅节点" },
]);
const selectedTagId = ref<string>("1515");
const currentFeedName = ref("公益推广");
const items = ref<CharityFeedItem[]>([]);
const loading = ref(false);
const error = ref("");
const lastFetchedAt = ref("");
const unreadCount = ref(0);
const updatedCount = ref(0);
const sourceProfileName = ref("");
const sourceAccountName = ref("");
const initialized = ref(false);
let pollingTimer: number | null = null;

function tagMeta(feedId: string) {
  return charityTags.value.find((tag: CharityFeedTag) => tag.id === feedId) ?? { id: feedId, name: feedId };
}

async function refreshCharityFeed(silent = false) {
  if (loading.value) return;
  loading.value = true;
  if (!silent) error.value = "";
  try {
    const result = await runCommand<CharityFeedResult>("fetch_charity_feed", {
      feedId: selectedTagId.value,
    });
    items.value = result.items;
    lastFetchedAt.value = result.fetchedAt;
    updatedCount.value = result.updatedCount;
    currentFeedName.value = result.feedName || tagMeta(selectedTagId.value).name;
    sourceProfileName.value = result.sourceProfileName;
    sourceAccountName.value = result.sourceAccountName;
    initialized.value = result.initialized;
    if (result.initialized && result.newCount > 0) {
      unreadCount.value += result.newCount;
    }
    error.value = "";
  } catch (cause) {
    error.value = String(cause);
  } finally {
    loading.value = false;
  }
}

async function selectTag(tagId: string) {
  if (tagId === selectedTagId.value) return;
  selectedTagId.value = tagId;
  unreadCount.value = 0;
  await refreshCharityFeed();
}

function startCharityMonitor() {
  if (pollingTimer !== null) return;
  void refreshCharityFeed(true);
  pollingTimer = window.setInterval(() => {
    if (document.visibilityState === "visible") void refreshCharityFeed(true);
  }, 5 * 60 * 1000);
}

function stopCharityMonitor() {
  if (pollingTimer !== null) {
    window.clearInterval(pollingTimer);
    pollingTimer = null;
  }
}

function markCharityFeedRead() {
  unreadCount.value = 0;
}

export function useCharityMonitor() {
  return {
    charityTags,
    selectedTagId,
    currentFeedName,
    charityFeedItems: items,
    charityFeedLoading: loading,
    charityFeedError: error,
    charityFeedLastFetchedAt: lastFetchedAt,
    charityFeedUnreadCount: unreadCount,
    charityFeedUpdatedCount: updatedCount,
    charityFeedSourceProfileName: sourceProfileName,
    charityFeedSourceAccountName: sourceAccountName,
    charityFeedInitialized: initialized,
    refreshCharityFeed,
    selectTag,
    startCharityMonitor,
    stopCharityMonitor,
    markCharityFeedRead,
  };
}
