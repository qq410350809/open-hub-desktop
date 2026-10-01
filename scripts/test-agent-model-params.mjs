#!/usr/bin/env node
// Run with: node scripts/test-agent-model-params.mjs
// Only reads the real TS module below. All configuration/preferences are synthetic,
// and save/read/reload is an in-memory JSON round trip, not a real Agent file write.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import ts from "typescript";

const moduleUrl = new URL("../src/composables/localtools/modelParams.ts", import.meta.url);
const source = await readFile(moduleUrl, "utf8");
const compiled = ts.transpileModule(source, {
  fileName: "modelParams.ts",
  compilerOptions: {
    target: ts.ScriptTarget.ES2022,
    module: ts.ModuleKind.ESNext,
    verbatimModuleSyntax: true,
    sourceMap: false,
  },
  reportDiagnostics: true,
});
assert.deepEqual(
  (compiled.diagnostics ?? []).filter((item) => item.category === ts.DiagnosticCategory.Error),
  [],
  "the real modelParams.ts must transpile without errors",
);
const api = await import(`data:text/javascript;base64,${Buffer.from(compiled.outputText).toString("base64")}`);
const {
  THINKING_EFFORT_OPTIONS, SOURCE_LABELS, normalizeEfforts, catalogEfforts,
  catalogDefaultEffort, catalogParams, snapshotParams, patchModelParams, modelParamGroupKey,
  resolveModelParams, hydrateModelParams,
} = api;
const ALL_EFFORTS = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
const FIELDS = ["contextWindow", "maxOutput", "defaultReasoningEffort", "efforts"];
const values = (result) => Object.fromEntries(FIELDS.map((key) => [key, result[key]]));
const sources = (result) => Object.fromEntries(FIELDS.map((key) => [key, result[`${key}Source`]]));

function caps(overrides = {}) {
  return {
    contextLength: 200_000, maxOutputTokens: 64_000,
    supportsReasoning: true, reasoningOptions: [{ kind: "effort", values: ["low", "high"] }],
    outputModalities: ["text"], interleavedFields: [], hasFastMode: false,
    supportsTemperature: true, supportsToolCall: true, supportsStructuredOutput: false,
    openWeights: false, ...overrides,
  };
}

function snapshot(overrides = {}) {
  return {
    tool: "opencode", toolName: "Synthetic test Agent", files: [], providers: [], models: [],
    contentHash: "synthetic", effectNote: "", warning: "", providerMode: "all",
    ...overrides,
    defaults: {
      model: "", provider: "", reasoningEffort: "", reasoningEffortOptions: [],
      perModelEffort: {}, ...overrides.defaults,
    },
    context: {
      contextWindow: null, maxOutputTokens: null, autoCompactTokenLimit: null,
      maxThinkingTokens: null, ...overrides.context,
    },
    thinking: {
      effortLevel: "", effortLevelOptions: [], maxThinkingTokens: null, ...overrides.thinking,
    },
  };
}

function model(provider, id, contextWindow = 0, maxOutput = 0) {
  return { provider, id, name: id, contextWindow, maxOutput };
}

function freezeDeep(value) {
  for (const child of Object.values(value)) {
    if (child !== null && typeof child === "object") freezeDeep(child);
  }
  return Object.freeze(value);
}

test("public API, source labels and empty resolution", () => {
  assert.deepEqual([...THINKING_EFFORT_OPTIONS], ALL_EFFORTS);
  assert.deepEqual(SOURCE_LABELS, {
    override: "自定义", parentUser: "父级自定义", catalog: "目录",
    hydrated: "配置文件", snapshot: "Agent 默认", none: "未设",
  });
  // ModelParamOverride is deliberately a TS-only export, erased with its type imports.
  assert.equal(Object.hasOwn(api, "ModelParamOverride"), false);
  for (const name of ["normalizeEfforts", "catalogEfforts", "catalogDefaultEffort", "catalogParams",
    "snapshotParams", "patchModelParams", "modelParamGroupKey", "resolveModelParams", "hydrateModelParams"]) {
    assert.equal(typeof api[name], "function", name);
  }
  assert.deepEqual(resolveModelParams(), {
    contextWindow: null, maxOutput: null, defaultReasoningEffort: "", efforts: [],
    contextWindowSource: "none", maxOutputSource: "none",
    defaultReasoningEffortSource: "none", effortsSource: "none",
    hasContextWindowOverride: false, hasMaxOutputOverride: false,
    hasEffortOverride: false, hasEffortsListOverride: false,
    isOverridden: false, overriddenCount: 0,
  });
});

