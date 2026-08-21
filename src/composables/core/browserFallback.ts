import { emptySite, type LibraryData, type ModelCatalogDetail, type ModelCatalogSyncResult, type ProxyIpAnalysis, type ProxyPoolState, type SiteRecord } from "../../types";
import {
  DEFAULT_IGNORE_ADDRESSES,
  DEFAULT_PROXY_PORT,
  PROXY_SPEED_TEST_URL,
  buildProxyBaseUrl,
} from "../../constants";
import {
  allPreviewModels,
  previewModelSnapshot,
  previewProviders,
} from "./mockData";

export let browserData: LibraryData | null = null;

export let browserProxyPool: ProxyPoolState = {
  subscriptions: [],
  nodes: [],
  channels: [],
  defaultChannelId: "default",
  activeNodeId: "",
  activeNode: null,
  enabled: false,
  ignoreAddresses: DEFAULT_IGNORE_ADDRESSES,
  speedTestUrl: PROXY_SPEED_TEST_URL,
  runtimeAvailable: true,
  runtimePath: "/Applications/Clash Verge.app/Contents/MacOS/verge-mihomo",
  runtimeError: "",
  nodeCount: 0,
  subscriptionCount: 0,
  invalidNodeCount: 0,
};

/** dev mock：生成近几日不同模式与状态的请求日志（分页与状态统计共用一份） */
export function makeMockProxyLogs() {
  const mockLogs = [];
  const now = Date.now();
  const mk = (i: number, stream: boolean, status: number) => {
    const dur = 1200 + ((i * 733) % 9000);
    const reasoningTokens = i % 3 === 0 || i === 17 || i === 23 ? 120 + i * 8 : 0;
    return {
      id: `mock-${i}`,
      timestamp: new Date(now - i * 3_600_000).toLocaleString("sv-SE").replace("T", " ").slice(0, 19),
      method: "POST",
      path: "/v1/chat/completions",
      channelId: "opencode",
      model: stream ? "claude-sonnet-4" : "gpt-4o",
      stream,
      statusCode: status,
      durationMs: dur,
      ttftMs: 320 + (i % 5) * 40,
      promptTokens: 2400 + i * 100,
      promptCacheHitTokens: i % 2 === 0 ? 900 + i * 10 : 0,
      promptCacheMissTokens: 1500,
      completionTokens: 320 + i * 40,
      reasoningTokens,
      totalTokens: (2400 + i * 100) + (320 + i * 40),
      errorMessage: status >= 400 ? `上游返回异常 (HTTP ${status})` : undefined,
      requestBody: `{\n  "model": "${stream ? "claude-sonnet-4" : "gpt-4o"}",\n  "stream": ${stream}\n}`,
      responseBody: i === 17
        ? `{\n  "id": "msg_mock17",\n  "type": "message",\n  "role": "assistant",\n  "content": [\n    {\n      "type": "thinking",\n      "thinking": "这是 Anthropic 格式的思考过程（第 17 条）：先拆解需求，再逐步推理。\\n第二步验证假设。\\n第三步给出结论。"\n    },\n    {\n      "type": "text",\n      "text": "这是 Anthropic 同步响应的正文内容（第 17 条）"\n    }\n  ],\n  "usage": { "input_tokens": 0, "output_tokens": 0 }\n}`
        : i === 23
          ? `{\n  "id": "resp_mock23",\n  "output": [\n    {\n      "type": "reasoning",\n      "summary": [\n        { "type": "summary_text", "text": "这是 Responses API 的推理摘要（第 23 条）：先分析上下文。\\n再推导候选方案。" }\n      ]\n    },\n    {\n      "type": "message",\n      "content": [\n        { "type": "output_text", "text": "这是 Responses API 的最终正文（第 23 条）" }\n      ]\n    }\n  ],\n  "usage": { "prompt_tokens": 0, "completion_tokens": 0 }\n}`
          : stream
            ? ` thinking\n让我分析这个问题，需要先拆解需求。\n response\n\n这是流式响应的最终正文内容（第 ${i} 条）。`
            : `{\n  "choices": [\n    {\n      "message": {\n        "role": "assistant",\n        "content": "这是同步响应的正文内容（第 ${i} 条）"\n      }\n    }\n  ]\n}`,
      nodeName: "HK-01 香港节点",
    };
  };
  for (let i = 0; i < 32; i++) {
    mockLogs.push(mk(i, i % 2 === 0, i % 5 === 0 ? 502 : 200));
  }
  return mockLogs;
}

