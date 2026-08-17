import { computed, ref, watch } from "vue";
import { runCommand, useLibrary } from "./useLibrary";
import { usePreferences } from "./usePreferences";
import { useModelCatalog } from "./useModelCatalog";
import { useTokenStats } from "./useTokenStats";
import type {
  GatewayApiKeyItem,
  GatewayStatus,
  ModelAggGroupMode,
  ModelCatalogItem,
  SiteModelCacheEntry,
} from "../types";

/** 单个 Key 在聚合视图中的信息（模型为该 Key 实际可用的模型 ID 列表）。 */
export interface ModelAggKeyInfo {
  siteId: string;
  accountLabel: string;
  key: string;
  group: string;
  models: string[];
}

/** 独立展示的站点条目内一个分组小节。 */
export interface ModelAggSiteGroup {
  group: string;
  mode: ModelAggGroupMode;
  keys: ModelAggKeyInfo[];
}

/** 独立展示的站点条目（该站点未被聚合抽走的分组）。 */
export interface ModelAggSiteEntry {
  kind: "site";
  /** 排序键 = 站点 ID。 */
  orderKey: string;
  siteId: string;
  siteName: string;
  systemType: string;
  groups: ModelAggSiteGroup[];
}

/** 聚合块内一个站点的 Key 集合。 */
export interface ModelAggGroupSite {
  siteId: string;
  siteName: string;
  systemType: string;
  keys: ModelAggKeyInfo[];
}

/** 聚合块：同名分组的 Key 跨站合并展示。 */
export interface ModelAggGroupEntry {
  kind: "group";
  /** 排序键 = `group:<分组名>`。 */
  orderKey: string;
  group: string;
  sites: ModelAggGroupSite[];
}

export type ModelAggEntry = ModelAggSiteEntry | ModelAggGroupEntry;

export interface ModelAggTreeModel {
  /** 节点键：匹配上模型库为 `catalog:<canonicalKey>`，否则为 `raw:<原始模型 ID>`。 */
  key: string;
  /** 树上显示的名称：匹配上模型库用 displayName，否则用去掉厂商标识的 ID。 */
  label: string;
  /** 归属到该节点的原始聚合模型 ID（同一库模型可对应多个带前缀的站点 ID）。 */
  rawIds: string[];
  providerCount: number;
  matched: boolean;
  canonicalKey?: string;
}

export interface ModelAggTreeVendor {
  vendor: string;
  models: ModelAggTreeModel[];
}

const DEFAULT_GROUP = "默认分组";
const DEFAULT_VENDOR = "未知厂商";

/** 去掉前置厂商标识：取最后一个“/”之后的值作为模型 ID。 */
function stripVendorPrefix(id: string): string {
  const value = id.trim();
  const index = value.lastIndexOf("/");
  const base = index >= 0 ? value.slice(index + 1) : value;
  return base.trim();
}

/** 去厂商标识并忽略大小写后的模型 ID（两侧统一规范化）。 */
function normalizeModelId(id: string): string {
  return stripVendorPrefix(id).toLowerCase();
}

const { sites } = useLibrary();
const { preferences, updatePreferences } = usePreferences();
const { modelCatalog } = useModelCatalog();
const { tokenUsage } = useTokenStats();

// —— 全局单例状态 ——
const modelAggEntries = ref<SiteModelCacheEntry[]>([]);
const modelAggLoading = ref(false);
const modelAggLoaded = ref(false);
const modelAggError = ref("");
const selectedModelId = ref<string | null>(null);
const modelTreeSearch = ref("");
const expandedVendors = ref<Set<string>>(new Set());

/**
 * 手动取消勾选（从树中隐藏）的模型：按“去厂商前缀 + 小写”后的模型 ID 保存，
 * 与节点形态（raw:/catalog:）无关，模型库异步加载前后勾选状态保持一致。
 * 默认空 = 全部选中。
 */
const hiddenModelIds = ref<Set<string>>(normalizeHiddenModels(preferences.modelAggHiddenModels));

