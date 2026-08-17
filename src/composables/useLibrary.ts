import { invoke } from "@tauri-apps/api/core";
import { ref } from "vue";
import { emptySite, type ChromeUsageSite, type LibraryData, type ModelCatalogDetail, type ModelCatalogItem, type ModelCatalogProvider, type ModelCatalogSnapshot, type ModelCatalogSyncResult, type ProxyIpAnalysis, type ProxyPoolState, type SiteRecord } from "../types";

export const isTauri = "__TAURI_INTERNALS__" in window;

let browserData: LibraryData | null = null;
const previewProviders: ModelCatalogProvider[] = [
  { id: "openai", name: "OpenAI", api: "https://api.openai.com/v1", doc: "https://platform.openai.com/docs", tier: "lab", subscription: false, count: 258, dateModified: "2026-08-17" },
  { id: "anthropic", name: "Anthropic", api: "https://api.anthropic.com/v1", doc: "https://docs.anthropic.com", tier: "lab", subscription: false, count: 179, dateModified: "2026-08-17" },
  { id: "google", name: "Google", api: null, doc: "https://ai.google.dev", tier: "lab", subscription: false, count: 162, dateModified: "2026-08-17" },
  { id: "deepseek", name: "DeepSeek", api: "https://api.deepseek.com", doc: "https://api-docs.deepseek.com", tier: "lab", subscription: false, count: 92, dateModified: "2026-08-17" },
  { id: "openrouter", name: "OpenRouter", api: "https://openrouter.ai/api/v1", doc: "https://openrouter.ai/docs", tier: "gateway", subscription: false, count: 320, dateModified: "2026-08-17" },
  { id: "deepinfra", name: "Deep Infra", api: null, doc: "https://deepinfra.com", tier: "cloud", subscription: false, count: 58, dateModified: "2026-08-17" },
  { id: "siliconflow", name: "SiliconFlow", api: "https://api.siliconflow.cn/v1", doc: "https://docs.siliconflow.cn", tier: "cloud", subscription: false, count: 110, dateModified: "2026-08-17" },
  { id: "nano-gpt", name: "NanoGPT", api: "https://nano-gpt.com/api/v1", doc: "https://docs.nano-gpt.com", tier: "gateway", subscription: false, count: 601, dateModified: "2026-08-17" },
];

