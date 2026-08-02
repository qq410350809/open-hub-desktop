<script setup lang="ts">
import { computed } from "vue";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import CustomSelect from "./CustomSelect.vue";
import { usePreferences } from "../composables/usePreferences";

const store = useStore();
const { preferences, updatePreferences } = usePreferences();

const tagOptions = computed(() => [
  { value: "all", text: "全部标签" },
  ...store.allTags.value.map((tag) => ({ value: tag, text: tag })),
]);

const levelOptions = [
  { value: "all", text: "全部等级" },
  { value: "0", text: "LV0" },
  { value: "1", text: "LV1" },
  { value: "2", text: "LV2" },
  { value: "3", text: "LV3" },
];

const featureOptions = [
  { value: "all", text: "全部功能" },
  { value: "checkin", text: "支持签到" },
  { value: "translation", text: "沉浸式翻译" },
  { value: "ldc", text: "支持 LDC" },
  { value: "nsfw", text: "支持 NSFW" },
  { value: "invite", text: "需要邀请码" },
];

const systemTypeOptions = [
  { value: "all", text: "全部系统类型" },
  { value: "newapi", text: "NewAPI" },
  { value: "sub2api", text: "Sub2API" },
  { value: "0v0", text: "0v0" },
  { value: "unknown", text: "未知类型" },
];

function toggleSidebar() {
  updatePreferences({ sidebarCollapsed: !preferences.sidebarCollapsed });
}
</script>

<template>
  <aside class="app-sidebar">
    <div class="brand">
      <img src="/icon.png" width="40" height="40" alt="" />
      <span>
        <strong>OpenHub</strong>
        <small>{{ store.sites.value.length }} 个本地站点</small>
      </span>
    </div>

    <section class="toolbar" aria-label="站点筛选">
      <CustomSelect
        class="tag-select"
        :options="tagOptions"
        :model-value="store.tag.value"
        @update:model-value="store.tag.value = $event"
        aria-label="标签筛选"
      />
      <CustomSelect
        :options="levelOptions"
        :model-value="store.level.value"
        @update:model-value="store.level.value = $event"
        aria-label="等级筛选"
      />
      <CustomSelect
        :options="featureOptions"
        :model-value="store.feature.value"
        @update:model-value="store.feature.value = $event"
        aria-label="功能筛选"
      />
      <CustomSelect
        :options="systemTypeOptions"
        :model-value="store.systemTypeFilter.value"
        @update:model-value="store.systemTypeFilter.value = $event"
        aria-label="系统类型筛选"
      />

      <div class="toolbar-actions">
        <div class="filter-segment" role="group" aria-label="使用状态">
          <button
            id="all-usage-filter"
            type="button"
            :class="{ active: store.usageFilter.value === 'all' }"
            :aria-pressed="store.usageFilter.value === 'all'"
            @click="store.setUsageFilter('all')"
          >全部</button>
          <button
            id="personal-filter"
            type="button"
            :class="{ active: store.usageFilter.value === 'personal' }"
            :aria-pressed="store.usageFilter.value === 'personal'"
            @click="store.setUsageFilter('personal')"
          >在用</button>
        </div>
        <div class="filter-segment" role="group" aria-label="站点状态">
          <button
            id="active-filter"
            type="button"
            :class="{ active: store.runawayFilter.value === 'active' }"
            :aria-pressed="store.runawayFilter.value === 'active'"
            @click="store.setRunawayFilter('active')"
          >存活</button>
          <button
            id="runaway-filter"
            type="button"
            :class="{ active: store.runawayFilter.value === 'runaway' }"
            :aria-pressed="store.runawayFilter.value === 'runaway'"
            @click="store.setRunawayFilter('runaway')"
          >跑路</button>
        </div>
      </div>
    </section>

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
        :class="{ active: store.page.value === 'settings' }"
        aria-label="打开设置"
        @click="store.openSettings()"
        v-html="icons.settings"
      />
    </div>
  </aside>
</template>
