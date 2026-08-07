<script setup lang="ts">
import { ref, watch, nextTick, onMounted, onUnmounted } from "vue";

const props = defineProps<{
  tags: string[];
  isPersonal?: boolean;
  isPending?: boolean;
}>();

const listRef = ref<HTMLElement>();
const overflowCount = ref(0);
const hiddenTags = ref<string[]>([]);

function layout() {
  const list = listRef.value;
  if (!list || list.clientWidth <= 0) return;

  const chips = [...list.querySelectorAll<HTMLElement>(".tag-chip")];
  const overflow = list.querySelector<HTMLElement>(".tag-overflow");
  if (!overflow || !chips.length) return;

  chips.forEach((chip) => { chip.hidden = false; });
  overflow.hidden = true;
  const gap = Number.parseFloat(getComputedStyle(list).columnGap) || 0;
  const widths = chips.map((chip) => chip.getBoundingClientRect().width);
  let visibleCount = chips.length;

  for (let count = chips.length; count >= 0; count -= 1) {
    const hiddenCount = chips.length - count;
    let overflowWidth = 0;
    if (hiddenCount > 0) {
      overflow.textContent = `+${hiddenCount}`;
      overflow.hidden = false;
      overflowWidth = overflow.getBoundingClientRect().width;
    } else {
      overflow.hidden = true;
    }
    const itemCount = count + (hiddenCount > 0 ? 1 : 0);
    const totalWidth =
      widths.slice(0, count).reduce((sum, width) => sum + width, 0) +
      overflowWidth +
      Math.max(0, itemCount - 1) * gap;
    if (totalWidth <= list.clientWidth || count === 0) {
      visibleCount = count;
      break;
    }
  }

  chips.forEach((chip, index) => { chip.hidden = index >= visibleCount; });
  const hidden = chips.slice(visibleCount).map((chip) => chip.textContent?.trim() ?? "").filter(Boolean);
  overflowCount.value = hidden.length;
  hiddenTags.value = hidden;
  overflow.hidden = hidden.length === 0;
  if (hidden.length) {
    overflow.textContent = `+${hidden.length}`;
    overflow.dataset.hiddenTags = hidden.join("、");
    overflow.setAttribute("aria-label", `隐藏标签：${hidden.join("、")}`);
  } else {
    delete overflow.dataset.hiddenTags;
  }
}

let resizeObserver: ResizeObserver | null = null;

onMounted(() => {
  nextTick(layout);
  if (listRef.value) {
    resizeObserver = new ResizeObserver(layout);
    resizeObserver.observe(listRef.value);
  }
});

onUnmounted(() => {
  resizeObserver?.disconnect();
});

watch(() => props.tags, () => nextTick(layout), { deep: true });
</script>

<template>
  <div ref="listRef" class="tag-list">
    <span v-if="isPersonal" class="tag-chip tag-personal">在用</span>
    <span v-else-if="isPending" class="tag-chip tag-pending">待定</span>
    <span
      v-for="tag in tags.filter((t) => t.trim().toUpperCase() !== 'UNKNOWN' && t.trim() !== '未知')"
      :key="tag"
      class="tag-chip"
    >{{ tag }}</span>
    <span class="tag-overflow" tabindex="0" aria-label="查看隐藏标签" :hidden="overflowCount === 0">
      +{{ overflowCount }}
    </span>
  </div>
</template>