const previewModels: ModelCatalogItem[] = [
  {
    id: "openai/gpt-5-turbo",
    slug: "gpt5turbo",
    name: "GPT-5 Turbo",
    lab: "openai",
    kind: "text",
    family: "gpt-5",
    knowledge: "2025-12",
    status: "ga",
    openWeights: false,
    reasoning: true,
    toolCall: true,
    attachment: true,
    structured: true,
    temperature: true,
    inputModalities: ["text", "image", "audio", "video", "pdf"],
    contextLength: 256_000,
    contextMin: 128_000,
    contextMax: 256_000,
    maxOutputTokens: 32_768,
    refProvider: "openai",
    refOfficial: true,
    refInputCost: 1.5,
    refOutputCost: 6.0,
    refCacheReadCost: 0.375,
    minProvider: "openrouter",
    minInputCost: 1.25,
    minOutputCost: 5.0,
    minCacheReadCost: 0.3,
    priceSpread: 1.2,
    blendedMin: 2.18,
    blendedTrusted: 2.62,
    blendedRef: 2.625,
    hostCount: 28,
    pricedHostCount: 24,
    freeHostCount: 2,
    subHostCount: 2,
    hostProviders: ["openai", "openrouter", "deepinfra", "nano-gpt"],
    aaIdx: 94.2,
    aaCoding: 96.5,
    aaAgentic: 92.1,
    aaSpeed: 112.0,
    aaTtft: 0.65,
    aaTaskCost: 0.182,
    benchmarkCount: 35,
    releaseDate: "2026-04-10",
    lastUpdated: "2026-08-17",
  },
  {
    id: "anthropic/claude-3-7-sonnet",
    slug: "claude37sonnet",
    name: "Claude 3.7 Sonnet",
    lab: "anthropic",
    kind: "text",
    family: "claude-sonnet",
    knowledge: "2026-02",
    status: "ga",
    openWeights: false,
    reasoning: true,
    toolCall: true,
    attachment: true,
    structured: true,
    temperature: true,
    inputModalities: ["text", "image", "pdf"],
    contextLength: 200_000,
    contextMin: 200_000,
    contextMax: 200_000,
    maxOutputTokens: 64_000,
    refProvider: "anthropic",
    refOfficial: true,
    refInputCost: 3.0,
    refOutputCost: 15.0,
    refCacheReadCost: 0.3,
    minProvider: "kilo",
    minInputCost: 2.7,
    minOutputCost: 13.5,
    minCacheReadCost: 0.27,
    priceSpread: 1.11,
    blendedMin: 5.4,
    blendedTrusted: 6.0,
    blendedRef: 6.0,
    hostCount: 35,
    pricedHostCount: 32,
    freeHostCount: 1,
    subHostCount: 2,
    hostProviders: ["anthropic", "openrouter", "nano-gpt", "deepinfra"],
    aaIdx: 96.8,
    aaCoding: 98.2,
    aaAgentic: 95.4,
    aaSpeed: 88.0,
    aaTtft: 0.82,
    aaTaskCost: 0.245,
    benchmarkCount: 42,
    releaseDate: "2026-02-24",
    lastUpdated: "2026-08-17",
  },
  {
    id: "deepseek/deepseek-r1",
    slug: "deepseekr1",
    name: "DeepSeek R1",
    lab: "deepseek",
    kind: "text",
    family: "deepseek-r1",
    knowledge: "2024-12",
    status: "ga",
    openWeights: true,
    reasoning: true,
    toolCall: true,
    attachment: false,
    structured: true,
    temperature: true,
    inputModalities: ["text"],
    contextLength: 128_000,
    contextMin: 64_000,
    contextMax: 128_000,
    maxOutputTokens: 32_768,
    refProvider: "deepseek",
    refOfficial: true,
    refInputCost: 0.55,
    refOutputCost: 2.19,
    refCacheReadCost: 0.14,
    minProvider: "siliconflow",
    minInputCost: 0.28,
    minOutputCost: 1.10,
    minCacheReadCost: 0.07,
    priceSpread: 2.0,
    blendedMin: 0.485,
    blendedTrusted: 0.96,
    blendedRef: 0.96,
    hostCount: 56,
    pricedHostCount: 48,
    freeHostCount: 4,
    subHostCount: 4,
    hostProviders: ["deepseek", "siliconflow", "openrouter", "deepinfra", "nano-gpt"],
    aaIdx: 91.5,
    aaCoding: 94.0,
    aaAgentic: 88.6,
    aaSpeed: 64.0,
    aaTtft: 1.12,
    aaTaskCost: 0.089,
    benchmarkCount: 30,
    releaseDate: "2025-01-20",
    lastUpdated: "2026-08-17",
  },
  {
    id: "zhipuai/glm-5.2",
    slug: "glm52",
    name: "GLM-5.2",
    lab: "zhipuai",
    kind: "text",
    family: "glm",
    knowledge: "2026-05",
    status: "ga",
    openWeights: true,
    reasoning: true,
    toolCall: true,
    attachment: false,
    structured: true,
    temperature: true,
    inputModalities: ["text", "image"],
    contextLength: 1_000_000,
    contextMin: 96_000,
    contextMax: 1_049_000,
    maxOutputTokens: 131_072,
    refProvider: "zai",
    refOfficial: true,
    refInputCost: 1.4,
    refOutputCost: 4.4,
    refCacheReadCost: 0.26,
    minProvider: "nano-gpt",
    minInputCost: 0.42,
    minOutputCost: 1.32,
    minCacheReadCost: 0.078,
    priceSpread: 5.5,
    blendedMin: 0.645,
    blendedTrusted: 1.075,
    blendedRef: 2.15,
    hostCount: 80,
    pricedHostCount: 69,
    freeHostCount: 3,
    subHostCount: 6,
    hostProviders: ["zai", "siliconflow", "openrouter", "nano-gpt", "deepinfra"],
    aaIdx: 52.6,
    aaCoding: 68.8,
    aaAgentic: 45.7,
    aaSpeed: 139.0,
    aaTtft: 1.37,
    aaTaskCost: 0.3206,
    benchmarkCount: 19,
    releaseDate: "2026-06-13",
    lastUpdated: "2026-08-17",
  },
  {
    id: "google/gemini-3-pro-image",
    slug: "gemini3proimage",
    name: "Gemini 3 Pro Image",
    lab: "google",
    kind: "image",
    family: "gemini-pro",
    knowledge: "2025-01",
    status: "ga",
    openWeights: false,
    reasoning: true,
    toolCall: false,
    attachment: true,
    structured: true,
    temperature: true,
    inputModalities: ["text", "image", "pdf"],
    contextLength: 65_536,
    contextMin: 65_536,
    contextMax: 131_072,
    maxOutputTokens: 32_768,
    refProvider: "google",
    refOfficial: true,
    refInputCost: 1.25,
    refOutputCost: 5.0,
    refCacheReadCost: 0.31,
    minProvider: "openrouter",
    minInputCost: 1.0,
    minOutputCost: 4.0,
    minCacheReadCost: 0.25,
    priceSpread: 1.25,
    blendedMin: 1.75,
    blendedTrusted: 2.18,
    blendedRef: 2.18,
    hostCount: 12,
    pricedHostCount: 10,
    freeHostCount: 1,
    subHostCount: 1,
    hostProviders: ["google", "openrouter"],
    aaIdx: null,
    aaCoding: null,
    aaAgentic: null,
    aaSpeed: null,
    aaTtft: null,
    aaTaskCost: null,
    benchmarkCount: 0,
    releaseDate: "2026-03-15",
    lastUpdated: "2026-08-17",
  },
  {
    id: "alibaba/qwen3-embedding-8b",
    slug: "qwen3embedding8b",
    name: "Qwen3-Embedding-8B",
    lab: "alibaba",
    kind: "embedding",
    family: "qwen",
    knowledge: "2024-12",
    status: "ga",
    openWeights: true,
    reasoning: false,
    toolCall: false,
    attachment: false,
    structured: false,
    temperature: false,
    inputModalities: ["text"],
    contextLength: 40_960,
    contextMin: 32_000,
    contextMax: 40_960,
    maxOutputTokens: 4_096,
    refProvider: "alibaba",
    refOfficial: true,
    refInputCost: 0.05,
    refOutputCost: 0,
    refCacheReadCost: 0,
    minProvider: "siliconflow",
    minInputCost: 0.02,
    minOutputCost: 0,
    minCacheReadCost: 0,
    priceSpread: 2.5,
    blendedMin: 0.015,
    blendedTrusted: 0.035,
    blendedRef: 0.035,
    hostCount: 18,
    pricedHostCount: 15,
    freeHostCount: 2,
    subHostCount: 1,
    hostProviders: ["alibaba", "siliconflow", "deepinfra"],
    aaIdx: null,
    aaCoding: null,
    aaAgentic: null,
    aaSpeed: null,
    aaTtft: null,
    aaTaskCost: null,
    benchmarkCount: 0,
    releaseDate: "2025-06-20",
    lastUpdated: "2026-08-17",
  },
];

