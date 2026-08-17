<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import type { ModelCatalogDetail, ModelCatalogItem } from "../types";
import AppTable, { type AppTableColumn } from "./AppTable.vue";
import CustomSelect from "./CustomSelect.vue";

const store = useStore();

// —— 视图模式 ——
type ViewMode = "cards" | "table" | "providers" | "arena";
const currentView = ref<ViewMode>("cards");

// —— 搜索与过滤 ——
const query = ref("");
const selectedLab = ref("all");
const pricingFilter = ref("all");
const statusFilter = ref("all");
const activeTab = ref("all");
const sortBy = ref("default");
const selectedProviderFilter = ref<string | null>(null);

// —— 特性过滤开关 ——
const featureFilters = ref({
  reasoning: false,
  toolCall: false,
  attachment: false,
  structured: false,
  openWeights: false,
  highSpeed: false,
  topQuality: false,
});

function toggleFeature(key: keyof typeof featureFilters.value) {
  featureFilters.value[key] = !featureFilters.value[key];
  currentPage.value = 1;
}

// —— 分页与排序 ——
const currentPage = ref(1);
const pageSize = ref(48);
const sorting = ref<Array<{ id: string; desc: boolean }>>([]);

// —— 详情抽屉 ——
const selectedId = ref("");
const detail = ref<ModelCatalogDetail | null>(null);
const detailLoading = ref(false);
const detailError = ref("");
const activeDetailTab = ref<"specs" | "pricing" | "aa" | "providers" | "code" | "raw">("specs");
const rawCopied = ref(false);
const codeCopied = ref(false);
const codeLanguage = ref<"python" | "node" | "curl">("python");
const codeSelectedProvider = ref("");

// —— 交互式成本计算器状态 ——
const calcMonthlyInputTokens = ref(10); // 百万 Tokens (M)
const calcMonthlyOutputTokens = ref(2); // 百万 Tokens (M)
const calcCurrency = ref<"USD" | "CNY">("USD");
const exchangeRate = 7.25;

// —— 多模型对战对比 Arena ——
const comparedModelIds = ref<string[]>([]);
const showArenaModal = ref(false);

function toggleCompareModel(id: string) {
  const idx = comparedModelIds.value.indexOf(id);
  if (idx >= 0) {
    comparedModelIds.value.splice(idx, 1);
  } else {
    if (comparedModelIds.value.length >= 4) {
      comparedModelIds.value.shift();
    }
    comparedModelIds.value.push(id);
  }
}

function clearCompare() {
  comparedModelIds.value = [];
}

const comparedModels = computed(() => {
  return comparedModelIds.value
    .map((id) => store.modelCatalog.value.models.find((m) => m.id === id))
    .filter((m): m is ModelCatalogItem => Boolean(m));
});

// —— 类型标签 ——
const typeTabs = [
  { key: "all", label: "全部", icon: "layers" },
  { key: "text", label: "对话/文本", icon: "chat" },
  { key: "image", label: "图像生成", icon: "eye" },
  { key: "video", label: "视频生成", icon: "video" },
  { key: "audio", label: "语音/音频", icon: "activity" },
  { key: "embedding", label: "向量嵌入", icon: "database" },
  { key: "classify", label: "分类审核", icon: "eyeOff" },
  { key: "rerank", label: "重排检索", icon: "pulse" },
] as const;

// —— 热门厂商列表 ——
const popularLabs = [
  { id: "openai", name: "OpenAI", logo: "openai", tone: "success" },
  { id: "anthropic", name: "Anthropic", logo: "anthropic", tone: "brand" },
  { id: "google", name: "Google", logo: "google", tone: "info" },
  { id: "deepseek", name: "DeepSeek", logo: "deepseek", tone: "violet" },
  { id: "alibaba", name: "Qwen", logo: "alibaba", tone: "warning" },
  { id: "zhipuai", name: "智谱 GLM", logo: "zhipuai", tone: "violet" },
  { id: "meta", name: "Llama", logo: "meta", tone: "info" },
  { id: "mistral", name: "Mistral", logo: "mistral", tone: "brand" },
  { id: "moonshotai", name: "Kimi", logo: "moonshotai", tone: "success" },
  { id: "minimax", name: "MiniMax", logo: "minimax", tone: "brand" },
  { id: "xai", name: "Grok", logo: "xai", tone: "neutral" },
];

const kindCounts = computed(() => {
  const counts: Record<string, number> = { all: store.modelCatalog.value.models.length };
  for (const model of store.modelCatalog.value.models) {
    const k = model.kind || "text";
    counts[k] = (counts[k] ?? 0) + 1;
  }
  return counts;
});

// —— 宏观数据统计指标 ——
const metrics = computed(() => {
  const models = store.modelCatalog.value.models;
  const totalModels = models.length;
  const openWeightsCount = models.filter((m) => m.openWeights).length;
  const reasoningCount = models.filter((m) => m.reasoning).length;
  const totalProviders = store.modelCatalog.value.providers.length;

  let topQualityModel: ModelCatalogItem | null = null;
  let fastestModel: ModelCatalogItem | null = null;
  let maxSpread = 1;

  for (const m of models) {
    if (m.aaIdx && (!topQualityModel || (topQualityModel.aaIdx ?? 0) < m.aaIdx)) {
      topQualityModel = m;
    }
    if (m.aaSpeed && (!fastestModel || (fastestModel.aaSpeed ?? 0) < m.aaSpeed)) {
      fastestModel = m;
    }
    if (m.priceSpread > maxSpread) {
      maxSpread = m.priceSpread;
    }
  }

  return {
    totalModels,
    openWeightsCount,
    reasoningCount,
    totalProviders,
    topQualityModel,
    fastestModel,
    maxSpread,
  };
});

// —— 厂商选项 ——
const labs = computed(() => {
  const set = new Set(store.modelCatalog.value.models.map((m) => m.lab).filter(Boolean));
  return Array.from(set).sort((a, b) => a.localeCompare(b));
});

const labOptions = computed(() => [
  { value: "all", text: "全部厂商" },
  ...labs.value.map((lab) => {
    const count = store.modelCatalog.value.models.filter((m) => m.lab === lab).length;
    return { value: lab, text: `${labLabel(lab)} (${count})` };
  }),
]);

const pricingOptions = [
  { value: "all", text: "全部价格" },
  { value: "freeHost", text: "免费可用 (Free Host)" },
  { value: "paid", text: "有标价 ($)" },
  { value: "refOfficial", text: "官方直销 (Official)" },
  { value: "spread", text: "显著价差 (>1.5x)" },
  { value: "budget", text: "极低单价 (<$0.5/1M)" },
];

const statusOptions = [
  { value: "all", text: "全部状态" },
  { value: "ga", text: "正式版 (GA)" },
  { value: "beta", text: "测试/预览 (Beta)" },
  { value: "deprecated", text: "已废弃 (Deprecated)" },
];

const sortOptions = [
  { value: "default", text: "综合排序" },
  { value: "aa_idx_desc", text: "AA 质量评分最高" },
  { value: "aa_speed_desc", text: "生成速率最快" },
  { value: "min_price_asc", text: "最低市场价 (从低到高)" },
  { value: "ref_price_asc", text: "官方价格 (从低到高)" },
  { value: "context_desc", text: "上下文长度 (从大到小)" },
  { value: "host_count_desc", text: "支持渠道最多" },
  { value: "spread_desc", text: "价差倍数最高" },
];

// —— 过滤与排序后的模型列表 ——
const filteredModels = computed(() => {
  const term = query.value.trim().toLowerCase();
  let list = store.modelCatalog.value.models.filter((model) => {
    // 1. 类型 Tab
    if (activeTab.value !== "all") {
      if ((model.kind || "text") !== activeTab.value) return false;
    }
    // 2. 厂商筛选
    if (selectedLab.value !== "all" && model.lab !== selectedLab.value) return false;
    // 3. 供应商过滤
    if (selectedProviderFilter.value && !model.hostProviders.includes(selectedProviderFilter.value)) {
      return false;
    }
    // 4. 价格筛选
    if (pricingFilter.value === "paid") {
      const hasPrice = model.refInputCost > 0 || model.refOutputCost > 0 || model.minInputCost > 0 || model.minOutputCost > 0;
      if (!hasPrice) return false;
    } else if (pricingFilter.value === "freeHost") {
      if (model.freeHostCount <= 0) return false;
    } else if (pricingFilter.value === "refOfficial") {
      if (!model.refOfficial) return false;
    } else if (pricingFilter.value === "spread") {
      if (model.priceSpread < 1.5) return false;
    } else if (pricingFilter.value === "budget") {
      if (model.minInputCost <= 0 || model.minInputCost > 0.5) return false;
    }
    // 5. 状态筛选
    if (statusFilter.value !== "all") {
      if (statusFilter.value === "ga" && model.status !== "ga") return false;
      if (statusFilter.value === "beta" && model.status !== "beta" && model.status !== "preview") return false;
      if (statusFilter.value === "deprecated" && model.status !== "deprecated") return false;
    }
    // 6. 特性快捷开关
    if (featureFilters.value.reasoning && !model.reasoning) return false;
    if (featureFilters.value.toolCall && !model.toolCall) return false;
    if (featureFilters.value.attachment && !model.attachment) return false;
    if (featureFilters.value.structured && !model.structured) return false;
    if (featureFilters.value.openWeights && !model.openWeights) return false;
    if (featureFilters.value.highSpeed && (!model.aaSpeed || model.aaSpeed < 100)) return false;
    if (featureFilters.value.topQuality && (!model.aaIdx || model.aaIdx < 80)) return false;

    // 7. 搜索关键字
    if (!term) return true;
    const haystack = [
      model.id,
      model.name,
      model.slug,
      model.lab,
      model.family ?? "",
      model.knowledge ?? "",
      labLabel(model.lab),
      ...(model.hostProviders ?? []),
    ].join(" ").toLowerCase();
    return haystack.includes(term);
  });

  // 排序
  if (sortBy.value === "aa_idx_desc") {
    list = [...list].sort((a, b) => (b.aaIdx ?? -1) - (a.aaIdx ?? -1));
  } else if (sortBy.value === "aa_speed_desc") {
    list = [...list].sort((a, b) => (b.aaSpeed ?? -1) - (a.aaSpeed ?? -1));
  } else if (sortBy.value === "min_price_asc") {
    list = [...list].sort((a, b) => {
      const aP = a.minInputCost > 0 ? a.minInputCost : 99999;
      const bP = b.minInputCost > 0 ? b.minInputCost : 99999;
      return aP - bP;
    });
  } else if (sortBy.value === "ref_price_asc") {
    list = [...list].sort((a, b) => {
      const aP = a.refInputCost > 0 ? a.refInputCost : 99999;
      const bP = b.refInputCost > 0 ? b.refInputCost : 99999;
      return aP - bP;
    });
  } else if (sortBy.value === "context_desc") {
    list = [...list].sort((a, b) => b.contextLength - a.contextLength);
  } else if (sortBy.value === "host_count_desc") {
    list = [...list].sort((a, b) => b.hostCount - a.hostCount);
  } else if (sortBy.value === "spread_desc") {
    list = [...list].sort((a, b) => b.priceSpread - a.priceSpread);
  }

  return list;
});

// —— 分页切片 ——
const paginatedModels = computed(() => {
  const start = (currentPage.value - 1) * pageSize.value;
  return filteredModels.value.slice(start, start + pageSize.value);
});

const totalPages = computed(() => Math.max(1, Math.ceil(filteredModels.value.length / pageSize.value)));

// —— 供应商列表与统计 ——
const providersList = computed(() => {
  const term = query.value.trim().toLowerCase();
  const list = store.modelCatalog.value.providers;
  if (!term) return list;
  return list.filter((p) => p.name.toLowerCase().includes(term) || p.id.toLowerCase().includes(term));
});

// —— 大表视图列配置 ——
const tableColumns = computed<AppTableColumn[]>(() => [
  { key: "name", title: "模型名称 / ID", width: "minmax(240px, 1.4fr)", sortable: true },
  { key: "lab", title: "厂商", width: "110px", sortable: true },
  { key: "contextLength", title: "上下文", width: "95px", align: "right" as const, sortable: true },
  { key: "maxOutputTokens", title: "最大输出", width: "90px", align: "right" as const, sortable: true },
  { key: "refPrice", title: "参考价格 (入/出/1M)", width: "160px", align: "right" as const, sortable: true },
  { key: "minPrice", title: "全网最低价 (1M)", width: "160px", align: "right" as const, sortable: true },
  { key: "hostCount", title: "渠道数", width: "85px", align: "right" as const, sortable: true },
  { key: "aaScores", title: "AA 质量/速度", width: "130px", align: "right" as const, sortable: true },
]);

const selectedModel = computed(() =>
  store.modelCatalog.value.models.find((model) => model.id === selectedId.value) ?? null,
);

