import { openUrl } from "@tauri-apps/plugin-opener";
import { runCommand, isTauri } from "../core/ipc";
import { useLibrary } from "./useLibrary";
import { useChromeSession } from "./useChromeSession";
import { useToast } from "../core/useToast";
import { useUIState } from "../ui/useUIState";
import { useConfirm } from "../ui/useConfirm";
import type {
  AddressItem,
  ChromeSessionInfo,
  OpenUrlInChromeSessionsResult,
  SiteRecord,
  SiteLinkKind,
  SiteUsageState,
} from "../../types";
import { systemTypeLabel } from "../../types";

const { loadLibrary } = useLibrary();
const { chromeUsageAccounts } = useChromeSession();
const { showToast } = useToast();
const { confirm } = useConfirm();
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

async function importSite(
  siteUrl: string,
  usageState: SiteUsageState = "all",
  useProxyPool = false,
): Promise<SiteRecord> {
  try {
    const site = await runCommand<SiteRecord>("import_site", { siteUrl, usageState, useProxyPool });
    closeModal();
    await loadLibrary();
    const usageLabel = usageState === "personal" ? "在用" : usageState === "pending" ? "待定" : "全部";
    showToast(`已导入「${site.name}」${site.systemType ? `（${systemTypeLabel(site.systemType)}）` : ""} · ${usageLabel}`);
    return site;
  } catch (error) {
    showToast(`导入失败：${String(error)}`, true);
    throw error;
  }
}

async function deleteSite(site: SiteRecord) {
  const accepted = await confirm({
    title: "删除站点",
    message: `确定删除「${site.name}」吗？此操作会永久移除本地记录。`,
    confirmText: "删除",
    danger: true,
  });
  if (!accepted) return;
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

async function setUsageState(site: SiteRecord, state: "personal" | "pending" | "unused") {
  try {
    await runCommand<SiteRecord>("set_usage_state", { id: site.id, state });
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

/** 站点关联的 Chrome 账号会话（与卡片 / 列表同口径：不按筛选、不按会话有效性过滤） */
function siteSessions(siteId: string): ChromeSessionInfo[] {
  if (!siteId) return [];
  return chromeUsageAccounts.value[siteId] ?? [];
}

/** 账号显示名：`用户名（账户名）`，缺失或与账号重名时退回单一名称；失效 / 异常会话额外标注。
 *
 *  后端在 Profile 名为 `Default` / `您的 Chrome` 之类时会用账号名顶替 profile_name
 *  （见 core/db.rs 的会话读取），直接拼接会出现 `a@b.com（a@b.com）` 这种重复文案。 */
function sessionLabel(session: ChromeSessionInfo): string {
  const account = session.username?.trim() || session.accountName.trim();
  const detail =
    account === session.username?.trim() && session.accountName.trim()
      ? session.accountName.trim()
      : session.profileName;
  const base = account
    ? detail && detail !== account
      ? `${account}（${detail}）`
      : account
    : session.profileName;
  if (!session.isValid) return `${base} · 会话失效`;
  if (session.syncError) return `${base} · 同步异常`;
  return base;
}

/** 用哪个 Chrome Profile 打开站点地址：显式选择 > 该站点第一个账号 > 空串。
 *
 *  规则：只要站点关联了账号就走账号的 Chrome Profile，**只有没有任何账号**时才回退
 *  系统默认浏览器 —— 单账号站点同样应该落在那个账号的浏览器里。 */
function preferredProfileId(siteId: string, chosenProfileId = ""): string {
  const sessions = siteSessions(siteId);
  // 显式选择必须是本站点自己的账号：陈旧 / 串站点的 profileId 一律忽略，退回第一个账号
  if (chosenProfileId && sessions.some((session) => session.profileId === chosenProfileId)) {
    return chosenProfileId;
  }
  return sessions[0]?.profileId ?? "";
}

/** 打开站点地址：有关联账号 → 该账号的 Chrome Profile；没有任何账号 → 系统默认浏览器 */
async function openSiteAddress(url: string, siteId: string, chosenProfileId = "") {
  if (!url) return;
  const profileId = preferredProfileId(siteId, chosenProfileId);
  if (profileId) {
    await openExternalInChromeProfile(url, profileId);
    return;
  }
  await openExternal(url);
}

function chromeSessionsOpenToast(result: OpenUrlInChromeSessionsResult): string {
  // 优先用账号缓存里的站点昵称（profileId 映射），没有缓存时回退邮箱/Profile 名。
  const nicknameByProfile = new Map<string, string>();
  for (const sessions of Object.values(chromeUsageAccounts.value)) {
    for (const session of sessions) {
      if (session.username) nicknameByProfile.set(session.profileId, session.username);
    }
  }
  const names = result.profiles
    .map((session) =>
      nicknameByProfile.get(session.profileId)
      || session.accountName.trim()
      || session.profileName.trim()
    )
    .filter(Boolean);
  const unique = [...new Set(names)];
  const suffix = unique.length ? `（${unique.join("、")}）` : "";
  if (result.errors.length && result.opened > 0) {
    return `已在 ${result.opened}/${result.attempted} 个会话浏览器中打开${suffix}`;
  }
  return `已在 ${result.opened} 个会话浏览器中打开${suffix}`;
}

async function openExternalInChromeSessions(url: string) {
  if (!url) return;
  if (!isTauri) {
    await openExternal(url);
    return;
  }
  try {
    const result = await runCommand<OpenUrlInChromeSessionsResult>("open_url_in_chrome_sessions", { url });
    if (!result.attempted) {
      await openExternal(url);
      return;
    }
    if (result.opened === 0) {
      showToast(result.errors[0] || "无法使用 Chrome 会话打开链接", true);
      await openExternal(url);
      return;
    }
    showToast(chromeSessionsOpenToast(result));
    if (result.errors.length) {
      showToast(`部分账号未能打开：${result.errors.join("；")}`, true);
    }
  } catch (error) {
    showToast(`无法按会话打开链接：${String(error)}`, true);
    await openExternal(url);
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
    setUsageState,
    toggleRunaway,
    openExternal,
    siteSessions,
    sessionLabel,
    preferredProfileId,
    openSiteAddress,
    openExternalInChromeProfile,
    openExternalInChromeSessions,
    copyAddress,
    addressItems,
    allAddressItems,
  };
}
