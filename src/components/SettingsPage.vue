<script setup lang="ts">
import { ref, computed, watch, nextTick } from "vue";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import { usePreferences } from "../composables/usePreferences";
import { useTheme } from "../composables/useTheme";
import { useProxyPool, KERNEL_DOWNLOAD_MIRRORS } from "../composables/useProxyPool";
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
  geoipStatus,
  geoipLoading,
  geoipDownloading,
  geoipDownloadProgress,
  kernelSelectedMirror,
  kernelCustomMirror,
  loadMihomoKernelStatus,
  checkMihomoKernelUpdate,
  downloadOrUpdateMihomoKernel,
  downloadOrUpdateGeoip,
} = useProxyPool();

const kernelParsedVersion = computed(() => {
  const raw = kernelStatus.value?.version || "";
  if (!raw) return { tag: "未安装", arch: "" };
  const tagMatch = raw.match(/v\d+\.\d+(\.\d+)?/i);
  const tag = tagMatch ? tagMatch[0] : (raw.split(" ")[0] || raw);
  let arch = "";
  if (/darwin/i.test(raw)) arch = "macOS";
  else if (/windows/i.test(raw)) arch = "Windows";
  else if (/linux/i.test(raw)) arch = "Linux";

  if (/arm64|aarch64/i.test(raw)) arch += " ARM64";
  else if (/amd64|x86_64|x64/i.test(raw)) arch += " x64";

  return { tag, arch: arch.trim() };
});

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
                <!-- 全局组件下载加速源选择栏 -->
                <div class="settings-row component-mirror-row">
                  <div>
                    <strong>组件下载加速源</strong>
                    <small>共用于内置 Mihomo 内核与 GeoIP 数据库的极速拉取与在线更新</small>
                  </div>
                  <div class="component-mirror-controls">
                    <select v-model="kernelSelectedMirror" class="kernel-mirror-select">
                      <option v-for="m in KERNEL_DOWNLOAD_MIRRORS" :key="m.value" :value="m.value">
                        {{ m.text }}
                      </option>
                    </select>
                    <input
                      v-if="kernelSelectedMirror === 'custom'"
                      v-model="kernelCustomMirror"
                      type="text"
                      class="kernel-custom-mirror-input"
                      placeholder="https://your-mirror.com/"
                    />
                  </div>
                </div>

                <!-- 专属美化版 Mihomo 内核卡片 -->
                <div class="kernel-card">
                  <div class="kernel-card-header">
                    <div class="kernel-card-identity">
                      <div class="kernel-icon-badge">
                        <span v-html="icons.wifi" />
                      </div>
                      <div class="kernel-identity-text">
                        <div class="kernel-title-row">
                          <span class="kernel-title">内置 Mihomo 内核</span>
                          <span v-if="kernelLoading" class="kernel-badge is-loading">检测中…</span>
                          <span v-else-if="kernelStatus?.installed" class="kernel-badge is-ready">
                            <i class="dot" /> 运行就绪
                          </span>
                          <span v-else class="kernel-badge is-missing">
                            <i class="dot" /> 待安装
                          </span>
                        </div>
                        <div class="kernel-subtitle">
                          <template v-if="kernelStatus?.installed">
                            <span>版本</span>
                            <strong class="kernel-version-tag">{{ kernelParsedVersion.tag }}</strong>
                            <span v-if="kernelParsedVersion.arch" class="kernel-arch-tag">{{ kernelParsedVersion.arch }}</span>
                            <span
                              v-if="kernelStatus.latestVersion && kernelStatus.latestVersion !== kernelParsedVersion.tag"
                              class="kernel-update-pill"
                            >
                              发现新版本 {{ kernelStatus.latestVersion }}
                            </span>
                          </template>
                          <span v-else class="kernel-missing-text">未检测到内置内核，点击右侧按钮一键下载</span>
                        </div>
                      </div>
                    </div>

                    <!-- 操作按钮组 -->
                    <div class="kernel-card-actions">
                      <button
                        v-if="kernelStatus?.installed"
                        type="button"
                        class="kernel-btn-secondary"
                        :disabled="kernelLoading || kernelChecking || kernelDownloading"
                        @click="checkMihomoKernelUpdate()"
                      >
                        <span class="btn-icon" :class="{ 'is-spinning': kernelChecking }" v-html="icons.restore" />
                        <span>{{ kernelChecking ? "检查中…" : "检查更新" }}</span>
                      </button>

                      <button
                        type="button"
                        class="kernel-btn-primary"
                        :class="{ 'is-accent': !kernelStatus?.installed }"
                        :disabled="kernelLoading || kernelDownloading"
                        @click="downloadOrUpdateMihomoKernel()"
                      >
                        <span class="btn-icon" v-html="icons.download || icons.restore" />
                        <span>{{ kernelDownloading ? "正在下载…" : (kernelStatus?.installed ? "重新下载 / 更新" : "一键下载内核") }}</span>
                      </button>
                    </div>
                  </div>

                  <!-- 下载进度条 -->
                  <div v-if="kernelDownloading" class="kernel-progress-wrapper">
                    <div class="kernel-progress-track">
                      <div
                        class="kernel-progress-fill"
                        :style="{ width: `${Math.max(4, Math.round(kernelDownloadProgress.progress * 100))}%` }"
                      />
                    </div>
                    <div class="kernel-progress-meta">
                      <span class="kernel-progress-msg">{{ kernelDownloadProgress.message }}</span>
                      <span class="kernel-progress-pct">{{ Math.round(kernelDownloadProgress.progress * 100) }}%</span>
                    </div>
                  </div>
                </div>

                <!-- 专属 GeoIP 数据库管理卡片 -->
                <div class="kernel-card geoip-card">
                  <div class="kernel-card-header">
                    <div class="kernel-card-identity">
                      <div class="kernel-icon-badge is-geoip">
                        <span v-html="icons.globe" />
                      </div>
                      <div class="kernel-identity-text">
                        <div class="kernel-title-row">
                          <span class="kernel-title">GeoIP 国家与地域数据库</span>
                          <span v-if="geoipLoading" class="kernel-badge is-loading">检测中…</span>
                          <span v-else-if="geoipStatus?.installed" class="kernel-badge is-ready">
                            <i class="dot" /> 运行就绪
                          </span>
                          <span v-else class="kernel-badge is-missing">
                            <i class="dot" /> 待安装
                          </span>
                        </div>
                        <div class="kernel-subtitle">
                          <template v-if="geoipStatus?.installed">
                            <span>数据库大小</span>
                            <strong class="kernel-version-tag">{{ geoipStatus.fileSizeFormatted }}</strong>
                            <span v-if="geoipStatus.updatedAt" class="kernel-arch-tag">更新于 {{ geoipStatus.updatedAt }}</span>
                          </template>
                          <span v-else class="kernel-missing-text">未检测到本地 GeoIP 数据库，建议下载以获得精准节点国旗与地域解析</span>
                        </div>
                      </div>
                    </div>

                    <!-- 操作按钮组 -->
                    <div class="kernel-card-actions">
                      <button
                        type="button"
                        class="kernel-btn-primary"
                        :class="{ 'is-accent': !geoipStatus?.installed }"
                        :disabled="geoipLoading || geoipDownloading"
                        @click="downloadOrUpdateGeoip()"
                      >
                        <span class="btn-icon" :class="{ 'is-spinning': geoipDownloading }" v-html="icons.download || icons.restore" />
                        <span>{{ geoipDownloading ? "正在下载…" : (geoipStatus?.installed ? "重新下载 / 更新" : "一键下载 GeoIP") }}</span>
                      </button>
                    </div>
                  </div>

                  <!-- 下载进度条 -->
                  <div v-if="geoipDownloading" class="kernel-progress-wrapper">
                    <div class="kernel-progress-track">
                      <div
                        class="kernel-progress-fill"
                        :style="{ width: `${Math.max(4, Math.round(geoipDownloadProgress.progress * 100))}%` }"
                      />
                    </div>
                    <div class="kernel-progress-meta">
                      <span class="kernel-progress-msg">{{ geoipDownloadProgress.message }}</span>
                      <span class="kernel-progress-pct">{{ Math.round(geoipDownloadProgress.progress * 100) }}%</span>
                    </div>
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

