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

interface SiteModelsResult {
  models: LiveModelItem[];
  source: ModelApiSource;
  keys: string[];
}

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
const sessionKeysBySite = new Map<string, LiveAccountKeys[]>();
const keyFetchCompletedSites = new Set<string>();
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

watch(
  () => store.siteModelsDialogOpen.value,
  (open) => {
    if (open) {
      nextTick(() => closeBtnRef.value?.focus());
      document.body.classList.add("modal-open");
      liveModels.value = [];
      liveAccountKeys.value = site.value ? (sessionKeysBySite.get(site.value.id) ?? []) : [];
      liveError.value = "";
      searchQuery.value = "";
      apiSource.value = "none";
      let hasCache = false;
      if (site.value) {
        try {
          const cached = localStorage.getItem(`openhub_models_${site.value.id}`);
          if (cached) {
            const data = JSON.parse(cached);
            if (data && Array.isArray(data.models)) {
              liveModels.value = data.models;
              apiSource.value = data.apiSource || "none";
              hasCache = true;
            }
          }
        } catch (e) {}
      }

      if (!hasCache) {
        void fetchLiveModels();
      } else if (
        site.value &&
        ["newapi-key", "sub2api-key"].includes(apiSource.value) &&
        !keyFetchCompletedSites.has(site.value.id)
      ) {
        void fetchLiveModels(false);
      }
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

function processAndCacheModels(
  rawList: any[],
  source: ModelApiSource,
  accounts: LiveAccountKeys[] = [],
) {
  const parsed = rawList
    .map((item: any) => ({
      id: typeof item === "string" ? item : String(item.model_name || item.id || item.name || item.model || item),
      owned_by: item.owner || item.owned_by || item.ownedBy || undefined,
    }))
    .filter((item, index, items) =>
      items.findIndex((candidate) => candidate.id === item.id) === index,
    );
  parsed.sort((a, b) => a.id.localeCompare(b.id));
  liveModels.value = parsed;
  liveAccountKeys.value = accounts.map((account) => ({
    ...account,
    keys: [...new Set(account.keys.map((key) => String(key).trim()).filter(Boolean))],
  }));
  apiSource.value = source;
  if (site.value) {
    sessionKeysBySite.set(site.value.id, liveAccountKeys.value);
    keyFetchCompletedSites.add(site.value.id);
    if (parsed.length > 0) {
      localStorage.setItem(`openhub_models_${site.value.id}`, JSON.stringify({
        models: parsed,
        apiSource: source
      }));
    }
  }
}

function onBackdropClick(event: MouseEvent) {
  if (event.target === event.currentTarget) close();
}

async function copyModelId(modelId: string) {
  await store.copyAddress(modelId, "模型标识");
}

async function copyApiKey(key: string, index: number, accountName: string) {
  await store.copyAddress(key, `${accountName} API Key ${index + 1}`);
}

async function fetchLiveModels(clearModels = true) {
  const requestedSite = site.value;
  if (!requestedSite) {
    liveFetching.value = false;
    return;
  }
  const requestId = ++liveFetchRequestId;
  let requestFinished = false;
  const requestIsCurrent = () => requestId === liveFetchRequestId;
  const finishRequest = () => {
    if (requestFinished) return;
    requestFinished = true;
    window.clearTimeout(timeoutId);
    if (requestIsCurrent()) liveFetching.value = false;
  };
  const timeoutId = window.setTimeout(() => {
    if (!requestIsCurrent()) return;
    liveFetchRequestId += 1;
    liveFetching.value = false;
    liveError.value = "模型与 Key 获取超时，请检查站点或 Chrome 验证状态后重试。";
  }, 120_000);
  liveFetching.value = true;
  liveError.value = "";
  liveAccountKeys.value = [];
  sessionKeysBySite.delete(requestedSite.id);
  keyFetchCompletedSites.delete(requestedSite.id);
  if (clearModels) {
    liveModels.value = [];
    apiSource.value = "none";
  }

  let baseUrl = requestedSite.apiBaseUrl.trim();
  if (!baseUrl.endsWith("/")) baseUrl += "/";

  // 1. 如果在 Tauri 桌面端运行，优先调用 Rust reqwest 后端命令（无 CORS / 无 Webview 限制）
  if (isTauri) {
    try {
      const sessions = (store.chromeUsageAccounts.value[requestedSite.id] ?? [])
        .filter((session) => session.isValid);
      const results: SiteModelsResult[] = [];
      const accounts: LiveAccountKeys[] = [];
      const accountErrors: string[] = [];

      if (sessions.length > 0) {
        for (const session of sessions) {
          try {
            const result = await invoke<SiteModelsResult>("fetch_site_models_json", {
              url: baseUrl,
              siteId: requestedSite.id,
              profileId: session.profileId,
            });
            if (!requestIsCurrent()) {
              finishRequest();
              return;
            }
            results.push(result);
            accounts.push({
              profileId: session.profileId,
              profileName: session.profileName,
              accountName: session.accountName,
              username: session.username,
              keys: result.keys ?? [],
              error: "",
            });
          } catch (error) {
            const message = String(error);
            accountErrors.push(`${session.accountName || session.profileName}：${message}`);
            accounts.push({
              profileId: session.profileId,
              profileName: session.profileName,
              accountName: session.accountName,
              username: session.username,
              keys: [],
              error: message,
            });
          }
        }
      } else {
        const result = await invoke<SiteModelsResult>("fetch_site_models_json", {
          url: baseUrl,
          siteId: requestedSite.id,
          profileId: null,
        });
        if (!requestIsCurrent()) {
          finishRequest();
          return;
        }
        results.push(result);
        if ((result.keys ?? []).length > 0) {
          accounts.push({
            profileId: "",
            profileName: "当前会话",
            accountName: "当前会话",
            username: "",
            keys: result.keys,
            error: "",
          });
        }
      }

      const models = results.flatMap((result) => result.models ?? []);
      const accountKeyCount = accounts.reduce((total, account) => total + account.keys.length, 0);
      if (models.length === 0 && accountKeyCount === 0) {
        throw new Error("接口返回了空数据，该站点可能未公开模型列表。");
      }
      const source = results.find((result) =>
        ["newapi-key", "sub2api-key"].includes(result.source),
      )?.source ?? results[0]?.source ?? "models";
      processAndCacheModels(models, source, accounts);
      finishRequest();
      await store.loadLibrary();
      if (!requestIsCurrent()) return;
      if (models.length === 0) {
        liveError.value = "Key 已同步，但模型接口未返回可用模型。";
      } else if (accountErrors.length > 0) {
        liveError.value = `部分账号 Key 同步失败：${accountErrors.join("；")}`;
      }
      return;
    } catch (err: any) {
      console.warn("Tauri fetch_site_models_json 请求失败", err);
      if (requestIsCurrent()) {
        liveError.value = String(err?.message || err || "接口拉取失败");
      }
      finishRequest();
      return; // Tauri 环境下请求失败直接返回，不再走浏览器的 Web fetch 降级（因为浏览器大概率也会跨域失败）
    }
  }

  // 2. 网页浏览器降级策略 (Web fetch)
  try {
    const pricingUrl = `${baseUrl}api/pricing`;
    const res = await fetch(pricingUrl, { signal: AbortSignal.timeout(6000) });
    if (!requestIsCurrent()) {
      finishRequest();
      return;
    }
    if (res.ok) {
      const json = await res.json();
      const rawList = Array.isArray(json?.data) ? json.data : Array.isArray(json) ? json : [];
      if (rawList.length > 0) {
        processAndCacheModels(rawList, "pricing");
        finishRequest();
        return;
      }
    }
  } catch {
    // 降级继续
  }

  try {
    const v1Url = `${baseUrl}v1/models`;
    const res = await fetch(v1Url, { signal: AbortSignal.timeout(6000) });
    if (!requestIsCurrent()) {
      finishRequest();
      return;
    }
    if (res.ok) {
      const json = await res.json();
      const rawList = Array.isArray(json?.data) ? json.data : Array.isArray(json) ? json : [];
      if (rawList.length > 0) {
        processAndCacheModels(rawList, "models");
        finishRequest();
        return;
      }
    }
    liveError.value = "接口未返回有效数据。";
  } catch (err: any) {
    if (requestIsCurrent() && !liveError.value) {
      liveError.value = err?.message || "无法拉取在线模型，接口可能需要 API Key 或存在网络限制（如跨域 CORS）";
    }
  } finally {
    finishRequest();
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
            @click="fetchLiveModels()"
          >
            <span v-html="icons.restore" />
            <span>{{ liveFetching ? '获取中…' : '刷新模型' }}</span>
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

          <!-- 在线接口获取到的模型 (NewAPI /api/pricing 或 /v1/models) -->
          <div v-if="liveModels.length > 0" class="models-section">
            <div class="section-title">
              <span class="live-dot" />
              <span>
                在线接口模型列表
                <small v-if="apiSource === 'newapi-key'">（通过 NewAPI Key 获取，共 {{ filteredLiveModels.length }} 个）</small>
                <small v-else-if="apiSource === 'sub2api-key'">（通过 Sub2API Key 获取，共 {{ filteredLiveModels.length }} 个）</small>
                <small v-if="apiSource === 'pricing'">（源自 NewAPI /api/pricing，共 {{ filteredLiveModels.length }} 个）</small>
                <small v-else-if="apiSource === 'models'">（源自 /v1/models，共 {{ filteredLiveModels.length }} 个）</small>
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
            <span>正在读取 {{ site?.name }} 的账号 Key 与实时模型列表…</span>
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
