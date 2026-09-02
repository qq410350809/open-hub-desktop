<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { icons } from "../../icons";
import { useToast } from "../../composables/useToast";
import { useConfirm } from "../../composables/useConfirm";
import { useModelTest } from "../../composables/modeltest/useModelTest";
import { BUILTIN_SUITES, type ProbePrompt } from "../../composables/modeltest/builtinSuites";
import AppTable, { type AppTableColumn } from "../common/AppTable.vue";
import type { ProbeResult, TestRunRecord } from "../../composables/modeltest/useModelTest";
import { formatDuration } from "../../utils";

const { showToast } = useToast();
const { confirm } = useConfirm();
const mt = useModelTest();

// —— 页面状态 ——
const activeTab = ref<"matrix" | "detail" | "history">("matrix");
const initialized = ref(false);

// —— 目标选择（渠道分组的模型多选）——
const targetPickerOpen = ref(false);
const expandedChannels = ref<Set<string>>(new Set());

interface ChannelOption {
  id: string;
  name: string;
  enabled: boolean;
}

const channelOptions = computed<ChannelOption[]>(() => {
  const channels = mt.channels.value?.channels ?? [];
  return channels.map((c: any) => ({
    id: c.id,
    name: c.name,
    enabled: c.enabled !== false,
  }));
});

function modelsOfChannel(channelId: string): string[] {
  const list = mt.modelsForChannel?.(channelId) ?? mt.channelModels.value?.[channelId] ?? [];
  return list;
}

function toggleChannelExpand(channelId: string) {
  const next = new Set(expandedChannels.value);
  if (next.has(channelId)) next.delete(channelId);
  else next.add(channelId);
  expandedChannels.value = next;
}

function isTargetSelected(channelId: string, model: string): boolean {
  return mt.selectedTargets.value.some((t) => t.channelId === channelId && t.model === model);
}

function toggleTarget(channelId: string, model: string) {
  const list = mt.selectedTargets.value;
  const index = list.findIndex((t) => t.channelId === channelId && t.model === model);
  if (index >= 0) list.splice(index, 1);
  else list.push({ channelId, model });
}

function channelSelectedCount(channelId: string): number {
  return mt.selectedTargets.value.filter((t) => t.channelId === channelId).length;
}

function removeTarget(target: { channelId: string; model: string }) {
  const list = mt.selectedTargets.value;
  const index = list.findIndex((t) => t.channelId === target.channelId && t.model === target.model);
  if (index >= 0) list.splice(index, 1);
}

function channelNameOf(channelId: string): string {
  return channelOptions.value.find((c) => c.id === channelId)?.name ?? channelId;
}

// —— 套件勾选 ——
function isPromptSelected(id: string): boolean {
  return mt.selectedPromptIds.value.has(id);
}

function togglePrompt(id: string) {
  const next = new Set(mt.selectedPromptIds.value);
  if (next.has(id)) next.delete(id);
  else next.add(id);
  mt.selectedPromptIds.value = next;
}

// —— 自定义提示词弹窗 ——
const promptEditorOpen = ref(false);
const editingPrompt = ref<ProbePrompt | null>(null);
const promptForm = ref<{ name: string; category: string; text: string; maxTokens: number; checkKind: string; checkValue: string; judge: boolean }>({
  name: "", category: "自定义", text: "", maxTokens: 512, checkKind: "none", checkValue: "", judge: false,
});

function openPromptEditor(prompt?: ProbePrompt) {
  editingPrompt.value = prompt ?? null;
  promptForm.value = prompt
    ? {
        name: prompt.name,
        category: prompt.category,
        text: prompt.text,
        maxTokens: prompt.maxTokens,
        checkKind: prompt.check?.kind ?? "none",
        checkValue: prompt.check?.value ?? "",
        judge: prompt.judge,
      }
    : { name: "", category: "自定义", text: "", maxTokens: 512, checkKind: "none", checkValue: "", judge: false };
  promptEditorOpen.value = true;
  document.body.classList.add("modal-open");
}

function closePromptEditor() {
  promptEditorOpen.value = false;
  document.body.classList.remove("modal-open");
}

async function savePrompt() {
  const form = promptForm.value;
  if (!form.name.trim() || !form.text.trim()) {
    showToast("提示词名称与内容不能为空", true);
    return;
  }
  const prompt: ProbePrompt = {
    id: editingPrompt.value?.id ?? `custom-${Date.now()}`,
    name: form.name.trim(),
    category: form.category.trim() || "自定义",
    text: form.text,
    maxTokens: form.maxTokens,
    temperature: 0.3,
    check: form.checkKind === "none" ? undefined : { kind: form.checkKind as any, value: form.checkValue, tolerance: 0 },
    judge: form.judge,
  };
  const list = editingPrompt.value
    ? mt.customPrompts.value.map((p) => (p.id === prompt.id ? prompt : p))
    : [...mt.customPrompts.value, prompt];
  try {
    await mt.saveCustomPrompts(list);
    // 新保存的自定义题默认勾选
    if (!editingPrompt.value) togglePrompt(prompt.id);
    closePromptEditor();
    showToast("自定义提示词已保存");
  } catch (error) {
    showToast(`保存失败：${String(error)}`, true);
  }
}

async function deletePrompt(prompt: ProbePrompt) {
  const accepted = await confirm({
    title: "删除自定义提示词",
    message: `确定删除「${prompt.name}」吗？`,
    confirmText: "删除",
    danger: true,
  });
  if (!accepted) return;
  const list = mt.customPrompts.value.filter((p) => p.id !== prompt.id);
  try {
    await mt.saveCustomPrompts(list);
    mt.selectedPromptIds.value = new Set([...mt.selectedPromptIds.value].filter((id) => id !== prompt.id));
    showToast("已删除");
  } catch (error) {
    showToast(`删除失败：${String(error)}`, true);
  }
}

