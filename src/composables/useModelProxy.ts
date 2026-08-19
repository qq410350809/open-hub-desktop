import { ref } from "vue";
import { runCommand } from "./useLibrary";
import { useToast } from "./useToast";

export interface ChannelConfig {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  protocol: string;
  upstreamUrl: string;
  apiKey?: string;
  useProxyPool: boolean;
}

export interface OpencodeProxyConfig {
  enabled: boolean;
  port: number;
  apiKey: string;
  channels: ChannelConfig[];
  timeoutSeconds: number;
  recordRequestBody?: boolean;
}

export interface OpencodeProxyStatus {
  running: boolean;
  port: number;
  url: string;
  totalRequests: number;
  successfulRequests: number;
  failedRequests: number;
  uptimeSeconds: number;
  modelsCount: number;
  channelsCount: number;
  totalPromptTokens?: number;
  totalCompletionTokens?: number;
  totalReasoningTokens?: number;
  totalCacheHitTokens?: number;
  totalTokens?: number;
  todayTotalTokens?: number;
}

export interface ProxyRequestLog {
  id: string;
  timestamp: string;
  method: string;
  path: string;
  channelId: string;
  model: string;
  stream: boolean;
  statusCode: number;
  durationMs: number;
  ttftMs?: number;
  promptTokens?: number;
  promptCacheHitTokens?: number;
  promptCacheMissTokens?: number;
  completionTokens?: number;
  reasoningTokens?: number;
  totalTokens?: number;
  errorMessage?: string;
  requestBody?: string;
  responseBody?: string;
  nodeName?: string;
}

const proxyConfig = ref<OpencodeProxyConfig>({
  enabled: true,
  port: 8088,
  apiKey: "",
  channels: [
    {
      id: "opencode",
      name: "OpenCode",
      description: "OpenCode 官方 Public 免费直连通道，免 Key 访问在线优质编码与推理模型",
      enabled: true,
      protocol: "opencode",
      upstreamUrl: "https://opencode.ai/zen/v1",
      apiKey: "public",
      useProxyPool: false,
    },
  ],
  timeoutSeconds: 300,
});

const proxyStatus = ref<OpencodeProxyStatus>({
  running: false,
  port: 8088,
  url: "http://127.0.0.1:8088/v1",
  totalRequests: 0,
  successfulRequests: 0,
  failedRequests: 0,
  uptimeSeconds: 0,
  modelsCount: 0,
  channelsCount: 1,
  totalPromptTokens: 0,
  totalCompletionTokens: 0,
  totalReasoningTokens: 0,
  totalCacheHitTokens: 0,
  totalTokens: 0,
  todayTotalTokens: 0,
});

const proxyLoading = ref(false);
const savingConfig = ref(false);
const togglingServer = ref(false);
const testingHealth = ref(false);
const fetchingModels = ref(false);
const modelsList = ref<string[]>([]);
const healthResult = ref<any>(null);
const healthResultTime = ref<string>("");
const proxyLogs = ref<ProxyRequestLog[]>([]);
const loadingLogs = ref(false);

