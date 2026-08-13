<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import type { ModelCatalogDetail, ModelCatalogItem } from "../types";
import AppTable, { type AppTableColumn } from "./AppTable.vue";

const store = useStore();
const query = ref("");
const manufacturer = ref("all");
const pricing = ref("all");
const activeTab = ref("chat");
const selectedKey = ref("");
const detail = ref<ModelCatalogDetail | null>(null);
const detailLoading = ref(false);
const detailError = ref("");
const sourceTab = ref("merged");
const showRaw = ref(false);
const currentPage = ref(1);
const pageSize = ref(50);
const sorting = ref<Array<{ id: string; desc: boolean }>>([]);

// —— 类型标签：不同模型类型侧重不同，列表列也随之变化 ——
const typeTabs = [
  { key: "chat", label: "对话", icon: "chat" },
  { key: "embedding", label: "向量", icon: "database" },
  { key: "image_generation", label: "图像", icon: "eye" },
  { key: "audio_speech", label: "语音", icon: "activity" },
  { key: "completion", label: "补全", icon: "rows" },
  { key: "rerank", label: "重排", icon: "pulse" },
  { key: "other", label: "其他", icon: "more" },
] as const;

const typeOrder: Record<string, number> = {
  chat: 0,
  completion: 1,
  embedding: 2,
  image_generation: 3,
  audio_speech: 4,
  audio_transcription: 5,
  rerank: 6,
  moderation: 7,
  search: 8,
};

const typeCounts = computed(() => {
  const counts: Record<string, number> = {};
  for (const model of store.modelCatalog.value.models) {
    const tab = tabKeyForMode(model.mode);
    counts[tab] = (counts[tab] ?? 0) + 1;
  }
  return counts;
});

function tabKeyForMode(mode: string): string {
  if (mode === "chat" || mode === "completion" || mode === "embedding") return mode;
  if (mode === "image_generation" || mode === "audio_speech" || mode === "audio_transcription" || mode === "rerank") return mode;
  return "other";
}

function tabMatches(model: ModelCatalogItem, tab: string): boolean {
  if (tab === "other") return !["chat", "completion", "embedding", "image_generation", "audio_speech", "audio_transcription", "rerank"].includes(model.mode);
  return model.mode === tab;
}

const manufacturers = computed(() => [
  "all",
  ...new Set(store.modelCatalog.value.models.map((model) => model.manufacturer).filter(Boolean)),
].sort((a, b) => a === "all" ? -1 : b === "all" ? 1 : a.localeCompare(b)));

const filteredModels = computed(() => {
  const term = query.value.trim().toLocaleLowerCase("zh-CN");
  return store.modelCatalog.value.models
    .filter((model) => tabMatches(model, activeTab.value))
    .filter((model) => {
      if (manufacturer.value !== "all" && model.manufacturer !== manufacturer.value) return false;
      const hasPrice = model.inputCostPerToken > 0 || model.outputCostPerToken > 0 || model.imageCost > 0 || model.requestCost > 0 || model.audioInputCostPerToken > 0 || model.audioOutputCostPerToken > 0;
      if (pricing.value === "paid" && !hasPrice) return false;
      if (pricing.value === "free" && hasPrice) return false;
      if (!term) return true;
      return [
        model.canonicalKey,
        model.displayName,
        model.manufacturer,
        model.mode,
        ...model.capabilities,
      ].join(" ").toLocaleLowerCase("zh-CN").includes(term);
    })
    .sort((a, b) => {
      const ma = typeOrder[a.mode] ?? 99;
      const mb = typeOrder[b.mode] ?? 99;
      if (ma !== mb) return ma - mb;
      return a.displayName.localeCompare(b.displayName, "zh-CN");
    });
});

