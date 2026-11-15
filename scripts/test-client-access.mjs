#!/usr/bin/env node
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { spawnSync } from "node:child_process";
import ts from "typescript";

const source = await readFile(new URL("../src/composables/proxy/clientAccess.ts", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
});
const { gatewayClientEndpoints, clientBaseUrl, parseClientModels, fetchClientModels,
  clientRequestExample, clientConnectionJson } = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`);

test("preference changes preserve retired Agent settings without activating them", async () => {
  const saved = {
    theme: "dark", sidebarCollapsed: false,
    agentIdentityModes: { codex: "model" },
    agentModelConfigs: { codex: { childOverrides: { "site/model": { maxOutput: 8192 } } } },
    accountAliases: { "site:A": "existing alias" },
  };
  const storage = new Map([["ldoh:preferences", JSON.stringify(saved)]]);
  const previousStorage = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
  Object.defineProperty(globalThis, "localStorage", { configurable: true, value: {
    getItem: (key) => storage.get(key) ?? null,
    setItem: (key, value) => storage.set(key, value),
  } });
  try {
    const preferencesSource = await readFile(new URL("../src/composables/core/usePreferences.ts", import.meta.url), "utf8");
    const compiled = ts.transpileModule(preferencesSource, {
      compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
    }).outputText.replace('from "vue"', `from ${JSON.stringify(import.meta.resolve("vue"))}`);
    const { usePreferences } = await import(`data:text/javascript;base64,${Buffer.from(compiled).toString("base64")}`);
    const { nextTick } = await import("vue");
    const { preferences, updatePreferences } = usePreferences();
    assert.equal(preferences.theme, "dark");
    updatePreferences({ sidebarCollapsed: true });
    await nextTick();
    const persisted = JSON.parse(storage.get("ldoh:preferences"));
    assert.equal(persisted.sidebarCollapsed, true);
    for (const field of ["agentIdentityModes", "agentModelConfigs", "accountAliases"]) {
      assert.deepEqual(persisted[field], saved[field]);
    }
  } finally {
    if (previousStorage) Object.defineProperty(globalThis, "localStorage", previousStorage);
    else delete globalThis.localStorage;
  }
});

const origin = "https://hub.example:9443";

test("client Base URLs distinguish SDK roots from full request endpoints", () => {
  assert.equal(clientBaseUrl(`${origin}/`, "chat"), `${origin}/v1`);
  assert.equal(clientBaseUrl(origin, "responses"), `${origin}/v1`);
  assert.equal(clientBaseUrl(origin, "anthropic"), origin);
  assert.equal(clientBaseUrl(origin, "gemini"), `${origin}/v1beta`);
  assert.equal(gatewayClientEndpoints(`${origin}/`).anthropic, `${origin}/v1/messages`);
  assert.equal(gatewayClientEndpoints("http://127.0.0.1:17997").models, "http://127.0.0.1:17997/v1/models");
});

test("model discovery preserves complete route IDs and deduplicates without inventing models", () => {
  assert.deepEqual(parseClientModels({ object: "list", data: [
    { id: "site-b/vendor/model" }, { id: "site-a/model" }, { id: "site-b/vendor/model" },
  ] }), ["site-a/model", "site-b/vendor/model"]);
  assert.deepEqual(parseClientModels({ data: [] }), []);
  for (const invalid of [null, {}, { Ok: { data: [] } }, { data: [{ id: "" }] }, { data: ["model"] }, { data: [{ id: 7 }] }]) {
    assert.throws(() => parseClientModels(invalid));
  }
});

test("model discovery authenticates only to the selected gateway and carries cancellation", async () => {
  const controller = new AbortController();
  let called = 0;
  const models = await fetchClientModels(origin, "test-gateway-key", controller.signal, async (url, options) => {
    called++;
    assert.equal(url, `${origin}/v1/models`);
    assert.deepEqual(options.headers, { Authorization: "Bearer test-gateway-key" });
    assert.equal(options.credentials, "omit");
    assert.equal(options.cache, "no-store");
    assert.equal(options.signal, controller.signal);
    return new Response(JSON.stringify({ data: [{ id: "site/model" }] }));
  });
  assert.deepEqual(models, ["site/model"]);
  assert.equal(called, 1);
});

test("discovery failures remain errors rather than empty successes or reflected response bodies", async () => {
  for (const [status, message] of [[401, /Key 校验失败/], [503, /尚未启用/], [500, /HTTP 500/]]) {
    await assert.rejects(fetchClientModels(origin, "key", new AbortController().signal,
      async () => new Response("private upstream response", { status })), (error) => {
      assert.match(error.message, message);
      assert.ok(!error.message.includes("private"));
      return true;
    });
  }
  await assert.rejects(fetchClientModels(origin, "key", new AbortController().signal,
    async () => new Response("<html>preview</html>")), /没有返回 JSON/);
});

function captureCurl(protocol, model) {
  const command = clientRequestExample(origin, protocol, model);
  const script = `curl() { printf '%s\\0' "$@"; }\n${command}`;
  const result = spawnSync("/bin/bash", ["-c", script], {
    encoding: "utf8", env: { ...process.env, OPENHUB_API_KEY: "test-key-$literal" },
  });
  assert.equal(result.status, 0, result.stderr);
  const args = result.stdout.split("\0").filter(Boolean);
  return { command, url: args[0], headers: args.flatMap((arg, index) => arg === "-H" ? [args[index + 1]] : []),
    body: JSON.parse(args[args.indexOf("-d") + 1]) };
}

test("Chat and Responses examples send the selected complete model with the right request shape", () => {
  const model = "site/vendor/model";
  const chat = captureCurl("chat", model);
  assert.equal(chat.url, `${origin}/v1/chat/completions`);
  assert.equal(chat.body.model, model);
  assert.deepEqual(chat.body.messages, [{ role: "user", content: "Hello" }]);
  assert.ok(chat.headers.includes("Authorization: Bearer test-key-$literal"));
  const responses = captureCurl("responses", model);
  assert.equal(responses.url, `${origin}/v1/responses`);
  assert.deepEqual(responses.body, { model, input: "Hello" });
});

test("Anthropic and Gemini examples use native paths, headers and payloads", () => {
  const anthropic = captureCurl("anthropic", "site/claude");
  assert.equal(anthropic.url, `${origin}/v1/messages`);
  assert.ok(anthropic.headers.includes("x-api-key: test-key-$literal"));
  assert.ok(anthropic.headers.includes("anthropic-version: 2023-06-01"));
  assert.equal(anthropic.body.max_tokens, 1024);
  const gemini = captureCurl("gemini", "site/gemini beta");
  assert.equal(gemini.url, `${origin}/v1beta/models/site/gemini%20beta:generateContent`);
  assert.ok(gemini.headers.includes("x-goog-api-key: test-key-$literal"));
  assert.deepEqual(gemini.body.contents, [{ role: "user", parts: [{ text: "Hello" }] }]);
});

test("request examples quote unusual model IDs safely and never contain a real Key", () => {
  const model = "site/model'\"; $(printf compromised) `printf compromised`";
  for (const protocol of ["chat", "responses", "anthropic"]) {
    const captured = captureCurl(protocol, model);
    assert.equal(captured.body.model, model);
    assert.ok(!captured.command.includes("test-key-$literal"));
  }
  const settings = JSON.parse(clientConnectionJson(origin, "responses", model));
  assert.equal(settings.apiKey, "YOUR_OPENHUB_API_KEY");
  assert.equal(settings.model, model);
  assert.equal(settings.protocol, "openai-responses");
});
