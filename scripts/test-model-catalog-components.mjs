#!/usr/bin/env node
// Run the real Vue components with an in-memory host. No browser or user data is accessed.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { compileScript, parse } from "vue/compiler-sfc";
import { createRenderer, h, nextTick, ref } from "vue";
import ts from "typescript";

const vueUrl = import.meta.resolve("vue");
const dataModule = (text) => `data:text/javascript;base64,${Buffer.from(text).toString("base64")}`;
const transpile = (text) => ts.transpileModule(text, { compilerOptions: {
  target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext,
} }).outputText;
const helperUrl = dataModule(transpile(await readFile(new URL("../src/composables/model/catalogView.ts", import.meta.url), "utf8")));
const fixture = await import(dataModule(transpile(await readFile(new URL("../src/composables/core/mockData.ts", import.meta.url), "utf8"))));
const iconUrl = dataModule('export const icons = Object.fromEntries(["close","database","restore","sliders","chevron","search"].map(key => [key,"<svg></svg>"]));');
const catalogUrl = dataModule('export const useModelCatalog = () => globalThis.__catalogComponentTest.catalog;');
const toastUrl = dataModule('export const useToast = () => ({showToast: (...args) => globalThis.__catalogComponentTest.toasts.push(args)});');
const actionsUrl = dataModule('export const useSiteActions = () => ({openExternal: async () => {}});');
const modules = new Map();
for (const name of ["CatalogDialog", "CatalogComparison", "CatalogModelDetail", "ModelCatalogPage"]) {
  const location = name === "ModelCatalogPage" ? "pages" : "model";
  const file = new URL(`../src/components/${location}/${name}.vue`, import.meta.url);
  const source = await readFile(file, "utf8");
  const { descriptor } = parse(source);
  const compiled = compileScript(descriptor, { id: `test-${name}`, inlineTemplate: true });
  let text = transpile(compiled.content).replace(/from (["'])vue\1/g, `from ${JSON.stringify(vueUrl)}`)
    .replace('from "../../icons"', `from ${JSON.stringify(iconUrl)}`)
    .replace('from "../../composables/model/catalogView"', `from ${JSON.stringify(helperUrl)}`)
    .replace('from "../../composables/model/useModelCatalog"', `from ${JSON.stringify(catalogUrl)}`)
    .replace('from "../../composables/core/useToast"', `from ${JSON.stringify(toastUrl)}`)
    .replace('from "../../composables/site/useSiteActions"', `from ${JSON.stringify(actionsUrl)}`)
    .replace('import "../../styles/model-catalog.css";', "");
  for (const [component, url] of modules) {
    text = text.replace(`from "./${component}.vue"`, `from ${JSON.stringify(url)}`)
      .replace(`from "../model/${component}.vue"`, `from ${JSON.stringify(url)}`);
  }
  modules.set(name, dataModule(text));
}
const Page = (await import(modules.get("ModelCatalogPage"))).default;
const Comparison = (await import(modules.get("CatalogComparison"))).default;

class HostNode {
  constructor(tag = "", text = "") {
    this.tag = tag; this.tagName = tag.toUpperCase(); this.text = text;
    this.children = []; this.props = {}; this.listeners = new Map(); this.parentNode = null;
    this.isConnected = true; this.focusCount = 0; this.__v_skip = true;
  }
  get textContent() { return this.text + this.children.map((child) => child.textContent).join(""); }
  get options() { return this.children.filter((node) => node.tag === "option"); }
  getRootNode() { return document; }
  addEventListener(name, handler) { this.listeners.set(name, handler); }
  removeEventListener(name) { this.listeners.delete(name); }
  setAttribute(name, value) { this.props[name] = value; }
  removeAttribute(name) { delete this.props[name]; }
  showModal() { this.open = true; }
  close() { this.open = false; }
  focus() { document.activeElement = this; this.focusCount++; }
  getBoundingClientRect() { return { left: 0, top: 0, right: 800, bottom: 900 }; }
}
const all = (node) => [node, ...node.children.flatMap(all)];
const classHas = (node, name) => String(node.props.class || "").split(/\s+/).includes(name);
const settle = async () => { await nextTick(); await new Promise((resolve) => setImmediate(resolve)); await nextTick(); };
function click(node) {
  assert.ok(node, "interactive element must exist");
  node.focus();
  node.props.onClick?.({ target: node, currentTarget: node, preventDefault() {}, stopPropagation() {} });
}
function mount(component = Page, componentProps = {}) {
  const body = new HostNode("body"), root = new HostNode("main"), trigger = new HostNode("button");
  const descriptors = ["document", "HTMLElement", "Document", "ShadowRoot"].map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)]);
  class TestDocument {}
  class TestShadowRoot {}
  const documentHost = new TestDocument();
  documentHost.activeElement = trigger;
  Object.defineProperty(globalThis, "document", { configurable: true, value: documentHost });
  Object.defineProperty(globalThis, "HTMLElement", { configurable: true, value: HostNode });
  Object.defineProperty(globalThis, "Document", { configurable: true, value: TestDocument });
  Object.defineProperty(globalThis, "ShadowRoot", { configurable: true, value: TestShadowRoot });
  const pending = [];
  const snapshot = structuredClone(fixture.previewModelSnapshot);
  const state = {
    modelCatalog: ref(snapshot), modelCatalogLoading: ref(false), modelCatalogSyncing: ref(false),
    modelCatalogError: ref(""), modelCatalogSyncMessage: ref(""),
    loadModelCatalog: async () => {}, syncModelCatalog: async () => {},
    getModelCatalogDetail: (id) => new Promise((resolve, reject) => pending.push({ id, resolve, reject })),
  };
  globalThis.__catalogComponentTest = { catalog: state, toasts: [] };
  const renderer = createRenderer({
    createElement: (tag) => new HostNode(tag), createText: (text) => new HostNode("", text), createComment: () => new HostNode(),
    setText: (node, text) => { node.text = text; },
    setElementText: (node, text) => { node.text = text; node.children = []; },
    insert(node, parent, anchor = null) {
      if (node.parentNode) node.parentNode.children = node.parentNode.children.filter((child) => child !== node);
      const index = anchor ? parent.children.indexOf(anchor) : -1;
      parent.children.splice(index < 0 ? parent.children.length : index, 0, node); node.parentNode = parent;
    },
    remove(node) { if (node.parentNode) node.parentNode.children = node.parentNode.children.filter((child) => child !== node); node.isConnected = false; },
    parentNode: (node) => node.parentNode,
    nextSibling: (node) => node.parentNode?.children[node.parentNode.children.indexOf(node) + 1],
    querySelector: (selector) => selector === "body" ? body : null,
    patchProp(node, key, _previous, value) { node.props[key] = value; if (key === "value") node._value = value; node[key] = value; },
  });
  const app = renderer.createApp({ render: () => h(component, componentProps) });
  app.mount(root);
  const nodes = () => [...all(root), ...all(body)];
  return { state, pending, root, body, trigger, nodes,
    find: (name) => nodes().find((node) => classHas(node, name)),
    cleanup() {
      app.unmount();
      for (const [key, descriptor] of descriptors) {
        if (descriptor) Object.defineProperty(globalThis, key, descriptor); else delete globalThis[key];
      }
      delete globalThis.__catalogComponentTest;
    },
  };
}

