<script setup lang="ts">
import { onMounted, onUnmounted, computed, watch } from "vue";
import { icons } from "./icons";
import { useStore } from "./composables/useStore";
import { usePreferences } from "./composables/usePreferences";
import { useTheme } from "./composables/useTheme";
import { useToast } from "./composables/useToast";
import { useTooltip } from "./composables/useTooltip";
import AppSidebar from "./components/AppSidebar.vue";
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

const sidebarCollapsed = computed(() => preferences.sidebarCollapsed);

function applyFont() {
  const root = document.documentElement;
  // Font Family
  if (preferences.fontFamily === 'serif') {
    root.style.setProperty('--font', 'Georgia, "Times New Roman", Times, serif');
  } else if (preferences.fontFamily === 'mono') {
    root.style.setProperty('--font', 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace');
  } else if (preferences.fontFamily === 'system') {
    root.style.setProperty('--font', '-apple-system, BlinkMacSystemFont, "PingFang SC", "Microsoft YaHei", "Segoe UI", sans-serif');
  } else {
    root.style.setProperty('--font', `"${preferences.fontFamily}", -apple-system, BlinkMacSystemFont, "PingFang SC", sans-serif`);
  }

  // Font Size
  if (preferences.fontSize === 'small') {
    root.style.setProperty('--base-font-size', '13px');
  } else if (preferences.fontSize === 'large') {
    root.style.setProperty('--base-font-size', '15px');
  } else {
    root.style.setProperty('--base-font-size', '14px');
  }
}

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
    if (store.syncDialogOpen.value) store.closeSyncDialog();
    else if (store.chromeSessionDialogOpen.value) store.closeChromeSessionDialog();
    else if (store.previewDialogOpen.value) store.closePreview();
    else if (store.linkDialogOpen.value) store.closeLinkDialog();
    else if (store.modalOpen.value) store.closeModal();
    else if (store.page.value === "settings") store.closeSettings();
    else if (store.page.value === "charity" || store.page.value === "proxy") store.openLibrary();
  }
}

onMounted(async () => {
  applyTheme();
  applyFont();
  document.addEventListener("pointerover", onPointerOver);
  document.addEventListener("pointerout", onPointerOut);
  document.addEventListener("focusin", onFocusIn);
  document.addEventListener("focusout", onFocusOut);
  document.addEventListener("pointerdown", onPointerDown);
  document.addEventListener("scroll", onScroll, { capture: true, passive: true });
  window.addEventListener("resize", onScroll, { passive: true });
  document.addEventListener("keydown", onKeydown);
  await Promise.all([store.loadLibrary(), store.loadProxyPool()]);
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
  store.stopCharityMonitor();
});

watch(
  () => preferences.fontFamily,
  applyFont
);
watch(
  () => preferences.fontSize,
  applyFont
);
</script>

<template>
  <div class="app-layout" :class="{ 'sidebar-collapsed': sidebarCollapsed }">
    <AppSidebar />

    <div class="app-workspace">
      <nav class="workspace-tabs" aria-label="主要模块" role="tablist">
        <div class="workspace-tab-list">
          <button
            id="library-tab"
            type="button"
            role="tab"
            aria-controls="library-panel"
            :aria-selected="store.page.value === 'library'"
            :tabindex="store.page.value === 'library' ? 0 : -1"
            :class="{ active: store.page.value === 'library' }"
            @click="store.openLibrary()"
          >
            <span v-html="icons.database" />
            <strong>站点库</strong>
            <small>{{ store.sites.value.length }}</small>
          </button>
          <button
            id="charity-tab"
            type="button"
            role="tab"
            aria-controls="charity-panel"
            :aria-selected="store.page.value === 'charity'"
            :tabindex="store.page.value === 'charity' ? 0 : -1"
            :class="{ active: store.page.value === 'charity' }"
            @click="store.openCharityMonitor()"
          >
            <span v-html="icons.heartPulse" />
            <strong>公益监听</strong>
            <i v-if="store.charityFeedUnreadCount.value">{{ Math.min(store.charityFeedUnreadCount.value, 99) }}</i>
          </button>
          <button
            id="proxy-tab"
            type="button"
            role="tab"
            aria-controls="proxy-panel"
            :aria-selected="store.page.value === 'proxy'"
            :tabindex="store.page.value === 'proxy' ? 0 : -1"
            :class="{ active: store.page.value === 'proxy' }"
            @click="store.openProxyPool()"
          >
            <span v-html="icons.wifi" />
            <strong>代理池</strong>
            <small>{{ store.proxyPool.value.nodeCount }}</small>
          </button>
        </div>
      </nav>

      <div class="workspace-view">
        <section
          v-if="store.page.value === 'library'"
          id="library-panel"
          class="library-page"
          role="tabpanel"
          aria-labelledby="library-tab"
        >
          <header class="app-header">
            <div class="header-inner">
              <label class="search-box">
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
              <div class="header-actions">
                <button
                  class="secondary-button sync-button"
                  :disabled="store.syncingModelKeys.value"
                  :data-tooltip="store.usageFilter.value === 'personal'
                    ? '同步当前列表内在用站点的 Chrome 会话'
                    : '根据当前存活/跑路状态，从 ldoh 同步站点'"
                  @click="store.openSyncDialog()"
                >
                  <span v-html="icons.restore" /><span>同步站点</span>
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
          </header>

          <SiteGrid />
        </section>
        <div
          v-else-if="store.page.value === 'charity'"
          id="charity-panel"
          class="charity-panel"
          role="tabpanel"
          aria-labelledby="charity-tab"
        >
          <CharityMonitorPage />
        </div>
        <div
          v-else-if="store.page.value === 'proxy'"
          id="proxy-panel"
          class="proxy-panel"
          role="tabpanel"
          aria-labelledby="proxy-tab"
        >
          <ProxyPoolPage />
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
</template>
