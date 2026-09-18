<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, reactive, ref, watch } from "vue";
import { icons } from "../../icons";
import { useLocalTools } from "../../composables/localtools/useLocalTools";
import {
  useModelProxy,
  refreshProxyConfig,
  channelAlias,
  isOpenCodeFreeChannel,
  type ChannelConfig,
} from "../../composables/proxy/useModelProxy";
import { runCommand } from "../../composables/core/ipc";
import { useConfirm } from "../../composables/useConfirm";
import { useToast } from "../../composables/core/useToast";
import { usePreferences } from "../../composables/usePreferences";
import { useModelCatalog } from "../../composables/model/useModelCatalog";
import CustomSelect from "../common/CustomSelect.vue";
import { DEFAULT_SERVICE_PORT } from "../../constants";
import type {
  LocalToolConfigFile,
  LocalToolConfigPatch,
  LocalToolDefaultsSection,
  LocalToolDiffEntry,
  LocalToolIdentityMode,
  LocalToolModelEntry,
  LocalToolProviderEntry,
  LocalToolProviderMode,
  ModelCapabilities,
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
  diffReport,
  diffLoading,
  configRevision,
  loadToolList,
  selectTool,
  reloadSnapshot,
  saveSnapshot,
  loadBackups,
  restoreBackup,
  diffTargets,
} = useLocalTools();
const { proxyStatus, proxyConfig, loadCachedModels, modelsForChannel } = useModelProxy();
const { confirm } = useConfirm();
const { showToast } = useToast();
const { preferences, updatePreferences } = usePreferences();
const { modelCatalogSyncing } = useModelCatalog();

const siteCaches = ref<Record<string, SiteModelCache>>({});
const siteCachesLoading = ref(false);

// —— 身份模式与展开状态（必须置顶，供整个组件依赖项初始化） ——
const SINGLE_GATEWAY_PROVIDER_ID = "openhub-gateway";
const expandedKeys = ref<Set<string>>(new Set());

const identityMode = computed<LocalToolIdentityMode>(
  () => preferences.agentIdentityModes?.[activeTool.value] ?? "channel",
);
const modelIdentityMode = computed(() => identityMode.value === "model");

/** 模式切换旁的一句话说明，避免用户只看到两个标签不知差异。 */
const identityModeCaption = computed(() =>
  modelIdentityMode.value
    ? "整份清单合并为一条网关接入；下面按模型列出，展开看提供它的站点 · 账号"
    : "每个「渠道 · 账号」各成一条供应商条目；下面按账号列出，展开看模型",
);

function setIdentityMode(mode: LocalToolIdentityMode) {
  if (mode === identityMode.value) return;
  updatePreferences({
    agentIdentityModes: { ...preferences.agentIdentityModes, [activeTool.value]: mode },
  });
  // 两种模式的列表结构不同（条目 → 模型），展开态一律收起重来，避免残留上一模式的展开行。
  expandedKeys.value = new Set();
}

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
  // 换 Agent 后清单完全不同，展开态一并重置。
  expandedKeys.value = new Set();
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

/**
 * 顶部「生效」按钮是否适用于当前模式：模型×站点整单写入，或一次全部的工具合并写入。
 *
 * 按钮**始终渲染**（不适用时禁用）——它的出现/消失会改变右侧按钮组宽度，
 * 触发第一行换行，表现为切换模式时按钮上下跳动。
 */
const canApplyAll = computed(() => modelIdentityMode.value || allEndpoint.value);

const providerModeLabel = computed(() => {
  if (providerMode.value === "single") return "一路接入";
  if (providerMode.value === "switch") return "一次一家";
  return "一次全部";
});
const providerModeHint = computed(() => {
  const listHint = modelIdentityMode.value
    ? "清单每行是一个模型，点行首箭头展开看哪些站点 · 账号提供它。"
    : "清单每行是一个「渠道 · 账号」，点行首箭头展开看模型。";
  if (modelIdentityMode.value) {
    return `模型×站点模式：整份清单合并为一条网关接入，同一模型可由多个站点提供；点顶部「生效」写入整份清单。${listHint}`;
  }
  if (providerMode.value === "single") {
    return `该 Agent 只有一路接入。点某一行「生效」，会把这一路指到网关，并用该渠道的别名定向。${listHint}`;
  }
  if (providerMode.value === "switch") {
    return `可同时保留多家，运行时一次只用当前选中的那家。点某一行「生效」会写入该条并设为当前供应商。${listHint}`;
  }
  return `可一次加载全部反代条目。点顶部「生效」会把当前清单里能用的条目合并进 Agent 配置。${listHint}`;
});

interface ProxyInventoryRow {
  id: string;
  channelId: string;
  channelName: string;
  account: string;
  accountLabel: string;
  keyIndex: number;
  key: string;
  /** OpenCode 免 Key 匿名模式：无真实 Key 但网关照常出网，视同有一路可用 Key。 */
  anonymousKey: boolean;
  protocol: string;
  alias: string;
  siteId: string;
  group: string;
  models: string[];
}

/** 该行是否可参与生效：有真实 Key，或 OpenCode 匿名模式（匿名 Key 也是 Key）。 */
function rowHasKey(row: ProxyInventoryRow) {
  return !!row.key || row.anonymousKey;
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

/** 无真实 Key 时的组副标题：OpenCode 匿名模式视同有 Key，其余才是真未同步。 */
function anonymousGroupLabel(row: ProxyInventoryRow | undefined) {
  return row?.anonymousKey ? "匿名模式（免 Key）" : "未同步 Key";
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
  // 站点同步的昵称最直观，优先显示；否则退回邮箱（去域名）/ Profile 名。
  if (account.username.trim()) return account.username.trim();
  const raw = account.accountName || account.profileName || "账号";
  // 邮箱形式的账号去掉 @ 后的域名部分，标识更简洁
  const at = raw.indexOf("@");
  return at > 0 ? raw.slice(0, at) : raw;
}

/** 账号在别名表里的键:账号身份(邮箱/用户名)本身,所有站点共用一份别名。 */
function accountAliasKey(account: SiteModelCacheAccount | null) {
  const identity = account?.accountName || account?.profileId || account?.username || "default";
  return identity.toLowerCase();
}

/** 账号显示名:优先用户设置的别名(全站点共用),否则邮箱去域名的账号名。 */
function accountDisplayName(account: SiteModelCacheAccount | null) {
  if (account) {
    const alias = preferences.accountAliases[accountAliasKey(account)]?.trim();
    if (alias) return alias;
  }
  return account ? accountLabel(account) : "未同步账号";
}

/** 站点渠道 Key 的分组名：站点缓存的 keyGroups 直接存 key → 分组名。 */
function siteKeyGroup(account: SiteModelCacheAccount | null, key: string) {
  return account?.keyGroups?.[key]?.trim() || "默认分组";
}

/** 手动渠道 Key 的分组名：经 keyRules 的 key → groupId，再查 keyGroups 定义。 */
function channelKeyGroup(channel: ChannelConfig, key: string) {
  const groupId = channel.keyRules?.find((rule) => rule.key === key)?.groupId;
  const group = groupId ? channel.keyGroups?.find((item) => item.id === groupId) : undefined;
  return group?.name?.trim() || "默认分组";
}

/**
 * 剥掉模型 id 上的渠道别名前缀（`alias/model` → `model`），统一成清单使用的裸名口径。
 * 只剥渠道别名：`deepseek-ai/DeepSeek-V3` 这类上游厂商命名空间是模型 ID 的一部分，
 * 网关会拿它去比对 Key 的模型授权与出网，剥掉会导致「没有任何 Key 支持该模型」。
 */
function bareModelId(id: string, alias: string) {
  const prefix = alias ? `${alias}/` : "";
  return prefix && id.toLowerCase().startsWith(prefix.toLowerCase()) ? id.slice(prefix.length) : id;
}

function modelsForInventory(channel: ChannelConfig, account: SiteModelCacheAccount | null, key: string) {
  const alias = channelAlias(channel);
  const accountKeyModels = account?.keyModels;
  // 账号拉取过按 Key 的模型映射时，只认该 Key 自己的条目（无条目/为空 = 该 Key 无模型），
  // 不借用同账号其他 Key 的数据；账号没有 Key 级数据时才回退站点/渠道缓存
  const hasKeyLevelData = !!accountKeyModels && Object.keys(accountKeyModels).length > 0;
  const owned: string[] = hasKeyLevelData
    ? (key ? accountKeyModels?.[key] : undefined)?.map((item) => item.id) ?? []
    : [
        ...(channel.siteId
          ? (siteCaches.value[channel.siteId]?.models ?? []).map((item) => item.id)
          : []),
        ...modelsForChannel(channel.id),
      ];
  if (!owned.length && channel.enabledModels?.length) {
    owned.push(...channel.enabledModels);
  }
  const bare = new Set<string>();
  for (const id of owned) {
    const value = bareModelId(id, alias).trim();
    if (value) bare.add(value);
  }
  // 只保留反代「管理可用模型」勾选的模型；勾选记录可能带前缀，按裸名比对
  const allow = channel.enabledModels;
  let visible = [...bare];
  if (allow && allow.length > 0) {
    const allowBare = new Set(allow.map((m) => bareModelId(m, alias)).filter(Boolean));
    visible = visible.filter((model) => allowBare.has(model));
  }
  return visible.map((model) => (alias ? `${alias}/${model}` : model));
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
        const label = accountDisplayName(account);
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
              anonymousKey: false,
              protocol,
              alias,
              siteId: channel.siteId || "",
              group: siteKeyGroup(account, key),
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
            anonymousKey: false,
            protocol,
            alias,
            siteId: channel.siteId || "",
            group: "默认分组",
            models: modelsForInventory(channel, account, ""),
          });
        }
      }
    } else {
      const keys = channelKeys(channel);
      // 内置 opencode 渠道未配 Key 时走匿名模式，匿名 Key 也算一路可用 Key
      const anonymous = keys.length === 0 && isOpenCodeFreeChannel(channel);
      // 手动渠道没有独立账号概念，内置渠道直接用渠道名当展示标识
      const manualLabel = channel.statsId != null && channel.statsId > 0 && channel.statsId < 101 ? "" : "手动渠道";
      if (keys.length) {
        keys.forEach((key, keyIndex) => {
          rows.push({
            id: managedProviderId(channel.id, "default", keyIndex),
            channelId: channel.id,
            channelName: channel.name || alias || channel.id,
            account: "default",
            accountLabel: manualLabel,
            keyIndex,
            key,
            anonymousKey: false,
            protocol,
            alias,
            siteId: "",
            group: channelKeyGroup(channel, key),
            models: modelsForInventory(channel, null, key),
          });
        });
      } else {
        rows.push({
          id: managedProviderId(channel.id, "default", 0),
          channelId: channel.id,
          channelName: channel.name || alias || channel.id,
          account: "default",
          accountLabel: manualLabel,
          keyIndex: 0,
          key: "",
          anonymousKey: anonymous,
          protocol,
          alias,
          siteId: "",
          group: "默认分组",
          models: modelsForInventory(channel, null, ""),
        });
      }
    }
  }
  // 没有命中任何已选模型的 Key 不展示
  return rows.filter((row) => row.models.length > 0);
});

const usableRows = computed(() => inventoryRows.value.filter(rowHasKey));

/**
 * 展开区的一个可编辑条目。参数设置跟随**界面粒度**（用户看到的这一项），
 * 而不是内部写入条目 —— 常规模式下一个账号的多路 Key 会各写一条供应商，
 * 但用户只想设一次模型参数，因此一个条目可覆盖多个写入目标。
 */
interface InventoryEntry {
  /** 参数回显用键（组内唯一）：取该条目首个写入目标。 */
  key: string;
  /** 展示文本：常规 = 模型名；模型×站点 = 站点 · 账号[ · 分组]。 */
  label: string;
  /** 完整模型标识符（如 alias/model 或 provider/model） */
  modelId: string;
  /** 是否按标识符展示（等宽字体）。 */
  mono: boolean;
  /** 该条目实际管辖的全部写入目标键（保存时逐个写入）。 */
  scope: string[];
}

/** 清单分组：一个可展开条目。两种模式的分组维度不同，但结构一致（默认收起）。 */
interface InventoryGroup {
  key: string;
  /** 行标题：常规 = 渠道别名 · 账号；模型×站点 = 裸模型名。 */
  title: string;
  /** 标题下的一行摘要。 */
  subtitle: string;
  /** 展开区标题。 */
  detailLabel: string;
  /** 展开区可编辑条目。 */
  entries: InventoryEntry[];
  /** 展开区尾注（渠道、站点、协议等）。 */
  footnote: string[];
  /** 组内条目（状态判定与逐行生效用）。 */
  rows: ProxyInventoryRow[];
}

/**
 * 展示用模型名：只取最后一个斜杠之后的部分。
 *
 * 仅用于展示。渠道别名与上游命名空间前缀（`x666/deepseek-ai/DeepSeek-V4`）是路由用的，
 * 在清单里逐行重复没有信息量；写入 Agent 配置时仍用完整 ID，否则网关会路由不到。
 */
function modelDisplayName(model: string) {
  const at = model.lastIndexOf("/");
  return at >= 0 ? model.slice(at + 1) : model;
}

/**
 * 分组键：大小写不敏感。
 *
 * 同一模型在不同站点可能大小写写法不一（`GLM-5.3-Flash` / `glm-5.3-flash`），
 * 对用户是同一个模型，必须归成一行，否则清单里会出现看起来重复的条目。
 */
function modelGroupKey(name: string) {
  return name.toLowerCase();
}

