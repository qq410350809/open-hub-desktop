<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { listen, type UnlistenFn } from "../../composables/core/events";
import type { EChartsOption } from "../../echarts";
import EChart from "../common/EChart.vue";
import DateRangeDropdown from "../common/DateRangeDropdown.vue";
import AppTable, { type AppTableColumn } from "../common/AppTable.vue";
import CustomSelect from "../common/CustomSelect.vue";
import { icons } from "../../icons";
import { useStore } from "../../composables/useStore";
import { usePreferences } from "../../composables/usePreferences";
import { useProxyTokenStats } from "../../composables/proxy/useProxyTokenStats";
import { useModelProxy } from "../../composables/proxy/useModelProxy";
import { useConfirm } from "../../composables/ui/useConfirm";
import type {
  TokenModelMapping,
  TokenOfficialModel,
  InsightEvidence as InsightEvidenceItem,
} from "../../types";
import {
  bucketModelTotals,
  bucketSourceTotals,
  bucketTotals,
  buildDailyMapFromBuckets,
  cacheHitRateOf,
  bucketKeyFor,
  buildHealthTimeline,
  buildPrecedingKeys,
  buildTrendDetailFromBuckets,
  healthLevelOf,
  buildTrendFromBuckets,
  estimateRequestCount,
  mergeModelTotals,
  buildModelMappingLookup,
  formatCompact,
  formatRate,
  formatTokens,
  isKnownModel,
  isKnownSource,
  localDateOf,
  parseLocal,
  toLocalDate,
} from "../../composables/tokenStatsAgg";
import type { TrendGranularity } from "../../composables/tokenStatsAgg";
import { isTauri, localTokenStatsAvailable, runLocalCommand } from "../../composables/useLibrary";

// 页面形态由路由决定：「本地统计」扫描本机 AI 工具日志（客户端能力），
// 「网关统计」读取反代网关记账（服务端能力）；同一套聚合视图按 mode 渲染。
const props = defineProps<{
  /** 统计数据来源：local = 本地终端日志采集；proxy = 反代网关聚合 */
  mode: "local" | "proxy";
}>();

const store = useStore();
const { preferences } = usePreferences();

// 全局日期范围（useTokenStats 持有）：以 computed 代理给 DateRangeDropdown 的 v-model
const rangeFrom = computed({
  get: () => store.tokenStatsFrom.value,
  set: (v: string) => {
    store.tokenStatsFrom.value = v;
  },
});
const rangeTo = computed({
  get: () => store.tokenStatsTo.value,
  set: (v: string) => {
    store.tokenStatsTo.value = v;
  },
});

// —— 数据模式：本地日志采集 / 反代网关聚合 ——
// 两种模式共用同一套聚合层与视图；mode 由父级路由（菜单）固定传入
const statsMode = computed(() => props.mode);
const proxyStore = useProxyTokenStats();
const activeUsage = computed(
  () =>
    (statsMode.value === "local"
      ? store.tokenUsage.value
      : proxyStore.proxyTokenReport.value?.usage) ?? null,
);
const activeHealth = computed(
  () =>
    (statsMode.value === "local"
      ? store.requestHealth.value
      : proxyStore.proxyTokenReport.value?.health) ?? null,
);
const activeLoading = computed(() =>
  statsMode.value === "local" ? store.tokenUsageLoading.value : proxyStore.proxyTokenLoading.value,
);
const activeError = computed(() =>
  statsMode.value === "local" ? store.tokenUsageError.value : proxyStore.proxyTokenError.value,
);

// 反代模式：时间范围变化即重新拉取（本地模式由 useTokenStats 自身处理）
watch(
  [() => store.tokenStatsFrom.value, () => store.tokenStatsTo.value],
  ([from, to]) => {
    if (statsMode.value === "proxy") {
      void proxyStore.loadProxyTokenUsage(from, to);
    }
  },
);

// —— 4 大深度分析弹窗状态 ——
const toolsModalOpen = ref(false);
const modelsModalOpen = ref(false);
const projectsModalOpen = ref(false);
const auditModalOpen = ref(false);
const healthModalOpen = ref(false);

// —— 趋势图表显示维度 ——
type TrendMetric = "total" | "breakdown" | "reasoning" | "requests";
const trendMetric = ref<TrendMetric>("total");

// —— 搜索与过滤 ——
const modelSearch = ref("");
const projectSearch = ref("");
const sourceSearch = ref("");

// —— 趋势粒度：根据顶部所选时间区间自动决定 X 轴粒度 ——
const trendGranularity = computed<TrendGranularity>(() => {
  const from = store.tokenStatsFrom.value;
  const to = store.tokenStatsTo.value;
  if (from && to) {
    const days = Math.round((parseLocal(to).getTime() - parseLocal(from).getTime()) / 86_400_000) + 1;
    if (days < 7) return "hour";
    if (days <= 92) return "day";
    return "month";
  }
  return "day";
});

function trendUnitLabel(): string {
  switch (trendGranularity.value) {
    case "hour": return "逐小时";
    case "day": return "逐日";
    case "month": return "逐月";
    default: return "逐日";
  }
}

// —— 统计重建状态与阶段日志 ——
type RefreshPhase = "confirm" | "running" | "success" | "error";
type RefreshLogStatus = "running" | "success" | "error";
type TokenCollectorProgress = {
  stage: string;
  status: RefreshLogStatus;
  message: string;
};
type RefreshLogEntry = TokenCollectorProgress & {
  id: number;
  time: string;
};

const refreshDialogOpen = ref(false);
const refreshPhase = ref<RefreshPhase>("confirm");
const refreshLogs = ref<RefreshLogEntry[]>([]);
const refreshLogListRef = ref<HTMLOListElement>();
let refreshLogId = 0;
let unlistenTokenCollectorProgress: UnlistenFn | undefined;
let tokenStatsPageMounted = true;

const refreshStageLabels: Record<string, string> = {
  prepare: "准备",
  cache: "缓存",
  scan: "扫描",
  aggregate: "汇总",
  database: "数据库",
  view: "页面",
  complete: "完成",
  error: "错误",
};

const refreshStatusTitle = computed(() => {
  if (refreshPhase.value === "running") return "正在重建 Token 统计";
  if (refreshPhase.value === "success") return "统计重建完成";
  if (refreshPhase.value === "error") return "统计重建失败";
  return "重建 Token 统计";
});

const refreshStatusDescription = computed(() => {
  if (refreshPhase.value === "running") return "正在重新读取多端日志并重建本地数据库，请稍候。";
  if (refreshPhase.value === "success") return "本地数据库与当前页面快照均已更新。";
  if (refreshPhase.value === "error") return "任务未能完成，请根据下方日志检查后重试。";
  return "清除本地临时解析缓存，重新完整扫描各 AI 工具的本地日志并写入 SQLite。";
});

function appendRefreshLog(progress: TokenCollectorProgress) {
  const last = refreshLogs.value[refreshLogs.value.length - 1];
  if (last?.stage === progress.stage && last.status === progress.status && last.message === progress.message) {
    return;
  }
  refreshLogs.value.push({
    ...progress,
    id: ++refreshLogId,
    time: new Date().toLocaleTimeString("zh-CN", {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
      hour12: false,
    }),
  });
}

function openRefreshDialog() {
  refreshDialogOpen.value = true;
  if (store.tokenCollectorSyncing.value) {
    refreshPhase.value = "running";
    return;
  }
  refreshPhase.value = "confirm";
  refreshLogs.value = [];
}

function closeRefreshDialog() {
  refreshDialogOpen.value = false;
}

async function startRefresh() {
  if (store.tokenCollectorSyncing.value) return;
  refreshPhase.value = "running";
  refreshLogs.value = [];
  appendRefreshLog({ stage: "prepare", status: "running", message: "统计重建请求已提交" });

  try {
    await store.syncTokenCollector(true);
  } catch (error) {
    appendRefreshLog({
      stage: "error",
      status: "error",
      message: String(error),
    });
    refreshPhase.value = "error";
    return;
  }

  appendRefreshLog({ stage: "view", status: "running", message: "正在重新读取数据库快照并更新页面" });
  try {
    await store.refreshTokenDatabaseView(true);
    appendRefreshLog({ stage: "view", status: "success", message: "页面快照已更新" });
    appendRefreshLog({ stage: "complete", status: "success", message: "统计重建全部完成" });
    refreshPhase.value = "success";
  } catch (error) {
    appendRefreshLog({
      stage: "view",
      status: "error",
      message: `页面更新失败：${String(error)}`,
    });
    refreshPhase.value = "error";
  }
}

// —— 模型映射弹窗（AI 分析：原始模型名 → 正式模型） ——
const mappingDialogOpen = ref(false);
const mappingChannelId = ref("");
const mappingModel = ref("");
const mappingCatalogGroups = ref<{ label: string; options: { value: string; text: string }[] }[]>([]);
const mappingCatalogLoading = ref(false);
const mappingCatalogError = ref("");
const mappingInitializing = ref(false);
const mappingInitializationError = ref("");
const mappingProxy = useModelProxy();
const { confirm: confirmMappingForce } = useConfirm();

const mappingOriginLabels: Record<string, string> = {
  rule: "规则",
  ai: "AI 建议",
  manual: "手工",
};
const mappingReviewLabels: Record<TokenModelMapping["reviewStatus"], string> = {
  pending: "待识别",
  suggested: "待审核",
  approved: "已生效",
  rejected: "已驳回",
};

// —— 映射管理：标签切换「原始模型列表 / 转换后模型列表」 ——
const MAPPING_CUSTOM = "__custom__";
type MappingViewMode = "raw" | "converted";
type MappingRowFilter = "all" | "unmapped" | "mapped";
type MappingSortKey = "name" | "tokens" | "status";
const mappingView = ref<MappingViewMode>("raw");
const mappingRawSearch = ref("");
const mappingRawFilter = ref<MappingRowFilter>("all");
const mappingSortKey = ref<MappingSortKey>("tokens");
const mappingRawSelected = ref<string[]>([]);
const mappingTargetSearch = ref("");
const mappingBatchTarget = ref("");
const mappingBatchCustom = ref("");
const mappingBatchSaving = ref(false);
/** 行内「自定义…」编辑中的行（rawKey）。 */
const mappingEditingKey = ref("");
const mappingEditingValue = ref("");
/** 视图二：展开来源明细的目标模型。 */
const mappingExpandedTarget = ref("");
const mappingNewOfficial = ref("");
const mappingAddingOfficial = ref(false);
/** 正在重命名/合并的转换后目标模型。 */
const mappingRenamingTarget = ref("");
const mappingRenameValue = ref("");
const mappingRenaming = ref(false);

/** 映射目标下拉通用选项：未映射 / 自定义…；正式模型清单通过 groups 分组注入。 */
const mappingTargetSelectOptions = [
  { value: "", text: "未映射" },
  { value: MAPPING_CUSTOM, text: "自定义…" },
];

/** 原始模型 → 当前用量（累计 Token），未映射的高用量模型优先处理。 */
const mappingUsageByModel = computed(() => {
  const totals = new Map<string, number>();
  for (const item of bucketModelTotals(allBuckets.value)) {
    totals.set(item.model, item.totalTokens);
  }
  return totals;
});

function mappingUsageOf(rawModel: string): number {
  return mappingUsageByModel.value.get(rawModel) ?? 0;
}

/** 视图一：原始模型行（筛选 + 排序 + 搜索），选择模式 = 多选。 */
const mappingRawRows = computed(() => {
  const q = mappingRawSearch.value.trim().toLowerCase();
  const filter = mappingRawFilter.value;
  let rows = store.tokenModelMappings.value;
  if (filter === "unmapped") rows = rows.filter((row) => row.reviewStatus !== "approved");
  else if (filter === "mapped") rows = rows.filter((row) => row.reviewStatus === "approved");
  if (q) rows = rows.filter((row) => row.rawModel.toLowerCase().includes(q));
  const usage = mappingUsageByModel.value;
  const statusRank = (row: TokenModelMapping) => {
    const ranks: Record<TokenModelMapping["reviewStatus"], number> = {
      suggested: 0,
      pending: 1,
      rejected: 2,
      approved: 3,
    };
    return ranks[row.reviewStatus] ?? 4;
  };
  return rows.slice().sort((a, b) => {
    if (mappingSortKey.value === "name") {
      return a.rawModel.localeCompare(b.rawModel, undefined, { numeric: true });
    }
    if (mappingSortKey.value === "status") {
      return statusRank(a) - statusRank(b) ||
        a.rawModel.localeCompare(b.rawModel, undefined, { numeric: true });
    }
    return (usage.get(b.rawModel) ?? 0) - (usage.get(a.rawModel) ?? 0);
  });
});

const mappingUnmappedRows = computed(() =>
  store.tokenModelMappings.value.filter((row) => row.reviewStatus !== "approved"),
);
const mappingPendingCount = computed(() =>
  store.tokenModelMappings.value.filter((row) => row.reviewStatus === "pending" || row.reviewStatus === "rejected").length,
);
const mappingSuggestedCount = computed(() =>
  store.tokenModelMappings.value.filter((row) => row.reviewStatus === "suggested").length,
);
const mappingApprovedCount = computed(() =>
  store.tokenModelMappings.value.filter((row) => row.reviewStatus === "approved").length,
);

const mappingCoverage = computed(() => {
  const total = store.tokenModelMappings.value.length;
  const mapped = mappingApprovedCount.value;
  return { total, mapped, unmapped: total - mapped, percent: total ? Math.round((mapped / total) * 100) : 100 };
});

function setMappingFilter(filter: MappingRowFilter) {
  mappingRawFilter.value = filter;
  ensureMappingCatalog();
}

const mappingRawAllSelected = computed(
  () => mappingRawRows.value.length > 0 &&
    mappingRawRows.value.every((row) => mappingRawSelected.value.includes(row.rawModel)),
);

function toggleMappingRawAll(event: Event) {
  const checked = (event.target as HTMLInputElement).checked;
  mappingRawSelected.value = checked
    ? [...new Set([...mappingRawSelected.value, ...mappingRawRows.value.map((row) => row.rawModel)])]
    : mappingRawSelected.value.filter(
        (name) => !mappingRawRows.value.some((row) => row.rawModel === name),
      );
}

/** 一键选中当前筛选下所有未映射行（配合批量映射快速收口）。 */
function selectAllUnmapped() {
  mappingRawFilter.value = "unmapped";
  mappingRawSelected.value = [
    ...new Set([...mappingRawSelected.value, ...mappingUnmappedRows.value.map((row) => row.rawModel)]),
  ];
}

/** 单行保存映射；officialModel 传空串表示清除。 */
async function saveMappingOfficial(row: TokenModelMapping, officialModel: string): Promise<boolean> {
  if (officialModel === row.officialModel) return true;
  const ok = await store.setTokenModelMapping(row.rawModel, officialModel);
  if (ok) void store.refreshTokenDatabaseView(false);
  else store.showToast("映射保存失败", true);
  return ok;
}

/** 行内下拉选择：未映射直接清除；自定义… 进入行内输入；其余按所选正式模型保存。 */
async function onRowTargetChange(row: TokenModelMapping, value: string) {
  if (value === MAPPING_CUSTOM) {
    mappingEditingKey.value = row.rawKey;
    mappingEditingValue.value = row.officialModel;
    return;
  }
  await saveMappingOfficial(row, value);
}

async function confirmRowCustom(row: TokenModelMapping) {
  const value = mappingEditingValue.value.trim();
  if (!value) {
    store.showToast("请输入自定义模型 ID", true);
    return;
  }
  if (await saveMappingOfficial(row, value)) {
    mappingEditingKey.value = "";
    mappingEditingValue.value = "";
  }
}

function cancelRowCustom() {
  mappingEditingKey.value = "";
  mappingEditingValue.value = "";
}

const mappingBatchReady = computed(() => {
  if (mappingBatchSaving.value || mappingRawSelected.value.length === 0) return false;
  if (mappingBatchTarget.value === MAPPING_CUSTOM) return Boolean(mappingBatchCustom.value.trim());
  return Boolean(mappingBatchTarget.value);
});

/** 批量映射：勾选的原始模型统一映射到所选目标（含自定义输入，自动注册为 user 来源）。 */
async function applyMappingBatch(clear = false) {
  if (mappingBatchSaving.value) return;
  const rows = mappingRawSelected.value;
  if (rows.length === 0) return;
  const target = clear ? "" : (mappingBatchTarget.value === MAPPING_CUSTOM
    ? mappingBatchCustom.value.trim()
    : mappingBatchTarget.value);
  if (!clear && !target) return;
  mappingBatchSaving.value = true;
  let succeeded = 0;
  for (const rawModel of rows) {
    if (await store.setTokenModelMapping(rawModel, target)) succeeded += 1;
  }
  mappingBatchSaving.value = false;
  mappingRawSelected.value = [];
  if (clear) {
    store.showToast(`已清除 ${succeeded}/${rows.length} 条映射`, succeeded < rows.length);
  } else {
    store.showToast(`已映射 ${succeeded}/${rows.length} 条到 ${target}`, succeeded < rows.length);
  }
  if (succeeded) void store.refreshTokenDatabaseView(false);
}

/** 视图二：转换后模型分组（目标名 → 来源原始模型行），选择模式 = 单选展开管理。 */
const mappingConvertedGroups = computed(() => {
  const groups = new Map<string, TokenModelMapping[]>();
  for (const row of store.tokenModelMappings.value) {
    const target = row.officialModel.trim();
    if (!target || row.reviewStatus !== "approved") continue;
    let bucket = groups.get(target);
    if (!bucket) {
      bucket = [];
      groups.set(target, bucket);
    }
    bucket.push(row);
  }
  const usage = mappingUsageByModel.value;
  return [...groups.entries()]
    .map(([name, sources]) => ({
      name,
      sources: sources.sort((a, b) =>
        a.rawModel.localeCompare(b.rawModel, undefined, { numeric: true }),
      ),
      tokens: sources.reduce((sum, row) => sum + (usage.get(row.rawModel) ?? 0), 0),
    }))
    .sort((a, b) => b.tokens - a.tokens || a.name.localeCompare(b.name, undefined, { numeric: true }));
});

const mappingConvertedFiltered = computed(() => {
  const q = mappingTargetSearch.value.trim().toLowerCase();
  if (!q) return mappingConvertedGroups.value;
  return mappingConvertedGroups.value.filter((group) =>
    group.name.toLowerCase().includes(q) ||
    group.sources.some((row) => row.rawModel.toLowerCase().includes(q)),
  );
});

function toggleExpandedTarget(name: string) {
  mappingExpandedTarget.value = mappingExpandedTarget.value === name ? "" : name;
}

/** 移除单个来源：把该原始模型的映射清空（回到未映射）。 */
async function removeMappingSource(row: TokenModelMapping) {
  if (await store.setTokenModelMapping(row.rawModel, "")) {
    store.showToast(`已移除 ${row.rawModel} 的映射`);
    void store.refreshTokenDatabaseView(false);
  }
}

/** 清除某个目标下的全部映射。 */
async function clearTargetMappings(name: string) {
  const group = mappingConvertedGroups.value.find((item) => item.name === name);
  if (!group) return;
  const ok = await confirmMappingForce({
    title: "清除映射",
    message: `将把 ${group.sources.length} 个原始模型（${group.sources.map((r) => r.rawModel).slice(0, 5).join("、")}${group.sources.length > 5 ? " 等" : ""}）的映射全部清除，统计将按原始模型名归组。确定继续？`,
    confirmText: "清除",
    danger: true,
  });
  if (!ok) return;
  let succeeded = 0;
  for (const row of group.sources) {
    if (await store.setTokenModelMapping(row.rawModel, "")) succeeded += 1;
  }
  store.showToast(`已清除 ${succeeded}/${group.sources.length} 条映射`, succeeded < group.sources.length);
  if (succeeded) void store.refreshTokenDatabaseView(false);
}

/** 重命名目标：把该目标下全部来源改映射到新名字（可用于合并重复目标）。 */
function startRenameTarget(name: string) {
  mappingRenamingTarget.value = name;
  mappingRenameValue.value = name;
}

function cancelRenameTarget() {
  mappingRenamingTarget.value = "";
  mappingRenameValue.value = "";
}

async function confirmRenameTarget(name: string) {
  const value = mappingRenameValue.value.trim();
  if (!value || value === name) {
    cancelRenameTarget();
    return;
  }
  const group = mappingConvertedGroups.value.find((item) => item.name === name);
  if (!group) return;
  mappingRenaming.value = true;
  let succeeded = 0;
  for (const row of group.sources) {
    if (await store.setTokenModelMapping(row.rawModel, value)) succeeded += 1;
  }
  mappingRenaming.value = false;
  mappingRenamingTarget.value = "";
  if (succeeded === group.sources.length) {
    store.showToast(`已将 ${succeeded} 条来源重命名为 ${value}`);
  } else {
    store.showToast(`重命名完成 ${succeeded}/${group.sources.length} 条`, true);
  }
  if (succeeded) void store.refreshTokenDatabaseView(false);
}

/** 删除自定义/AI 来源的正式模型；仅当该模型没有被任何映射引用时允许。 */
async function deleteOfficialModel(name: string) {
  const group = mappingConvertedGroups.value.find((item) => item.name === name);
  if (group) {
    store.showToast("该模型仍有映射引用，请先清除或改映射后再删除", true);
    return;
  }
  const ok = await confirmMappingForce({
    title: "删除自定义模型",
    message: `确定从正式模型清单中删除「${name}」吗？`,
    confirmText: "删除",
    danger: true,
  });
  if (!ok) return;
  const error = await store.removeTokenOfficialModel(name);
  if (error) {
    store.showToast(error, true);
    return;
  }
  store.showToast(`已删除 ${name}`);
  mappingCatalogGroups.value = [];
  void ensureMappingCatalog(true);
}

/** 添加自定义正式模型（source=user），保存后刷新候选清单。 */
async function addMappingOfficialModel() {
  const name = mappingNewOfficial.value.trim();
  if (!name || mappingAddingOfficial.value) return;
  mappingAddingOfficial.value = true;
  const ok = await store.addTokenOfficialModel(name);
  mappingAddingOfficial.value = false;
  if (ok) {
    mappingNewOfficial.value = "";
    store.showToast(`已添加自定义模型 ${name}`);
    mappingCatalogGroups.value = [];
    void ensureMappingCatalog(true);
  } else {
    store.showToast("添加自定义模型失败", true);
  }
}

// 渠道下拉：仅列出已启用的反代渠道；分析请求必须指定渠道
const mappingChannelOptions = computed(() => {
  const channels = mappingProxy.proxyConfig.value.channels.filter((channel) => channel.enabled);
  return channels.map((channel) => ({ value: channel.id, text: channel.name || channel.id }));
});

// 无渠道可选时禁用分析；有渠道但尚未选中时默认取第一个
const mappingHasChannels = computed(() => mappingChannelOptions.value.length > 0);
watch(mappingChannelOptions, (options) => {
  if (!options.some((opt) => opt.value === mappingChannelId.value)) {
    mappingChannelId.value = options[0]?.value ?? "";
  }
}, { immediate: true });

// 分析模型候选：渠道的原始模型列表（不受「管理可用模型」白名单限制），支持自由输入任意模型 ID
const mappingModelSuggestions = computed(() => {
  const channel = mappingProxy.proxyConfig.value.channels.find(
    (item) => item.id === mappingChannelId.value,
  );
  if (!channel) return [];
  return [...new Set(mappingProxy.modelsForChannel(channel.id))];
});

