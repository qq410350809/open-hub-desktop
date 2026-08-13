<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import AppTable, { type AppTableColumn } from "./AppTable.vue";

const store = useStore();

/** 标签管理弹窗状态 */
const tagManagerOpen = ref(false);
const newTagId = ref("");
const newTagName = ref("");
const newTagUrl = ref("");
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
  newTagUrl.value = "";
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
    await store.addCharitySource(id, name, newTagUrl.value.trim() || undefined);
    newTagId.value = "";
    newTagName.value = "";
    newTagUrl.value = "";
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
  if (!confirm(`确定删除标签「${name}」吗？已抓取的帖子不会被删除。`)) return;
  try {
    await store.removeCharitySource(id);
  } catch (cause) {
    tagManagerError.value = String(cause);
  }
}

/** 进行中任务耗时前端本地跳动（不依赖后端每秒写库）。 */
const nowTick = ref(Date.now());
const expandedLogId = ref<number | null>(null);

function toggleLogDetail(id: number) {
  expandedLogId.value = expandedLogId.value === id ? null : id;
}

function logDetailText(entry: { message?: string; nodeName?: string; durationMs?: number | null; status: string; at: string }) {
  const lines: string[] = [];
  if (entry.message) lines.push(entry.message);
  if (entry.nodeName) lines.push(`节点：${entry.nodeName}`);
  lines.push(`耗时：${formatDuration(liveDurationMs(entry))}`);
  return lines.join("\n");
}
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
  // 触发依赖 nowTick，使 running 行每 250ms 重算
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

const pageLabel = computed(() => {
  const shown = store.charityFeedDisplayedCount.value;
  const total = store.charityFeedTotalCount.value || shown;
  return `${shown} / ${total}`;
});

function formatPublishedAt(value: string) {
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
  const amount = Number(value ?? 0);
  if (!Number.isFinite(amount) || amount <= 0) return "0";
  if (amount < 1000) return String(Math.round(amount));
  if (amount < 10000) {
    const text = (amount / 1000).toFixed(1).replace(/\.0$/, "");
    return `${text}k`;
  }
  if (amount < 1000000) return `${Math.round(amount / 1000)}k`;
  return `${(amount / 1000000).toFixed(1).replace(/\.0$/, "")}m`;
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
  if (minutes < 60) return `${minutes} 分钟`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} 小时`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${days} 天`;
  return formatPublishedAt(raw);
}

const topicColumns: AppTableColumn[] = [
  { key: "title", title: "话题", width: "minmax(220px,1fr)" },
  { key: "author", title: "作者", width: "120px" },
  { key: "publishedAt", title: "创建时间", width: "120px" },
  { key: "replyCount", title: "回复", width: "70px", align: "right", sortable: true },
  { key: "views", title: "浏览量", width: "80px", align: "right", sortable: true },
  { key: "lastActivityAt", title: "活动", width: "110px" },
];