/** 名称排序：大小写不敏感，数字段按数值比较（`gpt-5` 排在 `gpt-10` 之前）。 */
const nameCollator = new Intl.Collator("zh-Hans-CN", { numeric: true, sensitivity: "base" });

function sortByName(groups: InventoryGroup[]) {
  return [...groups].sort((a, b) => nameCollator.compare(a.title, b.title));
}

/**
 * 「站点 · 账号[ · 分组]」：同一站点+账号下只有一种 Key 分组时省略分组段，
 * 多种分组才需要它来区分（与写入配置的供应商标识同一口径）。
 */
function sourceLabel(row: ProxyInventoryRow) {
  const parts = [row.alias || row.channelName, row.accountLabel].filter(Boolean);
  const groups = new Set(
    inventoryRows.value
      .filter((item) => item.channelId === row.channelId && item.accountLabel === row.accountLabel)
      .map((item) => item.group),
  );
  if (groups.size > 1) parts.push(row.group);
  return parts.join(" · ");
}

/** 参数设置键：`providerId::modelId`（= 实际写入条目）。 */
function settingsKey(providerId: string, modelId: string) {
  return `${providerId}::${modelId}`;
}

/**
 * 思考档键里的模型段：适配器统一用「裸模型」——OpenCode 的模型 ID 自带供应商前缀
 * （`openhub-x/x666/deepseek-v4`），落盘前会被适配器剥掉，这里必须同样剥一次，
 * 否则写出的档位键对不上、读回也找不到。
 */
function shortModelId(providerId: string, modelId: string) {
  const prefix = `${providerId}/`;
  return modelId.startsWith(prefix) ? modelId.slice(prefix.length) : modelId;
}

/** 思考档在 `perModelEffort` 里的键（与适配器落盘口径一致）。 */
function perModelEffortKey(providerId: string, modelId: string) {
  return `${providerId}/${shortModelId(providerId, modelId)}`;
}

/**
 * 该行实际写入用的供应商标识 —— 必须与 `buildPatch` 完全一致，
 * 否则参数键对不上写出的条目，回显与写入会各说各话。
 * 模型×站点模式整份清单合并为一条网关供应商，常规模式逐行各成一条。
 */
function writeProviderId(row: ProxyInventoryRow) {
  return modelIdentityMode.value ? SINGLE_GATEWAY_PROVIDER_ID : row.id;
}

/** 该行某模型的实际写入条目键。 */
function rowEntryKey(row: ProxyInventoryRow, model: string) {
  const providerId = writeProviderId(row);
  return settingsKey(providerId, modelIdForPatch(model, providerId));
}

/** 常规模式：按「渠道 · 账号」分组，展开列出该账号下的模型（短名，可设参数）。 */
function accountGroups(): InventoryGroup[] {
  const groups = new Map<string, InventoryGroup>();
  for (const row of inventoryRows.value) {
    const key = `${row.channelId}::${row.account}`;
    let group = groups.get(key);
    if (!group) {
      group = {
        key,
        title: [row.alias || row.channelName, row.accountLabel].filter(Boolean).join(" · "),
        subtitle: "",
        detailLabel: "模型",
        entries: [],
        footnote: [],
        rows: [],
      };
      groups.set(key, group);
    }
    group.rows.push(row);
    for (const model of row.models) {
      const name = modelDisplayName(model);
      const target = settingsKey(row.id, modelIdForPatch(model, row.id));
      const existing = group.entries.find((entry) => modelGroupKey(entry.label) === modelGroupKey(name));
      if (existing) {
        // 同账号的多路 Key 各写一条供应商：参数条目追加写入目标，一次设置覆盖全部 Key。
        if (!existing.scope.includes(target)) existing.scope.push(target);
        continue;
      }
      group.entries.push({ key: target, label: name, modelId: model, mono: true, scope: [target] });
    }
  }
  for (const group of groups.values()) {
    const head = group.rows[0];
    const keys = group.rows.filter(rowHasKey);
    const masked = keys.length
      ? keys.map((row) => (row.key ? maskKey(row.key) : "匿名 Key")).join("、")
      : anonymousGroupLabel(group.rows[0]);
    const keyGroups = [...new Set(group.rows.map((row) => row.group))];
    group.entries.sort((a, b) => nameCollator.compare(a.label, b.label));
    group.subtitle = [
      masked,
      ...(keyGroups.length > 1 ? [keyGroups.join("/")] : []),
      `${group.entries.length} 个模型`,
    ].join(" · ");
    group.footnote = [
      head.channelName,
      head.siteId ? `站点 ${head.siteId}` : "",
      protocolLabel(head.protocol),
      `${group.rows.filter(rowHasKey).length} 路 Key`,
    ].filter(Boolean);
  }
  return sortByName([...groups.values()]);
}

/** 模型×站点模式：按展示模型名分组（大小写不敏感），展开列出提供它的站点（可设参数）。 */
function modelGroups(): InventoryGroup[] {
  const groups = new Map<string, InventoryGroup>();
  for (const row of inventoryRows.value) {
    for (const model of row.models) {
      const name = modelDisplayName(model);
      const key = modelGroupKey(name);
      let group = groups.get(key);
      if (!group) {
        group = {
          key: `model::${key}`,
          title: name,
          subtitle: "",
          detailLabel: "提供该模型的站点",
          entries: [],
          footnote: [],
          rows: [],
        };
        groups.set(key, group);
      }
      if (!group.rows.includes(row)) group.rows.push(row);
      const label = sourceLabel(row);
      const target = rowEntryKey(row, model);
      // 同一站点下多个账号共享同一个模型 ID（别名相同），对网关就是同一条配置，
      // 参数无法按账号分开 —— 合并成一项并把账号并列展示，避免给出可分别编辑的假象。
      const existing = group.entries.find((entry) => entry.key === target);
      if (existing) {
        if (!existing.scope.includes(target)) existing.scope.push(target);
        if (!existing.label.includes(label)) existing.label = `${existing.label}、${label}`;
        continue;
      }
      group.entries.push({ key: target, label, modelId: model, mono: false, scope: [target] });
    }
  }
  for (const group of groups.values()) {
    const sites = new Set(group.rows.map((row) => `${row.channelId}::${row.account}`));
    const withKey = group.rows.filter(rowHasKey).length;
    group.subtitle = `${sites.size} 个站点提供 · ${withKey} 路 Key 可用`;
    group.entries.sort((a, b) => nameCollator.compare(a.label, b.label));
  }
  return sortByName([...groups.values()]);
}

const inventoryGroups = computed<InventoryGroup[]>(() =>
  modelIdentityMode.value ? modelGroups() : accountGroups(),
);

function isGroupExpanded(key: string) {
  return expandedKeys.value.has(key);
}

function toggleGroup(key: string) {
  const next = new Set(expandedKeys.value);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  expandedKeys.value = next;
}

/** 组状态：组内任一 Key 生效/漂移即体现到组上，收起时也能看出概况。 */
function groupState(group: InventoryGroup) {
  const states = group.rows.map((row) => rowState(row));
  if (states.includes("drifted")) return "drifted" as const;
  if (states.some((s) => s === "applied" || s === "current")) return "applied" as const;
  return "pending" as const;
}

function groupDiffNote(group: InventoryGroup): string {
  return group.rows.map((row) => rowDiffNote(row)).find(Boolean) ?? "";
}

function groupCanApply(group: InventoryGroup) {
  return !wholeListComparison.value && group.rows.some(rowHasKey);
}

/** 组内可点击「生效」的条目：默认取第一条带 Key 的。 */
function groupPrimaryRow(group: InventoryGroup): ProxyInventoryRow | null {
  return group.rows.find(rowHasKey) ?? null;
}

async function applyGroup(group: InventoryGroup) {
  const row = groupPrimaryRow(group);
  if (row) await applyRow(row);
}

const gatewayPort = computed(() =>
  proxyStatus.value?.port || proxyConfig.value?.port || DEFAULT_SERVICE_PORT,
);
const gatewayKey = computed(() => proxyConfig.value?.apiKey?.trim() || "");
const gatewayOrigin = computed(() => `http://127.0.0.1:${gatewayPort.value}`);
const gatewayBaseUrl = computed(() =>
  activeTool.value === "claude" ? gatewayOrigin.value : `${gatewayOrigin.value}/v1`,
);
const gatewayReady = computed(() => !!proxyStatus.value?.running);

// —— 模型参数（最大窗口 / 最大输出 / 默认思考级别 / 思考级别筛选配置）——
//
// 父子级联与覆盖机制：
// 1. 父列表（每个渠道/供应商）：定义基准默认值（最大窗口、最大输出、默认思考级别、思考级别筛选配置）。
// 2. 子列表（该渠道下属的具体模型）：默认全部继承父级配置（显示占位与继承标记）；
//    用户在子列表中修改任意字段，即视为覆盖父级配置（高亮显示已覆盖标记，并支持一键恢复继承）。
// 3. 有效值计算：子级覆盖优先 > 父级配置 > 工具默认。落盘时写入完整配置。

const THINKING_EFFORT_OPTIONS = [
  "off",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
] as const;

// —— models.dev 按模型能力（思考档位 / 上下文 / 输出上限 / 交错字段等）——
//
// 目录快照（Rust 侧 models.dev 富化结果）里已存好每个模型的 reasoning_options 等字段，
// 这里按清单里出现的 modelId 批量拉取（`get_model_capabilities`，归一化匹配、查不到不臆造），
// 用于把「这个模型**实际支持**哪些思考档位」体现在逐模型参数行上，
// 并提供窗口/输出上限的一键载入。

const modelCapabilities = ref<Record<string, ModelCapabilities>>({});

function normalizeModelKey(raw: string): string {
  let s = raw.trim().toLowerCase();
  const cut = s.search(/[:@]/);
  if (cut >= 0) s = s.slice(0, cut);
  const slash = s.lastIndexOf("/");
  if (slash >= 0) s = s.slice(slash + 1);
  return s.replace(/[._]+/g, "-");
}

/** 该条目对应的 models.dev 能力；未命中或目录未同步时为 null。 */
function capabilitiesFor(entry: InventoryEntry): ModelCapabilities | null {
  if (!entry.modelId) return null;
  const direct = modelCapabilities.value[entry.modelId];
  if (direct) return direct;
  // 同归一化键的其它拼写（大小写 / 前缀差异）也算命中
  const normalized = normalizeModelKey(entry.modelId);
  for (const [key, caps] of Object.entries(modelCapabilities.value)) {
    if (normalizeModelKey(key) === normalized) return caps;
  }
  return null;
}

/**
 * 模型在 models.dev 声明的思考档位，已归一化到本页词表（`none` → `off`）。
 *
 * 模型行的芯片全集固定用规范词表 [`THINKING_EFFORT_OPTIONS`]，与父级「思考级别筛选配置」
 * 同一口径 —— 不能拿目录声明值当全集：目录用 `none`、本页用 `off`，且声明集常与父级
 * 勾选集不相交（父级选了 off/xhigh，模型只声明 low/medium/high），当全集会导致
 * 「父级勾选的档位在模型行整块消失、词表还对不上」。声明值改用本函数作支持标记，
 * 只用于提示与一键载入。
 *
 * 返回空数组表示「目录无数据」（未命中或只声明 `toggle`），不表示「不支持思考」；
 * 调用方据此区分「未声明该档」与「目录没数据」，避免误标。
 */
function declaredModelEfforts(entry: InventoryEntry): string[] {
  const caps = capabilitiesFor(entry);
  const values =
    caps?.reasoningOptions.find((option) => option.kind === "effort")?.values ?? [];
  const out: string[] = [];
  for (const raw of values) {
    const level = raw.trim().toLowerCase();
    const normalized = level === "none" ? "off" : level;
    if (
      (THINKING_EFFORT_OPTIONS as readonly string[]).includes(normalized) &&
      !out.includes(normalized)
    ) {
      out.push(normalized);
    }
  }
  return out;
}

/** 该档是否未被 models.dev 声明；目录无数据时不判定，避免把「没数据」标成「不支持」。 */
function childEffortUndeclared(entry: InventoryEntry, level: string): boolean {
  const declared = declaredModelEfforts(entry);
  return declared.length > 0 && !declared.includes(level);
}

/** 模型行档位按钮的提示：区分「切换/继承」与「目录未声明该档」。 */
function childEffortChipTitle(groupKey: string, entry: InventoryEntry, level: string): string {
  const overridden = getEffectiveModelParams(groupKey, entry.key, entry.modelId).hasEffortsListOverride;
  const base = overridden ? `切换 ${level}` : (modelIdentityMode.value ? "继承自父级；点击自定义覆盖" : `切换 ${level}`);
  return childEffortUndeclared(entry, level) ? `${base}（models.dev 未声明该档）` : base;
}

/** 该模型是否只有开/关式思考（toggle），供 UI 提示「仅支持开/关」。 */
function modelReasoningToggleOnly(entry: InventoryEntry): boolean {
  const caps = capabilitiesFor(entry);
  if (!caps || !caps.reasoningOptions.length) return false;
  return !caps.reasoningOptions.some((option) => option.kind === "effort");
}

async function refreshModelCapabilities() {
  const keys = [
    ...new Set(
      inventoryGroups.value
        .flatMap((group) => group.entries.map((entry) => entry.modelId))
        .filter((modelId) => modelId && !(modelId in modelCapabilities.value)),
    ),
  ];
  if (!keys.length) return;
  try {
    const result = await runCommand<Record<string, ModelCapabilities>>(
      "get_model_capabilities",
      { keys },
    );
    // 未命中的 key 后端不返回：记为 null，避免每次清单变化都重复请求
    for (const key of keys) {
      modelCapabilities.value[key] = result[key] ?? (null as unknown as ModelCapabilities);
    }
  } catch {
    /* 目录尚未同步时静默：参数行回落到全局档位列表 */
  }
}

