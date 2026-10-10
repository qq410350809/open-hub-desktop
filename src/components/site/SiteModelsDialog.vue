<script setup lang="ts">
import { computed, ref, watch, nextTick } from "vue";
import { runCommand, useLibrary } from "../../composables/useLibrary";
import { icons } from "../../icons";
import { useStore } from "../../composables/useStore";
import { logoText, describeModelHealth, describeModelStatusStrip, describeModelStatusStripTitle, describeModelWindowLabel, modelHealthValue, MODEL_STATUS_SLOT_COUNT } from "../../utils";
import type { ModelHealthBadge, ModelStatusSlot } from "../../utils";
import { isUnknownSystemType, systemTypeLabel } from "../../types";
import type { SiteModelHealth } from "../../types";
import { useToast } from "../../composables/core/useToast";
import { useConfirm } from "../../composables/ui/useConfirm";
import {
  useModelProxy,
  proxyConfig,
  refreshProxyConfig,
} from "../../composables/proxy/useModelProxy";

interface LiveModelItem {
  id: string;
  owned_by?: string;
  ownedBy?: string;
}

interface FetchSiteModelsResult {
  models: LiveModelItem[];
  source: string;
  keys: string[];
  keyGroups?: Record<string, string>;
  keyModels?: Record<string, LiveModelItem[]>;
  errors?: string[];
  /** 实际产出这批 Key 的账号（Chrome Profile）；空表示无归属。 */
  profileId?: string;
  /** 全站模型健康度（模型 ID → 健康度）；站点无该接口时为空。 */
  modelHealth?: Record<string, SiteModelHealth>;
}

type ModelApiSource = "newapi-key" | "sub2api-key" | "pricing" | "models" | "none";

interface LiveAccountKeys {
  profileId: string;
  profileName: string;
  accountName: string;
  username: string;
  keys: string[];
  keyGroups?: Record<string, string>;
  /** 每个 Key 对应的模型列表（逐 Key 查询 /v1/models 的结果）。 */
  keyModels?: Record<string, LiveModelItem[]>;
  error: string;
}

const isTauri = "__TAURI_INTERNALS__" in window;
const store = useStore();
const { usageSites } = useLibrary();
const { showToast } = useToast();
const { confirm } = useConfirm();
const closeBtnRef = ref<HTMLButtonElement>();
/**
 * 「无效模型」的成功率阈值：成功率低于该值即视为无效（含 0%）。
 * 与健康度分级一致——<50% 就是最差的红档（level 1）。留一个极小余量，
 * 让「有极少请求且全失败」的 0.x% 也归入无效。
 */
const UNHEALTHY_SUCCESS_RATE = 0.01;
const searchQuery = ref("");
const liveFetching = ref(false);
/** 当前正在执行的同步类型；用于区分 Key/模型按钮各自的转圈状态。 */
const liveFetchingKind = ref<"keys" | "models" | null>(null);
const liveError = ref("");
const liveModels = ref<LiveModelItem[]>([]);
/** 全站模型健康度（模型 ID → 健康度），来自 NewAPI 的性能指标接口。 */
const modelHealth = ref<Record<string, SiteModelHealth>>({});
const liveAccountKeys = ref<LiveAccountKeys[]>([]);
const apiSource = ref<ModelApiSource>("none");
/** 当前选中的 API Key；选中后右侧只显示该 Key 对应的模型。 */
const selectedKeyId = ref<string | null>(null);
let liveFetchRequestId = 0;

const site = computed(() => store.siteModelsSite.value);
const liveKeyCount = computed(() =>
  liveAccountKeys.value.reduce((total, account) => total + account.keys.length, 0),
);

// —— 反代渠道管理：站点与模型反代网关的导入/移除 ——
const modelProxy = useModelProxy();
/** 站点当前对应的反代渠道；存在即视为「已反代」 */
const siteProxy = computed(() =>
  site.value ? proxyConfig.value.channels.find((c) => c.siteId === site.value?.id) : undefined,
);
const proxyBusy = ref(false);

/** 导入反代：将站点创建为反代渠道（运行时使用关联站点 Key） */
async function importSiteProxy() {
  const target = site.value;
  if (!target || proxyBusy.value) return;
  proxyBusy.value = true;
  try {
    // 先刷新配置，避免基于过期数据重复导入
    await refreshProxyConfig();
    if (proxyConfig.value.channels.some((c) => c.siteId === target.id)) {
      showToast("该站点已导入反代", true);
      return;
    }
    const ok = await modelProxy.addSiteProxyChannel(target);
    if (ok) showToast(`已导入反代：站点「${target.name}」已创建反代渠道`);
  } finally {
    proxyBusy.value = false;
  }
}

/** 移除反代：删除站点对应的反代渠道（不影响站点库数据） */
async function removeSiteProxy() {
  const target = site.value;
  if (!target || proxyBusy.value) return;
  const okConfirm = await confirm({
    title: "移除反代渠道",
    message: `确定移除站点「${target.name}」的反代渠道吗？该渠道的 Key 分组与模型配置将一并删除，站点库数据不受影响。`,
    confirmText: "移除",
    danger: true,
  });
  if (!okConfirm) return;
  proxyBusy.value = true;
  try {
    const ok = await modelProxy.removeSiteProxyChannel(target.id);
    if (ok) showToast(`已移除反代渠道「${target.name}」`);
  } finally {
    proxyBusy.value = false;
  }
}

