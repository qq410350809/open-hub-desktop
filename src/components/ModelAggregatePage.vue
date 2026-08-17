<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import { systemTypeLabel } from "../types";
import type {
  ModelAggEntry,
  ModelAggKeyInfo,
  ModelAggTreeModel,
  ModelAggTreeVendor,
} from "../composables/useModelAggregate";

const store = useStore();

// —— 模型勾选弹窗 ——
const pickerOpen = ref(false);
const pickerSearch = ref("");
const pickerSelectedKeys = ref<Set<string>>(new Set());
const pickerInitialSelectedKeys = ref<Set<string>>(new Set());

function selectedModelKeys(): Set<string> {
  const selected = new Set<string>();
  for (const vendor of store.modelAggTree.value) {
    for (const model of vendor.models) {
      if (!store.isNodeHidden(model)) selected.add(model.key);
    }
  }
  return selected;
}

function openPicker() {
  pickerSearch.value = "";
  const selected = selectedModelKeys();
  pickerSelectedKeys.value = new Set(selected);
  pickerInitialSelectedKeys.value = new Set(selected);
  pickerOpen.value = true;
}

function closePicker() {
  pickerOpen.value = false;
}

const pickerVendors = computed<ModelAggTreeVendor[]>(() => {
  const keyword = pickerSearch.value.trim().toLowerCase();
  if (!keyword) return store.modelAggTree.value;
  return store.modelAggTree.value
    .map((vendor) => ({
      vendor: vendor.vendor,
      models: vendor.models.filter(
        (model) =>
          model.label.toLowerCase().includes(keyword) ||
          model.rawIds.some((id) => id.toLowerCase().includes(keyword)) ||
          (model.canonicalKey?.toLowerCase().includes(keyword) ?? false) ||
          vendor.vendor.toLowerCase().includes(keyword),
      ),
    }))
    .filter((vendor) => vendor.models.length > 0);
});

function isModelSelected(model: ModelAggTreeModel): boolean {
  return pickerSelectedKeys.value.has(model.key);
}

const pickerSelectionStats = computed(() => ({
  selected: pickerSelectedKeys.value.size,
  total: store.modelSelectionStats.value.total,
}));

const pickerDirty = computed(() => {
  const selected = pickerSelectedKeys.value;
  const initial = pickerInitialSelectedKeys.value;
  if (selected.size !== initial.size) return true;
  for (const key of selected) {
    if (!initial.has(key)) return true;
  }
  return false;
});

function groupSelectedState(models: ModelAggTreeModel[]): {
  all: boolean;
  some: boolean;
  selectedCount: number;
} {
  let selectedCount = 0;
  for (const model of models) {
    if (isModelSelected(model)) selectedCount += 1;
  }
  return {
    all: selectedCount === models.length,
    some: selectedCount > 0,
    selectedCount,
  };
}

function toggleGroup(models: ModelAggTreeModel[], select: boolean) {
  const next = new Set(pickerSelectedKeys.value);
  for (const model of models) {
    if (select) next.add(model.key);
    else next.delete(model.key);
  }
  pickerSelectedKeys.value = next;
}

function setPickerModelSelected(model: ModelAggTreeModel, selected: boolean) {
  toggleGroup([model], selected);
}

function setAllPickerModelsSelected(selected: boolean) {
  if (!selected) {
    pickerSelectedKeys.value = new Set();
    return;
  }
  pickerSelectedKeys.value = new Set(
    store.modelAggTree.value.flatMap((vendor) => vendor.models.map((model) => model.key)),
  );
}

function selectOnlyUsedPickerModels() {
  const selected = new Set<string>();
  for (const vendor of store.modelAggTree.value) {
    for (const model of vendor.models) {
      if (store.isNodeUsed(model.rawIds)) selected.add(model.key);
    }
  }
  pickerSelectedKeys.value = selected;
}

function savePicker() {
  if (!pickerDirty.value) return;
  store.saveModelSelection(pickerSelectedKeys.value);
  pickerOpen.value = false;
  store.showToast("模型筛选已保存");
}