let capabilityDebounce: number | null = null;
watch(
  inventoryGroups,
  () => {
    if (capabilityDebounce !== null) window.clearTimeout(capabilityDebounce);
    capabilityDebounce = window.setTimeout(() => {
      capabilityDebounce = null;
      void refreshModelCapabilities();
    }, 300);
  },
  { immediate: true, deep: false },
);

// 目录同步完成后清空缓存并重新拉取：首次进入页面时可能目录尚未同步完，
// modelCapabilities 中所有 key 被标记为 null，之后不会重试。
// 监听同步状态变化，从 true→false（完成）时清空并重新请求。
watch(
  () => modelCatalogSyncing.value,
  (syncing, prev) => {
    if (prev && !syncing) {
      modelCapabilities.value = {};
      void refreshModelCapabilities();
    }
  },
);

/** 能力摘要徽标：上下文 / 输出上限 / 交错字段 / 快速档，目录未命中时为空。 */
function capabilityBadges(caps: ModelCapabilities): string[] {
  const badges: string[] = [];
  if (caps.contextLength > 0) badges.push(`窗口 ${formatTokenCount(caps.contextLength)}`);
  if (caps.maxOutputTokens > 0) badges.push(`输出 ${formatTokenCount(caps.maxOutputTokens)}`);
  if (caps.maxInputTokens && caps.maxInputTokens < caps.contextLength) {
    badges.push(`输入上限 ${formatTokenCount(caps.maxInputTokens)}`);
  }
  if (caps.interleavedFields.length) badges.push(`交错 ${caps.interleavedFields.join("/")}`);
  if (caps.hasFastMode) badges.push("快速档");
  const outputs = (caps.outputModalities ?? []).filter((m) => m !== "text");
  if (outputs.length) badges.push(`输出模态 ${outputs.join("/")}`);
  return badges;
}

/** 一键把 models.dev 的窗口/输出上限与思考档位载入为该模型的覆盖参数。 */
function loadCapabilitiesToChild(entry: InventoryEntry) {
  const caps = capabilitiesFor(entry);
  if (!caps) return;
  // 载入的是目录声明档位（已归一到本页词表），而非全集：这是「按目录收敛」的显式动作
  const declared = declaredModelEfforts(entry);
  const patch: ChildParamOverride = declared.length ? { efforts: declared } : {};
  if (caps.contextLength > 0) patch.contextWindow = caps.contextLength;
  if (caps.maxOutputTokens > 0) patch.maxOutput = caps.maxOutputTokens;
  if (!Object.keys(patch).length) {
    showToast("该模型在 models.dev 没有可用参数", true);
    return;
  }
  setChildOverride(entry, patch);
  showToast(
    declared.length
      ? `已载入 models.dev 参数：${entry.label || entry.modelId}（思考档 ${declared.join("/")}）`
      : `已载入 models.dev 参数：${entry.label || entry.modelId}`,
  );
}

interface ParentParamValue {
  contextWindow: number | null;
  maxOutput: number | null;
  defaultReasoningEffort: string;
  efforts: string[];
}

interface ChildParamOverride {
  contextWindow?: number | null;
  maxOutput?: number | null;
  defaultReasoningEffort?: string;
  efforts?: string[];
}

const parentConfigs = reactive<Record<string, ParentParamValue>>({});
const childOverrides = reactive<Record<string, ChildParamOverride>>({});
const sublistSearch = reactive<Record<string, string>>({});

const CONTEXT_PRESETS = [
  { label: "128K", value: 131072 },
  { label: "200K", value: 204800 },
  { label: "1M", value: 1048576 },
];

const OUTPUT_PRESETS = [
  { label: "4K", value: 4096 },
  { label: "8K", value: 8192 },
  { label: "16K", value: 16384 },
  { label: "64K", value: 65536 },
];

function formatTokenCount(num: number | null | undefined): string {
  if (num == null || num <= 0) return "";
  if (num >= 1048576) {
    return `${(num / 1048576).toFixed(num % 1048576 === 0 ? 0 : 1)}M`;
  }
  if (num >= 1024 && num % 1024 === 0) {
    return `${num / 1024}K`;
  }
  return num.toLocaleString();
}

async function copyModelId(modelId: string) {
  try {
    await navigator.clipboard.writeText(modelId);
    showToast(`已复制模型 ID: ${modelId}`);
  } catch {
    showToast("复制失败", true);
  }
}

function persistConfigs() {
  if (!activeTool.value) return;
  const tool = activeTool.value;
  const stored = preferences.agentModelConfigs ?? {};
  updatePreferences({
    agentModelConfigs: {
      ...stored,
      [tool]: {
        parentConfigs: JSON.parse(JSON.stringify(parentConfigs)),
        childOverrides: JSON.parse(JSON.stringify(childOverrides)),
      },
    },
  });
}

function loadStoredConfigs() {
  if (!activeTool.value) return;
  const saved = preferences.agentModelConfigs?.[activeTool.value];
  if (saved) {
    if (saved.parentConfigs) {
      for (const [k, v] of Object.entries(saved.parentConfigs)) {
        const val = { ...v } as ParentParamValue;
        if (!Array.isArray(val.efforts)) {
          val.efforts = [...THINKING_EFFORT_OPTIONS];
        }
        parentConfigs[k] = val;
      }
    }
    if (saved.childOverrides) {
      for (const [k, v] of Object.entries(saved.childOverrides)) {
        childOverrides[k] = { ...v } as ChildParamOverride;
      }
    }
  }
}

function getParentConfig(groupKey: string): ParentParamValue {
  const existing = parentConfigs[groupKey];
  if (existing) {
    if (!Array.isArray(existing.efforts)) {
      existing.efforts = [...THINKING_EFFORT_OPTIONS];
    }
    return existing;
  }

  const snap = snapshot.value;
  const initEfforts = snap?.defaults?.reasoningEffortOptions?.length
    ? [...snap.defaults.reasoningEffortOptions]
    : snap?.thinking?.effortLevelOptions?.length
      ? [...snap.thinking.effortLevelOptions]
      : [...THINKING_EFFORT_OPTIONS];

  const defaultEffort = snap?.defaults?.reasoningEffort || snap?.thinking?.effortLevel || "";

  const config: ParentParamValue = {
    contextWindow: snap?.context?.contextWindow ?? null,
    maxOutput: snap?.context?.maxOutputTokens ?? null,
    defaultReasoningEffort: defaultEffort,
    efforts: initEfforts.filter((e) => (THINKING_EFFORT_OPTIONS as readonly string[]).includes(e)),
  };
  if (!config.efforts.length) {
    config.efforts = [...THINKING_EFFORT_OPTIONS];
  }
  return config;
}

function setParentConfig(groupKey: string, patch: Partial<ParentParamValue>) {
  const current = getParentConfig(groupKey);
  parentConfigs[groupKey] = { ...current, ...patch };
  persistConfigs();
}

function applyContextPresetToParent(groupKey: string, value: number) {
  setParentConfig(groupKey, { contextWindow: value });
}

function applyOutputPresetToParent(groupKey: string, value: number) {
  setParentConfig(groupKey, { maxOutput: value });
}

function toggleParentEffort(groupKey: string, level: string) {
  const current = getParentConfig(groupKey).efforts ?? [];
  const next = current.includes(level)
    ? current.filter((item) => item !== level)
    : [...current, level];
  setParentConfig(groupKey, { efforts: next });
}

function selectAllParentEfforts(groupKey: string) {
  setParentConfig(groupKey, { efforts: [...THINKING_EFFORT_OPTIONS] });
}

function clearParentEfforts(groupKey: string) {
  setParentConfig(groupKey, { efforts: [] });
}

function getChildOverride(entryKey: string): ChildParamOverride {
  return childOverrides[entryKey] ?? {};
}

function getEffectiveModelParams(groupKey: string, entryKey: string, modelId?: string) {
  const child = getChildOverride(entryKey);

  const hasContextWindowOverride = child.contextWindow !== undefined;
  const hasMaxOutputOverride = child.maxOutput !== undefined;
  const hasEffortOverride = child.defaultReasoningEffort !== undefined;
  const hasEffortsListOverride = child.efforts !== undefined;

  // 两种模式共用：未覆盖 efforts 时优先从 models.dev 匹配该模型实际支持的思考档位
  const modelDerivedEfforts = (() => {
    if (hasEffortsListOverride || !modelId) return null;
    // 通过 modelId 构造临时 InventoryEntry 调用 declaredModelEfforts
    const tempEntry: InventoryEntry = { key: entryKey, label: "", modelId, mono: false, scope: [] };
    const declared = declaredModelEfforts(tempEntry);
    return declared.length > 0 ? declared : null;
  })();

  // 常规模式：无父级统一配置，子模型参数独立设置，未设项回退到 Agent 级快照
  if (!modelIdentityMode.value) {
    const snap = snapshot.value;
    const fallbackContext = snap?.context?.contextWindow ?? null;
    const fallbackOutput = snap?.context?.maxOutputTokens ?? null;
    const fallbackEffort = snap?.defaults?.reasoningEffort || snap?.thinking?.effortLevel || "";
    const fallbackEfforts = snap?.defaults?.reasoningEffortOptions?.length
      ? [...snap.defaults.reasoningEffortOptions]
      : snap?.thinking?.effortLevelOptions?.length
        ? [...snap.thinking.effortLevelOptions]
        : [...THINKING_EFFORT_OPTIONS];

    const contextWindow = hasContextWindowOverride ? child.contextWindow : fallbackContext;
    const maxOutput = hasMaxOutputOverride ? child.maxOutput : fallbackOutput;
    const defaultReasoningEffort = hasEffortOverride ? child.defaultReasoningEffort : fallbackEffort;
    // 未覆盖时优先用 models.dev 匹配到的思考档位，其次回退到 Agent 级快照
    const efforts = hasEffortsListOverride
      ? child.efforts
      : (modelDerivedEfforts ?? fallbackEfforts);

    const isOverridden = hasContextWindowOverride || hasMaxOutputOverride || hasEffortOverride || hasEffortsListOverride;
    const overriddenCount = (hasContextWindowOverride ? 1 : 0)
      + (hasMaxOutputOverride ? 1 : 0)
      + (hasEffortOverride ? 1 : 0)
      + (hasEffortsListOverride ? 1 : 0);

    return {
      contextWindow,
      maxOutput,
      defaultReasoningEffort: defaultReasoningEffort ?? "",
      efforts: Array.isArray(efforts) ? efforts : [...THINKING_EFFORT_OPTIONS],
      isOverridden,
      overriddenCount,
      hasContextWindowOverride,
      hasMaxOutputOverride,
      hasEffortOverride,
      hasEffortsListOverride,
    };
  }

  // 模型×站点模式：子模型默认继承父级配置，可逐项覆盖
  const parent = getParentConfig(groupKey);

  const contextWindow = hasContextWindowOverride ? child.contextWindow : parent.contextWindow;
  const maxOutput = hasMaxOutputOverride ? child.maxOutput : parent.maxOutput;
  const defaultReasoningEffort = hasEffortOverride ? child.defaultReasoningEffort : parent.defaultReasoningEffort;
  // 未覆盖时优先用 models.dev 匹配到的思考档位，其次继承父级
  const efforts = hasEffortsListOverride
    ? child.efforts
    : (modelDerivedEfforts ?? parent.efforts);

  const isOverridden = hasContextWindowOverride || hasMaxOutputOverride || hasEffortOverride || hasEffortsListOverride;
  const overriddenCount = (hasContextWindowOverride ? 1 : 0)
    + (hasMaxOutputOverride ? 1 : 0)
    + (hasEffortOverride ? 1 : 0)
    + (hasEffortsListOverride ? 1 : 0);

  return {
    contextWindow,
    maxOutput,
    defaultReasoningEffort: defaultReasoningEffort ?? "",
    efforts: Array.isArray(efforts) ? efforts : [...THINKING_EFFORT_OPTIONS],
    isOverridden,
    overriddenCount,
    hasContextWindowOverride,
    hasMaxOutputOverride,
    hasEffortOverride,
    hasEffortsListOverride,
  };
}

function setChildOverride(entry: InventoryEntry, patch: Partial<ChildParamOverride>) {
  for (const key of entry.scope) {
    const current = childOverrides[key] ?? {};
    childOverrides[key] = { ...current, ...patch };
  }
  persistConfigs();
}

function resetChildOverride(entry: InventoryEntry) {
  for (const key of entry.scope) {
    delete childOverrides[key];
  }
  persistConfigs();
}

function resetAllChildren(group: InventoryGroup) {
  for (const entry of group.entries) {
    resetChildOverride(entry);
  }
}

function toggleChildEffort(groupKey: string, entry: InventoryEntry, level: string) {
  const currentEfforts = getEffectiveModelParams(groupKey, entry.key, entry.modelId).efforts ?? [];
  const next = currentEfforts.includes(level)
    ? currentEfforts.filter((item) => item !== level)
    : [...currentEfforts, level];
  setChildOverride(entry, { efforts: next });
}

function setChildEffortsInherited(entry: InventoryEntry) {
  setChildOverride(entry, { efforts: undefined });
}

function groupOverriddenCount(group: InventoryGroup): number {
  if (!modelIdentityMode.value) return 0;
  return group.entries.filter((entry) => getEffectiveModelParams(group.key, entry.key, entry.modelId).isOverridden).length;
}

