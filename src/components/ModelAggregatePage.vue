<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import { systemTypeLabel } from "../types";
import type { GatewayApiKeyItem } from "../types";
import type {
  ModelAggEntry,
  ModelAggGroupEntry,
  ModelAggKeyInfo,
  ModelAggTreeModel,
  ModelAggTreeVendor,
} from "../composables/useModelAggregate";

const store = useStore();

// —— 复制反馈状态 ——
const copyFeedback = ref(false);

// —— 模型勾选弹窗 ——
const pickerOpen = ref(false);
const pickerSearch = ref("");
const pickerSelectedKeys = ref<Set<string>>(new Set());
const pickerInitialSelectedKeys = ref<Set<string>>(new Set());

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
  document.body.classList.add("modal-open");
}

function closeGatewayModal() {
  gatewayModalOpen.value = false;
  document.body.classList.remove("modal-open");
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
    closeGatewayModal();
    store.showToast("API 密钥配置已保存并实时生效");
  } catch (err) {
    store.showToast(String(err), true);
  }
}

function copyGatewayUrl() {
  const url = store.gatewayStatus.value?.url || `http://127.0.0.1:${store.preferences.gatewayPort || 17896}/v1`;
  void store.copyAddress(url, "本地聚合网关 API 端点地址");
  copyFeedback.value = true;
  setTimeout(() => {
    copyFeedback.value = false;
  }, 2000);
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
  document.body.classList.add("modal-open");
}

