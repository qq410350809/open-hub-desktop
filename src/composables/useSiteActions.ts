import { openUrl } from "@tauri-apps/plugin-opener";
import { runCommand, useLibrary } from "./useLibrary";
import { useToast } from "./useToast";
import { useUIState } from "./useUIState";
import type { AddressItem, SiteRecord, SiteLinkKind } from "../types";

const isTauri = "__TAURI_INTERNALS__" in window;
const { loadLibrary } = useLibrary();
const { showToast } = useToast();
const { editingId, closeModal } = useUIState();

async function saveSite(input: SiteRecord): Promise<boolean> {
  try {
    if (editingId.value) {
      await runCommand<SiteRecord>("update_site", { id: editingId.value, input });
    } else {
      await runCommand<SiteRecord>("create_site", { input });
    }
    closeModal();
    await loadLibrary();
    showToast(editingId.value ? "站点已更新" : "站点已添加");
    return true;
  } catch (error) {
    showToast(String(error), true);
    return false;
  }
}

async function importSite(siteUrl: string): Promise<SiteRecord> {
  try {
    const site = await runCommand<SiteRecord>("import_site", { siteUrl });
    closeModal();
    await loadLibrary();
    showToast(`已导入「${site.name}」${site.systemType ? `（${site.systemType}）` : ""}`);
    return site;
  } catch (error) {
    showToast(`导入失败：${String(error)}`, true);
    throw error;
  }
}

async function deleteSite(site: SiteRecord) {
  if (!window.confirm(`确定删除"${site.name}"吗？此操作会永久移除本地记录。`)) return;
  try {
    await runCommand<void>("delete_site", { id: site.id });
    await loadLibrary();
    showToast("站点已删除");
  } catch (error) {
    showToast(String(error), true);
  }
}

async function togglePersonal(site: SiteRecord) {
  await runCommand<SiteRecord>("toggle_personal", { id: site.id });
  await loadLibrary();
}

async function togglePending(site: SiteRecord) {
  await runCommand<SiteRecord>("toggle_pending", { id: site.id });
  await loadLibrary();
}

async function cycleUsageState(site: SiteRecord) {
  try {
    await runCommand<SiteRecord>("cycle_usage_state", { id: site.id });
    await loadLibrary();
  } catch (error) {
    showToast(`状态更新失败：${String(error)}`, true);
  }
}

async function toggleRunaway(site: SiteRecord) {
  const wasRunaway = site.isRunaway;
  try {
    await runCommand<SiteRecord>("toggle_runaway", { id: site.id });
    await loadLibrary();
    showToast(wasRunaway ? "已恢复为存活站点" : "已移入跑路列表");
  } catch (error) {
    showToast(`状态更新失败：${String(error)}`, true);
  }
}

async function openExternal(url: string) {
  if (!url) return;
  try {
    if (isTauri) await openUrl(url);
    else window.open(url, "_blank", "noopener");
  } catch (error) {
    showToast(`无法打开链接：${String(error)}`, true);
  }
}

async function openExternalInChromeProfile(url: string, profileId: string) {
  if (!url) return;
  if (!profileId || !isTauri) {
    await openExternal(url);
    return;
  }
  try {
    await runCommand<void>("open_url_in_chrome_profile", { url, profileId });
  } catch (error) {
    showToast(`无法使用所选 Chrome 账户打开链接：${String(error)}`, true);
  }
}

async function copyAddress(url: string, label: string) {
  try {
    await navigator.clipboard.writeText(url);
    showToast(`${label}已复制`);
  } catch {
    showToast("复制失败，请手动复制", true);
  }
}

function addressItems(site: SiteRecord, kind: SiteLinkKind): AddressItem[] {
  if (kind === "api") return [{ label: "API 地址", url: site.apiBaseUrl }].filter((item) => item.url.trim());
  if (kind === "checkin")
    return [{ label: "签到地址", url: site.checkinUrl, note: site.checkinNote }].filter((item) => item.url.trim());
  if (kind === "benefit") return [{ label: "福利站地址", url: site.benefitUrl }].filter((item) => item.url.trim());
  if (kind === "status") return [{ label: "状态页地址", url: site.statusUrl }].filter((item) => item.url.trim());
  return site.extensionLinks
    .filter((item) => item.url.trim())
    .map((item) => ({ label: item.label.trim() || "扩展链接", url: item.url.trim() }));
}

function allAddressItems(site: SiteRecord): AddressItem[] {
  return (["api", "checkin", "benefit", "status", "extension"] as SiteLinkKind[]).flatMap((kind) =>
    addressItems(site, kind),
  );
}

export function useSiteActions() {
  return {
    saveSite,
    importSite,
    deleteSite,
    togglePersonal,
    togglePending,
    cycleUsageState,
    toggleRunaway,
    openExternal,
    openExternalInChromeProfile,
    copyAddress,
    addressItems,
    allAddressItems,
  };
}