test("normalizeEfforts maps none to off, trims, deduplicates and orders without mutation", () => {
  const input = Object.freeze([" MAX ", "None", "HIGH", "low", "OFF", "unknown", "", "high", "minimal"]);
  assert.deepEqual(normalizeEfforts(input), ["off", "minimal", "low", "high", "max"]);
  assert.deepEqual(normalizeEfforts([]), []);
});

test("catalog no-reasoning, toggle-only and explicitly empty effort lists remain []", () => {
  for (const capability of [
    caps({ supportsReasoning: false, reasoningOptions: [] }),
    caps({ reasoningOptions: [{ kind: "toggle", values: [] }] }),
    caps({ reasoningOptions: [{ kind: "effort", values: [] }] }),
  ]) {
    assert.deepEqual(catalogEfforts(capability), []);
    const result = resolveModelParams({}, {}, capability, { efforts: ["high"] }, { efforts: ALL_EFFORTS });
    assert.deepEqual(result.efforts, []);
    assert.equal(result.effortsSource, "catalog");
  }
});

test("unknown reasoning with no effort declaration is null, not an empty or full list", () => {
  for (const capability of [null, undefined, caps({ reasoningOptions: [] }),
    caps({ reasoningOptions: [{ kind: "budget", values: [] }] })]) {
    assert.equal(catalogEfforts(capability), null);
    assert.equal(Object.hasOwn(catalogParams(capability), "efforts"), false);
  }
  const result = resolveModelParams({}, {}, caps({ reasoningOptions: [] }), { efforts: ["medium"] });
  assert.deepEqual(result.efforts, ["medium"]);
  assert.equal(result.effortsSource, "hydrated");
});

test("catalogParams normalizes declared efforts and omits unknown numeric limits", () => {
  assert.deepEqual(catalogParams(null), {});
  assert.deepEqual(catalogParams(caps({
    contextLength: 0, maxOutputTokens: -1,
    reasoningOptions: [
      { kind: "effort", values: ["none", "HIGH"] },
      { kind: "effort", values: ["low", "high", "invalid"] },
      { kind: "toggle", values: [] },
    ],
  })), { efforts: ["off", "low", "high"], defaultReasoningEffort: "high" });
});

test("snapshotParams restores Agent defaults and falls back to thinking settings", () => {
  const snap = freezeDeep(snapshot({
    context: { contextWindow: 0, maxOutputTokens: 8192 },
    defaults: { reasoningEffort: "low", reasoningEffortOptions: ["high", "none"] },
    thinking: { effortLevel: "max", effortLevelOptions: ["medium"] },
  }));
  assert.deepEqual(snapshotParams(snap), {
    contextWindow: 0, maxOutput: 8192, defaultReasoningEffort: "low", efforts: ["off", "high"],
  });
  assert.deepEqual(snapshotParams(snapshot({
    thinking: { effortLevel: "medium", effortLevelOptions: ["HIGH", "none", "low"] },
  })), { contextWindow: null, maxOutput: null, defaultReasoningEffort: "medium", efforts: ["off", "low", "high"] });
  assert.deepEqual(snapshotParams(null), {
    contextWindow: null, maxOutput: null, defaultReasoningEffort: "", efforts: ALL_EFFORTS,
  });
});

test("catalog default effort is the model's highest configurable level and precedes disk", () => {
  // 用户要求：默认思考级别取「最大可配置的思考级别」。
  assert.equal(catalogDefaultEffort(caps()), "high");
  assert.equal(catalogDefaultEffort(caps({
    reasoningOptions: [{ kind: "effort", values: ["none", "minimal", "low", "medium", "high", "xhigh", "max"] }],
  })), "max");
  assert.equal(catalogDefaultEffort(caps({ reasoningOptions: [{ kind: "effort", values: ["none"] }] })), "off");
  // 只有后端算好的最高档（无档位集合）时也应采用。
  assert.equal(catalogDefaultEffort(caps({ reasoningOptions: [], reasoningEffortMax: "xhigh" })), "xhigh");
  // 未声明（未命中 / 仅 toggle / 不支持思考）不许臆造。
  for (const capability of [null, undefined, caps({ supportsReasoning: false, reasoningOptions: [] }),
    caps({ reasoningOptions: [{ kind: "toggle", values: [] }] })]) {
    assert.equal(catalogDefaultEffort(capability), null);
    assert.equal(Object.hasOwn(catalogParams(capability), "defaultReasoningEffort"), false);
  }

  const disk = { contextWindow: 111, maxOutput: 222, efforts: ["max"], defaultReasoningEffort: "medium" };
  const fallback = { contextWindow: 333, maxOutput: 444, efforts: ALL_EFFORTS, defaultReasoningEffort: "low" };
  const result = resolveModelParams({}, {}, caps(), disk, fallback);
  assert.deepEqual(values(result), {
    contextWindow: 200_000, maxOutput: 64_000, defaultReasoningEffort: "high", efforts: ["low", "high"],
  });
  assert.deepEqual(sources(result), {
    contextWindow: "catalog", maxOutput: "catalog", defaultReasoningEffort: "catalog", efforts: "catalog",
  });
  // 用户显式覆盖仍然优先于目录最高档。
  assert.equal(resolveModelParams({ defaultReasoningEffort: "low" }, {}, caps()).defaultReasoningEffort, "low");
  assert.equal(resolveModelParams({}, { defaultReasoningEffort: "minimal" }, caps()).defaultReasoningEffort, "minimal");
});

