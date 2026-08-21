<script setup lang="ts">
import { ref, computed } from "vue";
import { icons } from "../../icons";
import { useStore } from "../../composables/useStore";
import { logoText } from "../../utils";
import type { ModelCategory } from "../../types";

const store = useStore();

const searchQuery = ref("");
const selectedCategory = ref<ModelCategory>("all");
const expandedModelId = ref<string | null>(null);

interface ModelDefinition {
  id: string;
  name: string;
  category: ModelCategory;
  vendorName: string;
  keywords: string[];
  description: string;
}

const MODEL_DEFINITIONS: ModelDefinition[] = [
  // OpenAI
  {
    id: "gpt-4o",
    name: "GPT-4o / GPT-4 旗舰系列",
    category: "openai",
    vendorName: "OpenAI",
    keywords: ["gpt", "gpt-4", "gpt4", "gpt-4o", "codex"],
    description: "OpenAI 旗舰全能大语言模型与代码生成模型",
  },
  {
    id: "gpt-mini",
    name: "GPT-4o Mini / Nano 轻量系列",
    category: "openai",
    vendorName: "OpenAI",
    keywords: ["mini", "nano", "gpt-4.1-mini"],
    description: "OpenAI 极速低成本轻量级大模型",
  },
  {
    id: "openai-o1",
    name: "OpenAI o1 / o3 深度推理模型",
    category: "openai",
    vendorName: "OpenAI",
    keywords: ["o1", "o3", "reasoning"],
    description: "OpenAI 专门面向复杂数学、科学与代码推理的模型",
  },

  // Anthropic
  {
    id: "claude-3-5",
    name: "Claude 3.5 Sonnet / Haiku",
    category: "claude",
    vendorName: "Anthropic",
    keywords: ["claude", "sonnet", "haiku", "claude-3.5"],
    description: "Anthropic 业界领先的逻辑推理与编程大模型",
  },
  {
    id: "claude-opus",
    name: "Claude 3 / 4 Opus",
    category: "claude",
    vendorName: "Anthropic",
    keywords: ["opus", "claude-opus", "opus-4.8"],
    description: "Anthropic 顶尖长文本与复杂逻辑分析模型",
  },
  {
    id: "claude-code",
    name: "Claude Code 终端专属",
    category: "claude",
    vendorName: "Anthropic",
    keywords: ["claude code", "claudecode"],
    description: "针对 CLI 开发与智能 Agent 优化的 Claude Code 接口",
  },

  // DeepSeek
  {
    id: "deepseek-v3",
    name: "DeepSeek-V3 / V4 Pro 全能系列",
    category: "deepseek",
    vendorName: "DeepSeek",
    keywords: ["deepseek", "deepseek-v3", "deepseek-v4"],
    description: "深度求索前沿高性能开源/商业通用模型",
  },
  {
    id: "deepseek-r1",
    name: "DeepSeek-R1 强推理系列",
    category: "deepseek",
    vendorName: "DeepSeek",
    keywords: ["deepseek-r1", "r1"],
    description: "DeepSeek 突破性强化学习自进化推理模型",
  },

  // Google
  {
    id: "gemini-pro",
    name: "Gemini 1.5 / 2.5 / 3.0 Pro",
    category: "gemini",
    vendorName: "Google",
    keywords: ["gemini", "gemini-1.5", "gemini-2.5", "gemini-3"],
    description: "Google 超长上下文多模态理解大模型",
  },
  {
    id: "gemini-flash",
    name: "Gemini Flash 极速多模态",
    category: "gemini",
    vendorName: "Google",
    keywords: ["flash", "gemini flash"],
    description: "Google 高并发毫秒级响应多模态模型",
  },

  // xAI
  {
    id: "grok-all",
    name: "Grok 2 / Grok 3 / 4 系列",
    category: "grok",
    vendorName: "xAI",
    keywords: ["grok"],
    description: "xAI 具备实时互联网知识与强幽默逻辑的模型",
  },

  // 国产大模型
  {
    id: "qwen",
    name: "Qwen / 通义千问全系列",
    category: "domestic",
    vendorName: "阿里",
    keywords: ["qwen", "通义千问"],
    description: "阿里云开源/商业双轨道全能大语言模型",
  },
  {
    id: "glm",
    name: "GLM-4 / 智谱清言",
    category: "domestic",
    vendorName: "智谱AI",
    keywords: ["glm", "智谱"],
    description: "智谱 AI 中英文旗舰认知大语言模型",
  },
  {
    id: "kimi",
    name: "Kimi / Moonshot 长文本",
    category: "domestic",
    vendorName: "月之暗面",
    keywords: ["kimi", "moonshot"],
    description: "月之暗面超长无损上下文无障碍对话模型",
  },
  {
    id: "minimax",
    name: "MiniMax / abab 系列",
    category: "domestic",
    vendorName: "MiniMax",
    keywords: ["minimax", "abab"],
    description: "MiniMax 多模态文本、语音与代码通用大模型",
  },
  {
    id: "mimo",
    name: "MiMo / 小米模型",
    category: "domestic",
    vendorName: "小米/MiMo",
    keywords: ["mimo"],
    description: "小米/MiMo 垂直场景优化模型",
  },

  // 翻译与专用
  {
    id: "immersive-translation",
    name: "沉浸式翻译专用模型",
    category: "other",
    vendorName: "翻译专区",
    keywords: ["翻译"],
    description: "专门针对沉浸式翻译、双语对译与文本润色优化的节点",
  },
];

