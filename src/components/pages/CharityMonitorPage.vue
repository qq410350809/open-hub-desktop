<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { icons } from "../../icons";
import { useStore } from "../../composables/useStore";
import { useConfirm } from "../../composables/useConfirm";
import AppTable, { type AppTableColumn } from "../common/AppTable.vue";
import type { CharityFeedItem, CharitySyncLogEntry } from "../../types";
import type { SortingState } from "@tanstack/table-core";
import { formatCompactCount as formatCompactCountUtil, formatDuration as formatDurationUtil } from "../../utils";

const store = useStore();
const { confirm } = useConfirm();

// —— 帖子详情弹窗状态 ——
const selectedPost = ref<CharityFeedItem | null>(null);

function openPostDetail(item: CharityFeedItem) {
  selectedPost.value = item;
  document.body.classList.add("modal-open");
}

function closePostDetail() {
  selectedPost.value = null;
  document.body.classList.remove("modal-open");
}

function openPostInBrowser(item: CharityFeedItem) {
  void store.openExternal(item.link);
}

// —— 快捷复制链接 ——
const copyFeedbackId = ref<string | null>(null);
async function copyPostLink(item: CharityFeedItem) {
  try {
    await navigator.clipboard.writeText(item.link);
    copyFeedbackId.value = item.id;
    setTimeout(() => {
      if (copyFeedbackId.value === item.id) {
        copyFeedbackId.value = null;
      }
    }, 1800);
  } catch {
    // ignore
  }
}

// —— 列表过滤与排序 ——
// 属性筛选（全部/热门/置顶/今日）已下推到后端，与分页同源，避免“只筛当前页”导致分页错乱。
const topicSorting = ref<SortingState>([]);

function isToday(dateStr?: string): boolean {
  if (!dateStr) return false;
  const d = new Date(dateStr);
  if (Number.isNaN(d.getTime())) return false;
  const today = new Date();
  return (
    d.getFullYear() === today.getFullYear() &&
    d.getMonth() === today.getMonth() &&
    d.getDate() === today.getDate()
  );
}

const displayItems = computed(() => store.charityFeedItems.value);

// —— 标签管理弹窗 ——
const tagManagerOpen = ref(false);
const newTagId = ref("");
const newTagName = ref("");
const tagManagerError = ref("");

function openTagManager() {
  tagManagerOpen.value = true;
  tagManagerError.value = "";
  document.body.classList.add("modal-open");
}

function closeTagManager() {
  tagManagerOpen.value = false;
  newTagId.value = "";
  newTagName.value = "";
  tagManagerError.value = "";
  document.body.classList.remove("modal-open");
}

async function handleAddTag() {
  const id = newTagId.value.trim();
  const name = newTagName.value.trim();
  if (!id || !name) {
    tagManagerError.value = "标签 ID 和名称不能为空";
    return;
  }
  try {
    tagManagerError.value = "";
    await store.addCharitySource(id, name);
    newTagId.value = "";
    newTagName.value = "";
  } catch (cause) {
    tagManagerError.value = String(cause);
  }
}

async function handleToggleTag(id: string, enabled: boolean) {
  try {
    await store.updateCharitySource(id, { enabled });
  } catch (cause) {
    tagManagerError.value = String(cause);
  }
}

async function handleRemoveTag(id: string, name: string) {
  const accepted = await confirm({
    title: "删除标签",
    message: `确定删除标签「${name}」吗？仅属于该标签的帖子将一并删除，同时属于其他订阅标签的帖子会保留。`,
    confirmText: "删除",
    danger: true,
  });
  if (!accepted) return;
  try {
    await store.removeCharitySource(id);
  } catch (cause) {
    tagManagerError.value = String(cause);
  }
}

// 标签 ID 点击后在浏览器打开对应的 linux.do 标签页
function openTagInBrowser(id: string) {
  void store.openExternal(`https://linux.do/tag/${id}-tag/${id}`);
}

// —— 同步日志管理与终端 ——
type LogStatusFilter = "all" | "running" | "success" | "error";
const logStatusFilter = ref<LogStatusFilter>("all");
const expandedLogId = ref<number | null>(null);

const filteredSyncLogs = computed(() => {
  const logs = store.charitySyncLog.value;
  if (logStatusFilter.value === "all") return logs;
  if (logStatusFilter.value === "running") return logs.filter((l) => l.status === "running");
  if (logStatusFilter.value === "success") return logs.filter((l) => l.status === "success");
  if (logStatusFilter.value === "error") {
    return logs.filter((l) => l.status === "error" || l.status === "failed");
  }
  return logs;
});

function toggleLogDetail(id: number) {
  expandedLogId.value = expandedLogId.value === id ? null : id;
}

function isRoundLogEntry(entry: CharitySyncLogEntry) {
  return entry.feedId === "round" || !!entry.detail?.feeds?.length;
}

function logSummaryText(entry: CharitySyncLogEntry) {
  const time = formatLogTime(entry.at);
  const node = entry.nodeName || "智能轮询调度";
  const stage = stageText(entry.stage);
  if (isRoundLogEntry(entry)) {
    const feeds = entry.detail?.feeds ?? [];
    const okCount = feeds.filter((feed) => feed.status === "success").length;
    return `${stage}于 ${time} 结束，共 ${feeds.length} 个标签（成功 ${okCount} 个），合计新增 ${entry.detail?.totalNew ?? 0} 条 / 更新 ${entry.detail?.totalUpdated ?? 0} 条。`;
  }
  if (entry.status === "success") {
    return `${stage}「${entry.feedName}」于 ${time} 通过节点「${node}」完成，耗时 ${formatDuration(liveDurationMs(entry))}。`;
  }
  if (entry.status === "cancelled") {
    return `「${entry.feedName}」的${stage}于 ${time} 被取消。`;
  }
  return `「${entry.feedName}」的${stage}于 ${time} 失败：${entry.message}`;
}

interface LogDetailCell {
  text: string;
  cls?: string;
}

function logDetailRows(entry: CharitySyncLogEntry): LogDetailCell[][] {
  const rows: LogDetailCell[][] = [];
  if (isRoundLogEntry(entry)) {
    for (const feed of entry.detail?.feeds ?? []) {
      rows.push([
        { text: feed.name || feed.id },
        { text: statusText(feed.status), cls: `is-${feed.status}` },
        { text: String(feed.new ?? 0), cls: "num" },
        { text: String(feed.updated ?? 0), cls: "num" },
      ]);
    }
    rows.push([
      { text: "合计" },
      { text: "" },
      { text: String(entry.detail?.totalNew ?? 0), cls: "num" },
      { text: String(entry.detail?.totalUpdated ?? 0), cls: "num" },
    ]);
    return rows;
  }
  rows.push([{ text: "新增帖子" }, { text: String(entry.detail?.new ?? 0), cls: "num" }]);
  rows.push([{ text: "更新帖子" }, { text: String(entry.detail?.updated ?? 0), cls: "num" }]);
  if (entry.detail?.unread != null) {
    rows.push([{ text: "当前未读" }, { text: String(entry.detail.unread), cls: "num" }]);
  }
  rows.push([{ text: "使用节点" }, { text: entry.nodeName || "智能轮询调度" }]);
  rows.push([{ text: "耗时" }, { text: formatDuration(liveDurationMs(entry)) }]);
  return rows;
}

const nowTick = ref(Date.now());
let tickTimer: number | null = null;

function ensureTickTimer() {
  const hasRunning = store.charitySyncLog.value.some((e) => e.status === "running");
  if (hasRunning && tickTimer == null) {
    tickTimer = window.setInterval(() => {
      nowTick.value = Date.now();
    }, 250);
  } else if (!hasRunning && tickTimer != null) {
    window.clearInterval(tickTimer);
    tickTimer = null;
  }
}

function liveDurationMs(entry: { status: string; at: string; durationMs?: number | null }) {
  if (entry.status !== "running") {
    return entry.durationMs ?? 0;
  }
  void nowTick.value;
  const raw = entry.at || "";
  const normalized = raw.includes("T") ? raw : raw.replace(" ", "T");
  const withZone = /Z$|[+-]\d{2}:?\d{2}$/.test(normalized) ? normalized : `${normalized}Z`;
  const started = new Date(withZone).getTime();
  if (!Number.isFinite(started)) return entry.durationMs ?? 0;
  return Math.max(0, Date.now() - started);
}

