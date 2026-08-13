<script setup lang="ts">
import { onMounted, onUnmounted, computed } from "vue";
import { icons } from "./icons";
import { useStore } from "./composables/useStore";
import { isTauri, runCommand } from "./composables/useLibrary";
import { SYSTEM_TYPES } from "./types";
import { usePreferences } from "./composables/usePreferences";
import { useTheme } from "./composables/useTheme";
import { useToast } from "./composables/useToast";
import { useTooltip } from "./composables/useTooltip";
import { useContextMenu } from "./composables/useContextMenu";
import AppSidebar from "./components/AppSidebar.vue";
import CustomSelect from "./components/CustomSelect.vue";
import SiteGrid from "./components/SiteGrid.vue";
import SiteFormModal from "./components/SiteFormModal.vue";
import LinkDialog from "./components/LinkDialog.vue";
import PreviewDialog from "./components/PreviewDialog.vue";
import SettingsPage from "./components/SettingsPage.vue";
import SyncSitesDialog from "./components/SyncSitesDialog.vue";
import ChromeSessionDialog from "./components/ChromeSessionDialog.vue";
import SiteModelsDialog from "./components/SiteModelsDialog.vue";
import CharityMonitorPage from "./components/CharityMonitorPage.vue";
import ProxyPoolPage from "./components/ProxyPoolPage.vue";
import TokenStatsPage from "./components/TokenStatsPage.vue";
import ModelCatalogPage from "./components/ModelCatalogPage.vue";

const store = useStore();

const tagOptions = computed(() => [
  { value: "all", text: "全部标签" },
  ...store.allTags.value.map((tag) => ({ value: tag, text: tag })),
]);
const levelOptions = [
  { value: "all", text: "全部等级" },
  { value: "0", text: "LV0" },
  { value: "1", text: "LV1" },
  { value: "2", text: "LV2" },
  { value: "3", text: "LV3" },
];
const featureOptions = [
  { value: "all", text: "全部功能" },
  { value: "checkin", text: "支持签到" },
  { value: "translation", text: "沉浸式翻译" },
  { value: "ldc", text: "支持 LDC" },
  { value: "nsfw", text: "支持 NSFW" },
  { value: "invite", text: "需要邀请码" },
];
const systemTypeOptions = [
  { value: "all", text: "全部系统类型" },
  ...SYSTEM_TYPES,
  { value: "unknown", text: "未知类型" },
];
const { preferences } = usePreferences();
const { applyTheme } = useTheme();
const { message, isError, visible } = useToast();
const {
  tooltipText,
  tooltipVisible,
  tooltipLeft,
  tooltipTop,
  tooltipArrowLeft,
  tooltipBelow,
  onPointerOver,
  onPointerOut,
  onFocusIn,
  onFocusOut,
  onPointerDown,
  onScroll,
} = useTooltip();
const {
  visible: contextMenuVisible,
  left: contextMenuLeft,
  top: contextMenuTop,
  items: contextMenuItems,
  runAction: runContextMenuAction,
} = useContextMenu();

const sidebarCollapsed = computed(() => preferences.sidebarCollapsed);


function onKeydown(event: KeyboardEvent) {
  // ⌘K 聚焦搜索
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
    if (
      store.page.value === "library" &&
      !store.modalOpen.value &&
      !store.previewDialogOpen.value &&
      !store.linkDialogOpen.value &&
      !store.chromeSessionDialogOpen.value &&
      !store.syncDialogOpen.value
    ) {
      event.preventDefault();
      const search = document.querySelector<HTMLInputElement>("#search-input");
      search?.focus();
      search?.select();
    }
  }
  // Escape 关闭弹窗
  if (event.key === "Escape") {
    if (store.charitySyncLogOpen.value) store.closeCharitySyncLog();
    else if (store.syncDialogOpen.value) store.closeSyncDialog();
    else if (store.chromeSessionDialogOpen.value) store.closeChromeSessionDialog();
    else if (store.previewDialogOpen.value) store.closePreview();
    else if (store.linkDialogOpen.value) store.closeLinkDialog();
    else if (store.modalOpen.value) store.closeModal();
    else if (store.page.value === "settings") store.closeSettings();
    else if (["modelparams", "charity", "proxy", "tokenstats"].includes(store.page.value)) store.openLibrary();
  }
}

function onMenuReload() {
  // 右键“强制刷新”：类似 F5，重新加载整个页面。
  window.location.reload();
}

function onMenuNavigate(event: Event) {
  const detail = (event as CustomEvent<{ page?: string }>).detail;
  const page = detail?.page;
  if (page === "library") store.openLibrary();
  else if (page === "modelparams") store.openModelParams();
  else if (page === "charity") store.openCharityMonitor();
  else if (page === "proxy") store.openProxyPool();
  else if (page === "tokenstats") store.openTokenStats();
  else if (page === "settings") store.openSettings();
}

