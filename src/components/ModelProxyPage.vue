<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from "vue";
import { icons } from "../icons";
import { useModelProxy, type ChannelConfig, type ProxyRequestLog } from "../composables/useModelProxy";
import { useToast } from "../composables/useToast";

const { showToast } = useToast();
const {
  proxyConfig,
  proxyStatus,
  savingConfig,
  togglingServer,
  testingHealth,
  fetchingModels,
  modelsList,
  healthResult,
  healthResultTime,
  proxyLogs,
  loadingLogs,
  loadProxyData,
  refreshStatus,
  saveConfig,
  toggleServer,
  testHealth,
  refreshModels,
  fetchLogs,
  clearLogs,
  copyProxyUrl,
  copyProxyKey,
} = useModelProxy();

const showKey = ref(false);
const gatewaySearchQuery = ref("");
const channelSearchQuery = ref("");
const logSearchQuery = ref("");
const logStatusFilter = ref<"all" | "success" | "error">("all");
const configModalOpen = ref(false);
const healthModalOpen = ref(false);
const gatewayModelsModalOpen = ref(false);
const currentMainTab = ref<"console" | "channels" | "logs">("console");
const channelModelsModalOpen = ref(false);
const clearLogsModalOpen = ref(false);
const clearingLogs = ref(false);
const selectedLogForDetail = ref<ProxyRequestLog | null>(null);
const selectedChannel = ref<ChannelConfig | null>(null);
const copiedModelId = ref<string | null>(null);

async function handleClearLogs(mode: "payload_only" | "all") {
  clearingLogs.value = true;
  try {
    await clearLogs(mode);
    clearLogsModalOpen.value = false;
  } finally {
    clearingLogs.value = false;
  }
}

let uptimeTicker: number | null = null;
let statusPollTimer: number | null = null;

onMounted(async () => {
  await loadProxyData();
  await refreshModels();
  await fetchLogs();

  uptimeTicker = window.setInterval(() => {
    if (proxyStatus.value.running) {
      proxyStatus.value.uptimeSeconds += 1;
    }
  }, 1000);

  statusPollTimer = window.setInterval(async () => {
    if (proxyStatus.value.running) {
      await refreshStatus();
      if (currentMainTab.value === "logs") {
        await fetchLogs();
      }
    }
  }, 3000);
});

onUnmounted(() => {
  if (uptimeTicker !== null) {
    clearInterval(uptimeTicker);
    uptimeTicker = null;
  }
  if (statusPollTimer !== null) {
    clearInterval(statusPollTimer);
    statusPollTimer = null;
  }
});

async function switchToLogsTab() {
  currentMainTab.value = "logs";
  await fetchLogs();
}

async function handleSave() {
  const ok = await saveConfig(proxyConfig.value);
  if (ok) {
    configModalOpen.value = false;
  }
}

async function handleChannelSave(channel: ChannelConfig) {
  const ok = await saveConfig(proxyConfig.value);
  if (ok) {
    showToast(`已更新「${channel.name}」渠道设置`);
  }
}

async function handleOpenHealthModal() {
  healthModalOpen.value = true;
  await testHealth();
}

function closeHealthModal() {
  healthModalOpen.value = false;
}

function handleOpenGatewayModelsModal() {
  gatewayModelsModalOpen.value = true;
  if (modelsList.value.length === 0) {
    refreshModels();
  }
}

function closeGatewayModelsModal() {
  gatewayModelsModalOpen.value = false;
}

function handleOpenChannelModelsModal(channel: ChannelConfig) {
  selectedChannel.value = channel;
  channelModelsModalOpen.value = true;
  if (modelsList.value.length === 0) {
    refreshModels();
  }
}

function closeChannelModelsModal() {
  channelModelsModalOpen.value = false;
}

const detailActiveTab = ref<"tokens" | "request" | "response" | "meta" | "error">("tokens");

function openLogDetail(log: ProxyRequestLog) {
  selectedLogForDetail.value = log;
  if (log.statusCode >= 400) {
    detailActiveTab.value = "error";
  } else {
    detailActiveTab.value = "tokens";
  }
}

function closeLogDetail() {
  selectedLogForDetail.value = null;
}

function getCacheHitRate(log: ProxyRequestLog): string {
  const prompt = log.promptTokens || 0;
  const hit = log.promptCacheHitTokens || 0;
  if (prompt <= 0) return "0.0";
  return ((hit / prompt) * 100).toFixed(1);
}

function getReasoningRate(log: ProxyRequestLog): string {
  const completion = log.completionTokens || 0;
  const reasoning = log.reasoningTokens || 0;
  if (completion <= 0) return "0.0";
  return ((reasoning / completion) * 100).toFixed(1);
}

function getEstimatedTps(log: ProxyRequestLog): string {
  const tokens = log.completionTokens || 0;
  if (tokens <= 0) return "0.0";
  const genDur = log.ttftMs && log.durationMs > log.ttftMs ? log.durationMs - log.ttftMs : log.durationMs;
  if (genDur <= 0) return "0.0";
  return ((tokens / genDur) * 1000).toFixed(1);
}

async function copyText(text: string, label = "内容") {
  try {
    await navigator.clipboard.writeText(text);
    showToast(`已复制 ${label}`);
  } catch {
    showToast("复制失败", true);
  }
}

function getErrorSuggestion(log: ProxyRequestLog): string {
  if (log.statusCode === 401) {
    return "本地 Bearer API Key 校验未通过。请检查调用工具中的 Authorization Header 是否与服务配置中的访问密钥一致。";
  }
  if (log.statusCode === 502 || log.statusCode === 504) {
    return "上游网关连接超时或连接中断。若开启了代理池，请检查代理节点是否通畅；若未开启，请检查上游端点连通性。";
  }
  if (log.statusCode === 503) {
    return "该渠道当前在系统配置中处于「已禁用」状态，请在主界面开启对应渠道开关。";
  }
  if (log.statusCode === 400 || log.statusCode === 422) {
    return "请求体参数格式不正确，或传入了上游不支持的模型名称与特殊参数。";
  }
  if (log.statusCode === 429) {
    return "OpenCode Public 免费通道对单出口 IP 存在频次限制 (Rate limit exceeded)。解决方案：① 在对应渠道卡片开启「内部代理池轮询」，网关将自动通过系统代理池中 ≤1000ms 的多个健康节点轮询出口 IP，并在遭遇频次限制时自动秒级重试切换；② 切换其他免费模型（如 deepseek-v4-flash-free / nemotron-3-ultra-free / mimo-v2.5-free / laguna-s-2.1-free）；③ 稍候 30 秒后自动恢复。";
  }
  return "请根据下方原始错误响应体排查上游返回的具体原因。";
}

function formatCompactToken(val?: number | null): string {
  const num = Number(val ?? 0);
  if (!Number.isFinite(num) || num <= 0) return "0";
  if (num < 1000) return String(Math.round(num));
  if (num < 1_000_000) {
    const k = (num / 1000).toFixed(num < 100_000 ? 1 : 0).replace(/\.0$/, "");
    return `${k}k`;
  }
  const m = (num / 1_000_000).toFixed(num < 100_000_000 ? 1 : 0).replace(/\.0$/, "");
  return `${m}m`;
}

function formatSec(ms?: number | null): string {
  if (ms == null || !Number.isFinite(ms)) return "--";
  const sec = ms / 1000;
  if (sec < 0.01 && ms > 0) return "<0.01s";
  return `${sec.toFixed(2)}s`;
}

interface HealthCheckItem {
  name: string;
  endpoint: string;
  status: string;
  message: string;
  auth?: string;
}

const healthCheckList = computed<HealthCheckItem[]>(() => {
  if (!healthResult.value) return [];
  if (Array.isArray(healthResult.value)) {
    return healthResult.value as HealthCheckItem[];
  }
  if (healthResult.value.checks && Array.isArray(healthResult.value.checks)) {
    return healthResult.value.checks as HealthCheckItem[];
  }
  if (healthResult.value.error) {
    return [
      {
        name: "健康检查连接",
        endpoint: "/healthz",
        status: "error",
        message: String(healthResult.value.error),
        auth: "公开",
      },
    ];
  }
  return [
    {
      name: "本地服务",
      endpoint: "/healthz",
      status: "ok",
      message: JSON.stringify(healthResult.value),
      auth: "公开",
    },
  ];
});

const healthAllPassed = computed(() => {
  if (healthCheckList.value.length === 0) return false;
  return healthCheckList.value.every((item: HealthCheckItem) => item.status === "ok");
});

export interface ChannelModelGroup {
  channel: ChannelConfig;
  models: string[];
}

const gatewayGroupedModels = computed<ChannelModelGroup[]>(() => {
  const q = gatewaySearchQuery.value.trim().toLowerCase();
  return proxyConfig.value.channels.map((channel) => {
    let models = channel.id === "opencode" ? modelsList.value : [];
    if (q) {
      models = models.filter(
        (m) => m.toLowerCase().includes(q) || `opencode/${m}`.toLowerCase().includes(q)
      );
    }
    return {
      channel,
      models,
    };
  });
});

const totalGatewayModelsCount = computed(() => {
  return gatewayGroupedModels.value.reduce((acc, g) => acc + g.models.length, 0);
});

const filteredChannelModels = computed(() => {
  const q = channelSearchQuery.value.trim().toLowerCase();
  const list = modelsList.value;
  if (!q) return list;
  return list.filter((m) => m.toLowerCase().includes(q) || `opencode/${m}`.toLowerCase().includes(q));
});

const filteredLogs = computed<ProxyRequestLog[]>(() => {
  let list = proxyLogs.value;
  const filter = logStatusFilter.value;
  if (filter === "success") {
    list = list.filter((l) => l.statusCode >= 200 && l.statusCode < 300);
  } else if (filter === "error") {
    list = list.filter((l) => l.statusCode >= 400);
  }

  const q = logSearchQuery.value.trim().toLowerCase();
  if (q) {
    list = list.filter(
      (l) =>
        l.model.toLowerCase().includes(q) ||
        l.path.toLowerCase().includes(q) ||
        String(l.statusCode).includes(q) ||
        l.channelId.toLowerCase().includes(q) ||
        (l.errorMessage && l.errorMessage.toLowerCase().includes(q))
    );
  }
  return list;
});

const logCounts = computed(() => {
  const all = proxyLogs.value.length;
  const success = proxyLogs.value.filter((l) => l.statusCode >= 200 && l.statusCode < 300).length;
  const error = proxyLogs.value.filter((l) => l.statusCode >= 400).length;
  return { all, success, error };
});

function formatNumber(num: number | undefined | null): string {
  if (num === undefined || num === null) return "0";
  return num.toLocaleString();
}

