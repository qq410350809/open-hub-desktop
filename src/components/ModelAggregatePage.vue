<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import { systemTypeLabel } from "../types";
import type {
  ModelAggEntry,
  ModelAggGroupEntry,
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

import type { GatewayApiKeyItem } from "../types";

// —— API 密钥与本地网关维护弹窗 ——
const gatewayModalOpen = ref(false);
const draftApiKeys = ref<GatewayApiKeyItem[]>([]);

function createRandomKey(): string {
  const chars = "abcdef0123456789";
  let rand = "";
  for (let i = 0; i < 24; i++) {
    rand += chars[Math.floor(Math.random() * chars.length)];
  }
  return `sk-oh-${rand}`;
}

function openGatewayModal() {
  draftApiKeys.value = store.preferences.gatewayApiKeys.map((item) => ({ ...item }));
  if (draftApiKeys.value.length === 0 && store.preferences.gatewayApiKey.trim()) {
    draftApiKeys.value.push({
      id: "default",
      name: "默认客户端",
      key: store.preferences.gatewayApiKey.trim(),
      enabled: true,
      createdAt: Date.now(),
    });
  }
  gatewayModalOpen.value = true;
}

function closeGatewayModal() {
  gatewayModalOpen.value = false;
}

function addApiKey() {
  draftApiKeys.value.push({
    id: `key-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`,
    name: `密钥 ${draftApiKeys.value.length + 1}`,
    key: createRandomKey(),
    enabled: true,
    createdAt: Date.now(),
  });
}

function removeApiKey(index: number) {
  draftApiKeys.value.splice(index, 1);
}

function regenerateApiKey(index: number) {
  if (draftApiKeys.value[index]) {
    draftApiKeys.value[index].key = createRandomKey();
  }
}

function copySpecificKey(key: string, name: string) {
  void store.copyAddress(key, `${name || "API Key"}`);
}

async function handleSaveGatewaySettings() {
  try {
    const port = store.preferences.gatewayPort || 17896;
    const cleanKeys = draftApiKeys.value
      .map((item) => ({
        ...item,
        name: item.name.trim(),
        key: item.key.trim(),
      }))
      .filter((item) => item.key.length > 0);

    await store.updateGatewaySettings({
      port,
      apiKeys: cleanKeys,
      enabled: true,
    });
    gatewayModalOpen.value = false;
    store.showToast("API 密钥配置已保存并实时生效");
  } catch (err) {
    store.showToast(String(err), true);
  }
}

function copyGatewayUrl() {
  const url = store.gatewayStatus.value?.url || `http://127.0.0.1:${store.preferences.gatewayPort || 17896}/v1`;
  void store.copyAddress(url, "本地聚合网关 API 端点地址");
}

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
  store.showToast("模型筛选已保存并同步至本地网关");
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
  void store.loadGatewayStatus();
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

function getSiteGroupAggregatedInfo(keys: ModelAggKeyInfo[]) {
  const sortedKeys = [...keys].sort(
    (a, b) =>
      (a.accountLabel || "").localeCompare(b.accountLabel || "", undefined, {
        numeric: true,
        sensitivity: "base",
      }) || a.key.localeCompare(b.key),
  );
  const modelSet = new Set<string>();
  for (const k of sortedKeys) {
    for (const m of k.models) {
      modelSet.add(m);
    }
  }
  const maskedKeyPreview = sortedKeys
    .map((k) => {
      const masked = maskApiKey(k.key);
      return k.accountLabel ? `${masked} (${k.accountLabel})` : masked;
    })
    .join(" · ");

  return {
    keyCount: sortedKeys.length,
    maskedKeyPreview,
    modelCount: modelSet.size,
  };
}

function getGroupAggregatedInfo(entry: ModelAggGroupEntry) {
  const allKeys: ModelAggKeyInfo[] = [];
  const modelSet = new Set<string>();

  for (const site of entry.sites) {
    for (const k of site.keys) {
      allKeys.push(k);
      for (const m of k.models) {
        modelSet.add(m);
      }
    }
  }

  allKeys.sort(
    (a, b) =>
      (a.accountLabel || "").localeCompare(b.accountLabel || "", undefined, {
        numeric: true,
        sensitivity: "base",
      }) || a.key.localeCompare(b.key),
  );

  const maskedKeyPreview = allKeys
    .map((k) => {
      const masked = maskApiKey(k.key);
      return k.accountLabel ? `${masked} (${k.accountLabel})` : masked;
    })
    .join(" · ");

  return {
    keyCount: allKeys.length,
    allKeys,
    maskedKeyPreview,
    modelCount: modelSet.size,
  };
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
        <span class="modelagg-eyebrow">OpenHub · 模型与多 Key 轮询网关</span>
        <h1>模型聚合</h1>
        <p>
          聚合 {{ store.modelAggStats.value.siteCount }} 个站点 ·
          {{ store.modelAggStats.value.modelCount }} 个模型 ·
          {{ store.modelAggStats.value.groupCount }} 个分组 ·
          {{ store.modelAggStats.value.keyCount }} 个 Key
        </p>
      </div>
      <div class="modelagg-header-actions">
        <button
          class="secondary-button"
          type="button"
          title="维护与查看本地聚合网关 API 密钥与端点配置"
          @click="openGatewayModal()"
        >
          <span v-html="icons.key" />
          <span>API 密钥</span>
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
          <div class="modelagg-tree-toolbar-actions">
            <button class="text-button modelagg-expand-toggle" type="button" @click="toggleExpandAll">
              {{ store.expandedVendors.value.size === 0 ? "展开全部" : "收起全部" }}
            </button>
            <button
              class="text-button modelagg-filter-toggle"
              type="button"
              :title="`勾选要在左侧树显示的模型（已选 ${store.modelSelectionStats.value.selected} / ${store.modelSelectionStats.value.total}）`"
              @click="openPicker()"
            >
              <span v-html="icons.grid" />
              <span>模型筛选 ({{ store.modelSelectionStats.value.selected }}/{{ store.modelSelectionStats.value.total }})</span>
            </button>
          </div>
        </div>

        <div class="modelagg-tree-scroll">
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
              {{ store.selectedModelProviderCount.value }} 站提供 · {{ store.selectedModelChannelCount.value }} 个通道
            </span>
          </template>
          <template v-else>
            <span class="modelagg-list-title">请在左侧选择模型以查看通道</span>
          </template>
        </div>

        <div class="modelagg-list-scroll">
          <div v-if="store.modelAggLoading.value" class="modelagg-loading">
            <span class="spinner" />
            <strong>正在读取本地模型缓存…</strong>
          </div>

          <div
            v-else-if="store.modelAggStats.value.keyCount === 0"
            class="modelagg-empty"
          >
            <span v-html="icons.layers" />
            <strong>本地还没有站点的 Key 缓存</strong>
            <p>先到站点库同步站点的 Key 与模型，再回到这里查看聚合与调度池。</p>
            <button class="secondary-button" type="button" @click="store.openLibrary()">
              前往站点库
            </button>
          </div>

          <div
            v-else-if="!store.selectedTreeNode.value"
            class="modelagg-empty"
          >
            <span v-html="icons.sparkles" />
            <strong>请在左侧选择一个模型</strong>
            <p>选择模型后，右侧将显示提供该模型的站点、分组与具体 Key 调度通道</p>
          </div>

          <div
            v-else-if="entries.length === 0"
            class="modelagg-empty"
          >
            <span v-html="icons.search" />
            <strong>暂无站点 / Key 提供该模型</strong>
            <p>当前选中的模型在已配置的站点分组中暂无对应 Key</p>
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
                      :aria-label="`分组 ${section.group} 模式`"
                    >
                      <button
                        type="button"
                        :class="{ active: store.groupMode(section.group) === 'aggregate' }"
                        title="聚合：该分组多个 Key 合并为一个通道内部轮询"
                        @click="store.setGroupMode(section.group, 'aggregate')"
                      >聚合</button>
                      <button
                        type="button"
                        :class="{ active: store.groupMode(section.group) === 'independent' }"
                        title="独立：每个 Key 作为独立通道参与调度"
                        @click="store.setGroupMode(section.group, 'independent')"
                      >独立</button>
                    </div>
                  </div>
                  <div
                    class="modelagg-key-row"
                    :class="{ 'modelagg-key-row-merged': store.groupMode(section.group) === 'aggregate' }"
                  >
                    <div class="modelagg-merged-key-info">
                      <code class="modelagg-key-code" :title="getSiteGroupAggregatedInfo(section.keys).maskedKeyPreview">
                        {{ getSiteGroupAggregatedInfo(section.keys).maskedKeyPreview }}
                      </code>
                    </div>
                    <small class="modelagg-key-count">
                      {{ store.groupMode(section.group) === 'aggregate'
                          ? `${getSiteGroupAggregatedInfo(section.keys).keyCount} Key 聚合轮询 · ${getSiteGroupAggregatedInfo(section.keys).modelCount} 个模型`
                          : (getSiteGroupAggregatedInfo(section.keys).keyCount > 1
                              ? `${getSiteGroupAggregatedInfo(section.keys).keyCount} Key 独立通道 · ${getSiteGroupAggregatedInfo(section.keys).modelCount} 个模型`
                              : `${getSiteGroupAggregatedInfo(section.keys).modelCount} 个模型`)
                      }}
                    </small>
                  </div>
                </div>
              </div>

              <div v-else class="modelagg-groups modelagg-group-block">
                <div class="modelagg-group-head modelagg-group-block-head">
                  <span class="modelagg-group-name" :title="entry.group">{{ entry.group }}</span>
                  <div
                    class="preference-segment modelagg-mode-toggle"
                    role="group"
                    :aria-label="`分组 ${entry.group} 展示与路由模式`"
                  >
                    <button
                      type="button"
                      :class="{ active: store.groupMode(entry.group) === 'aggregate' }"
                      title="聚合：该分组下的多个 Key 合并为一个通道并在内部轮询"
                      @click="store.setGroupMode(entry.group, 'aggregate')"
                    >聚合</button>
                    <button
                      type="button"
                      :class="{ active: store.groupMode(entry.group) === 'independent' }"
                      title="独立：每个 Key 作为独立通道参与调度"
                      @click="store.setGroupMode(entry.group, 'independent')"
                    >独立</button>
                  </div>
                </div>

                <!-- 聚合与独立模式：两个 Key 均合并成一行展示，仅右侧说明不同 -->
                <div
                  class="modelagg-key-row"
                  :class="{ 'modelagg-key-row-merged': store.groupMode(entry.group) === 'aggregate' }"
                >
                  <div class="modelagg-merged-key-info">
                    <code class="modelagg-key-code" :title="getGroupAggregatedInfo(entry).maskedKeyPreview">
                      {{ getGroupAggregatedInfo(entry).maskedKeyPreview }}
                    </code>
                  </div>
                  <small class="modelagg-key-count">
                    {{ store.groupMode(entry.group) === 'aggregate'
                        ? `${getGroupAggregatedInfo(entry).keyCount} Key 聚合轮询 · ${getGroupAggregatedInfo(entry).modelCount} 个模型`
                        : (getGroupAggregatedInfo(entry).keyCount > 1
                            ? `${getGroupAggregatedInfo(entry).keyCount} Key 独立通道 · ${getGroupAggregatedInfo(entry).modelCount} 个模型`
                            : `${getGroupAggregatedInfo(entry).modelCount} 个模型`)
                    }}
                  </small>
                </div>
              </div>
            </article>
          </template>
        </div>
      </section>
    </div>

    <!-- API 密钥与网关接入维护弹窗 -->
    <Teleport to="body">
      <div
        v-if="gatewayModalOpen"
        class="modelagg-picker-backdrop"
        @click.self="closeGatewayModal()"
      >
        <section
          class="modelagg-picker modelagg-gw-modal"
          role="dialog"
          aria-modal="true"
          aria-labelledby="modelagg-gw-modal-title"
          @click.stop
        >
          <header class="modelagg-picker-head">
            <div>
              <h2 id="modelagg-gw-modal-title">API 密钥管理</h2>
              <p>统一本地 OpenAI / Claude 兼容端点，支持为不同客户端或项目分配独立 API Key 轮询调用。</p>
            </div>
            <button
              class="close-button"
              type="button"
              aria-label="关闭"
              @click="closeGatewayModal()"
              v-html="icons.close"
            />
          </header>

          <div class="modelagg-gw-body">
            <!-- 统一端点卡片 -->
            <div class="modelagg-gw-endpoint-card">
              <div class="modelagg-gw-endpoint-info">
                <span class="modelagg-gw-label">API 基础地址 (Base URL)</span>
                <code class="modelagg-gw-url-box">http://127.0.0.1:{{ store.preferences.gatewayPort || 17896 }}/v1</code>
              </div>
              <button class="secondary-button" type="button" @click="copyGatewayUrl()">
                <span v-html="icons.copy" />
                <span>复制地址</span>
              </button>
            </div>

            <!-- 密钥列表头部 -->
            <div class="modelagg-gw-keys-header">
              <div>
                <strong>API 密钥列表</strong>
                <small>（已配置 {{ draftApiKeys.length }} 个密钥，列表为空时本地免认证）</small>
              </div>
              <button class="secondary-button modelagg-gw-add-key-btn" type="button" @click="addApiKey()">
                <span v-html="icons.plus" />
                <span>添加密钥</span>
              </button>
            </div>

            <!-- 空状态 -->
            <div v-if="draftApiKeys.length === 0" class="modelagg-gw-empty-keys">
              <span v-html="icons.key" />
              <p>当前未配置任何 API 密钥，客户端填任意字符串均可免认证调用本地网关。</p>
              <button class="secondary-button" type="button" @click="addApiKey()">
                <span v-html="icons.plus" />
                <span>创建第一个密钥</span>
              </button>
            </div>

            <!-- 密钥列表 -->
            <div v-else class="modelagg-gw-keys-list">
              <div
                v-for="(item, index) in draftApiKeys"
                :key="item.id"
                class="modelagg-gw-key-card"
              >
                <div class="modelagg-gw-key-row-top">
                  <input
                    v-model="item.name"
                    class="modelagg-gw-key-name-input"
                    type="text"
                    placeholder="名称 / 备注（如 Cursor、Claude Code、Chatbox 等）"
                  />
                  <label class="modelagg-gw-key-enable">
                    <input v-model="item.enabled" type="checkbox" />
                    <span>{{ item.enabled ? '已启用' : '已停用' }}</span>
                  </label>
                </div>
                <div class="modelagg-gw-key-row-bottom">
                  <input
                    v-model="item.key"
                    class="modelagg-gw-key-input"
                    type="text"
                    placeholder="sk-oh-..."
                  />
                  <div class="modelagg-gw-key-actions">
                    <button
                      class="icon-button"
                      type="button"
                      title="复制完整 API Key"
                      @click="copySpecificKey(item.key, item.name)"
                      v-html="icons.copy"
                    />
                    <button
                      class="icon-button"
                      type="button"
                      title="重新随机生成密钥"
                      @click="regenerateApiKey(index)"
                      v-html="icons.restore"
                    />
                    <button
                      class="icon-button modelagg-gw-del-btn"
                      type="button"
                      title="删除该密钥"
                      @click="removeApiKey(index)"
                      v-html="icons.trash"
                    />
                  </div>
                </div>
              </div>
            </div>
          </div>

          <footer class="modal-footer modelagg-picker-footer">
            <button class="secondary-button" type="button" @click="closeGatewayModal()">
              取消
            </button>
            <button
              class="save-button"
              type="button"
              @click="handleSaveGatewaySettings()"
            >
              <span v-html="icons.check" />
              <span>保存配置</span>
            </button>
          </footer>
        </section>
      </div>
    </Teleport>

    <!-- 模型勾选弹窗 -->
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
                {{ pickerSelectionStats.total }} 个模型，保存后未勾选的将从左侧树及网关路由池隐藏。
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