export function useModelProxy() {
  const { showToast } = useToast();

  async function refreshStatus() {
    try {
      const status = await runCommand<OpencodeProxyStatus>("get_opencode_proxy_status");
      if (status) proxyStatus.value = status;
    } catch {
      // background polling
    }
  }

  async function loadProxyData() {
    proxyLoading.value = true;
    try {
      const [cfg, status] = await Promise.all([
        runCommand<OpencodeProxyConfig>("get_opencode_proxy_config"),
        runCommand<OpencodeProxyStatus>("get_opencode_proxy_status"),
      ]);
      if (cfg) {
        if (!cfg.channels || cfg.channels.length === 0) {
          cfg.channels = [
            {
              id: "opencode",
              name: "OpenCode",
              description: "OpenCode 官方 Public 免费直连通道，免 Key 访问在线优质编码与推理模型",
              enabled: true,
              protocol: "opencode",
              upstreamUrl: "https://opencode.ai/zen/v1",
              apiKey: "public",
              useProxyPool: false,
            },
          ];
        }
        proxyConfig.value = cfg;
      }
      if (status) proxyStatus.value = status;
    } catch (e) {
      console.error("加载模型反代配置失败:", e);
    } finally {
      proxyLoading.value = false;
    }
  }

  async function saveConfig(newConfig: OpencodeProxyConfig) {
    savingConfig.value = true;
    try {
      const status = await runCommand<OpencodeProxyStatus>("save_opencode_proxy_config_cmd", {
        config: newConfig,
      });
      if (status) proxyStatus.value = status;
      proxyConfig.value = { ...newConfig };
      showToast("反代配置与渠道设置已保存");
      return true;
    } catch (e) {
      showToast(`保存配置失败: ${String(e)}`, true);
      return false;
    } finally {
      savingConfig.value = false;
    }
  }

  async function toggleServer() {
    togglingServer.value = true;
    try {
      if (proxyStatus.value.running) {
        const status = await runCommand<OpencodeProxyStatus>("stop_opencode_proxy");
        if (status) proxyStatus.value = status;
        showToast("模型反代服务已停止");
      } else {
        const status = await runCommand<OpencodeProxyStatus>("start_opencode_proxy");
        if (status) proxyStatus.value = status;
        showToast(`模型反代服务已在端口 ${proxyStatus.value.port} 启动`);
      }
    } catch (e) {
      showToast(`操作服务失败: ${String(e)}`, true);
    } finally {
      togglingServer.value = false;
    }
  }

  async function testHealth() {
    testingHealth.value = true;
    healthResult.value = null;
    try {
      const res = await runCommand<any>("test_opencode_proxy_health");
      healthResult.value = res;
      healthResultTime.value = new Date().toLocaleTimeString();
      showToast("健康检查通过 (200 OK)");
      return res;
    } catch (e) {
      healthResult.value = { error: String(e) };
      healthResultTime.value = new Date().toLocaleTimeString();
      showToast(`健康检查未通过: ${String(e)}`, true);
      return null;
    } finally {
      testingHealth.value = false;
    }
  }

  async function refreshModels() {
    fetchingModels.value = true;
    try {
      const list = await runCommand<string[]>("fetch_opencode_models");
      if (Array.isArray(list)) {
        modelsList.value = list;
        showToast(`成功从上游获取 ${list.length} 个可用免费模型`);
      }
    } catch (e) {
      console.warn("拉取模型失败:", e);
    } finally {
      fetchingModels.value = false;
    }
  }

  async function fetchLogs() {
    loadingLogs.value = true;
    try {
      const list = await runCommand<ProxyRequestLog[]>("get_opencode_proxy_logs", { limit: 200 });
      if (Array.isArray(list)) {
        proxyLogs.value = list;
      }
    } catch (e) {
      console.warn("获取请求日志失败:", e);
    } finally {
      loadingLogs.value = false;
    }
  }

  async function clearLogs() {
    try {
      await runCommand("clear_opencode_proxy_logs");
      proxyLogs.value = [];
      showToast("已清空请求日志");
    } catch (e) {
      showToast(`清空日志失败: ${String(e)}`, true);
    }
  }

  async function copyProxyUrl() {
    const url = proxyStatus.value.url || `http://127.0.0.1:${proxyConfig.value.port}/v1`;
    try {
      await navigator.clipboard.writeText(url);
      showToast(`已复制 Base URL: ${url}`);
    } catch {
      showToast("复制失败", true);
    }
  }

  async function copyProxyKey() {
    const key = proxyConfig.value.apiKey || "";
    if (!key) {
      showToast("当前未配置访问密钥（免密直连）");
      return;
    }
    try {
      await navigator.clipboard.writeText(key);
      showToast("已复制访问密钥到剪贴板");
    } catch {
      showToast("复制失败", true);
    }
  }

  return {
    proxyConfig,
    proxyStatus,
    proxyLoading,
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
  };
}
