import { ref, computed, type ComputedRef } from "vue";
import { useLibrary } from "../site/useLibrary";
import type { SiteRecord, SiteLinkKind } from "../../types";

const { sites } = useLibrary();

// 页面状态（tokenstats = 本地客户端统计；gatewaystats = 反代网关服务端统计）
const page = ref<"library" | "modelparams" | "modelproxy" | "modeltest" | "charity" | "proxy" | "tokenstats" | "gatewaystats" | "settings">("tokenstats");
const editingId = ref<string | null>(null);
const activeTab = ref<"basic" | "features" | "maintenance">("basic");

// 弹窗状态
const modalOpen = ref(false);
const linkDialogOpen = ref(false);
const previewDialogOpen = ref(false);
const chromeSessionDialogOpen = ref(false);
const syncDialogOpen = ref(false);
const siteModelsDialogOpen = ref(false);

// 链接弹窗数据
const linkDialogKind = ref<SiteLinkKind>("api");
const linkDialogSite = ref<SiteRecord | null>(null);
const linkDialogTrigger = ref<HTMLElement | null>(null);

// 预览弹窗数据
const previewSite = ref<SiteRecord | null>(null);
const previewTrigger = ref<HTMLElement | null>(null);

// 站点模型弹窗数据
const siteModelsSite = ref<SiteRecord | null>(null);

const editingSite: ComputedRef<SiteRecord | null> = computed(() =>
  editingId.value ? sites.value.find((site) => site.id === editingId.value) ?? null : null,
);

function openSettings() { page.value = "settings"; }
function closeSettings() { page.value = "tokenstats"; }
function openModelParams() { page.value = "modelparams"; }
function openModelProxy() { page.value = "modelproxy"; }
function openModelTest() { page.value = "modeltest"; }
function openLibrary() { page.value = "library"; }
function openCharityMonitor() { page.value = "charity"; }
function openProxyPool() { page.value = "proxy"; }
function openTokenStats() { page.value = "tokenstats"; }
function openGatewayStats() { page.value = "gatewaystats"; }

function openSiteModelsDialog(site: SiteRecord) {
  siteModelsSite.value = site;
  siteModelsDialogOpen.value = true;
}

function closeSiteModelsDialog() {
  siteModelsDialogOpen.value = false;
  siteModelsSite.value = null;
}

function openModal(site?: SiteRecord) {
  editingId.value = site?.id ?? null;
  activeTab.value = "basic";
  modalOpen.value = true;
}

function closeModal() {
  modalOpen.value = false;
  editingId.value = null;
}

function openLinkDialog(site: SiteRecord, kind: SiteLinkKind, trigger: HTMLElement) {
  linkDialogSite.value = site;
  linkDialogKind.value = kind;
  linkDialogTrigger.value = trigger;
  linkDialogOpen.value = true;
}

function closeLinkDialog() {
  linkDialogOpen.value = false;
  linkDialogSite.value = null;
  linkDialogTrigger.value?.focus();
  linkDialogTrigger.value = null;
}

function openPreview(site: SiteRecord, trigger: HTMLElement) {
  previewSite.value = site;
  previewTrigger.value = trigger;
  previewDialogOpen.value = true;
}

function closePreview() {
  previewDialogOpen.value = false;
  previewSite.value = null;
  previewTrigger.value?.focus();
  previewTrigger.value = null;
}

export function useUIState() {
  return {
    page,
    editingId,
    activeTab,
    modalOpen,
    linkDialogOpen,
    previewDialogOpen,
    chromeSessionDialogOpen,
    syncDialogOpen,
    siteModelsDialogOpen,
    linkDialogKind,
    linkDialogSite,
    linkDialogTrigger,
    previewSite,
    previewTrigger,
    siteModelsSite,
    editingSite,
    openSettings,
    closeSettings,
    openModelParams,
    openModelProxy,
    openModelTest,
    openLibrary,
    openCharityMonitor,
    openProxyPool,
    openTokenStats,
    openGatewayStats,
    openSiteModelsDialog,
    closeSiteModelsDialog,
    openModal,
    closeModal,
    openLinkDialog,
    closeLinkDialog,
    openPreview,
    closePreview,
  };
}