const selectedLabel = computed(
  () => store.currentFeedName.value || store.selectedTagId.value || "公益监听",
);

// —— 格式化函数 ——
function formatPublishedAt(value?: string) {
  if (!value) return "时间未知";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value || "时间未知";
  return new Intl.DateTimeFormat("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(date);
}

function formatCompactCount(value?: number | null) {
  return formatCompactCountUtil(value);
}

function formatRelativeActivity(value?: string, fallback?: string) {
  const raw = (value || fallback || "").trim();
  if (!raw) return "—";
  const date = new Date(raw);
  if (Number.isNaN(date.getTime())) return formatPublishedAt(raw);
  const diffMs = Date.now() - date.getTime();
  if (diffMs < 0) return formatPublishedAt(raw);
  const minutes = Math.floor(diffMs / 60000);
  if (minutes < 1) return "刚刚";
  if (minutes < 60) return `${minutes} 分钟前`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} 小时前`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${days} 天前`;
  return formatPublishedAt(raw);
}

function formatLogTime(value: string) {
  if (!value) return "--:--:--";
  const normalized = value.includes("T") ? value : value.replace(" ", "T");
  const withZone = /Z$|[+-]\d{2}:?\d{2}$/.test(normalized) ? normalized : `${normalized}Z`;
  const date = new Date(withZone);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(date);
}

function formatDuration(ms?: number) {
  return formatDurationUtil(ms);
}

function statusText(status: string) {
  switch (status) {
    case "running": return "进行中";
    case "success": return "成功";
    case "failed":
    case "error": return "失败";
    case "skipped": return "跳过";
    case "cancelled": return "已取消";
    default: return status || "日志";
  }
}

function stageText(stage: string) {
  switch (stage) {
    case "poll": return "后台轮询";
    case "manual": return "手动刷新";
    case "trying": return "尝试节点";
    case "done": return "同步完成";
    case "skipped": return "跳过";
    case "error": return "同步失败";
    case "failed": return "节点失败";
    default: return stage || "—";
  }
}

// —— 分类标签色彩定制 ——
function getCategoryTagStyle(category: string) {
  const c = category.toLowerCase();
  if (c.includes("claude")) return { color: "#d97706", bg: "rgba(217, 119, 6, 0.12)", border: "rgba(217, 119, 6, 0.3)" };
  if (c.includes("openai") || c.includes("gpt")) return { color: "#10b981", bg: "rgba(16, 185, 129, 0.12)", border: "rgba(16, 185, 129, 0.3)" };
  if (c.includes("deepseek") || c.includes("r1")) return { color: "#3b82f6", bg: "rgba(59, 130, 246, 0.12)", border: "rgba(59, 130, 246, 0.3)" };
  if (c.includes("gemini") || c.includes("google")) return { color: "#8b5cf6", bg: "rgba(139, 92, 246, 0.12)", border: "rgba(139, 92, 246, 0.3)" };
  if (c.includes("公益") || c.includes("免费")) return { color: "#ec4899", bg: "rgba(236, 72, 153, 0.12)", border: "rgba(236, 72, 153, 0.3)" };
  if (c.includes("公告") || c.includes("置顶") || c.includes("官方")) return { color: "#ef4444", bg: "rgba(239, 68, 68, 0.12)", border: "rgba(239, 68, 68, 0.3)" };
  return { color: "var(--muted)", bg: "var(--surface-hover)", border: "var(--line)" };
}

function authorInitials(author?: string) {
  if (!author) return "L";
  return author.slice(0, 2).toUpperCase();
}

// —— 表格列定义 ——
const topicColumns: AppTableColumn[] = [
  { key: "title", title: "话题 / 标题", sortable: false },
  { key: "author", title: "作者", width: "120px", sortable: true },
  { key: "publishedAt", title: "发布时间", width: "115px", sortable: true },
  { key: "replyCount", title: "回复", width: "80px", align: "right", sortable: true },
  { key: "views", title: "浏览", width: "85px", align: "right", sortable: true },
  { key: "lastActivityAt", title: "最近活跃", width: "100px", sortable: true },
  { key: "actions", title: "快捷操作", width: "110px", align: "center", sortable: false },
];

function topicRowClass(row: { pinned?: boolean }) {
  return [row.pinned && "is-pinned"].filter(Boolean).join(" ");
}

onMounted(() => {
  void store.loadCharityFeedLocal();
  tickTimer = window.setInterval(() => {
    nowTick.value = Date.now();
    ensureTickTimer();
  }, 250);
});

onUnmounted(() => {
  if (tickTimer != null) {
    window.clearInterval(tickTimer);
    tickTimer = null;
  }
});
</script>