// —— 评审模型选项 ——
const judgeModelOptions = computed(() => {
  const opts: { value: string; text: string }[] = [];
  for (const channel of channelOptions.value) {
    if (!channel.enabled) continue;
    for (const model of modelsOfChannel(channel.id)) {
      opts.push({ value: `${channel.id}::${model}`, text: `${channel.name} / ${model}` });
    }
  }
  return opts;
});

const judgeModelValue = computed({
  get: () => (mt.judgeChannelId.value && mt.judgeModel.value ? `${mt.judgeChannelId.value}::${mt.judgeModel.value}` : ""),
  set: (value: string) => {
    if (!value) {
      mt.judgeChannelId.value = "";
      mt.judgeModel.value = "";
      return;
    }
    const [channelId, ...rest] = value.split("::");
    mt.judgeChannelId.value = channelId;
    mt.judgeModel.value = rest.join("::");
  },
});

// —— 运行 ——
async function handleStartRun() {
  if (!mt.canRun.value) return;
  try {
    await mt.startRun();
    activeTab.value = "matrix";
    showToast(`测试已启动：${mt.totalTests.value} 项`);
  } catch (error) {
    showToast(`启动失败：${String(error)}`, true);
  }
}

async function handleCancelRun() {
  try {
    await mt.cancelRun();
    showToast("已发送取消请求");
  } catch (error) {
    showToast(`取消失败：${String(error)}`, true);
  }
}

// —— 对比矩阵 ——
interface MatrixRow {
  channelId: string;
  channelName: string;
  model: string;
  cells: Record<string, ProbeResult | undefined>;
  summary: {
    total: number;
    okCount: number;
    avgScore?: number;
    avgDurationMs?: number;
    avgTokensPerSec?: number;
  };
}

const matrixColumns = computed<AppTableColumn[]>(() => {
  const cols: AppTableColumn[] = [
    { key: "model", title: "渠道 / 模型", width: "minmax(200px, 1fr)", sortable: false },
  ];
  for (const prompt of mt.selectedPrompts.value.length ? mt.selectedPrompts.value : currentPromptList()) {
    cols.push({ key: prompt.id, title: prompt.name, width: "132px", sortable: false, align: "center" });
  }
  cols.push({ key: "summary", title: "汇总", width: "210px", sortable: false, align: "center" });
  return cols;
});

function currentPromptList(): ProbePrompt[] {
  return [...BUILTIN_SUITES, ...mt.customPrompts.value];
}

const matrixRows = computed<MatrixRow[]>(() => {
  // 按「渠道×模型」分组当前结果
  const grouped = new Map<string, MatrixRow>();
  for (const result of mt.currentResults.value) {
    const key = `${result.channelId}::${result.model}`;
    let row = grouped.get(key);
    if (!row) {
      row = {
        channelId: result.channelId,
        channelName: result.channelName,
        model: result.model,
        cells: {},
        summary: { total: 0, okCount: 0 },
      };
      grouped.set(key, row);
    }
    row.cells[result.promptId] = result;
    row.summary.total += 1;
    if (result.ok) row.summary.okCount += 1;
  }

  const rows = [...grouped.values()];
  // 汇总计算
  for (const row of rows) {
    const results = Object.values(row.cells).filter(Boolean) as ProbeResult[];
    const scores = results.map((r) => r.score).filter((s): s is number => typeof s === "number");
    const durations = results.map((r) => r.durationMs).filter((d): d is number => typeof d === "number");
    const tps = results.map((r) => r.tokensPerSec).filter((t): t is number => typeof t === "number");
    row.summary.avgScore = scores.length ? scores.reduce((a, b) => a + b, 0) / scores.length : undefined;
    row.summary.avgDurationMs = durations.length ? Math.round(durations.reduce((a, b) => a + b, 0) / durations.length) : undefined;
    row.summary.avgTokensPerSec = tps.length ? Math.round((tps.reduce((a, b) => a + b, 0) / tps.length) * 10) / 10 : undefined;
  }
  // 按平均分排序（无分按成功率）
  rows.sort((a, b) => (b.summary.avgScore ?? -1) - (a.summary.avgScore ?? -1));
  return rows;
});

function scoreClass(result?: ProbeResult): string {
  if (!result) return "";
  if (!result.ok) return "is-error";
  const score = result.score;
  if (typeof score !== "number") return result.ok ? "is-pass" : "";
  if (score >= 8) return "is-great";
  if (score >= 5) return "is-mid";
  return "is-low";
}

function cellText(result?: ProbeResult): string {
  if (!result) return "—";
  if (!result.ok) return "✗";
  const score = result.score;
  if (typeof score === "number") return score.toFixed(1);
  return "✓";
}

