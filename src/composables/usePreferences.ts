import { reactive, watch } from "vue";
import type {
  ModelAggGroupMode,
  Preferences,
  ThemePreference,
  ProxyNodeViewModePreference,
} from "../types";

const PREFERENCES_KEY = "ldoh:preferences";

const defaultPreferences: Preferences = {
  theme: "system",
  defaultRunawayFilter: "active",
  defaultUsageFilter: "all",
  proxyNodeViewMode: "list",
  sidebarCollapsed: false,
  modelAggSiteOrder: [],
  modelAggGroupModes: {},
  modelAggHiddenModels: [],
};

/** 过滤出非空字符串并保持顺序（用于站点顺序、隐藏模型键等列表型偏好）。 */
function normalizeStringList(raw: unknown): string[] {
  if (!Array.isArray(raw)) return [];
  return raw.filter((item): item is string => typeof item === "string" && item.trim().length > 0);
}

function normalizeModelAggGroupModes(raw: unknown): Record<string, ModelAggGroupMode> {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return {};
  const modes: Record<string, ModelAggGroupMode> = {};
  for (const [group, mode] of Object.entries(raw as Record<string, unknown>)) {
    if (group.trim().length > 0 && (mode === "aggregate" || mode === "independent")) {
      modes[group] = mode;
    }
  }
  return modes;
}

function loadPreferences(): Preferences {
  try {
    const saved = JSON.parse(
      localStorage.getItem(PREFERENCES_KEY) ?? "{}",
    ) as Partial<Preferences>;
    const legacyTheme = localStorage.getItem("ldoh:theme");
    const proxyNodeViewMode = saved.proxyNodeViewMode === "country" ? "country" : "list";
    return {
      theme: ["system", "light", "dark"].includes(String(saved.theme))
        ? (saved.theme as ThemePreference)
        : legacyTheme === "dark"
          ? "dark"
          : legacyTheme === "light"
            ? "light"
            : defaultPreferences.theme,
      defaultRunawayFilter: saved.defaultRunawayFilter ?? "active",
      defaultUsageFilter: saved.defaultUsageFilter ?? "all",
      proxyNodeViewMode: proxyNodeViewMode as ProxyNodeViewModePreference,
      sidebarCollapsed: Boolean(saved.sidebarCollapsed),
      modelAggSiteOrder: normalizeStringList(saved.modelAggSiteOrder),
      modelAggGroupModes: normalizeModelAggGroupModes(saved.modelAggGroupModes),
      modelAggHiddenModels: normalizeStringList(saved.modelAggHiddenModels),
    };
  } catch {
    return { ...defaultPreferences };
  }
}

// —— 全局单例 ——
const preferences = reactive<Preferences>(loadPreferences());

function savePreferences() {
  localStorage.setItem(PREFERENCES_KEY, JSON.stringify(preferences));
}

// 自动持久化（只注册一次）
let watchInstalled = false;
function ensureWatch() {
  if (watchInstalled) return;
  watchInstalled = true;
  watch(preferences, savePreferences, { deep: true });
}
ensureWatch();

export function usePreferences() {
  function updatePreferences(update: Partial<Preferences>) {
    Object.assign(preferences, update);
  }

  return { preferences, updatePreferences };
}
