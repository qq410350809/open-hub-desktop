<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "../../composables/useStore";
import { icons } from "../../icons";
import type { SiteRecord } from "../../types";

const props = defineProps<{ site: SiteRecord }>();
const store = useStore();
const address = computed(() => props.site.auxBaseUrl?.trim() ?? "");
const configured = computed(() => Boolean(address.value));
</script>

<template>
  <span class="site-aux-controls">
    <button
      class="site-aux-trigger"
      :class="{ 'is-configured': configured }"
      type="button"
      :disabled="!configured"
      :title="configured ? `复制辅助站点地址：${address}` : '未配置辅助地址，可在编辑站点中添加'"
      :aria-label="`${site.name}：复制辅助站点地址`"
      @click.stop="store.copyAddress(address, '辅助站点地址')"
    ><span aria-hidden="true" v-html="icons.copy" /></button>
    <button
      class="site-aux-trigger"
      :class="{ 'is-configured': configured }"
      type="button"
      :disabled="!configured"
      :title="configured ? `在浏览器中打开辅助站点地址：${address}` : '未配置辅助地址，可在编辑站点中添加'"
      :aria-label="`${site.name}：在浏览器中打开辅助站点地址`"
      @click.stop="store.openSiteAddress(address, site.id)"
    ><span aria-hidden="true" v-html="icons.external" /></button>
  </span>
</template>

<style scoped>
.site-aux-controls { display: inline-flex; align-items: center; flex-shrink: 0; gap: 4px; }
.site-aux-trigger { display: inline-flex; align-items: center; justify-content: center; flex-shrink: 0; width: 28px; height: 28px; padding: 6px; box-sizing: border-box; border: 1px solid var(--line); border-radius: 7px; background: var(--surface-soft); color: var(--muted); cursor: pointer; }
.site-aux-trigger:disabled { opacity: .45; cursor: default; }
.site-aux-trigger > span { display: inline-flex; }
.site-aux-trigger :deep(svg) { width: 14px; height: 14px; }
.site-aux-trigger:hover:not(:disabled), .site-aux-trigger.is-configured { color: var(--text); border-color: var(--line-strong); }
</style>
