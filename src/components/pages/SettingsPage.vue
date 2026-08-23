<script setup lang="ts">
import { ref, watch, nextTick } from "vue";
import { icons } from "../../icons";
import { useStore } from "../../composables/useStore";
import { usePreferences } from "../../composables/usePreferences";
import { useTheme } from "../../composables/useTheme";
import { runCommand } from "../../composables/useLibrary";
import { getSessionToken, setSessionToken } from "../../composables/core/ipc";
import type { ThemePreference, ProxyNodeViewModePreference } from "../../types";

const store = useStore();
const { preferences, updatePreferences } = usePreferences();
const { setThemePreference } = useTheme();

const closeBtnRef = ref<HTMLButtonElement>();

watch(
  () => store.page.value,
  (page) => {
    if (page === "settings") {
      nextTick(() => closeBtnRef.value?.focus());
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

const loggingOut = ref(false);

/** 退出登录：注销后端会话并清除本地令牌，回到登录页。 */
async function logout() {
  if (loggingOut.value) return;
  loggingOut.value = true;
  try {
    const token = getSessionToken();
    if (token) {
      await runCommand("logout", { token }).catch(() => undefined);
    }
  } finally {
    setSessionToken("");
    window.dispatchEvent(new Event("openhub-auth-expired"));
    loggingOut.value = false;
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

            <!-- 账号与登录 -->
            <section class="settings-section">
              <div class="settings-section-title">
                <span v-html="icons.user" />
                <div>
                  <h2>账号与登录</h2>
                  <p>退出当前登录会话，下次访问需要重新输入用户名和密码</p>
                </div>
              </div>
              <div class="settings-rows">
                <div class="settings-row">
                  <div>
                    <strong>退出登录</strong>
                    <small>清除本机保存的登录令牌，不会影响服务运行与其他已登录设备</small>
                  </div>
                  <button
                    type="button"
                    class="secondary-button settings-logout-button"
                    :disabled="loggingOut"
                    @click="logout"
                  >
                    <span v-html="icons.close" /><span>{{ loggingOut ? "正在退出…" : "退出登录" }}</span>
                  </button>
                </div>
              </div>
            </section>

            <!-- 代理池与内核 -->
            <section class="settings-section">
              <div class="settings-section-title">
                <span v-html="icons.wifi" />
                <div>
                  <h2>代理池与内核</h2>
                  <p>OpenHub 内置独立代理内核与节点设置</p>
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