const dragKey = ref("");
const dropTargetKey = ref("");
const dropPlace = ref<"before" | "after">("before");

const searching = computed(() => store.modelTreeSearch.value.trim() !== "");
const entries = computed(() => store.modelAggRightEntries.value);

watch(searching, (value) => {
  if (value) store.expandVendorsForSearch();
});

onMounted(() => {
  if (!store.modelAggLoaded.value) void store.loadModelAggregation();
});

/** 一次性保存顺序和分组模式（模型筛选由弹窗单独保存）。 */
function saveChanges() {
  store.saveModelAgg();
  store.showToast("模型聚合设置已保存");
}

// 离开页面时丢弃未保存的草稿（操作只改内存，点「保存」才写盘）。
onBeforeUnmount(() => {
  if (store.modelAggDirty.value) {
    store.showToast("模型聚合有未保存的改动，已丢弃", true);
  }
});

function toggleExpandAll() {
  if (store.expandedVendors.value.size === 0) {
    store.expandedVendors.value = new Set(store.modelAggTree.value.map((item) => item.vendor));
  } else {
    store.collapseAllVendors();
  }
}

function maskApiKey(key: string): string {
  const value = key.trim();
  if (!value) return "—";
  if (value.length <= 6) return "•".repeat(6);
  const prefixLength = value.startsWith("sk-") ? 7 : 4;
  const suffixLength = Math.min(4, Math.max(2, Math.floor(value.length / 8)));
  if (value.length <= prefixLength + suffixLength) {
    return `${value.slice(0, 4)}${"•".repeat(6)}`;
  }
  return `${value.slice(0, prefixLength)}${"•".repeat(8)}${value.slice(-suffixLength)}`;
}

function copyKey(info: ModelAggKeyInfo) {
  void store.copyAddress(info.key, `${info.group} · ${info.accountLabel} 的 API Key`);
}

function modelTitle(model: ModelAggTreeModel): string {
  if (model.matched) {
    return [
      `模型库：${model.canonicalKey}`,
      `${model.providerCount} 个站点提供`,
      `原始 ID：${model.rawIds.join("、")}`,
    ].join("\n");
  }
  return `${model.rawIds[0]} · ${model.providerCount} 个站点提供`;
}

// —— 拖拽排序 ——
function onDragStart(entry: ModelAggEntry, event: DragEvent) {
  dragKey.value = entry.orderKey;
  if (event.dataTransfer) {
    event.dataTransfer.setData("text/plain", entry.orderKey);
    event.dataTransfer.effectAllowed = "move";
  }
}

function onDragOver(entry: ModelAggEntry, event: DragEvent) {
  if (!dragKey.value || dragKey.value === entry.orderKey) return;
  event.preventDefault();
  if (event.dataTransfer) event.dataTransfer.dropEffect = "move";
  const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
  dropTargetKey.value = entry.orderKey;
  dropPlace.value = event.clientY < rect.top + rect.height / 2 ? "before" : "after";
}

function onDragLeave(entry: ModelAggEntry, event: DragEvent) {
  if (dropTargetKey.value !== entry.orderKey) return;
  const related = event.relatedTarget as Node | null;
  if (related && (event.currentTarget as HTMLElement).contains(related)) return;
  dropTargetKey.value = "";
}

function onDrop(entry: ModelAggEntry) {
  if (
    dragKey.value &&
    dragKey.value !== entry.orderKey &&
    dropTargetKey.value === entry.orderKey
  ) {
    store.dropAggEntry(dragKey.value, entry.orderKey, dropPlace.value);
  }
  dragKey.value = "";
  dropTargetKey.value = "";
}

function onDragEnd() {
  dragKey.value = "";
  dropTargetKey.value = "";
}
</script>

