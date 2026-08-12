<script setup lang="ts">
import { ref, watch, nextTick } from "vue";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import { usePreferences } from "../composables/usePreferences";
import { useTheme } from "../composables/useTheme";
import { runCommand } from "../composables/useLibrary";
import type { LightweightState, ThemePreference, ProxyNodeViewModePreference } from "../types";

const store = useStore();
const { preferences, updatePreferences } = usePreferences();
const { setThemePreference } = useTheme();

const closeBtnRef = ref<HTMLButtonElement>();

watch(
  () => store.page.value,
  (page) => {
    if (page === "settings") {
      nextTick(() => closeBtnRef.value?.focus());
      void loadLightweightState();
    }
  },
);

function close() {
  store.closeSettings();
}

function onBackdropClick(event: MouseEvent) {
  if (event.target === event.currentTarget) close();
}

function setProxyNodeViewMode(mode: ProxyNodeViewModePreference) {
  updatePreferences({ proxyNodeViewMode: mode });
}

const lightweight = ref<LightweightState | null>(null);
const lightweightLoading = ref(false);

async function loadLightweightState() {
  lightweightLoading.value = true;
  try {
    lightweight.value = await runCommand<LightweightState>("get_lightweight_mode_state");
  } catch {
    lightweight.value = null;
  } finally {
    lightweightLoading.value = false;
  }
}

async function enterLightweightMode() {
  if (!lightweight.value?.running) return;
  try {
    const state = await runCommand<LightweightState>("enter_lightweight_mode");
    lightweight.value = { ...state, enabled: true };
  } catch (error) {
    store.showToast(String(error), true);
  }
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
            <p>按模块分组的应用偏好</p>
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
                  <p>界面主题</p>
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
              </div>
            </section>

            <!-- 站点库 -->
            <section class="settings-section">
              <div class="settings-section-title">
                <span v-html="icons.database" />
                <div>
                  <h2>站点库</h2>
                  <p>启动时的默认筛选范围</p>
                </div>
              </div>
              <div class="settings-rows">
                <div class="settings-row">
                  <div>
                    <strong>默认存活状态</strong>
                    <small>打开站点库时优先显示的状态</small>
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
                    <small>打开站点库时优先显示的范围</small>
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
                    <button
                      type="button"
                      :class="{ active: preferences.defaultUsageFilter === 'pending' }"
                      data-usage-choice="pending"
                      @click="updatePreferences({ defaultUsageFilter: 'pending' })"
                    >
                      <span v-html="icons.sessionImport" /><span>待定</span>
                    </button>
                  </div>
                </div>
              </div>
            </section>

            <!-- 轻量模式 -->
            <section class="settings-section">
              <div class="settings-section-title">
                <span v-html="icons.globe" />
                <div>
                  <h2>轻量模式</h2>
                  <p>关闭 GUI 窗口，只保留内核在后台运行，通过浏览器访问</p>
                </div>
              </div>
              <div class="settings-rows">
                <div class="settings-row">
                  <div>
                    <strong>一键轻量模式</strong>
                    <small v-if="lightweightLoading">正在读取服务状态…</small>
                    <small v-else-if="lightweight?.running">
                      访问地址：<code class="lightweight-address">{{ lightweight.url }}</code>
                    </small>
                    <small v-else>轻量模式服务未运行，请重启应用后重试</small>
                    <small v-if="lightweight?.enabled">已开启：下次启动将自动进入轻量模式，点 Dock 图标可唤出窗口</small>
                  </div>
                  <button
                    type="button"
                    class="secondary-button lightweight-enter-button"
                    :disabled="!lightweight?.running || lightweightLoading"
                    @click="enterLightweightMode"
                  >
                    <span v-html="icons.globe" /><span>启用并进入轻量模式</span>
                  </button>
                </div>
              </div>
            </section>

            <!-- 代理池 -->
            <section class="settings-section">
              <div class="settings-section-title">
                <span v-html="icons.wifi" />
                <div>
                  <h2>代理池</h2>
                  <p>节点列表的默认展示方式</p>
                </div>
              </div>
              <div class="settings-rows">
                <div class="settings-row">
                  <div>
                    <strong>节点列表显示</strong>
                    <small>进入代理池时默认使用普通列表或国家分组</small>
                  </div>
                  <div class="preference-segment" id="proxy-node-view-preference" role="group" aria-label="节点列表显示">
                    <button
                      type="button"
                      :class="{ active: preferences.proxyNodeViewMode === 'list' }"
                      data-proxy-view-choice="list"
                      @click="setProxyNodeViewMode('list')"
                    >
                      <span v-html="icons.rows" /><span>普通列表</span>
                    </button>
                    <button
                      type="button"
                      :class="{ active: preferences.proxyNodeViewMode === 'country' }"
                      data-proxy-view-choice="country"
                      @click="setProxyNodeViewMode('country')"
                    >
                      <span v-html="icons.globe" /><span>国家分组</span>
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