// —— 格式化与映射工具 ——
const labLabels: Record<string, string> = {
  openai: "OpenAI",
  anthropic: "Anthropic",
  google: "Google",
  deepseek: "DeepSeek",
  alibaba: "阿里巴巴 (Qwen)",
  zhipuai: "智谱 AI (GLM)",
  mistral: "Mistral AI",
  meta: "Meta (Llama)",
  moonshotai: "月之暗面 (Kimi)",
  minimax: "MiniMax (海螺)",
  xai: "xAI (Grok)",
  cohere: "Cohere",
  nvidia: "NVIDIA",
  xiaomi: "小米 (MiMo)",
  baidu: "百度 (文心)",
  bytedance: "字节跳动 (豆包)",
  tencent: "腾讯 (混元)",
  stepfun: "阶跃星辰",
  baai: "智源研究院",
  stability: "Stability AI",
  microsoft: "Microsoft",
  misc: "开源社区",
};

function labLabel(lab: string) {
  return labLabels[lab.toLowerCase()] || lab || "未知厂商";
}

function labTone(lab: string): string {
  const map: Record<string, string> = {
    openai: "success",
    anthropic: "brand",
    google: "info",
    deepseek: "violet",
    alibaba: "warning",
    zhipuai: "violet",
    mistral: "brand",
    meta: "info",
    moonshotai: "success",
    minimax: "brand",
    xai: "neutral",
    nvidia: "success",
  };
  return map[lab.toLowerCase()] ?? "neutral";
}

function labInitials(lab: string): string {
  const l = labLabel(lab);
  if (!l || l === "未知厂商") return "?";
  const clean = l.replace(/[^\p{L}\p{N} ]/gu, "").trim();
  if (!clean) return "?";
  const words = clean.split(/\s+/).filter(Boolean);
  if (words.length >= 2) return (words[0][0] + words[1][0]).toUpperCase();
  return clean.slice(0, 2).toUpperCase();
}

const kindLabels: Record<string, string> = {
  text: "文本/对话",
  image: "图像生成",
  video: "视频生成",
  audio: "语音/音频",
  embedding: "向量嵌入",
  classify: "分类/审核",
  rerank: "重排检索",
};

function kindLabel(kind: string) {
  return kindLabels[kind] || kind || "通用模型";
}

function kindTone(kind: string): string {
  const map: Record<string, string> = {
    text: "brand",
    image: "success",
    video: "violet",
    audio: "info",
    embedding: "neutral",
    classify: "danger",
    rerank: "warning",
  };
  return map[kind] ?? "neutral";
}

function kindIcon(kind: string): keyof typeof icons {
  const map: Record<string, keyof typeof icons> = {
    text: "chat",
    image: "eye",
    video: "video",
    audio: "activity",
    embedding: "database",
    classify: "eyeOff",
    rerank: "pulse",
  };
  return map[kind] ?? "cpu";
}

function formatTokens(value: number): string {
  if (!value) return "—";
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(value % 1_000_000 ? 1 : 0)}M`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(value % 1_000 ? 1 : 0)}K`;
  return String(value);
}

function formatPrice(cost: number | undefined | null): string {
  if (cost === undefined || cost === null || cost <= 0) return "—";
  if (cost < 0.01) return `$${cost.toFixed(4)}`;
  if (cost < 1) return `$${cost.toFixed(3)}`;
  return `$${cost.toFixed(2)}`;
}

function dateText(value: string) {
  if (!value) return "尚未同步";
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString("zh-CN", { hour12: false });
}

// —— 交互逻辑 ——
async function openModelDetail(model: ModelCatalogItem) {
  selectedId.value = model.id;
  detail.value = null;
  detailError.value = "";
  detailLoading.value = true;
  activeDetailTab.value = "specs";
  codeSelectedProvider.value = model.hostProviders[0] || model.refProvider || "openrouter";
  try {
    detail.value = await store.getModelCatalogDetail(model.id);
  } catch (error) {
    detailError.value = String(error);
  } finally {
    detailLoading.value = false;
  }
}

function closeDetail() {
  selectedId.value = "";
  detail.value = null;
}

function filterByProvider(providerId: string) {
  selectedProviderFilter.value = providerId;
  currentView.value = "cards";
  currentPage.value = 1;
}

function clearProviderFilter() {
  selectedProviderFilter.value = null;
}

function selectLabQuick(labId: string) {
  if (selectedLab.value === labId) {
    selectedLab.value = "all";
  } else {
    selectedLab.value = labId;
  }
  currentPage.value = 1;
}

// —— 成本计算器计算 ——
const calculatedCosts = computed(() => {
  if (!selectedModel.value) return null;
  const m = selectedModel.value;
  const inM = calcMonthlyInputTokens.value;
  const outM = calcMonthlyOutputTokens.value;

  const refInputTotal = inM * m.refInputCost;
  const refOutputTotal = outM * m.refOutputCost;
  const refTotalUSD = refInputTotal + refOutputTotal;

  const minInputTotal = inM * (m.minInputCost || m.refInputCost);
  const minOutputTotal = outM * (m.minOutputCost || m.refOutputCost);
  const minTotalUSD = minInputTotal + minOutputTotal;

  const savedUSD = Math.max(0, refTotalUSD - minTotalUSD);
  const rate = calcCurrency.value === "CNY" ? exchangeRate : 1;
  const symbol = calcCurrency.value === "CNY" ? "¥" : "$";

  return {
    refTotal: (refTotalUSD * rate).toFixed(2),
    minTotal: (minTotalUSD * rate).toFixed(2),
    savedTotal: (savedUSD * rate).toFixed(2),
    savedPercent: refTotalUSD > 0 ? Math.round((savedUSD / refTotalUSD) * 100) : 0,
    symbol,
  };
});

// —— 生成调用代码 ——
const generatedSnippet = computed(() => {
  if (!selectedModel.value) return "";
  const modelId = selectedModel.value.id;
  const matchedProv = detail.value?.providers.find((p) => p.id === codeSelectedProvider.value);
  const baseUrl = matchedProv?.api || "https://api.openai.com/v1";

  if (codeLanguage.value === "python") {
    return `from openai import OpenAI

client = OpenAI(
    base_url="${baseUrl}",
    api_key="YOUR_API_KEY",
)

response = client.chat.completions.create(
    model="${modelId}",
    messages=[
        {"role": "system", "content": "You are a helpful AI assistant."},
        {"role": "user", "content": "Hello! Introduce yourself."},
    ],
    temperature=0.7,
)

print(response.choices[0].message.content)`;
  }

  if (codeLanguage.value === "node") {
    return `import OpenAI from "openai";

const client = new OpenAI({
  baseURL: "${baseUrl}",
  apiKey: process.env.API_KEY || "YOUR_API_KEY",
});

async function main() {
  const completion = await client.chat.completions.create({
    model: "${modelId}",
    messages: [
      { role: "system", content: "You are a helpful AI assistant." },
      { role: "user", content: "Hello! Introduce yourself." },
    ],
  });

  console.log(completion.choices[0].message.content);
}

main();`;
  }

  return `curl "${baseUrl}/chat/completions" \\
  -H "Content-Type: application/json" \\
  -H "Authorization: Bearer YOUR_API_KEY" \\
  -d '{
    "model": "${modelId}",
    "messages": [
      {"role": "user", "content": "Hello!"}
    ]
  }'`;
});

async function copyCode() {
  await navigator.clipboard.writeText(generatedSnippet.value);
  codeCopied.value = true;
  setTimeout(() => (codeCopied.value = false), 2000);
}

async function copyRawJson() {
  if (!detail.value) return;
  await navigator.clipboard.writeText(JSON.stringify(detail.value.raw || detail.value.model, null, 2));
  rawCopied.value = true;
  setTimeout(() => (rawCopied.value = false), 2000);
}

async function manualSync() {
  const result = await store.syncModelCatalog(true);
  if (!result || !selectedId.value) return;
  const selected = store.modelCatalog.value.models.find((model) => model.id === selectedId.value);
  if (selected) await openModelDetail(selected);
}

watch([query, selectedLab, pricingFilter, statusFilter, sortBy, activeTab, selectedProviderFilter], () => {
  currentPage.value = 1;
});

onMounted(() => {
  if (!store.modelCatalog.value.models.length && !store.modelCatalogLoading.value) {
    void store.loadModelCatalog();
  }
});
</script>

