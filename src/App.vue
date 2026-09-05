<script setup lang="ts">
import { onMounted, onUnmounted, ref, computed } from "vue";
import LoginView from "./components/auth/LoginView.vue";
import {
  AuthExpiredError,
  getSessionToken,
  isIntegratedClient,
  onAuthExpired,
  resetAuthExpired,
  runCommand,
} from "./composables/core/ipc";
import { loadCapabilities, capabilities } from "./composables/core/capabilities";
import { listen, type UnlistenFn } from "./composables/core/events";
import { useStore } from "./composables/useStore";
import { usePreferences } from "./composables/usePreferences";
import { useTheme } from "./composables/useTheme";
import { useToast } from "./composables/useToast";
import { useTooltip } from "./composables/useTooltip";
import { useContextMenu } from "./composables/useContextMenu";
import AppSidebar from "./components/layout/AppSidebar.vue";
import SiteLibraryPage from "./components/pages/SiteLibraryPage.vue";
import SiteFormModal from "./components/site/SiteFormModal.vue";
import LinkDialog from "./components/common/LinkDialog.vue";
import PreviewDialog from "./components/common/PreviewDialog.vue";
import SettingsPage from "./components/pages/SettingsPage.vue";
import SyncSitesDialog from "./components/site/SyncSitesDialog.vue";
import ChromeSessionDialog from "./components/site/ChromeSessionDialog.vue";
import SiteModelsDialog from "./components/site/SiteModelsDialog.vue";
import ConfirmDialog from "./components/common/ConfirmDialog.vue";
import ComponentBootstrapDialog from "./components/common/ComponentBootstrapDialog.vue";
import CharityMonitorPage from "./components/pages/CharityMonitorPage.vue";
import ProxyPoolPage from "./components/pages/ProxyPoolPage.vue";
import TokenStatsPage from "./components/pages/TokenStatsPage.vue";
import ModelCatalogPage from "./components/pages/ModelCatalogPage.vue";
import ModelProxyPage from "./components/pages/ModelProxyPage.vue";

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
const authState = ref<"checking" | "locked" | "ready">("checking");
const loginHintUsername = ref("");
let businessStarted = false;
let removeAuthExpiredListener: (() => void) | null = null;
let authProbeTimer: number | null = null;
// 原生菜单 → 前端联动（后端 on_menu_event 发出）。
let menuUnlisteners: UnlistenFn[] = [];

/** 处理原生菜单的页面导航（与右键菜单 oh-menu-navigate 同一通路）。 */
function onNativeMenuNavigate(page: string) {
  switch (page) {
    case "library":
      store.openLibrary();
      break;
    case "modelparams":
      store.openModelParams();
      break;
    case "modelproxy":
      store.openModelProxy();
      break;
    case "charity":
      store.openCharityMonitor();
      break;
    case "proxy":
      store.openProxyPool();
      break;
    case "tokenstats":
      store.openTokenStats();
      break;
    case "gatewaystats":
      store.openGatewayStats();
      break;
    case "settings":
      store.openSettings();
      break;
    default:
      break;
  }
}

async function startNativeMenuListeners() {
  if (menuUnlisteners.length || !isIntegratedClient) return;
  try {
    menuUnlisteners = (
      await Promise.all([
        listen<string>("menu-navigate", (event) => onNativeMenuNavigate(event.payload)),
        listen("menu-new-site", () => {
          // 与站点库页内「导入站点」一致：切到站点库并打开新建弹窗。
          store.openLibrary();
          store.openModal();
        }),
        listen("menu-export-data", () => {
          // 复用本地统计页的导出入口。
          store.openTokenStats();
          window.dispatchEvent(new CustomEvent("oh-menu-export"));
        }),
        listen("menu-reload", () => window.location.reload()),
      ])
    ).map((result) => result);
  } catch (error) {
    console.warn("[OpenHub] 原生菜单事件监听失败：", error);
  }
}

function stopNativeMenuListeners() {
  menuUnlisteners.forEach((unlisten) => unlisten());
  menuUnlisteners = [];
}

function stopBusiness() {
  if (!businessStarted) return;
  businessStarted = false;
  stopNativeMenuListeners();
  store.stopCharityMonitor();
  store.stopDailyRefresh();
  store.stopTokenDatabaseRefresh();
  store.stopComponentEvents();
  store.stopModelCatalogEvents();
}

async function startBusiness() {
  if (businessStarted || authState.value !== "ready") return;
  businessStarted = true;
  void startNativeMenuListeners();
  store.startComponentEvents();
  store.startTokenDatabaseRefresh();
  const results = await Promise.allSettled([
    store.loadLibrary(),
    store.loadProxyPool(),
    store.initializeModelCatalog(),
  ]);
  if (authState.value !== "ready") return;
  const authFailure = results.find((result) => result.status === "rejected" && result.reason instanceof AuthExpiredError);
  if (authFailure) return;
  results
    .filter((result): result is PromiseRejectedResult => result.status === "rejected")
    .forEach((result) => console.warn("[OpenHub] 主界面初始化失败：", result.reason));
  if (authState.value === "ready") {
    await loadCapabilities();
    // 浏览器瘦客户端没有本地 AI 工具日志可扫描：默认落在网关统计页
    if (store.page.value === "tokenstats" && !capabilities.value.localTokenStats) {
      store.openGatewayStats();
    }
    store.startDailyRefresh();
    store.startCharityMonitor();
  }
}

