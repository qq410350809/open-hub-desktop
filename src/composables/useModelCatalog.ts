import { ref } from "vue";
import { listen } from "@tauri-apps/api/event";
import { isTauri, runCommand } from "./useLibrary";
import { useToast } from "./useToast";
import type {
  ModelCatalogDetail,
  ModelCatalogSnapshot,
  ModelCatalogSyncResult,
} from "../types";

const emptySnapshot = (): ModelCatalogSnapshot => ({
  models: [],
  total: 0,
  lastSyncedAt: "",
  syncedToday: false,
  sources: [],
});

const modelCatalog = ref<ModelCatalogSnapshot>(emptySnapshot());
const modelCatalogLoading = ref(false);
const modelCatalogSyncing = ref(false);
const modelCatalogError = ref("");
let eventInitialized = false;
let unlistenSyncStatus: (() => void) | undefined;
let dailySyncTimer: number | null = null;
const { showToast } = useToast();

async function loadModelCatalog() {
  modelCatalogLoading.value = true;
  modelCatalogError.value = "";
  try {
    modelCatalog.value = await runCommand<ModelCatalogSnapshot>("get_model_catalog");
  } catch (error) {
    modelCatalogError.value = String(error);
  } finally {
    modelCatalogLoading.value = false;
  }
}

async function syncModelCatalog(force = true) {
  if (modelCatalogSyncing.value) return null;
  modelCatalogSyncing.value = true;
  modelCatalogError.value = "";
  try {
    const result = await runCommand<ModelCatalogSyncResult>("sync_model_catalog", { force });
    modelCatalog.value = result.snapshot;
    if (force || result.synced) showToast(result.message);
    return result;
  } catch (error) {
    modelCatalogError.value = String(error);
    showToast(`模型参数同步失败：${String(error)}`, true);
    return null;
  } finally {
    modelCatalogSyncing.value = false;
  }
}

async function getModelCatalogDetail(canonicalKey: string) {
  return runCommand<ModelCatalogDetail>("get_model_catalog_detail", { canonicalKey });
}

function scheduleDailyModelCatalogSync() {
  if (dailySyncTimer !== null) window.clearTimeout(dailySyncTimer);
  const now = new Date();
  const nextRun = new Date(now);
  nextRun.setHours(24, 0, 5, 0);
  dailySyncTimer = window.setTimeout(async () => {
    await syncModelCatalog(false);
    scheduleDailyModelCatalogSync();
  }, Math.max(1_000, nextRun.getTime() - now.getTime()));
}

async function initializeModelCatalog() {
  if (isTauri && !eventInitialized) {
    eventInitialized = true;
    unlistenSyncStatus = await listen<{ status?: string; message?: string }>(
      "model-catalog-sync-status",
      async (event) => {
        const status = event.payload?.status;
        modelCatalogSyncing.value = status === "syncing";
        if (status === "complete") {
          await loadModelCatalog();
        } else if (status === "error") {
          modelCatalogError.value = event.payload?.message || "模型参数自动同步失败";
        }
      },
    );
  }
  await loadModelCatalog();
  if (!modelCatalog.value.syncedToday) {
    await syncModelCatalog(false);
  }
  scheduleDailyModelCatalogSync();
}

function stopModelCatalogEvents() {
  unlistenSyncStatus?.();
  unlistenSyncStatus = undefined;
  eventInitialized = false;
  if (dailySyncTimer !== null) {
    window.clearTimeout(dailySyncTimer);
    dailySyncTimer = null;
  }
}

export function useModelCatalog() {
  return {
    modelCatalog,
    modelCatalogLoading,
    modelCatalogSyncing,
    modelCatalogError,
    loadModelCatalog,
    syncModelCatalog,
    getModelCatalogDetail,
    initializeModelCatalog,
    stopModelCatalogEvents,
  };
}
