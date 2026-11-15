<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { icons } from "../../icons";
import { useToast } from "../../composables/core/useToast";
import {
  CLIENT_PROTOCOLS, clientBaseUrl, clientConnectionJson, clientRequestExample,
  fetchClientModels, gatewayClientEndpoints, type ClientProtocol,
} from "../../composables/proxy/clientAccess";

const props = defineProps<{ origin: string; apiKey: string; running: boolean }>();
const emit = defineEmits<{ close: []; configure: []; models: [] }>();
const { showToast } = useToast();
const dialog = ref<HTMLDialogElement>();
const protocol = ref<ClientProtocol>("chat");
const models = ref<string[]>([]);
const selectedModel = ref("");
const query = ref("");
const loading = ref(false);
const error = ref("");
const revealKey = ref(false);
let activeRequest: AbortController | null = null;
let returnFocus: HTMLElement | null = null;
const urls = computed(() => gatewayClientEndpoints(props.origin));
const baseUrl = computed(() => clientBaseUrl(props.origin, protocol.value));
const protocolHint = computed(() => CLIENT_PROTOCOLS.find((item) => item.id === protocol.value)!.hint);
const filteredModels = computed(() => models.value.filter((id) => id.toLowerCase().includes(query.value.trim().toLowerCase())));
const example = computed(() => selectedModel.value ? clientRequestExample(props.origin, protocol.value, selectedModel.value) : "");
const connection = computed(() => selectedModel.value ? clientConnectionJson(props.origin, protocol.value, selectedModel.value) : "");

async function reloadModels() {
  activeRequest?.abort();
  const controller = new AbortController();
  activeRequest = controller;
  loading.value = false;
  error.value = "";
  models.value = [];
  if (!props.running || !props.apiKey) return;
  loading.value = true;
  const timeout = window.setTimeout(() => controller.abort(), 10_000);
  try {
    const next = await fetchClientModels(props.origin, props.apiKey, controller.signal);
    if (controller !== activeRequest || controller.signal.aborted) return;
    models.value = next;
    if (!next.includes(selectedModel.value)) selectedModel.value = next[0] ?? "";
  } catch (cause) {
    if (controller !== activeRequest) return;
    selectedModel.value = "";
    error.value = controller.signal.aborted ? "读取模型目录超过 10 秒，请确认服务可访问后重试" : String(cause instanceof Error ? cause.message : cause);
  } finally {
    window.clearTimeout(timeout);
    if (controller === activeRequest) loading.value = false;
  }
}

watch(() => [props.origin, props.apiKey, props.running] as const, () => {
  selectedModel.value = "";
  revealKey.value = false;
  void reloadModels();
}, { immediate: true });

async function copy(value: string, label: string) {
  if (!value) return;
  try {
    await navigator.clipboard.writeText(value);
    showToast(`已复制${label}`);
  } catch { showToast("复制失败，请手动选择内容复制", true); }
}

function onBackdrop(event: MouseEvent) {
  if (event.target !== dialog.value) return;
  const rect = dialog.value!.getBoundingClientRect();
  if (event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom) emit("close");
}

onMounted(() => {
  returnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  dialog.value?.showModal();
});
onUnmounted(() => {
  activeRequest?.abort();
  activeRequest = null;
  dialog.value?.close();
  returnFocus?.focus();
});
</script>