function checkFieldWiseResolution(resolve) {
  const child = freezeDeep({ contextWindow: 123_456 });
  const parent = freezeDeep({ contextWindow: 999, maxOutput: 4096, defaultReasoningEffort: "medium" });
  const result = resolve(child, parent, caps(), { efforts: ["max"] }, { maxOutput: 10 });
  assert.deepEqual(values(result), {
    contextWindow: 123_456, maxOutput: 4096, defaultReasoningEffort: "medium", efforts: ["low", "high"],
  });
  assert.deepEqual(sources(result), {
    contextWindow: "override", maxOutput: "parentUser", defaultReasoningEffort: "parentUser", efforts: "catalog",
  });
  assert.equal(result.overriddenCount, 1);
  assert.equal(result.hasMaxOutputOverride, false);
  // 父级思考级别筛选是勾选集，必须落到模型行，不能被目录声明全集盖掉。
  const filtered = resolve(child, { ...parent, efforts: ["high", "max", "xhigh"] }, caps(), { efforts: ["max"] });
  assert.deepEqual(filtered.efforts, ["high"]);
  assert.equal(filtered.effortsSource, "parentUser");
  assert.equal(filtered.hasEffortsListOverride, false);
  // 目录未声明档位时，父级筛选原样生效（站点可自行支持更多档）。
  const undeclared = resolve({}, { efforts: ["off", "xhigh"] }, caps({ reasoningOptions: [] }), { efforts: ["max"] });
  assert.deepEqual(undeclared.efforts, ["off", "xhigh"]);
  assert.equal(undeclared.effortsSource, "parentUser");
}

test("child > parent > catalog is selected per field, never as a whole group", () => {
  checkFieldWiseResolution(resolveModelParams);
});

test("sparse disk and snapshot are also selected per field", () => {
  const result = resolveModelParams({}, {}, null, { maxOutput: 0, defaultReasoningEffort: "" }, {
    contextWindow: 32768, maxOutput: 999, defaultReasoningEffort: "high", efforts: ["low"],
  });
  assert.deepEqual(values(result), {
    contextWindow: 32768, maxOutput: 0, defaultReasoningEffort: "", efforts: ["low"],
  });
  assert.deepEqual(sources(result), {
    contextWindow: "snapshot", maxOutput: "hydrated", defaultReasoningEffort: "hydrated", efforts: "snapshot",
  });
  assert.equal(result.isOverridden, false);
});

function checkSparsePatch(patch) {
  const current = Object.freeze({ defaultReasoningEffort: "medium" });
  const edit = Object.freeze({ maxOutput: 4096 });
  const result = patch(current, edit);
  assert.deepEqual(result, { defaultReasoningEffort: "medium", maxOutput: 4096 });
  assert.notEqual(result, current);
  assert.deepEqual(current, { defaultReasoningEffort: "medium" });
  assert.equal(Object.hasOwn(result, "contextWindow"), false);
  assert.equal(Object.hasOwn(result, "efforts"), false);
}

test("patchModelParams only changes edited fields and does not capture inherited values", () => {
  checkSparsePatch(patchModelParams);
  const child = patchModelParams({}, { maxOutput: 4096 });
  assert.deepEqual(child, { maxOutput: 4096 });
  assert.equal(resolveModelParams(child, {}, caps()).contextWindowSource, "catalog");
});

