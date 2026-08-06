<script setup lang="ts">
import { ref, watch, nextTick, computed } from "vue";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import { usePreferences } from "../composables/usePreferences";
import { useTheme } from "../composables/useTheme";
import type { ThemePreference } from "../types";
import { invoke } from "@tauri-apps/api/core";
import CustomSelect from "./CustomSelect.vue";

const store = useStore();
const { preferences, updatePreferences } = usePreferences();
const { setThemePreference } = useTheme();

const closeBtnRef = ref<HTMLButtonElement>();
const systemFonts = ref<string[]>([]);
const fontOptions = computed(() => {
  const options = [
    { value: "system", text: "系统默认" },
    { value: "serif", text: "衬线体" },
    { value: "mono", text: "等宽体" }
  ];
  for (const font of systemFonts.value) {
    if (font !== "system" && font !== "serif" && font !== "mono") {
      options.push({ value: font, text: font });
    }
  }
  if (!options.some(o => o.value === preferences.fontFamily)) {
    options.push({ value: preferences.fontFamily, text: preferences.fontFamily });
  }
  return options;
});

const loadSystemFonts = async () => {
  if (systemFonts.value.length > 0) return;
  try {
    systemFonts.value = await invoke<string[]>("get_system_fonts");
  } catch (e) {
    console.error("Failed to load system fonts", e);
  }
};

watch(
  () => store.page.value,
  (page) => {
    if (page === "settings") {
      nextTick(() => closeBtnRef.value?.focus());
      loadSystemFonts();
    }
  },
);

function close() {
  store.closeSettings();
}

function onBackdropClick(event: MouseEvent) {
  if (event.target === event.currentTarget) close();
}

</script>

<template>
  <Teleport to="body">
    <div
      class="settings-page"
      id="settings-page"
      :hidden="store.page.value !== 'settings'"
      @click="onBackdropClick"
    >
      <div
        class="settings-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="settings-title"
      >
        <header class="settings-header">
          <div>
            <h1 id="settings-title">设置</h1>
            <p>应用偏好</p>
          </div>
          <button
            ref="closeBtnRef"
            class="close-button"
            id="close-settings"
            type="button"
            aria-label="关闭设置"
            @click="close"
            v-html="icons.close"
          />
        </header>
        <div class="settings-scroll">
          <div class="settings-content">
            <!-- 外观 -->
            <section class="settings-section">
              <div class="settings-section-title">
                <span v-html="icons.monitor" />
                <div>
                  <h2>外观</h2>
                  <p>界面主题与显示方式</p>
                </div>
              </div>
              <div class="settings-rows">
                <div class="settings-row">
                  <div>
                    <strong>主题模式</strong>
                    <small>跟随系统会随 macOS 外观自动切换</small>
                  </div>
                  <div class="preference-segment" id="theme-preference" role="group" aria-label="主题模式">
                    <button
                      type="button"
                      :class="{ active: preferences.theme === 'system' }"
                      data-theme-choice="system"
                      @click="setThemePreference('system' as ThemePreference)"
                    >跟随系统</button>
                    <button
                      type="button"
                      :class="{ active: preferences.theme === 'light' }"
                      data-theme-choice="light"
                      @click="setThemePreference('light' as ThemePreference)"
                    >明亮</button>
                    <button
                      type="button"
                      :class="{ active: preferences.theme === 'dark' }"
                      data-theme-choice="dark"
                      @click="setThemePreference('dark' as ThemePreference)"
                    >暗黑</button>
                  </div>
                </div>
                <div class="settings-row">
                  <div>
                    <strong>显示字体</strong>
                    <small>应用程序全局字体</small>
                  </div>
                  <div class="preference-segment" id="font-preference" role="group" aria-label="显示字体" style="width: 220px;">
                    <CustomSelect
                      :options="fontOptions"
                      :modelValue="preferences.fontFamily"
                      @update:modelValue="val => updatePreferences({ fontFamily: val })"
                    />
                  </div>
                </div>
                <div class="settings-row">
                  <div>
                    <strong>字体大小</strong>
                    <small>全局基础字体缩放</small>
                  </div>
                  <div class="preference-segment" id="fontsize-preference" role="group" aria-label="字体大小">
                    <button
                      type="button"
                      :class="{ active: preferences.fontSize === 'small' }"
                      data-size-choice="small"
                      @click="updatePreferences({ fontSize: 'small' })"
                    >较小</button>
                    <button
                      type="button"
                      :class="{ active: preferences.fontSize === 'medium' }"
                      data-size-choice="medium"
                      @click="updatePreferences({ fontSize: 'medium' })"
                    >标准</button>
                    <button
                      type="button"
                      :class="{ active: preferences.fontSize === 'large' }"
                      data-size-choice="large"
                      @click="updatePreferences({ fontSize: 'large' })"
                    >较大</button>
                  </div>
                </div>
              </div>
            </section>

            <!-- 浏览偏好 -->
            <section class="settings-section">
              <div class="settings-section-title">
                <span v-html="icons.settings" />
                <div>
                  <h2>浏览偏好</h2>
                  <p>启动状态与可见范围</p>
                </div>
              </div>
              <div class="settings-rows">
                <div class="settings-row">
                  <div>
                    <strong>默认存活状态</strong>
                    <small>设置启动时显示的站点状态</small>
                  </div>
                  <div class="preference-segment" id="runaway-preference" role="group" aria-label="默认存活状态">
                    <button
                      type="button"
                      :class="{ active: preferences.defaultRunawayFilter === 'active' }"
                      data-runaway-choice="active"
                      @click="updatePreferences({ defaultRunawayFilter: 'active' })"
                    >
                      <span v-html="icons.wifi" /><span>存活</span>
                    </button>
                    <button
                      type="button"
                      :class="{ active: preferences.defaultRunawayFilter === 'runaway' }"
                      data-runaway-choice="runaway"
                      @click="updatePreferences({ defaultRunawayFilter: 'runaway' })"
                    >
                      <span v-html="icons.wifiOff" /><span>跑路</span>
                    </button>
                  </div>
                </div>
                <div class="settings-row">
                  <div>
                    <strong>默认使用范围</strong>
                    <small>设置启动时显示的站点范围</small>
                  </div>
                  <div class="preference-segment" id="usage-preference" role="group" aria-label="默认使用范围">
                    <button
                      type="button"
                      :class="{ active: preferences.defaultUsageFilter === 'all' }"
                      data-usage-choice="all"
                      @click="updatePreferences({ defaultUsageFilter: 'all' })"
                    >
                      <span v-html="icons.globe" /><span>全部</span>
                    </button>
                    <button
                      type="button"
                      :class="{ active: preferences.defaultUsageFilter === 'personal' }"
                      data-usage-choice="personal"
                      @click="updatePreferences({ defaultUsageFilter: 'personal' })"
                    >
                      <span v-html="icons.bookmark" /><span>在用</span>
                    </button>
                  </div>
                </div>
              </div>
            </section>

            <!-- 关于 -->
            <section class="settings-section settings-about">
              <div class="settings-section-title">
                <span v-html="icons.info" />
                <div>
                  <h2>关于</h2>
                  <p>OpenHub</p>
                </div>
              </div>
              <div class="about-line">
                <span>版本</span>
                <strong>0.3.0</strong>
              </div>
            </section>
          </div>
        </div>
      </div>
    </div>
  </Teleport>
</template>