<template>
  <Teleport to="body">
    <dialog ref="dialog" class="client-access" aria-labelledby="client-access-title" @cancel.prevent="emit('close')" @click="onBackdrop">
      <header class="access-header">
        <div>
          <p class="access-eyebrow">OPENHUB GATEWAY</p>
          <h2 id="client-access-title">客户端接入</h2>
          <p>在第三方客户端填写网关地址、API Key 和模型 ID 即可接入。</p>
        </div>
        <button type="button" class="access-icon" aria-label="关闭客户端接入" autofocus @click="emit('close')"><span v-html="icons.close" /></button>
      </header>

      <div class="access-body">
        <div v-if="!running" class="access-notice" role="status">模型网关尚未启用。请先关闭窗口，在反代页启动服务。</div>
        <div v-if="!apiKey" class="access-notice" role="status">尚未配置网关 API Key。<button type="button" @click="emit('configure')">打开服务配置</button></div>
        <nav class="access-protocols" aria-label="接入协议">
          <button v-for="item in CLIENT_PROTOCOLS" :key="item.id" type="button" :class="{ active: protocol === item.id }" :aria-pressed="protocol === item.id" @click="protocol = item.id">{{ item.label }}</button>
        </nav>
        <p class="access-hint">{{ protocolHint }}</p>

        <div class="access-fields">
          <div class="access-field">
            <label>Base URL</label>
            <div class="access-value"><code>{{ baseUrl }}</code><button type="button" class="access-icon" aria-label="复制客户端 Base URL" @click="copy(baseUrl, 'Base URL')"><span v-html="icons.copy" /></button></div>
          </div>
          <div class="access-field">
            <label>网关 API Key</label>
            <div class="access-value"><code>{{ apiKey ? (revealKey ? apiKey : '••••••••••••••••••••') : '尚未配置' }}</code><button type="button" class="access-icon" :disabled="!apiKey" :aria-label="revealKey ? '隐藏网关 Key' : '显示网关 Key'" @click="revealKey = !revealKey"><span v-html="revealKey ? icons.eyeOff : icons.eye" /></button><button type="button" class="access-icon" :disabled="!apiKey" aria-label="复制客户端 API Key" @click="copy(apiKey, '网关 API Key')"><span v-html="icons.copy" /></button></div>
          </div>
          <div class="access-field access-directory">
            <label>模型发现地址</label>
            <div class="access-value"><code>{{ urls.models }}</code><button type="button" class="access-icon" aria-label="复制模型发现地址" @click="copy(urls.models, '模型发现地址')"><span v-html="icons.copy" /></button></div>
          </div>
        </div>

        <section class="access-models" aria-labelledby="access-model-title">
          <div class="access-section-head"><h3 id="access-model-title">模型调用 ID <span>{{ models.length }}</span></h3><button type="button" class="access-button" :disabled="loading || !running || !apiKey" @click="reloadModels">{{ loading ? '读取中…' : '刷新目录' }}</button></div>
          <p class="access-hint">目录来自网关当前的 /v1/models；渠道别名前缀是调用 ID 的一部分，请完整填写。</p>
          <p v-if="error" class="access-error" role="alert">{{ error }}</p>
          <p v-else-if="loading" class="access-empty" role="status">正在读取网关模型目录…</p>
          <template v-else-if="models.length">
            <input v-model="query" class="access-search" type="search" aria-label="搜索模型调用 ID" placeholder="搜索模型或渠道别名" />
            <div class="access-model-list" role="group" aria-label="选择模型调用 ID">
              <button v-for="id in filteredModels" :key="id" type="button" :class="{ selected: selectedModel === id }" :aria-pressed="selectedModel === id" @click="selectedModel = id"><code>{{ id }}</code><span v-if="selectedModel === id" v-html="icons.check" /></button>
              <p v-if="!filteredModels.length" class="access-empty">没有匹配的模型。</p>
            </div>
          </template>
          <p v-else-if="running && apiKey" class="access-empty">网关当前没有对外提供的模型。<button type="button" @click="emit('models')">查看可用模型</button></p>
          <div v-if="selectedModel && models.includes(selectedModel)" class="access-selected"><code>{{ selectedModel }}</code><button type="button" class="access-button" @click="copy(selectedModel, '模型调用 ID')">复制模型 ID</button></div>
        </section>

        <section v-if="example && models.includes(selectedModel)" class="access-example" aria-labelledby="access-example-title">
          <div class="access-section-head"><h3 id="access-example-title">请求示例</h3><button type="button" class="access-button" @click="copy(example, '请求示例')">复制 cURL</button></div>
          <p class="access-hint">示例适用于 Bash / zsh。先将环境变量 OPENHUB_API_KEY 设为上方复制的 Key，再运行。</p>
          <pre><code>{{ example }}</code></pre>
          <details><summary>接入信息 JSON（用于对照填写）</summary><pre><code>{{ connection }}</code></pre><button type="button" class="access-button" @click="copy(connection, '接入信息')">复制接入信息</button></details>
        </section>
        <p class="access-footnote">模型的上下文、输出上限和思考参数请在客户端设置。跨设备接入时，将本机地址替换为可访问的服务地址；Gemini 客户端若单独填写 API 版本，使用服务根地址并选择 v1beta。</p>
      </div>
    </dialog>
  </Teleport>
</template>

