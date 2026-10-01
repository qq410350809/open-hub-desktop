<script setup lang="ts">
import { computed } from "vue";
import type { ProxyNode } from "../../types";
import { proxyIpDetails, proxyIpKind, proxyIpLabel } from "../../composables/proxy/proxyIpInfo";

const props = defineProps<{ node: ProxyNode }>();
const details = computed(() => proxyIpDetails(props.node));
</script>

<template>
  <span
    class="pp-ip-kind-badge"
    :class="`is-${proxyIpKind(node)}`"
    :title="details"
    :aria-label="details"
    tabindex="0"
  >{{ proxyIpLabel(node) }}</span>
</template>

<style scoped>
.pp-ip-kind-badge {
  display: inline-flex;
  align-items: center;
  width: fit-content;
  flex-shrink: 0;
  padding: 1px 5px;
  border: 1px solid var(--line);
  border-radius: 3px;
  background: var(--page-bg);
  color: var(--muted);
  font-size: 10px;
  font-weight: 600;
  line-height: 1.5;
  white-space: nowrap;
  cursor: help;
}
.pp-ip-kind-badge:focus-visible {
  outline: 2px solid var(--brand-deep);
  outline-offset: 2px;
}
.is-hosting {
  background: rgba(139, 92, 246, 0.1);
  border-color: rgba(139, 92, 246, 0.25);
  color: #8b5cf6;
}
.is-residential {
  background: rgba(16, 185, 129, 0.1);
  border-color: rgba(16, 185, 129, 0.25);
  color: #059669;
}
.is-mobile {
  background: rgba(59, 130, 246, 0.1);
  border-color: rgba(59, 130, 246, 0.25);
  color: #3b82f6;
}
</style>
