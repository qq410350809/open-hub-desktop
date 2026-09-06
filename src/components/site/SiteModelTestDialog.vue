<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { Channel, invoke } from "@tauri-apps/api/core";
import { marked } from "marked";
import DOMPurify from "dompurify";
import { runCommand } from "../../composables/useLibrary";
import { icons } from "../../icons";
import { useStore } from "../../composables/useStore";
import { logoText } from "../../utils";
import { useToast } from "../../composables/core/useToast";

/** 流式事件负载（与后端 ChatStreamEvent camelCase 序列化对齐） */
interface ChatStreamPayload {
  kind: "delta" | "done" | "error" | "cancelled";
  content?: string;
  reasoning?: string;
  message?: string;
  firstTokenMs?: number;
  totalMs?: number;
  chars?: number;
}

/** 缓存的账号 Key 信息（get_site_model_cache 的 accounts 项） */
interface CacheAccount {
  profileId: string;
  profileName: string;
  accountName: string;
  username: string;
  keys: string[];
  keyGroups?: Record<string, string>;
  error: string;
}

/** 一条测试对话消息 */
interface TestMessage {
  /** 稳定唯一 id：模板中的流式状态比较依赖它保持响应式 */
  id: string;
  role: "user" | "assistant";
  content: string;
  /** 模型思考内容（reasoning_content / <think>），仅 assistant */
  reasoning?: string;
  /** 思考阶段持续时长（毫秒）：从发起请求到首个正文增量 */
  thinkingMs?: number;
  /** 发送时的思考级别（default 不显示） */
  level?: string;
  /** 发送时使用的模型名，仅 assistant */
  model?: string;
  error?: string;
  cancelled?: boolean;
  stats?: { firstTokenMs?: number; totalMs?: number; chars?: number } | null;
}

/** 预置对话测试模板：一键导入系统提示词（可选）与首条测试消息。
 *  设计原则：答案可自行验证 / 约束可逐条核对，能真实拉开模型能力差距。 */
interface TestTemplate {
  id: string;
  name: string;
  system?: string;
  message: string;
}