// —— 每种类型侧重不同的列 ——
const tableColumns = computed<AppTableColumn[]>(() => {
  const common = [
    { key: "displayName", title: "模型", width: "minmax(180px,1.2fr)", sortable: true },
    { key: "manufacturer", title: "厂商", width: "minmax(100px,.55fr)", sortable: true },
    { key: "contextLength", title: "上下文", width: "88px", align: "right" as const, sortable: true },
  ];
  const priceIn = { key: "inputCostPerToken", title: "输入 / 1M", width: "100px", align: "right" as const, sortable: true };
  const priceOut = { key: "outputCostPerToken", title: "输出 / 1M", width: "100px", align: "right" as const, sortable: true };

  if (activeTab.value === "chat") {
    return [...common, { key: "maxOutputTokens", title: "最大输出", width: "88px", align: "right" as const, sortable: true }, priceIn, priceOut];
  }
  if (activeTab.value === "completion") {
    return [...common, { key: "maxOutputTokens", title: "最大输出", width: "88px", align: "right" as const, sortable: true }, priceIn, priceOut];
  }
  if (activeTab.value === "embedding") {
    return [...common, { key: "maxInputTokens", title: "最大输入", width: "88px", align: "right" as const, sortable: true }, priceIn];
  }
  if (activeTab.value === "image_generation") {
    return [...common, { key: "imageCost", title: "图像 / 张", width: "92px", align: "right" as const, sortable: true }, { key: "requestCost", title: "请求费", width: "82px", align: "right" as const, sortable: true }];
  }
  if (activeTab.value === "audio_speech") {
    return [...common, { key: "audioInputCostPerToken", title: "音频输入 / 1M", width: "120px", align: "right" as const, sortable: true }, { key: "audioOutputCostPerToken", title: "音频输出 / 1M", width: "120px", align: "right" as const, sortable: true }];
  }
  if (activeTab.value === "rerank") {
    return [...common, priceIn];
  }
  // other
  return [...common, priceIn, priceOut];
});

const selectedModel = computed(() =>
  store.modelCatalog.value.models.find((model) => model.canonicalKey === selectedKey.value) ?? null,
);
const selectedEntry = computed(() => {
  if (!detail.value || sourceTab.value === "merged") return null;
  return detail.value.entries.find((entry) => `${entry.source}:${entry.sourceModelId}` === sourceTab.value) ?? null;
});

// —— 详情随类型变化：基础字段与价格字段与列表列侧重一致 ——
const detailFacts = computed<Array<{ label: string; value: string }>>(() => {
  const m = selectedModel.value;
  if (!m) return [];
  const facts: Array<{ label: string; value: string }> = [
    { label: "上下文", value: tokenText(m.contextLength) },
    { label: "最大输入", value: tokenText(m.maxInputTokens) },
  ];
  const noOutput = ["embedding", "rerank", "audio_transcription", "moderation", "search"].includes(m.mode);
  if (!noOutput) facts.push({ label: "最大输出", value: tokenText(m.maxOutputTokens) });
  facts.push({ label: "能力数", value: String(m.capabilities.length) });
  return facts;
});

const detailPrices = computed<Array<{ label: string; value: string; dim: boolean }>>(() => {
  const m = selectedModel.value;
  if (!m) return [];
  const out: Array<{ label: string; value: string; dim: boolean }> = [];
  const add = (label: string, value: number, kind: "perMillion" | "unit") => {
    if (!value) { out.push({ label, value: "—", dim: true }); return; }
    out.push({ label, value: kind === "perMillion" ? moneyPerMillion(value) : `$${value}`, dim: false });
  };
  const mode = m.mode;
  if (mode === "chat" || mode === "completion") {
    add("输入 Token", m.inputCostPerToken, "perMillion");
    add("输出 Token", m.outputCostPerToken, "perMillion");
    add("缓存读取", m.cacheReadCostPerToken, "perMillion");
    add("缓存写入", m.cacheWriteCostPerToken, "perMillion");
  } else if (mode === "embedding") {
    add("输入 Token", m.inputCostPerToken, "perMillion");
    add("缓存写入", m.cacheWriteCostPerToken, "perMillion");
  } else if (mode === "image_generation") {
    add("图像 / 张", m.imageCost, "unit");
    add("单次请求", m.requestCost, "unit");
  } else if (mode === "audio_speech") {
    add("音频输入", m.audioInputCostPerToken, "perMillion");
    add("音频输出", m.audioOutputCostPerToken, "perMillion");
  } else if (mode === "audio_transcription") {
    add("音频输入", m.audioInputCostPerToken, "perMillion");
  } else if (mode === "rerank" || mode === "moderation" || mode === "search") {
    add("输入 Token", m.inputCostPerToken, "perMillion");
  } else {
    add("输入 Token", m.inputCostPerToken, "perMillion");
    add("输出 Token", m.outputCostPerToken, "perMillion");
  }
  return out;
});