<template>
  <div class="mc-explorer-root">
    <!-- 1. 顶部宏观数据驾驶舱 -->
    <header class="mc-cockpit-bar">
      <div class="mc-cockpit-header">
        <div class="mc-brand-title">
          <div class="mc-brand-logo" v-html="icons.sparkles" />
          <div>
            <div class="mc-eyebrow">
              <span>LLMPricing · 全球大模型全景基准</span>
              <span class="mc-live-dot" />
            </div>
            <h1>模型全景控制台</h1>
          </div>
        </div>

        <div class="mc-cockpit-actions">
          <div class="mc-sync-status-card">
            <span class="mc-status-indicator" :class="{ synced: store.modelCatalog.value.syncedToday }" />
            <div class="mc-status-text">
              <strong>{{ store.modelCatalog.value.syncedToday ? "今日已同步" : "等待同步" }}</strong>
              <small>{{ dateText(store.modelCatalog.value.lastSyncedAt) }}</small>
            </div>
          </div>
          <button
            type="button"
            class="mc-sync-btn"
            :disabled="store.modelCatalogSyncing.value"
            @click="manualSync"
          >
            <span :class="{ 'is-spinning': store.modelCatalogSyncing.value }" v-html="icons.restore" />
            <span>{{ store.modelCatalogSyncing.value ? "同步中…" : "刷新全网数据" }}</span>
          </button>
        </div>
      </div>

      <!-- 宏观 4 大指标卡片 -->
      <div class="mc-metrics-grid">
        <div class="mc-metric-card">
          <div class="mc-metric-icon mc-tone-brand" v-html="icons.database" />
          <div class="mc-metric-info">
            <span class="mc-metric-label">收录大模型总数</span>
            <div class="mc-metric-val">
              <strong>{{ metrics.totalModels.toLocaleString() }}</strong>
              <small>含 {{ metrics.openWeightsCount }} 款开源权重</small>
            </div>
          </div>
        </div>

        <div class="mc-metric-card">
          <div class="mc-metric-icon mc-tone-info" v-html="icons.globe" />
          <div class="mc-metric-info">
            <span class="mc-metric-label">接入供应商渠道</span>
            <div class="mc-metric-val">
              <strong>{{ metrics.totalProviders }}</strong>
              <small>覆盖官方直销与聚合网关</small>
            </div>
          </div>
        </div>

        <div class="mc-metric-card">
          <div class="mc-metric-icon mc-tone-success" v-html="icons.card" />
          <div class="mc-metric-info">
            <span class="mc-metric-label">全网渠道最高价差</span>
            <div class="mc-metric-val">
              <strong class="text-success">{{ metrics.maxSpread.toFixed(1) }}x</strong>
              <small>多渠道比价最高节省 80%+</small>
            </div>
          </div>
        </div>

        <div class="mc-metric-card">
          <div class="mc-metric-icon mc-tone-violet" v-html="icons.flame" />
          <div class="mc-metric-info">
            <span class="mc-metric-label">AA 质量最高模型</span>
            <div class="mc-metric-val">
              <strong>{{ metrics.topQualityModel?.name || "Claude 3.7" }}</strong>
              <small class="text-violet">{{ metrics.topQualityModel?.aaIdx?.toFixed(1) }} 评测分</small>
            </div>
          </div>
        </div>
      </div>

      <!-- 热门厂商一键直达滚动栏 -->
      <div class="mc-popular-labs-bar">
        <span class="mc-labs-label">主流厂商：</span>
        <div class="mc-labs-scroll">
          <button
            type="button"
            class="mc-lab-chip"
            :class="{ active: selectedLab === 'all' }"
            @click="selectLabQuick('all')"
          >
            全部
          </button>
          <button
            v-for="lab in popularLabs"
            :key="lab.id"
            type="button"
            class="mc-lab-chip"
            :class="{ active: selectedLab === lab.id, [`mc-lab-${lab.tone}`]: true }"
            @click="selectLabQuick(lab.id)"
          >
            <span class="mc-lab-chip-avatar">{{ lab.name[0] }}</span>
            <span>{{ lab.name }}</span>
          </button>
        </div>
      </div>
    </header>

    <!-- 2. 控制中心：类型 Tab + 视图切换 + 筛选工具箱 -->
    <div class="mc-control-center">
      <!-- 上半区：分类 Tab 与 视图切换 -->
      <div class="mc-control-top-row">
        <!-- 模态类型 Tab -->
        <nav class="mc-kind-tabs" role="tablist">
          <button
            v-for="tab in typeTabs"
            :key="tab.key"
            type="button"
            role="tab"
            :aria-selected="activeTab === tab.key"
            :class="{ active: activeTab === tab.key }"
            @click="activeTab = tab.key"
          >
            <span v-html="icons[tab.icon]" />
            <span>{{ tab.label }}</span>
            <b class="mc-tab-badge">{{ kindCounts[tab.key] ?? 0 }}</b>
          </button>
        </nav>

        <!-- 视图切换模式 -->
        <div class="mc-view-switcher">
          <button
            type="button"
            :class="{ active: currentView === 'cards' }"
            title="画廊卡片视图"
            @click="currentView = 'cards'"
          >
            <span v-html="icons.grid" />
            <span>卡片</span>
          </button>
          <button
            type="button"
            :class="{ active: currentView === 'table' }"
            title="全景数据表视图"
            @click="currentView = 'table'"
          >
            <span v-html="icons.rows" />
            <span>数据表</span>
          </button>
          <button
            type="button"
            :class="{ active: currentView === 'providers' }"
            title="供应商拓扑矩阵"
            @click="currentView = 'providers'"
          >
            <span v-html="icons.globe" />
            <span>供应商</span>
          </button>
        </div>
      </div>

      <!-- 下半区：搜索、下拉筛选、特性快捷开关 -->
      <div class="mc-filters-row">
        <!-- 搜索框 -->
        <div class="mc-search-box">
          <span v-html="icons.search" />
          <input
            v-model="query"
            type="search"
            placeholder="搜索模型名称、ID、厂商、家族、支持渠道…"
          />
          <button v-if="query" type="button" class="mc-clear-search" @click="query = ''">
            <span v-html="icons.close" />
          </button>
        </div>

        <!-- 厂商下拉 -->
        <CustomSelect
          class="mc-filter-dropdown"
          :options="labOptions"
          :model-value="selectedLab"
          aria-label="厂商选择"
          @update:model-value="selectedLab = String($event)"
        />

        <!-- 价格模式 -->
        <CustomSelect
          class="mc-filter-dropdown"
          :options="pricingOptions"
          :model-value="pricingFilter"
          aria-label="价格模式"
          @update:model-value="pricingFilter = String($event)"
        />

        <!-- 状态过滤 -->
        <CustomSelect
          class="mc-filter-dropdown"
          :options="statusOptions"
          :model-value="statusFilter"
          aria-label="状态过滤"
          @update:model-value="statusFilter = String($event)"
        />

        <!-- 智能排序 -->
        <CustomSelect
          class="mc-filter-dropdown"
          :options="sortOptions"
          :model-value="sortBy"
          aria-label="排序规则"
          @update:model-value="sortBy = String($event)"
        />
      </div>

      <!-- 特性开关芯片栏 -->
      <div class="mc-feature-bar">
        <div class="mc-feature-chips">
          <span class="mc-chips-title">特性与基准：</span>
          <button
            type="button"
            class="mc-chip"
            :class="{ active: featureFilters.reasoning }"
            @click="toggleFeature('reasoning')"
          >
            🧠 深度思考推理
          </button>
          <button
            type="button"
            class="mc-chip"
            :class="{ active: featureFilters.toolCall }"
            @click="toggleFeature('toolCall')"
          >
            🛠️ 工具调用 (FC)
          </button>
          <button
            type="button"
            class="mc-chip"
            :class="{ active: featureFilters.attachment }"
            @click="toggleFeature('attachment')"
          >
            📄 附件直传
          </button>
          <button
            type="button"
            class="mc-chip"
            :class="{ active: featureFilters.structured }"
            @click="toggleFeature('structured')"
          >
            🧩 结构化 JSON
          </button>
          <button
            type="button"
            class="mc-chip"
            :class="{ active: featureFilters.openWeights }"
            @click="toggleFeature('openWeights')"
          >
            🔓 开源权重
          </button>
          <button
            type="button"
            class="mc-chip"
            :class="{ active: featureFilters.topQuality }"
            @click="toggleFeature('topQuality')"
          >
            🏆 AA 质量 > 80分
          </button>
          <button
            type="button"
            class="mc-chip"
            :class="{ active: featureFilters.highSpeed }"
            @click="toggleFeature('highSpeed')"
          >
            ⚡ 高速 > 100 t/s
          </button>
        </div>

        <!-- 当前过滤结果总数 -->
        <div class="mc-results-meta">
          <span v-if="selectedProviderFilter" class="mc-active-provider-filter">
            供应商：<b>{{ selectedProviderFilter }}</b>
            <button type="button" @click="clearProviderFilter"><span v-html="icons.close" /></button>
          </span>
          <span class="mc-filter-count">
            找到 <b>{{ filteredModels.length.toLocaleString() }}</b> 个匹配模型
          </span>
        </div>
      </div>
    </div>

    <!-- 3. 主视图区域 -->
    <main class="mc-main-content">
      <!-- 视图 A：智能画廊卡片流 -->
      <section v-if="currentView === 'cards'" class="mc-cards-view">
        <div v-if="store.modelCatalogLoading.value" class="mc-loading-state">
          <span class="is-spinning" v-html="icons.restore" />
          <p>正在读取模型全景数据…</p>
        </div>

        <div v-else-if="!filteredModels.length" class="mc-empty-state">
          <span v-html="icons.search" />
          <h3>未找到匹配的模型</h3>
          <p>请尝试重置筛选条件或更改搜索关键字</p>
        </div>

        <div v-else class="mc-cards-grid">
          <article
            v-for="model in paginatedModels"
            :key="model.id"
            class="mc-card"
            :class="{ 'is-selected': selectedId === model.id }"
            @click="openModelDetail(model)"
          >
            <!-- 卡片顶部：厂商 Avatar + 名称 + 类别 + 对比勾选 -->
            <div class="mc-card-head">
              <div class="mc-card-identity">
                <span class="mc-card-avatar" :class="`mc-tone-${labTone(model.lab)}`">
                  {{ labInitials(model.lab) }}
                </span>
                <div class="mc-card-title-box">
                  <div class="mc-card-title-row">
                    <h3 :title="model.name || model.id">{{ model.name || model.id }}</h3>
                    <span v-if="model.openWeights" class="mc-pill mc-pill-open" title="开源权重">开源</span>
                    <span v-if="model.status !== 'ga'" class="mc-pill mc-pill-beta">{{ model.status.toUpperCase() }}</span>
                  </div>
                  <small class="mc-card-id" :title="model.id">{{ model.id }}</small>
                </div>
              </div>

              <!-- 加入对比按钮 -->
              <button
                type="button"
                class="mc-card-compare-btn"
                :class="{ active: comparedModelIds.includes(model.id) }"
                title="加入对战对比"
                @click.stop="toggleCompareModel(model.id)"
              >
                <span v-html="comparedModelIds.includes(model.id) ? icons.check : icons.plus" />
              </button>
            </div>

            <!-- 卡片特性徽章栏 -->
            <div class="mc-card-badges">
              <span class="mc-card-tag" :class="`mc-tone-${labTone(model.lab)}`">
                {{ labLabel(model.lab) }}
              </span>
              <span class="mc-card-tag" :class="`mc-tone-${kindTone(model.kind)}`">
                <span class="mc-tag-icon" v-html="icons[kindIcon(model.kind)]" />
                {{ kindLabel(model.kind) }}
              </span>
              <span v-if="model.reasoning" class="mc-card-feat mc-feat-reasoning">🧠 深度推理</span>
              <span v-if="model.toolCall" class="mc-card-feat mc-feat-tool">🛠️ 工具调用</span>
              <span v-if="model.attachment" class="mc-card-feat">📄 多模态附件</span>
            </div>

            <!-- 参数规格条目 -->
            <div class="mc-card-specs">
              <div class="mc-spec-item">
                <span class="mc-spec-k">上下文区间</span>
                <strong class="mc-spec-v">{{ formatTokens(model.contextLength) }}</strong>
              </div>
              <div class="mc-spec-item">
                <span class="mc-spec-k">最大单次输出</span>
                <strong class="mc-spec-v">{{ formatTokens(model.maxOutputTokens) }}</strong>
              </div>
              <div class="mc-spec-item">
                <span class="mc-spec-k">支持渠道</span>
                <strong class="mc-spec-v">
                  {{ model.hostCount }} 家
                  <small v-if="model.freeHostCount > 0" class="mc-free-tag">({{ model.freeHostCount }} 免费)</small>
                </strong>
              </div>
            </div>

            <!-- 价格对比专区 -->
            <div class="mc-card-pricing-box">
              <div class="mc-price-col">
                <span class="mc-price-label">官方参考定价 (/1M)</span>
                <div class="mc-price-num">
                  <span>{{ formatPrice(model.refInputCost) }}</span>
                  <small>/</small>
                  <span>{{ formatPrice(model.refOutputCost) }}</span>
                </div>
              </div>

              <div class="mc-price-col mc-price-col-min">
                <div class="mc-price-label-row">
                  <span class="mc-price-label">全网最低渠道价</span>
                  <span v-if="model.priceSpread > 1.2" class="mc-savings-badge">
                    省 {{ Math.round((1 - 1 / model.priceSpread) * 100) }}%
                  </span>
                </div>
                <div class="mc-price-num text-success">
                  <strong>{{ formatPrice(model.minInputCost) }}</strong>
                  <small>/</small>
                  <strong>{{ formatPrice(model.minOutputCost) }}</strong>
                </div>
              </div>
            </div>

            <!-- 卡片底栏：AA 评测指标与进入详情 -->
            <div class="mc-card-footer">
              <div v-if="model.aaIdx || model.aaSpeed" class="mc-card-aa">
                <span v-if="model.aaIdx" class="mc-aa-badge-score">
                  <b v-html="icons.sparkles" />
                  {{ model.aaIdx.toFixed(1) }} 分
                </span>
                <span v-if="model.aaSpeed" class="mc-aa-badge-speed">
                  {{ Math.round(model.aaSpeed) }} tok/s
                </span>
              </div>
              <div v-else class="mc-card-aa-none">
                <span>标准基准收录</span>
              </div>

              <button type="button" class="mc-card-detail-btn" @click.stop="openModelDetail(model)">
                <span>全景参数</span>
                <span v-html="icons.chevron" />
              </button>
            </div>
          </article>
        </div>

        <!-- 卡片视图分页 -->
        <footer v-if="totalPages > 1" class="mc-pagination-bar">
          <button
            type="button"
            class="mc-page-btn"
            :disabled="currentPage <= 1"
            @click="currentPage--"
          >
            上一页
          </button>
          <span class="mc-page-info">第 {{ currentPage }} / {{ totalPages }} 页</span>
          <button
            type="button"
            class="mc-page-btn"
            :disabled="currentPage >= totalPages"
            @click="currentPage++"
          >
            下一页
          </button>
        </footer>
      </section>

      <!-- 视图 B：全景数据大表视图 -->
      <section v-else-if="currentView === 'table'" class="mc-table-view">
        <AppTable
          :rows="filteredModels"
          :columns="tableColumns"
          :row-key="(model: ModelCatalogItem) => model.id"
          :loading="store.modelCatalogLoading.value"
          empty-text="没有匹配的模型"
          :page="currentPage"
          :page-size="pageSize"
          :sorting="sorting"
          :selected-key="selectedId"
          clickable
          @update:page="currentPage = $event"
          @update:page-size="pageSize = $event"
          @update:sorting="sorting = $event"
          @select="openModelDetail"
        >
          <!-- 模型列 -->
          <template #cell-name="{ row }">
            <div class="mc-table-model-cell">
              <span class="mc-card-avatar mc-avatar-sm" :class="`mc-tone-${labTone(row.lab)}`">
                {{ labInitials(row.lab) }}
              </span>
              <div class="mc-table-model-info">
                <div class="mc-table-model-title">
                  <strong>{{ row.name || row.id }}</strong>
                  <span v-if="row.openWeights" class="mc-pill mc-pill-open">开源</span>
                  <span v-if="row.reasoning" class="mc-pill mc-pill-reasoning">🧠 推理</span>
                  <span v-if="row.status !== 'ga'" class="mc-pill mc-pill-beta">{{ row.status.toUpperCase() }}</span>
                </div>
                <small class="mc-table-model-id">{{ row.id }}</small>
              </div>
            </div>
          </template>

          <!-- 厂商列 -->
          <template #cell-lab="{ row }">
            <span class="mc-card-tag" :class="`mc-tone-${labTone(row.lab)}`">
              {{ labLabel(row.lab) }}
            </span>
          </template>

          <!-- 上下文 -->
          <template #cell-contextLength="{ row }">
            <span class="mc-mono-cell">{{ formatTokens(row.contextLength) }}</span>
          </template>

          <!-- 最大输出 -->
          <template #cell-maxOutputTokens="{ row }">
            <span class="mc-mono-cell">{{ formatTokens(row.maxOutputTokens) }}</span>
          </template>

          <!-- 参考价格 -->
          <template #cell-refPrice="{ row }">
            <div class="mc-table-price-cell">
              <span>{{ formatPrice(row.refInputCost) }} / {{ formatPrice(row.refOutputCost) }}</span>
              <small>{{ row.refProvider ? labLabel(row.refProvider) : "官方" }}</small>
            </div>
          </template>

          <!-- 最低价格 -->
          <template #cell-minPrice="{ row }">
            <div class="mc-table-price-cell text-success">
              <strong>{{ formatPrice(row.minInputCost) }} / {{ formatPrice(row.minOutputCost) }}</strong>
              <small>
                <span>{{ row.minProvider || "多渠道" }}</span>
                <b v-if="row.priceSpread > 1.2" class="mc-savings-badge-sm">-{{ Math.round((1 - 1 / row.priceSpread) * 100) }}%</b>
              </small>
            </div>
          </template>

          <!-- 渠道数 -->
          <template #cell-hostCount="{ row }">
            <div class="mc-table-host-cell">
              <strong>{{ row.hostCount }}</strong>
              <small v-if="row.freeHostCount > 0" class="mc-free-tag">{{ row.freeHostCount }} 免费</small>
            </div>
          </template>

          <!-- AA 评分 -->
          <template #cell-aaScores="{ row }">
            <div v-if="row.aaIdx || row.aaSpeed" class="mc-table-aa-cell">
              <span v-if="row.aaIdx" class="text-violet font-semibold">{{ row.aaIdx.toFixed(1) }} 分</span>
              <small v-if="row.aaSpeed">{{ Math.round(row.aaSpeed) }} t/s</small>
            </div>
            <span v-else class="muted">—</span>
          </template>
        </AppTable>
      </section>

      <!-- 视图 C：供应商拓扑矩阵视图 -->
      <section v-else-if="currentView === 'providers'" class="mc-providers-matrix-view">
        <div class="mc-matrix-header">
          <div>
            <h2>全网接入供应商拓扑（共 {{ providersList.length }} 家）</h2>
            <p>点击任意供应商卡片，可一键筛选并查看其支持的全部模型与 API 调用入口</p>
          </div>
        </div>

        <div class="mc-providers-matrix-grid">
          <article
            v-for="prov in providersList"
            :key="prov.id"
            class="mc-provider-matrix-card"
            :class="{ 'is-active-filter': selectedProviderFilter === prov.id }"
            @click="filterByProvider(prov.id)"
          >
            <div class="mc-pm-card-top">
              <div class="mc-pm-identity">
                <span class="mc-pm-avatar">{{ prov.name[0] }}</span>
                <div>
                  <strong>{{ prov.name }}</strong>
                  <div class="mc-pm-tags">
                    <span class="mc-tier-pill" :class="`mc-tier-${prov.tier || 'gateway'}`">
                      {{ (prov.tier || 'gateway').toUpperCase() }}
                    </span>
                    <span v-if="prov.subscription" class="mc-sub-pill">订阅制</span>
                  </div>
                </div>
              </div>

              <a
                v-if="prov.doc"
                :href="prov.doc"
                target="_blank"
                rel="noreferrer"
                class="mc-doc-link-btn"
                title="查看供应商官方文档"
                @click.stop
              >
                <span v-html="icons.external" />
              </a>
            </div>

            <p v-if="prov.api" class="mc-pm-api">
              <code>{{ prov.api }}</code>
            </p>

            <div class="mc-pm-footer">
              <span class="mc-pm-count">托管 <b>{{ prov.count }}</b> 款模型</span>
              <span class="mc-pm-action">查看模型 &rarr;</span>
            </div>
          </article>
        </div>
      </section>
    </main>

    <!-- 4. 多模型对战 Arena 浮动底栏 -->
    <aside v-if="comparedModelIds.length" class="mc-arena-dock">
      <div class="mc-arena-dock-content">
        <div class="mc-arena-dock-title">
          <span v-html="icons.flame" />
          <strong>模型对战对比 ({{ comparedModels.length }} / 4)</strong>
        </div>

        <div class="mc-arena-chips">
          <div v-for="m in comparedModels" :key="m.id" class="mc-arena-model-pill">
            <span>{{ m.name || m.id }}</span>
            <button type="button" @click="toggleCompareModel(m.id)">
              <span v-html="icons.close" />
            </button>
          </div>
        </div>

        <div class="mc-arena-dock-actions">
          <button type="button" class="mc-arena-clear-btn" @click="clearCompare">清空</button>
          <button
            type="button"
            class="primary-button mc-arena-launch-btn"
            :disabled="comparedModels.length < 2"
            @click="showArenaModal = true"
          >
            <span>开始横向对比</span>
            <span v-html="icons.arrowUp" />
          </button>
        </div>
      </div>
    </aside>

    <!-- 5. 全景模型详情深度抽屉 (Slide-over Drawer) -->
    <Teleport to="body">
      <div
        class="mc-drawer-backdrop"
        :hidden="!selectedModel"
        @click="closeDetail"
      >
        <aside
          class="mc-drawer-panel"
          role="dialog"
          aria-modal="true"
          :aria-label="selectedModel?.name || selectedModel?.id || '模型详情'"
          @click.stop
        >
          <template v-if="selectedModel">
            <!-- 抽屉头部 -->
            <header class="mc-drawer-header">
              <div class="mc-drawer-head-identity">
                <span class="mc-drawer-avatar" :class="`mc-tone-${labTone(selectedModel.lab)}`">
                  {{ labInitials(selectedModel.lab) }}
                </span>
                <div class="mc-drawer-title-box">
                  <div class="mc-drawer-tags">
                    <span class="mc-card-tag" :class="`mc-tone-${labTone(selectedModel.lab)}`">
                      {{ labLabel(selectedModel.lab) }}
                    </span>
                    <span class="mc-card-tag" :class="`mc-tone-${kindTone(selectedModel.kind)}`">
                      {{ kindLabel(selectedModel.kind) }}
                    </span>
                    <span v-if="selectedModel.openWeights" class="mc-pill mc-pill-open">开源权重</span>
                    <span v-if="selectedModel.status !== 'ga'" class="mc-pill mc-pill-beta">{{ selectedModel.status.toUpperCase() }}</span>
                  </div>
                  <h2>{{ selectedModel.name || selectedModel.id }}</h2>
                  <p class="mc-drawer-id">
                    <code>{{ selectedModel.id }}</code>
                  </p>
                </div>
              </div>

              <div class="mc-drawer-head-actions">
                <button
                  type="button"
                  class="mc-compare-toggle-btn"
                  :class="{ active: comparedModelIds.includes(selectedModel.id) }"
                  @click="toggleCompareModel(selectedModel.id)"
                >
                  <span v-html="comparedModelIds.includes(selectedModel.id) ? icons.check : icons.plus" />
                  <span>{{ comparedModelIds.includes(selectedModel.id) ? "已加入对战" : "加入对战" }}</span>
                </button>
                <button type="button" class="mc-drawer-close-btn" aria-label="关闭抽屉" @click="closeDetail">
                  <span v-html="icons.close" />
                </button>
              </div>
            </header>

            <!-- 抽屉 Tab 导航 -->
            <nav class="mc-drawer-tabs">
              <button
                type="button"
                :class="{ active: activeDetailTab === 'specs' }"
                @click="activeDetailTab = 'specs'"
              >
                <span v-html="icons.settings" />
                <span>特性矩阵</span>
              </button>
              <button
                type="button"
                :class="{ active: activeDetailTab === 'pricing' }"
                @click="activeDetailTab = 'pricing'"
              >
                <span v-html="icons.card" />
                <span>定价与算费器</span>
              </button>
              <button
                v-if="selectedModel.aaIdx || selectedModel.aaSpeed"
                type="button"
                :class="{ active: activeDetailTab === 'aa' }"
                @click="activeDetailTab = 'aa'"
              >
                <span v-html="icons.flame" />
                <span>AA 权威评测</span>
              </button>
              <button
                type="button"
                :class="{ active: activeDetailTab === 'providers' }"
                @click="activeDetailTab = 'providers'"
              >
                <span v-html="icons.globe" />
                <span>接入渠道 ({{ detail?.providers.length || selectedModel.hostProviders.length }})</span>
              </button>
              <button
                type="button"
                :class="{ active: activeDetailTab === 'code' }"
                @click="activeDetailTab = 'code'"
              >
                <span v-html="icons.edit" />
                <span>调用代码</span>
              </button>
              <button
                type="button"
                :class="{ active: activeDetailTab === 'raw' }"
                @click="activeDetailTab = 'raw'"
              >
                <span v-html="icons.database" />
                <span>原始 JSON</span>
              </button>
            </nav>

            <!-- 抽屉主体内容 -->
            <div class="mc-drawer-body">
              <div v-if="detailLoading" class="mc-loading-state">
                <span class="is-spinning" v-html="icons.restore" />
                <p>读取全景模型数据…</p>
              </div>

              <p v-else-if="detailError" class="mc-detail-error">{{ detailError }}</p>

              <template v-else-if="detail">
                <!-- TAB 1: 核心参数与能力矩阵 -->
                <div v-if="activeDetailTab === 'specs'" class="mc-tab-panel">
                  <div class="mc-specs-grid-6">
                    <div class="mc-spec-box">
                      <span>最大上下文容量</span>
                      <strong>{{ formatTokens(selectedModel.contextLength) }} Tokens</strong>
                    </div>
                    <div class="mc-spec-box">
                      <span>上下文支持区间</span>
                      <strong>{{ formatTokens(selectedModel.contextMin) }} ~ {{ formatTokens(selectedModel.contextMax) }}</strong>
                    </div>
                    <div class="mc-spec-box">
                      <span>最大单次输出</span>
                      <strong>{{ selectedModel.maxOutputTokens ? `${formatTokens(selectedModel.maxOutputTokens)} Tokens` : "—" }}</strong>
                    </div>
                    <div class="mc-spec-box">
                      <span>知识库截止日期</span>
                      <strong>{{ selectedModel.knowledge || "未指定" }}</strong>
                    </div>
                    <div class="mc-spec-box">
                      <span>官方发布时间</span>
                      <strong>{{ selectedModel.releaseDate || "—" }}</strong>
                    </div>
                    <div class="mc-spec-box">
                      <span>权重开放协议</span>
                      <strong>{{ selectedModel.openWeights ? "开源 (Open Weights)" : "闭源商用" }}</strong>
                    </div>
                  </div>

                  <!-- 能力清单 -->
                  <h4 class="mc-subheading">能力矩阵清单</h4>
                  <div class="mc-capability-matrix">
                    <div class="mc-cap-card" :class="{ enabled: selectedModel.reasoning }">
                      <span class="mc-cap-emoji">🧠</span>
                      <div>
                        <strong>深度思考推理 (Reasoning)</strong>
                        <p>{{ selectedModel.reasoning ? "具备原生深度思考链 (CoT) 与推理模式" : "标准非推理模型" }}</p>
                      </div>
                    </div>
                    <div class="mc-cap-card" :class="{ enabled: selectedModel.toolCall }">
                      <span class="mc-cap-emoji">🛠️</span>
                      <div>
                        <strong>函数与工具调用 (Tool Call)</strong>
                        <p>{{ selectedModel.toolCall ? "原生支持 Function Calling 与外部工具交互" : "暂不支持结构化工具调用" }}</p>
                      </div>
                    </div>
                    <div class="mc-cap-card" :class="{ enabled: selectedModel.structured }">
                      <span class="mc-cap-emoji">🧩</span>
                      <div>
                        <strong>结构化输出 (Structured JSON)</strong>
                        <p>{{ selectedModel.structured ? "严格遵循 JSON Schema 格式化输出" : "自由文本输出" }}</p>
                      </div>
                    </div>
                    <div class="mc-cap-card" :class="{ enabled: selectedModel.attachment }">
                      <span class="mc-cap-emoji">📄</span>
                      <div>
                        <strong>多模态文件附件 (Attachments)</strong>
                        <p>{{ selectedModel.attachment ? "支持文档、图片等附件直接输入" : "仅限纯文本输入" }}</p>
                      </div>
                    </div>
                  </div>

                  <!-- 输入模态 -->
                  <div v-if="selectedModel.inputModalities.length" class="mc-modalities-box">
                    <span class="mc-modalities-title">支持输入模态：</span>
                    <div class="mc-mod-tags">
                      <span v-for="mod in selectedModel.inputModalities" :key="mod" class="mc-mod-badge">
                        {{ mod.toUpperCase() }}
                      </span>
                    </div>
                  </div>
                </div>

                <!-- TAB 2: 定价与交互式算费器 -->
                <div v-else-if="activeDetailTab === 'pricing'" class="mc-tab-panel">
                  <div class="mc-pricing-cards-3">
                    <div class="mc-pricing-card">
                      <div class="mc-pcard-header">
                        <span class="mc-pcard-tag mc-tone-brand">官方 / 参考定价</span>
                        <small>{{ selectedModel.refProvider ? labLabel(selectedModel.refProvider) : "官方标准" }}</small>
                      </div>
                      <div class="mc-pcard-rows">
                        <div><span>输入 (Input / 1M)</span><strong>{{ formatPrice(selectedModel.refInputCost) }}</strong></div>
                        <div><span>输出 (Output / 1M)</span><strong>{{ formatPrice(selectedModel.refOutputCost) }}</strong></div>
                        <div><span>缓存读取 (Cache)</span><strong>{{ formatPrice(selectedModel.refCacheReadCost) }}</strong></div>
                      </div>
                    </div>

                    <div class="mc-pricing-card mc-pcard-lowest">
                      <div class="mc-pcard-header">
                        <span class="mc-pcard-tag mc-tone-success">全网最低渠道价</span>
                        <small>{{ selectedModel.minProvider || "最低" }}</small>
                      </div>
                      <div class="mc-pcard-rows">
                        <div><span>最低输入</span><strong class="text-success">{{ formatPrice(selectedModel.minInputCost) }}</strong></div>
                        <div><span>最低输出</span><strong class="text-success">{{ formatPrice(selectedModel.minOutputCost) }}</strong></div>
                        <div><span>最低缓存读取</span><strong class="text-success">{{ formatPrice(selectedModel.minCacheReadCost) }}</strong></div>
                      </div>
                    </div>

                    <div class="mc-pricing-card mc-pcard-spread">
                      <div class="mc-pcard-header">
                        <span class="mc-pcard-tag mc-tone-violet">价差与渠道潜力</span>
                      </div>
                      <div class="mc-pcard-rows">
                        <div><span>最高/最低价差</span><strong>{{ selectedModel.priceSpread.toFixed(1) }}x</strong></div>
                        <div><span>Blended 指数</span><strong>{{ formatPrice(selectedModel.blendedMin) }}</strong></div>
                        <div><span>支持供应商</span><strong>{{ selectedModel.hostCount }} 家</strong></div>
                      </div>
                    </div>
                  </div>

                  <!-- 交互式实时成本计算器 -->
                  <div class="mc-cost-calculator">
                    <div class="mc-calc-head">
                      <div>
                        <h4><span class="mc-calc-icon" v-html="icons.card" />交互式 Token 月度费用计算器</h4>
                        <p>拖动滑块自定义每月 Token 用量，实时对比官方直销与最低渠道费用</p>
                      </div>
                      <div class="mc-currency-toggle">
                        <button
                          type="button"
                          :class="{ active: calcCurrency === 'USD' }"
                          @click="calcCurrency = 'USD'"
                        >
                          USD ($)
                        </button>
                        <button
                          type="button"
                          :class="{ active: calcCurrency === 'CNY' }"
                          @click="calcCurrency = 'CNY'"
                        >
                          CNY (¥)
                        </button>
                      </div>
                    </div>

                    <div class="mc-calc-sliders">
                      <div class="mc-slider-group">
                        <div class="mc-slider-label">
                          <span>每月输入 Tokens</span>
                          <b>{{ calcMonthlyInputTokens }}M Tokens</b>
                        </div>
                        <input
                          v-model.number="calcMonthlyInputTokens"
                          type="range"
                          min="1"
                          max="100"
                          step="1"
                        />
                      </div>

                      <div class="mc-slider-group">
                        <div class="mc-slider-label">
                          <span>每月输出 Tokens</span>
                          <b>{{ calcMonthlyOutputTokens }}M Tokens</b>
                        </div>
                        <input
                          v-model.number="calcMonthlyOutputTokens"
                          type="range"
                          min="1"
                          max="50"
                          step="1"
                        />
                      </div>
                    </div>

                    <div v-if="calculatedCosts" class="mc-calc-results-bar">
                      <div class="mc-calc-res-item">
                        <span>官方预估月费</span>
                        <strong>{{ calculatedCosts.symbol }}{{ calculatedCosts.refTotal }}</strong>
                      </div>
                      <div class="mc-calc-res-item">
                        <span>最低渠道月费</span>
                        <strong class="text-success">{{ calculatedCosts.symbol }}{{ calculatedCosts.minTotal }}</strong>
                      </div>
                      <div class="mc-calc-res-item mc-calc-res-saved">
                        <span>预计每月节省</span>
                        <strong>{{ calculatedCosts.symbol }}{{ calculatedCosts.savedTotal }} <small>({{ calculatedCosts.savedPercent }}%)</small></strong>
                      </div>
                    </div>
                  </div>
                </div>

                <!-- TAB 3: Artificial Analysis 权威评测 -->
                <div v-else-if="activeDetailTab === 'aa'" class="mc-tab-panel">
                  <div class="mc-aa-hero-card">
                    <div class="mc-aa-hero-title">
                      <span v-html="icons.sparkles" />
                      <div>
                        <h4>Artificial Analysis 全球大模型权威基准评测</h4>
                        <p>基于标准化测试集评估模型综合质量、代码能力、Agentic 表现与吞吐速率</p>
                      </div>
                    </div>

                    <div class="mc-aa-gauges-grid">
                      <div v-if="selectedModel.aaIdx" class="mc-gauge-card">
                        <span class="mc-gauge-k">综合质量指数 (Quality Index)</span>
                        <div class="mc-gauge-bar-wrap">
                          <div class="mc-gauge-bar" :style="{ width: `${Math.min(100, selectedModel.aaIdx)}%` }" />
                        </div>
                        <strong class="mc-gauge-v">{{ selectedModel.aaIdx.toFixed(1) }} <small>/ 100</small></strong>
                      </div>

                      <div v-if="selectedModel.aaCoding" class="mc-gauge-card">
                        <span class="mc-gauge-k">代码编程能力 (Coding Score)</span>
                        <div class="mc-gauge-bar-wrap">
                          <div class="mc-gauge-bar mc-bar-brand" :style="{ width: `${Math.min(100, selectedModel.aaCoding)}%` }" />
                        </div>
                        <strong class="mc-gauge-v">{{ selectedModel.aaCoding.toFixed(1) }} <small>/ 100</small></strong>
                      </div>

                      <div v-if="selectedModel.aaAgentic" class="mc-gauge-card">
                        <span class="mc-gauge-k">智能体评分 (Agentic Score)</span>
                        <div class="mc-gauge-bar-wrap">
                          <div class="mc-gauge-bar mc-bar-violet" :style="{ width: `${Math.min(100, selectedModel.aaAgentic)}%` }" />
                        </div>
                        <strong class="mc-gauge-v">{{ selectedModel.aaAgentic.toFixed(1) }} <small>/ 100</small></strong>
                      </div>

                      <div v-if="selectedModel.aaSpeed" class="mc-gauge-card">
                        <span class="mc-gauge-k">输出生成速率 (Speed)</span>
                        <strong class="mc-gauge-v text-success">{{ Math.round(selectedModel.aaSpeed) }} <small>tok/s</small></strong>
                      </div>

                      <div v-if="selectedModel.aaTtft" class="mc-gauge-card">
                        <span class="mc-gauge-k">首字延迟 (TTFT Latency)</span>
                        <strong class="mc-gauge-v">{{ selectedModel.aaTtft.toFixed(2) }} <small>秒</small></strong>
                      </div>

                      <div v-if="selectedModel.aaTaskCost" class="mc-gauge-card">
                        <span class="mc-gauge-k">基准任务单次平均费用</span>
                        <strong class="mc-gauge-v">${{ selectedModel.aaTaskCost.toFixed(4) }}</strong>
                      </div>
                    </div>
                  </div>
                </div>

                <!-- TAB 4: 接入渠道与供应商拓扑 -->
                <div v-else-if="activeDetailTab === 'providers'" class="mc-tab-panel">
                  <div class="mc-providers-panel-grid">
                    <div
                      v-for="prov in detail.providers"
                      :key="prov.id"
                      class="mc-prov-card"
                    >
                      <div class="mc-prov-card-top">
                        <div class="mc-prov-info">
                          <strong>{{ prov.name }}</strong>
                          <span class="mc-tier-pill" :class="`mc-tier-${prov.tier || 'gateway'}`">
                            {{ (prov.tier || 'gateway').toUpperCase() }}
                          </span>
                          <span v-if="prov.subscription" class="mc-sub-pill">订阅制</span>
                        </div>
                        <a
                          v-if="prov.doc"
                          :href="prov.doc"
                          target="_blank"
                          rel="noreferrer"
                          class="mc-doc-link-btn"
                          title="查看官方文档"
                        >
                          <span v-html="icons.external" />
                        </a>
                      </div>

                      <div v-if="prov.api" class="mc-prov-api-row">
                        <code>{{ prov.api }}</code>
                      </div>
                    </div>
                  </div>
                </div>

                <!-- TAB 5: 快捷调用代码生成 -->
                <div v-else-if="activeDetailTab === 'code'" class="mc-tab-panel">
                  <div class="mc-code-snippet-box">
                    <div class="mc-snippet-toolbar">
                      <div class="mc-code-lang-selector">
                        <button
                          type="button"
                          :class="{ active: codeLanguage === 'python' }"
                          @click="codeLanguage = 'python'"
                        >
                          Python (OpenAI SDK)
                        </button>
                        <button
                          type="button"
                          :class="{ active: codeLanguage === 'node' }"
                          @click="codeLanguage = 'node'"
                        >
                          Node.js (TypeScript)
                        </button>
                        <button
                          type="button"
                          :class="{ active: codeLanguage === 'curl' }"
                          @click="codeLanguage = 'curl'"
                        >
                          cURL
                        </button>
                      </div>

                      <button type="button" class="mc-copy-btn" @click="copyCode">
                        <span v-html="codeCopied ? icons.check : icons.copy" />
                        <span>{{ codeCopied ? "已复制" : "复制代码" }}</span>
                      </button>
                    </div>

                    <pre class="mc-code-block"><code>{{ generatedSnippet }}</code></pre>
                  </div>
                </div>

                <!-- TAB 6: 原始数据检查器 -->
                <div v-else-if="activeDetailTab === 'raw'" class="mc-tab-panel">
                  <div class="mc-raw-box">
                    <div class="mc-raw-top">
                      <span>完整 JSON 元数据</span>
                      <button type="button" class="mc-copy-btn" @click="copyRawJson">
                        <span v-html="rawCopied ? icons.check : icons.copy" />
                        <span>{{ rawCopied ? "已复制" : "复制 JSON" }}</span>
                      </button>
                    </div>
                    <pre class="mc-code-block"><code>{{ JSON.stringify(detail.raw || detail.model, null, 2) }}</code></pre>
                  </div>
                </div>
              </template>
            </div>
          </template>
        </aside>
      </div>
    </Teleport>

    <!-- 6. 多模型对战对比 Arena 全屏弹窗 -->
    <Teleport to="body">
      <div
        v-if="showArenaModal && comparedModels.length >= 2"
        class="mc-arena-modal-backdrop"
        @click="showArenaModal = false"
      >
        <div class="mc-arena-modal" role="dialog" aria-modal="true" @click.stop>
          <header class="mc-arena-modal-head">
            <div class="mc-arena-modal-title">
              <span class="mc-arena-flame" v-html="icons.flame" />
              <div>
                <h2>多模型全方位横向对战 (Arena Comparison)</h2>
                <p>对比 {{ comparedModels.length }} 款模型的参数规格、定价阶梯、能力支持与 AA 评测指标</p>
              </div>
            </div>
            <button type="button" class="mc-drawer-close-btn" @click="showArenaModal = false">
              <span v-html="icons.close" />
            </button>
          </header>

          <div class="mc-arena-table-wrap">
            <table class="mc-arena-table">
              <thead>
                <tr>
                  <th class="mc-arena-feature-col">对比维度</th>
                  <th v-for="m in comparedModels" :key="m.id">
                    <div class="mc-arena-th-cell">
                      <span class="mc-card-avatar" :class="`mc-tone-${labTone(m.lab)}`">
                        {{ labInitials(m.lab) }}
                      </span>
                      <div>
                        <strong>{{ m.name || m.id }}</strong>
                        <small>{{ labLabel(m.lab) }}</small>
                      </div>
                    </div>
                  </th>
                </tr>
              </thead>
              <tbody>
                <tr>
                  <td class="mc-arena-feature-col">模型类别</td>
                  <td v-for="m in comparedModels" :key="m.id">
                    <span class="mc-card-tag" :class="`mc-tone-${kindTone(m.kind)}`">{{ kindLabel(m.kind) }}</span>
                  </td>
                </tr>
                <tr>
                  <td class="mc-arena-feature-col">开源权重</td>
                  <td v-for="m in comparedModels" :key="m.id">
                    <span :class="m.openWeights ? 'text-success font-semibold' : 'muted'">
                      {{ m.openWeights ? "✓ 开源" : "闭源商用" }}
                    </span>
                  </td>
                </tr>
                <tr>
                  <td class="mc-arena-feature-col">最大上下文</td>
                  <td v-for="m in comparedModels" :key="m.id">
                    <strong>{{ formatTokens(m.contextLength) }} Tokens</strong>
                  </td>
                </tr>
                <tr>
                  <td class="mc-arena-feature-col">最大输出限制</td>
                  <td v-for="m in comparedModels" :key="m.id">
                    <span>{{ formatTokens(m.maxOutputTokens) }}</span>
                  </td>
                </tr>
                <tr>
                  <td class="mc-arena-feature-col">深度思考推理</td>
                  <td v-for="m in comparedModels" :key="m.id">
                    <span>{{ m.reasoning ? "🧠 支持" : "—" }}</span>
                  </td>
                </tr>
                <tr>
                  <td class="mc-arena-feature-col">函数/工具调用</td>
                  <td v-for="m in comparedModels" :key="m.id">
                    <span>{{ m.toolCall ? "🛠️ 支持" : "—" }}</span>
                  </td>
                </tr>
                <tr>
                  <td class="mc-arena-feature-col">官方参考价格 (/1M)</td>
                  <td v-for="m in comparedModels" :key="m.id">
                    <strong>{{ formatPrice(m.refInputCost) }} / {{ formatPrice(m.refOutputCost) }}</strong>
                  </td>
                </tr>
                <tr>
                  <td class="mc-arena-feature-col">全网最低渠道价 (/1M)</td>
                  <td v-for="m in comparedModels" :key="m.id">
                    <strong class="text-success">{{ formatPrice(m.minInputCost) }} / {{ formatPrice(m.minOutputCost) }}</strong>
                  </td>
                </tr>
                <tr>
                  <td class="mc-arena-feature-col">渠道价差倍数</td>
                  <td v-for="m in comparedModels" :key="m.id">
                    <span class="mc-savings-badge">{{ m.priceSpread.toFixed(1) }}x</span>
                  </td>
                </tr>
                <tr>
                  <td class="mc-arena-feature-col">AA 综合质量指数</td>
                  <td v-for="m in comparedModels" :key="m.id">
                    <strong class="text-violet font-bold">{{ m.aaIdx ? `${m.aaIdx.toFixed(1)} 分` : "—" }}</strong>
                  </td>
                </tr>
                <tr>
                  <td class="mc-arena-feature-col">AA 生成速率</td>
                  <td v-for="m in comparedModels" :key="m.id">
                    <span>{{ m.aaSpeed ? `${Math.round(m.aaSpeed)} tok/s` : "—" }}</span>
                  </td>
                </tr>
                <tr>
                  <td class="mc-arena-feature-col">支持供应商数量</td>
                  <td v-for="m in comparedModels" :key="m.id">
                    <span>{{ m.hostCount }} 家</span>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        </div>
      </div>
    </Teleport>
  </div>