test("0, null, [], and empty string remain explicit overrides, with accurate flags", () => {
  const parent = { contextWindow: 12, maxOutput: 34, defaultReasoningEffort: "high", efforts: ["high"] };
  for (const limits of [{ contextWindow: 0, maxOutput: null }, { contextWindow: null, maxOutput: 0 }]) {
    const explicit = { ...limits, defaultReasoningEffort: "", efforts: [] };
    const child = patchModelParams({}, explicit);
    assert.deepEqual(child, explicit);
    const result = resolveModelParams(child, parent, caps(), parent, parent);
    assert.deepEqual(values(result), explicit);
    assert.deepEqual(sources(result), Object.fromEntries(FIELDS.map((key) => [key, "override"])));
    assert.equal(result.overriddenCount, 4);
    assert.equal(result.isOverridden, true);
    for (const key of ["hasContextWindowOverride", "hasMaxOutputOverride", "hasEffortOverride", "hasEffortsListOverride"]) {
      assert.equal(result[key], true, key);
    }
  }
});

test("undefined removes own keys and restores inheritance independently for every field", () => {
  const current = freezeDeep({ contextWindow: 0, maxOutput: null, defaultReasoningEffort: "", efforts: [] });
  const parent = { contextWindow: 128_000, maxOutput: 8192, defaultReasoningEffort: "high", efforts: ["low", "high"] };
  for (const key of FIELDS) {
    const child = patchModelParams(current, { [key]: undefined });
    assert.equal(Object.hasOwn(child, key), false, key);
    const result = resolveModelParams(child, parent, caps());
    // 思考级别筛选与目录声明求交：父级 ["low","high"] 与目录 ["low","high"] 仍是父级。
    const expected = key === "efforts" ? ["low", "high"] : parent[key];
    assert.deepEqual(result[key], expected, key);
    assert.equal(result[`${key}Source`], "parentUser", key);
    assert.equal(result.overriddenCount, 3);
  }
  const cleared = patchModelParams(current, Object.fromEntries(FIELDS.map((key) => [key, undefined])));
  assert.deepEqual(cleared, {});
  assert.equal(resolveModelParams(cleared, parent, caps()).isOverridden, false);
  assert.equal(resolveModelParams({ contextWindow: undefined }, parent).contextWindowSource, "parentUser");
});

test("equal-to-catalog/disk custom values retain override provenance, not value-equality inference", () => {
  const explicit = { contextWindow: 200_000, maxOutput: 64_000, defaultReasoningEffort: "high", efforts: ["low", "high"] };
  for (const capability of [caps(), null]) {
    const child = patchModelParams({}, explicit);
    const result = resolveModelParams(child, {}, capability, structuredClone(explicit), structuredClone(explicit));
    assert.deepEqual(values(result), explicit);
    assert.deepEqual(sources(result), Object.fromEntries(FIELDS.map((key) => [key, "override"])));
    assert.equal(result.overriddenCount, 4);
    assert.deepEqual(child, explicit);
  }
  const parentResult = resolveModelParams({}, explicit, caps(), explicit);
  assert.deepEqual(sources(parentResult), Object.fromEntries(FIELDS.map((key) => [key, "parentUser"])));
  assert.equal(parentResult.overriddenCount, 0);
});

test("catalog refresh updates unedited fields while keeping child and parent overrides", () => {
  const child = patchModelParams({}, { maxOutput: 4096 });
  const parent = { defaultReasoningEffort: "medium" };
  const before = resolveModelParams(child, parent, caps());
  const after = resolveModelParams(child, parent, caps({
    contextLength: 1_000_000, maxOutputTokens: 128_000,
    reasoningOptions: [{ kind: "effort", values: ["none", "minimal", "max"] }],
  }));
  assert.equal(before.contextWindow, 200_000);
  assert.deepEqual(before.efforts, ["low", "high"]);
  assert.deepEqual(values(after), {
    contextWindow: 1_000_000, maxOutput: 4096, defaultReasoningEffort: "medium", efforts: ["off", "minimal", "max"],
  });
  assert.deepEqual(sources(after), {
    contextWindow: "catalog", maxOutput: "override", defaultReasoningEffort: "parentUser", efforts: "catalog",
  });
  assert.deepEqual(child, { maxOutput: 4096 });
});

