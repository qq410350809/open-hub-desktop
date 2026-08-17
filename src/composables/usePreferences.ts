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
  gatewayEnabled: true,
  gatewayPort: 17896,
  gatewayApiKey: "",
  gatewayApiKeys: [
    {
      id: "default",
      name: "默认客户端",
      key: "sk-oh-default",
      enabled: true,
      createdAt: Date.now(),
    },
  ],
};

/** 过滤出非空字符串并保持顺序（用于站点顺序、隐藏模型键等列表型偏好）。 */
function normalizeStringList(raw: unknown): string[] {
  if (!Array.isArray(raw)) return [];
  return raw.filter((item): item is string => typeof item === "string" && item.trim().length > 0);
}

function normalizeGatewayApiKeys(raw: unknown, legacyKey?: string): Preferences["gatewayApiKeys"] {
  if (Array.isArray(raw) && raw.length > 0) {
    const list = raw
      .filter((item): item is Record<string, unknown> => typeof item === "object" && item !== null)
      .map((item, idx) => ({
        id: typeof item.id === "string" && item.id.trim() ? item.id.trim() : `key-${idx + 1}`,
        name: typeof item.name === "string" ? item.name.trim() : `Key ${idx + 1}`,
        key: typeof item.key === "string" ? item.key.trim() : "",
        enabled: item.enabled !== false,
        createdAt: typeof item.createdAt === "number" ? item.createdAt : Date.now(),
      }))
      .filter((item) => item.key.length > 0);
    if (list.length > 0) return list;
  }
  if (typeof legacyKey === "string" && legacyKey.trim()) {
    return [
      {
        id: "default",
        name: "默认客户端",
        key: legacyKey.trim(),
        enabled: true,
        createdAt: Date.now(),
      },
    ];
  }
  return [...defaultPreferences.gatewayApiKeys];
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
    const gatewayPort = typeof saved.gatewayPort === "number" && saved.gatewayPort > 0 && saved.gatewayPort <= 65535
      ? (saved.gatewayPort === 52020 ? 17896 : saved.gatewayPort)
      : 17896;
    const legacyApiKey = typeof saved.gatewayApiKey === "string" ? saved.gatewayApiKey.trim() : "";
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
      gatewayEnabled: saved.gatewayEnabled !== undefined ? Boolean(saved.gatewayEnabled) : true,
      gatewayPort,
      gatewayApiKey: legacyApiKey,
      gatewayApiKeys: normalizeGatewayApiKeys(saved.gatewayApiKeys, legacyApiKey),
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