async function showDesktopWindow() {
  try {
    await runCommand("show_main_window");
  } catch (error) {
    store.showToast(String(error), true);
  }
}

onMounted(async () => {
  applyTheme();

  document.addEventListener("pointerover", onPointerOver);
  document.addEventListener("pointerout", onPointerOut);
  document.addEventListener("focusin", onFocusIn);
  document.addEventListener("focusout", onFocusOut);
  document.addEventListener("pointerdown", onPointerDown);
  document.addEventListener("scroll", onScroll, { capture: true, passive: true });
  window.addEventListener("resize", onScroll, { passive: true });
  document.addEventListener("keydown", onKeydown);
  window.addEventListener("oh-menu-reload", onMenuReload);
  window.addEventListener("oh-menu-navigate", onMenuNavigate);
  // 查询定时器立即启动，只读 SQLite，不等待其他页面数据初始化。
  store.startTokenDatabaseRefresh();
  await Promise.all([
    store.loadLibrary(),
    store.loadProxyPool(),
    store.initializeModelCatalog(),
  ]);
  store.startDailyRefresh();
  store.startCharityMonitor();
});

onUnmounted(() => {
  document.removeEventListener("pointerover", onPointerOver);
  document.removeEventListener("pointerout", onPointerOut);
  document.removeEventListener("focusin", onFocusIn);
  document.removeEventListener("focusout", onFocusOut);
  document.removeEventListener("pointerdown", onPointerDown);
  document.removeEventListener("scroll", onScroll, { capture: true });
  window.removeEventListener("resize", onScroll);
  document.removeEventListener("keydown", onKeydown);
  window.removeEventListener("oh-menu-reload", onMenuReload);
  window.removeEventListener("oh-menu-navigate", onMenuNavigate);
  store.stopCharityMonitor();
  store.stopDailyRefresh();
  store.stopTokenDatabaseRefresh();
  store.stopModelCatalogEvents();
});

</script>