test("model grouping uses model::barelowercase across route aliases; channels use channel::account", () => {
  for (const [channel, account, id] of [
    ["route-a", "first", "vendor-a/GPT-5"],
    ["route-b", "second", "route-b/vendor-b/gpt-5"],
    ["route-c", "third", "gPt-5"],
  ]) {
    assert.equal(modelParamGroupKey(true, channel, account, id), "model::gpt-5");
    assert.equal(modelParamGroupKey(false, channel, account, id), `${channel}::${account}`);
  }
  assert.notEqual(modelParamGroupKey(true, "a", "b", "gpt-5"), modelParamGroupKey(true, "a", "b", "gpt-4"));
  assert.equal(modelParamGroupKey(false, "a", "one", "x"), modelParamGroupKey(false, "a", "one", "y"));
  assert.notEqual(modelParamGroupKey(false, "a", "one", "x"), modelParamGroupKey(false, "a", "two", "x"));
});

test("OpenCode snapshot effort keys keep the provider-prefixed model ID", () => {
  const snap = freezeDeep(snapshot({
    tool: "opencode", models: [model("route-a", "route-a/vendor/model-x", 128_000, 8192)],
    defaults: { perModelEffort: { "route-a/vendor/model-x": " high,none,LOW,high ", "route-b/effort-only": "max" } },
  }));
  assert.deepEqual(hydrateModelParams(snap), {
    "route-a::route-a/vendor/model-x": { contextWindow: 128_000, maxOutput: 8192, efforts: ["off", "low", "high"] },
    "route-b::route-b/effort-only": { efforts: ["max"] },
  });
});

test("dsh snapshot effort keys strip only the provider prefix and retain model namespaces", () => {
  const snap = freezeDeep(snapshot({
    tool: "dsh", models: [model("route-a", "vendor/model-x", 96_000, 4096)],
    defaults: { perModelEffort: { "route-a/vendor/model-x": "none,medium", "route-b/effort-only": "" } },
  }));
  assert.deepEqual(hydrateModelParams(snap), {
    "route-a::vendor/model-x": { contextWindow: 96_000, maxOutput: 4096, efforts: ["off", "medium"] },
    "route-b::effort-only": { efforts: [] },
  });
});

test("Claude opus/sonnet/haiku mappings are not mistaken for effort lists", () => {
  assert.deepEqual(hydrateModelParams(snapshot({
    tool: "claude", models: [model("anthropic", "claude-sonnet", 200_000, 8192)],
    defaults: { perModelEffort: {
      opus: "anthropic/claude-opus", sonnet: "anthropic/claude-sonnet", haiku: "anthropic/claude-haiku",
      malformed: "high", "/missing-provider": "low",
    } },
  })), { "anthropic::claude-sonnet": { contextWindow: 200_000, maxOutput: 8192 } });
});

test("hydrate rebuilds disk state, drops removed models/fields and never mutates child preferences", () => {
  const children = freezeDeep({
    "route-a::keep": { contextWindow: 50_000, efforts: [...ALL_EFFORTS] },
    "route-a::removed": { maxOutput: 1234 },
  });
  const childrenBefore = JSON.stringify(children);
  const firstSnap = freezeDeep(snapshot({
    tool: "dsh", models: [model("route-a", "keep", 50_000, 8192), model("route-a", "removed", 64_000, 4096)],
    defaults: { perModelEffort: { "route-a/keep": ALL_EFFORTS.join(","), "route-a/removed": "high" } },
  }));
  const first = freezeDeep(hydrateModelParams(firstSnap));
  let disk = first;
  disk = hydrateModelParams(freezeDeep(snapshot({ tool: "dsh", models: [model("route-a", "keep", 0, 2048)] })));
  assert.deepEqual(disk, { "route-a::keep": { maxOutput: 2048 } });
  assert.notEqual(disk, first);
  assert.equal(Object.hasOwn(disk, "route-a::removed"), false);
  assert.equal(Object.hasOwn(disk["route-a::keep"], "contextWindow"), false);
  assert.equal(Object.hasOwn(disk["route-a::keep"], "efforts"), false);
  assert.equal(first["route-a::removed"].contextWindow, 64_000);
  const inherited = resolveModelParams({}, {}, null, disk["route-a::keep"]);
  assert.equal(inherited.contextWindow, null);
  assert.deepEqual(inherited.efforts, []);
  const customized = resolveModelParams(children["route-a::keep"], {}, null, disk["route-a::keep"]);
  assert.equal(customized.contextWindow, 50_000);
  assert.equal(customized.contextWindowSource, "override");
  assert.deepEqual(customized.efforts, ALL_EFFORTS);
  assert.equal(JSON.stringify(children), childrenBefore);
  assert.deepEqual(hydrateModelParams(snapshot()), {});
  assert.deepEqual(hydrateModelParams(null), {});
});

