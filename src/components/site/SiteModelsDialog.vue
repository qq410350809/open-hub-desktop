<script setup lang="ts">
import { computed, ref, watch, nextTick } from "vue";
import { runCommand, useLibrary } from "../../composables/useLibrary";
import { icons } from "../../icons";
import { useStore } from "../../composables/useStore";
import { logoText } from "../../utils";
import { isUnknownSystemType, systemTypeLabel } from "../../types";
import { useToast } from "../../composables/core/useToast";
import { useConfirm } from "../../composables/ui/useConfirm";

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
const searchQuery = ref("");
const liveFetching = ref(false);
/** 当前正在执行的同步类型；用于区分 Key/模型按钮各自的转圈状态。 */
const liveFetchingKind = ref<"keys" | "models" | null>(null);
const liveError = ref("");
const liveModels = ref<LiveModelItem[]>([]);
const liveAccountKeys = ref<LiveAccountKeys[]>([]);
const apiSource = ref<ModelApiSource>("none");
/** 当前选中的 API Key；选中后右侧只显示该 Key 对应的模型。 */
const selectedKeyId = ref<string | null>(null);
let liveFetchRequestId = 0;

const site = computed(() => store.siteModelsSite.value);
const liveKeyCount = computed(() =>
  liveAccountKeys.value.reduce((total, account) => total + account.keys.length, 0),
);

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

const filteredLiveModels = computed(() => {
  const source = currentActiveModels.value;
  const q = searchQuery.value.trim().toLowerCase();
  if (!q) return source;
  return source.filter(
    (m) => m.id.toLowerCase().includes(q) || (m.owned_by && m.owned_by.toLowerCase().includes(q)),
  );
});

const modelCountLabel = computed(() => {
  const source = currentActiveModels.value;
  const total = source.length;
  const q = searchQuery.value.trim();
  return q ? `${filteredLiveModels.value.length} / ${total}` : String(total);
});

interface LocalSiteModelCache {
  models: LiveModelItem[];
  apiSource: ModelApiSource;
  accounts: LiveAccountKeys[];
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
    apiSource.value = data.apiSource || "none";
    const accounts = Array.isArray(data.accounts) ? data.accounts : [];
    liveAccountKeys.value = accounts
      .slice()
      .sort((a, b) =>
        (a.username || a.accountName || a.profileName || "").localeCompare(
          b.username || b.accountName || b.profileName || "",
          undefined,
          { numeric: true, sensitivity: "base" }
        )
      );
    return accounts.length > 0 || liveModels.value.length > 0;
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
      liveError.value = "";
      searchQuery.value = "";
      apiSource.value = "none";
      selectedKeyId.value = null;
      addingKeyForProfile.value = null;
      newKeyGroup.value = "";
      newKeyValue.value = "";
      void refreshModels();
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
      const sessions = siteUsage?.sessions?.filter((s) => s.isValid) ?? [];
      let baseUrl = requestedSite.apiBaseUrl.trim();
      if (!baseUrl.endsWith("/")) baseUrl += "/";
      if (sessions.length === 0) {
        // 没有有效账号，尝试不带 profileId 请求。
        try {
          const result = await runCommand<FetchSiteModelsResult>("fetch_site_models_json", {
            url: baseUrl,
            siteId: requestedSite.id,
          });
          // 同步 Key 成功获取数据后，保存前清理掉这个站点原来的对应旧数据，避免数据冲突与旧 Key 残留
          await runCommand("clear_site_model_cache_for_site", { siteId: requestedSite.id });
          await runCommand("save_site_model_cache_for_account", {
            siteId: requestedSite.id,
            account: {
              profileId: "",
              profileName: "",
              accountName: "",
              username: "",
              keys: result.keys ?? [],
              keyGroups: result.keyGroups ?? {},
              keyModels: result.keyModels ?? {},
              error: "",
            },
            result,
            preserveKeys: false,
          });
        } catch {
          /* 忽略，继续读缓存 */
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
      const result = await runCommand<FetchSiteModelsResult>("sync_models_for_cached_keys", {
        siteId: requestedSite.id,
      });
      if (requestId !== liveFetchRequestId) return;
      if (result.errors?.length && liveKeyCount.value === 0) {
        liveError.value = result.errors.join("\n");
      }
    }
    await store.loadLibrary();
    const cached = await readCachedModels(requestedSite.id);
    if (requestId !== liveFetchRequestId) return;
    if (!cached) {
      liveError.value = "暂无本地模型数据，请先同步或手动添加 Key。";
    } else if (liveModels.value.length === 0 && liveKeyCount.value === 0) {
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
            <!-- 同步 Key：重新拉取站点 API Key 列表；未知架构站点无 Key 提取能力，不提供该入口 -->
            <button
              v-if="site && !isUnknownSystemType(site.systemType)"
              type="button"
              class="site-models-icon-btn"
              :disabled="liveFetching"
              :aria-label="liveFetchingKind === 'keys' ? '正在同步 Key' : '同步 Key：拉取站点 API Key 列表'"
              title="同步 Key：拉取站点 API Key 列表"
              @click="refreshModels('keys')"
            >
              <span v-html="icons.key" :class="{ 'site-models-spin': liveFetchingKind === 'keys' }" />
            </button>
            <button
              type="button"
              class="site-models-icon-btn"
              :disabled="liveFetching"
              :aria-label="liveFetchingKind === 'models' ? '正在同步模型' : '同步模型：按 Key 逐个拉取 /v1/models 并保存'"
              title="同步模型：按 Key 逐个拉取 /v1/models 并保存"
              @click="refreshModels('models')"
            >
              <span v-html="icons.restore" :class="{ 'site-models-spin': liveFetchingKind === 'models' }" />
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
                        <p v-else class="site-models-side-empty">暂无 Key</p>
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
              <div class="site-models-panel-title">
                <strong>{{ selectedKeyId ? "选中 Key 的模型" : "支持的模型" }}</strong>
                <small class="site-models-source">{{
                  selectedKeyId ? maskApiKey(selectedKeyId) : apiSourceLabel
                }}</small>
                <span class="site-models-count">{{ modelCountLabel }}</span>
              </div>

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
                <strong>未找到匹配模型</strong>
                <p>试试其他关键词。</p>
              </div>

              <div v-else class="site-models-grid">
                <button
                  v-for="model in filteredLiveModels"
                  :key="model.id"
                  type="button"
                  class="site-models-item"
                  :title="`复制模型 ID：${model.id}`"
                  @click="copyModelId(model.id)"
                >
                  <span class="site-models-item-info">
                    <strong :title="model.id">{{ model.id }}</strong>
                    <small v-if="model.owned_by" class="site-models-item-vendor"
                      >by {{ model.owned_by }}</small
                    >
                  </span>
                  <span class="site-models-item-copy" v-html="icons.copy" aria-hidden="true" />
                </button>
              </div>
            </div>
          </div>
        </div>
      </section>
    </div>
  </Teleport>
</template>