function filteredEntries(group: InventoryGroup): InventoryEntry[] {
  const query = (sublistSearch[group.key] ?? "").trim().toLowerCase();
  if (!query) return group.entries;
  return group.entries.filter(
    (entry) =>
      entry.label.toLowerCase().includes(query) ||
      (entry.modelId && entry.modelId.toLowerCase().includes(query)),
  );
}

function hydrateFromSnapshot() {
  const snap = snapshot.value;
  if (!snap) return;

  for (const group of inventoryGroups.value) {
    if (!parentConfigs[group.key]) {
      getParentConfig(group.key);
    }
  }

  for (const model of snap.models) {
    const key = settingsKey(model.provider, model.id);
    if (!childOverrides[key] && (model.contextWindow > 0 || model.maxOutput > 0)) {
      childOverrides[key] = {
        contextWindow: model.contextWindow > 0 ? model.contextWindow : undefined,
        maxOutput: model.maxOutput > 0 ? model.maxOutput : undefined,
      };
    }
  }

  // Agent 级默认思考档位列表：与之一致的 perModelEffort 是旧版默认值冗余写入，
  // 不应加载为 override —— 否则会阻止 getEffectiveModelParams 从 models.dev 获取该模型实际档位。
  const agentDefaultEfforts = new Set(
    snap.defaults?.reasoningEffortOptions?.length
      ? snap.defaults.reasoningEffortOptions
      : snap.thinking?.effortLevelOptions?.length
        ? snap.thinking.effortLevelOptions
        : THINKING_EFFORT_OPTIONS,
  );
  const isStaleDefault = (raw: string) => {
    const parts = raw.split(",").map((s) => s.trim()).filter(Boolean);
    return parts.length === agentDefaultEfforts.size
      && parts.every((p) => agentDefaultEfforts.has(p));
  };

  for (const [effortKey, rawEffort] of Object.entries(snap.defaults.perModelEffort ?? {})) {
    const at = effortKey.indexOf("/");
    if (at > 0) {
      const providerId = effortKey.slice(0, at);
      const shortId = effortKey.slice(at + 1);
      const key = settingsKey(providerId, modelIdForPatch(shortId, providerId));
      if (!childOverrides[key]?.efforts && rawEffort && !isStaleDefault(rawEffort)) {
        const efforts = rawEffort.split(",").map((s) => s.trim()).filter(Boolean);
        if (efforts.length) {
          childOverrides[key] = {
            ...(childOverrides[key] ?? {}),
            efforts,
          };
        }
      }
    }
  }

  // 清除 loadStoredConfigs 加载的旧版默认值 efforts（与 Agent 级默认列表完全一致的 override）
  for (const [key, override] of Object.entries(childOverrides)) {
    if (override.efforts && override.efforts.length === agentDefaultEfforts.size
      && override.efforts.every((e) => agentDefaultEfforts.has(e))) {
      delete childOverrides[key].efforts;
      if (childOverrides[key].contextWindow === undefined && childOverrides[key].maxOutput === undefined
        && childOverrides[key].defaultReasoningEffort === undefined) {
        delete childOverrides[key];
      }
    }
  }
}

watch(
  () => activeTool.value,
  () => {
    Object.keys(parentConfigs).forEach((k) => delete parentConfigs[k]);
    Object.keys(childOverrides).forEach((k) => delete childOverrides[k]);
    loadStoredConfigs();
    hydrateFromSnapshot();
  },
  { immediate: true },
);

watch(
  () => snapshot.value,
  () => {
    hydrateFromSnapshot();
  },
);

function parseLimitInput(event: Event): number | null {
  const raw = (event.target as HTMLInputElement).value.trim();
  if (!raw) return null;
  const value = Number(raw);
  return Number.isFinite(value) && value >= 0 ? Math.floor(value) : null;
}

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

/**
 * 模型写进配置时的实际 ID：OpenCode 的模型 key 是「供应商/模型」，
 * 写入时带供应商标识（adapter 落盘时会只剥供应商标识、保留渠道路由别名）。
 */
function modelIdForPatch(model: string, providerId: string) {
  return activeTool.value === "opencode" ? `${providerId}/${model}` : model;
}

function modelsForPatch(row: ProxyInventoryRow, providerId: string): LocalToolModelEntry[] {
  const groupKey = `${row.channelId}::${row.account}`;
  return row.models.map((model) => {
    const id = modelIdForPatch(model, providerId);
    const targetKey = settingsKey(providerId, id);
    const effective = getEffectiveModelParams(groupKey, targetKey, model);
    return {
      id,
      name: modelDisplayName(model),
      provider: providerId,
      contextWindow: effective.contextWindow ?? 0,
      maxOutput: effective.maxOutput ?? 0,
    };
  });
}

/** 逐模型思考档：适配器从 `defaults.perModelEffort` 读取，键为「供应商/模型 ID」。 */
function perModelEffortForPatch(rows: ProxyInventoryRow[]) {
  const out: Record<string, string> = { ...(snapshot.value?.defaults.perModelEffort ?? {}) };
  for (const row of rows) {
    const providerId = writeProviderId(row);
    const groupKey = `${row.channelId}::${row.account}`;
    for (const model of row.models) {
      const modelId = modelIdForPatch(model, providerId);
      const targetKey = settingsKey(providerId, modelId);
      const effective = getEffectiveModelParams(groupKey, targetKey, model);
      const effortKey = perModelEffortKey(providerId, modelId);
      if (effective.efforts && effective.efforts.length) {
        out[effortKey] = effective.efforts.join(",");
      }
    }
  }
  return out;
}

/** 由给定清单行构造供应商条目（providerId 在两种模式下不同）。 */
function providerFromRows(rows: ProxyInventoryRow[], providerId: string): LocalToolProviderEntry {
  const head = rows[0];
  const models = [...new Set(rows.flatMap((row) => row.models))];
  return {
    id: providerId,
    name: providerId === SINGLE_GATEWAY_PROVIDER_ID
      ? "OpenHub 网关 · 模型×站点"
      : rowIdentifier(head),
    baseUrl: gatewayBaseUrl.value,
    apiKey: gatewayKey.value,
    protocol: toolProtocol(head),
    models: models.map((model) => modelIdForPatch(model, providerId)),
  };
}

/** 存在的配置文件及其相对本软件上次写入的状态。 */
const managedFiles = computed(() => (snapshot.value?.files ?? []).filter((file) => file.exists));

/** 生效时的覆盖策略说明：指纹吻合直接覆盖，否则先备份。 */
function managedStateLabel(file: LocalToolConfigFile) {
  if (file.managed === "intact") return "本软件写入，未被改动，生效时直接覆盖";
  if (file.managed === "modified") return "本软件写入后被外部修改，生效时先备份";
  return "非本软件写入，生效时先备份";
}

/** 写进 Agent 配置的供应商标识：站点别名-账号[-Key 分组]。 */
function rowIdentifier(row: ProxyInventoryRow) {
  const parts = [row.alias || row.channelName, row.accountLabel].filter(Boolean);
  const groups = new Set(
    inventoryRows.value
      .filter((item) => item.channelId === row.channelId && item.accountLabel === row.accountLabel)
      .map((item) => item.group),
  );
  if (groups.size > 1) parts.push(row.group);
  return parts.join("-");
}

/** 磁盘上现有的受管供应商标识（用于顶部汇总与无 diff 时的兜底展示）。 */
const appliedManagedIds = computed(() => {
  const ids = new Set<string>();
  const report = diffReport.value;
  if (report) {
    for (const id of report.managedProviders) ids.add(id);
    return ids;
  }
  for (const provider of snapshot.value?.providers ?? []) {
    if (provider.id.startsWith("openhub-")) ids.add(provider.id);
  }
  return ids;
});

/** 行标识 → 一致性结果（由后端语义比对给出）。 */
const diffIndex = computed(() => {
  const map = new Map<string, LocalToolDiffEntry>();
  for (const entry of diffReport.value?.entries ?? []) map.set(entry.key, entry);
  return map;
});

/**
 * 该行是否已生效。以「磁盘配置 == 这行组装的 patch」为准（后端语义比对）；
 * 尚未比对时（diff 未返回）不做任何「已生效」断言，避免误导。
 *
 * 整单比对的场景（模型×站点 / 一次全部）下所有行共享同一个整单结论。
 */
function rowDiff(row: ProxyInventoryRow) {
  return diffIndex.value.get(diffKeyForRow(row)) ?? null;
}

/**
 * 该行相对磁盘配置的状态。
 *
 * 判定口径按工具真实的写入语义区分，避免把「本来就没生效」误报成「不一致」：
 * - 整单比对（模型×站点 / 一次全部）：磁盘上必然同时存在多行条目，逐行无法归因，
 *   行内只区分是否与整单结论一致，细节放到顶部汇总；
 * - 一次一家（codex）：可存多家，只有当前选中的那家才谈得上「内容是否漂移」；
 * - 一路接入（claude）：磁盘上只能有一路，比对一致的那条就是当前接入。
 */
function rowState(row: ProxyInventoryRow): "applied" | "current" | "drifted" | "pending" {
  if (!snapshot.value || !rowHasKey(row)) return "pending";
  const entry = rowDiff(row);
  if (wholeListComparison.value) return entry?.consistent ? "applied" : "pending";
  if (switchEndpoint.value) {
    if (snapshot.value.defaults.provider !== row.id) return "pending";
    if (!entry) return "current";
    return entry.consistent ? "current" : "drifted";
  }
  if (entry) return entry.consistent ? "applied" : "pending";
  return appliedManagedIds.value.has(row.id) ? "applied" : "pending";
}

function isRowApplied(row: ProxyInventoryRow) {
  const state = rowState(row);
  return state === "applied" || state === "current";
}

/** 仅「这条已在配置里、但内容与清单对不上」才提示差异，其余情况不打扰。 */
function rowDiffNote(row: ProxyInventoryRow): string {
  if (rowState(row) !== "drifted") return "";
  const entry = rowDiff(row);
  if (!entry?.differences.length) return "";
  const head = entry.differences[0];
  const more = entry.differences.length > 1 ? `（另 ${entry.differences.length - 1} 处）` : "";
  return `${head}${more}`;
}

const allApplied = computed(() => {
  const rows = usableRows.value;
  if (!rows.length) return false;
  const report = diffReport.value;
  if (report) {
    if (wholeListComparison.value) {
      return report.entries.some((e) => e.key === WHOLE_LIST_KEY && e.consistent);
    }
    return rows.every((row) => isRowApplied(row));
  }
  return rows.every((row) => appliedManagedIds.value.has(row.id));
});

/**
 * 顶部一致性汇总。整单比对给一个总判断（差异明细也在这里），
 * 逐行比对给「已生效 / 未生效」计数 —— 一路接入类工具本就只能有一路生效，
 * 把它们逐行报成「不一致」只会制造噪音。
 */
const consistencySummary = computed(() => {
  const rows = usableRows.value;
  const report = diffReport.value;
  if (!report || !rows.length) return null;

  // 明细只摘前几条：完整差异由后端限量返回，这里再收一道，避免撑爆布局。
  const brief = (differences: string[]) => {
    const head = differences.slice(0, 2).join("；");
    const rest = differences.length - 2;
    return rest > 0 ? `${head}；等 ${rest + 2} 处` : head;
  };

  if (wholeListComparison.value) {
    const entry = report.entries.find((e) => e.key === WHOLE_LIST_KEY);
    if (!entry) return null;
    if (entry.consistent) return { tone: "ok" as const, text: "整份清单与磁盘配置一致", detail: "" };
    return {
      tone: "warn" as const,
      text: "整份清单与磁盘配置不一致",
      detail: brief(entry.differences),
    };
  }

  const states = rows.map((row) => rowState(row));
  const applied = states.filter((s) => s === "applied" || s === "current").length;
  const drifted = states.filter((s) => s === "drifted").length;
  const parts = [
    `已生效 ${applied} 行`,
    applied === rows.length ? "" : `未生效 ${rows.length - applied} 行`,
    drifted ? `内容漂移 ${drifted} 行` : "",
  ].filter(Boolean);
  return {
    tone: drifted ? ("warn" as const) : ("ok" as const),
    text: `一致性校验：${parts.join(" · ")}`,
    detail: drifted ? brief(rows.map(rowDiffNote).filter(Boolean)) : "",
  };
});

function applyBlockedReason(rows: ProxyInventoryRow[]) {
  if (!snapshot.value) return "尚未读取到 Agent 配置";
  if (!gatewayKey.value) return "网关 API Key 尚未生成，请先打开模型反代";
  if (!rows.length) return "当前没有可生效的反代条目";
  if (rows.some((row) => !rowHasKey(row))) return "有条目还没有 Key，请先到站点库同步";
  return "";
}

/**
 * 默认项：把清单里头的模型写进 Agent 的默认模型 / 档位映射。
 *
 * 只有「这次写入本身就该改默认项」时才动它：
 * - 模型×站点模式：整份清单参与，claude 的 opus/sonnet/haiku 按行序各取首个模型，
 *   天然支持三档指向不同站点；
 * - 常规模式的一次一家（codex）：被选中的那条即当前供应商，默认项随之切换；
 * - 常规模式的一路接入（claude）：这一路就是唯一接入；
 * - 常规模式的一次全部：写入只合并供应商与模型，不碰用户既有的默认模型。
 */
