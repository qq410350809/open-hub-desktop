/** Read-only client connection data. These helpers never read or write client config files. */
export type ClientProtocol = "chat" | "responses" | "anthropic" | "gemini";

export function gatewayClientEndpoints(origin: string) {
  const root = origin.replace(/\/+$/, "");
  return {
    root,
    openai: `${root}/v1`,
    models: `${root}/v1/models`,
    chat: `${root}/v1/chat/completions`,
    responses: `${root}/v1/responses`,
    anthropic: `${root}/v1/messages`,
    gemini: `${root}/v1beta`,
  };
}

export const CLIENT_PROTOCOLS: { id: ClientProtocol; label: string; hint: string }[] = [
  { id: "chat", label: "OpenAI Chat", hint: "选择 OpenAI 兼容协议；Base URL 填到 /v1。" },
  { id: "responses", label: "OpenAI Responses", hint: "选择 Responses 协议（如 Codex）；Base URL 填到 /v1。" },
  { id: "anthropic", label: "Anthropic", hint: "选择 Anthropic 协议；Base URL 填服务根地址，客户端会追加 /v1/messages。" },
  { id: "gemini", label: "Gemini", hint: "选择 Gemini 原生协议；版本化 Base URL 填到 /v1beta，完整请求地址见下方示例。" },
];

export function clientBaseUrl(origin: string, protocol: ClientProtocol): string {
  const urls = gatewayClientEndpoints(origin);
  return protocol === "anthropic" ? urls.root : protocol === "gemini" ? urls.gemini : urls.openai;
}

/** Only accept a real OpenAI models response; HTML, RPC envelopes and errors are not empty lists. */
export function parseClientModels(value: unknown): string[] {
  if (!value || typeof value !== "object" || !Array.isArray((value as { data?: unknown }).data)) {
    throw new Error("模型目录格式不正确，请确认地址指向 OpenHub 模型网关");
  }
  const items = (value as { data: unknown[] }).data;
  if (items.some((item) => !item || typeof item !== "object" ||
    typeof (item as { id?: unknown }).id !== "string" || !(item as { id: string }).id.trim())) {
    throw new Error("模型目录包含无效的模型 ID，请刷新后重试");
  }
  return [...new Set(items.map((item) => (item as { id: string }).id))].sort();
}

export async function fetchClientModels(
  origin: string,
  apiKey: string,
  signal: AbortSignal,
  request: typeof fetch = fetch,
): Promise<string[]> {
  const response = await request(gatewayClientEndpoints(origin).models, {
    headers: { Authorization: `Bearer ${apiKey}` },
    cache: "no-store",
    credentials: "omit",
    signal,
  });
  if (!response.ok) {
    if (response.status === 401) throw new Error("网关 API Key 校验失败，请重新读取服务配置");
    if (response.status === 503) throw new Error("模型网关尚未启用，请先启动服务");
    throw new Error(`读取网关模型目录失败（HTTP ${response.status}）`);
  }
  let value: unknown;
  try { value = await response.json(); }
  catch { throw new Error("模型目录没有返回 JSON，请确认当前服务地址可用"); }
  return parseClientModels(value);
}

function shellQuote(value: string): string {
  return `'${value.replace(/'/g, `'"'"'`)}'`;
}

/** Example uses an environment variable for the Key; copying never embeds a real credential. */
export function clientRequestExample(origin: string, protocol: ClientProtocol, model: string): string {
  const urls = gatewayClientEndpoints(origin);
  let endpoint: string;
  let payload: unknown;
  let headers = ['  -H "Authorization: Bearer $OPENHUB_API_KEY"'];
  switch (protocol) {
    case "responses":
      endpoint = urls.responses;
      payload = { model, input: "Hello" };
      break;
    case "anthropic":
      endpoint = urls.anthropic;
      headers = ['  -H "x-api-key: $OPENHUB_API_KEY"', '  -H "anthropic-version: 2023-06-01"'];
      payload = { model, max_tokens: 1024, messages: [{ role: "user", content: "Hello" }] };
      break;
    case "gemini":
      // Keep alias/model slashes as path separators, encode the rest of the ID.
      endpoint = `${urls.gemini}/models/${model.split("/").map(encodeURIComponent).join("/")}:generateContent`;
      headers = ['  -H "x-goog-api-key: $OPENHUB_API_KEY"'];
      payload = { contents: [{ role: "user", parts: [{ text: "Hello" }] }] };
      break;
    default:
      endpoint = urls.chat;
      payload = { model, messages: [{ role: "user", content: "Hello" }] };
  }
  return [
    '# 将 OPENHUB_API_KEY 环境变量设为复制的网关 API Key',
    `curl ${shellQuote(endpoint)}`,
    ...headers,
    '  -H "Content-Type: application/json"',
    `  -d ${shellQuote(JSON.stringify(payload, null, 2))}`,
  ].map((line, index, lines) => index > 0 && index < lines.length - 1 ? `${line} \\` : line).join("\n");
}

export function clientConnectionJson(origin: string, protocol: ClientProtocol, model: string): string {
  return JSON.stringify({
    protocol: { chat: "openai", responses: "openai-responses", anthropic: "anthropic", gemini: "gemini" }[protocol],
    baseUrl: clientBaseUrl(origin, protocol),
    apiKey: "YOUR_OPENHUB_API_KEY",
    model,
  }, null, 2);
}