function topicRowClass(row: { isNew?: boolean; pinned?: boolean }) {
  return [row.isNew && "is-new", row.pinned && "is-pinned"].filter(Boolean).join(" ");
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
  if (ms == null || ms < 0) return "—";
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

function statusText(status: string) {
  switch (status) {
    case "running":
      return "进行中";
    case "success":
      return "成功";
    case "failed":
      return "失败";
    case "error":
      return "失败";
    case "skipped":
      return "跳过";
    case "cancelled":
      return "已取消";
    default:
      return status || "日志";
  }
}

function stageText(stage: string) {
  switch (stage) {
    case "poll":
      return "后台轮询";
    case "manual":
      return "手动刷新";
    case "trying":
      return "尝试节点";
    case "done":
      return "同步完成";
    case "skipped":
      return "跳过";
    case "error":
      return "同步失败";
    case "failed":
      return "节点失败";
    default:
      return stage || "—";
  }
}

function onLogBackdropClick(event: MouseEvent) {
  if (event.target === event.currentTarget) store.closeCharitySyncLog();
}

onMounted(() => {
  // 进入页面只查本地库；同步完全后台，不在这里触发
  void store.loadCharityFeedLocal();
  // 已读标记延后，避免和首屏读库抢同一把锁
  window.setTimeout(() => {
    void store.markCharityFeedRead();
  }, 400);
  // 日志弹窗打开时保持 tick，让 running 耗时跳动
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
  <main class="charity-monitor-page">
    <header class="charity-monitor-header">
      <div>
        <span class="charity-monitor-eyebrow">LINUX.DO · {{ selectedLabel }}</span>
        <h1>公益监听</h1>
        <p>定时同步：每 5 分钟（:00/:05/:10…）。</p>
      </div>
      <div class="charity-header-actions">
        <button
          class="secondary-button"
          type="button"
          :class="{ active: store.charitySyncLogOpen.value }"
          @click="store.toggleCharitySyncLog()"
        >
          <span v-html="icons.info" />
          <span>同步日志</span>
        </button>
        <button
          class="secondary-button charity-refresh-button"
          type="button"
          :disabled="store.charityFeedRefreshAllBusy.value"
          title="临时触发：取消未完成任务并立即同步全部标签（定时仍为每 5 分钟对齐点）"
          @click="store.refreshCharityFeed()"
        >
          <span
            :class="{ 'is-spinning': store.charityFeedRefreshAllBusy.value }"
            v-html="icons.restore"
          />
          <span>{{ store.charityFeedRefreshAllBusy.value ? "正在提交…" : "立即刷新全部" }}</span>
        </button>
      </div>
    </header>

    <div class="charity-monitor-topbar">
      <div v-if="store.charityFeedSyncing.value" class="charity-monitor-status">
        <span class="is-spinning" v-html="icons.restore" />
        <span>正在后台同步 {{ store.charityTags.value.filter(t => t.id !== "all" && t.enabled !== false).length }} 个标签，完成后自动刷新列表…</span>
      </div>

      <!-- 筛选 + 统计整合为一块工具条 -->
      <section class="charity-toolbar" aria-label="筛选与统计">
        <div class="charity-toolbar-filter">
          <label class="charity-tag-label" for="charity-tag-select">标签</label>
          <select
            id="charity-tag-select"
            class="charity-tag-select"
            :value="store.selectedTagId.value"
            @change="store.selectTag(($event.target as HTMLSelectElement).value)"
          >
            <option v-for="tag in store.charityTags.value" :key="tag.id" :value="tag.id">
              {{ tag.name }}{{ tag.enabled === false ? ' (已禁用)' : '' }}
            </option>
          </select>
          <button
            class="charity-tag-manage-button"
            type="button"
            title="管理监听标签"
            @click="openTagManager()"
            v-html="icons.settings"
          />
          <div class="charity-search-box">
            <span class="charity-search-icon" v-html="icons.search" aria-hidden="true" />
            <input
              id="charity-search-input"
              class="charity-search-input"
              type="search"
              :value="store.searchKeyword.value"
              placeholder="搜索标题 / 作者 / 分类…"
              @input="store.setSearchKeyword(($event.target as HTMLInputElement).value)"
            />
            <button
              v-if="store.searchKeyword.value"
              class="charity-search-clear"
              type="button"
              aria-label="清空搜索"
              @click="store.setSearchKeyword(''); ($event.target as HTMLButtonElement).closest('.charity-search-box')?.querySelector('input')?.focus()"
              v-html="icons.close"
            />
          </div>
        </div>
        <div class="charity-summary-stats">
          <div class="charity-summary-stat" title="今天发布的帖子数">
            <strong>{{ store.charityFeedTodayCount.value }}</strong>
            <span>今日帖子</span>
          </div>
          <div class="charity-summary-stat" title="当前标签帖子总数">
            <strong>{{ store.charityFeedTotalCount.value }}</strong>
            <span>共 {{ pageLabel.split(" / ")[0] }} 条</span>
          </div>
          <div class="charity-summary-stat" title="未读新帖">
            <strong>{{ store.charityFeedSelectedUnreadCount.value }}</strong>
            <span>未读</span>
          </div>
        </div>
      </section>

      <div v-if="store.charityFeedError.value" class="charity-monitor-error">
        <span v-html="icons.wifiOff" />
        <div>
          <strong>{{
            store.charityFeedError.value.includes("跳过") ? "本轮已跳过" : "同步失败"
          }}</strong>
          <p>{{ store.charityFeedError.value }}</p>
        </div>
      </div>
    </div>

    <div class="charity-monitor-body">
      <div
        v-if="store.charityFeedLoading.value && store.charityFeedItems.value.length === 0"
        class="charity-monitor-empty"
      >
        <span class="is-spinning" v-html="icons.restore" />
        <strong>正在读取本地公益数据…</strong>
      </div>

      <section
        v-else-if="store.charityFeedItems.value.length"
        class="charity-topic-list"
        aria-label="最近公益帖子"
      >
        <button
          v-if="store.charityFeedSelectedUnreadCount.value > 0"
          class="charity-new-topics-banner"
          type="button"
          @click="store.markCharityFeedRead()"
        >
          查看 {{ store.charityFeedSelectedUnreadCount.value }} 个新的或更新的话题
        </button>

        <AppTable
          :rows="store.charityFeedItems.value"
          :columns="topicColumns"
          :row-key="(item: any) => item.id"
          :loading="store.charityFeedLoading.value"
          empty-text="本地暂无公益帖子"
          :page="store.charityFeedCurrentPage.value"
          :page-size="20"
          :page-size-options="[20]"
          :total="store.charityFeedTotalCount.value"
          manual-pagination
          :row-class="topicRowClass"
          clickable
          @row-click="(item: any) => store.openExternal(item.link)"
          @update:page="(page: number) => store.goCharityPage(page)"
        >
          <template #cell-title="{ row }">
            <div class="charity-topic-cell">
              <div class="charity-topic-title-row">
                <span
                  v-if="row.pinned"
                  class="charity-topic-pin"
                  title="置顶"
                  aria-hidden="true"
                  v-html="icons.pin"
                ></span>
                <h2 :title="row.title">{{ row.title }}</h2>
                <span v-if="row.isNew" class="charity-new-badge">NEW</span>
                <span
                  v-if="store.selectedTagId.value === 'all' && row.feedNames?.length"
                  class="charity-topic-feed-tags"
                >
                  <span v-for="name in row.feedNames" :key="name" class="charity-topic-feed-tag">{{ name }}</span>
                </span>
              </div>
              <div class="charity-topic-meta" v-if="row.categories.length">
                <span
                  v-for="category in row.categories.slice(0, 3)"
                  :key="category"
                  class="charity-topic-tag"
                  :class="{ 'is-announcement': /公告|置顶|官方/.test(category) }"
                >{{ category }}</span>
              </div>
            </div>
          </template>
          <template #cell-author="{ row }">{{ row.author || '—' }}</template>
          <template #cell-publishedAt="{ row }"><time :datetime="row.publishedAt">{{ formatPublishedAt(row.publishedAt) }}</time></template>
          <template #cell-replyCount="{ row }"><strong>{{ formatCompactCount(row.replyCount) }}</strong></template>
          <template #cell-views="{ row }"><strong>{{ formatCompactCount(row.views) }}</strong></template>
          <template #cell-lastActivityAt="{ row }">{{ formatRelativeActivity(row.lastActivityAt, row.publishedAt) }}</template>
        </AppTable>
      </section>

      <div
        v-else
        class="charity-monitor-empty"
      >
        <span v-html="icons.heartPulse" />
        <strong>本地暂无公益帖子</strong>
      </div>
    </div>

    <Teleport to="body">
      <div
        v-if="tagManagerOpen"
        class="charity-sync-log-backdrop"
        @click="closeTagManager()"
      >
        <section
          class="charity-sync-log-dialog charity-tag-manager-dialog"
          role="dialog"
          aria-modal="true"
          aria-labelledby="charity-tag-manager-title"
          @click.stop
        >
          <header class="charity-sync-log-header">
            <div>
              <h2 id="charity-tag-manager-title">管理监听标签</h2>
              <p>添加或移除 Linux.do 标签，标签 ID 即 Linux.do 标签编号。</p>
            </div>
            <div class="charity-sync-log-actions">
              <button
                class="close-button"
                type="button"
                aria-label="关闭"
                @click="closeTagManager()"
                v-html="icons.close"
              />
            </div>
          </header>

          <div class="charity-sync-log-body charity-tag-manager-body">
            <div v-if="tagManagerError" class="charity-tag-manager-error">{{ tagManagerError }}</div>

            <div class="charity-tag-add-row">
              <input
                v-model="newTagId"
                class="charity-tag-input"
                type="text"
                placeholder="标签 ID（如 1515）"
              />
              <input
                v-model="newTagName"
                class="charity-tag-input charity-tag-name-input"
                type="text"
                placeholder="显示名称（如 公益推广）"
              />
              <input
                v-model="newTagUrl"
                class="charity-tag-input charity-tag-url-input"
                type="text"
                placeholder="自定义 URL（可选，留空自动生成）"
              />
              <button
                class="primary-button"
                type="button"
                :disabled="store.charitySourcesLoading.value"
                @click="handleAddTag()"
              >
                <span v-html="icons.plus" />
                <span>添加</span>
              </button>
            </div>

            <ol class="charity-tag-list">
              <li
                v-for="tag in store.charityTags.value.filter(t => t.id !== 'all')"
                :key="tag.id"
                :class="{ 'is-disabled': tag.enabled === false }"
              >
                <label class="charity-tag-toggle">
                  <input
                    type="checkbox"
                    :checked="tag.enabled !== false"
                    @change="handleToggleTag(tag.id, ($event.target as HTMLInputElement).checked)"
                  />
                </label>
                <span class="charity-tag-id">{{ tag.id }}</span>
                <span class="charity-tag-name-display">{{ tag.name }}</span>
                <button
                  class="charity-tag-remove-button"
                  type="button"
                  title="删除标签"
                  :disabled="store.charitySourcesLoading.value"
                  @click="handleRemoveTag(tag.id, tag.name)"
                  v-html="icons.trash"
                />
              </li>
            </ol>

            <div v-if="store.charityTags.value.filter(t => t.id !== 'all').length === 0" class="charity-sync-log-empty">
              暂无监听标签
            </div>
          </div>
        </section>
      </div>
    </Teleport>

    <Teleport to="body">
      <div
        v-if="store.charitySyncLogOpen.value"
        class="charity-sync-log-backdrop"
        @click="onLogBackdropClick"
      >
        <section
          class="charity-sync-log-dialog"
          role="dialog"
          aria-modal="true"
          aria-labelledby="charity-sync-log-title"
          @click.stop
        >
          <header class="charity-sync-log-header">
            <div>
              <h2 id="charity-sync-log-title">同步日志</h2>
              <p>
                后台轮询与手动刷新记录会持久保存。
                <span v-if="store.charityProxyPoolSummary.value" class="charity-pool-summary">
                  池中有效节点
                  <strong>{{ store.charityProxyPoolSummary.value.validCount }}</strong>
                  · ≤500ms 候选
                  <strong>{{ store.charityProxyPoolSummary.value.candidateCount }}</strong>
                </span>
              </p>
            </div>
            <div class="charity-sync-log-actions">
              <button
                class="text-button"
                type="button"
                :disabled="store.charitySyncLogLoading.value"
                @click="store.loadCharitySyncLogs()"
              >
                刷新
              </button>
              <button class="text-button" type="button" @click="store.clearCharitySyncLog()">
                清空
              </button>
              <button
                class="close-button"
                type="button"
                aria-label="关闭同步日志"
                @click="store.closeCharitySyncLog()"
                v-html="icons.close"
              />
            </div>
          </header>

          <div class="charity-sync-log-body">
            <div v-if="store.charitySyncLogLoading.value && !store.charitySyncLog.value.length" class="charity-sync-log-empty">
              正在读取本地同步日志…
            </div>
            <div v-else-if="!store.charitySyncLog.value.length" class="charity-sync-log-empty">
              暂无同步记录
            </div>
            <ol v-else class="charity-sync-log-list">
              <li
                v-for="entry in store.charitySyncLog.value"
                :key="entry.id"
                :class="[`is-${entry.status}`, { 'is-expanded': expandedLogId === entry.id }]"
                @click="toggleLogDetail(entry.id)"
              >
                <span class="log-expand-arrow" aria-hidden="true">{{
                  expandedLogId === entry.id ? "▾" : "▸"
                }}</span>
                <time>{{ formatLogTime(entry.at) }}</time>
                <strong class="log-feed">{{ entry.feedName || entry.feedId }}</strong>
                <span class="log-stage">
                  <i>{{ stageText(entry.stage) }}</i>
                  <em>{{ statusText(entry.status) }}</em>
                </span>
                <span class="log-node">{{ entry.nodeName || "排队/切换中…" }}</span>
                <span class="log-duration" :class="{ 'is-live': entry.status === 'running' }">
                  {{ formatDuration(liveDurationMs(entry)) }}
                </span>
                <div v-if="expandedLogId === entry.id" class="log-detail" @click.stop>
                  <pre>{{ logDetailText(entry) }}</pre>
                </div>
              </li>
            </ol>
          </div>
        </section>
      </div>
    </Teleport>
  </main>
</template>
