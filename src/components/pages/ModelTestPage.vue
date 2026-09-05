<script setup lang="ts">
import { computed, onMounted, ref, watchEffect } from "vue";
import { icons } from "../../icons";
import { useToast } from "../../composables/useToast";
import { useConfirm } from "../../composables/useConfirm";
import { useModelTest } from "../../composables/modeltest/useModelTest";
import AppTable, { type AppTableColumn } from "../common/AppTable.vue";
import CustomSelect from "../common/CustomSelect.vue";
import type {
  DetectionProbe,
  ProbeResult,
  TargetVerdict,
  TestRunRecord,
  VerdictKind,
} from "../../composables/modeltest/useModelTest";
import { formatDuration } from "../../utils";

const { showToast } = useToast();
const { confirm } = useConfirm();
const mt = useModelTest();

// —— 页面状态 ——
const activeTab = ref<"verdicts" | "compare" | "history">("verdicts");
const initialized = ref(false);

// —— 家族与结论的展示元数据 ——
const FAMILY_LABELS: Record<string, string> = {
  gpt: "OpenAI GPT",
  claude: "Anthropic Claude",
  gemini: "Google Gemini",
  deepseek: "DeepSeek",
  qwen: "阿里通义千问",
  kimi: "月之暗面 Kimi",
  glm: "智谱 GLM",
  doubao: "字节豆包",
  llama: "Meta LLaMA",
  mistral: "Mistral",
  ernie: "百度文心",
};

function familyLabel(family?: string): string {
  if (!family) return "—";
  return FAMILY_LABELS[family] || family;
}

const VERDICT_META: Record<VerdictKind, { label: string; cls: string }> = {
  ok: { label: "可信", cls: "is-ok" },
  suspicious: { label: "可疑", cls: "is-warn" },
  impersonation: { label: "疑似冒名", cls: "is-bad" },
  unreachable: { label: "不可达", cls: "is-bad" },
};

const CATEGORY_LABELS: Record<string, string> = {
  identity: "身份自述",
  fingerprint: "判别指纹",
  capability: "降智能力",
};

function categoryLabel(category: string): string {
  return CATEGORY_LABELS[category] || category;
}

// —— 目标选择（渠道分组的模型多选，弹窗内完成）——
const targetPickerOpen = ref(false);
const targetSearch = ref("");

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

// 搜索过滤：渠道名命中保留该渠道全部模型，否则按模型名过滤
const filteredTargetChannels = computed(() => {
  const q = targetSearch.value.trim().toLowerCase();
  return channelOptions.value
    .map((c) => {
      const models = modelsOfChannel(c.id);
      const channelHit = !!q && c.name.toLowerCase().includes(q);
      return { ...c, models: q && !channelHit ? models.filter((m) => m.toLowerCase().includes(q)) : models };
    })
    .filter((c) => c.models.length > 0);
});

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

function channelAllSelected(channelId: string, models: string[]): boolean {
  return models.length > 0 && models.every((m) => isTargetSelected(channelId, m));
}

function toggleChannelModels(channelId: string, models: string[], select: boolean) {
  if (!select) {
    mt.selectedTargets.value = mt.selectedTargets.value.filter(
      (t) => !(t.channelId === channelId && models.includes(t.model)),
    );
    return;
  }
  for (const model of models) {
    if (!isTargetSelected(channelId, model)) mt.selectedTargets.value.push({ channelId, model });
  }
}

function clearAllTargets() {
  mt.selectedTargets.value = [];
}

// —— 手动添加目标（模型缓存里没有的模型）——
const manualChannelId = ref("");
const manualModel = ref("");

const manualChannelOptions = computed(() => [
  { value: "", text: "选择渠道…" },
  ...channelOptions.value
    .filter((c) => c.enabled)
    .map((c) => ({ value: c.id, text: c.name })),
]);

function addManualTarget() {
  const model = manualModel.value.trim();
  if (!manualChannelId.value || !model) {
    showToast("请选择渠道并输入模型名", true);
    return;
  }
  if (isTargetSelected(manualChannelId.value, model)) {
    showToast("该目标已在列表中");
    return;
  }
  mt.selectedTargets.value.push({ channelId: manualChannelId.value, model });
  manualModel.value = "";
  showToast("已添加检测目标");
}

// —— 探测题勾选（弹窗内完成）——
const probePickerOpen = ref(false);

const probeGroups = computed(() => {
  const groups = new Map<string, DetectionProbe[]>();
  for (const p of mt.suites.value) {
    let bucket = groups.get(p.category);
    if (!bucket) {
      bucket = [];
      groups.set(p.category, bucket);
    }
    bucket.push(p);
  }
  return [...groups.entries()].map(([category, probes]) => ({ category, probes }));
});

function isProbeSelected(id: string): boolean {
  return mt.selectedProbeIds.value.has(id);
}

function toggleProbe(id: string) {
  const next = new Set(mt.selectedProbeIds.value);
  if (next.has(id)) next.delete(id);
  else next.add(id);
  mt.selectedProbeIds.value = next;
}

function toggleAllProbes(select: boolean) {
  mt.selectedProbeIds.value = select
    ? new Set(mt.suites.value.map((p) => p.id))
    : new Set();
}

// —— 运行 ——
// 禁用原因：按钮 disabled 时以 title 展示，避免用户猜测为何不可点
const canRunReason = computed(() => {
  if (mt.isRunning.value) return "检测进行中";
  if (mt.selectedTargets.value.length === 0) return "请先选择被测模型";
  if (mt.selectedProbeIds.value.size === 0) return "请先勾选探测题";
  return "";
});