<style scoped>
.kernel-card {
  margin-bottom: 16px;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg);
  padding: 16px 18px;
  box-shadow: var(--shadow-xs);
  display: flex;
  flex-direction: column;
  gap: 12px;
  transition: border-color 0.2s, box-shadow 0.2s;
}

.kernel-card:hover {
  border-color: var(--line-strong);
  box-shadow: var(--shadow-sm);
}

.kernel-card-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
}

.kernel-card-identity {
  display: flex;
  align-items: center;
  gap: 12px;
  min-width: 0;
}

.kernel-icon-badge {
  width: 38px;
  height: 38px;
  border-radius: var(--r-md);
  background: linear-gradient(135deg, rgba(59, 130, 246, 0.14), rgba(99, 102, 241, 0.08));
  border: 1px solid rgba(59, 130, 246, 0.22);
  color: #3b82f6;
  display: grid;
  place-items: center;
  flex-shrink: 0;
}

.kernel-icon-badge.is-geoip {
  background: linear-gradient(135deg, rgba(16, 185, 129, 0.14), rgba(6, 182, 212, 0.08));
  border-color: rgba(16, 185, 129, 0.25);
  color: #10b981;
}

.kernel-icon-badge svg {
  width: 18px;
  height: 18px;
}

.kernel-identity-text {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-width: 0;
}

