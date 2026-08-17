import { ref } from "vue";
import { listen } from "@tauri-apps/api/event";
import { runCommand, useLibrary } from "./useLibrary";
import { useToast } from "./useToast";
import type {
  AutoSyncProgress,
  AutoSyncRoundSummary,
  AutoSyncSettings,
  AutoSyncStatus,
} from "../types";

const { loadLibrary } = useLibrary();
const { showToast } = useToast();

// 自动会话同步的设置与最近一轮结果（后端调度器写入 app_meta）。
const autoSyncSettings = ref<AutoSyncSettings | null>(null);
const autoSyncStatus = ref<AutoSyncStatus | null>(null);
const autoSyncRoundRunning = ref(false);
// 最近一次自动轮次的进度日志（弹窗/设置页展示用，保留最近 50 条）。
const autoSyncLogs = ref<Array<AutoSyncProgress & { id: number }>>([]);
let autoSyncLogId = 0;
let unlistenRound: (() => void) | undefined;
let unlistenProgress: (() => void) | undefined;
let listenersBound = false;

function appendAutoSyncLog(progress: AutoSyncProgress) {
  autoSyncLogs.value.push({ ...progress, id: ++autoSyncLogId });
  if (autoSyncLogs.value.length > 50) {
    autoSyncLogs.value.splice(0, autoSyncLogs.value.length - 50);
  }
}

async function loadAutoSyncState() {
  try {
    const [settings, status] = await Promise.all([
      runCommand<AutoSyncSettings>("get_auto_sync_settings"),
      runCommand<AutoSyncStatus>("get_auto_sync_status"),
    ]);
    autoSyncSettings.value = settings;
    autoSyncStatus.value = status;
  } catch {
    // 轻量模式未就绪或数据库锁定时静默失败，下次打开设置页再读。
  }
}

async function updateAutoSyncSettings(patch: Partial<AutoSyncSettings>) {
  try {
    autoSyncSettings.value = await runCommand<AutoSyncSettings>("set_auto_sync_settings", {
      enabled: patch.enabled,
      intervalMinutes: patch.intervalMinutes,
    });
    showToast(
      patch.enabled === false
        ? "已关闭自动会话同步"
        : patch.enabled === true
          ? "已开启自动会话同步"
          : `自动同步间隔已调整为 ${autoSyncSettings.value.intervalMinutes} 分钟`,
    );
    await loadAutoSyncState();
  } catch (error) {
    showToast(`保存自动同步设置失败：${String(error)}`, true);
  }
}

async function requestAutoSyncRound() {
  if (autoSyncRoundRunning.value) return;
  autoSyncRoundRunning.value = true;
  try {
    await runCommand("request_auto_sync_round");
    showToast("已请求立即执行一轮自动同步（后台进行，不打扰浏览器）");
  } catch (error) {
    showToast(`请求自动同步失败：${String(error)}`, true);
  } finally {
    autoSyncRoundRunning.value = false;
  }
}

async function onAutoSyncRound(summary: AutoSyncRoundSummary) {
  autoSyncStatus.value = {
    ...(autoSyncStatus.value ?? {
      enabled: true,
      intervalMinutes: 30,
      lastRoundAt: 0,
      lastSummary: null,
    }),
    lastRoundAt: summary.finishedAt,
    lastSummary: summary,
  };
  // 自动恢复成功 / 需要人工过盾的账号以 toast 通知；普通保活不打扰。
  if (summary.recovered.length > 0) {
    const names = summary.recovered.map((item) => `${item.siteName}（${item.accountLabel}）`).join("、");
    showToast(`自动同步已恢复 ${summary.recovered.length} 个失效账号：${names}`);
    await loadLibrary();
  }
  if (summary.pendingManual.length > 0) {
    const first = summary.pendingManual[0];
    showToast(
      `${first.siteName} 的会话需要人工完成 Cloudflare 验证：请在站点卡片打开「Chrome 会话」手动同步一次`,
      true,
    );
  }
}

const isTauri = "__TAURI_INTERNALS__" in window;

async function bindAutoSyncListeners() {
  if (!isTauri || listenersBound) return;
  listenersBound = true;
  unlistenRound = await listen<AutoSyncRoundSummary>("auto-sync-round", (event) => {
    void onAutoSyncRound(event.payload);
  });
  unlistenProgress = await listen<AutoSyncProgress>("auto-sync-progress", (event) => {
    appendAutoSyncLog(event.payload);
  });
}

function unbindAutoSyncListeners() {
  listenersBound = false;
  unlistenRound?.();
  unlistenProgress?.();
  unlistenRound = undefined;
  unlistenProgress = undefined;
}

async function initializeAutoSync() {
  await loadAutoSyncState();
  await bindAutoSyncListeners();
}

export function useAutoSync() {
  return {
    autoSyncSettings,
    autoSyncStatus,
    autoSyncRoundRunning,
    autoSyncLogs,
    initializeAutoSync,
    unbindAutoSyncListeners,
    loadAutoSyncState,
    updateAutoSyncSettings,
    requestAutoSyncRound,
  };
}
