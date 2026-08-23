<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { useProxyPool } from "../../composables/proxy/useProxyPool";
import { runCommand } from "../../composables/core/ipc";
import type { GeoipStatus, MihomoKernelStatus } from "../../types";

interface ComponentBootstrapStatus {
  mihomo: MihomoKernelStatus;
  geoip: GeoipStatus;
}

const proxy = useProxyPool();
const visible = ref(false);
const checking = ref(false);
const initializing = ref(false);
const started = ref(false);
const error = ref("");
const dismissed = ref(false);

const mihomoStatus = computed(() => proxy.kernelStatus.value);
const geoipStatus = computed(() => proxy.geoipStatus.value);
const mihomoDownloading = computed(() => proxy.kernelDownloading.value);
const mihomoLoading = computed(() => proxy.kernelLoading.value);
const geoipDownloading = computed(() => proxy.geoipDownloading.value);
const geoipLoading = computed(() => proxy.geoipLoading.value);
const mihomoDownloadProgress = computed(() => proxy.kernelDownloadProgress.value);
const geoipDownloadProgress = computed(() => proxy.geoipDownloadProgress.value);
const mihomoReady = computed(() => mihomoStatus.value?.installed === true);
const geoipReady = computed(() => geoipStatus.value?.installed === true);
const allReady = computed(() => mihomoReady.value && geoipReady.value);
const mihomoProgress = computed(() => Math.round((mihomoDownloadProgress.value.progress || 0) * 100));
const geoipProgress = computed(() => Math.round((geoipDownloadProgress.value.progress || 0) * 100));

function statusLabel(installed: boolean, downloading: boolean, loading: boolean) {
  if (installed) return "已就绪";
  if (downloading) return "下载中";
  if (loading) return "检测中";
  return "待初始化";
}

async function loadStatus() {
  checking.value = true;
  error.value = "";
  try {
    const status = await runCommand<ComponentBootstrapStatus>("get_component_bootstrap_status");
    proxy.kernelStatus.value = status.mihomo;
    proxy.geoipStatus.value = status.geoip;
    if (!allReady.value) {
      visible.value = true;
    }
  } catch (reason) {
    // 静态预览或不支持组件服务的 Web 页面不应被初始化引导阻塞。
    console.info("[OpenHub] 组件初始化状态不可用：", reason);
  } finally {
    checking.value = false;
  }
}

async function initialize() {
  started.value = true;
  initializing.value = true;
  error.value = "";
  const tasks: Promise<unknown>[] = [];
  if (!mihomoReady.value) tasks.push(proxy.downloadOrUpdateMihomoKernel());
  if (!geoipReady.value) tasks.push(proxy.downloadOrUpdateGeoip());
  try {
    await Promise.all(tasks);
    await loadStatus();
    if (allReady.value) visible.value = false;
  } catch (reason) {
    error.value = String(reason);
  } finally {
    initializing.value = false;
  }
}

async function retryMihomo() {
  error.value = "";
  try {
    await proxy.downloadOrUpdateMihomoKernel();
    await loadStatus();
  } catch (reason) {
    error.value = String(reason);
  }
}

async function retryGeoip() {
  error.value = "";
  try {
    await proxy.downloadOrUpdateGeoip();
    await loadStatus();
  } catch (reason) {
    error.value = String(reason);
  }
}

function dismiss() {
  dismissed.value = true;
  visible.value = false;
}

onMounted(() => {
  void loadStatus();
});
</script>

