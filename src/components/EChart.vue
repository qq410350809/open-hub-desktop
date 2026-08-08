<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, watch } from "vue";
import { init, type ECharts, type EChartsOption } from "../echarts";

const props = withDefaults(
  defineProps<{
    option: EChartsOption;
    height?: string;
    autoresize?: boolean;
  }>(),
  { height: "240px", autoresize: true },
);

const el = ref<HTMLDivElement>();
let chart: ECharts | null = null;
let ro: ResizeObserver | null = null;

function render() {
  if (!chart) return;
  chart.setOption(props.option, true);
}

onMounted(() => {
  if (!el.value) return;
  chart = init(el.value, undefined, { renderer: "canvas" });
  render();
  if (props.autoresize !== false) {
    ro = new ResizeObserver(() => chart?.resize());
    ro.observe(el.value);
  }
  window.addEventListener("resize", () => chart?.resize());
});

watch(() => props.option, () => render(), { deep: true });

onBeforeUnmount(() => {
  ro?.disconnect();
  chart?.dispose();
  chart = null;
});
</script>

<template>
  <div
    ref="el"
    class="echart-box"
    :style="{ width: '100%', height: props.height }"
  ></div>
</template>

<style scoped>
.echart-box { min-width: 0; min-height: 0; }
</style>