function formatDurationShort(ms?: number): string {
  if (ms == null) return "";
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

// —— 明细 ——
const detailFilterChannel = ref("");
const detailResults = computed(() => {
  if (!detailFilterChannel.value) return mt.currentResults.value;
  return mt.currentResults.value.filter((r) => r.channelId === detailFilterChannel.value);
});

const resultColumns: AppTableColumn[] = [
  { key: "channelName", title: "渠道", width: "130px", sortable: true },
  { key: "model", title: "模型", width: "minmax(160px, 1fr)", sortable: true },
  { key: "promptName", title: "题目", width: "140px", sortable: true },
  { key: "category", title: "类别", width: "90px", sortable: true },
  { key: "ok", title: "状态", width: "72px", align: "center", sortable: true },
  { key: "score", title: "得分", width: "72px", align: "right", sortable: true },
  { key: "durationMs", title: "耗时", width: "90px", align: "right", sortable: true },
  { key: "tokensPerSec", title: "tokens/s", width: "92px", align: "right", sortable: true },
];

function openResultDetail(result: ProbeResult) {
  detailResult.value = result;
  document.body.classList.add("modal-open");
}

const detailResult = ref<ProbeResult | null>(null);

function closeResultDetail() {
  detailResult.value = null;
  document.body.classList.remove("modal-open");
}

function resultStatusText(result: ProbeResult): string {
  return result.ok ? "✓ 成功" : "✗ 失败";
}

// —— 历史 ——
const historyColumns: AppTableColumn[] = [
  { key: "startedAt", title: "开始时间", width: "170px", sortable: false },
  { key: "targetCount", title: "目标数", width: "80px", align: "right", sortable: false },
  { key: "promptCount", title: "题数", width: "72px", align: "right", sortable: false },
  { key: "status", title: "状态", width: "96px", align: "center", sortable: false },
  { key: "elapsed", title: "耗时", width: "100px", align: "right", sortable: false },
  { key: "actions", title: "操作", width: "150px", align: "center", sortable: false },
];

function historyStatusText(run: TestRunRecord): string {
  const map: Record<string, string> = {
    running: "进行中",
    finished: "已完成",
    cancelled: "已取消",
    error: "出错",
  };
  return map[run.status] ?? run.status;
}

function historyElapsed(run: TestRunRecord): string {
  if (!run.finishedAt) return "—";
  const start = new Date(run.startedAt).getTime();
  const end = new Date(run.finishedAt).getTime();
  if (Number.isNaN(start) || Number.isNaN(end)) return "—";
  return formatDuration(Math.max(0, end - start));
}

async function loadRun(run: TestRunRecord) {
  try {
    await mt.loadRunResults(run.id);
    activeTab.value = "matrix";
    showToast(`已载入第 ${run.id} 次运行结果`);
  } catch (error) {
    showToast(`载入失败：${String(error)}`, true);
  }
}

async function removeRun(run: TestRunRecord) {
  const accepted = await confirm({
    title: "删除测试记录",
    message: `确定删除第 ${run.id} 次运行及其全部结果吗？`,
    confirmText: "删除",
    danger: true,
  });
  if (!accepted) return;
  try {
    await mt.deleteRun(run.id);
    showToast("已删除");
  } catch (error) {
    showToast(`删除失败：${String(error)}`, true);
  }
}

onMounted(async () => {
  if (initialized.value) return;
  initialized.value = true;
  await mt.init();
});
</script>

<template>
  <main class="model-test-page mt-page">
    <!-- ============ 顶部配置区 ============ -->
    <header class="mt-config-card">
      <div class="mt-config-row">
        <div class="mt-config-block is-targets">
          <div class="mt-block-head">
            <span class="mt-block-label">被测目标</span>
            <button type="button" class="mt-link-btn" @click="targetPickerOpen = !targetPickerOpen">
              {{ targetPickerOpen ? "收起" : "选择模型" }}
            </button>
          </div>
          <div v-if="mt.selectedTargets.value.length === 0" class="mt-empty-targets">
            尚未选择模型，点击「选择模型」从已启用渠道挑选
          </div>
          <div v-else class="mt-target-chips">
            <span
              v-for="target in mt.selectedTargets.value"
              :key="`${target.channelId}::${target.model}`"
              class="mt-chip"
            >
              {{ channelNameOf(target.channelId) }} / {{ target.model }}
              <button type="button" class="mt-chip-x" aria-label="移除" @click="removeTarget(target)">×</button>
            </span>
          </div>

          <!-- 渠道分组选择器 -->
          <div v-if="targetPickerOpen" class="mt-target-picker">
            <div v-if="channelOptions.length === 0" class="mt-picker-empty">
              没有可用渠道，请先在「模型反代」页添加并启用渠道
            </div>
            <div v-for="channel in channelOptions" :key="channel.id" class="mt-picker-channel">
              <button
                type="button"
                class="mt-picker-channel-head"
                @click="toggleChannelExpand(channel.id)"
              >
                <span class="mt-picker-arrow" :class="{ open: expandedChannels.has(channel.id) }">▸</span>
                <span class="mt-picker-channel-name">{{ channel.name }}</span>
                <span v-if="!channel.enabled" class="mt-picker-channel-disabled">未启用</span>
                <span v-else-if="channelSelectedCount(channel.id)" class="mt-picker-count">
                  已选 {{ channelSelectedCount(channel.id) }}
                </span>
              </button>
              <div v-if="expandedChannels.has(channel.id)" class="mt-picker-models">
                <div v-if="modelsOfChannel(channel.id).length === 0" class="mt-picker-empty">
                  暂无模型缓存，可在「模型反代」页拉取
                </div>
                <button
                  v-for="model in modelsOfChannel(channel.id)"
                  :key="model"
                  type="button"
                  class="mt-model-option"
                  :class="{ selected: isTargetSelected(channel.id, model), disabled: !channel.enabled }"
                  :disabled="!channel.enabled"
                  @click="toggleTarget(channel.id, model)"
                >
                  {{ model }}
                </button>
              </div>
            </div>
          </div>
        </div>

        <div class="mt-config-block">
          <div class="mt-block-head">
            <span class="mt-block-label">测试题</span>
            <span class="mt-block-hint">已选 {{ mt.selectedPrompts.value.length }} / {{ mt.allPrompts.value.length }}</span>
          </div>
          <div class="mt-prompt-grid">
            <button
              v-for="prompt in BUILTIN_SUITES"
              :key="prompt.id"
              type="button"
              class="mt-prompt-card"
              :class="{ selected: isPromptSelected(prompt.id) }"
              @click="togglePrompt(prompt.id)"
            >
              <span class="mt-prompt-name">{{ prompt.name }}</span>
              <span class="mt-prompt-cat">{{ prompt.category }}<template v-if="prompt.judge"> · 评审</template></span>
            </button>
            <button
              v-for="prompt in mt.customPrompts.value"
              :key="prompt.id"
              type="button"
              class="mt-prompt-card is-custom"
              :class="{ selected: isPromptSelected(prompt.id) }"
              @click="togglePrompt(prompt.id)"
            >
              <span class="mt-prompt-name">{{ prompt.name }}</span>
              <span class="mt-prompt-cat">{{ prompt.category }}<template v-if="prompt.judge"> · 评审</template></span>
              <span class="mt-prompt-actions">
                <i
                  class="mt-prompt-action"
                  title="编辑"
                  @click.stop="openPromptEditor(prompt)"
                  v-html="icons.code"
                />
                <i
                  class="mt-prompt-action is-danger"
                  title="删除"
                  @click.stop="deletePrompt(prompt)"
                  v-html="icons.close"
                />
              </span>
            </button>
            <button type="button" class="mt-prompt-card is-add" @click="openPromptEditor()">
              <span class="mt-prompt-name">＋ 自定义</span>
            </button>
          </div>
        </div>
      </div>

      <div class="mt-config-row is-params">
        <label class="mt-param">
          <span class="mt-param-label">并发数</span>
          <input v-model.number="mt.concurrency.value" type="number" min="1" max="16" class="mt-input" />
        </label>
        <label class="mt-param">
          <span class="mt-param-label">超时(秒)</span>
          <input v-model.number="mt.timeoutSeconds.value" type="number" min="10" max="600" class="mt-input" />
        </label>
        <label class="mt-param is-check">
          <input v-model="mt.enableJudge.value" type="checkbox" class="mt-checkbox" />
          <span class="mt-param-label">启用评审模型</span>
        </label>
        <div v-if="mt.enableJudge.value" class="mt-param is-judge">
          <select v-model="judgeModelValue" class="mt-input mt-select">
            <option value="">选择评审模型…</option>
            <option v-for="opt in judgeModelOptions" :key="opt.value" :value="opt.value">{{ opt.text }}</option>
          </select>
        </div>
        <div class="mt-run-actions">
          <button
            type="button"
            class="mt-run-btn"
            :disabled="!mt.canRun.value"
            @click="handleStartRun"
          >
            <span v-html="icons.play" /> 运行测试（{{ mt.totalTests.value }} 项）
          </button>
          <button
            v-if="mt.isRunning.value"
            type="button"
            class="mt-cancel-btn"
            @click="handleCancelRun"
          >
            取消
          </button>
        </div>
      </div>

      <!-- 进度条 -->
      <div v-if="mt.isRunning.value && mt.progress.value" class="mt-progress-bar">
        <div class="mt-progress-fill" :style="{ width: `${mt.progressPercent.value}%` }" />
        <span class="mt-progress-text">
          {{ mt.progress.value.completed }} / {{ mt.progress.value.total }}
        </span>
      </div>
    </header>

    <!-- ============ 主体三 Tab ============ -->
    <nav class="mt-tabs" role="tablist">
      <button
        type="button"
        class="mt-tab"
        :class="{ active: activeTab === 'matrix' }"
        role="tab"
        @click="activeTab = 'matrix'"
      >对比矩阵</button>
      <button
        type="button"
        class="mt-tab"
        :class="{ active: activeTab === 'detail' }"
        role="tab"
        @click="activeTab = 'detail'"
      >明细（{{ mt.currentResults.value.length }}）</button>
      <button
        type="button"
        class="mt-tab"
        :class="{ active: activeTab === 'history' }"
        role="tab"
        @click="activeTab = 'history'"
      >历史（{{ mt.historyRuns.value.length }}）</button>
    </nav>

    <section class="mt-body">
      <!-- —— Tab 1：对比矩阵 —— -->
      <div v-if="activeTab === 'matrix'" class="mt-matrix">
        <AppTable
          :rows="matrixRows"
          :columns="matrixColumns"
          :row-key="(row: MatrixRow) => `${row.channelId}::${row.model}`"
          :show-pagination="false"
          empty-text="尚无测试结果，选择目标后点击「运行测试」"
        >
          <template #cell-model="{ row }">
            <div class="mt-matrix-model">
              <span class="mt-matrix-channel">{{ row.channelName }}</span>
              <span class="mt-matrix-model-name">{{ row.model }}</span>
            </div>
          </template>
          <template #cell-summary="{ row }">
            <div class="mt-matrix-summary">
              <span class="mt-sum-score">{{ row.summary.avgScore != null ? row.summary.avgScore.toFixed(1) : "—" }}</span>
              <span class="mt-sum-item">{{ row.summary.okCount }}/{{ row.summary.total }} 成功</span>
              <span v-if="row.summary.avgDurationMs != null" class="mt-sum-item">{{ formatDurationShort(row.summary.avgDurationMs) }}</span>
              <span v-if="row.summary.avgTokensPerSec != null" class="mt-sum-item">{{ row.summary.avgTokensPerSec }} t/s</span>
            </div>
          </template>
          <template v-for="prompt in (mt.selectedPrompts.value.length ? mt.selectedPrompts.value : currentPromptList())" :key="prompt.id" #[`cell-${prompt.id}`]="{ row }">
            <div class="mt-matrix-cell" :class="scoreClass(row.cells[prompt.id])" :title="row.cells[prompt.id]?.error || row.cells[prompt.id]?.autoCheck?.detail || row.cells[prompt.id]?.judge?.reason || ''">
              <span class="mt-cell-score">{{ cellText(row.cells[prompt.id]) }}</span>
              <span v-if="row.cells[prompt.id]?.durationMs != null" class="mt-cell-dur">
                {{ formatDurationShort(row.cells[prompt.id]!.durationMs) }}
              </span>
            </div>
          </template>
        </AppTable>
      </div>

      <!-- —— Tab 2：明细 —— -->
      <div v-else-if="activeTab === 'detail'" class="mt-detail">
        <div v-if="mt.currentResults.value.length === 0" class="mt-body-empty">
          尚无明细数据
        </div>
        <AppTable
          v-else
          :rows="detailResults"
          :columns="resultColumns"
          :row-key="(row: ProbeResult) => `${row.channelId}::${row.model}::${row.promptId}`"
          :page-size="25"
          clickable
          empty-text="尚无明细数据"
          @select="openResultDetail"
        >
          <template #cell-ok="{ row }">
            <span class="mt-status" :class="row.ok ? 'is-ok' : 'is-fail'">{{ resultStatusText(row) }}</span>
          </template>
          <template #cell-score="{ row }">
            <span :class="scoreClass(row)">{{ row.score != null ? row.score.toFixed(1) : "—" }}</span>
          </template>
          <template #cell-durationMs="{ row }">
            {{ row.durationMs != null ? formatDurationShort(row.durationMs) : "—" }}
          </template>
          <template #cell-tokensPerSec="{ row }">
            {{ row.tokensPerSec != null ? row.tokensPerSec.toFixed(1) : "—" }}
          </template>
        </AppTable>
      </div>

      <!-- —— Tab 3：历史 —— -->
      <div v-else class="mt-history">
        <AppTable
          :rows="mt.historyRuns.value"
          :columns="historyColumns"
          :row-key="(row: TestRunRecord) => row.id"
          :page-size="25"
          empty-text="暂无历史记录"
        >
          <template #cell-status="{ row }">
            <span class="mt-status" :class="`is-${row.status}`">{{ historyStatusText(row) }}</span>
          </template>
          <template #cell-elapsed="{ row }">{{ historyElapsed(row) }}</template>
          <template #cell-actions="{ row }">
            <div class="mt-history-actions">
              <button type="button" class="mt-mini-btn" @click="loadRun(row)">载入</button>
              <button type="button" class="mt-mini-btn is-danger" @click="removeRun(row)">删除</button>
            </div>
          </template>
        </AppTable>
      </div>
    </section>

    <!-- ============ 明细结果详情弹窗 ============ -->
    <Teleport to="body">
      <div v-if="detailResult" class="mt-modal-backdrop" @click.self="closeResultDetail">
        <section class="mt-modal-card is-wide" role="dialog" aria-modal="true">
          <header class="mt-modal-header">
            <h2>{{ detailResult.channelName }} / {{ detailResult.model }} · {{ detailResult.promptName }}</h2>
            <button type="button" class="mt-modal-close" aria-label="关闭" @click="closeResultDetail">×</button>
          </header>
          <div class="mt-modal-body">
            <div class="mt-detail-meta">
              <span class="mt-detail-meta-item">
                <label>状态</label>
                <span class="mt-status" :class="detailResult.ok ? 'is-ok' : 'is-fail'">{{ resultStatusText(detailResult) }}</span>
              </span>
              <span v-if="detailResult.score != null" class="mt-detail-meta-item">
                <label>得分</label>
                <strong>{{ detailResult.score.toFixed(1) }}</strong>
              </span>
              <span v-if="detailResult.durationMs != null" class="mt-detail-meta-item">
                <label>耗时</label>
                <strong>{{ formatDurationShort(detailResult.durationMs) }}</strong>
              </span>
              <span v-if="detailResult.promptTokens != null" class="mt-detail-meta-item">
                <label>输入 tokens</label>
                <strong>{{ detailResult.promptTokens }}</strong>
              </span>
              <span v-if="detailResult.completionTokens != null" class="mt-detail-meta-item">
                <label>输出 tokens</label>
                <strong>{{ detailResult.completionTokens }}</strong>
              </span>
            </div>

            <div v-if="detailResult.error" class="mt-detail-section is-error">
              <h3>错误信息</h3>
              <pre class="mt-detail-pre">{{ detailResult.error }}</pre>
            </div>

            <div v-if="detailResult.autoCheck" class="mt-detail-section">
              <h3>自动判分（{{ detailResult.autoCheck.kind }}）</h3>
              <p :class="detailResult.autoCheck.passed ? 'is-ok-text' : 'is-fail-text'">
                {{ detailResult.autoCheck.passed ? "✓ 通过" : "✗ 未通过" }} · {{ detailResult.autoCheck.detail }}
              </p>
            </div>

            <div v-if="detailResult.judge" class="mt-detail-section">
              <h3>评审意见</h3>
              <p>
                <strong v-if="detailResult.judge.score != null">评审分：{{ detailResult.judge.score.toFixed(1) }}</strong>
              </p>
              <p class="mt-judge-reason">{{ detailResult.judge.reason }}</p>
            </div>

            <div v-if="detailResult.responseText" class="mt-detail-section">
              <h3>模型响应</h3>
              <pre class="mt-detail-pre">{{ detailResult.responseText }}</pre>
            </div>
          </div>
        </section>
      </div>
    </Teleport>

    <!-- ============ 自定义提示词编辑弹窗 ============ -->
    <Teleport to="body">
      <div v-if="promptEditorOpen" class="mt-modal-backdrop" @click.self="closePromptEditor">
        <section class="mt-modal-card" role="dialog" aria-modal="true">
          <header class="mt-modal-header">
            <h2>{{ editingPrompt ? "编辑自定义提示词" : "新建自定义提示词" }}</h2>
            <button type="button" class="mt-modal-close" aria-label="关闭" @click="closePromptEditor">×</button>
          </header>
          <div class="mt-modal-body">
            <div class="mt-form-grid">
              <label class="mt-form-field">
                <span class="mt-form-label">名称 *</span>
                <input v-model="promptForm.name" type="text" class="mt-input" placeholder="如：SQL 生成" />
              </label>
              <label class="mt-form-field">
                <span class="mt-form-label">类别</span>
                <input v-model="promptForm.category" type="text" class="mt-input" placeholder="如：代码" />
              </label>
              <label class="mt-form-field is-full">
                <span class="mt-form-label">提示词文本 *</span>
                <textarea v-model="promptForm.text" class="mt-input mt-textarea" rows="6" placeholder="输入要发给被测模型的提示词" />
              </label>
              <label class="mt-form-field">
                <span class="mt-form-label">最大输出 tokens</span>
                <input v-model.number="promptForm.maxTokens" type="number" min="16" max="8192" class="mt-input" />
              </label>
              <label class="mt-form-field">
                <span class="mt-form-label">客观判分</span>
                <select v-model="promptForm.checkKind" class="mt-input mt-select">
                  <option value="none">无（评审或仅记录）</option>
                  <option value="contains">包含关键词</option>
                  <option value="not_contains">不包含关键词</option>
                  <option value="number">数值匹配</option>
                  <option value="json">合法 JSON</option>
                </select>
              </label>
              <label v-if="promptForm.checkKind === 'contains' || promptForm.checkKind === 'not_contains'" class="mt-form-field is-full">
                <span class="mt-form-label">关键词（逗号分隔，任一命中即通过）</span>
                <input v-model="promptForm.checkValue" type="text" class="mt-input" placeholder="如：312211" />
              </label>
              <label v-if="promptForm.checkKind === 'number'" class="mt-form-field is-full">
                <span class="mt-form-label">期望数值</span>
                <input v-model="promptForm.checkValue" type="text" class="mt-input" placeholder="如：324" />
              </label>
              <label v-if="promptForm.checkKind === 'json'" class="mt-form-field is-full">
                <span class="mt-form-label">JSON 须包含的子串（可选）</span>
                <input v-model="promptForm.checkValue" type="text" class="mt-input" placeholder="留空仅校验合法性" />
              </label>
              <label class="mt-form-field is-check">
                <input v-model="promptForm.judge" type="checkbox" class="mt-checkbox" />
                <span class="mt-form-label">由评审模型打分</span>
              </label>
            </div>
          </div>
          <footer class="mt-modal-footer">
            <button type="button" class="mt-mini-btn" @click="closePromptEditor">取消</button>
            <button type="button" class="mt-run-btn" @click="savePrompt">保存</button>
          </footer>
        </section>
      </div>
    </Teleport>
  </main>
</template>

<style scoped>
/* ===================== 页面骨架 ===================== */
.mt-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
  height: 100%;
  overflow-y: auto;
  padding: 20px;
  background: var(--page-bg);
  color: var(--text);
}