</template>

<style scoped>
/* —— 根布局 —— */
.mc-explorer-root {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  background: var(--bg);
  color: var(--text);
  overflow-y: auto;
  position: relative;
}

/* —— 1. 顶部宏观驾驶舱 —— */
.mc-cockpit-bar {
  padding: 20px 24px 16px;
  background: var(--surface);
  border-bottom: 1px solid var(--line);
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.mc-cockpit-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 16px;
  flex-wrap: wrap;
}

.mc-brand-title {
  display: flex;
  align-items: center;
  gap: 12px;
}

.mc-brand-logo {
  width: 42px;
  height: 42px;
  border-radius: var(--r-lg);
  background: linear-gradient(135deg, var(--brand), #818cf8);
  display: flex;
  align-items: center;
  justify-content: center;
  color: #fff;
  box-shadow: 0 4px 12px var(--brand-glow);
}
.mc-brand-logo :deep(svg) {
  width: 22px;
  height: 22px;
}

.mc-eyebrow {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 11px;
  font-weight: 600;
  color: var(--brand);
  text-transform: uppercase;
  letter-spacing: 0.04em;
}

.mc-live-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--success);
  box-shadow: 0 0 0 3px rgba(18, 166, 101, 0.2);
  animation: pulse-dot 2s infinite;
}

