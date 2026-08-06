<script setup lang="ts">
import { computed, ref, watch, nextTick } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import { logoText } from "../utils";

interface LiveModelItem {
  id: string;
  owned_by?: string;
  ownedBy?: string;
}

type ModelApiSource = "newapi-key" | "sub2api-key" | "pricing" | "models" | "none";

interface LiveAccountKeys {
  profileId: string;
  profileName: string;
  accountName: string;
  username: string;
  keys: string[];
  error: string;
}

const isTauri = "__TAURI_INTERNALS__" in window;
const store = useStore();
const closeBtnRef = ref<HTMLButtonElement>();
const searchQuery = ref("");
const liveFetching = ref(false);
const liveError = ref("");
const liveModels = ref<LiveModelItem[]>([]);
const liveAccountKeys = ref<LiveAccountKeys[]>([]);
const apiSource = ref<ModelApiSource>("none");
let liveFetchRequestId = 0;

const site = computed(() => store.siteModelsSite.value);
const liveKeyCount = computed(() =>
  liveAccountKeys.value.reduce((total, account) => total + account.keys.length, 0),
);

const logo = computed(() =>
  site.value ? logoText(site.value.apiBaseUrl, site.value.name) : "",
);

const filteredLiveModels = computed(() => {
  const q = searchQuery.value.trim().toLowerCase();
  if (!q) return liveModels.value;
  return liveModels.value.filter(
    (m) => m.id.toLowerCase().includes(q) || (m.owned_by && m.owned_by.toLowerCase().includes(q)),
  );
});

interface LocalSiteModelCache {
  models: LiveModelItem[];
  apiSource: ModelApiSource;
  accounts: LiveAccountKeys[];
}