/* ===================== 配置卡 ===================== */
.mt-config-card {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-xl, 14px);
  padding: 16px 18px;
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.mt-config-row {
  display: grid;
  grid-template-columns: minmax(280px, 1fr) minmax(320px, 1.4fr);
  gap: 18px;
}

.mt-config-row.is-params {
  grid-template-columns: auto auto auto minmax(200px, 1fr) auto;
  align-items: center;
  gap: 12px;
  padding-top: 12px;
  border-top: 1px solid var(--line);
}

.mt-config-block {
  display: flex;
  flex-direction: column;
  gap: 8px;
  min-width: 0;
}

.mt-block-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}

.mt-block-label {
  font-size: 12px;
  font-weight: 600;
  color: var(--muted);
  text-transform: uppercase;
  letter-spacing: 0.04em;
}

.mt-block-hint {
  font-size: 12px;
  color: var(--muted);
}

.mt-link-btn {
  background: none;
  border: none;
  color: var(--brand);
  font-size: 12px;
  cursor: pointer;
  padding: 2px 6px;
  border-radius: 4px;
}
.mt-link-btn:hover { background: var(--surface-hover); }

.mt-empty-targets {
  font-size: 13px;
  color: var(--muted);
  padding: 8px 0;
}

.mt-target-chips {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}