@keyframes pulse-dot {
  0%, 100% { transform: scale(1); opacity: 1; }
  50% { transform: scale(1.3); opacity: 0.6; }
}

.mc-brand-title h1 {
  margin: 2px 0 0;
  font-size: 20px;
  font-weight: 700;
  letter-spacing: -0.02em;
}

.mc-cockpit-actions {
  display: flex;
  align-items: center;
  gap: 12px;
}

.mc-sync-status-card {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 12px;
  border-radius: var(--r-md);
  background: var(--surface-soft);
  border: 1px solid var(--line-soft);
}

.mc-status-indicator {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--warning);
}
.mc-status-indicator.synced {
  background: var(--success);
}

.mc-status-text {
  display: flex;
  flex-direction: column;
}
.mc-status-text strong {
  font-size: 11px;
  color: var(--text);
}
.mc-status-text small {
  font-size: 9.5px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
}

.mc-sync-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 8px 14px;
  border-radius: var(--r-md);
  font-size: 12px;
  font-weight: 600;
  background: var(--surface-soft);
  border: 1px solid var(--line);
  color: var(--text);
  cursor: pointer;
  transition: all 0.15s ease;
}
.mc-sync-btn:hover {
  background: var(--surface-hover);
  border-color: var(--brand);
}
.mc-sync-btn :deep(svg) {
  width: 14px;
  height: 14px;
}

