/**
 * 模型验真（渠道降智检查 + 指纹检测）状态管理
 *
 * 探测题库由后端内置目录（get_detection_suites）维护，前端只按 id 勾选。
 */

import { ref, computed } from "vue";
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

export interface CheckSpec {
  kind: string;
  value: string;
  tolerance: number;
}

export interface FamilyExpectation {
  family: string;
  patterns: string[];
}

export interface DetectionProbe {
  id: string;
  name: string;
  category: "identity" | "fingerprint" | "capability";
  description: string;
  text: string;
  /** 同义变体问法（答案不变），发送时随机选择以去除同质化 */
  variants: string[];
  maxTokens: number;
  temperature: number;
  check?: CheckSpec;
  expected: FamilyExpectation[];
  repeats: boolean;
}

export interface RunParams {
  targets: ProbeTarget[];
  probeIds: string[];
  repeats: number;
  concurrency: number;
  timeoutSeconds: number;
}

export interface AutoCheckOutcome {
  kind: string;
  passed: boolean;
  detail: string;
}

export interface ProbeResult {
  channelId: string;
  channelName: string;
  model: string;
  probeId: string;
  probeName: string;
  category: string;
  sampleIndex: number;
  ok: boolean;
  durationMs?: number;
  promptTokens?: number;
  completionTokens?: number;
  tokensPerSec?: number;
  autoCheck?: AutoCheckOutcome;
  familyMatch?: string;
  /** 实际发送的最终提问（随机变体 + 对话包装后） */
  requestText?: string;
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

export type VerdictKind = "ok" | "suspicious" | "impersonation" | "unreachable";

export interface TargetVerdict {
  channelId: string;
  channelName: string;
  model: string;
  verdict: VerdictKind;
  claimedFamily?: string;
  detectedFamily?: string;
  identityFamily?: string;
  identityConsistent?: boolean;
  capabilityPassed: number;
  capabilityTotal: number;
  consistencyRate?: number;
  totalRequests: number;
  okCount: number;
  avgDurationMs?: number;
  avgTokensPerSec?: number;
  issues: string[];
  results: ProbeResult[];
}

export interface TestRunRecord {
  id: number;
  startedAt: string;
  finishedAt?: string;
  status: string;
  targetCount: number;
  probeCount: number;
  repeats: number;
  config: any;
  summary?: { targets?: TargetVerdict[] };
}

export interface RunStartInfo {
  runId: number;
  total: number;
}

// ============================================================================
// State
// ============================================================================

const selectedTargets = ref<ProbeTarget[]>([]);
const selectedProbeIds = ref<Set<string>>(new Set());
const suites = ref<DetectionProbe[]>([]);

// Run parameters
const repeats = ref(3);
const concurrency = ref(4);
const timeoutSeconds = ref(120);

// Runtime state
const isRunning = ref(false);
const currentRunId = ref<number | null>(null);
const progress = ref<RunProgress | null>(null);
/** 运行中实时收到的探测明细（结束后由 verdicts 接管展示） */
const liveResults = ref<ProbeResult[]>([]);
/** 按目标聚合的验真结论（运行结束载入或历史载入） */
const verdicts = ref<TargetVerdict[]>([]);

// History
const historyRuns = ref<TestRunRecord[]>([]);
const historyLoading = ref(false);

// ============================================================================
// Computed
// ============================================================================

const selectedProbes = computed(() =>
  suites.value.filter((p) => selectedProbeIds.value.has(p.id)),
);

const totalRequests = computed(() => {
  const perTarget = selectedProbes.value.reduce(
    (sum, p) => sum + (p.repeats ? repeats.value : 1),
    0,
  );
  return selectedTargets.value.length * perTarget;
});

const canRun = computed(
  () =>
    !isRunning.value &&
    selectedTargets.value.length > 0 &&
    selectedProbeIds.value.size > 0,
);

const progressPercent = computed(() => {
  if (!progress.value || progress.value.total === 0) return 0;
  return Math.round((progress.value.completed / progress.value.total) * 100);
});

/** 结论分布（用于历史列表与结果页概览） */
const verdictCounts = computed(() => {
  const counts: Record<VerdictKind, number> = {
    ok: 0,
    suspicious: 0,
    impersonation: 0,
    unreachable: 0,
  };
  for (const v of verdicts.value) counts[v.verdict] = (counts[v.verdict] || 0) + 1;
  return counts;
});

// ============================================================================
// Actions
// ============================================================================

async function loadSuites() {
  try {
    const list = await runCommand<DetectionProbe[]>("get_detection_suites");
    suites.value = list || [];
    // 首次载入默认全选
    if (selectedProbeIds.value.size === 0 && suites.value.length > 0) {
      selectedProbeIds.value = new Set(suites.value.map((p) => p.id));
    }
  } catch (error) {
    console.error("Failed to load detection suites:", error);
  }
}

async function startRun() {
  if (!canRun.value) return;

  const params: RunParams = {
    targets: selectedTargets.value,
    probeIds: [...selectedProbeIds.value],
    repeats: repeats.value,
    concurrency: concurrency.value,
    timeoutSeconds: timeoutSeconds.value,
  };

  const result = await runCommand<RunStartInfo>("run_model_test", { params });
  isRunning.value = true;
  currentRunId.value = result.runId;
  liveResults.value = [];
  verdicts.value = [];
  progress.value = {
    runId: result.runId,
    phase: "running",
    completed: 0,
    total: result.total,
  };
}

async function cancelRun() {
  if (!isRunning.value) return;
  await runCommand("cancel_model_test");
}

async function loadHistory(limit = 50) {
  historyLoading.value = true;
  try {
    const runs = await runCommand<TestRunRecord[]>("list_model_test_runs", { limit });
    historyRuns.value = runs || [];
  } catch (error) {
    console.error("Failed to load detection history:", error);
  } finally {
    historyLoading.value = false;
  }
}

async function loadRunResults(runId: number) {
  const list = await runCommand<TargetVerdict[]>("get_model_test_results", { runId });
  verdicts.value = list || [];
  liveResults.value = [];
  currentRunId.value = runId;
}

async function deleteRun(runId: number) {
  await runCommand<number>("delete_model_test_run", { runId });
  if (currentRunId.value === runId) {
    verdicts.value = [];
    liveResults.value = [];
    currentRunId.value = null;
  }
  await loadHistory();
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

    progress.value = data;
    if (data.result) liveResults.value.push(data.result);

    if (data.phase === "finished" || data.phase === "cancelled" || data.phase === "error") {
      isRunning.value = false;
      // 收尾后拉取权威结论（与落库内容一致）
      void loadRunResults(data.runId).catch(() => {});
      void loadHistory();
    }
  });
}

// ============================================================================
// Export
// ============================================================================

export function useModelTest() {
  const modelProxy = useModelProxy();

  // 首次进入页面：挂事件监听 + 加载真实渠道配置（含模型缓存）+ 探测目录 + 历史
  async function init() {
    await setupProgressListener();
    void modelProxy.loadProxyData();
    void loadSuites();
    void loadHistory();
  }

  return {
    // State
    selectedTargets,
    selectedProbeIds,
    suites,
    selectedProbes,

    // Run params
    repeats,
    concurrency,
    timeoutSeconds,

    // Runtime
    isRunning,
    currentRunId,
    progress,
    progressPercent,
    liveResults,
    verdicts,
    verdictCounts,

    // History
    historyRuns,
    historyLoading,

    // Computed
    totalRequests,
    canRun,

    // Actions
    init,
    startRun,
    cancelRun,
    loadHistory,
    loadRunResults,
    deleteRun,

    // From modelProxy
    channels: modelProxy.proxyConfig,
    channelModels: modelProxy.channelModels,
    loadCachedModels: modelProxy.loadCachedModels,
    modelsForChannel: modelProxy.modelsForChannel,
  };
}
