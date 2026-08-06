<script setup lang="ts">
import { computed, onMounted } from "vue";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";

const store = useStore();

const sourceLabel = computed(() => {
  if (!store.charityFeedSourceProfileName.value) return "公开 RSS";
  return `Chrome ${store.charityFeedSourceProfileName.value}${store.charityFeedSourceAccountName.value ? ` · ${store.charityFeedSourceAccountName.value}` : ""}`;
});

const selectedLabel = computed(() => store.currentFeedName.value || store.selectedTagId.value || "公益监听");

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

function formatFetchedAt(value: string) {
  if (!value) return "尚未刷新";
  const normalized = value.includes("T") ? value : value.replace(" ", "T") + "Z";
  const date = new Date(normalized);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(date);
}

onMounted(() => {
  store.markCharityFeedRead();
  if (store.charityFeedItems.value.length === 0 && !store.charityFeedLoading.value) {
    void store.refreshCharityFeed();
  }
});
</script>

<template>
  <main class="charity-monitor-page">
    <header class="charity-monitor-header">
      <div>
        <span class="charity-monitor-eyebrow">LINUX.DO · {{ selectedLabel }}</span>
        <h1>公益监听 · {{ selectedLabel }}</h1>
        <p>每 5 分钟检查一次 RSS 变化，优先展示最近发布的帖子。</p>
      </div>
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
        class="secondary-button charity-refresh-button"
        type="button"
        :disabled="store.charityFeedLoading.value"
        @click="store.refreshCharityFeed()"
      >
        <span :class="{ 'is-spinning': store.charityFeedLoading.value }" v-html="icons.restore" />
        <span>{{ store.charityFeedLoading.value ? "刷新中…" : "立即刷新" }}</span>
      </button>
    </header>

    <div class="charity-monitor-scroll">
      <section class="charity-monitor-summary">
        <div>
          <strong>{{ store.charityFeedItems.value.length }}</strong>
          <span>最近帖子</span>
        </div>
        <div>
          <strong>{{ store.charityFeedUnreadCount.value }}</strong>
          <span>新增帖子</span>
        </div>
        <div>
          <strong>{{ store.charityFeedUpdatedCount.value }}</strong>
          <span>内容变化</span>
        </div>
        <div class="charity-monitor-source">
          <strong>{{ sourceLabel }}</strong>
          <span>上次刷新 {{ formatFetchedAt(store.charityFeedLastFetchedAt.value) }}</span>
        </div>
      </section>

      <div v-if="store.charityFeedError.value" class="charity-monitor-error">
        <span v-html="icons.wifiOff" />
        <div>
          <strong>RSS 刷新失败</strong>
          <p>{{ store.charityFeedError.value }}</p>
        </div>
      </div>

      <div v-if="store.charityFeedLoading.value && store.charityFeedItems.value.length === 0" class="charity-monitor-empty">
        <span class="is-spinning" v-html="icons.restore" />
        <strong>正在读取公益推广 RSS…</strong>
      </div>

      <section v-else-if="store.charityFeedItems.value.length" class="charity-post-list" aria-label="最近公益帖子">
        <article
          v-for="(item, index) in store.charityFeedItems.value"
          :key="item.id"
          class="charity-post-card"
          :class="{ 'is-new': item.isNew }"
        >
          <div class="charity-post-rank">{{ String(index + 1).padStart(2, "0") }}</div>
          <div class="charity-post-content">
            <header>
              <div class="charity-post-meta">
                <span v-if="item.isNew" class="charity-new-badge">NEW</span>
                <time>{{ formatPublishedAt(item.publishedAt) }}</time>
                <span v-if="item.author">{{ item.author }}</span>
              </div>
              <h2>{{ item.title }}</h2>
            </header>
            <p v-if="item.summary">{{ item.summary }}</p>
            <footer>
              <div class="charity-post-tags">
                <span v-for="category in item.categories.slice(0, 4)" :key="category">{{ category }}</span>
              </div>
              <button class="charity-open-button" type="button" @click="store.openExternal(item.link)">
                <span>查看帖子</span><span v-html="icons.external" />
              </button>
            </footer>
          </div>
        </article>
      </section>

      <div v-else-if="!store.charityFeedLoading.value" class="charity-monitor-empty">
        <span v-html="icons.heartPulse" />
        <strong>暂未获取到公益帖子</strong>
      </div>
    </div>
  </main>
</template>