/**
 * 排序和分组模式先落在内存草稿里，只有点页头的「保存」按钮才写入偏好。
 * 模型筛选使用弹窗内的独立草稿，由弹窗的「保存」一次性提交。
 */
/** 分组展示模式草稿（独立是默认值，不记录）。 */
const draftGroupModes = ref<Record<string, ModelAggGroupMode>>({
  ...preferences.modelAggGroupModes,
});
/** 右侧条目顺序草稿。 */
const draftSiteOrder = ref<string[]>([...preferences.modelAggSiteOrder]);

/** 兼容旧版按节点 key（catalog:/raw: 前缀）保存的数据，统一转成规范化模型 ID。 */
function normalizeHiddenModels(raw: unknown): Set<string> {
  const list = Array.isArray(raw) ? raw : [];
  const set = new Set<string>();
  for (const item of list) {
    if (typeof item !== "string" || !item.trim()) continue;
    const value = item.startsWith("catalog:") || item.startsWith("raw:")
      ? item.slice(item.indexOf(":") + 1)
      : item;
    const norm = normalizeModelId(value);
    if (norm) set.add(norm);
  }
  return set;
}

const siteById = computed(() => {
  const map = new Map<
    string,
    { name: string; systemType: string; inUse: boolean }
  >();
  for (const site of sites.value) {
    map.set(site.id, {
      name: site.name || site.id,
      systemType: site.systemType || "",
      // 站点使用状态为“在用”（isPersonal 标记，见 site_crud 的 next_usage_state 循环）
      // 且存活（非跑路）才作为模型数据来源。
      inUse: site.isPersonal && !site.isRunaway,
    });
  }
  return map;
});

/** 站点是否「在用且存活」；不在站点库中的缓存条目无法判断，按保留处理。 */
function isSiteInUse(siteId: string): boolean {
  return siteById.value.get(siteId)?.inUse ?? true;
}

/** 全部 Key 的扁平信息（含分组与可用模型；无逐 Key 数据时回退站点级模型）。只统计在用站点。 */
const allKeyInfos = computed<ModelAggKeyInfo[]>(() => {
  const infos: ModelAggKeyInfo[] = [];
  for (const entry of modelAggEntries.value) {
    if (!isSiteInUse(entry.siteId)) continue;
    const siteModels = (entry.cache.models ?? []).map((model) => model.id);
    for (const account of entry.cache.accounts ?? []) {
      const accountLabel =
        account.username || account.accountName || account.profileName || "未命名账号";
      for (const key of account.keys ?? []) {
        const keyModels = account.keyModels?.[key];
        infos.push({
          siteId: entry.siteId,
          accountLabel,
          key,
          group: account.keyGroups?.[key]?.trim() || DEFAULT_GROUP,
          // keyModels[key] 为 undefined 说明没有逐 Key 数据，回退站点级模型。
          models: keyModels ? keyModels.map((model) => model.id) : siteModels,
        });
      }
    }
  }
  infos.sort(
    (a, b) =>
      (a.accountLabel || "").localeCompare(b.accountLabel || "", undefined, {
        numeric: true,
        sensitivity: "base",
      }) || a.key.localeCompare(b.key),
  );
  return infos;
});

/** 模型 → 提供方站点集合。 */
const providerIndex = computed(() => {
  const index = new Map<string, Set<string>>();
  for (const info of allKeyInfos.value) {
    for (const model of info.models) {
      let providers = index.get(model);
      if (!providers) {
        providers = new Set<string>();
        index.set(model, providers);
      }
      providers.add(info.siteId);
    }
  }
  return index;
});

/** 模型 ID → 厂商名（来自缓存中的 ownedBy，取首个非空；用于未匹配模型库的兜底分组）。 */
const modelVendorMap = computed(() => {
  const map = new Map<string, string>();
  for (const entry of modelAggEntries.value) {
    for (const model of entry.cache.models ?? []) {
      if (!map.has(model.id)) map.set(model.id, model.ownedBy?.trim() || "");
    }
    for (const account of entry.cache.accounts ?? []) {
      for (const list of Object.values(account.keyModels ?? {})) {
        for (const model of list) {
          if (!map.has(model.id)) map.set(model.id, model.ownedBy?.trim() || "");
        }
      }
    }
  }
  return map;
});

