import { ref } from "vue";
import { clientMode, getSessionToken, isIntegratedClient, isThinClient, localTokenStatsAvailable, notifyAuthExpired } from "./ipc";

export interface Capabilities {
  mode: "integrated" | "thin" | "web" | "server";
  /** 当前客户端是否可读取本机 AI 工具日志。 */
  localTokenStats: boolean;
  /** 兼容旧字段；等价于 localTokenStats。 */
  tokenLocalLogs: boolean;
  chromeSync: boolean;
  proxyTokenStats: boolean;
  desktopIntegration: boolean;
  loaded: boolean;
}

export const capabilities = ref<Capabilities>({
  mode: clientMode,
  localTokenStats: localTokenStatsAvailable,
  tokenLocalLogs: localTokenStatsAvailable,
  chromeSync: isIntegratedClient,
  proxyTokenStats: true,
  desktopIntegration: isIntegratedClient,
  loaded: false,
});

/**
 * 合并本地客户端能力与远程服务能力。
 * 瘦客户端必须保留本地 Token 能力，同时从 Web 服务读取反代能力；
 * 浏览器只接受 Web 服务能力，不能把服务主机的本地文件能力当成自己的能力。
 */
export async function loadCapabilities(): Promise<void> {
  const local = {
    localTokenStats: localTokenStatsAvailable,
    tokenLocalLogs: localTokenStatsAvailable,
    chromeSync: isIntegratedClient,
    desktopIntegration: isIntegratedClient,
  };

  if (isIntegratedClient) {
    capabilities.value = {
      ...local,
      mode: "integrated",
      proxyTokenStats: true,
      loaded: true,
    };
    return;
  }

  try {
    const response = await fetch("/api/caps", {
      headers: {
        ...(getSessionToken() ? { "X-OpenHub-Token": getSessionToken() } : {}),
      },
    });
    if (response.status === 401) {
      notifyAuthExpired();
      return;
    }
    const data = (await response.json()) as {
      mode?: "server" | "web";
      chromeSync?: boolean;
      tokenLocalLogs?: boolean;
      localTokenStats?: boolean;
      proxyTokenStats?: boolean;
      desktopIntegration?: boolean;
    };
    const remoteLocal = data.localTokenStats === true || data.tokenLocalLogs === true;
    capabilities.value = {
      mode: clientMode,
      ...local,
      // 瘦客户端的本地数据能力由本地壳决定，不能被远程 false 覆盖。
      localTokenStats: isThinClient ? local.localTokenStats : remoteLocal,
      tokenLocalLogs: isThinClient ? local.tokenLocalLogs : remoteLocal,
      chromeSync: isThinClient ? local.chromeSync : data.chromeSync === true,
      desktopIntegration: isThinClient ? local.desktopIntegration : data.desktopIntegration === true,
      proxyTokenStats: data.proxyTokenStats !== false,
      loaded: true,
    };
  } catch {
    // 服务暂不可达时，瘦客户端仍可展示本地 Token；Web 端不伪造本地能力。
    capabilities.value = {
      ...local,
      mode: clientMode,
      proxyTokenStats: clientMode !== "web",
      loaded: true,
    };
  }
}