async function handleStartRun() {
  if (!mt.canRun.value) return;
  try {
    await mt.startRun();
    activeTab.value = "verdicts";
    showToast(`验真检测已启动：${mt.totalRequests.value} 次请求`);
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

// —— 运行中的实时明细表 ——
const liveColumns: AppTableColumn[] = [
  { key: "target", title: "目标", width: "minmax(180px, 1fr)", sortable: false },
  { key: "probeName", title: "探测题", width: "150px", sortable: false },
  { key: "sample", title: "采样", width: "60px", align: "center", sortable: false },
  { key: "ok", title: "状态", width: "72px", align: "center", sortable: false },
  { key: "familyMatch", title: "家族命中", width: "120px", sortable: false },
  { key: "durationMs", title: "耗时", width: "90px", align: "right", sortable: false },
];

function formatDurationShort(ms?: number): string {
  if (ms == null) return "";
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

// —— 结论卡 ——
const expandedTargets = ref<Set<string>>(new Set());

function targetKey(v: { channelId: string; model: string }): string {
  return `${v.channelId}::${v.model}`;
}

function toggleExpand(key: string) {
  const next = new Set(expandedTargets.value);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  expandedTargets.value = next;
}

function capabilityRate(v: TargetVerdict): number | null {
  if (!v.capabilityTotal) return null;
  return v.capabilityPassed / v.capabilityTotal;
}

const verdictResultColumns: AppTableColumn[] = [
  { key: "category", title: "维度", width: "96px", sortable: false },
  { key: "probeName", title: "探测题", width: "minmax(140px, 1fr)", sortable: false },
  { key: "sample", title: "采样", width: "56px", align: "center", sortable: false },
  { key: "ok", title: "状态", width: "64px", align: "center", sortable: false },
  { key: "familyMatch", title: "家族命中", width: "120px", sortable: false },
  { key: "durationMs", title: "耗时", width: "84px", align: "right", sortable: false },
  { key: "excerpt", title: "回答摘要", width: "minmax(160px, 1.2fr)", sortable: false },
];

function excerpt(result: ProbeResult): string {
  const text = result.responseText || result.error || "";
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > 60 ? `${flat.slice(0, 60)}…` : flat;
}

// —— 结果明细弹窗 ——
const detailResult = ref<ProbeResult | null>(null);

function openResultDetail(result: ProbeResult) {
  detailResult.value = result;
}

function closeResultDetail() {
  detailResult.value = null;
}

// 任一弹窗打开时锁住页面滚动
watchEffect(() => {
  const anyOpen = targetPickerOpen.value || probePickerOpen.value || detailResult.value != null;
  document.body.classList.toggle("modal-open", anyOpen);
});

// —— 跨渠道对比 ——
interface CompareGroup {
  model: string;
  channels: TargetVerdict[];
  probes: DetectionProbe[];
}

const compareGroups = computed<CompareGroup[]>(() => {
  const byModel = new Map<string, TargetVerdict[]>();
  for (const v of mt.verdicts.value) {
    let bucket = byModel.get(v.model);
    if (!bucket) {
      bucket = [];
      byModel.set(v.model, bucket);
    }
    bucket.push(v);
  }
  const groups: CompareGroup[] = [];
  for (const [model, verdicts] of byModel.entries()) {
    if (verdicts.length < 2) continue;
    const ids = new Set<string>();
    for (const v of verdicts) {
      for (const r of v.results) {
        if (r.category === "identity" || r.category === "fingerprint") ids.add(r.probeId);
      }
    }
    const probes = mt.suites.value.filter((p) => ids.has(p.id));
    groups.push({ model, channels: verdicts, probes });
  }
  return groups;
});

function compareCell(
  group: CompareGroup,
  channelId: string,
  probeId: string,
): ProbeResult | undefined {
  const verdict = group.channels.find((v) => v.channelId === channelId);
  return verdict?.results.find((r) => r.probeId === probeId && r.sampleIndex === 0);
}

// 同一探测题下，与多数家族命中不一致的渠道标红
function compareOutliers(group: CompareGroup, probeId: string): Set<string> {
  const votes = new Map<string, number>();
  for (const v of group.channels) {
    const cell = compareCell(group, v.channelId, probeId);
    const family = cell?.familyMatch;
    if (family) votes.set(family, (votes.get(family) || 0) + 1);
  }
  if (votes.size <= 1) return new Set();
  let majority = "";
  let majorityCount = 0;
  for (const [family, count] of votes.entries()) {
    if (count > majorityCount) {
      majority = family;
      majorityCount = count;
    }
  }
  const outliers = new Set<string>();
  for (const v of group.channels) {
    const cell = compareCell(group, v.channelId, probeId);
    if (cell?.familyMatch && cell.familyMatch !== majority) outliers.add(v.channelId);
  }
  return outliers;
}

// —— 历史 ——
async function refreshHistory() {
  try {
    await mt.loadHistory();
  } catch (error) {
    showToast(`刷新历史失败：${String(error)}`, true);
  }
}

const historyColumns: AppTableColumn[] = [
  { key: "startedAt", title: "开始时间", width: "165px", sortable: false },
  { key: "targetCount", title: "目标数", width: "72px", align: "right", sortable: false },
  { key: "probeCount", title: "探测题", width: "72px", align: "right", sortable: false },
  { key: "repeats", title: "采样", width: "60px", align: "right", sortable: false },
  { key: "status", title: "状态", width: "88px", align: "center", sortable: false },
  { key: "distribution", title: "结论分布", width: "minmax(180px, 1fr)", sortable: false },
  { key: "elapsed", title: "耗时", width: "90px", align: "right", sortable: false },
  { key: "actions", title: "操作", width: "130px", align: "center", sortable: false },
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

function historyDistribution(run: TestRunRecord): string {
  const targets = run.summary?.targets;
  if (!targets || targets.length === 0) return "—";
  const counts = new Map<VerdictKind, number>();
  for (const v of targets) counts.set(v.verdict, (counts.get(v.verdict) || 0) + 1);
  return (Object.keys(VERDICT_META) as VerdictKind[])
    .filter((kind) => counts.get(kind))
    .map((kind) => `${counts.get(kind)} ${VERDICT_META[kind].label}`)
    .join(" · ");
}

async function loadRun(run: TestRunRecord) {
  try {
    await mt.loadRunResults(run.id);
    activeTab.value = "verdicts";
    showToast(`已载入第 ${run.id} 次检测结论`);
  } catch (error) {
    showToast(`载入失败：${String(error)}`, true);
  }
}

async function removeRun(run: TestRunRecord) {
  const accepted = await confirm({
    title: "删除检测记录",
    message: `确定删除第 ${run.id} 次检测及其全部结果吗？`,
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
    <!-- ============ 顶部工具条 ============ -->
    <header class="mt-toolbar">
      <div class="mt-toolbar-row">
        <button
          type="button"
          class="mt-picker-trigger"
          :class="{ 'is-set': mt.selectedTargets.value.length > 0 }"
          title="选择要验真的渠道与模型"
          @click="targetPickerOpen = true"
        >
          <span class="mt-picker-trigger-label">被测目标</span>
          <span class="mt-picker-trigger-count" :class="{ 'is-none': mt.selectedTargets.value.length === 0 }">
            {{ mt.selectedTargets.value.length === 0 ? "未选择" : `${mt.selectedTargets.value.length} 个模型` }}
          </span>
          <span class="mt-picker-trigger-caret" v-html="icons.chevron" />
        </button>

        <button
          type="button"
          class="mt-picker-trigger"
          :class="{ 'is-none': mt.selectedProbeIds.value.size === 0 }"
          title="勾选要运行的探测题"
          @click="probePickerOpen = true"
        >
          <span class="mt-picker-trigger-label">探测题</span>
          <span class="mt-picker-trigger-count" :class="{ 'is-none': mt.selectedProbeIds.value.size === 0 }">
            {{ mt.selectedProbeIds.value.size }} / {{ mt.suites.value.length }}
          </span>
          <span class="mt-picker-trigger-caret" v-html="icons.chevron" />
        </button>

        <div class="mt-toolbar-params">
          <label class="mt-param" title="一致性采样题的重复次数">
            <span class="mt-param-label">重复采样</span>
            <input v-model.number="mt.repeats.value" type="number" min="1" max="5" class="mt-input" />
          </label>
          <label class="mt-param" title="同时请求的并发数">
            <span class="mt-param-label">并发</span>
            <input v-model.number="mt.concurrency.value" type="number" min="1" max="16" class="mt-input" />
          </label>
          <label class="mt-param" title="单次请求超时时间">
            <span class="mt-param-label">超时(秒)</span>
            <input v-model.number="mt.timeoutSeconds.value" type="number" min="10" max="600" class="mt-input" />
          </label>
        </div>

        <div class="mt-run-actions">
          <button
            type="button"
            class="mt-run-btn"
            :disabled="!mt.canRun.value"
            :title="canRunReason || `共 ${mt.totalRequests.value} 次请求`"
            @click="handleStartRun"
          >
            <span v-html="icons.shield" /> 开始验真（{{ mt.totalRequests.value }} 次请求）
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
        :class="{ active: activeTab === 'verdicts' }"
        role="tab"
        @click="activeTab = 'verdicts'"
      >检测结果（{{ mt.verdicts.value.length }}）</button>
      <button
        type="button"
        class="mt-tab"
        :class="{ active: activeTab === 'compare' }"
        role="tab"
        @click="activeTab = 'compare'"
      >跨渠道对比（{{ compareGroups.length }}）</button>
      <button
        type="button"
        class="mt-tab"
        :class="{ active: activeTab === 'history' }"
        role="tab"
        @click="activeTab = 'history'"
      >历史（{{ mt.historyRuns.value.length }}）</button>
    </nav>

    <section class="mt-body">
      <!-- —— Tab 1：检测结果 —— -->
      <div v-if="activeTab === 'verdicts'" class="mt-verdicts">
        <!-- 运行中：实时明细 -->
        <template v-if="mt.isRunning.value">
          <div class="mt-live-head">检测进行中，实时结果：</div>
          <AppTable
            :rows="mt.liveResults.value"
            :columns="liveColumns"
            :row-key="(row: ProbeResult) => `${row.channelId}::${row.model}::${row.probeId}::${row.sampleIndex}`"
            :page-size="25"
            empty-text="等待首批结果…"
          >
            <template #cell-target="{ row }">
              <div class="mt-matrix-model">
                <span class="mt-matrix-channel">{{ row.channelName }}</span>
                <span class="mt-matrix-model-name">{{ row.model }}</span>
              </div>
            </template>
            <template #cell-sample="{ row }">#{{ row.sampleIndex + 1 }}</template>
            <template #cell-ok="{ row }">
              <span class="mt-status" :class="row.ok ? 'is-ok' : 'is-fail'">{{ row.ok ? "✓" : "✗" }}</span>
            </template>
            <template #cell-familyMatch="{ row }">
              <span v-if="row.familyMatch" class="mt-family-chip">{{ familyLabel(row.familyMatch) }}</span>
              <span v-else class="mt-muted">—</span>
            </template>
            <template #cell-durationMs="{ row }">
              {{ row.durationMs != null ? formatDurationShort(row.durationMs) : "—" }}
            </template>
          </AppTable>
        </template>

        <!-- 有结论：验真结论卡 -->
        <template v-else-if="mt.verdicts.value.length > 0">
          <div class="mt-verdict-summary">
            <span class="mt-sum-chip is-ok">可信 {{ mt.verdictCounts.value.ok }}</span>
            <span class="mt-sum-chip is-warn">可疑 {{ mt.verdictCounts.value.suspicious }}</span>
            <span class="mt-sum-chip is-bad">疑似冒名 {{ mt.verdictCounts.value.impersonation }}</span>
            <span class="mt-sum-chip is-bad">不可达 {{ mt.verdictCounts.value.unreachable }}</span>
          </div>

          <article
            v-for="verdict in mt.verdicts.value"
            :key="targetKey(verdict)"
            class="mt-verdict-card"
            :class="VERDICT_META[verdict.verdict]?.cls"
          >
            <header class="mt-verdict-head">
              <div class="mt-verdict-title">
                <span class="mt-matrix-channel">{{ verdict.channelName }}</span>
                <span class="mt-matrix-model-name">{{ verdict.model }}</span>
              </div>
              <span class="mt-verdict-badge" :class="VERDICT_META[verdict.verdict]?.cls">
                {{ VERDICT_META[verdict.verdict]?.label || verdict.verdict }}
              </span>
            </header>

            <div class="mt-verdict-facts">
              <span class="mt-fact">
                <label>标称家族</label>
                <strong>{{ familyLabel(verdict.claimedFamily) }}</strong>
              </span>
              <span class="mt-fact">
                <label>指纹检出</label>
                <strong :class="{ 'is-bad-text': verdict.detectedFamily && verdict.detectedFamily !== verdict.claimedFamily }">
                  {{ familyLabel(verdict.detectedFamily) }}
                </strong>
              </span>
              <span class="mt-fact">
                <label>自述家族</label>
                <strong :class="{ 'is-bad-text': verdict.identityConsistent === false }">
                  {{ familyLabel(verdict.identityFamily) }}
                </strong>
              </span>
              <span class="mt-fact">
                <label>能力题</label>
                <strong>{{ verdict.capabilityTotal ? `${verdict.capabilityPassed}/${verdict.capabilityTotal}` : "—" }}</strong>
              </span>
              <span class="mt-fact">
                <label>一致性</label>
                <strong>{{ verdict.consistencyRate != null ? `${Math.round(verdict.consistencyRate * 100)}%` : "—" }}</strong>
              </span>
              <span class="mt-fact">
                <label>平均耗时</label>
                <strong>{{ verdict.avgDurationMs != null ? formatDurationShort(verdict.avgDurationMs) : "—" }}</strong>
              </span>
              <span class="mt-fact">
                <label>成功/请求</label>
                <strong>{{ verdict.okCount }}/{{ verdict.totalRequests }}</strong>
              </span>
            </div>

            <div v-if="capabilityRate(verdict) != null" class="mt-cap-bar">
              <div
                class="mt-cap-fill"
                :class="{ 'is-low': (capabilityRate(verdict) as number) < 0.5 }"
                :style="{ width: `${Math.round((capabilityRate(verdict) as number) * 100)}%` }"
              />
            </div>

            <ul v-if="verdict.issues.length" class="mt-verdict-issues">
              <li v-for="(issue, index) in verdict.issues" :key="index">{{ issue }}</li>
            </ul>

            <footer class="mt-verdict-foot">
              <button type="button" class="mt-mini-btn" @click="toggleExpand(targetKey(verdict))">
                {{ expandedTargets.has(targetKey(verdict)) ? "收起明细" : `展开明细（${verdict.results.length}）` }}
              </button>
            </footer>

            <AppTable
              v-if="expandedTargets.has(targetKey(verdict))"
              class="mt-verdict-results"
              :rows="verdict.results"
              :columns="verdictResultColumns"
              :row-key="(row: ProbeResult) => `${row.probeId}::${row.sampleIndex}`"
              :page-size="15"
              clickable
              empty-text="无明细"
              @select="openResultDetail"
            >
              <template #cell-category="{ row }">{{ categoryLabel(row.category) }}</template>
              <template #cell-sample="{ row }">#{{ row.sampleIndex + 1 }}</template>
              <template #cell-ok="{ row }">
                <span class="mt-status" :class="row.ok ? 'is-ok' : 'is-fail'">{{ row.ok ? "✓" : "✗" }}</span>
              </template>
              <template #cell-familyMatch="{ row }">
                <span v-if="row.familyMatch" class="mt-family-chip">{{ familyLabel(row.familyMatch) }}</span>
                <span v-else class="mt-muted">—</span>
              </template>
              <template #cell-durationMs="{ row }">
                {{ row.durationMs != null ? formatDurationShort(row.durationMs) : "—" }}
              </template>
              <template #cell-excerpt="{ row }">
                <span class="mt-excerpt">{{ excerpt(row) }}</span>
              </template>
            </AppTable>
          </article>
        </template>

        <div v-else class="mt-body-empty">
          尚无检测结果：选择被测目标与探测题后点击「开始验真」
        </div>
      </div>

      <!-- —— Tab 2：跨渠道对比 —— -->
      <div v-else-if="activeTab === 'compare'" class="mt-compare">
        <div v-if="compareGroups.length === 0" class="mt-body-empty">
          同一模型名经多个渠道检测后，这里会并排对比各渠道的指纹答案
        </div>
        <section v-for="group in compareGroups" :key="group.model" class="mt-compare-group">
          <h3 class="mt-compare-title">{{ group.model }}</h3>
          <div class="mt-compare-scroll">
            <table class="mt-compare-table">
              <thead>
                <tr>
                  <th class="mt-compare-probe-col">探测题</th>
                  <th v-for="v in group.channels" :key="v.channelId">
                    {{ v.channelName }}
                    <span class="mt-verdict-badge is-tiny" :class="VERDICT_META[v.verdict]?.cls">
                      {{ VERDICT_META[v.verdict]?.label || v.verdict }}
                    </span>
                  </th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="probe in group.probes" :key="probe.id">
                  <td class="mt-compare-probe-col">
                    {{ probe.name }}
                    <span class="mt-compare-cat">{{ categoryLabel(probe.category) }}</span>
                  </td>
                  <td
                    v-for="v in group.channels"
                    :key="v.channelId"
                    :class="{ 'is-outlier': compareOutliers(group, probe.id).has(v.channelId) }"
                  >
                    <template v-if="compareCell(group, v.channelId, probe.id)">
                      <span
                        v-if="compareCell(group, v.channelId, probe.id)!.familyMatch"
                        class="mt-family-chip"
                      >{{ familyLabel(compareCell(group, v.channelId, probe.id)!.familyMatch) }}</span>
                      <div class="mt-excerpt">{{ excerpt(compareCell(group, v.channelId, probe.id)!) }}</div>
                    </template>
                    <span v-else class="mt-muted">—</span>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        </section>
      </div>

      <!-- —— Tab 3：历史 —— -->
      <div v-else class="mt-history">
        <div class="mt-detail-toolbar">
          <span class="mt-detail-count">{{ mt.historyRuns.value.length }} 次检测记录</span>
          <button type="button" class="mt-mini-btn" title="刷新历史记录" @click="refreshHistory">
            <span v-html="icons.restore" /> 刷新
          </button>
        </div>
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
          <template #cell-distribution="{ row }">{{ historyDistribution(row) }}</template>
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

    <!-- ============ 探测结果明细弹窗 ============ -->
    <Teleport to="body">
      <div v-if="detailResult" class="mt-modal-backdrop" @click.self="closeResultDetail">
        <section class="mt-modal-card is-wide" role="dialog" aria-modal="true">
          <header class="mt-modal-header">
            <h2>{{ detailResult.channelName }} / {{ detailResult.model }} · {{ detailResult.probeName }}</h2>
            <button type="button" class="mt-modal-close" aria-label="关闭" @click="closeResultDetail">×</button>
          </header>
          <div class="mt-modal-body">
            <div class="mt-detail-meta">
              <span class="mt-detail-meta-item">
                <label>状态</label>
                <span class="mt-status" :class="detailResult.ok ? 'is-ok' : 'is-fail'">
                  {{ detailResult.ok ? "✓ 通过" : "✗ 未通过" }}
                </span>
              </span>
              <span class="mt-detail-meta-item">
                <label>维度</label>
                <strong>{{ categoryLabel(detailResult.category) }}</strong>
              </span>
              <span v-if="detailResult.familyMatch" class="mt-detail-meta-item">
                <label>家族命中</label>
                <strong>{{ familyLabel(detailResult.familyMatch) }}</strong>
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

            <div v-if="detailResult.requestText" class="mt-detail-section">
              <h3>实际提问（随机变体 + 对话包装）</h3>
              <pre class="mt-detail-pre">{{ detailResult.requestText }}</pre>
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

            <div v-if="detailResult.responseText" class="mt-detail-section">
              <h3>模型响应</h3>
              <pre class="mt-detail-pre">{{ detailResult.responseText }}</pre>
            </div>
          </div>
        </section>
      </div>
    </Teleport>

    <!-- ============ 被测目标选择弹窗 ============ -->
    <Teleport to="body">
      <div v-if="targetPickerOpen" class="mt-modal-backdrop" @click.self="targetPickerOpen = false">
        <section class="mt-modal-card is-picker" role="dialog" aria-modal="true">
          <header class="mt-modal-header">
            <h2>选择被测模型</h2>
            <button type="button" class="mt-modal-close" aria-label="关闭" @click="targetPickerOpen = false">×</button>
          </header>
          <div class="mt-modal-body is-flush">
            <div class="mt-picker-search">
              <span class="mt-picker-search-icon" v-html="icons.search" />
              <input
                v-model="targetSearch"
                type="text"
                class="mt-input is-search"
                placeholder="搜索渠道或模型…"
              />
            </div>
            <div class="mt-picker-scroll">
              <div v-if="filteredTargetChannels.length === 0" class="mt-picker-empty">
                {{ channelOptions.length === 0 ? "没有可用渠道，请先在「模型反代」页添加并启用渠道" : "没有匹配的渠道或模型" }}
              </div>
              <div v-for="channel in filteredTargetChannels" :key="channel.id" class="mt-picker-channel">
                <div class="mt-picker-channel-head">
                  <span class="mt-picker-channel-name">{{ channel.name }}</span>
                  <span v-if="!channel.enabled" class="mt-picker-channel-disabled">未启用</span>
                  <span v-else class="mt-picker-channel-count">
                    {{ channelSelectedCount(channel.id) }} / {{ channel.models.length }}
                  </span>
                  <button
                    v-if="channel.enabled && channel.models.length"
                    type="button"
                    class="mt-mini-btn"
                    @click="toggleChannelModels(channel.id, channel.models, !channelAllSelected(channel.id, channel.models))"
                  >
                    {{ channelAllSelected(channel.id, channel.models) ? "清空" : "全选" }}
                  </button>
                </div>
                <div class="mt-picker-models">
                  <div v-if="channel.models.length === 0" class="mt-picker-empty is-inline">
                    暂无模型缓存，可在「模型反代」页拉取，或在下方手动输入模型名
                  </div>
                  <button
                    v-for="model in channel.models"
                    :key="model"
                    type="button"
                    class="mt-model-option"
                    :class="{ selected: isTargetSelected(channel.id, model), disabled: !channel.enabled }"
                    :disabled="!channel.enabled"
                    @click="toggleTarget(channel.id, model)"
                  >
                    <span class="mt-model-check" v-html="icons.check" />
                    {{ model }}
                  </button>
                </div>
              </div>
            </div>
            <!-- 手动添加：模型缓存里没有的模型 -->
            <div class="mt-manual-add">
              <span class="mt-param-label">手动添加</span>
              <div class="mt-manual-channel">
                <CustomSelect
                  :options="manualChannelOptions"
                  :model-value="manualChannelId"
                  aria-label="选择渠道"
                  searchable
                  @update:model-value="manualChannelId = String($event)"
                />
              </div>
              <input
                v-model="manualModel"
                type="text"
                class="mt-input is-model"
                placeholder="模型名，如 gpt-4o"
                @keyup.enter="addManualTarget"
              />
              <button type="button" class="mt-mini-btn" @click="addManualTarget">
                <span v-html="icons.plus" /> 添加
              </button>
            </div>
          </div>
          <footer class="mt-modal-footer is-split">
            <div class="mt-modal-footer-left">
              <span class="mt-modal-hint">已选 {{ mt.selectedTargets.value.length }} 个模型</span>
              <button
                v-if="mt.selectedTargets.value.length > 0"
                type="button"
                class="mt-mini-btn is-danger"
                @click="clearAllTargets"
              >清空</button>
            </div>
            <button type="button" class="mt-run-btn" @click="targetPickerOpen = false">完成</button>
          </footer>
        </section>
      </div>
    </Teleport>

    <!-- ============ 探测题选择弹窗 ============ -->
    <Teleport to="body">
      <div v-if="probePickerOpen" class="mt-modal-backdrop" @click.self="probePickerOpen = false">
        <section class="mt-modal-card is-picker" role="dialog" aria-modal="true">
          <header class="mt-modal-header">
            <h2>选择探测题</h2>
            <button type="button" class="mt-modal-close" aria-label="关闭" @click="probePickerOpen = false">×</button>
          </header>
          <div class="mt-modal-body is-flush">
            <div class="mt-picker-hint">
              所有探测题都会用随机闲聊对话包装、问法随机变换后发送（答案基准不变），降低被渠道识别为测试流量的风险。
            </div>
            <div class="mt-picker-scroll">
              <div v-for="group in probeGroups" :key="group.category" class="mt-prompt-group">
                <div class="mt-prompt-group-head">
                  <span class="mt-prompt-group-name">{{ categoryLabel(group.category) }}</span>
                  <span class="mt-prompt-group-count">
                    {{ group.probes.filter((p) => isProbeSelected(p.id)).length }} / {{ group.probes.length }}
                  </span>
                </div>
                <div class="mt-prompt-rows">
                  <button
                    v-for="probe in group.probes"
                    :key="probe.id"
                    type="button"
                    class="mt-prompt-row"
                    :class="{ selected: isProbeSelected(probe.id) }"
                    @click="toggleProbe(probe.id)"
                  >
                    <span class="mt-prompt-check" v-html="icons.check" />
                    <span class="mt-prompt-row-body">
                      <span class="mt-prompt-row-name">
                        {{ probe.name }}
                        <span v-if="probe.variants.length > 1" class="mt-prompt-repeat-badge">{{ probe.variants.length }} 种问法</span>
                        <span v-if="probe.repeats" class="mt-prompt-repeat-badge">采样×{{ mt.repeats.value }}</span>
                      </span>
                      <span class="mt-prompt-row-desc">{{ probe.description }}</span>
                    </span>
                  </button>
                </div>
              </div>
            </div>
          </div>
          <footer class="mt-modal-footer is-split">
            <div class="mt-modal-hint">已选 {{ mt.selectedProbeIds.value.size }} / {{ mt.suites.value.length }}</div>
            <div class="mt-modal-footer-actions">
              <button type="button" class="mt-mini-btn" @click="toggleAllProbes(mt.selectedProbeIds.value.size < mt.suites.value.length)">
                {{ mt.selectedProbeIds.value.size < mt.suites.value.length ? "全选" : "清空" }}
              </button>
              <button type="button" class="mt-run-btn" @click="probePickerOpen = false">完成</button>
            </div>
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

/* ===================== 顶部工具条 ===================== */
.mt-toolbar {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-xl, 14px);
  padding: 12px 16px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.mt-toolbar-row {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 10px;
}

.mt-toolbar-params {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 12px;
  padding: 0 4px;
  margin-left: 4px;
  border-left: 1px solid var(--line);
}

/* —— 弹窗选择器触发按钮 —— */
.mt-picker-trigger {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  background: var(--page-bg);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  padding: 7px 10px;
  font-size: 13px;
  color: var(--text);
  cursor: pointer;
  max-width: 320px;
}
.mt-picker-trigger:hover { border-color: var(--brand); }

.mt-picker-trigger-label {
  color: var(--muted);
  font-size: 12px;
  white-space: nowrap;
}

.mt-picker-trigger-count {
  font-weight: 600;
  font-size: 12.5px;
  color: var(--brand);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.mt-picker-trigger-count.is-none {
  color: var(--muted);
  font-weight: 400;
}

.mt-picker-trigger-caret {
  display: inline-flex;
  color: var(--muted);
}
.mt-picker-trigger-caret :deep(svg) { width: 13px; height: 13px; }

/* —— 参数行 —— */
.mt-param {
  display: flex;
  align-items: center;
  gap: 6px;
}

.mt-param-label {
  font-size: 12.5px;
  color: var(--muted);
  white-space: nowrap;
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

.mt-muted { color: var(--muted); }

/* ===================== 运行中实时明细 ===================== */
.mt-live-head {
  font-size: 12.5px;
  color: var(--muted);
  padding-bottom: 8px;
}

/* ===================== 结论卡 ===================== */
.mt-verdict-summary {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin-bottom: 12px;
}

.mt-sum-chip {
  font-size: 12px;
  font-weight: 600;
  border-radius: var(--r-full, 999px);
  padding: 3px 12px;
  border: 1px solid var(--line);
  background: var(--surface);
}
.mt-sum-chip.is-ok { color: #1a7f37; border-color: color-mix(in srgb, #2da44e 40%, transparent); }
.mt-sum-chip.is-warn { color: #9a6700; border-color: color-mix(in srgb, #bf8700 45%, transparent); }
.mt-sum-chip.is-bad { color: #cf222e; border-color: color-mix(in srgb, #cf222e 40%, transparent); }

.mt-verdict-card {
  background: var(--surface);
  border: 1px solid var(--line);
  border-left-width: 4px;
  border-radius: var(--r-xl, 14px);
  padding: 14px 16px;
  margin-bottom: 12px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.mt-verdict-card.is-ok { border-left-color: #2da44e; }
.mt-verdict-card.is-warn { border-left-color: #bf8700; }
.mt-verdict-card.is-bad { border-left-color: #cf222e; }

.mt-verdict-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

.mt-verdict-title {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.mt-matrix-channel {
  font-size: 11.5px;
  color: var(--muted);
}

.mt-matrix-model-name {
  font-size: 13.5px;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.mt-verdict-badge {
  flex: none;
  font-size: 12px;
  font-weight: 700;
  border-radius: var(--r-full, 999px);
  padding: 3px 12px;
}
.mt-verdict-badge.is-tiny { font-size: 10.5px; padding: 1px 8px; margin-left: 6px; }
.mt-verdict-badge.is-ok { background: color-mix(in srgb, #2da44e 14%, transparent); color: #1a7f37; }
.mt-verdict-badge.is-warn { background: color-mix(in srgb, #bf8700 16%, transparent); color: #9a6700; }
.mt-verdict-badge.is-bad { background: color-mix(in srgb, #cf222e 14%, transparent); color: #cf222e; }

.mt-verdict-facts {
  display: flex;
  flex-wrap: wrap;
  gap: 14px 20px;
}

.mt-fact {
  display: flex;
  flex-direction: column;
  gap: 1px;
}
.mt-fact label {
  font-size: 11px;
  color: var(--muted);
}
.mt-fact strong {
  font-size: 13px;
  font-weight: 600;
}
.is-bad-text { color: #cf222e !important; }

.mt-cap-bar {
  height: 6px;
  border-radius: 3px;
  background: var(--page-bg);
  border: 1px solid var(--line);
  overflow: hidden;
}
.mt-cap-fill {
  height: 100%;
  background: #2da44e;
  transition: width 0.3s ease;
}
.mt-cap-fill.is-low { background: #cf222e; }

.mt-verdict-issues {
  margin: 0;
  padding-left: 18px;
  display: flex;
  flex-direction: column;
  gap: 3px;
}
.mt-verdict-issues li {
  font-size: 12.5px;
  color: #9a6700;
  line-height: 1.5;
}
.mt-verdict-card.is-bad .mt-verdict-issues li { color: #cf222e; }

.mt-verdict-foot {
  display: flex;
  justify-content: flex-end;
}

.mt-verdict-results { margin-top: 4px; }

.mt-family-chip {
  display: inline-block;
  font-size: 11.5px;
  border: 1px solid color-mix(in srgb, var(--brand) 40%, transparent);
  color: var(--brand);
  border-radius: var(--r-full, 999px);
  padding: 1px 9px;
  white-space: nowrap;
}

.mt-excerpt {
  font-size: 12px;
  color: var(--muted);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  display: block;
  max-width: 320px;
}

/* ===================== 跨渠道对比 ===================== */
.mt-compare-group {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-xl, 14px);
  padding: 14px 16px;
  margin-bottom: 14px;
}

.mt-compare-title {
  margin: 0 0 10px;
  font-size: 14px;
}

.mt-compare-scroll { overflow-x: auto; }

.mt-compare-table {
  border-collapse: collapse;
  width: 100%;
  font-size: 12.5px;
}
.mt-compare-table th,
.mt-compare-table td {
  border: 1px solid var(--line);
  padding: 7px 10px;
  text-align: left;
  vertical-align: top;
  min-width: 180px;
}
.mt-compare-table th {
  background: var(--page-bg);
  font-weight: 600;
  white-space: nowrap;
}
.mt-compare-probe-col { min-width: 150px !important; }
.mt-compare-cat {
  display: block;
  font-size: 10.5px;
  color: var(--muted);
}
.mt-compare-table td.is-outlier {
  background: color-mix(in srgb, #cf222e 10%, transparent);
}

/* ===================== 历史 ===================== */
.mt-detail-toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding-bottom: 10px;
}

.mt-detail-count {
  font-size: 12px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
}

.mt-status {
  font-size: 12px;
  font-weight: 600;
}
.mt-status.is-ok { color: #1a7f37; }
.mt-status.is-fail, .mt-status.is-error { color: #cf222e; }
.mt-status.is-running { color: var(--brand); }
.mt-status.is-cancelled { color: #9a6700; }
.mt-status.is-finished { color: #1a7f37; }

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
.mt-mini-btn :deep(svg) { width: 12px; height: 12px; vertical-align: -1.5px; }

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
.mt-modal-card.is-picker { max-width: 680px; }

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

.mt-modal-body.is-flush {
  padding: 0;
  gap: 0;
}

.mt-modal-footer {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  padding: 12px 18px;
  border-top: 1px solid var(--line);
}

.mt-modal-footer.is-split {
  align-items: center;
  justify-content: space-between;
}

.mt-modal-footer-left {
  display: flex;
  align-items: center;
  gap: 10px;
}

.mt-modal-footer-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}

.mt-modal-hint {
  font-size: 12.5px;
  color: var(--muted);
}

/* —— 目标选择弹窗 —— */
.mt-picker-scroll {
  max-height: 48vh;
  overflow-y: auto;
  padding: 4px 18px 14px;
}

.mt-picker-search {
  position: relative;
  padding: 12px 18px;
  border-bottom: 1px solid var(--line);
}

.mt-picker-search-icon {
  position: absolute;
  left: 30px;
  top: 50%;
  transform: translateY(-58%);
  display: inline-flex;
  color: var(--muted);
  pointer-events: none;
}
.mt-picker-search-icon :deep(svg) { width: 14px; height: 14px; }

.mt-input.is-search {
  width: 100%;
  padding-left: 30px;
}

.mt-picker-channel + .mt-picker-channel {
  border-top: 1px solid var(--line);
}

.mt-picker-channel-head {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 0 6px;
}

.mt-picker-channel-name { font-weight: 600; font-size: 13px; }

.mt-picker-channel-disabled {
  font-size: 11px;
  color: var(--muted);
  padding: 1px 6px;
  border-radius: 999px;
  background: var(--surface-hover);
}

.mt-picker-channel-count {
  font-size: 12px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
}

.mt-picker-channel-head .mt-mini-btn {
  margin-left: auto;
  padding: 2px 10px;
  font-size: 11.5px;
}

.mt-picker-models {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  padding: 0 0 12px;
}

.mt-model-option {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--text);
  border-radius: var(--r-full, 999px);
  padding: 4px 11px;
  font-size: 12px;
  cursor: pointer;
}

.mt-model-check {
  display: none;
  color: var(--brand);
}
.mt-model-check :deep(svg) { width: 11px; height: 11px; }

.mt-model-option:hover { border-color: var(--brand); }
.mt-model-option.selected {
  background: color-mix(in srgb, var(--brand) 14%, transparent);
  border-color: var(--brand);
  color: var(--brand);
}
.mt-model-option.selected .mt-model-check { display: inline-flex; }
.mt-model-option.disabled {
  opacity: 0.45;
  cursor: not-allowed;
}

.mt-picker-empty {
  font-size: 12px;
  color: var(--muted);
  padding: 16px 0;
}
.mt-picker-empty.is-inline { padding: 4px 0 8px; }

/* —— 手动添加目标 —— */
.mt-manual-add {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 12px 18px;
  border-top: 1px solid var(--line);
  background: var(--page-bg);
}

.mt-manual-channel { width: 200px; flex: none; }

.mt-input.is-model {
  width: auto;
  flex: 1;
  min-width: 0;
}

/* —— 探测题弹窗 —— */
.mt-picker-hint {
  font-size: 12px;
  color: var(--muted);
  line-height: 1.5;
  padding: 10px 18px;
  border-bottom: 1px solid var(--line);
  background: var(--page-bg);
}

.mt-prompt-group + .mt-prompt-group {
  border-top: 1px solid var(--line);
}

.mt-prompt-group-head {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 0 6px;
}

.mt-prompt-group-name {
  font-size: 13px;
  font-weight: 600;
}

.mt-prompt-group-count {
  font-size: 12px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
}

.mt-prompt-rows {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding-bottom: 12px;
}

.mt-prompt-row {
  display: flex;
  align-items: flex-start;
  gap: 9px;
  border: 1px solid transparent;
  background: none;
  border-radius: var(--r-md, 8px);
  padding: 7px 10px;
  font-size: 13px;
  color: var(--text);
  cursor: pointer;
  text-align: left;
  width: 100%;
}
.mt-prompt-row:hover { background: var(--surface-hover); }
.mt-prompt-row.selected {
  background: color-mix(in srgb, var(--brand) 10%, transparent);
  border-color: color-mix(in srgb, var(--brand) 35%, transparent);
}

.mt-prompt-check {
  display: inline-flex;
  width: 16px;
  height: 16px;
  align-items: center;
  justify-content: center;
  border: 1.5px solid var(--line);
  border-radius: 5px;
  color: transparent;
  flex: none;
  margin-top: 2px;
  transition: background 0.12s, border-color 0.12s;
}
.mt-prompt-check :deep(svg) { width: 10px; height: 10px; }
.mt-prompt-row.selected .mt-prompt-check {
  background: var(--brand);
  border-color: var(--brand);
  color: #fff;
}

.mt-prompt-row-body {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.mt-prompt-row-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.mt-prompt-row-desc {
  font-size: 11.5px;
  color: var(--muted);
  line-height: 1.45;
}

.mt-prompt-repeat-badge {
  font-size: 10.5px;
  color: var(--brand);
  border: 1px solid color-mix(in srgb, var(--brand) 40%, transparent);
  border-radius: 999px;
  padding: 0 7px;
  line-height: 16px;
  margin-left: 6px;
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

.is-ok-text { color: #1a7f37; }
.is-fail-text { color: #cf222e; }

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

/* ===================== 响应式 ===================== */
@media (max-width: 1100px) {
  .mt-toolbar-params {
    border-left: none;
    padding-left: 0;
    margin-left: 0;
  }
  .mt-run-actions {
    margin-left: 0;
    width: 100%;
  }
}
</style>
