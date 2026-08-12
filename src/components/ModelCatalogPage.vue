<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import type { ModelCatalogDetail, ModelCatalogItem } from "../types";
import AppTable, { type AppTableColumn } from "./AppTable.vue";

const store = useStore();
const query = ref("");
const manufacturer = ref("all");
const mode = ref("all");
const pricing = ref("all");
const selectedKey = ref("");
const detailOpen = ref(false);
const detail = ref<ModelCatalogDetail | null>(null);
const detailLoading = ref(false);
const detailError = ref("");
const sourceTab = ref("merged");
const showRaw = ref(false);
const currentPage = ref(1);
const pageSize = ref(50);
const sorting = ref<Array<{ id: string; desc: boolean }>>([]);

const manufacturers = computed(() => [
  "all",
  ...new Set(store.modelCatalog.value.models.map((model) => model.manufacturer).filter(Boolean)),
].sort((a, b) => a === "all" ? -1 : b === "all" ? 1 : a.localeCompare(b)));

const modes = computed(() => [
  "all",
  ...new Set(store.modelCatalog.value.models.map((model) => model.mode).filter(Boolean)),
].sort((a, b) => a === "all" ? -1 : b === "all" ? 1 : a.localeCompare(b)));

const filteredModels = computed(() => {
  const term = query.value.trim().toLocaleLowerCase("zh-CN");
  return store.modelCatalog.value.models.filter((model) => {
    if (manufacturer.value !== "all" && model.manufacturer !== manufacturer.value) return false;
    if (mode.value !== "all" && model.mode !== mode.value) return false;
    const hasPrice = model.inputCostPerToken > 0 || model.outputCostPerToken > 0 || model.imageCost > 0 || model.requestCost > 0;
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
  });
});

const tableColumns = computed<AppTableColumn[]>(() => [
  { key: "displayName", title: "模型", width: "minmax(180px,1fr)", sortable: true },
  { key: "manufacturer", title: "厂商", width: "minmax(110px,.6fr)", sortable: true },
  { key: "mode", title: "类型", width: "70px", sortable: true },
  { key: "contextLength", title: "上下文", width: "80px", align: "right", sortable: true },
  { key: "inputCostPerToken", title: "输入 / 1M", width: "100px", align: "right", sortable: true },
  { key: "outputCostPerToken", title: "输出 / 1M", width: "100px", align: "right", sortable: true },
]);