function formatUptime(seconds: number) {
  if (seconds < 60) return `${seconds} 秒`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分钟`;
  const hours = Math.floor(seconds / 3600);
  const mins = Math.floor((seconds % 3600) / 60);
  return `${hours} 小时 ${mins} 分`;
}

async function copyModel(modelId: string) {
  const fullId = `opencode/${modelId}`;
  try {
    await navigator.clipboard.writeText(fullId);
    copiedModelId.value = modelId;
    showToast(`已复制模型 ID: ${fullId}`);
    setTimeout(() => {
      if (copiedModelId.value === modelId) {
        copiedModelId.value = null;
      }
    }, 1500);
  } catch {
    showToast("复制失败", true);
  }
}
</script>

<template>
  <div class="mp-page" role="main">
    <!-- 顶栏标题与快捷控制 -->
    <header class="mp-header">
      <div class="mp-header-left">
        <div class="mp-header-title-row">
          <span class="mp-icon" v-html="icons.repeat" />
          <h1>模型反代</h1>
          <span
            class="mp-status-pill"
            :class="{ active: proxyStatus.running }"
          >
            <span class="mp-status-dot" />
            <span>{{ proxyStatus.running ? "运行中" : "已停止" }}</span>
          </span>
          <span class="mp-channel-pill">多渠道架构</span>
        </div>
        <p class="mp-subtitle">
          本地模型反向代理网关 · 对外提供标准兼容的 OpenAI 和 Anthropic API
        </p>
      </div>

      <div class="mp-header-actions">
        <!-- 服务配置弹窗触发按钮 -->
        <button
          type="button"
          class="mp-btn mp-btn-ghost"
          title="修改反代端口与访问密钥"
          @click="configModalOpen = true"
        >
          <span v-html="icons.settings" />
          <span>服务配置</span>
        </button>

        <!-- 网关可用模型总览弹窗触发按钮 -->
        <button
          type="button"
          class="mp-btn mp-btn-ghost"
          :disabled="fetchingModels"
          title="查看网关聚合的可用模型目录与快速复制"
          @click="handleOpenGatewayModelsModal"
        >
          <span v-html="icons.cpu" />
          <span>{{ fetchingModels ? "加载中…" : "可用模型" }}</span>
          <span v-if="proxyStatus.modelsCount || modelsList.length" class="mp-btn-badge">
            {{ proxyStatus.modelsCount || modelsList.length }}
          </span>
        </button>

        <!-- 健康检查弹窗触发按钮 -->
        <button
          type="button"
          class="mp-btn mp-btn-ghost"
          :disabled="testingHealth"
          title="打开健康检查报告弹窗"
          @click="handleOpenHealthModal"
        >
          <span v-html="icons.pulse" />
          <span>{{ testingHealth ? "测试中…" : "健康检查" }}</span>
        </button>

        <!-- 启动/停止服务按钮 -->
        <button
          type="button"
          class="mp-btn"
          :class="proxyStatus.running ? 'mp-btn-danger' : 'mp-btn-primary'"
          :disabled="togglingServer"
          @click="toggleServer"
        >
          <span v-html="proxyStatus.running ? icons.pause : icons.play" />
          <span>{{ togglingServer ? "操作中…" : (proxyStatus.running ? "停止服务" : "启动服务") }}</span>
        </button>
      </div>
    </header>

    <!-- 一级页面标签导航切换 (Tab Switcher) -->
    <div class="mp-main-tab-nav">
      <button
        type="button"
        class="mp-main-nav-btn"
        :class="{ active: currentMainTab === 'console' }"
        @click="currentMainTab = 'console'"
      >
        <span v-html="icons.activity" />
        <span>反代控制台</span>
      </button>

      <button
        type="button"
        class="mp-main-nav-btn"
        :class="{ active: currentMainTab === 'channels' }"
        @click="currentMainTab = 'channels'"
      >
        <span v-html="icons.shield" />
        <span>反代渠道</span>
        <span class="mp-tab-count-pill font-mono">
          {{ proxyConfig.channels.length }}
        </span>
      </button>

      <button
        type="button"
        class="mp-main-nav-btn"
        :class="{ active: currentMainTab === 'logs' }"
        @click="switchToLogsTab"
      >
        <span v-html="icons.rows" />
        <span>请求调用日志</span>
        <span v-if="logCounts.all > 0" class="mp-tab-count-pill font-mono">
          {{ logCounts.all }}
        </span>
        <span v-if="logCounts.error > 0" class="mp-tab-count-pill is-err font-mono" title="有异常/失败请求">
          {{ logCounts.error }} 异常
        </span>
      </button>
    </div>

    <!-- 选项卡 1: 反代控制台 (Console / Overview) -->
    <div v-if="currentMainTab === 'console'" class="mp-tab-pane">
      <!-- 运行概览与连接参数 -->
      <section class="mp-card mp-overview-card">
        <div class="mp-card-header">
          <div class="mp-card-title-group">
            <span class="mp-card-icon" v-html="icons.activity" />
            <h2>服务连接与状态</h2>
          </div>
          <span class="mp-tag font-mono">http://127.0.0.1:{{ proxyStatus.port || proxyConfig.port }}</span>
        </div>

        <div class="mp-metrics-row">
          <div class="mp-metric-box">
            <label>服务端口</label>
            <strong class="font-mono text-brand">{{ proxyStatus.port || proxyConfig.port }}</strong>
          </div>
          <div class="mp-metric-box">
            <label>累计请求</label>
            <strong class="font-mono">{{ proxyStatus.totalRequests }}</strong>
          </div>
          <div class="mp-metric-box">
            <label>成功 / 失败</label>
            <strong class="font-mono text-success">{{ proxyStatus.successfulRequests }} <span class="text-muted">/</span> <span class="text-danger">{{ proxyStatus.failedRequests }}</span></strong>
          </div>
          <div class="mp-metric-box">
            <label>运行时长</label>
            <strong>{{ proxyStatus.running ? formatUptime(proxyStatus.uptimeSeconds) : '--' }}</strong>
          </div>
        </div>

        <!-- 累计 Token 使用统计 (累加总量) -->
        <div class="mp-metrics-tokens-panel">
          <div class="mp-mtp-head">
            <div class="mp-mtp-title-wrap">
              <span class="mp-mtp-icon">⚡</span>
              <span class="mp-mtp-title">累计 Token 使用统计 (累加总量)</span>
            </div>
            <div class="mp-mtp-total-badge" title="累计所有请求消耗的 Token 总量">
              <span class="text-muted">总计消耗:</span>
              <strong class="font-mono text-brand">{{ formatNumber(proxyStatus.totalTokens || 0) }}</strong>
              <span class="text-muted text-xs">Tokens</span>
            </div>
          </div>

          <div class="mp-mtp-grid">
            <div class="mp-mtp-card is-in" title="累计接收的 Prompt 输入 Token">
              <div class="mp-mtp-card-head">
                <span class="mp-mtp-dot is-in" />
                <span class="mp-mtp-card-label">累计输入</span>
              </div>
              <strong class="mp-mtp-card-val font-mono">{{ formatNumber(proxyStatus.totalPromptTokens || 0) }}</strong>
              <small class="mp-mtp-card-sub text-muted">上下文与提示词输入</small>
            </div>

            <div class="mp-mtp-card is-hit" title="累计命中的前缀缓存 Token（极大节省延迟与算力）">
              <div class="mp-mtp-card-head">
                <span class="mp-mtp-dot is-hit" />
                <span class="mp-mtp-card-label">累计缓存命中</span>
              </div>
              <strong class="mp-mtp-card-val font-mono text-brand">{{ formatNumber(proxyStatus.totalCacheHitTokens || 0) }}</strong>
              <small class="mp-mtp-card-sub">
                <span v-if="(proxyStatus.totalPromptTokens || 0) > 0" class="text-brand font-semibold font-mono">
                  命中率 {{ Math.round(((proxyStatus.totalCacheHitTokens || 0) / (proxyStatus.totalPromptTokens || 1)) * 100) }}%
                </span>
                <span v-else class="text-muted">前缀缓存极速复用</span>
              </small>
            </div>

            <div class="mp-mtp-card is-think" title="累计模型深度思考与思维链 Token">
              <div class="mp-mtp-card-head">
                <span class="mp-mtp-dot is-think" />
                <span class="mp-mtp-card-label">累计思考推理</span>
              </div>
              <strong class="mp-mtp-card-val font-mono">{{ formatNumber(proxyStatus.totalReasoningTokens || 0) }}</strong>
              <small class="mp-mtp-card-sub text-muted">
                <span v-if="(proxyStatus.totalCompletionTokens || 0) > 0" class="font-mono">
                  思维链占比 {{ Math.round(((proxyStatus.totalReasoningTokens || 0) / (proxyStatus.totalCompletionTokens || 1)) * 100) }}%
                </span>
                <span v-else>深度思考推理消耗</span>
              </small>
            </div>

            <div class="mp-mtp-card is-out" title="累计模型回复生成的 Completion 输出 Token">
              <div class="mp-mtp-card-head">
                <span class="mp-mtp-dot is-out" />
                <span class="mp-mtp-card-label">累计生成输出</span>
              </div>
              <strong class="mp-mtp-card-val font-mono">{{ formatNumber(proxyStatus.totalCompletionTokens || 0) }}</strong>
              <small class="mp-mtp-card-sub text-muted">服务端模型回复生成</small>
            </div>
          </div>
        </div>

        <div class="mp-endpoint-list">
          <div class="mp-endpoint-item">
            <span class="mp-ep-label">Base URL</span>
            <code class="mp-ep-code">{{ proxyStatus.url || `http://127.0.0.1:${proxyConfig.port}/v1` }}</code>
            <button
              type="button"
              class="mp-action-btn"
              title="复制 Base URL"
              @click="copyProxyUrl"
            >
              <span v-html="icons.copy" />
              <span>复制</span>
            </button>
          </div>

          <div class="mp-endpoint-item">
            <span class="mp-ep-label">API Key</span>
            <code class="mp-ep-code">
              {{ showKey ? (proxyConfig.apiKey || '(未配置密钥，免密直接访问)') : (proxyConfig.apiKey ? '••••••••••••••••••••' : '(未配置密钥，免密直接访问)') }}
            </code>
            <div class="mp-ep-btns">
              <button
                v-if="proxyConfig.apiKey"
                type="button"
                class="mp-action-btn mp-btn-icon-only"
                :title="showKey ? '隐藏密钥' : '显示密钥'"
                @click="showKey = !showKey"
              >
                <span v-html="showKey ? icons.eyeOff : icons.eye" />
              </button>
              <button
                type="button"
                class="mp-action-btn"
                title="复制 API Key"
                @click="copyProxyKey"
              >
                <span v-html="icons.copy" />
                <span>复制</span>
              </button>
            </div>
          </div>
        </div>
      </section>
    </div>

    <!-- 选项卡 2: 反代上游渠道 (Channels Matrix) -->
    <div v-else-if="currentMainTab === 'channels'" class="mp-tab-pane">
      <div class="mp-section-head">
        <div class="mp-card-title-group">
          <span class="mp-card-icon" v-html="icons.shield" />
          <h2>反代上游渠道 ({{ proxyConfig.channels.length }})</h2>
        </div>
        <small class="text-muted">独立管理各个上游反代通道与内部代理池轮询</small>
      </div>

      <div class="mp-channels-grid">
        <!-- 渠道卡片 -->
        <div
          v-for="channel in proxyConfig.channels"
          :key="channel.id"
          class="mp-channel-card"
          :class="{ 'is-disabled': !channel.enabled }"
        >
          <div class="mp-channel-card-head">
            <div class="mp-channel-card-title">
              <div class="mp-channel-badge-icon">
                <span v-html="icons.cpu" />
              </div>
              <div>
                <h3>{{ channel.name }}</h3>
                <span class="mp-proto-tag">{{ channel.protocol.toUpperCase() }} 协议</span>
              </div>
            </div>

            <label class="mp-switch-wrap" :title="channel.enabled ? '点击禁用该渠道' : '点击启用该渠道'">
              <input
                v-model="channel.enabled"
                type="checkbox"
                @change="handleChannelSave(channel)"
              />
              <span class="mp-switch-round" />
            </label>
          </div>

          <p class="mp-channel-desc">
            {{ channel.description }}
          </p>

          <!-- 内部代理池轮询配置项 -->
          <div class="mp-proxy-pool-box">
            <div class="mp-proxy-pool-row">
              <div class="mp-proxy-pool-label">
                <span class="mp-pp-icon" v-html="icons.repeat" />
                <span>内部代理池轮询</span>
              </div>
              <label class="mp-switch-wrap" :title="channel.useProxyPool ? '点击关闭代理池轮询' : '点击开启内部代理池轮询'">
                <input
                  v-model="channel.useProxyPool"
                  type="checkbox"
                  @change="handleChannelSave(channel)"
                />
                <span class="mp-switch-round" />
              </label>
            </div>

            <div v-if="channel.useProxyPool" class="mp-proxy-pool-status is-active">
              <span class="mp-status-dot-sm" />
              <span>优先直连，报错自动按速度切换至代理池 <strong>≤ 1000ms</strong> 节点（粘性保持）</span>
            </div>
            <div v-else class="mp-proxy-pool-status is-inactive">
              <span>当前网络模式：直接连接 (直连上游通道)</span>
            </div>
          </div>

          <div class="mp-channel-card-footer">
            <div class="mp-channel-stat">
              <span>在线可用模型</span>
              <strong class="font-mono text-brand">{{ modelsList.length }} 个</strong>
            </div>
            <button
              type="button"
              class="mp-action-btn"
              title="查看此渠道的模型目录"
              @click="handleOpenChannelModelsModal(channel)"
            >
              <span v-html="icons.search" />
              <span>查看模型</span>
            </button>
          </div>
        </div>
      </div>
    </div>

    <!-- 选项卡 3: 请求调用日志全景 (In-page Full View) -->
    <div v-else-if="currentMainTab === 'logs'" class="mp-tab-pane mp-logs-page-view">
      <!-- 顶部固化数据库状态指示与操作栏 -->
      <div class="mp-logs-summary-bar">
        <div class="mp-lsb-left">
          <div class="mp-lsb-badge-group">
            <span class="mp-lsb-pill" title="所有日志已实时持久化到本地 SQLite 数据库，软件重启不丢失">
              <span class="mp-lsb-dot" />
              <span>已固化存储至本地 SQLite 数据库</span>
            </span>
            <span class="mp-header-chip font-mono">共 {{ proxyLogs.length }} 条记录</span>
            <span v-if="logCounts.error > 0" class="mp-header-chip is-danger font-mono">{{ logCounts.error }} 条异常</span>
          </div>
        </div>

        <div class="mp-logs-actions">
          <button
            type="button"
            class="mp-btn mp-btn-ghost mp-btn-sm"
            :disabled="loadingLogs"
            title="从本地数据库刷新最新日志"
            @click="fetchLogs"
          >
            <span :class="{ 'mp-spin': loadingLogs }" v-html="icons.restore" />
            <span>{{ loadingLogs ? "刷新中…" : "刷新日志" }}</span>
          </button>
          <button
            type="button"
            class="mp-btn mp-btn-ghost mp-btn-sm text-danger"
            :disabled="proxyLogs.length === 0"
            title="清理本地 SQLite 数据库中的反代请求日志"
            @click="clearLogsModalOpen = true"
          >
            <span v-html="icons.trash" />
            <span>清空数据库日志</span>
          </button>
        </div>
      </div>

      <!-- 请求日志主卡片 -->
      <div class="mp-card mp-logs-main-card">
        <!-- 筛选与操作工具栏 -->
        <div class="mp-logs-toolbar">
          <div class="mp-log-filter-tabs">
            <button
              type="button"
              class="mp-log-tab-btn"
              :class="{ active: logStatusFilter === 'all' }"
              @click="logStatusFilter = 'all'"
            >
              全部 ({{ logCounts.all }})
            </button>
            <button
              type="button"
              class="mp-log-tab-btn"
              :class="{ active: logStatusFilter === 'success' }"
              @click="logStatusFilter = 'success'"
            >
              成功 ({{ logCounts.success }})
            </button>
            <button
              type="button"
              class="mp-log-tab-btn"
              :class="{ active: logStatusFilter === 'error' }"
              @click="logStatusFilter = 'error'"
            >
              异常/失败 ({{ logCounts.error }})
            </button>
          </div>

          <div class="mp-search-box flex-1">
            <span v-html="icons.search" />
            <input
              v-model="logSearchQuery"
              type="search"
              placeholder="搜索模型名称、请求路径、HTTP状态码或错误提示…"
              class="mp-search-input-lg"
            />
            <button
              v-if="logSearchQuery"
              type="button"
              class="mp-search-clear-btn"
              title="清空搜索"
              @click="logSearchQuery = ''"
            >
              <span v-html="icons.close" />
            </button>
          </div>
        </div>

        <!-- 请求日志表格 -->
        <div class="mp-logs-table-wrap">
          <table class="mp-logs-table">
            <thead>
              <tr>
                <th style="width: 105px;">请求时间</th>
                <th style="width: 135px;">方法与路径</th>
                <th style="width: 180px;">渠道 / 模型</th>
                <th style="width: 125px;">出网节点</th>
                <th style="width: 60px;">模式</th>
                <th style="width: 70px;">状态</th>
                <th style="min-width: 175px;">Token 统计</th>
                <th style="width: 90px;">耗时</th>
                <th style="width: 65px; text-align: center;">操作</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="log in filteredLogs"
                :key="log.id"
                class="mp-log-row"
                :class="{ 'has-error': log.statusCode >= 400 }"
                @click="openLogDetail(log)"
              >
                <td>
                  <div class="mp-log-time-col font-mono">
                    <span class="mp-log-date">{{ log.timestamp ? log.timestamp.split(' ')[0] : '--' }}</span>
                    <strong class="mp-log-time">{{ log.timestamp && log.timestamp.split(' ')[1] ? log.timestamp.split(' ')[1] : log.timestamp }}</strong>
                  </div>
                </td>
                <td>
                  <div class="mp-log-method-path">
                    <span class="mp-method-tag" :class="`method-${log.method.toLowerCase()}`">{{ log.method }}</span>
                    <code class="mp-path-code">{{ log.path }}</code>
                  </div>
                </td>
                <td>
                  <div class="mp-log-model-col">
                    <div class="mp-log-chan-row">
                      <span class="mp-proto-tag">{{ log.channelId.toUpperCase() }}</span>
                    </div>
                    <strong class="mp-log-model-name font-mono" :title="log.model">{{ log.model }}</strong>
                  </div>
                </td>
                <td>
                  <div class="mp-log-node-wrap">
                    <span
                      class="mp-node-pill font-mono"
                      :class="log.nodeName && log.nodeName !== '直连通道' ? 'is-proxy' : 'is-direct'"
                      :title="log.nodeName ? `出网网络节点：${log.nodeName}` : '出网网络：直连通道'"
                    >
                      <span class="mp-node-dot" />
                      <span class="truncate">{{ log.nodeName || '直连通道' }}</span>
                    </span>
                  </div>
                </td>
                <td>
                  <span class="mp-stream-tag" :class="{ 'is-stream': log.stream }">
                    {{ log.stream ? "SSE" : "JSON" }}
                  </span>
                </td>
                <td>
                  <span
                    class="mp-status-tag"
                    :class="log.statusCode >= 200 && log.statusCode < 300 ? 'tag-ok' : 'tag-err'"
                  >
                    <span class="mp-status-dot-sm" />
                    <span>{{ log.statusCode }}</span>
                  </span>
                </td>
                <td>
                  <!-- Token 统计列：输入 / 缓存 / 输出 / 思考，两行两列等宽卡片 -->
                  <div v-if="log.promptTokens !== undefined || log.completionTokens !== undefined" class="mp-log-tokens-cell font-mono">
                    <div class="mp-token-pill-row">
                      <span class="mp-token-tag is-in" :title="`输入 Token: ${log.promptTokens ?? 0}`">
                        <span>输入</span>
                        <strong>{{ formatCompactToken(log.promptTokens) }}</strong>
                      </span>
                      <span v-if="log.promptCacheHitTokens" class="mp-token-tag is-hit" :title="`缓存命中: ${log.promptCacheHitTokens}`">
                        <span>缓存</span>
                        <strong>{{ formatCompactToken(log.promptCacheHitTokens) }}</strong>
                      </span>
                    </div>
                    <div class="mp-token-pill-row">
                      <span class="mp-token-tag is-out" :title="`输出 Token: ${log.completionTokens ?? 0}`">
                        <span>输出</span>
                        <strong>{{ formatCompactToken(log.completionTokens) }}</strong>
                      </span>
                      <span v-if="log.reasoningTokens" class="mp-token-tag is-think" :title="`思考推理: ${log.reasoningTokens}`">
                        <span>思考</span>
                        <strong>{{ formatCompactToken(log.reasoningTokens) }}</strong>
                      </span>
                    </div>
                  </div>
                  <span v-else class="text-muted text-xs font-mono">--</span>
                </td>
                <td>
                  <div class="mp-dur-col font-mono text-xs">
                    <div class="mp-dur-row">
                      <span class="mp-dur-label">首:</span>
                      <span class="mp-dur-val text-brand">{{ log.ttftMs ? formatSec(log.ttftMs) : '--' }}</span>
                    </div>
                    <div class="mp-dur-row">
                      <span class="mp-dur-label">总:</span>
                      <span class="mp-dur-val" :class="log.durationMs > 2000 ? 'text-warn font-semibold' : 'text-text font-semibold'">{{ formatSec(log.durationMs) }}</span>
                    </div>
                  </div>
                </td>
                <td style="text-align: center;">
                  <button
                    type="button"
                    class="mp-btn-text"
                    title="查看该条请求详情与全文报文"
                    @click.stop="openLogDetail(log)"
                  >
                    详情
                  </button>
                </td>
              </tr>

              <tr v-if="filteredLogs.length === 0">
                <td colspan="9" class="text-center py-10 text-muted">
                  <div class="mp-empty-box">
                    <div class="mp-empty-icon" v-html="icons.rows" />
                    <p v-if="loadingLogs">正在读取本地数据库请求日志…</p>
                    <p v-else-if="proxyLogs.length > 0">未检索到匹配的请求记录</p>
                    <p v-else>暂无反代调用记录 · 当有客户端发起 API 请求并完结后将持久化保存并显示在这里</p>
                  </div>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>
    </div>

    <!-- 服务配置弹出框 (Modal Dialog) -->
    <div
      v-if="configModalOpen"
      class="mp-modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-labelledby="mp-config-modal-title"
      @click.self="configModalOpen = false"
    >
      <div class="mp-modal-box mp-modal-box-sm">
        <div class="mp-modal-header">
          <div class="mp-modal-title-group">
            <span class="mp-modal-icon" v-html="icons.settings" />
            <div>
              <h3 id="mp-config-modal-title">反代服务配置</h3>
              <small class="text-muted">设置本地反代端口与访问密钥</small>
            </div>
          </div>
          <button
            type="button"
            class="mp-modal-close"
            title="关闭弹窗 (Esc)"
            @click="configModalOpen = false"
          >
            <span v-html="icons.close" />
          </button>
        </div>

        <form class="mp-modal-body" @submit.prevent="handleSave">
          <div class="mp-field">
            <label for="mp-port">监听端口</label>
            <input
              id="mp-port"
              v-model.number="proxyConfig.port"
              type="number"
              min="1024"
              max="65535"
              class="mp-input font-mono"
              placeholder="8088"
              required
            />
            <small>本地独立绑定的 HTTP 服务端口</small>
          </div>

          <div class="mp-field">
            <label for="mp-apikey">访问密钥</label>
            <input
              id="mp-apikey"
              v-model="proxyConfig.apiKey"
              type="text"
              class="mp-input font-mono"
              placeholder="留空表示免密访问，或自定义如 sk-proxy"
            />
            <small>客户端调用反代服务时的本地 Bearer Key 校验（留空则免密）</small>
          </div>

          <!-- 记录请求全文开关 (默认关闭) -->
          <div class="mp-field">
            <div class="mp-proxy-pool-row" style="padding: 0;">
              <label for="mp-record-body" style="font-size: 13px; font-weight: 600; color: var(--text); cursor: pointer;">记录请求全文</label>
              <label class="mp-switch-wrap" :title="proxyConfig.recordRequestBody ? '点击关闭请求全文记录' : '点击开启请求全文记录'">
                <input
                  id="mp-record-body"
                  v-model="proxyConfig.recordRequestBody"
                  type="checkbox"
                />
                <span class="mp-switch-round" />
              </label>
            </div>
            <small>开启后在请求日志中保存客户端传入的完整 JSON 报文（默认关闭以节省内存）</small>
          </div>
        </form>

        <div class="mp-modal-footer">
          <button
            type="button"
            class="mp-btn mp-btn-ghost"
            @click="configModalOpen = false"
          >
            取消
          </button>
          <button
            type="button"
            class="mp-btn mp-btn-primary"
            :disabled="savingConfig"
            @click="handleSave"
          >
            <span v-html="icons.check" />
            <span>{{ savingConfig ? "保存中…" : "保存并应用" }}</span>
          </button>
        </div>
      </div>
    </div>

    <!-- 请求日志详情弹窗 (Request Log Detail Modal with Token Analytics & Full Payloads) -->
    <div
      v-if="selectedLogForDetail"
      class="mp-modal-backdrop mp-sub-backdrop"
      role="dialog"
      aria-modal="true"
      aria-labelledby="mp-log-detail-title"
      @click.self="closeLogDetail"
    >
      <div class="mp-modal-box mp-modal-box-extra-wide mp-log-detail-box">
        <div class="mp-modal-header">
          <div class="mp-modal-title-group">
            <div class="mp-modal-badge-icon" :class="selectedLogForDetail.statusCode >= 400 ? 'is-error' : 'is-success'">
              <span v-html="selectedLogForDetail.statusCode >= 400 ? icons.alert : icons.check" />
            </div>
            <div>
              <div class="mp-modal-title-wrap">
                <h3 id="mp-log-detail-title">请求详情与 Token 全面分析</h3>
                <span
                  class="mp-status-tag"
                  :class="selectedLogForDetail.statusCode >= 200 && selectedLogForDetail.statusCode < 300 ? 'tag-ok' : 'tag-err'"
                >
                  <span class="mp-status-dot-sm" />
                  <span>HTTP {{ selectedLogForDetail.statusCode }}</span>
                </span>
                <span class="mp-header-chip font-mono">{{ selectedLogForDetail.durationMs }}ms</span>
                <span v-if="selectedLogForDetail.ttftMs" class="mp-header-chip font-mono">首字 {{ selectedLogForDetail.ttftMs }}ms</span>
                <span class="mp-header-chip font-mono" :class="selectedLogForDetail.nodeName && selectedLogForDetail.nodeName !== '直连通道' ? 'is-proxy-chip' : ''">🌐 {{ selectedLogForDetail.nodeName || '直连通道' }}</span>
                <span v-if="selectedLogForDetail.stream" class="mp-stream-tag is-stream">SSE 实时流</span>
              </div>
              <small class="text-muted">请求 ID: {{ selectedLogForDetail.id }} · 记录时间: {{ selectedLogForDetail.timestamp }}</small>
            </div>
          </div>
          <button
            type="button"
            class="mp-modal-close"
            title="关闭详情 (Esc)"
            @click="closeLogDetail"
          >
            <span v-html="icons.close" />
          </button>
        </div>

        <div class="mp-modal-body">
          <!-- 4 宫格 Token 消耗与缓存命中仪表盘 -->
          <div class="mp-token-dashboard-grid">
            <!-- 卡片 1: 输入 Tokens -->
            <div class="mp-token-card is-in">
              <div class="mp-tc-head">
                <span class="mp-tc-label">📥 输入 Token</span>
                <span class="mp-tc-badge" :class="(selectedLogForDetail.promptCacheHitTokens || 0) > 0 ? 'badge-hit' : ''">
                  {{ (selectedLogForDetail.promptCacheHitTokens || 0) > 0 ? `⚡ 命中率 ${getCacheHitRate(selectedLogForDetail)}%` : '未命中前缀' }}
                </span>
              </div>
              <div class="mp-tc-value font-mono">
                {{ selectedLogForDetail.promptTokens ?? 0 }}
              </div>
              <div class="mp-tc-foot font-mono">
                <span>⚡ 命中: <strong>{{ selectedLogForDetail.promptCacheHitTokens ?? 0 }}</strong></span>
                <span class="mp-tc-divider">·</span>
                <span>新增: <strong>{{ selectedLogForDetail.promptCacheMissTokens ?? ((selectedLogForDetail.promptTokens ?? 0) - (selectedLogForDetail.promptCacheHitTokens ?? 0)) }}</strong></span>
              </div>
            </div>

            <!-- 卡片 2: 缓存命中 Tokens -->
            <div class="mp-token-card is-hit">
              <div class="mp-tc-head">
                <span class="mp-tc-label">⚡ 缓存命中</span>
                <span class="mp-tc-badge badge-hit">前缀缓存复用</span>
              </div>
              <div class="mp-tc-value font-mono text-brand">
                {{ selectedLogForDetail.promptCacheHitTokens ?? 0 }}
              </div>
              <div class="mp-tc-foot">
                <span v-if="(selectedLogForDetail.promptCacheHitTokens || 0) > 0" class="text-success font-semibold">
                  ✓ 已复用前缀缓存，节省计算
                </span>
                <span v-else class="text-muted">首轮请求或上下文前缀未命中</span>
              </div>
            </div>

            <!-- 卡片 3: 思考/推理 Tokens -->
            <div class="mp-token-card is-think">
              <div class="mp-tc-head">
                <span class="mp-tc-label">🧠 思考推理</span>
                <span class="mp-tc-badge">深度思考</span>
              </div>
              <div class="mp-tc-value font-mono" style="color: #8b5cf6;">
                {{ selectedLogForDetail.reasoningTokens ?? 0 }}
              </div>
              <div class="mp-tc-foot font-mono">
                <span>思维链占比: <strong>{{ getReasoningRate(selectedLogForDetail) }}%</strong></span>
              </div>
            </div>

            <!-- 卡片 4: 输出 Tokens -->
            <div class="mp-token-card is-out">
              <div class="mp-tc-head">
                <span class="mp-tc-label">📤 生成输出</span>
                <span class="mp-tc-badge">总计 {{ selectedLogForDetail.totalTokens ?? ((selectedLogForDetail.promptTokens || 0) + (selectedLogForDetail.completionTokens || 0)) }} Token</span>
              </div>
              <div class="mp-tc-value font-mono" style="color: #10b981;">
                {{ selectedLogForDetail.completionTokens ?? 0 }}
              </div>
              <div class="mp-tc-foot font-mono">
                <span>生成速率: <strong>~{{ getEstimatedTps(selectedLogForDetail) }}</strong> Token/秒</span>
                <span v-if="selectedLogForDetail.ttftMs" class="mp-tc-divider">·</span>
                <span v-if="selectedLogForDetail.ttftMs">首字 <strong>{{ selectedLogForDetail.ttftMs }}ms</strong></span>
              </div>
            </div>
          </div>

          <!-- 选项卡导航栏 (Tabs) -->
          <div class="mp-detail-tabs-bar">
            <button
              v-if="selectedLogForDetail.statusCode >= 400 || selectedLogForDetail.errorMessage"
              type="button"
              class="mp-detail-tab-btn is-error"
              :class="{ active: detailActiveTab === 'error' }"
              @click="detailActiveTab = 'error'"
            >
              <span v-html="icons.alert" />
              <span>⚠️ 错误诊断</span>
            </button>

            <button
              type="button"
              class="mp-detail-tab-btn"
              :class="{ active: detailActiveTab === 'tokens' }"
              @click="detailActiveTab = 'tokens'"
            >
              <span v-html="icons.chart" />
              <span>📊 Token 与网络概览</span>
            </button>

            <button
              type="button"
              class="mp-detail-tab-btn"
              :class="{ active: detailActiveTab === 'request' }"
              @click="detailActiveTab = 'request'"
            >
              <span v-html="icons.code" />
              <span>📝 客户端请求全文</span>
              <span v-if="selectedLogForDetail.requestBody" class="mp-tab-badge font-mono">{{ selectedLogForDetail.requestBody.length }}B</span>
            </button>

            <button
              type="button"
              class="mp-detail-tab-btn"
              :class="{ active: detailActiveTab === 'response' }"
              @click="detailActiveTab = 'response'"
            >
              <span v-html="icons.message" />
              <span>💬 响应全文 / 思考过程</span>
              <span v-if="selectedLogForDetail.responseBody" class="mp-tab-badge font-mono">{{ selectedLogForDetail.responseBody.length }}B</span>
            </button>

            <button
              type="button"
              class="mp-detail-tab-btn"
              :class="{ active: detailActiveTab === 'meta' }"
              @click="detailActiveTab = 'meta'"
            >
              <span v-html="icons.settings" />
              <span>🔍 路由与调用元数据</span>
            </button>
          </div>

          <!-- 选项卡内容 1: 错误诊断 -->
          <div v-if="detailActiveTab === 'error'" class="mp-detail-tab-content">
            <div class="mp-log-error-banner">
              <div class="mp-leb-icon" v-html="icons.alert" />
              <div class="mp-leb-content">
                <div class="mp-leb-title">
                  <strong>错误原因分析 (HTTP {{ selectedLogForDetail.statusCode }})</strong>
                </div>
                <p class="mp-leb-reason">{{ selectedLogForDetail.errorMessage || '上游返回非 200 异常状态' }}</p>
                <div class="mp-leb-suggestion">
                  <span class="mp-leb-tag">💡 排查建议</span>
                  <span>{{ getErrorSuggestion(selectedLogForDetail) }}</span>
                </div>
              </div>
            </div>

            <div v-if="selectedLogForDetail.responseBody" class="mp-log-raw-box">
              <div class="mp-lrb-header">
                <label>上游原始错误响应报文</label>
                <button
                  type="button"
                  class="mp-action-btn"
                  title="复制原始错误报文"
                  @click="copyText(selectedLogForDetail.responseBody, '错误响应报文')"
                >
                  <span v-html="icons.copy" />
                  <span>复制</span>
                </button>
              </div>
              <pre class="mp-lrb-pre font-mono">{{ selectedLogForDetail.responseBody }}</pre>
            </div>
          </div>

          <!-- 选项卡内容 2: Token 与网络概览 -->
          <div v-if="detailActiveTab === 'tokens'" class="mp-detail-tab-content">
            <div class="mp-log-detail-grid">
              <div class="mp-ld-item">
                <label>输入 Token</label>
                <div class="mp-ld-val font-mono">
                  <span>{{ selectedLogForDetail.promptTokens ?? 0 }} Tokens</span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>⚡ 缓存命中 Token</label>
                <div class="mp-ld-val font-mono text-brand">
                  <span>{{ selectedLogForDetail.promptCacheHitTokens ?? 0 }} Tokens ({{ getCacheHitRate(selectedLogForDetail) }}%)</span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>🧠 思考推理 Token</label>
                <div class="mp-ld-val font-mono" style="color: #8b5cf6;">
                  <span>{{ selectedLogForDetail.reasoningTokens ?? 0 }} Tokens ({{ getReasoningRate(selectedLogForDetail) }}%)</span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>📤 输出生成 Token</label>
                <div class="mp-ld-val font-mono text-success">
                  <span>{{ selectedLogForDetail.completionTokens ?? 0 }} Tokens</span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>首字响应耗时</label>
                <div class="mp-ld-val font-mono text-brand">
                  <span>{{ selectedLogForDetail.ttftMs ? `${selectedLogForDetail.ttftMs} ms` : '同步即达' }}</span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>响应总耗时</label>
                <div class="mp-ld-val font-mono">
                  <span>{{ selectedLogForDetail.durationMs }} ms</span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>总 Token 消耗</label>
                <div class="mp-ld-val font-mono">
                  <strong>{{ selectedLogForDetail.totalTokens ?? ((selectedLogForDetail.promptTokens || 0) + (selectedLogForDetail.completionTokens || 0)) }} Tokens</strong>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>平均生成速率</label>
                <div class="mp-ld-val font-mono">
                  <span>~{{ getEstimatedTps(selectedLogForDetail) }} Token/秒</span>
                </div>
              </div>
            </div>
          </div>

          <!-- 选项卡内容 3: 请求全文 -->
          <div v-if="detailActiveTab === 'request'" class="mp-detail-tab-content">
            <div class="mp-log-raw-box">
              <div class="mp-lrb-header">
                <div class="flex-center-start gap-2">
                  <label>客户端完整请求报文</label>
                  <span class="mp-stream-tag" :class="{ 'is-stream': selectedLogForDetail.stream }">
                    {{ selectedLogForDetail.stream ? "流式传输" : "同步响应" }}
                  </span>
                </div>
                <button
                  v-if="selectedLogForDetail.requestBody"
                  type="button"
                  class="mp-action-btn"
                  title="一键复制客户端完整请求报文"
                  @click="copyText(selectedLogForDetail.requestBody, '请求全文')"
                >
                  <span v-html="icons.copy" />
                  <span>复制全文</span>
                </button>
              </div>
              <pre class="mp-lrb-pre font-mono">{{ selectedLogForDetail.requestBody || '// 未开启「记录请求全文」开关\n// 默认不保存请求全文以节省内存与存储空间。如需在日志中查看客户端发送的完整请求 JSON，请在右上角「服务配置」中开启「记录请求全文」开关。' }}</pre>
            </div>
          </div>

          <!-- 选项卡内容 4: 响应全文 / 思考链 -->
          <div v-if="detailActiveTab === 'response'" class="mp-detail-tab-content">
            <div class="mp-log-raw-box">
              <div class="mp-lrb-header">
                <label>服务端响应内容与思考过程</label>
                <button
                  v-if="selectedLogForDetail.responseBody"
                  type="button"
                  class="mp-action-btn"
                  title="一键复制服务端响应全文"
                  @click="copyText(selectedLogForDetail.responseBody, '响应全文')"
                >
                  <span v-html="icons.copy" />
                  <span>复制全文</span>
                </button>
              </div>
              <pre class="mp-lrb-pre font-mono">{{ selectedLogForDetail.responseBody || '未捕获或流式尚未产生内容' }}</pre>
            </div>
          </div>

          <!-- 选项卡内容 5: 元数据与参数 -->
          <div v-if="detailActiveTab === 'meta'" class="mp-detail-tab-content">
            <div class="mp-log-detail-grid">
              <div class="mp-ld-item">
                <label>请求方法与路径</label>
                <div class="mp-ld-val">
                  <span class="mp-method-tag" :class="`method-${selectedLogForDetail.method.toLowerCase()}`">{{ selectedLogForDetail.method }}</span>
                  <code class="font-mono">{{ selectedLogForDetail.path }}</code>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>上游渠道与模型</label>
                <div class="mp-ld-val">
                  <span class="mp-proto-tag">{{ selectedLogForDetail.channelId.toUpperCase() }}</span>
                  <strong class="font-mono">{{ selectedLogForDetail.model }}</strong>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>出网通道 / 节点</label>
                <div class="mp-ld-val">
                  <span
                    class="mp-node-pill font-mono"
                    :class="selectedLogForDetail.nodeName && selectedLogForDetail.nodeName !== '直连通道' ? 'is-proxy' : 'is-direct'"
                  >
                    <span class="mp-node-dot" />
                    <span>{{ selectedLogForDetail.nodeName || '直连通道' }}</span>
                  </span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>响应耗时</label>
                <div class="mp-ld-val font-mono">
                  <span>{{ selectedLogForDetail.durationMs }} ms</span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>传输协议模式</label>
                <div class="mp-ld-val">
                  <span>{{ selectedLogForDetail.stream ? "SSE 实时流式传输" : "标准 JSON 同步响应" }}</span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>请求唯一标识</label>
                <div class="mp-ld-val font-mono text-muted text-xs">
                  <span>{{ selectedLogForDetail.id }}</span>
                </div>
              </div>

              <div class="mp-ld-item">
                <label>请求时间戳</label>
                <div class="mp-ld-val font-mono text-muted">
                  <span>{{ selectedLogForDetail.timestamp }}</span>
                </div>
              </div>
            </div>
          </div>
        </div>

        <div class="mp-modal-footer">
          <div class="mp-modal-footer-hint">
            <span>支持使用键盘 <kbd>Esc</kbd> 快速关闭详情</span>
          </div>
          <button
            type="button"
            class="mp-btn mp-btn-primary"
            @click="closeLogDetail"
          >
            确定
          </button>
        </div>
      </div>
    </div>

    <!-- 弹窗 1: 顶栏「可用模型」- 反代网关对外聚合可用模型总览 (Gateway Models Modal) -->
    <div
      v-if="gatewayModelsModalOpen"
      class="mp-modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-labelledby="mp-gateway-models-title"
      @click.self="closeGatewayModelsModal"
    >
      <div class="mp-modal-box mp-modal-box-wide">
        <div class="mp-modal-header">
          <div class="mp-modal-title-group">
            <div class="mp-modal-badge-icon">
              <span v-html="icons.cpu" />
            </div>
            <div>
              <div class="mp-modal-title-wrap">
                <h3 id="mp-gateway-models-title">反代网关可用模型总览</h3>
                <span class="mp-header-chip">{{ totalGatewayModelsCount }} 个就绪模型</span>
              </div>
              <small class="text-muted">网关聚合对外提供的模型目录 · 点击模型或复制按钮即可一键复制调用 ID</small>
            </div>
          </div>
          <button
            type="button"
            class="mp-modal-close"
            title="关闭弹窗 (Esc)"
            @click="closeGatewayModelsModal"
          >
            <span v-html="icons.close" />
          </button>
        </div>

        <div class="mp-modal-body">
          <div class="mp-models-modal-toolbar">
            <div class="mp-search-box flex-1">
              <span v-html="icons.search" />
              <input
                v-model="gatewaySearchQuery"
                type="search"
                placeholder="搜索模型名称 (如 deepseek, nemotron, mimo, laguna)…"
                class="mp-search-input-lg"
              />
              <button
                v-if="gatewaySearchQuery"
                type="button"
                class="mp-search-clear-btn"
                title="清空搜索"
                @click="gatewaySearchQuery = ''"
              >
                <span v-html="icons.close" />
              </button>
            </div>
            <button
              type="button"
              class="mp-btn mp-btn-ghost"
              :disabled="fetchingModels"
              title="重新拉取并更新网关模型列表"
              @click="refreshModels"
            >
              <span :class="{ 'mp-spin': fetchingModels }" v-html="icons.restore" />
              <span>{{ fetchingModels ? "正在拉取…" : "刷新列表" }}</span>
            </button>
          </div>

          <!-- 按渠道分组展示模型矩阵 -->
          <div class="mp-channel-groups-container">
            <div
              v-for="group in gatewayGroupedModels"
              :key="group.channel.id"
              class="mp-channel-group-block"
            >
              <!-- 分组头部栏 -->
              <div class="mp-group-header">
                <div class="mp-group-title-row">
                  <div class="mp-group-icon">
                    <span v-html="icons.shield" />
                  </div>
                  <div class="mp-group-name-wrap">
                    <h4 class="mp-group-name">{{ group.channel.name }} 渠道</h4>
                    <span class="mp-proto-tag">{{ group.channel.protocol.toUpperCase() }} 协议</span>
                    <span class="mp-group-count-badge">{{ group.models.length }} 个模型</span>
                  </div>
                </div>
                <div class="mp-group-meta">
                  <span class="mp-group-endpoint font-mono">{{ group.channel.upstreamUrl }}</span>
                  <span
                    class="mp-status-pill mp-status-pill-xs"
                    :class="{ active: group.channel.enabled }"
                  >
                    <span class="mp-status-dot" />
                    <span>{{ group.channel.enabled ? '已启用' : '已禁用' }}</span>
                  </span>
                </div>
              </div>

              <!-- 属于该渠道的模型卡片矩阵 -->
              <div v-if="group.models.length > 0" class="mp-model-cards-grid">
                <div
                  v-for="model in group.models"
                  :key="model"
                  class="mp-model-elegant-card"
                  :class="{ 'is-copied': copiedModelId === model }"
                  @click="copyModel(model)"
                >
                  <div class="mp-mec-left">
                    <div class="mp-mec-title-row">
                      <span class="mp-model-free-badge">免费</span>
                      <span class="mp-model-name-title">{{ model }}</span>
                    </div>
                    <div class="mp-mec-id-row">
                      <span class="mp-mec-id-label">调用 ID:</span>
                      <code class="mp-mec-id-code">opencode/{{ model }}</code>
                    </div>
                  </div>

                  <div class="mp-mec-right">
                    <button
                      type="button"
                      class="mp-copy-action-btn"
                      :class="{ 'copied': copiedModelId === model }"
                      :title="`复制 opencode/${model}`"
                      @click.stop="copyModel(model)"
                    >
                      <span v-html="copiedModelId === model ? icons.check : icons.copy" />
                      <span>{{ copiedModelId === model ? '已复制' : '复制 ID' }}</span>
                    </button>
                  </div>
                </div>
              </div>

              <div v-else class="mp-group-empty-note text-muted text-xs">
                <span v-if="!group.channel.enabled">该渠道当前已被禁用</span>
                <span v-else-if="gatewaySearchQuery">未检索到匹配的模型</span>
                <span v-else>暂无就绪模型</span>
              </div>
            </div>
          </div>

          <div v-if="totalGatewayModelsCount === 0" class="mp-empty-box">
            <div class="mp-empty-icon" v-html="icons.cpu" />
            <p v-if="fetchingModels">正在从各渠道上游拉取可用模型列表…</p>
            <p v-else>暂无匹配的可用模型</p>
          </div>
        </div>

        <div class="mp-modal-footer">
          <div class="mp-modal-footer-hint text-muted text-xs">
            <span>💡 提示：在客户端工具中输入调用 ID 即可发起调用</span>
          </div>
          <button
            type="button"
            class="mp-btn mp-btn-primary"
            @click="closeGatewayModelsModal"
          >
            完成
          </button>
        </div>
      </div>
    </div>

    <!-- 弹窗 2: 渠道卡片「查看模型」- 单个渠道专属模型目录 (Channel Models Modal) -->
    <div
      v-if="channelModelsModalOpen"
      class="mp-modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-labelledby="mp-channel-models-title"
      @click.self="closeChannelModelsModal"
    >
      <div class="mp-modal-box mp-modal-box-wide">
        <div class="mp-modal-header">
          <div class="mp-modal-title-group">
            <div class="mp-modal-badge-icon">
              <span v-html="icons.shield" />
            </div>
            <div>
              <div class="mp-modal-title-wrap">
                <h3 id="mp-channel-models-title">{{ selectedChannel?.name || 'OpenCode' }} · 上游模型目录</h3>
                <span class="mp-header-chip">{{ filteredChannelModels.length }} 个在线模型</span>
                <span class="mp-header-endpoint-chip font-mono">{{ selectedChannel?.upstreamUrl }}</span>
              </div>
              <small class="text-muted">上游 Public 免费通道支持的在线模型 · 自动映射为网关调用 ID</small>
            </div>
          </div>
          <button
            type="button"
            class="mp-modal-close"
            title="关闭弹窗 (Esc)"
            @click="closeChannelModelsModal"
          >
            <span v-html="icons.close" />
          </button>
        </div>

        <div class="mp-modal-body">
          <div class="mp-models-modal-toolbar">
            <div class="mp-search-box flex-1">
              <span v-html="icons.search" />
              <input
                v-model="channelSearchQuery"
                type="search"
                placeholder="搜索此渠道下的模型 (如 deepseek, nemotron, mimo, laguna)…"
                class="mp-search-input-lg"
              />
              <button
                v-if="channelSearchQuery"
                type="button"
                class="mp-search-clear-btn"
                title="清空搜索"
                @click="channelSearchQuery = ''"
              >
                <span v-html="icons.close" />
              </button>
            </div>
            <button
              type="button"
              class="mp-btn mp-btn-ghost"
              :disabled="fetchingModels"
              title="从该上游渠道重新获取模型列表"
              @click="refreshModels"
            >
              <span :class="{ 'mp-spin': fetchingModels }" v-html="icons.restore" />
              <span>{{ fetchingModels ? "正在拉取…" : "刷新上游模型" }}</span>
            </button>
          </div>

          <!-- 精致双列模型卡片矩阵 -->
          <div class="mp-model-cards-grid">
            <div
              v-for="model in filteredChannelModels"
              :key="model"
              class="mp-model-elegant-card"
              :class="{ 'is-copied': copiedModelId === model }"
              @click="copyModel(model)"
            >
              <div class="mp-mec-left">
                <div class="mp-mec-title-row">
                  <span class="mp-model-free-badge">免费</span>
                  <span class="mp-model-name-title">{{ model }}</span>
                </div>
                <div class="mp-mec-id-row">
                  <span class="mp-mec-id-label">网关 ID:</span>
                  <code class="mp-mec-id-code">opencode/{{ model }}</code>
                </div>
              </div>

              <div class="mp-mec-right">
                <button
                  type="button"
                  class="mp-copy-action-btn"
                  :class="{ 'copied': copiedModelId === model }"
                  :title="`复制 opencode/${model}`"
                  @click.stop="copyModel(model)"
                >
                  <span v-html="copiedModelId === model ? icons.check : icons.copy" />
                  <span>{{ copiedModelId === model ? '已复制' : '复制网关 ID' }}</span>
                </button>
              </div>
            </div>
          </div>

          <div v-if="filteredChannelModels.length === 0" class="mp-empty-box">
            <div class="mp-empty-icon" v-html="icons.shield" />
            <p v-if="fetchingModels">正在从上游渠道拉取最新模型…</p>
            <p v-else>暂无匹配的渠道模型</p>
          </div>
        </div>

        <div class="mp-modal-footer">
          <div class="mp-modal-footer-hint text-muted text-xs">
            <span>💡 提示：该渠道所有模型均无需 API Key，通过网关即可免密高速调用</span>
          </div>
          <button
            type="button"
            class="mp-btn mp-btn-primary"
            @click="closeChannelModelsModal"
          >
            完成
          </button>
        </div>
      </div>
    </div>

    <!-- 健康检查与服务状态弹出框 (宽屏合并弹窗) -->
    <div
      v-if="healthModalOpen"
      class="mp-modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-labelledby="mp-health-modal-title"
      @click.self="closeHealthModal"
    >
      <div class="mp-modal-box mp-modal-box-extra-wide">
        <div class="mp-modal-header">
          <div class="mp-modal-title-group">
            <span class="mp-modal-icon" v-html="icons.pulse" />
            <div>
              <h3 id="mp-health-modal-title">服务连接状态与健康检查报告</h3>
              <small class="text-muted">检测时间：{{ healthResultTime || '刚刚' }} · 端点：http://127.0.0.1:{{ proxyStatus.port || proxyConfig.port }}</small>
            </div>
          </div>
          <button
            type="button"
            class="mp-modal-close"
            title="关闭弹窗 (Esc)"
            @click="closeHealthModal"
          >
            <span v-html="icons.close" />
          </button>
        </div>

        <div class="mp-modal-body">
          <!-- 运行概览指标块 -->
          <div class="mp-metrics-row">
            <div class="mp-metric-box">
              <label>服务端口</label>
              <strong class="font-mono text-brand">{{ proxyStatus.port || proxyConfig.port }}</strong>
            </div>
            <div class="mp-metric-box">
              <label>累计请求</label>
              <strong class="font-mono">{{ proxyStatus.totalRequests }}</strong>
            </div>
            <div class="mp-metric-box">
              <label>成功 / 失败</label>
              <strong class="font-mono text-success">{{ proxyStatus.successfulRequests }} <span class="text-muted">/</span> <span class="text-danger">{{ proxyStatus.failedRequests }}</span></strong>
            </div>
            <div class="mp-metric-box">
              <label>运行时长</label>
              <strong>{{ proxyStatus.running ? formatUptime(proxyStatus.uptimeSeconds) : '--' }}</strong>
            </div>
          </div>

          <!-- Base URL & API Key 快捷条目 -->
          <div class="mp-endpoint-list">
            <div class="mp-endpoint-item">
              <span class="mp-ep-label">Base URL</span>
              <code class="mp-ep-code">{{ proxyStatus.url || `http://127.0.0.1:${proxyConfig.port}/v1` }}</code>
              <button
                type="button"
                class="mp-action-btn"
                title="复制 Base URL"
                @click="copyProxyUrl"
              >
                <span v-html="icons.copy" />
                <span>复制</span>
              </button>
            </div>

            <div class="mp-endpoint-item">
              <span class="mp-ep-label">API Key</span>
              <code class="mp-ep-code">
                {{ showKey ? (proxyConfig.apiKey || '(未配置密钥，免密直接访问)') : (proxyConfig.apiKey ? '••••••••••••••••••••' : '(未配置密钥，免密直接访问)') }}
              </code>
              <div class="mp-ep-btns">
                <button
                  v-if="proxyConfig.apiKey"
                  type="button"
                  class="mp-action-btn mp-btn-icon-only"
                  :title="showKey ? '隐藏密钥' : '显示密钥'"
                  @click="showKey = !showKey"
                >
                  <span v-html="showKey ? icons.eyeOff : icons.eye" />
                </button>
                <button
                  type="button"
                  class="mp-action-btn"
                  title="复制 API Key"
                  @click="copyProxyKey"
                >
                  <span v-html="icons.copy" />
                  <span>复制</span>
                </button>
              </div>
            </div>
          </div>

          <!-- 诊断状态横幅 -->
          <div
            class="mp-health-summary-banner"
            :class="healthAllPassed ? 'is-success' : 'is-warning'"
          >
            <span
              class="mp-summary-icon"
              v-html="healthAllPassed ? icons.check : icons.alert"
            />
            <div class="mp-summary-text">
              <strong>{{ healthAllPassed ? '所有端点与通道运行正常' : '部分端点存在提示或异常' }}</strong>
              <p v-if="healthCheckList.length > 0">共完成 {{ healthCheckList.length }} 项端点与通道连通性检测</p>
              <p v-else>正在连接服务并获取健康检查数据…</p>
            </div>
          </div>

          <!-- 详细检测数组表格 -->
          <div class="mp-health-table-wrap">
            <table class="mp-health-table">
              <thead>
                <tr>
                  <th style="width: 220px;">检测项</th>
                  <th style="width: 200px;">端点 / 路径</th>
                  <th style="width: 90px;">状态</th>
                  <th style="width: 100px;">鉴权</th>
                  <th>检测详情与协议说明</th>
                </tr>
              </thead>
              <tbody>
                <tr
                  v-for="(item, idx) in healthCheckList"
                  :key="idx"
                  class="mp-health-row"
                >
                  <td class="font-medium text-text">{{ item.name }}</td>
                  <td><code class="mp-ep-code-sm">{{ item.endpoint }}</code></td>
                  <td>
                    <span
                      class="mp-status-tag"
                      :class="{
                        'tag-ok': item.status === 'ok',
                        'tag-warn': item.status === 'warning' || item.status === 'disabled',
                        'tag-err': item.status === 'error',
                      }"
                    >
                      <span class="mp-status-dot-sm" />
                      <span>{{ item.status === 'ok' ? '正常' : (item.status === 'warning' ? '提示' : '异常') }}</span>
                    </span>
                  </td>
                  <td>
                    <span class="mp-auth-tag">{{ item.auth || '--' }}</span>
                  </td>
                  <td class="text-muted text-sm">{{ item.message }}</td>
                </tr>
                <tr v-if="healthCheckList.length === 0">
                  <td colspan="5" class="text-center py-6 text-muted">
                    {{ testingHealth ? '正在执行健康检查…' : '暂无检查数据' }}
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        </div>

        <div class="mp-modal-footer">
          <button
            type="button"
            class="mp-btn mp-btn-ghost"
            :disabled="testingHealth"
            @click="testHealth"
          >
            <span v-html="icons.restore" />
            <span>{{ testingHealth ? "正在重新测试…" : "重新检测" }}</span>
          </button>
          <button
            type="button"
            class="mp-btn mp-btn-primary"
            @click="closeHealthModal"
          >
            确定
          </button>
        </div>
      </div>
    </div>

    <!-- 数据库日志清理选项弹窗 (Clear Logs Options Modal) -->
    <div
      v-if="clearLogsModalOpen"
      class="mp-modal-backdrop mp-sub-backdrop"
      role="dialog"
      aria-modal="true"
      aria-labelledby="mp-clear-modal-title"
      @click.self="clearLogsModalOpen = false"
    >
      <div class="mp-modal-box mp-modal-box-sm mp-clear-logs-box">
        <div class="mp-modal-header">
          <div class="mp-card-title-group">
            <span class="mp-modal-icon text-danger" v-html="icons.trash" />
            <h3 id="mp-clear-modal-title">清理数据库日志</h3>
          </div>
          <button
            type="button"
            class="mp-modal-close"
            title="关闭 (Esc)"
            @click="clearLogsModalOpen = false"
          >
            <span v-html="icons.close" />
          </button>
        </div>

        <div class="mp-modal-body">
          <p class="mp-clear-modal-desc">
            请选择要执行的本地 SQLite 数据库日志清理模式：
          </p>

          <div class="mp-clear-options-grid">
            <!-- 选项 1: 仅清空请求/响应报文 -->
            <div class="mp-clear-option-card">
              <div class="mp-coc-head">
                <div class="mp-coc-title-wrap">
                  <span class="mp-coc-icon">📝</span>
                  <div>
                    <strong>清理请求与响应详细内容</strong>
                    <span class="mp-coc-badge">推荐（大幅节省磁盘空间）</span>
                  </div>
                </div>
              </div>
              <p class="mp-coc-desc">
                仅清空日志中保存的客户端请求全文与服务端响应内容（释放大体积存储），<strong>保留</strong>历史调用时间、状态码、模型、节点、耗时与 Token 统计等列表索引。
              </p>
              <div class="mp-coc-action">
                <button
                  type="button"
                  class="mp-btn mp-btn-ghost mp-btn-sm"
                  :disabled="clearingLogs"
                  @click="handleClearLogs('payload_only')"
                >
                  <span>{{ clearingLogs ? "清理中…" : "仅清理报文全文" }}</span>
                </button>
              </div>
            </div>

            <!-- 选项 2: 完全清空所有记录 -->
            <div class="mp-clear-option-card is-danger">
              <div class="mp-coc-head">
                <div class="mp-coc-title-wrap">
                  <span class="mp-coc-icon text-danger">🗑️</span>
                  <div>
                    <strong class="text-danger">彻底清空全部记录与统计</strong>
                    <span class="mp-coc-badge is-danger">全部删除</span>
                  </div>
                </div>
              </div>
              <p class="mp-coc-desc">
                从本地 SQLite 数据库中永久删除所有反代调用日志记录，并将控制台运行时计数器与 Token 统计归零。
              </p>
              <div class="mp-coc-action">
                <button
                  type="button"
                  class="mp-btn mp-btn-danger mp-btn-sm"
                  :disabled="clearingLogs"
                  @click="handleClearLogs('all')"
                >
                  <span>{{ clearingLogs ? "清空中…" : "彻底清空所有记录" }}</span>
                </button>
              </div>
            </div>
          </div>
        </div>

        <div class="mp-modal-footer">
          <button
            type="button"
            class="mp-btn mp-btn-ghost"
            @click="clearLogsModalOpen = false"
          >
            取消
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.mp-page {
  padding: 20px 24px 40px;
  width: 100%;
  max-width: 100%;
  box-sizing: border-box;
  margin: 0 auto;
  display: flex;
  flex-direction: column;
  gap: 16px;
  min-width: 0;
}

