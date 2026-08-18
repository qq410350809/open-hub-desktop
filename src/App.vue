<script setup lang="ts">
import { onMounted, onUnmounted, computed } from "vue";
import { icons } from "./icons";
import { useStore } from "./composables/useStore";
import { isTauri, runCommand } from "./composables/useLibrary";
import { usePreferences } from "./composables/usePreferences";
import { useTheme } from "./composables/useTheme";
import { useToast } from "./composables/useToast";
import { useTooltip } from "./composables/useTooltip";
import { useContextMenu } from "./composables/useContextMenu";
import AppSidebar from "./components/AppSidebar.vue";
import SiteLibraryPage from "./components/SiteLibraryPage.vue";
import SiteFormModal from "./components/SiteFormModal.vue";
import LinkDialog from "./components/LinkDialog.vue";
import PreviewDialog from "./components/PreviewDialog.vue";
import SettingsPage from "./components/SettingsPage.vue";
import SyncSitesDialog from "./components/SyncSitesDialog.vue";
import ChromeSessionDialog from "./components/ChromeSessionDialog.vue";
import SiteModelsDialog from "./components/SiteModelsDialog.vue";
import ConfirmDialog from "./components/ConfirmDialog.vue";
import CharityMonitorPage from "./components/CharityMonitorPage.vue";
import ProxyPoolPage from "./components/ProxyPoolPage.vue";
import TokenStatsPage from "./components/TokenStatsPage.vue";
import ModelCatalogPage from "./components/ModelCatalogPage.vue";
import ModelAggregatePage from "./components/ModelAggregatePage.vue";

const store = useStore();
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
    else if (["library", "modelparams", "modelagg", "charity", "proxy", "tokenstats"].includes(store.page.value)) store.openTokenStats();
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
  else if (page === "modelagg") store.openModelAgg();
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
    store.loadModelAggregation(),
  ]);
  store.startDailyRefresh();
  store.startCharityMonitor();
  // 自动会话同步：读取设置并订阅轮次事件（恢复成功/需人工过盾时 toast 提醒）。
  store.initializeAutoSync();
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
  store.unbindAutoSyncListeners();
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
        <div
          v-if="store.page.value === 'library'"
          id="library-panel"
          class="library-panel"
          aria-labelledby="library-nav"
        >
          <SiteLibraryPage />
        </div>
        <div
          v-else-if="store.page.value === 'modelparams'"
          id="model-params-panel"
          class="model-params-panel"
          aria-labelledby="modelparams-nav"
        >
          <ModelCatalogPage />
        </div>
        <div
          v-else-if="store.page.value === 'modelagg'"
          id="model-agg-panel"
          class="modelagg-panel"
          aria-labelledby="modelagg-nav"
        >
          <ModelAggregatePage />
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
  <ConfirmDialog />
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
