<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, reactive, ref, watch } from "vue";
import { icons } from "../../icons";
import { useLocalTools } from "../../composables/localtools/useLocalTools";
import {
  useModelProxy,
  refreshProxyConfig,
  channelAlias,
  type ChannelConfig,
} from "../../composables/proxy/useModelProxy";
import { runCommand } from "../../composables/core/ipc";
import { useConfirm } from "../../composables/useConfirm";
import { useToast } from "../../composables/core/useToast";
import CustomSelect from "../common/CustomSelect.vue";
import { DEFAULT_SERVICE_PORT } from "../../constants";
import type {
  LocalToolModelEntry,
  LocalToolProviderEntry,
  LocalToolProviderMode,
  SiteModelCache,
  SiteModelCacheAccount,
  SiteModelCacheEntry,
} from "../../types";

const {
  toolList,
  toolListLoading,
  visibleTools,
  activeTool,
  snapshot,
  snapshotLoading,
  saving,
  backups,
  backupsLoading,
  loadToolList,
  selectTool,
  reloadSnapshot,
  saveSnapshot,
  loadBackups,
  restoreBackup,
} = useLocalTools();
const { proxyStatus, proxyConfig, loadCachedModels, modelsForChannel } = useModelProxy();
const { confirm } = useConfirm();
const { showToast } = useToast();

const siteCaches = ref<Record<string, SiteModelCache>>({});
const siteCachesLoading = ref(false);

onMounted(() => {
  void loadToolList();
  void refreshProxyInventory();
});

async function refreshProxyInventory() {
  await refreshProxyConfig();
  try {
    await loadCachedModels();
  } catch {
    /* 模型缓存缺失时仍展示渠道清单 */
  }
  siteCachesLoading.value = true;
  try {
    const entries = await runCommand<SiteModelCacheEntry[]>("get_all_site_model_caches");
    const next: Record<string, SiteModelCache> = {};
    for (const entry of entries ?? []) {
      if (entry?.siteId) next[entry.siteId] = entry.cache;
    }
    siteCaches.value = next;
  } catch {
    siteCaches.value = {};
  } finally {
    siteCachesLoading.value = false;
  }
}

const toolOptions = computed(() =>
  visibleTools.value.map((tool) => ({
    value: tool.tool,
    text: `${tool.toolName}${tool.detected ? "" : " · 未检测到"}`,
  })),
);

watch(
  visibleTools,
  (tools) => {
    if (!activeTool.value && tools.length) {
      void selectTool((tools.find((tool) => tool.detected) ?? tools[0]).tool);
    }
  },
  { immediate: true },
);

async function chooseTool(tool: string) {
  if (tool === activeTool.value || saving.value || snapshotLoading.value) return;
  await selectTool(tool);
}

async function reload() {
  if (saving.value || snapshotLoading.value) return;
  await Promise.all([reloadSnapshot(), refreshProxyInventory()]);
}

function parseTokenInput(event: Event): number {
  const raw = (event.target as HTMLInputElement).value;
  const value = Number(raw);
  return Number.isFinite(value) ? value : 0;
}

const activeOverview = computed(
  () => toolList.value?.tools.find((tool) => tool.tool === activeTool.value) ?? null,
);

const PROVIDER_MODE_BY_TOOL: Record<string, LocalToolProviderMode> = {
  claude: "single",
  antigravity: "single",
  codex: "switch",
  opencode: "all",
  zcode: "all",
  dsh: "all",
};

const providerMode = computed<LocalToolProviderMode>(() =>
  snapshot.value?.providerMode
  ?? activeOverview.value?.providerMode
  ?? PROVIDER_MODE_BY_TOOL[activeTool.value]
  ?? "all",
);
const switchEndpoint = computed(() => providerMode.value === "switch");
const allEndpoint = computed(() => providerMode.value === "all");

const providerModeLabel = computed(() => {
  if (providerMode.value === "single") return "一路接入";
  if (providerMode.value === "switch") return "一次一家";
  return "一次全部";
});
const providerModeHint = computed(() => {
  if (providerMode.value === "single") {
    return "该工具只有一路接入。点某一行「生效」，会把这一路指到网关，并用该渠道的别名定向。";
  }
  if (providerMode.value === "switch") {
    return "可同时保留多家，运行时一次只用当前选中的那家。点某一行「生效」会写入该条并设为当前供应商。";
  }
  return "可一次加载全部反代条目。点顶部「生效」会把当前清单里能用的条目合并进工具配置。";
});

interface ProxyInventoryRow {
  id: string;
  channelId: string;
  channelName: string;
  account: string;
  accountLabel: string;
  keyIndex: number;
  key: string;
  protocol: string;
  alias: string;
  siteId: string;
  models: string[];
}

function sanitizeIdPart(raw: string) {
  let out = "";
  for (const ch of raw) {
    if (/[a-zA-Z0-9]/.test(ch)) out += ch.toLowerCase();
    else if (!out.endsWith("_")) out += "_";
  }
  const trimmed = out.replace(/^_+|_+$/g, "") || "item";
  return trimmed.slice(0, 48);
}

