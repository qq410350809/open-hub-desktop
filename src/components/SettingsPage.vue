<script setup lang="ts">
import { ref, watch, nextTick } from "vue";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import { usePreferences } from "../composables/usePreferences";
import { useTheme } from "../composables/useTheme";
import { useProxyPool } from "../composables/useProxyPool";
import { runCommand } from "../composables/useLibrary";
import type { LightweightState, ThemePreference, ProxyNodeViewModePreference } from "../types";

const store = useStore();
const { preferences, updatePreferences } = usePreferences();
const { setThemePreference } = useTheme();
const {
  kernelStatus,
  kernelLoading,
  kernelChecking,
  kernelDownloading,
  kernelDownloadProgress,
  loadMihomoKernelStatus,
  checkMihomoKernelUpdate,
  downloadOrUpdateMihomoKernel,
} = useProxyPool();

const AUTO_SYNC_INTERVAL_CHOICES = [15, 30, 60] as const;

function setAutoSyncEnabled(enabled: boolean) {
  void store.updateAutoSyncSettings({ enabled });
}

function setAutoSyncInterval(intervalMinutes: number) {
  void store.updateAutoSyncSettings({ intervalMinutes });
}

function formatAutoSyncTime(at: number) {
  if (!at) return "尚未运行";
  const date = new Date(at * 1000);
  const sameDay = new Date().toDateString() === date.toDateString();
  const time = `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
  return sameDay ? `今天 ${time}` : `${date.getMonth() + 1}/${date.getDate()} ${time}`;
}

const closeBtnRef = ref<HTMLButtonElement>();

watch(
  () => store.page.value,
  (page) => {
    if (page === "settings") {
      nextTick(() => closeBtnRef.value?.focus());
      void loadLightweightState();
      void store.loadAutoSyncState();
      void loadMihomoKernelStatus();
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

            <!-- 自动会话同步 -->
            <section class="settings-section">
              <div class="settings-section-title">
                <span v-html="icons.restore" />
                <div>
                  <h2>自动会话同步</h2>
                  <p>后台定期刷新在用站点的账号额度与签到；刷新令牌模式会同步访问令牌，会话失效时自动通过 Chrome 恢复</p>
                </div>
              </div>
              <div class="settings-rows">
                <div class="settings-row">
                  <div>
                    <strong>自动同步</strong>
                    <small>浏览器恢复只使用静默与后台标签，绝不弹出前台窗口；需要人工过盾时会提示</small>
                  </div>
                  <div class="preference-segment" id="auto-sync-enabled" role="group" aria-label="自动会话同步">
                    <button
                      type="button"
                      :class="{ active: store.autoSyncSettings.value?.enabled }"
                      data-auto-sync-choice="on"
                      @click="setAutoSyncEnabled(true)"
                    >
                      <span v-html="icons.check" /><span>开启</span>
                    </button>
                    <button
                      type="button"
                      :class="{ active: store.autoSyncSettings.value && !store.autoSyncSettings.value.enabled }"
                      data-auto-sync-choice="off"
                      @click="setAutoSyncEnabled(false)"
                    >
                      <span v-html="icons.close" /><span>关闭</span>
                    </button>
                  </div>
                </div>
                <div class="settings-row">
                  <div>
                    <strong>同步间隔</strong>
                    <small>每轮执行直连保活 → 失效账号恢复 → Key/模型刷新（过期缓存每天补刷一次）</small>
                  </div>
                  <div class="preference-segment" id="auto-sync-interval" role="group" aria-label="自动同步间隔">
                    <button
                      v-for="choice in AUTO_SYNC_INTERVAL_CHOICES"
                      :key="choice"
                      type="button"
                      :class="{ active: (store.autoSyncSettings.value?.intervalMinutes ?? 30) === choice }"
                      :data-auto-sync-interval="choice"
                      @click="setAutoSyncInterval(choice)"
                    >
                      <span v-html="icons.clock" /><span>{{ choice }} 分钟</span>
                    </button>
                  </div>
                </div>
                <div class="settings-row">
                  <div>
                    <strong>最近一轮</strong>
                    <small v-if="store.autoSyncStatus.value?.lastSummary">
                      {{ formatAutoSyncTime(store.autoSyncStatus.value.lastRoundAt) }} ·
                      保活 {{ store.autoSyncStatus.value.lastSummary.refreshedAccounts }} 个账号，
                      恢复 {{ store.autoSyncStatus.value.lastSummary.recovered.length }} 个，
                      待人工 {{ store.autoSyncStatus.value.lastSummary.pendingManual.length }} 个，
                      Key/模型 {{ store.autoSyncStatus.value.lastSummary.modelsRefreshed }} 成功
                    </small>
                    <small v-else>{{ formatAutoSyncTime(store.autoSyncStatus.value?.lastRoundAt ?? 0) }}</small>
                  </div>
                  <button
                    type="button"
                    class="secondary-button"
                    :disabled="store.autoSyncRoundRunning.value || !store.autoSyncSettings.value?.enabled"
                    @click="store.requestAutoSyncRound()"
                  >
                    <span v-html="icons.restore" /><span>立即同步一轮</span>
                  </button>
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
                    <strong>内置 Mihomo 内核</strong>
                    <small v-if="kernelLoading">正在检测内核状态…</small>
                    <small v-else-if="kernelStatus?.installed">
                      版本：<code>{{ kernelStatus.version }}</code>
                      <span v-if="kernelStatus.latestVersion && kernelStatus.latestVersion !== kernelStatus.version" style="color: var(--primary, #3b82f6); font-weight: 500">
                        · 发现新版本 {{ kernelStatus.latestVersion }}
                      </span>
                      <br />
                      路径：<code class="lightweight-address">{{ kernelStatus.path }}</code>
                    </small>
                    <small v-else style="color: var(--danger, #ef4444)">
                      尚未安装内置内核，请点击右侧按钮一键下载
                    </small>
                    <div v-if="kernelDownloading" class="kernel-progress-box" style="margin-top: 8px;">
                      <div style="background: rgba(0,0,0,0.1); border-radius: 4px; height: 6px; overflow: hidden; width: 100%; max-width: 320px;">
                        <div style="background: var(--primary, #3b82f6); height: 100%; transition: width 0.2s;" :style="{ width: `${Math.max(5, Math.round(kernelDownloadProgress.progress * 100))}%` }" />
                      </div>
                      <small style="color: var(--primary, #3b82f6); margin-top: 4px; display: block;">{{ kernelDownloadProgress.message }}</small>
                    </div>
                  </div>
                  <div style="display: flex; gap: 8px; align-items: center">
                    <button
                      v-if="kernelStatus?.installed"
                      type="button"
                      class="secondary-button"
                      :disabled="kernelLoading || kernelChecking || kernelDownloading"
                      @click="checkMihomoKernelUpdate"
                    >
                      <span v-html="icons.restore" /><span>{{ kernelChecking ? "检查中…" : "检查更新" }}</span>
                    </button>
                    <button
                      type="button"
                      class="secondary-button"
                      :style="!kernelStatus?.installed ? 'background: var(--primary, #3b82f6); color: white; border-color: transparent;' : ''"
                      :disabled="kernelLoading || kernelDownloading"
                      @click="downloadOrUpdateMihomoKernel"
                    >
                      <span v-html="icons.download || icons.restore" />
                      <span>{{ kernelDownloading ? "正在下载…" : (kernelStatus?.installed ? "重新下载 / 更新" : "一键下载内核") }}</span>
                    </button>
                  </div>
                </div>

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