async function openMappingDialog() {
  mappingDialogOpen.value = true;
  mappingView.value = "raw";
  mappingRawSearch.value = "";
  mappingRawSelected.value = [];
  mappingTargetSearch.value = "";
  mappingBatchTarget.value = "";
  mappingBatchCustom.value = "";
  mappingEditingKey.value = "";
  mappingExpandedTarget.value = "";
  mappingNewOfficial.value = "";
  mappingInitializing.value = true;
  mappingInitializationError.value = "";
  const names = new Set<string>();
  for (const bucket of allBuckets.value) {
    const model = (bucket.model || "").trim();
    if (model && !model.includes("-unknown-")) names.add(model);
  }
  try {
    await Promise.all([
      ensureMappingCatalog(),
      mappingProxy.loadProxyData(),
      store.bootstrapTokenModelMappings([...names]),
    ]);
  } catch (error) {
    mappingInitializationError.value = String(error);
  } finally {
    mappingInitializing.value = false;
  }
}

function closeMappingDialog() {
  mappingDialogOpen.value = false;
}

async function ensureMappingCatalog(force = false) {
  if (mappingCatalogLoading.value) return;
  if (!force && mappingCatalogGroups.value.length) return;
  mappingCatalogLoading.value = true;
  mappingCatalogError.value = "";
  try {
    // 候选来自正式模型清单（目录导入 + user 手工 + AI 学习），自定义模型会实时出现在这里
    const models = await runLocalCommand<TokenOfficialModel[]>("get_token_official_models");
    const groups = new Map<string, { value: string; text: string }[]>();
    const seen = new Set<string>();
    for (const model of models) {
      const name = model.name.trim();
      if (!name || seen.has(name)) continue;
      seen.add(name);
      const lab = model.lab || "其他";
      let bucket = groups.get(lab);
      if (!bucket) {
        bucket = [];
        groups.set(lab, bucket);
      }
      bucket.push({ value: name, text: name });
    }
    mappingCatalogGroups.value = [...groups.entries()]
      .sort((a, b) => a[0].localeCompare(b[0]))
      .map(([label, options]) => ({
        label,
        options: options.sort((a, b) => a.text.localeCompare(b.text)),
      }));
  } catch (error) {
    mappingCatalogError.value = String(error);
  } finally {
    mappingCatalogLoading.value = false;
  }
}

async function runMappingAnalyze(force: boolean) {
  if (store.tokenModelAnalyzing.value || mappingInitializing.value) return;
  if (force) {
    const ok = await confirmMappingForce({
      title: "重新生成建议",
      message:
        "将重新识别待处理、已驳回和现有建议；已批准及手工映射不会被自动覆盖。确定继续？",
      confirmText: "重新识别",
      danger: false,
    });
    if (!ok) return;
  }
  const report = await store.analyzeTokenModelMappings({
    channelId: mappingChannelId.value || null,
    model: mappingModel.value || null,
    force,
  });
  if (report) {
    const summary = `识别完成：${report.resolved} 条建议等待审核 / ${report.analyzed} 条已提交`;
    store.showToast(
      report.warnings.length ? `${summary}；${report.warnings.length} 个批次告警` : summary,
      report.resolved === 0 && report.analyzed > 0,
    );
  } else {
    store.showToast(store.tokenModelAnalyzeError.value || "AI 辅助识别失败", true);
  }
}

async function approveMappingSuggestion(row: TokenModelMapping) {
  if (!await store.approveTokenModelMapping(row.rawModel)) {
    store.showToast(store.tokenModelMappingsError.value || "批准映射失败", true);
    return;
  }
  store.showToast(`已批准 ${row.rawModel} → ${row.officialModel}`);
  void store.refreshTokenDatabaseView(false);
}

async function rejectMappingSuggestion(row: TokenModelMapping) {
  if (!await store.rejectTokenModelMapping(row.rawModel)) {
    store.showToast(store.tokenModelMappingsError.value || "驳回建议失败", true);
    return;
  }
  store.showToast(`已驳回 ${row.rawModel} 的 AI 建议`);
}

async function reopenMapping(row: TokenModelMapping) {
  if (!await store.reopenTokenModelMapping(row.rawModel)) {
    store.showToast(store.tokenModelMappingsError.value || "重新识别失败", true);
    return;
  }
  store.showToast(`已将 ${row.rawModel} 放回待识别队列`);
}

// —— AI 用量洞察：证据构建（确定性计算，全部可追溯） ——
const insightDialogOpen = ref(false);
const insightModel = ref("");
const insightSubmittedRange = computed(() => store.tokenInsightReport.value?.rangeLabel ?? "");

function formatTokenCompact(value: number): string {
  if (value >= 1_000_000_000) return `${(value / 1_000_000_000).toFixed(2)}B`;
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(2)}M`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(1)}K`;
  return String(Math.round(value));
}

function insightModelSuggestions(): string[] {
  return mappingModelSuggestions.value;
}

/** 环比对比区间：与所选范围等长的紧邻前段。 */
function previousRangeDates(): { from: string; to: string } | null {
  const from = store.tokenStatsFrom.value;
  const to = store.tokenStatsTo.value;
  if (!from || !to) return null;
  const fromDate = parseLocal(from);
  const toDate = parseLocal(to);
  const days = Math.round((toDate.getTime() - fromDate.getTime()) / 86_400_000) + 1;
  if (days <= 0 || days > 366) return null;
  const prevTo = new Date(fromDate);
  prevTo.setDate(prevTo.getDate() - 1);
  const prevFrom = new Date(prevTo);
  prevFrom.setDate(prevFrom.getDate() - (days - 1));
  return { from: toLocalDate(prevFrom), to: toLocalDate(prevTo) };
}

const insightEvidenceCount = computed(() => buildInsightEvidence().evidence.length);

/** 从当前筛选桶构建证据包；只包含有数据支撑的证据项。 */
function buildInsightEvidence(): { rangeLabel: string; evidence: InsightEvidenceItem[] } {
  const evidence: InsightEvidenceItem[] = [];
  const from = store.tokenStatsFrom.value;
  const to = store.tokenStatsTo.value;
  const label = rangeLabel.value;
  const buckets = filteredBuckets.value;
  if (!buckets.length) return { rangeLabel: label, evidence };

  const total = bucketTotal.value.total;
  const conversations = bucketTotal.value.conversations;
  let requests = 0;
  let requestsEstimated = false;
  let costUsd = 0;
  for (const bucket of buckets) {
    if (bucket.requestCount != null) requests += bucket.requestCount;
    else requestsEstimated = true;
    costUsd += bucket.costUsd || 0;
  }
  const dayCount = Math.max(1, rangeDays.value);
  evidence.push({
    id: "total",
    summary: `本期总消耗 ${formatTokenCompact(total)} tokens，覆盖 ${dayCount} 天，对话 ${conversations} 轮`,
    value: `total=${formatTokenCompact(total)}; days=${dayCount}; dailyAvg=${formatTokenCompact(total / dayCount)}`,
  });
  evidence.push({
    id: "requests",
    summary: requestsEstimated
      ? `API 请求总数无法从该来源精确统计（按输出估算约 ${formatTokenCompact(requests)}）`
      : `API 请求总数 ${formatTokenCompact(requests)}`,
    value: `requests=${requests}${requestsEstimated ? "; estimated=true" : ""}`,
  });
  if (costUsd > 0) {
    evidence.push({
      id: "cost",
      summary: `来源上报成本合计 $${costUsd.toFixed(2)}`,
      value: `costUsd=${costUsd.toFixed(2)}`,
    });
  }

  // 环比：等长紧邻前段
  const previous = previousRangeDates();
  if (previous) {
    const prevBuckets = allBuckets.value.filter((bucket) => {
      if (!isKnownModel(bucket.model) || !isKnownSource(bucket.source)) return false;
      const day = localDateOf(bucket.timestamp);
      return day >= previous.from && day <= previous.to;
    });
    const prevTotal = prevBuckets.reduce((sum, bucket) => sum + (bucket.totalTokens || 0), 0);
    if (prevTotal > 0 || total > 0) {
      const change = prevTotal > 0 ? ((total - prevTotal) / prevTotal) * 100 : null;
      evidence.push({
        id: "period_change",
        summary: change == null
          ? `上期（${previous.from} ~ ${previous.to}）消耗 ${formatTokenCompact(prevTotal)}，本期 ${formatTokenCompact(total)}（上期无数据，无法计算百分比）`
          : `环比上期（${previous.from} ~ ${previous.to}，消耗 ${formatTokenCompact(prevTotal)}）变化 ${change >= 0 ? "+" : ""}${change.toFixed(1)}%`,
        value: `current=${formatTokenCompact(total)}; previous=${formatTokenCompact(prevTotal)}; change=${change == null ? "n/a" : `${change.toFixed(1)}%`}`,
      });
    }
  }

  // 缓存命中率
  const cache = cacheBreakdown.value;
  if (cache.hitRate != null) {
    evidence.push({
      id: "cache",
      summary: `缓存读取 ${formatTokenCompact(cache.read)} / 写入 ${formatTokenCompact(cache.write)}，命中率 ${(cache.hitRate * 100).toFixed(1)}%`,
      value: `hitRate=${(cache.hitRate * 100).toFixed(1)}%`,
    });
  }

  // Top 模型集中度（映射后口径）
  const models = byModel.value;
  if (models.length) {
    const top = models.slice(0, 3);
    const topShare = top.reduce((sum, item) => sum + shareOf(item.totalTokens, total), 0);
    evidence.push({
      id: "models",
      summary: `Top${top.length} 模型 ${top.map((item) => `${item.model}(${formatTokenCompact(item.totalTokens)}, ${shareOf(item.totalTokens, total).toFixed(1)}%)`).join("、")}，合计占比 ${topShare.toFixed(1)}%`,
      value: `topModels=${topShare.toFixed(1)}%`,
    });
    const top1 = models[0];
    evidence.push({
      id: "top_model",
      summary: `用量最高的模型是 ${top1.model}，${formatTokenCompact(top1.totalTokens)} tokens（占 ${shareOf(top1.totalTokens, total).toFixed(1)}%），请求 ${formatTokenCompact(top1.requests)} 次，缓存命中 ${top1.cacheHitRate == null ? "未知" : `${(top1.cacheHitRate * 100).toFixed(1)}%`}`,
      value: `model=${top1.model}; share=${shareOf(top1.totalTokens, total).toFixed(1)}%`,
    });
  }

  // Top 工具集中度
  const sources = bySource.value;
  if (sources.length) {
    const top = sources.slice(0, 3);
    evidence.push({
      id: "sources",
      summary: `Top${top.length} 工具 ${top.map((item) => `${sourceLabel(item.source)}(${formatTokenCompact(item.totalTokens)}, ${shareOf(item.totalTokens, total).toFixed(1)}%)`).join("、")}`,
      value: `topSources=${top.map((item) => item.source).join(",")}`,
    });
  }

  // 项目 / 渠道集中度
  if (projectUsage.value.length) {
    const top = projectUsage.value[0];
    evidence.push({
      id: "projects",
      summary: `用量最高的${statsMode.value === "local" ? "项目" : "渠道"}是 ${top.project}，${formatTokenCompact(top.totalTokens)} tokens（占 ${shareOf(top.totalTokens, total).toFixed(1)}%）`,
      value: `project=${top.project}; share=${shareOf(top.totalTokens, total).toFixed(1)}%`,
    });
  }

  // 日趋势峰值：找出显著高于日均的日期（> 2 倍日均且 > 1000 tokens）
  const dailyEntries = [...dailyMap.value.entries()].sort((a, b) => a[0].localeCompare(b[0]));
  if (dailyEntries.length >= 3) {
    const values = dailyEntries.map(([, stat]) => stat.total);
    const mean = values.reduce((sum, value) => sum + value, 0) / values.length;
    const peaks = dailyEntries
      .filter(([, stat]) => stat.total > mean * 2 && stat.total > 1000)
      .sort((a, b) => b[1].total - a[1].total)
      .slice(0, 3);
    if (peaks.length) {
      evidence.push({
        id: "peaks",
        summary: `日均 ${formatTokenCompact(mean)}；有 ${peaks.length} 天显著高于均值：${peaks.map(([date, stat]) => `${date}（${formatTokenCompact(stat.total)}，${(stat.total / mean).toFixed(1)}x）`).join("、")}`,
        value: `dailyAvg=${formatTokenCompact(mean)}; peaks=${peaks.map(([date]) => date).join(",")}`,
      });
    }
    const quietDays = dailyEntries.filter(([, stat]) => stat.total === 0).length;
    if (quietDays > 0) {
      evidence.push({
        id: "gaps",
        summary: `所选范围内有 ${quietDays} 天完全没有用量记录`,
        value: `quietDays=${quietDays}`,
      });
    }
  }

  // 数据质量提示：估算比例
  const estimatedTokens = buckets.reduce((sum, bucket) => sum + (bucket.estimatedTokens || 0), 0);
  if (estimatedTokens > 0 && total > 0) {
    const ratio = (estimatedTokens / total) * 100;
    evidence.push({
      id: "data_quality",
      summary: `${ratio.toFixed(1)}% 的 tokens 来自本地估算而非来源直接上报，相关数字存在不确定性`,
      value: `estimatedRatio=${ratio.toFixed(1)}%`,
    });
  }

  return { rangeLabel: label, evidence };
}

/** 提交洞察分析：范围以提交时为准并写入报告快照。 */
async function runInsightAnalysis() {
  if (store.tokenInsightAnalyzing.value) return;
  const { rangeLabel: label, evidence } = buildInsightEvidence();
  if (!evidence.length) {
    store.showToast("当前时间范围没有可用数据，先调整日期区间", true);
    return;
  }
  const model = insightModel.value.trim();
  if (!model) {
    store.showToast("请先填写分析模型 ID", true);
    return;
  }
  const report = await store.analyzeTokenInsights({
    rangeLabel: label,
    analysisModel: model,
    evidence,
  });
  if (report) {
    store.showToast(
      report.findings.length || report.recommendations.length
        ? `洞察完成：${report.findings.length} 项发现 / ${report.recommendations.length} 项建议`
        : "AI 未能给出可信结论",
      !(report.findings.length || report.recommendations.length),
    );
    insightDialogOpen.value = true;
  } else {
    store.showToast(store.tokenInsightError.value || "洞察分析失败", true);
  }
}

function openInsightDialog() {
  insightDialogOpen.value = true;
  if (!insightModel.value) insightModel.value = mappingModel.value;
}

/** 证据 ID → 中文说明，用于报告中的“依据”标注。 */
function findingEvidenceText(ids: string[]): string {
  return ids.join("、");
}

const insightReportTime = computed(() => {
  const raw = store.tokenInsightReport.value?.generatedAt ?? "";
  if (!raw) return "";
  const date = new Date(raw);
  if (Number.isNaN(date.getTime())) return raw;
  return date.toLocaleString("zh-CN", { hour12: false });
});


// —— 本地 AI Agent 路径诊断弹窗 ——
const agentDialogOpen = ref(false);
const agentKindLabels: Record<string, string> = {
  config: "配置",
  data: "数据",
  database: "数据库",
  logs: "日志",
};
const localAgents = computed(() => store.localAgentPaths.value?.agents ?? []);
const visibleAgents = computed(() => localAgents.value.filter((a) => a.detected));
const localAgentsHome = computed(() => store.localAgentPaths.value?.home ?? "");
const localAgentEnvOverrides = computed(() => store.localAgentPaths.value?.envOverrides ?? []);
const detectedAgentsCount = computed(() => localAgents.value.filter((a) => a.detected).length);
const localAgentsCollectedAt = computed(() => {
  const raw = store.localAgentPaths.value?.collectedAt ?? "";
  return raw.length >= 16 ? raw.slice(5, 16).replace("T", " ") : "";
});

function formatAgentCount(value: number): string {
  return value >= 10000 ? `${(value / 1000).toFixed(1)}k` : String(value);
}

function displayAgentPath(path: string): string {
  const home = localAgentsHome.value;
  if (home && path.startsWith(`${home}/`)) return `~${path.slice(home.length)}`;
  return path;
}

function agentPathSegments(path: string): string[] {
  return displayAgentPath(path)
    .split("/")
    .filter(Boolean)
    .map((part, index, parts) => (index < parts.length - 1 ? `${part}/` : part));
}

function openAgentDialog() {
  agentDialogOpen.value = true;
  void store.loadLocalAgentPaths();
}

function closeAgentDialog() {
  agentDialogOpen.value = false;
}

async function copyAgentPath(path: string) {
  if (!path) return;
  try {
    await navigator.clipboard.writeText(path);
    store.showToast("已复制路径至剪贴板");
  } catch {
    store.showToast("复制失败", true);
  }
}

// —— 数据导出弹窗 ——
const exportDialogOpen = ref(false);
function openExportDialog() {
  exportDialogOpen.value = true;
}

// 原生菜单「文件 → 导出数据」联动：切到本地统计页后打开导出弹窗。
window.addEventListener("oh-menu-export", () => {
  if (!exportDialogOpen.value) openExportDialog();
});
function closeExportDialog() {
  exportDialogOpen.value = false;
}

async function downloadFile(filename: string, content: string, mimeType: string) {
  if (isTauri) {
    try {
      // Base64 编码以安全传输二进制内容（如 CSV 的 BOM）
      const encoder = new TextEncoder();
      const bytes = encoder.encode(content);
      let binary = "";
      for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
      const base64 = btoa(binary);
      const result = await runLocalCommand<{ path: string | null; cancelled: boolean }>("save_export_file", {
        args: { filename, content: base64 },
      });
      if (result.cancelled) return;
      store.showToast(`已导出到 ${result.path}`);
    } catch (e) {
      store.showToast(`导出失败: ${e}`);
    }
  } else {
    const blob = new Blob([content], { type: mimeType });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = filename;
    a.click();
    URL.revokeObjectURL(url);
    store.showToast("已导出文件");
  }
}

async function exportDataAsJson() {
  const payload = {
    exportTime: new Date().toISOString(),
    timeRange: {
      from: store.tokenStatsFrom.value || null,
      to: store.tokenStatsTo.value || null,
      label: rangeLabel.value,
    },
    summary: {
      totalTokens: bucketTotal.value.total,
      inputTokens: rangeSplits.value.input,
      outputTokens: rangeSplits.value.output,
      cacheTokens: rangeSplits.value.cache,
      cacheReadTokens: cacheBreakdown.value.read,
      cacheWriteTokens: cacheBreakdown.value.write,
      cacheHitRate: cacheHitRate.value,
      dailyAverage: dailyAverage.value,
      activeDays: activeDays.value,
      streakDays: streakDays.value,
      dialogues: healthTimeline.value.totalDialogues,
      requests: healthTimeline.value.totalRequests,
    },
    sources: bySource.value,
    models: byModel.value,
    projects: projectUsage.value,
    trendDetails: trendDetailList.value,
  };
  await downloadFile(
    `openhub-token-stats-${toLocalDate(new Date())}.json`,
    JSON.stringify(payload, null, 2),
    "application/json",
  );
  closeExportDialog();
}

async function exportDataAsCsv() {
  const rows: string[] = [];
  rows.push("时间,总计Token,输入Token,输出Token,缓存读取Token,缓存写入Token,缓存命中率,推理Token,对话数,请求数");
  for (const item of trendDetailList.value) {
    const hitRate = item.cacheHitRate != null ? `${(item.cacheHitRate * 100).toFixed(2)}%` : "0%";
    rows.push(`"${item.label}",${item.total},${item.input},${item.output},${item.cacheRead},${item.cacheWrite},"${hitRate}",${item.reasoning},${item.sessions},${item.requests}`);
  }
  await downloadFile(
    `openhub-token-trend-${toLocalDate(new Date())}.csv`,
    "\uFEFF" + rows.join("\n"),
    "text/csv;charset=utf-8;",
  );
  closeExportDialog();
}

// —— 表格列配置（宽度总和需控制在弹窗内容宽 ~920px 内，避免横向滚动条） ——
// 反代模式无对话/轮次概念，自动剔除对话类列
const dailyColumns = computed<AppTableColumn[]>(() => {
  const cols: AppTableColumn[] = [
    { key: "label", title: "时间节点", width: "minmax(120px, 1.2fr)", sortable: true },
    { key: "total", title: "总量 Tokens", width: "88px", align: "right", sortable: true },
    { key: "input", title: "输入", width: "72px", align: "right", sortable: true },
    { key: "output", title: "输出", width: "72px", align: "right", sortable: true },
    { key: "cacheRead", title: "缓存读取", width: "76px", align: "right", sortable: true },
    { key: "cacheWrite", title: "缓存写入", width: "76px", align: "right", sortable: true },
    { key: "cacheHitRate", title: "缓存命中率", width: "80px", align: "right", sortable: true },
    { key: "reasoning", title: "深度推理", width: "72px", align: "right", sortable: true },
  ];
  if (statsMode.value === "local") {
    cols.push({ key: "sessions", title: "对话轮次", width: "72px", align: "right", sortable: true });
  }
  cols.push({ key: "requests", title: "API 请求数", width: "80px", align: "right", sortable: true });
  return cols;
});

const projectColumns = computed<AppTableColumn[]>(() => {
  const cols: AppTableColumn[] = [
    { key: "project", title: statsMode.value === "local" ? "项目 / 工作区" : "渠道", width: "minmax(130px, 1.5fr)", sortable: true },
    { key: "totalTokens", title: "消耗总计", width: "90px", align: "right", sortable: true },
    { key: "share", title: "占比", width: "78px", align: "right", sortable: false },
    { key: "input", title: "输入", width: "74px", align: "right", sortable: true },
    { key: "output", title: "输出", width: "74px", align: "right", sortable: true },
    { key: "cacheRead", title: "缓存读取", width: "74px", align: "right", sortable: true },
    { key: "cacheWrite", title: "缓存写入", width: "74px", align: "right", sortable: true },
    { key: "cacheHitRate", title: "缓存命中率", width: "82px", align: "right", sortable: true },
    { key: "reasoning", title: "推理", width: "74px", align: "right", sortable: true },
  ];
  if (statsMode.value === "local") {
    cols.push({ key: "sessions", title: "对话轮次", width: "74px", align: "right", sortable: true });
  }
  cols.push({ key: "requests", title: "请求数", width: "74px", align: "right", sortable: true });
  return cols;
});

const sourceColumns = computed<AppTableColumn[]>(() => {
  const cols: AppTableColumn[] = [
    { key: "source", title: "工具 / 来源", width: "minmax(130px, 1.4fr)", sortable: true },
    { key: "totalTokens", title: "总量 Tokens", width: "90px", align: "right", sortable: true },
    { key: "share", title: "占比", width: "78px", align: "right", sortable: false },
    { key: "inputTokens", title: "输入", width: "74px", align: "right", sortable: true },
    { key: "outputTokens", title: "输出", width: "74px", align: "right", sortable: true },
    { key: "cacheReadTokens", title: "缓存读取", width: "74px", align: "right", sortable: true },
    { key: "cacheWriteTokens", title: "缓存写入", width: "74px", align: "right", sortable: true },
    { key: "cacheHitRate", title: "缓存命中率", width: "82px", align: "right", sortable: true },
    { key: "reasoningTokens", title: "推理", width: "74px", align: "right", sortable: true },
  ];
  if (statsMode.value === "local") {
    cols.push({ key: "conversations", title: "对话数", width: "74px", align: "right", sortable: true });
  }
  cols.push({ key: "requests", title: "请求数", width: "74px", align: "right", sortable: true });
  return cols;
});

