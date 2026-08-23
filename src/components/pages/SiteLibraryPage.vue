<script setup lang="ts">
import { capabilities } from "../../composables/core/capabilities";
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { icons } from "../../icons";
import { formatDate, formatRateLimit, logoText } from "../../utils";
import { useStore } from "../../composables/useStore";
import { runCommand, isTauri } from "../../composables/useLibrary";
import AppTable, { type AppTableColumn } from "../common/AppTable.vue";
import CustomSelect from "../common/CustomSelect.vue";
import type {
  ChromeSessionInfo,
  SiteModelCache,
  SiteModelItem,
  SiteRecord,
} from "../../types";
import {
  SYSTEM_TYPES,
  isNewApiCompatible,
  normalizeSystemType,
  systemTypeLabel,
} from "../../types";

const store = useStore();

// —— 视图模式 ——
type ViewMode = "cards" | "table" | "topology";
const currentView = ref<ViewMode>("cards");

// —— 搜索与筛选 ——
const query = ref("");
const selectedUsageTab = ref<"all" | "personal" | "pending">("all");
const selectedAliveTab = ref<"all" | "active" | "runaway">("all");
const selectedSystemType = ref("all");
const selectedLevel = ref("all");
const selectedTag = ref("all");
const sortBy = ref("default");
const popularChip = ref("all");

// —— 特性能力快捷开关 ——
const featureFilters = ref({
  checkin: false,
  translation: false,
  ldc: false,
  nsfw: false,
  invite: false,
  hasKeys: false,
  checkedInToday: false,
  syncError: false,
  proxyPool: false,
});

function toggleFeature(key: keyof typeof featureFilters.value) {
  featureFilters.value[key] = !featureFilters.value[key];
  currentPage.value = 1;
}

// —— 分页与排序 ——
const currentPage = ref(1);
const pageSize = ref(36);
const tableSorting = ref<Array<{ id: string; desc: boolean }>>([]);

// —— 批量多选与对比池 ——
const batchSelectedIds = ref<string[]>([]);

function toggleSelectSite(id: string) {
  const idx = batchSelectedIds.value.indexOf(id);
  if (idx >= 0) {
    batchSelectedIds.value.splice(idx, 1);
  } else {
    batchSelectedIds.value.push(id);
  }
}

function clearBatchSelection() {
  batchSelectedIds.value = [];
}

// —— 全景详情深度抽屉 (Slide-over Drawer) ——
const selectedSiteId = ref("");
const activeDetailTab = ref<"overview" | "accounts" | "models" | "raw">("overview");
const drawerModelCache = ref<SiteModelCache | null>(null);
const drawerModelsLoading = ref(false);
const drawerModelsError = ref("");
const drawerModelSearch = ref("");
const drawerSelectedKey = ref<string | null>(null);
const drawerLiveFetchingKind = ref<"keys" | "models" | null>(null);
const idCopied = ref(false);

const selectedSite = computed<SiteRecord | null>(() => {
  if (!selectedSiteId.value) return null;
  return store.sites.value.find((s) => s.id === selectedSiteId.value) ?? null;
});

// —— 快捷热门芯片列表 (仅展示有数值 count > 0 的项，支持换行与展开/收起) ——
const isChipsExpanded = ref(false);
const CHIPS_COLLAPSED_LIMIT = 8;

const popularChipsList = computed(() => {
  const allChips = [
    { id: "all", label: "全部", count: store.sites.value.length },
    { id: "newapi", label: "NewAPI", count: countBySystem("newapi") + countBySystem("newapi2") },
    { id: "sub2api", label: "Sub2API", count: countBySystem("sub2api") },
    { id: "oneapi", label: "One API", count: countBySystem("one-api") },
    { id: "sub2one", label: "Sub2One", count: countBySystem("sub2one") },
    { id: "tag_free", label: "公益 / 免费", count: countByTag("免费") + countByTag("公益") },
    { id: "tag_official", label: "官转直连", count: countByTag("官转") },
    { id: "feat_trans", label: "沉浸式翻译", count: store.sites.value.filter((s) => s.supportsImmersiveTranslation).length },
    { id: "feat_ldc", label: "LDC", count: store.sites.value.filter((s) => s.supportsLdc).length },
    { id: "feat_checkin", label: "每日签到", count: store.sites.value.filter((s) => s.supportsCheckin).length },
    { id: "feat_nsfw", label: "18+ NSFW", count: store.sites.value.filter((s) => s.supportsNsfw).length },
    { id: "feat_invite", label: "需要邀请码", count: store.sites.value.filter((s) => s.requiresInviteCode).length },
  ];
  // 必须要有值 (count > 0)
  return allChips.filter((chip) => chip.count > 0);
});

const visibleChips = computed(() => {
  if (isChipsExpanded.value || popularChipsList.value.length <= CHIPS_COLLAPSED_LIMIT) {
    return popularChipsList.value;
  }
  return popularChipsList.value.slice(0, CHIPS_COLLAPSED_LIMIT);
});

const hiddenChipsCount = computed(() =>
  Math.max(0, popularChipsList.value.length - CHIPS_COLLAPSED_LIMIT)
);

function countBySystem(sys: string): number {
  const norm = normalizeSystemType(sys);
  return store.sites.value.filter((s) => normalizeSystemType(s.systemType) === norm).length;
}

function countByTag(tagName: string): number {
  return store.sites.value.filter((s) => s.tags.includes(tagName)).length;
}

function selectPopularChip(chipId: string) {
  if (popularChip.value === chipId) {
    popularChip.value = "all";
  } else {
    popularChip.value = chipId;
  }
  currentPage.value = 1;
}

// —— 宏观驾驶舱 4 大核心指标 ——
const metrics = computed(() => {
  const allSites = store.sites.value;
  const totalSites = allSites.length;
  const activeSites = allSites.filter((s) => !s.isRunaway).length;
  const personalSites = allSites.filter((s) => s.isPersonal).length;
  const pendingSites = allSites.filter((s) => s.isPending).length;
  const runawaySites = allSites.filter((s) => s.isRunaway).length;

  // 账号会话与令牌统计
  let totalAccounts = 0;
  let accountsWithTokens = 0;
  let checkedInAccounts = 0;
  let errorAccounts = 0;
  let totalQuotaNumber = 0;
  let accountsWithQuota = 0;

  const usageMap = store.chromeUsageAccounts.value;
  for (const siteId of Object.keys(usageMap)) {
    const sessions = usageMap[siteId] ?? [];
    for (const session of sessions) {
      totalAccounts += 1;
      if (session.hasAccessToken) accountsWithTokens += 1;
      if (session.checkedInToday) checkedInAccounts += 1;
      if (session.syncError || session.checkinError) errorAccounts += 1;
      if (session.remaining !== null && Number.isFinite(session.remaining)) {
        totalQuotaNumber += session.remaining;
        accountsWithQuota += 1;
      }
    }
  }

  const totalQuotaText = accountsWithQuota > 0
    ? `¥ ${new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 2 }).format(totalQuotaNumber)}`
    : "未读取额度";

  // 架构分布
  const newApiCount = allSites.filter((s) => isNewApiCompatible(s.systemType)).length;
  const sub2ApiCount = allSites.filter((s) => normalizeSystemType(s.systemType) === "sub2api").length;

  return {
    totalSites,
    activeSites,
    personalSites,
    pendingSites,
    runawaySites,
    totalAccounts,
    accountsWithTokens,
    checkedInAccounts,
    errorAccounts,
    totalQuotaNumber,
    accountsWithQuota,
    totalQuotaText,
    newApiCount,
    sub2ApiCount,
  };
});

// —— 分类状态 Tab 计数 ——
const tabCounts = computed(() => {
  const allSites = store.sites.value;
  return {
    all: allSites.length,
    personal: allSites.filter((s) => s.isPersonal).length,
    pending: allSites.filter((s) => s.isPending).length,
    active: allSites.filter((s) => !s.isRunaway).length,
    runaway: allSites.filter((s) => s.isRunaway).length,
  };
});

// —— 下拉筛选选项 ——
const systemTypeOptions = computed(() => [
  { value: "all", text: "全部系统架构" },
  ...SYSTEM_TYPES.map((st) => {
    const count = countBySystem(st.value);
    return { value: st.value, text: `${st.text} (${count})` };
  }),
  { value: "unknown", text: `其他 / 未知 (${store.sites.value.filter((s) => !s.systemType).length})` },
]);

const levelOptions = [
  { value: "all", text: "全部注册门槛" },
  { value: "0", text: "LV0 · 无限制开放" },
  { value: "1", text: "LV1 · 需基础账号" },
  { value: "2", text: "LV2 · 进阶门槛" },
  { value: "3", text: "LV3+ · 高限制门槛" },
];

const tagOptions = computed(() => {
  const allTagsSet = new Set<string>();
  for (const s of store.sites.value) {
    for (const t of s.tags) if (t.trim()) allTagsSet.add(t.trim());
  }
  return [
    { value: "all", text: "全部标签分类" },
    ...Array.from(allTagsSet).sort().map((tag) => {
      const count = countByTag(tag);
      return { value: tag, text: `${tag} (${count})` };
    }),
  ];
});

const sortOptions = [
  { value: "default", text: "默认综合排序 (在用/活跃优先)" },
  { value: "updated_desc", text: "最近更新时间 (从新到旧)" },
  { value: "level_asc", text: "注册等级门槛 (从低到高)" },
  { value: "level_desc", text: "注册等级门槛 (从高到低)" },
  { value: "name_asc", text: "站点名称 (A → Z)" },
];

// —— 核心过滤与排序计算 ——
const filteredSites = computed(() => {
  const term = query.value.trim().toLocaleLowerCase("zh-CN");
  let list = store.sites.value.filter((site) => {
    // 1. 使用状态维度 (Dimension 1: Usage state)
    if (selectedUsageTab.value === "personal" && !site.isPersonal) return false;
    if (selectedUsageTab.value === "pending" && !site.isPending) return false;

    // 2. 站点存活与健康维度 (Dimension 2: Alive / Operational state)
    if (selectedAliveTab.value === "active" && site.isRunaway) return false;
    if (selectedAliveTab.value === "runaway" && !site.isRunaway) return false;

    // 2. 热门芯片
    if (popularChip.value === "newapi" && !isNewApiCompatible(site.systemType)) return false;
    if (popularChip.value === "sub2api" && normalizeSystemType(site.systemType) !== "sub2api") return false;
    if (popularChip.value === "oneapi" && normalizeSystemType(site.systemType) !== "oneapi") return false;
    if (popularChip.value === "sub2one" && normalizeSystemType(site.systemType) !== "sub2one") return false;
    if (popularChip.value === "tag_free" && !site.tags.includes("免费") && !site.tags.includes("公益")) return false;
    if (popularChip.value === "tag_official" && !site.tags.includes("官转")) return false;
    if (popularChip.value === "feat_trans" && !site.supportsImmersiveTranslation) return false;
    if (popularChip.value === "feat_ldc" && !site.supportsLdc) return false;
    if (popularChip.value === "feat_checkin" && !site.supportsCheckin) return false;
    if (popularChip.value === "feat_nsfw" && !site.supportsNsfw) return false;
    if (popularChip.value === "feat_invite" && !site.requiresInviteCode) return false;

    // 3. 系统类型
    if (selectedSystemType.value !== "all") {
      const siteNorm = normalizeSystemType(site.systemType);
      if (selectedSystemType.value === "unknown") {
        if (site.systemType && siteNorm) return false;
      } else {
        if (siteNorm !== normalizeSystemType(selectedSystemType.value)) return false;
      }
    }

    // 4. 等级门槛
    if (selectedLevel.value !== "all") {
      const lvl = Number(selectedLevel.value);
      if (lvl >= 3) {
        if (site.registrationLimit < 3) return false;
      } else {
        if (site.registrationLimit !== lvl) return false;
      }
    }

    // 5. 标签
    if (selectedTag.value !== "all" && !site.tags.includes(selectedTag.value)) return false;

    // 6. 特性开关
    if (featureFilters.value.checkin && !site.supportsCheckin) return false;
    if (featureFilters.value.translation && !site.supportsImmersiveTranslation) return false;
    if (featureFilters.value.ldc && !site.supportsLdc) return false;
    if (featureFilters.value.nsfw && !site.supportsNsfw) return false;
    if (featureFilters.value.invite && !site.requiresInviteCode) return false;
    if (featureFilters.value.proxyPool && !site.useProxyPool) return false;

    const sessions = store.chromeUsageAccounts.value[site.id] ?? [];
    if (featureFilters.value.hasKeys) {
      const hasKey = sessions.some((s) => s.apiKeyCount > 0);
      if (!hasKey) return false;
    }
    if (featureFilters.value.checkedInToday) {
      const checked = sessions.some((s) => s.checkedInToday);
      if (!checked) return false;
    }
    if (featureFilters.value.syncError) {
      const hasErr = sessions.some((s) => s.syncError || s.checkinError || (s.apiSyncError && !s.apiKeyCount && !s.apiModelCount));
      if (!hasErr) return false;
    }

    // 7. 搜索关键词
    if (!term) return true;
    const haystack = [
      site.name,
      site.apiBaseUrl,
      site.description,
      site.rateLimit,
      systemTypeLabel(site.systemType),
      ...site.tags,
      ...site.maintainers.map((m) => `${m.name} ${m.username || ""} ${m.id || ""}`),
    ]
      .join(" ")
      .toLocaleLowerCase("zh-CN");

    return haystack.includes(term);
  });

  // 8. 排序逻辑
  if (sortBy.value === "updated_desc") {
    list = [...list].sort((a, b) => new Date(b.updatedAt).getTime() - new Date(a.updatedAt).getTime());
  } else if (sortBy.value === "level_asc") {
    list = [...list].sort((a, b) => a.registrationLimit - b.registrationLimit);
  } else if (sortBy.value === "level_desc") {
    list = [...list].sort((a, b) => b.registrationLimit - a.registrationLimit);
  } else if (sortBy.value === "name_asc") {
    list = [...list].sort((a, b) => a.name.localeCompare(b.name, "zh-CN"));
  } else {
    // 默认综合排序：拓扑视图下优先展示包含账号数据的站点；再按在用 > 待定 > 存活，更新时间
    list = [...list].sort((a, b) => {
      if (currentView.value === "topology") {
        const hasAccA = (store.chromeUsageAccounts.value[a.id]?.length ?? 0) > 0 ? 1 : 0;
        const hasAccB = (store.chromeUsageAccounts.value[b.id]?.length ?? 0) > 0 ? 1 : 0;
        if (hasAccA !== hasAccB) return hasAccB - hasAccA;
      }
      const scoreA = (a.isPersonal ? 100 : a.isPending ? 50 : 0) - (a.isRunaway ? 200 : 0);
      const scoreB = (b.isPersonal ? 100 : b.isPending ? 50 : 0) - (b.isRunaway ? 200 : 0);
      if (scoreA !== scoreB) return scoreB - scoreA;
      return new Date(b.updatedAt).getTime() - new Date(a.updatedAt).getTime();
    });
  }

  return list;
});

// —— 分页切片 ——
const paginatedSites = computed(() => {
  const start = (currentPage.value - 1) * pageSize.value;
  return filteredSites.value.slice(start, start + pageSize.value);
});

const totalPages = computed(() => Math.max(1, Math.ceil(filteredSites.value.length / pageSize.value)));

// —— 全景表格列配置 ——
const tableColumns = computed<AppTableColumn[]>(() => [
  { key: "siteInfo", title: "站点标识 / 架构", width: "auto", sortable: true },
  { key: "systemType", title: "系统架构", width: "120px", align: "center" as const, sortable: true },
  { key: "accounts", title: "账号与会话", width: "125px", align: "center" as const, sortable: false },
  { key: "quota", title: "剩余额度", width: "120px", align: "right" as const, sortable: true },
  { key: "regLevel", title: "注册等级", width: "85px", align: "center" as const, sortable: true },
  { key: "rateLimit", title: "速率限制", width: "105px", align: "center" as const, sortable: true },
  { key: "capabilities", title: "特性能力", width: "120px", align: "center" as const, sortable: false },
  { key: "updatedAt", title: "更新时间", width: "135px", align: "right" as const, sortable: true },
  { key: "actions", title: "操作", width: "105px", align: "right" as const, sortable: false },
]);

// —— 辅助格式化方法 ——
function getSiteSessions(siteId: string): ChromeSessionInfo[] {
  return store.chromeUsageAccounts.value[siteId] ?? [];
}

function getSiteErrorMessage(siteId: string): string {
  const sessions = getSiteSessions(siteId);
  for (const s of sessions) {
    if (s.syncError) return s.syncError;
    if (s.checkinError) return s.checkinError;
    if (s.apiSyncError && !s.apiKeyCount && !s.apiModelCount) return s.apiSyncError;
  }
  return "";
}