function managedProviderId(channelId: string, account: string, keyIndex: number) {
  return `openhub-${sanitizeIdPart(channelId)}_${sanitizeIdPart(account)}_${keyIndex}`;
}

function maskKey(key: string) {
  const value = key.trim();
  if (!value) return "未同步 Key";
  if (value.length <= 8) return "••••••••";
  const prefix = value.startsWith("sk-") ? 7 : 4;
  const suffix = Math.min(4, Math.max(2, Math.floor(value.length / 8)));
  return `${value.slice(0, prefix)}••••••••${value.slice(-suffix)}`;
}

function protocolLabel(protocol: string) {
  const value = protocol.trim().toLowerCase();
  if (value === "anthropic") return "Anthropic";
  if (value === "openai-responses" || value === "responses") return "Responses API";
  if (value === "gemini") return "Gemini";
  if (value === "opencode") return "OpenCode";
  if (value === "openai-completions") return "Completions";
  if (value.includes("openai")) return "OpenAI";
  return protocol.trim() || "默认协议";
}

function channelKeys(channel: ChannelConfig) {
  const list = channel.apiKeys?.length
    ? channel.apiKeys
    : (channel.apiKey ? [channel.apiKey] : []);
  return list.map((key) => key.trim()).filter(Boolean);
}

function accountLabel(account: SiteModelCacheAccount) {
  return account.accountName || account.profileName || account.username || "账号";
}

function modelsForInventory(channel: ChannelConfig, account: SiteModelCacheAccount | null, key: string) {
  const alias = channelAlias(channel);
  const fromKey = key && account?.keyModels?.[key]?.map((item) => item.id).filter(Boolean);
  const fromAccount = account?.keyModels
    ? Object.values(account.keyModels).flat().map((item) => item.id).filter(Boolean)
    : [];
  const fromCache = channel.siteId
    ? (siteCaches.value[channel.siteId]?.models ?? []).map((item) => item.id).filter(Boolean)
    : [];
  const fromChannel = [
    ...(channel.enabledModels ?? []),
    ...modelsForChannel(channel.id),
  ].filter(Boolean);
  const unique = new Set<string>();
  for (const id of [...(fromKey ?? []), ...fromAccount, ...fromCache, ...fromChannel]) {
    const bare = id.includes("/") ? id.slice(id.indexOf("/") + 1) : id;
    if (bare) unique.add(bare);
  }
  return [...unique].map((model) => (alias ? `${alias}/${model}` : model));
}

const inventoryRows = computed<ProxyInventoryRow[]>(() => {
  const rows: ProxyInventoryRow[] = [];
  for (const channel of proxyConfig.value?.channels ?? []) {
    const alias = channelAlias(channel);
    const protocol = channel.protocol || "";
    if (channel.siteId) {
      const cache = siteCaches.value[channel.siteId];
      const accounts = cache?.accounts?.length ? cache.accounts : [null];
      for (const account of accounts) {
        const keys = (account?.keys ?? []).map((key) => key.trim()).filter(Boolean);
        const label = account ? accountLabel(account) : "未同步账号";
        const accountId = account?.profileId || account?.accountName || account?.username || "default";
        if (keys.length) {
          keys.forEach((key, keyIndex) => {
            rows.push({
              id: managedProviderId(channel.id, accountId, keyIndex),
              channelId: channel.id,
              channelName: channel.name || alias || channel.id,
              account: accountId,
              accountLabel: label,
              keyIndex,
              key,
              protocol,
              alias,
              siteId: channel.siteId || "",
              models: modelsForInventory(channel, account, key),
            });
          });
        } else {
          rows.push({
            id: managedProviderId(channel.id, accountId, 0),
            channelId: channel.id,
            channelName: channel.name || alias || channel.id,
            account: accountId,
            accountLabel: label,
            keyIndex: 0,
            key: "",
            protocol,
            alias,
            siteId: channel.siteId || "",
            models: modelsForInventory(channel, account, ""),
          });
        }
      }
    } else {
      const keys = channelKeys(channel);
      if (keys.length) {
        keys.forEach((key, keyIndex) => {
          rows.push({
            id: managedProviderId(channel.id, "default", keyIndex),
            channelId: channel.id,
            channelName: channel.name || alias || channel.id,
            account: "default",
            accountLabel: "手动渠道",
            keyIndex,
            key,
            protocol,
            alias,
            siteId: "",
            models: modelsForInventory(channel, null, key),
          });
        });
      } else {
        rows.push({
          id: managedProviderId(channel.id, "default", 0),
          channelId: channel.id,
          channelName: channel.name || alias || channel.id,
          account: "default",
          accountLabel: "手动渠道",
          keyIndex: 0,
          key: "",
          protocol,
          alias,
          siteId: "",
          models: modelsForInventory(channel, null, ""),
        });
      }
    }
  }
  return rows;
});