interface CatalogMatcher {
  /** 规范化 ID（去厂商前缀、小写）→ 库模型，用于精确匹配。 */
  exact: Map<string, ModelCatalogItem>;
  /** 按规范化 ID 长度降序排列的库模型，用于子串回退（首个命中即最长）。 */
  substring: ModelCatalogItem[];
}

/** 模型库（模型参数页）索引：canonicalKey 去厂商前缀后参与匹配。 */
const catalogMatcher = computed<CatalogMatcher>(() => {
  const exact = new Map<string, ModelCatalogItem>();
  const items: ModelCatalogItem[] = [];
  for (const item of modelCatalog.value.models ?? []) {
    const norm = normalizeModelId(item.canonicalKey);
    if (!norm) continue;
    if (!exact.has(norm)) exact.set(norm, item);
    items.push(item);
  }
  items.sort((left, right) =>
    normalizeModelId(right.canonicalKey).length - normalizeModelId(left.canonicalKey).length,
  );
  return { exact, substring: items };
});

/**
 * 把聚合模型 ID 归属到模型库模型：
 * 1. 去掉厂商标识（最后一个“/”之后）后忽略大小写精确匹配；
 * 2. 匹配不上时，库的规范化 ID 是聚合 ID（去前缀后）的子串也归属，
 *    多个命中取最长的库 ID。
 */
function resolveCatalogModel(rawId: string): ModelCatalogItem | null {
  const matcher = catalogMatcher.value;
  if (matcher.exact.size === 0) return null;
  const base = normalizeModelId(rawId);
  if (!base) return null;
  const exact = matcher.exact.get(base);
  if (exact) return exact;
  for (const item of matcher.substring) {
    if (base.includes(normalizeModelId(item.canonicalKey))) return item;
  }
  return null;
}

function catalogVendor(item: ModelCatalogItem): string {
  const vendor = item.manufacturer?.trim();
  return vendor && vendor !== "unknown" ? vendor : DEFAULT_VENDOR;
}

/** 左侧树：厂商 → 模型。匹配上模型库的按库厂商分组并合并同库的原始 ID，其余按 ownedBy 兜底。 */
const modelAggTree = computed<ModelAggTreeVendor[]>(() => {
  const vendors = new Map<string, Map<string, ModelAggTreeModel>>();
  for (const [rawId, providers] of providerIndex.value) {
    const catalog = resolveCatalogModel(rawId);
    const base = stripVendorPrefix(rawId);
    if (catalog) {
      const vendor = catalogVendor(catalog);
      let models = vendors.get(vendor);
      if (!models) {
        models = new Map();
        vendors.set(vendor, models);
      }
      const key = `catalog:${catalog.canonicalKey}`;
      const existing = models.get(key);
      if (existing) {
        existing.rawIds.push(rawId);
        existing.providerCount += countNewProviders(existing, rawId, providers);
      } else {
        const displayName = catalog.displayName?.trim() || base || rawId;
        const catalogBase = stripVendorPrefix(catalog.canonicalKey);
        // 名称后括号显示模型 id；仅当名称与 id 完全同字符时才省略，避免重复。
        const label =
          displayName === catalogBase ? displayName : `${displayName} (${catalogBase})`;
        models.set(key, {
          key,
          label,
          rawIds: [rawId],
          providerCount: providers.size,
          matched: true,
          canonicalKey: catalog.canonicalKey,
        });
      }
    } else {
      const vendor = modelVendorMap.value.get(rawId) || DEFAULT_VENDOR;
      let models = vendors.get(vendor);
      if (!models) {
        models = new Map();
        vendors.set(vendor, models);
      }
      const key = `raw:${rawId}`;
      if (!models.has(key)) {
        models.set(key, {
          key,
          // 名称即去前缀 id；有厂商前缀时括号补全完整原始 id。
          label: base && base !== rawId ? `${base} (${rawId})` : base || rawId,
          rawIds: [rawId],
          providerCount: providers.size,
          matched: false,
        });
      }
    }
  }
  return [...vendors.entries()]
    .map(([vendor, models]) => ({
      vendor,
      models: [...models.values()].sort((left, right) =>
        left.label.localeCompare(right.label, "zh-CN"),
      ),
    }))
    .sort((left, right) => {
      if (left.vendor === DEFAULT_VENDOR) return 1;
      if (right.vendor === DEFAULT_VENDOR) return -1;
      return left.vendor.localeCompare(right.vendor, "zh-CN");
    });
});

