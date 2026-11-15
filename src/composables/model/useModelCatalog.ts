import { ref } from "vue";
import { listen } from "../core/events";
import { runCommand } from "../core/ipc";
import { useToast } from "../core/useToast";
import type { ModelCatalogDetail, ModelCatalogSnapshot, ModelCatalogSyncResult } from "../../types";
const emptySnapshot = (): ModelCatalogSnapshot => ({
    models: [], providers: [], total: 0, lastSyncedAt: "", syncedToday: false, sources: [], meta: {},
});
const modelCatalog = ref<ModelCatalogSnapshot>(emptySnapshot());
const modelCatalogLoading = ref(false);
const modelCatalogSyncing = ref(false);
const modelCatalogError = ref("");
const modelCatalogSyncMessage = ref("");
const { showToast } = useToast();
let generation = 0;
let dataRevision = 0;
let readSequence = 0;
let remoteSyncing = false;
let pendingRead: {
    revision: number;
    promise: Promise<void>;
} | null = null;
let pendingSync: Promise<ModelCatalogSyncResult | null> | null = null;
let initialization: Promise<void> | null = null;
let unlistenSyncStatus: (() => void) | undefined;
let dailySyncTimer: number | null = null;
function updateSyncState() { modelCatalogSyncing.value = remoteSyncing || pendingSync !== null; }
/** Coalesce identical reads. A read started before a sync may never replace its newer snapshot. */
function loadModelCatalog(): Promise<void> {
    if (pendingRead?.revision === dataRevision)
        return pendingRead.promise;
    const revision = dataRevision;
    const request = ++readSequence;
    const lifecycle = generation;
    modelCatalogLoading.value = true;
    modelCatalogError.value = "";
    const task = Promise.resolve().then(async () => {
        try {
            const snapshot = await runCommand<ModelCatalogSnapshot>("get_model_catalog");
            if (lifecycle === generation && request === readSequence && revision === dataRevision)
                modelCatalog.value = snapshot;
        }
        catch (error) {
            if (lifecycle === generation && request === readSequence && revision === dataRevision)
                modelCatalogError.value = String(error);
        }
        finally {
            if (lifecycle === generation && request === readSequence)
                modelCatalogLoading.value = false;
            if (pendingRead?.promise === task)
                pendingRead = null;
        }
    });
    pendingRead = { revision, promise: task };
    return task;
}
function syncModelCatalog(force = true): Promise<ModelCatalogSyncResult | null> {
    if (pendingSync)
        return pendingSync;
    if (remoteSyncing)
        return Promise.resolve(null);
    const revision = ++dataRevision;
    const lifecycle = generation;
    modelCatalogError.value = "";
    modelCatalogSyncMessage.value = "正在拉取模型参数与渠道明细，已有目录仍可浏览。";
    const task = Promise.resolve().then(async () => {
        try {
            const result = await runCommand<ModelCatalogSyncResult>("sync_model_catalog", { force });
            if (lifecycle === generation && revision === dataRevision) {
                // Invalidate any cache read launched while this synchronization was running.
                dataRevision++;
                modelCatalog.value = result.snapshot;
                modelCatalogSyncMessage.value = result.message;
                if (force || result.synced)
                    showToast(result.message);
            }
            return result;
        }
        catch (error) {
            if (lifecycle === generation) {
                modelCatalogError.value = String(error);
                showToast(`模型参数同步失败：${String(error)}`, true);
            }
            return null;
        }
        finally {
            if (pendingSync === task) {
                pendingSync = null;
                remoteSyncing = false;
                updateSyncState();
            }
        }
    });
    pendingSync = task;
    updateSyncState();
    return task;
}
function getModelCatalogDetail(id: string) {
    return runCommand<ModelCatalogDetail>("get_model_catalog_detail", { id, canonicalKey: id });
}
function scheduleDailyModelCatalogSync(lifecycle: number) {
    if (lifecycle !== generation)
        return;
    if (dailySyncTimer !== null)
        window.clearTimeout(dailySyncTimer);
    const now = new Date();
    const nextRun = new Date(now);
    nextRun.setHours(24, 0, 5, 0);
    dailySyncTimer = window.setTimeout(async () => {
        dailySyncTimer = null;
        if (lifecycle !== generation)
            return;
        await syncModelCatalog(false);
        if (lifecycle === generation)
            scheduleDailyModelCatalogSync(lifecycle);
    }, Math.max(1000, nextRun.getTime() - now.getTime()));
}
function initializeModelCatalog(): Promise<void> {
    if (initialization)
        return initialization;
    const lifecycle = generation;
    initialization = (async () => {
        try {
            const unlisten = await listen<{
                status?: string;
                message?: string;
            }>("model-catalog-sync-status", (event) => {
                if (lifecycle !== generation)
                    return;
                const { status, message } = event.payload ?? {};
                if (message)
                    modelCatalogSyncMessage.value = message;
                if (status === "syncing") {
                    if (!pendingSync)
                        dataRevision++;
                    remoteSyncing = true;
                }
                else if (status === "complete" || status === "error") {
                    remoteSyncing = false;
                    if (status === "error")
                        modelCatalogError.value = message || "模型参数自动同步失败";
                    else if (!pendingSync) {
                        dataRevision++;
                        void loadModelCatalog();
                    }
                }
                updateSyncState();
            });
            if (lifecycle !== generation) {
                unlisten();
                return;
            }
            unlistenSyncStatus = unlisten;
        }
        catch (error) {
            console.warn("模型参数进度监听暂不可用：", error);
        }
        if (lifecycle !== generation)
            return;
        await loadModelCatalog();
        if (lifecycle !== generation)
            return;
        if (!modelCatalog.value.syncedToday)
            await syncModelCatalog(false);
        if (lifecycle === generation)
            scheduleDailyModelCatalogSync(lifecycle);
    })();
    return initialization;
}
function stopModelCatalogEvents() {
    generation++;
    dataRevision++;
    readSequence++;
    unlistenSyncStatus?.();
    unlistenSyncStatus = undefined;
    initialization = null;
    pendingRead = null;
    pendingSync = null;
    remoteSyncing = false;
    modelCatalogLoading.value = false;
    updateSyncState();
    if (dailySyncTimer !== null) {
        window.clearTimeout(dailySyncTimer);
        dailySyncTimer = null;
    }
}
export function useModelCatalog() {
    return { modelCatalog, modelCatalogLoading, modelCatalogSyncing, modelCatalogError, modelCatalogSyncMessage,
        loadModelCatalog, syncModelCatalog, getModelCatalogDetail, initializeModelCatalog, stopModelCatalogEvents };
}