function closePicker() {
  pickerOpen.value = false;
  document.body.classList.remove("modal-open");
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
  closePicker();
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
  <main class="modelagg-page ma-dashboard">
    <!-- 顶部宏观智控驾驶舱 (Cockpit Bar) -->
    <header class="ma-cockpit-bar">
      <div class="ma-cockpit-left">
        <div class="ma-brand-section">
          <div class="ma-eyebrow-row">
            <span class="ma-live-dot" />
            <span class="ma-eyebrow-text">多密钥模型聚合网关</span>
            <span class="ma-eyebrow-badge">本地智能网关 · 模型聚合</span>
          </div>
          <div class="ma-title-row">
            <h1>模型聚合网关</h1>
          </div>
          <p class="ma-cockpit-subtitle">
            聚合 <strong>{{ store.modelAggStats.value.siteCount }}</strong> 个站点 ·
            <strong>{{ store.modelAggStats.value.modelCount }}</strong> 款模型 ·
            <strong>{{ store.modelAggStats.value.groupCount }}</strong> 个分组 ·
            <strong>{{ store.modelAggStats.value.keyCount }}</strong> 个 Key 调度通道
          </p>
        </div>
      </div>

      <div class="ma-cockpit-right">
        <button
          type="button"
          class="ma-btn-secondary"
          :class="{ 'is-copied': copyFeedback }"
          title="复制本地聚合网关 API 端点地址"
          @click="copyGatewayUrl"
        >
          <span v-html="copyFeedback ? icons.check : icons.link" />
          <span>{{ copyFeedback ? "已复制端点" : "复制网关端点" }}</span>
        </button>

        <button
          type="button"
          class="ma-btn-secondary"
          title="维护与查看本地聚合网关 API 密钥与端点配置"
          @click="openGatewayModal()"
        >
          <span v-html="icons.key" />
          <span>API 密钥</span>
          <span class="ma-count-chip">{{ store.preferences.gatewayApiKeys.length }}</span>
        </button>

        <button
          type="button"
          class="ma-btn-primary"
          title="勾选要在左侧树及本地网关路由池显示的模型"
          @click="openPicker()"
        >
          <span v-html="icons.grid" />
          <span>模型筛选 ({{ store.modelSelectionStats.value.selected }}/{{ store.modelSelectionStats.value.total }})</span>
        </button>
      </div>
    </header>

    <!-- 状态反馈横幅 -->
    <div v-if="store.modelAggError.value" class="ma-error-banner" role="alert">
      <span class="ma-error-icon" v-html="icons.wifiOff" />
      <div class="ma-error-content">
        <strong>读取模型缓存失败</strong>
        <p>{{ store.modelAggError.value }}</p>
      </div>
    </div>

    <!-- 4 大核心 KPI Bento 指标卡 (Stats Deck) -->
    <section class="ma-stats-deck" aria-label="模型聚合核心指标概览">
      <!-- 卡片 1: 聚合模型总数 -->
      <div class="ma-stat-card">
        <div class="ma-stat-header">
          <span class="ma-stat-tag is-blue">
            <span v-html="icons.sparkles" />
            <span>活跃模型</span>
          </span>
          <span class="ma-stat-pill is-blue">聚合模型</span>
        </div>
        <div class="ma-stat-main">
          <strong>{{ store.modelAggStats.value.modelCount }}</strong>
          <span class="ma-stat-unit">款模型</span>
        </div>
        <div class="ma-stat-footer">
          <span>已启用 <strong>{{ store.modelSelectionStats.value.selected }}</strong> / {{ store.modelSelectionStats.value.total }} 款模型</span>
        </div>
      </div>

      <!-- 卡片 2: 接入站点网络 -->
      <div class="ma-stat-card">
        <div class="ma-stat-header">
          <span class="ma-stat-tag is-emerald">
            <span v-html="icons.database" />
            <span>已连接站点</span>
          </span>
          <span class="ma-stat-pill is-emerald">多站点</span>
        </div>
        <div class="ma-stat-main">
          <strong>{{ store.modelAggStats.value.siteCount }}</strong>
          <span class="ma-stat-unit">个接入站点</span>
        </div>
        <div class="ma-stat-footer">
          <span>涵盖 <strong>{{ store.modelAggStats.value.groupCount }}</strong> 个业务与渠道分组</span>
        </div>
      </div>

      <!-- 卡片 3: 调度池 Key 矩阵 -->
      <div class="ma-stat-card">
        <div class="ma-stat-header">
          <span class="ma-stat-tag is-purple">
            <span v-html="icons.key" />
            <span>路由密钥</span>
          </span>
          <span class="ma-stat-pill is-purple">通道矩阵</span>
        </div>
        <div class="ma-stat-main">
          <strong>{{ store.modelAggStats.value.keyCount }}</strong>
          <span class="ma-stat-unit">个 Key 出口</span>
        </div>
        <div class="ma-stat-footer">
          <span>支持多 Key 聚合轮询与故障平滑切换</span>
        </div>
      </div>

      <!-- 卡片 4: 本地网关端点 -->
      <div class="ma-stat-card">
        <div class="ma-stat-header">
          <span class="ma-stat-tag is-orange">
            <span v-html="icons.activity" />
            <span>本地网关</span>
          </span>
          <span class="ma-stat-pill is-orange">兼容网关</span>
        </div>
        <div class="ma-stat-main">
          <strong>:{{ store.preferences.gatewayPort || 17896 }}</strong>
          <span class="ma-stat-unit">端口监听</span>
        </div>
        <div class="ma-stat-footer">
          <span>OpenAI / Claude API 统一代理路由</span>
        </div>
      </div>
    </section>

    <!-- 左右分屏主工作区 (Dual-Pane Master-Detail Workspace) -->
    <div class="ma-workspace">
      <!-- 左栏：模型目录树 (Model Tree Catalog) -->
      <aside class="ma-tree-pane" aria-label="模型树">
        <div class="ma-tree-toolbar">
          <div class="ma-search-box">
            <span class="ma-search-icon" v-html="icons.search" />
            <input
              v-model="store.modelTreeSearch.value"
              class="ma-search-input"
              type="search"
              placeholder="搜索模型 / 厂商…"
              aria-label="搜索模型或厂商"
            />
            <button
              v-if="store.modelTreeSearch.value"
              type="button"
              class="ma-search-clear"
              aria-label="清空搜索"
              @click="store.modelTreeSearch.value = ''"
              v-html="icons.close"
            />
          </div>
          <div class="ma-tree-actions">
            <button type="button" class="ma-text-btn" @click="toggleExpandAll">
              {{ store.expandedVendors.value.size === 0 ? "展开全部" : "收起全部" }}
            </button>
            <button type="button" class="ma-text-btn" @click="openPicker()">
              筛选 ({{ store.modelSelectionStats.value.selected }})
            </button>
          </div>
        </div>

        <div class="ma-tree-scroll">
          <div
            v-if="store.modelAggLoading.value && !store.modelAggLoaded.value"
            class="ma-tree-empty"
          >
            <span class="ma-mini-spinner" />
            <span>正在读取本地模型缓存…</span>
          </div>

          <template v-else-if="store.filteredModelTree.value.length">
            <div
              v-for="vendor in store.filteredModelTree.value"
              :key="vendor.vendor"
              class="ma-vendor-group"
            >
              <button
                type="button"
                class="ma-vendor-header"
                :class="{ 'is-expanded': store.expandedVendors.value.has(vendor.vendor) || searching }"
                @click="store.toggleVendor(vendor.vendor)"
              >
                <span
                  class="ma-vendor-chevron"
                  :class="{ 'is-expanded': store.expandedVendors.value.has(vendor.vendor) || searching }"
                >
                  ▼
                </span>
                <strong class="ma-vendor-name">{{ vendor.vendor }}</strong>
                <span class="ma-vendor-count">{{ vendor.models.length }}</span>
              </button>

              <div
                v-if="store.expandedVendors.value.has(vendor.vendor) || searching"
                class="ma-vendor-models-list"
              >
                <button
                  v-for="model in vendor.models"
                  :key="model.key"
                  type="button"
                  class="ma-model-item-btn"
                  :class="{
                    'is-active': store.selectedModelId.value === model.key,
                    'is-unmatched': !model.matched,
                  }"
                  :title="modelTitle(model)"
                  @click="store.selectModel(model.key)"
                >
                  <span class="ma-model-label">{{ model.label }}</span>
                  <span class="ma-model-badge">{{ model.providerCount }} 站</span>
                </button>
              </div>
            </div>
          </template>

          <div v-else class="ma-tree-empty">
            <span v-html="icons.search" />
            <span v-if="store.modelSelectionStats.value.total > 0 && store.modelSelectionStats.value.selected === 0">
              模型已全部取消勾选
            </span>
            <span v-else>没有匹配的模型</span>
          </div>
        </div>
      </aside>

      <!-- 右栏：通道优先级与调度阵列 (Priority & Routing Deck) -->
      <section class="ma-list-pane" aria-label="站点与通道优先级调度">
        <!-- 头部选中模型指示条 -->
        <div class="ma-list-header">
          <template v-if="store.selectedTreeNode.value">
            <div class="ma-selected-model-pill">
              <span class="ma-model-icon" v-html="icons.sparkles" />
              <code class="ma-selected-code" :title="modelTitle(store.selectedTreeNode.value)">
                {{ store.selectedTreeNode.value.label }}
              </code>
            </div>
            <span class="ma-list-meta-count">
              <strong>{{ store.selectedModelProviderCount.value }}</strong> 个站点提供 ·
              <strong>{{ store.selectedModelChannelCount.value }}</strong> 个调度通道
            </span>
          </template>
          <template v-else>
            <span class="ma-list-placeholder">请在左侧模型目录中选择模型以查看调度通道</span>
          </template>
        </div>

        <div class="ma-list-scroll">
          <div v-if="store.modelAggLoading.value" class="ma-loading-state">
            <span class="ma-spinner" />
            <strong>正在读取本地模型缓存…</strong>
          </div>

          <div
            v-else-if="store.modelAggStats.value.keyCount === 0"
            class="ma-empty-state"
          >
            <span class="ma-empty-icon" v-html="icons.layers" />
            <strong>本地还没有站点的 Key 缓存</strong>
            <p>先前往站点库同步站点的 Key 与可用模型，再回到这里查看聚合与调度池。</p>
            <button type="button" class="ma-btn-secondary" @click="store.openLibrary()">
              前往站点库
            </button>
          </div>

          <div
            v-else-if="!store.selectedTreeNode.value"
            class="ma-empty-state"
          >
            <span class="ma-empty-icon" v-html="icons.sparkles" />
            <strong>请在左侧选择一个模型</strong>
            <p>选择模型后，右侧将显示提供该模型的站点、分组与具体 Key 调度通道，支持拖拽调整负载与优先级</p>
          </div>

          <div
            v-else-if="entries.length === 0"
            class="ma-empty-state"
          >
            <span class="ma-empty-icon" v-html="icons.search" />
            <strong>暂无站点 / Key 提供该模型</strong>
            <p>当前选中的模型在已配置的站点分组中暂无可用的 Key 出口</p>
          </div>

          <!-- 可拖拽调序卡片列表 -->
          <div v-else class="ma-entries-list">
            <article
              v-for="(entry, index) in entries"
              :key="entry.orderKey"
              class="ma-entry-card"
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
              <header class="ma-entry-head">
                <div class="ma-entry-left">
                  <span class="ma-grip-handle" aria-hidden="true" title="按住拖拽调整路由优先级" v-html="icons.grip" />
                  <span class="ma-priority-index">#{{ index + 1 }}</span>
                  <template v-if="entry.kind === 'site'">
                    <strong class="ma-entry-title">{{ entry.siteName }}</strong>
                    <span v-if="entry.systemType" class="ma-system-badge">
                      {{ systemTypeLabel(entry.systemType) }}
                    </span>
                  </template>
                  <template v-else>
                    <strong class="ma-entry-title is-group">
                      {{ entry.group }}
                    </strong>
                  </template>
                </div>

                <div class="ma-entry-actions">
                  <button
                    type="button"
                    class="ma-move-btn"
                    aria-label="上移优先级"
                    title="上移优先级"
                    :disabled="index === 0"
                    @click="store.moveAggEntry(entry.orderKey, -1)"
                    v-html="icons.arrowUp"
                  />
                  <button
                    type="button"
                    class="ma-move-btn"
                    aria-label="下移优先级"
                    title="下移优先级"
                    :disabled="index === entries.length - 1"
                    @click="store.moveAggEntry(entry.orderKey, 1)"
                    v-html="icons.chevron"
                  />
                </div>
              </header>

              <!-- 站点模式下的分组列表 -->
              <div v-if="entry.kind === 'site'" class="ma-groups-container">
                <div v-for="section in entry.groups" :key="section.group" class="ma-group-box">
                  <div class="ma-group-head">
                    <span class="ma-group-name" :title="section.group">{{ section.group }}</span>
                    <div
                      class="ma-mode-segment"
                      role="group"
                      :aria-label="`分组 ${section.group} 模式`"
                    >
                      <button
                        type="button"
                        class="ma-mode-btn"
                        :class="{ active: store.groupMode(section.group) === 'aggregate' }"
                        title="聚合：该分组多个 Key 合并为一个通道内部轮询"
                        @click="store.setGroupMode(section.group, 'aggregate')"
                      >
                        聚合轮询
                      </button>
                      <button
                        type="button"
                        class="ma-mode-btn"
                        :class="{ active: store.groupMode(section.group) === 'independent' }"
                        title="独立：每个 Key 作为独立通道参与调度"
                        @click="store.setGroupMode(section.group, 'independent')"
                      >
                        独立通道
                      </button>
                    </div>
                  </div>

                  <div
                    class="ma-key-row"
                    :class="{ 'is-merged': store.groupMode(section.group) === 'aggregate' }"
                  >
                    <div class="ma-key-info">
                      <code class="ma-key-code" :title="getSiteGroupAggregatedInfo(section.keys).maskedKeyPreview">
                        {{ getSiteGroupAggregatedInfo(section.keys).maskedKeyPreview }}
                      </code>
                    </div>
                    <span class="ma-key-desc">
                      {{
                        store.groupMode(section.group) === "aggregate"
                          ? `${getSiteGroupAggregatedInfo(section.keys).keyCount} Key 聚合轮询 · ${getSiteGroupAggregatedInfo(section.keys).modelCount} 模型`
                          : getSiteGroupAggregatedInfo(section.keys).keyCount > 1
                            ? `${getSiteGroupAggregatedInfo(section.keys).keyCount} Key 独立通道 · ${getSiteGroupAggregatedInfo(section.keys).modelCount} 模型`
                            : `${getSiteGroupAggregatedInfo(section.keys).modelCount} 款模型`
                      }}
                    </span>
                  </div>
                </div>
              </div>

              <!-- 跨站点分组模式 -->
              <div v-else class="ma-groups-container">
                <div class="ma-group-box">
                  <div class="ma-group-head">
                    <span class="ma-group-name" :title="entry.group">{{ entry.group }}</span>
                    <div
                      class="ma-mode-segment"
                      role="group"
                      :aria-label="`分组 ${entry.group} 模式`"
                    >
                      <button
                        type="button"
                        class="ma-mode-btn"
                        :class="{ active: store.groupMode(entry.group) === 'aggregate' }"
                        title="聚合：该分组下的多个 Key 合并为一个通道并在内部轮询"
                        @click="store.setGroupMode(entry.group, 'aggregate')"
                      >
                        聚合轮询
                      </button>
                      <button
                        type="button"
                        class="ma-mode-btn"
                        :class="{ active: store.groupMode(entry.group) === 'independent' }"
                        title="独立：每个 Key 作为独立通道参与调度"
                        @click="store.setGroupMode(entry.group, 'independent')"
                      >
                        独立通道
                      </button>
                    </div>
                  </div>

                  <div
                    class="ma-key-row"
                    :class="{ 'is-merged': store.groupMode(entry.group) === 'aggregate' }"
                  >
                    <div class="ma-key-info">
                      <code class="ma-key-code" :title="getGroupAggregatedInfo(entry).maskedKeyPreview">
                        {{ getGroupAggregatedInfo(entry).maskedKeyPreview }}
                      </code>
                    </div>
                    <span class="ma-key-desc">
                      {{
                        store.groupMode(entry.group) === "aggregate"
                          ? `${getGroupAggregatedInfo(entry).keyCount} Key 聚合轮询 · ${getGroupAggregatedInfo(entry).modelCount} 模型`
                          : getGroupAggregatedInfo(entry).keyCount > 1
                            ? `${getGroupAggregatedInfo(entry).keyCount} Key 独立通道 · ${getGroupAggregatedInfo(entry).modelCount} 模型`
                            : `${getGroupAggregatedInfo(entry).modelCount} 款模型`
                      }}
                    </span>
                  </div>
                </div>
              </div>
            </article>
          </div>
        </div>
      </section>
    </div>

    <!-- ============================================================
         两大独立全功能弹窗体系 (Dedicated Modals)
         ============================================================ -->

    <!-- 1. API 密钥与网关接入维护弹窗 (Gateway Modal) -->
    <Teleport to="body">
      <Transition name="ma-modal-fade">
        <div
          v-if="gatewayModalOpen"
          class="ma-modal-backdrop"
          @click.self="closeGatewayModal()"
        >
          <section
            class="ma-modal-card is-gateway"
            role="dialog"
            aria-modal="true"
            aria-labelledby="ma-gw-title"
            @click.stop
          >
            <header class="ma-modal-header">
              <div class="ma-modal-title-group">
                <div class="ma-modal-eyebrow">OpenAI / Claude 兼容网关</div>
                <h2 id="ma-gw-title">API 密钥与本地网关管理</h2>
                <p>统一本地 OpenAI / Claude 兼容端点，支持为不同客户端（Cursor, Claude Code, Chatbox 等）分配独立 API Key。</p>
              </div>
              <button type="button" class="ma-modal-close-btn" aria-label="关闭" @click="closeGatewayModal()">×</button>
            </header>

            <div class="ma-modal-body">
              <!-- 统一端点卡片 -->
              <div class="ma-gw-endpoint-card">
                <div class="ma-gw-endpoint-info">
                  <span class="ma-gw-label">本地网关基础接口地址</span>
                  <code class="ma-gw-url-box">http://127.0.0.1:{{ store.preferences.gatewayPort || 17896 }}/v1</code>
                </div>
                <button type="button" class="ma-btn-secondary ma-btn-sm" @click="copyGatewayUrl()">
                  <span v-html="icons.copy" />
                  <span>复制地址</span>
                </button>
              </div>

              <!-- 密钥列表头部 -->
              <div class="ma-gw-keys-header">
                <div>
                  <strong>客户端 API 密钥列表</strong>
                  <small>（已配置 {{ draftApiKeys.length }} 个密钥，列表为空时本地免认证访问）</small>
                </div>
                <button type="button" class="ma-btn-secondary ma-btn-sm" @click="addApiKey()">
                  <span v-html="icons.plus" />
                  <span>添加密钥</span>
                </button>
              </div>

              <!-- 密钥空状态 -->
              <div v-if="draftApiKeys.length === 0" class="ma-gw-empty-keys">
                <span v-html="icons.key" />
                <p>当前未配置任何 API 密钥，客户端填任意字符串均可免认证调用本地网关。</p>
                <button type="button" class="ma-btn-secondary" @click="addApiKey()">
                  <span v-html="icons.plus" />
                  <span>创建第一个密钥</span>
                </button>
              </div>

              <!-- 密钥列表卡片 -->
              <div v-else class="ma-gw-keys-list">
                <div
                  v-for="(item, index) in draftApiKeys"
                  :key="item.id"
                  class="ma-gw-key-card"
                >
                  <div class="ma-gw-key-row-top">
                    <input
                      v-model="item.name"
                      class="ma-input ma-gw-key-name"
                      type="text"
                      placeholder="客户端备注名称（如 Cursor、Claude Code、Chatbox 等）"
                    />
                    <label class="ma-gw-key-enable">
                      <input v-model="item.enabled" type="checkbox" />
                      <span>{{ item.enabled ? "已启用" : "已停用" }}</span>
                    </label>
                  </div>
                  <div class="ma-gw-key-row-bottom">
                    <input
                      v-model="item.key"
                      class="ma-input ma-gw-key-token"
                      type="text"
                      placeholder="sk-oh-..."
                    />
                    <div class="ma-gw-key-actions">
                      <button
                        type="button"
                        class="ma-icon-btn"
                        title="复制完整 API Key"
                        @click="copySpecificKey(item.key, item.name)"
                        v-html="icons.copy"
                      />
                      <button
                        type="button"
                        class="ma-icon-btn"
                        title="重新随机生成密钥"
                        @click="regenerateApiKey(index)"
                        v-html="icons.restore"
                      />
                      <button
                        type="button"
                        class="ma-icon-btn is-danger"
                        title="删除该密钥"
                        @click="removeApiKey(index)"
                        v-html="icons.trash"
                      />
                    </div>
                  </div>
                </div>
              </div>
            </div>

            <footer class="ma-modal-footer">
              <button type="button" class="ma-btn-cancel" @click="closeGatewayModal()">取消</button>
              <button type="button" class="ma-btn-primary" @click="handleSaveGatewaySettings()">
                <span v-html="icons.check" />
                <span>保存配置</span>
              </button>
            </footer>
          </section>
        </div>
      </Transition>
    </Teleport>

    <!-- 2. 模型勾选与路由筛选弹窗 (Picker Modal) -->
    <Teleport to="body">
      <Transition name="ma-modal-fade">
        <div
          v-if="pickerOpen"
          class="ma-modal-backdrop"
          @click.self="closePicker()"
        >
          <section
            class="ma-modal-card is-picker"
            role="dialog"
            aria-modal="true"
            aria-labelledby="ma-picker-title"
            @click.stop
          >
            <header class="ma-modal-header">
              <div class="ma-modal-title-group">
                <div class="ma-modal-eyebrow">模型可见性与路由过滤</div>
                <h2 id="ma-picker-title">筛选展示与路由模型</h2>
                <p>
                  已选 {{ pickerSelectionStats.selected }} /
                  {{ pickerSelectionStats.total }} 款模型，保存后未勾选的模型将从左侧树及网关路由池隐藏。
                </p>
              </div>
              <button type="button" class="ma-modal-close-btn" aria-label="关闭" @click="closePicker()">×</button>
            </header>

            <div class="ma-picker-toolbar">
              <div class="ma-search-box ma-picker-search">
                <span class="ma-search-icon" v-html="icons.search" />
                <input
                  v-model="pickerSearch"
                  class="ma-search-input"
                  type="search"
                  placeholder="搜索模型 / 厂商…"
                  aria-label="搜索模型或厂商"
                />
              </div>
              <div class="ma-picker-actions">
                <button type="button" class="ma-btn-secondary ma-btn-sm" @click="setAllPickerModelsSelected(true)">
                  全选
                </button>
                <button type="button" class="ma-btn-secondary ma-btn-sm" @click="setAllPickerModelsSelected(false)">
                  全不选
                </button>
                <button
                  type="button"
                  class="ma-btn-secondary ma-btn-sm"
                  :disabled="store.usedModelNames.value.size === 0"
                  title="只保留 Token 统计里实际用过的模型"
                  @click="selectOnlyUsedPickerModels()"
                >
                  仅用过的
                </button>
              </div>
            </div>

            <div class="ma-modal-body ma-picker-body">
              <div v-if="pickerVendors.length === 0" class="ma-tree-empty">
                <span v-html="icons.search" />
                <span>没有匹配的模型</span>
              </div>
              <template v-else>
                <div
                  v-for="vendor in pickerVendors"
                  :key="vendor.vendor"
                  class="ma-picker-vendor-group"
                >
                  <label class="ma-picker-vendor-head">
                    <input
                      type="checkbox"
                      :checked="groupSelectedState(vendor.models).all"
                      :indeterminate="!groupSelectedState(vendor.models).all && groupSelectedState(vendor.models).some"
                      @change="toggleGroup(vendor.models, ($event.target as HTMLInputElement).checked)"
                    />
                    <strong>{{ vendor.vendor }}</strong>
                    <small>{{ groupSelectedState(vendor.models).selectedCount }}/{{ vendor.models.length }}</small>
                  </label>
                  <div class="ma-picker-models-grid">
                    <label
                      v-for="model in vendor.models"
                      :key="model.key"
                      class="ma-picker-model-row"
                    >
                      <input
                        type="checkbox"
                        :checked="isModelSelected(model)"
                        @change="setPickerModelSelected(model, ($event.target as HTMLInputElement).checked)"
                      />
                      <span class="ma-picker-model-name" :title="modelTitle(model)">{{ model.label }}</span>
                      <small class="ma-picker-model-count">{{ model.providerCount }} 站</small>
                    </label>
                  </div>
                </div>
              </template>
            </div>

            <footer class="ma-modal-footer">
              <button type="button" class="ma-btn-cancel" @click="closePicker()">
                取消
              </button>
              <button
                type="button"
                class="ma-btn-primary"
                :disabled="!pickerDirty"
                :title="pickerDirty ? '一次性保存模型筛选' : '筛选没有变化'"
                @click="savePicker()"
              >
                <span v-html="icons.check" />
                <span>保存筛选</span>
              </button>
            </footer>
          </section>
        </div>
      </Transition>
    </Teleport>
  </main>
