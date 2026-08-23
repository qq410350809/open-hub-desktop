import { invoke } from "@tauri-apps/api/core";
import { browserFallback } from "./browserFallback";

export const isTauri = "__TAURI_INTERNALS__" in window;

/**
 * 跨端 IPC / RPC 命令执行器。
 * 桌面端走 Tauri invoke，浏览器端优先走轻量模式 HTTP RPC，无内核时回退到浏览器本地模拟数据。
 */
export async function runCommand<T>(
  command: string,
  args: Record<string, unknown> = {},
): Promise<T> {
  if (isTauri) return invoke<T>(command, args);

  // 轻量模式：优先走真实内核 HTTP RPC。
  // 仅当内核不可达（未启动服务 / 纯静态预览）时才回退到本地模拟数据。
  try {
    return await rpc<T>(command, args);
  } catch (error) {
    if (error instanceof Error && (error.name === "RpcUnavailable" || error.name === "RpcStaticPreview")) {
      return await browserFallback<T>(command, args);
    }
    throw error;
  }
}

/** 当前登录会话令牌的存取（localStorage 持久化，进程重启后仍有效至过期）。 */
const SESSION_KEY = "openhub_session";

export function getSessionToken(): string {
  try {
    return localStorage.getItem(SESSION_KEY) ?? "";
  } catch {
    return "";
  }
}

export function setSessionToken(token: string): void {
  try {
    if (token) localStorage.setItem(SESSION_KEY, token);
    else localStorage.removeItem(SESSION_KEY);
  } catch {
    /* 隐私模式等场景忽略存储失败 */
  }
}

/** 轻量模式 / 浏览器环境下的 RPC 请求：与桌面端 invoke 共用同一套命令名。 */
export async function rpc<T>(
  command: string,
  args: Record<string, unknown> = {},
): Promise<T> {
  const urlToken = new URLSearchParams(window.location.search).get("token") ?? "";
  const token = getSessionToken() || urlToken;
  let response: Response;
  try {
    response = await fetch("/api/rpc", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        ...(token ? { "X-OpenHub-Token": token } : {}),
      },
      body: JSON.stringify({ command, args }),
    });
  } catch (cause) {
    const error = new Error(`轻量模式内核不可达：${String(cause)}`, { cause });
    error.name = "RpcUnavailable";
    throw error;
  }
  let payload: unknown;
  try {
    payload = await response.json();
  } catch {
    // 返回的不是 JSON：说明当前是纯静态预览（如直接打开 dist/index.html），
    // 没有内核服务，交给模拟数据兜底。
    const error = new Error("轻量模式服务不可用（当前为静态预览）");
    error.name = "RpcStaticPreview";
    throw error;
  }
  const body = payload as { data?: unknown; error?: string };
  if (body.error) throw new Error(body.error);
  return unwrapResult(body.data) as T;
}

/**
 * 与桌面端 invoke 对齐：Rust Result 在 RPC 分发里序列化为 {"Ok":..} / {"Err":..}，
 * 桌面 invoke 会自动解包，这里统一解包，否则各命令拿到包装壳后字段全空。
 */
export function unwrapResult(data: unknown): unknown {
  if (data !== null && typeof data === "object") {
    const record = data as Record<string, unknown>;
    if ("Ok" in record) return record.Ok;
    if ("Err" in record) {
      const message = record.Err;
      throw new Error(typeof message === "string" ? message : JSON.stringify(message));
    }
  }
  return data;
}