function getSiteQuotaText(siteId: string): string {
  const sessions = getSiteSessions(siteId);
  if (!sessions.length) return "未关联";
  const validWithQuota = sessions.filter((s) => s.remaining !== null && Number.isFinite(s.remaining));
  if (!validWithQuota.length) return "未读取";
  const total = validWithQuota.reduce((acc, cur) => acc + (cur.remaining ?? 0), 0);
  const unit = validWithQuota[0]?.unit || "";
  const num = new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 2 }).format(total);
  return unit ? `${num} ${unit}` : num;
}

function formatAccountQuota(remaining: number | null | undefined, unit?: string): string {
  if (remaining === null || remaining === undefined || !Number.isFinite(remaining)) return "未同步额度";
  const num = new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 2 }).format(remaining);
  return unit ? `${num} ${unit}` : num;
}

function systemTone(raw: string): string {
  const norm = normalizeSystemType(raw);
  if (norm.includes("newapi")) return "brand";
  if (norm.includes("sub2api")) return "violet";
  if (norm.includes("oneapi") || norm.includes("onehub")) return "info";
  if (norm.includes("openai") || norm.includes("claude") || norm.includes("gemini")) return "success";
  return "neutral";
}

function siteInitials(apiBaseUrl: string, name: string): string {
  return logoText(apiBaseUrl, name).slice(0, 3).toUpperCase();
}

function hasActiveFilters(): boolean {
  return (
    Boolean(query.value.trim()) ||
    selectedUsageTab.value !== "all" ||
    selectedAliveTab.value !== "all" ||
    popularChip.value !== "all" ||
    selectedSystemType.value !== "all" ||
    selectedLevel.value !== "all" ||
    selectedTag.value !== "all" ||
    sortBy.value !== "default" ||
    Object.values(featureFilters.value).some(Boolean)
  );
}

function resetAllFilters() {
  query.value = "";
  selectedUsageTab.value = "all";
  selectedAliveTab.value = "all";
  popularChip.value = "all";
  selectedSystemType.value = "all";
  selectedLevel.value = "all";
  selectedTag.value = "all";
  sortBy.value = "default";
  for (const k of Object.keys(featureFilters.value) as Array<keyof typeof featureFilters.value>) {
    featureFilters.value[k] = false;
  }
  currentPage.value = 1;
}

// —— 抽屉交互逻辑 ——
async function openModelDetail(site: SiteRecord) {
  selectedSiteId.value = site.id;
  activeDetailTab.value = "overview";
  drawerModelSearch.value = "";
  drawerSelectedKey.value = null;
  drawerModelsError.value = "";
  drawerModelCache.value = null;
  await readDrawerModelCache(site.id);
}

function closeDetail() {
  selectedSiteId.value = "";
  drawerModelCache.value = null;
}

async function readDrawerModelCache(siteId: string) {
  if (!isTauri) return;
  drawerModelsLoading.value = true;
  try {
    const data = await runCommand<SiteModelCache>("get_site_model_cache", { siteId });
    if (data) {
      drawerModelCache.value = data;
    }
  } catch (err) {
    drawerModelsError.value = String(err);
  } finally {
    drawerModelsLoading.value = false;
  }
}

async function triggerDrawerSync(mode: "keys" | "models") {
  if (!selectedSite.value) return;
  const site = selectedSite.value;
  drawerLiveFetchingKind.value = mode;
  try {
    const sessions = getSiteSessions(site.id);
    let baseUrl = site.apiBaseUrl.trim();
    if (!baseUrl.endsWith("/")) baseUrl += "/";

    if (sessions.length === 0) {
      const result = await runCommand<any>("fetch_site_models_json", {
        url: baseUrl,
        siteId: site.id,
      });
      // 同步 Key 成功获取数据后，保存前清理掉这个站点原来的对应旧数据，避免数据冲突
      if (mode === "keys") {
        await runCommand("clear_site_model_cache_for_site", { siteId: site.id });
      }
      await runCommand("save_site_model_cache_for_account", {
        siteId: site.id,
        account: {
          profileId: "",
          profileName: "",
          accountName: "",
          username: "",
          keys: result.keys ?? [],
          keyGroups: result.keyGroups ?? {},
          keyModels: result.keyModels ?? {},
          error: "",
        },
        result,
        preserveKeys: mode === "models",
      });
    } else {
      let clearedOldSiteData = false;
      for (const session of sessions) {
        if (mode === "models" && (!session.apiKeyCount || session.apiKeyCount === 0)) continue;
        try {
          const result = await runCommand<any>("fetch_site_models_json", {
            url: baseUrl,
            siteId: site.id,
            profileId: session.profileId,
          });
          // 同步 Key 成功获取数据后，首次保存前清理掉这个站点原来的对应旧数据，避免数据冲突
          if (mode === "keys" && !clearedOldSiteData) {
            await runCommand("clear_site_model_cache_for_site", { siteId: site.id });
            clearedOldSiteData = true;
          }
          await runCommand("save_site_model_cache_for_account", {
            siteId: site.id,
            account: {
              profileId: session.profileId,
              profileName: session.profileName,
              accountName: session.accountName,
              username: session.username,
              keys: result.keys ?? [],
              keyGroups: result.keyGroups ?? {},
              keyModels: result.keyModels ?? {},
              error: "",
            },
            result,
            preserveKeys: mode === "models",
          });
        } catch (err) {
          console.warn("同步账号模型失败:", err);
        }
      }
    }
    await readDrawerModelCache(site.id);
    store.showToast(mode === "keys" ? "Key 与支持模型已同步完成" : "模型列表已同步完成");
  } catch (err) {
    store.showToast(`同步失败：${String(err)}`, true);
  } finally {
    drawerLiveFetchingKind.value = null;
  }
}

async function handleTopologyBatchModelSync() {
  if (store.syncingModelKeys.value) return;
  const siteIds = filteredSites.value.map((s) => s.id);
  store.showToast("开始根据 API Key 批量同步模型…");
  await store.syncAllModelKeys(siteIds);
}

async function copyText(text: string, label = "内容") {
  await store.copyAddress(text, label);
  idCopied.value = true;
  setTimeout(() => (idCopied.value = false), 2000);
}

async function copySessionCookie(site: SiteRecord, session: ChromeSessionInfo) {
  const url = site.checkinUrl?.trim() || site.apiBaseUrl?.trim() || "";
  if (!url) {
    store.showToast("该站点地址无效", true);
    return;
  }
  try {
    const value = await runCommand<{ cookie: string; cookieCount: number; profileName: string }>("read_chrome_session", {
      url,
      profileId: session.profileId,
    });
    if (value?.cookie) {
      await navigator.clipboard.writeText(value.cookie);
      store.showToast(`已复制「${value.profileName}」的 ${value.cookieCount} 个 Cookie 到剪贴板`);
    } else {
      store.showToast("未检测到有效 Cookie", true);
    }
  } catch (err) {
    store.showToast(`读取 Cookie 失败: ${String(err)}`, true);
  }
}

function maskApiKey(key: string): string {
  const value = key.trim();
  if (!value) return "—";
  if (value.length <= 8) return "••••••••";
  const prefix = value.startsWith("sk-") ? 7 : 4;
  const suffix = Math.min(4, Math.max(2, Math.floor(value.length / 8)));
  return `${value.slice(0, prefix)}••••••••${value.slice(-suffix)}`;
}

const drawerFilteredModels = computed<SiteModelItem[]>(() => {
  if (!drawerModelCache.value) return [];
  let list = drawerModelCache.value.models || [];
  if (drawerSelectedKey.value) {
    for (const acc of drawerModelCache.value.accounts || []) {
      if (acc.keys.includes(drawerSelectedKey.value)) {
        const km = acc.keyModels?.[drawerSelectedKey.value];
        if (Array.isArray(km)) {
          list = km;
          break;
        }
      }
    }
  }
  const q = drawerModelSearch.value.trim().toLowerCase();
  if (!q) return list;
  return list.filter((m) => m.id.toLowerCase().includes(q) || (m.ownedBy && m.ownedBy.toLowerCase().includes(q)));
});

// —— 批量操作 ——
async function batchSetUsage(state: "personal" | "pending" | "unused") {
  if (!batchSelectedIds.value.length) return;
  for (const id of batchSelectedIds.value) {
    const site = store.sites.value.find((s) => s.id === id);
    if (site) await store.setUsageState(site, state);
  }
  store.showToast(`已批量更新 ${batchSelectedIds.value.length} 个站点状态`);
  clearBatchSelection();
}

async function batchToggleRunaway() {
  if (!batchSelectedIds.value.length) return;
  for (const id of batchSelectedIds.value) {
    const site = store.sites.value.find((s) => s.id === id);
    if (site) await store.toggleRunaway(site);
  }
  store.showToast(`已批量更新 ${batchSelectedIds.value.length} 个站点存活状态`);
  clearBatchSelection();
}

async function batchSyncSession() {
  if (!batchSelectedIds.value.length) return;
  const selectedSites = store.sites.value.filter((s) => batchSelectedIds.value.includes(s.id));
  if (!selectedSites.length) return;

  if (selectedSites.length === 1) {
    store.syncChromeSession(selectedSites[0], document.body);
  } else {
    const hasPending = selectedSites.some((s) => s.isPending);
    store.openSyncDialog("quota", hasPending ? "pending" : "personal", batchSelectedIds.value);
  }
  clearBatchSelection();
}

// —— 键盘快捷键 (⌘ K 聚焦搜索) ——
function handleGlobalKeydown(e: KeyboardEvent) {
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
    e.preventDefault();
    const searchInput = document.getElementById("sl-search-input") as HTMLInputElement | null;
    searchInput?.focus();
    searchInput?.select();
  }
}

watch(selectedUsageTab, (tab) => {
  if (tab === "personal" || tab === "pending") {
    store.setUsageFilter(tab);
  } else {
    store.setUsageFilter("all");
  }
});

watch(selectedAliveTab, (tab) => {
  if (tab === "runaway") {
    store.setRunawayFilter("runaway");
  } else if (tab === "active") {
    store.setRunawayFilter("active");
  } else {
    store.setRunawayFilter("all");
  }
});

watch(
  [query, selectedUsageTab, selectedAliveTab, selectedSystemType, selectedLevel, selectedTag, sortBy, popularChip, currentView],
  () => {
    currentPage.value = 1;
  },
);

onMounted(() => {
  window.addEventListener("keydown", handleGlobalKeydown);
  if (!store.loading.value) {
    void store.loadLibrary();
  }
});

onUnmounted(() => {
  window.removeEventListener("keydown", handleGlobalKeydown);
});
</script>