<template>
  <Teleport to="body">
    <Transition name="component-bootstrap-fade">
      <div v-if="visible && !dismissed" class="component-bootstrap-backdrop">
        <section class="component-bootstrap-dialog" role="dialog" aria-modal="true" aria-labelledby="component-bootstrap-title">
          <header class="component-bootstrap-header">
            <div>
              <span class="component-bootstrap-eyebrow">FIRST RUN SETUP</span>
              <h2 id="component-bootstrap-title">准备 OpenHub 运行组件</h2>
              <p>组件按需安装，不会占用安装包空间。完成后代理池和节点地域识别即可使用。</p>
            </div>
          </header>

          <div class="component-bootstrap-body">
            <div class="component-bootstrap-note">
              <span class="component-bootstrap-note-dot" />
              <span>主程序可以离线启动；组件下载需要网络，失败后可以稍后重试。</span>
            </div>

            <article class="component-bootstrap-item">
              <div class="component-bootstrap-item-icon is-mihomo">M</div>
              <div class="component-bootstrap-item-main">
                <div class="component-bootstrap-item-title">
                  <strong>Mihomo 代理内核</strong>
                  <span class="component-bootstrap-status" :class="{ ready: mihomoReady, loading: mihomoDownloading || mihomoLoading }">
                    {{ statusLabel(mihomoReady, mihomoDownloading, mihomoLoading) }}
                  </span>
                </div>
                <p>代理池测速、节点连接和代理规则运行所需的本地内核。</p>
                <div v-if="mihomoDownloading || (started && !mihomoReady)" class="component-bootstrap-progress">
                  <div class="component-bootstrap-track"><i :style="{ width: `${Math.max(4, mihomoProgress)}%` }" /></div>
                  <div class="component-bootstrap-meta">
                    <span>{{ mihomoDownloadProgress.message || "等待下载…" }}</span>
                    <span>{{ mihomoProgress }}%</span>
                  </div>
                </div>
                <div v-if="mihomoReady" class="component-bootstrap-detail">版本 {{ mihomoStatus?.version || "已安装" }}</div>
                <button v-if="!initializing && started && !mihomoReady" type="button" class="component-bootstrap-retry" @click="retryMihomo">重试 Mihomo</button>
              </div>
            </article>

            <article class="component-bootstrap-item">
              <div class="component-bootstrap-item-icon is-geoip">G</div>
              <div class="component-bootstrap-item-main">
                <div class="component-bootstrap-item-title">
                  <strong>GeoIP 地域数据库</strong>
                  <span class="component-bootstrap-status" :class="{ ready: geoipReady, loading: geoipDownloading || geoipLoading }">
                    {{ statusLabel(geoipReady, geoipDownloading, geoipLoading) }}
                  </span>
                </div>
                <p>为代理节点补充国家、地区和地域分类信息。</p>
                <div v-if="geoipDownloading || (started && !geoipReady)" class="component-bootstrap-progress">
                  <div class="component-bootstrap-track"><i :style="{ width: `${Math.max(4, geoipProgress)}%` }" /></div>
                  <div class="component-bootstrap-meta">
                    <span>{{ geoipDownloadProgress.message || "等待下载…" }}</span>
                    <span>{{ geoipProgress }}%</span>
                  </div>
                </div>
                <div v-if="geoipReady" class="component-bootstrap-detail">数据库 {{ geoipStatus?.fileSizeFormatted || "已安装" }}</div>
                <button v-if="!initializing && started && !geoipReady" type="button" class="component-bootstrap-retry" @click="retryGeoip">重试 GeoIP</button>
              </div>
            </article>

            <p v-if="error" class="component-bootstrap-error">{{ error }}</p>
          </div>

          <footer class="component-bootstrap-footer">
            <button type="button" class="component-bootstrap-later" :disabled="initializing || checking" @click="dismiss">稍后处理</button>
            <button type="button" class="component-bootstrap-primary" :disabled="initializing || checking || allReady" @click="initialize">
              {{ checking ? "正在检查…" : initializing ? "正在初始化…" : allReady ? "已完成" : "开始初始化" }}
            </button>
          </footer>
        </section>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
