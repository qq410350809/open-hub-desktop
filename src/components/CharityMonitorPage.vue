<script setup lang="ts">
import { computed, onMounted } from "vue";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";

const store = useStore();

const sourceLabel = computed(() => {
  if (store.charityFeedUsedNodeName.value) return store.charityFeedUsedNodeName.value;
  if (store.charityFeedSourceProfileName.value) {
    return `Chrome ${store.charityFeedSourceProfileName.value}${
      store.charityFeedSourceAccountName.value ? ` · ${store.charityFeedSourceAccountName.value}` : ""
    }`;
  }
  return "本地库";
});

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


function formatFetchedAt(value: string) {
  if (!value) return "尚未同步";
  const normalized = value.includes("T") ? value : value.replace(" ", "T") + "Z";
  const date = new Date(normalized);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(date);
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
});
</script>

<template>
  <main class="charity-monitor-page">
    <header class="charity-monitor-header">
      <div>
        <span class="charity-monitor-eyebrow">LINUX.DO · {{ selectedLabel }}</span>
        <h1>公益监听 · {{ selectedLabel }}</h1>
        <p>界面只读本地库。后台定时同步过程请点“同步日志”查看。</p>
      </div>
      <div class="charity-header-actions">
        <div class="charity-tag-picker">
          <label class="charity-tag-label" for="charity-tag-select">标签</label>
          <select
            id="charity-tag-select"
            class="charity-tag-select"
            :value="store.selectedTagId.value"
            @change="store.selectTag(($event.target as HTMLSelectElement).value)"
          >
            <option v-for="tag in store.charityTags.value" :key="tag.id" :value="tag.id">
              {{ tag.name }}
            </option>
          </select>
        </div>
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
          title="取消全部未完成历史任务，并立即刷新所有标签"
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

    <div class="charity-monitor-scroll">
      <section class="charity-monitor-summary" aria-label="当前标签概览">
        <div class="charity-summary-main">
          <div class="charity-summary-stat">
            <strong>{{ pageLabel }}</strong>
            <span>已加载 / 本地总数</span>
          </div>
          <div class="charity-summary-stat">
            <strong>{{ store.charityFeedSelectedUnreadCount.value }}</strong>
            <span>本标签未读</span>
          </div>
          <div class="charity-summary-stat">
            <strong>{{ store.charityFeedUpdatedCount.value }}</strong>
            <span>最近同步变更</span>
          </div>
        </div>
        <div class="charity-summary-meta">
          <div>
            <small>数据来源</small>
            <strong>{{ sourceLabel }}</strong>
          </div>
          <div>
            <small>上次同步</small>
            <strong>{{ formatFetchedAt(store.charityFeedLastFetchedAt.value) }}</strong>
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
        <div class="charity-topic-table" role="table">
          <div class="charity-topic-header" role="row">
            <span class="col-topic" role="columnheader">话题</span>
            <span class="col-author" role="columnheader">作者</span>
            <span class="col-created" role="columnheader">创建时间</span>
            <span class="col-replies" role="columnheader">回复</span>
            <span class="col-views" role="columnheader">浏览量</span>
            <span class="col-activity" role="columnheader">活动</span>
          </div>

          <button
            v-if="store.charityFeedSelectedUnreadCount.value > 0"
            class="charity-new-topics-banner"
            type="button"
            @click="store.markCharityFeedRead()"
          >
            查看 {{ store.charityFeedSelectedUnreadCount.value }} 个新的或更新的话题
          </button>

          <article
            v-for="item in store.charityFeedItems.value"
            :key="item.id"
            class="charity-topic-row"
            :class="{ 'is-new': item.isNew, 'is-pinned': item.pinned }"
            role="row"
            tabindex="0"
            @click="store.openExternal(item.link)"
            @keydown.enter.prevent="store.openExternal(item.link)"
            @keydown.space.prevent="store.openExternal(item.link)"
          >
            <div class="col-topic" role="cell">
              <div class="charity-topic-title-row">
                <span
                  v-if="item.pinned"
                  class="charity-topic-pin"
                  title="置顶"
                  aria-hidden="true"
                  v-html="icons.pin"
                ></span>
                <h2 :title="item.title">{{ item.title }}</h2>
                <span v-if="item.isNew" class="charity-new-badge">NEW</span>
              </div>
              <div class="charity-topic-meta" v-if="item.categories.length">
                <span
                  v-for="category in item.categories.slice(0, 3)"
                  :key="category"
                  class="charity-topic-tag"
                  :class="{ 'is-announcement': /公告|置顶|官方/.test(category) }"
                >{{ category }}</span>
              </div>
            </div>

            <div class="col-author" role="cell" :title="item.author || '未知作者'">
              <span>{{ item.author || '—' }}</span>
            </div>
            <div class="col-created" role="cell" :title="item.publishedAt || '创建时间未知'">
              <time :datetime="item.publishedAt">{{ formatPublishedAt(item.publishedAt) }}</time>
            </div>
            <div class="col-replies" role="cell" title="回复数">
              <strong>{{ formatCompactCount(item.replyCount) }}</strong>
            </div>
            <div class="col-views" role="cell" title="浏览量">
              <strong>{{ formatCompactCount(item.views) }}</strong>
            </div>
            <div class="col-activity" role="cell" title="最近活动">
              <span>{{ formatRelativeActivity(item.lastActivityAt, item.publishedAt) }}</span>
            </div>
          </article>
        </div>
      </section>

      <div
        v-if="store.charityFeedItems.value.length && store.charityFeedHasMore.value"
        class="charity-load-more"
      >
        <button
          class="secondary-button"
          type="button"
          :disabled="store.charityFeedLoadingMore.value || store.charityFeedLoading.value"
          @click="store.loadMoreCharityFeed()"
        >
          <span
            :class="{ 'is-spinning': store.charityFeedLoadingMore.value }"
            v-html="icons.restore"
          />
          <span>{{
            store.charityFeedLoadingMore.value
              ? "加载中…"
              : `加载更多（${store.charityFeedDisplayedCount.value}/${store.charityFeedTotalCount.value}）`
          }}</span>
        </button>
      </div>

      <div
        v-else-if="!store.charityFeedLoading.value && !store.charityFeedItems.value.length"
        class="charity-monitor-empty"
      >
        <span v-html="icons.heartPulse" />
        <strong>本地暂无公益帖子</strong>
      </div>
    </div>

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
              <p>后台轮询与手动刷新记录会持久保存，重启后仍可查看。</p>
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
                :class="[`is-${entry.status}`]"
                :title="entry.message"
              >
                <time>{{ formatLogTime(entry.at) }}</time>
                <strong class="log-feed">{{ entry.feedName || entry.feedId }}</strong>
                <span class="log-stage">
                  <i>{{ stageText(entry.stage) }}</i>
                  <em>{{ statusText(entry.status) }}</em>
                </span>
                <span class="log-node">{{ entry.nodeName || "—" }}</span>
                <span class="log-duration">{{ formatDuration(entry.durationMs) }}</span>
              </li>
            </ol>
          </div>
        </section>
      </div>
    </Teleport>
  </main>
</template>