const logo = computed(() =>
  site.value ? logoText(site.value.apiBaseUrl, site.value.name) : "",
);

const apiSourceLabel = computed(() => {
  switch (apiSource.value) {
    case "newapi-key":
      return "通过 NewAPI Key 获取";
    case "sub2api-key":
      return "通过 Sub2API Key 获取";
    case "pricing":
      return "同步自站点定价数据";
    case "models":
      return "同步自站点模型数据";
    default:
      return "本地模型数据";
  }
});

/** 选中 Key 时，该 Key 对应的模型列表；未选中时为 null 表示未选中特定 Key。 */
const selectedKeyModels = computed<LiveModelItem[] | null>(() => {
  const keyId = selectedKeyId.value;
  if (!keyId) return null;
  for (const account of liveAccountKeys.value) {
    if (account.keys.includes(keyId)) {
      const models = account.keyModels?.[keyId];
      return Array.isArray(models) ? models : [];
    }
  }
  return [];
});

/** 当前生效的模型列表：如果选中了 Key，以该 Key 的模型为准（即使为 0 个）；未选中 Key 时展示站点全量模型。 */
const currentActiveModels = computed<LiveModelItem[]>(() => {
  if (selectedKeyId.value !== null) {
    return selectedKeyModels.value ?? [];
  }
  return liveModels.value;
});

/** 隐藏「成功率极低」的模型开关。 */
const hideUnhealthy = ref(false);

/** 模型是否被视为「无效」：成功率为 0 或极低（低于阈值）。
 *
 *  口径只认「有实测成功率且极低」，不用「站点没上报健康度」判无效——
 *  无数据只说明站点没有该模型的流量记录，不等于模型不可用（很多冷门模型
 *  就是长期无人调用）。健康度数值与徽标同源（modelHealthValue）。 */
function isUnhealthyModel(modelId: string): boolean {
  const value = modelHealthValue(modelHealth.value[modelId]);
  if (value === null) return false;
  return value < UNHEALTHY_SUCCESS_RATE;
}

/** 被隐藏的无效模型数量，用于开关上的提示与空列表文案。 */
const hiddenUnhealthyCount = computed(() => {
  if (!hideUnhealthy.value) return 0;
  return currentActiveModels.value.filter((model) => isUnhealthyModel(model.id)).length;
});

const filteredLiveModels = computed(() => {
  let source = currentActiveModels.value;
  if (hideUnhealthy.value) {
    source = source.filter((model) => !isUnhealthyModel(model.id));
  }
  const q = searchQuery.value.trim().toLowerCase();
  if (!q) return source;
  return source.filter(
    (m) => m.id.toLowerCase().includes(q) || (m.owned_by && m.owned_by.toLowerCase().includes(q)),
  );
});

const modelCountLabel = computed(() => {
  const source = currentActiveModels.value;
  const total = source.length;
  // 有筛选（搜索或隐藏无效）时显示「命中 / 总数」，让用户知道过滤了多少。
  const filtering = Boolean(searchQuery.value.trim()) || hideUnhealthy.value;
  return filtering ? `${filteredLiveModels.value.length} / ${total}` : String(total);
});

/** 开关按钮标题：说明口径与当前隐藏了多少个模型。 */
const hideUnhealthyTitle = computed(() => {
  const base = `隐藏成功率极低（低于 ${Math.round(UNHEALTHY_SUCCESS_RATE * 100)}%，含 0%）的模型；站点未上报健康度（无数据）的模型不算无效，仍会保留`;
  return hideUnhealthy.value && hiddenUnhealthyCount.value > 0
    ? `${base}；当前已隐藏 ${hiddenUnhealthyCount.value} 个`
    : base;
});

/** 站点是否上报过任何模型健康度；用于在面板上给出一次性的口径说明。 */
const hasModelHealth = computed(() => Object.keys(modelHealth.value).length > 0);

/** 站点是否上报过任何模型健康度：上报过就画 24 格条带。
 *  只给汇总成功率、没给逐时段序列的（旧数据里连 series 字段都没有）条带是
 *  24 格灰，但那是「有健康度、只是没有时段分布」，不该连条带一起藏掉。 */
const hasStatusStrip = computed(() => Object.keys(modelHealth.value).length > 0);

// 每个模型只解析一次：模板里徽标要读 level/label/title 三处，
// 逐次调用会把同样的格式化跑三遍。
const healthBadges = computed(() => {
  const badges = new Map<string, ModelHealthBadge>();
  for (const [modelId, health] of Object.entries(modelHealth.value)) {
    const badge = describeModelHealth(health);
    if (badge) badges.set(modelId, badge);
  }
  return badges;
});

function healthBadgeOf(modelId: string) {
  return healthBadges.value.get(modelId);
}