const selectedModel = computed(() =>
  store.modelCatalog.value.models.find((model) => model.canonicalKey === selectedKey.value) ?? null,
);
const selectedEntry = computed(() => {
  if (!detail.value || sourceTab.value === "merged") return null;
  return detail.value.entries.find((entry) => `${entry.source}:${entry.sourceModelId}` === sourceTab.value) ?? null;
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
function manufacturerLabel(value: string) {
  const labels: Record<string, string> = {
    openai: "OpenAI", anthropic: "Anthropic", google: "Google", deepseek: "DeepSeek",
    qwen: "Qwen", "x-ai": "xAI", mistralai: "Mistral", "meta-llama": "Meta Llama",
    cohere: "Cohere", perplexity: "Perplexity", "z-ai": "Z.ai", unknown: "未知厂商",
  };
  return labels[value] || value || "未知厂商";
}
function tokenText(value: number) {
  if (!value) return "—";
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(value % 1_000_000 ? 1 : 0)}M`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(value % 1_000 ? 1 : 0)}K`;
  return String(value);
}
function moneyPerMillion(value: number) {
  if (!value) return "—";
  return `$${(value * 1_000_000).toLocaleString("en-US", { maximumFractionDigits: 4 })} / 1M`;
}
function shortMoney(value: number) {
  if (!value) return "—";
  return `$${(value * 1_000_000).toLocaleString("en-US", { maximumFractionDigits: 3 })} / 1M`;
}
function dateText(value: string) {
  if (!value) return "尚未同步";
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString("zh-CN", { hour12: false });
}
async function selectModel(model: ModelCatalogItem) {
  selectedKey.value = model.canonicalKey;
  detailOpen.value = true;
  sourceTab.value = "merged";
  showRaw.value = false;
  detail.value = null;
  detailError.value = "";
  detailLoading.value = true;
  try { detail.value = await store.getModelCatalogDetail(model.canonicalKey); }
  catch (error) { detailError.value = String(error); }
  finally { detailLoading.value = false; }
}

function closeDetail() { detailOpen.value = false; }

async function manualSync() {
  const result = await store.syncModelCatalog(true);
  if (!result || !selectedKey.value) return;
  const selected = store.modelCatalog.value.models.find((model) => model.canonicalKey === selectedKey.value);
  if (selected) await selectModel(selected);
  else { selectedKey.value = ""; detail.value = null; detailOpen.value = false; }
}

watch([query, manufacturer, mode, pricing, pageSize], () => { currentPage.value = 1; });
watch(() => store.modelCatalog.value.lastSyncedAt, () => {
  const selected = store.modelCatalog.value.models.find((model) => model.canonicalKey === selectedKey.value);
  if (selected && selectedKey.value && detailOpen.value) void selectModel(selected);
});
watch(filteredModels, () => {
  if (selectedKey.value && !filteredModels.value.some((model) => model.canonicalKey === selectedKey.value)) {
    selectedKey.value = "";
    detail.value = null;
    detailOpen.value = false;
    showRaw.value = false;
  }
});

onMounted(() => {
  if (!store.modelCatalog.value.models.length && !store.modelCatalogLoading.value) void store.loadModelCatalog();
});
</script>

<template>
  <section class="model-catalog-page" aria-labelledby="modelparams-title">
    <header class="model-catalog-header">
      <div class="model-catalog-heading">
        <span class="model-catalog-heading-icon" v-html="icons.cpu" />
        <div><h1 id="modelparams-title">模型参数</h1><p>按厂商、类型、上下文和价格查询；每天自动同步一次</p></div>
      </div>
      <div class="model-catalog-header-actions">
        <div class="model-catalog-sync-meta"><span :class="{ active: store.modelCatalog.value.syncedToday }" /><div><strong>{{ store.modelCatalog.value.syncedToday ? "今天已同步" : "等待同步" }}</strong><small>{{ dateText(store.modelCatalog.value.lastSyncedAt) }}</small></div></div>
        <button class="primary-button model-catalog-sync" type="button" :disabled="store.modelCatalogSyncing.value" @click="manualSync"><span :class="{ 'is-spinning': store.modelCatalogSyncing.value }" v-html="icons.restore" /><span>{{ store.modelCatalogSyncing.value ? "同步中…" : "立即同步" }}</span></button>
      </div>
    </header>

    <div class="model-catalog-toolbar">
      <label class="search-box model-catalog-search"><span v-html="icons.search" /><input v-model="query" type="search" placeholder="搜索模型名称或 ID…" /></label>
      <select v-model="manufacturer" aria-label="模型厂商筛选"><option value="all">全部厂商</option><option v-for="item in manufacturers.filter(item => item !== 'all')" :key="item" :value="item">{{ manufacturerLabel(item) }}</option></select>
      <select v-model="mode" aria-label="模型类型筛选"><option value="all">全部类型</option><option v-for="item in modes.filter(item => item !== 'all')" :key="item" :value="item">{{ modeLabel(item) }}</option></select>
      <select v-model="pricing" aria-label="价格筛选"><option value="all">全部价格</option><option value="paid">有价格</option><option value="free">无价格</option></select>
      <div class="model-catalog-toolbar-stats">
        <b>{{ store.modelCatalog.value.total.toLocaleString() }}</b><span>个模型</span>
        <i></i>
        <b>{{ Math.max(0, manufacturers.length - 1).toLocaleString() }}</b><span>个厂商</span>
      </div>
      <p v-if="store.modelCatalogError.value" class="model-catalog-error">{{ store.modelCatalogError.value }}</p>
    </div>

    <div class="model-catalog-layout">
      <section class="model-catalog-list-panel" :class="{ 'has-detail': detailOpen && selectedModel }">
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
            <div class="model-table-model">
              <strong>{{ row.displayName || row.canonicalKey }}</strong>
              <small>{{ row.canonicalKey }}</small>
            </div>
          </template>
          <template #cell-manufacturer="{ row }">{{ manufacturerLabel(row.manufacturer) }}</template>
          <template #cell-mode="{ row }"><i class="model-table-pill">{{ modeLabel(row.mode) }}</i></template>
          <template #cell-contextLength="{ row }">{{ tokenText(row.contextLength) }}</template>
          <template #cell-inputCostPerToken="{ row }">{{ shortMoney(row.inputCostPerToken) }}</template>
          <template #cell-outputCostPerToken="{ row }">{{ shortMoney(row.outputCostPerToken) }}</template>
        </AppTable>
      </section>

      <aside class="model-catalog-detail-panel" :class="{ open: detailOpen && selectedModel }">
        <div v-if="!selectedModel || !detailOpen" class="model-catalog-detail-empty">
          <span v-html="icons.cpu" />
          <h2>选择一个模型</h2>
          <p>点击左侧任意行查看完整参数与价格</p>
        </div>
        <template v-else>
          <header class="model-catalog-detail-head">
            <div class="model-catalog-detail-head-main">
              <span>{{ manufacturerLabel(selectedModel.manufacturer) }}</span>
              <h2>{{ selectedModel.displayName || selectedModel.canonicalKey }}</h2>
              <code>{{ selectedModel.canonicalKey }}</code>
            </div>
            <button type="button" aria-label="关闭详情" @click="closeDetail" v-html="icons.close" />
          </header>
          <div class="model-catalog-detail-scroll">
            <div v-if="detailLoading" class="model-catalog-empty">正在读取详情…</div>
            <p v-else-if="detailError" class="model-catalog-detail-error">{{ detailError }}</p>
            <template v-else-if="detail">
              <div class="model-catalog-detail-badges">
                <b>{{ modeLabel(selectedModel.mode) }}</b>
                <b v-if="detail.entries.length > 1">{{ detail.entries.length }} 条来源</b>
              </div>

              <section class="model-catalog-section">
                <h3>基础参数</h3>
                <div class="model-catalog-facts">
                  <article><span>上下文</span><strong>{{ tokenText(selectedModel.contextLength) }}</strong></article>
                  <article><span>最大输入</span><strong>{{ tokenText(selectedModel.maxInputTokens) }}</strong></article>
                  <article><span>最大输出</span><strong>{{ tokenText(selectedModel.maxOutputTokens) }}</strong></article>
                  <article><span>能力数</span><strong>{{ selectedModel.capabilities.length }}</strong></article>
                </div>
              </section>

              <section class="model-catalog-section">
                <h3>参考价格</h3>
                <div class="model-catalog-price-grid">
                  <article><span>输入 Token</span><strong>{{ moneyPerMillion(selectedModel.inputCostPerToken) }}</strong></article>
                  <article><span>输出 Token</span><strong>{{ moneyPerMillion(selectedModel.outputCostPerToken) }}</strong></article>
                  <article><span>缓存读取</span><strong>{{ moneyPerMillion(selectedModel.cacheReadCostPerToken) }}</strong></article>
                  <article><span>缓存写入</span><strong>{{ moneyPerMillion(selectedModel.cacheWriteCostPerToken) }}</strong></article>
                  <article><span>图像</span><strong>{{ selectedModel.imageCost ? `$${selectedModel.imageCost}` : "—" }}</strong></article>
                  <article><span>单次请求</span><strong>{{ selectedModel.requestCost ? `$${selectedModel.requestCost}` : "—" }}</strong></article>
                </div>
                <p class="model-catalog-section-note">优先采用主目录价格；缺失字段由补偿数据填充，单位 /1M tokens。</p>
              </section>

              <section class="model-catalog-section">
                <h3>能力标签</h3>
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
      </aside>
    </div>
  </section>
</template>
