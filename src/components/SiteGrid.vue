<script setup lang="ts">
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import SiteCard from "./SiteCard.vue";

const store = useStore();
</script>

<template>
  <main class="main-content">
    <div class="result-bar">
      <span v-if="store.loading.value">正在读取本地资料库…</span>
      <span v-else>显示 {{ store.filteredSites.value.length }} / {{ store.sites.value.length }} 个本地站点</span>
      <button v-if="store.hasFilters.value" id="clear-filter" @click="store.clearFilters()">
        清除筛选
      </button>
    </div>

    <section
      class="site-grid"
      :hidden="store.filteredSites.value.length === 0"
    >
      <SiteCard
        v-for="site in store.filteredSites.value"
        :key="site.id"
        :site="site"
      />
    </section>

    <section class="empty-state" :hidden="store.filteredSites.value.length !== 0">
      <div v-html="icons.search" />
      <h2>没有匹配的站点</h2>
      <p>尝试修改搜索词或清除筛选条件。</p>
      <button class="secondary-button" id="empty-clear" @click="store.clearFilters()">
        清除筛选
      </button>
    </section>
  </main>
</template>