.mt-chip {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 3px 6px 3px 10px;
  border-radius: var(--r-full, 999px);
  background: color-mix(in srgb, var(--brand) 12%, transparent);
  color: var(--brand);
  font-size: 12px;
  max-width: 100%;
}

.mt-chip-x {
  background: none;
  border: none;
  color: inherit;
  cursor: pointer;
  font-size: 13px;
  line-height: 1;
  padding: 0 2px;
  opacity: 0.7;
}
.mt-chip-x:hover { opacity: 1; }

/* —— 目标选择器 —— */
.mt-target-picker {
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  max-height: 240px;
  overflow-y: auto;
  background: var(--page-bg);
}

.mt-picker-channel + .mt-picker-channel {
  border-top: 1px solid var(--line);
}

.mt-picker-channel-head {
  display: flex;
  align-items: center;
  gap: 6px;
  width: 100%;
  background: none;
  border: none;
  padding: 8px 10px;
  cursor: pointer;
  font-size: 13px;
  color: var(--text);
  text-align: left;
}
.mt-picker-channel-head:hover { background: var(--surface-hover); }

.mt-picker-arrow {
  display: inline-block;
  transition: transform 0.15s;
  color: var(--muted);
}
.mt-picker-arrow.open { transform: rotate(90deg); }