<template>
  <main class="charity-monitor-page cm-dashboard">
    <!-- 顶部宏观智控驾驶舱 (Cockpit Bar) -->
    <header class="cm-cockpit-bar">
      <div class="cm-cockpit-left">
        <div class="cm-brand-section">
          <div class="cm-eyebrow-row">
            <span class="cm-live-dot" />
            <span class="cm-eyebrow-text">公益社区动态中心</span>
            <span class="cm-eyebrow-badge">LINUX.DO · {{ selectedLabel }}</span>
          </div>
          <div class="cm-title-row">
            <h1>公益与社区监听</h1>
          </div>
          <p class="cm-cockpit-subtitle">
            每 5 分钟定时智能对齐 · 候选代理池: <strong>{{ store.charityProxyPoolSummary.value?.validCount ?? 0 }}</strong> 节点在线 (≤500ms 候选: {{ store.charityProxyPoolSummary.value?.candidateCount ?? 0 }}) · 路由节点: <code>{{ store.charityFeedUsedNodeName.value || "智能调度" }}</code>
          </p>
        </div>
      </div>

      <div class="cm-cockpit-right">
        <button
          type="button"
          class="cm-btn-secondary"
          :class="{ active: store.charitySyncLogOpen.value }"
          title="查看后台轮询与节点请求诊断日志"
          @click="store.toggleCharitySyncLog()"
        >
          <span v-html="icons.info" />
          <span>同步日志</span>
          <span v-if="store.charityFeedSyncing.value" class="cm-mini-spinner" />
        </button>

        <button
          type="button"
          class="cm-btn-secondary"
          title="管理与配置 Linux.do 监听标签源"
          @click="openTagManager()"
        >
          <span v-html="icons.settings" />
          <span>标签管理</span>
          <span class="cm-count-chip">{{ store.charityTags.value.length - 1 }}</span>
        </button>

        <button
          type="button"
          class="cm-btn-primary"
          :disabled="store.charityFeedRefreshAllBusy.value || store.charityFeedSyncing.value"
          title="立即触发全量标签抓取与快照更新"
          @click="store.refreshCharityFeed()"
        >
          <span
            :class="{ 'is-spinning': store.charityFeedRefreshAllBusy.value || store.charityFeedSyncing.value }"
            v-html="icons.restore"
          />
          <span>{{
            store.charityFeedRefreshAllBusy.value
              ? "正在提交…"
              : store.charityFeedSyncing.value
                ? "同步中…"
                : "立即刷新全部"
          }}</span>
        </button>
      </div>
    </header>

    <!-- 异常状态横幅 -->
    <div v-if="store.charityFeedError.value" class="cm-error-banner" role="alert">
      <span class="cm-error-icon" v-html="icons.wifiOff" />
      <div class="cm-error-content">
        <strong>{{ store.charityFeedError.value.includes("跳过") ? "本轮同步已跳过" : "同步状态提示" }}</strong>
        <p>{{ store.charityFeedError.value }}</p>
      </div>
    </div>

    <!-- 页面核心视口容器 -->
    <div class="cm-dashboard-body">
      <!-- 4 大核心 KPI Bento 指标卡 (High-Impact Stats Deck) -->
      <section class="cm-stats-deck" aria-label="核心指标概览">
        <!-- 卡片 1: 今日最新发布 -->
        <div class="cm-stat-card">
          <div class="cm-stat-header">
            <span class="cm-stat-tag is-orange">
              <span v-html="icons.flame" />
              <span>今日新帖</span>
            </span>
            <span class="cm-stat-pill is-orange">近 24 小时</span>
          </div>
          <div class="cm-stat-main">
            <strong>{{ store.charityFeedTodayCount.value }}</strong>
            <span class="cm-stat-unit">篇新帖</span>
          </div>
          <div class="cm-stat-footer">
            <span>社区今日最新发布与免费额度推送</span>
          </div>
        </div>

        <!-- 卡片 2: 分类与总账 -->
        <div class="cm-stat-card">
          <div class="cm-stat-header">
            <span class="cm-stat-tag is-blue">
              <span v-html="icons.database" />
              <span>总收录</span>
            </span>
            <span class="cm-stat-pill is-blue">{{ selectedLabel }}</span>
          </div>
          <div class="cm-stat-main">
            <strong>{{ store.charityFeedTotalCount.value }}</strong>
            <span class="cm-stat-unit">条收录</span>
          </div>
          <div class="cm-stat-footer">
            <span>当前分页展示 <strong>{{ store.charityFeedDisplayedCount.value }}</strong> 条记录</span>
          </div>
        </div>

        <!-- 卡片 3: 代理节点网络 -->
        <div class="cm-stat-card">
          <div class="cm-stat-header">
            <span class="cm-stat-tag is-purple">
              <span v-html="icons.globe" />
              <span>代理网络</span>
            </span>
            <span class="cm-stat-pill is-purple">
              {{ store.charityProxyPoolSummary.value?.validCount ? "网络畅通" : "本地直连" }}
            </span>
          </div>
          <div class="cm-stat-main">
            <strong>{{ store.charityProxyPoolSummary.value?.validCount ?? 0 }}</strong>
            <span class="cm-stat-unit">有效节点</span>
          </div>
          <div class="cm-stat-footer">
            <span>极速候选 ≤500ms: <strong>{{ store.charityProxyPoolSummary.value?.candidateCount ?? 0 }}</strong> 个</span>
          </div>
        </div>

        <!-- 卡片 4: 时效与健康 -->
        <div class="cm-stat-card">
          <div class="cm-stat-header">
            <span class="cm-stat-tag is-emerald">
              <span v-html="icons.heartPulse" />
              <span>同步时间线</span>
            </span>
            <span class="cm-stat-pill is-emerald">5 min 轮询</span>
          </div>
          <div class="cm-stat-main">
            <strong class="cm-stat-time">{{ formatRelativeActivity(store.charityFeedLastFetchedAt.value) }}</strong>
          </div>
          <div class="cm-stat-footer">
            <span>上次同步: <code>{{ formatPublishedAt(store.charityFeedLastFetchedAt.value) }}</code></span>
          </div>
        </div>
      </section>

      <!-- 交互式指令工具条 (Interactive Command Bar) -->
      <section class="cm-command-strip" aria-label="筛选与搜索工具条">
        <div class="cm-strip-left">
          <!-- 标签源快速切换胶囊 -->
          <div class="cm-tag-pills-slider">
            <button
              v-for="tag in store.charityTags.value"
              :key="tag.id"
              type="button"
              class="cm-tag-pill"
              :class="{ active: store.selectedTagId.value === tag.id, 'is-disabled': tag.enabled === false }"
              @click="store.selectTag(tag.id)"
            >
              <span>{{ tag.name }}</span>
              <span v-if="tag.id === 'all' && store.charityFeedTotalCount.value > 0" class="cm-pill-count">
                {{ store.charityFeedTotalCount.value }}
              </span>
            </button>
          </div>

          <div class="cm-strip-divider" />

          <!-- 属性快捷过滤胶囊（筛选在后端执行，分页与筛选结果一致） -->
          <div class="cm-prop-filters">
            <button
              type="button"
              class="cm-filter-btn"
              :class="{ active: store.charityPropertyFilter.value === 'all' }"
              @click="store.setCharityPropertyFilter('all')"
            >全部</button>
            <button
              type="button"
              class="cm-filter-btn"
              :class="{ active: store.charityPropertyFilter.value === 'hot' }"
              title="回复数 ≥ 20 或浏览量 ≥ 500"
              @click="store.setCharityPropertyFilter('hot')"
            >
              <span>🔥 热门</span>
            </button>
            <button
              type="button"
              class="cm-filter-btn"
              :class="{ active: store.charityPropertyFilter.value === 'pinned' }"
              title="仅查看置顶公告与精华帖"
              @click="store.setCharityPropertyFilter('pinned')"
            >
              <span>📌 置顶</span>
            </button>
            <button
              type="button"
              class="cm-filter-btn"
              :class="{ active: store.charityPropertyFilter.value === 'today' }"
              title="仅查看今日发布的帖子"
              @click="store.setCharityPropertyFilter('today')"
            >
              <span>⚡ 今日</span>
            </button>
          </div>
        </div>

        <div class="cm-strip-right">
          <!-- 搜索输入框 -->
          <div class="cm-search-box">
            <span class="cm-search-icon" v-html="icons.search" />
            <input
              id="charity-search-input"
              class="cm-search-input"
              type="search"
              :value="store.searchKeyword.value"
              placeholder="搜索标题 / 作者 / 分类关键词…"
              @input="store.setSearchKeyword(($event.target as HTMLInputElement).value)"
            />
            <button
              v-if="store.searchKeyword.value"
              type="button"
              class="cm-search-clear"
              aria-label="清空搜索"
              @click="store.setSearchKeyword('')"
              v-html="icons.close"
            />
          </div>
        </div>
      </section>

      <!-- 主数据表格容器 (Main Feed Table) -->
      <section class="cm-feed-container" aria-label="公益社区帖子列表">
        <AppTable
          :rows="displayItems"
          :columns="topicColumns"
          :row-key="(item: any) => item.id"
          :loading="store.charityFeedLoading.value"
          empty-text="暂无匹配的社区公益记录"
          :page="store.charityFeedCurrentPage.value"
          :page-size="store.charityFeedPageSize.value"
          :page-size-options="[20, 50, 100]"
          :total="store.charityFeedTotalCount.value"
          :sorting="topicSorting"
          manual-pagination
          :row-class="topicRowClass"
          :selected-key="selectedPost?.id ?? null"
          clickable
          @select="(item: any) => openPostDetail(item)"
          @update:page="(page: number) => store.goCharityPage(page)"
          @update:page-size="(size: number) => store.setCharityPageSize(size)"
          @update:sorting="(s: any) => topicSorting = s"
        >
          <!-- 话题列 -->
          <template #cell-title="{ row }">
            <div class="cm-topic-cell">
              <div class="cm-topic-line">
                <span v-if="row.pinned" class="cm-pin-badge" title="置顶主题" v-html="icons.pin" />
                <span v-if="isToday(row.publishedAt)" class="cm-new-badge">NEW</span>
                <h2
                  class="cm-topic-title"
                  :title="`在浏览器中打开：${row.title}`"
                  @click.stop="openPostInBrowser(row)"
                >
                  {{ row.title }}
                </h2>
                <span v-if="store.selectedTagId.value === 'all' && row.feedNames?.length" class="cm-source-tags">
                  <span v-for="name in row.feedNames" :key="name" class="cm-source-tag">{{ name }}</span>
                </span>
              </div>

              <!-- 分类标签气泡组 -->
              <div v-if="row.categories?.length" class="cm-topic-tags-row">
                <span
                  v-for="category in row.categories.slice(0, 4)"
                  :key="category"
                  class="cm-category-chip"
                  :style="{
                    color: getCategoryTagStyle(category).color,
                    background: getCategoryTagStyle(category).bg,
                    borderColor: getCategoryTagStyle(category).border
                  }"
                >
                  {{ category }}
                </span>
              </div>
            </div>
          </template>

          <!-- 作者列 -->
          <template #cell-author="{ row }">
            <div class="cm-author-cell" :title="row.author || '未知作者'">
              <span class="cm-author-avatar">{{ authorInitials(row.author) }}</span>
              <span class="cm-author-name">{{ row.author || "—" }}</span>
            </div>
          </template>

          <!-- 发布时间列 -->
          <template #cell-publishedAt="{ row }">
            <time class="cm-time-cell" :datetime="row.publishedAt">{{ formatPublishedAt(row.publishedAt) }}</time>
          </template>

          <!-- 回复数 -->
          <template #cell-replyCount="{ row }">
            <span class="cm-metric-cell is-replies" :class="{ 'is-high': (row.replyCount ?? 0) >= 30 }">
              <span class="cm-metric-icon" v-html="icons.chat" />
              <strong>{{ formatCompactCount(row.replyCount) }}</strong>
            </span>
          </template>

          <!-- 浏览量 -->
          <template #cell-views="{ row }">
            <span class="cm-metric-cell is-views" :class="{ 'is-high': (row.views ?? 0) >= 1000 }">
              <span class="cm-metric-icon" v-html="icons.eye" />
              <strong>{{ formatCompactCount(row.views) }}</strong>
            </span>
          </template>

          <!-- 最近活跃 -->
          <template #cell-lastActivityAt="{ row }">
            <span class="cm-activity-chip">
              {{ formatRelativeActivity(row.lastActivityAt, row.publishedAt) }}
            </span>
          </template>

          <!-- 快捷操作列 -->
          <template #cell-actions="{ row }">
            <div class="cm-actions-cell" @click.stop>
              <button
                type="button"
                class="cm-action-icon-btn"
                :class="{ 'is-copied': copyFeedbackId === row.id }"
                :title="copyFeedbackId === row.id ? '已复制链接！' : '复制帖子链接'"
                @click="copyPostLink(row)"
              >
                <span v-html="copyFeedbackId === row.id ? icons.check : icons.copy" />
              </button>
              <button
                type="button"
                class="cm-action-icon-btn"
                title="在系统默认浏览器中打开"
                @click="openPostInBrowser(row)"
              >
                <span v-html="icons.external" />
              </button>
              <button
                type="button"
                class="cm-action-icon-btn"
                title="查看详情与提炼信息"
                @click="openPostDetail(row)"
              >
                <span v-html="icons.eye" />
              </button>
            </div>
          </template>
        </AppTable>
      </section>
    </div>

    <!-- ============================================================
         三大独立弹窗 (Teleport Modals)
         ============================================================ -->

    <!-- 1. 帖子详情与快速解析弹窗 (Post Detail Modal) -->
    <Teleport to="body">
      <Transition name="cm-modal-fade">
        <div v-if="selectedPost" class="cm-modal-backdrop">
          <section class="cm-modal-card is-detail" role="dialog" aria-modal="true">
            <header class="cm-modal-header">
              <div class="cm-modal-title-group">
                <div class="cm-modal-eyebrow">
                  <span v-if="selectedPost.pinned" class="cm-pin-badge" v-html="icons.pin" />
                  <span>公益帖子详情</span>
                </div>
                <h2>{{ selectedPost.title }}</h2>
              </div>
              <button type="button" class="cm-modal-close-btn" aria-label="关闭" @click="closePostDetail">×</button>
            </header>

            <div class="cm-modal-body">
              <!-- 元信息横幅 -->
              <div class="cm-detail-meta-bar">
                <div class="cm-meta-item">
                  <span class="cm-meta-label">发布者:</span>
                  <strong class="cm-meta-val">{{ selectedPost.author || "匿名用户" }}</strong>
                </div>
                <div class="cm-meta-item">
                  <span class="cm-meta-label">发布时间:</span>
                  <span class="cm-meta-val">{{ formatPublishedAt(selectedPost.publishedAt) }}</span>
                </div>
                <div class="cm-meta-item">
                  <span class="cm-meta-label">回复 / 浏览:</span>
                  <span class="cm-meta-val">{{ formatCompactCount(selectedPost.replyCount) }} 评 / {{ formatCompactCount(selectedPost.views) }} 阅</span>
                </div>
                <div class="cm-meta-item">
                  <span class="cm-meta-label">最后活跃:</span>
                  <span class="cm-meta-val">{{ formatRelativeActivity(selectedPost.lastActivityAt, selectedPost.publishedAt) }}</span>
                </div>
              </div>

              <!-- 分类标签 -->
              <div v-if="selectedPost.categories?.length" class="cm-detail-tags-row">
                <span
                  v-for="cat in selectedPost.categories"
                  :key="cat"
                  class="cm-category-chip"
                  :style="{
                    color: getCategoryTagStyle(cat).color,
                    background: getCategoryTagStyle(cat).bg,
                    borderColor: getCategoryTagStyle(cat).border
                  }"
                >
                  {{ cat }}
                </span>
              </div>

              <!-- 摘要 / 提取内容 -->
              <div class="cm-detail-content-box">
                <div class="cm-detail-section-title">
                  <span v-html="icons.sparkles" />
                  <span>帖子摘要与提炼</span>
                </div>
                <div v-if="selectedPost.summary" class="cm-detail-text">
                  {{ selectedPost.summary }}
                </div>
                <div v-else class="cm-detail-empty-summary">
                  该帖子暂无长正文快照，点击下方「在浏览器中打开」可直接查阅完整讨论与回复。
                </div>
              </div>

              <!-- 原始链接 -->
              <div class="cm-detail-link-box">
                <span class="cm-link-label">原始主题链接:</span>
                <code class="cm-link-code">{{ selectedPost.link }}</code>
              </div>
            </div>

            <footer class="cm-modal-footer">
              <button
                type="button"
                class="cm-btn-secondary"
                @click="copyPostLink(selectedPost!)"
              >
                <span v-html="copyFeedbackId === selectedPost.id ? icons.check : icons.copy" />
                <span>{{ copyFeedbackId === selectedPost.id ? "已复制链接" : "复制链接" }}</span>
              </button>
              <button
                type="button"
                class="cm-btn-primary"
                @click="openPostInBrowser(selectedPost!)"
              >
                <span v-html="icons.external" />
                <span>在浏览器中打开</span>
              </button>
              <button type="button" class="cm-btn-cancel" @click="closePostDetail">关闭</button>
            </footer>
          </section>
        </div>
      </Transition>
    </Teleport>

    <!-- 2. 标签源管理中心弹窗 (Tag Manager Modal) -->
    <Teleport to="body">
      <Transition name="cm-modal-fade">
        <div v-if="tagManagerOpen" class="cm-modal-backdrop">
          <section class="cm-modal-card is-tag-mgr" role="dialog" aria-modal="true">
            <header class="cm-modal-header">
              <div>
                <h2>管理监听标签源</h2>
                <p>配置需要自动轮询抓取的 Linux.do 标签频道与订阅源</p>
              </div>
              <button type="button" class="cm-modal-close-btn" aria-label="关闭" @click="closeTagManager">×</button>
            </header>

            <div class="cm-modal-body">
              <div v-if="tagManagerError" class="cm-dialog-error-box">
                <span v-html="icons.alert" />
                <span>{{ tagManagerError }}</span>
              </div>

              <!-- 添加新标签表单 -->
              <div class="cm-tag-add-card">
                <div class="cm-tag-add-title">添加新标签源</div>
                <div class="cm-tag-add-grid">
                  <input
                    v-model="newTagId"
                    class="cm-input"
                    type="text"
                    placeholder="标签 ID (如 1515 或 key)"
                  />
                  <input
                    v-model="newTagName"
                    class="cm-input"
                    type="text"
                    placeholder="显示名称 (如 公益推广)"
                  />
                  <button
                    type="button"
                    class="cm-btn-primary cm-add-btn"
                    :disabled="store.charitySourcesLoading.value"
                    @click="handleAddTag"
                  >
                    <span v-html="icons.plus" />
                    <span>添加标签</span>
                  </button>
                </div>
              </div>

              <!-- 现有标签列表 -->
              <div class="cm-tags-list-wrapper">
                <div class="cm-tags-list-header">
                  <span>已配置的标签频道 ({{ store.charityTags.value.filter(t => t.id !== 'all').length }})</span>
                </div>
                <div class="cm-tags-list">
                  <div
                    v-for="tag in store.charityTags.value.filter(t => t.id !== 'all')"
                    :key="tag.id"
                    class="cm-tag-item"
                    :class="{ 'is-disabled': tag.enabled === false }"
                  >
                    <label class="cm-switch-wrap" :title="tag.enabled === false ? '点击启用' : '点击禁用'">
                      <input
                        type="checkbox"
                        :checked="tag.enabled !== false"
                        @change="handleToggleTag(tag.id, ($event.target as HTMLInputElement).checked)"
                      />
                      <span class="cm-switch-slider" />
                    </label>

                    <div class="cm-tag-info">
                      <div class="cm-tag-name-row">
                        <strong>{{ tag.name }}</strong>
                        <button
                          type="button"
                          class="cm-tag-id-badge"
                          :title="`在浏览器中打开标签页（linux.do/tag/${tag.id}-tag/${tag.id}）`"
                          @click="openTagInBrowser(tag.id)"
                        >ID: {{ tag.id }}</button>
                        <span class="cm-tag-status-badge" :class="{ 'is-on': tag.enabled !== false }">
                          {{ tag.enabled === false ? "已停用" : "活跃监听" }}
                        </span>
                      </div>
                    </div>

                    <button
                      type="button"
                      class="cm-tag-delete-btn"
                      title="删除此标签源"
                      :disabled="store.charitySourcesLoading.value"
                      @click="handleRemoveTag(tag.id, tag.name)"
                      v-html="icons.trash"
                    />
                  </div>
                </div>
              </div>
            </div>

            <footer class="cm-modal-footer">
              <button type="button" class="cm-btn-cancel" @click="closeTagManager">完成并关闭</button>
            </footer>
          </section>
        </div>
      </Transition>
    </Teleport>

    <!-- 3. 同步与节点诊断日志终端弹窗 (Sync Log Terminal Modal) -->
    <Teleport to="body">
      <Transition name="cm-modal-fade">
        <div v-if="store.charitySyncLogOpen.value" class="cm-modal-backdrop">
          <section class="cm-modal-card is-terminal" role="dialog" aria-modal="true">
            <header class="cm-modal-header">
              <div>
                <h2>同步诊断与节点执行日志</h2>
                <p>
                  后台定时轮询、代理网络切换与抓取流水记录
                  <span v-if="store.charityProxyPoolSummary.value" class="cm-pool-pill">
                    ● 有效节点 {{ store.charityProxyPoolSummary.value.validCount }} · ≤500ms 极速 {{ store.charityProxyPoolSummary.value.candidateCount }}
                  </span>
                </p>
              </div>
              <button type="button" class="cm-modal-close-btn" aria-label="关闭" @click="store.closeCharitySyncLog()">×</button>
            </header>

            <div class="cm-modal-body cm-terminal-body">
              <!-- 筛选条 -->
              <div class="cm-terminal-toolbar">
                <div class="cm-log-filters">
                  <button
                    type="button"
                    class="cm-log-tab"
                    :class="{ active: logStatusFilter === 'all' }"
                    @click="logStatusFilter = 'all'"
                  >全部 ({{ store.charitySyncLog.value.length }})</button>
                  <button
                    type="button"
                    class="cm-log-tab"
                    :class="{ active: logStatusFilter === 'running' }"
                    @click="logStatusFilter = 'running'"
                  >进行中</button>
                  <button
                    type="button"
                    class="cm-log-tab"
                    :class="{ active: logStatusFilter === 'success' }"
                    @click="logStatusFilter = 'success'"
                  >成功</button>
                  <button
                    type="button"
                    class="cm-log-tab"
                    :class="{ active: logStatusFilter === 'error' }"
                    @click="logStatusFilter = 'error'"
                  >异常/失败</button>
                </div>

                <div class="cm-terminal-actions">
                  <button
                    type="button"
                    class="cm-btn-secondary cm-btn-sm"
                    :disabled="store.charitySyncLogLoading.value"
                    @click="store.loadCharitySyncLogs()"
                  >
                    <span v-html="icons.restore" />
                    <span>刷新</span>
                  </button>
                  <button
                    type="button"
                    class="cm-btn-secondary cm-btn-sm"
                    @click="store.clearCharitySyncLog()"
                  >
                    <span v-html="icons.trash" />
                    <span>清空</span>
                  </button>
                </div>
              </div>

              <!-- 终端日志记录 -->
              <div class="cm-terminal-screen">
                <div v-if="store.charitySyncLogLoading.value && !store.charitySyncLog.value.length" class="cm-terminal-empty">
                  <span class="cm-mini-spinner" />
                  <span>正在读取本地 SQLite 同步流水日志…</span>
                </div>
                <div v-else-if="!filteredSyncLogs.length" class="cm-terminal-empty">
                  暂无匹配的同步日志记录
                </div>
                <ol v-else class="cm-terminal-list">
                  <li
                    v-for="entry in filteredSyncLogs"
                    :key="entry.id"
                    :class="[`is-${entry.status}`, { 'is-expanded': expandedLogId === entry.id }]"
                    @click="toggleLogDetail(entry.id)"
                  >
                    <span class="cm-log-arrow">{{ expandedLogId === entry.id ? "▼" : "▶" }}</span>
                    <time class="cm-log-time">{{ formatLogTime(entry.at) }}</time>
                    <strong class="cm-log-feed">{{ entry.feedName || entry.feedId }}</strong>
                    <span class="cm-log-stage-badge">
                      <i>{{ stageText(entry.stage) }}</i>
                      <em :class="`is-${entry.status}`">{{ statusText(entry.status) }}</em>
                    </span>
                    <span class="cm-log-message" :title="entry.message">{{ entry.message }}</span>
                    <span class="cm-log-node">{{ entry.nodeName || "智能轮询调度" }}</span>
                    <span class="cm-log-duration" :class="{ 'is-live': entry.status === 'running' }">
                      {{ formatDuration(liveDurationMs(entry)) }}
                    </span>
                    <div v-if="expandedLogId === entry.id" class="cm-log-detail-box" @click.stop>
                      <div class="cm-log-detail-title">
                        <strong>{{ entry.feedName || entry.feedId }}</strong>
                        <span class="cm-log-detail-node">节点：{{ entry.nodeName || "智能轮询调度" }}</span>
                      </div>
                      <p class="cm-log-detail-summary">{{ logSummaryText(entry) }}</p>
                      <table v-if="logDetailRows(entry).length" class="cm-log-detail-table">
                        <thead v-if="isRoundLogEntry(entry)">
                          <tr>
                            <th>标签</th>
                            <th>状态</th>
                            <th>新增</th>
                            <th>更新</th>
                          </tr>
                        </thead>
                        <tbody>
                          <tr v-for="(row, rowIndex) in logDetailRows(entry)" :key="rowIndex">
                            <td v-for="(cell, cellIndex) in row" :key="cellIndex" :class="cell.cls">
                              {{ cell.text }}
                            </td>
                          </tr>
                        </tbody>
                      </table>
                    </div>
                  </li>
                </ol>
              </div>
            </div>

            <footer class="cm-modal-footer">
              <button type="button" class="cm-btn-cancel" @click="store.closeCharitySyncLog()">关闭</button>
            </footer>
          </section>
        </div>
      </Transition>
    </Teleport>
  </main>
