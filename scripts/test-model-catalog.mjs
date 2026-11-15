#!/usr/bin/env node
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import ts from "typescript";

async function load(path, replace = (value) => value) {
  const source = await readFile(new URL(path, import.meta.url), "utf8");
  const compiled = ts.transpileModule(source, { compilerOptions: {
    target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext,
  } }).outputText;
  return import(`data:text/javascript;base64,${Buffer.from(replace(compiled)).toString("base64")}`);
}
const view = await load("../src/composables/model/catalogView.ts");
const fixture = await load("../src/composables/core/mockData.ts");
const base = fixture.previewModels[0];
const model = (id, changes = {}) => ({ ...structuredClone(base), id, name: id, ...changes });
const index = (models) => view.buildCatalogIndex(models, [{ id: "host", name: "Display Provider" }]);
const filters = (changes = {}) => ({ ...view.emptyCatalogFilters(), ...changes });

test("multi-word search matches normalized lab, model aliases and provider display name", () => {
  const models = index([model("one", { officialLab: "alibaba", hostProviders: ["host"], family: "family-42" }), model("two", { hostProviders: [] })]);
  assert.deepEqual(view.filterCatalog(models, filters({ query: "Qwen family-42 Display" }), "name").map((m) => m.id), ["one"]);
  assert.equal(view.filterCatalog(models, filters({ query: "Qwen missing" }), "name").length, 0);
});

test("all visible filters combine and resetting creates independent capability arrays", () => {
  const models = index([
    model("match", { kind: "text", officialLab: "openai", contextLength: 256000, reasoning: true, toolCall: true, status: "ga", hostProviders: ["host"], officialHostCount: 1 }),
    model("no-tools", { kind: "text", officialLab: "openai", contextLength: 256000, reasoning: true, toolCall: false, status: "ga", hostProviders: ["host"], officialHostCount: 1 }),
  ]);
  const selected = filters({ kind: "text", lab: "openai", provider: "host", status: "ga", minContext: 128000, origin: "official", capabilities: ["reasoning", "toolCall"] });
  assert.deepEqual(view.filterCatalog(models, selected, "name").map((m) => m.id), ["match"]);
  selected.capabilities.push("openWeights");
  assert.deepEqual(view.emptyCatalogFilters().capabilities, []);
  assert.equal(view.filterCatalog(models, filters({ status: "preview" }), "name").length, 0);
});

test("free, subscription and missing prices have different filtering and sorting semantics", () => {
  const models = index([
    model("missing", { freeHostCount: 0, subHostCount: 0, subscriptionChannelProviders: [], minInputCost: 0, minOutputCost: 0, refInputCost: 0, refOutputCost: 0 }),
    model("subscription", { freeHostCount: 0, subHostCount: 1, subscriptionChannelProviders: ["host"], minInputCost: 0, minOutputCost: 0, refInputCost: 0, refOutputCost: 0 }),
    model("paid", { freeHostCount: 0, subHostCount: 0, subscriptionChannelProviders: [], minInputCost: 0.1 }),
    model("free", { freeHostCount: 1, subHostCount: 0, subscriptionChannelProviders: [] }),
  ]);
  assert.deepEqual(view.filterCatalog(models, filters({ pricing: "free" }), "name").map((m) => m.id), ["free"]);
  assert.deepEqual(view.filterCatalog(models, filters({ pricing: "subscription" }), "name").map((m) => m.id), ["subscription"]);
  assert.deepEqual(view.filterCatalog(models, filters(), "price").map((m) => m.id), ["free", "paid", "missing", "subscription"]);
  assert.equal(view.priceText(null), "未标价");
  assert.equal(view.priceText(0), "$0");
  assert.equal(view.referencePriceText(model("unknown", { refOfficial: true, refInputCost: 0, refOutputCost: 0, refProvider: "host", freeChannelProviders: [], subscriptionChannelProviders: [] })), "官方未标价");
});

test("unknown numeric values sort last; real zero scores remain valid and ties are stable", () => {
  const models = index([
    model("unknown", { aaIdx: null, contextLength: 0 }),
    model("zero", { aaIdx: 0, contextLength: 32000 }),
    model("z", { aaIdx: 20, contextLength: 128000 }),
    model("a", { aaIdx: 20, contextLength: 128000 }),
  ]);
  assert.deepEqual(view.filterCatalog(models, filters(), "quality").map((m) => m.id), ["a", "z", "zero", "unknown"]);
  assert.deepEqual(view.filterCatalog(models, filters(), "context").map((m) => m.id), ["a", "z", "zero", "unknown"]);
  assert.deepEqual(models.map((entry) => entry.model.id), ["unknown", "zero", "z", "a"]);
});