/* 顶栏 */
.mp-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 16px;
  width: 100%;
  box-sizing: border-box;
  flex-wrap: wrap;
}

.mp-header-title-row {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

.mp-header-title-row .mp-icon {
  width: 28px;
  height: 28px;
  color: var(--brand);
  display: inline-flex;
}

.mp-header-title-row .mp-icon :deep(svg) {
  width: 100%;
  height: 100%;
}

.mp-header h1 {
  font-size: 24px;
  font-weight: 800;
  color: var(--text);
  margin: 0;
  letter-spacing: -0.02em;
}

.mp-subtitle {
  margin: 6px 0 0;
  font-size: 13px;
  color: var(--muted);
}

.mp-status-pill {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  font-weight: 600;
  padding: 3px 10px;
  border-radius: 999px;
  background: var(--surface-soft);
  color: var(--muted);
  border: 1px solid var(--line);
}

.mp-status-pill.active {
  background: var(--brand-soft);
  color: var(--brand-deep);
  border-color: color-mix(in srgb, var(--brand) 40%, transparent);
}

.mp-status-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--muted);
}

.mp-status-pill.active .mp-status-dot {
  background: var(--brand);
  box-shadow: 0 0 8px var(--brand);
  animation: mp-pulse 2s infinite;
}

.mp-channel-pill {
  font-size: 11.5px;
  font-weight: 600;
  padding: 2px 8px;
  border-radius: var(--r-xs);
  background: var(--surface-soft);
  color: var(--brand-deep);
  border: 1px solid color-mix(in srgb, var(--brand) 30%, transparent);
}

