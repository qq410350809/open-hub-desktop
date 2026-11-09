/**
 * 前端全局常量集中管理
 *
 * 将散落在各 composable / 组件中的硬编码值统一提取到此处，
 * 便于后续修改端口、URL 或默认配置时一处生效、处处生效。
 */

import type { PublicLibrarySourceOption } from "./types";

// ── 模型反代（同源 Web 服务端点）──

/** dev 隔离形态端口（后端 debug 构建 / OPENHUB_PROFILE=dev 时使用）。 */
export const DEV_SERVICE_PORT = 17996;

/**
 * 主 Web 服务默认端口；模型 API 与 Web UI 共用该端口。
 * vite dev server（tauri dev）下前端走 dev 隔离端口，与正式版（17896）互不抢占；
 * 实际监听端口以后端 status 返回为准，此处仅作初始回退值。
 */
export const DEFAULT_SERVICE_PORT = import.meta.env.DEV ? DEV_SERVICE_PORT : 17896;


export const API_PATH_V1 = "/v1";

/** OpenAI Responses API 路径 */
export const API_PATH_RESPONSES = "/v1/responses";

/** Gemini 兼容 API 路径 */
export const API_PATH_GEMINI = "/v1/gemini";

/** Claude Messages 兼容 API 路径 */
export const API_PATH_MESSAGES = "/v1/messages";

/** 当前 Web 服务的模型 API Origin。桌面内嵌 Web 与浏览器访问均使用当前页面源。 */
export function modelApiOrigin(): string {
  return window.location.origin;
}

/** 构建同源 OpenAI 兼容 API URL */
export function buildProxyBaseUrl(): string {
  return `${modelApiOrigin()}${API_PATH_V1}`;
}

/** 构建同源 Responses API URL */
export function buildProxyResponsesUrl(): string {
  return `${modelApiOrigin()}${API_PATH_RESPONSES}`;
}

/** 构建同源 Gemini API URL */
export function buildProxyGeminiUrl(): string {
  return `${modelApiOrigin()}${API_PATH_GEMINI}`;
}

/** 构建同源 Claude Messages API URL */
export function buildProxyMessagesUrl(): string {
  return `${modelApiOrigin()}${API_PATH_MESSAGES}`;
}


// ── 代理池（Proxy Pool）──

/** 默认忽略代理的地址列表 */
export const DEFAULT_IGNORE_ADDRESSES =
  "localhost,127.0.0.1,::1,.local,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16";

/** 代理节点测速 URL */
export const PROXY_SPEED_TEST_URL = "http://www.gstatic.com/generate_204";

// ── 同步 ──

/** 远程登录地址 */
export const REMOTE_LOGIN_URL = "https://ldoh.105117.xyz/";

/**
 * 「同步公共库」的来源清单。
 *
 * ldoh 是主来源（站点 id 稳定、字段最全，但要在 Chrome 登录）；
 * 白嫖公益站目录是补充来源（公开可抓、无需登录，能从详情页补出 API 基址）。
 * 顺序即同步顺序：先 ldoh 再白嫖，后者与前者重合的站点按地址键合并。
 */
export const PUBLIC_LIBRARY_SOURCES: PublicLibrarySourceOption[] = [
  {
    id: "ldoh",
    label: "ldoh 公益站目录",
    requiresLogin: true,
    hint: "字段最全，需要在 Chrome 登录",
  },
  {
    id: "baipiao",
    label: "白嫖公益站目录",
    requiresLogin: false,
    hint: "公开目录，可从详情页补 API 基址",
  },
];

// ── 远程访问 ──
