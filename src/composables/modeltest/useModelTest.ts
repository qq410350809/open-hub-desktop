/**
 * 模型能力测试状态管理
 */

import { ref, computed } from "vue";
import type { ProbePrompt } from "./builtinSuites";
import { BUILTIN_SUITES } from "./builtinSuites";
import { useModelProxy } from "../useModelProxy";
import { runCommand } from "../core/ipc";
import { listen, type UnlistenFn } from "../core/events";

// ============================================================================
// Types (matching Rust backend)
// ============================================================================

export interface ProbeTarget {
  channelId: string;
  model: string;
}

export interface JudgeSpec {
  channelId: string;
  model: string;
}

export interface RunParams {
  targets: ProbeTarget[];
  prompts: ProbePrompt[];
  concurrency: number;
  timeoutSeconds: number;
  judge?: JudgeSpec;
}

export interface AutoCheckOutcome {
  kind: string;
  passed: boolean;
  detail: string;
}

export interface JudgeOutcome {
  score?: number;
  reason: string;
}

export interface ProbeResult {
  channelId: string;
  channelName: string;
  model: string;
  promptId: string;
  promptName: string;
  category: string;
  ok: boolean;
  durationMs?: number;
  promptTokens?: number;
  completionTokens?: number;
  tokensPerSec?: number;
  autoCheck?: AutoCheckOutcome;
  score?: number;
  judge?: JudgeOutcome;
  error?: string;
  responseText?: string;
}

export interface RunProgress {
  runId: number;
  phase: "running" | "finished" | "cancelled" | "error";
  completed: number;
  total: number;
  result?: ProbeResult;
}

export interface TestRunRecord {
  id: number;
  startedAt: string;
  finishedAt?: string;
  status: string;
  targetCount: number;
  promptCount: number;
  config: any;
  summary?: any;
}

export interface ModelSummary {
  channelId: string;
  channelName: string;
  model: string;
  total: number;
  okCount: number;
  avgScore?: number;
  avgDurationMs?: number;
  avgTokensPerSec?: number;
}

export interface RunStartInfo {
  runId: number;
  total: number;
}

// ============================================================================
// State
// ============================================================================

const selectedTargets = ref<ProbeTarget[]>([]);
const selectedPromptIds = ref<Set<string>>(new Set(BUILTIN_SUITES.map((p) => p.id)));
const customPrompts = ref<ProbePrompt[]>([]);

// Run parameters
const concurrency = ref(4);
const timeoutSeconds = ref(120);
const judgeChannelId = ref("");
const judgeModel = ref("");
const enableJudge = ref(true);

// Runtime state
const isRunning = ref(false);
const currentRunId = ref<number | null>(null);
const progress = ref<RunProgress | null>(null);
const currentResults = ref<ProbeResult[]>([]);

// History
const historyRuns = ref<TestRunRecord[]>([]);
const historyLoading = ref(false);

// Last config
const lastConfigLoaded = ref(false);

// ============================================================================
// Computed
// ============================================================================

const allPrompts = computed(() => {
  return [...BUILTIN_SUITES, ...customPrompts.value];
});

const selectedPrompts = computed(() => {
  return allPrompts.value.filter((p) => selectedPromptIds.value.has(p.id));
});

const totalTests = computed(() => {
  return selectedTargets.value.length * selectedPrompts.value.length;
});

const canRun = computed(() => {
  return (
    !isRunning.value &&
    selectedTargets.value.length > 0 &&
    selectedPrompts.value.length > 0
  );
});

const progressPercent = computed(() => {
  if (!progress.value || progress.value.total === 0) return 0;
  return Math.round((progress.value.completed / progress.value.total) * 100);
});

// Group results by model for matrix view
const resultsByModel = computed(() => {
  const grouped = new Map<string, ProbeResult[]>();
  for (const result of currentResults.value) {
    const key = `${result.channelId}::${result.model}`;
    if (!grouped.has(key)) {
      grouped.set(key, []);
    }
    grouped.get(key)!.push(result);
  }
  return grouped;
});

// Calculate model summaries
const modelSummaries = computed(() => {
  const summaries: ModelSummary[] = [];

  for (const [key, results] of resultsByModel.value.entries()) {
    const [channelId, model] = key.split("::");
    const okCount = results.filter((r) => r.ok).length;
    const scores = results.map((r) => r.score).filter((s) => s !== undefined && s !== null) as number[];
    const durations = results.map((r) => r.durationMs).filter((d) => d !== undefined && d !== null) as number[];
    const tps = results.map((r) => r.tokensPerSec).filter((t) => t !== undefined && t !== null) as number[];

    summaries.push({
      channelId,
      channelName: results[0]?.channelName || "",
      model,
      total: results.length,
      okCount,
      avgScore: scores.length > 0 ? scores.reduce((a, b) => a + b, 0) / scores.length : undefined,
      avgDurationMs: durations.length > 0 ? Math.round(durations.reduce((a, b) => a + b, 0) / durations.length) : undefined,
      avgTokensPerSec: tps.length > 0 ? Math.round(tps.reduce((a, b) => a + b, 0) / tps.length * 10) / 10 : undefined,
    });
  }

  // Sort by avgScore desc
  summaries.sort((a, b) => (b.avgScore || 0) - (a.avgScore || 0));

  return summaries;
});

// ============================================================================
// Actions
// ============================================================================