@keyframes mp-pulse {
  0%, 100% { opacity: 1; transform: scale(1); }
  50% { opacity: 0.5; transform: scale(1.15); }
}

.mp-header-actions {
  display: flex;
  align-items: center;
  gap: 10px;
}

/* 按钮规范 */
.mp-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 36px;
  padding: 0 14px;
  border-radius: var(--r-md, 8px);
  font-size: 13px;
  font-weight: 600;
  cursor: pointer;
  border: 1px solid transparent;
  transition: all 0.18s var(--ease);
}

.mp-btn-sm {
  height: 30px;
  padding: 0 10px;
  font-size: 12px;
}

.mp-btn :deep(svg) {
  width: 15px;
  height: 15px;
}

.mp-btn-badge {
  font-size: 11px;
  padding: 1px 6px;
  border-radius: 999px;
  background: var(--brand-soft);
  color: var(--brand-deep);
  font-weight: 700;
  margin-left: 2px;
}

.mp-btn-badge-danger {
  background: color-mix(in srgb, var(--danger) 15%, transparent);
  color: var(--danger);
}

.mp-btn-primary {
  background: var(--brand);
  color: #fff;
  border-color: var(--brand);
}

.mp-btn-primary:hover {
  background: var(--brand-deep);
  transform: translateY(-1px);
}