</template>

<style scoped>
.ma-dashboard {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  background: var(--page-bg);
  color: var(--text);
  overflow: hidden;
}

/* ============================================================
   1. 顶部全景智控驾驶舱 (Cockpit Bar)
   ============================================================ */
.ma-cockpit-bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 12px 20px;
  background: var(--surface);
  border-bottom: 1px solid var(--line);
  flex-shrink: 0;
}

.ma-cockpit-left {
  display: flex;
  align-items: center;
  gap: 12px;
}

.ma-brand-section {
  display: flex;
  flex-direction: column;
  gap: 1px;
}

.ma-eyebrow-row {
  display: flex;
  align-items: center;
  gap: 6px;
}

.ma-live-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: #10b981;
  box-shadow: 0 0 8px #10b981;
  animation: maPulse 2s infinite ease-in-out;
}

@keyframes maPulse {
  0%, 100% { opacity: 1; transform: scale(1); }
  50% { opacity: 0.4; transform: scale(1.25); }
}

.ma-eyebrow-text {
  font-size: 10px;
  font-weight: 750;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--brand);
}

.ma-eyebrow-badge {
  padding: 1px 6px;
  border-radius: var(--r-full);
  background: color-mix(in srgb, var(--brand) 12%, transparent);
  color: var(--brand);
  font-size: 9.5px;
  font-weight: 700;
}

