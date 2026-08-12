<script setup lang="ts">
import { computed } from "vue";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import { usePreferences } from "../composables/usePreferences";
import { formatCompact, localDateOf, toLocalDate } from "../composables/tokenStatsAgg";

const store = useStore();
const { preferences, updatePreferences } = usePreferences();

const navItems = computed(() => [
  {
    id: "library",
    label: "站点库",
    icon: icons.database,
    active: store.page.value === "library",
    badge: String(store.sites.value.length),
    onClick: () => store.openLibrary(),
  },
  {
    id: "modelparams",
    label: "模型参数",
    icon: icons.cpu,
    active: store.page.value === "modelparams",
    badge: store.modelCatalog.value.total
      ? store.modelCatalog.value.total >= 1_000
        ? `${(store.modelCatalog.value.total / 1_000).toFixed(1)}K`
        : String(store.modelCatalog.value.total)
      : "",
    onClick: () => store.openModelParams(),
  },
  {
    id: "charity",
    label: "公益监听",
    icon: icons.heartPulse,
    active: store.page.value === "charity",
    // 菜单徽标：今日发布的帖子数（非未读）
    badge: store.charityFeedTodayCount.value
      ? String(Math.min(store.charityFeedTodayCount.value, 99))
      : "",
    onClick: () => store.openCharityMonitor(),
  },
  {
    id: "proxy",
    label: "代理池",
    icon: icons.wifi,
    active: store.page.value === "proxy",
    badge: store.proxyPool.value.nodeCount
      ? String(store.proxyPool.value.nodeCount)
      : "",
    onClick: () => store.openProxyPool(),
  },
  {
    id: "tokenstats",
    label: "Token 统计",
    icon: icons.chart,
    active: store.page.value === "tokenstats",
    badge: todayTokenBadge.value,
    onClick: () => store.openTokenStats(),
  },
]);

// 今天消耗的 token 数（用于侧边栏菜单徽标）
const todayTokenTotal = computed(() => {
  const buckets = store.tokenUsage.value?.buckets ?? [];
  const today = toLocalDate(new Date());
  let total = 0;
  for (const bucket of buckets) {
    if (localDateOf(bucket.timestamp) === today) {
      total += bucket.totalTokens || 0;
    }
  }
  return total;
});
// 徽标文案：0 不显示
const todayTokenBadge = computed(() =>
  todayTokenTotal.value > 0 ? formatCompact(todayTokenTotal.value) : "",
);

function toggleSidebar() {
  updatePreferences({ sidebarCollapsed: !preferences.sidebarCollapsed });
}
</script>

<template>
  <aside class="app-sidebar" aria-label="应用导航">
    <div class="brand">
      <img src="/logo.svg" alt="" />
      <span>
        <strong>OpenHub</strong>
        <small>本地站点资料库</small>
      </span>
    </div>

    <nav class="sidebar-nav" aria-label="主要模块">
      <button
        v-for="item in navItems"
        :id="`${item.id}-nav`"
        :key="item.id"
        type="button"
        class="nav-item"
        :class="{ active: item.active }"
        :aria-current="item.active ? 'page' : undefined"
        @click="item.onClick()"
      >
        <span class="nav-item-icon" v-html="item.icon" />
        <span class="nav-item-label">{{ item.label }}</span>
        <small v-if="item.badge" class="nav-item-badge">{{ item.badge }}</small>
      </button>
    </nav>

    <div class="sidebar-footer">
      <button
        class="icon-button sidebar-collapse"
        id="sidebar-collapse"
        type="button"
        :aria-label="preferences.sidebarCollapsed ? '展开侧栏' : '收起侧栏'"
        @click="toggleSidebar"
        v-html="preferences.sidebarCollapsed ? icons.sidebarOpen : icons.sidebarClose"
      />
      <button
        class="icon-button"
        id="settings-toggle"
        type="button"
        :class="{ active: store.page.value === 'settings' }"
        aria-label="打开设置"
        @click="store.openSettings()"
        v-html="icons.settings"
      />
    </div>
  </aside>
</template>