// 状态条：每个模型都有一条 24 格条带——有流量的时段上色，没流量的留灰
// （模型整条没有流量就是 24 格全灰）。
// 无数据模型没有自己的 windowStart/格宽，用同站点任一有数据模型的窗口对齐，
// 保证全灰条带和有数据条带落在同一条时间轴上（格宽也跟着窗口走，否则
// 7 天窗口的站点会错位）。
const statusStrips = computed(() => {
  const strips = new Map<string, ModelStatusSlot[]>();
  const healthMap = modelHealth.value;
  const fallbackHealth = Object.values(healthMap).find(
    (health) => typeof health.windowStart === "number" && (health.windowStart ?? 0) > 0,
  );
  for (const model of currentActiveModels.value) {
    strips.set(model.id, describeModelStatusStrip(healthMap[model.id], fallbackHealth));
  }
  return strips;
});

function statusStripOf(modelId: string) {
  return statusStrips.value.get(modelId) ?? [];
}

/** 状态条容器口径说明；任一健康度条目即可提供窗口与格宽。 */
const statusStripTitle = computed(() =>
  describeModelStatusStripTitle(Object.values(modelHealth.value)[0]),
);

/** 状态条可读性兜底：给 role=img 一句完整描述，供读屏而非鼠标悬停使用。 */
const statusStripLabel = computed(
  () =>
    `${describeModelWindowLabel(Object.values(modelHealth.value)[0])}成功率状态条，共 ${MODEL_STATUS_SLOT_COUNT} 个时段`,
);

/** 说明段落里的窗口口径：站点窗口不是 24 小时时不能写死「近 24 小时逐时」。 */
const healthNoteWindow = computed(() =>
  describeModelWindowLabel(Object.values(modelHealth.value)[0]),
);

interface LocalSiteModelCache {
  models: LiveModelItem[];
  apiSource: ModelApiSource;
  accounts: LiveAccountKeys[];
  modelHealth?: Record<string, SiteModelHealth>;
}

async function readCachedModels(siteId: string): Promise<boolean> {
  if (!isTauri) return false;
  try {
    const data = await runCommand<LocalSiteModelCache>("get_site_model_cache", { siteId });
    if (!data || !Array.isArray(data.models)) return false;
    liveModels.value = (data.models as LiveModelItem[]).map((model: LiveModelItem) => ({
      ...model,
      owned_by: model.owned_by || model.ownedBy,
    }));
    // 同步 Key 时顺带抓的全站健康度。站点没有该接口时为空，
    // 界面只是不显示徽标，模型列表本身不受影响。
    modelHealth.value = data.modelHealth ?? {};
    apiSource.value = data.apiSource || "none";
    const cachedAccounts = (Array.isArray(data.accounts) ? data.accounts : []).filter(
      // 历史遗留的空壳行：既无账号归属（profileId 空）又没有任何 Key，
      // 只是一次失败的站点级同步落下的残骸，没有「账号」可言，直接忽略。
      (account: LiveAccountKeys) => Boolean(account.profileId) || (account.keys?.length ?? 0) > 0,
    );
    // 账号来源 = Chrome 会话账号 ∪ 模型缓存账号，按 profileId 合并：
    // Chrome 上检测到的账号（含未同步过 Key 的）默认就展示，缓存账号
    // 则把已同步的 Key/分组/模型映射带上，覆盖同 profileId 的占位账号。
    const chromeAccounts =
      usageSites.value.find((item) => item.siteId === siteId)?.sessions ?? [];
    const byProfile = new Map<string, LiveAccountKeys>();
    for (const session of chromeAccounts) {
      byProfile.set(session.profileId || session.accountName || session.profileName, {
        profileId: session.profileId,
        profileName: session.profileName,
        accountName: session.accountName,
        username: session.username,
        keys: [],
        keyGroups: {},
        keyModels: {},
        error: "",
      });
    }
    for (const account of cachedAccounts) {
      byProfile.set(account.profileId || account.accountName || account.profileName, account);
    }
    liveAccountKeys.value = [...byProfile.values()].sort((a, b) =>
      (a.username || a.accountName || a.profileName || "").localeCompare(
        b.username || b.accountName || b.profileName || "",
        undefined,
        { numeric: true, sensitivity: "base" }
      )
    );
    return liveAccountKeys.value.length > 0 || liveModels.value.length > 0;
  } catch {
    return false;
  }
}

watch(
  () => store.siteModelsDialogOpen.value,
  (open) => {
    if (open) {
      nextTick(() => closeBtnRef.value?.focus());
      document.body.classList.add("modal-open");
      liveModels.value = [];
      liveAccountKeys.value = [];
      modelHealth.value = {};
      liveError.value = "";
      searchQuery.value = "";
      // 每次打开弹窗重置筛选开关：默认展示全部模型，「隐藏无效」由用户按需开启。
      hideUnhealthy.value = false;
      apiSource.value = "none";
      selectedKeyId.value = null;
      addingKeyForProfile.value = null;
      newKeyGroup.value = "";
      newKeyValue.value = "";
      void refreshModels();
      // 拉取最新反代渠道配置，驱动「导入反代/移除反代」按钮状态
      void refreshProxyConfig();
    } else {
      liveFetchRequestId += 1;
      liveFetching.value = false;
      liveFetchingKind.value = null;
      document.body.classList.remove("modal-open");
    }
  },
);