const usableRows = computed(() => inventoryRows.value.filter((row) => !!row.key));

const gatewayPort = computed(() =>
  proxyStatus.value?.port || proxyConfig.value?.port || DEFAULT_SERVICE_PORT,
);
const gatewayKey = computed(() => proxyConfig.value?.apiKey?.trim() || "");
const gatewayOrigin = computed(() => `http://127.0.0.1:${gatewayPort.value}`);
const gatewayBaseUrl = computed(() =>
  activeTool.value === "claude" ? gatewayOrigin.value : `${gatewayOrigin.value}/v1`,
);
const gatewayReady = computed(() => !!proxyStatus.value?.running);

function toolProtocol(row: ProxyInventoryRow) {
  if (activeTool.value === "claude") return "anthropic";
  if (activeTool.value === "codex") return "responses";
  if (activeTool.value === "opencode") return "@ai-sdk/openai-compatible";
  if (activeTool.value === "dsh") {
    return row.protocol === "anthropic" ? "anthropic" : "openai-completions";
  }
  if (activeTool.value === "zcode") {
    return row.protocol === "anthropic" ? "anthropic" : "openai";
  }
  return row.protocol || "openai";
}

function modelsForPatch(row: ProxyInventoryRow): LocalToolModelEntry[] {
  return row.models.map((model) => {
    const bare = model.includes("/") ? model.slice(model.indexOf("/") + 1) : model;
    const id = activeTool.value === "opencode" ? `${row.id}/${model}` : model;
    return {
      id,
      name: bare || model,
      provider: row.id,
      contextWindow: 0,
      maxOutput: 0,
    };
  });
}

function providerFromRow(row: ProxyInventoryRow): LocalToolProviderEntry {
  return {
    id: row.id,
    name: row.accountLabel && row.accountLabel !== "手动渠道"
      ? `${row.channelName} · ${row.accountLabel}`
      : row.channelName,
    baseUrl: gatewayBaseUrl.value,
    apiKey: gatewayKey.value,
    protocol: toolProtocol(row),
    models: modelsForPatch(row).map((model) => model.id),
  };
}

function rowInitial(row: ProxyInventoryRow) {
  const name = (row.channelName || "?").trim();
  return Array.from(name)[0] || "?";
}

function rowSubtitle(row: ProxyInventoryRow) {
  const parts = [
    row.accountLabel,
    maskKey(row.key),
    protocolLabel(row.protocol),
  ];
  if (row.alias) parts.push(row.alias);
  if (row.models.length) parts.push(`${row.models.length} 个模型`);
  return parts.join(" · ");
}

const appliedManagedIds = computed(() => {
  const ids = new Set<string>();
  for (const provider of snapshot.value?.providers ?? []) {
    if (provider.id.startsWith("openhub-")) ids.add(provider.id);
  }
  return ids;
});

function isRowApplied(row: ProxyInventoryRow) {
  if (!snapshot.value) return false;
  if (activeTool.value === "claude") {
    const current = snapshot.value.providers[0];
    if (!current) return false;
    if (current.id.startsWith("openhub-")) return current.id === row.id;
    const url = (current.baseUrl || "").replace(/\/+$/, "");
    const expected = gatewayBaseUrl.value.replace(/\/+$/, "");
    return url === expected || url === `${expected}/v1`;
  }
  if (switchEndpoint.value) {
    return snapshot.value.defaults.provider === row.id;
  }
  return appliedManagedIds.value.has(row.id);
}

const allApplied = computed(() =>
  usableRows.value.length > 0 && usableRows.value.every((row) => appliedManagedIds.value.has(row.id)),
);

function applyBlockedReason(rows: ProxyInventoryRow[]) {
  if (!snapshot.value) return "尚未读取到工具配置";
  if (!gatewayKey.value) return "网关 API Key 尚未生成，请先打开模型反代";
  if (!rows.length) return "当前没有可生效的反代条目";
  if (rows.some((row) => !row.key)) return "有条目还没有 Key，请先到站点库同步";
  return "";
}

function patchDefaults(selected?: ProxyInventoryRow) {
  const defaults = snapshot.value
    ? JSON.parse(JSON.stringify(snapshot.value.defaults))
    : { model: "", provider: "", reasoningEffort: "", reasoningEffortOptions: [], perModelEffort: {} };
  if (switchEndpoint.value && selected) {
    defaults.provider = selected.id;
    if (selected.models[0]) defaults.model = selected.models[0];
  }
  if (activeTool.value === "claude" && selected) {
    const selectedModels = selected.models;
    if (selectedModels[0]) defaults.model = selectedModels[0];
    const tiers = ["opus", "sonnet", "haiku"];
    tiers.forEach((tier, index) => {
      const current = defaults.perModelEffort?.[tier] || "";
      const bareCurrent = current.includes("/") ? current.slice(current.indexOf("/") + 1) : current;
      const matched = selectedModels.find(
        (model) => model === current || (bareCurrent && model.endsWith(`/${bareCurrent}`)),
      );
      const next = matched || selectedModels[index] || selectedModels[0];
      if (next) defaults.perModelEffort[tier] = next;
    });
  }
  return defaults;
}