<template>
  <div class="sl-explorer-root">
    <!-- 1. 顶部宏观数据驾驶舱 -->
    <header class="sl-cockpit-bar">
      <div class="sl-cockpit-header">
        <div class="sl-brand-section">
          <div class="sl-eyebrow-row">
            <span class="sl-live-dot" />
            <span class="sl-eyebrow-text">OpenHub · 站点资料库全景控制台</span>
          </div>
          <div class="sl-title-row">
            <h1>站点库控制台</h1>
          </div>
          <p class="sl-cockpit-subtitle">本地站点资料库 · 公共库一键同步 · 账号额度与健康签到监控</p>
        </div>

        <div class="sl-cockpit-actions">
          <div class="sl-sync-status-card">
            <span class="sl-status-indicator" :class="{ synced: !store.syncingSites.value && !store.syncingModelKeys.value }" />
            <div class="sl-status-text">
              <strong>{{ store.syncingSites.value || store.syncingModelKeys.value ? "正在同步中…" : "本地数据已就绪" }}</strong>
              <small>已收录 {{ store.sites.value.length }} 个站点 · {{ metrics.totalAccounts }} 个会话</small>
            </div>
          </div>

          <button
            v-if="capabilities.chromeSync"
            type="button"
            class="sl-btn-secondary"
            :disabled="store.syncingSites.value || store.syncingModelKeys.value"
            title="从远程仓库同步最新公共站点资料库"
            @click="store.openSyncDialog('remote')"
          >
            <span v-html="icons.globe" />
            <span>同步公共库</span>
          </button>

          <button
            type="button"
            class="sl-btn-primary"
            @click="store.openModal()"
          >
            <span v-html="icons.plus" />
            <span>添加站点</span>
          </button>
        </div>
      </div>

      <!-- 宏观 4 大指标卡片 (纯信息展示，无点击事件) -->
      <div class="sl-metrics-grid">
        <div class="sl-metric-card">
          <div class="sl-metric-icon sl-tone-brand" v-html="icons.layers" />
          <div class="sl-metric-info">
            <span class="sl-metric-label">收录站点总数</span>
            <div class="sl-metric-val">
              <strong>{{ metrics.totalSites }}</strong>
              <small>存活 {{ metrics.activeSites }} · 跑路 {{ metrics.runawaySites }}</small>
            </div>
          </div>
        </div>

        <div class="sl-metric-card">
          <div class="sl-metric-icon sl-tone-success" v-html="icons.bookmark" />
          <div class="sl-metric-info">
            <span class="sl-metric-label">在用与待定监控</span>
            <div class="sl-metric-val">
              <strong class="text-success">{{ metrics.personalSites }} 在用</strong>
              <small>待定 {{ metrics.pendingSites }}</small>
            </div>
          </div>
        </div>

        <div class="sl-metric-card">
          <div class="sl-metric-icon sl-tone-warning" v-html="icons.card" />
          <div class="sl-metric-info">
            <span class="sl-metric-label">账户总额度池</span>
            <div class="sl-metric-val">
              <strong class="text-warning">{{ metrics.totalQuotaText }}</strong>
              <small>已读取 {{ metrics.accountsWithQuota }} / {{ metrics.totalAccounts }} 账号额度</small>
            </div>
          </div>
        </div>

        <div class="sl-metric-card">
          <div class="sl-metric-icon sl-tone-violet" v-html="icons.calendar" />
          <div class="sl-metric-info">
            <span class="sl-metric-label">今日签到与健康监控</span>
            <div class="sl-metric-val">
              <strong class="text-violet">{{ metrics.checkedInAccounts }} 已签到</strong>
              <small :class="{ 'text-danger': metrics.errorAccounts > 0 }">
                {{ metrics.errorAccounts > 0 ? `${metrics.errorAccounts} 个账号同步异常` : '全部账号状态正常' }}
              </small>
            </div>
          </div>
        </div>
      </div>

      <!-- 快捷热门分类一键直达栏 (支持换行与展开/收起) -->
      <div class="sl-popular-chips-bar">
        <span class="sl-chips-label">快捷直达：</span>
        <div class="sl-chips-wrap">
          <button
            v-for="chip in visibleChips"
            :key="chip.id"
            type="button"
            class="sl-chip-btn"
            :class="{ active: popularChip === chip.id }"
            @click="selectPopularChip(chip.id)"
          >
            <span>{{ chip.label }}</span>
            <b class="sl-chip-num">{{ chip.count }}</b>
          </button>

          <!-- 展开 / 收起 按钮 -->
          <button
            v-if="popularChipsList.length > CHIPS_COLLAPSED_LIMIT"
            type="button"
            class="sl-chips-toggle-btn"
            :class="{ active: isChipsExpanded }"
            :title="isChipsExpanded ? '收起快捷直达' : `展开更多 (${hiddenChipsCount})`"
            @click="isChipsExpanded = !isChipsExpanded"
          >
            <span>{{ isChipsExpanded ? "收起" : `展开更多 (${hiddenChipsCount})` }}</span>
            <span class="sl-toggle-arrow" :class="{ 'is-up': isChipsExpanded }" v-html="icons.chevron" />
          </button>
        </div>
      </div>
    </header>

    <!-- 2. 控制中心：分类 Tab + 视图切换 + 筛选工具箱 -->
    <div class="sl-control-center">
      <!-- 上半区：分类 Tab 与 视图切换 -->
      <div class="sl-control-top-row">
        <!-- 左侧多维筛选组：使用状态维度 + 运营存活维度 -->
        <div class="sl-control-tabs-group">
          <!-- 维度 1：使用状态分类 Tab -->
          <nav class="sl-usage-tabs" role="tablist" aria-label="使用状态筛选">
            <button
              type="button"
              role="tab"
              :class="{ active: selectedUsageTab === 'all' }"
              title="全部使用状态"
              @click="selectedUsageTab = 'all'"
            >
              <span v-html="icons.layers" />
              <span>全部</span>
              <b class="sl-tab-badge">{{ tabCounts.all }}</b>
            </button>
            <button
              type="button"
              role="tab"
              :class="{ active: selectedUsageTab === 'personal' }"
              title="在用站点"
              @click="selectedUsageTab = 'personal'"
            >
              <span v-html="icons.bookmark" />
              <span>在用</span>
              <b class="sl-tab-badge">{{ tabCounts.personal }}</b>
            </button>
            <button
              type="button"
              role="tab"
              :class="{ active: selectedUsageTab === 'pending' }"
              title="待定站点"
              @click="selectedUsageTab = 'pending'"
            >
              <span v-html="icons.clock" />
              <span>待定</span>
              <b class="sl-tab-badge">{{ tabCounts.pending }}</b>
            </button>
          </nav>

          <span class="sl-tabs-divider" />

          <!-- 维度 2：存活与健康状态分段开关 -->
          <div class="sl-alive-tabs" role="group" aria-label="站点存活状态筛选">
            <button
              type="button"
              :class="{ active: selectedAliveTab === 'all' }"
              title="全部站点状态"
              @click="selectedAliveTab = 'all'"
            >
              <span>全部状态</span>
              <b class="sl-tab-badge">{{ tabCounts.all }}</b>
            </button>
            <button
              type="button"
              class="sl-alive-btn is-active"
              :class="{ active: selectedAliveTab === 'active' }"
              title="筛选正常存活站点"
              @click="selectedAliveTab = 'active'"
            >
              <span class="sl-alive-dot is-live" />
              <span>存活</span>
              <b class="sl-tab-badge">{{ tabCounts.active }}</b>
            </button>
            <button
              type="button"
              class="sl-alive-btn is-runaway"
              :class="{ active: selectedAliveTab === 'runaway' }"
              title="筛选已跑路或失效站点"
              @click="selectedAliveTab = 'runaway'"
            >
              <span class="sl-alive-dot is-dead" />
              <span>跑路</span>
              <b class="sl-tab-badge">{{ tabCounts.runaway }}</b>
            </button>
          </div>
        </div>

        <!-- 视图切换模式 -->
        <div class="sl-view-switcher">
          <button
            type="button"
            :class="{ active: currentView === 'cards' }"
            title="画廊卡片视图"
            @click="currentView = 'cards'"
          >
            <span v-html="icons.grid" />
            <span>画廊卡片</span>
          </button>
          <button
            type="button"
            :class="{ active: currentView === 'table' }"
            title="全景数据表视图"
            @click="currentView = 'table'"
          >
            <span v-html="icons.rows" />
            <span>全景数据表</span>
          </button>
          <button
            type="button"
            :class="{ active: currentView === 'topology' }"
            title="账号会话拓扑视图"
            @click="currentView = 'topology'"
          >
            <span v-html="icons.user" />
            <span>账号拓扑矩阵</span>
          </button>
        </div>
      </div>

      <!-- 中间区：搜索与筛选下拉 -->
      <div class="sl-filters-row">
        <!-- 搜索框 -->
        <div class="sl-search-box">
          <span v-html="icons.search" />
          <input
            id="sl-search-input"
            v-model="query"
            type="search"
            placeholder="搜索站点名称、API 地址、描述、标签、维护者…"
            autocomplete="off"
          />
          <button v-if="query" type="button" class="sl-clear-search" @click="query = ''">
            <span v-html="icons.close" />
          </button>
          <kbd>⌘ K</kbd>
        </div>

        <!-- 系统架构下拉 -->
        <CustomSelect
          class="sl-filter-dropdown"
          :options="systemTypeOptions"
          :model-value="selectedSystemType"
          aria-label="系统架构选择"
          @update:model-value="selectedSystemType = String($event)"
        />

        <!-- 注册门槛下拉 -->
        <CustomSelect
          class="sl-filter-dropdown"
          :options="levelOptions"
          :model-value="selectedLevel"
          aria-label="注册等级门槛"
          @update:model-value="selectedLevel = String($event)"
        />

        <!-- 标签下拉 -->
        <CustomSelect
          class="sl-filter-dropdown"
          :options="tagOptions"
          :model-value="selectedTag"
          aria-label="标签筛选"
          @update:model-value="selectedTag = String($event)"
        />

        <!-- 智能排序 -->
        <CustomSelect
          class="sl-filter-dropdown"
          :options="sortOptions"
          :model-value="sortBy"
          aria-label="排序规则"
          @update:model-value="sortBy = String($event)"
        />
      </div>

      <!-- 下半区：特性能力快捷开关芯片栏 -->
      <div class="sl-feature-bar">
        <div class="sl-feature-chips">
          <span class="sl-chips-title">特性与能力：</span>
          <button
            type="button"
            class="sl-chip"
            :class="{ active: featureFilters.checkin }"
            @click="toggleFeature('checkin')"
          >
            📅 每日签到
          </button>
          <button
            type="button"
            class="sl-chip"
            :class="{ active: featureFilters.translation }"
            @click="toggleFeature('translation')"
          >
            🌐 沉浸式翻译
          </button>
          <button
            type="button"
            class="sl-chip"
            :class="{ active: featureFilters.ldc }"
            @click="toggleFeature('ldc')"
          >
            💳 LDC 支付
          </button>
          <button
            type="button"
            class="sl-chip"
            :class="{ active: featureFilters.nsfw }"
            @click="toggleFeature('nsfw')"
          >
            🔞 18+ NSFW
          </button>
          <button
            type="button"
            class="sl-chip"
            :class="{ active: featureFilters.invite }"
            @click="toggleFeature('invite')"
          >
            🎟️ 需邀请码
          </button>
          <button
            type="button"
            class="sl-chip"
            :class="{ active: featureFilters.hasKeys }"
            @click="toggleFeature('hasKeys')"
          >
            🔑 已同步 Key
          </button>
          <button
            type="button"
            class="sl-chip"
            :class="{ active: featureFilters.checkedInToday }"
            @click="toggleFeature('checkedInToday')"
          >
            ✅ 今日已签到
          </button>
          <button
            type="button"
            class="sl-chip"
            :class="{ active: featureFilters.syncError }"
            @click="toggleFeature('syncError')"
          >
            ⚠️ 同步存在异常
          </button>
          <button
            type="button"
            class="sl-chip"
            :class="{ active: featureFilters.proxyPool }"
            @click="toggleFeature('proxyPool')"
          >
            🛡️ 走代理池
          </button>
        </div>

        <!-- 当前过滤结果总数与清空按钮 -->
        <div class="sl-results-meta">
          <button
            v-if="hasActiveFilters()"
            type="button"
            class="sl-clear-filters-btn"
            @click="resetAllFilters"
          >
            <span v-html="icons.close" />
            <span>重置筛选</span>
          </button>
          <span class="sl-filter-count">
            匹配到 <b>{{ filteredSites.length }}</b> / {{ store.sites.value.length }} 个站点
          </span>
        </div>
      </div>
    </div>

    <!-- 3. 主视图区域 -->
    <main class="sl-main-content" :class="{ 'is-table-mode': currentView === 'table', 'is-topology-mode': currentView === 'topology' }">
      <!-- 视图 A：智能画廊卡片流 -->
      <section v-if="currentView === 'cards'" class="sl-cards-view">
        <div v-if="store.loading.value" class="sl-loading-state">
          <span class="is-spinning" v-html="icons.restore" />
          <p>正在读取本地站点资料库…</p>
        </div>

        <div v-else-if="!filteredSites.length" class="sl-empty-state">
          <span v-html="icons.search" />
          <h3>未找到匹配的站点</h3>
          <p>请尝试重置筛选条件或更改搜索关键字</p>
          <button type="button" class="sl-btn-secondary" @click="resetAllFilters">
            清除全部筛选
          </button>
        </div>

        <div v-else class="sl-cards-grid">
          <article
            v-for="site in paginatedSites"
            :key="site.id"
            class="sl-card"
            :class="{
              'is-selected': selectedSiteId === site.id,
              'is-runaway': site.isRunaway,
              'is-personal': site.isPersonal,
              'is-pending': site.isPending,
            }"
            @click="openModelDetail(site)"
          >
            <!-- 卡片顶部：Avatar + 标题 + 架构徽章 + 多选/更多菜单 -->
            <div class="sl-card-head">
              <div class="sl-card-identity">
                <span class="sl-card-avatar" :class="`sl-tone-${systemTone(site.systemType)}`">
                  {{ siteInitials(site.apiBaseUrl, site.name) }}
                </span>
                <div class="sl-card-title-box">
                  <div class="sl-card-title-row">
                    <h3 :title="site.name">{{ site.name }}</h3>
                    <span v-if="site.systemType" class="sl-pill sl-pill-system" :class="`sl-pill-${systemTone(site.systemType)}`">
                      {{ systemTypeLabel(site.systemType) }}
                    </span>
                    <span v-if="site.isRunaway" class="sl-pill sl-pill-runaway">已跑路</span>
                    <span v-else-if="site.isPersonal" class="sl-pill sl-pill-personal">在用</span>
                    <span v-else-if="site.isPending" class="sl-pill sl-pill-pending">待定</span>
                    <span v-if="site.isFakeCharity" class="sl-pill sl-pill-fake">伪公益</span>
                  </div>
                  <small class="sl-card-url" :title="site.apiBaseUrl">{{ site.apiBaseUrl }}</small>
                </div>
              </div>

              <!-- 右上角多选与告警 -->
              <div class="sl-card-head-tools" @click.stop>
                <span
                  v-if="getSiteErrorMessage(site.id)"
                  class="sl-card-err-badge"
                  :title="getSiteErrorMessage(site.id)"
                  v-html="icons.info"
                />
                <button
                  type="button"
                  class="sl-card-select-btn"
                  :class="{ active: batchSelectedIds.includes(site.id) }"
                  title="选择此站点进行批量操作"
                  @click="toggleSelectSite(site.id)"
                >
                  <span v-html="batchSelectedIds.includes(site.id) ? icons.check : icons.plus" />
                </button>
              </div>
            </div>

            <!-- 卡片特性与标签徽章栏 -->
            <div class="sl-card-badges">
              <span class="sl-card-tag sl-tone-neutral">LV{{ site.registrationLimit }}</span>
              <span v-if="formatRateLimit(site.rateLimit)" class="sl-card-tag sl-tone-brand" :title="`速率限制：${formatRateLimit(site.rateLimit)}`">
                {{ formatRateLimit(site.rateLimit) }}
              </span>
              <span v-if="site.supportsCheckin" class="sl-card-feat">📅 每日签到</span>
              <span v-if="site.supportsImmersiveTranslation" class="sl-card-feat">🌐 沉浸式翻译</span>
              <span v-if="site.supportsLdc" class="sl-card-feat">💳 LDC</span>
              <span v-if="site.supportsNsfw" class="sl-card-feat sl-feat-nsfw">🔞 18+</span>
              <span v-if="site.requiresInviteCode" class="sl-card-feat sl-feat-invite">🎟️ 邀请码</span>
            </div>

            <!-- 标签列表 -->
            <div v-if="site.tags.length" class="sl-card-tags-row">
              <span v-for="t in site.tags.slice(0, 4)" :key="t" class="sl-tag-chip">{{ t }}</span>
              <span v-if="site.tags.length > 4" class="sl-tag-more">+{{ site.tags.length - 4 }}</span>
            </div>

            <!-- 账号额度或概览条目 -->
            <div class="sl-card-specs">
              <div class="sl-spec-item">
                <span class="sl-spec-k">关联会话</span>
                <strong class="sl-spec-v">{{ getSiteSessions(site.id).length }} 个账号</strong>
              </div>
              <div class="sl-spec-item">
                <span class="sl-spec-k">剩余额度</span>
                <strong class="sl-spec-v text-brand">{{ getSiteQuotaText(site.id) }}</strong>
              </div>
              <div class="sl-spec-item">
                <span class="sl-spec-k">更新时间</span>
                <strong class="sl-spec-v font-mono">{{ formatDate(site.updatedAt) }}</strong>
              </div>
            </div>

            <!-- 描述文本 -->
            <p class="sl-card-desc" :class="{ muted: !site.description }">
              {{ site.description || "暂无描述说明。" }}
            </p>

            <!-- 卡片底栏：快捷链接与全景按钮 -->
            <div class="sl-card-footer" @click.stop>
              <div class="sl-card-links">
                <button
                  v-if="site.apiBaseUrl"
                  type="button"
                  class="sl-link-btn"
                  title="API 地址"
                  @click="store.openLinkDialog(site, 'api', $event.currentTarget as HTMLElement)"
                >
                  <span v-html="icons.link" />
                </button>
                <button
                  v-if="site.checkinUrl"
                  type="button"
                  class="sl-link-btn active"
                  title="签到地址"
                  @click="store.openLinkDialog(site, 'checkin', $event.currentTarget as HTMLElement)"
                >
                  <span v-html="icons.calendar" />
                </button>
                <button
                  v-if="site.benefitUrl"
                  type="button"
                  class="sl-link-btn"
                  title="福利站地址"
                  @click="store.openLinkDialog(site, 'benefit', $event.currentTarget as HTMLElement)"
                >
                  <span v-html="icons.gift" />
                </button>
                <button
                  v-if="site.statusUrl"
                  type="button"
                  class="sl-link-btn"
                  title="状态页地址"
                  @click="store.openLinkDialog(site, 'status', $event.currentTarget as HTMLElement)"
                >
                  <span v-html="icons.pulse" />
                </button>
              </div>

              <div class="sl-card-footer-actions" @click.stop>
                <button
                  type="button"
                  class="sl-action-icon-btn"
                  title="查看支持模型与 Key"
                  @click.stop="store.openSiteModelsDialog(site)"
                >
                  <span v-html="icons.cpu" />
                </button>
                <button
                  type="button"
                  class="sl-action-icon-btn"
                  title="全景详情"
                  @click.stop="openModelDetail(site)"
                >
                  <span v-html="icons.info" />
                </button>
                <button
                  type="button"
                  class="sl-action-icon-btn"
                  title="编辑站点"
                  @click.stop="store.openModal(site)"
                >
                  <span v-html="icons.edit" />
                </button>
              </div>
            </div>
          </article>
        </div>

        <!-- 卡片视图分页 -->
        <footer v-if="totalPages > 1" class="sl-pagination-bar">
          <button
            type="button"
            class="sl-page-btn"
            :disabled="currentPage <= 1"
            @click="currentPage--"
          >
            上一页
          </button>
          <span class="sl-page-info">第 {{ currentPage }} / {{ totalPages }} 页</span>
          <button
            type="button"
            class="sl-page-btn"
            :disabled="currentPage >= totalPages"
            @click="currentPage++"
          >
            下一页
          </button>
        </footer>
      </section>

      <!-- 视图 B：全景数据大表视图 -->
      <section v-else-if="currentView === 'table'" class="sl-table-view">
        <AppTable
          :rows="filteredSites"
          :columns="tableColumns"
          :row-key="(site: SiteRecord) => site.id"
          :loading="store.loading.value"
          empty-text="没有匹配的站点"
          :page="currentPage"
          :page-size="pageSize"
          :sorting="tableSorting"
          :selected-key="selectedSiteId"
          clickable
          @update:page="currentPage = $event"
          @update:page-size="pageSize = $event"
          @update:sorting="tableSorting = $event"
          @select="openModelDetail"
        >
          <!-- 站点信息列 -->
          <template #cell-siteInfo="{ row }">
            <div class="sl-table-site-cell">
              <span class="sl-card-avatar sl-avatar-sm" :class="`sl-tone-${systemTone(row.systemType)}`">
                {{ siteInitials(row.apiBaseUrl, row.name) }}
              </span>
              <div class="sl-table-site-info">
                <div class="sl-table-site-title">
                  <strong>{{ row.name }}</strong>
                  <span v-if="row.isRunaway" class="sl-pill sl-pill-runaway">跑路</span>
                  <span v-else-if="row.isPersonal" class="sl-pill sl-pill-personal">在用</span>
                  <span v-else-if="row.isPending" class="sl-pill sl-pill-pending">待定</span>
                </div>
                <small class="sl-table-site-url">{{ row.apiBaseUrl }}</small>
              </div>
            </div>
          </template>

          <!-- 系统架构列 -->
          <template #cell-systemType="{ row }">
            <span class="sl-pill sl-pill-system" :class="`sl-pill-${systemTone(row.systemType)}`">
              {{ systemTypeLabel(row.systemType) || "未知架构" }}
            </span>
          </template>

          <!-- 账号与会话列 -->
          <template #cell-accounts="{ row }">
            <div class="sl-table-accounts-cell">
              <span>{{ getSiteSessions(row.id).length }} 个账号</span>
              <small v-if="normalizeSystemType(row.systemType) === 'newapi2' && getSiteSessions(row.id).some((s: ChromeSessionInfo) => s.hasAccessToken)" class="text-brand">含刷新令牌</small>
            </div>
          </template>

          <!-- 剩余额度列 -->
          <template #cell-quota="{ row }">
            <span class="font-mono font-semibold text-brand">{{ getSiteQuotaText(row.id) }}</span>
          </template>

          <!-- 注册门槛列 -->
          <template #cell-regLevel="{ row }">
            <span class="sl-level-badge">LV{{ row.registrationLimit }}</span>
          </template>

          <!-- 速率限制列 -->
          <template #cell-rateLimit="{ row }">
            <span class="font-mono text-muted text-xs">{{ formatRateLimit(row.rateLimit) || "—" }}</span>
          </template>

          <!-- 特性能力列 -->
          <template #cell-capabilities="{ row }">
            <div class="sl-table-caps-cell">
              <span v-if="row.supportsCheckin" title="支持每日签到">📅</span>
              <span v-if="row.supportsImmersiveTranslation" title="支持沉浸式翻译">🌐</span>
              <span v-if="row.supportsLdc" title="支持 LDC 支付">💳</span>
              <span v-if="row.supportsNsfw" title="支持 18+ NSFW">🔞</span>
              <span v-if="row.useProxyPool" title="开启代理池">🛡️</span>
            </div>
          </template>

          <!-- 更新时间列 -->
          <template #cell-updatedAt="{ row }">
            <span class="font-mono text-xs text-faint">{{ formatDate(row.updatedAt) }}</span>
          </template>

          <!-- 快捷操作列 -->
          <template #cell-actions="{ row }">
            <div class="sl-table-actions-cell" @click.stop>
              <button
                type="button"
                class="sl-action-icon-btn"
                title="查看支持模型与 Key"
                @click="store.openSiteModelsDialog(row)"
              >
                <span v-html="icons.cpu" />
              </button>
              <button
                type="button"
                class="sl-action-icon-btn"
                title="全景详情"
                @click="openModelDetail(row)"
              >
                <span v-html="icons.info" />
              </button>
              <button
                type="button"
                class="sl-action-icon-btn"
                title="编辑站点"
                @click="store.openModal(row)"
              >
                <span v-html="icons.edit" />
              </button>
            </div>
          </template>
        </AppTable>
      </section>

      <!-- 视图 C：账号会话拓扑矩阵视图 -->
      <section v-else-if="currentView === 'topology'" class="sl-topology-view">
        <div class="sl-topology-header">
          <div>
            <h2>全网站点与 Chrome 账号拓扑矩阵</h2>
            <p>实时监控并管理各个站点的 Chrome 授权会话、Access Token、额度消耗与签到状态</p>
          </div>
          <div class="sl-topology-header-actions">
            <button
              type="button"
              class="sl-btn-secondary"
              title="同步拓扑站点的 Chrome 账号会话与可用额度"
              @click="store.openSyncDialog('quota', selectedUsageTab === 'pending' ? 'pending' : 'personal')"
            >
              <span v-html="icons.restore" />
              <span>同步额度</span>
            </button>
            <button
              type="button"
              class="sl-btn-primary"
              :disabled="store.syncingModelKeys.value"
              title="根据 API Key 调用 /v1/models 批量同步站点与账号支持模型"
              @click="handleTopologyBatchModelSync"
            >
              <span v-html="icons.cpu" :class="{ 'is-spinning': store.syncingModelKeys.value }" />
              <span>{{ store.syncingModelKeys.value ? '正在同步模型…' : '同步模型' }}</span>
            </button>
          </div>
        </div>

        <div v-if="!filteredSites.length" class="sl-empty-state">
          <span v-html="icons.search" />
          <h3>未找到匹配的站点</h3>
          <p>请尝试重置筛选条件或更改搜索关键字</p>
          <button type="button" class="sl-btn-secondary" @click="resetAllFilters">
            清除全部筛选
          </button>
        </div>

        <div v-else class="sl-topology-grid">
          <article
            v-for="site in paginatedSites"
            :key="site.id"
            class="sl-topo-card"
            :class="{
              'has-sessions': getSiteSessions(site.id).length > 0,
              'is-selected': selectedSiteId === site.id,
              'is-runaway': site.isRunaway,
              'is-personal': site.isPersonal,
              'is-pending': site.isPending,
            }"
            @click="openModelDetail(site)"
          >
            <!-- 拓扑卡片头部：标识 + 状态徽章 + 多选 + 快捷操作 -->
            <div class="sl-topo-card-head" @click.stop>
              <div class="sl-topo-site-id" @click="openModelDetail(site)">
                <span class="sl-card-avatar sl-avatar-sm" :class="`sl-tone-${systemTone(site.systemType)}`">
                  {{ siteInitials(site.apiBaseUrl, site.name) }}
                </span>
                <div class="sl-topo-title-box">
                  <div class="sl-topo-title-row">
                    <strong :title="site.name">{{ site.name }}</strong>
                    <span class="sl-pill sl-pill-system" :class="`sl-pill-${systemTone(site.systemType)}`">
                      {{ systemTypeLabel(site.systemType) || "未知架构" }}
                    </span>
                    <span v-if="site.isRunaway" class="sl-pill sl-pill-runaway">已跑路</span>
                    <span v-else-if="site.isPersonal" class="sl-pill sl-pill-personal">在用</span>
                    <span v-else-if="site.isPending" class="sl-pill sl-pill-pending">待定</span>
                    <span v-if="site.isFakeCharity" class="sl-pill sl-pill-fake">伪公益</span>
                  </div>
                  <small class="sl-card-url" :title="site.apiBaseUrl">{{ site.apiBaseUrl }}</small>
                </div>
              </div>

              <div class="sl-topo-head-actions">
                <span
                  v-if="getSiteErrorMessage(site.id)"
                  class="sl-card-err-badge"
                  :title="getSiteErrorMessage(site.id)"
                  v-html="icons.info"
                />
                <!-- 批量选择按钮 -->
                <button
                  type="button"
                  class="sl-topo-head-btn sl-topo-select-btn"
                  :class="{ active: batchSelectedIds.includes(site.id) }"
                  title="选择此站点进行批量操作"
                  @click.stop="toggleSelectSite(site.id)"
                >
                  <span v-html="batchSelectedIds.includes(site.id) ? icons.check : icons.plus" />
                </button>

                <!-- 同步会话 -->
                <button
                  type="button"
                  class="sl-topo-head-btn sl-topo-sync-btn"
                  :class="{ 'is-syncing': store.chromeSessionSyncActive.value && store.chromeSessionSite.value?.id === site.id }"
                  title="提取或同步此站点的 Chrome 会话与额度"
                  @click.stop="store.syncChromeSession(site, $event.currentTarget as HTMLElement)"
                >
                  <span v-html="icons.restore" />
                </button>
              </div>
            </div>


            <!-- 会话账号列表 -->
            <div class="sl-topo-sessions-list" @click.stop>
              <template v-if="getSiteSessions(site.id).length > 0">
                <div
                  v-for="session in getSiteSessions(site.id)"
                  :key="session.profileId"
                  class="sl-topo-session-row"
                  :class="{ 'has-error': session.syncError || session.checkinError || (session.apiSyncError && !session.apiKeyCount && !session.apiModelCount) }"
                >
                  <div class="sl-topo-session-meta">
                    <span class="sl-topo-user-icon" v-html="icons.user" />
                    <div class="sl-topo-user-info">
                      <div class="sl-topo-user-line">
                        <strong>{{ session.username || session.accountName || session.profileName }}</strong>
                        <span v-if="session.newapiUserId" class="sl-user-id-pill">ID: {{ session.newapiUserId }}</span>
                        <template v-if="normalizeSystemType(site.systemType) === 'newapi2'">
                          <span v-if="session.hasAccessToken" class="sl-token-pill active" title="已从 Chrome 会话中获取访问令牌">已取令牌</span>
                          <span v-else class="sl-token-pill" title="未获取到访问令牌，需刷新">无令牌</span>
                        </template>
                        <template v-if="site.supportsCheckin || session.checkinEnabled">
                          <span v-if="session.checkedInToday" class="sl-token-pill sl-token-checked" title="今日已成功签到">今日已签到</span>
                          <span v-else-if="session.checkinEnabled" class="sl-token-pill sl-token-uncheck" title="站点开启签到，今日尚未签到">今日未签到</span>
                          <span v-else class="sl-token-pill sl-token-disabled" title="站点接口返回404/403/人机验证或功能未启用，无法签到">无法签到</span>
                        </template>
                        <span v-if="session.syncError || session.checkinError" class="sl-token-pill sl-token-err" :title="session.syncError || session.checkinError">异常</span>
                      </div>
                      <small class="sl-topo-subline">
                        <span>配置：{{ session.profileName || "默认配置" }}</span>
                        <span v-if="session.accountUpdatedAt" class="sl-topo-subtime">· {{ formatDate(session.accountUpdatedAt) }}</span>
                      </small>
                    </div>
                  </div>

                  <div class="sl-topo-session-right">
                    <div class="sl-topo-session-quota">
                      <strong class="font-mono text-brand">{{ formatAccountQuota(session.remaining, session.unit) }}</strong>
                      <small v-if="session.apiKeyCount || session.apiModelCount">
                        {{ session.apiKeyCount || 0 }} Key · {{ session.apiModelCount || 0 }} 模型
                      </small>
                    </div>
                    <div class="sl-topo-session-btns">
                      <!-- 复制 Cookie 按钮 -->
                      <button
                        type="button"
                        class="sl-action-icon-btn"
                        :title="`复制「${session.profileName}」的 Chrome Cookie`"
                        @click.stop="copySessionCookie(site, session)"
                      >
                        <span v-html="icons.copy" />
                      </button>
                      <!-- 在 Chrome 对应配置中打开站点 -->
                      <button
                        type="button"
                        class="sl-action-icon-btn sl-topo-open-btn"
                        :title="`使用 ${session.profileName} 在 Chrome 中快捷打开此站点`"
                        @click.stop="store.openExternalInChromeProfile(site.apiBaseUrl, session.profileId)"
                      >
                        <span v-html="icons.external" />
                      </button>
                      <!-- 删除会话账号（解除关联） -->
                      <button
                        type="button"
                        class="sl-action-icon-btn sl-topo-delete-btn"
                        :title="`删除「${session.profileName}」会话账号，解除与本站点的关联`"
                        @click.stop="store.deleteSiteAccount(site, session)"
                      >
                        <span v-html="icons.trash" />
                      </button>
                    </div>
                  </div>
                </div>
              </template>
              <div v-else class="sl-topo-empty-sessions">
                <span v-html="icons.user" />
                <p>暂无关联的 Chrome 账号会话</p>
                <button
                  type="button"
                  class="sl-link-action"
                  @click="store.syncChromeSession(site, $event.currentTarget as HTMLElement)"
                >
                  点击提取会话 &rarr;
                </button>
              </div>
            </div>

            <!-- 卡片底栏：快捷链接、模型详情入口与状态切换 -->
            <div class="sl-topo-card-footer" @click.stop>
              <div class="sl-card-links">
                <button
                  v-if="site.apiBaseUrl"
                  type="button"
                  class="sl-link-btn"
                  title="API 接口地址"
                  @click="store.openLinkDialog(site, 'api', $event.currentTarget as HTMLElement)"
                >
                  <span v-html="icons.link" />
                </button>
                <button
                  v-if="site.checkinUrl"
                  type="button"
                  class="sl-link-btn active"
                  title="签到页面地址"
                  @click="store.openLinkDialog(site, 'checkin', $event.currentTarget as HTMLElement)"
                >
                  <span v-html="icons.calendar" />
                </button>
                <button
                  v-if="site.benefitUrl"
                  type="button"
                  class="sl-link-btn"
                  title="福利站地址"
                  @click="store.openLinkDialog(site, 'benefit', $event.currentTarget as HTMLElement)"
                >
                  <span v-html="icons.gift" />
                </button>
                <button
                  v-if="site.statusUrl"
                  type="button"
                  class="sl-link-btn"
                  title="系统状态页地址"
                  @click="store.openLinkDialog(site, 'status', $event.currentTarget as HTMLElement)"
                >
                  <span v-html="icons.pulse" />
                </button>
              </div>

              <div class="sl-topo-footer-actions" @click.stop>
                <!-- 查看支持模型与 Key -->
                <button
                  type="button"
                  class="sl-action-icon-btn"
                  title="查看支持模型与 Key"
                  @click.stop="store.openSiteModelsDialog(site)"
                >
                  <span v-html="icons.cpu" />
                </button>

                <!-- 全景详情 -->
                <button
                  type="button"
                  class="sl-action-icon-btn"
                  title="全景详情"
                  @click.stop="openModelDetail(site)"
                >
                  <span v-html="icons.info" />
                </button>

                <!-- 编辑站点 -->
                <button
                  type="button"
                  class="sl-action-icon-btn"
                  title="编辑站点"
                  @click.stop="store.openModal(site)"
                >
                  <span v-html="icons.edit" />
                </button>
              </div>
            </div>
          </article>
        </div>

        <!-- 拓扑视图分页栏 -->
        <footer v-if="totalPages > 1" class="sl-pagination-bar">
          <button
            type="button"
            class="sl-page-btn"
            :disabled="currentPage <= 1"
            @click="currentPage--"
          >
            上一页
          </button>
          <span class="sl-page-info">第 {{ currentPage }} / {{ totalPages }} 页</span>
          <button
            type="button"
            class="sl-page-btn"
            :disabled="currentPage >= totalPages"
            @click="currentPage++"
          >
            下一页
          </button>
        </footer>
      </section>
    </main>

    <!-- 4. 多选批量操作浮动底栏 (Batch Operations Dock) -->
    <aside v-if="batchSelectedIds.length" class="sl-batch-dock">
      <div class="sl-batch-dock-content">
        <div class="sl-batch-dock-title">
          <span v-html="icons.check" />
          <strong>已选择 {{ batchSelectedIds.length }} 个站点</strong>
        </div>

        <div class="sl-batch-actions">
          <button type="button" class="sl-batch-btn" @click="batchSetUsage('personal')">
            转换为在用
          </button>
          <button type="button" class="sl-batch-btn" @click="batchSetUsage('pending')">
            转换为待定
          </button>
          <button type="button" class="sl-batch-btn" @click="batchSetUsage('unused')">
            设为未在用
          </button>
          <button type="button" class="sl-batch-btn" @click="batchToggleRunaway">
            切换跑路/存活
          </button>
          <button type="button" class="sl-batch-btn sl-btn-accent" @click="batchSyncSession">
            批量会话同步
          </button>
        </div>

        <div class="sl-batch-dock-right">
          <button type="button" class="sl-batch-clear-btn" @click="clearBatchSelection">
            取消选择
          </button>
        </div>
      </div>
    </aside>

    <!-- 5. 全景站点详情深度抽屉 (Slide-over Detail Drawer) -->
    <Teleport to="body">
      <div
        class="sl-drawer-backdrop"
        :hidden="!selectedSite"
        @click="closeDetail"
      >
        <aside
          class="sl-drawer-panel"
          role="dialog"
          aria-modal="true"
          :aria-label="selectedSite?.name || '站点详情'"
          @click.stop
        >
          <template v-if="selectedSite">
            <!-- 抽屉头部 -->
            <header class="sl-drawer-header">
              <div class="sl-drawer-head-identity">
                <span class="sl-drawer-avatar" :class="`sl-tone-${systemTone(selectedSite.systemType)}`">
                  {{ siteInitials(selectedSite.apiBaseUrl, selectedSite.name) }}
                </span>
                <div class="sl-drawer-title-box">
                  <div class="sl-drawer-tags">
                    <span class="sl-meta-system">{{ systemTypeLabel(selectedSite.systemType) || "通用架构" }}</span>
                    <span class="sl-meta-sep">·</span>
                    <span v-if="selectedSite.isRunaway" class="sl-pill sl-pill-runaway">已跑路</span>
                    <span v-else-if="selectedSite.isPersonal" class="sl-pill sl-pill-personal">在用站点</span>
                    <span v-else-if="selectedSite.isPending" class="sl-pill sl-pill-pending">待定站点</span>
                    <span v-else class="sl-pill sl-pill-unused">未在用</span>
                    <span class="sl-meta-sep">·</span>
                    <span class="sl-level-badge">LV{{ selectedSite.registrationLimit }}</span>
                  </div>

                  <h2 class="sl-drawer-title">{{ selectedSite.name }}</h2>
                  <p class="sl-drawer-url font-mono">{{ selectedSite.apiBaseUrl }}</p>
                </div>
              </div>

              <div class="sl-drawer-head-actions">
                <button
                  type="button"
                  class="sl-btn-copy"
                  @click="copyText(selectedSite.apiBaseUrl, 'API 地址')"
                >
                  <span v-html="idCopied ? icons.check : icons.copy" />
                  <span>{{ idCopied ? "已复制" : "复制 API" }}</span>
                </button>
                <button
                  type="button"
                  class="sl-btn-edit"
                  @click="store.openModal(selectedSite)"
                >
                  <span v-html="icons.edit" />
                  <span>编辑站点</span>
                </button>
                <button
                  type="button"
                  class="sl-drawer-close-btn"
                  aria-label="关闭抽屉"
                  @click="closeDetail"
                >
                  <span v-html="icons.close" />
                </button>
              </div>
            </header>

            <!-- 抽屉 Tab 导航 -->
            <nav class="sl-drawer-tabs">
              <button
                type="button"
                :class="{ active: activeDetailTab === 'overview' }"
                @click="activeDetailTab = 'overview'"
              >
                <span v-html="icons.layers" />
                <span>全景概览</span>
              </button>
              <button
                type="button"
                :class="{ active: activeDetailTab === 'accounts' }"
                @click="activeDetailTab = 'accounts'"
              >
                <span v-html="icons.user" />
                <span>账号与额度 ({{ getSiteSessions(selectedSite.id).length }})</span>
              </button>
              <button
                type="button"
                :class="{ active: activeDetailTab === 'models' }"
                @click="activeDetailTab = 'models'"
              >
                <span v-html="icons.cpu" />
                <span>关联 Key 与支持模型</span>
              </button>
            </nav>

            <!-- 抽屉主体内容 -->
            <div class="sl-drawer-body">
              <!-- TAB 1: 全景概览 -->
              <div v-if="activeDetailTab === 'overview'" class="sl-tab-panel">
                <!-- 站点概览描述 -->
                <div class="sl-section-card">
                  <div class="sl-section-head">
                    <span v-html="icons.info" />
                    <strong>站点说明与描述</strong>
                  </div>
                  <p class="sl-desc-text">{{ selectedSite.description || "暂无站点详细说明，可随时点击右上角进行编辑补充。" }}</p>
                </div>

                <!-- 核心参数矩阵网格 -->
                <div class="sl-facts-grid">
                  <div class="sl-fact-box">
                    <span class="sl-fact-k">API 基础地址</span>
                    <strong class="sl-fact-v font-mono text-brand">{{ selectedSite.apiBaseUrl || "未配置" }}</strong>
                  </div>
                  <div class="sl-fact-box">
                    <span class="sl-fact-k">注册等级门槛</span>
                    <strong class="sl-fact-v">LV{{ selectedSite.registrationLimit }}</strong>
                  </div>
                  <div class="sl-fact-box">
                    <span class="sl-fact-k">速率限制</span>
                    <strong class="sl-fact-v">{{ formatRateLimit(selectedSite.rateLimit) || "无限制 / 未填写" }}</strong>
                  </div>
                  <div class="sl-fact-box">
                    <span class="sl-fact-k">系统架构类型</span>
                    <strong class="sl-fact-v">{{ systemTypeLabel(selectedSite.systemType) || "通用系统" }}</strong>
                  </div>
                  <div class="sl-fact-box">
                    <span class="sl-fact-k">代理池配置</span>
                    <strong class="sl-fact-v">{{ selectedSite.useProxyPool ? "强制通过 Clash 代理池" : "直连访问" }}</strong>
                  </div>
                  <div class="sl-fact-box">
                    <span class="sl-fact-k">最近更新时间</span>
                    <strong class="sl-fact-v font-mono">{{ formatDate(selectedSite.updatedAt) }}</strong>
                  </div>
                </div>

                <!-- 特性与能力标签矩阵 -->
                <div class="sl-section-card">
                  <div class="sl-section-head">
                    <span v-html="icons.sparkles" />
                    <strong>支持特性与业务能力</strong>
                  </div>
                  <div class="sl-chips-wrap">
                    <span class="sl-cap-pill" :class="{ 'is-disabled': !selectedSite.supportsCheckin }">📅 每日签到</span>
                    <span class="sl-cap-pill" :class="{ 'is-disabled': !selectedSite.supportsImmersiveTranslation }">🌐 沉浸式翻译</span>
                    <span class="sl-cap-pill" :class="{ 'is-disabled': !selectedSite.supportsLdc }">💳 LDC 支付</span>
                    <span class="sl-cap-pill" :class="{ 'is-disabled': !selectedSite.supportsNsfw }">🔞 18+ NSFW</span>
                    <span class="sl-cap-pill" :class="{ 'is-disabled': !selectedSite.requiresInviteCode }">🎟️ 需邀请码</span>
                    <span class="sl-cap-pill" :class="{ 'is-disabled': !selectedSite.useProxyPool }">🛡️ 代理池接入</span>
                  </div>
                </div>

                <!-- 标签列表 -->
                <div v-if="selectedSite.tags.length" class="sl-section-card">
                  <div class="sl-section-head">
                    <span v-html="icons.bookmark" />
                    <strong>站点所属标签</strong>
                  </div>
                  <div class="sl-tags-cloud">
                    <span v-for="t in selectedSite.tags" :key="t" class="sl-tag-badge">{{ t }}</span>
                  </div>
                </div>

                <!-- 维护者信息 -->
                <div class="sl-section-card">
                  <div class="sl-section-head">
                    <span v-html="icons.users" />
                    <strong>维护者列表 ({{ selectedSite.maintainers.length }})</strong>
                  </div>
                  <div v-if="selectedSite.maintainers.length" class="sl-maintainers-grid">
                    <div v-for="m in selectedSite.maintainers" :key="m.id || m.username" class="sl-maintainer-card">
                      <span class="sl-maintainer-avatar">{{ m.name[0] || m.username?.[0] || 'U' }}</span>
                      <div class="sl-maintainer-info">
                        <strong>{{ m.name }}</strong>
                        <small v-if="m.username">@{{ m.username }}</small>
                        <small v-if="m.id" class="sl-faint">ID: {{ m.id }}</small>
                      </div>
                      <button
                        v-if="m.profileUrl"
                        type="button"
                        class="sl-btn-icon-xs"
                        title="打开维护者主页"
                        @click="store.openExternal(m.profileUrl)"
                      >
                        <span v-html="icons.external" />
                      </button>
                    </div>
                  </div>
                  <p v-else class="sl-empty-hint">未配置维护者信息</p>
                </div>

                <!-- 相关链接清单 -->
                <div class="sl-section-card">
                  <div class="sl-section-head">
                    <span v-html="icons.link" />
                    <strong>相关直达链接</strong>
                  </div>
                  <div class="sl-links-list">
                    <div v-if="selectedSite.apiBaseUrl" class="sl-link-item">
                      <div>
                        <strong>API 服务地址</strong>
                        <small>{{ selectedSite.apiBaseUrl }}</small>
                      </div>
                      <div class="sl-link-item-actions">
                        <button type="button" class="sl-btn-xs" @click="store.openExternal(selectedSite.apiBaseUrl)">打开</button>
                        <button type="button" class="sl-btn-xs" @click="copyText(selectedSite.apiBaseUrl, 'API 地址')">复制</button>
                      </div>
                    </div>
                    <div v-if="selectedSite.checkinUrl" class="sl-link-item">
                      <div>
                        <strong>签到地址 {{ selectedSite.checkinNote ? `(${selectedSite.checkinNote})` : '' }}</strong>
                        <small>{{ selectedSite.checkinUrl }}</small>
                      </div>
                      <div class="sl-link-item-actions">
                        <button type="button" class="sl-btn-xs" @click="store.openExternal(selectedSite.checkinUrl)">打开</button>
                        <button type="button" class="sl-btn-xs" @click="copyText(selectedSite.checkinUrl, '签到地址')">复制</button>
                      </div>
                    </div>
                    <div v-if="selectedSite.benefitUrl" class="sl-link-item">
                      <div>
                        <strong>福利站地址</strong>
                        <small>{{ selectedSite.benefitUrl }}</small>
                      </div>
                      <div class="sl-link-item-actions">
                        <button type="button" class="sl-btn-xs" @click="store.openExternal(selectedSite.benefitUrl)">打开</button>
                        <button type="button" class="sl-btn-xs" @click="copyText(selectedSite.benefitUrl, '福利站地址')">复制</button>
                      </div>
                    </div>
                    <div v-if="selectedSite.statusUrl" class="sl-link-item">
                      <div>
                        <strong>系统状态页</strong>
                        <small>{{ selectedSite.statusUrl }}</small>
                      </div>
                      <div class="sl-link-item-actions">
                        <button type="button" class="sl-btn-xs" @click="store.openExternal(selectedSite.statusUrl)">打开</button>
                        <button type="button" class="sl-btn-xs" @click="copyText(selectedSite.statusUrl, '状态页地址')">复制</button>
                      </div>
                    </div>
                  </div>
                </div>
              </div>

              <!-- TAB 2: 账号与额度 -->
              <div v-else-if="activeDetailTab === 'accounts'" class="sl-tab-panel">
                <div class="sl-tab-action-bar">
                  <span class="sl-tab-section-title">已关联 Chrome 授权账号</span>
                  <button
                    type="button"
                    class="sl-btn-secondary"
                    @click="store.syncChromeSession(selectedSite, $event.currentTarget as HTMLElement)"
                  >
                    <span v-html="icons.restore" />
                    <span>同步此站点全部账号</span>
                  </button>
                </div>

                <div v-if="getSiteSessions(selectedSite.id).length" class="sl-drawer-accounts-grid">
                  <div
                    v-for="session in getSiteSessions(selectedSite.id)"
                    :key="session.profileId"
                    class="sl-drawer-acc-card"
                  >
                    <div class="sl-drawer-acc-head">
                      <div class="sl-drawer-acc-user">
                        <span class="sl-drawer-acc-avatar" v-html="icons.user" />
                        <div>
                          <strong>{{ session.username || session.accountName || session.profileName }}</strong>
                          <div class="sl-acc-chips-row">
                            <span v-if="session.newapiUserId" class="sl-user-id-pill">UID: {{ session.newapiUserId }}</span>
                            <template v-if="normalizeSystemType(selectedSite.systemType) === 'newapi2'">
                              <span v-if="session.hasAccessToken" class="sl-token-pill active" title="已从 Chrome 会话中获取访问令牌">已取令牌</span>
                              <span v-else class="sl-token-pill" title="未获取到访问令牌，需刷新">无令牌</span>
                            </template>
                            <template v-if="selectedSite.supportsCheckin || session.checkinEnabled">
                              <span v-if="session.checkedInToday" class="sl-token-pill sl-token-checked" title="今日已成功签到">今日已签到</span>
                              <span v-else-if="session.checkinEnabled" class="sl-token-pill sl-token-uncheck" title="站点开启签到，今日尚未签到">今日未签到</span>
                              <span v-else class="sl-token-pill sl-token-disabled" title="站点接口返回404/403/人机验证或功能未启用，无法签到">无法签到</span>
                            </template>
                            <span v-if="session.syncError || session.checkinError" class="sl-token-pill sl-token-err" :title="session.syncError || session.checkinError">异常</span>
                          </div>
                        </div>
                      </div>

                      <div class="sl-drawer-acc-quota-block">
                        <div class="sl-drawer-acc-quota">
                          <span class="sl-acc-quota-k">可用额度</span>
                          <strong class="sl-acc-quota-v text-brand">{{ formatAccountQuota(session.remaining, session.unit) }}</strong>
                        </div>
                        <div class="sl-drawer-acc-btn-group">
                          <button
                            type="button"
                            class="sl-btn-xs"
                            :title="`使用 ${session.profileName} 在 Chrome 中打开此站点`"
                            @click="store.openExternalInChromeProfile(selectedSite.apiBaseUrl, session.profileId)"
                          >
                            <span v-html="icons.external" />
                            <span>打开站点</span>
                          </button>
                          <button
                            type="button"
                            class="sl-btn-xs is-danger"
                            :title="`删除「${session.profileName}」会话账号，解除与本站点的关联`"
                            @click="store.deleteSiteAccount(selectedSite, session)"
                          >
                            <span v-html="icons.trash" />
                            <span>删除账号</span>
                          </button>
                        </div>
                      </div>
                    </div>

                    <div class="sl-drawer-acc-facts">
                      <div class="sl-acc-fact">
                        <span>Chrome 配置</span>
                        <strong>{{ session.profileName || "默认" }}</strong>
                      </div>
                      <div class="sl-acc-fact">
                        <span>已同步 Key</span>
                        <strong>{{ session.apiKeyCount || 0 }} 个</strong>
                      </div>
                      <div class="sl-acc-fact">
                        <span>已同步模型</span>
                        <strong>{{ session.apiModelCount || 0 }} 款</strong>
                      </div>
                      <div class="sl-acc-fact">
                        <span>最后同步时间</span>
                        <strong class="font-mono text-xs">{{ formatDate(session.accountUpdatedAt) }}</strong>
                      </div>
                    </div>

                    <div v-if="session.syncError || session.checkinError" class="sl-drawer-acc-error">
                      <span v-html="icons.info" />
                      <span>{{ session.syncError || session.checkinError }}</span>
                    </div>
                  </div>
                </div>
                <div v-else class="sl-empty-state-compact">
                  <span v-html="icons.user" />
                  <p>该站点尚未提取或关联 Chrome 授权账号会话</p>
                  <button
                    type="button"
                    class="sl-btn-primary"
                    @click="store.syncChromeSession(selectedSite, $event.currentTarget as HTMLElement)"
                  >
                    立即提取 Chrome 会话
                  </button>
                </div>
              </div>

              <!-- TAB 3: 关联 Key 与支持模型 -->
              <div v-else-if="activeDetailTab === 'models'" class="sl-tab-panel">
                <div class="sl-tab-action-bar">
                  <div class="sl-models-search-box">
                    <span v-html="icons.search" />
                    <input
                      v-model="drawerModelSearch"
                      type="search"
                      placeholder="搜索此站点的支持模型 ID 或厂商…"
                    />
                  </div>

                  <div class="sl-models-sync-btns">
                    <button
                      type="button"
                      class="sl-btn-xs"
                      :disabled="drawerLiveFetchingKind !== null"
                      @click="triggerDrawerSync('keys')"
                    >
                      <span v-html="icons.key" :class="{ 'is-spinning': drawerLiveFetchingKind === 'keys' }" />
                      <span>同步 Key</span>
                    </button>
                    <button
                      type="button"
                      class="sl-btn-xs sl-btn-accent"
                      :disabled="drawerLiveFetchingKind !== null"
                      @click="triggerDrawerSync('models')"
                    >
                      <span v-html="icons.restore" :class="{ 'is-spinning': drawerLiveFetchingKind === 'models' }" />
                      <span>同步模型</span>
                    </button>
                  </div>
                </div>

                <!-- 账号与 Key 筛选栏 -->
                <div v-if="drawerModelCache?.accounts?.length" class="sl-keys-bar">
                  <span class="sl-keys-label">关联 Key：</span>
                  <div class="sl-keys-chips">
                    <button
                      type="button"
                      class="sl-key-chip"
                      :class="{ active: drawerSelectedKey === null }"
                      @click="drawerSelectedKey = null"
                    >
                      全部模型 ({{ drawerModelCache.models?.length || 0 }})
                    </button>
                    <template v-for="acc in drawerModelCache.accounts" :key="acc.profileId">
                      <button
                        v-for="k in acc.keys"
                        :key="k"
                        type="button"
                        class="sl-key-chip font-mono"
                        :class="{ active: drawerSelectedKey === k }"
                        @click="drawerSelectedKey = drawerSelectedKey === k ? null : k"
                      >
                        <span>{{ maskApiKey(k) }}</span>
                        <small>({{ acc.keyModels?.[k]?.length ?? '—' }})</small>
                      </button>
                    </template>
                  </div>
                </div>

                <div v-if="drawerModelsLoading" class="sl-loading-state">
                  <span class="is-spinning" v-html="icons.restore" />
                  <p>正在读取模型与 Key 列表…</p>
                </div>

                <div v-else-if="!drawerFilteredModels.length" class="sl-empty-state-compact">
                  <span v-html="icons.cpu" />
                  <p>{{ drawerModelSearch ? "没有匹配的模型" : "暂无已缓存的站点支持模型数据" }}</p>
                  <button
                    type="button"
                    class="sl-btn-secondary"
                    @click="triggerDrawerSync('models')"
                  >
                    立即拉取最新模型列表
                  </button>
                </div>

                <div v-else class="sl-drawer-models-grid">
                  <div
                    v-for="m in drawerFilteredModels"
                    :key="m.id"
                    class="sl-drawer-model-item"
                    @click="copyText(m.id, '模型 ID')"
                  >
                    <div class="sl-model-item-info">
                      <strong class="font-mono">{{ m.id }}</strong>
                      <small v-if="m.ownedBy" class="text-faint">厂商: {{ m.ownedBy }}</small>
                    </div>
                    <button type="button" class="sl-model-copy-btn" title="复制模型 ID">
                      <span v-html="icons.copy" />
                    </button>
                  </div>
                </div>
              </div>
            </div>
          </template>
        </aside>
      </div>
    </Teleport>
  </div>