function patchDefaults(rows: ProxyInventoryRow[], providerId: string, whole: boolean): LocalToolDefaultsSection {
  const defaults: LocalToolDefaultsSection = snapshot.value
    ? JSON.parse(JSON.stringify(snapshot.value.defaults))
    : { model: "", provider: "", reasoningEffort: "", reasoningEffortOptions: [], perModelEffort: {} };
  if (!rows.length) return defaults;

  const setsDefaults =
    modelIdentityMode.value
    || (!whole && !allEndpoint.value && rows.length === 1);
  if (!setsDefaults) return defaults;

  if (switchEndpoint.value) defaults.provider = providerId;
  if (activeTool.value === "claude") {
    // 档位模型：模型模式按行各取首个（跨站点混搭），常规模式用本行前三个。
    const tierModels = modelIdentityMode.value
      ? [...new Set(rows.map((row) => row.models[0]).filter(Boolean))]
      : rows[0].models;
    const tiers = ["opus", "sonnet", "haiku"];
    tiers.forEach((tier, index) => {
      let next = tierModels[index] ?? tierModels[0];
      if (!modelIdentityMode.value) {
        // 常规模式：优先保留该档已映射到本行模型的现值。
        const current = defaults.perModelEffort?.[tier] || "";
        const bareCurrent = current.includes("/") ? current.slice(current.indexOf("/") + 1) : current;
        next = rows[0].models.find(
          (model) => model === current || (bareCurrent && model.endsWith(`/${bareCurrent}`)),
        ) ?? next;
      }
      if (next) defaults.perModelEffort[tier] = next;
    });
  }
  const allModels = [...new Set(rows.flatMap((row) => row.models))];
  if (allModels[0]) defaults.model = modelIdForPatch(allModels[0], providerId);
  return defaults;
}

/**
 * 组装一份完整 patch。两种模式共用同一份清单与写入地址，区别只在供应商如何切分：
 * - 常规：`rows` 各成一条供应商（一次一家的工具只更新被选中的那条，其余受管条目保留）；
 * - 模型×站点：整份清单合并为一条网关供应商，模型串带渠道别名。
 */
function buildPatch(rows: ProxyInventoryRow[], whole: boolean): LocalToolConfigPatch {
  const modelMode = modelIdentityMode.value;
  const singleProviderId = modelMode ? SINGLE_GATEWAY_PROVIDER_ID : (rows[0]?.id ?? SINGLE_GATEWAY_PROVIDER_ID);
  let providers: LocalToolProviderEntry[] = [];
  let models: LocalToolModelEntry[] = [];

  if (!rows.length) {
    return {
      baseHash: snapshot.value?.contentHash ?? "",
      providers: [],
      models: [],
      defaults: patchDefaults([], singleProviderId, whole),
      context: {
        contextWindow: null,
        autoCompactTokenLimit: null,
        maxOutputTokens: null,
        maxThinkingTokens: null,
      },
      thinking: {
        effortLevel: "",
        effortLevelOptions: [...THINKING_EFFORT_OPTIONS],
        maxThinkingTokens: null,
      },
    };
  }

  if (modelMode) {
    providers = [providerFromRows(rows, SINGLE_GATEWAY_PROVIDER_ID)];
    models = rows.flatMap((row) => modelsForPatch(row, SINGLE_GATEWAY_PROVIDER_ID));
  } else if (switchEndpoint.value && !whole) {
    const selected = rows[0];
    const selectedProvider = providerFromRows([selected], selected.id);
    // Codex 可以保存多家供应商；切换时只更新当前条目，已有 OpenHub 条目继续保留。
    const existing = snapshot.value?.providers ?? [];
    providers = [
      ...existing.filter((provider) => !provider.id.startsWith("openhub-")),
      ...existing.filter(
        (provider) => provider.id.startsWith("openhub-") && provider.id !== selected.id,
      ),
      selectedProvider,
    ];
    models = [
      ...(snapshot.value?.models ?? []).filter((model) => model.provider !== selected.id),
      ...modelsForPatch(selected, selected.id),
    ];
  } else {
    providers = rows.map((row) => providerFromRows([row], row.id));
    models = rows.flatMap((row) => modelsForPatch(row, row.id));
  }

  const defaults = patchDefaults(rows, singleProviderId, whole);
  defaults.perModelEffort = perModelEffortForPatch(rows);

  // 父级统一参数仅在模型×站点模式生效：同一模型跨站点参数一致，父级定义基准。
  // 常规模式各供应商/模型独立，无父级统一设置，直接沿用 Agent 级快照值。
  const headRow = rows[0];
  const headGroupKey = headRow ? `${headRow.channelId}::${headRow.account}` : "";
  const parentCfg = modelMode ? getParentConfig(headGroupKey) : null;

  if (parentCfg?.defaultReasoningEffort) {
    defaults.reasoningEffort = parentCfg.defaultReasoningEffort;
  }
  if (parentCfg?.efforts && parentCfg.efforts.length) {
    defaults.reasoningEffortOptions = parentCfg.efforts;
  }

  const context = {
    contextWindow: parentCfg?.contextWindow ?? snapshot.value?.context?.contextWindow ?? null,
    autoCompactTokenLimit: snapshot.value?.context?.autoCompactTokenLimit ?? null,
    maxOutputTokens: parentCfg?.maxOutput ?? snapshot.value?.context?.maxOutputTokens ?? null,
    maxThinkingTokens: snapshot.value?.context?.maxThinkingTokens ?? null,
  };

  const thinking = {
    effortLevel: parentCfg?.defaultReasoningEffort || snapshot.value?.thinking?.effortLevel || "",
    effortLevelOptions: parentCfg?.efforts && parentCfg.efforts.length ? parentCfg.efforts : (snapshot.value?.thinking?.effortLevelOptions ?? [...THINKING_EFFORT_OPTIONS]),
    maxThinkingTokens: snapshot.value?.thinking?.maxThinkingTokens ?? null,
  };

  return {
    baseHash: snapshot.value?.contentHash ?? "",
    providers,
    models,
    defaults,
    context,
    thinking,
  };
}

/** 整单比对的目标键：一次写入覆盖整份清单的场景共用它。 */
const WHOLE_LIST_KEY = "__all__";

/**
 * 该工具的清单是否只能「整单」比对。
 *
 * - 模型×站点：整份清单合并为一条供应商，写入天然是整单操作；
 * - 一次全部：行内不提供单条「生效」（反代清单本就是一次性全量加载），
 *   因此磁盘上必然同时存在多行条目，逐行比对会把别行当成「多出的供应商」。
 *
 * 其余情形（一路接入 / 一次一家）逐行写入，按行比对才有意义。
 */
const wholeListComparison = computed(() => modelIdentityMode.value || allEndpoint.value);

function diffKeyForRow(row: ProxyInventoryRow) {
  return wholeListComparison.value ? WHOLE_LIST_KEY : row.id;
}

async function refreshDiff() {
  if (!snapshot.value) return;
  const usable = usableRows.value;
  if (!usable.length) return;
  if (wholeListComparison.value) {
    await diffTargets([{ key: WHOLE_LIST_KEY, patch: buildPatch(usable, true) }]);
    return;
  }
  await diffTargets(usable.map((row) => ({ key: row.id, patch: buildPatch([row], false) })));
}

let diffDebounceTimer: number | null = null;
function scheduleDiff() {
  if (diffDebounceTimer !== null) clearTimeout(diffDebounceTimer);
  diffDebounceTimer = window.setTimeout(() => {
    diffDebounceTimer = null;
    void refreshDiff();
  }, 200);
}

// 配置、清单或身份模式变化后重新比对：徽标始终反映「磁盘现状 vs 当前清单」。
// 这些依赖变化很频繁（站点缓存刷新也会触发），比对失败静默处理，不打扰用户。
watch(
  [configRevision, () => usableRows.value, identityMode],
  () => {
    scheduleDiff();
  },
  { immediate: true, flush: "post" },
);

async function applyRows(rows: ProxyInventoryRow[], whole: boolean) {
  if (!snapshot.value || saving.value || snapshotLoading.value) return;
  const blocked = applyBlockedReason(rows);
  if (blocked) {
    showToast(blocked, true);
    return;
  }
  if (!gatewayReady.value) {
    showToast("网关未在运行，仍会写入当前端口地址，启动后再用");
  }
  const ok = await saveSnapshot(buildPatch(rows, whole));
  if (ok) {
    await loadToolList();
    await refreshDiff();
  }
}

async function applyRow(row: ProxyInventoryRow) {
  if (allEndpoint.value || modelIdentityMode.value) return;
  if (!rowHasKey(row)) {
    showToast("这条还没有 Key，请先到站点库同步后再生效", true);
    return;
  }
  await applyRows([row], false);
}

async function applyAll() {
  // 该模式按单条生效（按钮此时为禁用态，这里再兜一道，避免键盘触发）。
  if (!canApplyAll.value) {
    showToast("该模式按单条生效，请点清单里某一行的「生效」", true);
    return;
  }
  const missing = inventoryRows.value.length - usableRows.value.length;
  if (!usableRows.value.length) {
    showToast(inventoryRows.value.length ? "清单里的条目都还没有 Key，请先同步" : "还没有反代渠道", true);
    return;
  }
  if (missing > 0) {
    showToast(`将跳过 ${missing} 条没有 Key 的条目`);
  }
  await applyRows(usableRows.value, true);
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

// —— 账号别名管理(按站点列全部浏览器账号) ——
const aliasModalOpen = ref(false);
const aliasDrafts = reactive<Record<string, string>>({});

/** 别名管理弹窗:按"账号身份"去重后的全部账号(所有站点共用一份别名);附各账号出现的站点名。 */
const aliasSites = computed(() => {
  const accounts: Array<{ key: string; defaultLabel: string; profileId: string; sites: string[] }> = [];
  const byKey = new Map<string, { key: string; defaultLabel: string; profileId: string; sites: string[] }>();
  for (const channel of proxyConfig.value?.channels ?? []) {
    if (!channel.siteId) continue;
    const cache = siteCaches.value[channel.siteId];
    if (!cache?.accounts?.length) continue;
    const siteName = channel.name || channelAlias(channel) || channel.id;
    for (const account of cache.accounts) {
      const key = accountAliasKey(account);
      let entry = byKey.get(key);
      if (!entry) {
        entry = {
          key,
          defaultLabel: accountLabel(account),
          profileId: account.profileId || account.accountName || account.username || "",
          sites: [],
        };
        byKey.set(key, entry);
        accounts.push(entry);
      }
      if (!entry.sites.includes(siteName)) entry.sites.push(siteName);
    }
  }
  return accounts;
});

function openAliasModal() {
  for (const item of aliasSites.value) {
    aliasDrafts[item.key] = preferences.accountAliases[item.key]?.trim() ?? "";
  }
  aliasModalOpen.value = true;
  document.body.classList.add("modal-open");
}

function closeAliasModal() {
  aliasModalOpen.value = false;
  document.body.classList.remove("modal-open");
}

function saveAccountAliases() {
  const next: Record<string, string> = { ...preferences.accountAliases };
  for (const item of aliasSites.value) {
    const value = aliasDrafts[item.key]?.trim() ?? "";
    if (value) next[item.key] = value;
    else delete next[item.key];
  }
  updatePreferences({ accountAliases: next });
  closeAliasModal();
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

const modalOpen = computed(() => toolSettingsModalOpen.value || backupModalOpen.value || aliasModalOpen.value);
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
  if (diffDebounceTimer !== null) {
    clearTimeout(diffDebounceTimer);
    diffDebounceTimer = null;
  }
  if (modalOpen.value) document.body.classList.remove("modal-open");
});
</script>

