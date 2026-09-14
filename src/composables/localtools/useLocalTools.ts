import { ref, computed } from "vue";
import { runLocalCommand } from "../core/ipc";
import { useToast } from "../core/useToast";
import type {
  LocalToolBackupEntry,
  LocalToolConfigPatch,
  LocalToolConfigSaveResult,
  LocalToolConfigSnapshot,
  LocalToolDiffReport,
  LocalToolDiffTarget,
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
/** 清单与磁盘配置的一致性报告（key = 行标识）；未比对时为空。 */
const diffReport = ref<LocalToolDiffReport | null>(null);
const diffLoading = ref(false);
/** 每次读盘/写入后自增，供页面在配置变化时重新比对。 */
const configRevision = ref(0);

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
  diffReport.value = null;
  if (!tool) return;
  await reloadSnapshot();
}

/** 实时读盘重新快照（打开工具 / 冲突后重载）。 */
async function reloadSnapshot() {
  if (!activeTool.value) return;
  snapshotLoading.value = true;
  diffReport.value = null;
  try {
    snapshot.value = await runLocalCommand<LocalToolConfigSnapshot>(
      "get_local_tool_config",
      { tool: activeTool.value },
    );
    configRevision.value += 1;
  } catch (error) {
    showToast(`配置读取失败：${error}`, true);
  } finally {
    snapshotLoading.value = false;
  }
}

/**
 * 比对「清单组装的配置」与磁盘现状：返回的报告里每行带 consistent 标记。
 * 静默失败（返回 null）—— 清单页会退回「未比对」展示，不打扰用户。
 *
 * 清单变化会连续触发比对，只采纳最后一次发出的结果，避免旧响应覆盖新结论。
 */
let diffSeq = 0;

async function diffTargets(
  targets: LocalToolDiffTarget[],
): Promise<LocalToolDiffReport | null> {
  if (!activeTool.value) return null;
  const seq = ++diffSeq;
  diffLoading.value = true;
  try {
    const report = await runLocalCommand<LocalToolDiffReport>("diff_local_tool_config", {
      tool: activeTool.value,
      targets,
    });
    if (seq !== diffSeq) return report;
    diffReport.value = report;
    return report;
  } catch {
    if (seq === diffSeq) diffReport.value = null;
    return null;
  } finally {
    if (seq === diffSeq) diffLoading.value = false;
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
    // 写入后磁盘已变，落库前的比对结论作废，由调用方重新比对。
    diffReport.value = null;
    configRevision.value += 1;
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
    diffReport,
    diffLoading,
    configRevision,
    loadToolList,
    selectTool,
    reloadSnapshot,
    saveSnapshot,
    loadBackups,
    restoreBackup,
    diffTargets,
  };
}