</template>

<style scoped>
.cm-dashboard {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  background: var(--page-bg);
  color: var(--text);
  overflow: hidden;
}

/* ============================================================
   1. 顶部全景智控驾驶舱 (Cockpit Bar)
   ============================================================ */
.cm-cockpit-bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 12px 20px;
  background: var(--surface);
  border-bottom: 1px solid var(--line);
  flex-shrink: 0;
}

.cm-cockpit-left {
  display: flex;
  align-items: center;
  gap: 12px;
}

.cm-brand-section {
  display: flex;
  flex-direction: column;
  gap: 1px;
}

.cm-eyebrow-row {
  display: flex;
  align-items: center;
  gap: 6px;
}

.cm-live-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: #10b981;
  box-shadow: 0 0 8px #10b981;
  animation: cmPulse 2s infinite ease-in-out;
}

@keyframes cmPulse {
  0%, 100% { opacity: 1; transform: scale(1); }
  50% { opacity: 0.4; transform: scale(1.25); }
}

.cm-eyebrow-text {
  font-size: 10px;
  font-weight: 750;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--brand);
}

.cm-eyebrow-badge {
  padding: 1px 6px;
  border-radius: var(--r-full);
  background: color-mix(in srgb, var(--brand) 12%, transparent);
  color: var(--brand);
  font-size: 9.5px;
  font-weight: 700;
}