.mt-picker-channel-name { font-weight: 600; }

.mt-picker-channel-disabled {
  font-size: 11px;
  color: var(--muted);
  padding: 1px 6px;
  border-radius: 999px;
  background: var(--surface-hover);
}

.mt-picker-count {
  font-size: 11px;
  color: var(--brand);
}

.mt-picker-models {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  padding: 4px 10px 10px 26px;
}

.mt-model-option {
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--text);
  border-radius: var(--r-full, 999px);
  padding: 3px 10px;
  font-size: 12px;
  cursor: pointer;
}
.mt-model-option:hover { border-color: var(--brand); }
.mt-model-option.selected {
  background: color-mix(in srgb, var(--brand) 16%, transparent);
  border-color: var(--brand);
  color: var(--brand);
}
.mt-model-option.disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.mt-picker-empty {
  font-size: 12px;
  color: var(--muted);
  padding: 10px;
}

/* —— 套件卡片 —— */
.mt-prompt-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(120px, 1fr));
  gap: 8px;
}

.mt-prompt-card {
  position: relative;
  display: flex;
  flex-direction: column;
  gap: 3px;
  align-items: flex-start;
  padding: 8px 10px;
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  background: var(--surface);
  cursor: pointer;
  text-align: left;
  color: var(--text);
}
.mt-prompt-card:hover { border-color: var(--brand); }
.mt-prompt-card.selected {
  border-color: var(--brand);
  background: color-mix(in srgb, var(--brand) 10%, transparent);
}
.mt-prompt-card.is-add {
  align-items: center;
  justify-content: center;
  border-style: dashed;
  color: var(--muted);
}
.mt-prompt-card.is-add:hover { color: var(--brand); }

