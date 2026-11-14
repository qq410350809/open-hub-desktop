#!/usr/bin/env node
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import ts from "typescript";

const source = await readFile(new URL("../src/composables/proxy/modelSync.ts", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
});
const { runModelSyncQueue, withModelSyncDeadline } = await import(
  `data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`
);
const stateSource = await readFile(new URL("../src/composables/proxy/modelState.ts", import.meta.url), "utf8");
const stateModule = ts.transpileModule(stateSource, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
});
const { parseModelSyncResult, createLatestLoader } = await import(
  `data:text/javascript;base64,${Buffer.from(stateModule.outputText).toString("base64")}`
);

const deferred = () => {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
};
const flush = () => new Promise((resolve) => setImmediate(resolve));

test("a slow channel does not block other slots and every channel runs once", async () => {
  const gates = Array.from({ length: 6 }, deferred);
  const started = [];
  const finished = [];
  let active = 0;
  let peak = 0;
  const queue = runModelSyncQueue(gates, async (gate, index) => {
    started.push(index);
    peak = Math.max(peak, ++active);
    await gate.promise;
    finished.push(index);
    active--;
  }, () => false);
  assert.deepEqual(started, [0, 1, 2]);
  gates[1].resolve();
  await flush();
  assert.deepEqual(started, [0, 1, 2, 3]);
  gates[3].resolve();
  await flush();
  assert.deepEqual(started, [0, 1, 2, 3, 4]);
  gates.forEach((gate) => gate.resolve());
  await queue;
  assert.equal(peak, 3);
  assert.deepEqual(finished.sort(), [0, 1, 2, 3, 4, 5]);
});

test("stop prevents queued channels from starting while active work finishes", async () => {
  let stopped = false;
  const gate = deferred();
  const started = [];
  const finished = [];
  const queue = runModelSyncQueue([0, 1, 2, 3, 4], async (index) => {
    started.push(index);
    await gate.promise;
    finished.push(index);
  }, () => stopped);
  stopped = true;
  gate.resolve();
  await queue;
  assert.deepEqual(started, [0, 1, 2]);
  assert.deepEqual(finished, [0, 1, 2]);
});

test("lost IPC is bounded and late rejection is consumed", async () => {
  let reject;
  const late = new Promise((_, fail) => { reject = fail; });
  await assert.rejects(withModelSyncDeadline(late, 10), /同步响应超时/);
  reject(new Error("late transport failure"));
  await flush();
});

test("discovery replies normalize current and legacy channel identifiers", () => {
  const { models, errors } = parseModelSyncResult([
    [{ channelId: "a", models: [" m1 ", "m1", "", null, "m2"] }, { channel_id: "b", models: [] }],
    [{ channel_id: "bad", channel_name: "broken", error: "HTTP 401" }],
  ]);
  assert.deepEqual({ ...models }, { a: ["m1", "m2"], b: [] });
  assert.equal(errors[0].channelId, "bad");
  assert.equal(errors[0].error, "HTTP 401");
  assert.deepEqual({ ...parseModelSyncResult([{ channel_id: "old", models: ["legacy"] }]).models }, { old: ["legacy"] });
});

test("malformed discovery replies cannot look like valid channel successes", () => {
  assert.throws(() => parseModelSyncResult({}), /响应格式无效/);
  const { models } = parseModelSyncResult([null, { channelId: "missing", models: false }, { channelId: "__proto__", models: ["m1"] }]);
  assert.equal("missing" in models, false);
  assert.deepEqual(models.__proto__, ["m1"]);
  assert.equal(Object.getPrototypeOf(models), null);
});

test("slow older reads cannot overwrite a newer filter response", async () => {
  const load = createLatestLoader();
  const old = deferred();
  const recent = deferred();
  const committed = [];
  const pending = [];
  const first = load(() => old.promise, (value) => committed.push(value), (value) => pending.push(value));
  const second = load(() => recent.promise, (value) => committed.push(value), (value) => pending.push(value));
  old.resolve("old");
  await first;
  assert.deepEqual(pending, [true, true], "old request must not clear the newer loading state");
  recent.resolve("latest");
  await second;
  assert.deepEqual(committed, ["latest"]);
  assert.deepEqual(pending, [true, true, false]);
});

test("stale read errors are ignored while the latest failure remains actionable", async () => {
  const load = createLatestLoader();
  let reject;
  const first = load(() => new Promise((_, fail) => { reject = fail; }), () => assert.fail("stale commit"));
  await load(() => Promise.resolve("latest"), () => {});
  reject(new Error("old failure"));
  await first;
  await assert.rejects(load(() => Promise.reject(new Error("current failure")), () => {}), /current failure/);
});

test("deadline preserves successes and actionable backend errors", async () => {
  assert.deepEqual(await withModelSyncDeadline(Promise.resolve(["m1"]), 100), ["m1"]);
  await assert.rejects(withModelSyncDeadline(Promise.reject(new Error("HTTP 401")), 100), /HTTP 401/);
});