const previewGeneratedModels: ModelCatalogItem[] = Array.from({ length: 94 }, (_, index) => {
  const labs = ["openai", "anthropic", "google", "deepseek", "alibaba", "zhipuai", "mistral", "meta"];
  const lab = labs[index % labs.length];
  const kinds = ["text", "text", "text", "image", "video", "audio", "embedding", "rerank"];
  const kind = kinds[index % kinds.length];
  const costIn = Number((((index % 9) + 1) * 0.25).toFixed(3));
  const costOut = kind === "embedding" || kind === "rerank" ? 0 : Number((costIn * 3.5).toFixed(3));

  return {
    id: `${lab}/model-preview-${index + 1}`,
    slug: `modelpreview${index + 1}`,
    name: `${lab.toUpperCase()} Model ${index + 1}`,
    lab,
    kind,
    family: `${lab}-gen`,
    knowledge: "2025-06",
    status: index % 15 === 0 ? "beta" : index % 25 === 0 ? "deprecated" : "ga",
    openWeights: index % 2 === 0,
    reasoning: index % 3 === 0,
    toolCall: kind === "text",
    attachment: kind === "text" || kind === "image",
    structured: kind === "text",
    temperature: kind === "text",
    inputModalities: ["text"],
    contextLength: 32_000 * ((index % 8) + 1),
    contextMin: 32_000,
    contextMax: 32_000 * ((index % 8) + 1),
    maxOutputTokens: 8_192 * ((index % 4) + 1),
    refProvider: lab,
    refOfficial: true,
    refInputCost: costIn,
    refOutputCost: costOut,
    refCacheReadCost: Number((costIn * 0.2).toFixed(4)),
    minProvider: "nano-gpt",
    minInputCost: Number((costIn * 0.7).toFixed(3)),
    minOutputCost: Number((costOut * 0.7).toFixed(3)),
    minCacheReadCost: Number((costIn * 0.15).toFixed(4)),
    priceSpread: 1.43,
    blendedMin: Number((costIn * 0.75).toFixed(3)),
    blendedTrusted: Number((costIn * 0.9).toFixed(3)),
    blendedRef: costIn,
    hostCount: ((index % 20) + 2) * 3,
    pricedHostCount: ((index % 18) + 2) * 2,
    freeHostCount: index % 4 === 0 ? 2 : 0,
    subHostCount: index % 5 === 0 ? 1 : 0,
    hostProviders: [lab, "openrouter", "deepinfra", "nano-gpt", "siliconflow"],
    aaIdx: index % 4 === 0 ? 70 + (index % 25) : null,
    aaCoding: index % 4 === 0 ? 72 + (index % 25) : null,
    aaAgentic: index % 4 === 0 ? 68 + (index % 25) : null,
    aaSpeed: index % 4 === 0 ? 80 + (index % 60) : null,
    aaTtft: index % 4 === 0 ? 0.7 + ((index % 10) * 0.1) : null,
    aaTaskCost: index % 4 === 0 ? 0.1 + ((index % 5) * 0.05) : null,
    benchmarkCount: index % 4 === 0 ? 15 : 0,
    releaseDate: "2025-11-10",
    lastUpdated: "2026-08-17",
  };
});

const allPreviewModels = [...previewModels, ...previewGeneratedModels];

const previewModelSnapshot: ModelCatalogSnapshot = {
  models: allPreviewModels,
  providers: previewProviders,
  total: allPreviewModels.length,
  lastSyncedAt: new Date().toISOString(),
  syncedToday: true,
  sources: [
    { source: "llmpricing_manifest", url: "https://llmpricing.dev/rows/manifest.json", fetchedAt: new Date().toISOString(), recordCount: previewProviders.length },
    { source: "rows-000.json", url: "https://llmpricing.dev/rows/rows-000.json", fetchedAt: new Date().toISOString(), recordCount: 400 },
  ],
  meta: {
    syncedAt: new Date().toISOString(),
    providers: previewProviders.length,
    models: allPreviewModels.length,
    source: "https://llmpricing.dev",
  },
};

let browserProxyPool: ProxyPoolState = {
  subscriptions: [], nodes: [], channels: [], defaultChannelId: "default",
  activeNodeId: "", activeNode: null, enabled: false,
  ignoreAddresses: "localhost,127.0.0.1,::1,.local,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16",
  speedTestUrl: "http://www.gstatic.com/generate_204", runtimeAvailable: true,
  runtimePath: "/Applications/Clash Verge.app/Contents/MacOS/verge-mihomo",
  runtimeError: "", nodeCount: 0, subscriptionCount: 0,
  invalidNodeCount: 0,
};

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