</template>

<style scoped>
/* ============================================================
   站点库全景控制台核心样式 (参照模型参数设计体系)
   ============================================================ */
.sl-explorer-root {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  background: var(--page-bg, #0d1117);
  color: var(--text, #c9d1d9);
  overflow: hidden;
}

/* 1. 顶部驾驶舱 */
.sl-cockpit-bar {
  padding: 12px 20px 10px;
  background: var(--surface);
  border-bottom: 1px solid var(--line);
  flex-shrink: 0;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.sl-cockpit-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 16px;
}

.sl-brand-section {
  display: flex;
  flex-direction: column;
  gap: 1px;
  min-width: 0;
}

.sl-eyebrow-row {
  display: flex;
  align-items: center;
  gap: 6px;
}

.sl-eyebrow-text {
  font-size: 10px;
  color: var(--brand, #58a6ff);
  font-weight: 750;
  letter-spacing: 0.06em;
  text-transform: uppercase;
}

.sl-live-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--success, #2ea043);
  box-shadow: 0 0 8px var(--success, #2ea043);
  animation: pulse-dot 2s infinite;
}

@keyframes pulse-dot {
  0%, 100% { opacity: 1; transform: scale(1); }
  50% { opacity: 0.5; transform: scale(1.2); }
}

.sl-title-row {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

.sl-title-row h1 {
  margin: 0;
  font-size: 18px;
  font-weight: 750;
  color: var(--text, #f0f6fc);
  line-height: 1.2;
}

.sl-cockpit-subtitle {
  margin: 0;
  font-size: 11px;
  color: var(--muted, #8b949e);
}

.sl-cockpit-subtitle strong {
  color: var(--text, #f0f6fc);
  font-weight: 600;
}

.sl-cockpit-actions {
  display: flex;
  align-items: center;
  gap: 12px;
}

/* 头部按钮与其他页面驾驶舱横条对齐（32px 高度） */
.sl-cockpit-actions .sl-btn-secondary,
.sl-cockpit-actions .sl-btn-primary {
  height: 32px;
  padding: 0 12px;
  font-size: 12px;
}

.sl-sync-status-card {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 12px;
  background: var(--surface, #21262d);
  border: 1px solid var(--line, #30363d);
  border-radius: var(--r-md, 8px);
}

.sl-status-indicator {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--warning, #d29922);
}

.sl-status-indicator.synced {
  background: var(--success, #3fb950);
}

.sl-status-text {
  display: flex;
  flex-direction: column;
}

.sl-status-text strong {
  font-size: 11.5px;
  color: var(--text, #f0f6fc);
}

.sl-status-text small {
  font-size: 10px;
  color: var(--muted, #8b949e);
}

/* 按钮规范 */
.sl-btn-primary {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 36px;
  padding: 0 14px;
  border-radius: var(--r-md, 8px);
  background: var(--brand, #1f6feb);
  color: #fff;
  font-size: 12.5px;
  font-weight: 600;
  border: none;
  cursor: pointer;
  transition: background 0.15s ease;
}

.sl-btn-primary:hover {
  background: color-mix(in srgb, var(--brand, #1f6feb) 85%, white);
}

.sl-btn-secondary {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 36px;
  padding: 0 12px;
  border-radius: var(--r-md, 8px);
  background: var(--surface, #21262d);
  color: var(--text, #c9d1d9);
  font-size: 12.5px;
  font-weight: 550;
  border: 1px solid var(--line, #30363d);
  cursor: pointer;
  transition: all 0.15s ease;
}

.sl-btn-secondary:hover:not(:disabled) {
  background: var(--surface-hover, #30363d);
  color: var(--text, #f0f6fc);
}

.sl-btn-secondary:disabled {
  opacity: 0.5;
  cursor: wait;
}

.sl-btn-xs {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  height: 26px;
  padding: 0 8px;
  border-radius: var(--r-sm, 6px);
  background: var(--surface, #21262d);
  color: var(--text, #c9d1d9);
  font-size: 11px;
  border: 1px solid var(--line, #30363d);
  cursor: pointer;
}

.sl-btn-xs:hover:not(:disabled) {
  background: var(--surface-hover, #30363d);
  color: var(--text, #f0f6fc);
}

.sl-btn-accent {
  background: color-mix(in srgb, var(--brand, #1f6feb) 20%, var(--surface, #21262d)) !important;
  color: var(--brand, #58a6ff) !important;
  border-color: color-mix(in srgb, var(--brand, #1f6feb) 40%, transparent) !important;
}

/* 宏观 4 大指标 */
.sl-metrics-grid {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 12px;
}

.sl-metric-card {
  padding: 10px 14px;
  background: var(--surface, #21262d);
  border: 1px solid var(--line, #30363d);
  border-radius: var(--r-md, 8px);
  display: flex;
  align-items: center;
  gap: 12px;
  cursor: default;
  user-select: none;
}

.sl-metric-icon {
  width: 36px;
  height: 36px;
  border-radius: var(--r-md, 8px);
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
}

.sl-metric-icon svg {
  width: 18px;
  height: 18px;
}

.sl-tone-brand { background: color-mix(in srgb, var(--brand, #1f6feb) 15%, transparent); color: var(--brand, #58a6ff); }
.sl-tone-success { background: color-mix(in srgb, var(--success, #2ea043) 15%, transparent); color: var(--success, #3fb950); }
.sl-tone-violet { background: color-mix(in srgb, #a371f7 15%, transparent); color: #bc8cff; }
.sl-tone-info { background: color-mix(in srgb, #388bfd 15%, transparent); color: #79c0ff; }
.sl-tone-neutral { background: var(--surface-soft, #161b22); color: var(--muted, #8b949e); }

.sl-metric-info {
  display: flex;
  flex-direction: column;
  min-width: 0;
  flex: 1;
}

.sl-metric-label {
  font-size: 11px;
  color: var(--muted, #8b949e);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.sl-metric-val {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 2px;
  margin-top: 3px;
  min-width: 0;
}

.sl-metric-val strong {
  font-size: 16.5px;
  font-weight: 700;
  color: var(--text, #f0f6fc);
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  line-height: 1.25;
  max-width: 100%;
}

.sl-metric-val small {
  font-size: 10.5px;
  color: var(--muted, #8b949e);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  line-height: 1.3;
  max-width: 100%;
}

/* 热门分类一键直达区域 */
.sl-popular-chips-bar {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  position: relative;
  transition: all 0.2s ease;
}

.sl-chips-label {
  font-size: 11.5px;
  color: var(--muted, #8b949e);
  flex-shrink: 0;
  padding-top: 4px;
  line-height: 1;
}

.sl-chips-wrap {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px 8px;
  flex: 1;
  min-width: 0;
}

.sl-chip-btn {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 3px 10px;
  border-radius: var(--r-full);
  background: var(--surface);
  border: 1px solid var(--line);
  color: var(--text);
  font-size: 11.5px;
  font-weight: 500;
  cursor: pointer;
  white-space: nowrap;
  transition: all 0.15s ease;
}

.sl-chip-btn:hover {
  background: var(--surface-hover);
  border-color: var(--line-strong);
}

.sl-chip-btn.active {
  background: var(--brand-soft);
  border-color: var(--brand);
  color: var(--brand-deep);
  font-weight: 600;
}

.sl-chip-num {
  font-size: 9.5px;
  padding: 1px 5px;
  border-radius: var(--r-full);
  background: var(--surface-hover);
  color: var(--muted);
}

.sl-chip-btn.active .sl-chip-num {
  background: var(--brand);
  color: #fff;
}

.sl-chips-toggle-btn {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 3px 10px;
  border-radius: var(--r-full);
  background: var(--surface-soft);
  border: 1px solid var(--line-soft, var(--line));
  color: var(--muted);
  font-size: 11px;
  font-weight: 500;
  cursor: pointer;
  white-space: nowrap;
  transition: all 0.15s ease;
}

.sl-chips-toggle-btn:hover {
  color: var(--text);
  background: var(--surface-hover);
  border-color: var(--line-strong);
}

.sl-chips-toggle-btn.active {
  color: var(--brand-deep);
  border-color: var(--brand);
  background: var(--brand-soft);
}

.sl-toggle-arrow {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 12px;
  height: 12px;
  transition: transform 0.2s ease;
}

.sl-toggle-arrow.is-up {
  transform: rotate(180deg);
}

.sl-toggle-arrow svg {
  width: 12px;
  height: 12px;
}

/* 2. 控制中心 */
.sl-control-center {
  padding: 12px 24px;
  background: var(--page-bg, #0d1117);
  border-bottom: 1px solid var(--line, #30363d);
  display: flex;
  flex-direction: column;
  gap: 10px;
  flex-shrink: 0;
}

.sl-control-top-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
}

.sl-control-tabs-group {
  display: flex;
  align-items: center;
  gap: 12px;
  min-width: 0;
  overflow-x: auto;
  scrollbar-width: none;
}
.sl-control-tabs-group::-webkit-scrollbar {
  display: none;
}

.sl-tabs-divider {
  width: 1px;
  height: 18px;
  background: var(--line);
  flex-shrink: 0;
}

.sl-alive-tabs {
  display: inline-flex;
  align-items: center;
  gap: 3px;
  padding: 3px;
  border-radius: var(--r-md);
  background: var(--surface);
  border: 1px solid var(--line);
  flex-shrink: 0;
}

.sl-alive-tabs button {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 4px 10px;
  border-radius: calc(var(--r-md) - 2px);
  font-size: 11.5px;
  font-weight: 500;
  border: 1px solid transparent;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  transition: all 0.15s ease;
  white-space: nowrap;
}

.sl-alive-tabs button:hover {
  color: var(--text);
  background: var(--surface-hover);
}

.sl-usage-tabs button:disabled,
.sl-alive-tabs button:disabled {
  opacity: 0.35;
  cursor: not-allowed;
  pointer-events: none;
}

.sl-alive-tabs button.active {
  background: var(--brand-soft);
  border-color: var(--brand);
  color: var(--brand-deep);
  font-weight: 600;
  box-shadow: var(--shadow-xs);
}

.sl-alive-tabs button.active .sl-tab-badge {
  background: var(--brand);
  color: #fff;
}

.sl-alive-tabs button.is-runaway.active {
  background: color-mix(in srgb, var(--danger, #f85149) 12%, var(--surface));
  border-color: var(--danger, #f85149);
  color: var(--danger, #f85149);
}

.sl-alive-tabs button.is-runaway.active .sl-tab-badge {
  background: var(--danger, #f85149);
  color: #fff;
}

.sl-alive-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  flex-shrink: 0;
}
.sl-alive-dot.is-live {
  background: #10b981;
}
.sl-alive-dot.is-dead {
  background: #ef4444;
}

.sl-usage-tabs {
  display: flex;
  align-items: center;
  gap: 4px;
  overflow-x: auto;
  scrollbar-width: none;
  -ms-overflow-style: none;
}
.sl-usage-tabs::-webkit-scrollbar {
  display: none;
  width: 0;
  height: 0;
}

.sl-usage-tabs button {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 5px 12px;
  border-radius: var(--r-md);
  font-size: 12px;
  font-weight: 500;
  border: 1px solid transparent;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  transition: all 0.15s ease;
  white-space: nowrap;
}

.sl-usage-tabs button:hover {
  background: var(--surface-hover);
  color: var(--text);
}

.sl-usage-tabs button.active {
  background: var(--brand-soft);
  border-color: var(--brand);
  color: var(--brand-deep);
  font-weight: 600;
  box-shadow: var(--shadow-xs);
}

.sl-usage-tabs button :deep(svg),
.sl-usage-tabs button svg {
  width: 14px;
  height: 14px;
  color: currentColor;
}

.sl-tab-badge {
  font-size: 10px;
  font-weight: 600;
  padding: 1px 5.5px;
  border-radius: var(--r-full);
  background: var(--surface-hover);
  color: var(--muted);
  transition: all 0.15s ease;
}

.sl-usage-tabs button.active .sl-tab-badge {
  background: var(--brand);
  color: #fff;
}

.sl-view-switcher {
  display: inline-flex;
  padding: 2px;
  border-radius: var(--r-md);
  background: var(--surface-soft);
  border: 1px solid var(--line-soft, var(--line));
}

.sl-view-switcher button {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 5px 10px;
  border-radius: var(--r-sm);
  font-size: 11.5px;
  font-weight: 500;
  border: none;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  transition: all 0.15s ease;
}

.sl-view-switcher button:hover {
  color: var(--text);
}

.sl-view-switcher button.active {
  background: var(--surface);
  color: var(--text);
  font-weight: 600;
  box-shadow: var(--shadow-xs);
}

.sl-view-switcher button :deep(svg),
.sl-view-switcher button svg {
  width: 13px;
  height: 13px;
}

/* 筛选行 */
.sl-filters-row {
  display: grid;
  grid-template-columns: minmax(220px, 1.5fr) repeat(4, minmax(130px, 1fr));
  gap: 8px;
}

.sl-search-box {
  position: relative;
  display: flex;
  align-items: center;
  background: var(--surface, #21262d);
  border: 1px solid var(--line, #30363d);
  border-radius: var(--r-md, 8px);
  padding: 0 10px;
  height: 36px;
}

.sl-search-box svg {
  width: 15px;
  height: 15px;
  color: var(--muted, #8b949e);
  flex-shrink: 0;
}

.sl-search-box input {
  width: 100%;
  height: 100%;
  border: none;
  background: transparent;
  color: var(--text, #f0f6fc);
  font-size: 12px;
  padding: 0 8px;
  outline: none;
}

.sl-search-box kbd {
  font-size: 9.5px;
  color: var(--faint, #6e7681);
  background: var(--surface-soft, #161b22);
  border: 1px solid var(--line, #30363d);
  border-radius: 4px;
  padding: 1px 4px;
  flex-shrink: 0;
}

.sl-clear-search {
  background: none;
  border: none;
  color: var(--muted, #8b949e);
  cursor: pointer;
  padding: 2px;
  display: flex;
  align-items: center;
}

.sl-filter-dropdown {
  height: 36px;
}

/* 特性开关芯片栏 */
.sl-feature-bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

.sl-feature-chips {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.sl-chips-title {
  font-size: 11px;
  color: var(--muted, #8b949e);
}

.sl-chip {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 3px 9px;
  border-radius: var(--r-full);
  font-size: 11px;
  font-weight: 500;
  border: 1px solid var(--line);
  background: var(--surface-soft);
  color: var(--muted);
  cursor: pointer;
  transition: all 0.15s ease;
}

.sl-chip:hover {
  background: var(--surface-hover);
  color: var(--text);
}

.sl-chip.active {
  background: var(--brand-soft);
  border-color: var(--brand);
  color: var(--brand-deep);
  font-weight: 600;
}

.sl-results-meta {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-shrink: 0;
}

.sl-clear-filters-btn {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  background: none;
  border: none;
  color: var(--danger, #f85149);
  font-size: 11.5px;
  cursor: pointer;
}

.sl-clear-filters-btn svg {
  width: 12px;
  height: 12px;
}

.sl-filter-count {
  font-size: 11px;
  color: var(--muted, #8b949e);
}

.sl-filter-count b {
  color: var(--text, #f0f6fc);
}

/* 3. 主视图 */
.sl-main-content {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 16px 24px 32px;
}

.sl-main-content.is-table-mode {
  overflow: hidden;
  display: flex;
  flex-direction: column;
  padding: 16px 24px 20px;
}

/* 视图 A：卡片网格 */
.sl-cards-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
  gap: 14px;
}

.sl-card {
  background: var(--surface, #161b22);
  border: 1px solid var(--line, #30363d);
  border-radius: var(--r-lg, 10px);
  padding: 14px;
  display: flex;
  flex-direction: column;
  gap: 10px;
  cursor: pointer;
  transition: all 0.18s cubic-bezier(0.16, 1, 0.3, 1);
  position: relative;
}

.sl-card:hover {
  border-color: color-mix(in srgb, var(--brand, #58a6ff) 45%, var(--line, #30363d));
  box-shadow: 0 4px 16px rgba(0,0,0,0.25);
  transform: translateY(-2px);
}

.sl-card.is-selected {
  border-color: var(--brand, #58a6ff);
  box-shadow: 0 0 0 1px var(--brand, #58a6ff);
}

.sl-card.is-runaway {
  opacity: 0.75;
  border-color: color-mix(in srgb, var(--danger, #f85149) 30%, var(--line, #30363d));
}

.sl-card-head {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 10px;
}

.sl-card-identity {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
}

.sl-card-avatar {
  width: 38px;
  height: 38px;
  border-radius: var(--r-md, 8px);
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 13px;
  font-weight: 750;
  flex-shrink: 0;
}

.sl-avatar-sm {
  width: 28px;
  height: 28px;
  font-size: 11px;
}

.sl-card-title-box {
  min-width: 0;
  display: flex;
  flex-direction: column;
}

.sl-card-title-row {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.sl-card-title-row h3 {
  margin: 0;
  font-size: 14px;
  font-weight: 700;
  color: var(--text, #f0f6fc);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.sl-card-url {
  font-size: 10px;
  color: var(--faint, #6e7681);
  font-family: monospace;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  margin-top: 2px;
}

.sl-card-head-tools {
  display: flex;
  align-items: center;
  gap: 4px;
}

.sl-card-select-btn {
  width: 24px;
  height: 24px;
  border-radius: 6px;
  background: var(--surface-soft, #21262d);
  border: 1px solid var(--line, #30363d);
  color: var(--muted, #8b949e);
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
}

.sl-card-select-btn svg {
  width: 12px;
  height: 12px;
}

.sl-card-select-btn:hover {
  color: var(--text, #f0f6fc);
}

.sl-card-select-btn.active {
  background: var(--brand, #1f6feb);
  color: #fff;
  border-color: var(--brand, #1f6feb);
}

.sl-card-err-badge {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 24px;
  height: 24px;
  border-radius: 6px;
  background: color-mix(in srgb, var(--danger, #f85149) 14%, transparent);
  border: 1px solid color-mix(in srgb, var(--danger, #f85149) 35%, transparent);
  color: var(--danger, #f85149);
  cursor: help;
  flex-shrink: 0;
  box-sizing: border-box;
}

.sl-card-err-badge svg {
  width: 13px;
  height: 13px;
  display: block;
}

.sl-token-pill.sl-token-err {
  background: color-mix(in srgb, var(--danger, #f85149) 15%, transparent);
  color: var(--danger, #f85149);
  border: 1px solid color-mix(in srgb, var(--danger, #f85149) 30%, transparent);
  cursor: help;
}

/* 药丸与徽章 */
.sl-pill {
  font-size: 9.5px;
  padding: 1px 6px;
  border-radius: 4px;
  font-weight: 600;
}

.sl-pill-system {
  background: var(--surface-soft, #21262d);
  border: 1px solid var(--line, #30363d);
  color: var(--muted, #8b949e);
}

.sl-pill-brand { color: var(--brand, #58a6ff); border-color: color-mix(in srgb, var(--brand, #58a6ff) 30%, transparent); }
.sl-pill-violet { color: #bc8cff; border-color: color-mix(in srgb, #bc8cff) 30%, transparent); }
.sl-pill-info { color: #79c0ff; border-color: color-mix(in srgb, #79c0ff 30%, transparent); }
.sl-pill-success { color: #3fb950; border-color: color-mix(in srgb, #3fb950 30%, transparent); }

.sl-pill-runaway { background: color-mix(in srgb, var(--danger, #f85149) 15%, transparent); color: var(--danger, #f85149); }
.sl-pill-personal { background: color-mix(in srgb, var(--success, #2ea043) 15%, transparent); color: var(--success, #3fb950); }
.sl-pill-pending { background: color-mix(in srgb, var(--warning, #d29922) 15%, transparent); color: var(--warning, #d29922); }
.sl-pill-fake { background: color-mix(in srgb, #ff7b72 15%, transparent); color: #ff7b72; }

.sl-card-badges {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.sl-card-tag {
  font-size: 10px;
  padding: 2px 6px;
  border-radius: 4px;
  background: var(--surface-soft, #21262d);
  border: 1px solid var(--line, #30363d);
  color: var(--muted, #8b949e);
}

.sl-card-feat {
  font-size: 9.5px;
  padding: 2px 6px;
  border-radius: 4px;
  background: color-mix(in srgb, var(--brand, #1f6feb) 10%, transparent);
  border: 1px solid color-mix(in srgb, var(--brand, #1f6feb) 20%, transparent);
  color: var(--text, #c9d1d9);
}

.sl-feat-nsfw { color: #f85149; border-color: color-mix(in srgb, #f85149 25%, transparent); }
.sl-feat-invite { color: #d29922; border-color: color-mix(in srgb, #d29922 25%, transparent); }

.sl-card-tags-row {
  display: flex;
  align-items: center;
  gap: 4px;
  flex-wrap: wrap;
}

.sl-tag-chip {
  font-size: 9px;
  padding: 1px 5px;
  border-radius: 3px;
  background: var(--surface-hover, #282e36);
  color: var(--faint, #8b949e);
}

.sl-tag-more {
  font-size: 8.5px;
  color: var(--faint, #6e7681);
}

.sl-card-specs {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 8px;
  padding: 8px 10px;
  background: var(--surface-soft, #0d1117);
  border-radius: var(--r-md, 8px);
}

.sl-spec-item {
  display: flex;
  flex-direction: column;
}

.sl-spec-k {
  font-size: 9.5px;
  color: var(--muted, #8b949e);
}

.sl-spec-v {
  font-size: 11.5px;
  color: var(--text, #f0f6fc);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.sl-card-desc {
  margin: 0;
  font-size: 11px;
  line-height: 1.4;
  color: var(--muted, #8b949e);
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
  min-height: 30px;
}

.sl-card-desc.muted {
  color: var(--faint, #6e7681);
  font-style: italic;
}

.sl-card-footer {
  display: flex;
  justify-content: space-between;
  align-items: center;
  border-top: 1px solid var(--line-soft, #21262d);
  padding-top: 8px;
  margin-top: auto;
}

.sl-card-links {
  display: flex;
  align-items: center;
  gap: 4px;
}

.sl-link-btn {
  width: 26px;
  height: 26px;
  border-radius: 6px;
  background: var(--surface-soft, #21262d);
  border: 1px solid var(--line, #30363d);
  color: var(--muted, #8b949e);
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  transition: all 0.15s ease;
}

.sl-link-btn svg {
  width: 13px;
  height: 13px;
}

.sl-link-btn:hover {
  background: var(--surface-hover, #30363d);
  color: var(--text, #f0f6fc);
}

.sl-link-btn.active {
  color: var(--brand, #58a6ff);
}

.sl-card-footer-actions {
  display: flex;
  align-items: center;
  gap: 6px;
}

.sl-card-models-btn {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  height: 26px;
  padding: 0 8px;
  border-radius: 6px;
  background: var(--surface-soft, #21262d);
  border: 1px solid var(--line, #30363d);
  color: var(--text, #c9d1d9);
  font-size: 10.5px;
  cursor: pointer;
}

.sl-card-models-btn svg {
  width: 12px;
  height: 12px;
}

.sl-card-models-btn:hover {
  background: var(--surface-hover, #30363d);
  color: var(--text, #f0f6fc);
}

.sl-card-detail-btn {
  display: inline-flex;
  align-items: center;
  gap: 2px;
  height: 26px;
  padding: 0 8px;
  border-radius: 6px;
  background: color-mix(in srgb, var(--brand, #1f6feb) 15%, transparent);
  border: 1px solid color-mix(in srgb, var(--brand, #1f6feb) 30%, transparent);
  color: var(--brand, #58a6ff);
  font-size: 10.5px;
  font-weight: 600;
  cursor: pointer;
}

.sl-card-detail-btn svg {
  width: 12px;
  height: 12px;
}

.sl-card-detail-btn:hover {
  background: var(--brand, #1f6feb);
  color: #fff;
}

/* 分页 */
.sl-pagination-bar {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 12px;
  margin-top: 20px;
}

.sl-page-btn {
  padding: 6px 14px;
  border-radius: 6px;
  background: var(--surface, #21262d);
  border: 1px solid var(--line, #30363d);
  color: var(--text, #c9d1d9);
  font-size: 12px;
  cursor: pointer;
}

.sl-page-btn:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}

.sl-page-info {
  font-size: 12px;
  color: var(--muted, #8b949e);
}

/* 视图 B：表格定制单元格 */
.sl-table-view {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 10px);
  overflow: hidden;
  height: 100%;
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  box-shadow: var(--shadow-sm);
}

.sl-table-view :deep(.app-table-wrap) {
  height: 100%;
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
}

.sl-table-view :deep(.app-table-scroll) {
  flex: 1;
  min-height: 0;
  overflow: auto;
}

.sl-table-view :deep(.app-table) {
  width: 100%;
  table-layout: fixed;
  border-collapse: separate;
  border-spacing: 0;
}

.sl-table-view :deep(.app-table-th),
.sl-table-view :deep(.app-table-td) {
  padding: 10px 12px;
  vertical-align: middle;
  border-bottom: 1px solid var(--line-soft, var(--line));
}

.sl-table-view :deep(.app-table-th) {
  background: var(--surface-soft);
  color: var(--muted);
  font-size: 11.5px;
  font-weight: 600;
  white-space: nowrap;
}

.sl-table-view :deep(.app-table-tr.clickable:hover) {
  background: var(--surface-hover);
}

.sl-table-view :deep(.app-table-tr.clickable.active) {
  background: var(--brand-soft);
}

.sl-table-site-cell {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
  max-width: 100%;
}

.sl-table-site-info {
  display: flex;
  flex-direction: column;
  min-width: 0;
  flex: 1;
  overflow: hidden;
}

.sl-table-site-title {
  display: flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
}

.sl-table-site-title strong {
  font-size: 12.5px;
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 100%;
}

.sl-table-site-url {
  font-size: 10.5px;
  color: var(--muted);
  font-family: monospace;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 100%;
}

.sl-table-accounts-cell {
  display: flex;
  flex-direction: column;
  align-items: center;
  font-size: 11.5px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.sl-level-badge {
  font-size: 10.5px;
  font-weight: 700;
  color: var(--brand);
}

.sl-table-caps-cell {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 4px;
  font-size: 13px;
}

.sl-table-actions-cell {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 4px;
}

.sl-action-icon-btn {
  width: 26px;
  height: 26px;
  padding: 0;
  margin: 0;
  border-radius: 6px;
  background: var(--surface-soft);
  border: 1px solid var(--line);
  color: var(--muted);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  line-height: 0;
  vertical-align: middle;
  cursor: pointer;
  box-sizing: border-box;
}

.sl-action-icon-btn span {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 13px;
  height: 13px;
  line-height: 0;
}

.sl-action-icon-btn svg {
  display: block;
  width: 13px;
  height: 13px;
  flex-shrink: 0;
}

.sl-action-icon-btn:hover {
  background: var(--surface-hover);
  color: var(--text);
}

.sl-action-icon-btn.sl-topo-delete-btn:hover {
  background: color-mix(in srgb, var(--danger, #f85149) 15%, var(--surface-hover, #30363d));
  color: var(--danger, #f85149);
  border-color: color-mix(in srgb, var(--danger, #f85149) 35%, transparent);
}

.sl-btn-xs.is-danger {
  color: var(--danger, #f85149);
  border-color: color-mix(in srgb, var(--danger, #f85149) 35%, transparent);
}

.sl-btn-xs.is-danger:hover:not(:disabled) {
  background: color-mix(in srgb, var(--danger, #f85149) 15%, var(--surface-hover, #30363d));
  color: var(--danger, #f85149);
}

/* 视图 C：拓扑矩阵 */
.sl-topology-view {
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.sl-topology-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 12px 16px;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  gap: 12px;
}

.sl-topology-header-actions {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  flex-shrink: 0;
}

.sl-topology-header h2 {
  margin: 0;
  font-size: 15px;
  font-weight: 750;
  color: var(--text);
}

.sl-topology-header p {
  margin: 2px 0 0;
  font-size: 11.5px;
  color: var(--muted);
}

.sl-topology-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(380px, 1fr));
  gap: 14px;
}

.sl-topo-card {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 10px);
  padding: 14px;
  display: flex;
  flex-direction: column;
  gap: 10px;
  cursor: pointer;
  transition: border-color .2s var(--ease), box-shadow .2s var(--ease), transform .18s var(--ease);
}

.sl-topo-card:hover {
  border-color: var(--line-strong);
  box-shadow: var(--shadow-sm);
}

.sl-topo-card.is-selected {
  border-color: var(--brand);
  box-shadow: 0 0 0 2px var(--brand-glow);
}

.sl-topo-card.is-runaway {
  opacity: 0.82;
  border-color: color-mix(in srgb, var(--danger) 30%, var(--line));
}

.sl-topo-card.has-sessions {
  border-color: color-mix(in srgb, var(--brand) 30%, var(--line));
}

.sl-topo-card-head {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 10px;
}

.sl-topo-site-id {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  min-width: 0;
  flex: 1;
}

.sl-topo-title-box {
  display: flex;
  flex-direction: column;
  min-width: 0;
  flex: 1;
  gap: 2px;
}

.sl-topo-title-row {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.sl-topo-title-row strong {
  font-size: 13.5px;
  font-weight: 700;
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.sl-topo-head-actions {
  display: flex;
  align-items: center;
  gap: 5px;
  flex-shrink: 0;
}

.sl-topo-head-btn {
  width: 28px;
  height: 28px;
  padding: 0;
  margin: 0;
  border-radius: var(--r-xs, 6px);
  background: var(--surface-soft);
  border: 1px solid var(--line);
  color: var(--muted);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  line-height: 0;
  vertical-align: middle;
  cursor: pointer;
  box-shadow: 0 1px 2px rgba(16, 35, 25, 0.03);
  transition: all .18s var(--ease);
  box-sizing: border-box;
}

.sl-topo-head-btn span {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 14px;
  height: 14px;
  line-height: 0;
}

.sl-topo-head-btn svg {
  display: block;
  width: 14px;
  height: 14px;
  flex-shrink: 0;
  transition: transform .25s var(--ease), color .18s var(--ease);
}

.sl-topo-head-btn:hover {
  background: var(--surface-hover);
  border-color: var(--line-strong);
  color: var(--text);
  transform: translateY(-1px);
  box-shadow: 0 2px 6px rgba(16, 35, 25, 0.06);
}

.sl-topo-head-btn:active {
  transform: translateY(0) scale(0.95);
}

.sl-topo-sync-btn:hover {
  background: var(--brand-soft);
  border-color: color-mix(in srgb, var(--brand) 40%, transparent);
  color: var(--brand-deep);
}

.sl-topo-sync-btn:hover svg {
  transform: rotate(180deg);
}

.sl-topo-sync-btn.is-syncing svg {
  animation: sl-spin 1s linear infinite;
  color: var(--brand);
}

.sl-topo-select-btn.active {
  background: var(--brand);
  color: #fff;
  border-color: var(--brand);
  box-shadow: 0 1px 4px var(--brand-glow);
}

.sl-topo-edit-btn:hover svg {
  transform: scale(1.12);
}

.sl-topo-badges {
  display: flex;
  align-items: center;
  gap: 5px;
  flex-wrap: wrap;
  margin: -2px 0 2px;
}

.sl-topo-sessions-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.sl-topo-session-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 8px 12px;
  background: var(--surface-soft);
  border: 1px solid var(--line-soft);
  border-radius: var(--r-sm, 6px);
  gap: 10px;
  transition: all .15s ease;
}

.sl-topo-session-row:hover {
  background: var(--surface-hover);
  border-color: var(--line-strong);
}

.sl-topo-session-row.has-error {
  border-color: color-mix(in srgb, var(--danger) 40%, transparent);
  background: color-mix(in srgb, var(--danger-soft) 45%, var(--surface-soft));
}

.sl-topo-session-meta {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  min-width: 0;
  flex: 1;
}

.sl-topo-user-icon {
  width: 28px;
  height: 28px;
  border-radius: var(--r-xs);
  background: var(--sidebar-soft);
  color: var(--brand);
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
  margin-top: 1px;
}

.sl-topo-user-icon svg {
  width: 15px;
  height: 15px;
}

.sl-topo-user-info {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
  flex: 1;
}

.sl-topo-user-line {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.sl-topo-user-line strong {
  font-size: 12.5px;
  color: var(--text);
}

.sl-user-id-pill {
  font-size: 9.5px;
  padding: 1px 4px;
  border-radius: 3px;
  background: var(--page-bg);
  color: var(--faint);
  font-family: monospace;
  border: 1px solid var(--line-soft);
}

.sl-token-pill {
  font-size: 9.5px;
  padding: 1px 5px;
  border-radius: 3px;
  background: var(--page-bg);
  color: var(--muted);
  border: 1px solid var(--line-soft);
}

.sl-token-pill.active {
  background: var(--brand-soft);
  color: var(--brand-deep);
  border-color: color-mix(in srgb, var(--brand) 28%, transparent);
  font-weight: 600;
}

.sl-token-pill.sl-token-checked {
  background: var(--success-soft);
  color: var(--success);
  border-color: color-mix(in srgb, var(--success) 28%, transparent);
  font-weight: 600;
}

.sl-token-pill.sl-token-uncheck {
  background: var(--warning-soft);
  color: var(--warning);
  border-color: color-mix(in srgb, var(--warning) 28%, transparent);
}

.sl-token-pill.sl-token-disabled {
  background: var(--bg-card-muted, rgba(148, 163, 184, 0.12));
  color: var(--text-tertiary, #94a3b8);
  border-color: rgba(148, 163, 184, 0.25);
  font-weight: 500;
}

.sl-topo-subline {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 10.5px;
  color: var(--muted);
}

.sl-topo-err-hint {
  display: flex;
  align-items: center;
  gap: 4px;
  font-size: 10.5px;
  color: var(--danger);
  margin-top: 2px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.sl-topo-err-hint svg {
  width: 12px;
  height: 12px;
  flex-shrink: 0;
}

.sl-topo-session-right {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-shrink: 0;
}

.sl-topo-session-btns {
  display: flex;
  align-items: center;
  gap: 4px;
}

.sl-topo-session-quota {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
}

.sl-topo-session-quota strong {
  font-size: 12.5px;
  font-weight: 700;
}

.sl-topo-session-quota small {
  font-size: 9.5px;
  color: var(--muted);
}

.sl-topo-card-footer {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 8px;
  padding-top: 8px;
  border-top: 1px solid var(--line-soft);
}

.sl-topo-footer-actions {
  display: flex;
  align-items: center;
  gap: 6px;
}

.sl-btn-text-xs {
  background: transparent;
  border: 1px solid var(--line);
  border-radius: var(--r-xs);
  padding: 2px 8px;
  font-size: 11px;
  font-weight: 600;
  color: var(--muted);
  cursor: pointer;
  transition: all .15s ease;
}

.sl-btn-text-xs:hover {
  border-color: var(--brand);
  color: var(--brand-deep);
  background: var(--brand-soft);
}

.sl-btn-sync {
  font-weight: 600;
}

.sl-btn-detail {
  background: var(--brand-soft);
  border-color: color-mix(in srgb, var(--brand) 28%, transparent);
  color: var(--brand-deep);
  font-weight: 650;
}

.sl-btn-detail:hover {
  background: var(--brand);
  color: #fff;
  border-color: var(--brand);
}

.sl-topo-empty-sessions {
  padding: 18px;
  text-align: center;
  background: var(--surface-soft);
  border-radius: var(--r-sm, 6px);
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 4px;
}

.sl-topo-empty-sessions svg {
  width: 20px;
  height: 20px;
  color: var(--faint);
}

.sl-topo-empty-sessions p {
  margin: 0;
  font-size: 11px;
  color: var(--faint);
}

.sl-link-action {
  background: none;
  border: none;
  color: var(--brand, #58a6ff);
  font-size: 11px;
  cursor: pointer;
  margin-top: 4px;
}

/* 4. 批量操作浮动底栏 */
.sl-batch-dock {
  position: fixed;
  bottom: 24px;
  left: 50%;
  transform: translateX(-50%);
  z-index: 100;
  background: var(--surface, #161b22);
  border: 1px solid var(--line, #30363d);
  border-radius: var(--r-lg, 10px);
  box-shadow: 0 8px 32px rgba(0,0,0,0.45);
  padding: 8px 16px;
  animation: slide-up 0.2s cubic-bezier(0.16, 1, 0.3, 1);
}

@keyframes slide-up {
  from { transform: translate(-50%, 20px); opacity: 0; }
  to { transform: translate(-50%, 0); opacity: 1; }
}

.sl-batch-dock-content {
  display: flex;
  align-items: center;
  gap: 16px;
}

.sl-batch-dock-title {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12.5px;
  color: var(--brand, #58a6ff);
}

.sl-batch-dock-title svg {
  width: 14px;
  height: 14px;
}

.sl-batch-actions {
  display: flex;
  align-items: center;
  gap: 6px;
}

.sl-batch-btn {
  padding: 5px 10px;
  border-radius: 6px;
  background: var(--surface-soft, #21262d);
  border: 1px solid var(--line, #30363d);
  color: var(--text, #c9d1d9);
  font-size: 11.5px;
  cursor: pointer;
}

.sl-batch-btn:hover {
  background: var(--surface-hover, #30363d);
  color: var(--text, #f0f6fc);
}

.sl-batch-clear-btn {
  background: none;
  border: none;
  color: var(--muted, #8b949e);
  font-size: 11.5px;
  cursor: pointer;
}

.sl-batch-clear-btn:hover {
  color: var(--danger, #f85149);
}

/* 5. 全景详情深度抽屉 */
.sl-drawer-backdrop {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.55);
  backdrop-filter: blur(4px);
  z-index: 1000;
  display: flex;
  justify-content: flex-end;
}

.sl-drawer-panel {
  width: min(720px, 92vw);
  height: 100%;
  background: var(--page-bg, #0d1117);
  border-left: 1px solid var(--line, #30363d);
  display: flex;
  flex-direction: column;
  box-shadow: -8px 0 32px rgba(0,0,0,0.5);
  animation: drawer-in 0.22s cubic-bezier(0.16, 1, 0.3, 1);
}

@keyframes drawer-in {
  from { transform: translateX(100%); }
  to { transform: translateX(0); }
}

.sl-drawer-header {
  padding: 16px 20px;
  background: var(--page-header, #161b22);
  border-bottom: 1px solid var(--line, #30363d);
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 16px;
}

.sl-drawer-head-identity {
  display: flex;
  align-items: center;
  gap: 12px;
  min-width: 0;
}

.sl-drawer-avatar {
  width: 44px;
  height: 44px;
  border-radius: var(--r-md, 8px);
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 15px;
  font-weight: 750;
  flex-shrink: 0;
}

.sl-drawer-title-box {
  min-width: 0;
  display: flex;
  flex-direction: column;
}

.sl-drawer-tags {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 11px;
}

.sl-meta-system {
  color: var(--brand, #58a6ff);
  font-weight: 650;
}

.sl-meta-sep {
  color: var(--faint, #6e7681);
}

.sl-drawer-title {
  margin: 2px 0 0;
  font-size: 18px;
  font-weight: 750;
  color: var(--text, #f0f6fc);
}

.sl-drawer-url {
  font-size: 11px;
  color: var(--muted, #8b949e);
  margin: 2px 0 0;
}

.sl-drawer-head-actions {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-shrink: 0;
}

.sl-btn-copy, .sl-btn-edit {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  height: 30px;
  padding: 0 10px;
  border-radius: 6px;
  background: var(--surface, #21262d);
  border: 1px solid var(--line, #30363d);
  color: var(--text, #c9d1d9);
  font-size: 11.5px;
  cursor: pointer;
}

.sl-btn-copy svg, .sl-btn-edit svg {
  width: 13px;
  height: 13px;
}

.sl-btn-copy:hover, .sl-btn-edit:hover {
  background: var(--surface-hover, #30363d);
  color: var(--text, #f0f6fc);
}

.sl-drawer-close-btn {
  width: 30px;
  height: 30px;
  border-radius: 6px;
  background: var(--surface, #21262d);
  border: 1px solid var(--line, #30363d);
  color: var(--muted, #8b949e);
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
}

.sl-drawer-close-btn svg {
  width: 14px;
  height: 14px;
}

.sl-drawer-close-btn:hover {
  color: var(--text, #f0f6fc);
}

.sl-drawer-tabs {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 8px 20px;
  background: var(--page-header, #161b22);
  border-bottom: 1px solid var(--line, #30363d);
}

.sl-drawer-tabs button {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 6px 12px;
  border-radius: 6px;
  background: transparent;
  border: none;
  color: var(--muted, #8b949e);
  font-size: 12px;
  font-weight: 550;
  cursor: pointer;
  transition: all 0.15s ease;
}

.sl-drawer-tabs button svg {
  width: 14px;
  height: 14px;
}

.sl-drawer-tabs button:hover {
  color: var(--text, #f0f6fc);
}

.sl-drawer-tabs button.active {
  background: var(--surface, #21262d);
  color: var(--brand, #58a6ff);
}

.sl-drawer-body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 16px 20px 32px;
}

.sl-tab-panel {
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.sl-section-card {
  padding: 14px;
  background: var(--surface, #161b22);
  border: 1px solid var(--line, #30363d);
  border-radius: var(--r-md, 8px);
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.sl-section-head {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12.5px;
  color: var(--text, #f0f6fc);
}

.sl-section-head svg {
  width: 15px;
  height: 15px;
  color: var(--brand, #58a6ff);
}

.sl-desc-text {
  margin: 0;
  font-size: 12px;
  line-height: 1.5;
  color: var(--muted, #8b949e);
}

.sl-facts-grid {
  display: grid;
  grid-template-columns: repeat(2, 1fr);
  gap: 10px;
}

.sl-fact-box {
  padding: 10px 12px;
  background: var(--surface, #161b22);
  border: 1px solid var(--line, #30363d);
  border-radius: var(--r-md, 8px);
  display: flex;
  flex-direction: column;
}

.sl-fact-k {
  font-size: 10.5px;
  color: var(--muted, #8b949e);
}

.sl-fact-v {
  font-size: 12.5px;
  color: var(--text, #f0f6fc);
  margin-top: 2px;
}

.sl-chips-wrap {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.sl-cap-pill {
  font-size: 11px;
  padding: 4px 8px;
  border-radius: 6px;
  background: color-mix(in srgb, var(--brand, #1f6feb) 15%, transparent);
  border: 1px solid color-mix(in srgb, var(--brand, #1f6feb) 30%, transparent);
  color: var(--text, #c9d1d9);
}

.sl-cap-pill.is-disabled {
  background: var(--surface-soft, #0d1117);
  border-color: var(--line, #30363d);
  color: var(--faint, #6e7681);
  text-decoration: line-through;
  opacity: 0.6;
}

.sl-tags-cloud {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.sl-tag-badge {
  font-size: 10.5px;
  padding: 3px 8px;
  border-radius: 999px;
  background: var(--surface-soft, #21262d);
  border: 1px solid var(--line, #30363d);
  color: var(--muted, #8b949e);
}

.sl-maintainers-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(200px, 1fr));
  gap: 8px;
}

.sl-maintainer-card {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 10px;
  background: var(--surface-soft, #0d1117);
  border-radius: var(--r-sm, 6px);
  border: 1px solid var(--line-soft, #21262d);
}

.sl-maintainer-avatar {
  width: 26px;
  height: 26px;
  border-radius: 50%;
  background: var(--brand, #1f6feb);
  color: #fff;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 11px;
  font-weight: 700;
  flex-shrink: 0;
}

.sl-maintainer-info {
  display: flex;
  flex-direction: column;
  min-width: 0;
}

.sl-maintainer-info strong {
  font-size: 11.5px;
  color: var(--text, #f0f6fc);
}

.sl-maintainer-info small {
  font-size: 10px;
  color: var(--muted, #8b949e);
}

.sl-faint {
  color: var(--faint, #6e7681) !important;
}

.sl-btn-icon-xs {
  margin-left: auto;
  background: none;
  border: none;
  color: var(--muted, #8b949e);
  cursor: pointer;
  display: flex;
  align-items: center;
}

.sl-btn-icon-xs svg {
  width: 13px;
  height: 13px;
}

.sl-links-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.sl-link-item {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 8px 12px;
  background: var(--surface-soft, #0d1117);
  border: 1px solid var(--line-soft, #21262d);
  border-radius: var(--r-sm, 6px);
}

.sl-link-item strong {
  font-size: 12px;
  color: var(--text, #f0f6fc);
  display: block;
}

.sl-link-item small {
  font-size: 10.5px;
  color: var(--muted, #8b949e);
  font-family: monospace;
}

.sl-link-item-actions {
  display: flex;
  align-items: center;
  gap: 6px;
}

.sl-empty-hint {
  font-size: 11.5px;
  color: var(--faint, #6e7681);
  margin: 0;
}

/* 抽屉 TAB 2/3 */
.sl-tab-action-bar {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
}

.sl-tab-section-title {
  font-size: 13px;
  font-weight: 650;
  color: var(--text, #f0f6fc);
}

.sl-drawer-accounts-grid {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.sl-drawer-acc-card {
  padding: 12px 14px;
  background: var(--surface, #161b22);
  border: 1px solid var(--line, #30363d);
  border-radius: var(--r-md, 8px);
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.sl-drawer-acc-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.sl-drawer-acc-user {
  display: flex;
  align-items: center;
  gap: 10px;
}

.sl-drawer-acc-avatar {
  width: 32px;
  height: 32px;
  border-radius: var(--r-md, 8px);
  background: var(--surface-soft, #21262d);
  color: var(--brand, #58a6ff);
  display: flex;
  align-items: center;
  justify-content: center;
}

.sl-drawer-acc-avatar svg {
  width: 16px;
  height: 16px;
}

.sl-acc-chips-row {
  display: flex;
  align-items: center;
  gap: 4px;
  margin-top: 2px;
}

.sl-drawer-acc-quota-block {
  display: flex;
  align-items: center;
  gap: 12px;
}

.sl-drawer-acc-btn-group {
  display: flex;
  align-items: center;
  gap: 6px;
}

.sl-drawer-acc-quota {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
}

.sl-acc-quota-k {
  font-size: 9.5px;
  color: var(--muted, #8b949e);
}

.sl-acc-quota-v {
  font-size: 14px;
  font-weight: 700;
}

.sl-drawer-acc-facts {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 8px;
  padding: 8px 10px;
  background: var(--surface-soft, #0d1117);
  border-radius: var(--r-sm, 6px);
}

.sl-acc-fact {
  display: flex;
  flex-direction: column;
}

.sl-acc-fact span {
  font-size: 9.5px;
  color: var(--muted, #8b949e);
}

.sl-acc-fact strong {
  font-size: 11.5px;
  color: var(--text, #f0f6fc);
}

.sl-drawer-acc-error {
  padding: 6px 10px;
  border-radius: var(--r-sm, 6px);
  background: color-mix(in srgb, var(--danger, #f85149) 15%, transparent);
  color: var(--danger, #f85149);
  font-size: 11px;
  display: flex;
  align-items: center;
  gap: 6px;
}

.sl-drawer-acc-error svg {
  width: 14px;
  height: 14px;
  flex-shrink: 0;
}

.sl-empty-state-compact {
  padding: 32px 20px;
  text-align: center;
  background: var(--surface, #161b22);
  border: 1px dashed var(--line, #30363d);
  border-radius: var(--r-md, 8px);
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
}

.sl-empty-state-compact svg {
  width: 28px;
  height: 28px;
  color: var(--faint, #6e7681);
}

.sl-empty-state-compact p {
  margin: 0;
  font-size: 12px;
  color: var(--muted, #8b949e);
}

/* 抽屉模型与 Key 选项卡 */
.sl-models-search-box {
  position: relative;
  display: flex;
  align-items: center;
  background: var(--surface, #21262d);
  border: 1px solid var(--line, #30363d);
  border-radius: var(--r-md, 8px);
  padding: 0 10px;
  height: 32px;
  flex: 1;
}

.sl-models-search-box svg {
  width: 13px;
  height: 13px;
  color: var(--muted, #8b949e);
}

.sl-models-search-box input {
  width: 100%;
  border: none;
  background: transparent;
  color: var(--text, #f0f6fc);
  font-size: 11.5px;
  padding: 0 6px;
  outline: none;
}

.sl-models-sync-btns {
  display: flex;
  align-items: center;
  gap: 6px;
}

.sl-keys-bar {
  display: flex;
  align-items: center;
  gap: 8px;
  overflow: hidden;
}

.sl-keys-label {
  font-size: 11px;
  color: var(--muted, #8b949e);
  flex-shrink: 0;
}

.sl-keys-chips {
  display: flex;
  align-items: center;
  gap: 6px;
  overflow-x: auto;
}

.sl-key-chip {
  padding: 3px 8px;
  border-radius: 6px;
  background: var(--surface, #21262d);
  border: 1px solid var(--line, #30363d);
  color: var(--muted, #8b949e);
  font-size: 10.5px;
  cursor: pointer;
  white-space: nowrap;
}

.sl-key-chip.active {
  background: color-mix(in srgb, var(--brand, #1f6feb) 20%, var(--surface, #21262d));
  color: var(--brand, #58a6ff);
  border-color: color-mix(in srgb, var(--brand, #1f6feb) 50%, transparent);
}

.sl-drawer-models-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(200px, 1fr));
  gap: 8px;
}

.sl-drawer-model-item {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 8px 10px;
  background: var(--surface, #161b22);
  border: 1px solid var(--line, #30363d);
  border-radius: var(--r-sm, 6px);
  cursor: pointer;
  transition: all 0.15s ease;
}

.sl-drawer-model-item:hover {
  background: var(--surface-hover, #282e36);
  border-color: color-mix(in srgb, var(--brand, #58a6ff) 35%, var(--line, #30363d));
}

.sl-model-item-info {
  display: flex;
  flex-direction: column;
  min-width: 0;
}

.sl-model-item-info strong {
  font-size: 11px;
  color: var(--text, #f0f6fc);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.sl-model-copy-btn {
  background: none;
  border: none;
  color: var(--muted, #8b949e);
  cursor: pointer;
  padding: 2px;
  display: flex;
  align-items: center;
}

.sl-model-copy-btn svg {
  width: 12px;
  height: 12px;
}

/* 空状态与加载中 */
.sl-loading-state, .sl-empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 60px 20px;
  text-align: center;
  color: var(--muted, #8b949e);
}

.sl-loading-state svg, .sl-empty-state svg {
  width: 36px;
  height: 36px;
  margin-bottom: 12px;
  color: var(--faint, #6e7681);
}

.sl-empty-state h3 {
  margin: 0 0 6px;
  color: var(--text, #f0f6fc);
  font-size: 16px;
}

.sl-empty-state p {
  margin: 0 0 16px;
  font-size: 12px;
}

.is-spinning {
  animation: spin 1s linear infinite;
}

@keyframes spin {
  from { transform: rotate(0deg); }
  to { transform: rotate(360deg); }
}

@media (max-width: 1024px) {
  .sl-metrics-grid {
    grid-template-columns: repeat(2, 1fr);
  }
  .sl-filters-row {
    grid-template-columns: 1fr 1fr;
  }
}
</style>