/** 合并原始 ID 到同一节点时按站点去重：只统计尚未计入的提供方站点。 */
function countNewProviders(
  node: ModelAggTreeModel,
  rawId: string,
  providers: Set<string>,
): number {
  // 同一节点可能对应多个原始 ID，需与其它 ID 的提供方站点并集后去重。
  const seen = new Set<string>();
  for (const other of node.rawIds) {
    if (other === rawId) continue;
    for (const siteId of providerIndex.value.get(other) ?? []) seen.add(siteId);
  }
  let added = 0;
  for (const siteId of providers) {
    if (!seen.has(siteId)) {
      seen.add(siteId);
      added += 1;
    }
  }
  return added;
}

/** Token 统计里实际用过的模型名（去厂商标识、忽略大小写）。 */
const usedModelNames = computed<Set<string>>(() => {
  const names = new Set<string>();
  for (const bucket of tokenUsage.value?.buckets ?? []) {
    const norm = normalizeModelId(bucket.model ?? "");
    if (norm) names.add(norm);
  }
  return names;
});

const usedModelNameList = computed<string[]>(() => [...usedModelNames.value]);

/** 节点是否在“仅用过”范围内：任一原始 ID 与用过的模型名相等，或是某个用过的模型名的子串。 */
function isNodeUsed(rawIds: string[]): boolean {
  const used = usedModelNames.value;
  if (used.size === 0) return false;
  for (const rawId of rawIds) {
    const norm = normalizeModelId(rawId);
    if (!norm) continue;
    if (used.has(norm)) return true;
    // 统计里的名字常带版本后缀（如 gpt-4o-2024-11-20），节点是其子串即视为用过。
    for (const name of usedModelNameList.value) {
      if (name.includes(norm)) return true;
    }
  }
  return false;
}

/** 节点是否被取消勾选（其任一原始 ID 在隐藏集合中）。 */
function isNodeHidden(node: ModelAggTreeModel): boolean {
  const hidden = hiddenModelIds.value;
  if (hidden.size === 0) return false;
  return node.rawIds.some((rawId) => hidden.has(normalizeModelId(rawId)));
}

/** 应用搜索词与勾选筛选后的树（命中显示名、原始 ID、canonicalKey 或厂商名）。 */
const filteredModelTree = computed<ModelAggTreeVendor[]>(() => {
  const keyword = modelTreeSearch.value.trim().toLowerCase();
  return modelAggTree.value
    .map((vendor) => ({
      vendor: vendor.vendor,
      models: vendor.models.filter(
        (model) =>
          !isNodeHidden(model) &&
          (!keyword ||
            model.label.toLowerCase().includes(keyword) ||
            model.rawIds.some((id) => id.toLowerCase().includes(keyword)) ||
            (model.canonicalKey?.toLowerCase().includes(keyword) ?? false) ||
            vendor.vendor.toLowerCase().includes(keyword)),
      ),
    }))
    .filter((vendor) => vendor.models.length > 0);
});

/** 当前搜索 + 筛选范围内的模型节点数（“全部模型”入口徽标）。 */
const filteredModelCount = computed(() =>
  filteredModelTree.value.reduce((sum, vendor) => sum + vendor.models.length, 0),
);

/** 勾选统计：全部节点数与已勾选（未被隐藏）数。 */
const modelSelectionStats = computed(() => {
  let total = 0;
  let selected = 0;
  for (const vendor of modelAggTree.value) {
    for (const model of vendor.models) {
      total += 1;
      if (!isNodeHidden(model)) selected += 1;
    }
  }
  return { total, selected };
});