<style scoped>
.client-access { width: min(820px, calc(100vw - 32px)); max-height: calc(100dvh - 48px); padding: 0; border: 1px solid var(--line); border-radius: 18px; background: var(--surface); color: var(--text); box-shadow: 0 24px 80px #0004; overflow: auto; }
.client-access::backdrop { background: #10172588; backdrop-filter: blur(5px); }
.access-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 20px; padding: 24px 28px 20px; border-bottom: 1px solid var(--line); }
.access-header h2 { margin: 5px 0 8px; font-size: 23px; letter-spacing: -.5px; }
.access-header p, .access-hint, .access-footnote { color: var(--muted); font-size: 12px; line-height: 1.7; margin: 0; }
.access-header .access-eyebrow { font-size: 10px; letter-spacing: 1.6px; color: var(--brand); font-weight: 700; }
.access-body { display: grid; gap: 18px; padding: 24px 28px; }
.access-protocols { display: flex; gap: 6px; padding: 5px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface-soft); }
.access-protocols button { flex: 1; border: 0; background: transparent; color: var(--muted); padding: 9px 8px; border-radius: 6px; font: inherit; font-size: 12px; cursor: pointer; }
.access-protocols button.active { background: var(--surface); color: var(--text); box-shadow: 0 1px 5px #0001; font-weight: 650; }
.access-fields { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; }
.access-field { min-width: 0; }
.access-field label { display: block; font-size: 11px; color: var(--muted); margin-bottom: 7px; }
.access-value { display: flex; align-items: center; gap: 6px; padding: 8px 10px; min-height: 42px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface-soft); }
.access-value code { flex: 1; overflow-wrap: anywhere; min-width: 0; font-size: 12px; }
.access-directory { grid-column: 1 / -1; }
.access-icon { display: inline-flex; align-items: center; justify-content: center; width: 28px; height: 28px; padding: 5px; flex-shrink: 0; border: 0; border-radius: 5px; background: transparent; color: var(--muted); cursor: pointer; }
.access-icon:hover:not(:disabled), .access-button:hover:not(:disabled) { color: var(--text); background: var(--surface-soft); }
.access-icon :deep(svg), .access-model-list :deep(svg) { width: 15px; height: 15px; }
.access-icon:disabled, .access-button:disabled { opacity: .45; cursor: default; }
.access-section-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-bottom: 8px; }
.access-section-head h3 { margin: 0; font-size: 13px; font-weight: 650; }
.access-section-head h3 span { font-size: 11px; color: var(--muted); margin-left: 5px; }
.access-button { border: 1px solid var(--line); border-radius: 6px; background: var(--surface); color: var(--text); font: inherit; font-size: 11px; padding: 6px 10px; cursor: pointer; flex-shrink: 0; }
.access-search { width: 100%; box-sizing: border-box; border: 1px solid var(--line); border-radius: 7px; padding: 10px 12px; margin-top: 12px; background: var(--surface); color: var(--text); font: inherit; font-size: 12px; }
.access-model-list { max-height: 180px; overflow: auto; display: grid; gap: 3px; padding: 5px; margin-top: 7px; border: 1px solid var(--line); border-radius: 8px; }
.access-model-list > button { display: flex; justify-content: space-between; align-items: center; gap: 10px; text-align: left; border: 0; border-radius: 5px; padding: 8px 10px; background: transparent; color: var(--text); cursor: pointer; overflow-wrap: anywhere; }
.access-model-list > button.selected { background: var(--surface-soft); color: var(--brand); }
.access-model-list code, .access-selected code { font-size: 12px; }
.access-selected { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-top: 10px; }
.access-selected code { overflow-wrap: anywhere; }
.access-example pre { margin: 10px 0 0; max-height: 230px; overflow: auto; border-radius: 9px; border: 1px solid var(--line); padding: 14px; background: var(--surface-soft); font-size: 11px; line-height: 1.7; }
.access-example details { font-size: 12px; margin-top: 12px; }
.access-example summary { color: var(--muted); cursor: pointer; }
.access-example details .access-button { margin-top: 8px; }
.access-notice, .access-error { padding: 12px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface-soft); font-size: 12px; line-height: 1.7; margin: 0; }
.access-error { color: var(--danger, #dc3545); margin-top: 12px; }
.access-empty { color: var(--muted); font-size: 12px; padding: 16px 10px; text-align: center; }
.access-notice button, .access-empty button { border: 0; background: transparent; color: var(--brand); cursor: pointer; font: inherit; }
.access-footnote { padding-top: 14px; border-top: 1px solid var(--line); }
@media (max-width: 600px) { .access-header, .access-body { padding: 18px; } .access-fields { grid-template-columns: 1fr; } .access-protocols { flex-wrap: wrap; } .access-protocols button { flex-basis: 40%; } }
</style>
