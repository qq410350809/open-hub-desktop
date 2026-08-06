import { invoke } from "@tauri-apps/api/core";
import { ref } from "vue";
import { emptySite, type ChromeUsageSite, type LibraryData, type ProxyIpAnalysis, type ProxyPoolState, type SiteRecord } from "../types";

const isTauri = "__TAURI_INTERNALS__" in window;

let browserData: LibraryData | null = null;
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

  // 浏览器降级模式 — 仅用于纯前端预览
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
    url.pathname = "/";
    url.search = "";
    url.hash = "";
    const site = {
      ...emptySite(),
      id: `local-${Date.now()}`,
      name: url.hostname.replace(/^www\./, ""),
      apiBaseUrl: url.toString(),
      icon: new URL("/favicon.ico", url).toString(),
      updatedAt: new Date().toISOString(),
    };
    browserData!.sites.unshift(site);
    return site as T;
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

export function useLibrary() {
  return { sites, suggestedTags, usageSites, loading, loadLibrary };
}
