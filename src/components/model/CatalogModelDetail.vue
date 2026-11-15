<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import type { ModelCatalogDetail, ModelCatalogItem, ModelCatalogSourceStatus } from "../../types";
import { useModelCatalog } from "../../composables/model/useModelCatalog";
import { useSiteActions } from "../../composables/site/useSiteActions";
import { useToast } from "../../composables/core/useToast";
import { CAPABILITIES, effectiveLab, estimateHostCost, hostPriceText, kindLabel, labLabel, modalityLabel, priceText, reasoningText, safeCatalogUrl, statusLabel, tokenExactText, tokenText, } from "../../composables/model/catalogView";
import CatalogDialog from "./CatalogDialog.vue";
const props = defineProps<{
    model: ModelCatalogItem;
    sources: ModelCatalogSourceStatus[];
    compared: boolean;
    compareFull: boolean;
}>();
const emit = defineEmits<{
    close: [
    ];
    compare: [
    ];
}>();
const { getModelCatalogDetail } = useModelCatalog();
const { openExternal } = useSiteActions();
const { showToast } = useToast();
const tab = ref<"parameters" | "channels" | "sources">("parameters");
const detail = ref<ModelCatalogDetail | null>(null);
const loading = ref(false);
const error = ref("");
const channelQuery = ref("");
const channelScope = ref("all");
const model = computed(() => detail.value?.model ?? props.model);
const hosts = computed(() => (detail.value?.hosts ?? []).filter((host) => {
    if (channelScope.value === "official" && !host.official)
        return false;
    if (channelScope.value === "thirdParty" && host.official)
        return false;
    if (channelScope.value === "free" && (!host.isFree || host.subscription))
        return false;
    if (channelScope.value === "subscription" && !host.subscription)
        return false;
    const query = channelQuery.value.trim().toLowerCase();
    return !query || `${host.name} ${host.provider} ${host.modelId ?? ""}`.toLowerCase().includes(query);
}));
const inputMillions = ref(10);
const outputMillions = ref(2);
const description = computed(() => model.value.modelsDevExtras?.description);
const links = computed(() => (model.value.modelsDevExtras?.links ?? []).flatMap((link) => {
    const url = safeCatalogUrl(link?.url);
    return url ? [{ url, label: String(link.label || link.type || "参考文档") }] : [];
}));
const weights = computed(() => (model.value.modelsDevExtras?.weights ?? []).flatMap((link) => {
    const url = safeCatalogUrl(link?.url);
    return url ? [{ url, label: String(link.label || link.quantization || "权重文件") }] : [];
}));
const identitySource = computed(() => ({ canonical: "models.dev 标准条目", llmpricing_lab: "llmpricing 厂商声明", keyword_rule: "本地关键词推断", inherited: "同名模型继承", canonical_reverse: "标准条目反向匹配", unknown: "归属未确定" } as Record<string, string>)[model.value.identitySource] || "未声明");
const benchmarks = computed(() => [
    { label: "AA 综合质量", value: model.value.aaIdx, unit: "" },
    { label: "AA 编程", value: model.value.aaCoding, unit: "" },
    { label: "AA Agent 任务", value: model.value.aaAgentic, unit: "" },
    { label: "生成速度", value: model.value.aaSpeed, unit: "tokens/s" },
    { label: "首 Token 延迟", value: model.value.aaTtft, unit: "s" },
].filter((item) => item.value != null && Number.isFinite(item.value)));
const extraBenchmarks = computed(() => model.value.modelsDevExtras?.benchmarks ?? []);
let revision = 0;
let disposed = false;
async function reload() {
    const request = ++revision;
    const id = props.model.id;
    loading.value = true;
    error.value = "";
    try {
        const next = await getModelCatalogDetail(id);
        if (!disposed && request === revision && props.model.id === id) {
            if (!next || next.model.id !== id)
                throw new Error("详情与当前模型不一致，请重试");
            detail.value = next;
        }
    }
    catch (cause) {
        if (!disposed && request === revision)
            error.value = cause instanceof Error ? cause.message : String(cause);
    }
    finally {
        if (!disposed && request === revision)
            loading.value = false;
    }
}
watch(() => props.model, () => {
    detail.value = null;
    tab.value = "parameters";
    channelQuery.value = "";
    channelScope.value = "all";
    void reload();
}, { immediate: true });
onBeforeUnmount(() => { disposed = true; revision++; });
async function copy(value: string, label: string) {
    try {
        await navigator.clipboard.writeText(value);
        showToast(`已复制${label}`);
    }
    catch {
        showToast("复制失败，请手动选择内容复制", true);
    }
}
function open(value: unknown) { const url = safeCatalogUrl(value); if (url)
    void openExternal(url); }
