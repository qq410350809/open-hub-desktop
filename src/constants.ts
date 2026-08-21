/**
 * 前端全局常量集中管理
 *
 * 将散落在各 composable / 组件中的硬编码值统一提取到此处，
 * 便于后续修改端口、URL 或默认配置时一处生效、处处生效。
 */

// ── 模型反代（Model Proxy）──

/** 模型反代默认监听端口 */
export const DEFAULT_PROXY_PORT = 8088;

/** 本地回环地址前缀（统一拼接 URL 使用） */
export const LOCALHOST = "127.0.0.1";

/** OpenAI 兼容 API 路径前缀 */
export const API_PATH_V1 = "/v1";

/** Gemini 兼容 API 路径 */
export const API_PATH_GEMINI = "/v1/gemini";

/** Claude Messages 兼容 API 路径 */
export const API_PATH_MESSAGES = "/v1/messages";

/** 构建本地反代 Base URL */
export function buildProxyBaseUrl(port: number): string {
  return `http://${LOCALHOST}:${port}${API_PATH_V1}`;
}

/** 构建本地反代 Gemini URL */
export function buildProxyGeminiUrl(port: number): string {
  return `http://${LOCALHOST}:${port}${API_PATH_GEMINI}`;
}

/** 构建本地反代 Claude Messages URL */
export function buildProxyMessagesUrl(port: number): string {
  return `http://${LOCALHOST}:${port}${API_PATH_MESSAGES}`;
}

/** OpenCode 官方上游 URL */
export const OPENCODE_UPSTREAM_URL = "https://opencode.ai/zen/v1";

// ── 代理池（Proxy Pool）──

/** 默认忽略代理的地址列表 */
export const DEFAULT_IGNORE_ADDRESSES =
  "localhost,127.0.0.1,::1,.local,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16";

/** 代理节点测速 URL */
export const PROXY_SPEED_TEST_URL = "http://www.gstatic.com/generate_204";

// ── 同步 ──

/** 远程登录地址 */
export const REMOTE_LOGIN_URL = "https://ldoh.105117.xyz/";

// ── 轻量模式内核 ──

/** 轻量模式内核默认端口 */
export const KERNEL_DEFAULT_PORT = 17896;
