#!/usr/bin/env node
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import ts from "typescript";

async function loadModule(path) {
  const source = await readFile(new URL(path, import.meta.url), "utf8");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
  });
  return import(`data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`);
}
const { createSiteKeySync } = await loadModule("../src/composables/site/siteModelSync.ts");
const { createScopedLoader } = await loadModule("../src/composables/site/siteRequestState.ts");
const site = { id: "site", apiBaseUrl: "https://example.com/" };
const account = (profileId) => ({ profileId, profileName: profileId, accountName: "", username: "" });
const reply = (overrides = {}) => ({ models: [{ id: "model" }], keys: ["new-key"], source: "newapi-key", errors: [], ...overrides });
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
};

function commands(fetch) {
  const saved = [];
  const calls = [];
  const command = async (name, args) => {
    calls.push([name, args]);
    if (name === "fetch_site_models_json") return fetch(args);
    if (name === "save_site_model_cache_for_account") { saved.push(args); return; }
    assert.fail(`unexpected command: ${name}`);
  };
  return { command, saved, calls };
}

test("successful account sync replaces only that account, without a site-wide delete", async () => {
  const api = commands(async () => reply());
  const result = await createSiteKeySync(api.command)(site, account("A"));
  assert.equal(result.status, "success");
  assert.equal(result.keyCount, 1);
  assert.deepEqual(api.saved.map((row) => row.account.profileId), ["A"]);
  assert.equal(api.saved[0].preserveKeys, false);
  assert.deepEqual(api.saved[0].result.keys, ["new-key"]);
});

test("network failure records the account error with preservation enabled", async () => {
  const api = commands(async () => { throw new Error("HTTP 503"); });
  const result = await createSiteKeySync(api.command)(site, account("A"));
  assert.equal(result.status, "error");
  assert.match(api.saved[0].account.error, /503/);
  assert.equal(api.saved[0].preserveKeys, true);
  assert.equal(api.saved[0].result, null);
});

test("empty discovery cannot clear Keys, but independently returned health is saved", async () => {
  const health = { model: { successRate: 1 } };
  const api = commands(async () => reply({ keys: [], models: [], modelHealth: health, errors: ["Key endpoint failed"] }));
  const result = await createSiteKeySync(api.command)(site, account("A"));
  assert.equal(result.status, "error");
  assert.equal(api.saved[0].preserveKeys, true);
  assert.deepEqual(api.saved[0].result.modelHealth, health);
});

test("partial data is saved and reported as partial rather than full success", async () => {
  const api = commands(async () => reply({ errors: ["second key failed"] }));
  const result = await createSiteKeySync(api.command)(site, account("A"));
  assert.equal(result.status, "partial");
  assert.equal(result.keyCount, 1);
  assert.match(api.saved[0].account.error, /second key failed/);
});

test("owner discovery requires a confirmed profile and never creates an anonymous row", async () => {
  const noOwner = commands(async () => reply());
  const failed = await createSiteKeySync(noOwner.command)(site);
  assert.equal(failed.status, "error");
  assert.equal(noOwner.saved.length, 0);
  const owned = commands(async () => reply({ profileId: "B" }));
  const ok = await createSiteKeySync(owned.command)(site);
  assert.equal(ok.status, "success");
  assert.equal(owned.saved[0].account.profileId, "B");
});

test("wrong-account data is discarded and cannot overwrite either account's Keys", async () => {
  const api = commands(async () => reply({ profileId: "B" }));
  const result = await createSiteKeySync(api.command)(site, account("A"));
  assert.equal(result.status, "error");
  assert.match(result.error, /账号不一致/);
  assert.equal(api.saved[0].account.profileId, "A");
  assert.equal(api.saved[0].result, null);
  assert.equal(api.saved[0].preserveKeys, true);
});

test("overlapping UI entry points coalesce one account fetch and release it for retry", async () => {
  const gate = deferred();
  const api = commands(() => gate.promise);
  const sync = createSiteKeySync(api.command);
  const first = sync(site, account("A"));
  const duplicate = sync(site, account("A"));
  assert.equal(first, duplicate);
  gate.resolve(reply());
  await Promise.all([first, duplicate]);
  assert.equal(api.calls.filter(([name]) => name === "fetch_site_models_json").length, 1);
  assert.equal(api.saved.length, 1);
  await sync(site, account("A"));
  assert.equal(api.saved.length, 2);
});

test("one account failing does not block another account's success", async () => {
  const api = commands(async ({ profileId }) => {
    if (profileId === "A") throw new Error("A unavailable");
    return reply({ profileId });
  });
  const sync = createSiteKeySync(api.command);
  const results = await Promise.all([sync(site, account("A")), sync(site, account("B"))]);
  assert.deepEqual(results.map((result) => result.status), ["error", "success"]);
  assert.equal(api.saved.find((row) => row.account.profileId === "A").preserveKeys, true);
  assert.equal(api.saved.find((row) => row.account.profileId === "B").preserveKeys, false);
});

test("a cache save failure stays actionable and does not strand account ownership", async () => {
  let fail = true;
  const sync = createSiteKeySync(async (name) => {
    if (name === "fetch_site_models_json") return reply();
    if (fail) { fail = false; throw new Error("disk full"); }
  });
  await assert.rejects(sync(site, account("A")), /disk full/);
  assert.equal((await sync(site, account("A"))).status, "success");
});

test("switching sites before a read returns prevents cross-site data updates", async () => {
  const reader = createScopedLoader();
  const pending = deferred();
  let selected = "A";
  const commits = [];
  const old = reader.load(() => pending.promise, () => selected === "A", (value) => commits.push(value));
  selected = "B";
  await reader.load(() => Promise.resolve("B cache"), () => selected === "B", (value) => commits.push(value));
  pending.resolve("A cache");
  assert.equal(await old, false);
  assert.deepEqual(commits, ["B cache"]);
});

test("closing then reopening the same site invalidates the previous request", async () => {
  const reader = createScopedLoader();
  const pending = deferred();
  const commits = [];
  const read = reader.load(() => pending.promise, () => true, (value) => commits.push(value));
  reader.invalidate();
  pending.resolve("closed view");
  assert.equal(await read, false);
  assert.deepEqual(commits, []);
});

test("old reloads cannot clear loading or report stale errors for a newer reload", async () => {
  const reader = createScopedLoader();
  const old = deferred(), recent = deferred();
  const pending = [], commits = [];
  const first = reader.load(() => old.promise, () => true, (v) => commits.push(v), (v) => pending.push(v));
  const second = reader.load(() => recent.promise, () => true, (v) => commits.push(v), (v) => pending.push(v));
  old.reject(new Error("stale failure"));
  assert.equal(await first, false);
  assert.deepEqual(pending, [true, true]);
  recent.resolve("latest");
  assert.equal(await second, true);
  assert.deepEqual(commits, ["latest"]);
  assert.deepEqual(pending, [true, true, false]);
  await assert.rejects(reader.load(() => Promise.reject(new Error("current failure")), () => true, () => {}), /current failure/);
});