test("catalog components keep collapsed filters visible and clearable", async () => {
  const ui = mount();
  try {
    await settle();
    const table = ui.find("catalog-model-table");
    assert.ok(table);
    const search = ui.find("catalog-search").children.find((node) => node.tag === "input");
    search.props["onUpdate:modelValue"]("definitely-no-match");
    await settle();
    assert.ok(ui.find("catalog-empty").textContent.includes("没有符合条件"));
    assert.ok(ui.find("catalog-filter-chips").textContent.includes("definitely-no-match"));
    click(ui.find("catalog-reset")); await settle();
    click(ui.nodes().find((node) => node.props["aria-controls"] === "catalog-advanced")); await settle();
    const ability = ui.find("catalog-capability-filters").children.find((node) => node.tag === "label").children.find((node) => node.tag === "input");
    ability.checked = true; ability.props.onChange(); await settle();
    click(ui.nodes().find((node) => node.props["aria-controls"] === "catalog-advanced")); await settle();
    assert.ok(!ui.find("catalog-advanced"));
    assert.ok(ui.find("catalog-filter-chips").textContent.includes("思考推理"));
    click(ui.find("catalog-reset")); await settle();
    assert.ok(!ui.find("catalog-filter-chips"));
  } finally { ui.cleanup(); }
});