function close() {
  store.closeSiteModelsDialog();
}

function onBackdropClick(event: MouseEvent) {
  if (event.target === event.currentTarget) close();
}

async function refreshModels(mode: "cache" | "keys" | "models" = "cache") {
  const requestedSite = site.value;
  if (!requestedSite) return;
  const requestId = ++liveFetchRequestId;
  liveFetching.value = true;
  liveFetchingKind.value = mode === "keys" ? "keys" : mode === "models" ? "models" : null;
  liveError.value = "";
  liveModels.value = [];
  // 同步模型只刷新右侧模型列表：保留左侧 Key 树、来源标签与选中 Key，不重置它们。
  if (mode !== "models") {
    liveAccountKeys.value = [];
    apiSource.value = "none";
    selectedKeyId.value = null;
  }
  try {
    if (mode === "keys") {
      // 重新拉取各账号的 Key 列表并重建缓存（含模型映射）；
      // 仅已知架构站点提供该入口，未知站点无 Key 提取能力。
      const siteUsage = usageSites.value.find((item) => item.siteId === requestedSite.id);
      // 不按 isValid 过滤：isValid=0 只说明上次账号刷新没拿到登录凭据，账号行与
      // Chrome profile 都还在，逐账号同步仍能借缓存凭据取 Key，失败原因也会按
      // 账号落到各自的 error 上。过滤掉等于让这些账号彻底没有同步机会。
      const sessions = siteUsage?.sessions ?? [];
      let baseUrl = requestedSite.apiBaseUrl.trim();
      if (!baseUrl.endsWith("/")) baseUrl += "/";
      if (sessions.length === 0) {
        // 站点上还没有任何账号记录，尝试不带 profileId 请求。后端会借有效账号的
        // 会话抓取，并在 result.profileId 标注 Key 的真实归属：按它落库，避免 Key
        // 脱离账号挂到无名行；后端没给出归属（profileId 为空）或一个 Key
        // 都没取到时，就当作「没有」——不落库。后端已把每个账号的失败原因写回
        // 它自己的缓存行，界面在账号下方显示「读取失败」，顶层不再挂诊断。
        try {
          const result = await runCommand<FetchSiteModelsResult>("fetch_site_models_json", {
            url: baseUrl,
            siteId: requestedSite.id,
          });
          const ownerId = result.profileId || "";
          const keys = result.keys ?? [];
          // 「没有」就不落库，但也不能提前 return：还得往下读缓存，
          // 否则刚写回各账号的失败原因没机会渲染成账号下方的「读取失败」。
          if (ownerId && keys.length > 0) {
            // 同步 Key 成功获取数据后，保存前清理掉这个站点原来的对应旧数据，避免数据冲突与旧 Key 残留
            await runCommand("clear_site_model_cache_for_site", { siteId: requestedSite.id });
            await runCommand("save_site_model_cache_for_account", {
              siteId: requestedSite.id,
              account: {
                profileId: ownerId,
                profileName: "",
                accountName: "",
                username: "",
                keys,
                keyGroups: result.keyGroups ?? {},
                keyModels: result.keyModels ?? {},
                error: "",
              },
              result,
              preserveKeys: false,
            });
          }
        } catch {
          // 不在顶层提示：单个账号的读取失败由后端写回各账号行、在账号下方显示。
          // 这里只是「没取到 Key」，不额外弹一条全局诊断。
        }
      } else {
        let clearedOldSiteData = false;
        for (const session of sessions) {
          if (requestId !== liveFetchRequestId) return;
          try {
            const result = await runCommand<FetchSiteModelsResult>("fetch_site_models_json", {
              url: baseUrl,
              siteId: requestedSite.id,
              profileId: session.profileId,
            });
            // 同步 Key 成功获取数据后，首次保存前清理掉这个站点原来的对应旧数据，避免数据冲突与旧 Key 残留
            if (!clearedOldSiteData) {
              await runCommand("clear_site_model_cache_for_site", { siteId: requestedSite.id });
              clearedOldSiteData = true;
            }
            await runCommand("save_site_model_cache_for_account", {
              siteId: requestedSite.id,
              account: {
                profileId: session.profileId,
                profileName: session.profileName,
                accountName: session.accountName,
                username: session.username,
                keys: result.keys ?? [],
                keyGroups: result.keyGroups ?? {},
                keyModels: result.keyModels ?? {},
                error: "",
              },
              result,
              preserveKeys: false,
            });
          } catch (error) {
            await runCommand("save_site_model_cache_for_account", {
              siteId: requestedSite.id,
              account: {
                profileId: session.profileId,
                profileName: session.profileName,
                accountName: session.accountName,
                username: session.username,
                keys: [],
                keyGroups: {},
                keyModels: {},
                error: String(error),
              },
              result: null,
              preserveKeys: false,
            });
          }
        }
      }
    } else if (mode === "models") {
      // 以缓存中的 Key 集合为准逐 Key 拉取 /v1/models（含手动添加的 Key，
      // 与其它入口的「同步 Key」语义一致：不重建 Key 列表，只刷新模型映射）。
      // 站点健康度由后端并发抓取（与 Key 是两条独立通道，任一方失败都不牵连
      // 另一方）；这里不需要单独去拉，下面读缓存即是最新数据。
      const result = await runCommand<FetchSiteModelsResult>("sync_models_for_cached_keys", {
        siteId: requestedSite.id,
      });
      if (requestId !== liveFetchRequestId) return;
      // 模型拉失败时后端仍会返回结果（错误在 errors 里），健康度照样跟着回来；
      // 只有下面这行报错提示，不能因此中断后面的读缓存。
      if (result.errors?.length && liveKeyCount.value === 0) {
        liveError.value = result.errors.join("\n");
      }
    }
    await store.loadLibrary();
    const cached = await readCachedModels(requestedSite.id);
    if (requestId !== liveFetchRequestId) return;
    // 已经报出具体原因（同步失败 / 无可用账号）时不再用通用文案盖掉。
    if (!liveError.value && !cached) {
      liveError.value = "暂无本地模型数据，请先同步或手动添加 Key。";
    } else if (
      !liveError.value &&
      liveModels.value.length === 0 &&
      liveKeyCount.value === 0 &&
      // 账号行已经各自显示「读取失败」时，顶层不再重复一条泛化提示。
      // 有健康度数据时同理：那一轮模型没拉成、但健康度到了，不该整屏只报失败。
      !liveAccountKeys.value.some((account) => account.error) &&
      !hasModelHealth.value
    ) {
      liveError.value = "本地同步数据中没有可用 Key 或模型。";
    }
  } catch (error) {
    if (requestId === liveFetchRequestId) liveError.value = String(error);
  } finally {
    if (requestId === liveFetchRequestId) {
      liveFetching.value = false;
      liveFetchingKind.value = null;
    }
  }
}