async function startRun() {
  if (!canRun.value) return;

  const judge = enableJudge.value && judgeChannelId.value && judgeModel.value
    ? { channelId: judgeChannelId.value, model: judgeModel.value }
    : undefined;

  const params: RunParams = {
    targets: selectedTargets.value,
    prompts: selectedPrompts.value,
    concurrency: concurrency.value,
    timeoutSeconds: timeoutSeconds.value,
    judge,
  };

  try {
    const result = await runCommand<RunStartInfo>("run_model_test", { params });
    isRunning.value = true;
    currentRunId.value = result.runId;
    currentResults.value = [];
    progress.value = {
      runId: result.runId,
      phase: "running",
      completed: 0,
      total: result.total,
    };

    // Save last config
    await saveLastConfig();
  } catch (error) {
    console.error("Failed to start model test:", error);
    throw error;
  }
}

async function cancelRun() {
  if (!isRunning.value) return;

  try {
    await runCommand("cancel_model_test");
  } catch (error) {
    console.error("Failed to cancel model test:", error);
  }
}

async function loadHistory(limit = 50) {
  historyLoading.value = true;
  try {
    const runs = await runCommand<TestRunRecord[]>("list_model_test_runs", { limit });
    historyRuns.value = runs;
  } catch (error) {
    console.error("Failed to load test history:", error);
  } finally {
    historyLoading.value = false;
  }
}

async function loadRunResults(runId: number) {
  try {
    const results = await runCommand<ProbeResult[]>("get_model_test_results", { runId });
    currentResults.value = results;
    currentRunId.value = runId;
  } catch (error) {
    console.error("Failed to load run results:", error);
    throw error;
  }
}

async function deleteRun(runId: number) {
  try {
    await runCommand<number>("delete_model_test_run", { runId });
    await loadHistory();
  } catch (error) {
    console.error("Failed to delete run:", error);
    throw error;
  }
}

async function loadCustomPrompts() {
  try {
    const prompts = await runCommand<ProbePrompt[]>("get_model_test_custom_prompts");
    customPrompts.value = prompts;
  } catch (error) {
    console.error("Failed to load custom prompts:", error);
  }
}

async function saveCustomPrompts(prompts: ProbePrompt[]) {
  try {
    await runCommand("save_model_test_custom_prompts", { prompts });
    customPrompts.value = prompts;
  } catch (error) {
    console.error("Failed to save custom prompts:", error);
    throw error;
  }
}

async function loadLastConfig() {
  if (lastConfigLoaded.value) return;

  try {
    const config = await runCommand<any>("get_model_test_last_config");
    if (config) {
      if (config.targets) selectedTargets.value = config.targets;
      if (config.promptIds) selectedPromptIds.value = new Set(config.promptIds);
      if (config.concurrency) concurrency.value = config.concurrency;
      if (config.timeoutSeconds) timeoutSeconds.value = config.timeoutSeconds;
      if (config.judgeChannelId) judgeChannelId.value = config.judgeChannelId;
      if (config.judgeModel) judgeModel.value = config.judgeModel;
      if (config.enableJudge !== undefined) enableJudge.value = config.enableJudge;
    }
    lastConfigLoaded.value = true;
  } catch (error) {
    console.error("Failed to load last config:", error);
  }
}

async function saveLastConfig() {
  const config = {
    targets: selectedTargets.value,
    promptIds: Array.from(selectedPromptIds.value),
    concurrency: concurrency.value,
    timeoutSeconds: timeoutSeconds.value,
    judgeChannelId: judgeChannelId.value,
    judgeModel: judgeModel.value,
    enableJudge: enableJudge.value,
  };

  try {
    await runCommand("save_model_test_last_config", { config });
  } catch (error) {
    console.error("Failed to save last config:", error);
  }
}

// ============================================================================
// Event Listeners
// ============================================================================

let progressUnlisten: UnlistenFn | undefined;

async function setupProgressListener() {
  if (progressUnlisten) return;
  progressUnlisten = await listen<RunProgress>("model-test-progress", (event) => {
    const data = event.payload;

    // 只接受当前运行的事件，防止加载历史结果时被旧事件污染
    if (currentRunId.value !== null && data.runId !== currentRunId.value) return;

    // Update progress
    progress.value = data;

    // Append result if present
    if (data.result) {
      currentResults.value.push(data.result);
    }

    // Update running state
    if (data.phase === "finished" || data.phase === "cancelled" || data.phase === "error") {
      isRunning.value = false;
      void loadHistory();
    }
  });
}

// ============================================================================
// Exports
// ============================================================================

export function useModelTest() {
  const modelProxy = useModelProxy();

  // 首次进入页面时初始化：挂事件监听 + 拉渠道模型缓存 + 载入历史配置
  async function init() {
    await setupProgressListener();
    if (!modelProxy.channelModels || Object.keys(modelProxy.channelModels.value ?? {}).length === 0) {
      void modelProxy.loadCachedModels();
    }
    void loadLastConfig();
    void loadCustomPrompts();
    void loadHistory();
  }

  return {
    // State
    selectedTargets,
    selectedPromptIds,
    customPrompts,
    allPrompts,
    selectedPrompts,

    // Run params
    concurrency,
    timeoutSeconds,
    judgeChannelId,
    judgeModel,
    enableJudge,

    // Runtime
    isRunning,
    currentRunId,
    progress,
    progressPercent,
    currentResults,
    resultsByModel,
    modelSummaries,

    // History
    historyRuns,
    historyLoading,

    // Computed
    totalTests,
    canRun,

    // Actions
    init,
    startRun,
    cancelRun,
    loadHistory,
    loadRunResults,
    deleteRun,
    loadCustomPrompts,
    saveCustomPrompts,
    loadLastConfig,
    saveLastConfig,

    // From modelProxy
    channels: modelProxy.proxyConfig,
    channelModels: modelProxy.channelModels,
    loadCachedModels: modelProxy.loadCachedModels,
    modelsForChannel: modelProxy.modelsForChannel,
  };
}