/** 轻量模式 / 浏览器环境下的 RPC 请求：与桌面端 invoke 共用同一套命令名。 */
export async function rpc<T>(
  command: string,
  args: Record<string, unknown> = {},
): Promise<T> {
  const token = new URLSearchParams(window.location.search).get("token") ?? "";
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
function unwrapResult(data: unknown): unknown {
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

/** 浏览器降级模式 — 仅用于纯前端预览（无内核服务时）。 */
async function browserFallback<T>(
  command: string,
  args: Record<string, unknown> = {},
): Promise<T> {

  if (!browserData) {
    const response = await fetch("/sites.json");
    const raw = (await response.json()) as { sites: SiteRecord[]; tags: string[] };
    browserData = {
      sites: raw.sites.map((site) => ({ ...emptySite(), ...site })),
      suggestedTags: raw.tags,
      usageSites: [],
    };
  }

  if (command === "list_library") return browserData as T;
  if (command === "get_proxy_pool_state") {
    if (!browserProxyPool.channels.length) {
      const now = new Date().toISOString();
      browserProxyPool.channels = [{
        id: "default", name: "默认通道", nodeId: "", node: null, testUrl: "",
        accountCount: 0, accounts: [], createdAt: now, updatedAt: now,
      }];
      browserProxyPool.defaultChannelId = "default";
    }
    return structuredClone(browserProxyPool) as T;
  }
  if (command === "analyze_proxy_nodes") {
    const countryFromName = (name: string) => {
      const value = name.toUpperCase();
      if (value.includes("JP") || name.includes("日本")) return ["JP", "日本"] as const;
      if (value.includes("HK") || name.includes("香港")) return ["HK", "香港"] as const;
      if (value.includes("SG") || name.includes("新加坡")) return ["SG", "新加坡"] as const;
      if (value.includes("US") || name.includes("美国")) return ["US", "美国"] as const;
      return ["ZZ", "未知地区"] as const;
    };
    const analysisNodes = browserProxyPool.nodes.map((node) => {
      const isIp = /^[0-9a-f:.]+$/i.test(node.server) && node.server.includes(".");
      const resolvedIps = isIp ? [node.server] : [];
      const [countryCode, countryName] = countryFromName(node.name);
      return {
        nodeId: node.id,
        nodeName: node.name,
        server: node.server,
        resolvedIps,
        primaryIp: resolvedIps[0] || "",
        classification: resolvedIps.length ? "public" : "unresolved",
        countryCode,
        countryName,
        error: resolvedIps.length ? "" : "浏览器预览模式不执行 DNS 解析",
      };
    });
    const groups = new Map<string, { key: string; label: string; classification: string; countryCode: string; countryName: string; nodeIds: string[]; nodeCount: number }>();
    for (const node of analysisNodes) {
      const key = node.countryCode;
      const current = groups.get(key) || {
        key,
        label: node.countryName,
        classification: key === "ZZ" ? "unknown" : "country",
        countryCode: node.countryCode,
        countryName: node.countryName,
        nodeIds: [],
        nodeCount: 0,
      };
      if (!current.nodeIds.includes(node.nodeId)) current.nodeIds.push(node.nodeId);
      current.nodeCount = current.nodeIds.length;
      groups.set(key, current);
    }
    return {
      analyzedAt: String(Math.floor(Date.now() / 1000)),
      geoipAvailable: false,
      geoipDatabasePath: "",
      totalNodes: analysisNodes.length,
      resolvedNodes: analysisNodes.filter((node) => node.resolvedIps.length > 0).length,
      unresolvedNodes: analysisNodes.filter((node) => node.resolvedIps.length === 0).length,
      uniqueIps: new Set(analysisNodes.flatMap((node) => node.resolvedIps)).size,
      nodes: analysisNodes,
      groups: [...groups.values()].sort((left, right) => right.nodeCount - left.nodeCount),
    } as T as ProxyIpAnalysis as T;
  }
  if (command === "save_proxy_subscription") {
    const id = String(args.id || `browser-sub-${Date.now()}`);
    const current = browserProxyPool.subscriptions.find((item) => item.id === id);
    const subscription = {
      id, name: String(args.name || ""), url: String(args.url || ""),
      nodeCount: current?.nodeCount ?? 0, lastError: "",
      createdAt: current?.createdAt ?? new Date().toISOString(), updatedAt: new Date().toISOString(),
    };
    browserProxyPool.subscriptions = [subscription, ...browserProxyPool.subscriptions.filter((item) => item.id !== id)];
    browserProxyPool.subscriptionCount = browserProxyPool.subscriptions.length;
    return subscription as T;
  }
  if (command === "delete_proxy_subscription") {
    const id = String(args.id);
    const source = browserProxyPool.subscriptions.find((item) => item.id === id);
    browserProxyPool.subscriptions = browserProxyPool.subscriptions.filter((item) => item.id !== id);
    if (source) browserProxyPool.nodes = browserProxyPool.nodes.filter((node) => !node.subscriptionNames.includes(source.name));
    browserProxyPool.subscriptionCount = browserProxyPool.subscriptions.length;
    browserProxyPool.nodeCount = browserProxyPool.nodes.length;
    return browserProxyPool as T;
  }
  if (command === "refresh_proxy_subscription") {
    const id = String(args.id);
    const source = browserProxyPool.subscriptions.find((item) => item.id === id)!;
    if (!browserProxyPool.nodes.length) {
      const samples = [
        ["demo-jp-1", "JP - 日本 - 06", "http", "jp1.example.com", 52, false],
        ["demo-hk-1", "HK - HKT - 14", "vmess", "hk1.example.com", 55, false],
        ["demo-hk-2", "HK - 香港原生 - 11", "vmess", "hk2.example.com", 61, false],
        ["demo-jp-2", "JP - 日本东京 - 02", "http", "jp2.example.com", 70, false],
        ["demo-hk-3", "HK - 香港HGC - 05", "socks5", "hk3.example.com", 86, false],
        ["demo-hk-4", "HK - BGP | AnyTLS - 16", "anytls", "hk4.example.com", 98, true],
        ["demo-us-1", "US - 洛杉矶 - 02", "http", "us1.example.com", 160, false],
        ["demo-us-2", "US - 硅谷 - 12", "socks5", "us2.example.com", 350, false],
        ["demo-us-3", "US - 美国BGP - 07", "socks5", "us3.example.com", 684, false],
      ] as const;
      browserProxyPool.nodes = samples.map(([id, name, proxyType, server, latencyMs, udp]) => {
        const countryCode = name.startsWith("JP") ? "JP" : name.startsWith("HK") ? "HK" : name.startsWith("US") ? "US" : "ZZ";
        const countryName = countryCode === "JP" ? "日本" : countryCode === "HK" ? "香港" : countryCode === "US" ? "美国" : "未知地区";
        return {
          id, subscriptionNames: [source.name], name, proxyType, server, port: 443,
          cipher: "", udp, latencyMs, testStatus: "success", testedAt: new Date().toISOString(),
          channelLatencyMs: null, channelTestStatus: "",
          countryCode, countryName, classification: "public", primaryIp: "",
          updatedAt: new Date().toISOString(),
        };
      });
    }
    source.nodeCount = browserProxyPool.nodes.length;
    browserProxyPool.nodeCount = browserProxyPool.nodes.length;
    return { subscription: source, added: source.nodeCount, total: source.nodeCount, discarded: 0 } as T;
  }
  if (command === "set_proxy_pool_settings") {
    browserProxyPool.ignoreAddresses = String(args.ignoreAddresses || "");
    browserProxyPool.speedTestUrl = "http://www.gstatic.com/generate_204";
    return browserProxyPool as T;
  }
  if (command === "save_proxy_channel") {
    const id = String(args.id || `browser-channel-${Date.now()}`);
    const current = browserProxyPool.channels.find((item) => item.id === id);
    const now = new Date().toISOString();
    const channel = {
      id,
      name: String(args.name || "通道"),
      nodeId: current?.nodeId ?? "",
      node: current?.node ?? null,
      testUrl: String(args.testUrl || ""),
      accountCount: current?.accountCount ?? 0,
      accounts: current?.accounts ?? [],
      createdAt: current?.createdAt ?? now,
      updatedAt: now,
    };
    browserProxyPool.channels = [channel, ...browserProxyPool.channels.filter((item) => item.id !== id)];
    return structuredClone(browserProxyPool) as T;
  }
  if (command === "delete_proxy_channel") {
    const id = String(args.id);
    browserProxyPool.channels = browserProxyPool.channels.filter((item) => item.id !== id);
    if (!browserProxyPool.channels.length) {
      const now = new Date().toISOString();
      browserProxyPool.channels = [{
        id: "default", name: "默认通道", nodeId: "", node: null, testUrl: "",
        accountCount: 0, accounts: [], createdAt: now, updatedAt: now,
      }];
    }
    return structuredClone(browserProxyPool) as T;
  }
  if (command === "set_proxy_channel_node") {
    const channelId = String(args.channelId);
    const nodeId = String(args.nodeId);
    const channel = browserProxyPool.channels.find((item) => item.id === channelId);
    if (channel) {
      channel.nodeId = nodeId;
      channel.node = browserProxyPool.nodes.find((item) => item.id === nodeId) ?? null;
      channel.updatedAt = new Date().toISOString();
    }
    return structuredClone(browserProxyPool) as T;
  }
  if (command === "assign_account_proxy_channel" || command === "unassign_account_proxy_channel") {
    const profileId = String(args.profileId || "");
    if (command === "assign_account_proxy_channel") {
      const channelId = String(args.channelId || "");
      const existing = browserProxyPool.channels.find(
        (channel) => channel.id !== channelId && channel.accounts.some((account) => account.profileId === profileId),
      );
      if (existing) throw new Error(`该账号已归属通道「${existing.name}」`);
      const target = browserProxyPool.channels.find((channel) => channel.id === channelId);
      if (target && !target.accounts.some((account) => account.profileId === profileId)) {
        target.accounts.push({ profileId });
        target.accountCount = target.accounts.length;
      }
    } else {
      for (const channel of browserProxyPool.channels) {
        channel.accounts = channel.accounts.filter((account) => account.profileId !== profileId);
        channel.accountCount = channel.accounts.length;
      }
    }
    return structuredClone(browserProxyPool) as T;
  }
  if (command === "test_proxy_channel_nodes") {
    browserProxyPool.nodes.forEach((node) => {
      if (node.latencyMs == null || node.latencyMs > 500) return;
      node.channelLatencyMs = 96 + Math.floor(Math.random() * 260);
      node.channelTestStatus = "success";
    });
    return structuredClone(browserProxyPool) as T;
  }
  if (command === "set_active_proxy_node") {
    browserProxyPool.activeNodeId = String(args.nodeId);
    browserProxyPool.activeNode = browserProxyPool.nodes.find((item) => item.id === args.nodeId) ?? null;
    browserProxyPool.enabled = true;
    return browserProxyPool as T;
  }
  if (command === "clear_active_proxy_node") {
    browserProxyPool.activeNodeId = "";
    browserProxyPool.activeNode = null;
    browserProxyPool.enabled = false;
    return browserProxyPool as T;
  }
  if (command === "test_proxy_node") {
    const node = browserProxyPool.nodes.find((item) => item.id === args.nodeId)!;
    node.latencyMs = node.latencyMs ?? 128;
    node.testStatus = "success";
    node.testedAt = new Date().toISOString();
    return node as T;
  }
  if (command === "test_all_proxy_nodes" || command === "test_proxy_nodes") {
    const requested = command === "test_proxy_nodes"
      ? new Set((args.nodeIds as string[] | undefined) ?? [])
      : null;
    browserProxyPool.nodes.forEach((node) => {
      if (requested && !requested.has(node.id)) return;
      node.latencyMs = node.latencyMs ?? 128;
      node.testStatus = "success";
      node.testedAt = new Date().toISOString();
    });
    return structuredClone(browserProxyPool) as T;
  }
  if (command === "cancel_proxy_node_tests") return false as T;
  if (command === "get_charity_feed") {
    return {
      feedId: String(args.feedId || "1515"),
      feedName: "公益推广",
      items: [],
      fetchedAt: "",
      changed: false,
      newCount: 0,
      updatedCount: 0,
      initialized: false,
      sourceProfileName: "",
      sourceAccountName: "",
      status: "local",
      message: "",
      usedNodeId: "",
      usedNodeName: "",
      unreadCount: 0,
      skipped: false,
      totalCount: 0,
      offset: Number(args.offset || 0),
      limit: Number(args.limit || 20),
      hasMore: false,
    } as T;
  }
  if (command === "fetch_charity_feed") {
    return {
      feedId: String(args.feedId || "1515"),
      feedName: "公益推广",
      items: [],
      fetchedAt: new Date().toISOString(),
      changed: false,
      newCount: 0,
      updatedCount: 0,
      initialized: true,
      sourceProfileName: "",
      sourceAccountName: "",
      status: "skipped",
      message: "浏览器模式无代理节点，已跳过",
      usedNodeId: "",
      usedNodeName: "",
      unreadCount: 0,
      skipped: true,
    } as T;
  }
  if (command === "mark_charity_feed_read") return 0 as T;
  if (command === "get_charity_unread_total") return 0 as T;
  if (command === "get_charity_sync_logs") return [] as T;
  if (command === "clear_charity_sync_logs") return undefined as T;
  if (command === "set_charity_monitor_visible") return undefined as T;
  if (command === "request_charity_round") return undefined as T;

  // 浏览器降级模式下无桌面内核调度器，自动同步返回“已关闭”的空状态。
  if (command === "get_auto_sync_settings") return { enabled: false, intervalMinutes: 30 } as T;
  if (command === "set_auto_sync_settings") {
    return {
      enabled: Boolean(args.enabled),
      intervalMinutes: Number(args.intervalMinutes || 30),
    } as T;
  }
  if (command === "get_auto_sync_status") {
    return {
      enabled: false,
      intervalMinutes: 30,
      lastRoundAt: 0,
      lastSummary: null,
    } as T;
  }
  if (command === "request_auto_sync_round") return undefined as T;

  if (command === "get_token_stats") {
    return {
      available: true,
      sessions: [],
      sessionCount: 0,
      summary: {
        sessions: 0, productiveSessions: 0, oneShotSessions: 0, editTurns: 0, retries: 0,
        totalTokens: 0, costUsd: 0, editTokens: 0, editCostUsd: 0, productiveRate: 0,
        oneShotRate: null, editSessions: 0, firstPassSessions: 0, editSessionRate: 0,
        firstPassRate: null, tokensPerEdit: null, costPerEdit: null,
      },
      byModel: [],
      subagents: [],
      provenance: {},
    } as T;
  }
  if (command === "sync_token_data") {
    return {
      available: true,
      changed: false,
      skipped: true,
      elapsedMs: 8,
      updatedAt: new Date().toISOString(),
      message: "开发模式已复用 OpenHub 本地采集缓存",
    } as T;
  }
  if (command === "get_token_usage") {
    const buckets: any[] = [];
    const now = Date.now();
    let i = 0;
    for (let d = 0; d < 3; d++) {
      for (let h = 8; h < 23; h++) {
        const total = 3000000 + ((i*7919) % 5000000);
        buckets.push({ source: "claude", model: "claude-sonnet-4",
          timestamp: new Date(now - d*86400000 - h*3600000).toISOString(),
          totalTokens: total, billableTotalTokens: Math.floor(total*0.6),
          inputTokens: Math.floor(total*0.5), cachedInputTokens: Math.floor(total*0.25),
          cacheCreationInputTokens: Math.floor(total*0.05), outputTokens: Math.floor(total*0.15),
          reasoningOutputTokens: Math.floor(total*0.05), conversationCount: (i%3)+1,
          costUsd: 0, pricingAvailable: false, estimatedTokens: 0 });
        i++;
      }
    }
    return { available: true, buckets, startDate: "", endDate: "", pricingSource: "dev-mock" } as T;
  }
  if (command === "get_token_raw_logs") {
    return { available: false, sessions: [], conversations: [], requests: [] } as T;
  }
  if (command === "get_token_request_health") {
    // dev mock：模拟近 10 天逐小时 对话/请求健康（少量失败）
    const buckets: any[] = [];
    const now = new Date();
    const dayStart = new Date(now.getFullYear(), now.getMonth(), now.getDate());
    for (let d = 0; d < 10; d++) {
      for (let h = 0; h < 24; h++) {
        const hour = new Date(dayStart.getTime() - d * 86400000 + h * 3600000);
        const ts = hour.toISOString().slice(0, 13); // YYYY-MM-DDTHH
        const dialogues = 1 + ((d * 5 + h) % 4); // 用户发起 turns
        const success = 5 + ((d * 31 + h * 7) % 12);
        const failed = (d + h) % 5 === 0 ? 1 + (h % 3) : 0;
        const requests = success + failed + ((d + h) % 4); // mock extracted request count
        buckets.push({ hour: `${ts}:00:00.000Z`, dialogues, requests, success, failed });
      }
    }
    return {
      available: true,
      buckets,
      bySource: [
        { source: "codex", dialogues: 120, requests: 800, success: 700, failed: 20 },
        { source: "claude", dialogues: 80, requests: 600, success: 580, failed: 10 },
        { source: "mimo", dialogues: 90, requests: 900, success: 880, failed: 5 },
      ],
    } as T;
  }
  if (command === "get_local_agent_paths") {
    const home = "~/";
    const mk = (source: string, name: string, root: string, paths: { kind: string; label: string; path: string; exists: boolean }[]) =>
      ({ source, name, root, detected: paths.some((p) => p.exists), paths });
    return {
      available: true,
      home,
      agents: [
        mk("codex", "Codex", `${home}.codex`, [
          { kind: "config", label: "配置 config.toml", path: `${home}.codex/config.toml`, exists: false },
          { kind: "data", label: "会话 sessions", path: `${home}.codex/sessions`, exists: false },
        ]),
        mk("claude", "Claude Code", `${home}.claude`, [
          { kind: "config", label: "项目设置 settings.json", path: `${home}.claude/settings.json`, exists: false },
          { kind: "data", label: "会话项目 projects", path: `${home}.claude/projects`, exists: false },
        ]),
        mk("opencode", "OpenCode", `${home}.local/share/opencode`, [
          { kind: "database", label: "数据库 opencode.db", path: `${home}.local/share/opencode/opencode.db`, exists: false },
        ]),
      ],
    } as T;
  }
  if (command === "get_all_site_model_caches") {
    // 与 previewModelSnapshot 配套：gpt-5.4 验证精确匹配 + 跨站合并，
    // openai/gpt-5.4-preview 验证子串回退匹配，claude-sonnet-4 验证未匹配兜底。
    return [
      {
        siteId: "preview-a",
        cache: {
          models: [
            { id: "anthropic/claude-sonnet-4", ownedBy: "anthropic" },
            { id: "gpt-5.4", ownedBy: "openai" },
          ],
          accounts: [{
            profileId: "p1", profileName: "默认", accountName: "preview@a", username: "preview-a",
            keys: ["sk-preview-aaaa1111", "sk-preview-bbbb2222"],
            keyGroups: { "sk-preview-aaaa1111": "vip", "sk-preview-bbbb2222": "default" },
            keyModels: {
              "sk-preview-aaaa1111": [{ id: "anthropic/claude-sonnet-4", ownedBy: "anthropic" }],
              "sk-preview-bbbb2222": [{ id: "gpt-5.4", ownedBy: "openai" }],
            },
          }],
        },
      },
      {
        siteId: "preview-b",
        cache: {
          models: [{ id: "openai/gpt-5.4-preview", ownedBy: "openai" }],
          accounts: [{
            profileId: "p1", profileName: "默认", accountName: "preview@b", username: "preview-b",
            keys: ["sk-preview-cccc3333"],
            keyGroups: { "sk-preview-cccc3333": "vip" },
            keyModels: { "sk-preview-cccc3333": [{ id: "openai/gpt-5.4-preview", ownedBy: "openai" }] },
          }],
        },
      },
    ] as T;
  }
  if (command === "get_model_catalog") return structuredClone(previewModelSnapshot) as T;
  if (command === "get_model_catalog_detail") {
    const key = String(args.id || args.canonicalKey || "");
    const model = allPreviewModels.find((item) => item.id === key || item.slug === key);
    if (!model) throw new Error("模型参数不存在");
    const matchedProviders = previewProviders.filter((p) => model.hostProviders.includes(p.id));
    const detail: ModelCatalogDetail = {
      model,
      providers: matchedProviders,
      raw: { id: model.id, name: model.name, preview: true },
    };
    return structuredClone(detail) as T;
  }
  if (command === "sync_model_catalog") {
    const result: ModelCatalogSyncResult = {
      synced: true,
      skipped: false,
      message: "模型参数预览数据已刷新",
      snapshot: structuredClone(previewModelSnapshot),
    };
    return result as T;
  }
  if (command === "detect_site_system_types") return 0 as T;
  if (command === "create_site") {
    const site = {
      ...(args.input as SiteRecord),
      id: `local-${Date.now()}`,
      updatedAt: new Date().toISOString(),
    };
    browserData!.sites.unshift(site);
    return site as T;
  }
  if (command === "import_site") {
    const url = new URL(String(args.siteUrl));
    const usageState = String(args.usageState ?? "all");
    url.pathname = "/";
    url.search = "";
    url.hash = "";
    const site = {
      ...emptySite(),
      id: `local-${Date.now()}`,
      name: url.hostname.replace(/^www\./, ""),
      apiBaseUrl: url.toString(),
      icon: new URL("/favicon.ico", url).toString(),
      isPersonal: usageState === "personal",
      isPending: usageState === "pending",
      updatedAt: new Date().toISOString(),
    };
    browserData!.sites.unshift(site);
    return site as T;
  }
  if (command === "mark_sites_with_chrome_sessions") {
    return {
      scanned: ((args.siteIds as string[] | undefined) ?? browserData!.sites.map((site) => site.id)).length,
      detected: 0,
      accounts: 0,
      warnings: 0,
      newlyMarked: 0,
      sites: [],
    } as T;
  }
  if (command === "update_site") {
    const id = String(args.id);
    const index = browserData!.sites.findIndex((site) => site.id === id);
    browserData!.sites[index] = {
      ...(args.input as SiteRecord),
      id,
      updatedAt: new Date().toISOString(),
    };
    return browserData!.sites[index] as T;
  }
  if (command === "delete_site") {
    browserData!.sites = browserData!.sites.filter((site) => site.id !== args.id);
    return undefined as T;
  }
  if (command === "toggle_personal") {
    const site = browserData!.sites.find((item) => item.id === args.id)!;
    site.isPersonal = !site.isPersonal;
    if (site.isPersonal) site.isPending = false;
    site.updatedAt = new Date().toISOString();
    return site as T;
  }
  if (command === "toggle_pending") {
    const site = browserData!.sites.find((item) => item.id === args.id)!;
    site.isPending = !site.isPending;
    if (site.isPending) site.isPersonal = false;
    site.updatedAt = new Date().toISOString();
    return site as T;
  }
  if (command === "cycle_usage_state") {
    const site = browserData!.sites.find((item) => item.id === args.id)!;
    if (site.isPersonal) {
      // 在用 → 待定
      site.isPersonal = false;
      site.isPending = true;
    } else if (site.isPending) {
      // 待定 → 未在用
      site.isPersonal = false;
      site.isPending = false;
    } else {
      // 未在用 → 在用
      site.isPersonal = true;
      site.isPending = false;
    }
    site.updatedAt = new Date().toISOString();
    return site as T;
  }
  if (command === "set_usage_state") {
    const site = browserData!.sites.find((item) => item.id === args.id)!;
    const state = String(args.state);
    site.isPersonal = state === "personal";
    site.isPending = state === "pending";
    site.updatedAt = new Date().toISOString();
    return site as T;
  }
  if (command === "toggle_runaway") {
    const site = browserData!.sites.find((item) => item.id === args.id)!;
    site.isRunaway = !site.isRunaway;
    site.updatedAt = new Date().toISOString();
    return site as T;
  }
  throw new Error(`Unsupported command: ${command}`);

}
// —— 全局单例状态 ——
const sites = ref<SiteRecord[]>([]);
const suggestedTags = ref<string[]>([]);
const usageSites = ref<ChromeUsageSite[]>([]);
const loading = ref(false);
let dailyRefreshTimer: number | null = null;

async function loadLibrary() {
  loading.value = true;
  try {
    const data = await runCommand<LibraryData>("list_library");
    sites.value = data.sites.map((site) => ({ ...emptySite(), ...site }));
    suggestedTags.value = data.suggestedTags;
    usageSites.value = data.usageSites ?? [];
  } finally {
    loading.value = false;
  }
}

function scheduleDailyRefresh() {
  if (dailyRefreshTimer !== null) {
    window.clearTimeout(dailyRefreshTimer);
  }

  const now = new Date();
  const nextMidnight = new Date(now);
  nextMidnight.setHours(24, 0, 0, 0);
  // 留出 1 秒，确保本地日期已经切换。只重新读取 SQLite，
  // 不会触发任何网络同步，因此不会打断正在进行的任务。
  const delay = Math.max(1000, nextMidnight.getTime() - now.getTime() + 1000);
  dailyRefreshTimer = window.setTimeout(async () => {
    try {
      await loadLibrary();
    } finally {
      scheduleDailyRefresh();
    }
  }, delay);
}

function startDailyRefresh() {
  scheduleDailyRefresh();
}

function stopDailyRefresh() {
  if (dailyRefreshTimer !== null) {
    window.clearTimeout(dailyRefreshTimer);
    dailyRefreshTimer = null;
  }
}

export function useLibrary() {
  return {
    sites,
    suggestedTags,
    usageSites,
    loading,
    loadLibrary,
    startDailyRefresh,
    stopDailyRefresh,
  };
}