async function copyModelId(modelId: string) {
  await store.copyAddress(modelId, "模型标识");
}

async function copyApiKey(key: string, index: number, accountName: string) {
  await store.copyAddress(key, `${accountName} API Key ${index + 1}`);
}

function accountLabel(account: LiveAccountKeys): string {
  return account.username || account.accountName || account.profileName || "未命名账号";
}

function accountDetail(account: LiveAccountKeys): string {
  // 用户名优先作为主标签；副标签再展示 Chrome 账号与配置名，便于区分同配置下的多账号。
  if (account.username) {
    return [account.accountName, account.profileName]
      .filter((value) => value && value !== account.username)
      .join(" · ");
  }
  return account.profileName || "";
}

function maskApiKey(key: string): string {
  const value = key.trim();
  if (!value) return "—";
  if (value.length <= 6) return `${"•".repeat(6)}`;
  const prefixLength = value.startsWith("sk-") ? 7 : 4;
  const suffixLength = Math.min(4, Math.max(2, Math.floor(value.length / 8)));
  if (value.length <= prefixLength + suffixLength) {
    return `${value.slice(0, 4)}${"•".repeat(6)}`;
  }
  return `${value.slice(0, prefixLength)}${"•".repeat(8)}${value.slice(-suffixLength)}`;
}

function keyGroup(account: LiveAccountKeys, key: string): string {
  return account.keyGroups?.[key]?.trim() || "默认分组";
}

/** 点击 Key 行时切换选中态，右侧模型列表随之联动。 */
function selectKey(key: string) {
  selectedKeyId.value = selectedKeyId.value === key ? null : key;
}

/** 某个 Key 对应的模型数量；没有映射数据时返回 null。 */
function keyModelCount(key: string): number | null {
  for (const account of liveAccountKeys.value) {
    const models = account.keyModels?.[key];
    if (models) return models.length;
  }
  return null;
}

// —— 手动管理 Key ——
/** 正在添加 Key 的账号（profileId），控制行内输入框显示。 */
const addingKeyForProfile = ref<string | null>(null);
const newKeyGroup = ref("");
const newKeyValue = ref("");
const newKeySaving = ref(false);
/** 正在删除的 Key（账号-键），控制删除按钮禁用态。 */
const removingKeyId = ref<string | null>(null);

function startAddKey(account: LiveAccountKeys) {
  addingKeyForProfile.value = account.profileId || account.accountName || "";
  newKeyGroup.value = "";
  newKeyValue.value = "";
}

function cancelAddKey() {
  addingKeyForProfile.value = null;
  newKeyGroup.value = "";
  newKeyValue.value = "";
}

/** 某账号是否处于「正在添加 Key」状态。 */
function isAddingKey(account: LiveAccountKeys): boolean {
  return addingKeyForProfile.value === (account.profileId || account.accountName || "");
}

function accountProfileId(account: LiveAccountKeys): string {
  return account.profileId || account.accountName || "";
}