function startAuthProbe() {
  if (authProbeTimer !== null) return;
  authProbeTimer = window.setInterval(async () => {
    if (authState.value !== "ready") return;
    try {
      const state = await runCommand<{ required: boolean; authenticated: boolean }>(
        "get_login_state",
        { token: getSessionToken() },
      );
      if (state.required && !state.authenticated) lockApplication();
    } catch (error) {
      if (error instanceof AuthExpiredError) lockApplication();
    }
  }, 60_000);
}

function stopAuthProbe() {
  if (authProbeTimer !== null) {
    window.clearInterval(authProbeTimer);
    authProbeTimer = null;
  }
}

function lockApplication() {
  if (authState.value === "locked") return;
  stopAuthProbe();
  stopBusiness();
  authState.value = "locked";
}

async function checkAuthentication() {
  try {
    const state = await runCommand<{ required: boolean; authenticated: boolean; username: string }>(
      "get_login_state",
      { token: getSessionToken() },
    );
    loginHintUsername.value = state.username || "";
    authState.value = !state.required || state.authenticated ? "ready" : "locked";
  } catch (error) {
    if (error instanceof AuthExpiredError) return;
    // 纯静态预览 / 服务不可达时保留原有模拟数据预览能力。
    console.warn("[OpenHub] 登录状态检查失败，进入预览模式：", error);
    authState.value = "ready";
  }
  if (authState.value === "ready") await startBusiness();
  if (authState.value === "ready") startAuthProbe();
}

function onAuthenticated() {
  resetAuthExpired();
  authState.value = "ready";
  startAuthProbe();
  void startBusiness();
}

function onKeydown(event: KeyboardEvent) {
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
  if (event.key === "Escape") {
    if (store.charitySyncLogOpen.value) store.closeCharitySyncLog();
    else if (store.syncDialogOpen.value) store.closeSyncDialog();
    else if (store.chromeSessionDialogOpen.value) store.closeChromeSessionDialog();
    else if (store.previewDialogOpen.value) store.closePreview();
    else if (store.linkDialogOpen.value) store.closeLinkDialog();
    else if (store.modalOpen.value) store.closeModal();
      else if (store.page.value === "settings") store.closeSettings();
    else if (["library", "modelparams", "charity", "proxy", "tokenstats", "gatewaystats"].includes(store.page.value)) store.openTokenStats();
  }
}

function onMenuReload() {
  window.location.reload();
}

function onMenuNavigate(event: Event) {
  const detail = (event as CustomEvent<{ page?: string }>).detail;
  const page = detail?.page;
  if (page === "library") store.openLibrary();
  else if (page === "modelparams") store.openModelParams();
  else if (page === "modelproxy") store.openModelProxy();
  else if (page === "charity") store.openCharityMonitor();
  else if (page === "proxy") store.openProxyPool();
  else if (page === "tokenstats") store.openTokenStats();
  else if (page === "gatewaystats") store.openGatewayStats();
  else if (page === "settings") store.openSettings();
}

onMounted(() => {
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
  removeAuthExpiredListener = onAuthExpired(lockApplication);
  void checkAuthentication();
});

onUnmounted(() => {
  removeAuthExpiredListener?.();
  removeAuthExpiredListener = null;
  stopAuthProbe();
  stopBusiness();
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
});
</script>

<template>
  <LoginView
    v-if="authState === 'locked'"
    :hint-username="loginHintUsername"
    @authenticated="onAuthenticated"
  />
  <div v-else-if="authState !== 'checking'" class="app-layout" :class="{ 'sidebar-collapsed': sidebarCollapsed }">
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
          v-else-if="store.page.value === 'modelproxy'"
          id="model-proxy-panel"
          class="modelproxy-panel"
          aria-labelledby="modelproxy-nav"
        >
          <ModelProxyPage />
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
          <TokenStatsPage mode="local" />
        </div>
        <div
          v-else-if="store.page.value === 'gatewaystats'"
          id="gateway-stats-panel"
          class="token-stats-panel"
          aria-labelledby="gatewaystats-nav"
        >
          <TokenStatsPage mode="proxy" />
        </div>
      </div>
    </div>
  </div>

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

  <div
    class="toast"
    id="toast"
    role="status"
    :class="{ visible: visible, error: isError }"
  >{{ message }}</div>

  <SiteFormModal />
  <LinkDialog />
  <PreviewDialog />
  <SyncSitesDialog />
  <ChromeSessionDialog />
  <SiteModelsDialog />
  <ConfirmDialog />
  <ComponentBootstrapDialog v-if="authState === 'ready'" />
  <SettingsPage />

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
