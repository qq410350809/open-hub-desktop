import { reactive, watch } from "vue";
import type {
  Preferences,
  ThemePreference,
  ProxyNodeViewModePreference,
  ProxySortMode,
} from "../../types";

const PREFERENCES_KEY = "ldoh:preferences";

const defaultPreferences: Preferences = {
  theme: "system",
  defaultRunawayFilter: "active",
  defaultUsageFilter: "all",
  proxyNodeViewMode: "list",
  proxyNodeSortMode: "latency",
  sidebarCollapsed: false,
};

function loadPreferences(): Preferences {
  try {
    const saved = JSON.parse(
      localStorage.getItem(PREFERENCES_KEY) ?? "{}",
    ) as Partial<Preferences>;
    const legacyTheme = localStorage.getItem("ldoh:theme");
    const proxyNodeViewMode = saved.proxyNodeViewMode === "country" ? "country" : "list";
    const proxyNodeSortMode = (["latency", "speed", "name"] as const).includes(
      saved.proxyNodeSortMode as ProxySortMode,
    )
      ? (saved.proxyNodeSortMode as ProxySortMode)
      : defaultPreferences.proxyNodeSortMode;
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
      proxyNodeSortMode,
      sidebarCollapsed: Boolean(saved.sidebarCollapsed),
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