.mp-btn-danger {
  background: var(--danger);
  color: #fff;
  border-color: var(--danger);
}

.mp-btn-danger:hover {
  opacity: 0.9;
}

.mp-btn-ghost {
  background: var(--surface);
  border-color: var(--line);
  color: var(--text);
}

.mp-btn-ghost:hover {
  background: var(--surface-hover);
  border-color: var(--line-strong);
}

/* 一级页面 Tab 切换导航条 */
.mp-main-tab-nav {
  display: flex;
  align-items: center;
  gap: 8px;
  background: var(--surface);
  padding: 5px;
  border-radius: var(--r-lg, 12px);
  border: 1px solid var(--line);
  box-shadow: 0 1px 4px rgba(0, 0, 0, 0.02);
  width: fit-content;
}

.mp-main-nav-btn {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 8px 16px;
  border-radius: var(--r-md, 8px);
  border: none;
  background: transparent;
  color: var(--muted);
  font-size: 13.5px;
  font-weight: 650;
  cursor: pointer;
  transition: all 0.18s ease;
}

.mp-main-nav-btn :deep(svg) {
  width: 16px;
  height: 16px;
}

.mp-main-nav-btn:hover {
  color: var(--text);
  background: var(--surface-hover);
}

.mp-main-nav-btn.active {
  background: var(--brand);
  color: #fff;
  box-shadow: 0 2px 8px color-mix(in srgb, var(--brand) 35%, transparent);
}

