import { ref } from "vue";
import { emptySite, type ChromeUsageSite, type LibraryData, type SiteRecord } from "../../types";
import { runCommand } from "../core/ipc";
import { createScopedLoader } from "./siteRequestState";

// —— 全局单例状态 ——
const sites = ref<SiteRecord[]>([]);
const suggestedTags = ref<string[]>([]);
const usageSites = ref<ChromeUsageSite[]>([]);
const loading = ref(false);
let dailyRefreshTimer: number | null = null;
let dailyRefreshEnabled = false;
const libraryLoader = createScopedLoader();

export async function loadLibrary() {
  await libraryLoader.load(
    () => runCommand<LibraryData>("list_library"),
    () => true,
    (data) => {
      sites.value = data.sites.map((site) => ({ ...emptySite(), ...site }));
      suggestedTags.value = data.suggestedTags;
      usageSites.value = data.usageSites ?? [];
    },
    (pending) => { loading.value = pending; },
  );
}

function scheduleDailyRefresh() {
  if (!dailyRefreshEnabled) return;
  if (dailyRefreshTimer !== null) {
    window.clearTimeout(dailyRefreshTimer);
  }

  const now = new Date();
  const nextMidnight = new Date(now);
  nextMidnight.setHours(24, 0, 0, 0);
  // 留出 1 秒，确保本地日期已经切换。只重新读取 SQLite，
  // 不会触发任何网络同步，因此不会打断正在进行的任务。
  const delay = Math.max(1000, nextMidnight.getTime() - now.getTime() + 1000);
  dailyRefreshTimer = window.setTimeout(async () => {
    try {
      await loadLibrary();
    } catch (error) {
      console.warn("每日站点缓存刷新失败:", error);
    } finally {
      scheduleDailyRefresh();
    }
  }, delay);
}

export function startDailyRefresh() {
  dailyRefreshEnabled = true;
  scheduleDailyRefresh();
}

export function stopDailyRefresh() {
  dailyRefreshEnabled = false;
  if (dailyRefreshTimer !== null) {
    window.clearTimeout(dailyRefreshTimer);
    dailyRefreshTimer = null;
  }
}

export function useLibrary() {
  return {
    sites,
    suggestedTags,
    usageSites,
    loading,
    loadLibrary,
    startDailyRefresh,
    stopDailyRefresh,
  };
}