test("pagination clamps out-of-range pages after a snapshot shrinks", () => {
  const values = Array.from({ length: 26 }, (_, i) => i);
  assert.deepEqual(view.catalogPage(values, 999, 25), { page: 2, pages: 2, start: 26, end: 26, items: [25] });
  assert.deepEqual(view.catalogPage([], 99, 25), { page: 1, pages: 1, start: 0, end: 0, items: [] });
  assert.equal(view.catalogPage(values, -1, 25).page, 1);
});

test("comparison limit does not silently discard previously selected models", () => {
  const ids = ["a", "b", "c", "d"];
  assert.equal(view.toggleCatalogComparison(ids, "e"), ids);
  assert.deepEqual(view.toggleCatalogComparison(ids, "b"), ["a", "c", "d"]);
  assert.deepEqual(view.toggleCatalogComparison(["a"], "b"), ["a", "b"]);
});

test("comparison exposes exact token differences hidden by compact K/M formatting", () => {
  const a = model("a", { contextLength: 128000, maxInputTokens: null, maxOutputTokens: 8192 });
  const b = model("b", { contextLength: 128001, maxInputTokens: null, maxOutputTokens: 8193 });
  assert.equal(view.tokenText(a.contextLength), view.tokenText(b.contextLength));
  const rows = view.catalogComparisonRows([a, b]);
  const context = rows.find((row) => row.label === "上下文窗口");
  assert.deepEqual(context.values, ["128,000 tokens", "128,001 tokens"]);
  assert.equal(context.differs, true);
  assert.equal(rows.find((row) => row.label === "最大输出").differs, true);
  assert.equal(rows.find((row) => row.label === "最大输入").differs, false);
  assert.equal(view.tokenExactText(NaN), "未声明");
  assert.equal(view.tokenExactText(0), "未声明");
});

test("cost estimates use one host's quote and do not turn missing or subscription prices into free", () => {
  const host = { input: 2, output: 8, subscription: false, isFree: false };
  assert.equal(view.estimateHostCost(host, 10, 2), 36);
  assert.equal(view.estimateHostCost({ ...host, output: null }, 10, 2), null);
  assert.equal(view.estimateHostCost({ ...host, input: null }, 0, 2), 16);
  assert.equal(view.estimateHostCost({ ...host, subscription: true }, 10, 2), null);
  assert.equal(view.estimateHostCost({ ...host, isFree: true }, 10, 2), 0);
  assert.equal(view.estimateHostCost(host, -1, 2), null);
  assert.equal(view.hostPriceText({ ...host, input: null }, "input"), "未标价");
  assert.equal(view.hostPriceText({ ...host, subscription: true }, "input"), "订阅制");
});

test("reasoning declarations, missing capacity and external links retain their meaning", () => {
  assert.equal(view.reasoningText({ reasoning: true, reasoningOptions: [] }), "支持，档位未声明");
  assert.equal(view.reasoningText({ reasoning: true, reasoningOptions: [{ kind: "toggle", values: [] }] }), "开 / 关");
  assert.deepEqual(view.reasoningLevels([{ kind: "effort", values: ["low", "high"] }, { kind: "effort", values: ["high", "max"] }]), ["low", "high", "max"]);
  assert.equal(view.tokenText(0), "未声明");
  assert.equal(view.safeCatalogUrl("javascript:alert(1)"), null);
  assert.equal(view.safeCatalogUrl("https://user:password@example.com"), null);
  assert.equal(view.safeCatalogUrl("https://example.com/docs"), "https://example.com/docs");
});