.mt-prompt-name {
  font-size: 12.5px;
  font-weight: 600;
}

.mt-prompt-cat {
  font-size: 11px;
  color: var(--muted);
}

.mt-prompt-actions {
  position: absolute;
  top: 4px;
  right: 4px;
  display: flex;
  gap: 2px;
  opacity: 0;
  transition: opacity 0.15s;
}
.mt-prompt-card:hover .mt-prompt-actions { opacity: 1; }

.mt-prompt-action {
  display: inline-flex;
  width: 18px;
  height: 18px;
  align-items: center;
  justify-content: center;
  border-radius: 4px;
  cursor: pointer;
  color: var(--muted);
}
.mt-prompt-action:hover { background: var(--surface-hover); color: var(--text); }
.mt-prompt-action.is-danger:hover { color: var(--danger, #e5534b); }
.mt-prompt-action :deep(svg) { width: 12px; height: 12px; }

/* —— 参数行 —— */
.mt-param {
  display: flex;
  align-items: center;
  gap: 6px;
}

.mt-param.is-check { gap: 4px; }

.mt-param-label {
  font-size: 12.5px;
  color: var(--muted);
  white-space: nowrap;
}

.mt-checkbox {
  accent-color: var(--brand);
}

.mt-input {
  background: var(--page-bg);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  color: var(--text);
  padding: 5px 8px;
  font-size: 13px;
  width: 72px;
}
.mt-input:focus { outline: none; border-color: var(--brand); }

.mt-select {
  width: 240px;
  cursor: pointer;
}

.mt-run-actions {
  display: flex;
  gap: 8px;
  margin-left: auto;
}

.mt-run-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  background: var(--brand);
  color: #fff;
  border: none;
  border-radius: var(--r-md, 8px);
  padding: 7px 16px;
  font-size: 13px;
  font-weight: 600;
  cursor: pointer;
}
.mt-run-btn:hover:not(:disabled) { filter: brightness(1.08); }
.mt-run-btn:disabled { opacity: 0.45; cursor: not-allowed; }
.mt-run-btn :deep(svg) { width: 13px; height: 13px; }

.mt-cancel-btn {
  background: var(--surface);
  border: 1px solid var(--line);
  color: var(--text);
  border-radius: var(--r-md, 8px);
  padding: 7px 14px;
  font-size: 13px;
  cursor: pointer;
}
.mt-cancel-btn:hover { border-color: var(--danger, #e5534b); color: var(--danger, #e5534b); }

/* —— 进度条 —— */
.mt-progress-bar {
  position: relative;
  height: 20px;
  border-radius: var(--r-full, 999px);
  background: var(--page-bg);
  border: 1px solid var(--line);
  overflow: hidden;
}

.mt-progress-fill {
  height: 100%;
  background: color-mix(in srgb, var(--brand) 70%, transparent);
  transition: width 0.3s ease;
}

.mt-progress-text {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 11.5px;
  color: var(--text);
  font-variant-numeric: tabular-nums;
}

/* ===================== Tab 导航 ===================== */
.mt-tabs {
  display: flex;
  gap: 4px;
  border-bottom: 1px solid var(--line);
}

.mt-tab {
  background: none;
  border: none;
  border-bottom: 2px solid transparent;
  color: var(--muted);
  padding: 8px 14px;
  font-size: 13.5px;
  cursor: pointer;
}
.mt-tab:hover { color: var(--text); }
.mt-tab.active {
  color: var(--brand);
  border-bottom-color: var(--brand);
  font-weight: 600;
}

.mt-body {
  flex: 1;
  min-height: 0;
}

.mt-body-empty {
  padding: 40px 0;
  text-align: center;
  color: var(--muted);
  font-size: 13px;
}

/* ===================== 矩阵 ===================== */
.mt-matrix-model {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.mt-matrix-channel {
  font-size: 11.5px;
  color: var(--muted);
}

.mt-matrix-model-name {
  font-size: 13px;
  font-weight: 600;
}

.mt-matrix-cell {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 1px;
  padding: 4px 6px;
  border-radius: var(--r-md, 8px);
}

.mt-cell-score {
  font-size: 13px;
  font-weight: 700;
  font-variant-numeric: tabular-nums;
}

.mt-cell-dur {
  font-size: 10.5px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
}

.mt-matrix-cell.is-great { background: color-mix(in srgb, #2da44e 14%, transparent); color: #1a7f37; }
.mt-matrix-cell.is-mid { background: color-mix(in srgb, #bf8700 16%, transparent); color: #9a6700; }
.mt-matrix-cell.is-low { background: color-mix(in srgb, #cf222e 12%, transparent); color: #cf222e; }
.mt-matrix-cell.is-error { background: color-mix(in srgb, #cf222e 16%, transparent); color: #cf222e; }
.mt-matrix-cell.is-pass { color: var(--brand); }

.mt-matrix-summary {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 2px;
}

.mt-sum-score {
  font-size: 15px;
  font-weight: 700;
  color: var(--brand);
  font-variant-numeric: tabular-nums;
}

.mt-sum-item {
  font-size: 10.5px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
}

/* ===================== 明细 / 历史 ===================== */
.mt-status {
  font-size: 12px;
  font-weight: 600;
}
.mt-status.is-ok { color: #1a7f37; }
.mt-status.is-fail, .mt-status.is-error { color: #cf222e; }
.mt-status.is-running { color: var(--brand); }
.mt-status.is-cancelled { color: #9a6700; }

.mt-history-actions {
  display: flex;
  gap: 6px;
  justify-content: center;
}

.mt-mini-btn {
  background: var(--surface);
  border: 1px solid var(--line);
  color: var(--text);
  border-radius: var(--r-md, 8px);
  padding: 3px 10px;
  font-size: 12px;
  cursor: pointer;
}
.mt-mini-btn:hover { border-color: var(--brand); color: var(--brand); }
.mt-mini-btn.is-danger:hover { border-color: var(--danger, #e5534b); color: var(--danger, #e5534b); }

/* ===================== 弹窗 ===================== */
.mt-modal-backdrop {
  position: fixed;
  inset: 0;
  z-index: 2000;
  background: rgba(0, 0, 0, 0.5);
  backdrop-filter: blur(4px);
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 20px;
}

.mt-modal-card {
  width: 100%;
  max-width: 620px;
  max-height: 85vh;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-xl, 14px);
  box-shadow: 0 20px 48px rgba(0, 0, 0, 0.25);
  display: flex;
  flex-direction: column;
}
.mt-modal-card.is-wide { max-width: 780px; }

.mt-modal-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 14px 18px;
  border-bottom: 1px solid var(--line);
}

.mt-modal-header h2 {
  font-size: 15px;
  margin: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.mt-modal-close {
  background: none;
  border: none;
  font-size: 20px;
  color: var(--muted);
  cursor: pointer;
  line-height: 1;
  padding: 2px 6px;
  border-radius: 4px;
}
.mt-modal-close:hover { background: var(--surface-hover); color: var(--text); }

.mt-modal-body {
  padding: 16px 18px;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.mt-modal-footer {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  padding: 12px 18px;
  border-top: 1px solid var(--line);
}

/* —— 结果详情 —— */
.mt-detail-meta {
  display: flex;
  flex-wrap: wrap;
  gap: 16px;
}

.mt-detail-meta-item {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.mt-detail-meta-item label {
  font-size: 11px;
  color: var(--muted);
}
.mt-detail-meta-item strong {
  font-size: 13.5px;
}

.mt-detail-section h3 {
  font-size: 12.5px;
  color: var(--muted);
  margin: 0 0 6px;
  text-transform: uppercase;
  letter-spacing: 0.04em;
}

.mt-detail-section p {
  margin: 0;
  font-size: 13px;
  line-height: 1.6;
}

.is-ok-text, .mt-status.is-finished { color: #1a7f37; }
.is-fail-text { color: #cf222e; }

.mt-judge-reason {
  color: var(--muted);
  margin-top: 4px !important;
  white-space: pre-wrap;
}

.mt-detail-pre {
  margin: 0;
  padding: 10px 12px;
  background: var(--page-bg);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  font-size: 12px;
  line-height: 1.55;
  white-space: pre-wrap;
  word-break: break-word;
  max-height: 300px;
  overflow-y: auto;
}

/* —— 提示词表单 —— */
.mt-form-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px;
}

.mt-form-field {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.mt-form-field.is-full { grid-column: 1 / -1; }
.mt-form-field.is-check { flex-direction: row; align-items: center; gap: 6px; }

.mt-form-label {
  font-size: 12px;
  color: var(--muted);
}

.mt-textarea {
  width: 100%;
  resize: vertical;
  min-height: 120px;
  font-family: inherit;
}

/* ===================== 响应式 ===================== */
@media (max-width: 1100px) {
  .mt-config-row {
    grid-template-columns: 1fr;
  }
  .mt-config-row.is-params {
    flex-wrap: wrap;
    display: flex;
  }
  .mt-form-grid {
    grid-template-columns: 1fr;
  }
}
</style>
