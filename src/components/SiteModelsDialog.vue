<script setup lang="ts">
import { computed, ref, watch, nextTick } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import { logoText } from "../utils";

interface LiveModelItem {
  id: string;
  owned_by?: string;
  model_ratio?: number;
  completion_ratio?: number;
  model_price?: number;
  quota_type?: number;
  group?: string;
}

const isTauri = "__TAURI_INTERNALS__" in window;
const store = useStore();
const closeBtnRef = ref<HTMLButtonElement>();
const searchQuery = ref("");
const liveFetching = ref(false);
const liveError = ref("");
const liveModels = ref<LiveModelItem[]>([]);
const apiSource = ref<"pricing" | "models" | "none">("none");

const site = computed(() => store.siteModelsSite.value);

const logo = computed(() =>
  site.value ? logoText(site.value.apiBaseUrl, site.value.name) : "",
);

const parsedModels = computed(() => {
  if (!site.value) return [];

  const text = [
    site.value.name,
    site.value.description,
    site.value.systemType,
    ...site.value.tags,
  ]
    .join(" ")
    .toLowerCase();

  const list: Array<{ id: string; name: string; vendor: string }> = [];

  const modelMap: Array<{ id: string; name: string; vendor: string; kw: string[] }> = [
    { id: "gpt-4o", name: "GPT-4o", vendor: "OpenAI", kw: ["gpt-4o", "gpt-4", "gpt"] },
    { id: "gpt-4o-mini", name: "GPT-4o Mini", vendor: "OpenAI", kw: ["mini", "nano"] },
    { id: "o1-o3", name: "OpenAI o1 / o3", vendor: "OpenAI", kw: ["o1", "o3"] },
    { id: "claude-3-5-sonnet", name: "Claude 3.5 Sonnet", vendor: "Anthropic", kw: ["claude-3.5", "sonnet", "claude"] },
    { id: "claude-opus", name: "Claude Opus", vendor: "Anthropic", kw: ["opus"] },
    { id: "claude-code", name: "Claude Code", vendor: "Anthropic", kw: ["claude code", "claudecode"] },
    { id: "deepseek-v3", name: "DeepSeek-V3 / V4", vendor: "DeepSeek", kw: ["deepseek", "deepseek-v3", "deepseek-v4"] },
    { id: "deepseek-r1", name: "DeepSeek-R1", vendor: "DeepSeek", kw: ["deepseek-r1", "r1"] },
    { id: "gemini-1-5-pro", name: "Gemini 1.5 / 2.5 Pro", vendor: "Google", kw: ["gemini", "gemini-1.5", "gemini-2.5"] },
    { id: "gemini-flash", name: "Gemini Flash", vendor: "Google", kw: ["flash"] },
    { id: "grok-2", name: "Grok 2 / Grok 3", vendor: "xAI", kw: ["grok"] },
    { id: "qwen-max", name: "Qwen / 通义千问", vendor: "阿里", kw: ["qwen", "通义千问"] },
    { id: "glm-4", name: "GLM-4 / 智谱", vendor: "智谱AI", kw: ["glm"] },
    { id: "kimi", name: "Kimi / Moonshot", vendor: "月之暗面", kw: ["kimi", "moonshot"] },
    { id: "minimax", name: "MiniMax", vendor: "MiniMax", kw: ["minimax", "abab"] },
    { id: "mimo", name: "MiMo", vendor: "小米", kw: ["mimo"] },
    { id: "codex", name: "Codex", vendor: "OpenAI", kw: ["codex"] },
  ];

  for (const item of modelMap) {
    if (item.kw.some((k) => text.includes(k))) {
      list.push({ id: item.id, name: item.name, vendor: item.vendor });
    }
  }

  for (const tag of site.value.tags) {
    if (!list.some((m) => m.name.toLowerCase().includes(tag.toLowerCase()))) {
      list.push({ id: tag.toLowerCase(), name: tag, vendor: "标签" });
    }
  }

  return list;
});

const filteredModels = computed(() => {
  const q = searchQuery.value.trim().toLowerCase();
  if (!q) return parsedModels.value;
  return parsedModels.value.filter(
    (m) => m.name.toLowerCase().includes(q) || m.vendor.toLowerCase().includes(q) || m.id.toLowerCase().includes(q),
  );
});

const filteredLiveModels = computed(() => {
  const q = searchQuery.value.trim().toLowerCase();
  if (!q) return liveModels.value;
  return liveModels.value.filter(
    (m) => m.id.toLowerCase().includes(q) || (m.owned_by && m.owned_by.toLowerCase().includes(q)) || (m.group && m.group.toLowerCase().includes(q)),
  );
});