.ma-title-row h1 {
  font-size: 18px;
  font-weight: 750;
  color: var(--text);
  margin: 0;
  line-height: 1.2;
}

.ma-cockpit-subtitle {
  font-size: 11px;
  color: var(--muted);
  margin: 0;
}

.ma-cockpit-subtitle strong {
  color: var(--text);
  font-weight: 600;
}

.ma-cockpit-right {
  display: flex;
  align-items: center;
  gap: 8px;
}

.ma-btn-primary {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 32px;
  padding: 0 12px;
  border-radius: var(--r-md, 8px);
  border: 1px solid color-mix(in srgb, var(--brand, #388bfd) 35%, transparent);
  background: color-mix(in srgb, var(--brand, #388bfd) 12%, var(--surface));
  color: var(--brand-deep, var(--brand, #388bfd));
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.15s ease;
  white-space: nowrap;
}

.ma-btn-primary:hover:not(:disabled) {
  background: color-mix(in srgb, var(--brand, #388bfd) 20%, var(--surface));
  border-color: var(--brand);
  transform: translateY(-1px);
}

.ma-btn-primary:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.ma-btn-primary :deep(svg) {
  width: 13px;
  height: 13px;
}

.ma-btn-secondary {
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
  font-weight: 600;
  cursor: pointer;
  transition: all 0.15s ease;
  white-space: nowrap;
}

.ma-btn-secondary:hover {
  background: var(--surface-hover);
  border-color: var(--line-hover);
  transform: translateY(-1px);
}

.ma-btn-secondary.is-copied {
  border-color: #10b981;
  color: #10b981;
  background: rgba(16, 185, 129, 0.08);
}

.ma-btn-secondary :deep(svg) {
  width: 13px;
  height: 13px;
}

.ma-btn-sm {
  height: 26px;
  padding: 0 8px;
  font-size: 11px;
}

.ma-count-chip {
  padding: 1px 5px;
  border-radius: var(--r-full);
  background: var(--page-bg);
  color: var(--muted);
  font-size: 9.5px;
  font-weight: 700;
}

.ma-error-banner {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 20px;
  font-size: 11.5px;
  background: rgba(239, 68, 68, 0.1);
  border-bottom: 1px solid rgba(239, 68, 68, 0.2);
  color: #ef4444;
  flex-shrink: 0;
}

.ma-error-icon :deep(svg) {
  width: 15px;
  height: 15px;
}

/* ============================================================
   2. 4 大 Bento KPI 指标卡 (Stats Deck)
   ============================================================ */
.ma-stats-deck {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 10px;
  padding: 12px 18px 0;
  flex-shrink: 0;
}

@media (max-width: 1100px) {
  .ma-stats-deck {
    grid-template-columns: repeat(2, 1fr);
  }
}

.ma-stat-card {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 10px);
  padding: 10px 14px;
  display: flex;
  flex-direction: column;
  min-height: 80px;
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.02);
  transition: all 0.15s ease;
}

.ma-stat-card:hover {
  border-color: var(--line-hover);
}

.ma-stat-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 6px;
  margin-bottom: 4px;
}

.ma-stat-tag {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 10px;
  font-weight: 750;
  letter-spacing: 0.05em;
  text-transform: uppercase;
}

.ma-stat-tag :deep(svg) {
  width: 12px;
  height: 12px;
}

.ma-stat-tag.is-blue { color: #3b82f6; }
.ma-stat-tag.is-emerald { color: #10b981; }
.ma-stat-tag.is-purple { color: #a855f7; }
.ma-stat-tag.is-orange { color: #f97316; }

.ma-stat-pill {
  padding: 1px 6px;
  border-radius: var(--r-full);
  font-size: 9.5px;
  font-weight: 700;
}

.ma-stat-pill.is-blue { background: rgba(59, 130, 246, 0.12); color: #3b82f6; }
.ma-stat-pill.is-emerald { background: rgba(16, 185, 129, 0.12); color: #10b981; }
.ma-stat-pill.is-purple { background: rgba(168, 85, 247, 0.12); color: #a855f7; }
.ma-stat-pill.is-orange { background: rgba(249, 115, 22, 0.12); color: #f97316; }

.ma-stat-main {
  display: flex;
  align-items: baseline;
  gap: 5px;
  margin-bottom: 4px;
}

.ma-stat-main strong {
  font-size: 22px;
  font-weight: 800;
  line-height: 1;
  font-variant-numeric: tabular-nums;
  letter-spacing: -0.02em;
}

.ma-stat-unit {
  font-size: 11px;
  color: var(--muted);
  font-weight: 600;
}

.ma-stat-footer {
  font-size: 10.5px;
  color: var(--muted);
  margin-top: auto;
}

.ma-stat-footer strong {
  color: var(--text);
}

/* ============================================================
   3. 左右分屏主工作区 (Dual-Pane Workspace)
   ============================================================ */
.ma-workspace {
  flex: 1;
  min-height: 0;
  display: grid;
  grid-template-columns: 280px 1fr;
  gap: 12px;
  padding: 12px 18px 18px;
  overflow: hidden;
}

@media (max-width: 900px) {
  .ma-workspace {
    grid-template-columns: 240px 1fr;
  }
}

/* 左侧模型树面板 */
.ma-tree-pane {
  display: flex;
  flex-direction: column;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 10px);
  overflow: hidden;
  min-height: 0;
}

.ma-tree-toolbar {
  padding: 8px 10px;
  border-bottom: 1px solid var(--line);
  display: flex;
  flex-direction: column;
  gap: 6px;
  background: var(--page-bg);
}

.ma-search-box {
  position: relative;
  display: flex;
  align-items: center;
  width: 100%;
}

.ma-search-icon {
  position: absolute;
  left: 8px;
  color: var(--muted);
  pointer-events: none;
  display: flex;
  align-items: center;
}

.ma-search-icon :deep(svg) {
  width: 12px;
  height: 12px;
}

.ma-search-input {
  width: 100%;
  height: 26px;
  padding: 0 24px 0 24px;
  border-radius: 5px;
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--text);
  font-size: 11px;
  outline: none;
  transition: all 0.15s ease;
}

.ma-search-input:focus {
  border-color: var(--brand);
  box-shadow: 0 0 0 2px var(--brand-soft);
}

.ma-search-clear {
  position: absolute;
  right: 6px;
  border: none;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  padding: 0;
  display: flex;
  align-items: center;
}

.ma-search-clear :deep(svg) {
  width: 10px;
  height: 10px;
}

.ma-tree-actions {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 6px;
}

.ma-text-btn {
  background: transparent;
  border: none;
  color: var(--muted);
  font-size: 10.5px;
  font-weight: 600;
  cursor: pointer;
  padding: 2px 4px;
  border-radius: 4px;
}

.ma-text-btn:hover {
  color: var(--brand);
  background: var(--surface-hover);
}

.ma-tree-scroll {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  overflow-x: hidden;
  padding: 6px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.ma-vendor-group {
  display: flex;
  flex-direction: column;
  border-radius: 6px;
}

.ma-vendor-header {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 6px 8px;
  border: none;
  background: transparent;
  color: var(--text);
  font-size: 11.5px;
  cursor: pointer;
  border-radius: 5px;
  text-align: left;
  transition: background 0.12s ease;
}

.ma-vendor-header:hover {
  background: var(--surface-hover);
}

.ma-vendor-chevron {
  font-size: 8px;
  color: var(--muted);
  transition: transform 0.15s ease;
  width: 8px;
  transform: rotate(-90deg);
}

.ma-vendor-chevron.is-expanded {
  transform: rotate(0deg);
}

.ma-vendor-name {
  flex: 1;
  font-weight: 700;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.ma-vendor-count {
  font-size: 10px;
  color: var(--muted);
  background: var(--page-bg);
  padding: 0 4px;
  border-radius: 3px;
}

.ma-vendor-models-list {
  display: flex;
  flex-direction: column;
  gap: 1px;
  padding-left: 14px;
  margin-top: 1px;
}

.ma-model-item-btn {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 6px;
  padding: 5px 8px;
  border: 1px solid transparent;
  background: transparent;
  color: var(--text);
  font-size: 11px;
  font-weight: 550;
  border-radius: 5px;
  cursor: pointer;
  text-align: left;
  transition: all 0.12s ease;
}

.ma-model-item-btn:hover {
  background: var(--surface-hover);
}

.ma-model-item-btn.is-active {
  background: var(--brand-soft);
  color: var(--brand-deep);
  border-color: color-mix(in srgb, var(--brand) 30%, transparent);
  font-weight: 650;
}

.ma-model-label {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.ma-model-badge {
  font-size: 9px;
  padding: 1px 4px;
  border-radius: 3px;
  background: var(--page-bg);
  color: var(--muted);
  white-space: nowrap;
}

.ma-model-item-btn.is-active .ma-model-badge {
  background: var(--surface);
  color: var(--brand-deep);
}

.ma-tree-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 30px 10px;
  color: var(--muted);
  font-size: 11.5px;
  gap: 6px;
}

/* 右侧通道与调度面板 */
.ma-list-pane {
  display: flex;
  flex-direction: column;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 10px);
  overflow: hidden;
  min-height: 0;
}

.ma-list-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 8px 14px;
  background: var(--page-bg);
  border-bottom: 1px solid var(--line);
  min-height: 42px;
  box-sizing: border-box;
}

.ma-selected-model-pill {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}

.ma-model-icon :deep(svg) {
  width: 14px;
  height: 14px;
  color: var(--brand);
}

.ma-selected-code {
  font-size: 13px;
  font-weight: 700;
  color: var(--text);
}

.ma-list-meta-count {
  font-size: 11.5px;
  color: var(--muted);
}

.ma-list-meta-count strong {
  color: var(--brand);
}

.ma-list-placeholder {
  font-size: 12px;
  color: var(--muted);
}

.ma-list-scroll {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 12px 14px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.ma-loading-state,
.ma-empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 50px 20px;
  color: var(--muted);
  gap: 10px;
  font-size: 12px;
}

.ma-empty-icon :deep(svg) {
  width: 36px;
  height: 36px;
  color: var(--muted);
}

.ma-entries-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.ma-entry-card {
  background: var(--page-bg);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  padding: 10px 12px;
  display: flex;
  flex-direction: column;
  gap: 8px;
  transition: all 0.15s ease;
}

.ma-entry-card:hover {
  border-color: var(--line-hover);
  box-shadow: 0 2px 6px rgba(0, 0, 0, 0.03);
}

.ma-entry-card.is-dragging {
  opacity: 0.4;
}

.ma-entry-card.is-drop-before {
  border-top: 2px solid var(--brand);
}

.ma-entry-card.is-drop-after {
  border-bottom: 2px solid var(--brand);
}

.ma-entry-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
}

.ma-entry-left {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
  flex: 1;
}

.ma-grip-handle {
  display: inline-flex;
  align-items: center;
  color: var(--muted);
  cursor: grab;
  padding: 2px;
}

.ma-grip-handle :deep(svg) {
  width: 12px;
  height: 12px;
}

.ma-priority-index {
  font-size: 10px;
  font-weight: 800;
  color: var(--muted);
  background: var(--surface);
  padding: 1px 4px;
  border-radius: 3px;
}

.ma-entry-title {
  font-size: 13px;
  font-weight: 700;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.ma-entry-title.is-group {
  color: var(--brand);
}

.ma-system-badge {
  padding: 1px 5px;
  border-radius: 3px;
  background: var(--surface);
  border: 1px solid var(--line);
  font-size: 9.5px;
  color: var(--muted);
}

.ma-entry-actions {
  display: flex;
  align-items: center;
  gap: 2px;
}

.ma-move-btn {
  width: 22px;
  height: 22px;
  border-radius: 4px;
  border: none;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
}

.ma-move-btn:hover:not(:disabled) {
  background: var(--surface);
  color: var(--text);
}

.ma-move-btn:disabled {
  opacity: 0.3;
  cursor: default;
}

.ma-move-btn :deep(svg) {
  width: 11px;
  height: 11px;
}

.ma-groups-container {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.ma-group-box {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: 6px;
  padding: 8px 10px;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.ma-group-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}

.ma-group-name {
  font-size: 11.5px;
  font-weight: 650;
  color: var(--text);
}

.ma-mode-segment {
  display: inline-flex;
  align-items: center;
  gap: 1px;
  background: var(--page-bg);
  padding: 2px;
  border-radius: 5px;
  border: 1px solid var(--line);
}

.ma-mode-btn {
  height: 20px;
  padding: 0 6px;
  border-radius: 3px;
  border: none;
  background: transparent;
  color: var(--muted);
  font-size: 10px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.12s ease;
}

.ma-mode-btn.active {
  background: var(--surface);
  color: var(--brand);
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.05);
}

.ma-key-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  background: var(--page-bg);
  padding: 4px 8px;
  border-radius: 4px;
}

.ma-key-info {
  flex: 1;
  min-width: 0;
}

.ma-key-code {
  font-size: 10.5px;
  font-family: monospace;
  color: var(--muted);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  display: block;
}

.ma-key-desc {
  font-size: 10px;
  color: var(--muted);
  white-space: nowrap;
}

/* ============================================================
   4. 弹窗体系 (Modal Dialogs)
   ============================================================ */
.ma-modal-backdrop {
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

.ma-modal-card {
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

.ma-modal-card.is-gateway {
  max-width: 680px;
}

.ma-modal-card.is-picker {
  max-width: 720px;
}

.ma-modal-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  padding: 14px 18px;
  border-bottom: 1px solid var(--line);
  flex-shrink: 0;
}

.ma-modal-title-group h2 {
  font-size: 15px;
  font-weight: 750;
  margin: 2px 0 0;
}

.ma-modal-eyebrow {
  font-size: 9.5px;
  font-weight: 750;
  letter-spacing: 0.05em;
  color: var(--brand);
  text-transform: uppercase;
}

.ma-modal-header p {
  font-size: 11px;
  color: var(--muted);
  margin: 2px 0 0;
}

.ma-modal-close-btn {
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

.ma-modal-close-btn:hover {
  background: var(--surface-hover);
  color: var(--text);
}

.ma-modal-body {
  padding: 16px 18px;
  overflow-y: auto;
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.ma-modal-footer {
  padding: 10px 18px;
  border-top: 1px solid var(--line);
  display: flex;
  justify-content: flex-end;
  align-items: center;
  gap: 8px;
  background: var(--page-bg);
  flex-shrink: 0;
}

.ma-btn-cancel {
  height: 30px;
  padding: 0 14px;
  border-radius: var(--r-md, 6px);
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--text);
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
}

.ma-btn-cancel:hover {
  background: var(--surface-hover);
}

.ma-input {
  padding: 6px 10px;
  border-radius: 6px;
  border: 1px solid var(--line);
  background: var(--page-bg);
  color: var(--text);
  font-size: 12px;
  outline: none;
  box-sizing: border-box;
}

.ma-input:focus {
  border-color: var(--brand);
  background: var(--surface);
}

/* Gateway Modal Elements */
.ma-gw-endpoint-card {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 10px 14px;
  background: var(--page-bg);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
}

.ma-gw-endpoint-info {
  display: flex;
  flex-direction: column;
  gap: 3px;
}

.ma-gw-label {
  font-size: 10.5px;
  color: var(--muted);
  font-weight: 600;
}

.ma-gw-url-box {
  font-size: 12.5px;
  font-weight: 700;
  color: var(--brand);
  font-family: monospace;
}

.ma-gw-keys-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-size: 12px;
}

.ma-gw-empty-keys {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 30px;
  color: var(--muted);
  font-size: 11.5px;
  gap: 8px;
  background: var(--page-bg);
  border: 1px dashed var(--line);
  border-radius: 8px;
}

.ma-gw-keys-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
  max-height: 280px;
  overflow-y: auto;
}

.ma-gw-key-card {
  background: var(--page-bg);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  padding: 10px 12px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.ma-gw-key-row-top {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}

.ma-gw-key-name {
  flex: 1;
  font-size: 11.5px;
}

.ma-gw-key-enable {
  display: flex;
  align-items: center;
  gap: 5px;
  font-size: 11px;
  color: var(--muted);
  cursor: pointer;
}

.ma-gw-key-row-bottom {
  display: flex;
  align-items: center;
  gap: 8px;
}

.ma-gw-key-token {
  flex: 1;
  font-family: monospace;
  font-size: 11px;
}

.ma-gw-key-actions {
  display: flex;
  align-items: center;
  gap: 4px;
}

.ma-icon-btn {
  width: 26px;
  height: 26px;
  border-radius: 5px;
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--muted);
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
}

.ma-icon-btn:hover {
  color: var(--text);
  border-color: var(--line-hover);
}

.ma-icon-btn.is-danger:hover {
  color: #ef4444;
  border-color: rgba(239, 68, 68, 0.3);
}

.ma-icon-btn :deep(svg) {
  width: 12px;
  height: 12px;
}

/* Picker Modal Elements */
.ma-picker-toolbar {
  padding: 8px 18px;
  background: var(--page-bg);
  border-bottom: 1px solid var(--line);
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
}

.ma-picker-search {
  max-width: 280px;
}

.ma-picker-actions {
  display: flex;
  align-items: center;
  gap: 6px;
}

.ma-picker-body {
  padding: 12px 18px;
  max-height: 400px;
}

.ma-picker-vendor-group {
  background: var(--page-bg);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  padding: 10px 12px;
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin-bottom: 8px;
}

.ma-picker-vendor-head {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  font-weight: 750;
  cursor: pointer;
}

.ma-picker-models-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(200px, 1fr));
  gap: 6px;
  padding-left: 20px;
}

.ma-picker-model-row {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 4px 6px;
  border-radius: 4px;
  background: var(--surface);
  font-size: 11px;
  cursor: pointer;
}

.ma-picker-model-row:hover {
  background: var(--surface-hover);
}

.ma-picker-model-name {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.ma-picker-model-count {
  font-size: 9px;
  color: var(--muted);
}

.ma-modal-fade-enter-active,
.ma-modal-fade-leave-active {
  transition: opacity 0.2s ease;
}

.ma-modal-fade-enter-from,
.ma-modal-fade-leave-to {
  opacity: 0;
}
</style>