const TEST_TEMPLATES: TestTemplate[] = [
  {
    id: "logic",
    name: "复杂推理",
    message:
      "一座岛上每个人要么永远说真话（君子），要么永远说假话（骗子）。\nA 说：「我们两人中至少有一个人是骗子。」\nB 没有说话。\n请问 A、B 分别是君子还是骗子？请给出完整推理，并说明为什么另一种组合都不成立。",
  },
  {
    id: "math",
    name: "数学期望",
    message:
      "抛一枚均匀硬币，直到连续出现两次正面为止。求平均需要抛掷次数的期望值，请给出完整推导过程（建议用状态法建立方程求解）。",
  },
  {
    id: "code",
    name: "代码进阶",
    message:
      "请用 Python 实现函数 num_to_cn(amount: float) -> str，把金额转换为人民币大写。要求：\n1. 0 < amount < 10^12；\n2. 正确处理「零」的合并规则：1005 → 壹仟零伍元整，1024.05 → 壹仟零贰拾肆元零伍分；\n3. 正确处理整元：120 → 壹佰贰拾元整，1000000 → 壹佰万元整；\n4. 附至少 5 个测试用例及预期输出。",
  },
  {
    id: "instruct",
    name: "指令遵循",
    message:
      "请写一段介绍咖啡的短文，必须同时满足以下全部约束：\n1. 恰好 3 句话；\n2. 三句的字数严格递增；\n3. 全文不出现「的」字；\n4. 必须包含「醇厚」与「回甘」两个词；\n5. 以句号结尾。\n写完后逐条自检，说明每条约束是否满足。",
  },
  {
    id: "synthesize",
    name: "信息综合",
    message:
      "小组 5 名成员对本周完成的任务数各说了一句话，且全部为真：\n小赵：我完成了 4 个。\n小李：我完成的任务数恰好是小赵的 2 倍。\n小钱：我比小李多 1 个，但比小孙少 2 个。\n小孙：我们五个人一共完成了 40 个任务。\n小王：我完成的数量恰好等于全组的平均值。\n问：小王完成了多少个？请写出完整推理。",
  },
  {
    id: "knowledge",
    name: "知识深度",
    message:
      "请解释 Transformer 缩放点积注意力的完整计算流程：设 batch=1、seq=16、d_model=512、注意力头数 h=8，逐步给出从输入到输出的每一步维度变化；并回答缩放因子为什么选 √d_k 而不是别的值——如果不做缩放会发生什么？",
  },
  {
    id: "json",
    name: "结构化输出",
    system: "你是严格遵守指令的测试助手，只输出被要求的内容，不附加任何解释。",
    message:
      "只输出一个合法 JSON 对象，禁止使用 Markdown 代码块或任何解释文字。要求：\ncaseId：字符串，格式为 TEST- 后跟 4 位数字；\nitems：数组，恰好 3 个元素，每个元素为 {\"name\": 字符串, \"priority\": 1~5 的整数}，且三个 priority 互不相同；\ntotalPriority：整数，必须等于 items 中所有 priority 之和；\ntags：数组，必须包含 \"alpha\" 与 \"beta\"，可选是否包含 \"gamma\"。",
  },
  {
    id: "longform",
    name: "长文流式",
    message:
      "请用 Markdown 表格从「连接建立、传输可靠性、有序性、拥塞控制、头部开销、实时性、典型场景、常见端口」8 个维度对比 TCP 与 UDP；表格之后用一段话给出「视频直播 vs 文件传输」的协议选型建议。内容尽量完整，篇幅不要太短。",
  },
  {
    // 经典自指 / 反注入陷阱题：待复述内容里混着命令词「repeat」，无标准答案、定性观察。
    // 强模型应识别：指令与内容的边界歧义、Nameeee 无语义不可"理解式复述"、
    // 机械重复与深度分析的任务冲突、字面执行会引发递归、缺少可验证的成功标准。
    // 弱模型的典型失分：盲目执行 repeat、对乱码强行解读、陷入 repeat repeat… 递归。
    id: "selfref",
    name: "自指陷阱",
    message: "重复一遍我说的话:repeat Nameeee 并深度分析一下这个任务有什么不合理的地方",
  },
];

const isTauri = "__TAURI_INTERNALS__" in window;
const store = useStore();
const { showToast } = useToast();

const site = computed(() => store.siteTestSite.value);
const logo = computed(() =>
  site.value ? logoText(site.value.apiBaseUrl, site.value.name) : "",
);

const cacheModels = ref<string[]>([]);
const cacheAccounts = ref<CacheAccount[]>([]);
const cacheLoading = ref(false);
const selectedModel = ref("");
const selectedKey = ref("");
/** 思考级别：default=不传参（站点默认）；off=关闭；minimal/low/medium/high/xhigh/max=推理力度；budget=自定义思考预算 */
const thinkingLevel = ref<
  "default" | "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "budget"
>("default");
/** 自定义思考预算（tokens），Anthropic 最低 1024 */
const budgetTokens = ref(8192);
const draft = ref("");
const messages = ref<TestMessage[]>([]);
/** 会话级系统提示词：由模板导入，发送时作为首条 system 消息下发；点击标签可移除 */
const systemPrompt = ref("");
/** 系统提示词编辑器展开态 */
const sysEditorOpen = ref(false);
const streaming = ref(false);
const messagesBodyRef = ref<HTMLElement>();
const composerRef = ref<HTMLTextAreaElement>();

/** 正在流式输出的助手消息 id：必须用 ref 让模板比较保持响应式 */
const currentAssistantId = ref<string | null>(null);
/** 当前流式请求 ID（取消用） */
let activeRequestId: string | null = null;
let requestCounter = 0;
let messageCounter = 0;

const nextMessageId = () => `test-msg-${requestCounter}-${messageCounter++}`;