.is-spinning :deep(svg),
.is-spinning {
  animation: spin 0.8s linear infinite;
}
@keyframes spin {
  to { transform: rotate(360deg); }
}

/* 4 大宏观指标 */
.mc-metrics-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
  gap: 12px;
}

.mc-metric-card {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px 14px;
  border-radius: var(--r-lg);
  background: var(--surface-soft);
  border: 1px solid var(--line-soft);
  transition: all 0.2s ease;
}
.mc-metric-card:hover {
  border-color: var(--line);
  transform: translateY(-1px);
  box-shadow: var(--shadow-sm);
}

.mc-metric-icon {
  width: 38px;
  height: 38px;
  border-radius: var(--r-md);
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
}
.mc-metric-icon :deep(svg) {
  width: 18px;
  height: 18px;
}

.mc-tone-brand { background: var(--brand-soft); color: var(--brand); }
.mc-tone-info { background: var(--info-soft); color: var(--info); }
.mc-tone-success { background: var(--success-soft); color: var(--success); }
.mc-tone-violet { background: var(--violet-soft); color: var(--violet); }
.mc-tone-warning { background: var(--warning-soft); color: var(--warning); }
.mc-tone-danger { background: var(--danger-soft); color: var(--danger); }
.mc-tone-neutral { background: var(--surface-hover); color: var(--muted); }

.mc-metric-info {
  display: flex;
  flex-direction: column;
  min-width: 0;
}
.mc-metric-label {
  font-size: 11px;
  color: var(--muted);
}
.mc-metric-val {
  display: flex;
  align-items: baseline;
  gap: 6px;
  margin-top: 2px;
}
.mc-metric-val strong {
  font-size: 17px;
  font-weight: 700;
  color: var(--text);
  font-variant-numeric: tabular-nums;
}
.mc-metric-val small {
  font-size: 10px;
  color: var(--muted);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

/* 热门厂商直达 */
.mc-popular-labs-bar {
  display: flex;
  align-items: center;
  gap: 8px;
  padding-top: 4px;
}
.mc-labs-label {
  font-size: 11.5px;
  color: var(--muted);
  flex-shrink: 0;
}
.mc-labs-scroll {
  display: flex;
  align-items: center;
  gap: 6px;
  overflow-x: auto;
  padding-bottom: 2px;
}
.mc-lab-chip {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 3px 9px;
  border-radius: var(--r-full);
  font-size: 11.5px;
  font-weight: 500;
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--text);
  cursor: pointer;
  transition: all 0.15s ease;
  white-space: nowrap;
}
.mc-lab-chip:hover {
  background: var(--surface-hover);
  border-color: var(--line-strong);
}
.mc-lab-chip.active {
  background: var(--brand-soft);
  border-color: var(--brand);
  color: var(--brand-deep);
  font-weight: 600;
}
.mc-lab-chip-avatar {
  width: 14px;
  height: 14px;
  border-radius: 50%;
  background: rgba(0, 0, 0, 0.08);
  font-size: 9px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
}

/* —— 2. 控制中心 —— */
.mc-control-center {
  padding: 14px 24px;
  background: var(--surface);
  border-bottom: 1px solid var(--line);
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.mc-control-top-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 16px;
  flex-wrap: wrap;
}

.mc-kind-tabs {
  display: flex;
  align-items: center;
  gap: 4px;
  overflow-x: auto;
}
.mc-kind-tabs button {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 6px 12px;
  border-radius: var(--r-md);
  font-size: 12px;
  font-weight: 500;
  border: none;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  transition: all 0.15s ease;
  white-space: nowrap;
}
.mc-kind-tabs button:hover {
  background: var(--surface-hover);
  color: var(--text);
}
.mc-kind-tabs button.active {
  background: var(--surface-soft);
  color: var(--text);
  font-weight: 600;
  box-shadow: var(--shadow-xs);
}
.mc-kind-tabs button :deep(svg) {
  width: 14px;
  height: 14px;
}

.mc-tab-badge {
  font-size: 10px;
  font-weight: 600;
  padding: 1px 5px;
  border-radius: var(--r-full);
  background: var(--surface-hover);
  color: var(--muted);
}
.mc-kind-tabs button.active .mc-tab-badge {
  background: var(--brand);
  color: #fff;
}

.mc-view-switcher {
  display: inline-flex;
  padding: 2px;
  border-radius: var(--r-md);
  background: var(--surface-soft);
  border: 1px solid var(--line-soft);
}
.mc-view-switcher button {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 5px 10px;
  border-radius: var(--r-sm);
  font-size: 11.5px;
  font-weight: 500;
  border: none;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  transition: all 0.15s ease;
}
.mc-view-switcher button:hover {
  color: var(--text);
}
.mc-view-switcher button.active {
  background: var(--surface);
  color: var(--text);
  font-weight: 600;
  box-shadow: var(--shadow-xs);
}
.mc-view-switcher button :deep(svg) {
  width: 13px;
  height: 13px;
}

/* 筛选行 */
.mc-filters-row {
  display: grid;
  grid-template-columns: minmax(220px, 1.5fr) repeat(4, minmax(130px, 1fr));
  gap: 8px;
}

.mc-search-box {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 0 10px;
  height: 36px;
  border-radius: var(--r-md);
  border: 1px solid var(--line);
  background: var(--surface-soft);
  color: var(--muted);
}
.mc-search-box:focus-within {
  border-color: var(--brand);
  box-shadow: 0 0 0 3px var(--brand-glow);
  background: var(--surface);
}
.mc-search-box input {
  flex: 1;
  border: none;
  background: transparent;
  outline: none;
  font-size: 12px;
  color: var(--text);
}
.mc-search-box :deep(svg) {
  width: 14px;
  height: 14px;
}
.mc-clear-search {
  border: none;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  padding: 2px;
}
.mc-clear-search:hover { color: var(--text); }

.mc-filter-dropdown {
  height: 36px;
}

/* 特性芯片栏 */
.mc-feature-bar {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.mc-feature-chips {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}
.mc-chips-title {
  font-size: 11px;
  color: var(--muted);
}
.mc-chip {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 3px 9px;
  border-radius: var(--r-full);
  font-size: 11px;
  font-weight: 500;
  border: 1px solid var(--line);
  background: var(--surface-soft);
  color: var(--muted);
  cursor: pointer;
  transition: all 0.15s ease;
}
.mc-chip:hover {
  background: var(--surface-hover);
  color: var(--text);
}
.mc-chip.active {
  background: var(--brand-soft);
  border-color: var(--brand);
  color: var(--brand-deep);
  font-weight: 600;
}

.mc-results-meta {
  display: flex;
  align-items: center;
  gap: 10px;
}
.mc-filter-count {
  font-size: 11px;
  color: var(--muted);
}
.mc-filter-count b {
  color: var(--text);
}

.mc-active-provider-filter {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 2px 8px;
  border-radius: var(--r-sm);
  background: var(--brand-soft);
  border: 1px solid var(--brand);
  color: var(--brand-deep);
  font-size: 11px;
}
.mc-active-provider-filter button {
  border: none;
  background: transparent;
  cursor: pointer;
  color: var(--brand-deep);
  padding: 0;
  display: flex;
}
.mc-active-provider-filter :deep(svg) {
  width: 10px;
  height: 10px;
}

/* —— 3. 主内容区域 —— */
.mc-main-content {
  flex: 1;
  padding: 20px 24px 80px;
}

/* 卡片视图 */
.mc-cards-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
  gap: 16px;
}

.mc-card {
  border-radius: var(--r-xl);
  background: var(--surface);
  border: 1px solid var(--line);
  padding: 16px;
  display: flex;
  flex-direction: column;
  gap: 12px;
  cursor: pointer;
  transition: all 0.2s ease;
  position: relative;
}
.mc-card:hover {
  transform: translateY(-2px);
  border-color: var(--brand);
  box-shadow: var(--shadow-md);
}
.mc-card.is-selected {
  border-color: var(--brand);
  box-shadow: 0 0 0 2px var(--brand-glow);
}

.mc-card-head {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 10px;
}

.mc-card-identity {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
}