test("detail failure is retryable; closing restores focus and ignores a late response", async () => {
  const ui = mount();
  try {
    await settle();
    const opener = ui.find("catalog-model-name");
    click(opener); await settle();
    assert.equal(ui.find("catalog-dialog").open, true);
    ui.pending[0].reject(new Error("simulated detail failure")); await settle();
    assert.ok(ui.find("catalog-notice").textContent.includes("simulated detail failure"));
    click(all(ui.find("catalog-notice")).find((node) => node.tag === "button")); await settle();
    assert.equal(ui.pending.length, 2);
    const close = ui.find("catalog-dialog-close");
    click(close); await settle();
    assert.ok(!ui.find("catalog-dialog"));
    assert.equal(document.activeElement, opener);
    ui.pending[1].resolve({ model: ui.state.modelCatalog.value.models.find((model) => model.id === ui.pending[1].id), hosts: [], providers: [], raw: {} });
    await settle();
    assert.ok(!ui.find("catalog-dialog"));
  } finally { ui.cleanup(); }
});

test("comparison selection reaches four without replacing a model; comparison and native cancel work", async () => {
  const ui = mount();
  try {
    await settle();
    const boxes = () => all(ui.find("catalog-model-table")).filter((node) => node.tag === "input");
    for (let i = 0; i < 4; i++) { boxes()[i].props.onChange(); await settle(); }
    assert.equal(boxes().filter((node) => node.props.checked).length, 4);
    assert.equal(boxes()[4].props.disabled, true);
    click(all(ui.find("catalog-compare-tray")).find((node) => classHas(node, "is-primary"))); await settle();
    assert.ok(ui.find("catalog-comparison"));
    let prevented = false;
    ui.find("catalog-dialog").props.onCancel({ preventDefault() { prevented = true; } }); await settle();
    assert.equal(prevented, true);
    assert.ok(!ui.find("catalog-dialog"));
    assert.equal(boxes().filter((node) => node.props.checked).length, 4);
  } finally { ui.cleanup(); }
});

test("catalog refresh closes an incomplete comparison without reopening it on a later selection", async () => {
  const ui = mount();
  try {
    await settle();
    const boxes = () => all(ui.find("catalog-model-table")).filter((node) => node.tag === "input");
    const names = () => all(ui.find("catalog-model-table")).filter((node) => classHas(node, "catalog-model-name"));
    const survivorName = names()[0].textContent;
    const removedName = names()[1].textContent;
    boxes()[0].props.onChange(); await settle();
    boxes()[1].props.onChange(); await settle();
    click(all(ui.find("catalog-compare-tray")).find((node) => classHas(node, "is-primary"))); await settle();
    assert.ok(ui.find("catalog-comparison"));
    ui.state.modelCatalog.value = {
      ...ui.state.modelCatalog.value,
      models: ui.state.modelCatalog.value.models.filter((model) => model.name !== removedName),
    };
    await settle();
    assert.ok(!ui.find("catalog-comparison"));
    assert.ok(ui.find("catalog-compare-tray").textContent.includes(survivorName));
    const next = boxes().find((node) => !node.props.checked);
    next.props.onChange(); await settle();
    assert.equal(boxes().filter((node) => node.props.checked).length, 2);
    assert.ok(!ui.find("catalog-comparison"), "new selections must require explicitly reopening comparison");
  } finally { ui.cleanup(); }
});

test("comparison differences toggle preserves small exact token differences", async () => {
  const base = fixture.previewModels[0];
  const ui = mount(Comparison, { models: [
    { ...base, id: "a", contextLength: 128000 }, { ...base, id: "b", contextLength: 128001 },
  ] });
  try {
    await settle();
    const checkbox = ui.nodes().find((node) => node.tag === "input");
    checkbox.props["onUpdate:modelValue"](true); await settle();
    const rows = all(ui.find("catalog-comparison")).filter((node) => node.tag === "tr");
    assert.equal(rows.length, 2);
    assert.ok(rows[1].textContent.includes("128,000 tokens"));
    assert.ok(rows[1].textContent.includes("128,001 tokens"));
  } finally { ui.cleanup(); }
});