// —— 思考计时：思考中每秒跳动，结束后记录精确时长 ——
const thinkingSeconds = ref(0);
let thinkingTimer: number | null = null;
let thinkingStartedAt = 0;

function startThinkingTimer() {
  stopThinkingTimer();
  thinkingSeconds.value = 0;
  thinkingStartedAt = Date.now();
  thinkingTimer = window.setInterval(() => {
    thinkingSeconds.value = Math.round((Date.now() - thinkingStartedAt) / 1000);
  }, 1000);
}

function stopThinkingTimer() {
  if (thinkingTimer !== null) {
    window.clearInterval(thinkingTimer);
    thinkingTimer = null;
  }
}

const hasCachedKeys = computed(() =>
  cacheAccounts.value.some((account) => account.keys.length > 0),
);

const THINKING_LEVEL_LABELS: Record<string, string> = {
  off: "关闭",
  minimal: "最小",
  low: "低",
  medium: "中",
  high: "高",
  xhigh: "极高",
  max: "Max",
};

const accountLabel = (account: CacheAccount) =>
  account.username || account.accountName || account.profileName || "未命名账号";

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

/** Markdown 渲染（marked + DOMPurify 消毒，防上游注入） */
function renderMarkdown(text: string): string {
  try {
    return DOMPurify.sanitize(marked.parse(text, { async: false }) as string);
  } catch {
    return `<pre>${text.replace(/</g, "&lt;")}</pre>`;
  }
}

function statsLine(message: TestMessage): string {
  const stats = message.stats;
  if (!stats) return "";
  const parts: string[] = [];
  if (stats.firstTokenMs != null) parts.push(`首字 ${(stats.firstTokenMs / 1000).toFixed(2)}s`);
  if (stats.totalMs != null) parts.push(`共 ${(stats.totalMs / 1000).toFixed(2)}s`);
  if (message.reasoning) parts.push(`思考 ${message.reasoning.length} 字`);
  if (stats.chars != null) parts.push(`正文 ${stats.chars} 字`);
  return parts.join(" · ");
}

/** 是否正处于思考阶段：消息在流式输出且尚未开始正文/未出错 */
function isThinking(message: TestMessage): boolean {
  return streaming.value && currentAssistantId.value === message.id && !message.content && !message.error;
}

/** 思考块标题：思考中显示实时秒数，结束后显示持续时长（参照「🧠 思考 · 持续了几秒」样式） */
function thinkLabel(message: TestMessage): string {
  if (isThinking(message)) return `思考中 · ${thinkingSeconds.value} 秒`;
  if (message.thinkingMs != null) {
    const seconds = message.thinkingMs / 1000;
    return `思考 · 持续了 ${seconds < 10 ? seconds.toFixed(1) : Math.round(seconds)} 秒`;
  }
  return "思考";
}

function scrollToBottom() {
  void nextTick(() => {
    const body = messagesBodyRef.value;
    if (body) body.scrollTop = body.scrollHeight;
  });
}

async function loadSiteCache(siteId: string) {
  cacheLoading.value = true;
  try {
    const data = await runCommand<{ models?: { id: string }[]; accounts?: CacheAccount[] }>(
      "get_site_model_cache",
      { siteId },
    );
    const models = Array.isArray(data?.models)
      ? data.models.map((m) => m.id).filter(Boolean)
      : [];
    const accounts = Array.isArray(data?.accounts) ? data.accounts : [];
    cacheModels.value = [...new Set(models)];
    cacheAccounts.value = accounts.filter((account) => account.keys?.length > 0);
    // 默认选中：首个模型 + 首个可用 Key
    if (!selectedModel.value || !cacheModels.value.includes(selectedModel.value)) {
      selectedModel.value = cacheModels.value[0] ?? "";
    }
    if (!hasKeySelected()) {
      selectedKey.value = cacheAccounts.value[0]?.keys[0] ?? "";
    }
  } catch {
    cacheModels.value = [];
    cacheAccounts.value = [];
  } finally {
    cacheLoading.value = false;
  }
}