// Inject transport, events and timers into the real catalog composable for race/lifecycle tests.
const vueUrl = import.meta.resolve("vue");
let harnessId = 0;
async function stateHarness(command, listenImpl) {
  const key = `__catalog_test_${harnessId++}`;
  const timers = new Map();
  const toasts = [];
  let timerId = 0;
  let eventHandler;
  globalThis[key] = {
    command,
    listen: listenImpl ?? (async (_, handler) => { eventHandler = handler; return () => { eventHandler = undefined; }; }),
    useToast: () => ({ showToast: (...args) => toasts.push(args) }),
    window: { setTimeout: (callback) => { const id = ++timerId; timers.set(id, callback); return id; }, clearTimeout: (id) => timers.delete(id) },
  };
  const api = await load("../src/composables/model/useModelCatalog.ts", (text) => text
    .replace('from "vue"', `from ${JSON.stringify(vueUrl)}`)
    .replace('import { listen } from "../core/events";', `const listen = globalThis.${key}.listen;`)
    .replace('import { runCommand } from "../core/ipc";', `const runCommand = globalThis.${key}.command;`)
    .replace('import { useToast } from "../core/useToast";', `const useToast = globalThis.${key}.useToast; const window = globalThis.${key}.window;`));
  return { ...api.useModelCatalog(), timers, toasts, event: (payload) => eventHandler?.({ payload }), cleanup: () => { api.useModelCatalog().stopModelCatalogEvents(); delete globalThis[key]; } };
}
const deferred = () => { let resolve, reject; const promise = new Promise((done, fail) => { resolve = done; reject = fail; }); return { promise, resolve, reject }; };
const snapshot = (name, syncedToday = true) => ({ models: [model(name)], providers: [], total: 1, sources: [], meta: {}, lastSyncedAt: "2026-11-15", syncedToday });
const syncReply = (name) => ({ snapshot: snapshot(name), synced: true, skipped: false, message: "同步完成" });
const flush = () => new Promise((resolve) => setImmediate(resolve));

test("old reads cannot replace a synchronized snapshot; concurrent calls coalesce", async () => {
  const read = deferred(), sync = deferred();
  const calls = [];
  const state = await stateHarness(async (command) => { calls.push(command); return command === "get_model_catalog" ? read.promise : sync.promise; });
  try {
    const first = state.loadModelCatalog();
    assert.equal(state.loadModelCatalog(), first);
    const saving = state.syncModelCatalog(true);
    assert.equal(state.syncModelCatalog(true), saving);
    sync.resolve(syncReply("new"));
    await saving;
    read.resolve(snapshot("old"));
    await first;
    assert.equal(state.modelCatalog.value.models[0].id, "new");
    assert.deepEqual(calls, ["get_model_catalog", "sync_model_catalog"]);
    assert.equal(state.modelCatalogLoading.value, false);
    assert.equal(state.modelCatalogSyncing.value, false);
  } finally { state.cleanup(); }
});

test("failed synchronization preserves the previous snapshot and exposes an actionable error", async () => {
  const state = await stateHarness(async (command) => {
    if (command === "get_model_catalog") return snapshot("cached");
    throw new Error("source timeout");
  });
  try {
    await state.loadModelCatalog();
    assert.equal(await state.syncModelCatalog(true), null);
    assert.equal(state.modelCatalog.value.models[0].id, "cached");
    assert.match(state.modelCatalogError.value, /source timeout/);
    assert.equal(state.modelCatalogSyncing.value, false);
  } finally { state.cleanup(); }
});

test("Web-compatible progress events update status and reload a completed remote sync", async () => {
  let current = "initial";
  const state = await stateHarness(async () => snapshot(current));
  try {
    await state.initializeModelCatalog();
    state.event({ status: "syncing", message: "下载分片 2/3" });
    assert.equal(state.modelCatalogSyncing.value, true);
    assert.equal(state.modelCatalogSyncMessage.value, "下载分片 2/3");
    current = "remote-new";
    state.event({ status: "complete", message: "完成" });
    await flush();
    assert.equal(state.modelCatalog.value.models[0].id, "remote-new");
    assert.equal(state.modelCatalogSyncing.value, false);
  } finally { state.cleanup(); }
});

test("stopping during event registration disposes the late listener without starting reads", async () => {
  const registration = deferred();
  let disposed = 0, commands = 0;
  const state = await stateHarness(async () => { commands++; return snapshot("unused"); }, async () => registration.promise);
  const initializing = state.initializeModelCatalog();
  state.stopModelCatalogEvents();
  registration.resolve(() => { disposed++; });
  await initializing;
  assert.equal(disposed, 1);
  assert.equal(commands, 0);
  assert.equal(state.timers.size, 0);
  state.cleanup();
});

test("stopping an in-flight daily sync prevents stale updates and timer resurrection", async () => {
  const syncing = deferred();
  const state = await stateHarness(async (command) => command === "get_model_catalog" ? snapshot("cached") : syncing.promise);
  await state.initializeModelCatalog();
  assert.equal(state.timers.size, 1);
  const [id, callback] = [...state.timers][0];
  state.timers.delete(id);
  const running = callback();
  await flush();
  state.stopModelCatalogEvents();
  syncing.resolve(syncReply("late"));
  await running;
  assert.equal(state.modelCatalog.value.models[0].id, "cached");
  assert.equal(state.timers.size, 0);
  assert.equal(state.modelCatalogSyncing.value, false);
  state.cleanup();
});