<template>
  <div class="app-layout" :class="{ 'sidebar-collapsed': sidebarCollapsed }">
    <div v-if="!isTauri" class="lightweight-banner" role="status">
      <span class="lightweight-banner-icon" v-html="icons.globe" />
      <span class="lightweight-banner-text">轻量模式：正在通过浏览器访问本地内核</span>
      <button type="button" class="secondary-button lightweight-banner-button" @click="showDesktopWindow">
        打开桌面窗口
      </button>
    </div>
    <AppSidebar />

    <div class="app-workspace">
      <div class="workspace-view">
        <section
          v-if="store.page.value === 'library'"
          id="library-panel"
          class="library-page"
          aria-labelledby="library-nav"
        >
          <header class="library-header">
            <div class="library-header-bar">
              <div class="library-heading">
                <span class="library-eyebrow">OpenHub · 站点资料库</span>
                <h1>站点库</h1>
                <p>已筛选 {{ store.filteredSites.value.length }} / 共 {{ store.sites.value.length }} 个站点</p>
              </div>
              <div class="library-header-actions">
                <button
                  class="secondary-button sync-button"
                  :disabled="store.syncingSites.value || store.syncingModelKeys.value"
                  :data-tooltip="store.usageFilter.value === 'all'
                    ? '根据当前存活/跑路状态，从 ldoh 同步站点'
                    : `同步当前${store.usageFilter.value === 'pending' ? '待定' : '在用'}站点的账号额度`"
                  @click="store.openSyncDialog()"
                >
                  <span v-html="store.usageFilter.value === 'all' ? icons.restore : icons.activity" />
                  <span>{{ store.usageFilter.value === 'all' ? '同步站点' : '额度同步' }}</span>
                </button>
                <button
                  v-if="store.runawayFilter.value === 'active'"
                  class="primary-button"
                  id="add-site"
                  @click="store.openModal()"
                >
                  <span v-html="icons.plus" /><span>添加站点</span>
                </button>
              </div>
            </div>

            <div class="library-toolbar" aria-label="站点筛选">
              <div class="library-toolbar-main">
                <div class="filter-segment surface library-usage-switch" role="group" aria-label="站点库视图">
                  <button
                    id="all-usage-filter"
                    type="button"
                    :class="{ active: store.usageFilter.value === 'all' }"
                    :aria-pressed="store.usageFilter.value === 'all'"
                    @click="store.setUsageFilter('all')"
                  >全部</button>
                  <button
                    id="personal-filter"
                    type="button"
                    :class="{ active: store.usageFilter.value === 'personal' }"
                    :aria-pressed="store.usageFilter.value === 'personal'"
                    @click="store.setUsageFilter('personal')"
                  >在用</button>
                  <button
                    id="pending-filter"
                    type="button"
                    :class="{ active: store.usageFilter.value === 'pending' }"
                    :aria-pressed="store.usageFilter.value === 'pending'"
                    @click="store.setUsageFilter('pending')"
                  >待定</button>
                </div>
                <label class="search-box library-search">
                  <span v-html="icons.search" />
                  <input
                    id="search-input"
                    v-model="store.query.value"
                    type="search"
                    placeholder="搜索站点、API 地址或标签…"
                    autocomplete="off"
                  />
                  <kbd>⌘ K</kbd>
                </label>
                <div class="library-filter-segments">
                  <div class="filter-segment surface is-runaway" role="group" aria-label="站点状态">
                    <button
                      id="active-filter"
                      type="button"
                      :class="{ active: store.runawayFilter.value === 'active' }"
                      :aria-pressed="store.runawayFilter.value === 'active'"
                      @click="store.setRunawayFilter('active')"
                    >存活</button>
                    <button
                      id="runaway-filter"
                      type="button"
                      :class="{ active: store.runawayFilter.value === 'runaway' }"
                      :aria-pressed="store.runawayFilter.value === 'runaway'"
                      @click="store.setRunawayFilter('runaway')"
                    >跑路</button>
                  </div>
                  <button
                    v-if="store.hasFilters.value"
                    class="text-button library-clear-filters"
                    id="clear-filter-header"
                    type="button"
                    @click="store.clearFilters()"
                  >清除筛选</button>
                </div>
              </div>
              <div class="library-filter-selects">
                <CustomSelect
                  class="library-select"
                  :options="tagOptions"
                  :model-value="store.tag.value"
                  @update:model-value="store.tag.value = $event"
                  aria-label="标签筛选"
                />
                <CustomSelect
                  class="library-select"
                  :options="levelOptions"
                  :model-value="store.level.value"
                  @update:model-value="store.level.value = $event"
                  aria-label="等级筛选"
                />
                <CustomSelect
                  class="library-select"
                  :options="featureOptions"
                  :model-value="store.feature.value"
                  @update:model-value="store.feature.value = $event"
                  aria-label="功能筛选"
                />
                <CustomSelect
                  class="library-select"
                  :options="systemTypeOptions"
                  :model-value="store.systemTypeFilter.value"
                  @update:model-value="store.systemTypeFilter.value = $event"
                  aria-label="系统类型筛选"
                />
              </div>
            </div>
          </header>

          <SiteGrid />
        </section>
        <div
          v-else-if="store.page.value === 'modelparams'"
          id="model-params-panel"
          class="model-params-panel"
          aria-labelledby="modelparams-nav"
        >
          <ModelCatalogPage />
        </div>
        <div
          v-else-if="store.page.value === 'charity'"
          id="charity-panel"
          class="charity-panel"
          aria-labelledby="charity-nav"
        >
          <CharityMonitorPage />
        </div>
        <div
          v-else-if="store.page.value === 'proxy'"
          id="proxy-panel"
          class="proxy-panel"
          aria-labelledby="proxy-nav"
        >
          <ProxyPoolPage />
        </div>
        <div
          v-else-if="store.page.value === 'tokenstats'"
          id="token-stats-panel"
          class="token-stats-panel"
          aria-labelledby="tokenstats-nav"
        >
          <TokenStatsPage />
        </div>
      </div>
    </div>
  </div>

  <!-- Tooltip -->
  <div
    class="ui-tooltip"
    id="ui-tooltip"
    role="tooltip"
    :hidden="!tooltipVisible"
    :style="{
      left: tooltipLeft + 'px',
      top: tooltipTop + 'px',
      '--tooltip-arrow-left': tooltipArrowLeft + 'px',
    }"
    :class="{ 'is-below': tooltipBelow }"
  >{{ tooltipText }}</div>

  <!-- Toast -->
  <div
    class="toast"
    id="toast"
    role="status"
    :class="{ visible: visible, error: isError }"
  >{{ message }}</div>

  <!-- 弹窗们 -->
  <SiteFormModal />
  <LinkDialog />
  <PreviewDialog />
  <SyncSitesDialog />
  <ChromeSessionDialog />
  <SiteModelsDialog />
  <SettingsPage />

  <!-- 中文右键菜单：覆盖 WKWebView 默认英文菜单 -->
  <div
    v-if="contextMenuVisible"
    class="oh-context-menu"
    role="menu"
    :style="{ left: contextMenuLeft + 'px', top: contextMenuTop + 'px' }"
    @contextmenu.prevent
  >
    <template v-for="item in contextMenuItems" :key="item.id">
      <div v-if="item.separator" class="oh-context-menu-sep" role="separator"></div>
      <button
        v-else
        type="button"
        class="oh-context-menu-item"
        role="menuitem"
        :disabled="!item.enabled"
        @click="runContextMenuAction(item.id)"
      >
        <span>{{ item.label }}</span>
        <kbd v-if="item.accelerator">{{ item.accelerator }}</kbd>
      </button>
    </template>
  </div>
</template>
