import { watch } from "vue";
import { usePreferences } from "./usePreferences";
import type { ThemePreference } from "../types";

const themeMedia = window.matchMedia("(prefers-color-scheme: dark)");

function resolveTheme() {
  const { preferences } = usePreferences();
  return preferences.theme === "system"
    ? themeMedia.matches
      ? "dark"
      : "light"
    : preferences.theme;
}

function applyTheme() {
  document.documentElement.dataset.theme = resolveTheme();
}

export function useTheme() {
  const { preferences, updatePreferences } = usePreferences();

  function setThemePreference(theme: ThemePreference): void {
    updatePreferences({ theme });
    localStorage.removeItem("ldoh:theme");
    applyTheme();
  }

  return { preferences, setThemePreference, applyTheme };
}

// —— 全局监听（只注册一次）——
(function initThemeWatch() {
  const { preferences } = usePreferences();
  watch(() => preferences.theme, applyTheme);
  themeMedia.addEventListener("change", () => {
    if (preferences.theme === "system") applyTheme();
  });
})();