</script>

<template>
  <CatalogDialog title="模型详情" drawer @close="emit('close')">
    <header class="catalog-detail-header">
      <div class="catalog-eyebrow">
        {{ labLabel(effectiveLab(model)) }}
        <span>·</span>
        {{ kindLabel(model.kind) }}
      </div>
      <h2>{{ model.name }}</h2>
      <div class="catalog-id-line">
        <code>{{ model.id }}</code>
        <button type="button" class="catalog-text-button" @click="copy(model.id, '目录标识')">复制目录标识</button>
      </div>
      <p class="catalog-muted">目录标识用于参数检索；实际网关调用 ID 请从「客户端接入」复制。</p>
      <div class="catalog-detail-actions">
        <span class="catalog-tag">{{ statusLabel(model.status) }}</span>
        <button type="button" class="catalog-button" :disabled="!compared && compareFull" @click="emit('compare')">
          {{ compared ? '已加入对比 · 移除' : compareFull ? '对比已满（4/4）' : '加入对比' }}
        </button>
      </div>
    </header>
    <nav class="catalog-detail-tabs" aria-label="详情内容">
      <button
        type="button"
        :class="{ active: tab === 'parameters' }"
        :aria-pressed="tab === 'parameters'"
        @click="tab = 'parameters'"
      >
        能力参数
      </button>
      <button
        type="button"
        :class="{ active: tab === 'channels' }"
        :aria-pressed="tab === 'channels'"
        @click="tab = 'channels'"
      >
        渠道与价格
      </button>
      <button type="button" :class="{ active: tab === 'sources' }" :aria-pressed="tab === 'sources'" @click="tab = 'sources'">来源与评测</button>
    </nav>
    <div class="catalog-detail-body">
      <div v-if="error" class="catalog-notice is-error" role="alert">
        详情加载失败：
        {{ error }}
        。当前仍显示目录快照。
        <button type="button" class="catalog-text-button" @click="reload">重试</button>
      </div>
      <p v-else-if="loading" class="catalog-muted" role="status">正在读取渠道明细…</p>
      <template v-if="tab === 'parameters'">
        <p v-if="description" class="catalog-description">{{ description }}</p>
        <section aria-label="容量参数" class="catalog-capacity-grid">
          <div>
            <span>上下文窗口</span>
            <strong :title="tokenExactText(model.contextLength)">{{ tokenText(model.contextLength) }}</strong>
            <small>输入与输出共享的总容量</small>
          </div>
          <div>
            <span>最大输入</span>
            <strong :title="tokenExactText(model.maxInputTokens)">{{ tokenText(model.maxInputTokens) }}</strong>
            <small>未声明时不等同于上下文窗口</small>
          </div>
          <div>
            <span>最大输出</span>
            <strong :title="tokenExactText(model.maxOutputTokens)">{{ tokenText(model.maxOutputTokens) }}</strong>
            <small>单次响应的输出上限</small>
          </div>
        </section>
        <section class="catalog-section">
          <h3>支持能力</h3>
          <div class="catalog-capabilities-grid">
            <div v-for="capability in CAPABILITIES" :key="capability.key">
              <span>{{ capability.label }}</span>
              <strong :class="{ 'is-supported': model[capability.key] }">{{ model[capability.key] ? '支持' : '未声明支持' }}</strong>
            </div>
          </div>
        </section>
        <section class="catalog-section">
          <h3>思考与输入输出</h3>
          <dl class="catalog-facts">
            <div>
              <dt>思考档位</dt>
              <dd>{{ reasoningText(model) }}</dd>
            </div>
            <div>
              <dt>输入类型</dt>
              <dd>{{ model.inputModalities.map(modalityLabel).join(' / ') || '未声明' }}</dd>
            </div>
            <div>
              <dt>输出类型</dt>
              <dd>{{ (model.outputModalities ?? []).map(modalityLabel).join(' / ') || '未声明' }}</dd>
            </div>
            <div>
              <dt>推理字段</dt>
              <dd>
                <code>{{ model.interleavedFields?.join(' / ') || '未声明' }}</code>
              </dd>
            </div>
            <div>
              <dt>知识截止</dt>
              <dd>{{ model.knowledge || '未声明' }}</dd>
            </div>
            <div>
              <dt>发布日期</dt>
              <dd>{{ model.releaseDate || '未声明' }}</dd>
            </div>
            <div>
              <dt>许可证</dt>
              <dd>{{ model.modelsDevExtras?.license || '未声明' }}</dd>
            </div>
          </dl>
          <p class="catalog-muted">以上为目录聚合值；不同渠道的容量、思考档位和快速模式可能不同，请在「渠道与价格」核对。参数设置在使用模型的客户端中完成。</p>
        </section>
        <details v-if="model.contextMin > 0 && model.contextMax > model.contextMin" class="catalog-disclosure">
          <summary>渠道上下文存在差异</summary>
          <p class="catalog-muted">已登记的渠道范围为 {{ tokenText(model.contextMin) }}–{{ tokenText(model.contextMax) }}，以实际接入渠道的声明为准。</p>
        </details>
      </template>
      <template v-else-if="tab === 'channels'">
        <p class="catalog-muted">公开目录中的服务商报价，单位为 USD / 百万 tokens；不代表本机站点库或网关的实时可用性。</p>
        <div class="catalog-inline-filters">
          <input v-model="channelQuery" type="search" aria-label="搜索渠道" placeholder="搜索服务商或渠道模型 ID" />
          <select v-model="channelScope" aria-label="渠道性质">
            <option value="all">全部渠道</option>
            <option value="official">原厂官方</option>
            <option value="thirdParty">第三方</option>
            <option value="free">免费</option>
            <option value="subscription">订阅制</option>
          </select>
        </div>
        <div v-if="hosts.length" class="catalog-table-scroll">
          <table class="catalog-host-table">
            <thead>
              <tr>
                <th>服务商 / 渠道声明</th>
                <th>性质</th>
                <th>输入 / 输出</th>
                <th>缓存读 / 写</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="(host, index) in hosts" :key="`${host.provider}:${host.modelId}:${index}`">
                <td>
                  <strong>{{ host.name }}</strong>
                  <code class="catalog-secondary-id">{{ host.modelId || host.provider }}</code>
                  <details class="catalog-host-declaration">
                    <summary>容量与思考参数</summary>
                    <dl class="catalog-facts">
                      <div>
                        <dt>上下文</dt>
                        <dd>{{ tokenText(host.context) }}</dd>
                      </div>
                      <div>
                        <dt>输入上限</dt>
                        <dd>{{ tokenText(host.inputLimit) }}</dd>
                      </div>
                      <div>
                        <dt>输出上限</dt>
                        <dd>{{ tokenText(host.outputLimit) }}</dd>
                      </div>
                      <div>
                        <dt>思考档位</dt>
                        <dd>{{ reasoningText({ reasoning: model.reasoning, reasoningOptions: host.reasoningOptions }) }}</dd>
                      </div>
                      <div>
                        <dt>输出类型</dt>
                        <dd>{{ host.outputModalities?.map(modalityLabel).join(' / ') || '未声明' }}</dd>
                      </div>
                      <div>
                        <dt>推理字段</dt>
                        <dd>{{ host.interleavedField || '未声明' }}</dd>
                      </div>
                      <div>
                        <dt>快速模式</dt>
                        <dd>{{ host.fastMode ? '已声明，可能额外计费' : '未声明' }}</dd>
                      </div>
                    </dl>
                    <button
                      v-if="safeCatalogUrl(host.doc)"
                      type="button"
                      class="catalog-text-button"
                      @click="open(host.doc)"
                    >
                      查看渠道文档 ↗
                    </button>
                  </details>
                </td>
                <td>
                  <span class="catalog-tag" :class="{ 'is-brand': host.official }">{{ host.official ? '原厂' : '第三方' }}</span>
                  <small class="catalog-block-muted">{{ host.subscription ? '订阅制' : host.isFree ? '免费渠道' : '按量 / 未声明' }}</small>
                </td>
                <td>
                  {{ hostPriceText(host, 'input') }}
                  <span class="catalog-block-muted">{{ hostPriceText(host, 'output') }}</span>
                </td>
                <td>
                  {{ hostPriceText(host, 'cacheRead') }}
                  <span class="catalog-block-muted">{{ hostPriceText(host, 'cacheWrite') }}</span>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <p v-else-if="!loading" class="catalog-empty-inline">{{ detail ? '没有符合条件的渠道。' : '渠道明细暂不可用，请重试读取。' }}</p>
        <details v-if="detail?.hosts.length" class="catalog-disclosure">
          <summary>按用量估算费用</summary>
          <p class="catalog-muted">按当前筛选渠道各自的报价计算，不拼接不同渠道的输入和输出最低价。未包含缓存折扣、阶梯价、税费和额外模式费用。</p>
          <div class="catalog-cost-inputs">
            <label>
              输入（百万 tokens）
              <input v-model.number="inputMillions" type="number" min="0" step="1" />
            </label>
            <label>
              输出（百万 tokens）
              <input v-model.number="outputMillions" type="number" min="0" step="1" />
            </label>
          </div>
          <dl class="catalog-facts">
            <div v-for="(host, index) in hosts" :key="`${host.provider}:${index}`">
              <dt>
                {{ host.name }}
                <small v-if="host.modelId">· {{ host.modelId }}</small>
              </dt>
              <dd>
                {{ estimateHostCost(host, inputMillions, outputMillions) == null ? '无法按当前报价估算' : priceText(estimateHostCost(host, inputMillions, outputMillions)) }}
              </dd>
            </div>
          </dl>
        </details>
      </template>
      <template v-else>
        <section class="catalog-section">
          <h3>模型身份与来源</h3>
          <dl class="catalog-facts">
            <div>
              <dt>模型厂商</dt>
              <dd>{{ labLabel(effectiveLab(model)) }}</dd>
            </div>
            <div>
              <dt>原始模型 ID</dt>
              <dd>
                <code>{{ model.officialModelId || '未声明' }}</code>
              </dd>
            </div>
            <div>
              <dt>标准条目 ID</dt>
              <dd>
                <code>{{ model.canonicalId || '未匹配' }}</code>
              </dd>
            </div>
            <div>
              <dt>身份来源</dt>
              <dd>{{ identitySource }}</dd>
            </div>
            <div>
              <dt>记录更新</dt>
              <dd>{{ model.lastUpdated || '未声明' }}</dd>
            </div>
          </dl>
          <div class="catalog-source-links">
            <button
              v-for="link in links"
              :key="link.url"
              type="button"
              class="catalog-text-button"
              @click="open(link.url)"
            >
              {{ link.label }}
              ↗
            </button>
            <button
              v-for="link in weights"
              :key="link.url"
              type="button"
              class="catalog-text-button"
              @click="open(link.url)"
            >
              {{ link.label }}
              ↗
            </button>
          </div>
        </section>
        <section class="catalog-section">
          <h3>评测参考</h3>
          <p class="catalog-muted">不同测试环境与模型变体可能影响结果，未提供的数据保留为空。</p>
          <dl v-if="benchmarks.length" class="catalog-facts">
            <div v-for="item in benchmarks" :key="item.label">
              <dt>{{ item.label }}</dt>
              <dd>{{ item.value }}{{ item.unit }}</dd>
            </div>
          </dl>
          <p v-else class="catalog-empty-inline">暂无 AA 评测数据。</p>
          <details v-if="extraBenchmarks.length" class="catalog-disclosure">
            <summary>其他基准测试 · {{ extraBenchmarks.length }}</summary>
            <dl class="catalog-facts">
              <div v-for="(item, index) in extraBenchmarks" :key="index">
                <dt>
                  {{ item.name }}
                  <small v-if="item.source">· {{ item.source }}</small>
                </dt>
                <dd>{{ item.score ?? '未声明' }}{{ item.metric }}</dd>
              </div>
            </dl>
          </details>
        </section>
        <section class="catalog-section">
          <h3>目录同步记录</h3>
          <dl class="catalog-facts">
            <div v-for="source in sources.filter((item) => !item.source.startsWith('rows-'))" :key="source.source">
              <dt>
                {{ source.source }}
                <small>· {{ source.recordCount.toLocaleString() }} 条</small>
              </dt>
              <dd>{{ source.fetchedAt || '时间未记录' }}</dd>
            </div>
          </dl>
          <details class="catalog-disclosure">
            <summary>查看原始详情 JSON</summary>
            <button
              type="button"
              class="catalog-button"
              :disabled="!detail"
              @click="copy(JSON.stringify(detail, null, 2), '详情 JSON')"
            >
              复制 JSON
            </button>
            <pre>{{ detail ? JSON.stringify(detail.raw, null, 2) : '详情尚未加载' }}</pre>
          </details>
        </section>
      </template>
    </div>
  </CatalogDialog>
</template>
