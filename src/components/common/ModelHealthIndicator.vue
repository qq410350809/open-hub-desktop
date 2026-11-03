<script setup lang="ts">
/**
 * 模型健康度指示：成功率数值徽标 + 自适应槽数时段状态条（站点原生
 * 1 小时格 → 24 格，30 分钟格 → 48 格，更细按原生格宽整数倍合并）。
 *
 * 与站点模型弹窗、站点库抽屉用同一套口径（`utils.ts` 的 describeModelHealth /
 * describeModelStatusStrip / describeModelStatusStripTitle），色板沿用全局
 * `.site-models-item-health*` 样式，三处展示不会各说各话。
 *
 * 组件刻意不包根元素（多根节点）：徽标要插进行尾的标题行、状态条要另起一块
 * 放在卡片下方，两者在不同父容器里布局，包一层反而会破坏原有 flex 结构。
 */
import { computed } from "vue";
import {
  describeModelHealth,
  describeModelSnapshot,
  describeModelStatusStrip,
  describeModelStatusStripTitle,
  describeModelWindowLabel,
  modelStatusStripGapPercent,
  type ModelHealthBadge,
  type ModelStatusSlot,
} from "../../utils";
import type { SiteModelHealth } from "../../types";

const props = withDefaults(
  defineProps<{
    /** 该模型在所属站点上报的健康度；缺失时不渲染徽标。 */
    health?: SiteModelHealth | null;
    /** 同站点任一带窗口起点的健康度：给没有自身起点的条目对齐时间轴与格宽。 */
    fallback?: SiteModelHealth | null;
    /** 是否显示成功率数值徽标。 */
    badge?: boolean;
    /** 是否显示时段状态条。 */
    strip?: boolean;
    /**
     * 竖排合并为一个块（窄卡片用）：徽标与状态条上下排列，并占满整行宽度。
     * 默认 false —— 徽标与状态条分别插入父容器的不同位置（行尾 / 下方），
     * 这时组件刻意不包根元素。
     */
    stacked?: boolean;
  }>(),
  { health: null, fallback: null, badge: true, strip: true, stacked: false },
);

const badge = computed<ModelHealthBadge | null>(() =>
  props.badge && props.health ? describeModelHealth(props.health) : null,
);

const slots = computed<ModelStatusSlot[]>(() =>
  describeModelStatusStrip(props.health, props.fallback),
);

/** 快照源只有当前状态：用状态灯展示，不铺时间轴（无自身条目时沿用站点形态）。 */
const snapshot = computed(() => describeModelSnapshot(props.health ?? props.fallback));

const reference = computed(() => props.health ?? props.fallback);

const stripTitle = computed(() => describeModelStatusStripTitle(reference.value));

const stripLabel = computed(
  () =>
    `${describeModelWindowLabel(reference.value)}成功率状态条，共 ${slots.value.length} 个时段`,
);

/** 列间距随槽数缩放（缝 = 柱宽 × 0.375），全局条带样式读这个变量。 */
const stripGap = computed(() => ({
  "--health-strip-gap": modelStatusStripGapPercent(slots.value.length),
}));
</script>

<style scoped>
/* 竖排模式：徽标与状态条上下排列，各占满整行宽度。 */
.model-health-indicator { display: flex; flex-direction: column; align-items: stretch; gap: 3px; }
.model-health-indicator .site-models-item-health { margin-top: 0; }
</style>

<template>
  <span v-if="stacked" class="model-health-indicator">
    <span
      v-if="badge"
      class="site-models-item-health-value"
      :class="`is-lv${badge.level}`"
      :title="badge.title"
    >{{ badge.label }}</span>
    <span class="site-models-item-health">
      <span
        v-if="snapshot"
        class="site-models-item-health-snapshot"
        :class="`is-lv${snapshot.level}`"
        role="img"
        :aria-label="`实时探测状态：${snapshot.label}`"
        :title="snapshot.title"
      >
        <span class="site-models-item-health-snapshot-dot" />
        <span v-if="!badge" class="site-models-item-health-snapshot-label">{{ snapshot.label }}</span>
      </span>
      <span
        v-else
        class="site-models-item-health-strip"
        role="img"
        :aria-label="stripLabel"
        :title="stripTitle"
        :style="stripGap"
      >
        <span
          v-for="slot in slots"
          :key="slot.ts"
          class="site-models-item-health-slot"
          :class="slot.level ? `is-lv${slot.level}` : 'is-idle'"
          :title="slot.title"
        />
      </span>
    </span>
  </span>
  <template v-else>
    <span
      v-if="badge"
      class="site-models-item-health-value"
      :class="`is-lv${badge.level}`"
      :title="badge.title"
    >{{ badge.label }}</span>
    <span v-if="strip" class="site-models-item-health">
      <span
        v-if="snapshot"
        class="site-models-item-health-snapshot"
        :class="`is-lv${snapshot.level}`"
        role="img"
        :aria-label="`实时探测状态：${snapshot.label}`"
        :title="snapshot.title"
      >
        <span class="site-models-item-health-snapshot-dot" />
        <span v-if="!badge" class="site-models-item-health-snapshot-label">{{ snapshot.label }}</span>
      </span>
      <span
        v-else
        class="site-models-item-health-strip"
        role="img"
        :aria-label="stripLabel"
        :title="stripTitle"
        :style="stripGap"
      >
        <span
          v-for="slot in slots"
          :key="slot.ts"
          class="site-models-item-health-slot"
          :class="slot.level ? `is-lv${slot.level}` : 'is-idle'"
          :title="slot.title"
        />
      </span>
    </span>
  </template>
</template>