/** 一次性应用并持久化弹窗中的模型勾选结果。 */
function saveModelSelection(selectedModelKeys: ReadonlySet<string>) {
  const next = new Set<string>();
  for (const vendor of modelAggTree.value) {
    for (const model of vendor.models) {
      if (selectedModelKeys.has(model.key)) continue;
      for (const rawId of model.rawIds) {
        const norm = normalizeModelId(rawId);
        if (norm) next.add(norm);
      }
    }
  }
  hiddenModelIds.value = next;
  updatePreferences({ modelAggHiddenModels: [...next] });
  void syncGatewayConfig();
}

/** 汇总统计（页头副标题与侧栏徽标用）；模型数为归属合并后的树节点数，站点只计在用。 */
const modelAggStats = computed(() => {
  const groups = new Set<string>();
  for (const info of allKeyInfos.value) groups.add(info.group);
  return {
    siteCount: modelAggEntries.value.filter((entry) => isSiteInUse(entry.siteId)).length,
    modelCount: modelAggTree.value.reduce((sum, vendor) => sum + vendor.models.length, 0),
    groupCount: groups.size,
    keyCount: allKeyInfos.value.length,
  };
});

/** 当前选中的树节点；未选中为 null。 */
const selectedTreeNode = computed<ModelAggTreeModel | null>(() => {
  const key = selectedModelId.value;
  if (!key) return null;
  for (const vendor of modelAggTree.value) {
    const node = vendor.models.find((model) => model.key === key);
    if (node) return node;
  }
  return null;
});

/** 选中节点对应的原始聚合模型 ID 集合（右侧按此过滤）。 */
const selectedRawIds = computed<Set<string>>(() => {
  const node = selectedTreeNode.value;
  return node ? new Set(node.rawIds) : new Set<string>();
});

/** 当前选中模型的提供方站点数（原始 ID 的提供方并集）；未选中为 0。 */
const selectedModelProviderCount = computed(() => {
  const rawIds = selectedRawIds.value;
  if (rawIds.size === 0) return 0;
  const sites = new Set<string>();
  for (const rawId of rawIds) {
    for (const siteId of providerIndex.value.get(rawId) ?? []) sites.add(siteId);
  }
  return sites.size;
});

function groupMode(group: string): ModelAggGroupMode {
  return draftGroupModes.value[group] ?? "independent";
}

/** 当前选中模型的有效调度通道总数：聚合分组计为 1 个通道，独立分组按其实际 Key 数量分别计入通道。 */
const selectedModelChannelCount = computed<number>(() => {
  let count = 0;
  for (const entry of modelAggRightEntries.value) {
    if (entry.kind === "group") {
      const mode = groupMode(entry.group);
      if (mode === "aggregate") {
        count += 1;
      } else {
        for (const site of entry.sites) {
          count += site.keys.length;
        }
      }
    } else {
      for (const section of entry.groups) {
        const mode = groupMode(section.group);
        if (mode === "aggregate") {
          count += 1;
        } else {
          count += section.keys.length;
        }
      }
    }
  }
  return count;
});