async function applyRows(rows: ProxyInventoryRow[]) {
  if (!snapshot.value || saving.value || snapshotLoading.value) return;
  const blocked = applyBlockedReason(rows);
  if (blocked) {
    showToast(blocked, true);
    return;
  }
  if (!gatewayReady.value) {
    showToast("网关未在运行，仍会写入当前端口地址，启动后再用");
  }
  let providers = rows.map(providerFromRow);
  let models = rows.flatMap(modelsForPatch);
  if (switchEndpoint.value && rows.length === 1) {
    const selected = rows[0];
    const selectedProvider = providers[0];
    // Codex 可以保存多家供应商；切换时只更新当前条目，已有 OpenHub 条目继续保留。
    providers = [
      ...(snapshot.value.providers || []).filter(
        (provider) => !provider.id.startsWith("openhub-") && provider.id !== selected.id,
      ),
      ...(snapshot.value.providers || []).filter(
        (provider) => provider.id.startsWith("openhub-") && provider.id !== selected.id,
      ),
      selectedProvider,
    ];
    models = [
      ...(snapshot.value.models || []).filter((model) => model.provider !== selected.id),
      ...modelsForPatch(selected),
    ];
  }
  const defaults = patchDefaults(rows.length === 1 ? rows[0] : undefined);
  const ok = await saveSnapshot({
    baseHash: snapshot.value.contentHash,
    providers,
    models,
    defaults,
    context: snapshot.value.context,
    thinking: snapshot.value.thinking,
  });
  if (ok) await loadToolList();
}

async function applyRow(row: ProxyInventoryRow) {
  if (allEndpoint.value) return;
  if (!row.key) {
    showToast("这条还没有 Key，请先到站点库同步后再生效", true);
    return;
  }
  await applyRows([row]);
}

async function applyAll() {
  if (!allEndpoint.value) return;
  const missing = inventoryRows.value.length - usableRows.value.length;
  if (!usableRows.value.length) {
    showToast(inventoryRows.value.length ? "清单里的条目都还没有 Key，请先同步" : "还没有反代渠道", true);
    return;
  }
  if (missing > 0) {
    showToast(`将跳过 ${missing} 条没有 Key 的条目`);
  }
  await applyRows(usableRows.value);
}

const toolSettingsModalOpen = ref(false);
const settingsDraft = reactive({
  defaults: {
    model: "",
    provider: "",
    reasoningEffort: "",
    reasoningEffortOptions: [] as string[],
    perModelEffort: {} as Record<string, string>,
  },
  context: {
    contextWindow: null as number | null,
    autoCompactTokenLimit: null as number | null,
    maxOutputTokens: null as number | null,
    maxThinkingTokens: null as number | null,
  },
  thinking: {
    effortLevel: "",
    effortLevelOptions: [] as string[],
    maxThinkingTokens: null as number | null,
  },
});

function openToolSettings() {
  if (!snapshot.value || saving.value || snapshotLoading.value) return;
  settingsDraft.defaults = JSON.parse(JSON.stringify(snapshot.value.defaults));
  settingsDraft.context = JSON.parse(JSON.stringify(snapshot.value.context));
  settingsDraft.thinking = JSON.parse(JSON.stringify(snapshot.value.thinking));
  toolSettingsModalOpen.value = true;
  document.body.classList.add("modal-open");
}

function closeToolSettings() {
  toolSettingsModalOpen.value = false;
  document.body.classList.remove("modal-open");
}

async function submitToolSettings() {
  if (!snapshot.value) return;
  const tokenLimits = [
    settingsDraft.context.contextWindow,
    settingsDraft.context.autoCompactTokenLimit,
    settingsDraft.context.maxOutputTokens,
    settingsDraft.thinking.maxThinkingTokens,
  ];
  if (tokenLimits.some((value) => value != null && (!Number.isSafeInteger(value) || value < 0))) {
    showToast("Token 数量必须为非负整数", true);
    return;
  }
  const ok = await saveSnapshot({
    baseHash: snapshot.value.contentHash,
    providers: snapshot.value.providers,
    models: snapshot.value.models,
    defaults: JSON.parse(JSON.stringify(settingsDraft.defaults)),
    context: { ...settingsDraft.context },
    thinking: JSON.parse(JSON.stringify(settingsDraft.thinking)),
  });
  if (ok) closeToolSettings();
}

const backupModalOpen = ref(false);
function openBackupModal() {
  if (saving.value || snapshotLoading.value) return;
  void loadBackups();
  backupModalOpen.value = true;
  document.body.classList.add("modal-open");
}
function closeBackupModal() {
  backupModalOpen.value = false;
  document.body.classList.remove("modal-open");
}
async function restore(name: string, fileLabel: string) {
  const ok = await confirm({
    title: "还原第三方配置",
    message: `将把 ${fileLabel} 恢复到这次备份（当前文件会先再备份一份）。`,
    confirmText: "还原",
  });
  if (!ok) return;
  if (await restoreBackup(name)) closeBackupModal();
}

