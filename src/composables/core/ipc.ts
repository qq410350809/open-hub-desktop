import { invoke } from "@tauri-apps/api/core";
import { browserFallback } from "./browserFallback";

export type ClientMode = "integrated" | "thin" | "web";

/** Tauri 只表示存在本地壳，不代表所有业务都应走本地 IPC。 */
export const isTauri = "__TAURI_INTERNALS__" in window;
export const clientMode: ClientMode = (() => {
  const configured = (import.meta.env.VITE_OPENHUB_CLIENT_MODE ?? "").toLowerCase();
  if (configured === "thin") return "thin";
  if (configured === "web") return "web";
  return isTauri ? "integrated" : "web";
})();

export const isIntegratedClient = clientMode === "integrated";
export const isThinClient = clientMode === "thin";
export const isWebClient = clientMode === "web";
export const localTokenStatsAvailable = isTauri && !isWebClient;

/** 只能在客户端本地数据平面执行的命令。 */
export const LOCAL_TOKEN_COMMANDS = new Set([
  "get_token_stats",
  "sync_token_data",
  "get_token_usage",
  "get_token_raw_logs",
  "get_token_request_health",
  "get_local_agent_paths",
  "get_token_model_mappings",
  "register_token_model_names",
  "set_token_model_mapping",
  "analyze_token_model_mappings",
]);

function localUnavailable(command: string): Error {
  return new Error(`命令 ${command} 仅在客户端本地可用；当前运行表面不提供本地 Token 数据`);
}

/** 直接调用客户端本地 IPC；不会把本地日志数据发送到服务端。 */
export async function runLocalCommand<T>(
  command: string,
  args: Record<string, unknown> = {},
): Promise<T> {
  if (!isTauri || clientMode === "web") throw localUnavailable(command);
  return invoke<T>(command, args);
}

/** HTTP 层认证失败，仅表示 OpenHub 自身的 RPC/SSE 会话失效。 */
export class AuthExpiredError extends Error {
  readonly code = "AUTH_REQUIRED";

  constructor(message = "登录会话无效或已过期，请重新登录") {
    super(message);
    this.name = "AuthExpiredError";
  }
}

const AUTH_EXPIRED_EVENT = "openhub-auth-expired";
let authExpiredNotified = false;

export function notifyAuthExpired(): void {
  if (authExpiredNotified) return;
  authExpiredNotified = true;
  setSessionToken("");
  window.dispatchEvent(new Event(AUTH_EXPIRED_EVENT));
}

export function resetAuthExpired(): void {
  authExpiredNotified = false;
}

export function onAuthExpired(handler: () => void): () => void {
  const listener = () => handler();
  window.addEventListener(AUTH_EXPIRED_EVENT, listener);
  return () => window.removeEventListener(AUTH_EXPIRED_EVENT, listener);
}

/** 通过同源 HTTP RPC 调用 Web 服务。 */
export async function runServerCommand<T>(
  command: string,
  args: Record<string, unknown> = {},
): Promise<T> {
  try {
    return await rpc<T>(command, args);
  } catch (error) {
    if (error instanceof AuthExpiredError) throw error;
    if (error instanceof Error && (error.name === "RpcUnavailable" || error.name === "RpcStaticPreview")) {
      return await browserFallback<T>(command, args);
    }
    throw error;
  }
}

/**
 * 跨端业务命令：一体式客户端使用本地 IPC；瘦客户端和浏览器使用 Web 服务。
 * 本地 Token 页面使用 runLocalCommand，避免误把本地数据发给服务端。
 */
export async function runCommand<T>(
  command: string,
  args: Record<string, unknown> = {},
): Promise<T> {
  if (clientMode === "integrated") return runLocalCommand<T>(command, args);
  return runServerCommand<T>(command, args);
}

/** 当前登录会话令牌的存取（localStorage 持久化，服务端默认有效 7 天）。 */
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

/** Web RPC 请求；静态资源预览只在网络不可达或非 JSON 响应时使用模拟数据。 */
export async function rpc<T>(
  command: string,
  args: Record<string, unknown> = {},
): Promise<T> {
  const token = getSessionToken();
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
    const error = new Error(`OpenHub 服务不可达：${String(cause)}`, { cause });
    error.name = "RpcUnavailable";
    throw error;
  }

  if (response.status === 401) {
    let message = "登录会话无效或已过期，请重新登录";
    try {
      const body = (await response.json()) as { code?: string; error?: string };
      if (body.code === "AUTH_REQUIRED" || body.error) message = body.error || message;
    } catch {
      // 认证响应体不是 JSON 时仍按 OpenHub 401 处理。
    }
    const error = new AuthExpiredError(message);
    notifyAuthExpired();
    throw error;
  }

  let payload: unknown;
  try {
    payload = await response.json();
  } catch {
    if (!response.ok) {
      const error = new Error(`OpenHub 服务返回 HTTP ${response.status}`);
      error.name = "RpcUnavailable";
      throw error;
    }
    const error = new Error("OpenHub 服务不可用（当前为静态预览）");
    error.name = "RpcStaticPreview";
    throw error;
  }
  const body = payload as { data?: unknown; error?: string; code?: string };
  if (!response.ok) throw new Error(body.error || `OpenHub 服务返回 HTTP ${response.status}`);
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
