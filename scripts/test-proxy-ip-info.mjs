#!/usr/bin/env node
// Run with: node scripts/test-proxy-ip-info.mjs
// Offline regression tests against the real TS helpers; no live nodes or database.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";

async function compile(path, module = ts.ModuleKind.ESNext) {
  const source = await readFile(new URL(`../${path}`, import.meta.url), "utf8");
  return ts.transpileModule(source, {
    fileName: path,
    compilerOptions: { target: ts.ScriptTarget.ES2022, module, sourceMap: false },
  }).outputText;
}
const dataUrl = (source) => `data:text/javascript;base64,${Buffer.from(source).toString("base64")}`;
const infoUrl = dataUrl(await compile("src/composables/proxy/proxyIpInfo.ts"));
const infoApi = await import(infoUrl);
const progressApi = await import(dataUrl((await compile("src/composables/proxy/proxyTestProgress.ts")).replace('"./proxyIpInfo"', JSON.stringify(infoUrl))));
const { proxyIpKind, proxyIpLabel, proxyIpDetails, proxyIpSearchText, validProxyIpInfo } = infoApi;
const { createProxyTestProgress } = progressApi;

function info(overrides = {}) {
  return { ip: "1.1.1.1", kind: "hosting", isp: "Example Cloud", organization: "Example ASN", asn: "AS13335", source: "ip-api.com", checkedAt: new Date().toISOString(), status: "success", error: "", ...overrides };
}
function node(overrides = {}) {
  return { id: "one", primaryIp: "1.1.1.1", ipInfo: info(), latencyMs: 10, testStatus: "success", channelLatencyMs: 100, channelTestStatus: "success", testedAt: "historical", ...overrides };
}
function event(overrides = {}) {
  return { nodeId: "one", phase: "completed", status: "success", latencyMs: 45, speedMs: 200, stage: "speed", completed: 1, total: 1, primaryIp: "1.1.1.1", ipInfo: null, ...overrides };
}
function state(value) { return { nodes: [value], channels: [{ node: structuredClone(value) }], activeNode: structuredClone(value) }; }

test("four labels remain conservative, missing and mismatched metadata stay unidentified", () => {
  for (const [kind, label] of Object.entries({ hosting: "云服务器/机房", residential: "疑似家宽", mobile: "移动网络", unknown: "未知" })) {
    assert.equal(proxyIpLabel(node({ ipInfo: info({ kind }) })), label);
  }
  assert.equal(proxyIpLabel(node({ ipInfo: null })), "未识别");
  assert.equal(proxyIpKind(node({ primaryIp: "8.8.8.8" })), "unidentified");
  assert.equal(proxyIpKind(node({ ipInfo: info({ status: "error", error: "timeout" }) })), "unknown");
  assert.equal(proxyIpKind(node({ ipInfo: info({ kind: "unexpected" }) })), "unknown");
});

test("invalid dates, future metadata and expired positive/negative cache are not shown", () => {
  for (const checkedAt of ["invalid", "", new Date(Date.now() + 86400_000).toISOString()]) {
    assert.equal(validProxyIpInfo(node({ ipInfo: info({ checkedAt }) })), null);
  }
  assert.equal(validProxyIpInfo(node({ ipInfo: info({ checkedAt: new Date(Date.now() - 86401_000).toISOString() }) })), null);
  assert.equal(validProxyIpInfo(node({ ipInfo: info({ kind: "unknown", status: "error", checkedAt: new Date(Date.now() - 301_000).toISOString() }) })), null);
  assert.ok(validProxyIpInfo(node({ ipInfo: info({ checkedAt: new Date(Date.now() - 3600_000).toISOString() }) })));
});

test("tooltip/search include ISP, ASN, source and failure without promising residential service", () => {
  const value = node({ ipInfo: info({ kind: "residential" }) });
  assert.match(proxyIpSearchText(value), /疑似家宽.*Example Cloud.*AS13335/);
  for (const fragment of ["1.1.1.1", "Example Cloud", "AS13335", "ip-api.com", "查询时间", "不保证"]) {
    assert.ok(proxyIpDetails(value).includes(fragment), fragment);
  }
  assert.match(proxyIpDetails(node({ ipInfo: info({ kind: "unknown", status: "error", error: "服务限流" }) })), /服务限流.*不影响测速/);
});