.mp-tab-count-pill {
  font-size: 11px;
  font-weight: 700;
  padding: 1px 7px;
  border-radius: 999px;
  background: var(--surface-soft);
  color: var(--text);
  border: 1px solid var(--line);
}

.mp-main-nav-btn.active .mp-tab-count-pill {
  background: rgba(255, 255, 255, 0.25);
  color: #fff;
  border-color: transparent;
}

.mp-tab-count-pill.is-err {
  background: color-mix(in srgb, var(--danger) 15%, transparent);
  color: var(--danger);
  border-color: color-mix(in srgb, var(--danger) 30%, transparent);
}

.mp-main-nav-btn.active .mp-tab-count-pill.is-err {
  background: var(--danger);
  color: #fff;
  border-color: transparent;
}

.mp-tab-pane {
  display: flex;
  flex-direction: column;
  gap: 16px;
  width: 100%;
  max-width: 100%;
  box-sizing: border-box;
  min-width: 0;
  animation: fadeInTab 0.18s ease;
}

@keyframes fadeInTab {
  from { opacity: 0; transform: translateY(3px); }
  to { opacity: 1; transform: translateY(0); }
}

/* 请求日志全屏视图样式 */
.mp-logs-summary-bar {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
  width: 100%;
  box-sizing: border-box;
  flex-wrap: wrap;
}

.mp-lsb-left {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

.mp-lsb-badge-group {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.mp-lsb-pill {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  font-weight: 600;
  color: var(--brand-deep);
  background: var(--brand-soft);
  padding: 3px 10px;
  border-radius: 999px;
  border: 1px solid color-mix(in srgb, var(--brand) 25%, transparent);
}

.mp-lsb-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--brand);
}

.mp-logs-main-card {
  padding: 0;
  overflow: hidden;
  width: 100%;
  max-width: 100%;
  box-sizing: border-box;
}

.mp-logs-main-card .mp-logs-toolbar {
  padding: 16px 18px 12px;
  width: 100%;
  box-sizing: border-box;
}

.mp-logs-main-card .mp-logs-table-wrap {
  border-top: 1px solid var(--line);
  max-height: calc(100vh - 300px);
  min-height: 400px;
  overflow-y: auto;
  overflow-x: auto;
  width: 100%;
  max-width: 100%;
  box-sizing: border-box;
}

.mp-btn-text {
  background: transparent;
  border: 1px solid transparent;
  color: var(--brand);
  font-size: 11.5px;
  font-weight: 600;
  padding: 2px 8px;
  border-radius: 4px;
  cursor: pointer;
  transition: all 0.15s ease;
}

.mp-btn-text:hover {
  background: var(--brand-soft);
  border-color: var(--brand);
}

/* 卡片基类 */
.mp-card {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 12px);
  padding: 20px;
  display: flex;
  flex-direction: column;
  gap: 16px;
  box-shadow: 0 2px 8px rgba(16, 35, 25, 0.04);
}

.mp-card-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
}

.mp-card-title-group {
  display: flex;
  align-items: center;
  gap: 8px;
}

.mp-card-icon {
  width: 20px;
  height: 20px;
  color: var(--brand);
  display: inline-flex;
}

.mp-card-icon :deep(svg) {
  width: 100%;
  height: 100%;
}

.mp-card-header h2 {
  font-size: 15px;
  font-weight: 750;
  color: var(--text);
  margin: 0;
}

.mp-tag {
  font-size: 11.5px;
  padding: 2px 8px;
  border-radius: var(--r-xs);
  background: var(--surface-soft);
  color: var(--muted);
  border: 1px solid var(--line);
  font-weight: 600;
}

/* 指标卡片 */
.mp-metrics-row {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 10px;
}

.mp-metric-box {
  background: var(--surface-soft);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 10px 12px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.mp-metric-box label {
  font-size: 11px;
  color: var(--muted);
}

.mp-metric-box strong {
  font-size: 16px;
  font-weight: 750;
  color: var(--text);
}

/* 累计 Token 消耗与缓存统计面板 */
.mp-metrics-tokens-panel {
  background: var(--surface-soft);
  border: 1px solid color-mix(in srgb, var(--brand) 20%, var(--line));
  border-radius: var(--r-md);
  padding: 12px 14px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.mp-mtp-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}

.mp-mtp-title-wrap {
  display: flex;
  align-items: center;
  gap: 6px;
}

.mp-mtp-icon {
  color: var(--brand);
  font-size: 14px;
}

.mp-mtp-title {
  font-size: 12px;
  font-weight: 700;
  color: var(--text);
}

.mp-mtp-total-badge {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  background: var(--brand-soft);
  border: 1px solid color-mix(in srgb, var(--brand) 25%, transparent);
  padding: 2px 8px;
  border-radius: 999px;
  font-size: 11.5px;
}

.mp-mtp-total-badge strong {
  font-size: 13px;
}

.mp-mtp-grid {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 8px;
}

.mp-mtp-card {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-sm, 6px);
  padding: 8px 10px;
  display: flex;
  flex-direction: column;
  gap: 2px;
  transition: all 0.15s ease;
}

.mp-mtp-card:hover {
  border-color: color-mix(in srgb, var(--brand) 40%, var(--line));
  transform: translateY(-1px);
}

.mp-mtp-card-head {
  display: flex;
  align-items: center;
  gap: 5px;
}

.mp-mtp-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  flex-shrink: 0;
}

.mp-mtp-dot.is-in {
  background: #3b82f6;
}

.mp-mtp-dot.is-hit {
  background: var(--brand);
}

.mp-mtp-dot.is-think {
  background: #8b5cf6;
}

.mp-mtp-dot.is-out {
  background: #10b981;
}

.mp-mtp-card-label {
  font-size: 10.5px;
  font-weight: 650;
  color: var(--muted);
}

.mp-mtp-card-val {
  font-size: 15px;
  font-weight: 750;
  color: var(--text);
}

.mp-mtp-card.is-hit .mp-mtp-card-val {
  color: var(--brand-deep);
}

.mp-mtp-card-sub {
  font-size: 10.5px;
  line-height: 1.2;
}

@media (max-width: 860px) {
  .mp-mtp-grid {
    grid-template-columns: repeat(2, 1fr);
  }
}

/* 端点展示条目 */
.mp-endpoint-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.mp-endpoint-item {
  display: flex;
  align-items: center;
  gap: 10px;
  background: var(--surface-soft);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 8px 12px;
}

.mp-ep-label {
  font-size: 12px;
  font-weight: 700;
  color: var(--muted);
  min-width: 68px;
}

.mp-ep-code {
  flex: 1;
  font-family: var(--font-mono, monospace);
  font-size: 12.5px;
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.mp-ep-btns {
  display: flex;
  align-items: center;
  gap: 6px;
}

.mp-action-btn {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  height: 26px;
  padding: 0 8px;
  border-radius: var(--r-xs);
  background: var(--surface);
  border: 1px solid var(--line);
  color: var(--muted);
  font-size: 11.5px;
  cursor: pointer;
  transition: all 0.15s ease;
}

.mp-action-btn :deep(svg) {
  width: 12px;
  height: 12px;
}

.mp-action-btn:hover {
  background: var(--surface-hover);
  color: var(--text);
  border-color: var(--line-strong);
}

.mp-btn-icon-only {
  width: 26px;
  padding: 0;
  justify-content: center;
}

/* 渠道分区标题 */
.mp-section-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-top: 6px;
}

.mp-section-head h2 {
  font-size: 15px;
  font-weight: 750;
  color: var(--text);
  margin: 0;
}

/* 渠道卡片网格 (三列卡片布局) */
.mp-channels-grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 16px;
}

@media (max-width: 1100px) {
  .mp-channels-grid {
    grid-template-columns: repeat(2, 1fr);
  }
}

@media (max-width: 720px) {
  .mp-channels-grid {
    grid-template-columns: 1fr;
  }
}

.mp-channel-card {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 12px);
  padding: 18px;
  display: flex;
  flex-direction: column;
  gap: 14px;
  transition: all 0.2s ease;
  box-shadow: 0 2px 8px rgba(16, 35, 25, 0.04);
}

.mp-channel-card:hover {
  border-color: color-mix(in srgb, var(--brand) 40%, transparent);
  box-shadow: 0 4px 16px rgba(16, 35, 25, 0.08);
}

.mp-channel-card.is-disabled {
  opacity: 0.65;
  background: var(--surface-soft);
}

.mp-channel-card-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.mp-channel-card-title {
  display: flex;
  align-items: center;
  gap: 10px;
}

.mp-channel-badge-icon {
  width: 32px;
  height: 32px;
  border-radius: var(--r-md);
  background: var(--brand-soft);
  color: var(--brand);
  display: flex;
  align-items: center;
  justify-content: center;
}

.mp-channel-badge-icon :deep(svg) {
  width: 18px;
  height: 18px;
}

.mp-channel-card-title h3 {
  font-size: 15px;
  font-weight: 750;
  color: var(--text);
  margin: 0;
}

.mp-proto-tag {
  font-size: 10.5px;
  font-weight: 600;
  color: var(--brand-deep);
  background: var(--brand-soft);
  padding: 1px 6px;
  border-radius: 4px;
  display: inline-block;
}

.mp-channel-desc {
  font-size: 12.5px;
  color: var(--muted);
  margin: 0;
  line-height: 1.45;
  min-height: 36px;
}

/* 代理池配置块 */
.mp-proxy-pool-box {
  background: var(--surface-soft);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 12px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.mp-proxy-pool-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.mp-proxy-pool-label {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  font-weight: 650;
  color: var(--text);
}

.mp-pp-icon {
  width: 14px;
  height: 14px;
  color: var(--brand);
  display: inline-flex;
}

.mp-pp-icon :deep(svg) {
  width: 100%;
  height: 100%;
}

.mp-proxy-pool-status {
  font-size: 11.5px;
  line-height: 1.4;
  padding: 6px 8px;
  border-radius: var(--r-xs);
}

.mp-proxy-pool-status.is-active {
  background: var(--brand-soft);
  color: var(--brand-deep);
  display: flex;
  align-items: center;
  gap: 6px;
}

.mp-proxy-pool-status.is-active strong {
  color: var(--brand);
}

.mp-proxy-pool-status.is-inactive {
  color: var(--muted);
}

.mp-channel-card-footer {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding-top: 10px;
  border-top: 1px solid var(--line);
}

.mp-channel-stat {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.mp-channel-stat span {
  font-size: 11px;
  color: var(--muted);
}

.mp-channel-stat strong {
  font-size: 13px;
  color: var(--brand);
}

/* 开关组件 */
.mp-switch-wrap {
  position: relative;
  display: inline-block;
  width: 38px;
  height: 20px;
  cursor: pointer;
}

.mp-switch-wrap input {
  opacity: 0;
  width: 0;
  height: 0;
}

.mp-switch-round {
  position: absolute;
  inset: 0;
  background-color: var(--line);
  border-radius: 999px;
  transition: 0.2s;
}

.mp-switch-round:before {
  position: absolute;
  content: "";
  height: 14px;
  width: 14px;
  left: 3px;
  bottom: 3px;
  background-color: #fff;
  border-radius: 50%;
  transition: 0.2s;
}

.mp-switch-wrap input:checked + .mp-switch-round {
  background-color: var(--brand);
}

.mp-switch-wrap input:checked + .mp-switch-round:before {
  transform: translateX(18px);
}

/* 表单控件 */
.mp-field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  width: 100%;
}

.mp-field label {
  font-size: 12px;
  font-weight: 600;
  color: var(--text);
  display: flex;
  justify-content: space-between;
}

.mp-field small {
  font-size: 11px;
  color: var(--muted);
}

.mp-input {
  width: 100%;
  padding: 8px 10px;
  border-radius: var(--r-md);
  border: 1px solid var(--line);
  background: var(--surface-soft);
  color: var(--text);
  font-size: 13px;
  box-sizing: border-box;
}

.mp-input:focus {
  outline: none;
  border-color: var(--brand);
  box-shadow: 0 0 0 2px var(--brand-glow);
}

/* 弹窗模态框通用 */
.mp-modal-backdrop {
  position: fixed;
  inset: 0;
  z-index: 9000;
  background: rgba(10, 20, 15, 0.45);
  backdrop-filter: blur(4px);
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
}

.mp-sub-backdrop {
  z-index: 9100;
  background: rgba(10, 20, 15, 0.65);
}

.mp-modal-box {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 12px);
  width: 100%;
  max-width: 520px;
  max-height: 88vh;
  display: flex;
  flex-direction: column;
  box-shadow: 0 16px 40px rgba(0, 0, 0, 0.2);
  overflow: hidden;
  animation: mp-modal-fade 0.2s ease;
}