<template>
  <main class="modelagg-page">
    <header class="modelagg-header">
      <div>
        <span class="modelagg-eyebrow">OpenHub · 跨站模型总览</span>
        <h1>模型聚合</h1>
        <p>
          聚合 {{ store.modelAggStats.value.siteCount }} 个站点 ·
          {{ store.modelAggStats.value.modelCount }} 个模型 ·
          {{ store.modelAggStats.value.groupCount }} 个分组 ·
          {{ store.modelAggStats.value.keyCount }} 个 Key
          <span v-if="store.modelAggDirty.value" class="modelagg-dirty-hint">有未保存的改动</span>
        </p>
      </div>
      <div class="modelagg-header-actions">
        <button
          class="secondary-button"
          type="button"
          :title="`勾选要在左侧树显示的模型（已选 ${store.modelSelectionStats.value.selected} / ${store.modelSelectionStats.value.total}）`"
          @click="openPicker()"
        >
          <span v-html="icons.grid" />
          <span>模型筛选 {{ store.modelSelectionStats.value.selected }}/{{ store.modelSelectionStats.value.total }}</span>
        </button>
        <button
          class="secondary-button"
          type="button"
          :disabled="store.modelAggLoading.value"
          title="重新读取本地模型缓存（仅读库，不触发网络同步）"
          @click="store.loadModelAggregation(true)"
        >
          <span
            :class="{ 'is-spinning': store.modelAggLoading.value }"
            v-html="icons.restore"
          />
          <span>{{ store.modelAggLoading.value ? "读取中…" : "刷新缓存" }}</span>
        </button>
        <button
          v-if="store.modelAggDirty.value"
          class="text-button modelagg-discard"
          type="button"
          title="放弃未保存的改动，回到上次保存的状态"
          @click="store.discardModelAggChanges()"
        >
          撤销改动
        </button>
        <button
          class="save-button"
          type="button"
          :class="{ 'is-dirty': store.modelAggDirty.value }"
          :disabled="!store.modelAggDirty.value"
          :title="store.modelAggDirty.value ? '保存条目顺序与分组模式' : '没有未保存的改动'"
          @click="saveChanges()"
        >
          <span v-html="icons.check" />
          <span>{{ store.modelAggDirty.value ? "保存改动" : "已保存" }}</span>
        </button>
      </div>
    </header>

    <div v-if="store.modelAggError.value" class="modelagg-error">
      <span v-html="icons.wifiOff" />
      <div>
        <strong>读取模型缓存失败</strong>
        <p>{{ store.modelAggError.value }}</p>
      </div>
    </div>

    <div class="modelagg-body">
      <aside class="modelagg-tree" aria-label="模型树">
        <div class="modelagg-tree-toolbar">
          <div class="modelagg-search-box">
            <span class="modelagg-search-icon" v-html="icons.search" aria-hidden="true" />
            <input
              v-model="store.modelTreeSearch.value"
              type="search"
              placeholder="搜索模型 / 厂商…"
              aria-label="搜索模型或厂商"
            />
            <button
              v-if="store.modelTreeSearch.value"
              class="modelagg-search-clear"
              type="button"
              aria-label="清空搜索"
              @click="store.modelTreeSearch.value = ''"
              v-html="icons.close"
            />
          </div>
          <button class="text-button modelagg-expand-toggle" type="button" @click="toggleExpandAll">
            {{ store.expandedVendors.value.size === 0 ? "展开全部" : "收起全部" }}
          </button>
        </div>

        <div class="modelagg-tree-scroll">
          <button
            class="modelagg-tree-all"
            :class="{ active: !store.selectedModelId.value }"
            type="button"
            @click="store.clearSelectedModel()"
          >
            <span>全部模型</span>
            <small>{{ store.filteredModelCount.value }}</small>
          </button>

          <div
            v-if="store.modelAggLoading.value && !store.modelAggLoaded.value"
            class="modelagg-tree-empty"
          >
            <span class="is-spinning" v-html="icons.restore" />
            <span>正在读取本地模型缓存…</span>
          </div>

          <template v-else-if="store.filteredModelTree.value.length">
            <div
              v-for="vendor in store.filteredModelTree.value"
              :key="vendor.vendor"
              class="modelagg-vendor-node"
            >
              <button
                class="modelagg-vendor"
                type="button"
                :aria-expanded="store.expandedVendors.value.has(vendor.vendor) || searching"
                @click="store.toggleVendor(vendor.vendor)"
              >
                <span
                  class="modelagg-vendor-chevron"
                  :class="{ expanded: store.expandedVendors.value.has(vendor.vendor) || searching }"
                  v-html="icons.chevron"
                />
                <strong>{{ vendor.vendor }}</strong>
                <small>{{ vendor.models.length }}</small>
              </button>
              <div
                v-if="store.expandedVendors.value.has(vendor.vendor) || searching"
                class="modelagg-vendor-models"
              >
                <button
                  v-for="model in vendor.models"
                  :key="model.key"
                  class="modelagg-model"
                  :class="{ active: store.selectedModelId.value === model.key, 'is-unmatched': !model.matched }"
                  type="button"
                  :title="modelTitle(model)"
                  @click="store.selectModel(model.key)"
                >
                  <span class="modelagg-model-id">{{ model.label }}</span>
                  <small class="modelagg-model-badge">{{ model.providerCount }} 站</small>
                </button>
              </div>
            </div>
          </template>

          <div v-else class="modelagg-tree-empty">
            <span v-html="icons.search" />
            <span v-if="store.modelSelectionStats.value.total > 0 && store.modelSelectionStats.value.selected === 0">
              模型已全部取消勾选
            </span>
            <span v-else>没有匹配的模型</span>
          </div>
        </div>
      </aside>

      <section class="modelagg-list" aria-label="站点列表">
        <div class="modelagg-list-head">
          <template v-if="store.selectedTreeNode.value">
            <code
              class="modelagg-selected-model"
              :title="modelTitle(store.selectedTreeNode.value)"
            >{{ store.selectedTreeNode.value.label }}</code>
            <span class="modelagg-list-count">
              {{ store.selectedModelProviderCount.value }} 站提供 · {{ entries.length }} 个条目
            </span>
            <button class="text-button" type="button" @click="store.clearSelectedModel()">
              清除筛选
            </button>
          </template>
          <template v-else>
            <span class="modelagg-list-title">全部站点概览</span>
            <span class="modelagg-list-count">{{ entries.length }} 个条目</span>
          </template>
        </div>

        <div class="modelagg-list-scroll">
          <div
            v-if="store.modelAggLoading.value && entries.length === 0"
            class="modelagg-empty"
          >
            <span class="is-spinning" v-html="icons.restore" />
            <strong>正在读取本地模型缓存…</strong>
          </div>

          <div
            v-else-if="store.modelAggStats.value.keyCount === 0"
            class="modelagg-empty"
          >
            <span v-html="icons.layers" />
            <strong>本地还没有站点的 Key 缓存</strong>
            <p>先到站点库同步站点的 Key 与模型，再回到这里查看跨站聚合。</p>
            <button class="secondary-button" type="button" @click="store.openLibrary()">
              前往站点库
            </button>
          </div>

          <div
            v-else-if="entries.length === 0 && store.selectedTreeNode.value"
            class="modelagg-empty"
          >
            <span v-html="icons.search" />
            <strong>没有 Key 提供该模型</strong>
            <p>当前模型没有任何站点分组提供，试试清除筛选查看全部站点。</p>
            <button class="secondary-button" type="button" @click="store.clearSelectedModel()">
              清除筛选
            </button>
          </div>

          <template v-else>
            <article
              v-for="(entry, index) in entries"
              :key="entry.orderKey"
              class="modelagg-entry"
              :class="{
                'is-dragging': dragKey === entry.orderKey,
                'is-drop-before': dropTargetKey === entry.orderKey && dropPlace === 'before',
                'is-drop-after': dropTargetKey === entry.orderKey && dropPlace === 'after',
              }"
              draggable="true"
              @dragstart="onDragStart(entry, $event)"
              @dragover="onDragOver(entry, $event)"
              @dragleave="onDragLeave(entry, $event)"
              @drop.prevent="onDrop(entry)"
              @dragend="onDragEnd"
            >
              <header class="modelagg-entry-head">
                <span class="modelagg-grip" aria-hidden="true" title="拖拽调整顺序" v-html="icons.grip" />
                <template v-if="entry.kind === 'site'">
                  <strong class="modelagg-entry-title">{{ entry.siteName }}</strong>
                  <span v-if="entry.systemType" class="modelagg-type-badge">
                    {{ systemTypeLabel(entry.systemType) }}
                  </span>
                </template>
                <template v-else>
                  <strong class="modelagg-entry-title modelagg-entry-title-group">
                    {{ entry.group }}
                  </strong>
                  <span class="modelagg-type-badge">{{ entry.sites.length }} 站合并</span>
                </template>
                <div class="modelagg-entry-actions">
                  <button
                    class="icon-button modelagg-move"
                    type="button"
                    aria-label="上移"
                    title="上移"
                    :disabled="index === 0"
                    @click="store.moveAggEntry(entry.orderKey, -1)"
                    v-html="icons.arrowUp"
                  />
                  <button
                    class="icon-button modelagg-move"
                    type="button"
                    aria-label="下移"
                    title="下移"
                    :disabled="index === entries.length - 1"
                    @click="store.moveAggEntry(entry.orderKey, 1)"
                    v-html="icons.chevron"
                  />
                </div>
              </header>

              <div v-if="entry.kind === 'site'" class="modelagg-groups">
                <div v-for="section in entry.groups" :key="section.group" class="modelagg-group">
                  <div class="modelagg-group-head">
                    <span class="modelagg-group-name" :title="section.group">{{ section.group }}</span>
                    <div
                      class="preference-segment modelagg-mode-toggle"
                      role="group"
                      :aria-label="`分组 ${section.group} 展示模式`"
                    >
                      <button
                        type="button"
                        :class="{ active: store.groupMode(section.group) === 'aggregate' }"
                        title="把各站点中同名分组的 Key 合并成块展示"
                        @click="store.setGroupMode(section.group, 'aggregate')"
                      >聚合</button>
                      <button
                        type="button"
                        :class="{ active: store.groupMode(section.group) === 'independent' }"
                        title="该分组在各站点卡片内单独展示"
                        @click="store.setGroupMode(section.group, 'independent')"
                      >独立</button>
                    </div>
                  </div>
                  <div v-for="info in section.keys" :key="`${info.accountLabel}:${info.key}`" class="modelagg-key-row">
                    <code class="modelagg-key-code">{{ maskApiKey(info.key) }}</code>
                    <small class="modelagg-key-account" :title="info.accountLabel">{{ info.accountLabel }}</small>
                    <small class="modelagg-key-count">{{ info.models.length }} 个模型</small>
                    <button
                      class="modelagg-key-copy"
                      type="button"
                      aria-label="复制完整 API Key"
                      title="复制完整 API Key"
                      @click.stop="copyKey(info)"
                      v-html="icons.copy"
                    />
                  </div>
                </div>
              </div>

              <div v-else class="modelagg-groups modelagg-group-block">
                <div class="modelagg-group-head modelagg-group-block-head">
                  <span class="modelagg-group-hint">同名分组已跨站合并</span>
                  <div
                    class="preference-segment modelagg-mode-toggle"
                    role="group"
                    :aria-label="`分组 ${entry.group} 展示模式`"
                  >
                    <button
                      type="button"
                      :class="{ active: store.groupMode(entry.group) === 'aggregate' }"
                      @click="store.setGroupMode(entry.group, 'aggregate')"
                    >聚合</button>
                    <button
                      type="button"
                      :class="{ active: store.groupMode(entry.group) === 'independent' }"
                      title="该分组回到各站点卡片内单独展示"
                      @click="store.setGroupMode(entry.group, 'independent')"
                    >独立</button>
                  </div>
                </div>
                <div v-for="site in entry.sites" :key="site.siteId" class="modelagg-group-site">
                  <div class="modelagg-group-site-head">
                    <strong>{{ site.siteName }}</strong>
                    <span v-if="site.systemType" class="modelagg-type-badge">
                      {{ systemTypeLabel(site.systemType) }}
                    </span>
                  </div>
                  <div v-for="info in site.keys" :key="`${info.accountLabel}:${info.key}`" class="modelagg-key-row">
                    <code class="modelagg-key-code">{{ maskApiKey(info.key) }}</code>
                    <small class="modelagg-key-account" :title="info.accountLabel">{{ info.accountLabel }}</small>
                    <small class="modelagg-key-count">{{ info.models.length }} 个模型</small>
                    <button
                      class="modelagg-key-copy"
                      type="button"
                      aria-label="复制完整 API Key"
                      title="复制完整 API Key"
                      @click.stop="copyKey(info)"
                      v-html="icons.copy"
                    />
                  </div>
                </div>
              </div>
            </article>
          </template>
        </div>
      </section>
    </div>

    <Teleport to="body">
      <div
        v-if="pickerOpen"
        class="modelagg-picker-backdrop"
        @click.self="closePicker()"
      >
        <section
          class="modelagg-picker"
          role="dialog"
          aria-modal="true"
          aria-labelledby="modelagg-picker-title"
          @click.stop
        >
          <header class="modelagg-picker-head">
            <div>
              <h2 id="modelagg-picker-title">筛选模型</h2>
              <p>
                已选 {{ pickerSelectionStats.selected }} /
                {{ pickerSelectionStats.total }} 个模型，保存后未勾选的将从左侧树隐藏。
              </p>
            </div>
            <button
              class="close-button"
              type="button"
              aria-label="关闭"
              @click="closePicker()"
              v-html="icons.close"
            />
          </header>

          <div class="modelagg-picker-toolbar">
            <div class="modelagg-search-box modelagg-picker-search">
              <span class="modelagg-search-icon" v-html="icons.search" aria-hidden="true" />
              <input
                v-model="pickerSearch"
                type="search"
                placeholder="搜索模型 / 厂商…"
                aria-label="搜索模型或厂商"
              />
            </div>
            <div class="modelagg-picker-actions">
              <button class="text-button" type="button" @click="setAllPickerModelsSelected(true)">
                全选
              </button>
              <button class="text-button" type="button" @click="setAllPickerModelsSelected(false)">
                全不选
              </button>
              <button
                class="text-button"
                type="button"
                :disabled="store.usedModelNames.value.size === 0"
                title="只保留 Token 统计里实际用过的模型"
                @click="selectOnlyUsedPickerModels()"
              >
                仅用过的
              </button>
            </div>
          </div>

          <div class="modelagg-picker-body">
            <div v-if="pickerVendors.length === 0" class="modelagg-tree-empty">
              <span v-html="icons.search" />
              <span>没有匹配的模型</span>
            </div>
            <template v-else>
              <div
                v-for="vendor in pickerVendors"
                :key="vendor.vendor"
                class="modelagg-picker-group"
              >
                <label class="modelagg-picker-vendor">
                  <input
                    type="checkbox"
                    :checked="groupSelectedState(vendor.models).all"
                    :indeterminate="!groupSelectedState(vendor.models).all && groupSelectedState(vendor.models).some"
                    @change="toggleGroup(vendor.models, ($event.target as HTMLInputElement).checked)"
                  />
                  <strong>{{ vendor.vendor }}</strong>
                  <small>{{ groupSelectedState(vendor.models).selectedCount }}/{{ vendor.models.length }}</small>
                </label>
                <label
                  v-for="model in vendor.models"
                  :key="model.key"
                  class="modelagg-picker-row"
                >
                  <input
                    type="checkbox"
                    :checked="isModelSelected(model)"
                    @change="setPickerModelSelected(model, ($event.target as HTMLInputElement).checked)"
                  />
                  <span class="modelagg-picker-name" :title="modelTitle(model)">{{ model.label }}</span>
                  <small class="modelagg-picker-count">{{ model.providerCount }} 站</small>
                </label>
              </div>
            </template>
          </div>
          <footer class="modal-footer modelagg-picker-footer">
            <button class="secondary-button" type="button" @click="closePicker()">
              取消
            </button>
            <button
              class="save-button"
              type="button"
              :disabled="!pickerDirty"
              :title="pickerDirty ? '一次性保存模型筛选' : '筛选没有变化'"
              @click="savePicker()"
            >
              <span v-html="icons.check" />
              <span>保存</span>
            </button>
          </footer>
        </section>
      </div>
    </Teleport>
  </main>
</template>