test("completed then ip-info in the same frame keeps speed results and counts exactly once", () => {
  const p = createProxyTestProgress(["one"]);
  const value = node();
  assert.equal(p.accept(event({ phase: "started", completed: 0 })), true);
  assert.equal(p.accept(event()), true);
  assert.equal(p.accept(event({ phase: "ip-info", latencyMs: null, speedMs: null, ipInfo: info({ kind: "mobile" }) })), true);
  p.apply(value);
  assert.equal(value.latencyMs, 45);
  assert.equal(value.channelLatencyMs, 200);
  assert.equal(value.ipInfo.kind, "mobile");
  assert.equal(p.succeeded, 1);
  assert.equal(p.failed, 0);
  assert.deepEqual(p.progress, { completed: 1, total: 1 });
  assert.equal(p.accept(event()), false);
  assert.equal(p.succeeded, 1);
});

test("metadata-only arrival after metrics flush cannot change speed, status or test timestamp", () => {
  const p = createProxyTestProgress(["one"]);
  const value = node();
  p.accept(event()); p.apply(value); p.clearPending();
  const metrics = [value.latencyMs, value.channelLatencyMs, value.testStatus, value.testedAt];
  p.accept(event({ phase: "ip-info", status: "error", latencyMs: null, speedMs: null, ipInfo: info({ kind: "unknown", status: "error", error: "timeout" }) }));
  p.apply(value);
  assert.deepEqual([value.latencyMs, value.channelLatencyMs, value.testStatus, value.testedAt], metrics);
  assert.equal(value.ipInfo.kind, "unknown");
  assert.equal(p.succeeded, 1);
  assert.equal(p.failed, 0);
});

test("new measurement clears historical metadata; failed or unrelated nodes reject enrichment", () => {
  const p = createProxyTestProgress(["one"]);
  const value = node();
  assert.equal(p.accept(event({ phase: "ip-info", ipInfo: info() })), false);
  assert.equal(p.accept(event({ nodeId: "other" })), false);
  p.accept(event({ status: "error", primaryIp: null, latencyMs: null, speedMs: null }));
  p.apply(value);
  assert.equal(value.ipInfo, null);
  assert.equal(value.primaryIp, "");
  assert.equal(p.accept(event({ phase: "ip-info", ipInfo: info() })), false);
  assert.equal(p.failed, 1);
});

test("mismatched and older IP events are ignored", () => {
  const p = createProxyTestProgress(["one"]);
  p.accept(event());
  assert.equal(p.accept(event({ phase: "ip-info", primaryIp: "8.8.8.8", ipInfo: info({ ip: "8.8.8.8" }) })), false);
  assert.equal(p.accept(event({ phase: "ip-info", ipInfo: info({ kind: "mobile" }) })), true);
  assert.equal(p.accept(event({ phase: "ip-info", ipInfo: info({ checkedAt: new Date(Date.now() - 3600_000).toISOString() }) })), false);
});

test("final snapshot remains authoritative and stale RAF metrics never overwrite it", () => {
  const p = createProxyTestProgress(["one"]);
  p.accept(event());
  p.accept(event({ phase: "ip-info", ipInfo: info({ kind: "mobile" }) }));
  const snapshot = state(node({ latencyMs: 123, testedAt: "server-time", ipInfo: null }));
  p.settle(snapshot); p.apply(snapshot.nodes[0]);
  assert.equal(snapshot.nodes[0].latencyMs, 123);
  assert.equal(snapshot.nodes[0].testedAt, "server-time");
  assert.equal(snapshot.nodes[0].ipInfo.kind, "mobile");
  assert.equal(snapshot.channels[0].node.ipInfo.kind, "mobile");
  assert.equal(snapshot.activeNode.ipInfo.kind, "mobile");
  assert.equal(p.dirty.size, 0);
  assert.equal(p.accept(event({ phase: "started" })), false);
});