function checkSaveReadReload(tool, reload = (snap) => hydrateModelParams(snap)) {
  const provider = "route-a";
  const bareModel = "vendor/model-x";
  const id = tool === "opencode" ? `${provider}/${bareModel}` : bareModel;
  const key = `${provider}::${id}`;
  const custom = { contextWindow: 200_000, efforts: [...ALL_EFFORTS] };
  const preferences = { overrides: {
    [key]: patchModelParams({}, custom),
    untouched: { contextWindow: 777, defaultReasoningEffort: "low" },
  } };
  const initial = resolveModelParams(preferences.overrides[key], {}, caps(), {}, snapshotParams(snapshot()));
  // Simulate the config save payload and preference persistence separately.
  const configJson = JSON.stringify(snapshot({
    tool, models: [model(provider, id, initial.contextWindow, initial.maxOutput)],
    defaults: { perModelEffort: { [`${provider}/${bareModel}`]: initial.efforts.join(",") } },
  }));
  const preferencesJson = JSON.stringify(preferences);
  const readSnapshot = JSON.parse(configJson);
  const reloadedPreferences = JSON.parse(preferencesJson);
  const disk = reload(readSnapshot, reloadedPreferences);
  // This must fail if hydration deletes "redundant" custom values equal to disk/catalog.
  assert.deepEqual(reloadedPreferences, preferences, "reload must not erase any child preference");
  assert.deepEqual(disk[key], { contextWindow: 200_000, maxOutput: 64_000, efforts: ALL_EFFORTS });
  const child = reloadedPreferences.overrides[key];
  assert.deepEqual(child, custom);
  assert.equal(Object.hasOwn(child, "maxOutput"), false, "an untouched field must not become custom on save");
  const restored = resolveModelParams(child, {}, caps(), disk[key], snapshotParams(readSnapshot));
  assert.equal(restored.contextWindow, 200_000);
  assert.deepEqual(restored.efforts, ALL_EFFORTS);
  assert.equal(restored.contextWindowSource, "override");
  assert.equal(restored.effortsSource, "override");
  assert.equal(restored.maxOutputSource, "catalog");
  assert.equal(restored.overriddenCount, 2);
  const refreshedCaps = caps({
    contextLength: 800_000, maxOutputTokens: 128_000,
    reasoningOptions: [{ kind: "effort", values: ["minimal", "high"] }],
  });
  const refreshed = resolveModelParams(child, {}, refreshedCaps, disk[key]);
  assert.equal(refreshed.contextWindow, 200_000);
  assert.deepEqual(refreshed.efforts, ALL_EFFORTS);
  assert.equal(refreshed.maxOutput, 128_000);
  const reset = patchModelParams(child, { contextWindow: undefined, efforts: undefined });
  const inherited = resolveModelParams(reset, {}, refreshedCaps, disk[key]);
  assert.equal(inherited.contextWindow, 800_000);
  assert.deepEqual(inherited.efforts, ["minimal", "high"]);
  assert.equal(inherited.isOverridden, false);
}

for (const tool of ["opencode", "dsh"]) {
  test(`${tool} save/read/reload retains custom full vocabulary and equal-to-catalog/disk context`, () => {
    checkSaveReadReload(tool);
  });
}

// Negative controls invoke the SAME assertions as the positive cases. They must
// throw AssertionError (not a loader/fixture TypeError), proving the guards can fail.
const assertionFailure = { name: "AssertionError", code: "ERR_ASSERTION" };

test("negative control: field-wise assertions reject whole-child-group precedence", () => {
  assert.throws(() => checkFieldWiseResolution((child) => resolveModelParams(child)), assertionFailure);
});

test("negative control: sparse-patch assertions reject eagerly capturing inherited catalog fields", () => {
  assert.throws(() => checkSparsePatch((current, patch) => (
    patchModelParams({ ...catalogParams(caps()), ...current }, patch)
  )), assertionFailure);
});

test("negative control: reload assertions reject deleting explicit values merely equal to disk", () => {
  assert.throws(() => checkSaveReadReload("opencode", (snap, preferences) => {
    const disk = hydrateModelParams(snap);
    for (const [key, child] of Object.entries(preferences.overrides)) {
      for (const field of FIELDS) {
        if (Object.hasOwn(child, field) && JSON.stringify(child[field]) === JSON.stringify(disk[key]?.[field])) {
          delete child[field]; // Deliberately wrong equality-based hydration cleanup.
        }
      }
    }
    return disk;
  }), assertionFailure);
});