.component-bootstrap-backdrop {
  position: fixed;
  inset: 0;
  z-index: 220;
  display: grid;
  place-items: center;
  padding: 24px;
  background: rgba(7, 18, 12, .52);
  backdrop-filter: blur(8px);
}
.component-bootstrap-dialog {
  width: min(620px, 100%);
  overflow: hidden;
  border: 1px solid var(--line-strong);
  border-radius: var(--r-xl);
  background: var(--surface);
  box-shadow: var(--shadow-pop);
}
.component-bootstrap-header { padding: 28px 30px 22px; border-bottom: 1px solid var(--line-soft); }
.component-bootstrap-eyebrow { color: var(--brand); font-size: 10px; font-weight: 800; letter-spacing: .12em; }
.component-bootstrap-header h2 { margin: 8px 0 6px; color: var(--text); font-size: 22px; }
.component-bootstrap-header p, .component-bootstrap-item-main p { margin: 0; color: var(--muted); font-size: 12px; line-height: 1.6; }
.component-bootstrap-body { display: grid; gap: 12px; padding: 20px 30px 24px; }
.component-bootstrap-note { display: flex; gap: 8px; align-items: center; color: var(--muted); font-size: 11px; }
.component-bootstrap-note-dot { width: 7px; height: 7px; flex: 0 0 auto; border-radius: 50%; background: var(--brand); }
.component-bootstrap-item { display: flex; gap: 14px; padding: 15px; border: 1px solid var(--line); border-radius: var(--r-md); background: var(--surface-soft); }
.component-bootstrap-item-icon { display: grid; width: 36px; height: 36px; flex: 0 0 auto; place-items: center; border-radius: 10px; color: white; font-weight: 800; }
.component-bootstrap-item-icon.is-mihomo { background: var(--brand-deep); }
.component-bootstrap-item-icon.is-geoip { background: var(--info); }
.component-bootstrap-item-main { min-width: 0; flex: 1; }
.component-bootstrap-item-title { display: flex; align-items: center; justify-content: space-between; gap: 10px; margin-bottom: 4px; color: var(--text); font-size: 13px; }
.component-bootstrap-status { padding: 3px 7px; border-radius: var(--r-full); color: var(--warning); background: var(--warning-soft); font-size: 10px; white-space: nowrap; }
.component-bootstrap-status.ready { color: var(--success); background: var(--success-soft); }
.component-bootstrap-status.loading { color: var(--info); background: var(--info-soft); }
.component-bootstrap-progress { margin-top: 11px; }
.component-bootstrap-track { height: 5px; overflow: hidden; border-radius: 999px; background: var(--line); }
.component-bootstrap-track i { display: block; height: 100%; border-radius: inherit; background: var(--brand); transition: width .2s var(--ease); }
.component-bootstrap-meta { display: flex; justify-content: space-between; gap: 12px; margin-top: 5px; color: var(--muted); font-size: 10px; }
.component-bootstrap-detail { margin-top: 8px; color: var(--brand-deep); font-size: 11px; }
.component-bootstrap-retry { margin-top: 9px; padding: 0; border: 0; color: var(--brand-deep); background: transparent; font-size: 11px; cursor: pointer; }
.component-bootstrap-error { margin: 0; padding: 9px 10px; border-radius: var(--r-sm); color: var(--danger); background: var(--danger-soft); font-size: 11px; line-height: 1.5; overflow-wrap: anywhere; }
.component-bootstrap-footer { display: flex; justify-content: flex-end; gap: 10px; padding: 16px 30px 22px; border-top: 1px solid var(--line-soft); }
.component-bootstrap-later, .component-bootstrap-primary { min-height: 36px; padding: 0 15px; border: 1px solid var(--line-strong); border-radius: var(--r-sm); font-size: 12px; cursor: pointer; }
.component-bootstrap-later { color: var(--muted); background: var(--surface); }
.component-bootstrap-primary { border-color: var(--brand); color: white; background: var(--brand); }
.component-bootstrap-later:disabled, .component-bootstrap-primary:disabled { cursor: not-allowed; opacity: .55; }
.component-bootstrap-fade-enter-active, .component-bootstrap-fade-leave-active { transition: opacity .2s ease; }
.component-bootstrap-fade-enter-from, .component-bootstrap-fade-leave-to { opacity: 0; }
@media (max-width: 560px) {
  .component-bootstrap-backdrop { padding: 12px; }
  .component-bootstrap-header, .component-bootstrap-body, .component-bootstrap-footer { padding-left: 18px; padding-right: 18px; }
  .component-bootstrap-footer { justify-content: stretch; }
  .component-bootstrap-later, .component-bootstrap-primary { flex: 1; }
}
</style>