function hasKeySelected(): boolean {
  return cacheAccounts.value.some((account) => account.keys.includes(selectedKey.value));
}

function resetConversation() {
  cancelIfStreaming();
  stopThinkingTimer();
  messages.value = [];
  draft.value = "";
  // 系统提示词保留：作为会话级配置，便于同一人设下连续多轮测试
}

/** 一键导入模板：填入系统提示词（如有）并把测试消息放到输入框待发 */
function applyTemplate(template: TestTemplate) {
  if (streaming.value) return;
  systemPrompt.value = template.system?.trim() || "";
  draft.value = template.message;
  void nextTick(() => composerRef.value?.focus());
}

function onTemplateSelect(event: Event) {
  const id = (event.target as HTMLSelectElement).value;
  const template = TEST_TEMPLATES.find((item) => item.id === id);
  if (template) applyTemplate(template);
  (event.target as HTMLSelectElement).value = "";
}

function cancelIfStreaming() {
  if (activeRequestId) {
    void invoke("site_model_chat_cancel", { requestId: activeRequestId }).catch(() => {});
  }
}

watch(
  () => store.siteTestDialogOpen.value,
  (open) => {
    if (open) {
      document.body.classList.add("modal-open");
      messages.value = [];
      draft.value = "";
      systemPrompt.value = "";
      sysEditorOpen.value = false;
      cacheModels.value = [];
      cacheAccounts.value = [];
      selectedModel.value = "";
      selectedKey.value = "";
      thinkingLevel.value = "default";
      budgetTokens.value = 8192;
      const siteId = site.value?.id;
      if (siteId) void loadSiteCache(siteId);
      nextTick(() => composerRef.value?.focus());
    } else {
      cancelIfStreaming();
      stopThinkingTimer();
      document.body.classList.remove("modal-open");
    }
  },
);

function close() {
  store.closeSiteTestDialog();
}

function onBackdropClick(event: MouseEvent) {
  if (event.target === event.currentTarget) close();
}

function onComposerKeydown(event: KeyboardEvent) {
  if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
    event.preventDefault();
    void send();
  }
}