const detailPriceNote = computed(() => {
  const mode = selectedModel.value?.mode ?? "";
  if (mode === "image_generation") return "按生成图片数量计价；未标价字段默认缺失。";
  if (mode === "audio_speech" || mode === "audio_transcription") return "音频模型按音频 Token 计价，单位 /1M tokens。";
  if (mode === "embedding" || mode === "rerank") return "此类模型通常只有输入（或写入）计费，无输出计费。";
  return "优先采用主目录价格；缺失字段由补偿数据填充，单位 /1M tokens。";
});

const modeLabels: Record<string, string> = {
  chat: "对话",
  completion: "补全",
  embedding: "向量",
  image_generation: "图像",
  audio_speech: "语音生成",
  audio_transcription: "语音转写",
  rerank: "重排",
  moderation: "审核",
  search: "搜索",
};
function modeLabel(value: string) { return modeLabels[value] || value || "未知"; }
const modeIcons: Record<string, keyof typeof icons> = {
  chat: "chat",
  completion: "rows",
  embedding: "database",
  image_generation: "eye",
  audio_speech: "activity",
  audio_transcription: "activity",
  rerank: "pulse",
  moderation: "eyeOff",
  search: "search",
};
function modeIcon(value: string): keyof typeof icons { return modeIcons[value] ?? "cpu"; }
function manufacturerLabel(value: string) {
  const labels: Record<string, string> = {
    openai: "OpenAI", anthropic: "Anthropic", google: "Google", deepseek: "DeepSeek",
    qwen: "Qwen", "x-ai": "xAI", mistralai: "Mistral", "meta-llama": "Meta Llama",
    cohere: "Cohere", perplexity: "Perplexity", "z-ai": "Z.ai", unknown: "未知厂商",
  };
  return labels[value] || value || "未知厂商";
}
function modeTone(value: string) {
  const tones: Record<string, string> = {
    chat: "brand",
    completion: "violet",
    embedding: "info",
    image_generation: "success",
    audio_speech: "violet",
    audio_transcription: "info",
    rerank: "brand",
    moderation: "danger",
    search: "success",
  };
  return tones[value] ?? "neutral";
}
function vendorTone(value: string) {
  const tones: Record<string, string> = {
    openai: "success", anthropic: "brand", google: "info", deepseek: "violet",
    qwen: "violet", "x-ai": "neutral", mistralai: "brand", "meta-llama": "info",
    cohere: "success", perplexity: "neutral", "z-ai": "violet",
  };
  return tones[value] ?? "neutral";
}
function vendorInitials(value: string) {
  const label = manufacturerLabel(value);
  if (!label || label === "未知厂商") return "?";
  const clean = label.replace(/[^\p{L}\p{N} ]/gu, "").trim();
  if (!clean) return "?";
  const words = clean.split(/\s+/).filter(Boolean);
  if (words.length >= 2) return (words[0][0] + words[1][0]).toUpperCase();
  return clean.slice(0, 2).toUpperCase();
}
function tokenText(value: number) {
  if (!value) return "—";
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(value % 1_000_000 ? 1 : 0)}M`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(value % 1_000 ? 1 : 0)}K`;
  return String(value);
}
function moneyPerMillion(value: number) {
  if (!value) return "—";
  return `$${(value * 1_000_000).toLocaleString("en-US", { maximumFractionDigits: 4 })}`;
}
function shortMoney(value: number) {
  if (!value) return "—";
  return `$${(value * 1_000_000).toLocaleString("en-US", { maximumFractionDigits: 3 })}`;
}
function moneyPerUnit(value: number, unit = "") {
  if (!value) return "—";
  return `$${value.toLocaleString("en-US", { maximumFractionDigits: 4 })}${unit ? ` ${unit}` : ""}`;
}
function dateText(value: string) {
  if (!value) return "尚未同步";
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString("zh-CN", { hour12: false });
}
async function selectModel(model: ModelCatalogItem) {
  selectedKey.value = model.canonicalKey;
  sourceTab.value = "merged";
  showRaw.value = false;
  detail.value = null;
  detailError.value = "";
  detailLoading.value = true;
  try { detail.value = await store.getModelCatalogDetail(model.canonicalKey); }
  catch (error) { detailError.value = String(error); }
  finally { detailLoading.value = false; }
}