.mc-card-avatar {
  width: 36px;
  height: 36px;
  border-radius: var(--r-lg);
  display: flex;
  align-items: center;
  justify-content: center;
  font-weight: 700;
  font-size: 13px;
  flex-shrink: 0;
}

.mc-avatar-sm {
  width: 26px;
  height: 26px;
  border-radius: var(--r-sm);
  font-size: 10px;
}

.mc-card-title-box {
  display: flex;
  flex-direction: column;
  min-width: 0;
}

.mc-card-title-row {
  display: flex;
  align-items: center;
  gap: 6px;
}
.mc-card-title-row h3 {
  margin: 0;
  font-size: 14px;
  font-weight: 680;
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.mc-card-id {
  font-size: 10.5px;
  color: var(--muted);
  font-family: ui-monospace, monospace;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  margin-top: 2px;
}

.mc-card-compare-btn {
  width: 26px;
  height: 26px;
  border-radius: 50%;
  border: 1px solid var(--line);
  background: var(--surface-soft);
  color: var(--muted);
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  flex-shrink: 0;
  transition: all 0.15s ease;
}
.mc-card-compare-btn:hover {
  background: var(--brand-soft);
  color: var(--brand);
  border-color: var(--brand);
}
.mc-card-compare-btn.active {
  background: var(--brand);
  color: #fff;
  border-color: var(--brand);
}
.mc-card-compare-btn :deep(svg) {
  width: 12px;
  height: 12px;
}

.mc-card-badges {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.mc-card-tag {
  font-size: 10.5px;
  font-weight: 600;
  padding: 2px 7px;
  border-radius: var(--r-sm);
  display: inline-flex;
  align-items: center;
  gap: 4px;
}
.mc-card-tag :deep(svg) {
  width: 11px;
  height: 11px;
}

.mc-pill {
  font-size: 9.5px;
  font-weight: 700;
  padding: 1px 5px;
  border-radius: 4px;
  line-height: 1.2;
}
.mc-pill-open { background: var(--success-soft); color: var(--success); }
.mc-pill-beta { background: var(--warning-soft); color: var(--warning); }
.mc-pill-reasoning { background: var(--violet-soft); color: var(--violet); }

.mc-card-feat {
  font-size: 10px;
  padding: 2px 6px;
  border-radius: var(--r-sm);
  background: var(--surface-soft);
  color: var(--muted);
}
.mc-feat-reasoning {
  background: var(--violet-soft);
  color: var(--violet);
  font-weight: 600;
}
.mc-feat-tool {
  background: var(--brand-soft);
  color: var(--brand-deep);
}

.mc-card-specs {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 8px;
  padding: 8px 10px;
  border-radius: var(--r-md);
  background: var(--surface-soft);
}
.mc-spec-item {
  display: flex;
  flex-direction: column;
}
.mc-spec-k {
  font-size: 9.5px;
  color: var(--muted);
}
.mc-spec-v {
  font-size: 11.5px;
  font-weight: 600;
  color: var(--text);
  margin-top: 2px;
}
.mc-free-tag {
  font-size: 9.5px;
  color: var(--info);
  font-weight: normal;
}

.mc-card-pricing-box {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 10px;
  padding: 10px 12px;
  border-radius: var(--r-md);
  border: 1px solid var(--line-soft);
  background: var(--surface-soft);
}
.mc-price-col {
  display: flex;
  flex-direction: column;
}
.mc-price-label {
  font-size: 9.5px;
  color: var(--muted);
}
.mc-price-label-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
}
.mc-savings-badge {
  font-size: 9.5px;
  font-weight: 700;
  padding: 0 4px;
  border-radius: 3px;
  background: var(--success-soft);
  color: var(--success);
}
.mc-savings-badge-sm {
  font-size: 9px;
  font-weight: 700;
  margin-left: 3px;
  color: var(--success);
}

.mc-price-num {
  font-size: 12px;
  font-weight: 600;
  font-family: ui-monospace, monospace;
  margin-top: 3px;
  display: flex;
  align-items: baseline;
  gap: 3px;
}
.mc-price-num small {
  color: var(--muted);
  font-weight: normal;
}

.mc-card-footer {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-top: auto;
  padding-top: 4px;
}

.mc-card-aa {
  display: flex;
  align-items: center;
  gap: 6px;
}
.mc-aa-badge-score {
  display: inline-flex;
  align-items: center;
  gap: 3px;
  font-size: 10.5px;
  font-weight: 700;
  padding: 2px 6px;
  border-radius: var(--r-sm);
  background: var(--violet-soft);
  color: var(--violet);
}
.mc-aa-badge-score :deep(svg) {
  width: 10px;
  height: 10px;
}
.mc-aa-badge-speed {
  font-size: 10px;
  color: var(--muted);
}
.mc-card-aa-none {
  font-size: 10px;
  color: var(--faint);
}

.mc-card-detail-btn {
  display: inline-flex;
  align-items: center;
  gap: 2px;
  font-size: 11.5px;
  font-weight: 600;
  color: var(--brand);
  border: none;
  background: transparent;
  cursor: pointer;
  padding: 2px 0;
}
.mc-card-detail-btn:hover {
  text-decoration: underline;
}
.mc-card-detail-btn :deep(svg) {
  width: 12px;
  height: 12px;
}

/* 分页 */
.mc-pagination-bar {
  display: flex;
  justify-content: center;
  align-items: center;
  gap: 16px;
  padding: 24px 0 0;
}
.mc-page-btn {
  padding: 6px 14px;
  border-radius: var(--r-md);
  border: 1px solid var(--line);
  background: var(--surface);
  font-size: 12px;
  font-weight: 500;
  color: var(--text);
  cursor: pointer;
}
.mc-page-btn:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}
.mc-page-info {
  font-size: 12px;
  color: var(--muted);
}

/* 表格视图定制 */
.mc-table-model-cell {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
}
.mc-table-model-info {
  display: flex;
  flex-direction: column;
  min-width: 0;
}
.mc-table-model-title {
  display: flex;
  align-items: center;
  gap: 5px;
}
.mc-table-model-title strong {
  font-size: 12.5px;
  color: var(--text);
}
.mc-table-model-id {
  font-size: 10px;
  color: var(--muted);
  font-family: ui-monospace, monospace;
}
.mc-mono-cell {
  font-family: ui-monospace, monospace;
  font-size: 11.5px;
}
.mc-table-price-cell {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  font-size: 11.5px;
  font-family: ui-monospace, monospace;
}
.mc-table-price-cell small {
  font-size: 9.5px;
  color: var(--muted);
}
.mc-table-host-cell {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
}
.mc-table-aa-cell {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  font-size: 11.5px;
}

/* 供应商矩阵视图 */
.mc-providers-matrix-view {
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.mc-matrix-header h2 {
  margin: 0;
  font-size: 18px;
  font-weight: 700;
}
.mc-matrix-header p {
  margin: 4px 0 0;
  font-size: 12px;
  color: var(--muted);
}

.mc-providers-matrix-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
  gap: 14px;
}
.mc-provider-matrix-card {
  padding: 14px;
  border-radius: var(--r-xl);
  background: var(--surface);
  border: 1px solid var(--line);
  display: flex;
  flex-direction: column;
  gap: 10px;
  cursor: pointer;
  transition: all 0.15s ease;
}
.mc-provider-matrix-card:hover {
  border-color: var(--brand);
  transform: translateY(-2px);
  box-shadow: var(--shadow-sm);
}
.mc-provider-matrix-card.is-active-filter {
  border-color: var(--brand);
  background: var(--brand-soft);
}

.mc-pm-card-top {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
}
.mc-pm-identity {
  display: flex;
  align-items: center;
  gap: 10px;
}
.mc-pm-avatar {
  width: 32px;
  height: 32px;
  border-radius: var(--r-md);
  background: var(--surface-soft);
  display: flex;
  align-items: center;
  justify-content: center;
  font-weight: 700;
  font-size: 13px;
  color: var(--brand-deep);
}
.mc-pm-tags {
  display: flex;
  align-items: center;
  gap: 4px;
  margin-top: 2px;
}
.mc-tier-pill {
  font-size: 9px;
  font-weight: 700;
  padding: 1px 5px;
  border-radius: 3px;
}
.mc-tier-lab { background: var(--success-soft); color: var(--success); }
.mc-tier-gateway { background: var(--brand-soft); color: var(--brand-deep); }
.mc-tier-cloud { background: var(--info-soft); color: var(--info); }
.mc-sub-pill {
  font-size: 9px;
  padding: 1px 4px;
  border-radius: 3px;
  background: var(--warning-soft);
  color: var(--warning);
}
.mc-doc-link-btn {
  color: var(--muted);
  display: flex;
  padding: 4px;
}
.mc-doc-link-btn:hover { color: var(--brand); }
.mc-doc-link-btn :deep(svg) { width: 14px; height: 14px; }

.mc-pm-api code {
  font-size: 10.5px;
  font-family: ui-monospace, monospace;
  color: var(--muted);
  background: var(--surface-soft);
  padding: 2px 6px;
  border-radius: var(--r-xs);
  word-break: break-all;
}
.mc-pm-footer {
  display: flex;
  justify-content: space-between;
  align-items: center;
  font-size: 11.5px;
  color: var(--muted);
  margin-top: auto;
  border-top: 1px solid var(--line-soft);
  padding-top: 8px;
}
.mc-pm-action {
  font-weight: 600;
  color: var(--brand);
}

/* —— 4. 多模型对战 Arena 浮动 Dock —— */
.mc-arena-dock {
  position: fixed;
  bottom: 24px;
  left: 50%;
  transform: translateX(-50%);
  z-index: 100;
  width: min(840px, calc(100vw - 48px));
  border-radius: var(--r-xl);
  background: var(--surface);
  border: 1px solid var(--line-strong);
  box-shadow: var(--shadow-pop);
  padding: 10px 16px;
  animation: slide-up 0.25s var(--ease-spring);
}
@keyframes slide-up {
  from { transform: translate(-50%, 20px); opacity: 0; }
  to { transform: translate(-50%, 0); opacity: 1; }
}

.mc-arena-dock-content {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}
.mc-arena-dock-title {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
  color: var(--text);
  flex-shrink: 0;
}
.mc-arena-dock-title :deep(svg) {
  width: 16px;
  height: 16px;
  color: var(--brand);
}

.mc-arena-chips {
  display: flex;
  align-items: center;
  gap: 6px;
  overflow-x: auto;
  flex: 1;
}
.mc-arena-model-pill {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 4px 8px;
  border-radius: var(--r-full);
  background: var(--surface-soft);
  border: 1px solid var(--line);
  font-size: 11.5px;
  color: var(--text);
  white-space: nowrap;
}
.mc-arena-model-pill button {
  border: none;
  background: transparent;
  cursor: pointer;
  color: var(--muted);
  padding: 0;
  display: flex;
}
.mc-arena-model-pill button:hover { color: var(--danger); }
.mc-arena-model-pill :deep(svg) { width: 10px; height: 10px; }

.mc-arena-dock-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-shrink: 0;
}
.mc-arena-clear-btn {
  border: none;
  background: transparent;
  color: var(--muted);
  font-size: 12px;
  cursor: pointer;
  padding: 4px 8px;
}
.mc-arena-clear-btn:hover { color: var(--text); }
.mc-arena-launch-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 8px 14px;
  border-radius: var(--r-md);
  font-size: 12px;
  font-weight: 600;
}
.mc-arena-launch-btn :deep(svg) { width: 12px; height: 12px; }

/* —— 5. 全景模型详情深度抽屉 —— */
.mc-drawer-backdrop {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.45);
  backdrop-filter: blur(4px);
  z-index: 1000;
  display: flex;
  justify-content: flex-end;
}

.mc-drawer-panel {
  width: min(720px, 90vw);
  height: 100%;
  background: var(--surface);
  border-left: 1px solid var(--line);
  box-shadow: var(--shadow-pop);
  display: flex;
  flex-direction: column;
  animation: drawer-slide-in 0.25s var(--ease);
}
@keyframes drawer-slide-in {
  from { transform: translateX(100%); }
  to { transform: translateX(0); }
}

.mc-drawer-header {
  padding: 20px 24px;
  border-bottom: 1px solid var(--line);
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 16px;
}

.mc-drawer-head-identity {
  display: flex;
  align-items: center;
  gap: 14px;
  min-width: 0;
}
.mc-drawer-avatar {
  width: 48px;
  height: 48px;
  border-radius: var(--r-xl);
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 18px;
  font-weight: 700;
  flex-shrink: 0;
}
.mc-drawer-title-box {
  display: flex;
  flex-direction: column;
  min-width: 0;
}
.mc-drawer-tags {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-bottom: 4px;
}
.mc-drawer-title-box h2 {
  margin: 0;
  font-size: 18px;
  font-weight: 700;
  letter-spacing: -0.02em;
}
.mc-drawer-id {
  margin: 2px 0 0;
  font-size: 11.5px;
  color: var(--muted);
  font-family: ui-monospace, monospace;
}