/** 右侧条目：必须选中模型（左侧必须选中某个模型，右侧才显示具体的 Key 通道）。 */
const modelAggRightEntries = computed<ModelAggEntry[]>(() => {
  const selectedRaw = selectedRawIds.value;
  if (selectedRaw.size === 0) {
    return [];
  }
  interface SiteBucket {
    siteId: string;
    siteName: string;
    systemType: string;
    groups: Map<string, ModelAggKeyInfo[]>;
  }
  const buckets: SiteBucket[] = [];
  const bucketBySite = new Map<string, SiteBucket>();
  for (const info of allKeyInfos.value) {
    if (!info.models.some((id) => selectedRaw.has(id))) continue;
    let bucket = bucketBySite.get(info.siteId);
    if (!bucket) {
      const site = siteById.value.get(info.siteId);
      bucket = {
        siteId: info.siteId,
        siteName: site?.name || info.siteId,
        systemType: site?.systemType || "",
        groups: new Map(),
      };
      bucketBySite.set(info.siteId, bucket);
      buckets.push(bucket);
    }
    let keys = bucket.groups.get(info.group);
    if (!keys) {
      keys = [];
      bucket.groups.set(info.group, keys);
    }
    keys.push(info);
  }

  // 同名分组且模式为聚合 → 抽出为跨站聚合块；其余留在站点卡片内。
  const groupBlocks = new Map<string, ModelAggGroupEntry>();
  const siteEntries: ModelAggSiteEntry[] = [];
  for (const bucket of buckets) {
    const siteGroups: ModelAggSiteGroup[] = [];
    for (const [group, keys] of bucket.groups) {
      const mode = groupMode(group);
      if (mode === "aggregate") {
        let block = groupBlocks.get(group);
        if (!block) {
          block = {
            kind: "group",
            orderKey: `group:${group}`,
            group,
            sites: [],
          };
          groupBlocks.set(group, block);
        }
        block.sites.push({
          siteId: bucket.siteId,
          siteName: bucket.siteName,
          systemType: bucket.systemType,
          keys,
        });
      } else {
        siteGroups.push({ group, mode, keys });
      }
    }
    if (siteGroups.length > 0) {
      siteEntries.push({
        kind: "site",
        orderKey: bucket.siteId,
        siteId: bucket.siteId,
        siteName: bucket.siteName,
        systemType: bucket.systemType,
        groups: siteGroups,
      });
    }
  }

  // 默认顺序：站点卡片按名称、聚合块按分组名；草稿中记录过的键优先按记录顺序排列。
  siteEntries.sort((left, right) => left.siteName.localeCompare(right.siteName, "zh-CN"));
  const orderedBlocks = [...groupBlocks.values()].sort((left, right) =>
    left.group.localeCompare(right.group, "zh-CN"),
  );
  const entries: ModelAggEntry[] = [...siteEntries, ...orderedBlocks];
  const rank = new Map(
    draftSiteOrder.value.map((key, index) => [key, index] as const),
  );
  return entries.sort((left, right) => {
    const leftRank = rank.get(left.orderKey);
    const rightRank = rank.get(right.orderKey);
    if (leftRank !== undefined && rightRank !== undefined) return leftRank - rightRank;
    if (leftRank !== undefined) return -1;
    if (rightRank !== undefined) return 1;
    return 0;
  });
});

/** 保存右侧条目顺序并即时落盘与同步网关。 */
function updateDraftEntryOrder(keys: string[]) {
  const visible = new Set(keys);
  const hidden = draftSiteOrder.value.filter((key) => !visible.has(key));
  const next = [...keys, ...hidden];
  draftSiteOrder.value = next;
  updatePreferences({ modelAggSiteOrder: next });
  void syncGatewayConfig();
}

/** 把 fromKey 移动到 targetKey 的前/后；用于拖拽与上下移按钮。 */
function dropAggEntry(fromKey: string, targetKey: string, place: "before" | "after") {
  if (fromKey === targetKey) return;
  const keys = modelAggRightEntries.value
    .map((entry) => entry.orderKey)
    .filter((key) => key !== fromKey);
  let targetIndex = keys.indexOf(targetKey);
  if (targetIndex < 0) return;
  if (place === "after") targetIndex += 1;
  keys.splice(targetIndex, 0, fromKey);
  updateDraftEntryOrder(keys);
}

/** 上移/下移一个条目。 */
function moveAggEntry(orderKey: string, direction: -1 | 1) {
  const entries = modelAggRightEntries.value;
  const index = entries.findIndex((entry) => entry.orderKey === orderKey);
  const target = index + direction;
  if (index < 0 || target < 0 || target >= entries.length) return;
  dropAggEntry(orderKey, entries[target].orderKey, direction < 0 ? "before" : "after");
}

