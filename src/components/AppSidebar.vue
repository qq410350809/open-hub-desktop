<script setup lang="ts">
import { computed } from "vue";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import { usePreferences } from "../composables/usePreferences";

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
    id: "charity",
    label: "公益监听",
    icon: icons.heartPulse,
    active: store.page.value === "charity",
    badge: store.charityFeedUnreadCount.value
      ? String(Math.min(store.charityFeedUnreadCount.value, 99))
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
]);

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