/** 发送消息：携带全部历史走后端流式转发，逐增量渲染 */
async function send() {
  const target = site.value;
  if (!target || streaming.value) return;
  const model = selectedModel.value.trim();
  const apiKey = selectedKey.value.trim();
  const text = draft.value.trim();
  if (!text) return;
  if (!model) {
    showToast("请先选择或输入要测试的模型", true);
    return;
  }
  if (!apiKey) {
    showToast("请先选择或输入用于测试的 API Key", true);
    return;
  }
  draft.value = "";
  messages.value.push({ id: nextMessageId(), role: "user", content: text });
  // 系统提示词（模板导入）作为首条 system 消息，随后为完整多轮历史
  const history: { role: string; content: string }[] = [];
  const system = systemPrompt.value.trim();
  if (system) history.push({ role: "system", content: system });
  history.push(
    ...messages.value.map((message) => ({
      role: message.role,
      content: message.content,
    })),
  );
  // 注意：push 后必须取数组内的代理引用再变异，直接持有原始对象不会触发视图更新
  messages.value.push({
    id: nextMessageId(),
    role: "assistant",
    content: "",
    reasoning: "",
    model,
    // 消息上记录档位展示文案（default 记空串不显示）
    level:
      thinkingLevel.value === "default"
        ? ""
        : thinkingLevel.value === "budget"
          ? `预算 ${budgetTokens.value}`
          : THINKING_LEVEL_LABELS[thinkingLevel.value],
    stats: null,
  });
  const assistant = messages.value[messages.value.length - 1];
  currentAssistantId.value = assistant.id;
  streaming.value = true;
  startThinkingTimer();
  scrollToBottom();

  const requestId = `site-chat-test-${Date.now()}-${requestCounter++}`;
  activeRequestId = requestId;
  try {
    const channel = new Channel<ChatStreamPayload>();
    channel.onmessage = (event) => {
      if (event.kind === "delta") {
        if (event.reasoning) assistant.reasoning = (assistant.reasoning || "") + event.reasoning;
        if (event.content) {
          // 首个正文增量 = 思考阶段结束，记录思考时长
          if (!assistant.content) {
            assistant.thinkingMs = Date.now() - thinkingStartedAt;
            stopThinkingTimer();
          }
          assistant.content += event.content;
        }
        scrollToBottom();
      } else if (event.kind === "done") {
        assistant.stats = {
          firstTokenMs: event.firstTokenMs,
          totalMs: event.totalMs,
          chars: event.chars,
        };
        stopThinkingTimer();
        if (assistant.reasoning && assistant.thinkingMs == null) {
          assistant.thinkingMs = Date.now() - thinkingStartedAt;
        }
        streaming.value = false;
        scrollToBottom();
      } else if (event.kind === "cancelled") {
        assistant.cancelled = true;
        stopThinkingTimer();
        if (assistant.reasoning && assistant.thinkingMs == null) {
          assistant.thinkingMs = Date.now() - thinkingStartedAt;
        }
        streaming.value = false;
        scrollToBottom();
      } else if (event.kind === "error") {
        assistant.error = event.message || "未知错误";
        stopThinkingTimer();
        if (assistant.reasoning && assistant.thinkingMs == null) {
          assistant.thinkingMs = Date.now() - thinkingStartedAt;
        }
        streaming.value = false;
        scrollToBottom();
      }
    };
    await invoke("site_model_chat_stream", {
      requestId,
      url: target.apiBaseUrl,
      apiKey,
      model,
      thinkingLevel: thinkingLevel.value === "default" ? null : thinkingLevel.value,
      thinkingBudget: thinkingLevel.value === "budget" ? budgetTokens.value : null,
      messages: history,
      channel,
    });
  } catch (error) {
    assistant.error = String(error);
    streaming.value = false;
  } finally {
    if (currentAssistantId.value === assistant.id) {
      currentAssistantId.value = null;
      activeRequestId = null;
    }
    stopThinkingTimer();
    if (streaming.value) streaming.value = false;
    scrollToBottom();
  }
}
</script>