function formatSize(size: number): string {
  if (size >= 1_048_576) return `${(size / 1_048_576).toFixed(1)} MB`;
  if (size >= 1024) return `${(size / 1024).toFixed(1)} KB`;
  return `${size} B`;
}

const modalOpen = computed(() => toolSettingsModalOpen.value || backupModalOpen.value);
let modalTrigger: HTMLElement | null = null;
watch(modalOpen, async (open) => {
  if (open) {
    modalTrigger = document.activeElement as HTMLElement | null;
    await nextTick();
    document.querySelector<HTMLElement>(".lt-modal-backdrop .mini-modal")?.focus();
  } else {
    modalTrigger?.focus();
    modalTrigger = null;
  }
});

function handleDialogKeydown(event: KeyboardEvent, close: () => void) {
  if (event.key === "Escape") {
    event.stopPropagation();
    close();
  } else if (event.key === "Tab") {
    const controls = Array.from((event.currentTarget as HTMLElement).querySelectorAll<HTMLElement>(
      "button:not(:disabled), input:not(:disabled), select:not(:disabled), [tabindex=\"0\"]",
    )).filter((element) => element.getClientRects().length > 0);
    const first = controls[0];
    const last = controls.at(-1);
    const active = document.activeElement;
    if (event.shiftKey && (active === first || !controls.includes(active as HTMLElement))) {
      event.preventDefault();
      last?.focus();
    } else if (!event.shiftKey && (active === last || !controls.includes(active as HTMLElement))) {
      event.preventDefault();
      first?.focus();
    }
  }
}

onUnmounted(() => {
  if (modalOpen.value) document.body.classList.remove("modal-open");
});
</script>

