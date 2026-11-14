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

test("deadline preserves successes and actionable backend errors", async () => {
  assert.deepEqual(await withModelSyncDeadline(Promise.resolve(["m1"]), 100), ["m1"]);
  await assert.rejects(withModelSyncDeadline(Promise.reject(new Error("HTTP 401")), 100), /HTTP 401/);
});