/** 浏览器降级模式 — 仅用于纯前端预览（无内核服务时）。 */
export async function browserFallback<T>(
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
    browserProxyPool.speedTestUrl = PROXY_SPEED_TEST_URL;
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
    const buckets: any[] = [];
    const now = new Date();
    const dayStart = new Date(now.getFullYear(), now.getMonth(), now.getDate());
    for (let d = 0; d < 10; d++) {
      for (let h = 0; h < 24; h++) {
        const hour = new Date(dayStart.getTime() - d * 86400000 + h * 3600000);
        const ts = hour.toISOString().slice(0, 13);
        const dialogues = 1 + ((d * 5 + h) % 4);
        const success = 5 + ((d * 31 + h * 7) % 12);
        const failed = (d + h) % 5 === 0 ? 1 + (h % 3) : 0;
        const requests = success + failed + ((d + h) % 4);
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
      hosts: matchedProviders.map((p) => ({
        provider: p.id,
        name: p.name,
        modelId: null,
        tier: p.tier,
        subscription: p.subscription,
        input: p.id === model.minProvider ? model.minInputCost : p.id === model.refProvider ? model.refInputCost : null,
        output: p.id === model.minProvider ? model.minOutputCost : p.id === model.refProvider ? model.refOutputCost : null,
        cacheRead: p.id === model.minProvider ? model.minCacheReadCost : p.id === model.refProvider ? model.refCacheReadCost : null,
        cacheWrite: null,
        context: model.contextLength,
        outputLimit: model.maxOutputTokens,
        status: null,
        official: p.id === model.refProvider && model.refOfficial,
        doc: p.doc,
        isFree: false,
        isMin: p.id === model.minProvider,
        isRef: p.id === model.refProvider,
      })),
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
      site.isPersonal = false;
      site.isPending = true;
    } else if (site.isPending) {
      site.isPersonal = false;
      site.isPending = false;
    } else {
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
  if (command === "get_opencode_proxy_logs") {
    const mockLogs = makeMockProxyLogs();
    const filter = String(args.filter ?? "");
    const q = String(args.q ?? "").trim().toLowerCase();
    let list = mockLogs;
    if (filter === "success") list = list.filter((l) => l.statusCode >= 200 && l.statusCode < 300);
    else if (filter === "error") list = list.filter((l) => l.statusCode >= 400);
    if (q) {
      list = list.filter((l) =>
        l.model.toLowerCase().includes(q) ||
        l.path.toLowerCase().includes(q) ||
        String(l.statusCode).includes(q) ||
        (l.errorMessage ?? "").toLowerCase().includes(q)
      );
    }
    const fieldMap: Record<string, (l: (typeof mockLogs)[number]) => string | number> = {
      status: (l) => l.statusCode,
      tokens: (l) => l.totalTokens,
      duration: (l) => l.durationMs,
    };
    const sortBy = String(args.sortBy ?? "timestamp");
    const sortOrder = String(args.sortOrder ?? "desc") === "asc" ? 1 : -1;
    list = [...list].sort((a, b) => {
      const get = fieldMap[sortBy] ?? ((l: (typeof mockLogs)[number]) => l.timestamp);
      const av = get(a);
      const bv = get(b);
      return av < bv ? -sortOrder : av > bv ? sortOrder : 0;
    });
    const page = Math.max(1, Number(args.page ?? 1));
    const pageSize = Math.min(200, Math.max(1, Number(args.pageSize ?? 50)));
    return {
      items: list.slice((page - 1) * pageSize, page * pageSize),
      total: list.length,
      successTotal: list.filter((l) => l.statusCode >= 200 && l.statusCode < 300).length,
      errorTotal: list.filter((l) => l.statusCode >= 400).length,
    } as T;
  }
  if (command === "get_opencode_proxy_status") {
    const mockLogs = makeMockProxyLogs();
    const sum = (fn: (l: (typeof mockLogs)[number]) => number) => mockLogs.reduce((acc, l) => acc + fn(l), 0);
    return {
      running: true,
      port: DEFAULT_PROXY_PORT,
      url: buildProxyBaseUrl(DEFAULT_PROXY_PORT),
      totalRequests: mockLogs.length,
      successfulRequests: mockLogs.filter((l) => l.statusCode >= 200 && l.statusCode < 300).length,
      failedRequests: mockLogs.filter((l) => l.statusCode >= 400).length,
      uptimeSeconds: 5 * 3600,
      modelsCount: 4,
      channelsCount: 1,
      totalPromptTokens: sum((l) => l.promptTokens),
      totalCompletionTokens: sum((l) => l.completionTokens),
      totalReasoningTokens: sum((l) => l.reasoningTokens || 0),
      totalReasoningRequests: mockLogs.filter((l) => (l.reasoningTokens || 0) > 0).length,
      totalCacheHitTokens: sum((l) => l.promptCacheHitTokens || 0),
      totalTokens: sum((l) => l.totalTokens),
      todayTotalTokens: sum((l) => l.totalTokens),
    } as T;
  }
  throw new Error(`Unsupported command: ${command}`);
}