<template>
  <div class="local-tools-page">
    <header class="lt-cockpit-bar">
      <div class="lt-cockpit-left">
        <h1>本地工具</h1>
        <CustomSelect
          class="lt-tool-select"
          :options="toolOptions"
          :model-value="activeTool"
          aria-label="选择本地工具"
          :menu-min-width="220"
          @update:model-value="chooseTool(String($event))"
        />
        <span v-if="activeOverview" class="lt-tool-status" :class="{ detected: activeOverview.detected }">
          <span class="lt-status-dot" />
          {{ activeOverview.detected ? "已检测" : "未检测到" }}
        </span>
      </div>

      <div v-if="activeTool" class="lt-cockpit-right">
        <button
          v-if="allEndpoint"
          type="button"
          class="lt-btn-primary"
          :disabled="saving || snapshotLoading || !usableRows.length"
          title="把当前清单里能用的反代条目写入工具"
          @click="applyAll"
        >
          <span v-html="icons.check"></span>
          {{ saving ? "写入中…" : allApplied ? "已全部生效" : "生效" }}
        </button>
        <button type="button" class="lt-btn-secondary" title="重新读取配置" @click="reload">
          <span v-html="icons.restore"></span>
          <span>重新读取</span>
        </button>
        <button type="button" class="lt-btn-secondary" title="还原第三方配置备份" @click="openBackupModal">
          <span v-html="icons.clock"></span>
          <span>还原</span>
        </button>
        <button type="button" class="lt-btn-secondary" title="工具级默认项与上下文" @click="openToolSettings">
          <span v-html="icons.sliders"></span>
          <span>工具设置</span>
        </button>
      </div>
    </header>

    <div class="local-tools-layout">
      <div v-if="!activeTool" class="empty-state">
        <div v-html="icons.monitor"></div>
        <h2>{{ toolListLoading ? "扫描本地工具…" : "暂无可用工具" }}</h2>
        <p v-if="!toolListLoading">未检测到支持结构化配置的本地工具。</p>
        <button v-if="!toolListLoading" class="secondary-button" type="button" @click="loadToolList">重新扫描</button>
      </div>

      <section v-else class="provider-section">
        <header class="section-head">
          <div class="section-copy">
            <h4>
              反代清单
              <span class="section-count">{{ inventoryRows.length }}</span>
              <span class="provider-mode-badge">{{ providerModeLabel }}</span>
            </h4>
            <p v-if="snapshot?.effectNote" class="section-note">{{ snapshot.effectNote }}</p>
            <p class="section-note">{{ providerModeHint }}</p>
            <p class="section-note">
              写入地址 {{ gatewayBaseUrl }} · {{ gatewayReady ? "网关运行中" : "网关未运行" }}
            </p>
          </div>
        </header>

        <p v-if="snapshot?.warning" class="snapshot-warning">{{ snapshot.warning }}</p>

        <div v-if="snapshotLoading || siteCachesLoading" class="detail-loading">读取配置中…</div>

        <template v-else-if="snapshot">
          <div v-if="inventoryRows.length" class="provider-list">
            <article
              v-for="row in inventoryRows"
              :key="row.id"
              class="provider-row"
              :class="{ current: isRowApplied(row), disabled: !row.key }"
            >
              <div class="provider-identity">
                <span class="provider-avatar">{{ rowInitial(row) }}</span>
                <div>
                  <strong>{{ row.channelName }}</strong>
                  <span class="provider-meta">{{ rowSubtitle(row) }}</span>
                </div>
              </div>
              <div v-if="!allEndpoint" class="provider-row-actions">
                <button
                  class="use-button"
                  :class="{ active: isRowApplied(row) }"
                  type="button"
                  :disabled="saving || isRowApplied(row)"
                  :title="row.key ? (isRowApplied(row) ? '当前已生效' : '写入该条并生效') : '请先同步 Key'"
                  @click="applyRow(row)"
                >
                  <span v-html="icons.check"></span>
                  <span>{{ isRowApplied(row) ? "已生效" : "生效" }}</span>
                </button>
              </div>
            </article>
          </div>
          <div v-else class="provider-empty">
            <div class="provider-empty-icon" v-html="icons.monitor"></div>
            <strong>还没有反代条目</strong>
            <span>先在模型反代里接入站点或手动渠道，这里会按站点、账号、Key 逐条列出。</span>
          </div>
        </template>
      </section>
    </div>

    <Teleport to="body">
      <div v-if="toolSettingsModalOpen" class="modal-backdrop lt-modal-backdrop" @click.self="closeToolSettings">
        <section class="mini-modal settings-modal" role="dialog" aria-modal="true" tabindex="-1" @keydown="handleDialogKeydown($event, closeToolSettings)">
          <header class="modal-header">
            <div>
              <h2>工具设置</h2>
              <p>{{ activeOverview?.toolName ?? activeTool }}</p>
            </div>
            <button class="close-button" type="button" aria-label="关闭工具设置" @click="closeToolSettings" v-html="icons.close"></button>
          </header>
          <div class="mini-modal-body">
            <section class="settings-block">
              <h4>默认模型与思考级别</h4>
              <div class="form-grid narrow">
                <label class="field">
                  <span>默认模型</span>
                  <input v-model="settingsDraft.defaults.model" placeholder="如 gpt-5 / alias/model" />
                </label>
                <label v-if="settingsDraft.thinking.effortLevelOptions.length" class="field">
                  <span>思考级别（effortLevel）</span>
                  <select v-model="settingsDraft.thinking.effortLevel">
                    <option value="">（未设置）</option>
                    <option v-for="opt in settingsDraft.thinking.effortLevelOptions" :key="opt" :value="opt">{{ opt }}</option>
                  </select>
                </label>
                <label v-if="settingsDraft.defaults.reasoningEffortOptions.length" class="field">
                  <span>思考级别（model_reasoning_effort）</span>
                  <select v-model="settingsDraft.defaults.reasoningEffort">
                    <option value="">（未设置）</option>
                    <option v-for="opt in settingsDraft.defaults.reasoningEffortOptions" :key="opt" :value="opt">{{ opt }}</option>
                  </select>
                </label>
                <label v-if="settingsDraft.context.maxThinkingTokens != null || settingsDraft.thinking.maxThinkingTokens != null" class="field">
                  <span>思考 token 预算</span>
                  <input type="number" :value="settingsDraft.thinking.maxThinkingTokens ?? undefined" @input="settingsDraft.thinking.maxThinkingTokens = parseTokenInput($event)" />
                </label>
              </div>
            </section>
            <section v-if="settingsDraft.context.contextWindow != null || settingsDraft.context.autoCompactTokenLimit != null || settingsDraft.context.maxOutputTokens != null" class="settings-block">
              <h4>上下文设置</h4>
              <div class="form-grid narrow">
                <label v-if="settingsDraft.context.contextWindow != null" class="field">
                  <span>上下文窗口</span>
                  <input type="number" :value="settingsDraft.context.contextWindow ?? undefined" @input="settingsDraft.context.contextWindow = parseTokenInput($event)" />
                </label>
                <label v-if="settingsDraft.context.autoCompactTokenLimit != null" class="field">
                  <span>自动压缩阈值</span>
                  <input type="number" :value="settingsDraft.context.autoCompactTokenLimit ?? undefined" @input="settingsDraft.context.autoCompactTokenLimit = parseTokenInput($event)" />
                </label>
                <label v-if="settingsDraft.context.maxOutputTokens != null" class="field">
                  <span>最大输出</span>
                  <input type="number" :value="settingsDraft.context.maxOutputTokens ?? undefined" @input="settingsDraft.context.maxOutputTokens = parseTokenInput($event)" />
                </label>
              </div>
            </section>
          </div>
          <footer class="modal-footer">
            <button class="secondary-button" type="button" @click="closeToolSettings">取消</button>
            <button class="save-button" type="button" :disabled="saving" @click="submitToolSettings">保存设置</button>
          </footer>
        </section>
      </div>
    </Teleport>

    <Teleport to="body">
      <div v-if="backupModalOpen" class="modal-backdrop lt-modal-backdrop" @click.self="closeBackupModal">
        <section class="mini-modal" role="dialog" aria-modal="true" tabindex="-1" @keydown="handleDialogKeydown($event, closeBackupModal)">
          <header class="modal-header">
            <div>
              <h2>还原</h2>
              <p>生效前会备份第三方原文，可按具体配置文件还原。带 OpenHub 标识的是本软件写入的内容。</p>
            </div>
            <button class="close-button" type="button" @click="closeBackupModal" v-html="icons.close"></button>
          </header>
          <div class="mini-modal-body">
            <div v-if="backupsLoading" class="detail-loading">读取备份列表…</div>
            <p v-else-if="!backups.length" class="section-empty">还没有第三方配置备份。</p>
            <div v-for="backup in backups" :key="backup.name" class="row-card slim">
              <div class="row-main">
                <strong>{{ backup.fileLabel }}</strong>
                <span class="row-sub">{{ backup.createdAt }} · {{ formatSize(backup.size) }}</span>
              </div>
              <div class="row-actions">
                <button class="secondary-button" type="button" @click="restore(backup.name, backup.fileLabel)">还原</button>
              </div>
            </div>
          </div>
        </section>
      </div>
    </Teleport>
  </div>
