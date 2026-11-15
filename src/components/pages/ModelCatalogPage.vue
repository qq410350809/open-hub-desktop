<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from "vue";
import { icons } from "../../icons";
import { useModelCatalog } from "../../composables/model/useModelCatalog";
import { useToast } from "../../composables/core/useToast";
import { CAPABILITIES, CATALOG_SORTS, MODEL_KINDS, buildCatalogIndex, catalogPage, effectiveLab, emptyCatalogFilters, filterCatalog, kindLabel, labLabel, modelInputPrice, priceText, reasoningText, referencePriceText, statusLabel, toggleCatalogComparison, tokenExactText, tokenText, type CapabilityKey, type CatalogSort, } from "../../composables/model/catalogView";
import CatalogModelDetail from "../model/CatalogModelDetail.vue";
import CatalogComparison from "../model/CatalogComparison.vue";
import CatalogDialog from "../model/CatalogDialog.vue";
import "../../styles/model-catalog.css";
const { modelCatalog, modelCatalogLoading, modelCatalogSyncing, modelCatalogError, modelCatalogSyncMessage, loadModelCatalog, syncModelCatalog } = useModelCatalog();
const { showToast } = useToast();
const filters = reactive(emptyCatalogFilters());
const sort = ref<CatalogSort>("name");
const page = ref(1);
const pageSize = ref(25);
const advancedOpen = ref(false);
const sourcesOpen = ref(false);
const selectedId = ref("");
const comparedIds = ref<string[]>([]);
const comparing = ref(false);
const index = computed(() => buildCatalogIndex(modelCatalog.value.models, modelCatalog.value.providers));
const modelMap = computed(() => new Map(modelCatalog.value.models.map((model) => [model.id, model])));
const selectedModel = computed(() => modelMap.value.get(selectedId.value));
const comparedModels = computed(() => comparedIds.value.flatMap((id) => modelMap.value.has(id) ? [modelMap.value.get(id)!] : []));
const filtered = computed(() => filterCatalog(index.value, filters, sort.value));
const pagination = computed(() => catalogPage(filtered.value, page.value, Number(pageSize.value)));
const kindCounts = computed(() => {
    const counts = new Map<string, number>();
    for (const model of modelCatalog.value.models)
        counts.set(model.kind || "text", (counts.get(model.kind || "text") ?? 0) + 1);
    return counts;
});
const labs = computed(() => {
    const counts = new Map<string, number>();
    for (const entry of index.value)
        counts.set(entry.lab, (counts.get(entry.lab) ?? 0) + 1);
    return [...counts].map(([id, count]) => ({ id, count, label: labLabel(id) })).sort((a, b) => a.label.localeCompare(b.label, "zh-CN"));
});
const providers = computed(() => [...modelCatalog.value.providers].sort((a, b) => a.name.localeCompare(b.name)));
const statuses = computed(() => [...new Set(modelCatalog.value.models.map((model) => model.status).filter(Boolean))].sort());
const advancedCount = computed(() => Number(filters.provider !== "all") + Number(filters.status !== "all") + Number(filters.pricing !== "all") + Number(filters.origin !== "all") + Number(filters.minContext > 0) + filters.capabilities.length);
const filterChips = computed(() => {
    const chips: {
        id: string;
        label: string;
        clear: () => void;
    }[] = [];
    if (filters.query.trim())
        chips.push({ id: "query", label: `搜索：${filters.query.trim()}`, clear: () => { filters.query = ""; } });
    if (filters.kind !== "all")
        chips.push({ id: "kind", label: kindLabel(filters.kind), clear: () => { filters.kind = "all"; } });
    if (filters.lab !== "all")
        chips.push({ id: "lab", label: labLabel(filters.lab), clear: () => { filters.lab = "all"; } });
    if (filters.provider !== "all")
        chips.push({ id: "provider", label: `服务商：${providers.value.find((provider) => provider.id === filters.provider)?.name || filters.provider}`, clear: () => { filters.provider = "all"; } });
    if (filters.status !== "all")
        chips.push({ id: "status", label: statusLabel(filters.status), clear: () => { filters.status = "all"; } });
    if (filters.pricing !== "all")
        chips.push({ id: "pricing", label: ({ priced: "公开标价", free: "有免费渠道", subscription: "有订阅渠道" })[filters.pricing], clear: () => { filters.pricing = "all"; } });
    if (filters.origin !== "all")
        chips.push({ id: "origin", label: ({ official: "有官方渠道", thirdParty: "有第三方渠道", unknown: "厂商归属未确定" })[filters.origin], clear: () => { filters.origin = "all"; } });
    if (filters.minContext > 0)
        chips.push({ id: "context", label: `上下文 ≥ ${tokenText(filters.minContext)}`, clear: () => { filters.minContext = 0; } });
    for (const key of filters.capabilities)
        chips.push({ id: key, label: CAPABILITIES.find((item) => item.key === key)!.label, clear: () => toggleCapability(key) });
    return chips;
});
const primarySources = computed(() => modelCatalog.value.sources.filter((source) => !source.source.startsWith("rows-")));
const missingMainSource = computed(() => modelCatalog.value.models.length > 0 && !modelCatalog.value.sources.some((source) => source.source === "models_dev_catalog"));
function toggleCapability(key: CapabilityKey) {
    filters.capabilities = filters.capabilities.includes(key) ? filters.capabilities.filter((item) => item !== key) : [...filters.capabilities, key];
}
function resetFilters() { Object.assign(filters, emptyCatalogFilters()); }
function toggleCompare(id: string) {
    const next = toggleCatalogComparison(comparedIds.value, id);
    if (next === comparedIds.value)
        showToast("最多对比 4 个模型，请先移除一个", true);
    else
        comparedIds.value = next;
}
function removeComparison(id: string) { comparedIds.value = comparedIds.value.filter((value) => value !== id); }
async function copyId(id: string) {
    try {
        await navigator.clipboard.writeText(id);
        showToast("已复制目录标识；网关调用 ID 请从客户端接入复制");
    }
    catch {
        showToast("复制失败，请手动选择内容复制", true);
    }
}
function dateText(value: string) {
    if (!value)
        return "尚未同步";
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? value : date.toLocaleString("zh-CN", { hour12: false });
}
watch([filters, sort, pageSize], () => { page.value = 1; }, { deep: true });
watch(() => pagination.value.page, (value) => { if (page.value !== value)
    page.value = value; });