<template>
  <div class="local-tools-page">
    <header class="lt-cockpit-bar">
      <div class="lt-cockpit-row">
        <div class="lt-cockpit-left">
          <div class="lt-brand-section">
            <div class="lt-eyebrow-row">
              <span class="lt-live-dot" />
              <span class="lt-eyebrow-text">OpenHub · 本地 Agent 接入控制台</span>
            </div>
            <div class="lt-title-row">
              <h1>Agent 配置</h1>
            </div>
            <p class="lt-cockpit-subtitle">管理本机 AI 编程 Agent 的供应商接入 · 反代清单一键生效 · 配置备份还原</p>
          </div>
          <CustomSelect
            class="lt-tool-select"
            :options="toolOptions"
            :model-value="activeTool"
            aria-label="选择 Agent"
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
            type="button"
            class="lt-btn-primary"
            :disabled="!canApplyAll || saving || snapshotLoading || !usableRows.length"
            :title="canApplyAll
              ? '把当前清单里能用的反代条目写入 Agent 配置'
              : '该模式按单条生效，请点清单里某一行的「生效」'"
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
          <button type="button" class="lt-btn-secondary" title="按浏览器账号设置别名" @click="openAliasModal">
            <span v-html="icons.user"></span>
            <span>账号别名</span>
          </button>
          <button type="button" class="lt-btn-secondary" title="Agent 级默认项与上下文" @click="openToolSettings">
            <span v-html="icons.sliders"></span>
            <span>Agent 设置</span>
          </button>
        </div>
      </div>

      <div v-if="activeTool" class="lt-cockpit-sub">
        <div class="lt-mode-switch" role="group" aria-label="身份模式">
          <button
            type="button"
            class="lt-mode-option"
            :class="{ active: !modelIdentityMode }"
            title="每个「渠道·账号·Key」一条供应商条目"
            @click="setIdentityMode('channel')"
          >常规</button>
          <button
            type="button"
            class="lt-mode-option"
            :class="{ active: modelIdentityMode }"
            title="整份清单合并为一条网关供应商，模型 ID 带渠道别名定向到站点"
            @click="setIdentityMode('model')"
          >模型×站点</button>
        </div>
        <span class="lt-mode-caption">{{ identityModeCaption }}</span>
      </div>
    </header>

    <div class="local-tools-layout">
      <div v-if="!activeTool" class="empty-state">
        <div v-html="icons.sparkles"></div>
        <h2>{{ toolListLoading ? "扫描本机 Agent…" : "暂无可用 Agent" }}</h2>
        <p v-if="!toolListLoading">未检测到支持结构化配置的 Agent。</p>
        <button v-if="!toolListLoading" class="secondary-button" type="button" @click="loadToolList">重新扫描</button>
      </div>

      <section v-else class="provider-section">
        <header class="section-head">
          <div class="section-copy">
            <h4>
              反代清单
              <span class="section-count">
                {{ inventoryGroups.length }} {{ modelIdentityMode ? "个模型" : "个条目" }} · {{ inventoryRows.length }} 路 Key
              </span>
              <span class="provider-mode-badge" :title="providerModeHint">{{ providerModeLabel }}</span>
              <span class="provider-mode-badge identity" :title="modelIdentityMode ? '模型×站点模式：整份清单合并为一条网关接入' : '常规模式：按渠道 · 账号各成一条接入'">{{ modelIdentityMode ? "模型×站点" : "常规" }}</span>
              <span v-if="snapshot?.effectNote" class="provider-mode-badge effect">{{ snapshot.effectNote }}</span>
            </h4>
            <p v-if="usableRows.length && consistencySummary" class="section-note lt-consistency-minimal" :class="`is-${consistencySummary.tone ?? 'idle'}`">
              <span>{{ consistencySummary.text }}</span>
              <span v-if="diffLoading" class="lt-consistency-loading">（比对中…）</span>
              <span v-if="consistencySummary.detail" class="lt-consistency-detail" :title="consistencySummary.detail">
                · {{ consistencySummary.detail }}
              </span>
              <span v-if="managedFiles.length" class="lt-file-tag" :title="managedFiles.map(f => `${f.path} (${managedStateLabel(f)})`).join('\n')">
                · {{ managedFiles.map(f => `${f.label}（${managedStateLabel(f)}）`).join('，') }}
              </span>
            </p>
          </div>
        </header>

        <p v-if="snapshot?.warning" class="snapshot-warning">{{ snapshot.warning }}</p>

        <div v-if="snapshotLoading || siteCachesLoading" class="detail-loading">读取配置中…</div>

        <template v-else-if="snapshot">
          <div v-if="inventoryGroups.length" class="provider-list">
            <article
              v-for="group in inventoryGroups"
              :key="group.key"
              class="provider-card"
              :class="{
                current: groupState(group) === 'applied',
                drifted: groupState(group) === 'drifted',
                expanded: isGroupExpanded(group.key),
              }"
              :title="groupDiffNote(group) || undefined"
            >
              <!-- 渠道/供应商卡片头部 -->
              <div class="provider-card-header">
                <div class="provider-card-left">
                  <button
                    type="button"
                    class="provider-toggle-btn"
                    :aria-expanded="isGroupExpanded(group.key)"
                    :title="isGroupExpanded(group.key) ? '收起子模型' : `展开 ${group.entries.length} 个子模型`"
                    @click="toggleGroup(group.key)"
                  >
                    <span class="lt-chevron" :class="{ open: isGroupExpanded(group.key) }" v-html="icons.chevron"></span>
                  </button>
                  <div class="provider-identity" @click="toggleGroup(group.key)">
                    <span class="provider-avatar">{{ group.title.charAt(0).toUpperCase() || "?" }}</span>
                    <div class="provider-identity-texts">
                      <div class="provider-title-line">
                        <strong :class="{ mono: modelIdentityMode }">{{ group.title }}</strong>
                        <span v-if="groupOverriddenCount(group) > 0" class="sublist-override-pill" title="该渠道下有子模型自定义覆盖了配置">
                          {{ groupOverriddenCount(group) }} 个模型已覆盖
                        </span>
                      </div>
                      <span class="provider-meta">{{ group.subtitle }}</span>
                      <span v-if="groupDiffNote(group)" class="provider-diff">{{ groupDiffNote(group) }}</span>
                    </div>
                  </div>
                </div>

                <div class="provider-card-actions">
                  <span class="provider-state-tag" :class="groupState(group)">
                    {{ groupState(group) === 'applied' ? '已生效' : groupState(group) === 'drifted' ? '配置漂移' : '未生效' }}
                  </span>
                  <button
                    v-if="groupCanApply(group)"
                    class="use-button"
                    :class="{ active: groupState(group) === 'applied' }"
                    type="button"
                    :disabled="saving || groupState(group) === 'applied'"
                    :title="groupState(group) === 'drifted' ? '重新写入该条' : '写入该条并生效'"
                    @click="applyGroup(group)"
                  >
                    <span v-html="icons.check"></span>
                    <span>{{ groupState(group) === "drifted" ? "重新写入" : groupState(group) === "applied" ? "已生效" : "生效" }}</span>
                  </button>
                </div>
              </div>

              <!-- 父列表配置栏（仅模型×站点模式：同模型跨站点参数一致，父级统一定义基准配置） -->
              <div v-if="modelIdentityMode" class="parent-config-bar">
                <div class="parent-config-title">
                  <span class="parent-config-badge">父级默认配置</span>
                  <span class="parent-config-hint">下属 {{ group.entries.length }} 个站点默认继承此配置；子列表中可单独覆盖</span>
                </div>

                <div class="parent-config-grid">
                  <!-- 最大窗口 -->
                  <div class="cfg-field">
                    <div class="cfg-label-row">
                      <label>最大窗口</label>
                      <div class="cfg-presets">
                        <button
                          v-for="p in CONTEXT_PRESETS"
                          :key="p.label"
                          type="button"
                          class="preset-chip"
                          :class="{ active: getParentConfig(group.key).contextWindow === p.value }"
                          @click="applyContextPresetToParent(group.key, p.value)"
                        >{{ p.label }}</button>
                      </div>
                    </div>
                    <input
                      class="cfg-input"
                      type="number"
                      min="0"
                      placeholder="默认未设 (如 128000)"
                      :value="getParentConfig(group.key).contextWindow ?? ''"
                      @input="setParentConfig(group.key, { contextWindow: parseLimitInput($event) })"
                    />
                  </div>

                  <!-- 最大输出 -->
                  <div class="cfg-field">
                    <div class="cfg-label-row">
                      <label>最大输出</label>
                      <div class="cfg-presets">
                        <button
                          v-for="p in OUTPUT_PRESETS"
                          :key="p.label"
                          type="button"
                          class="preset-chip"
                          :class="{ active: getParentConfig(group.key).maxOutput === p.value }"
                          @click="applyOutputPresetToParent(group.key, p.value)"
                        >{{ p.label }}</button>
                      </div>
                    </div>
                    <input
                      class="cfg-input"
                      type="number"
                      min="0"
                      placeholder="默认未设 (如 8192)"
                      :value="getParentConfig(group.key).maxOutput ?? ''"
                      @input="setParentConfig(group.key, { maxOutput: parseLimitInput($event) })"
                    />
                  </div>

                  <!-- 默认思考级别 -->
                  <div class="cfg-field">
                    <div class="cfg-label-row">
                      <label>默认思考级别</label>
                      <span v-if="getParentConfig(group.key).defaultReasoningEffort" class="cfg-current-val">
                        {{ getParentConfig(group.key).defaultReasoningEffort }}
                      </span>
                    </div>
                    <select
                      class="cfg-select"
                      :value="getParentConfig(group.key).defaultReasoningEffort"
                      @change="setParentConfig(group.key, { defaultReasoningEffort: ($event.target as HTMLSelectElement).value })"
                    >
                      <option value="">未设置 / 默认</option>
                      <option v-for="lvl in THINKING_EFFORT_OPTIONS" :key="lvl" :value="lvl">{{ lvl }}</option>
                    </select>
                  </div>

                  <!-- 思考级别筛选配置 -->
                  <div class="cfg-field cfg-field-wide">
                    <div class="cfg-label-row">
                      <label>思考级别筛选配置</label>
                      <div class="cfg-effort-actions">
                        <button type="button" class="cfg-text-btn" @click="selectAllParentEfforts(group.key)">全选</button>
                        <button type="button" class="cfg-text-btn" @click="clearParentEfforts(group.key)">清空</button>
                      </div>
                    </div>
                    <div class="effort-chips-row">
                      <button
                        v-for="lvl in THINKING_EFFORT_OPTIONS"
                        :key="`parent-${group.key}-${lvl}`"
                        type="button"
                        class="effort-chip"
                        :class="{ active: (getParentConfig(group.key).efforts ?? []).includes(lvl) }"
                        @click="toggleParentEffort(group.key, lvl)"
                      >{{ lvl }}</button>
                    </div>
                  </div>
                </div>
              </div>

              <!-- 展开区：子模型列表（支持子列表覆盖父列表） -->
              <div v-if="isGroupExpanded(group.key)" class="sublist-section">
                <div class="sublist-header">
                  <div class="sublist-header-left">
                    <span class="sublist-title">{{ group.detailLabel }}</span>
                    <span class="sublist-count-badge">{{ filteredEntries(group).length }} / {{ group.entries.length }}</span>
                  </div>
                  <div class="sublist-header-center">
                    <input
                      v-model="sublistSearch[group.key]"
                      class="sublist-search-input"
                      type="text"
                      placeholder="搜索模型名称或 ID…"
                    />
                  </div>
                  <div class="sublist-header-right">
                    <button
                      v-if="modelIdentityMode && groupOverriddenCount(group) > 0"
                      type="button"
                      class="sublist-reset-all-btn"
                      title="清空当前所有子模型的覆盖参数，全部恢复继承父级配置"
                      @click="resetAllChildren(group)"
                    >
                      全部重置为继承
                    </button>
                  </div>
                </div>

                <div class="sublist-table">
                  <div class="sublist-table-header">
                    <div class="col-model">模型名称 / ID</div>
                    <div class="col-window">最大窗口</div>
                    <div class="col-output">最大输出</div>
                    <div class="col-effort">默认思考级别</div>
                    <div class="col-filter">思考级别筛选</div>
                    <div class="col-action">操作</div>
                  </div>

                  <div
                    v-for="entry in filteredEntries(group)"
                    :key="entry.key"
                    class="sublist-table-row"
                    :class="{ 'is-overridden': getEffectiveModelParams(group.key, entry.key, entry.modelId).isOverridden }"
                  >
                    <!-- 模型信息 -->
                    <div class="col-model">
                      <div class="model-name-row">
                        <strong class="model-display-name" :title="entry.label">{{ entry.label }}</strong>
                        <span
                          v-if="getEffectiveModelParams(group.key, entry.key, entry.modelId).isOverridden"
                          class="override-tag"
                          title="已自定义覆盖父级配置"
                        >
                          已覆盖 {{ getEffectiveModelParams(group.key, entry.key, entry.modelId).overriddenCount }} 项
                        </span>
                        <span v-else-if="modelIdentityMode" class="inherit-tag" title="当前完全继承父级配置">继承父级</span>
                      </div>
                      <div class="model-id-row">
                        <code class="model-id-code" :title="entry.modelId || entry.label">{{ entry.modelId || entry.label }}</code>
                        <button
                          type="button"
                          class="copy-id-btn"
                          title="复制完整模型 ID"
                          @click="copyModelId(entry.modelId || entry.label)"
                          v-html="icons.copy"
                        ></button>
                      </div>
                      <div v-if="capabilitiesFor(entry)" class="model-cap-row">
                        <span
                          v-for="badge in capabilityBadges(capabilitiesFor(entry)!)"
                          :key="badge"
                          class="model-cap-badge"
                          title="来自 models.dev 目录"
                        >{{ badge }}</span>
                        <span v-if="modelReasoningToggleOnly(entry)" class="model-cap-badge" title="models.dev 仅声明开/关式思考">思考仅开/关</span>
                        <button
                          type="button"
                          class="model-cap-load-btn"
                          title="把 models.dev 的窗口 / 输出上限 / 思考档位载入为本模型参数"
                          @click="loadCapabilitiesToChild(entry)"
                        >载入目录参数</button>
                      </div>
                    </div>

                    <!-- 最大窗口 -->
                    <div class="col-window">
                      <div class="sub-input-wrap">
                        <input
                          class="sub-input"
                          :class="{ 'is-active-override': getEffectiveModelParams(group.key, entry.key, entry.modelId).hasContextWindowOverride }"
                          type="number"
                          min="0"
                          :placeholder="modelIdentityMode ? '继承: ' + (formatTokenCount(getParentConfig(group.key).contextWindow) || '未设') : '未设 (如 128000)'"
                          :value="getChildOverride(entry.key).contextWindow ?? ''"
                          @input="setChildOverride(entry, { contextWindow: parseLimitInput($event) ?? undefined })"
                        />
                      </div>
                    </div>

                    <!-- 最大输出 -->
                    <div class="col-output">
                      <div class="sub-input-wrap">
                        <input
                          class="sub-input"
                          :class="{ 'is-active-override': getEffectiveModelParams(group.key, entry.key, entry.modelId).hasMaxOutputOverride }"
                          type="number"
                          min="0"
                          :placeholder="modelIdentityMode ? '继承: ' + (formatTokenCount(getParentConfig(group.key).maxOutput) || '未设') : '未设 (如 8192)'"
                          :value="getChildOverride(entry.key).maxOutput ?? ''"
                          @input="setChildOverride(entry, { maxOutput: parseLimitInput($event) ?? undefined })"
                        />
                      </div>
                    </div>

                    <!-- 默认思考级别 -->
                    <div class="col-effort">
                      <select
                        class="sub-select"
                        :class="{ 'is-active-override': getEffectiveModelParams(group.key, entry.key, entry.modelId).hasEffortOverride }"
                        :value="getChildOverride(entry.key).defaultReasoningEffort ?? ''"
                        @change="setChildOverride(entry, { defaultReasoningEffort: ($event.target as HTMLSelectElement).value || undefined })"
                      >
                        <option value="">{{ modelIdentityMode ? '继承父级 (' + (getParentConfig(group.key).defaultReasoningEffort || '未设置') + ')' : '未设置 / 默认' }}</option>
                        <option
                          v-for="lvl in THINKING_EFFORT_OPTIONS"
                          :key="lvl"
                          :value="lvl"
                        >{{ lvl }}{{ childEffortUndeclared(entry, lvl) ? '（未声明）' : '' }}</option>
                      </select>
                    </div>

                    <!-- 思考级别筛选 -->
                    <div class="col-filter">
                      <div class="sub-effort-chips">
                        <button
                          v-for="lvl in THINKING_EFFORT_OPTIONS"
                          :key="`${entry.key}-${lvl}`"
                          type="button"
                          class="sub-effort-chip"
                          :class="{
                            'active': (getEffectiveModelParams(group.key, entry.key, entry.modelId).efforts ?? []).includes(lvl),
                            'is-custom': getEffectiveModelParams(group.key, entry.key, entry.modelId).hasEffortsListOverride,
                            'is-undeclared': childEffortUndeclared(entry, lvl),
                          }"
                          :title="childEffortChipTitle(group.key, entry, lvl)"
                          @click="toggleChildEffort(group.key, entry, lvl)"
                        >{{ lvl }}</button>
                        <button
                          v-if="getEffectiveModelParams(group.key, entry.key, entry.modelId).hasEffortsListOverride"
                          type="button"
                          class="sub-effort-reset-btn"
                          :title="modelIdentityMode ? '恢复继承父级思考级别筛选' : '清除自定义思考级别筛选'"
                          @click="setChildEffortsInherited(entry)"
                        >
                          {{ modelIdentityMode ? '恢复继承' : '清除自定义' }}
                        </button>
                      </div>
                    </div>

                    <!-- 操作 -->
                    <div class="col-action">
                      <button
                        v-if="getEffectiveModelParams(group.key, entry.key, entry.modelId).isOverridden"
                        type="button"
                        class="row-reset-btn"
                        :title="modelIdentityMode ? '重置所有项为继承父级' : '清除当前模型的自定义参数'"
                        @click="resetChildOverride(entry)"
                      >
                        {{ modelIdentityMode ? '重置为继承' : '清除自定义' }}
                      </button>
                    </div>
                  </div>

                  <div v-if="!filteredEntries(group).length" class="sublist-empty">
                    <span>未搜索到匹配的模型</span>
                  </div>
                </div>

                <div v-if="group.footnote.length" class="sublist-footnote">
                  <span v-for="note in group.footnote" :key="note">{{ note }}</span>
                </div>
              </div>
            </article>
          </div>
          <div v-else class="provider-empty">
            <div class="provider-empty-icon" v-html="icons.monitor"></div>
            <strong>还没有反代条目</strong>
            <span>先在模型反代里接入站点或手动渠道并勾选「管理可用模型」；未勾选任何模型的 Key 不会在这里展示。</span>
          </div>
        </template>
      </section>
    </div>

    <Teleport to="body">
      <div v-if="aliasModalOpen" class="modal-backdrop lt-modal-backdrop" @click.self="closeAliasModal">
        <section class="mini-modal alias-modal" role="dialog" aria-modal="true" tabindex="-1" @keydown="handleDialogKeydown($event, closeAliasModal)">
          <header class="modal-header">
            <div>
              <h2>账号别名</h2>
              <p>所有站点共用 · 用于供应商标识显示</p>
            </div>
            <button class="close-button" type="button" aria-label="关闭账号别名" @click="closeAliasModal" v-html="icons.close"></button>
          </header>
          <div class="mini-modal-body">
            <div v-if="!aliasSites.length" class="alias-empty">还没有已同步账号的站点渠道。</div>
            <label v-for="item in aliasSites" :key="item.key" class="alias-row">
              <span class="alias-default" :title="item.profileId">
                {{ item.defaultLabel }}
                <small v-if="item.sites.length" class="alias-sites">{{ item.sites.join(" / ") }}</small>
              </span>
              <input
                v-model="aliasDrafts[item.key]"
                placeholder="别名(留空恢复默认)"
                maxlength="48"
              />
            </label>
            <p class="alias-hint">别名全站点共用，用于反代清单与写入 Agent 配置的供应商标识，仅在本机生效。</p>
          </div>
          <footer class="modal-footer">
            <button class="secondary-button" type="button" @click="closeAliasModal">取消</button>
            <button class="primary-button" type="button" @click="saveAccountAliases">保存</button>
          </footer>
        </section>
      </div>
      <div v-if="toolSettingsModalOpen" class="modal-backdrop lt-modal-backdrop" @click.self="closeToolSettings">
        <section class="mini-modal settings-modal" role="dialog" aria-modal="true" tabindex="-1" @keydown="handleDialogKeydown($event, closeToolSettings)">
          <header class="modal-header">
            <div>
              <h2>Agent 设置</h2>
              <p>{{ activeOverview?.toolName ?? activeTool }}</p>
            </div>
            <button class="close-button" type="button" aria-label="关闭 Agent 设置" @click="closeToolSettings" v-html="icons.close"></button>
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
  flex-direction: column;
  gap: 10px;
  padding: 12px 20px;
  background: var(--surface);
  border-bottom: 1px solid var(--line);
  flex-shrink: 0;
}