watch(
  () => store.siteModelsDialogOpen.value,
  (open) => {
    if (open) {
      nextTick(() => closeBtnRef.value?.focus());
      document.body.classList.add("modal-open");
      liveModels.value = [];
      liveError.value = "";
      searchQuery.value = "";
      apiSource.value = "none";
      // 打开弹窗自动请求 /api/pricing 接口
      fetchLiveModels();
    } else {
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

async function copyModelId(modelId: string) {
  await store.copyAddress(modelId, "模型标识");
}

async function fetchLiveModels() {
  if (!site.value) return;
  liveFetching.value = true;
  liveError.value = "";
  liveModels.value = [];
  apiSource.value = "none";

  let baseUrl = site.value.apiBaseUrl.trim();
  if (!baseUrl.endsWith("/")) baseUrl += "/";

  // 1. 如果在 Tauri 桌面端运行，优先调用 Rust reqwest 后端命令（无 CORS / 无 Webview 限制）
  if (isTauri) {
    try {
      const jsonStr = await invoke<string>("fetch_site_models_json", { url: baseUrl, siteId: site.value.id });
      if (!jsonStr || !jsonStr.trim()) {
        throw new Error("接口返回了空数据，该站点可能未公开模型列表。");
      }
      
      let json;
      try {
        json = JSON.parse(jsonStr);
      } catch (parseErr) {
        console.error("JSON Parse Error:", parseErr, "Response:", jsonStr.substring(0, 100));
        throw new Error("接口返回的数据格式不是标准 JSON，可能遇到了网页拦截、人机验证或 502 错误。");
      }

      const rawList = Array.isArray(json?.data) ? json.data : Array.isArray(json) ? json : [];
      if (rawList.length > 0) {
        liveModels.value = rawList.map((item: any) => ({
          id: String(item.model_name || item.id || item.name || item.model || item),
          owned_by: item.owner || item.owned_by || undefined,
          model_ratio: typeof item.model_ratio === "number" ? item.model_ratio : undefined,
          completion_ratio: typeof item.completion_ratio === "number" ? item.completion_ratio : undefined,
          model_price: typeof item.model_price === "number" ? item.model_price : undefined,
          quota_type: typeof item.quota_type === "number" ? item.quota_type : undefined,
          group: item.group ? String(item.group) : Array.isArray(item.enable_groups) ? item.enable_groups.join(",") : undefined,
        }));
        apiSource.value = rawList[0]?.model_name !== undefined ? "pricing" : "models";
        liveFetching.value = false;
        return;
      } else {
         throw new Error("接口返回的模型列表为空，该站点可能没有配置模型或接口无权限。");
      }
    } catch (err: any) {
      console.warn("Tauri fetch_site_models_json 请求失败", err);
      liveError.value = String(err?.message || err || "接口拉取失败");
      liveFetching.value = false;
      return; // Tauri 环境下请求失败直接返回，不再走浏览器的 Web fetch 降级（因为浏览器大概率也会跨域失败）
    }
  }

  // 2. 网页浏览器降级策略 (Web fetch)
  try {
    const pricingUrl = `${baseUrl}api/pricing`;
    const res = await fetch(pricingUrl, { signal: AbortSignal.timeout(6000) });
    if (res.ok) {
      const json = await res.json();
      const rawList = Array.isArray(json?.data) ? json.data : Array.isArray(json) ? json : [];
      if (rawList.length > 0) {
        liveModels.value = rawList.map((item: any) => ({
          id: String(item.model_name || item.id || item.name || item.model || item),
          owned_by: item.owner || item.owned_by || undefined,
          model_ratio: typeof item.model_ratio === "number" ? item.model_ratio : undefined,
          completion_ratio: typeof item.completion_ratio === "number" ? item.completion_ratio : undefined,
          model_price: typeof item.model_price === "number" ? item.model_price : undefined,
          quota_type: typeof item.quota_type === "number" ? item.quota_type : undefined,
          group: item.group ? String(item.group) : Array.isArray(item.enable_groups) ? item.enable_groups.join(",") : undefined,
        }));
        apiSource.value = "pricing";
        liveFetching.value = false;
        return;
      }
    }
  } catch {
    // 降级继续
  }

  try {
    const v1Url = `${baseUrl}v1/models`;
    const res = await fetch(v1Url, { signal: AbortSignal.timeout(6000) });
    if (res.ok) {
      const json = await res.json();
      const rawList = Array.isArray(json?.data) ? json.data : Array.isArray(json) ? json : [];
      if (rawList.length > 0) {
        liveModels.value = rawList.map((item: any) => ({
          id: typeof item === "string" ? item : String(item.id || item.name || item),
          owned_by: item.owned_by ? String(item.owned_by) : undefined,
        }));
        apiSource.value = "models";
        liveFetching.value = false;
        return;
      }
    }
    liveError.value = "接口未返回有效数据。";
  } catch (err: any) {
    if (!liveError.value) {
      liveError.value = err?.message || "无法拉取在线模型，接口可能需要 API Key 或存在网络限制（如跨域 CORS）";
    }
  } finally {
    liveFetching.value = false;
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
              type="search"
              placeholder="搜索模型标识、分组或厂商…"
            />
          </label>

          <button
            type="button"
            class="fetch-live-btn"
            :disabled="liveFetching"
            @click="fetchLiveModels"
          >
            <span v-html="icons.restore" />
            <span>{{ liveFetching ? '获取中…' : '刷新 /api/pricing' }}</span>
          </button>
        </div>

        <div class="dialog-body">
          <!-- 在线接口获取到的模型 (NewAPI /api/pricing 或 /v1/models) -->
          <div v-if="liveModels.length > 0" class="models-section">
            <div class="section-title">
              <span class="live-dot" />
              <span>
                在线接口模型列表
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
                  <div class="model-sub-meta">
                    <span v-if="model.quota_type === 1" class="ratio-badge price">
                      按次: ${{ model.model_price ?? 0 }}
                    </span>
                    <span v-else-if="model.model_ratio !== undefined" class="ratio-badge">
                      倍率: {{ model.model_ratio }}x
                      <template v-if="model.completion_ratio"> (补全: {{ model.completion_ratio }}x)</template>
                    </span>
                    <span v-if="model.group" class="group-badge">
                      {{ model.group }}
                    </span>
                    <small v-if="model.owned_by">by {{ model.owned_by }}</small>
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
            <span>正在向 {{ site?.name }} 请求 /api/pricing 获取实时模型列表…</span>
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