/** 设置某分组名的展示模式并即时落盘与同步网关，保持当前视觉位置不跳动。 */
function setGroupMode(group: string, mode: ModelAggGroupMode) {
  const currentEntries = modelAggRightEntries.value;
  const targetGroupKey = `group:${group}`;

  let currentOrder = [...draftSiteOrder.value];
  if (currentOrder.length === 0) {
    currentOrder = currentEntries.map((e) => e.orderKey);
  }

  if (mode === "aggregate") {
    // 寻找包含该分组的第一个站点在当前列表中的位置
    const siteIdx = currentOrder.findIndex((key) => {
      if (key.startsWith("group:")) return false;
      return allKeyInfos.value.some((k) => k.siteId === key && k.group === group);
    });

    currentOrder = currentOrder.filter((k) => k !== targetGroupKey);
    if (siteIdx >= 0) {
      currentOrder.splice(siteIdx, 0, targetGroupKey);
    } else {
      currentOrder.push(targetGroupKey);
    }
  } else {
    // 独立模式：找到原聚合块的位置并就地展开为对应站点
    const groupIdx = currentOrder.indexOf(targetGroupKey);
    if (groupIdx >= 0) {
      currentOrder = currentOrder.filter((k) => k !== targetGroupKey);
      const memberSiteIds = Array.from(
        new Set(allKeyInfos.value.filter((k) => k.group === group).map((k) => k.siteId)),
      );
      let insertIdx = groupIdx;
      for (const siteId of memberSiteIds) {
        if (!currentOrder.includes(siteId)) {
          currentOrder.splice(insertIdx, 0, siteId);
          insertIdx += 1;
        }
      }
    }
  }

  const modes = { ...preferences.modelAggGroupModes };
  if (mode === "independent") delete modes[group];
  else modes[group] = mode;

  draftGroupModes.value = { ...modes };
  draftSiteOrder.value = currentOrder;
  updatePreferences({
    modelAggGroupModes: modes,
    modelAggSiteOrder: currentOrder,
  });
  void syncGatewayConfig();
}

/** 排序或分组模式是否有未保存的变更（现已实时保存，始终为 false）。 */
const modelAggDirty = computed(() => false);

const gatewayStatus = ref<GatewayStatus | null>(null);
const gatewayLoading = ref(false);

async function loadGatewayStatus() {
  try {
    const status = await runCommand<GatewayStatus>("get_gateway_status");
    gatewayStatus.value = status;
  } catch (error) {
    console.error("Failed to load gateway status:", error);
  }
}

async function syncGatewayConfig() {
  try {
    const status = await runCommand<GatewayStatus>("update_gateway_config", {
      config: {
        enabled: preferences.gatewayEnabled,
        port: preferences.gatewayPort,
        apiKey: preferences.gatewayApiKey,
        apiKeys: preferences.gatewayApiKeys,
        modelAggGroupModes: preferences.modelAggGroupModes,
        modelAggHiddenNodes: preferences.modelAggHiddenModels,
      },
    });
    gatewayStatus.value = status;
  } catch (error) {
    console.error("Failed to sync gateway config:", error);
  }
}

async function startGateway(port?: number) {
  gatewayLoading.value = true;
  try {
    const targetPort = port ?? preferences.gatewayPort;
    const status = await runCommand<GatewayStatus>("start_gateway", { port: targetPort });
    gatewayStatus.value = status;
    updatePreferences({ gatewayEnabled: true, gatewayPort: status.port });
  } catch (error) {
    throw error;
  } finally {
    gatewayLoading.value = false;
  }
}

async function stopGateway() {
  gatewayLoading.value = true;
  try {
    const status = await runCommand<GatewayStatus>("stop_gateway");
    gatewayStatus.value = status;
    updatePreferences({ gatewayEnabled: false });
  } catch (error) {
    throw error;
  } finally {
    gatewayLoading.value = false;
  }
}

