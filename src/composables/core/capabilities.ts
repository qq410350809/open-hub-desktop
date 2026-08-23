import { ref } from "vue";
import { isTauri } from "./ipc";

export interface Capabilities {
  /** 本机检测到 Chrome 用户数据目录（会话同步可用） */
  chromeSync: boolean;
  /** 本机检测到 AI 工具日志目录（Token 本地采集可用） */
  tokenLocalLogs: boolean;
  /** 是否已完成探测 */
  loaded: boolean;
}

/**
 * 运行环境能力协商：桌面端视为全能力；
 * 远程服务部署时，依赖「用户本机文件」的功能按探测结果降级隐藏。
 */
export const capabilities = ref<Capabilities>({
  chromeSync: true,
  tokenLocalLogs: true,
  loaded: false,
});

/** 启动时调用一次。静态预览（内核不可达）保持全开，由命令自身错误提示兜底。 */
export async function loadCapabilities(): Promise<void> {
  if (isTauri) {
    capabilities.value = { chromeSync: true, tokenLocalLogs: true, loaded: true };
    return;
  }
  try {
    const response = await fetch("/api/caps");
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const data = (await response.json()) as {
      chromeSync?: boolean;
      tokenLocalLogs?: boolean;
    };
    capabilities.value = {
      chromeSync: data.chromeSync !== false,
      tokenLocalLogs: data.tokenLocalLogs !== false,
      loaded: true,
    };
  } catch {
    capabilities.value = { chromeSync: true, tokenLocalLogs: true, loaded: true };
  }
}