.mp-modal-box-sm {
  max-width: 480px;
}

.mp-modal-box-wide {
  max-width: 900px;
  width: 90vw;
}

.mp-modal-box-extra-wide {
  max-width: 1160px;
  width: 95vw;
}

@keyframes mp-modal-fade {
  from { opacity: 0; transform: scale(0.97); }
  to { opacity: 1; transform: scale(1); }
}

.mp-modal-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 16px 20px;
  border-bottom: 1px solid var(--line);
}

.mp-modal-title-group {
  display: flex;
  align-items: center;
  gap: 12px;
}

.mp-modal-badge-icon {
  width: 36px;
  height: 36px;
  border-radius: var(--r-md);
  background: var(--brand-soft);
  color: var(--brand);
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
}

.mp-modal-badge-icon.is-error {
  background: color-mix(in srgb, var(--danger) 15%, transparent);
  color: var(--danger);
}

.mp-modal-badge-icon.is-success {
  background: var(--brand-soft);
  color: var(--brand);
}

.mp-modal-badge-icon :deep(svg) {
  width: 20px;
  height: 20px;
}

.mp-modal-icon {
  width: 24px;
  height: 24px;
  color: var(--brand);
  display: inline-flex;
}

.mp-modal-icon :deep(svg) {
  width: 100%;
  height: 100%;
}

.mp-modal-title-wrap {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.mp-modal-header h3 {
  margin: 0;
  font-size: 16px;
  font-weight: 750;
  color: var(--text);
}

.mp-header-chip {
  font-size: 11px;
  font-weight: 700;
  padding: 1px 7px;
  border-radius: 999px;
  background: var(--brand-soft);
  color: var(--brand-deep);
}

.mp-header-endpoint-chip {
  font-size: 11px;
  padding: 1px 7px;
  border-radius: var(--r-xs);
  background: var(--surface-soft);
  color: var(--muted);
  border: 1px solid var(--line);
}

.mp-modal-close {
  width: 32px;
  height: 32px;
  border-radius: var(--r-xs);
  border: none;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  display: inline-flex;
  align-items: center;
  justify-content: center;
}

.mp-modal-close :deep(svg) {
  width: 18px;
  height: 18px;
}

.mp-modal-close:hover {
  background: var(--surface-hover);
  color: var(--text);
}

.mp-modal-body {
  padding: 20px;
  display: flex;
  flex-direction: column;
  gap: 16px;
  overflow-y: auto;
}

.mp-modal-footer {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 10px;
  padding: 14px 20px;
  border-top: 1px solid var(--line);
  background: var(--surface-soft);
}

.mp-modal-footer-hint {
  font-size: 12px;
  color: var(--muted);
}

/* 模型弹窗高级工具栏 */
.mp-models-modal-toolbar {
  display: flex;
  align-items: center;
  gap: 12px;
}

.mp-search-box {
  position: relative;
  display: flex;
  align-items: center;
}

.mp-search-box.flex-1 {
  flex: 1;
}

.mp-search-box :deep(svg) {
  position: absolute;
  left: 12px;
  width: 16px;
  height: 16px;
  color: var(--muted);
}

.mp-search-input-lg {
  width: 100%;
  height: 38px;
  padding: 0 32px 0 36px;
  border-radius: var(--r-md);
  border: 1px solid var(--line);
  background: var(--surface-soft);
  color: var(--text);
  font-size: 13px;
  box-sizing: border-box;
  transition: all 0.18s ease;
}

.mp-search-input-lg:focus {
  outline: none;
  border-color: var(--brand);
  box-shadow: 0 0 0 2px var(--brand-glow);
  background: var(--surface);
}

.mp-search-clear-btn {
  position: absolute;
  right: 8px;
  width: 20px;
  height: 20px;
  border-radius: 50%;
  border: none;
  background: var(--surface-hover);
  color: var(--muted);
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 0;
}

.mp-search-clear-btn :deep(svg) {
  position: static;
  width: 12px;
  height: 12px;
}

.mp-spin {
  animation: mp-spin-anim 0.8s linear infinite;
}

@keyframes mp-spin-anim {
  from { transform: rotate(0deg); }
  to { transform: rotate(360deg); }
}

/* 渠道分组展示容器 */
.mp-channel-groups-container {
  display: flex;
  flex-direction: column;
  gap: 20px;
  max-height: 480px;
  overflow-y: auto;
  padding-right: 4px;
}

.mp-channel-group-block {
  display: flex;
  flex-direction: column;
  gap: 10px;
  background: var(--surface-soft);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 12px);
  padding: 14px 16px;
}

.mp-group-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
  padding-bottom: 8px;
  border-bottom: 1px solid var(--line);
}

.mp-group-title-row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.mp-group-icon {
  width: 22px;
  height: 22px;
  color: var(--brand);
  display: inline-flex;
}

.mp-group-icon :deep(svg) {
  width: 100%;
  height: 100%;
}

.mp-group-name-wrap {
  display: flex;
  align-items: center;
  gap: 8px;
}

.mp-group-name {
  margin: 0;
  font-size: 14px;
  font-weight: 750;
  color: var(--text);
}

.mp-group-count-badge {
  font-size: 11px;
  font-weight: 600;
  padding: 1px 6px;
  border-radius: 999px;
  background: var(--surface);
  border: 1px solid var(--line);
  color: var(--muted);
}

.mp-group-meta {
  display: flex;
  align-items: center;
  gap: 8px;
}

.mp-group-endpoint {
  font-size: 11px;
  color: var(--muted);
  background: var(--surface);
  padding: 2px 6px;
  border-radius: 4px;
  border: 1px solid var(--line);
}

.mp-status-pill-xs {
  font-size: 10.5px;
  padding: 1px 7px;
}

.mp-group-empty-note {
  padding: 16px 0;
  text-align: center;
}

/* 优雅模型卡片双列矩阵 */
.mp-model-cards-grid {
  display: grid;
  grid-template-columns: repeat(2, 1fr);
  gap: 12px;
}

@media (max-width: 680px) {
  .mp-model-cards-grid {
    grid-template-columns: 1fr;
  }
}

.mp-model-elegant-card {
  background: var(--surface-soft);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 10px);
  padding: 12px 14px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  cursor: pointer;
  transition: all 0.2s cubic-bezier(0.4, 0, 0.2, 1);
  position: relative;
}

.mp-model-elegant-card:hover {
  background: var(--surface);
  border-color: color-mix(in srgb, var(--brand) 50%, transparent);
  transform: translateY(-1px);
  box-shadow: 0 4px 14px rgba(0, 0, 0, 0.06);
}

.mp-model-elegant-card.is-copied {
  border-color: var(--brand);
  background: var(--brand-soft);
}

.mp-mec-left {
  display: flex;
  flex-direction: column;
  gap: 5px;
  min-width: 0;
  flex: 1;
}

.mp-mec-title-row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.mp-model-free-badge {
  font-size: 10px;
  font-weight: 800;
  letter-spacing: 0.04em;
  padding: 1px 5px;
  border-radius: 4px;
  background: color-mix(in srgb, var(--brand) 20%, transparent);
  color: var(--brand-deep);
  flex-shrink: 0;
}