.kernel-title-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.kernel-title {
  font-size: 13.5px;
  font-weight: 700;
  color: var(--text);
}

.kernel-badge {
  font-size: 11px;
  padding: 2px 8px;
  border-radius: 12px;
  font-weight: 600;
  display: inline-flex;
  align-items: center;
  gap: 5px;
  line-height: 1.2;
}

.kernel-badge.is-ready {
  background: rgba(16, 185, 129, 0.12);
  color: #10b981;
  border: 1px solid rgba(16, 185, 129, 0.25);
}

.kernel-badge.is-ready .dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: #10b981;
  box-shadow: 0 0 6px rgba(16, 185, 129, 0.6);
}

.kernel-badge.is-missing {
  background: rgba(239, 68, 68, 0.12);
  color: #ef4444;
  border: 1px solid rgba(239, 68, 68, 0.25);
}

.kernel-badge.is-missing .dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: #ef4444;
  box-shadow: 0 0 6px rgba(239, 68, 68, 0.6);
}

.kernel-badge.is-loading {
  background: var(--surface-soft);
  color: var(--muted);
  border: 1px solid var(--line);
}

.kernel-subtitle {
  font-size: 12px;
  color: var(--muted);
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.kernel-version-tag {
  background: var(--surface-soft);
  padding: 1px 6px;
  border-radius: 4px;
  border: 1px solid var(--line);
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  font-size: 11.5px;
  font-weight: 600;
  color: var(--text);
}

.kernel-arch-tag {
  font-size: 10.5px;
  font-weight: 600;
  color: var(--faint);
  background: var(--surface-soft);
  padding: 1px 5px;
  border-radius: 3px;
  border: 1px solid var(--line-soft);
}

.kernel-update-pill {
  background: linear-gradient(135deg, #3b82f6, #6366f1);
  color: #fff;
  font-size: 10.5px;
  font-weight: 600;
  padding: 2px 8px;
  border-radius: 10px;
  box-shadow: 0 2px 8px rgba(59, 130, 246, 0.35);
  animation: pulse-subtle 2s infinite;
}

.kernel-missing-text {
  color: var(--danger, #ef4444);
  font-size: 11.5px;
}

.kernel-card-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-shrink: 0;
}

.kernel-btn-secondary,
.kernel-btn-primary {
  height: 32px;
  padding: 0 13px;
  font-size: 12px;
  font-weight: 600;
  border-radius: var(--r-md);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  cursor: pointer;
  white-space: nowrap;
  flex-shrink: 0;
  transition: all 0.15s var(--ease);
}

.kernel-btn-secondary {
  background: var(--surface-soft);
  border: 1px solid var(--line);
  color: var(--text);
}

.kernel-btn-secondary:hover:not(:disabled) {
  background: var(--surface-hover);
  border-color: var(--line-strong);
  box-shadow: var(--shadow-xs);
}

.kernel-btn-primary {
  background: var(--surface-soft);
  border: 1px solid var(--line);
  color: var(--text);
}

.kernel-btn-primary:hover:not(:disabled) {
  background: var(--surface-hover);
  border-color: var(--line-strong);
}

.kernel-btn-primary.is-accent {
  background: linear-gradient(135deg, #3b82f6, #2563eb);
  border: 1px solid #2563eb;
  color: #fff;
  box-shadow: 0 2px 8px rgba(37, 99, 235, 0.3);
}

.kernel-btn-primary.is-accent:hover:not(:disabled) {
  background: linear-gradient(135deg, #2563eb, #1d4ed8);
  box-shadow: 0 4px 12px rgba(37, 99, 235, 0.4);
}

.kernel-btn-secondary:disabled,
.kernel-btn-primary:disabled {
  opacity: 0.55;
  cursor: not-allowed;
}

.btn-icon {
  display: inline-flex;
  align-items: center;
  justify-content: center;
}

.btn-icon svg {
  width: 14px;
  height: 14px;
}

.btn-icon.is-spinning svg {
  animation: spin 1s linear infinite;
}

.kernel-progress-wrapper {
  background: var(--surface-soft);
  border: 1px solid var(--line-soft);
  border-radius: var(--r-md);
  padding: 10px 12px;
}

.kernel-progress-track {
  height: 6px;
  border-radius: 3px;
  background: rgba(0, 0, 0, 0.08);
  overflow: hidden;
}

.kernel-progress-fill {
  height: 100%;
  border-radius: 3px;
  background: linear-gradient(90deg, #3b82f6, #6366f1);
  transition: width 0.2s cubic-bezier(0.4, 0, 0.2, 1);
}

.kernel-progress-meta {
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-size: 11px;
  color: var(--muted);
  margin-top: 6px;
}

.kernel-progress-pct {
  font-weight: 700;
  color: #3b82f6;
  font-family: ui-monospace, monospace;
}

.kernel-path-footer {
  display: flex;
  align-items: center;
  gap: 8px;
  padding-top: 10px;
  border-top: 1px dashed var(--line-soft);
  font-size: 11px;
  color: var(--faint);
}

.kernel-path-label {
  flex-shrink: 0;
  font-weight: 500;
}

.kernel-path-value {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--muted);
  background: var(--surface-soft);
  padding: 2px 6px;
  border-radius: 4px;
  border: 1px solid var(--line-soft);
  max-width: 100%;
}

.component-mirror-controls {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.kernel-mirror-picker {
  display: inline-flex;
  align-items: center;
  gap: 8px;
}

.kernel-mirror-label {
  font-size: 11px;
  font-weight: 600;
  color: var(--faint);
  flex-shrink: 0;
}

.kernel-mirror-select {
  height: 28px;
  padding: 0 8px 0 10px;
  border-radius: var(--r-sm);
  background: var(--surface-soft);
  border: 1px solid var(--line);
  color: var(--text);
  font-size: 11.5px;
  font-weight: 500;
  outline: none;
  cursor: pointer;
  transition: border-color 0.15s;
}

.kernel-mirror-select:hover {
  border-color: var(--line-strong);
}

.kernel-custom-mirror-input {
  height: 28px;
  padding: 0 10px;
  border-radius: var(--r-sm);
  background: var(--surface-soft);
  border: 1px solid var(--line);
  color: var(--text);
  font-size: 11.5px;
  outline: none;
  width: 220px;
  transition: border-color 0.15s;
}

.kernel-custom-mirror-input:focus {
  border-color: var(--primary, #3b82f6);
}

@keyframes spin {
  from { transform: rotate(0deg); }
  to { transform: rotate(360deg); }
}

@keyframes pulse-subtle {
  0%, 100% { opacity: 1; transform: scale(1); }
  50% { opacity: 0.88; transform: scale(1.03); }
}
</style>
