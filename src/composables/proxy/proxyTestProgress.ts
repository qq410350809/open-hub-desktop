import type { ProxyNode, ProxyNodeTestProgress, ProxyPoolState } from "../../types";
import { validProxyIpInfo } from "./proxyIpInfo";

/** 每轮独立缓冲；IP 富化与测速完成分开，避免同帧互相覆盖或重复计数。 */
export function createProxyTestProgress(nodeIds: Iterable<string>, runId?: string) {
  const ids = new Set(nodeIds);
  const completed = new Map<string, ProxyNodeTestProgress>();
  const metrics = new Map<string, { payload: ProxyNodeTestProgress; testedAt: string }>();
  const ipResults = new Map<string, ProxyNodeTestProgress>();
  const finalIps = new Map<string, string>();
  const dirty = new Set<string>();
  const starts = new Set<string>();
  const stops = new Set<string>();
  let settled = false;
  let succeeded = 0;
  let failed = 0;
  let progress = { completed: 0, total: ids.size };

  function accept(payload: ProxyNodeTestProgress): boolean {
    if (runId !== undefined && payload.runId !== runId) return false;
    if (!ids.has(payload.nodeId)) return false;
    if (payload.phase === "ip-info") {
      const result = completed.get(payload.nodeId);
      const expectedIp = settled ? finalIps.get(payload.nodeId) : result?.status === "success" ? result.primaryIp : null;
      if (!payload.primaryIp || payload.primaryIp !== expectedIp) return false;
      if (!validProxyIpInfo({ primaryIp: payload.primaryIp, ipInfo: payload.ipInfo })) return false;
      const previous = ipResults.get(payload.nodeId)?.ipInfo;
      if (previous && Date.parse(previous.checkedAt) > Date.parse(payload.ipInfo!.checkedAt)) return false;
      ipResults.set(payload.nodeId, payload);
      dirty.add(payload.nodeId);
      return true;
    }
    if (settled || completed.has(payload.nodeId)) return false;
    if (payload.phase === "started") {
      starts.add(payload.nodeId);
      return true;
    }
    if (payload.phase !== "completed") return false;
    completed.set(payload.nodeId, payload);
    starts.delete(payload.nodeId);
    stops.add(payload.nodeId);
    progress = { completed: Math.max(progress.completed, payload.completed), total: payload.total };
    if (payload.status !== "cancelled") {
      if (payload.status === "success") succeeded += 1;
      else failed += 1;
      metrics.set(payload.nodeId, { payload, testedAt: new Date().toISOString() });
      dirty.add(payload.nodeId);
    }
    return true;
  }

  function applyIpInfo(node: ProxyNode) {
    const payload = ipResults.get(node.id);
    if (!payload || payload.primaryIp !== node.primaryIp) return;
    const info = validProxyIpInfo({ primaryIp: node.primaryIp, ipInfo: payload.ipInfo });
    const current = validProxyIpInfo(node);
    if (info && (!current || Date.parse(info.checkedAt) >= Date.parse(current.checkedAt))) node.ipInfo = info;
  }

  function apply(node: ProxyNode) {
    const result = metrics.get(node.id);
    if (result) {
      node.latencyMs = result.payload.latencyMs;
      node.testStatus = result.payload.status;
      node.channelLatencyMs = result.payload.speedMs ?? null;
      node.channelTestStatus = result.payload.speedMs != null ? "success" : "error";
      node.testedAt = result.testedAt;
      node.primaryIp = result.payload.primaryIp ?? "";
      // completed 只发布本轮实测 IP，不能沿用上一轮富化结果。
      node.ipInfo = null;
    }
    applyIpInfo(node);
  }

  function clearPending() {
    metrics.clear();
    dirty.clear();
    starts.clear();
    stops.clear();
  }

  function settle(state: ProxyPoolState) {
    settled = true;
    // 整表快照的测速指标/时间具有权威性，绝不再把 pending 指标刷回去。
    clearPending();
    // HTTP 命令响应可能早于 SSE 的 completed 帧；以最终实测 IP 接纳后续富化，
    // 但不再补写任何测速指标或递增计数。
    for (const node of state.nodes) {
      if (ids.has(node.id) && node.testStatus === "success" && node.primaryIp) finalIps.set(node.id, node.primaryIp);
    }
    state.nodes.forEach(applyIpInfo);
    state.channels.forEach((channel) => { if (channel.node) applyIpInfo(channel.node); });
    if (state.activeNode) applyIpInfo(state.activeNode);
  }

  return {
    ids, dirty, starts, stops, accept, apply, applyIpInfo, clearPending, settle,
    get settled() { return settled; },
    get succeeded() { return succeeded; },
    get failed() { return failed; },
    get progress() { return progress; },
    get receivedProgress() { return completed.size > 0; },
  };
}

/** 仅把请求发出后变化的测速字段合入响应；不追加节点、不恢复 active/channel 配置。 */
export function mergeProxyTestMetadata(
  incoming: ProxyPoolState,
  current: ProxyPoolState,
  changedAfterRequest: ReadonlySet<string>,
): ProxyPoolState {
  const currentNodes = new Map(current.nodes.map((node) => [node.id, node]));
  const merge = (node: ProxyNode): ProxyNode => {
    const latest = currentNodes.get(node.id);
    if (!latest || !changedAfterRequest.has(node.id)) return node;
    return {
      ...node,
      latencyMs: latest.latencyMs,
      testStatus: latest.testStatus,
      testedAt: latest.testedAt,
      channelLatencyMs: latest.channelLatencyMs,
      channelTestStatus: latest.channelTestStatus,
      primaryIp: latest.primaryIp,
      ipInfo: latest.ipInfo ?? null,
      countryCode: latest.countryCode,
      countryName: latest.countryName,
      classification: latest.classification,
    };
  };
  return {
    ...incoming,
    nodes: incoming.nodes.map(merge),
    channels: incoming.channels.map((channel) => ({ ...channel, node: channel.node ? merge(channel.node) : null })),
    activeNode: incoming.activeNode ? merge(incoming.activeNode) : null,
  };
}