.mc-drawer-head-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}
.mc-compare-toggle-btn {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 6px 10px;
  border-radius: var(--r-md);
  font-size: 11.5px;
  font-weight: 600;
  border: 1px solid var(--line);
  background: var(--surface-soft);
  color: var(--text);
  cursor: pointer;
}
.mc-compare-toggle-btn.active {
  background: var(--brand-soft);
  color: var(--brand-deep);
  border-color: var(--brand);
}
.mc-compare-toggle-btn :deep(svg) { width: 12px; height: 12px; }

.mc-drawer-close-btn {
  width: 32px;
  height: 32px;
  border-radius: 50%;
  border: 1px solid var(--line);
  background: var(--surface-soft);
  color: var(--muted);
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
}
.mc-drawer-close-btn:hover { color: var(--text); background: var(--surface-hover); }
.mc-drawer-close-btn :deep(svg) { width: 16px; height: 16px; }

/* 抽屉导航 Tab */
.mc-drawer-tabs {
  padding: 0 24px;
  border-bottom: 1px solid var(--line);
  display: flex;
  gap: 16px;
  overflow-x: auto;
  background: var(--surface-soft);
}
.mc-drawer-tabs button {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 12px 2px;
  border: none;
  background: transparent;
  color: var(--muted);
  font-size: 12.5px;
  font-weight: 500;
  cursor: pointer;
  border-bottom: 2px solid transparent;
  transition: all 0.15s ease;
  white-space: nowrap;
}
.mc-drawer-tabs button:hover { color: var(--text); }
.mc-drawer-tabs button.active {
  color: var(--brand);
  font-weight: 600;
  border-bottom-color: var(--brand);
}
.mc-drawer-tabs button :deep(svg) { width: 14px; height: 14px; }

.mc-drawer-body {
  flex: 1;
  padding: 24px;
  overflow-y: auto;
}

.mc-tab-panel {
  display: flex;
  flex-direction: column;
  gap: 20px;
}

/* Specs 6-Grid */
.mc-specs-grid-6 {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 10px;
}
.mc-spec-box {
  padding: 12px;
  border-radius: var(--r-lg);
  background: var(--surface-soft);
  border: 1px solid var(--line-soft);
  display: flex;
  flex-direction: column;
}
.mc-spec-box span { font-size: 11px; color: var(--muted); }
.mc-spec-box strong { font-size: 13px; font-weight: 600; color: var(--text); margin-top: 4px; }

.mc-subheading {
  margin: 0;
  font-size: 13px;
  font-weight: 700;
  color: var(--text);
}

.mc-capability-matrix {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 10px;
}
.mc-cap-card {
  padding: 12px;
  border-radius: var(--r-lg);
  background: var(--surface-soft);
  border: 1px solid var(--line-soft);
  display: flex;
  gap: 10px;
  opacity: 0.55;
}
.mc-cap-card.enabled {
  opacity: 1;
  border-color: var(--line);
  background: var(--surface);
  box-shadow: var(--shadow-xs);
}
.mc-cap-emoji { font-size: 18px; line-height: 1; }
.mc-cap-card strong { font-size: 12.5px; color: var(--text); }
.mc-cap-card p { margin: 2px 0 0; font-size: 11px; color: var(--muted); }

.mc-modalities-box {
  display: flex;
  align-items: center;
  gap: 8px;
}
.mc-modalities-title { font-size: 12px; color: var(--muted); }
.mc-mod-tags { display: flex; gap: 6px; }
.mc-mod-badge {
  font-size: 11px;
  font-weight: 700;
  padding: 2px 8px;
  border-radius: var(--r-sm);
  background: var(--brand-soft);
  color: var(--brand-deep);
}

/* 3-Cards Pricing */
.mc-pricing-cards-3 {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 12px;
}
.mc-pricing-card {
  padding: 14px;
  border-radius: var(--r-xl);
  background: var(--surface-soft);
  border: 1px solid var(--line-soft);
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.mc-pcard-lowest {
  border-color: var(--success);
  background: var(--success-soft);
}
.mc-pcard-spread {
  border-color: var(--violet);
  background: var(--violet-soft);
}
.mc-pcard-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}
.mc-pcard-tag {
  font-size: 11px;
  font-weight: 700;
  padding: 2px 6px;
  border-radius: var(--r-sm);
}
.mc-pcard-rows {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.mc-pcard-rows > div {
  display: flex;
  justify-content: space-between;
  font-size: 11.5px;
}
.mc-pcard-rows span { color: var(--muted); }
.mc-pcard-rows strong { font-family: ui-monospace, monospace; }

/* 算费器 */
.mc-cost-calculator {
  padding: 16px;
  border-radius: var(--r-xl);
  background: var(--surface-soft);
  border: 1px solid var(--line);
  display: flex;
  flex-direction: column;
  gap: 14px;
}
.mc-calc-head {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
}
.mc-calc-head h4 {
  margin: 0;
  font-size: 13.5px;
  font-weight: 700;
  display: flex;
  align-items: center;
  gap: 6px;
}
.mc-calc-head p { margin: 3px 0 0; font-size: 11px; color: var(--muted); }
.mc-calc-icon :deep(svg) { width: 14px; height: 14px; color: var(--brand); }

.mc-currency-toggle {
  display: inline-flex;
  padding: 2px;
  border-radius: var(--r-md);
  background: var(--surface);
  border: 1px solid var(--line);
}
.mc-currency-toggle button {
  padding: 3px 8px;
  border-radius: var(--r-sm);
  font-size: 10.5px;
  font-weight: 600;
  border: none;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
}
.mc-currency-toggle button.active {
  background: var(--brand);
  color: #fff;
}

.mc-calc-sliders {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 16px;
}
.mc-slider-group {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.mc-slider-label {
  display: flex;
  justify-content: space-between;
  font-size: 11.5px;
}
.mc-slider-label span { color: var(--muted); }
.mc-slider-label b { color: var(--brand-deep); font-weight: 700; }
.mc-slider-group input[type="range"] {
  accent-color: var(--brand);
}

.mc-calc-results-bar {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 10px;
  padding: 12px;
  border-radius: var(--r-lg);
  background: var(--surface);
  border: 1px solid var(--line-soft);
}
.mc-calc-res-item {
  display: flex;
  flex-direction: column;
}
.mc-calc-res-item span { font-size: 10.5px; color: var(--muted); }
.mc-calc-res-item strong { font-size: 16px; font-weight: 700; color: var(--text); margin-top: 2px; }
.mc-calc-res-saved strong { color: var(--success); }

/* AA Hero */
.mc-aa-hero-card {
  padding: 18px;
  border-radius: var(--r-xl);
  background: var(--surface-soft);
  border: 1px solid var(--line);
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.mc-aa-hero-title {
  display: flex;
  align-items: center;
  gap: 10px;
}
.mc-aa-hero-title :deep(svg) {
  width: 22px;
  height: 22px;
  color: var(--violet);
}
.mc-aa-hero-title h4 { margin: 0; font-size: 14px; font-weight: 700; }
.mc-aa-hero-title p { margin: 2px 0 0; font-size: 11.5px; color: var(--muted); }

.mc-aa-gauges-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px;
}
.mc-gauge-card {
  padding: 12px;
  border-radius: var(--r-lg);
  background: var(--surface);
  border: 1px solid var(--line-soft);
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.mc-gauge-k { font-size: 11px; color: var(--muted); }
.mc-gauge-bar-wrap {
  height: 6px;
  border-radius: 999px;
  background: var(--line-soft);
  overflow: hidden;
}
.mc-gauge-bar {
  height: 100%;
  border-radius: 999px;
  background: var(--success);
}
.mc-bar-brand { background: var(--brand); }
.mc-bar-violet { background: var(--violet); }
.mc-gauge-v { font-size: 16px; font-weight: 700; color: var(--text); }
.mc-gauge-v small { font-size: 11px; font-weight: normal; color: var(--muted); }

/* Providers Tab in Drawer */
.mc-providers-panel-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
  gap: 12px;
}
.mc-prov-card {
  padding: 12px;
  border-radius: var(--r-lg);
  background: var(--surface-soft);
  border: 1px solid var(--line-soft);
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.mc-prov-card-top {
  display: flex;
  justify-content: space-between;
  align-items: center;
}
.mc-prov-info {
  display: flex;
  align-items: center;
  gap: 6px;
}
.mc-prov-info strong { font-size: 13px; }
.mc-prov-api-row code {
  font-size: 10.5px;
  font-family: ui-monospace, monospace;
  color: var(--muted);
  background: var(--surface);
  padding: 2px 6px;
  border-radius: var(--r-xs);
  word-break: break-all;
}

/* Code Snippet & Raw */
.mc-code-snippet-box, .mc-raw-box {
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.mc-snippet-toolbar, .mc-raw-top {
  display: flex;
  justify-content: space-between;
  align-items: center;
}
.mc-code-lang-selector {
  display: flex;
  gap: 4px;
}
.mc-code-lang-selector button {
  padding: 5px 10px;
  border-radius: var(--r-md);
  font-size: 11.5px;
  font-weight: 600;
  border: 1px solid var(--line);
  background: var(--surface-soft);
  color: var(--muted);
  cursor: pointer;
}
.mc-code-lang-selector button.active {
  background: var(--brand-soft);
  border-color: var(--brand);
  color: var(--brand-deep);
}

.mc-copy-btn {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 5px 10px;
  border-radius: var(--r-md);
  font-size: 11.5px;
  font-weight: 600;
  border: 1px solid var(--line);
  background: var(--surface-soft);
  color: var(--text);
  cursor: pointer;
}
.mc-copy-btn:hover { background: var(--surface-hover); }
.mc-copy-btn :deep(svg) { width: 12px; height: 12px; }

.mc-code-block {
  margin: 0;
  padding: 16px;
  border-radius: var(--r-xl);
  background: #0f1117;
  color: #e2e8f0;
  font-family: ui-monospace, "SF Mono", Menlo, monospace;
  font-size: 12px;
  line-height: 1.5;
  overflow-x: auto;
  border: 1px solid rgba(255, 255, 255, 0.08);
}

/* —— 6. Arena 对战全屏弹窗 —— */
.mc-arena-modal-backdrop {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.6);
  backdrop-filter: blur(6px);
  z-index: 1100;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 32px;
}
.mc-arena-modal {
  width: min(1000px, 95vw);
  max-height: 90vh;
  border-radius: var(--r-2xl);
  background: var(--surface);
  border: 1px solid var(--line);
  box-shadow: var(--shadow-pop);
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
.mc-arena-modal-head {
  padding: 20px 24px;
  border-bottom: 1px solid var(--line);
  display: flex;
  justify-content: space-between;
  align-items: center;
}
.mc-arena-modal-title {
  display: flex;
  align-items: center;
  gap: 12px;
}
.mc-arena-flame {
  width: 38px;
  height: 38px;
  border-radius: var(--r-lg);
  background: var(--brand-soft);
  color: var(--brand);
  display: flex;
  align-items: center;
  justify-content: center;
}
.mc-arena-flame :deep(svg) { width: 20px; height: 20px; }
.mc-arena-modal-title h2 { margin: 0; font-size: 17px; font-weight: 700; }
.mc-arena-modal-title p { margin: 2px 0 0; font-size: 11.5px; color: var(--muted); }

.mc-arena-table-wrap {
  flex: 1;
  padding: 20px 24px;
  overflow: auto;
}
.mc-arena-table {
  width: 100%;
  border-collapse: collapse;
  text-align: left;
}
.mc-arena-table th, .mc-arena-table td {
  padding: 12px 14px;
  border-bottom: 1px solid var(--line-soft);
  font-size: 12.5px;
}
.mc-arena-feature-col {
  width: 160px;
  color: var(--muted);
  font-weight: 600;
}
.mc-arena-th-cell {
  display: flex;
  align-items: center;
  gap: 10px;
}
.mc-arena-th-cell strong { font-size: 13.5px; color: var(--text); }
.mc-arena-th-cell small { display: block; font-size: 10.5px; color: var(--muted); font-weight: normal; }

/* 缺省与加载状态 */
.mc-loading-state, .mc-empty-state {
  min-height: 280px;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 12px;
  color: var(--muted);
}
.mc-loading-state :deep(svg), .mc-empty-state :deep(svg) {
  width: 32px;
  height: 32px;
  color: var(--faint);
}
.mc-empty-state h3 { margin: 0; font-size: 16px; color: var(--text); }
.mc-empty-state p { margin: 0; font-size: 12px; color: var(--muted); }

.mc-detail-error {
  padding: 16px;
  border-radius: var(--r-lg);
  background: var(--danger-soft);
  color: var(--danger);
  font-size: 12.5px;
}

/* 常用文本色彩辅助类 */
.text-success { color: var(--success) !important; }
.text-violet { color: var(--violet) !important; }
.font-semibold { font-weight: 600; }
.font-bold { font-weight: 700; }
.muted { color: var(--muted); }
</style>
