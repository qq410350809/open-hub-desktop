<script setup lang="ts">
import { onMounted, onUnmounted, computed } from "vue";
import { icons } from "./icons";
import { useStore } from "./composables/useStore";
import { usePreferences } from "./composables/usePreferences";
import { useTheme } from "./composables/useTheme";
import { useToast } from "./composables/useToast";
import { useTooltip } from "./composables/useTooltip";
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
  { value: "newapi", text: "NewAPI" },
  { value: "sub2api", text: "Sub2API" },
  { value: "0v0", text: "0v0" },
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
    else if (store.page.value === "charity" || store.page.value === "proxy") store.openLibrary();
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
  await Promise.all([store.loadLibrary(), store.loadProxyPool()]);
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
  store.stopCharityMonitor();
  store.stopDailyRefresh();
});

</script>

<template>
  <div class="app-layout" :class="{ 'sidebar-collapsed': sidebarCollapsed }">
    <AppSidebar />

    <div class="app-workspace">
      <div class="workspace-view">
        <section
          v-if="store.page.value === 'library'"
          id="library-panel"
          class="library-page"
          aria-labelledby="library-nav"
        >
          <header class="app-header library-header">
            <div class="header-inner">
              <div class="library-heading">
                <strong>站点库</strong>
                <span>{{ store.filteredSites.value.length }} / {{ store.sites.value.length }}</span>
              </div>
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
                  :data-tooltip="store.usageFilter.value === 'personal' || store.usageFilter.value === 'pending'
                    ? '只提取浏览器会话数据（有数据即标待定，不检测站点类型）'
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

            <div class="library-filters" aria-label="站点筛选">
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
              <div class="library-filter-segments">
                <div class="filter-segment surface is-usage" role="group" aria-label="使用状态">
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
          </header>

          <SiteGrid />
        </section>
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
