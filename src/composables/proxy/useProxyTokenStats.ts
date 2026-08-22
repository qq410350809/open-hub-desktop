import { ref, type Ref } from "vue";
import type { RequestHealthReport, TokenUsageReport } from "../../types";
import { runCommand } from "../core/ipc";

/** Token 统计中心「反代模式」报表：与本地模式同构的用量桶 + 请求健康 */
export interface ProxyTokenUsageReport {
  usage: TokenUsageReport;
  health: RequestHealthReport;
}

// 反代模式数据（网关聚合表）：Token 统计中心第二个标签的数据源
const proxyTokenReport: Ref<ProxyTokenUsageReport | null> = ref(null);
const proxyTokenLoading = ref(false);
const proxyTokenError = ref("");
let proxyTokenLoadPromise: Promise<void> | null = null;

async function loadProxyTokenUsage(from?: string, to?: string) {
  if (proxyTokenLoadPromise) {
    return proxyTokenLoadPromise;
  }
  proxyTokenLoading.value = true;
  proxyTokenError.value = "";
  const promise = runCommand<ProxyTokenUsageReport>("get_proxy_token_usage", {
    from: from || null,
    to: to || null,
  })
    .then((report) => {
      proxyTokenReport.value = report;
    })
    .catch((error) => {
      proxyTokenError.value = String(error);
    })
    .finally(() => {
      proxyTokenLoading.value = false;
      proxyTokenLoadPromise = null;
    });
  proxyTokenLoadPromise = promise;
  return promise;
}

export function useProxyTokenStats() {
  return {
    proxyTokenReport,
    proxyTokenLoading,
    proxyTokenError,
    loadProxyTokenUsage,
  };
}