watch(() => comparedModels.value.length, (count) => {
    if (count < 2) comparing.value = false;
});
watch(modelMap, (map) => {
    comparedIds.value = comparedIds.value.filter((id) => map.has(id));
    if (selectedId.value && !map.has(selectedId.value))
        selectedId.value = "";
});
onMounted(() => { if (!modelCatalog.value.models.length && !modelCatalogLoading.value)
    void loadModelCatalog(); });
</script>

<template>
  <div class="catalog-page">
    <header class="catalog-page-header">
      <div>
        <p class="catalog-eyebrow">MODEL REFERENCE</p>
        <h1>模型参数</h1>
        <p class="catalog-subtitle">查上下文、输出上限与思考能力，按需查看渠道报价。</p>
      </div>
      <div class="catalog-header-actions">
        <div class="catalog-sync-summary">
          <span>{{ modelCatalogSyncing ? '正在同步目录' : modelCatalog.syncedToday ? '今日已更新' : '本地参数目录' }}</span>
          <small>{{ dateText(modelCatalog.lastSyncedAt) }}</small>
        </div>
        <button type="button" class="catalog-button" @click="sourcesOpen = true">
          <span v-html="icons.database" />
          数据来源
        </button>
        <button
          type="button"
          class="catalog-button is-primary"
          :disabled="modelCatalogSyncing || modelCatalogLoading"
          @click="syncModelCatalog(true)"
        >
          <span :class="{ 'catalog-spin': modelCatalogSyncing }" v-html="icons.restore" />
          {{ modelCatalogSyncing ? '同步中…' : '同步目录' }}
        </button>
      </div>
    </header>
    <div v-if="modelCatalogError" class="catalog-notice is-error" role="alert">
      {{ modelCatalogError }}
      <span v-if="modelCatalog.models.length">。仍可浏览上次保存的目录。</span>
      <button
        type="button"
        class="catalog-text-button"
        :disabled="modelCatalogSyncing || modelCatalogLoading"
        @click="modelCatalog.models.length ? syncModelCatalog(true) : loadModelCatalog()"
      >
        重试
      </button>
    </div>
    <div v-else-if="modelCatalogSyncing" class="catalog-notice" role="status">
      {{ modelCatalogSyncMessage || '正在拉取模型参数与渠道明细，已有目录仍可浏览。' }}
    </div>
    <div v-else-if="missingMainSource" class="catalog-notice" role="status">当前快照缺少 models.dev 主来源，部分能力参数可能不完整。可同步目录补全。</div>
    <section class="catalog-browser" aria-label="模型参数查询">
      <div class="catalog-toolbar">
        <div class="catalog-search">
          <span v-html="icons.search" />
          <input v-model="filters.query" type="search" aria-label="搜索模型参数" placeholder="搜索模型名称、ID、厂商或服务商" />
          <button v-if="filters.query" type="button" class="catalog-icon" aria-label="清空搜索" @click="filters.query = ''">
            <span v-html="icons.close" />
          </button>
        </div>
        <select v-model="filters.lab" class="catalog-select" aria-label="模型厂商">
          <option value="all">全部厂商</option>
          <option v-for="lab in labs" :key="lab.id" :value="lab.id">{{ lab.label }} · {{ lab.count }}</option>
        </select>
        <button
          type="button"
          class="catalog-button"
          :class="{ 'is-active': advancedOpen || advancedCount > 0 }"
          :aria-expanded="advancedOpen"
          aria-controls="catalog-advanced"
          @click="advancedOpen = !advancedOpen"
        >
          <span v-html="icons.sliders" />
          筛选
          <span v-if="advancedCount" class="catalog-count">{{ advancedCount }}</span>
        </button>
      </div>
      <div class="catalog-kind-tabs" role="group" aria-label="模型类型">
        <button
          v-for="kind in MODEL_KINDS"
          :key="kind.value"
          type="button"
          :aria-pressed="filters.kind === kind.value"
          :class="{ active: filters.kind === kind.value }"
          @click="filters.kind = kind.value"
        >
          {{ kind.label }}
          <span>{{ kind.value === 'all' ? modelCatalog.models.length : kindCounts.get(kind.value) ?? 0 }}</span>
        </button>
      </div>
      <div v-if="advancedOpen" id="catalog-advanced" class="catalog-advanced">
        <div class="catalog-filter-fields">
          <label>
            公开服务商
            <select v-model="filters.provider">
              <option value="all">全部服务商</option>
              <option v-for="provider in providers" :key="provider.id" :value="provider.id">{{ provider.name }}</option>
            </select>
          </label>
          <label>
            发布状态
            <select v-model="filters.status">
              <option value="all">全部状态</option>
              <option v-for="status in statuses" :key="status" :value="status">{{ statusLabel(status) }}</option>
            </select>
          </label>
          <label>
            渠道计费
            <select v-model="filters.pricing">
              <option value="all">不限计费</option>
              <option value="priced">有公开标价</option>
              <option value="free">有免费渠道</option>
              <option value="subscription">有订阅渠道</option>
            </select>
          </label>
          <label>
            渠道性质
            <select v-model="filters.origin">
              <option value="all">不限性质</option>
              <option value="official">有原厂官方渠道</option>
              <option value="thirdParty">有第三方渠道</option>
              <option value="unknown">厂商归属未确定</option>
            </select>
          </label>
          <label>
            上下文窗口
            <select v-model.number="filters.minContext">
              <option :value="0">不限容量</option>
              <option :value="32000">至少 32K</option>
              <option :value="128000">至少 128K</option>
              <option :value="200000">至少 200K</option>
              <option :value="1000000">至少 1M</option>
            </select>
          </label>
        </div>
        <fieldset class="catalog-capability-filters">
          <legend>
            支持能力
            <small>多选时需同时满足</small>
          </legend>
          <label v-for="capability in CAPABILITIES" :key="capability.key" class="catalog-check">
            <input
              type="checkbox"
              :checked="filters.capabilities.includes(capability.key)"
              @change="toggleCapability(capability.key)"
            />
            {{ capability.label }}
          </label>
        </fieldset>
      </div>
      <div v-if="filterChips.length" class="catalog-filter-chips" aria-label="当前生效的筛选">
        <button
          v-for="chip in filterChips"
          :key="chip.id"
          type="button"
          :aria-label="`取消筛选：${chip.label}`"
          @click="chip.clear()"
        >
          {{ chip.label }}
          <span v-html="icons.close" />
        </button>
        <button type="button" class="catalog-reset" @click="resetFilters">清空筛选</button>
      </div>
      <div class="catalog-results-bar">
        <p>
          <strong>{{ filtered.length.toLocaleString() }}</strong>
          个结果
          <span>/ {{ modelCatalog.models.length.toLocaleString() }} 个目录模型</span>
        </p>
        <label>
          排序
          <select v-model="sort" aria-label="模型排序">
            <option v-for="item in CATALOG_SORTS" :key="item.value" :value="item.value">{{ item.label }}</option>
          </select>
        </label>
      </div>
      <div v-if="modelCatalogLoading && !modelCatalog.models.length" class="catalog-empty" role="status">
        <span class="catalog-spin" v-html="icons.restore" />
        <h2>正在读取参数目录</h2>
        <p>已有缓存会优先加载。</p>
      </div>
      <div v-else-if="!modelCatalog.models.length" class="catalog-empty">
        <span v-html="icons.database" />
        <h2>尚无模型参数</h2>
        <p>同步公开目录后，可以查询模型能力和渠道报价。</p>
        <button
          type="button"
          class="catalog-button is-primary"
          :disabled="modelCatalogSyncing"
          @click="syncModelCatalog(true)"
        >
          同步目录
        </button>
      </div>
      <div v-else-if="!filtered.length" class="catalog-empty">
        <span v-html="icons.search" />
        <h2>没有符合条件的模型</h2>
        <p>尝试缩短关键词，或移除上方的筛选条件。</p>
        <button type="button" class="catalog-button" @click="resetFilters">清空筛选</button>
      </div>
      <div v-else class="catalog-table-scroll">
        <table class="catalog-model-table">
          <thead>
            <tr>
              <th scope="col" class="catalog-compare-col">对比</th>
              <th scope="col">模型 / 厂商</th>
              <th scope="col">上下文</th>
              <th scope="col">最大输出</th>
              <th scope="col">思考与能力</th>
              <th scope="col">
                <span title="官方公开报价，USD / 百万 tokens">官方输入 / 输出价</span>
              </th>
              <th scope="col">详情</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="model in pagination.items" :key="model.id" :class="{ 'is-selected': selectedId === model.id }">
              <td>
                <input
                  type="checkbox"
                  :checked="comparedIds.includes(model.id)"
                  :disabled="!comparedIds.includes(model.id) && comparedIds.length >= 4"
                  :aria-label="`对比 ${model.name}`"
                  @change="toggleCompare(model.id)"
                />
              </td>
              <td>
                <button type="button" class="catalog-model-name" @click="selectedId = model.id">{{ model.name }}</button>
                <span class="catalog-model-meta">
                  {{ labLabel(effectiveLab(model)) }}
                  <span>·</span>
                  {{ kindLabel(model.kind) }}
                  <small v-if="model.status && model.status !== 'ga'">{{ statusLabel(model.status) }}</small>
                </span>
                <div class="catalog-id-line">
                  <code>{{ model.id }}</code>
                  <button
                    type="button"
                    class="catalog-icon"
                    :aria-label="`复制 ${model.name} 的目录标识`"
                    @click="copyId(model.id)"
                  >
                    <span v-html="icons.copy" />
                  </button>
                </div>
              </td>
              <td class="catalog-token-cell" :title="tokenExactText(model.contextLength)">{{ tokenText(model.contextLength) }}</td>
              <td class="catalog-token-cell" :title="tokenExactText(model.maxOutputTokens)">{{ tokenText(model.maxOutputTokens) }}</td>
              <td>
                <div class="catalog-row-capabilities">
                  <span v-if="model.reasoning" class="catalog-tag is-brand" :title="reasoningText(model)">
                    思考
                    {{ model.reasoningEffortMax ? ` · ${model.reasoningEffortMax}` : '' }}
                  </span>
                  <span v-if="model.toolCall" class="catalog-tag">工具</span>
                  <span v-if="model.structured" class="catalog-tag">结构化</span>
                  <span v-if="model.attachment" class="catalog-tag">附件</span>
                  <span
                    v-if="!model.reasoning && !model.toolCall && !model.structured && !model.attachment"
                    class="catalog-muted"
                  >
                    未声明
                  </span>
                </div>
              </td>
              <td>
                <span class="catalog-price-cell">{{ referencePriceText(model) }}</span>
                <small v-if="model.freeHostCount > 0" class="catalog-block-muted">另有 {{ model.freeHostCount }} 个免费渠道</small>
                <small v-else-if="modelInputPrice(model) != null" class="catalog-block-muted">
                  市场最低输入
                  {{ priceText(modelInputPrice(model)) }}
                </small>
              </td>
              <td>
                <button
                  type="button"
                  class="catalog-text-button"
                  :aria-label="`查看 ${model.name} 的参数详情`"
                  @click="selectedId = model.id"
                >
                  查看
                  <span v-html="icons.chevron" />
                </button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <footer v-if="filtered.length" class="catalog-pagination">
        <p>{{ pagination.start }}–{{ pagination.end }} / {{ filtered.length.toLocaleString() }}</p>
        <div>
          <label>
            每页
            <select v-model.number="pageSize" aria-label="每页模型数">
              <option :value="25">25</option>
              <option :value="50">50</option>
              <option :value="100">100</option>
            </select>
          </label>
          <button type="button" class="catalog-button" :disabled="pagination.page <= 1" @click="page = pagination.page - 1">上一页</button>
          <span>{{ pagination.page }} / {{ pagination.pages }}</span>
          <button
            type="button"
            class="catalog-button"
            :disabled="pagination.page >= pagination.pages"
            @click="page = pagination.page + 1"
          >
            下一页
          </button>
        </div>
      </footer>
    </section>
    <p class="catalog-page-note">这是公开参数参考目录，不代表本机已接入或当前可用的模型。实际调用请到「模型反代 → 客户端接入」。价格为公开参考，不等同于站点的实际扣费。</p>
    <aside v-if="comparedModels.length" class="catalog-compare-tray" aria-label="已选对比模型">
      <div>
        <strong>
          参数对比
          <span>{{ comparedModels.length }} / 4</span>
        </strong>
        <button
          v-for="model in comparedModels"
          :key="model.id"
          type="button"
          :aria-label="`从对比中移除 ${model.name}`"
          @click="removeComparison(model.id)"
        >
          {{ model.name }}
          <span v-html="icons.close" />
        </button>
      </div>
      <div>
        <button type="button" class="catalog-text-button" @click="comparedIds = []">清空</button>
        <button
          type="button"
          class="catalog-button is-primary"
          :disabled="comparedModels.length < 2"
          @click="comparing = true"
        >
          {{ comparedModels.length < 2 ? '再选 1 个开始对比' : '开始对比' }}
        </button>
      </div>
    </aside>
    <CatalogModelDetail
      v-if="selectedModel"
      :model="selectedModel"
      :sources="modelCatalog.sources"
      :compared="comparedIds.includes(selectedModel.id)"
      :compare-full="comparedIds.length >= 4"
      @close="selectedId = ''"
      @compare="toggleCompare(selectedModel.id)"
    />
    <CatalogComparison
      v-if="comparing && comparedModels.length >= 2"
      :models="comparedModels"
      @close="comparing = false"
      @remove="removeComparison"
    />
    <CatalogDialog v-if="sourcesOpen" title="参数目录数据来源" @close="sourcesOpen = false">
      <header class="catalog-compare-header">
        <p class="catalog-eyebrow">CATALOG SOURCES</p>
        <h2>数据来源与同步</h2>
        <p class="catalog-muted">目录由公开模型参数与渠道报价合并而成，保存在本地。同步失败时保留已有快照。</p>
      </header>
      <div class="catalog-detail-body">
        <dl class="catalog-facts">
          <div>
            <dt>最近同步</dt>
            <dd>{{ dateText(modelCatalog.lastSyncedAt) }}</dd>
          </div>
          <div>
            <dt>收录模型 / 服务商</dt>
            <dd>{{ modelCatalog.models.length.toLocaleString() }} / {{ modelCatalog.providers.length.toLocaleString() }}</dd>
          </div>
        </dl>
        <section v-for="source in primarySources" :key="source.source" class="catalog-source-card">
          <h3>
            {{ source.source === 'models_dev_catalog' ? 'models.dev · 能力参数与渠道声明' : source.source === 'llmpricing_manifest' ? 'llmpricing · 模型与价格聚合' : source.source }}
          </h3>
          <p class="catalog-muted">{{ source.recordCount.toLocaleString() }} 条记录 · {{ dateText(source.fetchedAt) }}</p>
          <code>{{ source.url }}</code>
        </section>
        <p v-if="!primarySources.length" class="catalog-empty-inline">尚无来源记录，请同步目录。</p>
        <details class="catalog-disclosure">
          <summary>如何理解参数与报价</summary>
          <p class="catalog-muted">模型级参数可能汇总多个渠道的声明；具体接入时请查看渠道参数。未声明不等于不支持。免费与订阅分开展示，缺失报价不会按免费处理。同步默认每天更新一次，也可手动重新同步。</p>
        </details>
      </div>
    </CatalogDialog>
  </div>
</template>
