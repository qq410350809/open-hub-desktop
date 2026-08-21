import { computed, ref, type ComputedRef } from "vue";
import { useLibrary } from "./useLibrary";
import { usePreferences } from "../core/usePreferences";
import { KNOWN_SYSTEM_TYPES, normalizeSystemType, type SiteRecord } from "../../types";

// ============================================================
//  过滤状态 —— 模块级单例，所有组件共享
// ============================================================
const { sites, suggestedTags } = useLibrary();
const { preferences, updatePreferences } = usePreferences();

const runawayFilter = ref(preferences.defaultRunawayFilter);
const usageFilter = ref(preferences.defaultUsageFilter);
const query = ref("");
const tag = ref("all");
const level = ref("all");
const feature = ref("all");
const systemTypeFilter = ref("all");

const allTags: ComputedRef<string[]> = computed(() => [
  ...new Set([...suggestedTags.value, ...sites.value.flatMap((site) => site.tags)]),
]);

const filteredSites: ComputedRef<SiteRecord[]> = computed(() => {
  const q = query.value.trim().toLocaleLowerCase("zh-CN");
  return sites.value
    .filter((site) => {
      if (runawayFilter.value === "active" && site.isRunaway) return false;
      if (runawayFilter.value === "runaway" && !site.isRunaway) return false;
      if (usageFilter.value === "personal" && !site.isPersonal) return false;
      if (usageFilter.value === "pending" && !site.isPending) return false;
      if (usageFilter.value === "unused" && (site.isPersonal || site.isPending)) return false;
      if (tag.value !== "all" && !site.tags.includes(tag.value)) return false;
      if (level.value !== "all" && site.registrationLimit !== Number(level.value)) return false;
      if (!matchesFeature(site, feature.value)) return false;
      const siteSystemType = normalizeSystemType(site.systemType);
      if (systemTypeFilter.value === "unknown" && KNOWN_SYSTEM_TYPES.has(siteSystemType)) return false;
      if (
        !["all", "unknown"].includes(systemTypeFilter.value) &&
        siteSystemType !== normalizeSystemType(systemTypeFilter.value)
      ) return false;
      const content = [
        site.name,
        site.apiBaseUrl,
        site.description,
        site.rateLimit,
        ...site.tags,
        ...site.maintainers.map((item) => item.name),
      ]
        .join(" ")
        .toLocaleLowerCase("zh-CN");
      return !q || content.includes(q);
    });
});

const activeCount: ComputedRef<number> = computed(() => sites.value.filter((site) => !site.isRunaway).length);
const runawayCount: ComputedRef<number> = computed(() => sites.value.filter((site) => site.isRunaway).length);
const personalCount: ComputedRef<number> = computed(() => sites.value.filter((site) => site.isPersonal).length);
const pendingCount: ComputedRef<number> = computed(() => sites.value.filter((site) => site.isPending).length);

const hasFilters: ComputedRef<boolean> = computed(
  () =>
    Boolean(query.value) ||
    tag.value !== "all" ||
    level.value !== "all" ||
    feature.value !== "all" ||
    systemTypeFilter.value !== "all",
);

function clearFilters() {
  query.value = "";
  tag.value = "all";
  level.value = "all";
  feature.value = "all";
  systemTypeFilter.value = "all";
}

function setRunawayFilter(filter: string) {
  runawayFilter.value = filter;
  updatePreferences({ defaultRunawayFilter: filter });
}

function setUsageFilter(filter: string) {
  usageFilter.value = filter;
  updatePreferences({ defaultUsageFilter: filter });
}

function matchesFeature(site: SiteRecord, feature: string): boolean {
  switch (feature) {
    case "checkin": return site.supportsCheckin;
    case "translation": return site.supportsImmersiveTranslation;
    case "ldc": return site.supportsLdc;
    case "nsfw": return site.supportsNsfw;
    case "invite": return site.requiresInviteCode;
    default: return true;
  }
}

export function useFilterState() {
  return {
    runawayFilter,
    usageFilter,
    query,
    tag,
    level,
    feature,
    systemTypeFilter,
    allTags,
    filteredSites,
    activeCount,
    runawayCount,
    personalCount,
    pendingCount,
    hasFilters,
    clearFilters,
    setRunawayFilter,
    setUsageFilter,
    matchesFeature,
  };
}