async function updateGatewaySettings(settings: {
  port?: number;
  apiKey?: string;
  apiKeys?: GatewayApiKeyItem[];
  enabled?: boolean;
}) {
  gatewayLoading.value = true;
  try {
    const enabled = settings.enabled !== undefined ? settings.enabled : preferences.gatewayEnabled;
    const port = settings.port ?? preferences.gatewayPort;
    const apiKey = settings.apiKey !== undefined ? settings.apiKey : preferences.gatewayApiKey;
    const apiKeys = settings.apiKeys !== undefined ? settings.apiKeys : preferences.gatewayApiKeys;
    updatePreferences({
      gatewayEnabled: enabled,
      gatewayPort: port,
      gatewayApiKey: apiKey,
      gatewayApiKeys: apiKeys,
    });
    const status = await runCommand<GatewayStatus>("update_gateway_config", {
      config: {
        enabled,
        port,
        apiKey,
        apiKeys,
        modelAggGroupModes: preferences.modelAggGroupModes,
        modelAggHiddenNodes: preferences.modelAggHiddenModels,
      },
    });
    gatewayStatus.value = status;
  } catch (error) {
    throw error;
  } finally {
    gatewayLoading.value = false;
  }
}

/** 一次性把顺序和分组模式写入偏好。 */
function saveModelAgg() {
  updatePreferences({
    modelAggSiteOrder: [...draftSiteOrder.value],
    modelAggGroupModes: { ...draftGroupModes.value },
  });
  void syncGatewayConfig();
}

/** 放弃未保存的变更，回到上次保存的状态。 */
function discardModelAggChanges() {
  draftSiteOrder.value = [...preferences.modelAggSiteOrder];
  draftGroupModes.value = { ...preferences.modelAggGroupModes };
}

function selectModel(nodeKey: string) {
  selectedModelId.value = nodeKey;
}

function clearSelectedModel() {
  selectedModelId.value = null;
}

watch(
  () => modelAggTree.value,
  (tree) => {
    if (!selectedModelId.value && tree.length > 0 && tree[0].models.length > 0) {
      selectedModelId.value = tree[0].models[0].key;
    }
  },
  { immediate: true }
);

function toggleVendor(vendor: string) {
  const next = new Set(expandedVendors.value);
  if (next.has(vendor)) next.delete(vendor);
  else next.add(vendor);
  expandedVendors.value = next;
}

/** 搜索时自动展开全部命中的厂商。 */
function expandVendorsForSearch() {
  expandedVendors.value = new Set(filteredModelTree.value.map((vendor) => vendor.vendor));
}

function collapseAllVendors() {
  expandedVendors.value = new Set();
}

async function loadModelAggregation(force = false) {
  if (modelAggLoading.value) return;
  if (modelAggLoaded.value && !force) return;
  modelAggLoading.value = true;
  modelAggError.value = "";
  try {
    const data = await runCommand<SiteModelCacheEntry[]>("get_all_site_model_caches");
    modelAggEntries.value = data ?? [];
    modelAggLoaded.value = true;
    void loadGatewayStatus();
  } catch (error) {
    modelAggError.value = String(error);
  } finally {
    modelAggLoading.value = false;
  }
}

export function useModelAggregate() {
  return {
    modelAggEntries,
    modelAggLoading,
    modelAggLoaded,
    modelAggError,
    modelAggTree,
    filteredModelTree,
    filteredModelCount,
    modelSelectionStats,
    hiddenModelIds,
    isNodeHidden,
    saveModelSelection,
    usedModelNames,
    isNodeUsed,
    modelAggStats,
    modelAggModelCount: computed(() => modelAggStats.value.modelCount),
    modelAggRightEntries,
    selectedModelId,
    selectedTreeNode,
    selectedRawIds,
    selectedModelProviderCount,
    selectedModelChannelCount,
    modelTreeSearch,
    expandedVendors,
    groupMode,
    loadModelAggregation,
    selectModel,
    clearSelectedModel,
    toggleVendor,
    expandVendorsForSearch,
    collapseAllVendors,
    moveAggEntry,
    dropAggEntry,
    setGroupMode,
    modelAggDirty,
    saveModelAgg,
    discardModelAggChanges,
    gatewayStatus,
    gatewayLoading,
    loadGatewayStatus,
    startGateway,
    stopGateway,
    updateGatewaySettings,
    syncGatewayConfig,
  };
}