const healthColumns = computed<AppTableColumn[]>(() => {
  const cols: AppTableColumn[] = [
    { key: "label", title: "时段", width: "minmax(120px, 1.2fr)", sortable: true },
  ];
  if (statsMode.value === "local") {
    cols.push({ key: "dialogues", title: "对话数", width: "80px", align: "right", sortable: true });
  }
  cols.push(
    { key: "requests", title: "请求数", width: "82px", align: "right", sortable: true },
    { key: "success", title: "成功", width: "78px", align: "right", sortable: true },
    { key: "failed", title: "失败", width: "78px", align: "right", sortable: true },
    { key: "successRate", title: "成功率", width: "84px", align: "right", sortable: true },
    { key: "level", title: "健康等级", width: "86px", align: "center", sortable: true },
  );
  return cols;
});

const healthTableRows = computed(() =>
  healthTimeline.value.cells
    .filter((c) => c.requests > 0)
    .map((c) => ({
      ...c,
      successRate: c.successRate != null ? (c.successRate * 100).toFixed(1) + "%" : "—",
    })),
);

const modelColumns = computed<AppTableColumn[]>(() => {
  const cols: AppTableColumn[] = [
    { key: "model", title: "模型名称 / 家族", width: "minmax(140px, 1.6fr)", sortable: true },
    { key: "totalTokens", title: "总量 Tokens", width: "90px", align: "right", sortable: true },
    { key: "share", title: "占比", width: "78px", align: "right", sortable: false },
    { key: "inputTokens", title: "输入", width: "74px", align: "right", sortable: true },
    { key: "outputTokens", title: "输出", width: "74px", align: "right", sortable: true },
    { key: "cacheReadTokens", title: "缓存读取", width: "74px", align: "right", sortable: true },
    { key: "cacheWriteTokens", title: "缓存写入", width: "74px", align: "right", sortable: true },
    { key: "cacheHitRate", title: "缓存命中率", width: "82px", align: "right", sortable: true },
    { key: "reasoningTokens", title: "推理", width: "74px", align: "right", sortable: true },
  ];
  if (statsMode.value === "local") {
    cols.push({ key: "conversations", title: "对话", width: "74px", align: "right", sortable: true });
  }
  cols.push({ key: "requests", title: "请求数", width: "74px", align: "right", sortable: true });
  return cols;
});

const sessions = computed(() => store.tokenStats.value?.sessions ?? []);

// —— 供应商品牌色与配置 ——
const PROVIDER_COLORS: Record<string, string> = {
  claude: "#d97757",
  codex: "#3b82f6",
  cursor: "#8b5cf6",
  opencode: "#f59e0b",
  gemini: "#2196f3",
  antigravity: "#ff6900",
  kiro: "#a78bfa",
  copilot: "#0969da",
  openclaw: "#facc15",
  goose: "#ef4444",
  zed: "#14b8a6",
  catpawai: "#ec4899",
  "command-code": "#10b981",
  dsh: "#1e88e5",
};

function providerColor(source: string, index = 0): string {
  return PROVIDER_COLORS[source.toLowerCase()] || `hsl(${150 + index * 40}, 60%, 45%)`;
}

const HEALTH_LEVEL_COLORS = ["rgba(148,163,184,0.4)", "#ef4444", "#f97316", "#eab308", "#84cc16", "#10b981"];
function healthLevelColor(level: number): string {
  return HEALTH_LEVEL_COLORS[level] ?? HEALTH_LEVEL_COLORS[0];
}

const sourceNameMap: Record<string, string> = {
  claude: "Claude Code",
  codex: "Codex CLI",
  cursor: "Cursor",
  catpawai: "CatPawAI",
  gemini: "Gemini CLI",
  opencode: "OpenCode",
  kiro: "Kiro",
  copilot: "GitHub Copilot (VS Code)",
  openclaw: "OpenClaw",
  goose: "Goose AI",
  antigravity: "Google Antigravity",
  zed: "Zed Editor",
  "command-code": "Command Code",
  dsh: "DeepSeek CLI (DSH)",
  // —— 反代模式：按端点/SDK 推断的客户端标识 ——
  openhub: "OpenHub",
  sdk: "SDK / 脚本",
  "anthropic-api": "Anthropic 协议客户端",
  "responses-api": "Responses 协议客户端",
  "openai-api": "OpenAI 协议客户端",
  "gemini-api": "Gemini 协议客户端",
  other: "其他客户端",
};

function sourceLabel(source: string): string {
  return sourceNameMap[source.toLowerCase()] || source || "未知来源";
}

function shareOf(value: number, total: number): number {
  return total > 0 ? Math.min(100, (value / total) * 100) : 0;
}

// —— 小时用量桶过滤（数据源随模式切换：本地采集快照 / 反代网关聚合表） ——
const allBuckets = computed(() => activeUsage.value?.buckets ?? []);
const filteredBuckets = computed(() => {
  const buckets = allBuckets.value;
  const from = store.tokenStatsFrom.value;
  const to = store.tokenStatsTo.value;
  return buckets.filter((bucket) => {
    if (!isKnownModel(bucket.model) || !isKnownSource(bucket.source)) return false;
    const day = localDateOf(bucket.timestamp);
    return (!from || day >= from) && (!to || day <= to);
  });
});

const dailyMap = computed(() => buildDailyMapFromBuckets(filteredBuckets.value));
const bucketTotal = computed(() => bucketTotals(filteredBuckets.value));

// —— KPI 指标计算 ——
const activeDays = computed(() => dailyMap.value.size);
const streakDays = computed(() => {
  const keys = [...dailyMap.value.keys()].sort();
  if (!keys.length) return 0;
  const cursor = parseLocal(keys[keys.length - 1]);
  let streak = 0;
  while (dailyMap.value.has(toLocalDate(cursor))) {
    streak += 1;
    cursor.setDate(cursor.getDate() - 1);
  }
  return streak;
});

const rangeLabel = computed(() => {
  const from = store.tokenStatsFrom.value;
  const to = store.tokenStatsTo.value;
  if (!from && !to) return "全部时间";
  if (from === to) return from;
  return `${from || "…"} ~ ${to || "…"}`;
});

const rangeDays = computed(() => {
  const from = store.tokenStatsFrom.value;
  const to = store.tokenStatsTo.value;
  if (!from || !to) return activeDays.value || 1;
  return Math.max(1, Math.round((parseLocal(to).getTime() - parseLocal(from).getTime()) / 86_400_000) + 1);
});

const dailyAverage = computed(() => {
  const days = Math.max(1, activeDays.value);
  return bucketTotal.value.total / days;
});

const rangeSplits = computed(() => {
  let input = 0;
  let output = 0;
  let cache = 0;
  let reasoning = 0;
  for (const stat of dailyMap.value.values()) {
    input += stat.input;
    output += stat.output;
    cache += stat.cache;
    reasoning += stat.reasoning;
  }
  return { input, output, cache, reasoning };
});

// 缓存效能细分与命中率
const cacheBreakdown = computed(() => {
  let cacheRead = 0;
  let cacheWrite = 0;
  let fresh = 0;
  let estimatedInput = 0;
  for (const bucket of filteredBuckets.value) {
    cacheRead += bucket.cachedInputTokens || 0;
    cacheWrite += bucket.cacheCreationInputTokens || 0;
    fresh += bucket.inputTokens || 0;
    estimatedInput += bucket.estimatedInputTokens || 0;
  }
  const hitRate = cacheHitRateOf(cacheRead, cacheWrite, fresh, estimatedInput);
  const totalCached = cacheRead + cacheWrite;
  const reuseSpeedup = fresh > 0 && cacheRead > 0
    ? ((fresh + cacheRead) / fresh).toFixed(1)
    : null;
  return {
    read: cacheRead,
    write: cacheWrite,
    total: totalCached,
    fresh,
    hitRate,
    speedup: reuseSpeedup,
  };
});

const cacheHitRate = computed(() => cacheBreakdown.value.hitRate);

// 缓存命中率评级
const cacheHitRateRating = computed(() => {
  const rate = cacheHitRate.value;
  if (rate == null || rate <= 0) return { label: "暂无缓存", class: "is-none" };
  if (rate >= 0.7) return { label: "极高效率 ⚡", class: "is-excellent" };
  if (rate >= 0.4) return { label: "良好 ✦", class: "is-good" };
  return { label: "偏低 · 可优化", class: "is-fair" };
});

const totalTokensAll = computed(() => bucketTotal.value.total);

// 工具分布
const bySource = computed(() =>
  bucketSourceTotals(filteredBuckets.value).filter((item) => item.totalTokens > 0),
);
const filteredSources = computed(() => {
  const q = sourceSearch.value.trim().toLowerCase();
  if (!q) return bySource.value;
  return bySource.value.filter((s) =>
    s.source.toLowerCase().includes(q) || sourceLabel(s.source).toLowerCase().includes(q),
  );
});

// 模型分布：优先按映射表的正式模型名归组，未映射的回退现有归一化规则
const modelMappingLookup = computed(() => buildModelMappingLookup(store.tokenModelMappings.value));
const byModel = computed(() =>
  mergeModelTotals(bucketModelTotals(filteredBuckets.value), modelMappingLookup.value).filter(
    (item) => item.totalTokens > 0,
  ),
);
const filteredModels = computed(() => {
  const q = modelSearch.value.trim().toLowerCase();
  if (!q) return byModel.value;
  return byModel.value.filter((m) => m.model.toLowerCase().includes(q));
});

const topSources = computed(() => bySource.value.slice(0, 5));
const topModels = computed(() => byModel.value.slice(0, 5));

// 项目用量
type ProjectUsageItem = {
  project: string;
  sessions: number;
  requests: number;
  requestsEstimated: boolean;
  totalTokens: number;
  input: number;
  output: number;
  cache: number;
  cacheRead: number;
  cacheWrite: number;
  cacheHitRate: number | null;
  reasoning: number;
  costUsd: number;
  estimatedTokens: number;
};