/* 第一行：品牌 + 工具选择 + 状态 ｜ 右侧动作按钮。
   默认不换行，按钮组位置只由窗口宽度决定；品牌区可收缩、副标题省略号收尾，
   避免内容宽度变化（如切换模式后按钮可用态改变）把按钮组挤到下一行造成上下跳动。 */
.lt-cockpit-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  flex-wrap: nowrap;
}

/* 第二行：身份模式切换（独占一行，不再与状态徽标抢位）。
   说明文字单行省略：两种模式文案长度不同，换行数一旦不一致会让整块头高低跳。 */
.lt-cockpit-sub {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
}

.lt-mode-caption {
  color: var(--muted);
  font-size: 11.5px;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.lt-mode-switch { flex-shrink: 0; }

.lt-cockpit-left {
  display: flex;
  align-items: center;
  gap: 16px;
  min-width: 0;
  flex: 1 1 auto;
}

/* 品牌区：eyebrow 行 + 标题行 + 副标题，与其他页面驾驶舱对齐。
   可收缩（min-width: 0 + 副标题省略号），窗口变窄时先挤这里，右侧按钮组保持整块不换行。 */
.lt-brand-section {
  display: flex;
  flex-direction: column;
  gap: 1px;
  min-width: 0;
  flex: 1 1 auto;
}

.lt-eyebrow-row {
  display: flex;
  align-items: center;
  gap: 6px;
}

.lt-live-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--success, #2ea043);
  box-shadow: 0 0 8px var(--success, #2ea043);
  animation: ltPulse 2s infinite ease-in-out;
}

@keyframes ltPulse {
  0%, 100% { opacity: 1; transform: scale(1); }
  50% { opacity: 0.4; transform: scale(1.25); }
}

.lt-eyebrow-text {
  font-size: 10px;
  font-weight: 750;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--brand);
}

.lt-title-row {
  display: flex;
  align-items: center;
  gap: 10px;
}

.lt-title-row h1 {
  font-size: 18px;
  font-weight: 750;
  color: var(--text);
  margin: 0;
  line-height: 1.2;
  white-space: nowrap;
}

