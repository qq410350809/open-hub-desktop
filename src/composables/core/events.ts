import { isTauri } from "./ipc";

/**
 * 跨端事件监听：与 @tauri-apps/api/event 的 listen 同签名。
 * - 桌面端：直接转发 Tauri 窗口事件；
 * - 浏览器端（轻量模式 / 远程服务）：经 SSE /api/events 接收 EventBus 广播，
 *   事件信封为 { event, payload }，此处还原为与桌面一致的 { payload } 形状，
 *   业务调用点零改动。
 */
export type UnlistenFn = () => void;
type EventLike<T> = { payload: T };

let source: EventSource | null = null;
const browserListeners = new Map<string, Set<(payload: unknown) => void>>();

function ensureBrowserSource(): void {
  if (source || isTauri) return;
  const token = new URLSearchParams(window.location.search).get("token") ?? "";
  source = new EventSource(`/api/events${token ? `?token=${encodeURIComponent(token)}` : ""}`);
  source.onmessage = (message) => {
    try {
      const envelope = JSON.parse(message.data) as { event: string; payload?: unknown };
      browserListeners.get(envelope.event)?.forEach((handler) => {
        try {
          handler(envelope.payload);
        } catch (error) {
          console.error(`[OpenHub] 事件处理器异常（${envelope.event}）：`, error);
        }
      });
    } catch {
      // 坏帧直接忽略；EventSource 自带断线重连。
    }
  };
  source.onerror = () => {
    // 连接由浏览器自动重连，无需处理。
  };
}

export async function listen<T>(
  event: string,
  handler: (e: EventLike<T>) => void,
): Promise<UnlistenFn> {
  if (isTauri) {
    const tauriEvent = await import("@tauri-apps/api/event");
    return tauriEvent.listen<T>(event, (e) => handler({ payload: e.payload }));
  }

  ensureBrowserSource();
  await new Promise<void>((resolve) => {
    if (!source) return resolve();
    if (source.readyState === EventSource.OPEN) return resolve();
    const onOpen = () => {
      source?.removeEventListener("open", onOpen);
      resolve();
    };
    source.addEventListener("open", onOpen);
    // 兜底：2 秒内未 OPEN 也放行（后台重连期间不阻塞业务挂载）。
    setTimeout(resolve, 2000);
  });

  let set = browserListeners.get(event);
  if (!set) {
    set = new Set();
    browserListeners.set(event, set);
  }
  const wrapped = (payload: unknown) => handler({ payload } as EventLike<T>);
  set.add(wrapped);
  return () => {
    set?.delete(wrapped);
  };
}