function closeDetail() {
  selectedKey.value = "";
  detail.value = null;
  detailLoading.value = false;
  detailError.value = "";
  showRaw.value = false;
}

async function manualSync() {
  const result = await store.syncModelCatalog(true);
  if (!result || !selectedKey.value) return;
  const selected = store.modelCatalog.value.models.find((model) => model.canonicalKey === selectedKey.value);
  if (selected) await selectModel(selected);
  else closeDetail();
}

watch([query, manufacturer, pricing, pageSize], () => { currentPage.value = 1; });
watch(activeTab, () => {
  currentPage.value = 1;
  sorting.value = [];
});
watch(() => store.modelCatalog.value.lastSyncedAt, () => {
  const selected = store.modelCatalog.value.models.find((model) => model.canonicalKey === selectedKey.value);
  if (selected && selectedKey.value && detail.value) void selectModel(selected);
});
watch(filteredModels, () => {
  if (selectedKey.value && !filteredModels.value.some((model) => model.canonicalKey === selectedKey.value)) closeDetail();
});

onMounted(() => {
  if (!store.modelCatalog.value.models.length && !store.modelCatalogLoading.value) void store.loadModelCatalog();
});
</script>

<template>
  <section class="model-catalog-page" aria-labelledby="modelparams-title">
    <header class="model-catalog-header">
      <div class="model-catalog-heading">
        <div><span class="model-catalog-eyebrow">OpenRouter · 模型目录</span><h1 id="modelparams-title">模型参数</h1><p>按类型切换标签，每类列表侧重不同；点击行查看完整详情</p></div>
      </div>
      <div class="model-catalog-header-actions">
        <div class="model-catalog-sync-meta"><span :class="{ active: store.modelCatalog.value.syncedToday }" /><div><strong>{{ store.modelCatalog.value.syncedToday ? "今天已同步" : "等待同步" }}</strong><small>{{ dateText(store.modelCatalog.value.lastSyncedAt) }}</small></div></div>
        <button class="primary-button model-catalog-sync" type="button" :disabled="store.modelCatalogSyncing.value" @click="manualSync"><span :class="{ 'is-spinning': store.modelCatalogSyncing.value }" v-html="icons.restore" /><span>{{ store.modelCatalogSyncing.value ? "同步中…" : "立即同步" }}</span></button>
      </div>
    </header>

    <div class="model-catalog-toolbar">
      <div class="model-catalog-type-tabs" role="tablist" aria-label="模型类型">
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
          <b>{{ typeCounts[tab.key] ?? 0 }}</b>
        </button>
      </div>
      <div class="model-catalog-filters">
        <label class="search-box model-catalog-search"><span v-html="icons.search" /><input v-model="query" type="search" placeholder="搜索模型名称或 ID…" /></label>
        <select v-model="manufacturer" aria-label="模型厂商筛选"><option value="all">全部厂商</option><option v-for="item in manufacturers.filter(item => item !== 'all')" :key="item" :value="item">{{ manufacturerLabel(item) }}</option></select>
        <select v-model="pricing" aria-label="价格筛选"><option value="all">全部价格</option><option value="paid">有价格</option><option value="free">无价格</option></select>
        <div class="model-catalog-toolbar-stats">
          <b>{{ filteredModels.length.toLocaleString() }}</b><span>个模型</span>
          <i></i>
          <b>{{ Math.max(0, manufacturers.length - 1).toLocaleString() }}</b><span>个厂商</span>
        </div>
      </div>
      <p v-if="store.modelCatalogError.value" class="model-catalog-error">{{ store.modelCatalogError.value }}</p>
    </div>

    <div class="model-catalog-layout">
      <section class="model-catalog-list-panel">
        <AppTable
          :rows="filteredModels"
          :columns="tableColumns"
          :row-key="(model: ModelCatalogItem) => model.canonicalKey"
          :loading="store.modelCatalogLoading.value"
          empty-text="没有匹配的模型"
          :page="currentPage"
          :page-size="pageSize"
          :sorting="sorting"
          :selected-key="selectedKey"
          clickable
          @update:page="currentPage = $event"
          @update:page-size="pageSize = $event"
          @update:sorting="sorting = $event"
          @select="selectModel"
        >
          <template #cell-displayName="{ row }">
            <div class="mc-model-cell">
              <span class="mc-avatar" :class="`mc-tone-${vendorTone(row.manufacturer)}`">{{ vendorInitials(row.manufacturer) }}</span>
              <div class="mc-model-cell-text">
                <strong>{{ row.displayName || row.canonicalKey }}</strong>
                <small>{{ row.canonicalKey }}</small>
              </div>
            </div>
          </template>
          <template #cell-manufacturer="{ row }">{{ manufacturerLabel(row.manufacturer) }}</template>
          <template #cell-contextLength="{ row }">{{ tokenText(row.contextLength) }}</template>
          <template #cell-maxOutputTokens="{ row }">{{ tokenText(row.maxOutputTokens) }}</template>
          <template #cell-inputCostPerToken="{ row }">{{ shortMoney(row.inputCostPerToken) }}</template>
          <template #cell-outputCostPerToken="{ row }">{{ shortMoney(row.outputCostPerToken) }}</template>
          <template #cell-maxInputTokens="{ row }">{{ tokenText(row.maxInputTokens) }}</template>
          <template #cell-imageCost="{ row }">{{ moneyPerUnit(row.imageCost, "张") }}</template>
          <template #cell-requestCost="{ row }">{{ moneyPerUnit(row.requestCost, "次") }}</template>
          <template #cell-audioInputCostPerToken="{ row }">{{ shortMoney(row.audioInputCostPerToken) }}</template>
          <template #cell-audioOutputCostPerToken="{ row }">{{ shortMoney(row.audioOutputCostPerToken) }}</template>
        </AppTable>
      </section>
    </div>

    <!-- 详情弹窗：点击行打开，不再常驻挤占列表空间 -->
    <Teleport to="body">
      <div
        class="mc-detail-backdrop"
        :hidden="!selectedModel"
        @click="closeDetail"
      >
        <section
          class="mc-detail-dialog"
          role="dialog"
          aria-modal="true"
          :aria-label="selectedModel?.displayName || selectedModel?.canonicalKey || '模型详情'"
          @click.stop
        >
          <template v-if="selectedModel">
            <header class="dialog-header mc-detail-header">
              <div class="header-left">
                <span class="mc-avatar mc-avatar--lg" :class="`mc-tone-${vendorTone(selectedModel.manufacturer)}`">{{ vendorInitials(selectedModel.manufacturer) }}</span>
                <div class="mc-detail-heading">
                  <span class="mc-vendor-label" :class="`mc-tone-${vendorTone(selectedModel.manufacturer)}`">{{ manufacturerLabel(selectedModel.manufacturer) }}</span>
                  <h2>{{ selectedModel.displayName || selectedModel.canonicalKey }}</h2>
                  <p class="site-url"><code>{{ selectedModel.canonicalKey }}</code></p>
                </div>
              </div>
              <button type="button" class="close-button" aria-label="关闭详情" @click="closeDetail" v-html="icons.close" />
            </header>

            <div class="dialog-body mc-detail-body">
              <div v-if="detailLoading" class="model-catalog-empty">正在读取详情…</div>
              <p v-else-if="detailError" class="model-catalog-detail-error">{{ detailError }}</p>
              <template v-else-if="detail">
                <div class="mc-detail-meta">
                  <span class="mc-badge" :class="`mc-tone-${modeTone(selectedModel.mode)}`"><span class="mc-badge-icon" v-html="icons[modeIcon(selectedModel.mode)]" />{{ modeLabel(selectedModel.mode) }}</span>
                  <span v-if="detail.entries.length > 1" class="mc-badge mc-tone-neutral">{{ detail.entries.length }} 条来源</span>
                </div>

                <section class="model-catalog-section">
                  <h3><span class="mc-section-icon" v-html="icons.settings" />基础参数</h3>
                  <div class="model-catalog-facts mc-facts">
                    <article v-for="f in detailFacts" :key="f.label">
                      <span>{{ f.label }}</span>
                      <strong>{{ f.value }}</strong>
                    </article>
                  </div>
                </section>

                <section class="model-catalog-section">
                  <h3><span class="mc-section-icon" v-html="icons.card" />参考价格</h3>
                  <div class="model-catalog-price-grid mc-prices">
                    <article v-for="p in detailPrices" :key="p.label" :class="{ dim: p.dim }">
                      <span>{{ p.label }}</span>
                      <strong>{{ p.value }}</strong>
                    </article>
                  </div>
                  <p class="model-catalog-section-note">{{ detailPriceNote }}</p>
                </section>

                <section class="model-catalog-section">
                  <h3><span class="mc-section-icon" v-html="icons.sparkles" />能力标签</h3>
                  <div class="model-catalog-capabilities">
                    <span v-for="item in selectedModel.capabilities" :key="item">{{ item }}</span>
                    <span v-if="!selectedModel.capabilities.length" class="muted">—</span>
                  </div>
                </section>

                <section class="model-catalog-section raw-section">
                  <button type="button" class="model-catalog-raw-toggle" :class="{ active: showRaw }" @click="showRaw = !showRaw">
                    <span>原始参数与来源</span>
                    <small>{{ showRaw ? "收起" : "展开" }}</small>
                  </button>
                  <div v-if="showRaw" class="model-catalog-raw-body">
                    <div class="model-catalog-source-tabs" role="tablist">
                      <button type="button" :class="{ active: sourceTab === 'merged' }" @click="sourceTab = 'merged'">最终参数</button>
                      <button v-for="entry in detail.entries" :key="`${entry.source}:${entry.sourceModelId}`" type="button" :class="{ active: sourceTab === `${entry.source}:${entry.sourceModelId}` }" @click="sourceTab = `${entry.source}:${entry.sourceModelId}`">{{ entry.source === "openrouter" ? "主数据" : "补偿数据" }} · {{ entry.sourceModelId }}</button>
                    </div>
                    <div v-if="sourceTab === 'merged'" class="model-catalog-raw-summary"><pre>{{ JSON.stringify({ model: detail.model, pricing: detail.pricing }, null, 2) }}</pre></div>
                    <div v-else-if="selectedEntry" class="model-catalog-raw-summary">
                      <div class="model-catalog-entry-facts"><span><b>用途</b>{{ selectedEntry.source === "openrouter" ? "主数据" : "缺失补偿" }}</span><span><b>模型 ID</b>{{ selectedEntry.sourceModelId }}</span><span><b>类型</b>{{ modeLabel(selectedEntry.mode) }}</span></div>
                      <pre>{{ JSON.stringify(selectedEntry.raw, null, 2) }}</pre>
                    </div>
                  </div>
                </section>
              </template>
            </div>
          </template>
        </section>
      </div>
    </Teleport>
  </section>
</template>