.cm-title-row h1 {
  font-size: 18px;
  font-weight: 750;
  color: var(--text);
  margin: 0;
  line-height: 1.2;
}

.cm-cockpit-subtitle {
  font-size: 11px;
  color: var(--muted);
  margin: 0;
}

.cm-cockpit-subtitle strong {
  color: var(--text);
  font-weight: 600;
}

.cm-cockpit-subtitle code {
  font-size: 10.5px;
  background: var(--page-bg);
  padding: 1px 4px;
  border-radius: 4px;
}

.cm-cockpit-right {
  display: flex;
  align-items: center;
  gap: 8px;
}

.cm-btn-primary {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 32px;
  padding: 0 12px;
  border-radius: var(--r-md, 8px);
  border: 1px solid color-mix(in srgb, var(--brand, #388bfd) 35%, transparent);
  background: color-mix(in srgb, var(--brand, #388bfd) 12%, var(--surface));
  color: var(--brand-deep, var(--brand, #388bfd));
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.15s ease;
}

.cm-btn-primary:hover:not(:disabled) {
  background: color-mix(in srgb, var(--brand, #388bfd) 20%, var(--surface));
  border-color: var(--brand);
  transform: translateY(-1px);
}

.cm-btn-primary:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.cm-btn-primary :deep(svg) {
  width: 13px;
  height: 13px;
}

.cm-btn-secondary {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 32px;
  padding: 0 11px;
  border-radius: var(--r-md, 8px);
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--text);
  font-size: 12px;
  font-weight: 550;
  cursor: pointer;
  transition: all 0.15s ease;
}

.cm-btn-secondary:hover {
  background: var(--surface-hover);
  border-color: var(--line-hover);
  transform: translateY(-1px);
}

.cm-btn-secondary.active {
  background: var(--brand-soft);
  border-color: var(--brand);
  color: var(--brand-deep);
}

.cm-btn-secondary :deep(svg) {
  width: 13px;
  height: 13px;
  color: var(--muted);
}

.cm-count-chip {
  padding: 1px 5px;
  border-radius: var(--r-full);
  background: var(--page-bg);
  color: var(--muted);
  font-size: 9.5px;
  font-weight: 700;
}

.cm-mini-spinner {
  width: 10px;
  height: 10px;
  border: 2px solid var(--line);
  border-top-color: var(--brand);
  border-radius: 50%;
  animation: cmSpin 0.8s infinite linear;
}

.is-spinning {
  animation: cmSpin 1s infinite linear;
}

@keyframes cmSpin {
  100% { transform: rotate(360deg); }
}

.cm-error-banner {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 20px;
  background: rgba(239, 68, 68, 0.1);
  border-bottom: 1px solid rgba(239, 68, 68, 0.2);
  color: #ef4444;
  font-size: 11.5px;
}

.cm-error-icon :deep(svg) {
  width: 15px;
  height: 15px;
}

/* ============================================================
   2. 核心主内容区 (Dashboard Body)
   ============================================================ */
.cm-dashboard-body {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  padding: 12px 18px;
  gap: 10px;
}

/* ROW 1: 4 KPI Cards (Compact Bento Deck) */
.cm-stats-deck {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 10px;
  flex-shrink: 0;
}

@media (max-width: 1100px) {
  .cm-stats-deck {
    grid-template-columns: repeat(2, 1fr);
  }
}

.cm-stat-card {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 10px);
  padding: 10px 14px;
  display: flex;
  flex-direction: column;
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.02);
  transition: all 0.15s ease;
}

.cm-stat-card:hover {
  border-color: var(--line-hover);
}

.cm-stat-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 6px;
  margin-bottom: 4px;
}

.cm-stat-tag {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 10px;
  font-weight: 750;
  letter-spacing: 0.05em;
  text-transform: uppercase;
}

.cm-stat-tag :deep(svg) {
  width: 12px;
  height: 12px;
}

.cm-stat-tag.is-orange { color: #f97316; }
.cm-stat-tag.is-blue { color: #3b82f6; }
.cm-stat-tag.is-purple { color: #a855f7; }
.cm-stat-tag.is-emerald { color: #10b981; }

.cm-stat-pill {
  padding: 1px 6px;
  border-radius: var(--r-full);
  font-size: 9.5px;
  font-weight: 700;
}
.cm-stat-pill.is-orange { background: rgba(249, 115, 22, 0.12); color: #f97316; }
.cm-stat-pill.is-blue { background: rgba(59, 130, 246, 0.12); color: #3b82f6; }
.cm-stat-pill.is-purple { background: rgba(168, 85, 247, 0.12); color: #a855f7; }
.cm-stat-pill.is-emerald { background: rgba(16, 185, 129, 0.12); color: #10b981; }

.cm-stat-main {
  display: flex;
  align-items: baseline;
  gap: 5px;
  margin-bottom: 4px;
}

.cm-stat-main strong {
  font-size: 22px;
  font-weight: 800;
  line-height: 1;
  font-variant-numeric: tabular-nums;
  letter-spacing: -0.02em;
}

.cm-stat-main strong.cm-stat-time {
  font-size: 16px;
  font-weight: 750;
}

.cm-stat-unit {
  font-size: 11px;
  color: var(--muted);
  font-weight: 600;
}

.cm-stat-footer {
  font-size: 10.5px;
  color: var(--muted);
  margin-top: auto;
}

.cm-stat-footer strong {
  color: var(--text);
}

.cm-stat-footer code {
  font-size: 10px;
  background: var(--page-bg);
  padding: 1px 4px;
  border-radius: 3px;
}

/* ============================================================
   3. 指令工具条 (Command & Filter Strip)
   ============================================================ */
.cm-command-strip {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 10px);
  padding: 6px 10px;
  flex-shrink: 0;
}

.cm-strip-left {
  display: flex;
  align-items: center;
  gap: 8px;
  overflow-x: auto;
  min-width: 0;
  flex: 1;
}

.cm-tag-pills-slider {
  display: inline-flex;
  align-items: center;
  gap: 3px;
  background: var(--page-bg);
  padding: 2px;
  border-radius: var(--r-md, 7px);
  border: 1px solid var(--line);
}

.cm-tag-pill {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  height: 26px;
  padding: 0 9px;
  border-radius: 5px;
  border: none;
  background: transparent;
  color: var(--muted);
  font-size: 11.5px;
  font-weight: 600;
  cursor: pointer;
  transition: background 0.12s ease, color 0.12s ease;
  white-space: nowrap;
}

.cm-tag-pill:hover {
  background: var(--surface);
  color: var(--text);
}

.cm-tag-pill.active {
  background: var(--surface);
  color: var(--brand);
  font-weight: 600;
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.06);
}

.cm-tag-pill.is-disabled {
  opacity: 0.5;
}

.cm-pill-count {
  padding: 0 4px;
  border-radius: var(--r-full);
  background: var(--surface-hover);
  font-size: 9.5px;
  color: var(--muted);
}

.cm-strip-divider {
  width: 1px;
  height: 18px;
  background: var(--line);
  flex-shrink: 0;
}

.cm-prop-filters {
  display: inline-flex;
  align-items: center;
  gap: 2px;
}

.cm-filter-btn {
  height: 26px;
  padding: 0 8px;
  border-radius: 5px;
  border: 1px solid transparent;
  background: transparent;
  color: var(--muted);
  font-size: 11px;
  font-weight: 600;
  cursor: pointer;
  transition: background 0.12s ease, color 0.12s ease;
  white-space: nowrap;
}

.cm-filter-btn:hover {
  color: var(--text);
  background: var(--surface-hover);
}

.cm-filter-btn.active {
  background: var(--page-bg);
  border-color: var(--line);
  color: var(--text);
  font-weight: 600;
}

.cm-strip-right {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-shrink: 0;
}

.cm-search-box {
  position: relative;
  display: inline-flex;
  align-items: center;
  width: 260px;
}

.cm-search-icon {
  position: absolute;
  left: 8px;
  width: 13px;
  height: 13px;
  color: var(--muted);
  display: flex;
  align-items: center;
  justify-content: center;
  pointer-events: none;
}

.cm-search-icon :deep(svg) {
  width: 13px;
  height: 13px;
}

.cm-search-input {
  width: 100%;
  height: 28px;
  padding: 0 26px 0 26px;
  border-radius: 6px;
  border: 1px solid var(--line);
  background: var(--page-bg);
  color: var(--text);
  font-size: 11.5px;
  outline: none;
  transition: all 0.15s ease;
}

.cm-search-input:focus {
  border-color: var(--brand);
  background: var(--surface);
  box-shadow: 0 0 0 2px var(--brand-soft);
}

.cm-search-clear {
  position: absolute;
  right: 6px;
  width: 16px;
  height: 16px;
  border-radius: 50%;
  border: none;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 0;
}

.cm-search-clear:hover {
  color: var(--text);
}

.cm-search-clear :deep(svg) {
  width: 10px;
  height: 10px;
}

/* ============================================================
   4. 数据表格与单元格 (Feed Table Container)
   ============================================================ */
.cm-feed-container {
  flex: 1;
  min-height: 0;
  width: 100%;
  display: flex;
  flex-direction: column;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-lg, 10px);
  overflow: hidden;
}

.cm-feed-container :deep(.app-table-wrap) {
  width: 100%;
  height: 100%;
  min-height: 0;
  display: flex;
  flex-direction: column;
}

.cm-feed-container :deep(.app-table-scroll) {
  width: 100%;
  min-width: 0;
  flex: 1;
  overflow: auto;
}

.cm-feed-container :deep(.app-table) {
  width: 100%;
  table-layout: fixed;
  border-collapse: separate;
  border-spacing: 0;
}

.cm-feed-container :deep(.app-table-th),
.cm-feed-container :deep(.app-table-td) {
  min-width: 0;
}

.cm-topic-cell {
  width: 100%;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 3px;
  padding: 3px 0;
}

.cm-topic-line {
  display: flex;
  align-items: center;
  gap: 6px;
  width: 100%;
  min-width: 0;
  overflow: hidden;
}

.cm-pin-badge {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  color: #ef4444;
  flex-shrink: 0;
}

.cm-pin-badge :deep(svg) {
  width: 13px;
  height: 13px;
}

.cm-new-badge {
  padding: 0 4px;
  border-radius: 3px;
  background: #10b981;
  color: #ffffff;
  font-size: 9px;
  font-weight: 800;
  letter-spacing: 0.04em;
  flex-shrink: 0;
}

.cm-topic-title {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: 13px;
  font-weight: 600;
  color: var(--text);
  margin: 0;
  cursor: pointer;
  transition: color 0.12s ease;
  line-height: 1.35;
}

.cm-topic-title:hover {
  color: var(--brand);
  text-decoration: underline;
}

.cm-source-tags {
  display: inline-flex;
  gap: 3px;
  flex-shrink: 0;
}

.cm-source-tag {
  padding: 0 4px;
  border-radius: 3px;
  background: var(--surface-hover);
  color: var(--muted);
  font-size: 9.5px;
  font-weight: 600;
  white-space: nowrap;
}

.cm-topic-tags-row {
  display: flex;
  align-items: center;
  gap: 4px;
  flex-wrap: wrap;
}

.cm-category-chip {
  padding: 0 5px;
  border-radius: 3px;
  border: 1px solid;
  font-size: 9.5px;
  font-weight: 650;
  letter-spacing: 0.02em;
}

.cm-author-cell {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
}

.cm-author-avatar {
  width: 20px;
  height: 20px;
  border-radius: 50%;
  background: var(--brand-soft);
  color: var(--brand-deep);
  font-size: 9.5px;
  font-weight: 750;
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
}

.cm-author-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.cm-time-cell {
  font-size: 11.5px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
}

.cm-metric-cell {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 11.5px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
}

.cm-metric-icon :deep(svg) {
  width: 12px;
  height: 12px;
  color: var(--muted);
}

.cm-metric-cell.is-high strong {
  color: var(--brand);
}

.cm-activity-chip {
  display: inline-block;
  padding: 2px 6px;
  border-radius: 4px;
  background: var(--surface-hover);
  font-size: 10.5px;
  color: var(--muted);
  white-space: nowrap;
}

.cm-actions-cell {
  display: inline-flex;
  align-items: center;
  gap: 4px;
}

.cm-action-icon-btn {
  width: 24px;
  height: 24px;
  border-radius: 4px;
  border: 1px solid transparent;
  background: transparent;
  color: var(--muted);
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  transition: all 0.12s ease;
  padding: 0;
}

.cm-action-icon-btn:hover {
  background: var(--surface-hover);
  border-color: var(--line);
  color: var(--text);
}

.cm-action-icon-btn.is-copied {
  color: #10b981;
}

.cm-action-icon-btn :deep(svg) {
  width: 12px;
  height: 12px;
}

.cm-loading-state,
.cm-empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 40px;
  color: var(--muted);
  font-size: 12.5px;
  gap: 10px;
}

.cm-loading-spinner {
  width: 24px;
  height: 24px;
  border: 2.5px solid var(--line);
  border-top-color: var(--brand);
  border-radius: 50%;
  animation: cmSpin 0.8s infinite linear;
}

.cm-empty-icon :deep(svg) {
  width: 32px;
  height: 32px;
  color: var(--muted);
}

/* ============================================================
   5. 独立弹窗体系 (Modal Dialogs)
   ============================================================ */
.cm-modal-backdrop {
  position: fixed;
  inset: 0;
  z-index: 2000;
  background: rgba(0, 0, 0, 0.5);
  backdrop-filter: blur(4px);
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 20px;
}

.cm-modal-card {
  width: 100%;
  max-width: 640px;
  max-height: 85vh;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-xl, 14px);
  box-shadow: 0 20px 48px rgba(0, 0, 0, 0.25);
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.cm-modal-card.is-detail {
  max-width: 720px;
}

.cm-modal-card.is-tag-mgr {
  max-width: 680px;
}

.cm-modal-card.is-terminal {
  max-width: 860px;
}

.cm-modal-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  padding: 14px 18px;
  border-bottom: 1px solid var(--line);
  flex-shrink: 0;
}

.cm-modal-title-group h2 {
  font-size: 15px;
  font-weight: 750;
  margin: 2px 0 0;
  line-height: 1.35;
}

.cm-modal-eyebrow {
  display: flex;
  align-items: center;
  gap: 5px;
  font-size: 9.5px;
  font-weight: 750;
  letter-spacing: 0.05em;
  color: var(--brand);
  text-transform: uppercase;
}

.cm-modal-header p {
  font-size: 11px;
  color: var(--muted);
  margin: 2px 0 0;
}

.cm-pool-pill {
  display: inline-block;
  margin-left: 6px;
  color: #10b981;
  font-weight: 600;
}

.cm-modal-close-btn {
  width: 28px;
  height: 28px;
  border-radius: 6px;
  border: none;
  background: transparent;
  color: var(--muted);
  font-size: 16px;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
}

.cm-modal-close-btn:hover {
  background: var(--surface-hover);
  color: var(--text);
}

.cm-modal-body {
  padding: 16px 18px;
  overflow-y: auto;
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.cm-modal-footer {
  padding: 10px 18px;
  border-top: 1px solid var(--line);
  display: flex;
  justify-content: flex-end;
  align-items: center;
  gap: 8px;
  background: var(--page-bg);
  flex-shrink: 0;
}

.cm-btn-cancel {
  height: 30px;
  padding: 0 14px;
  border-radius: var(--r-md, 6px);
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--text);
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
}

.cm-btn-cancel:hover {
  background: var(--surface-hover);
}

/* Detail Modal Inner Styles */
.cm-detail-meta-bar {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(130px, 1fr));
  gap: 8px;
  padding: 8px 12px;
  background: var(--page-bg);
  border-radius: var(--r-md, 8px);
  border: 1px solid var(--line);
}

.cm-meta-item {
  display: flex;
  flex-direction: column;
  gap: 1px;
}

.cm-meta-label {
  font-size: 10px;
  color: var(--muted);
}

.cm-meta-val {
  font-size: 11.5px;
  font-weight: 600;
  color: var(--text);
}

.cm-detail-tags-row {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.cm-detail-content-box {
  background: var(--page-bg);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  padding: 12px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.cm-detail-section-title {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 11px;
  font-weight: 700;
  color: var(--brand);
}

.cm-detail-section-title :deep(svg) {
  width: 13px;
  height: 13px;
}

.cm-detail-text {
  font-size: 12.5px;
  line-height: 1.6;
  color: var(--text);
  white-space: pre-wrap;
  word-break: break-word;
}

.cm-detail-empty-summary {
  font-size: 12px;
  color: var(--muted);
  line-height: 1.5;
}

.cm-detail-link-box {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 11px;
}

.cm-link-label {
  color: var(--muted);
  flex-shrink: 0;
}

.cm-link-code {
  font-size: 10.5px;
  background: var(--page-bg);
  padding: 2px 6px;
  border-radius: 4px;
  color: var(--brand);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* Tag Manager Modal Inner Styles */
.cm-dialog-error-box {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 12px;
  border-radius: 6px;
  background: rgba(239, 68, 68, 0.1);
  border: 1px solid rgba(239, 68, 68, 0.25);
  color: #ef4444;
  font-size: 11.5px;
}

.cm-tag-add-card {
  background: var(--page-bg);
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  padding: 12px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.cm-tag-add-title {
  font-size: 11.5px;
  font-weight: 700;
}

.cm-tag-add-grid {
  display: grid;
  grid-template-columns: 140px 150px auto;
  gap: 8px;
  align-items: center;
}

.cm-input {
  height: 30px;
  padding: 0 8px;
  border-radius: 5px;
  border: 1px solid var(--line);
  background: var(--surface);
  color: var(--text);
  font-size: 11.5px;
  outline: none;
}

.cm-input:focus {
  border-color: var(--brand);
}

.cm-add-btn {
  height: 30px;
}

.cm-tags-list-wrapper {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.cm-tags-list-header {
  font-size: 11px;
  font-weight: 700;
  color: var(--muted);
}

.cm-tags-list {
  display: flex;
  flex-direction: column;
  gap: 6px;
  max-height: 280px;
  overflow-y: auto;
}

.cm-tag-item {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 12px;
  background: var(--page-bg);
  border: 1px solid var(--line);
  border-radius: 6px;
  transition: all 0.12s ease;
}

.cm-tag-item.is-disabled {
  opacity: 0.6;
}

.cm-switch-wrap {
  position: relative;
  display: inline-block;
  width: 28px;
  height: 16px;
  flex-shrink: 0;
}

.cm-switch-wrap input {
  opacity: 0;
  width: 0;
  height: 0;
}

.cm-switch-slider {
  position: absolute;
  cursor: pointer;
  inset: 0;
  background-color: var(--line);
  border-radius: 16px;
  transition: 0.2s;
}

.cm-switch-slider:before {
  position: absolute;
  content: "";
  height: 12px;
  width: 12px;
  left: 2px;
  bottom: 2px;
  background-color: white;
  border-radius: 50%;
  transition: 0.2s;
}

.cm-switch-wrap input:checked + .cm-switch-slider {
  background-color: #10b981;
}

.cm-switch-wrap input:checked + .cm-switch-slider:before {
  transform: translateX(12px);
}

.cm-tag-info {
  flex: 1;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.cm-tag-name-row {
  display: flex;
  align-items: center;
  gap: 6px;
}

.cm-tag-name-row strong {
  font-size: 12px;
}

.cm-tag-id-badge {
  padding: 0 4px;
  border-radius: 3px;
  background: var(--surface);
  border: 1px solid var(--line);
  font-family: inherit;
  font-size: 9.5px;
  color: var(--muted);
  cursor: pointer;
}

.cm-tag-id-badge:hover {
  color: var(--brand);
  border-color: var(--brand);
}

.cm-tag-status-badge {
  font-size: 9.5px;
  color: var(--muted);
}

.cm-tag-status-badge.is-on {
  color: #10b981;
  font-weight: 600;
}

.cm-tag-delete-btn {
  width: 24px;
  height: 24px;
  border-radius: 4px;
  border: none;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
}

.cm-tag-delete-btn:hover {
  color: #ef4444;
  background: var(--surface-hover);
}

.cm-tag-delete-btn :deep(svg) {
  width: 13px;
  height: 13px;
}

/* Terminal Modal Inner Styles */
.cm-terminal-body {
  padding: 12px 16px;
  gap: 8px;
}

.cm-terminal-toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}

.cm-log-filters {
  display: inline-flex;
  align-items: center;
  gap: 2px;
  background: var(--page-bg);
  padding: 2px;
  border-radius: 6px;
  border: 1px solid var(--line);
}

.cm-log-tab {
  height: 24px;
  padding: 0 8px;
  border-radius: 4px;
  border: none;
  background: transparent;
  color: var(--muted);
  font-size: 10.5px;
  font-weight: 550;
  cursor: pointer;
}

.cm-log-tab.active {
  background: var(--surface);
  color: var(--text);
  font-weight: 700;
}

.cm-terminal-actions {
  display: flex;
  align-items: center;
  gap: 6px;
}

.cm-btn-sm {
  height: 24px;
  padding: 0 8px;
  font-size: 10.5px;
}

.cm-terminal-screen {
  background: #090d16;
  border-radius: var(--r-md, 8px);
  border: 1px solid rgba(255, 255, 255, 0.1);
  padding: 10px 12px;
  color: #f8fafc;
  font-family: monospace;
  min-height: 260px;
  max-height: 380px;
  overflow-y: auto;
}

.cm-terminal-empty {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  padding: 40px;
  color: #64748b;
  font-size: 11px;
}

.cm-terminal-list {
  list-style: none;
  padding: 0;
  margin: 0;
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 10.5px;
}

.cm-terminal-list li {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 6px;
  border-radius: 4px;
  cursor: pointer;
  transition: background 0.1s ease;
  flex-wrap: wrap;
}

.cm-terminal-list li:hover {
  background: rgba(255, 255, 255, 0.05);
}

.cm-log-arrow {
  color: #64748b;
  font-size: 8px;
  width: 10px;
}

.cm-log-time {
  color: #64748b;
}

.cm-log-feed {
  color: #38bdf8;
  width: 90px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.cm-log-stage-badge {
  display: inline-flex;
  align-items: center;
  gap: 4px;
}

.cm-log-stage-badge i {
  font-style: normal;
  color: #94a3b8;
}

.cm-log-stage-badge em {
  font-style: normal;
  padding: 1px 4px;
  border-radius: 3px;
  font-weight: 700;
  font-size: 9.5px;
}

.cm-log-stage-badge em.is-success { background: rgba(16, 185, 129, 0.2); color: #34d399; }
.cm-log-stage-badge em.is-running { background: rgba(56, 189, 248, 0.2); color: #38bdf8; }
.cm-log-stage-badge em.is-error,
.cm-log-stage-badge em.is-failed { background: rgba(239, 68, 68, 0.2); color: #f87171; }

.cm-log-message {
  color: #cbd5e1;
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.cm-log-node {
  color: #94a3b8;
  flex: 0 1 auto;
  max-width: 180px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.cm-log-duration {
  color: #64748b;
  font-variant-numeric: tabular-nums;
}

.cm-log-duration.is-live {
  color: #38bdf8;
}

.cm-log-detail-box {
  width: 100%;
  margin-top: 4px;
  padding: 8px 10px;
  background: rgba(0, 0, 0, 0.4);
  border-radius: 4px;
  border: 1px solid rgba(255, 255, 255, 0.08);
}

.cm-log-detail-title {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 10px;
  margin-bottom: 6px;
  font-size: 11px;
}

.cm-log-detail-title strong {
  color: #7dd3fc;
  flex-shrink: 0;
}

.cm-log-detail-node {
  color: #94a3b8;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.cm-log-detail-summary {
  margin: 0 0 8px;
  font-size: 10.5px;
  line-height: 1.55;
  color: #cbd5e1;
}

.cm-log-detail-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 10.5px;
}

.cm-log-detail-table th,
.cm-log-detail-table td {
  padding: 3px 8px;
  border: 1px solid rgba(255, 255, 255, 0.08);
  text-align: left;
}

.cm-log-detail-table th {
  color: #94a3b8;
  font-weight: 600;
  background: rgba(255, 255, 255, 0.04);
}

.cm-log-detail-table td.num {
  font-variant-numeric: tabular-nums;
  text-align: right;
}

.cm-log-detail-table td.is-success { color: #34d399; }
.cm-log-detail-table td.is-failed,
.cm-log-detail-table td.is-error { color: #f87171; }
.cm-log-detail-table td.is-cancelled { color: #94a3b8; }

.cm-log-detail-table tbody tr:last-child td {
  color: #e2e8f0;
  font-weight: 600;
  background: rgba(255, 255, 255, 0.03);
}

.cm-log-detail-box pre {
  margin: 0;
  font-family: inherit;
  font-size: 10px;
  color: #cbd5e1;
  white-space: pre-wrap;
  word-break: break-all;
}

.cm-modal-fade-enter-active,
.cm-modal-fade-leave-active {
  transition: opacity 0.2s ease;
}

.cm-modal-fade-enter-from,
.cm-modal-fade-leave-to {
  opacity: 0;
}
</style>