.lt-cockpit-subtitle {
  font-size: 11px;
  color: var(--muted);
  margin: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.lt-tool-select {
  width: min(240px, 42vw);
  flex-shrink: 0;
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
  /* 不参与压缩、不折行：品牌区可收缩，状态徽标保持单行 pill */
  flex-shrink: 0;
  white-space: nowrap;
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
  /* 文案在「生效 / 写入中… / 已全部生效」间切换，定宽避免按钮组宽度抖动 */
  min-width: 104px;
  justify-content: center;
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
.provider-mode-badge.identity {
  border-color: color-mix(in srgb, var(--brand) 45%, var(--line));
  color: var(--brand);
}
.provider-mode-badge.effect {
  color: var(--muted);
  border-style: dashed;
  font-weight: 500;
}

/* 身份模式切换：常规（渠道·账号） / 模型×站点 */
.lt-mode-switch {
  display: inline-flex;
  align-items: center;
  height: 30px;
  padding: 2px;
  border: 1px solid var(--line);
  border-radius: 999px;
  background: var(--surface-soft, var(--surface));
  flex-shrink: 0;
}
.lt-mode-option {
  height: 24px;
  padding: 0 12px;
  border: 0;
  border-radius: 999px;
  background: transparent;
  color: var(--muted);
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  white-space: nowrap;
  transition: background 0.15s ease, color 0.15s ease;
}
.lt-mode-option:hover { color: var(--text); }
.lt-mode-option.active {
  background: var(--brand);
  color: #fff;
}
.section-note {
  margin: 4px 0 0;
  color: var(--muted);
  font-size: 12px;
}

.lt-consistency-minimal {
  margin: 6px 0 0;
  font-size: 11.5px;
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 4px 6px;
}
.lt-consistency-minimal.is-ok { color: var(--success, #2e9e5b); }
.lt-consistency-minimal.is-warn { color: var(--warning, #c98a1a); }
.lt-consistency-minimal .lt-consistency-detail {
  color: var(--muted);
  max-width: 520px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.lt-consistency-minimal .lt-file-tag {
  color: var(--muted);
  font-size: 11px;
}

.lt-file-states { display: flex; flex-wrap: wrap; gap: 6px; }
.lt-file-state {
  font-size: 11.5px;
  padding: 1px 8px;
  border-radius: 999px;
  border: 1px solid var(--border);
  color: var(--muted);
}
.lt-file-state.is-intact { color: var(--success, #2e9e5b); border-color: currentColor; }
.lt-file-state.is-modified { color: var(--warning, #c98a1a); border-color: currentColor; }
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
  display: flex;
  flex-direction: column;
  gap: 12px;
  overflow: auto;
  min-height: 0;
  flex: 1;
  padding: 2px;
}

/* 渠道卡片 */
.provider-card {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 14px 16px;
  border: 1px solid var(--line);
  border-radius: 12px;
  background: var(--surface);
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.04);
  transition: border-color 0.18s ease, box-shadow 0.18s ease;
}

.provider-card:hover {
  border-color: color-mix(in srgb, var(--brand) 30%, var(--line));
  box-shadow: 0 3px 10px rgba(0, 0, 0, 0.06);
}

.provider-card.current {
  border-color: color-mix(in srgb, var(--brand) 50%, var(--line));
  background: color-mix(in srgb, var(--brand) 3%, var(--surface));
}

.provider-card.drifted {
  border-color: color-mix(in srgb, var(--warning, #c98a1a) 50%, var(--line));
  background: color-mix(in srgb, var(--warning, #c98a1a) 3%, var(--surface));
}

/* 头部 */
.provider-card-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  flex-wrap: wrap;
}

.provider-card-left {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
  flex: 1 1 auto;
}

.provider-toggle-btn {
  display: grid;
  place-items: center;
  width: 26px;
  height: 26px;
  padding: 0;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--surface-soft, var(--surface));
  color: var(--muted);
  cursor: pointer;
  flex: 0 0 auto;
  transition: all 0.15s ease;
}

.provider-toggle-btn:hover {
  background: var(--surface-hover);
  color: var(--text);
  border-color: var(--line-strong, var(--line));
}

.lt-chevron {
  display: inline-grid;
  place-items: center;
  transition: transform 0.2s cubic-bezier(0.4, 0, 0.2, 1);
}

.lt-chevron.open {
  transform: rotate(180deg);
}

.provider-identity {
  display: flex;
  align-items: center;
  gap: 12px;
  min-width: 0;
  cursor: pointer;
  flex: 1 1 auto;
}

.provider-identity-texts {
  display: flex;
  flex-direction: column;
  gap: 3px;
  min-width: 0;
}

.provider-title-line {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.provider-title-line strong {
  font-size: 14px;
  font-weight: 700;
  color: var(--text);
  line-height: 1.2;
}

.sublist-override-pill {
  font-size: 11px;
  font-weight: 650;
  color: var(--brand);
  background: color-mix(in srgb, var(--brand) 12%, transparent);
  border: 1px solid color-mix(in srgb, var(--brand) 30%, transparent);
  padding: 1px 7px;
  border-radius: 999px;
  white-space: nowrap;
}

.provider-card-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-shrink: 0;
}

.provider-state-tag {
  display: inline-flex;
  align-items: center;
  padding: 2px 8px;
  border-radius: 999px;
  font-size: 11px;
  font-weight: 650;
  border: 1px solid var(--line);
  color: var(--muted);
  background: var(--surface-soft, var(--surface));
}

.provider-state-tag.applied {
  color: var(--success, #2e9e5b);
  border-color: color-mix(in srgb, var(--success, #2e9e5b) 40%, var(--line));
  background: color-mix(in srgb, var(--success, #2e9e5b) 10%, transparent);
}

.provider-state-tag.drifted {
  color: var(--warning, #c98a1a);
  border-color: color-mix(in srgb, var(--warning, #c98a1a) 40%, var(--line));
  background: color-mix(in srgb, var(--warning, #c98a1a) 10%, transparent);
}

/* 父列表配置栏 */
.parent-config-bar {
  margin-top: 4px;
  padding: 12px 14px;
  border-radius: 10px;
  background: color-mix(in srgb, var(--surface-soft, var(--surface)) 45%, var(--page-bg));
  border: 1px solid color-mix(in srgb, var(--line) 60%, transparent);
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.parent-config-title {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.parent-config-badge {
  font-size: 10.5px;
  font-weight: 750;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  padding: 2px 7px;
  border-radius: 5px;
  background: color-mix(in srgb, var(--brand) 15%, transparent);
  color: var(--brand);
}

.parent-config-hint {
  font-size: 11.5px;
  color: var(--muted);
}

.parent-config-grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 12px 16px;
}

@media (max-width: 1080px) {
  .parent-config-grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}

@media (max-width: 680px) {
  .parent-config-grid {
    grid-template-columns: 1fr;
  }
}

.cfg-field {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.cfg-field-wide {
  grid-column: 1 / -1;
}

.cfg-label-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}

.cfg-label-row label {
  font-size: 12px;
  font-weight: 650;
  color: var(--text);
}

.cfg-presets {
  display: inline-flex;
  gap: 4px;
}

.preset-chip {
  padding: 1px 6px;
  font-size: 10.5px;
  font-weight: 600;
  border-radius: 4px;
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--muted);
  cursor: pointer;
  transition: all 0.12s ease;
}

.preset-chip:hover {
  color: var(--text);
  border-color: var(--line-strong, var(--line));
}

.preset-chip.active {
  color: var(--brand);
  border-color: var(--brand);
  background: color-mix(in srgb, var(--brand) 10%, transparent);
}

.cfg-current-val {
  font-size: 11px;
  font-weight: 600;
  color: var(--brand);
}

.cfg-input {
  height: 32px;
  padding: 0 10px;
  border: 1px solid var(--line);
  border-radius: 7px;
  background: var(--surface);
  color: var(--text);
  font-size: 12.5px;
  outline: none;
  transition: border-color 0.15s ease, box-shadow 0.15s ease;
}

.cfg-input:focus {
  border-color: var(--brand);
  box-shadow: 0 0 0 2px color-mix(in srgb, var(--brand) 20%, transparent);
}

.cfg-select {
  height: 32px;
  padding: 0 8px;
  border: 1px solid var(--line);
  border-radius: 7px;
  background: var(--surface);
  color: var(--text);
  font-size: 12px;
  outline: none;
  cursor: pointer;
  transition: border-color 0.15s ease;
}

.cfg-select:focus {
  border-color: var(--brand);
}

.cfg-effort-actions {
  display: inline-flex;
  gap: 6px;
}

.cfg-text-btn {
  font-size: 11px;
  color: var(--brand);
  background: transparent;
  border: 0;
  padding: 0 2px;
  cursor: pointer;
  text-decoration: underline;
  text-underline-offset: 2px;
}

.cfg-text-btn:hover {
  opacity: 0.8;
}

.effort-chips-row {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}

.effort-chip {
  padding: 4px 10px;
  font-size: 11.5px;
  font-weight: 600;
  border-radius: 7px;
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--muted);
  cursor: pointer;
  transition: all 0.15s ease;
}

.effort-chip:hover {
  border-color: var(--line-strong, var(--line));
  color: var(--text);
}

.effort-chip.active {
  background: color-mix(in srgb, var(--brand) 12%, transparent);
  color: var(--brand);
  border-color: color-mix(in srgb, var(--brand) 45%, var(--line));
}

/* 子列表展开区 */
.sublist-section {
  margin-top: 4px;
  border-top: 1px solid var(--line);
  padding-top: 12px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.sublist-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  flex-wrap: wrap;
}

.sublist-header-left {
  display: flex;
  align-items: center;
  gap: 8px;
}

.sublist-title {
  font-size: 13px;
  font-weight: 700;
  color: var(--text);
}

.sublist-count-badge {
  font-size: 11px;
  font-weight: 650;
  color: var(--muted);
  padding: 1px 7px;
  border-radius: 999px;
  background: var(--surface-soft, var(--surface));
  border: 1px solid var(--line);
}

.sublist-header-center {
  flex: 1 1 240px;
  max-width: 320px;
}

.sublist-search-input {
  width: 100%;
  height: 28px;
  padding: 0 10px;
  border: 1px solid var(--line);
  border-radius: 6px;
  background: var(--surface);
  color: var(--text);
  font-size: 11.5px;
  outline: none;
}

.sublist-search-input:focus {
  border-color: var(--brand);
}

.sublist-reset-all-btn {
  padding: 4px 10px;
  font-size: 11.5px;
  font-weight: 600;
  border-radius: 6px;
  border: 1px solid color-mix(in srgb, var(--brand) 40%, var(--line));
  background: color-mix(in srgb, var(--brand) 8%, transparent);
  color: var(--brand);
  cursor: pointer;
  transition: all 0.15s ease;
}

.sublist-reset-all-btn:hover {
  background: color-mix(in srgb, var(--brand) 16%, transparent);
}

/* 子模型表格 */
.sublist-table {
  border: 1px solid var(--line);
  border-radius: 8px;
  overflow: hidden;
  background: var(--surface);
  display: flex;
  flex-direction: column;
}

.sublist-table-header {
  display: grid;
  grid-template-columns: minmax(180px, 2fr) 110px 110px 140px minmax(180px, 2fr) 80px;
  gap: 8px;
  padding: 8px 12px;
  background: color-mix(in srgb, var(--surface-soft, var(--surface)) 60%, var(--page-bg));
  border-bottom: 1px solid var(--line);
  font-size: 11px;
  font-weight: 700;
  color: var(--muted);
  align-items: center;
}

.sublist-table-row {
  display: grid;
  grid-template-columns: minmax(180px, 2fr) 110px 110px 140px minmax(180px, 2fr) 80px;
  gap: 8px;
  padding: 8px 12px;
  border-bottom: 1px solid color-mix(in srgb, var(--line) 40%, transparent);
  align-items: center;
  transition: background 0.12s ease;
}

.sublist-table-row:last-child {
  border-bottom: 0;
}

.sublist-table-row:hover {
  background: color-mix(in srgb, var(--surface-hover) 70%, transparent);
}

.sublist-table-row.is-overridden {
  background: color-mix(in srgb, var(--brand) 2.5%, var(--surface));
}

@media (max-width: 960px) {
  .sublist-table-header {
    display: none;
  }
  .sublist-table-row {
    grid-template-columns: 1fr 1fr;
    gap: 10px;
    padding: 12px;
  }
  .col-model, .col-filter {
    grid-column: 1 / -1;
  }
}

/* 表格列 */
.col-model {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.model-name-row {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.model-display-name {
  font-size: 12.5px;
  font-weight: 650;
  color: var(--text);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.override-tag {
  font-size: 10px;
  font-weight: 700;
  padding: 1px 5px;
  border-radius: 4px;
  color: var(--brand);
  background: color-mix(in srgb, var(--brand) 12%, transparent);
  border: 1px solid color-mix(in srgb, var(--brand) 30%, transparent);
  white-space: nowrap;
}

.inherit-tag {
  font-size: 10px;
  padding: 1px 5px;
  border-radius: 4px;
  color: var(--muted);
  background: color-mix(in srgb, var(--muted) 12%, transparent);
  white-space: nowrap;
}

.model-id-row {
  display: flex;
  align-items: center;
  gap: 4px;
  min-width: 0;
}

.model-id-code {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 11px;
  color: var(--muted);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.copy-id-btn {
  display: inline-grid;
  place-items: center;
  width: 18px;
  height: 18px;
  padding: 0;
  border: 0;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  opacity: 0.7;
  flex-shrink: 0;
}

.copy-id-btn:hover {
  opacity: 1;
  color: var(--brand);
}

.copy-id-btn :deep(svg) {
  width: 12px;
  height: 12px;
}

.model-cap-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 4px;
  margin-top: 3px;
}

.model-cap-badge {
  display: inline-block;
  padding: 1px 6px;
  border: 1px solid var(--line);
  border-radius: 999px;
  font-size: 10px;
  line-height: 1.5;
  color: var(--muted);
  white-space: nowrap;
}

.model-cap-load-btn {
  padding: 1px 6px;
  border: 1px dashed var(--brand);
  border-radius: 999px;
  background: transparent;
  font-size: 10px;
  line-height: 1.5;
  color: var(--brand);
  cursor: pointer;
  white-space: nowrap;
}

.model-cap-load-btn:hover {
  background: color-mix(in srgb, var(--brand) 12%, transparent);
}

.sub-input-wrap {
  width: 100%;
}

.sub-input {
  width: 100%;
  height: 28px;
  padding: 0 6px;
  border: 1px solid var(--line);
  border-radius: 6px;
  background: var(--surface);
  color: var(--text);
  font-size: 11.5px;
  outline: none;
  transition: all 0.15s ease;
}

.sub-input:focus {
  border-color: var(--brand);
}

.sub-input.is-active-override {
  border-color: color-mix(in srgb, var(--brand) 70%, var(--line));
  background: color-mix(in srgb, var(--brand) 4%, var(--surface));
  font-weight: 600;
  color: var(--brand);
}

.sub-select {
  width: 100%;
  height: 28px;
  padding: 0 6px;
  border: 1px solid var(--line);
  border-radius: 6px;
  background: var(--surface);
  color: var(--text);
  font-size: 11px;
  outline: none;
  cursor: pointer;
}

.sub-select.is-active-override {
  border-color: color-mix(in srgb, var(--brand) 70%, var(--line));
  background: color-mix(in srgb, var(--brand) 4%, var(--surface));
  color: var(--brand);
  font-weight: 600;
}

.sub-effort-chips {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 3px;
}

.sub-effort-chip {
  padding: 2px 6px;
  font-size: 10px;
  font-weight: 600;
  border-radius: 5px;
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--muted);
  cursor: pointer;
  transition: all 0.12s ease;
}

.sub-effort-chip:hover {
  border-color: var(--line-strong, var(--line));
  color: var(--text);
}

.sub-effort-chip.active {
  background: color-mix(in srgb, var(--muted) 15%, transparent);
  color: var(--text);
  border-color: var(--line-strong, var(--line));
}

.sub-effort-chip.active.is-custom {
  background: color-mix(in srgb, var(--brand) 15%, transparent);
  color: var(--brand);
  border-color: color-mix(in srgb, var(--brand) 50%, var(--line));
}
/* 目录未声明该档：虚线弱化，不隐藏 —— 档位仍可勾选（站点可自行支持更多档） */
.sub-effort-chip.is-undeclared {
  border-style: dashed;
  opacity: 0.62;
}
.sub-effort-chip.is-undeclared.active {
  opacity: 1;
}

.sub-effort-reset-btn {
  font-size: 10px;
  color: var(--muted);
  background: transparent;
  border: 0;
  text-decoration: underline;
  cursor: pointer;
  padding: 0 3px;
}

.sub-effort-reset-btn:hover {
  color: var(--brand);
}

.row-reset-btn {
  font-size: 11px;
  color: var(--muted);
  background: transparent;
  border: 1px solid var(--line);
  border-radius: 5px;
  padding: 3px 6px;
  cursor: pointer;
  white-space: nowrap;
  transition: all 0.12s ease;
}

.row-reset-btn:hover {
  color: var(--brand);
  border-color: var(--brand);
  background: color-mix(in srgb, var(--brand) 8%, transparent);
}

.sublist-empty {
  padding: 20px;
  text-align: center;
  color: var(--muted);
  font-size: 12px;
}

.sublist-footnote {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
  color: var(--muted);
  font-size: 11px;
  padding-top: 6px;
}
.provider-meta {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--muted);
  font-size: 12px;
}
/* 与磁盘配置不一致的具体原因（后端语义比对给出的首条差异） */
.provider-diff {
  color: var(--warning, #c98a1a);
  font-size: 11.5px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.lt-consistency .lt-consistency-loading { color: var(--muted); }
.lt-consistency.is-ok { color: var(--success, #2e9e5b); }
.lt-consistency.is-warn { color: var(--warning, #c98a1a); }
.lt-consistency-detail {
  display: block;
  margin-top: 2px;
  color: var(--muted);
  font-size: 11.5px;
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

/* 账号别名管理弹窗 */
.alias-modal { width: min(520px, calc(100vw - 48px)); }
.alias-hint { margin: 0; font-size: 11.5px; color: var(--muted); }
.alias-empty { font-size: 12.5px; color: var(--muted); text-align: center; padding: 12px 0; }
.alias-row { display: flex; align-items: center; gap: 10px; }
.alias-default { flex: 0 0 40%; min-width: 0; font-size: 12.5px; color: var(--muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.alias-sites { display: block; font-size: 10.5px; opacity: 0.75; }
.alias-row input {
  flex: 1;
  min-width: 0;
  height: 34px;
  padding: 0 10px;
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  outline: 0;
  background: var(--surface);
  color: var(--text);
  font-size: 12.5px;
}

@media (max-width: 980px) {
  /* 窄屏才允许换行：按钮组独占一行（此宽度下品牌区已被挤到上面，切换模式不会再跳动） */
  .lt-cockpit-row { flex-wrap: wrap; }
  .lt-cockpit-right { width: 100%; justify-content: flex-start; flex-wrap: wrap; }
  .provider-row {
    align-items: flex-start;
    padding: 10px 14px;
    gap: 8px 10px;
  }
  .provider-row-actions { flex-wrap: wrap; justify-content: flex-end; }
  .provider-detail { margin-left: 0; }
  .form-grid.narrow { grid-template-columns: 1fr; }
}
</style>