.mp-model-name-title {
  font-family: var(--font-mono, monospace);
  font-size: 13.5px;
  font-weight: 700;
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.mp-mec-id-row {
  display: flex;
  align-items: center;
  gap: 6px;
}

.mp-mec-id-label {
  font-size: 11px;
  color: var(--muted);
  flex-shrink: 0;
}

.mp-mec-id-code {
  font-family: var(--font-mono, monospace);
  font-size: 12px;
  color: var(--brand-deep);
  background: color-mix(in srgb, var(--brand) 10%, transparent);
  padding: 1px 6px;
  border-radius: 4px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.mp-mec-right {
  flex-shrink: 0;
}

.mp-copy-action-btn {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  height: 28px;
  padding: 0 10px;
  border-radius: var(--r-xs, 6px);
  background: var(--surface);
  border: 1px solid var(--line);
  color: var(--text);
  font-size: 11.5px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.18s ease;
}

.mp-copy-action-btn :deep(svg) {
  width: 12px;
  height: 12px;
  color: var(--muted);
}

.mp-copy-action-btn:hover {
  background: var(--brand-soft);
  border-color: var(--brand);
  color: var(--brand-deep);
}

.mp-copy-action-btn:hover :deep(svg) {
  color: var(--brand);
}

.mp-copy-action-btn.copied {
  background: var(--brand);
  border-color: var(--brand);
  color: #fff;
}

.mp-copy-action-btn.copied :deep(svg) {
  color: #fff;
}

.mp-empty-box {
  padding: 48px 0;
  text-align: center;
  color: var(--muted);
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 10px;
}

.mp-empty-icon {
  width: 36px;
  height: 36px;
  color: var(--line-strong);
  display: inline-flex;
}

.mp-empty-icon :deep(svg) {
  width: 100%;
  height: 100%;
}

.mp-empty-box p {
  margin: 0;
  font-size: 13px;
}

/* 健康检查弹窗样式 */
.mp-health-summary-banner {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px 16px;
  border-radius: var(--r-md);
  border: 1px solid transparent;
}

.mp-health-summary-banner.is-success {
  background: var(--brand-soft);
  border-color: color-mix(in srgb, var(--brand) 40%, transparent);
}

.mp-health-summary-banner.is-warning {
  background: color-mix(in srgb, var(--danger) 10%, transparent);
  border-color: color-mix(in srgb, var(--danger) 40%, transparent);
}

.mp-summary-icon {
  width: 24px;
  height: 24px;
  display: inline-flex;
  flex-shrink: 0;
}

.mp-health-summary-banner.is-success .mp-summary-icon {
  color: var(--brand);
}

.mp-health-summary-banner.is-warning .mp-summary-icon {
  color: var(--danger);
}

.mp-summary-text strong {
  font-size: 13.5px;
  color: var(--text);
  display: block;
}

.mp-summary-text p {
  margin: 2px 0 0;
  font-size: 12px;
  color: var(--muted);
}

.mp-health-table-wrap,
.mp-logs-table-wrap {
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  overflow: hidden;
  max-height: 480px;
  overflow-y: auto;
  overflow-x: auto;
  width: 100%;
  box-sizing: border-box;
}

.mp-health-table,
.mp-logs-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 12.5px;
  text-align: left;
}

.mp-health-table th,
.mp-logs-table th {
  background: var(--surface-soft);
  color: var(--muted);
  font-weight: 650;
  padding: 8px 12px;
  border-bottom: 1px solid var(--line);
  font-size: 11.5px;
  position: sticky;
  top: 0;
  z-index: 1;
}

.mp-health-table td,
.mp-logs-table td {
  padding: 10px 12px;
  border-bottom: 1px solid var(--line);
  vertical-align: middle;
}

.mp-health-row:last-child td {
  border-bottom: none;
}

.mp-ep-code-sm {
  font-family: var(--font-mono, monospace);
  font-size: 11.5px;
  color: var(--brand);
  background: var(--surface-soft);
  padding: 2px 6px;
  border-radius: 4px;
}

.mp-status-tag {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 2px 8px;
  border-radius: 999px;
  font-size: 11px;
  font-weight: 600;
}

.mp-status-dot-sm {
  width: 6px;
  height: 6px;
  border-radius: 50%;
}

.tag-ok {
  background: var(--brand-soft);
  color: var(--brand-deep);
}

.tag-ok .mp-status-dot-sm {
  background: var(--brand);
}

.tag-warn {
  background: color-mix(in srgb, #f59e0b 15%, transparent);
  color: #b45309;
}

.tag-warn .mp-status-dot-sm {
  background: #f59e0b;
}

.tag-err {
  background: color-mix(in srgb, var(--danger) 15%, transparent);
  color: var(--danger);
}

.tag-err .mp-status-dot-sm {
  background: var(--danger);
}

.mp-auth-tag {
  font-size: 11px;
  padding: 2px 6px;
  border-radius: 4px;
  background: var(--surface-soft);
  color: var(--muted);
  border: 1px solid var(--line);
}

/* 请求日志专用样式 */
.mp-logs-toolbar {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.mp-log-filter-tabs {
  display: flex;
  align-items: center;
  gap: 4px;
  background: var(--surface-soft);
  padding: 3px;
  border-radius: var(--r-md);
  border: 1px solid var(--line);
}

.mp-log-tab-btn {
  border: none;
  background: transparent;
  color: var(--muted);
  font-size: 12px;
  font-weight: 600;
  padding: 4px 10px;
  border-radius: var(--r-xs);
  cursor: pointer;
  transition: all 0.15s ease;
}

.mp-log-tab-btn:hover {
  color: var(--text);
}

.mp-log-tab-btn.active {
  background: var(--surface);
  color: var(--brand);
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.08);
}

.mp-logs-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}

.mp-log-row {
  cursor: pointer;
  transition: background 0.15s ease;
}

.mp-log-row:hover {
  background: var(--surface-hover);
}

.mp-log-row.has-error {
  background: color-mix(in srgb, var(--danger) 4%, transparent);
}

.mp-log-method-path {
  display: flex;
  align-items: center;
  gap: 6px;
}

.mp-method-tag {
  font-size: 10px;
  font-weight: 800;
  padding: 1px 5px;
  border-radius: 3px;
}

.method-post {
  background: color-mix(in srgb, var(--brand) 15%, transparent);
  color: var(--brand-deep);
}

.method-get {
  background: color-mix(in srgb, #3b82f6 15%, transparent);
  color: #2563eb;
}

.mp-path-code {
  font-family: var(--font-mono, monospace);
  font-size: 11.5px;
  color: var(--text);
}

.mp-log-time-col {
  display: flex;
  flex-direction: column;
  gap: 2px;
  line-height: 1.25;
}

.mp-log-time-col .mp-log-date {
  font-size: 11px;
  color: var(--muted);
}

.mp-log-time-col .mp-log-time {
  font-size: 12px;
  font-weight: 700;
  color: var(--text);
}

.mp-log-model-col {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 3px;
  min-width: 0;
  line-height: 1.25;
}

.mp-log-chan-row {
  display: flex;
  align-items: center;
}

.mp-log-model-col .mp-proto-tag {
  font-size: 9.5px;
  padding: 1px 5px;
  border-radius: 3px;
}

.mp-log-model-col .mp-log-model-name {
  font-size: 12px;
  font-weight: 650;
  color: var(--text);
  word-break: break-all;
}

.mp-log-node-wrap {
  display: flex;
  align-items: center;
  max-width: 120px;
}

.mp-node-pill {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  font-size: 11px;
  padding: 1.5px 7px;
  border-radius: 999px;
  white-space: nowrap;
  max-width: 100%;
}

.mp-node-pill.is-direct {
  background: color-mix(in srgb, #3b82f6 12%, transparent);
  color: #2563eb;
  border: 1px solid color-mix(in srgb, #3b82f6 25%, transparent);
}

.mp-node-pill.is-direct .mp-node-dot {
  background: #3b82f6;
}

.mp-node-pill.is-proxy {
  background: color-mix(in srgb, #8b5cf6 15%, transparent);
  color: #7c3aed;
  border: 1px solid color-mix(in srgb, #8b5cf6 30%, transparent);
}

.mp-node-pill.is-proxy .mp-node-dot {
  background: #8b5cf6;
}

.mp-node-dot {
  width: 5px;
  height: 5px;
  border-radius: 50%;
  flex-shrink: 0;
}

.is-proxy-chip {
  background: color-mix(in srgb, #8b5cf6 15%, transparent) !important;
  color: #7c3aed !important;
  border-color: color-mix(in srgb, #8b5cf6 30%, transparent) !important;
}

.mp-stream-tag {
  font-size: 10px;
  padding: 1px 5px;
  border-radius: 4px;
  background: var(--surface-soft);
  color: var(--muted);
  border: 1px solid var(--line);
}

.mp-stream-tag.is-stream {
  color: var(--brand-deep);
  background: var(--brand-soft);
  border-color: color-mix(in srgb, var(--brand) 25%, transparent);
}

.mp-log-err-preview {
  display: flex;
  align-items: center;
  gap: 6px;
  color: var(--danger);
  font-size: 11.5px;
  max-width: 320px;
}

.mp-err-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--danger);
  flex-shrink: 0;
}

.truncate {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.flex-center-start {
  display: flex;
  align-items: center;
}

.ml-1 {
  margin-left: 4px;
}

/* 请求详情弹窗专属样式 */
.mp-log-detail-box {
  max-width: 1180px;
  width: 94vw;
}

.mp-log-detail-box .mp-modal-header {
  padding: 16px 20px;
}

.mp-log-detail-box .mp-modal-title-group {
  flex: 1;
  min-width: 0;
}

.mp-log-detail-box .mp-modal-title-wrap {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: nowrap;
  white-space: nowrap;
}

.mp-log-detail-box .mp-modal-title-wrap > * {
  flex-shrink: 0;
}

.mp-log-error-banner {
  display: flex;
  align-items: flex-start;
  gap: 12px;
  background: color-mix(in srgb, var(--danger) 10%, transparent);
  border: 1px solid color-mix(in srgb, var(--danger) 35%, transparent);
  border-radius: var(--r-md);
  padding: 14px 16px;
}

.mp-leb-icon {
  width: 22px;
  height: 22px;
  color: var(--danger);
  display: inline-flex;
  flex-shrink: 0;
  margin-top: 1px;
}

.mp-leb-icon :deep(svg) {
  width: 100%;
  height: 100%;
}

.mp-leb-content {
  display: flex;
  flex-direction: column;
  gap: 6px;
  min-width: 0;
  flex: 1;
}

.mp-leb-title strong {
  font-size: 13.5px;
  color: var(--danger);
}

.mp-leb-reason {
  margin: 0;
  font-size: 12.5px;
  color: var(--text);
  font-weight: 550;
  word-break: break-all;
}

.mp-leb-suggestion {
  display: flex;
  align-items: flex-start;
  gap: 6px;
  font-size: 12px;
  color: var(--muted);
  background: var(--surface);
  padding: 6px 10px;
  border-radius: var(--r-xs);
  border: 1px solid color-mix(in srgb, var(--danger) 20%, transparent);
  margin-top: 4px;
}

.mp-leb-tag {
  font-weight: 700;
  color: var(--brand-deep);
  flex-shrink: 0;
}

.mp-log-success-banner {
  display: flex;
  align-items: center;
  gap: 12px;
  background: var(--brand-soft);
  border: 1px solid color-mix(in srgb, var(--brand) 40%, transparent);
  border-radius: var(--r-md);
  padding: 12px 16px;
}

.mp-lsb-icon {
  width: 22px;
  height: 22px;
  color: var(--brand);
  display: inline-flex;
  flex-shrink: 0;
}

.mp-lsb-icon :deep(svg) {
  width: 100%;
  height: 100%;
}

.mp-lsb-content strong {
  font-size: 13.5px;
  color: var(--brand-deep);
  display: block;
}

.mp-lsb-content p {
  margin: 2px 0 0;
  font-size: 12px;
  color: var(--muted);
}

.mp-log-detail-grid {
  display: grid;
  grid-template-columns: repeat(2, 1fr);
  gap: 12px;
}

@media (max-width: 600px) {
  .mp-log-detail-grid {
    grid-template-columns: 1fr;
  }
}

.mp-ld-item {
  background: var(--surface-soft);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 10px 12px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.mp-ld-item label {
  font-size: 11px;
  color: var(--muted);
  font-weight: 600;
}

.mp-ld-val {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12.5px;
  color: var(--text);
}

.mp-dur-col {
  display: flex;
  flex-direction: column;
  gap: 2px;
  line-height: 1.25;
}

.mp-dur-row {
  display: flex;
  align-items: center;
  gap: 4px;
}

.mp-dur-label {
  font-size: 11px;
  color: var(--muted);
  width: 18px;
  flex-shrink: 0;
}

.mp-dur-val {
  font-size: 11.5px;
}

.mp-log-tokens-cell {
  display: flex;
  flex-direction: column;
  gap: 3px;
}

.mp-token-pill-row {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 4px;
}

.mp-token-tag {
  font-size: 10px;
  font-weight: 600;
  padding: 1px 5px;
  border-radius: 3px;
  display: inline-flex;
  align-items: center;
  justify-content: space-between;
  gap: 6px;
  min-width: 0;
}

.mp-token-tag > strong {
  font-weight: 700;
  font-variant-numeric: tabular-nums;
}

.mp-token-tag.is-in {
  background: var(--surface-soft);
  color: var(--muted);
  border: 1px solid var(--line);
}

.mp-token-tag.is-hit {
  background: var(--brand-soft);
  color: var(--brand-deep);
  border: 1px solid color-mix(in srgb, var(--brand) 30%, transparent);
  font-weight: 700;
}

.mp-token-tag.is-out {
  background: color-mix(in srgb, #10b981 12%, transparent);
  color: #059669;
  border: 1px solid color-mix(in srgb, #10b981 25%, transparent);
}

.mp-token-tag.is-think {
  background: color-mix(in srgb, #8b5cf6 12%, transparent);
  color: #7c3aed;
  border: 1px solid color-mix(in srgb, #8b5cf6 25%, transparent);
  font-weight: 700;
}

/* 4 宫格 Token 仪表盘 */
.mp-token-dashboard-grid {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 12px;
}

@media (max-width: 768px) {
  .mp-token-dashboard-grid {
    grid-template-columns: repeat(2, 1fr);
  }
}

.mp-token-card {
  background: var(--surface-soft);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 12px 14px;
  display: flex;
  flex-direction: column;
  gap: 6px;
  transition: all 0.15s ease;
}

.mp-token-card.is-in {
  border-left: 3px solid var(--muted);
}

.mp-token-card.is-hit {
  border-left: 3px solid var(--brand);
  background: color-mix(in srgb, var(--brand) 4%, var(--surface-soft));
}

.mp-token-card.is-think {
  border-left: 3px solid #8b5cf6;
  background: color-mix(in srgb, #8b5cf6 4%, var(--surface-soft));
}

.mp-token-card.is-out {
  border-left: 3px solid #10b981;
  background: color-mix(in srgb, #10b981 4%, var(--surface-soft));
}

.mp-tc-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 4px;
}

.mp-tc-label {
  font-size: 11px;
  font-weight: 700;
  color: var(--muted);
}

.mp-tc-badge {
  font-size: 10px;
  font-weight: 600;
  padding: 1px 5px;
  border-radius: 3px;
  background: var(--surface);
  color: var(--muted);
  border: 1px solid var(--line);
}

.mp-tc-badge.badge-hit {
  background: var(--brand-soft);
  color: var(--brand-deep);
  border-color: color-mix(in srgb, var(--brand) 30%, transparent);
}

.mp-tc-value {
  font-size: 22px;
  font-weight: 800;
  color: var(--text);
  line-height: 1.2;
}

.mp-tc-foot {
  font-size: 11px;
  color: var(--muted);
  display: flex;
  align-items: center;
  gap: 4px;
  flex-wrap: wrap;
}

.mp-tc-divider {
  color: var(--line);
}

/* 选项卡导航栏 */
.mp-detail-tabs-bar {
  display: flex;
  align-items: center;
  gap: 6px;
  border-bottom: 1px solid var(--line);
  padding-bottom: 8px;
  flex-wrap: wrap;
}

.mp-detail-tab-btn {
  border: 1px solid transparent;
  background: var(--surface-soft);
  color: var(--muted);
  font-size: 12px;
  font-weight: 650;
  padding: 6px 12px;
  border-radius: var(--r-md);
  cursor: pointer;
  display: inline-flex;
  align-items: center;
  gap: 6px;
  transition: all 0.15s ease;
}

.mp-detail-tab-btn :deep(svg) {
  width: 14px;
  height: 14px;
}

.mp-detail-tab-btn:hover {
  color: var(--text);
  background: var(--surface-hover);
}

.mp-detail-tab-btn.active {
  background: var(--brand-soft);
  color: var(--brand-deep);
  border-color: color-mix(in srgb, var(--brand) 30%, transparent);
}

.mp-detail-tab-btn.is-error {
  color: var(--danger);
  background: color-mix(in srgb, var(--danger) 8%, transparent);
}

.mp-detail-tab-btn.is-error.active {
  background: color-mix(in srgb, var(--danger) 18%, transparent);
  border-color: color-mix(in srgb, var(--danger) 40%, transparent);
}

.mp-tab-badge {
  font-size: 10px;
  padding: 0 5px;
  border-radius: 999px;
  background: var(--surface);
  border: 1px solid var(--line);
  color: var(--muted);
}

.mp-detail-tab-content {
  display: flex;
  flex-direction: column;
  gap: 14px;
  animation: mp-fade-in 0.15s ease-out;
}

@keyframes mp-fade-in {
  from { opacity: 0.5; transform: translateY(2px); }
  to { opacity: 1; transform: translateY(0); }
}

.mp-log-raw-box {
  background: var(--surface-soft);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 12px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.mp-lrb-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.mp-lrb-header label {
  font-size: 11.5px;
  font-weight: 700;
  color: var(--muted);
}

.mp-lrb-pre {
  margin: 0;
  font-family: var(--font-mono, monospace);
  font-size: 11.5px;
  color: var(--text);
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-xs);
  padding: 12px 14px;
  white-space: pre-wrap;
  word-break: break-all;
  max-height: 360px;
  overflow-y: auto;
  line-height: 1.5;
}

/* 清理数据库日志弹窗样式 */
.mp-clear-logs-box {
  max-width: 540px;
}

.mp-clear-modal-desc {
  margin: 0 0 14px;
  font-size: 13px;
  color: var(--muted);
}

.mp-clear-options-grid {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.mp-clear-option-card {
  background: var(--surface-soft);
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  padding: 14px;
  display: flex;
  flex-direction: column;
  gap: 8px;
  transition: all 0.18s ease;
}

.mp-clear-option-card:hover {
  border-color: var(--line-strong);
  background: var(--surface-hover);
}

.mp-clear-option-card.is-danger {
  border-color: color-mix(in srgb, var(--danger) 25%, transparent);
  background: color-mix(in srgb, var(--danger) 4%, var(--surface-soft));
}

.mp-clear-option-card.is-danger:hover {
  border-color: color-mix(in srgb, var(--danger) 45%, transparent);
  background: color-mix(in srgb, var(--danger) 8%, var(--surface-soft));
}

.mp-coc-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.mp-coc-title-wrap {
  display: flex;
  align-items: center;
  gap: 10px;
}

.mp-coc-icon {
  font-size: 18px;
  line-height: 1;
}

.mp-coc-badge {
  font-size: 10px;
  font-weight: 700;
  padding: 1px 6px;
  border-radius: 999px;
  background: var(--brand-soft);
  color: var(--brand-deep);
  margin-left: 6px;
}

.mp-coc-badge.is-danger {
  background: color-mix(in srgb, var(--danger) 15%, transparent);
  color: var(--danger);
}

.mp-coc-desc {
  margin: 0;
  font-size: 12px;
  color: var(--muted);
  line-height: 1.5;
}

.mp-coc-action {
  display: flex;
  justify-content: flex-end;
  margin-top: 4px;
}
</style>