<template>
  <Teleport to="body">
    <div
      class="site-test-backdrop"
      id="site-test-dialog"
      :hidden="!store.siteTestDialogOpen.value"
      @click="onBackdropClick"
    >
      <section class="site-test-dialog" role="dialog" aria-modal="true" aria-labelledby="site-test-title">
        <header class="site-test-header">
          <div class="site-test-site">
            <div class="site-test-avatar" aria-hidden="true">{{ logo }}</div>
            <div class="site-test-site-meta">
              <h2 id="site-test-title" class="site-test-title">
                <span class="site-test-name">{{ site?.name || "站点" }}</span>
                <span class="site-test-badge">模型测试</span>
              </h2>
              <p class="site-test-url" :title="site?.apiBaseUrl">{{ site?.apiBaseUrl }}</p>
            </div>
          </div>

          <div class="site-test-actions">
            <select
              class="site-test-select site-test-template-select"
              value=""
              :disabled="streaming"
              title="一键导入预置测试模板（自动填入系统提示词与测试消息）"
              aria-label="导入预置测试模板"
              @change="onTemplateSelect"
            >
              <option value="" disabled>导入模板…</option>
              <option v-for="template in TEST_TEMPLATES" :key="template.id" :value="template.id">
                {{ template.name }}
              </option>
            </select>
            <button
              type="button"
              class="site-test-text-btn"
              :disabled="streaming"
              title="新对话：清空当前测试消息（系统提示词保留）"
              @click="resetConversation"
            >
              <span v-html="icons.plus" />
              <span>新对话</span>
            </button>
            <button
              type="button"
              class="site-test-icon-btn site-test-close"
              title="关闭"
              @click="close"
            >
              <span v-html="icons.close" />
            </button>
          </div>
        </header>

        <!-- 测试配置条：模型与 Key 选择 -->
        <div class="site-test-config">
          <label class="site-test-field">
            <span class="site-test-field-k">模型</span>
            <select
              v-if="cacheModels.length > 0"
              v-model="selectedModel"
              class="site-test-select"
              :disabled="streaming"
            >
              <option v-for="model in cacheModels" :key="model" :value="model">{{ model }}</option>
            </select>
            <input
              v-else
              v-model="selectedModel"
              type="text"
              class="site-test-select"
              placeholder="暂无缓存模型，输入模型 ID"
              :disabled="streaming"
              spellcheck="false"
            />
          </label>
          <label class="site-test-field">
            <span class="site-test-field-k">Key</span>
            <select
              v-if="hasCachedKeys"
              v-model="selectedKey"
              class="site-test-select"
              :disabled="streaming"
            >
              <optgroup v-for="account in cacheAccounts" :key="account.profileId" :label="accountLabel(account)">
                <option v-for="key in account.keys" :key="key" :value="key">{{ maskApiKey(key) }}</option>
              </optgroup>
            </select>
            <input
              v-else
              v-model="selectedKey"
              type="text"
              class="site-test-select"
              placeholder="暂无缓存 Key，粘贴 API Key"
              :disabled="streaming"
              spellcheck="false"
            />
          </label>
          <label class="site-test-field site-test-field-sm">
            <span class="site-test-field-k">思考</span>
            <select
              v-model="thinkingLevel"
              class="site-test-select"
              :disabled="streaming"
              title="思考级别：关闭（GLM/Qwen 参数）/ 最小 / 低中高 / 极高 / Max（OpenAI reasoning_effort）/ 自定义预算（Anthropic thinking.budget_tokens + Qwen thinking_budget）；站点不支持时报错会显示在回复里"
            >
              <option value="default">默认</option>
              <option value="off">关闭</option>
              <option value="minimal">最小</option>
              <option value="low">低</option>
              <option value="medium">中</option>
              <option value="high">高</option>
              <option value="xhigh">极高</option>
              <option value="max">Max</option>
              <option value="budget">自定义预算</option>
            </select>
          </label>
          <label v-if="thinkingLevel === 'budget'" class="site-test-field site-test-field-sm">
            <span class="site-test-field-k">预算</span>
            <input
              v-model.number="budgetTokens"
              type="number"
              class="site-test-select site-test-budget-input"
              min="1024"
              step="1024"
              :disabled="streaming"
              title="思考预算（tokens），最小 1024"
            />
          </label>
          <span v-if="cacheLoading" class="site-test-config-hint">读取缓存中…</span>
          <button
            v-if="systemPrompt"
            type="button"
            class="site-test-sys-chip"
            :title="`系统提示词：${systemPrompt}（点击编辑）`"
            @click="sysEditorOpen = !sysEditorOpen"
          >
            <span class="site-test-sys-k">系统</span>
            <span class="site-test-sys-v">{{ systemPrompt }}</span>
            <span
              class="site-test-sys-x"
              title="移除系统提示词"
              @click.stop="systemPrompt = ''"
            >
              <span v-html="icons.close" />
            </span>
          </button>
          <button
            v-else
            type="button"
            class="site-test-text-btn"
            :disabled="streaming"
            title="设置系统提示词（System Prompt）：以 system 角色随每轮请求下发"
            @click="sysEditorOpen = !sysEditorOpen"
          >
            <span v-html="icons.edit" />
            <span>系统提示词</span>
          </button>
        </div>

        <!-- 系统提示词编辑器：自定义 System Prompt -->
        <div v-if="sysEditorOpen" class="site-test-sys-editor">
          <textarea
            v-model="systemPrompt"
            rows="3"
            placeholder="输入系统提示词（System Prompt）……留空表示不设置；将以 system 角色随每轮请求下发。"
            spellcheck="false"
          />
          <div class="site-test-sys-editor-actions">
            <button
              type="button"
              class="site-test-text-btn"
              :disabled="!systemPrompt.trim()"
              title="清空系统提示词"
              @click="systemPrompt = ''"
            >
              <span>清空</span>
            </button>
            <button
              type="button"
              class="site-test-text-btn"
              title="收起编辑器（内容自动保存）"
              @click="sysEditorOpen = false"
            >
              <span>完成</span>
            </button>
          </div>
        </div>

        <!-- 对话消息区：Cherry Studio 式用户右气泡 + 助手左正文 -->
        <div ref="messagesBodyRef" class="site-test-body">
          <div v-if="messages.length === 0" class="site-test-empty">
            <div class="site-test-empty-icon" v-html="icons.chat" />
            <p>输入消息开始模型测试</p>
            <small>多轮流式对话 · Enter 发送 · Shift+Enter 换行</small>
            <div class="site-test-templates">
              <button
                v-for="template in TEST_TEMPLATES"
                :key="template.id"
                type="button"
                class="site-test-template-chip"
                :title="template.message"
                @click="applyTemplate(template)"
              >
                {{ template.name }}
              </button>
            </div>
          </div>

          <template v-for="message in messages" :key="message.id">
            <!-- 用户消息：右侧气泡 -->
            <div v-if="message.role === 'user'" class="site-test-row is-user">
              <div class="site-test-user-bubble">{{ message.content }}</div>
            </div>

            <!-- 助手消息：左侧模型名 + 思考 + Markdown 正文 -->
            <div v-else class="site-test-row is-assistant">
              <div class="site-test-assistant-head">
                <span class="site-test-assistant-logo" aria-hidden="true">{{ logo }}</span>
                <strong>{{ message.model || "assistant" }}{{ message.level ? ` · 思考:${message.level}` : "" }}</strong>
              </div>
              <details
                v-if="message.reasoning"
                class="site-test-think"
                :class="{ 'is-active': isThinking(message) }"
              >
                <summary>
                  <span class="site-test-think-emoji" aria-hidden="true">🧠</span>
                  {{ thinkLabel(message) }}
                </summary>
                <div class="site-test-think-body">{{ message.reasoning }}</div>
              </details>
              <div
                v-if="message.content"
                class="site-test-markdown"
                :class="{ 'is-streaming': streaming && currentAssistantId === message.id }"
                v-html="renderMarkdown(message.content)"
              />
              <p
                v-else-if="streaming && currentAssistantId === message.id && !message.reasoning && !message.error"
                class="site-test-waiting"
              >
                正在等待模型响应…
              </p>
              <div v-if="message.error" class="site-test-error">
                <span v-html="icons.info" />
                <span>{{ message.error }}</span>
              </div>
              <p v-if="message.cancelled" class="site-test-cancelled">已中止</p>
              <p v-if="message.stats" class="site-test-stats">{{ statsLine(message) }}</p>
            </div>
          </template>
        </div>

        <!-- 输入区 -->
        <footer class="site-test-composer">
          <textarea
            ref="composerRef"
            v-model="draft"
            class="site-test-input"
            rows="2"
            placeholder="输入测试消息…（Enter 发送，Shift+Enter 换行）"
            :disabled="streaming || !isTauri"
            @keydown="onComposerKeydown"
          />
          <button
            v-if="!streaming"
            type="button"
            class="site-test-send"
            :disabled="!draft.trim() || !isTauri"
            title="发送（Enter）"
            @click="send"
          >
            <span v-html="icons.arrowUp" />
          </button>
          <button
            v-else
            type="button"
            class="site-test-send is-stop"
            title="停止生成"
            @click="cancelIfStreaming"
          >
            <span v-html="icons.pause" />
          </button>
        </footer>
      </section>
    </div>
  </Teleport>
</template>
