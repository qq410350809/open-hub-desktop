import { ref, computed } from "vue";
import { runLocalCommand } from "../core/ipc";
import { useToast } from "../core/useToast";
import type {
  LocalToolBackupEntry,
  LocalToolConfigPatch,
  LocalToolConfigSaveResult,
  LocalToolConfigSnapshot,
  LocalToolListReport,
} from "../../types";

/**
 * 本地 AI 编程工具模型配置管理。
 * 全部命令走本地数据平面（runLocalCommand），实时读盘、无缓存。
 */

const toolList = ref<LocalToolListReport | null>(null);
const toolListLoading = ref(false);
const activeTool = ref<string>("");
const snapshot = ref<LocalToolConfigSnapshot | null>(null);
const snapshotLoading = ref(false);
const saving = ref(false);
const backups = ref<LocalToolBackupEntry[]>([]);
const backupsLoading = ref(false);

const { showToast } = useToast();

/** 工具列表：只收录支持结构化编辑的工具，全部展示。 */
const visibleTools = computed(() => toolList.value?.tools ?? []);

async function loadToolList() {
  toolListLoading.value = true;
  try {
    toolList.value = await runLocalCommand<LocalToolListReport>("list_local_tools");
  } catch (error) {
    const message = String(error);
    if (!message.includes("仅在客户端本地可用")) {
      showToast(`Agent 扫描失败：${error}`, true);
    }
  } finally {
    toolListLoading.value = false;
  }
}

async function selectTool(tool: string) {
  activeTool.value = tool;
  snapshot.value = null;
  if (!tool) return;
  await reloadSnapshot();
}

/** 实时读盘重新快照（打开工具 / 冲突后重载）。 */
async function reloadSnapshot() {
  if (!activeTool.value) return;
  snapshotLoading.value = true;
  try {
    snapshot.value = await runLocalCommand<LocalToolConfigSnapshot>(
      "get_local_tool_config",
      { tool: activeTool.value },
    );
  } catch (error) {
    showToast(`配置读取失败：${error}`, true);
  } finally {
    snapshotLoading.value = false;
  }
}

/** 保存配置。冲突时返回 false 并弹提示（调用方可据此询问重载）。 */
async function saveSnapshot(patch: LocalToolConfigPatch): Promise<LocalToolConfigSaveResult | null> {
  if (!activeTool.value || !snapshot.value) return null;
  saving.value = true;
  try {
    const result = await runLocalCommand<LocalToolConfigSaveResult>("save_local_tool_config", {
      tool: activeTool.value,
      patch,
    });
    snapshot.value = result.snapshot;
    const backupNote = result.backedUp?.length
      ? `，已先备份 ${result.backedUp.join("、")}（非本软件写入或已被外部修改）`
      : "";
    showToast(`已写入配置${backupNote}，${result.snapshot.effectNote}`);
    return result;
  } catch (error) {
    const message = String(error);
    if (message.includes("被外部程序修改")) {
      showToast(message, true);
    } else {
      showToast(`配置保存失败：${message}`, true);
    }
    return null;
  } finally {
    saving.value = false;
  }
}

async function loadBackups() {
  if (!activeTool.value) return;
  backupsLoading.value = true;
  try {
    backups.value = await runLocalCommand<LocalToolBackupEntry[]>("list_local_tool_backups", {
      tool: activeTool.value,
    });
  } catch (error) {
    showToast(`备份列表读取失败：${error}`, true);
  } finally {
    backupsLoading.value = false;
  }
}

async function restoreBackup(name: string) {
  if (!activeTool.value) return false;
  try {
    await runLocalCommand("restore_local_tool_backup", { tool: activeTool.value, name });
    showToast("备份已还原");
    await reloadSnapshot();
    return true;
  } catch (error) {
    showToast(`备份还原失败：${error}`, true);
    return false;
  }
}

export function useLocalTools() {
  return {
    toolList,
    toolListLoading,
    visibleTools,
    activeTool,
    snapshot,
    snapshotLoading,
    saving,
    backups,
    backupsLoading,
    loadToolList,
    selectTool,
    reloadSnapshot,
    saveSnapshot,
    loadBackups,
    restoreBackup,
  };
}