const projectUsage = computed<ProjectUsageItem[]>(() => {
  const groups = new Map<
    string,
    {
      project: string;
      sessions: number;
      requests: number;
      requestsEstimated: boolean;
      totalTokens: number;
      input: number;
      output: number;
      cache: number;
      cacheRead: number;
      cacheWrite: number;
      cacheHitRate: number | null;
      reasoning: number;
      costUsd: number;
      estimatedTokens: number;
      estimatedInput: number;
    }
  >();

  const isSessionUuid = (s: string) =>
    /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(s.trim()) ||
    s.startsWith("rollout-") ||
    s.startsWith("session-");

  const isCommonSubfolderName = (name: string) => {
    const lower = name.toLowerCase();
    return [
      "src",
      "src-tauri",
      "docs",
      "target",
      "bin",
      "node_modules",
      "pkg",
      "app",
      "core",
      "client",
      "server",
      "ui",
      "web",
      "sys",
      "staff",
      "third",
      "controller",
      "controllers",
      "model",
      "models",
      "view",
      "views",
      "service",
      "services",
      "scripts",
      "frontend",
      "backend",
      "dist",
      "build",
      "test",
      "tests",
      "public",
      "custom",
      "applications",
    ].includes(lower);
  };

  const normalizeProject = (rawKey?: string) => {
    let value = rawKey?.trim() || "";
    if (!value) return "全局 / 独立会话";

    if (isSessionUuid(value)) return "临时任务 / 独立会话";

    if (value.startsWith("file://")) {
      try {
        value = decodeURIComponent(value.replace(/^file:\/\//, ""));
      } catch {
        value = value.replace(/^file:\/\//, "");
      }
    }

    value = value.replace(/\\/g, "/").replace(/\/+$/, "");

    if (value.endsWith(".code-workspace")) {
      const parts = value.split("/");
      return parts[parts.length - 1].replace(/\.code-workspace$/, "");
    }

    if (value.includes("/")) {
      const parts = value.split("/").filter(Boolean);
      for (let i = parts.length - 1; i >= 0; i--) {
        const part = parts[i];
        if (
          !isCommonSubfolderName(part) &&
          part !== "Users" &&
          part !== "Applications" &&
          !isSessionUuid(part)
        ) {
          return part;
        }
      }
      return parts[parts.length - 1] || "全局 / 独立会话";
    }

    if (
      ["VS Code", "Copilot CLI", "Antigravity IDE", "Antigravity CLI", "DSH", "Codex"].includes(
        value
      )
    ) {
      return "全局 / 独立会话";
    }

    return value;
  };

  const ensureGroup = (rawKey?: string) => {
    const key = normalizeProject(rawKey);
    const current = groups.get(key) || {
      project: key,
      sessions: 0,
      requests: 0,
      requestsEstimated: false,
      totalTokens: 0,
      input: 0,
      output: 0,
      cache: 0,
      cacheRead: 0,
      cacheWrite: 0,
      cacheHitRate: null,
      reasoning: 0,
      costUsd: 0,
      estimatedTokens: 0,
      estimatedInput: 0,
    };
    groups.set(key, current);
    return current;
  };

  const projectBucketSources = new Set<string>();
  for (const bucket of filteredBuckets.value) {
    if (!bucket.projectKey?.trim()) continue;
    projectBucketSources.add(bucket.source.toLowerCase());
    const current = ensureGroup(bucket.projectKey);
    current.sessions += bucket.conversationCount || 0;
    if (bucket.requestCount != null) {
      current.requests += bucket.requestCount || 0;
    } else {
      current.requests += estimateRequestCount({
        conversationCount: bucket.conversationCount,
        outputTokens: bucket.outputTokens,
        reasoningOutputTokens: bucket.reasoningOutputTokens,
        totalTokens: bucket.totalTokens,
      });
      current.requestsEstimated = true;
    }
    current.totalTokens += bucket.totalTokens || 0;
    current.input += bucket.inputTokens || 0;
    current.output += bucket.outputTokens || 0;
    current.cache += (bucket.cachedInputTokens || 0) + (bucket.cacheCreationInputTokens || 0);
    current.cacheRead += bucket.cachedInputTokens || 0;
    current.cacheWrite += bucket.cacheCreationInputTokens || 0;
    current.reasoning += bucket.reasoningOutputTokens || 0;
    current.costUsd += bucket.costUsd || 0;
    current.estimatedTokens += bucket.estimatedTokens || 0;
    current.estimatedInput += bucket.estimatedInputTokens || 0;
  }

  // 会话级兜底仅本地模式可用（反代请求无会话概念，项目维度直接来自桶的渠道维度）
  if (statsMode.value === "local") {
    for (const session of sessions.value) {
      if (projectBucketSources.has((session.source || "").toLowerCase())) continue;
      const current = ensureGroup(session.projectKey);
      const sessionTurns = session.turns || 0;
      current.sessions += sessionTurns;
      current.requests += estimateRequestCount({
        conversationCount: sessionTurns,
        outputTokens: session.tokens?.outputTokens,
        reasoningOutputTokens: session.tokens?.reasoningOutputTokens,
      });
      current.requestsEstimated = true;
      current.totalTokens += session.totalTokens || 0;
      current.input += session.tokens?.inputTokens || 0;
      current.output += session.tokens?.outputTokens || 0;
      current.cache += (session.tokens?.cachedInputTokens || 0) + (session.tokens?.cacheCreationInputTokens || 0);
      current.cacheRead += session.tokens?.cachedInputTokens || 0;
      current.cacheWrite += session.tokens?.cacheCreationInputTokens || 0;
      current.reasoning += session.tokens?.reasoningOutputTokens || 0;
      current.costUsd += session.costUsd || 0;
      const usageKind = String(session.provenance?.tokenUsage || "");
      if (usageKind.includes("estimated")) {
        current.estimatedTokens += session.totalTokens || 0;
        current.estimatedInput += session.tokens?.inputTokens || 0;
      }
    }
  }

  return [...groups.values()]
    .map((item) => ({
      ...item,
      cacheHitRate: cacheHitRateOf(item.cacheRead, item.cacheWrite, item.input, item.estimatedInput),
    }))
    .filter((item) => item.totalTokens > 0)
    .sort((a, b) => b.totalTokens - a.totalTokens);
});

const filteredProjects = computed(() => {
  const q = projectSearch.value.trim().toLowerCase();
  if (!q) return projectUsage.value;
  return projectUsage.value.filter((p) => p.project.toLowerCase().includes(q));
});

// —— 趋势与明细数据 ——
const trendSeries = computed(() =>
  buildTrendFromBuckets(
    filteredBuckets.value,
    trendGranularity.value,
    store.tokenStatsFrom.value || undefined,
    store.tokenStatsTo.value || undefined,
  ),
);

const trendDetail = computed(() =>
  buildTrendDetailFromBuckets(
    filteredBuckets.value,
    trendGranularity.value,
    store.tokenStatsFrom.value || undefined,
    store.tokenStatsTo.value || undefined,
  ),
);

function formatDetailTime(label: string): string {
  switch (trendGranularity.value) {
    case "hour": {
      const m = label.match(/^(\d{4})-(\d{2}-\d{2}) (\d{2}:\d{2})$/);
      return m ? `${m[2]} ${m[3]}` : label;
    }
    case "day": {
      const m = label.match(/^\d{4}-(\d{2}-\d{2})$/);
      return m ? m[1] : label;
    }
    default:
      return label;
  }
}

// —— 请求健康时间线（反代模式下来自网关聚合表：真实 status_code 口径） ——
const healthTimeline = computed(() =>
  buildHealthTimeline(
    activeHealth.value?.buckets ?? [],
    trendGranularity.value,
    store.tokenStatsFrom.value || undefined,
    store.tokenStatsTo.value || undefined,
    allBuckets.value.map((b) => ({
      timestamp: b.timestamp,
      conversationCount: b.conversationCount || 0,
      outputTokens: b.outputTokens || 0,
      reasoningOutputTokens: b.reasoningOutputTokens || 0,
      totalTokens: b.totalTokens || 0,
      requests: b.requestCount ?? undefined,
    })),
  ),
);

const requestsPerTurnLabel = computed(() => {
  const dialogues = healthTimeline.value.totalDialogues;
  if (!dialogues) return "—";
  const avg = healthTimeline.value.totalRequests / dialogues;
  return avg >= 10 ? String(Math.round(avg)) : avg.toFixed(1);
});

const requestsByLabel = computed(() => {
  const map = new Map<string, { requests: number; requestsEstimated: boolean }>();
  for (const cell of healthTimeline.value.cells) {
    map.set(cell.label, {
      requests: Math.max(0, cell.requests || 0),
      requestsEstimated: cell.requestsEstimated || false,
    });
  }
  return map;
});

const trendDetailList = computed(() =>
  trendDetail.value
    .filter((item) => item.total > 0)
    .map((item) => {
      const hit = requestsByLabel.value.get(item.label);
      return {
        ...item,
        requests: hit?.requests ?? 0,
        requestsEstimated: hit?.requestsEstimated ?? false,
      };
    }),
);

// —— 健康时间线网格测量 ——
// 六行排列；HEALTH_CELL 为单列目标宽度，值越大列越少、方块越宽；HEALTH_GAP 与 CSS column-gap 保持一致
const HEALTH_ROWS = 6;
const HEALTH_CELL = 16;
const HEALTH_GAP = 4;
const healthGridRef = ref<HTMLElement | null>(null);
const healthCols = ref(24);
let healthRo: ResizeObserver | null = null;

const healthStatusInfo = computed(() => {
  const rate = healthTimeline.value.successRate;
  if (rate == null) return { label: "状态正常", class: "is-excellent" };
  if (rate >= 0.98) return { label: "极佳", class: "is-excellent" };
  if (rate >= 0.90) return { label: "良好", class: "is-good" };
  if (rate >= 0.80) return { label: "波动", class: "is-fair" };
  return { label: "异常", class: "is-bad" };
});

function measureHealthGrid() {
  const el = healthGridRef.value;
  if (!el) return;
  const width = el.clientWidth || el.getBoundingClientRect().width;
  if (width <= 0) return;
  const targetColWidth = HEALTH_CELL + HEALTH_GAP;
  const cols = Math.max(12, Math.round((width + HEALTH_GAP) / targetColWidth));
  if (cols !== healthCols.value) healthCols.value = cols;
}

type HealthDisplayCell = {
  key: string;
  label: string;
  dialogues: number;
  success: number;
  failed: number;
  requests: number;
  requestsEstimated?: boolean;
  successRate: number | null;
  level: number;
  pad?: boolean;
};

const healthBucketMap = computed(() => {
  const map = new Map<string, { dialogues: number; requests: number; success: number; failed: number; usage: number; usageEstimated: boolean }>();
  // 反代模式：合并区间前历史桶（precedingBuckets），矩阵前置补位格才能取到历史数据
  const healthBuckets = [
    ...(activeHealth.value?.buckets ?? []),
    ...(activeHealth.value?.precedingBuckets ?? []),
  ];
  for (const b of healthBuckets) {
    const { key } = bucketKeyFor(trendGranularity.value, b.hour);
    if (!key) continue;
    const cur = map.get(key) || { dialogues: 0, requests: 0, success: 0, failed: 0, usage: 0, usageEstimated: false };
    cur.dialogues += Number(b.dialogues || 0);
    cur.requests += b.requests || 0;
    cur.success += b.success || 0;
    cur.failed += b.failed || 0;
    map.set(key, cur);
  }
  for (const b of allBuckets.value) {
    const { key } = bucketKeyFor(trendGranularity.value, b.timestamp);
    if (!key) continue;
    const cur = map.get(key) || { dialogues: 0, requests: 0, success: 0, failed: 0, usage: 0, usageEstimated: false };
    if (b.requestCount != null) {
      cur.usage += b.requestCount || 0;
    } else {
      cur.usage += estimateRequestCount({
        conversationCount: b.conversationCount || 0,
        outputTokens: b.outputTokens || 0,
        reasoningOutputTokens: b.reasoningOutputTokens || 0,
        totalTokens: b.totalTokens || 0,
      });
      cur.usageEstimated = true;
    }
    map.set(key, cur);
  }
  return map;
});

const healthDisplayCells = computed<HealthDisplayCell[]>(() => {
  const source = healthTimeline.value.cells;
  const capacity = HEALTH_ROWS * Math.max(1, healthCols.value);
  let body: HealthDisplayCell[] = source.map((c) => ({ ...c, pad: false }));
  if (body.length > capacity) {
    body = body.slice(body.length - capacity);
  }

  const padCount = Math.max(0, capacity - body.length);
  if (padCount > 0) {
    const anchor = body[0]?.key || body[0]?.label || healthTimeline.value.startLabel || "";
    const preceding = buildPrecedingKeys(anchor, padCount, trendGranularity.value);
    const map = healthBucketMap.value;
    const mapped = preceding.map((p) => {
      const hit = map.get(p.key);
      const dialogues = hit?.dialogues ?? 0;
      const rawSuccess = hit?.success ?? 0;
      const rawFailed = hit?.failed ?? 0;
      const extractedRequests = hit?.requests ?? 0;
      const sampleRequests = rawSuccess + rawFailed;
      const usageRequests = hit?.usage ?? 0;
      const requests = extractedRequests > 0 ? extractedRequests : (usageRequests > 0 ? usageRequests : sampleRequests);
      const requestsEstimated = extractedRequests <= 0 && usageRequests > 0 && (hit?.usageEstimated ?? false);
      const failed = Math.max(0, rawFailed);
      const success = requests > 0
        ? Math.max(0, requests - Math.min(failed, requests))
        : rawSuccess;
      const successRate = requests > 0
        ? success / requests
        : (failed > 0 ? 0 : (sampleRequests > 0 ? rawSuccess / sampleRequests : null));
      return {
        key: `pre-${p.key}`,
        label: p.label,
        dialogues,
        success,
        failed,
        requests,
        requestsEstimated,
        successRate,
        level: healthLevelOf(successRate, requests > 0 || failed > 0, failed, requests),
        pad: false,
      };
    });
    body = [...mapped, ...body];
  }
  return body;
});

function healthCellTitle(cell: HealthDisplayCell): string {
  if (!cell.label && cell.pad) return "空档";
  if (!cell.label) return "—";
  const dialogues = cell.dialogues || 0;
  const dialoguePart = dialogues > 0 ? ` · 对话 ${formatTokens(dialogues)}` : "";
  if (cell.requests <= 0 && cell.failed <= 0) {
    return dialogues > 0
      ? `${cell.label}${dialoguePart} · 无请求`
      : `${cell.label} · 无请求`;
  }
  const reqPart = cell.requestsEstimated ? `≈${formatTokens(cell.requests)}` : formatTokens(cell.requests);
  const rateTxt = cell.successRate == null ? "—" : `${(cell.successRate * 100).toFixed(1)}%`;
  const failPart = cell.failed > 0
    ? ` · ⚠ 失败 ${formatTokens(cell.failed)}`
    : ` · 失败 0`;
  return `${cell.label}${dialoguePart} · 请求 ${reqPart} · 成功 ${formatTokens(cell.success)}${failPart} · 成功率 ${rateTxt}`;
}

// —— ECharts 交互配置 ——
const trendChartOption = computed<EChartsOption>(() => {
  const isDark = preferences.theme === "dark" || (preferences.theme === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  const textColor = isDark ? "#94a3b8" : "#64748b";
  const gridLineColor = isDark ? "rgba(255, 255, 255, 0.06)" : "rgba(0, 0, 0, 0.06)";
  const labels = trendDetail.value.map((item) => formatDetailTime(item.label));

  if (trendMetric.value === "breakdown") {
    return {
      tooltip: {
        trigger: "axis",
        axisPointer: { type: "shadow" },
        backgroundColor: isDark ? "rgba(15, 23, 42, 0.95)" : "rgba(255, 255, 255, 0.95)",
        borderColor: isDark ? "rgba(255, 255, 255, 0.15)" : "rgba(0, 0, 0, 0.1)",
        textStyle: { color: isDark ? "#f8fafc" : "#0f172a", fontSize: 12 },
      },
      legend: {
        data: ["输入 Tokens", "输出 Tokens", "缓存读取", "缓存写入", "推理 Tokens"],
        textStyle: { color: textColor, fontSize: 11 },
        top: 0,
        right: 10,
      },
      grid: { left: 45, right: 15, top: 35, bottom: 25 },
      xAxis: {
        type: "category",
        data: labels,
        axisLine: { lineStyle: { color: gridLineColor } },
        axisLabel: { color: textColor, fontSize: 10 },
      },
      yAxis: {
        type: "value",
        axisLabel: { color: textColor, fontSize: 10, formatter: (v: number) => formatCompact(v) },
        splitLine: { lineStyle: { color: gridLineColor, type: "dashed" } },
      },
      series: [
        {
          name: "输入 Tokens",
          type: "bar",
          stack: "total",
          data: trendDetail.value.map((i) => i.input),
          itemStyle: { color: "#0284c7" },
        },
        {
          name: "输出 Tokens",
          type: "bar",
          stack: "total",
          data: trendDetail.value.map((i) => i.output),
          itemStyle: { color: "#10b981" },
        },
        {
          name: "缓存读取",
          type: "bar",
          stack: "total",
          data: trendDetail.value.map((i) => i.cacheRead),
          itemStyle: { color: "#8b5cf6" },
        },
        {
          name: "缓存写入",
          type: "bar",
          stack: "total",
          data: trendDetail.value.map((i) => i.cacheWrite),
          itemStyle: { color: "#a78bfa" },
        },
        {
          name: "推理 Tokens",
          type: "bar",
          stack: "total",
          data: trendDetail.value.map((i) => i.reasoning),
          itemStyle: { color: "#f59e0b" },
        },
      ],
    };
  }

  if (trendMetric.value === "requests") {
    const reqData = trendDetailList.value.map((i) => i.requests);
    return {
      tooltip: {
        trigger: "axis",
        backgroundColor: isDark ? "rgba(15, 23, 42, 0.95)" : "rgba(255, 255, 255, 0.95)",
        borderColor: isDark ? "rgba(255, 255, 255, 0.15)" : "rgba(0, 0, 0, 0.1)",
        textStyle: { color: isDark ? "#f8fafc" : "#0f172a", fontSize: 12 },
      },
      grid: { left: 45, right: 15, top: 20, bottom: 25 },
      xAxis: {
        type: "category",
        data: trendDetailList.value.map((i) => formatDetailTime(i.label)),
        axisLine: { lineStyle: { color: gridLineColor } },
        axisLabel: { color: textColor, fontSize: 10 },
      },
      yAxis: {
        type: "value",
        axisLabel: { color: textColor, fontSize: 10, formatter: (v: number) => formatTokens(v) },
        splitLine: { lineStyle: { color: gridLineColor, type: "dashed" } },
      },
      series: [
        {
          name: "API 请求数",
          type: "line",
          smooth: 0.3,
          symbol: "circle",
          symbolSize: 6,
          data: reqData,
          lineStyle: { width: 2.5, color: "#06b6d4" },
          itemStyle: { color: "#06b6d4" },
          areaStyle: {
            color: {
              type: "linear",
              x: 0,
              y: 0,
              x2: 0,
              y2: 1,
              colorStops: [
                { offset: 0, color: "rgba(6, 182, 212, 0.35)" },
                { offset: 1, color: "rgba(6, 182, 212, 0.0)" },
              ],
            },
          },
        },
      ],
    };
  }

  // 默认总用量折线面积图
  const values = trendSeries.value.map((i) => i.value);
  return {
    tooltip: {
      trigger: "axis",
      backgroundColor: isDark ? "rgba(15, 23, 42, 0.95)" : "rgba(255, 255, 255, 0.95)",
      borderColor: isDark ? "rgba(255, 255, 255, 0.15)" : "rgba(0, 0, 0, 0.1)",
      textStyle: { color: isDark ? "#f8fafc" : "#0f172a", fontSize: 12 },
      formatter: (params: any) => {
        const p = Array.isArray(params) ? params[0] : params;
        const index = p.dataIndex;
        const detail = trendDetail.value[index];
        if (!detail) return `${p.name}: ${formatCompact(p.value)}`;
        const hitRateStr = detail.cacheHitRate != null ? `${(detail.cacheHitRate * 100).toFixed(1)}%` : "—";
        return `
          <div style="font-weight: 600; margin-bottom: 4px;">${detail.label}</div>
          <div>总计: <strong>${formatCompact(detail.total)}</strong> (${formatTokens(detail.total)})</div>
          <div style="color: #0284c7;">输入: ${formatCompact(detail.input)}</div>
          <div style="color: #10b981;">输出: ${formatCompact(detail.output)}</div>
          <div style="color: #8b5cf6;">缓存读取: ${formatCompact(detail.cacheRead)}</div>
          <div style="color: #a78bfa;">缓存写入: ${formatCompact(detail.cacheWrite)} (命中率 ${hitRateStr})</div>
          ${detail.reasoning > 0 ? `<div style="color: #f59e0b;">推理: ${formatCompact(detail.reasoning)}</div>` : ""}
          <div style="color: #06b6d4;">对话: ${detail.sessions} 轮</div>
        `;
      },
    },
    grid: { left: 45, right: 15, top: 20, bottom: 25 },
    xAxis: {
      type: "category",
      data: labels,
      axisLine: { lineStyle: { color: gridLineColor } },
      axisLabel: { color: textColor, fontSize: 10 },
    },
    yAxis: {
      type: "value",
      axisLabel: { color: textColor, fontSize: 10, formatter: (v: number) => formatCompact(v) },
      splitLine: { lineStyle: { color: gridLineColor, type: "dashed" } },
    },
    series: [
      {
        name: "Tokens",
        type: "line",
        data: values,
        smooth: 0.35,
        symbol: "circle",
        symbolSize: 6,
        lineStyle: { width: 3, color: "#10b981" },
        itemStyle: { color: "#10b981", borderColor: isDark ? "#0f172a" : "#ffffff", borderWidth: 2 },
        areaStyle: {
          color: {
            type: "linear",
            x: 0,
            y: 0,
            x2: 0,
            y2: 1,
            colorStops: [
              { offset: 0, color: "rgba(16, 185, 129, 0.4)" },
              { offset: 1, color: "rgba(16, 185, 129, 0.0)" },
            ],
          },
        },
      },
    ],
  };
});

watch(
  () => [healthTimeline.value.nodeCount, store.tokenStatsFrom.value, store.tokenStatsTo.value, trendGranularity.value],
  () => nextTick(() => measureHealthGrid()),
);

watch(
  () => refreshLogs.value.length,
  () => nextTick(() => {
    if (refreshLogListRef.value) {
      refreshLogListRef.value.scrollTop = refreshLogListRef.value.scrollHeight;
    }
  }),
);

// 反代网关模式轮询：与本地终端模式 5 秒快照刷新节奏一致
let proxyRefreshTimer: number | null = null;

onMounted(() => {
  tokenStatsPageMounted = true;
  // 预载反代配置与渠道模型缓存（会话级单例）：保证首次打开模型映射弹窗时分析模型下拉即时有值
  void mappingProxy.loadProxyData().catch(() => {});
  // 反代网关页：挂载即拉取一次，此后 5 秒轮询保持近实时
  if (statsMode.value === "proxy") {
    void proxyStore.loadProxyTokenUsage(store.tokenStatsFrom.value, store.tokenStatsTo.value);
    proxyRefreshTimer = window.setInterval(() => {
      void proxyStore.loadProxyTokenUsage(store.tokenStatsFrom.value, store.tokenStatsTo.value);
    }, 5_000);
  }
  if (localTokenStatsAvailable && statsMode.value === "local") {
    void store.loadTokenModelMappings();
    listen<TokenCollectorProgress>("token-collector-progress", ({ payload }) => {
      appendRefreshLog(payload);
    }, { local: true }).then((unlisten) => {
      if (!tokenStatsPageMounted) unlisten();
      else unlistenTokenCollectorProgress = unlisten;
    });
  }

  nextTick(() => {
    measureHealthGrid();
    if (typeof ResizeObserver !== "undefined" && healthGridRef.value) {
      healthRo = new ResizeObserver(() => measureHealthGrid());
      healthRo.observe(healthGridRef.value);
    }
    window.addEventListener("resize", measureHealthGrid);
  });
});

onBeforeUnmount(() => {
  tokenStatsPageMounted = false;
  if (proxyRefreshTimer != null) {
    window.clearInterval(proxyRefreshTimer);
    proxyRefreshTimer = null;
  }
  unlistenTokenCollectorProgress?.();
  unlistenTokenCollectorProgress = undefined;
  healthRo?.disconnect();
  healthRo = null;
  window.removeEventListener("resize", measureHealthGrid);
});
</script>

<template>
  <main class="token-stats-page tt-dashboard">
    <!-- 顶部宏观智控驾驶舱 (Macro Cockpit Bar) -->
    <header class="tt-cockpit-bar">
      <div class="tt-cockpit-left">
        <div class="tt-brand-section">
          <div class="tt-eyebrow-row">
            <span class="tt-live-dot" />
            <span class="tt-eyebrow-text">Token 用量分析中心</span>
          </div>
          <div class="tt-title-row">
            <h1>{{ statsMode === 'local' ? '本地 Token 统计' : '网关 Token 统计' }}</h1>
            <!-- 数据来源标签：客户端日志采集（本地）/ 服务端网关记账（网关） -->
            <span
              class="tt-mode-tab active"
              role="status"
              :title="statsMode === 'local' ? '扫描本机各 AI 工具的本地日志文件' : '模型反代网关的转发记账统计'"
            >{{ statsMode === 'local' ? '客户端 · 本地采集' : '服务端 · 反代网关' }}</span>
          </div>
          <p class="tt-cockpit-subtitle">
            <template v-if="statsMode === 'local'">
              本地终端日志采集 · SQLite 快照 · 覆盖 <strong>{{ bySource.length }}</strong> 款 AI 工具与 <strong>{{ byModel.length }}</strong> 个模型
            </template>
            <template v-else>
              反代网关请求记账 · 聚合表持久化 · 覆盖 <strong>{{ bySource.length }}</strong> 类客户端 · <strong>{{ byModel.length }}</strong> 个模型 · <strong>{{ projectUsage.length }}</strong> 个渠道
            </template>
          </p>
        </div>
      </div>

      <div class="tt-cockpit-right">
        <button
          v-if="statsMode === 'local'"
          type="button"
          class="tt-btn-rebuild"
          :disabled="!store.tokenCollectorSyncing.value && (store.tokenStatsLoading.value || store.tokenUsageLoading.value)"
          @click="openRefreshDialog"
        >
          <span :class="{ 'is-spinning': store.tokenStatsLoading.value || store.tokenCollectorSyncing.value }" v-html="icons.restore" />
          <span>{{ store.tokenCollectorSyncing.value ? "查看重建日志" : "重建统计" }}</span>
        </button>

        <button
          v-if="statsMode === 'local'"
          type="button"
          class="tt-btn-secondary"
          @click="openAgentDialog"
        >
          <span v-html="icons.cpu" />
          <span>本地 Agent</span>
          <span v-if="detectedAgentsCount > 0" class="tt-agent-count-chip">{{ detectedAgentsCount }} 在线</span>
        </button>

        <button
          v-if="statsMode === 'local'"
          type="button"
          class="tt-btn-secondary"
          title="用 AI 分析原始模型名与正式模型的映射关系"
          @click="openMappingDialog"
        >
          <span v-html="icons.link" />
          <span>模型映射</span>
          <span v-if="mappingPendingCount > 0" class="tt-agent-count-chip">{{ mappingPendingCount }} 待定</span>
        </button>

        <button
          type="button"
          class="tt-btn-secondary"
          @click="openExportDialog"
        >
          <span v-html="icons.download" />
          <span>导出报表</span>
        </button>
      </div>
    </header>

    <!-- 标题下方的筛选工具条：日期选择 + 维度弹窗按钮 -->
    <div class="tt-filter-toolbar">
      <DateRangeDropdown
        v-model:from="rangeFrom"
        v-model:to="rangeTo"
        @apply="store.onRangeChange()"
      />

      <div class="tt-cockpit-pills-group">
        <button
          type="button"
          class="tt-pill-btn"
          title="查看工具明细"
          @click="toolsModalOpen = true"
        >
          <span v-html="icons.cpu" />
          <span>工具 ({{ bySource.length }})</span>
        </button>

        <button
          type="button"
          class="tt-pill-btn"
          title="查看模型排行榜"
          @click="modelsModalOpen = true"
        >
          <span v-html="icons.database" />
          <span>模型 ({{ byModel.length }})</span>
        </button>

        <button
          type="button"
          class="tt-pill-btn"
          :title="statsMode === 'local' ? '查看各项目与工作区用量透视' : '查看各渠道用量透视'"
          @click="projectsModalOpen = true"
        >
          <span v-html="icons.folder" />
          <span>{{ statsMode === 'local' ? '项目' : '渠道' }} ({{ projectUsage.length }})</span>
        </button>

        <button
          type="button"
          class="tt-pill-btn"
          title="查看逐日逐时时序总账"
          @click="auditModalOpen = true"
        >
          <span v-html="icons.sliders" />
          <span>明细总账</span>
        </button>

        <button
          v-if="statsMode === 'local'"
          type="button"
          class="tt-pill-btn"
          title="让 AI 解读当前时间范围的用量数据，每个结论可追溯到证据"
          @click="openInsightDialog"
        >
          <span v-html="icons.sparkles" />
          <span>AI 洞察 ({{ insightEvidenceCount }})</span>
        </button>

        <button
          type="button"
          class="tt-pill-btn"
          title="查看请求健康矩阵明细"
          @click="healthModalOpen = true"
        >
          <span v-html="icons.activity" />
          <span>健康矩阵</span>
        </button>
      </div>
    </div>

    <!-- 首页零滚动条主视口 (No-Scroll Viewport Layout) -->
    <div class="tt-dashboard-body">
      <!-- 错误提示 -->
      <div v-if="activeError" class="tt-error-banner" role="alert">
        <span class="tt-error-icon" v-html="icons.alert" />
        <div class="tt-error-content">
          <strong>{{ statsMode === 'local' ? '读取 Token 数据异常' : '读取反代统计数据异常' }}</strong>
          <p>{{ activeError }}</p>
          <small v-if="statsMode === 'local'">OpenHub 会直接读取 Codex, Claude, Cursor, Antigravity, OpenCode, Kiro, Goose, Zed, Copilot 与 CatPawAI 的本地记录。</small>
          <small v-else>反代统计数据来自模型反代网关的 channel_daily_stats / channel_hourly_stats 聚合表，请确认网关已启用并产生过转发请求。</small>
        </div>
      </div>

      <!-- 加载中 -->
      <div v-if="activeLoading && !activeUsage" class="tt-loading-card">
        <div class="tt-loading-spinner" />
        <p>{{ statsMode === 'local' ? '正在读取本地 SQLite 数据库用量快照…' : '正在读取反代网关聚合统计数据…' }}</p>
      </div>

      <template v-else>
        <!-- ROW 1: 4 大核心 KPI 指标卡 (Compact Bento Deck) -->
        <section class="tt-kpi-deck" aria-label="核心指标大盘">
          <!-- KPI 1: Token 消耗大盘 -->
          <div class="tt-kpi-card tt-kpi-total">
            <div class="tt-kpi-card-inner">
              <div class="tt-kpi-header">
                <span class="tt-kpi-tag is-emerald">
                  <span v-html="icons.chart" />
                  <span>总用量</span>
                </span>
                <span class="tt-kpi-badge-hit is-good">
                  输出占比 {{ shareOf(rangeSplits.output, bucketTotal.total).toFixed(1) }}%
                </span>
              </div>
              <div class="tt-kpi-main-val">
                <strong>{{ formatCompact(bucketTotal.total) }}</strong>
                <span class="tt-kpi-unit">Tokens</span>
              </div>
              <div class="tt-kpi-progress-bar">
                <div
                  class="tt-prog-seg is-in"
                  :style="{ width: `${shareOf(rangeSplits.input, bucketTotal.total)}%` }"
                  :title="`输入: ${formatCompact(rangeSplits.input)} (${shareOf(rangeSplits.input, bucketTotal.total).toFixed(1)}%)`"
                />
                <div
                  class="tt-prog-seg is-out"
                  :style="{ width: `${shareOf(rangeSplits.output, bucketTotal.total)}%` }"
                  :title="`输出: ${formatCompact(rangeSplits.output)} (${shareOf(rangeSplits.output, bucketTotal.total).toFixed(1)}%)`"
                />
                <div
                  class="tt-prog-seg is-cache"
                  :style="{ width: `${shareOf(cacheBreakdown.read, bucketTotal.total)}%` }"
                  :title="`缓存: ${formatCompact(rangeSplits.cache)} (读 ${formatCompact(cacheBreakdown.read)} / 写 ${formatCompact(cacheBreakdown.write)}, 读占比 ${shareOf(cacheBreakdown.read, bucketTotal.total).toFixed(1)}%)`"
                />
              </div>
              <div class="tt-kpi-sub-pills">
                <span class="tt-sub-pill in"><i></i>输入 {{ formatCompact(rangeSplits.input) }}</span>
                <span class="tt-sub-pill out"><i></i>输出 {{ formatCompact(rangeSplits.output) }}</span>
                <span class="tt-sub-pill cache"><i></i>缓存读 {{ formatCompact(cacheBreakdown.read) }} · 写 {{ formatCompact(cacheBreakdown.write) }}</span>
                <span v-if="rangeSplits.reasoning > 0" class="tt-sub-pill reasoning"><i></i>推理 {{ formatCompact(rangeSplits.reasoning) }}</span>
              </div>
            </div>
          </div>

          <!-- KPI 2: 日均与连击活跃 -->
          <div class="tt-kpi-card">
            <div class="tt-kpi-card-inner">
              <div class="tt-kpi-header">
                <span class="tt-kpi-tag is-orange">
                  <span v-html="icons.flame" />
                  <span>消耗速率与连续</span>
                </span>
                <span v-if="streakDays > 1" class="tt-kpi-streak-pill">
                  🔥 连续 {{ streakDays }} 天
                </span>
              </div>
              <div class="tt-kpi-main-val">
                <strong>{{ formatCompact(dailyAverage) }}</strong>
                <span class="tt-kpi-unit">/ 活跃日均</span>
              </div>
              <div class="tt-kpi-meta-text">
                <span>跨度 <strong>{{ rangeDays }}</strong> 天 · 活跃 <strong>{{ activeDays }}</strong> 天</span>
                <span v-if="rangeDays > 0" class="tt-active-rate-badge">活跃率 {{ ((activeDays / rangeDays) * 100).toFixed(0) }}%</span>
              </div>
              <div class="tt-kpi-footer-note">
                统计区间: <code>{{ rangeLabel }}</code>
              </div>
            </div>
          </div>

          <!-- KPI 3: 会话与并发 API 调用（反代模式无对话概念，替换为请求成败口径） -->
          <div class="tt-kpi-card">
            <div class="tt-kpi-card-inner">
              <div class="tt-kpi-header">
                <span class="tt-kpi-tag is-blue">
                  <span v-html="icons.activity" />
                  <span>{{ statsMode === 'local' ? '对话与请求' : '请求与成功率' }}</span>
                </span>
                <span class="tt-kpi-badge-rate">
                  {{ healthTimeline.successRate != null ? (healthTimeline.successRate * 100).toFixed(1) + "% 成功率" : "—" }}
                </span>
              </div>
              <template v-if="statsMode === 'local'">
                <div class="tt-kpi-main-val">
                  <strong>{{ formatTokens(healthTimeline.totalDialogues) }}</strong>
                  <span class="tt-kpi-unit">轮对话</span>
                </div>
                <div class="tt-kpi-meta-text">
                  <span>真实 API 调用 <strong>{{ formatTokens(healthTimeline.totalRequests) }}</strong> 次</span>
                </div>
                <div class="tt-kpi-multiplier-pill">
                  <span>平均每轮触发 <strong>{{ requestsPerTurnLabel }}</strong> 次模型调用</span>
                </div>
              </template>
              <template v-else>
                <div class="tt-kpi-main-val">
                  <strong>{{ formatTokens(healthTimeline.totalRequests) }}</strong>
                  <span class="tt-kpi-unit">次转发请求</span>
                </div>
                <div class="tt-kpi-meta-text">
                  <span>成功 <strong class="text-success">{{ formatTokens(healthTimeline.totalSuccess) }}</strong> · 失败 <strong :class="{ 'text-danger': healthTimeline.totalFailed > 0 }">{{ formatTokens(healthTimeline.totalFailed) }}</strong></span>
                </div>
                <div class="tt-kpi-multiplier-pill">
                  <span>反代按真实 HTTP 状态码记账 · 成功率口径比本地估算更精确</span>
                </div>
              </template>
            </div>
          </div>

          <!-- KPI 4: 缓存与复用效能 -->
          <div class="tt-kpi-card">
            <div class="tt-kpi-card-inner">
              <div class="tt-kpi-header">
                <span class="tt-kpi-tag is-purple">
                  <span v-html="icons.sparkles" />
                  <span>缓存与效能</span>
                </span>
                <span
                  v-if="cacheHitRate != null && cacheHitRate > 0"
                  class="tt-kpi-badge-hit"
                  :class="cacheHitRateRating.class"
                >
                  {{ cacheHitRateRating.label }}
                </span>
                <span v-else class="tt-kpi-badge-hit is-none">暂无缓存</span>
              </div>
              <div class="tt-kpi-main-val">
                <template v-if="cacheHitRate != null && cacheHitRate > 0">
                  <strong>{{ (cacheHitRate * 100).toFixed(1) }}</strong>
                  <span class="tt-kpi-unit">% 命中率</span>
                </template>
                <template v-else>
                  <strong>0.0</strong>
                  <span class="tt-kpi-unit">% 命中率</span>
                </template>
              </div>
              <div class="tt-kpi-meta-text">
                <span>读取复用 <strong>{{ formatCompact(cacheBreakdown.read) }}</strong> · 创建写入 <strong>{{ formatCompact(cacheBreakdown.write) }}</strong></span>
              </div>
              <div class="tt-kpi-footer-note">
                <span v-if="cacheBreakdown.speedup">
                  ⚡ 吞吐加速 ≈ <strong>{{ cacheBreakdown.speedup }}x</strong> · 减少重复传输与响应延迟
                </span>
                <span v-else-if="cacheBreakdown.read > 0">
                  ⚡ 命中复用 {{ formatTokens(cacheBreakdown.read) }} Tokens
                </span>
                <span v-else>
                  Prompt Caching 命中可大幅加速首字与响应时间
                </span>
              </div>
            </div>
          </div>
        </section>

        <!-- 4 大核心全景图表与分布四等分大盘 (Equal 4-Quadrant Grid) -->
        <section class="tt-quad-grid">
          <!-- 1. 趋势图卡片 -->
          <div class="tt-card tt-chart-card">
            <header class="tt-card-header">
              <div class="tt-card-title-wrap">
                <h2>Token 消耗趋势</h2>
                <p>按 {{ trendUnitLabel() }} 聚合 · 共 {{ trendSeries.length }} 个时序节点</p>
              </div>
              <div class="tt-metric-switches">
                <button
                  type="button"
                  class="tt-metric-btn"
                  :class="{ active: trendMetric === 'total' }"
                  @click="trendMetric = 'total'"
                >总用量</button>
                <button
                  type="button"
                  class="tt-metric-btn"
                  :class="{ active: trendMetric === 'breakdown' }"
                  @click="trendMetric = 'breakdown'"
                >分项堆叠</button>
                <button
                  type="button"
                  class="tt-metric-btn"
                  :class="{ active: trendMetric === 'requests' }"
                  @click="trendMetric = 'requests'"
                >API 请求数</button>
              </div>
            </header>
            <div class="tt-card-body tt-chart-body">
              <EChart v-if="trendSeries.length" :option="trendChartOption" height="100%" />
              <div v-else class="tt-empty-state">当前时间区间内暂无时序记录</div>
            </div>
          </div>

          <!-- 2. 请求健康热力时间线 -->
          <div class="tt-card tt-health-card">
            <header class="tt-card-header">
              <div class="tt-card-title-wrap">
                <h2>请求健康矩阵</h2>
                <p>色阶按成功率：≥99% 绿 · 95–99% 浅绿 · 85–95% 黄 · 70–85% 橙 · &lt;70% 红</p>
              </div>
              <button type="button" class="tt-text-btn" @click="healthModalOpen = true">
                查看明细 ➔
              </button>
            </header>
            <div class="tt-card-body tt-health-body">
              <div v-if="healthTimeline.cells.length" class="tt-health-wrapper">
                <!-- 顶部 4 大健康遥测微指标 -->
                <div class="tt-health-kpi-bar">
                  <div class="tt-hk-card">
                    <span class="tt-hk-lbl">综合成功率</span>
                    <div class="tt-hk-val-box">
                      <strong class="tt-hk-num" :class="healthTimeline.successRate != null && healthTimeline.successRate < 0.95 ? 'text-warning' : 'text-success'">
                        {{ healthTimeline.successRate != null ? (healthTimeline.successRate * 100).toFixed(1) + '%' : '100%' }}
                      </strong>
                      <span class="tt-hk-badge" :class="healthStatusInfo.class">{{ healthStatusInfo.label }}</span>
                    </div>
                  </div>
                  <div class="tt-hk-card">
                    <span class="tt-hk-lbl">请求吞吐量</span>
                    <div class="tt-hk-val-box">
                      <strong class="tt-hk-num">{{ formatTokens(healthTimeline.totalRequests) }}</strong>
                      <small class="tt-hk-unit">次</small>
                    </div>
                  </div>
                  <div class="tt-hk-card">
                    <span class="tt-hk-lbl">异常/失败</span>
                    <div class="tt-hk-val-box">
                      <strong class="tt-hk-num" :class="{ 'text-danger': healthTimeline.totalFailed > 0 }">
                        {{ formatTokens(healthTimeline.totalFailed) }}
                      </strong>
                      <small class="tt-hk-unit">次</small>
                    </div>
                  </div>
                  <div class="tt-hk-card">
                    <span class="tt-hk-lbl">活跃监测时段</span>
                    <div class="tt-hk-val-box">
                      <strong class="tt-hk-num">{{ healthTimeline.activeCount }}</strong>
                      <small class="tt-hk-unit">/ {{ healthTimeline.nodeCount }}</small>
                    </div>
                  </div>
                </div>

                <!-- 热力矩阵主体 -->
                <div
                  ref="healthGridRef"
                  class="tt-health-grid"
                  :style="{ gridTemplateRows: `repeat(${HEALTH_ROWS}, 1fr)` }"
                >
                  <div
                    v-for="cell in healthDisplayCells"
                    :key="cell.key"
                    class="tt-health-cell"
                    :class="['lv' + cell.level, { 'is-pad': cell.pad }]"
                    :title="healthCellTitle(cell)"
                  />
                </div>

                <!-- 底部图例 -->
                <div class="tt-health-legend">
                  <span>故障频发</span>
                  <span class="tt-health-cell lv1" title="成功率 < 70%" />
                  <span class="tt-health-cell lv2" title="70% ~ 85%" />
                  <span class="tt-health-cell lv3" title="85% ~ 95%" />
                  <span class="tt-health-cell lv4" title="95% ~ 99%" />
                  <span class="tt-health-cell lv5" title="≥ 99%" />
                  <span>极度健康</span>
                  <span class="tt-health-cell lv0" />
                  <span class="muted">无请求</span>
                  <span class="tt-legend-meta">· 共 {{ healthTimeline.nodeCount }} 节点 · 活跃 {{ healthTimeline.activeCount }}</span>
                </div>
              </div>
              <div v-else class="tt-empty-state">当前时间区间内暂无请求健康记录</div>
            </div>
          </div>

          <!-- 3. 工具消耗分布 -->
          <div class="tt-card tt-preview-card">
            <header class="tt-card-header">
              <div>
                <h3>主要工具消耗分布</h3>
                <p>Top 5 客户端用量占比</p>
              </div>
              <button type="button" class="tt-text-btn" @click="toolsModalOpen = true">
                查看明细 ➔
              </button>
            </header>
            <div class="tt-card-body tt-preview-body">
              <div v-for="(item, idx) in topSources.slice(0, 5)" :key="item.source" class="tt-bar-row">
                <span class="tt-bar-dot" :style="{ background: providerColor(item.source, idx) }" />
                <span class="tt-bar-label">{{ sourceLabel(item.source) }}</span>
                <div class="tt-bar-track">
                  <div class="tt-bar-fill" :style="{ width: `${shareOf(item.totalTokens, totalTokensAll)}%`, background: providerColor(item.source, idx) }" />
                </div>
                <span class="tt-bar-pct">{{ shareOf(item.totalTokens, totalTokensAll).toFixed(1) }}%</span>
                <strong class="tt-bar-val">{{ formatCompact(item.totalTokens) }}</strong>
              </div>
            </div>
          </div>

          <!-- 4. 模型消耗排行 -->
          <div class="tt-card tt-preview-card">
            <header class="tt-card-header">
              <div>
                <h3>主要模型消耗排行</h3>
                <p>Top 5 旗舰模型用量占比</p>
              </div>
              <button type="button" class="tt-text-btn" @click="modelsModalOpen = true">
                查看排行榜 ➔
              </button>
            </header>
            <div class="tt-card-body tt-preview-body">
              <div v-for="(model, idx) in topModels.slice(0, 5)" :key="model.model" class="tt-bar-row">
                <span class="tt-bar-dot" :style="{ background: providerColor(model.model, idx) }" />
                <span class="tt-bar-label font-mono" :title="model.model">{{ model.model }}</span>
                <div class="tt-bar-track">
                  <div class="tt-bar-fill" :style="{ width: `${shareOf(model.totalTokens, totalTokensAll)}%`, background: providerColor(model.model, idx) }" />
                </div>
                <span class="tt-bar-pct">{{ shareOf(model.totalTokens, totalTokensAll).toFixed(1) }}%</span>
                <strong class="tt-bar-val">{{ formatCompact(model.totalTokens) }}</strong>
              </div>
            </div>
          </div>
        </section>
      </template>
    </div>

    <!-- ============================================================
         4 大深度分析弹窗 (Popups / Modal Drawers)
         ============================================================ -->

    <!-- 1. 工具与来源全景分析弹窗 (Tools Modal) -->
    <Transition name="tt-modal-fade">
      <div v-if="toolsModalOpen" class="tt-modal-backdrop">
        <section class="tt-modal-card is-wide" role="dialog" aria-modal="true">
          <header class="tt-modal-header">
            <div>
              <h2>工具与来源全景分析</h2>
              <p>覆盖本机探测到的所有 AI 编程工具与编辑器客户端用量</p>
            </div>
            <button type="button" class="tt-modal-close-btn" aria-label="关闭" @click="toolsModalOpen = false">×</button>
          </header>

          <div class="tt-modal-body">
            <div class="tt-filter-bar">
              <label class="tt-search-input">
                <span v-html="icons.search" />
                <input v-model="sourceSearch" type="search" placeholder="搜索 AI 工具、CLI 或编辑器…" />
              </label>
              <span class="tt-filter-count">共 {{ filteredSources.length }} 款工具</span>
            </div>

            <!-- 完整工具数据表 -->
            <div class="tt-table-wrap">
              <AppTable
                :rows="filteredSources"
                :columns="sourceColumns"
                :row-key="(item: any) => item.source"
                :page-size="10"
                empty-text="没有匹配的工具数据"
              >
                <template #cell-source="{ row }">
                  <div class="tt-cell-with-dot">
                    <span class="tt-bar-dot" :style="{ background: providerColor(row.source) }" />
                    <strong>{{ sourceLabel(row.source) }}</strong>
                    <code class="tt-muted-code">({{ row.source }})</code>
                  </div>
                </template>
                <template #cell-totalTokens="{ row }"><strong>{{ formatCompact(row.totalTokens) }}</strong></template>
                <template #cell-share="{ row }">{{ shareOf(row.totalTokens, totalTokensAll).toFixed(2) }}%</template>
                <template #cell-inputTokens="{ row }">{{ formatCompact(row.inputTokens) }}</template>
                <template #cell-outputTokens="{ row }">{{ formatCompact(row.outputTokens) }}</template>
                <template #cell-cacheReadTokens="{ row }">{{ formatCompact(row.cacheReadTokens) }}</template>
                <template #cell-cacheWriteTokens="{ row }">{{ formatCompact(row.cacheWriteTokens) }}</template>
                <template #cell-cacheHitRate="{ row }">{{ formatRate(row.cacheHitRate) }}</template>
                <template #cell-reasoningTokens="{ row }">{{ formatCompact(row.reasoningTokens) }}</template>
                <template #cell-conversations="{ row }">{{ formatTokens(row.conversations) }}</template>
                <template #cell-requests="{ row }">{{ formatTokens(row.requests) }}</template>
              </AppTable>
            </div>
          </div>
        </section>
      </div>
    </Transition>

    <!-- 2. 模型排行榜与家族透视弹窗 (Models Modal) -->
    <Transition name="tt-modal-fade">
      <div v-if="modelsModalOpen" class="tt-modal-backdrop">
        <section class="tt-modal-card is-wide" role="dialog" aria-modal="true">
          <header class="tt-modal-header">
            <div>
              <h2>模型排行榜与家族透视</h2>
              <p>按 Token 消耗总量倒序排列 · 同系列模型智能归并</p>
            </div>
            <button type="button" class="tt-modal-close-btn" aria-label="关闭" @click="modelsModalOpen = false">×</button>
          </header>

          <div class="tt-modal-body">
            <div class="tt-filter-bar">
              <label class="tt-search-input">
                <span v-html="icons.search" />
                <input v-model="modelSearch" type="search" placeholder="搜索模型名称（如 claude-3-7, gpt-4o, r1）…" />
              </label>
              <span class="tt-filter-count">共 {{ filteredModels.length }} 款模型</span>
            </div>

            <div class="tt-table-wrap">
              <AppTable
                :rows="filteredModels"
                :columns="modelColumns"
                :row-key="(item: any) => item.model"
                :page-size="15"
                empty-text="没有匹配的模型数据"
              >
                <template #cell-model="{ row }">
                  <div class="tt-cell-with-dot">
                    <span class="tt-bar-dot" :style="{ background: providerColor(row.model) }" />
                    <strong class="font-mono">{{ row.model }}</strong>
                  </div>
                </template>
                <template #cell-totalTokens="{ row }"><strong>{{ formatCompact(row.totalTokens) }}</strong></template>
                <template #cell-share="{ row }">{{ shareOf(row.totalTokens, totalTokensAll).toFixed(2) }}%</template>
                <template #cell-inputTokens="{ row }">{{ formatCompact(row.inputTokens) }}</template>
                <template #cell-outputTokens="{ row }">{{ formatCompact(row.outputTokens) }}</template>
                <template #cell-cacheReadTokens="{ row }">{{ formatCompact(row.cacheReadTokens) }}</template>
                <template #cell-cacheWriteTokens="{ row }">{{ formatCompact(row.cacheWriteTokens) }}</template>
                <template #cell-cacheHitRate="{ row }">{{ formatRate(row.cacheHitRate) }}</template>
                <template #cell-reasoningTokens="{ row }">{{ formatCompact(row.reasoningTokens) }}</template>
                <template #cell-conversations="{ row }">{{ formatTokens(row.conversations) }}</template>
                <template #cell-requests="{ row }">{{ formatTokens(row.requests) }}</template>
              </AppTable>
            </div>
          </div>
        </section>
      </div>
    </Transition>

    <!-- 3. 项目与工作区透视弹窗 (Projects Modal) -->
    <Transition name="tt-modal-fade">
      <div v-if="projectsModalOpen" class="tt-modal-backdrop">
        <section class="tt-modal-card is-wide" role="dialog" aria-modal="true">
          <header class="tt-modal-header">
            <div>
              <h2>{{ statsMode === 'local' ? '项目与工作区透视' : '渠道用量透视' }}</h2>
              <p>{{ statsMode === 'local' ? '从本地日志中自动提取的项目目录与工作区用量' : '反代转发按渠道维度汇总的用量透视' }}</p>
            </div>
            <button type="button" class="tt-modal-close-btn" aria-label="关闭" @click="projectsModalOpen = false">×</button>
          </header>

          <div class="tt-modal-body">
            <div class="tt-filter-bar">
              <label class="tt-search-input">
                <span v-html="icons.search" />
                <input v-model="projectSearch" type="search" placeholder="按项目名称或路径过滤…" />
              </label>
              <span class="tt-filter-count">共 {{ filteredProjects.length }} 个工作区</span>
            </div>

            <div class="tt-table-wrap">
              <AppTable
                :rows="filteredProjects"
                :columns="projectColumns"
                :row-key="(item: any) => item.project"
                :page-size="15"
                empty-text="没有匹配的项目记录"
              >
                <template #cell-project="{ row }">
                  <div class="tt-project-cell" :title="row.project">
                    <span v-if="row.project.includes('临时') || row.project.includes('独立') || row.project.includes('全局')" v-html="icons.chat" />
                    <span v-else v-html="icons.folder" />
                    <strong>{{ row.project }}</strong>
                  </div>
                </template>
                <template #cell-totalTokens="{ row }"><strong>{{ formatCompact(row.totalTokens) }}</strong></template>
                <template #cell-share="{ row }">{{ shareOf(row.totalTokens, totalTokensAll).toFixed(2) }}%</template>
                <template #cell-input="{ row }">{{ formatCompact(row.input) }}</template>
                <template #cell-output="{ row }">{{ formatCompact(row.output) }}</template>
                <template #cell-cacheRead="{ row }">{{ formatCompact(row.cacheRead) }}</template>
                <template #cell-cacheWrite="{ row }">{{ formatCompact(row.cacheWrite) }}</template>
                <template #cell-cacheHitRate="{ row }">{{ formatRate(row.cacheHitRate) }}</template>
                <template #cell-reasoning="{ row }">{{ formatCompact(row.reasoning) }}</template>
                <template #cell-sessions="{ row }">{{ formatTokens(row.sessions) }}</template>
                <template #cell-requests="{ row }">{{ row.requestsEstimated ? "≈" : "" }}{{ formatTokens(row.requests) }}</template>
              </AppTable>
            </div>
          </div>
        </section>
      </div>
    </Transition>

    <!-- 4. 逐日/逐时明细总账弹窗 (Audit Modal) -->
    <Transition name="tt-modal-fade">
      <div v-if="auditModalOpen" class="tt-modal-backdrop">
        <section class="tt-modal-card is-wide" role="dialog" aria-modal="true">
          <header class="tt-modal-header">
            <div>
              <h2>时序明细总账 (Granular Audit Ledger)</h2>
              <p>按当前所选时间跨度 · {{ trendUnitLabel() }} · 已过滤零用量空节点</p>
            </div>
            <button type="button" class="tt-modal-close-btn" aria-label="关闭" @click="auditModalOpen = false">×</button>
          </header>

          <div class="tt-modal-body">
            <div class="flex justify-between items-center">
              <span class="tt-filter-count">共 {{ trendDetailList.length }} 个时序节点</span>
              <button type="button" class="tt-btn-secondary" @click="exportDataAsCsv">
                <span v-html="icons.download" />
                <span>导出 CSV 表格</span>
              </button>
            </div>

            <div class="tt-table-wrap">
              <AppTable
                :rows="trendDetailList"
                :columns="dailyColumns"
                :row-key="(item: any) => item.label"
                :page-size="15"
                empty-text="该时间范围内没有时序记录"
              >
                <template #cell-label="{ row }">
                  <code>{{ row.label }}</code>
                </template>
                <template #cell-total="{ row }"><strong>{{ formatCompact(row.total) }}</strong></template>
                <template #cell-input="{ row }">{{ formatCompact(row.input) }}</template>
                <template #cell-output="{ row }">{{ formatCompact(row.output) }}</template>
                <template #cell-cacheRead="{ row }">{{ formatCompact(row.cacheRead) }}</template>
                <template #cell-cacheWrite="{ row }">{{ formatCompact(row.cacheWrite) }}</template>
                <template #cell-cacheHitRate="{ row }">{{ formatRate(row.cacheHitRate) }}</template>
                <template #cell-reasoning="{ row }">{{ formatCompact(row.reasoning) }}</template>
                <template #cell-sessions="{ row }">{{ formatTokens(row.sessions) }}</template>
                <template #cell-requests="{ row }">{{ row.requestsEstimated ? "≈" : "" }}{{ formatTokens(row.requests) }}</template>
              </AppTable>
            </div>
          </div>
        </section>
      </div>
    </Transition>

    <!-- 5. 请求健康矩阵明细弹窗 (Health Modal) -->
    <Transition name="tt-modal-fade">
      <div v-if="healthModalOpen" class="tt-modal-backdrop">
        <section class="tt-modal-card is-wide" role="dialog" aria-modal="true">
          <header class="tt-modal-header">
            <div>
              <h2>请求健康矩阵明细</h2>
              <p>每个时段的成功率、对话数与请求健康状态</p>
            </div>
            <button type="button" class="tt-modal-close-btn" aria-label="关闭" @click="healthModalOpen = false">×</button>
          </header>

          <div class="tt-modal-body">
            <span class="tt-filter-count">共 {{ healthTableRows.length }} 个时段</span>

            <div class="tt-table-wrap">
              <AppTable
                :rows="healthTableRows"
                :columns="healthColumns"
                :row-key="(item: any) => item.key || item.label"
                :page-size="15"
                empty-text="当前时间区间内暂无请求健康记录"
              >
                <template #cell-label="{ row }">
                  <div class="tt-cell-with-dot">
                    <span class="tt-bar-dot" :style="{ background: healthLevelColor(row.level) }" />
                    <code v-if="row.label">{{ row.label }}</code>
                    <span v-else class="muted">空档</span>
                  </div>
                </template>
                <template #cell-dialogues="{ row }">{{ formatTokens(row.dialogues) }}</template>
                <template #cell-requests="{ row }">{{ row.requestsEstimated ? "≈" : "" }}{{ formatTokens(row.requests) }}</template>
                <template #cell-success="{ row }">{{ formatTokens(row.success) }}</template>
                <template #cell-failed="{ row }">
                  <span :class="{ 'text-danger': row.failed > 0 }">{{ formatTokens(row.failed) }}</span>
                </template>
                <template #cell-successRate="{ row }">
                  <span :class="{ 'text-success': row.successRate !== '—' && Number(row.successRate) >= 99, 'text-warning': row.successRate !== '—' && Number(row.successRate) < 95 && Number(row.successRate) >= 70, 'text-danger': row.successRate !== '—' && Number(row.successRate) < 70 }">
                    {{ row.successRate }}
                  </span>
                </template>
                <template #cell-level="{ row }">
                  <span class="tt-health-cell" :class="'lv' + row.level" :title="'等级 ' + row.level" />
                </template>
              </AppTable>
            </div>
          </div>
        </section>
      </div>
    </Transition>

    <!-- 统计重建控制台弹窗 (Reconstruction Console Modal) -->
    <Transition name="tt-modal-fade">
      <div v-if="refreshDialogOpen" class="tt-modal-backdrop">
        <section class="tt-modal-card" role="dialog" aria-modal="true">
          <header class="tt-modal-header">
            <div>
              <h2>{{ refreshStatusTitle }}</h2>
              <p>{{ refreshStatusDescription }}</p>
            </div>
            <button type="button" class="tt-modal-close-btn" aria-label="关闭" @click="closeRefreshDialog">×</button>
          </header>

          <div class="tt-modal-body">
            <template v-if="refreshPhase === 'confirm'">
              <div class="tt-refresh-workflow">
                <div class="tt-wf-step">
                  <div class="tt-wf-num">1</div>
                  <div class="tt-wf-info">
                    <strong>清除本地快照与解析缓存</strong>
                    <small>删除旧解析索引与临时计算缓存，确保数据纯净</small>
                  </div>
                </div>
                <div class="tt-wf-step">
                  <div class="tt-wf-num">2</div>
                  <div class="tt-wf-info">
                    <strong>多端 AI 工具日志重新扫描</strong>
                    <small>完整读取 Codex, Claude, Cursor, Antigravity, OpenCode 等本地记录</small>
                  </div>
                </div>
                <div class="tt-wf-step">
                  <div class="tt-wf-num">3</div>
                  <div class="tt-wf-info">
                    <strong>重构 SQLite 数据库与前端快照</strong>
                    <small>建立小时/日/月多维聚合表，毫秒级即时呈现大盘</small>
                  </div>
                </div>
              </div>
              <div class="tt-refresh-tips">
                <span v-html="icons.info" />
                <p>重建过程仅读取本机日志，<strong>不会修改或删除任何外部 AI 工具的原始会话数据</strong>。</p>
              </div>
            </template>

            <template v-else>
              <div class="tt-refresh-running-bar" :class="`is-${refreshPhase}`">
                <span class="tt-state-icon" :class="{ 'is-spinning': refreshPhase === 'running' }">
                  {{ refreshPhase === "running" ? "↻" : (refreshPhase === "success" ? "✓" : "!") }}
                </span>
                <div>
                  <strong>{{ refreshStatusTitle }}</strong>
                  <p>{{ refreshStatusDescription }}</p>
                </div>
              </div>
              <div class="tt-log-terminal">
                <div class="tt-log-header">
                  <span>实时执行日志</span>
                  <span>{{ refreshLogs.length }} 条记录</span>
                </div>
                <ol ref="refreshLogListRef" class="tt-log-list">
                  <li v-for="entry in refreshLogs" :key="entry.id" :class="`is-${entry.status}`">
                    <time>{{ entry.time }}</time>
                    <span class="tt-log-stage">{{ refreshStageLabels[entry.stage] || entry.stage }}</span>
                    <p>{{ entry.message }}</p>
                    <i>{{ entry.status === "running" ? "…" : (entry.status === "success" ? "✓" : "!") }}</i>
                  </li>
                </ol>
              </div>
            </template>
          </div>

          <footer class="tt-modal-footer">
            <template v-if="refreshPhase === 'confirm'">
              <button type="button" class="tt-btn-cancel" @click="closeRefreshDialog">取消</button>
              <button type="button" class="tt-btn-primary" @click="startRefresh">
                <span v-html="icons.restore" />
                <span>开始完整重建</span>
              </button>
            </template>
            <template v-else>
              <span class="tt-footer-hint">
                {{ refreshPhase === "running" ? "后台运行中，可随时关闭此窗口。" : "重建已写入 SQLite 数据库。" }}
              </span>
              <button type="button" class="tt-btn-cancel" @click="closeRefreshDialog">
                {{ refreshPhase === "running" ? "后台运行" : "完成并关闭" }}
              </button>
            </template>
          </footer>
        </section>
      </div>
    </Transition>

    <!-- 模型映射弹窗（AI 分析：原始模型名 → 正式模型） -->
    <Transition name="tt-modal-fade">
      <div v-if="mappingDialogOpen" class="tt-modal-backdrop">
        <section class="tt-modal-card is-wide" role="dialog" aria-modal="true">
          <header class="tt-modal-header">
            <div>
              <h2>模型名称识别</h2>
              <p>AI 只生成可审核的正式模型建议；只有人工批准或手工设置后，结果才会改变统计归组。</p>
            </div>
            <button type="button" class="tt-modal-close-btn" aria-label="关闭" @click="closeMappingDialog">×</button>
          </header>

          <div class="tt-modal-body">
            <!-- 分析控制条 -->
            <div class="tt-mapping-controls">
              <label class="tt-mapping-field">
                <span>反代渠道</span>
                <CustomSelect
                  :options="mappingChannelOptions"
                  :model-value="mappingChannelId"
                  aria-label="选择反代渠道"
                  @update:model-value="mappingChannelId = String($event)"
                />
              </label>
              <label class="tt-mapping-field">
                <span>分析模型</span>
                <input
                  v-model="mappingModel"
                  type="text"
                  class="tt-mapping-model-input"
                  list="tt-mapping-model-suggestions"
                  placeholder="输入模型 ID，可自定义"
                  spellcheck="false"
                />
                <datalist id="tt-mapping-model-suggestions">
                  <option v-for="model in mappingModelSuggestions" :key="model" :value="model" />
                </datalist>
              </label>
              <button
                type="button"
                class="tt-btn-primary"
                :disabled="mappingInitializing || store.tokenModelAnalyzing.value || !mappingHasChannels || !mappingModel.trim()"
                @click="runMappingAnalyze(false)"
              >
                <span :class="{ 'is-spinning': store.tokenModelAnalyzing.value }" v-html="icons.sparkles" />
                <span>{{ store.tokenModelAnalyzing.value ? "识别中…" : "生成 AI 建议" }}</span>
              </button>
              <button
                type="button"
                class="tt-btn-cancel"
                :disabled="mappingInitializing || store.tokenModelAnalyzing.value || !mappingHasChannels || !mappingModel.trim()"
                @click="runMappingAnalyze(true)"
              >
                <span v-html="icons.restore" />
                <span>重新识别</span>
              </button>
            </div>
            <p v-if="!mappingHasChannels" class="tt-mapping-error">
              未检测到已启用的反代渠道，请先在「模型代理」页面启用一个渠道。
            </p>
            <p class="tt-mapping-hint">
              <span v-html="icons.info" />
              <span>
                识别请求只在本地桌面客户端经进程内网关入口发往所选渠道，无需开启网关服务；
                AI 不会创建新模型，也不会自动改变统计，建议必须人工审核后才生效。
              </span>
            </p>
            <p v-if="mappingInitializing" class="tt-mapping-hint">
              <span :class="{ 'is-spinning': true }" v-html="icons.restore" />
              <span>正在加载模型目录、渠道配置与本地映射队列…</span>
            </p>
            <p v-if="mappingInitializationError" class="tt-mapping-error">初始化失败：{{ mappingInitializationError }}</p>
            <p v-if="mappingCatalogError" class="tt-mapping-error">模型目录加载失败：{{ mappingCatalogError }}</p>
            <p v-if="store.tokenModelMappingsError.value" class="tt-mapping-error">映射队列错误：{{ store.tokenModelMappingsError.value }}</p>

            <div v-if="store.tokenModelAnalyzeProgress.value" class="tt-mapping-progress">
              <div>
                <strong>{{ store.tokenModelAnalyzeProgress.value.message }}</strong>
                <span>{{ store.tokenModelAnalyzeProgress.value.processed }} / {{ store.tokenModelAnalyzeProgress.value.total }}</span>
              </div>
              <div class="tt-mapping-progress-track">
                <i :style="{ width: `${store.tokenModelAnalyzeProgress.value.total ? (store.tokenModelAnalyzeProgress.value.processed / store.tokenModelAnalyzeProgress.value.total) * 100 : 0}%` }" />
              </div>
            </div>

            <!-- 分析报告 -->
            <div v-if="store.tokenModelAnalyzeReport.value" class="tt-mapping-report">
              <div class="tt-mapping-report-stats">
                <span>提交 <strong>{{ store.tokenModelAnalyzeReport.value.analyzed }}</strong></span>
                <span>跳过已批准 <strong>{{ store.tokenModelAnalyzeReport.value.skippedConfirmed }}</strong></span>
                <span>标准 <strong>{{ store.tokenModelAnalyzeReport.value.standardsUsed ?? 0 }}</strong></span>
                <span>待审核 <strong>{{ store.tokenModelAnalyzeReport.value.resolved }}</strong></span>
                <span>已拒绝 <strong>{{ store.tokenModelAnalyzeReport.value.rejectedInvalid ?? 0 }}</strong></span>
                <span>未决 <strong>{{ store.tokenModelAnalyzeReport.value.unresolved.length }}</strong></span>
              </div>
              <p
                v-if="store.tokenModelAnalyzeReport.value.unresolved.length"
                class="tt-mapping-unresolved"
              >
                未决：{{ store.tokenModelAnalyzeReport.value.unresolved.join("、") }}
              </p>
              <p
                v-for="(warning, index) in store.tokenModelAnalyzeReport.value.warnings"
                :key="index"
                class="tt-mapping-error"
              >{{ warning }}</p>
            </div>

            <!-- 映射管理：标签切换「原始模型列表 / 已生效模型」 -->
            <div class="tt-mapping-toolbar">
              <div class="tt-mapping-filters">
                <button
                  type="button"
                  :class="{ 'is-active': mappingView === 'raw' }"
                  @click="mappingView = 'raw'"
                >原始模型 ({{ store.tokenModelMappings.value.length }})</button>
                <button
                  type="button"
                  :class="{ 'is-active': mappingView === 'converted' }"
                  @click="mappingView = 'converted'"
                >已生效 ({{ mappingConvertedGroups.length }})</button>
              </div>
              <div class="tt-mapping-counter">
                <span v-if="mappingSuggestedCount > 0" class="tt-mapping-counter-chip is-brand">
                  {{ mappingSuggestedCount }} 条待审核
                </span>
                <span class="tt-mapping-counter-chip">已生效 {{ mappingApprovedCount }} / 共 {{ mappingCoverage.total }}</span>
              </div>
              <span v-if="mappingCatalogLoading" class="tt-mapping-catalog-hint">正式模型清单加载中…</span>
            </div>

            <!-- 视图一：原始模型列表（多选 + 行内编辑映射目标） -->
            <div v-if="mappingView === 'raw'" class="tt-mapping-view">
              <div class="tt-mapping-view-bar">
                <label class="tt-mapping-check">
                  <input
                    type="checkbox"
                    :checked="mappingRawAllSelected"
                    @change="toggleMappingRawAll"
                  />
                  <span>全选</span>
                </label>
                <input
                  v-model="mappingRawSearch"
                  type="search"
                  class="tt-mapping-pane-search"
                  placeholder="搜索原始模型…"
                />
                <small v-if="mappingRawSelected.length" class="tt-mapping-picked">
                  已选 {{ mappingRawSelected.length }} 项
                </small>
              </div>

              <div v-if="mappingRawSelected.length" class="tt-mapping-batch-bar">
                <span class="tt-mapping-batch-label">批量映射到</span>
                <CustomSelect
                  class="tt-mapping-batch-select"
                  :options="mappingTargetSelectOptions"
                  :groups="mappingCatalogGroups"
                  :model-value="mappingBatchTarget"
                  aria-label="选择批量映射目标"
                  @update:model-value="mappingBatchTarget = String($event)"
                />
                <input
                  v-if="mappingBatchTarget === '__custom__'"
                  v-model="mappingBatchCustom"
                  type="text"
                  class="tt-mapping-custom-target"
                  placeholder="输入自定义模型 ID（保存时自动注册）"
                  spellcheck="false"
                />
                <button
                  type="button"
                  class="tt-btn-primary"
                  :disabled="mappingBatchSaving || !mappingBatchTarget || (mappingBatchTarget === '__custom__' && !mappingBatchCustom.trim())"
                  @click="applyMappingBatch(false)"
                >{{ mappingBatchSaving ? "保存中…" : "应用映射" }}</button>
                <button
                  type="button"
                  class="tt-btn-cancel"
                  :disabled="mappingBatchSaving"
                  @click="applyMappingBatch(true)"
                >清除映射</button>
              </div>

              <div class="tt-mapping-list">
                <div
                  v-if="store.tokenModelMappingsLoading.value"
                  class="tt-mapping-pane-empty"
                >加载中…</div>
                <p v-else-if="mappingRawRows.length === 0" class="tt-mapping-pane-empty">
                  {{ mappingRawSearch ? "没有匹配的原始模型" : "暂无映射记录，点击「生成 AI 建议」开始识别" }}
                </p>
                <div
                  v-for="row in mappingRawRows"
                  v-else
                  :key="row.rawKey"
                  class="tt-mapping-row"
                  :class="{
                    'is-suggested': row.reviewStatus === 'suggested',
                    'is-rejected': row.reviewStatus === 'rejected',
                  }"
                >
                  <div class="tt-mapping-row-main">
                    <input
                      v-model="mappingRawSelected"
                      type="checkbox"
                      :value="row.rawModel"
                      :aria-label="`选择 ${row.rawModel}`"
                    />
                    <code class="tt-mapping-raw" :title="row.rawModel">{{ row.rawModel }}</code>
                    <div v-if="mappingEditingKey === row.rawKey" class="tt-mapping-row-custom">
                      <input
                        v-model="mappingEditingValue"
                        type="text"
                        class="tt-mapping-custom-target"
                        placeholder="输入自定义模型 ID"
                        spellcheck="false"
                        @keydown.enter.prevent="confirmRowCustom(row)"
                        @keydown.esc.prevent="cancelRowCustom"
                      />
                      <button type="button" class="tt-btn-primary" @click="confirmRowCustom(row)">保存</button>
                      <button type="button" class="tt-btn-cancel" @click="cancelRowCustom">取消</button>
                    </div>
                    <template v-else>
                      <CustomSelect
                        class="tt-mapping-row-select"
                        :options="mappingTargetSelectOptions"
                        :groups="mappingCatalogGroups"
                        :model-value="row.officialModel"
                        :aria-label="`为 ${row.rawModel} 选择映射目标`"
                        @update:model-value="onRowTargetChange(row, String($event))"
                      />
                      <span
                        v-if="row.reviewStatus !== 'approved'"
                        class="tt-mapping-review-badge"
                        :class="`is-${row.reviewStatus}`"
                      >{{ mappingReviewLabels[row.reviewStatus] }}</span>
                      <i v-else class="tt-origin-badge" :class="`is-${row.origin}`">
                        {{ mappingOriginLabels[row.origin] || row.origin }}
                      </i>
                      <span class="tt-mapping-row-spacer" />
                      <div v-if="row.reviewStatus === 'suggested'" class="tt-mapping-review-actions">
                        <button type="button" class="tt-mapping-approve" @click="approveMappingSuggestion(row)">批准</button>
                        <button type="button" class="tt-mapping-reject" @click="rejectMappingSuggestion(row)">驳回</button>
                      </div>
                      <button
                        v-else-if="row.reviewStatus === 'rejected'"
                        type="button"
                        class="tt-mapping-retry"
                        @click="reopenMapping(row)"
                      >重新识别</button>
                      <button
                        type="button"
                        class="tt-mapping-row-clear"
                        :disabled="!row.officialModel.trim()"
                        :aria-label="`清除 ${row.rawModel} 的映射`"
                        title="清除映射"
                        @click="saveMappingOfficial(row, '')"
                      >
                        <span v-html="icons.trash" />
                      </button>
                    </template>
                  </div>
                  <p
                    v-if="mappingEditingKey !== row.rawKey && row.reason && (row.reviewStatus === 'suggested' || row.reviewStatus === 'rejected')"
                    class="tt-mapping-row-reason"
                  >{{ row.reason }}</p>
                </div>
              </div>
            </div>

            <!-- 视图二：转换后模型列表（单选展开管理来源） -->
            <div v-else class="tt-mapping-view">
              <div class="tt-mapping-view-bar">
                <input
                  v-model="mappingTargetSearch"
                  type="search"
                  class="tt-mapping-pane-search"
                  placeholder="搜索目标模型或来源…"
                />
                <div class="tt-mapping-add-official">
                  <input
                    v-model="mappingNewOfficial"
                    type="text"
                    class="tt-mapping-custom-target"
                    placeholder="添加自定义模型 ID"
                    spellcheck="false"
                    @keydown.enter.prevent="addMappingOfficialModel"
                  />
                  <button
                    type="button"
                    class="tt-btn-primary"
                    :disabled="mappingAddingOfficial || !mappingNewOfficial.trim()"
                    @click="addMappingOfficialModel"
                  >{{ mappingAddingOfficial ? "添加中…" : "添加" }}</button>
                </div>
              </div>

              <div class="tt-mapping-list">
                <p
                  v-if="mappingConvertedFiltered.length === 0"
                  class="tt-mapping-pane-empty"
                >
                  {{ mappingTargetSearch ? "没有匹配的转换后模型" : "暂无转换后模型，先在「原始模型列表」建立映射" }}
                </p>
                <div
                  v-for="group in mappingConvertedFiltered"
                  v-else
                  :key="group.name"
                  class="tt-mapping-crow"
                  :class="{ 'is-open': mappingExpandedTarget === group.name }"
                >
                  <button type="button" class="tt-mapping-crow-head" @click="toggleExpandedTarget(group.name)">
                    <span class="tt-mapping-chevron" v-html="icons.chevron" />
                    <code class="tt-mapping-target-name">{{ group.name }}</code>
                    <small>{{ group.sources.length }} 个来源</small>
                  </button>
                  <div v-if="mappingExpandedTarget === group.name" class="tt-mapping-crow-body">
                    <div v-for="row in group.sources" :key="row.rawKey" class="tt-mapping-source-row">
                      <code class="tt-mapping-raw" :title="row.rawModel">{{ row.rawModel }}</code>
                      <i class="tt-origin-badge" :class="`is-${row.origin}`">
                        {{ mappingOriginLabels[row.origin] || row.origin }}
                      </i>
                      <button
                        type="button"
                        class="tt-mapping-row-clear"
                        :aria-label="`移除 ${row.rawModel} 的映射`"
                        title="移除此来源"
                        @click="removeMappingSource(row)"
                      >
                        <span v-html="icons.trash" />
                      </button>
                    </div>
                    <button
                      type="button"
                      class="tt-mapping-crow-clear"
                      @click="clearTargetMappings(group.name)"
                    >清除该目标的全部映射 ({{ group.sources.length }})</button>
                  </div>
                </div>
              </div>
            </div>
          </div>

          <footer class="tt-modal-footer">
            <span class="tt-footer-hint">
              视图一单行或批量设置映射目标（支持自定义，自动注册）；视图二按转换后模型查看/移除来源。手工映射立即生效。
            </span>
            <button type="button" class="tt-btn-cancel" @click="closeMappingDialog">关闭</button>
          </footer>
        </section>
      </div>
    </Transition>

    <!-- AI 用量洞察弹窗（证据可追溯解读） -->
    <Transition name="tt-modal-fade">
      <div v-if="insightDialogOpen" class="tt-modal-backdrop">
        <section class="tt-modal-card is-wide" role="dialog" aria-modal="true">
          <header class="tt-modal-header">
            <div>
              <h2>AI 用量洞察</h2>
              <p>证据由程序从当前时间范围数据计算得出；AI 只做解读，每个结论都标注了依据的证据。</p>
            </div>
            <button type="button" class="tt-modal-close-btn" aria-label="关闭" @click="insightDialogOpen = false">×</button>
          </header>

          <div class="tt-modal-body">
            <p class="tt-mapping-hint">
              <span v-html="icons.info" />
              <span>
                分析范围：<strong>{{ rangeLabel }}</strong>（共 {{ insightEvidenceCount }} 条证据）。
                洞察请求只在本地桌面客户端经进程内网关入口发往所选渠道；已生成的报告会保留其范围快照，
                切换日期后不会自动刷新。
              </span>
            </p>

            <div class="tt-mapping-controls">
              <label class="tt-mapping-field">
                <span>分析模型</span>
                <input
                  v-model="insightModel"
                  type="text"
                  class="tt-mapping-model-input"
                  list="tt-insight-model-suggestions"
                  placeholder="输入模型 ID，可自定义"
                  spellcheck="false"
                />
                <datalist id="tt-insight-model-suggestions">
                  <option v-for="model in insightModelSuggestions()" :key="model" :value="model" />
                </datalist>
              </label>
              <button
                type="button"
                class="tt-btn-primary"
                :disabled="store.tokenInsightAnalyzing.value || !insightModel.trim() || insightEvidenceCount === 0"
                @click="runInsightAnalysis"
              >
                <span :class="{ 'is-spinning': store.tokenInsightAnalyzing.value }" v-html="icons.sparkles" />
                <span>{{ store.tokenInsightAnalyzing.value ? "解读中…" : "生成洞察" }}</span>
              </button>
            </div>
            <p v-if="!insightEvidenceCount" class="tt-mapping-error">
              当前时间范围没有可用数据，请调整日期区间后重试。
            </p>
            <p v-if="store.tokenInsightError.value" class="tt-mapping-error">{{ store.tokenInsightError.value }}</p>

            <!-- 报告：仅在范围匹配时展示 -->
            <template v-if="store.tokenInsightReport.value">
              <div
                v-if="store.tokenInsightReport.value.rangeLabel !== rangeLabel"
                class="tt-insight-stale"
              >
                以下报告生成于「{{ store.tokenInsightReport.value.rangeLabel }}」（{{ insightReportTime }}，模型 {{ store.tokenInsightReport.value.analysisModel }}），
                与当前所选范围不同，可重新生成。
              </div>
              <div class="tt-insight-report">
                <p v-if="store.tokenInsightReport.value.headline" class="tt-insight-headline">
                  {{ store.tokenInsightReport.value.headline }}
                </p>
                <p v-if="store.tokenInsightReport.value.notice" class="tt-mapping-unresolved">
                  {{ store.tokenInsightReport.value.notice }}
                </p>
                <div class="tt-insight-meta">
                  <span>范围 {{ store.tokenInsightReport.value.rangeLabel }}</span>
                  <span>模型 {{ store.tokenInsightReport.value.analysisModel }}</span>
                  <span>{{ insightReportTime }}</span>
                  <span>证据引用 {{ store.tokenInsightReport.value.evidenceUsed }} / {{ store.tokenInsightReport.value.evidenceTotal }}</span>
                </div>

                <section v-if="store.tokenInsightReport.value.findings.length" class="tt-insight-section">
                  <h4>发现</h4>
                  <div
                    v-for="(finding, index) in store.tokenInsightReport.value.findings"
                    :key="`f-${index}`"
                    class="tt-insight-item"
                    :class="`is-${finding.severity}`"
                  >
                    <div class="tt-insight-item-head">
                      <i class="tt-insight-severity" :class="`is-${finding.severity}`" />
                      <strong>{{ finding.title }}</strong>
                    </div>
                    <p v-if="finding.detail">{{ finding.detail }}</p>
                    <small class="tt-insight-evidence">依据：{{ findingEvidenceText(finding.evidence) }}</small>
                  </div>
                </section>

                <section v-if="store.tokenInsightReport.value.recommendations.length" class="tt-insight-section">
                  <h4>建议</h4>
                  <div
                    v-for="(item, index) in store.tokenInsightReport.value.recommendations"
                    :key="`r-${index}`"
                    class="tt-insight-item"
                  >
                    <div class="tt-insight-item-head">
                      <i class="tt-insight-severity is-info" />
                      <strong>{{ item.title }}</strong>
                    </div>
                    <p v-if="item.detail">{{ item.detail }}</p>
                    <small class="tt-insight-evidence">依据：{{ findingEvidenceText(item.evidence) }}</small>
                  </div>
                </section>
              </div>
            </template>
          </div>

          <footer class="tt-modal-footer">
            <span class="tt-footer-hint">
              洞察只读取当前页面已加载的统计快照，不会上传原始日志；无证据支撑的 AI 结论会被自动忽略。
            </span>
            <button type="button" class="tt-btn-cancel" @click="insightDialogOpen = false">关闭</button>
          </footer>
        </section>
      </div>
    </Transition>

    <!-- 本地 AI Agent 路径诊断弹窗 (Local Agent Inspector Modal) -->
    <Transition name="tt-modal-fade">
      <div v-if="agentDialogOpen" class="tt-modal-backdrop">
        <section class="tt-modal-card is-wide" role="dialog" aria-modal="true">
          <header class="tt-modal-header">
            <div>
              <h2>本机 AI Agent 诊断终端</h2>
              <p>只读探测当前 macOS 系统中各 AI 编程工具的配置、数据库与日志目录</p>
            </div>
            <button type="button" class="tt-modal-close-btn" aria-label="关闭" @click="closeAgentDialog">×</button>
          </header>

          <div class="tt-modal-body">
            <div v-if="store.localAgentPathsLoading.value && !store.localAgentPaths.value" class="tt-loading-card">
              <div class="tt-loading-spinner" />
              <p>正在扫描本机 AI Agent 路径…</p>
            </div>
            <template v-else-if="store.localAgentPaths.value">
              <div class="tt-agent-overview-bar">
                <span class="tt-agent-meta-chip">系统根路径: <code>{{ localAgentsHome }}</code></span>
                <span class="tt-agent-meta-chip is-success">已检测 <strong>{{ detectedAgentsCount }}</strong> 款 Agent</span>
                <span v-if="localAgentsCollectedAt" class="tt-agent-meta-chip">采集时间: {{ localAgentsCollectedAt }}</span>
              </div>

              <div v-if="localAgentEnvOverrides.length" class="tt-agent-env-row">
                <span
                  v-for="override in localAgentEnvOverrides"
                  :key="override.key"
                  class="tt-agent-env-chip"
                  :title="override.value"
                >{{ override.key }} → {{ override.value }}</span>
              </div>

              <div class="tt-agent-cards-grid">
                <div
                  v-for="agent in visibleAgents"
                  :key="agent.source"
                  class="tt-agent-diag-card"
                  :class="{ 'is-detected': agent.detected }"
                >
                  <header class="tt-agent-diag-header">
                    <span class="tt-agent-dot" :class="{ on: agent.detected }" />
                    <strong>{{ agent.name }}</strong>
                    <span
                      v-if="agent.collectedEvents > 0 || agent.collectedSessions > 0"
                      class="tt-agent-stat-badge"
                    >
                      {{ formatAgentCount(agent.collectedSessions) }} 会话 · {{ formatAgentCount(agent.collectedEvents) }} 请求
                    </span>
                    <span class="tt-agent-status-tag" :class="{ on: agent.detected }">
                      {{ agent.detected ? "已检测" : "未安装/未激活" }}
                    </span>
                  </header>

                  <div class="tt-agent-root-row">
                    <span class="label">根目录:</span>
                    <code :title="agent.root">{{ displayAgentPath(agent.root) }}</code>
                  </div>

                  <ul class="tt-agent-path-list">
                    <li
                      v-for="(entry, eIdx) in agent.paths"
                      :key="eIdx"
                      :title="`点击复制路径: ${entry.path}`"
                      @click="copyAgentPath(entry.path)"
                    >
                      <span class="tt-path-kind-badge" :data-kind="entry.kind">{{ agentKindLabels[entry.kind] || entry.kind }}</span>
                      <span class="tt-path-text">
                        <span class="tt-path-label">{{ entry.label }}</span>
                        <code :class="{ missing: !entry.exists }"><span
                          v-for="(segment, segmentIndex) in agentPathSegments(entry.path)"
                          :key="segmentIndex"
                          class="tt-path-seg"
                        >{{ segment }}</span></code>
                      </span>
                      <span class="tt-path-status-icon" :class="{ exists: entry.exists }" />
                    </li>
                  </ul>
                </div>
              </div>
            </template>

            <span class="tt-footer-hint">点击任意路径行可直接复制完整路径至系统剪贴板。</span>
          </div>
        </section>
      </div>
    </Transition>

    <!-- 导出数据弹窗 (Export Modal) -->
    <Transition name="tt-modal-fade">
      <div v-if="exportDialogOpen" class="tt-modal-backdrop">
        <section class="tt-modal-card is-wide" role="dialog" aria-modal="true">
          <header class="tt-modal-header">
            <div>
              <h2>导出 Token 数据分析报表</h2>
              <p>导出当前所选时间范围 ({{ rangeLabel }}) 内的多维分析指标</p>
            </div>
            <button type="button" class="tt-modal-close-btn" aria-label="关闭" @click="closeExportDialog">×</button>
          </header>

          <div class="tt-modal-body">
            <div class="tt-export-options-grid">
              <div class="tt-export-option-card" @click="exportDataAsJson">
                <span class="tt-export-icon" v-html="icons.database" />
                <strong>导出完整 JSON 结构化报表</strong>
                <p>包含大盘概览、工具分布、模型排行榜、项目用量与完整时序明细，适合二次分析与归档。</p>
                <button type="button" class="tt-btn-primary">下载 JSON 文件</button>
              </div>

              <div class="tt-export-option-card" @click="exportDataAsCsv">
                <span class="tt-export-icon" v-html="icons.download" />
                <strong>导出时序明细 CSV 表格</strong>
                <p>导出逐日/逐小时 Token 消耗明细（含输入、输出、缓存读写、命中率与请求数），适合 Excel / Numbers 打开。</p>
                <button type="button" class="tt-btn-primary">下载 CSV 表格</button>
              </div>
            </div>
          </div>
        </section>
      </div>
    </Transition>
  </main>
</template>

<style scoped>
.tt-dashboard {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  background: var(--page-bg);
  color: var(--text);
  overflow: hidden;
}

/* ============================================================
   1. 顶部宏观智控驾驶舱 (Macro Cockpit Bar)
   ============================================================ */
.tt-cockpit-bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 12px 20px;
  background: var(--surface);
  border-bottom: 1px solid var(--line);
  flex-shrink: 0;
}

.tt-cockpit-left {
  display: flex;
  align-items: center;
  gap: 16px;
  flex-shrink: 0;
}

.tt-brand-section {
  display: flex;
  flex-direction: column;
  gap: 1px;
}

.tt-eyebrow-row {
  display: flex;
  align-items: center;
  gap: 6px;
}

.tt-live-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: #10b981;
  box-shadow: 0 0 8px #10b981;
  animation: ttPulse 2s infinite ease-in-out;
}

@keyframes ttPulse {
  0%, 100% { opacity: 1; transform: scale(1); }
  50% { opacity: 0.4; transform: scale(1.25); }
}

.tt-eyebrow-text {
  font-size: 10px;
  font-weight: 750;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--brand);
}