</template>

<style scoped>
.local-tools-page {
  flex: 1 1 auto;
  width: 100%;
  min-width: 0;
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  background: var(--page-bg);
  color: var(--text);
  overflow: hidden;
}

.lt-cockpit-bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 12px 20px;
  background: var(--surface);
  border-bottom: 1px solid var(--line);
  flex-shrink: 0;
}

.lt-cockpit-left {
  display: flex;
  align-items: center;
  gap: 12px;
  min-width: 0;
}

.lt-cockpit-left h1 {
  font-size: 18px;
  font-weight: 750;
  color: var(--text);
  margin: 0;
  line-height: 1.2;
  white-space: nowrap;
}

.lt-tool-select {
  width: min(240px, 42vw);
}

.lt-tool-status {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 3px 8px;
  border-radius: 999px;
  border: 1px solid var(--line);
  color: var(--muted);
  font-size: 11px;
  font-weight: 650;
}

.lt-tool-status.detected {
  color: var(--success);
  border-color: color-mix(in srgb, var(--success) 35%, var(--line));
  background: color-mix(in srgb, var(--success) 8%, var(--surface));
}

.lt-status-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: currentColor;
}

.lt-cockpit-right {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-shrink: 0;
}

.lt-btn-secondary,
.lt-btn-primary {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 32px;
  padding: 0 11px;
  border-radius: var(--r-md, 8px);
  font-size: 12px;
  font-weight: 550;
  cursor: pointer;
  white-space: nowrap;
  flex-shrink: 0;
}

.lt-btn-secondary {
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--text);
}

.lt-btn-secondary:hover {
  background: var(--surface-hover);
  border-color: var(--line-strong);
}

.lt-btn-secondary svg,
.lt-btn-primary svg {
  width: 13px;
  height: 13px;
}

.lt-btn-secondary svg { color: var(--muted); }

.lt-btn-primary {
  padding: 0 16px;
  border: none;
  background: var(--brand);
  color: #fff;
  font-weight: 600;
}

.lt-btn-primary:hover:not(:disabled) {
  box-shadow: 0 4px 12px var(--brand-glow, rgba(0, 0, 0, 0.15));
}

.lt-btn-primary:disabled {
  opacity: 0.55;
  cursor: not-allowed;
}

.local-tools-layout {
  display: flex;
  flex: 1;
  min-height: 0;
  padding: 16px 20px 20px;
  overflow: hidden;
}

.provider-section {
  flex: 1;
  min-width: 0;
  min-height: 0;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.empty-state {
  margin: 40px auto;
  max-width: 420px;
}

.section-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 12px;
  flex-shrink: 0;
}

.section-copy { min-width: 0; }
.section-head h4 {
  margin: 0;
  font-size: 13.5px;
  font-weight: 700;
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 6px;
}
.section-count {
  color: var(--muted);
  font-weight: 550;
}
.provider-mode-badge {
  display: inline-flex;
  align-items: center;
  height: 20px;
  padding: 0 7px;
  border-radius: 999px;
  border: 1px solid var(--line);
  background: var(--surface-soft, var(--surface));
  color: var(--muted);
  font-size: 11px;
  font-weight: 650;
}
.section-note {
  margin: 4px 0 0;
  color: var(--muted);
  font-size: 12px;
}