const categories = [
  { value: "all", label: "全部模型" },
  { value: "openai", label: "OpenAI" },
  { value: "claude", label: "Claude" },
  { value: "deepseek", label: "DeepSeek" },
  { value: "gemini", label: "Gemini" },
  { value: "grok", label: "Grok" },
  { value: "domestic", label: "国产模型" },
  { value: "other", label: "翻译/其它" },
];

const modelListWithSites = computed(() => {
  const sitesList = store.sites.value;

  return MODEL_DEFINITIONS.map((def) => {
    const matchingSites = sitesList.filter((site) => {
      // 检查标签、名称或描述中是否包含关键词
      const siteText = [
        site.name,
        site.description,
        site.systemType,
        ...site.tags,
      ]
        .join(" ")
        .toLowerCase();

      return def.keywords.some((kw) => siteText.includes(kw.toLowerCase()));
    });

    return {
      ...def,
      sites: matchingSites,
    };
  }).filter((model) => model.sites.length > 0);
});

const filteredModels = computed(() => {
  const q = searchQuery.value.trim().toLowerCase();
  const cat = selectedCategory.value;

  return modelListWithSites.value.filter((model) => {
    // 类别筛选
    if (cat !== "all" && model.category !== cat) return false;

    // 搜索关键词筛选
    if (!q) return true;
    const modelText = [
      model.name,
      model.vendorName,
      model.description,
      ...model.sites.map((s) => s.name),
    ]
      .join(" ")
      .toLowerCase();
    return modelText.includes(q);
  });
});

function toggleExpand(modelId: string) {
  if (expandedModelId.value === modelId) {
    expandedModelId.value = null;
  } else {
    expandedModelId.value = modelId;
  }
}

async function copyApiUrl(url: string, name: string) {
  await store.copyAddress(url, `${name} API 地址`);
}

async function openSite(url: string) {
  await store.openExternal(url);
}
</script>

<template>
  <div class="models-view">
    <header class="models-header">
      <div class="models-title-row">
        <div class="models-title-badge">
          <span v-html="icons.cpu" />
        </div>
        <div>
          <h2>AI 模型汇总库</h2>
          <p>按大语言模型类型全局查询全站支持情况，快速选择最佳 API 节点</p>
        </div>
      </div>

      <div class="models-toolbar">
        <label class="models-search">
          <span v-html="icons.search" />
          <input
            v-model="searchQuery"
            type="search"
            placeholder="搜模型、厂商或站点名称…"
          />
        </label>

        <div class="category-tabs" role="tablist">
          <button
            v-for="cat in categories"
            :key="cat.value"
            type="button"
            class="tab-chip"
            :class="{ active: selectedCategory === cat.value }"
            @click="selectedCategory = cat.value as ModelCategory"
          >
            {{ cat.label }}
          </button>
        </div>
      </div>
    </header>

    <div class="models-grid" v-if="filteredModels.length > 0">
      <article
        v-for="model in filteredModels"
        :key="model.id"
        class="model-card"
        :class="{ expanded: expandedModelId === model.id }"
      >
        <div class="model-card-header" @click="toggleExpand(model.id)">
          <div class="model-meta">
            <span class="vendor-badge" :data-vendor="model.category">
              {{ model.vendorName }}
            </span>
            <h3>{{ model.name }}</h3>
          </div>

          <p class="model-desc">{{ model.description }}</p>

          <div class="model-stats">
            <span class="sites-count-chip">
              <strong v-html="icons.globe" />
              <span>{{ model.sites.length }} 个站点支持</span>
            </span>

            <button type="button" class="expand-trigger" aria-label="展开支持站点">
              <span v-html="icons.chevron" />
            </button>
          </div>
        </div>

        <div class="model-sites-list" v-if="expandedModelId === model.id">
          <div class="sites-list-header">
            <h4>支持 {{ model.name }} 的本地站点 ({{ model.sites.length }})</h4>
          </div>

          <div class="sites-mini-grid">
            <div
              v-for="site in model.sites"
              :key="site.id"
              class="site-mini-card"
              :class="{ 'is-runaway': site.isRunaway }"
            >
              <div class="mini-card-top">
                <div class="site-avatar">{{ logoText(site.apiBaseUrl, site.name) }}</div>
                <div class="site-mini-info">
                  <strong>{{ site.name }}</strong>
                  <small>{{ site.apiBaseUrl }}</small>
                </div>
              </div>

              <div class="mini-card-tags">
                <span class="mini-chip level">LV{{ site.registrationLimit }}</span>
                <span v-if="site.supportsCheckin" class="mini-chip checkin">签到</span>
                <span v-if="site.supportsLdc" class="mini-chip ldc">LDC</span>
                <span v-if="site.requiresInviteCode" class="mini-chip invite">邀请码</span>
                <span v-if="site.isRunaway" class="mini-chip runaway">跑路</span>
              </div>

              <div class="mini-card-actions">
                <button
                  type="button"
                  class="action-btn"
                  title="复制 API 地址"
                  @click.stop="copyApiUrl(site.apiBaseUrl, site.name)"
                >
                  <span v-html="icons.copy" />
                  <span>复制 API</span>
                </button>
                <button
                  type="button"
                  class="action-btn primary"
                  title="访问站点"
                  @click.stop="openSite(site.apiBaseUrl)"
                >
                  <span v-html="icons.external" />
                  <span>打开</span>
                </button>
              </div>
            </div>
          </div>
        </div>
      </article>
    </div>

    <div v-else class="models-empty-state">
      <div v-html="icons.cpu" />
      <h3>未找到匹配的 AI 模型</h3>
      <p>尝试更短的关键词或切换上方“全部分类”筛选。</p>
    </div>
  </div>
</template>
