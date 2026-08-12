import { invoke } from "@tauri-apps/api/core";
import { ref } from "vue";
import { emptySite, type ChromeUsageSite, type LibraryData, type ModelCatalogDetail, type ModelCatalogItem, type ModelCatalogSnapshot, type ModelCatalogSyncResult, type ProxyIpAnalysis, type ProxyPoolState, type SiteRecord } from "../types";

export const isTauri = "__TAURI_INTERNALS__" in window;

let browserData: LibraryData | null = null;
const previewModels: ModelCatalogItem[] = [
  {
    canonicalKey: "openai/gpt-5.4",
    displayName: "GPT-5.4",
    manufacturer: "openai",
    mode: "chat",
    contextLength: 1_050_000,
    maxInputTokens: 1_000_000,
    maxOutputTokens: 50_000,
    inputCostPerToken: 2.5e-6,
    outputCostPerToken: 15e-6,
    cacheReadCostPerToken: 0.25e-6,
    cacheWriteCostPerToken: 0,
    imageCost: 0,
    audioInputCostPerToken: 0,
    audioOutputCostPerToken: 0,
    requestCost: 0,
    capabilities: ["function_calling", "reasoning", "response_schema", "input_modalities:image"],
  },
  {
    canonicalKey: "anthropic/claude-sonnet-4.6",
    displayName: "Claude Sonnet 4.6",
    manufacturer: "anthropic",
    mode: "chat",
    contextLength: 200_000,
    maxInputTokens: 200_000,
    maxOutputTokens: 64_000,
    inputCostPerToken: 3e-6,
    outputCostPerToken: 15e-6,
    cacheReadCostPerToken: 0.3e-6,
    cacheWriteCostPerToken: 3.75e-6,
    imageCost: 0,
    audioInputCostPerToken: 0,
    audioOutputCostPerToken: 0,
    requestCost: 0,
    capabilities: ["function_calling", "prompt_caching", "reasoning", "vision"],
  },
  {
    canonicalKey: "google/gemini-3.1-flash-image",
    displayName: "Gemini 3.1 Flash Image",
    manufacturer: "google",
    mode: "image_generation",
    contextLength: 1_048_576,
    maxInputTokens: 1_048_576,
    maxOutputTokens: 65_536,
    inputCostPerToken: 0.5e-6,
    outputCostPerToken: 3e-6,
    cacheReadCostPerToken: 0,
    cacheWriteCostPerToken: 0,
    imageCost: 0.04,
    audioInputCostPerToken: 0,
    audioOutputCostPerToken: 0,
    requestCost: 0,
    capabilities: ["image_generation", "input_modalities:image", "input_modalities:text"],
  },
];
const previewGeneratedModels: ModelCatalogItem[] = Array.from({ length: 137 }, (_, index) => {
  const manufacturers = ["openai", "anthropic", "google", "deepseek", "qwen", "meta-llama"];
  const manufacturer = manufacturers[index % manufacturers.length];
  return {
    canonicalKey: `${manufacturer}/preview-model-${index + 1}`,
    displayName: `Preview Model ${index + 1}`,
    manufacturer,
    mode: index % 7 === 0 ? "embedding" : "chat",
    contextLength: 32_000 * ((index % 8) + 1),
    maxInputTokens: 30_000,
    maxOutputTokens: 8_000,
    inputCostPerToken: ((index % 9) + 1) * 1e-7,
    outputCostPerToken: ((index % 11) + 1) * 4e-7,
    cacheReadCostPerToken: 0,
    cacheWriteCostPerToken: 0,
    imageCost: 0,
    audioInputCostPerToken: 0,
    audioOutputCostPerToken: 0,
    requestCost: 0,
    capabilities: ["function_calling", "response_schema"],
  };
});
const allPreviewModels = [...previewModels, ...previewGeneratedModels];

const previewModelSnapshot: ModelCatalogSnapshot = {
  models: allPreviewModels,
  total: allPreviewModels.length,
  lastSyncedAt: new Date().toISOString(),
  syncedToday: true,
  sources: [
    { source: "openrouter", url: "https://openrouter.ai/api/v1/models?output_modalities=all", fetchedAt: new Date().toISOString(), recordCount: 532 },
    { source: "litellm", url: "https://cdn.jsdelivr.net/gh/BerriAI/litellm@main/model_prices_and_context_window.json", fetchedAt: new Date().toISOString(), recordCount: 2987 },
  ],
};

let browserProxyPool: ProxyPoolState = {
  subscriptions: [], nodes: [], activeNodeId: "", activeNode: null, enabled: false,
  ignoreAddresses: "localhost,127.0.0.1,::1,.local,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16",
  speedTestUrl: "https://cp.cloudflare.com/generate_204", runtimeAvailable: true,
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
  if (command === "get_proxy_pool_state") return structuredClone(browserProxyPool) as T;
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
    browserProxyPool.speedTestUrl = String(args.speedTestUrl || "");
    return browserProxyPool as T;
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
  if (command === "get_model_catalog") return structuredClone(previewModelSnapshot) as T;
  if (command === "get_model_catalog_detail") {
    const model = allPreviewModels.find((item) => item.canonicalKey === args.canonicalKey);
    if (!model) throw new Error("模型参数不存在");
    const detail: ModelCatalogDetail = {
      model,
      pricing: {
        inputCostPerToken: model.inputCostPerToken,
        outputCostPerToken: model.outputCostPerToken,
        cacheReadCostPerToken: model.cacheReadCostPerToken,
        cacheWriteCostPerToken: model.cacheWriteCostPerToken,
      },
      entries: [
        {
          source: "openrouter",
          sourceModelId: model.canonicalKey,
          channel: "openrouter",
          mode: model.mode,
          displayName: model.displayName,
          contextLength: model.contextLength,
          maxInputTokens: model.maxInputTokens,
          maxOutputTokens: model.maxOutputTokens,
          inputCostPerToken: model.inputCostPerToken,
          outputCostPerToken: model.outputCostPerToken,
          cacheReadCostPerToken: model.cacheReadCostPerToken,
          cacheWriteCostPerToken: model.cacheWriteCostPerToken,
          imageCost: model.imageCost,
          audioInputCostPerToken: model.audioInputCostPerToken,
          audioOutputCostPerToken: model.audioOutputCostPerToken,
          requestCost: model.requestCost,
          raw: { id: model.canonicalKey, preview: true },
        },
        {
          source: "litellm",
          sourceModelId: model.canonicalKey.split("/").at(-1) ?? model.canonicalKey,
          channel: model.manufacturer,
          mode: model.mode,
          displayName: model.displayName,
          contextLength: model.contextLength,
          maxInputTokens: model.maxInputTokens,
          maxOutputTokens: model.maxOutputTokens,
          inputCostPerToken: model.inputCostPerToken * 1.2,
          outputCostPerToken: model.outputCostPerToken * 1.2,
          cacheReadCostPerToken: model.cacheReadCostPerToken,
          cacheWriteCostPerToken: model.cacheWriteCostPerToken,
          imageCost: model.imageCost,
          audioInputCostPerToken: model.audioInputCostPerToken,
          audioOutputCostPerToken: model.audioOutputCostPerToken,
          requestCost: model.requestCost,
          raw: { model: model.canonicalKey, preview: true, supplement: true },
        },
      ],
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