.tt-title-row {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

.tt-title-row h1 {
  font-size: 18px;
  font-weight: 750;
  color: var(--text);
  margin: 0;
  line-height: 1.2;
}

/* 数据来源标签（本地采集 / 反代网关） */
.tt-mode-tab {
  appearance: none;
  border: 0;
  background: transparent;
  color: var(--muted);
  font-size: 11px;
  font-weight: 650;
  padding: 3px 10px;
  border-radius: 6px;
  cursor: pointer;
  transition: background 0.15s ease, color 0.15s ease;
  white-space: nowrap;
}

.tt-mode-tab:hover {
  color: var(--text);
}

.tt-mode-tab.active {
  background: var(--brand, #10b981);
  color: #fff;
  box-shadow: 0 1px 4px rgba(16, 185, 129, 0.35);
}

.tt-cockpit-subtitle {
  font-size: 11px;
  color: var(--muted);
  margin: 0;
}

.tt-cockpit-subtitle strong {
  color: var(--text);
  font-weight: 600;
}

.tt-cockpit-right {
  display: flex;
  align-items: center;
  gap: 8px;
}

/* 标题下方筛选工具条：日期选择 + 维度按钮 */
.tt-filter-toolbar {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 20px;
  background: var(--surface);
  border-bottom: 1px solid var(--line);
  flex-shrink: 0;
}

/* 顶部快速视角气泡群组 */
.tt-cockpit-pills-group {
  display: inline-flex;
  align-items: center;
  gap: 2px;
  background: var(--page-bg);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  padding: 2px;
  flex-shrink: 0;
}

.tt-pill-btn {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  height: 28px;
  padding: 0 10px;
  border-radius: 6px;
  border: none;
  background: transparent;
  color: var(--muted);
  font-size: 12px;
  font-weight: 550;
  cursor: pointer;
  transition: all 0.12s ease;
  white-space: nowrap;
  flex-shrink: 0;
}

.tt-pill-btn:hover {
  background: var(--surface);
  color: var(--text);
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.04);
}

.tt-pill-btn :deep(svg) {
  width: 13px;
  height: 13px;
}

.tt-cockpit-divider {
  width: 1px;
  height: 20px;
  background: var(--line);
  margin: 0 2px;
  flex-shrink: 0;
}

/* 按钮规范 */
.tt-btn-rebuild {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 32px;
  padding: 0 12px;
  border-radius: var(--r-md, 8px);
  border: 1px solid color-mix(in srgb, var(--brand, #388bfd) 35%, transparent);
  background: color-mix(in srgb, var(--brand, #388bfd) 10%, var(--surface));
  color: var(--brand-deep, var(--brand, #388bfd));
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.15s ease;
  white-space: nowrap;
  flex-shrink: 0;
}

.tt-btn-rebuild:hover:not(:disabled) {
  background: color-mix(in srgb, var(--brand, #388bfd) 18%, var(--surface));
  border-color: var(--brand);
  transform: translateY(-1px);
}

.tt-btn-rebuild:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.tt-btn-rebuild :deep(svg) {
  width: 13px;
  height: 13px;
}

.tt-btn-secondary {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 32px;
  padding: 0 11px;
  border-radius: var(--r-md, 8px);
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--text);
  font-size: 12px;
  font-weight: 550;
  cursor: pointer;
  transition: all 0.15s ease;
  white-space: nowrap;
  flex-shrink: 0;
}

.tt-btn-secondary:hover {
  background: var(--surface-hover);
  border-color: var(--line-hover);
  transform: translateY(-1px);
}

.tt-btn-secondary :deep(svg) {
  width: 13px;
  height: 13px;
  color: var(--muted);
}

.tt-agent-count-chip {
  padding: 1px 5px;
  border-radius: var(--r-full);
  background: rgba(16, 185, 129, 0.12);
  color: #10b981;
  font-size: 9.5px;
  font-weight: 700;
}

.is-spinning {
  animation: ttSpin 1s infinite linear;
}

@keyframes ttSpin {
  100% { transform: rotate(360deg); }
}

/* ============================================================
   2. 首页主视口 (No-Scroll Viewport Layout)
   ============================================================ */
.tt-dashboard-body {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  padding: 12px 18px;
  gap: 10px;
}

/* ROW 1: 4 KPI Cards (Compact) */
.tt-kpi-deck {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 10px;
  flex-shrink: 0;
}

@media (max-width: 1200px) {
  .tt-kpi-deck {
    grid-template-columns: repeat(2, 1fr);
  }
}

.tt-kpi-card {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 10px);
  padding: 10px 14px;
  display: flex;
  flex-direction: column;
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.02);
  transition: all 0.15s ease;
}

.tt-kpi-card:hover {
  border-color: var(--line-hover);
}

.tt-kpi-card-inner {
  display: flex;
  flex-direction: column;
  height: 100%;
}

.tt-kpi-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 6px;
  margin-bottom: 4px;
}

.tt-kpi-tag {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 10px;
  font-weight: 750;
  letter-spacing: 0.05em;
  text-transform: uppercase;
}

.tt-kpi-tag :deep(svg) {
  width: 12px;
  height: 12px;
}

.tt-kpi-tag.is-emerald { color: #10b981; }
.tt-kpi-tag.is-orange { color: #f97316; }
.tt-kpi-tag.is-blue { color: #3b82f6; }
.tt-kpi-tag.is-purple { color: #a855f7; }

.tt-kpi-badge-hit {
  padding: 1px 6px;
  border-radius: var(--r-full);
  font-size: 10px;
  font-weight: 700;
}
.tt-kpi-badge-hit.is-excellent { background: rgba(16, 185, 129, 0.12); color: #10b981; }
.tt-kpi-badge-hit.is-good { background: rgba(59, 130, 246, 0.12); color: #3b82f6; }
.tt-kpi-badge-hit.is-fair { background: rgba(245, 158, 11, 0.12); color: #f59e0b; }
.tt-kpi-badge-hit.is-none { background: rgba(148, 163, 184, 0.12); color: #94a3b8; }

.tt-kpi-streak-pill,
.tt-kpi-speedup-pill,
.tt-kpi-savings-pill {
  padding: 1px 6px;
  border-radius: var(--r-full);
  font-size: 10px;
  font-weight: 700;
  background: rgba(249, 115, 22, 0.12);
  color: #f97316;
}

.tt-kpi-speedup-pill,
.tt-kpi-savings-pill {
  background: rgba(168, 85, 247, 0.12);
  color: #a855f7;
}

.tt-kpi-badge-rate {
  padding: 1px 6px;
  border-radius: var(--r-full);
  font-size: 10px;
  font-weight: 700;
  background: rgba(59, 130, 246, 0.12);
  color: #3b82f6;
}

.tt-kpi-main-val {
  display: flex;
  align-items: baseline;
  gap: 5px;
  margin-bottom: 4px;
}

.tt-kpi-main-val strong {
  font-size: 22px;
  font-weight: 800;
  line-height: 1;
  font-variant-numeric: tabular-nums;
  letter-spacing: -0.02em;
}

.tt-kpi-main-val strong.tt-val-unpriced {
  font-size: 14px;
  font-weight: 600;
  color: var(--muted);
}

.tt-kpi-unit {
  font-size: 11px;
  color: var(--muted);
  font-weight: 600;
}

/* 进度条 */
.tt-kpi-progress-bar {
  display: flex;
  height: 4px;
  border-radius: var(--r-full);
  background: var(--page-bg);
  overflow: hidden;
  margin-bottom: 6px;
}

.tt-prog-seg {
  height: 100%;
  transition: width 0.3s ease;
}
.tt-prog-seg.is-in { background: #3b82f6; }
.tt-prog-seg.is-out { background: #10b981; }
.tt-prog-seg.is-cache { background: #8b5cf6; }

.tt-kpi-sub-pills {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  font-size: 10.5px;
}

.tt-sub-pill {
  display: inline-flex;
  align-items: center;
  gap: 3px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
}

.tt-sub-pill i {
  width: 5px;
  height: 5px;
  border-radius: 50%;
}
.tt-sub-pill.in i { background: #3b82f6; }
.tt-sub-pill.out i { background: #10b981; }
.tt-sub-pill.cache i { background: #8b5cf6; }
.tt-sub-pill.reasoning i { background: #f59e0b; }

.tt-kpi-meta-text {
  font-size: 11px;
  color: var(--muted);
  margin-bottom: 4px;
}

.tt-kpi-meta-text strong {
  color: var(--text);
}

.tt-active-rate-badge {
  display: inline-block;
  margin-left: 4px;
  padding: 1px 4px;
  border-radius: 3px;
  background: var(--surface-hover);
  font-size: 9.5px;
  font-weight: 600;
}

.tt-kpi-footer-note {
  margin-top: auto;
  font-size: 10.5px;
  color: var(--muted);
}

.tt-kpi-multiplier-pill {
  margin-top: auto;
  padding: 2px 6px;
  border-radius: var(--r-md, 4px);
  background: var(--surface-hover);
  font-size: 10.5px;
  color: var(--muted);
}

.tt-kpi-multiplier-pill strong {
  color: #3b82f6;
  font-weight: 700;
}

/* 4 大核心全景图表与分布四等分大盘 (Equal 4-Quadrant 2x2 Grid) */
.tt-quad-grid {
  flex: 1;
  min-height: 0;
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  grid-template-rows: repeat(2, minmax(0, 1fr));
  gap: 10px;
  overflow: hidden;
}

@media (max-width: 1100px) {
  .tt-quad-grid {
    grid-template-columns: 1fr;
    grid-template-rows: auto;
    overflow-y: auto;
  }
}

.tt-card {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 10px);
  padding: 12px 14px;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.tt-card.tt-health-card {
  padding: 8px 6px;
}

.tt-card-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  margin-bottom: 8px;
  flex-shrink: 0;
}

.tt-card-title-wrap h2,
.tt-card-title-wrap h3 {
  font-size: 13.5px;
  font-weight: 700;
  margin: 0;
}

.tt-card-title-wrap p {
  font-size: 10.5px;
  color: var(--muted);
  margin: 1px 0 0;
}

.tt-metric-switches {
  display: flex;
  gap: 2px;
  background: var(--page-bg);
  padding: 2px;
  border-radius: var(--r-md, 6px);
  border: 1px solid var(--line);
}

.tt-metric-btn {
  height: 22px;
  padding: 0 8px;
  border-radius: 4px;
  border: none;
  background: transparent;
  color: var(--muted);
  font-size: 10.5px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.12s ease;
}

.tt-metric-btn.active {
  background: var(--surface);
  color: var(--text);
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.08);
}

.tt-chart-body,
.tt-health-body {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.tt-health-kpi-bar {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 6px;
  margin-bottom: 8px;
  flex-shrink: 0;
}

.tt-hk-card {
  padding: 5px 8px;
  border-radius: var(--r-sm, 6px);
  background: var(--page-bg);
  border: 1px solid var(--line-soft);
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.tt-hk-lbl {
  font-size: 10px;
  color: var(--muted);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.tt-hk-val-box {
  display: flex;
  align-items: baseline;
  gap: 4px;
}

.tt-hk-num {
  font-size: 13.5px;
  font-weight: 750;
  font-variant-numeric: tabular-nums;
  line-height: 1;
}

.tt-hk-unit {
  font-size: 9.5px;
  color: var(--muted);
}

.tt-hk-badge {
  font-size: 9px;
  font-weight: 700;
  padding: 1px 4px;
  border-radius: 3px;
  line-height: 1;
}
.tt-hk-badge.is-excellent { background: rgba(16, 185, 129, 0.15); color: #10b981; }
.tt-hk-badge.is-good { background: rgba(59, 130, 246, 0.15); color: #3b82f6; }
.tt-hk-badge.is-fair { background: rgba(245, 158, 11, 0.15); color: #f59e0b; }
.tt-hk-badge.is-bad { background: rgba(239, 68, 68, 0.15); color: #ef4444; }

.tt-health-wrapper {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  justify-content: space-between;
  gap: 6px;
}

.tt-health-grid {
  display: grid;
  grid-auto-flow: column;
  grid-auto-columns: minmax(0, 1fr);
  column-gap: 4px;
  row-gap: 5px;
  width: 100%;
  flex: 1;
  min-height: 0;
  align-content: center;
}

.tt-health-cell {
  width: 100%;
  height: 100%;
  border-radius: 3.5px;
  transition: transform 0.1s ease, box-shadow 0.1s ease;
}
.tt-health-cell:hover {
  transform: scale(1.35);
  z-index: 10;
  box-shadow: 0 2px 6px rgba(0, 0, 0, 0.2);
}

.tt-health-cell.lv0 { background: rgba(148, 163, 184, 0.15); }
.tt-health-cell.lv1 { background: #ef4444; }
.tt-health-cell.lv2 { background: #f97316; }
.tt-health-cell.lv3 { background: #eab308; }
.tt-health-cell.lv4 { background: #84cc16; }
.tt-health-cell.lv5 { background: #10b981; }

.tt-health-legend {
  display: flex;
  align-items: center;
  gap: 5px;
  font-size: 10px;
  color: var(--muted);
  flex-shrink: 0;
  padding-top: 4px;
}
.tt-health-legend .tt-health-cell {
  width: 8px;
  height: 8px;
}
.tt-legend-meta {
  margin-left: auto;
}

.tt-preview-card {
  padding: 12px 14px;
}

.tt-preview-body {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  justify-content: flex-start;
  gap: 4px;
}

.tt-bar-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 3px 0;
}

.tt-bar-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  flex-shrink: 0;
}

.tt-bar-label {
  font-size: 11.5px;
  font-weight: 600;
  width: 130px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.tt-bar-track {
  flex: 1;
  height: 5px;
  background: var(--page-bg);
  border-radius: var(--r-full);
  overflow: hidden;
}

.tt-bar-fill {
  height: 100%;
  border-radius: var(--r-full);
  transition: width 0.3s ease;
}

.tt-bar-pct {
  font-size: 10.5px;
  color: var(--muted);
  width: 40px;
  text-align: right;
  font-variant-numeric: tabular-nums;
}

.tt-bar-val {
  font-size: 11.5px;
  width: 60px;
  text-align: right;
  font-variant-numeric: tabular-nums;
}

.tt-text-btn {
  background: transparent;
  border: none;
  color: var(--brand);
  font-size: 11.5px;
  font-weight: 600;
  cursor: pointer;
  padding: 0;
}
.tt-text-btn:hover {
  text-decoration: underline;
}

/* ============================================================
   3. 弹窗对话框体系 (Modal Dialogs)
   ============================================================ */
.tt-modal-backdrop {
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

.tt-modal-card {
  width: 100%;
  max-width: 640px;
  max-height: 85vh;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-xl, 14px);
  box-shadow: 0 20px 48px rgba(0, 0, 0, 0.25);
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.tt-modal-card.is-wide {
  max-width: 960px;
}

.tt-modal-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 14px 18px;
  border-bottom: 1px solid var(--line);
  flex-shrink: 0;
}

.tt-modal-header h2 {
  font-size: 15px;
  font-weight: 750;
  margin: 0;
}

.tt-modal-header p {
  font-size: 11px;
  color: var(--muted);
  margin: 2px 0 0;
}

.tt-modal-close-btn {
  width: 28px;
  height: 28px;
  border-radius: 6px;
  border: none;
  background: transparent;
  color: var(--muted);
  font-size: 16px;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
}

.tt-modal-close-btn:hover {
  background: var(--surface-hover);
  color: var(--text);
}

.tt-modal-body {
  padding: 16px 18px;
  overflow-y: auto;
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.tt-modal-footer {
  padding: 10px 18px;
  border-top: 1px solid var(--line);
  display: flex;
  justify-content: flex-end;
  align-items: center;
  gap: 8px;
  background: var(--page-bg);
  flex-shrink: 0;
}

.tt-table-wrap {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  overflow: hidden;
  display: flex;
  flex-direction: column;
  flex: 1 1 auto;
  min-height: 0;
}

/* 过滤搜索栏 */
.tt-filter-bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  flex-shrink: 0;
}

.tt-search-input {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  height: 34px;
  padding: 0 12px;
  border-radius: var(--r-md, 8px);
  border: 1px solid var(--line);
  background: var(--page-bg);
  width: 320px;
}

.tt-search-input input {
  border: none;
  background: transparent;
  color: var(--text);
  font-size: 12.5px;
  width: 100%;
  outline: none;
}

.tt-filter-count {
  font-size: 11.5px;
  color: var(--muted);
}

.tt-cell-with-dot {
  display: flex;
  align-items: center;
  gap: 6px;
}

.tt-muted-code {
  font-size: 10px;
  color: var(--muted);
}

.tt-project-cell {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.tt-project-cell :deep(svg) {
  width: 14px;
  height: 14px;
  color: var(--muted);
  flex-shrink: 0;
}

.tt-btn-cancel {
  height: 32px;
  padding: 0 14px;
  border-radius: var(--r-md, 6px);
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--text);
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  display: inline-flex;
  align-items: center;
  gap: 6px;
  transition: all 0.15s ease;
}

.tt-btn-cancel:hover {
  background: var(--surface-hover);
}

.tt-btn-primary {
  height: 32px;
  padding: 0 16px;
  border-radius: var(--r-md, 6px);
  border: none;
  background: var(--brand);
  color: #fff;
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  display: inline-flex;
  align-items: center;
  gap: 6px;
  transition: all 0.15s ease;
}

.tt-btn-primary :deep(svg),
.tt-btn-cancel :deep(svg) {
  width: 14px;
  height: 14px;
  /* 内联 SVG 默认按基线对齐，图标下方会留出降部空隙导致视觉偏上；块级化消除该偏移 */
  display: block;
}

.tt-btn-primary:hover {
  background: var(--brand-deep);
  transform: translateY(-1px);
}

/* 步骤指示器 */
.tt-refresh-workflow {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.tt-wf-step {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 12px;
  border-radius: var(--r-md, 8px);
  background: var(--page-bg);
  border: 1px solid var(--line);
}

.tt-wf-num {
  width: 24px;
  height: 24px;
  border-radius: 50%;
  background: var(--brand);
  color: #fff;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 12px;
  font-weight: 700;
  flex-shrink: 0;
}

.tt-wf-info {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.tt-wf-info strong {
  font-size: 12.5px;
}

.tt-wf-info small {
  font-size: 11px;
  color: var(--muted);
}

.tt-refresh-tips {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 12px;
  border-radius: 6px;
  background: color-mix(in srgb, var(--brand) 10%, transparent);
  color: var(--text);
  font-size: 11.5px;
}

.tt-refresh-tips :deep(svg) {
  width: 14px;
  height: 14px;
  color: var(--brand);
  flex-shrink: 0;
}

.tt-refresh-tips p {
  margin: 0;
}

.tt-refresh-running-bar {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px;
  border-radius: var(--r-md, 8px);
  background: var(--page-bg);
  border: 1px solid var(--line);
}

.tt-state-icon {
  width: 28px;
  height: 28px;
  border-radius: 50%;
  background: var(--brand-soft);
  color: var(--brand);
  display: flex;
  align-items: center;
  justify-content: center;
  font-weight: 800;
  font-size: 13px;
}

.tt-log-terminal {
  background: #090d16;
  border-radius: var(--r-md, 8px);
  border: 1px solid rgba(255, 255, 255, 0.1);
  padding: 10px 12px;
  color: #f8fafc;
  font-family: monospace;
}

.tt-log-header {
  display: flex;
  justify-content: space-between;
  font-size: 10.5px;
  color: #64748b;
  padding-bottom: 6px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
  margin-bottom: 6px;
}

.tt-log-list {
  list-style: none;
  padding: 0;
  margin: 0;
  max-height: 160px;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 10.5px;
}

.tt-log-list li {
  display: flex;
  align-items: center;
  gap: 6px;
}

.tt-log-list time { color: #64748b; }
.tt-log-stage {
  padding: 1px 4px;
  border-radius: 3px;
  background: rgba(255, 255, 255, 0.1);
  color: #38bdf8;
}
.tt-log-list p { margin: 0; flex: 1; }

.tt-footer-hint {
  font-size: 11px;
  color: var(--muted);
  margin-right: auto;
}

/* Agent 诊断弹窗 */
.tt-agent-overview-bar {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  flex-shrink: 0;
}

.tt-agent-meta-chip {
  padding: 3px 8px;
  border-radius: var(--r-md, 6px);
  background: var(--page-bg);
  border: 1px solid var(--line);
  font-size: 11px;
  color: var(--muted);
}

.tt-agent-meta-chip.is-success {
  background: rgba(16, 185, 129, 0.12);
  color: #10b981;
  border-color: rgba(16, 185, 129, 0.3);
}

.tt-agent-env-row {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  flex-shrink: 0;
}

.tt-agent-env-chip {
  padding: 2px 6px;
  border-radius: 4px;
  background: var(--surface-hover);
  font-size: 10.5px;
  font-family: monospace;
}

.tt-agent-cards-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(360px, 1fr));
  gap: 10px;
}

.tt-agent-diag-card {
  background: var(--page-bg);
  border: 1px solid var(--line);
  border-radius: var(--r-lg);
  padding: 14px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.tt-agent-diag-card.is-detected {
  border-color: rgba(16, 185, 129, 0.4);
  background: var(--surface);
}

.tt-agent-diag-header {
  display: flex;
  align-items: center;
  gap: 8px;
}

.tt-agent-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: #94a3b8;
}
.tt-agent-dot.on {
  background: #10b981;
  box-shadow: 0 0 6px #10b981;
}

.tt-agent-stat-badge {
  padding: 1px 6px;
  border-radius: var(--r-full);
  background: var(--surface-hover);
  font-size: 10px;
  color: var(--muted);
}

.tt-agent-status-tag {
  margin-left: auto;
  font-size: 11px;
  color: #94a3b8;
}
.tt-agent-status-tag.on {
  color: #10b981;
  font-weight: 700;
}

.tt-agent-root-row {
  font-size: 11px;
  color: var(--muted);
  display: flex;
  align-items: center;
  gap: 6px;
}

.tt-agent-path-list {
  list-style: none;
  padding: 0;
  margin: 0;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.tt-agent-path-list li {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 8px;
  border-radius: 4px;
  background: var(--surface);
  cursor: pointer;
  transition: background 0.15s ease;
}

.tt-agent-path-list li:hover {
  background: var(--surface-hover);
}

.tt-path-kind-badge {
  padding: 1px 4px;
  border-radius: 3px;
  background: var(--brand-soft);
  color: var(--brand-deep);
  font-size: 9px;
  font-weight: 700;
}

.tt-path-text {
  flex: 1;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.tt-path-label {
  font-size: 10px;
  color: var(--muted);
}

.tt-path-text code {
  font-size: 11px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.tt-path-text code.missing {
  color: var(--muted);
  text-decoration: line-through;
}

.tt-path-status-icon {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: #94a3b8;
}
.tt-path-status-icon.exists {
  background: #10b981;
}

/* 导出选项 */
.tt-export-options-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 16px;
}

.tt-export-option-card {
  background: var(--page-bg);
  border: 1px solid var(--line);
  border-radius: var(--r-xl);
  padding: 20px;
  display: flex;
  flex-direction: column;
  align-items: center;
  text-align: center;
  gap: 12px;
  cursor: pointer;
  transition: all 0.15s ease;
}

.tt-export-option-card:hover {
  border-color: var(--brand);
  transform: translateY(-2px);
  box-shadow: 0 8px 20px rgba(0, 0, 0, 0.06);
}

.tt-export-icon {
  width: 44px;
  height: 44px;
  border-radius: 50%;
  background: var(--brand-soft);
  color: var(--brand);
  display: flex;
  align-items: center;
  justify-content: center;
}

.tt-export-icon :deep(svg) {
  width: 22px;
  height: 22px;
}

.tt-export-option-card strong {
  font-size: 14px;
}

.tt-export-option-card p {
  font-size: 12px;
  color: var(--muted);
  margin: 0;
  flex: 1;
}

/* 状态提示 */
.tt-error-banner {
  display: flex;
  align-items: flex-start;
  gap: 12px;
  padding: 14px 18px;
  border-radius: var(--r-xl);
  background: rgba(239, 68, 68, 0.1);
  border: 1px solid rgba(239, 68, 68, 0.3);
  color: #ef4444;
}

.tt-error-icon :deep(svg) {
  width: 20px;
  height: 20px;
}

.tt-error-content strong {
  display: block;
  font-size: 14px;
}

.tt-error-content p {
  margin: 4px 0 2px;
  font-size: 12px;
}

.tt-error-content small {
  color: var(--muted);
  font-size: 11px;
}

.tt-loading-card,
.tt-empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 40px;
  color: var(--muted);
  font-size: 13px;
  gap: 12px;
}

.tt-loading-spinner {
  width: 28px;
  height: 28px;
  border: 3px solid var(--line);
  border-top-color: var(--brand);
  border-radius: 50%;
  animation: ttSpin 0.8s infinite linear;
}

.tt-modal-fade-enter-active,
.tt-modal-fade-leave-active {
  transition: opacity 0.2s ease;
}

.tt-modal-fade-enter-from,
.tt-modal-fade-leave-to {
  opacity: 0;
}

/* ============================================================
   模型映射弹窗 (Model Mapping Modal)
   ============================================================ */
.tt-mapping-controls {
  display: flex;
  align-items: flex-end;
  gap: 10px;
  flex-wrap: wrap;
}

.tt-mapping-field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-width: 180px;
}

.tt-mapping-field > span {
  font-size: 11px;
  color: var(--muted);
}

.tt-mapping-field .select-box {
  height: 34px;
}

.tt-mapping-hint {
  display: flex;
  align-items: flex-start;
  gap: 6px;
  margin: 0;
  font-size: 11px;
  line-height: 1.6;
  color: var(--muted);
}

.tt-mapping-hint :deep(svg) {
  width: 13px;
  height: 13px;
  flex-shrink: 0;
  margin-top: 2px;
}

.tt-mapping-report {
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  background: var(--page-bg);
  padding: 10px 12px;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.tt-mapping-report-stats {
  display: flex;
  gap: 16px;
  flex-wrap: wrap;
  font-size: 12px;
  color: var(--muted);
}

.tt-mapping-report-stats strong {
  color: var(--text);
  font-size: 13px;
}

.tt-mapping-unresolved {
  margin: 0;
  font-size: 11px;
  color: var(--muted);
  word-break: break-all;
}

.tt-mapping-error {
  margin: 0;
  font-size: 11px;
  color: #ef4444;
  word-break: break-all;
}

.tt-mapping-raw {
  display: inline-block;
  max-width: 100%;
  padding: 3px 8px;
  border-radius: var(--r-sm, 6px);
  background: var(--surface-hover);
  font-family: var(--font-mono, ui-monospace, monospace);
  font-size: 11px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  vertical-align: middle;
}

.tt-mapping-view {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.tt-mapping-view-bar {
  display: flex;
  align-items: center;
  gap: 10px;
}

.tt-mapping-view-bar .tt-mapping-pane-search {
  flex: 1;
  min-width: 160px;
  max-width: 320px;
}

.tt-mapping-add-official {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-left: auto;
}

.tt-mapping-add-official .tt-mapping-custom-target {
  width: 240px;
}

.tt-mapping-check {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  font-size: 11px;
  color: var(--muted);
  cursor: pointer;
  white-space: nowrap;
}

.tt-mapping-picked {
  font-size: 11px;
  color: var(--brand-deep, var(--brand));
  font-weight: 600;
  white-space: nowrap;
}

.tt-mapping-batch-bar {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px;
  padding: 8px 10px;
  border: 1px solid var(--brand);
  border-radius: var(--r-md, 8px);
  background: var(--brand-soft);
}

.tt-mapping-batch-label {
  font-size: 11px;
  font-weight: 650;
  color: var(--brand-deep, var(--brand));
  white-space: nowrap;
}

.tt-mapping-batch-select {
  width: 220px;
  height: 30px;
  flex: 0 0 auto;
}

.tt-mapping-custom-target {
  flex: 1;
  min-width: 180px;
  height: 30px;
  padding: 0 10px;
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  background: var(--surface);
  color: var(--text);
  font-size: 12px;
  font-family: var(--font-mono, ui-monospace, monospace);
}

.tt-mapping-custom-target:focus {
  outline: none;
  border-color: var(--brand);
}

.tt-mapping-list {
  display: flex;
  flex-direction: column;
  gap: 2px;
  max-height: 44vh;
  overflow-y: auto;
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  background: var(--surface);
  padding: 6px;
}

.tt-mapping-pane-empty {
  margin: 0;
  padding: 18px 10px;
  text-align: center;
  font-size: 11px;
  color: var(--muted);
}

/* 视图一：原始模型行 —— 主行（勾选/名称/目标/徽章/操作）+ 理由子行 */
.tt-mapping-row {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 5px 8px;
  border-radius: var(--r-sm, 6px);
  border: 1px solid transparent;
  font-size: 11px;
}

.tt-mapping-row:hover {
  background: var(--surface-hover);
}

.tt-mapping-row.is-suggested {
  background: color-mix(in srgb, var(--brand) 7%, transparent);
  border-color: color-mix(in srgb, var(--brand) 20%, transparent);
}

.tt-mapping-row.is-suggested:hover {
  background: color-mix(in srgb, var(--brand) 11%, transparent);
}

.tt-mapping-row.is-rejected {
  opacity: 0.72;
}

.tt-mapping-row-main {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
}

.tt-mapping-row-main input[type="checkbox"] {
  flex: 0 0 auto;
  margin: 0;
}

.tt-mapping-row-main .tt-mapping-raw {
  flex: 0 1 200px;
  min-width: 110px;
}

.tt-mapping-row-main .tt-mapping-row-select {
  flex: 0 0 230px;
  max-width: none;
}

.tt-mapping-row-spacer {
  flex: 1 1 4px;
}

.tt-mapping-row-reason {
  margin: 0;
  padding-left: 26px;
  font-size: 10.5px;
  line-height: 1.5;
  color: var(--muted);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.tt-mapping-row-custom {
  flex: 1;
  display: flex;
  align-items: center;
  gap: 6px;
}

.tt-mapping-row-clear {
  flex: 0 0 auto;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 24px;
  height: 24px;
  padding: 0;
  border: none;
  border-radius: var(--r-sm, 6px);
  background: transparent;
  color: var(--muted);
  cursor: pointer;
}

.tt-mapping-row-clear:hover:not(:disabled) {
  background: rgba(239, 68, 68, 0.12);
  color: #ef4444;
}

.tt-mapping-row-clear:disabled {
  opacity: 0.3;
  cursor: default;
}

.tt-mapping-row-clear svg {
  width: 13px;
  height: 13px;
}

.tt-origin-badge {
  flex: 0 0 auto;
  font-style: normal;
  font-size: 10px;
  padding: 2px 7px;
  border-radius: var(--r-full, 999px);
  background: var(--surface-hover);
  color: var(--muted);
  white-space: nowrap;
}

.tt-origin-badge.is-ai {
  background: var(--brand-soft);
  color: var(--brand-deep, var(--brand));
}

.tt-origin-badge.is-manual {
  background: rgba(16, 185, 129, 0.16);
  color: #10b981;
}

/* 审核状态徽章：待识别 / 待审核 / 已生效 / 已驳回 */
.tt-mapping-review-badge {
  flex: 0 0 auto;
  font-size: 10px;
  font-weight: 600;
  padding: 2px 7px;
  border-radius: var(--r-full, 999px);
  white-space: nowrap;
}

.tt-mapping-review-badge.is-pending {
  background: var(--surface-hover);
  color: var(--muted);
}

.tt-mapping-review-badge.is-suggested {
  background: color-mix(in srgb, var(--brand) 14%, transparent);
  color: var(--brand-deep, var(--brand));
}

.tt-mapping-review-badge.is-approved {
  background: rgba(16, 185, 129, 0.16);
  color: #10b981;
}

.tt-mapping-review-badge.is-rejected {
  background: rgba(239, 68, 68, 0.12);
  color: #ef4444;
}

.tt-mapping-review-actions {
  flex: 0 0 auto;
  display: inline-flex;
  align-items: center;
  gap: 4px;
}

.tt-mapping-approve,
.tt-mapping-reject,
.tt-mapping-retry {
  height: 22px;
  padding: 0 9px;
  border-radius: var(--r-sm, 6px);
  border: 1px solid transparent;
  font-size: 11px;
  font-weight: 600;
  cursor: pointer;
  white-space: nowrap;
}

.tt-mapping-approve {
  background: var(--brand);
  color: #fff;
}

.tt-mapping-approve:hover {
  background: var(--brand-deep, var(--brand));
}

.tt-mapping-reject {
  background: transparent;
  border-color: var(--line);
  color: var(--muted);
}

.tt-mapping-reject:hover {
  border-color: rgba(239, 68, 68, 0.45);
  color: #ef4444;
}

.tt-mapping-retry {
  background: transparent;
  border-color: var(--line);
  color: var(--muted);
}

.tt-mapping-retry:hover {
  border-color: var(--brand);
  color: var(--brand);
}

/* 识别进度条 */
.tt-mapping-progress {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 8px 12px;
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  background: var(--page-bg);
  font-size: 11.5px;
}

.tt-mapping-progress > div:first-child {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
}

.tt-mapping-progress strong {
  font-size: 11.5px;
  font-weight: 600;
  color: var(--text);
}

.tt-mapping-progress span {
  color: var(--muted);
  font-variant-numeric: tabular-nums;
}

.tt-mapping-progress-track {
  height: 4px;
  border-radius: var(--r-full, 999px);
  background: var(--surface-hover);
  overflow: hidden;
}

.tt-mapping-progress-track i {
  display: block;
  height: 100%;
  border-radius: inherit;
  background: var(--brand);
  transition: width 0.25s ease;
}

/* ============================================================
   AI 用量洞察弹窗 (AI Insight Modal)
   ============================================================ */
.tt-insight-report {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.tt-insight-headline {
  margin: 0;
  padding: 12px 14px;
  border-radius: var(--r-md, 8px);
  background: color-mix(in srgb, var(--brand) 10%, transparent);
  border: 1px solid color-mix(in srgb, var(--brand) 30%, transparent);
  font-size: 13.5px;
  font-weight: 650;
  line-height: 1.6;
  color: var(--text);
}

.tt-insight-meta {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}

.tt-insight-meta span {
  padding: 2px 8px;
  border-radius: var(--r-full, 999px);
  background: var(--surface-hover);
  color: var(--muted);
  font-size: 10.5px;
  white-space: nowrap;
}

.tt-insight-section {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.tt-insight-section h4 {
  margin: 0;
  font-size: 12px;
  font-weight: 700;
  color: var(--muted);
  text-transform: uppercase;
  letter-spacing: 0.04em;
}

.tt-insight-item {
  padding: 10px 12px;
  border: 1px solid var(--line);
  border-left-width: 3px;
  border-radius: var(--r-sm, 6px);
  background: var(--page-bg);
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.tt-insight-item.is-high { border-left-color: #ef4444; }
.tt-insight-item.is-medium { border-left-color: #f97316; }
.tt-insight-item.is-low { border-left-color: #eab308; }
.tt-insight-item.is-info { border-left-color: var(--brand); }

.tt-insight-item-head {
  display: flex;
  align-items: center;
  gap: 8px;
}

.tt-insight-item-head strong {
  font-size: 12.5px;
  font-weight: 650;
}

.tt-insight-item p {
  margin: 0;
  font-size: 12px;
  line-height: 1.6;
  color: var(--text);
}

.tt-insight-severity {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex-shrink: 0;
}

.tt-insight-severity.is-high { background: #ef4444; }
.tt-insight-severity.is-medium { background: #f97316; }
.tt-insight-severity.is-low { background: #eab308; }
.tt-insight-severity.is-info { background: var(--brand); }

.tt-insight-evidence {
  font-size: 10.5px;
  color: var(--muted);
}

.tt-insight-stale {
  padding: 8px 12px;
  border-radius: var(--r-md, 8px);
  border: 1px solid color-mix(in srgb, #f97316 40%, transparent);
  background: color-mix(in srgb, #f97316 10%, transparent);
  color: var(--text);
  font-size: 11.5px;
  line-height: 1.6;
}


/* 视图二：转换后模型分组行 */
.tt-mapping-crow {
  border: 1px solid var(--line);
  border-radius: var(--r-sm, 6px);
  overflow: hidden;
}

.tt-mapping-crow.is-open {
  border-color: var(--line-strong, var(--line));
}

.tt-mapping-crow-head {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 7px 8px;
  border: none;
  background: transparent;
  color: var(--text);
  font-size: 11px;
  text-align: left;
  cursor: pointer;
}

.tt-mapping-crow-head:hover {
  background: var(--surface-hover);
}

.tt-mapping-crow-head small {
  margin-left: auto;
  color: var(--muted);
  font-size: 10px;
  white-space: nowrap;
}

.tt-mapping-chevron {
  display: inline-flex;
  transition: transform 0.15s var(--ease, ease);
  color: var(--muted);
}

.tt-mapping-crow.is-open .tt-mapping-chevron {
  transform: rotate(180deg);
}

.tt-mapping-crow-body {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 4px 6px 8px 28px;
  border-top: 1px dashed var(--line);
  background: var(--page-bg);
}

.tt-mapping-source-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 3px 4px;
  border-radius: var(--r-sm, 6px);
  font-size: 11px;
}

.tt-mapping-source-row:hover {
  background: var(--surface-hover);
}

.tt-mapping-source-row .tt-mapping-raw {
  flex: 1;
  min-width: 0;
  background: transparent;
  padding: 0;
}

.tt-mapping-crow-clear {
  align-self: flex-start;
  margin-top: 4px;
  padding: 3px 10px;
  border: 1px solid var(--line);
  border-radius: var(--r-full, 999px);
  background: var(--surface);
  color: #ef4444;
  font-size: 10px;
  cursor: pointer;
}

.tt-mapping-crow-clear:hover {
  border-color: #ef4444;
}

.tt-mapping-catalog-hint {
  font-size: 11px;
  color: var(--muted);
}

/* 工具条：左侧标签页 + 右侧计数 */
.tt-mapping-toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  flex-wrap: wrap;
}

.tt-mapping-filters {
  display: inline-flex;
  align-items: center;
  gap: 2px;
  background: var(--page-bg);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  padding: 2px;
}

.tt-mapping-filters button {
  height: 26px;
  padding: 0 11px;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: var(--muted);
  font-size: 11.5px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.12s ease;
  white-space: nowrap;
}

.tt-mapping-filters button:hover {
  color: var(--text);
}

.tt-mapping-filters button.is-active {
  background: var(--brand);
  color: #fff;
}

.tt-mapping-counter {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  margin-left: auto;
}

.tt-mapping-counter-chip {
  padding: 2px 9px;
  border-radius: var(--r-full, 999px);
  background: var(--page-bg);
  border: 1px solid var(--line);
  font-size: 10.5px;
  color: var(--muted);
  white-space: nowrap;
  font-variant-numeric: tabular-nums;
}

.tt-mapping-counter-chip.is-brand {
  background: color-mix(in srgb, var(--brand) 12%, transparent);
  border-color: color-mix(in srgb, var(--brand) 30%, transparent);
  color: var(--brand-deep, var(--brand));
  font-weight: 600;
}

.tt-mapping-raw {
  display: inline-block;
  max-width: 100%;
  padding: 3px 8px;
  border-radius: var(--r-sm, 6px);
  background: var(--surface-hover);
  font-family: var(--font-mono, ui-monospace, monospace);
  font-size: 11px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  vertical-align: middle;
}

.tt-mapping-model-input {
  width: 100%;
  height: 38px;
  padding: 0 12px;
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  background: var(--surface);
  color: var(--text);
  font-size: 12px;
  font-family: var(--font-mono, ui-monospace, monospace);
}

.tt-mapping-model-input:focus {
  outline: none;
  border-color: var(--brand);
}
</style>