test("SSE subscription resolves before stream end and receives metadata events", async () => {
  const compiled = await compile("src/composables/core/events.ts", ts.ModuleKind.CommonJS);
  let controller, aborted = false;
  const sandbox = {
    exports: {}, console, TextDecoder, DOMException, AbortController,
    window: { setTimeout, clearTimeout },
    require: () => ({ clientMode: "web", isIntegratedClient: false, getSessionToken: () => "", notifyAuthExpired() {}, AuthExpiredError: class extends Error {} }),
    fetch: async (_, options) => {
      const body = new ReadableStream({ start(value) { controller = value; } });
      options.signal.addEventListener("abort", () => { aborted = true; controller.close(); });
      return new Response(body, { headers: { "Content-Type": "text/event-stream" } });
    },
  };
  vm.runInNewContext(compiled, sandbox);
  let timeout;
  const received = [];
  try {
    const unsubscribe = await Promise.race([
      sandbox.exports.listen("proxy-node-test-progress", e => received.push(e.payload)),
      new Promise((_, reject) => { timeout = setTimeout(() => reject(new Error("SSE must not wait until the stream closes")), 500); }),
    ]);
    controller.enqueue(new TextEncoder().encode('data: {"event":"proxy-node-test-progress","payload":{"phase":"ip-info","nodeId":"one"}}\n\n'));
    await new Promise(resolve => setTimeout(resolve, 10));
    assert.equal(received.length, 1);
    assert.equal(received[0].phase, "ip-info");
    unsubscribe();
    assert.equal(aborted, true);
  } finally {
    clearTimeout(timeout);
    sandbox.exports.resetEventSource();
  }
});

test("runId rejects late events from previous batches, including legacy untagged events", () => {
  const p = createProxyTestProgress(["one"], "current-run");
  assert.equal(p.accept(event({ runId: "previous-run", completed: 100, total: 100 })), false);
  assert.equal(p.accept(event()), false);
  assert.equal(p.accept(event({ runId: "current-run", latencyMs: 222 })), true);
  assert.equal(p.accept(event({ phase: "ip-info", runId: "previous-run", ipInfo: info() })), false);
  assert.equal(p.accept(event({ phase: "ip-info", runId: "current-run", ipInfo: info() })), true);
  const value = node(); p.apply(value);
  assert.equal(value.latencyMs, 222);
  assert.equal(p.succeeded, 1);
  assert.deepEqual(p.progress, { completed: 1, total: 1 });
});

test("stale non-test snapshots keep changed test metadata without undoing settings", () => {
  const current = state(node({ primaryIp: "8.8.8.8", latencyMs: 222, countryCode: "US", ipInfo: info({ ip: "8.8.8.8", kind: "mobile" }) }));
  const stale = state(node({ name: "New configured name" }));
  stale.enabled = false;
  stale.activeNode = null;
  stale.channels[0].name = "New channel name";
  const merged = progressApi.mergeProxyTestMetadata(stale, current, new Set(["one"]));
  assert.equal(merged.nodes[0].primaryIp, "8.8.8.8");
  assert.equal(merged.nodes[0].latencyMs, 222);
  assert.equal(merged.nodes[0].ipInfo.kind, "mobile");
  assert.equal(merged.nodes[0].name, "New configured name");
  assert.equal(merged.channels[0].node.primaryIp, "8.8.8.8");
  assert.equal(merged.channels[0].name, "New channel name");
  assert.equal(merged.activeNode, null);
  assert.equal(merged.enabled, false);
  assert.equal(stale.nodes[0].primaryIp, "1.1.1.1", "merge must not mutate incoming snapshot");
});

test("snapshot merge neither revives deleted nodes nor overwrites unchanged nodes", () => {
  const current = state(node({ primaryIp: "8.8.8.8" }));
  const incoming = state(node());
  const unchanged = progressApi.mergeProxyTestMetadata(incoming, current, new Set());
  assert.equal(unchanged.nodes[0].primaryIp, "1.1.1.1");
  incoming.nodes = []; incoming.channels = []; incoming.activeNode = null;
  const deleted = progressApi.mergeProxyTestMetadata(incoming, current, new Set(["one"]));
  assert.deepEqual(deleted.nodes, []);
  assert.deepEqual(deleted.channels, []);
  assert.equal(deleted.activeNode, null);
});