async function submitAddKey(account: LiveAccountKeys) {
  const siteId = site.value?.id;
  const profileId = account.profileId || "";
  const group = newKeyGroup.value.trim();
  // 支持一次粘贴多个 Key：按换行/逗号/分号切分
  const keys = newKeyValue.value
    .split(/[\n,;，；]/)
    .map((item) => item.trim())
    .filter(Boolean);
  if (!siteId) return;
  if (keys.length === 0) {
    showToast("请输入要添加的 Key", true);
    return;
  }
  newKeySaving.value = true;
  try {
    let addedCount = 0;
    for (const key of keys) {
      const added = await runCommand<boolean>("add_site_model_cache_key", {
        siteId,
        profileId,
        key,
        groupName: group,
        profileName: account.profileName || "",
        username: account.username || account.accountName || "",
      });
      if (added) addedCount += 1;
    }
    addingKeyForProfile.value = null;
    newKeyGroup.value = "";
    newKeyValue.value = "";
    await store.loadLibrary();
    await readCachedModels(siteId);
    showToast(
      keys.length > addedCount
        ? `已添加 ${addedCount} 个 Key，${keys.length - addedCount} 个已存在被跳过`
        : `已添加 ${addedCount} 个 Key`,
    );
  } catch (error) {
    showToast(`添加 Key 失败：${String(error)}`, true);
  } finally {
    newKeySaving.value = false;
  }
}

async function removeKey(account: LiveAccountKeys, key: string) {
  const siteId = site.value?.id;
  const profileId = account.profileId || "";
  if (!siteId) return;
  const ok = await confirm({
    title: "删除 API Key",
    message: `确定删除 Key「${maskApiKey(key)}」吗？该 Key 的分组与模型数据将一并移除。`,
    confirmText: "删除",
    danger: true,
  });
  if (!ok) return;
  removingKeyId.value = `${profileId}:${key}`;
  try {
    await runCommand<boolean>("remove_site_model_cache_key", {
      siteId,
      profileId,
      key,
    });
    if (selectedKeyId.value === key) selectedKeyId.value = null;
    await store.loadLibrary();
    await readCachedModels(siteId);
    showToast("Key 已删除");
  } catch (error) {
    showToast(`删除 Key 失败：${String(error)}`, true);
  } finally {
    removingKeyId.value = null;
  }
}
</script>

