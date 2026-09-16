<script setup lang="ts">
import { onMounted, onUnmounted, ref, computed, onErrorCaptured } from "vue";
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
import { useCharityNotification } from "./composables/useCharityNotification";
import AppSidebar from "./components/layout/AppSidebar.vue";
import SiteLibraryPage from "./components/pages/SiteLibraryPage.vue";
import SiteFormModal from "./components/site/SiteFormModal.vue";
import LinkDialog from "./components/common/LinkDialog.vue";
import PreviewDialog from "./components/common/PreviewDialog.vue";
import SettingsPage from "./components/pages/SettingsPage.vue";
import SyncSitesDialog from "./components/site/SyncSitesDialog.vue";
import ChromeSessionDialog from "./components/site/ChromeSessionDialog.vue";
import SiteModelsDialog from "./components/site/SiteModelsDialog.vue";
import SiteModelTestDialog from "./components/site/SiteModelTestDialog.vue";
import ConfirmDialog from "./components/common/ConfirmDialog.vue";
import ComponentBootstrapDialog from "./components/common/ComponentBootstrapDialog.vue";
import CharityMonitorPage from "./components/pages/CharityMonitorPage.vue";
import ProxyPoolPage from "./components/pages/ProxyPoolPage.vue";
import TokenStatsPage from "./components/pages/TokenStatsPage.vue";
import ModelCatalogPage from "./components/pages/ModelCatalogPage.vue";
import ModelProxyPage from "./components/pages/ModelProxyPage.vue";
import LocalToolsPage from "./components/pages/LocalToolsPage.vue";

const store = useStore();
const { preferences } = usePreferences();
const { applyTheme } = useTheme();
const { message, isError, visible } = useToast();
// 公益监听通知监听注册在根组件：页面是 v-if 条件挂载，放页面里切走就失效
useCharityNotification();
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
let menuUnlisteners: UnlistenFn[] = [];

const pageError = ref<{ message: string; stack?: string } | null>(null);

onErrorCaptured((err, _instance, info) => {
  console.error("[OpenHub] 页面渲染异常已捕获：", err, info);
  pageError.value = {
    message: err instanceof Error ? err.message : String(err),
    stack: err instanceof Error ? err.stack : undefined,
  };
  return false; // 阻止异常向上传播导致根应用卸载白屏
});

function reloadApp() {
  window.location.reload();
}

function dismissPageError() {
  pageError.value = null;
  store.openTokenStats();
}

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
    else if (store.siteTestDialogOpen.value) store.closeSiteTestDialog();
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
  else if (page === "localtools") store.openLocalTools();
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
        <div
          v-else-if="store.page.value === 'localtools'"
          id="local-tools-panel"
          class="local-tools-panel"
          aria-labelledby="localtools-nav"
        >
          <LocalToolsPage />
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
  <SiteModelTestDialog />
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

  <!-- 全局错误边界保护弹窗：防止任何子页面异常导致白屏崩溃 -->
  <div v-if="pageError" class="oh-error-boundary-mask">
    <div class="oh-error-boundary-dialog" role="alertdialog" aria-modal="true">
      <div class="oh-error-header">
        <span class="oh-error-badge">⚠️ 页面渲染保护</span>
        <button type="button" class="close-button" aria-label="关闭" @click="dismissPageError">&times;</button>
      </div>
      <div class="oh-error-body">
        <p class="oh-error-title">该模块在渲染计算时遇到未捕获异常，已成功拦截以保护应用正常运行：</p>
        <pre class="oh-error-pre">{{ pageError.message }}</pre>
        <pre v-if="pageError.stack" class="oh-error-stack">{{ pageError.stack }}</pre>
      </div>
      <div class="oh-error-footer">
        <button type="button" class="btn btn-secondary" @click="reloadApp">重新加载客户端</button>
        <button type="button" class="btn btn-primary" @click="dismissPageError">返回本地统计页</button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.oh-error-boundary-mask {
  position: fixed;
  inset: 0;
  z-index: 99999;
  background: rgba(0, 0, 0, 0.65);
  backdrop-filter: blur(4px);
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
}
.oh-error-boundary-dialog {
  background: var(--bg-card, #1c1d21);
  border: 1px solid var(--border-color, #2e3038);
  border-radius: 12px;
  max-width: 640px;
  width: 100%;
  box-shadow: 0 16px 40px rgba(0, 0, 0, 0.5);
  overflow: hidden;
  display: flex;
  flex-direction: column;
}
.oh-error-header {
  padding: 16px 20px;
  border-bottom: 1px solid var(--border-color, #2e3038);
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.oh-error-badge {
  font-size: 14px;
  font-weight: 600;
  color: #ff9800;
}
.oh-error-body {
  padding: 20px;
  max-height: 60vh;
  overflow-y: auto;
}
.oh-error-title {
  font-size: 13px;
  color: var(--text-secondary, #a0a5b5);
  margin-bottom: 12px;
}
.oh-error-pre {
  background: rgba(255, 68, 68, 0.1);
  border: 1px solid rgba(255, 68, 68, 0.25);
  color: #ff6b6b;
  padding: 12px;
  border-radius: 6px;
  font-size: 12px;
  white-space: pre-wrap;
  word-break: break-all;
  font-family: monospace;
}
.oh-error-stack {
  margin-top: 8px;
  background: rgba(0, 0, 0, 0.3);
  padding: 10px;
  border-radius: 6px;
  font-size: 11px;
  color: var(--text-muted, #727788);
  white-space: pre-wrap;
  word-break: break-all;
  font-family: monospace;
  max-height: 160px;
  overflow-y: auto;
}
.oh-error-footer {
  padding: 14px 20px;
  border-top: 1px solid var(--border-color, #2e3038);
  display: flex;
  justify-content: flex-end;
  gap: 12px;
}
</style>