.snapshot-warning {
  padding: 10px 14px;
  margin: 0 0 12px;
  border: 1px solid color-mix(in srgb, #f59e0b 40%, var(--line));
  border-radius: var(--r-md);
  background: color-mix(in srgb, #f59e0b 8%, var(--surface));
  color: #b45309;
  font-size: 12.5px;
  flex-shrink: 0;
}

.detail-loading { padding: 24px; color: var(--muted); font-size: 13px; }

.provider-list {
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  overflow: auto;
  background: var(--surface);
  min-height: 0;
  flex: 1;
}

.provider-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  min-height: 64px;
  padding: 12px 16px;
  border-top: 1px solid var(--line-soft, var(--line));
}

.provider-row:first-of-type { border-top: 0; }
.provider-row:hover { background: var(--surface-hover); }
.provider-row.current {
  background: color-mix(in srgb, var(--brand) 6%, var(--surface));
}
.provider-row.disabled { opacity: 0.72; }
.provider-identity { display: flex; align-items: center; gap: 10px; min-width: 0; }
.provider-identity div { min-width: 0; display: flex; flex-direction: column; gap: 3px; }
.provider-identity strong {
  font-size: 13px;
  font-weight: 650;
  color: var(--text);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.provider-meta {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--muted);
  font-size: 12px;
}
.provider-avatar {
  width: 36px;
  height: 36px;
  display: grid;
  place-items: center;
  border-radius: 12px;
  background: var(--brand-soft);
  color: var(--brand-deep);
  font-size: 14px;
  font-weight: 700;
  flex: 0 0 auto;
}
.provider-row-actions {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 4px;
  flex-shrink: 0;
}
.use-button {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  height: 28px;
  padding: 0 10px 0 8px;
  border: 1px solid var(--line);
  border-radius: 999px;
  background: transparent;
  color: var(--muted);
  font-size: 12px;
  font-weight: 550;
  cursor: pointer;
}
.use-button:hover:not(:disabled) {
  background: var(--surface-hover);
  color: var(--text);
  border-color: var(--line-strong, var(--line));
}
.use-button.active,
.use-button:disabled.active {
  color: var(--brand-deep, var(--brand));
  border-color: color-mix(in srgb, var(--brand) 40%, var(--line));
  background: color-mix(in srgb, var(--brand) 8%, transparent);
  cursor: default;
}
.use-button :deep(svg) { width: 13px; height: 13px; }

.provider-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  padding: 64px 20px;
  border: 1px dashed var(--line-strong);
  border-radius: var(--r-md);
  color: var(--muted);
  text-align: center;
  flex: 1;
}
.provider-empty-icon {
  width: 40px;
  height: 40px;
  display: grid;
  place-items: center;
  border-radius: 10px;
  background: var(--brand-soft);
  color: var(--brand-deep);
}
.provider-empty strong { color: var(--text); }

.form-grid.narrow { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; }
.row-card {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 10px 14px;
  margin-bottom: 8px;
  border: 1px solid var(--line);
  border-radius: var(--r-md);
  background: var(--surface);
}
.row-card.slim { padding: 8px 12px; }
.row-main { display: flex; align-items: center; gap: 10px; min-width: 0; flex-wrap: wrap; }
.row-main strong { font-size: 12.5px; }
.row-sub { color: var(--muted); font-size: 11px; }
.row-actions { display: flex; gap: 6px; flex: 0 0 auto; }
.secondary-button {
  padding: 6px 12px;
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  background: var(--surface);
  color: var(--text);
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
}
.secondary-button:hover { border-color: var(--line-strong); background: var(--surface-hover); }
.secondary-button:disabled { opacity: 0.5; cursor: not-allowed; }
.section-empty { margin: 4px 0 0; color: var(--faint); font-size: 12px; }

.mini-modal {
  position: fixed;
  top: 50%;
  left: 50%;
  transform: translate(-50%, -50%);
  width: min(560px, calc(100vw - 48px));
  max-height: calc(100vh - 80px);
  display: grid;
  grid-template-rows: auto minmax(0, 1fr) auto;
  border: 1px solid var(--line-strong);
  border-radius: var(--r-lg);
  background: var(--surface);
  box-shadow: var(--shadow-md);
  outline: none;
}
.mini-modal .modal-header { min-height: 64px; padding: 16px 20px; }
.mini-modal .modal-header h2 { font-size: 16px; }
.mini-modal-body { overflow-y: auto; padding: 16px 20px; display: flex; flex-direction: column; gap: 12px; }
.mini-modal .modal-footer {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  padding: 14px 20px;
  border-top: 1px solid var(--line);
}
.save-button {
  padding: 8px 18px;
  border: 0;
  border-radius: var(--r-sm);
  background: var(--brand);
  color: #fff;
  font-size: 12.5px;
  font-weight: 650;
  cursor: pointer;
}
.save-button:disabled { opacity: 0.5; cursor: not-allowed; }
.settings-block h4 { margin: 0 0 10px; font-size: 13px; }

@media (max-width: 980px) {
  .lt-cockpit-bar { flex-wrap: wrap; }
  .lt-cockpit-right { width: 100%; justify-content: flex-start; flex-wrap: wrap; }
  .provider-row {
    align-items: flex-start;
    padding: 12px 14px;
    gap: 10px;
  }
  .provider-row-actions { flex-wrap: wrap; justify-content: flex-end; }
  .form-grid.narrow { grid-template-columns: 1fr; }
}
</style>