<template>
  <Teleport to="body">
    <div
      class="site-models-backdrop"
      id="site-models-dialog"
      :hidden="!store.siteModelsDialogOpen.value"
      @click="onBackdropClick"
    >
      <section
        class="site-models-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="site-models-title"
      >
        <header class="site-models-header">
          <div class="site-models-site">
            <div class="site-models-avatar" aria-hidden="true">{{ logo }}</div>
            <div class="site-models-site-meta">
              <h2 id="site-models-title" class="site-models-title">
                <span class="site-models-name">{{ site?.name || "站点" }}</span>
                <span v-if="site?.systemType" class="site-models-badge">{{ systemTypeLabel(site.systemType) }}</span>
              </h2>
              <p class="site-models-url" :title="site?.apiBaseUrl">{{ site?.apiBaseUrl }}</p>
            </div>
          </div>

          <div class="site-models-actions">
            <!-- 导入反代/移除反代：站点已有关联渠道时切换为移除；已跑路站点不允许导入 -->
            <button
              v-if="site && (siteProxy || !site.isRunaway)"
              type="button"
              class="site-models-text-btn site-models-proxy-btn"
              :class="{ 'is-active': !!siteProxy }"
              :disabled="proxyBusy"
              :title="siteProxy
                ? `移除反代：删除站点「${site.name}」对应的反代渠道（站点库数据不受影响）`
                : `导入反代：将站点「${site.name}」导入模型反代网关（运行时使用关联站点 Key）`"
              @click="siteProxy ? removeSiteProxy() : importSiteProxy()"
            >
              <span v-html="icons.repeat" />
              <span>{{ siteProxy ? "移除反代" : "导入反代" }}</span>
            </button>
            <!-- 同步 Key：重新拉取站点 API Key 列表；未知架构站点（无 NewAPI Key 体系）不提供该入口 -->
            <button
              v-if="site && !isUnknownSystemType(site.systemType)"
              type="button"
              class="site-models-text-btn"
              :disabled="liveFetching"
              :aria-label="liveFetchingKind === 'keys' ? '正在同步 Key' : '同步 Key：拉取站点 API Key 列表'"
              title="同步 Key：拉取站点 API Key 列表"
              @click="refreshModels('keys')"
            >
              <span v-html="icons.key" :class="{ 'site-models-spin': liveFetchingKind === 'keys' }" />
              <span>同步 Key</span>
            </button>
            <button
              type="button"
              class="site-models-text-btn"
              :disabled="liveFetching"
              :aria-label="liveFetchingKind === 'models' ? '正在同步模型' : '同步模型：按 Key 逐个拉取 /v1/models 并保存'"
              title="同步模型：按 Key 逐个拉取 /v1/models 并保存"
              @click="refreshModels('models')"
            >
              <span v-html="icons.restore" :class="{ 'site-models-spin': liveFetchingKind === 'models' }" />
              <span>同步模型</span>
            </button>
            <button
              ref="closeBtnRef"
              type="button"
              class="site-models-icon-btn site-models-close"
              aria-label="关闭模型窗口"
              title="关闭"
              @click="close"
              v-html="icons.close"
            />
          </div>
        </header>

        <div class="site-models-main">
          <aside class="site-models-side" aria-label="账号与 API Key">
            <div class="site-models-side-head">
              <span v-html="icons.key" />
              <strong>账号与 Key</strong>
              <small>{{ liveKeyCount }}</small>
            </div>

            <div class="site-models-side-body">
              <div class="site-models-side-section">
                <div class="site-models-side-label">
                  <span>账号与 Key</span>
                  <small>{{ liveAccountKeys.length }} / {{ liveKeyCount }}</small>
                </div>
                <div class="site-models-side-scroll">
                  <template v-if="liveAccountKeys.length > 0">
                    <div
                      v-for="account in liveAccountKeys"
                      :key="account.profileId || account.accountName"
                      class="site-models-tree-node"
                    >
                      <div
                        class="site-models-tree-parent"
                        :title="[accountLabel(account), accountDetail(account)].filter(Boolean).join(' · ') || '未命名账号'"
                      >
                        <span v-html="icons.user" />
                        <strong>
                          {{ accountLabel(account) }}<span
                            v-if="accountDetail(account)"
                            class="site-models-account-user"
                            >（{{ accountDetail(account) }}）</span
                          >
                        </strong>
                        <small>{{ account.keys.length }}</small>
                        <button
                          type="button"
                          class="site-models-key-manage"
                          :aria-label="`为 ${accountLabel(account)} 添加 API Key`"
                          title="添加 Key"
                          @click.stop="startAddKey(account)"
                        >
                          <span v-html="icons.plus" />
                        </button>
                      </div>
                      <div v-if="isAddingKey(account)" class="site-models-key-add">
                        <input
                          v-model="newKeyGroup"
                          type="text"
                          class="site-models-key-add-group"
                          placeholder="分组名"
                          :disabled="newKeySaving"
                          @keydown.enter.prevent="submitAddKey(account)"
                          @keydown.esc.prevent="cancelAddKey"
                        />
                        <textarea
                          v-model="newKeyValue"
                          class="site-models-key-add-value"
                          rows="2"
                          placeholder="粘贴 API Key，可一次粘贴多个（换行/逗号分隔）"
                          :disabled="newKeySaving"
                          @keydown.enter.exact.prevent="submitAddKey(account)"
                          @keydown.esc.prevent="cancelAddKey"
                        />
                        <button
                          type="button"
                          class="site-models-key-add-confirm"
                          :disabled="newKeySaving || !newKeyValue.trim()"
                          title="确认添加"
                          @click="submitAddKey(account)"
                        >
                          <span v-html="icons.check" />
                        </button>
                        <button
                          type="button"
                          class="site-models-key-add-cancel"
                          :disabled="newKeySaving"
                          title="取消"
                          @click="cancelAddKey"
                          v-html="icons.close"
                        />
                      </div>
                      <div class="site-models-tree-children">
                        <p v-if="account.error" class="site-models-account-error">{{ account.error }}</p>
                        <template v-if="account.keys.length > 0">
                          <div
                            v-for="(key, keyIndex) in account.keys"
                            :key="`${account.profileId || account.accountName}-${key}-${keyIndex}`"
                            class="site-models-key-row"
                            :class="{ 'is-selected': selectedKeyId === key }"
                            :title="keyGroup(account, key)"
                            @click="selectKey(key)"
                          >
                            <div class="site-models-key-meta">
                              <span class="site-models-key-line">
                                <code>{{ maskApiKey(key) }}</code>
                                <small class="site-models-key-group">{{
                                  keyGroup(account, key)
                                }}</small>
                              </span>
                              <small
                                v-if="keyModelCount(key) !== null"
                                class="site-models-key-model-count"
                              >{{ keyModelCount(key) }} 个模型</small>
                            </div>
                            <button
                              type="button"
                              class="site-models-copy"
                              :aria-label="`复制 ${accountLabel(account)} 的 API Key ${keyIndex + 1}`"
                              title="复制 Key"
                              @click.stop="copyApiKey(key, keyIndex, accountLabel(account))"
                            >
                              <span v-html="icons.copy" />
                            </button>
                            <button
                              type="button"
                              class="site-models-copy site-models-key-remove"
                              :disabled="removingKeyId === `${accountProfileId(account)}:${key}`"
                              :aria-label="`删除 ${accountLabel(account)} 的 API Key ${keyIndex + 1}`"
                              title="删除 Key"
                              @click.stop="removeKey(account, key)"
                            >
                              <span v-html="icons.trash" />
                            </button>
                          </div>
                        </template>
                        <p v-else-if="!account.error" class="site-models-side-empty">暂无 Key</p>
                      </div>
                    </div>
                  </template>
                  <p v-else class="site-models-side-empty">暂无账号</p>
                </div>
              </div>
            </div>
          </aside>

          <div class="site-models-panel">
            <div class="site-models-panel-head">
              <div class="site-models-panel-top">
                <div class="site-models-panel-title">
                  <strong>{{ selectedKeyId ? "选中 Key 的模型" : "支持的模型" }}</strong>
                  <small class="site-models-source">{{
                    selectedKeyId ? maskApiKey(selectedKeyId) : apiSourceLabel
                  }}</small>
                  <span class="site-models-count">{{ modelCountLabel }}</span>
                </div>

                <div class="site-models-panel-tools">
                  <label class="site-models-search">
                    <span v-html="icons.search" />
                    <input
                      v-model="searchQuery"
                      type="text"
                      placeholder="搜索模型标识或厂商…"
                    />
                    <button
                      v-if="searchQuery"
                      type="button"
                      class="site-models-search-clear"
                      aria-label="清除搜索"
                      @click="searchQuery = ''"
                      v-html="icons.close"
                    />
                  </label>

                  <!-- 隐藏无效模型：成功率极低（含 0%）。
                       开关态用眼睛图标区分（隐藏中＝划掉的眼睛）。 -->
                  <button
                    type="button"
                    class="site-models-text-btn site-models-filter-btn"
                    :class="{ 'is-active': hideUnhealthy }"
                    :aria-pressed="hideUnhealthy"
                    :aria-label="hideUnhealthyTitle"
                    :title="hideUnhealthyTitle"
                    @click="hideUnhealthy = !hideUnhealthy"
                  >
                    <span v-html="hideUnhealthy ? icons.eyeOff : icons.eye" />
                    <span>隐藏无效{{ hideUnhealthy && hiddenUnhealthyCount > 0 ? `（${hiddenUnhealthyCount}）` : "" }}</span>
                  </button>
                </div>
              </div>

              <p v-if="hasModelHealth" class="site-models-health-note">
                数字徽标为<b>最近有流量时段</b>的成功率（与色条最后一个亮块一致）；色条为站点上报的<b>全站</b>口径{{ healthNoteWindow }}逐时段成功率（与当前 Key
                无关，仅作健康度参考）：绿 ≥95%，浅绿 85%~95%，黄 70%~85%，橙 50%~70%，红 &lt;50%，灰 = 该时段无流量，悬停可看具体时段与请求量；能否使用以该 Key 的模型列表为准。
              </p>
            </div>

            <div class="site-models-scroll">
              <div v-if="liveFetching" class="site-models-state">
                <span class="site-models-state-icon site-models-spin" v-html="icons.restore" />
                <strong>正在读取本地数据</strong>
                <p>读取 {{ site?.name }} 的 Key 与模型信息…</p>
              </div>

              <div v-else-if="liveError" class="site-models-state site-models-state-error">
                <span class="site-models-state-icon" v-html="icons.info" />
                <strong>读取失败</strong>
                <p>{{ liveError }}</p>
              </div>

              <div v-else-if="currentActiveModels.length === 0" class="site-models-state">
                <span class="site-models-state-icon" v-html="icons.database" />
                <strong>{{ selectedKeyId ? "该 Key 暂无可用模型" : "暂无模型数据" }}</strong>
                <p>{{ selectedKeyId ? "该 Key 没有可用或同步到的模型数据。" : "本地同步数据中没有可用模型。" }}</p>
              </div>

              <div v-else-if="filteredLiveModels.length === 0" class="site-models-state">
                <span class="site-models-state-icon" v-html="icons.search" />
                <strong>{{ hideUnhealthy && !searchQuery.trim() ? "当前全部模型都无效" : "未找到匹配模型" }}</strong>
                <p v-if="hideUnhealthy && !searchQuery.trim()">
                  已隐藏 {{ currentActiveModels.length }} 个成功率极低的模型，关闭「隐藏无效」可查看。
                </p>
                <p v-else>试试其他关键词{{ hideUnhealthy ? "，或关闭「隐藏无效」扩大范围" : "" }}。</p>
              </div>

              <div v-else class="site-models-grid">
                <button
                  v-for="model in filteredLiveModels"
                  :key="model.id"
                  type="button"
                  class="site-models-item"
                  :title="healthBadgeOf(model.id)?.title || `复制模型 ID：${model.id}`"
                  @click="copyModelId(model.id)"
                >
                  <span class="site-models-item-info">
                    <span class="site-models-item-head">
                      <strong :title="model.id">{{ model.id }}</strong>
                      <!-- 拷贝图标紧跟模型 ID：它复制的就是这段文本，
                           放卡片最右会离复制对象太远（整卡可点，图标只是提示）。 -->
                      <span class="site-models-item-copy" v-html="icons.copy" aria-hidden="true" />
                      <span
                        v-if="healthBadgeOf(model.id)"
                        class="site-models-item-health-value"
                        :class="`is-lv${healthBadgeOf(model.id)!.level}`"
                        :title="healthBadgeOf(model.id)!.title"
                      >{{ healthBadgeOf(model.id)!.label }}</span>
                    </span>
                    <!-- 状态条：每个模型都画满 24 格——有流量的时段上色，
                         没流量的时段留灰（模型没有任何流量时整条全灰）。 -->
                    <span v-if="hasStatusStrip" class="site-models-item-health">
                      <span
                        class="site-models-item-health-strip"
                        role="img"
                        :aria-label="statusStripLabel"
                        :title="statusStripTitle"
                      >
                        <span
                          v-for="slot in statusStripOf(model.id)"
                          :key="slot.ts"
                          class="site-models-item-health-slot"
                          :class="slot.level ? `is-lv${slot.level}` : 'is-idle'"
                          :title="slot.title"
                        />
                      </span>
                    </span>
                  </span>
                </button>
              </div>
            </div>
          </div>
        </div>
      </section>
    </div>
  </Teleport>
</template>