async function readCachedModels(siteId: string): Promise<boolean> {
  if (!isTauri) return false;
  try {
    const data = await invoke<LocalSiteModelCache>("get_site_model_cache", { siteId });
    if (!data || !Array.isArray(data.models)) return false;
    liveModels.value = (data.models as LiveModelItem[]).map((model: LiveModelItem) => ({
      ...model,
      owned_by: model.owned_by || model.ownedBy,
    }));
    apiSource.value = data.apiSource || "none";
    const accounts = Array.isArray(data.accounts) ? data.accounts : [];
    liveAccountKeys.value = accounts;
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
      void refreshModels();
    } else {
      liveFetchRequestId += 1;
      liveFetching.value = false;
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

async function refreshModels() {
  const requestedSite = site.value;
  if (!requestedSite) return;
  const requestId = ++liveFetchRequestId;
  liveFetching.value = true;
  liveError.value = "";
  liveModels.value = [];
  liveAccountKeys.value = [];
  apiSource.value = "none";
  try {
    const cached = await readCachedModels(requestedSite.id);
    if (requestId !== liveFetchRequestId) return;
    if (!cached) {
      liveError.value = "暂无本地模型数据，请先执行同步会话。";
    } else if (liveModels.value.length === 0 && liveKeyCount.value === 0) {
      liveError.value = "本地同步数据中没有可用 Key 或模型。";
    }
  } catch (error) {
    if (requestId === liveFetchRequestId) liveError.value = String(error);
  } finally {
    if (requestId === liveFetchRequestId) liveFetching.value = false;
  }
}

async function copyModelId(modelId: string) {
  await store.copyAddress(modelId, "模型标识");
}

async function copyApiKey(key: string, index: number, accountName: string) {
  await store.copyAddress(key, `${accountName} API Key ${index + 1}`);
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
        <header class="dialog-header">
          <div class="header-left">
            <div class="site-avatar">{{ logo }}</div>
            <div>
              <h2 id="site-models-title">
                {{ site?.name || "站点" }}
                <span v-if="site?.systemType" class="system-badge">{{ site.systemType }}</span>
              </h2>
              <p class="site-url">{{ site?.apiBaseUrl }}</p>
            </div>
          </div>

          <button
            ref="closeBtnRef"
            class="close-button"
            type="button"
            aria-label="关闭模型窗口"
            @click="close"
            v-html="icons.close"
          />
        </header>

        <div class="dialog-toolbar">
          <label class="models-search-input">
            <span v-html="icons.search" />
            <input
              v-model="searchQuery"
              type="text"
              placeholder="搜索模型标识或厂商…"
            />
          </label>

          <button
            type="button"
            class="fetch-live-btn"
            :disabled="liveFetching"
            @click="refreshModels"
          >
            <span v-html="icons.restore" />
            <span>{{ liveFetching ? '读取中…' : '重新读取本地数据' }}</span>
          </button>
        </div>

        <div class="dialog-body">
          <section
            v-if="liveAccountKeys.length > 0 || liveModels.length > 0"
            class="site-api-keys"
            aria-label="可用 API Key"
          >
            <header>
              <span v-html="icons.key" />
              <strong>可用 Key</strong>
              <small>{{ liveKeyCount }} 个</small>
            </header>
            <div class="site-api-key-list">
              <div
                v-for="account in liveAccountKeys"
                :key="account.profileId || account.accountName"
                class="site-api-key-account"
              >
                <div class="site-api-key-account-header">
                  <span v-html="icons.user" />
                  <strong :title="account.accountName || account.profileName">
                    {{ account.accountName || account.profileName }}<span v-if="account.username">（{{ account.username }}）</span>
                  </strong>
                  <small>{{ account.keys.length }} 个</small>
                </div>
                <div v-for="(key, index) in account.keys" :key="key" class="site-api-key-row">
                  <code :title="key">{{ key }}</code>
                  <button
                    type="button"
                    class="copy-icon-btn"
                    :aria-label="`复制 ${account.accountName || account.profileName} 的 API Key ${index + 1}`"
                    title="复制 Key"
                    @click="copyApiKey(key, index, account.accountName || account.profileName)"
                  >
                    <span v-html="icons.copy" />
                  </button>
                </div>
                <p v-if="account.keys.length === 0" class="site-api-key-empty" :title="account.error">
                  {{ account.error ? "Key 同步失败" : "此账号没有可用 Key" }}
                </p>
              </div>
              <p v-if="liveAccountKeys.length === 0" class="site-api-key-empty">未读取到可用 Key</p>
            </div>
          </section>

          <div v-if="liveModels.length > 0" class="models-section">
            <div class="section-title">
              <span class="local-dot" />
              <span>
                本地模型列表
                <small v-if="apiSource === 'newapi-key'">（通过 NewAPI Key 获取，共 {{ filteredLiveModels.length }} 个）</small>
                <small v-else-if="apiSource === 'sub2api-key'">（通过 Sub2API Key 获取，共 {{ filteredLiveModels.length }} 个）</small>
                <small v-else-if="apiSource === 'pricing'">（同步自站点定价数据，共 {{ filteredLiveModels.length }} 个）</small>
                <small v-else-if="apiSource === 'models'">（同步自站点模型数据，共 {{ filteredLiveModels.length }} 个）</small>
              </span>
            </div>

            <div class="models-chips-grid">
              <div
                v-for="model in filteredLiveModels"
                :key="model.id"
                class="model-item-chip"
                @click="copyModelId(model.id)"
              >
                <div class="model-item-info">
                  <strong>{{ model.id }}</strong>
                  <div v-if="model.owned_by" class="model-sub-meta">
                    <small>by {{ model.owned_by }}</small>
                  </div>
                </div>
                <button type="button" class="copy-icon-btn" title="复制模型标识">
                  <span v-html="icons.copy" />
                </button>
              </div>
            </div>
          </div>

          <div v-else-if="liveFetching" class="loading-models-banner">
            <span v-html="icons.restore" class="spin-icon" />
            <span>正在读取 {{ site?.name }} 的本地 Key 与模型数据…</span>
          </div>

          <div v-if="liveError" class="live-error-banner">
            <span v-html="icons.info" />
            <span>{{ liveError }}</span>
          </div>

        </div>
      </section>
    </div>
  </Teleport>
</template>
