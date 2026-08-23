import { clientMode, getSessionToken, isIntegratedClient, notifyAuthExpired, AuthExpiredError } from "./ipc";

/**
 * 跨端事件监听：与 @tauri-apps/api/event 的 listen 同签名。
 * - 集成式客户端：默认直接转发 Tauri 窗口事件；
 * - 瘦客户端：默认经 SSE 接收远程服务事件，本地事件显式传入 `{ local: true }`；
 * - 浏览器端：经带 Session Header 的 fetch 流接收 EventBus 广播。
 */
export type UnlistenFn = () => void;
type EventLike<T> = { payload: T };
type ListenOptions = { local?: boolean };

let streamController: AbortController | null = null;
let streamReady: Promise<void> | null = null;
let reconnectTimer: number | null = null;
const browserListeners = new Map<string, Set<(payload: unknown) => void>>();

function dispatchMessage(data: string): void {
  try {
    const envelope = JSON.parse(data) as { event: string; payload?: unknown };
    browserListeners.get(envelope.event)?.forEach((handler) => {
      try {
        handler(envelope.payload);
      } catch (error) {
        console.error(`[OpenHub] 事件处理器异常（${envelope.event}）：`, error);
      }
    });
  } catch {
    // 坏帧直接忽略。
  }
}

function parseSseFrame(frame: string): void {
  const data = frame
    .split(/\r?\n/)
    .filter((line) => line.startsWith("data:"))
    .map((line) => line.slice(5).trimStart())
    .join("\n");
  if (data) dispatchMessage(data);
}

async function consumeStream(controller: AbortController): Promise<void> {
  const token = getSessionToken();
  const response = await fetch("/api/events", {
    headers: {
      Accept: "text/event-stream",
      ...(token ? { "X-OpenHub-Token": token } : {}),
    },
    signal: controller.signal,
  });
  if (response.status === 401) {
    notifyAuthExpired();
    throw new AuthExpiredError();
  }
  if (!response.ok) throw new Error(`OpenHub 事件服务返回 HTTP ${response.status}`);
  if (!response.body) throw new Error("OpenHub 事件服务未返回流");

  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  while (!controller.signal.aborted) {
    const { done, value } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    const frames = buffer.split(/\r?\n\r?\n/);
    buffer = frames.pop() ?? "";
    frames.forEach(parseSseFrame);
  }
  if (buffer.trim()) parseSseFrame(buffer);
}

function scheduleReconnect(): void {
  if (reconnectTimer !== null || browserListeners.size === 0) return;
  reconnectTimer = window.setTimeout(() => {
    reconnectTimer = null;
    void ensureBrowserSource();
  }, 1500);
}

function ensureBrowserSource(): Promise<void> {
  if (isIntegratedClient) return Promise.resolve();
  if (streamReady) return streamReady;

  const controller = new AbortController();
  streamController = controller;
  let resolveReady!: () => void;
  const ready = new Promise<void>((resolve) => {
    resolveReady = resolve;
  });
  streamReady = ready;
  void consumeStream(controller)
    .then(() => {
      resolveReady();
      if (!controller.signal.aborted) scheduleReconnect();
    })
    .catch((error: unknown) => {
      if (error instanceof AuthExpiredError || (error instanceof DOMException && error.name === "AbortError")) {
        resolveReady();
        return;
      }
      resolveReady();
      scheduleReconnect();
    })
    .finally(() => {
      if (streamController === controller) {
        streamController = null;
        streamReady = null;
      }
    });
  // 短暂网络故障不应阻塞监听器挂载，流会在后台重连。
  return ready;
}

export function resetEventSource(): void {
  if (reconnectTimer !== null) {
    window.clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  streamController?.abort();
  streamController = null;
  streamReady = null;
}

export async function listen<T>(
  event: string,
  handler: (e: EventLike<T>) => void,
  options: ListenOptions = {},
): Promise<UnlistenFn> {
  const useTauriEvent = isIntegratedClient || (clientMode === "thin" && options.local === true);
  if (useTauriEvent) {
    const tauriEvent = await import("@tauri-apps/api/event");
    return tauriEvent.listen<T>(event, (e) => handler({ payload: e.payload }));
  }

  let set = browserListeners.get(event);
  if (!set) {
    set = new Set();
    browserListeners.set(event, set);
  }
  const wrapped = (payload: unknown) => handler({ payload } as EventLike<T>);
  set.add(wrapped);
  await ensureBrowserSource();
  return () => {
    set?.delete(wrapped);
    if (set?.size === 0) browserListeners.delete(event);
    if (browserListeners.size === 0) resetEventSource();
  };
}
