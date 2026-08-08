<script setup lang="ts">
import { computed, onUnmounted, ref, watch } from "vue";
import { useStore } from "../composables/useStore";
import { icons } from "../icons";

const store = useStore();

// —— 快捷分组 ——
type RangeItem = { label: string; days: number; custom?: boolean; shift?: 'prev' | 'next' };
interface GroupDef { title: string; items: RangeItem[] }
const groups: GroupDef[] = [
  {
    title: "单日",
    items: [
      { label: "今日", days: -2 },
      { label: "昨日", days: -3 },
    ],
  },
  {
    title: "近 N 天",
    items: [
      { label: "近7天", days: 7 },
      { label: "近14天", days: 14 },
      { label: "近30天", days: 30 },
      { label: "近90天", days: 90 },
    ],
  },
  {
    title: "自然周期",
    items: [
      { label: "本周", days: -10 },
      { label: "本月", days: 0 },
      { label: "本季度", days: -4 },
      { label: "今年", days: -5 },
    ],
  },
  {
    title: "范围",
    items: [
      { label: "全部", days: -1 },
      { label: "自定义", days: -100, custom: true },
    ],
  },
];

const open = ref(false);
const rootRef = ref<HTMLElement>();
const customMode = ref(false);

// 当前命中的快捷项（若非自定义）
function currentDays(): number | null {
  for (const g of groups) {
    for (const it of g.items) {
      if (it.custom) continue;
      if (store.isCurrentRange(it.days)) return it.days;
    }
  }
  return null;
}
const hasCustomRange = computed(() => {
  const from = store.tokenStatsFrom.value;
  const to = store.tokenStatsTo.value;
  return (!!from || !!to) && currentDays() == null;
});
const activeLabel = computed(() => {
  if (customMode.value || hasCustomRange.value) return "自定义";
  const d = currentDays();
  if (d != null) {
    for (const g of groups) for (const it of g.items) if (it.days === d && !it.custom) return it.label;
  }
  return "全部";
});
const activeRangeText = computed(() => {
  const from = store.tokenStatsFrom.value;
  const to = store.tokenStatsTo.value;
  if (!from && !to) return "";
  return `${from || "…"} ~ ${to || "…"}`;
});

function toggle() {
  open.value = !open.value;
}
function close() {
  open.value = false;
  customMode.value = false;
  resetPicker();
}
function shiftDate(iso: string, delta: number): string {
  const d = new Date(`${iso}T00:00:00`);
  d.setDate(d.getDate() + delta);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}
// 前一天 / 后一天：相对当前选中的日期区间整体平移一天
function applyShift(delta: number) {
  const curFrom = store.tokenStatsFrom.value;
  const curTo = store.tokenStatsTo.value;
  const days = curFrom && curTo
    ? Math.round((new Date(`${curTo}T00:00:00`).getTime() - new Date(`${curFrom}T00:00:00`).getTime()) / 86_400_000)
    : 0;
  const base = curFrom || curTo || toLocalToday();
  const newFrom = shiftDate(base, delta);
  const newTo = shiftDate(base, delta + days);
  store.tokenStatsFrom.value = newFrom;
  store.tokenStatsTo.value = newTo;
  store.onRangeChange();
}
function pick(item: RangeItem) {
  if (item.custom) {
    customMode.value = true;
    return;
  }
  store.applyQuickRange(item.days);
  customMode.value = false;
  resetPicker();
  close();
}
function toLocalToday(): string {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

// —— 点击外部关闭 ——
function onDocClick(e: Event) {
  if (!open.value) return;
  const target = e.target as Node | null;
  if (rootRef.value && target && !rootRef.value.contains(target)) close();
}
watch(open, (o) => {
  if (o) {
    window.addEventListener("pointerdown", onDocClick, { capture: true });
    // 初始化日历视图到当前日期区间
    if (store.tokenStatsFrom.value) viewStart.value = toYM(store.tokenStatsFrom.value);
    else viewStart.value = toYM(new Date());
    pickedFrom.value = store.tokenStatsFrom.value;
    pickedTo.value = store.tokenStatsTo.value;
    rangeAnchor = null;
  } else {
    window.removeEventListener("pointerdown", onDocClick, { capture: true });
  }
});
onUnmounted(() => window.removeEventListener("pointerdown", onDocClick, { capture: true }));

// —— 双日历区间选择 ——
function pad(n: number) { return String(n).padStart(2, "0"); }
function toYM(d: Date | string) {
  if (typeof d === "string") return d.slice(0, 7);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}`;
}
function parseMonthKey(key: string) { const [y, m] = key.split("-").map(Number); return { y, m }; }
function addMonthsYM(key: string, delta: number) {
  const { y, m } = parseMonthKey(key);
  const target = new Date(y, m - 1 + delta, 1);
  return `${target.getFullYear()}-${pad(target.getMonth() + 1)}`;
}

const viewStart = ref(toYM(new Date()));
const viewEnd = computed(() => addMonthsYM(viewStart.value, 1));

function shiftMonth(delta: number) {
  const next = addMonthsYM(viewStart.value, delta);
  if (next > toYM(new Date())) return; // 不允许跳到未来月份
  if (next < "2000-01") return;
  viewStart.value = next;
}
function canShiftMonth(delta: number) {
  const next = addMonthsYM(viewStart.value, delta);
  if (next > toYM(new Date())) return false;
  if (next < "2000-01") return false;
  return true;
}

let rangeAnchor: string | null = null;
const pickedFrom = ref("");
const pickedTo = ref("");

function ymdOf(y: number, m: number, d: number) { return `${y}-${pad(m)}-${pad(d)}`; }
function daysInMonth(key: string) { const { y, m } = parseMonthKey(key); return new Date(y, m, 0).getDate(); }
function firstWeekday(key: string) { const { y, m } = parseMonthKey(key); return new Date(y, m - 1, 1).getDay(); }
const weekLabels = ["日", "一", "二", "三", "四", "五", "六"];

interface Cell { d: number; date: string; inRange: boolean; isStart: boolean; isEnd: boolean; today: boolean }
function monthCells(key: string): Cell[] {
  const { y, m } = parseMonthKey(key);
  const total = daysInMonth(key);
  const first = firstWeekday(key);
  const todayStr = ymdOf(new Date().getFullYear(), new Date().getMonth() + 1, new Date().getDate());
  const filled = !!(pickedFrom.value && pickedTo.value);
  const cells: Cell[] = [];
  for (let i = 0; i < first; i++) cells.push({ d: 0, date: "", inRange: false, isStart: false, isEnd: false, today: false });
  for (let d = 1; d <= total; d++) {
    const date = `${y}-${pad(m)}-${pad(d)}`;
    let inRange = false, isStart = false, isEnd = false;
    if (filled) {
      if (date >= pickedFrom.value && date <= pickedTo.value) inRange = true;
      if (date === pickedFrom.value) isStart = true;
      if (date === pickedTo.value) isEnd = true;
    } else if (rangeAnchor) {
      if (date === rangeAnchor) isStart = true;
      if (date >= rangeAnchor) inRange = true;
    }
    cells.push({ d, date, inRange, isStart, isEnd, today: date === todayStr });
  }
  return cells;
}

function clickDay(date: string) {
  if (pickedFrom.value && pickedTo.value) {
    pickedFrom.value = "";
    pickedTo.value = "";
    rangeAnchor = date;
    return;
  }
  if (!rangeAnchor) {
    rangeAnchor = date;
    return;
  }
  if (date < rangeAnchor) {
    pickedFrom.value = date;
    pickedTo.value = rangeAnchor;
  } else {
    pickedFrom.value = rangeAnchor;
    pickedTo.value = date;
  }
  rangeAnchor = null;
  // 选完区间后不自动关闭，由用户点"应用"确认
}
function resetPicker() {
  rangeAnchor = null;
  pickedFrom.value = "";
  pickedTo.value = "";
}
function applyCustomRange() {
  if (!pickedFrom.value || !pickedTo.value) return;
  store.tokenStatsFrom.value = pickedFrom.value;
  store.tokenStatsTo.value = pickedTo.value;
  store.onRangeChange();
  close();
}
</script>

<template>
  <div ref="rootRef" class="tt-range-dd" :class="{ open, 'is-custom': customMode }">
    <button
      v-if="activeRangeText"
      type="button"
      class="tt-range-arrow tt-range-arrow-prev"
      title="前一天"
      @click.stop.prevent="applyShift(-1)"
    >‹</button>
    <button type="button" class="tt-range-trigger" :class="{ active: customMode }" @click="toggle">
      <span class="tt-range-label">{{ activeLabel }}</span>
      <span v-if="activeRangeText" class="tt-range-sub">{{ activeRangeText }}</span>
      <span class="tt-range-caret" v-html="icons.chevron" />
    </button>
    <button
      v-if="activeRangeText"
      type="button"
      class="tt-range-arrow tt-range-arrow-next"
      title="后一天"
      @click.stop.prevent="applyShift(1)"
    >›</button>

    <div v-if="open" class="tt-range-pop" @click.stop>
      <template v-if="!customMode">
        <div class="tt-rp-groups">
          <div v-for="g in groups" :key="g.title" class="tt-rp-group">
            <div class="tt-rp-group-title">{{ g.title }}</div>
            <div class="tt-rp-group-items">
              <button
                v-for="it in g.items"
                :key="it.label"
                type="button"
                class="tt-rp-item"
                :class="{ active: !it.custom && store.isCurrentRange(it.days) }"
                @click="pick(it)"
              >{{ it.label }}</button>
            </div>
          </div>
        </div>
      </template>

      <template v-else>
        <div class="tt-rp-custom-head">
          <button type="button" class="tt-rp-back" @click="customMode = false">‹ 返回</button>
          <div class="tt-rp-custom-range">
            <span class="tt-rp-val">{{ pickedFrom || "起" }}</span>
            <span class="tt-rp-sep">→</span>
            <span class="tt-rp-val">{{ pickedTo || "止" }}</span>
          </div>
          <button type="button" class="tt-rp-clear" @click="resetPicker">清空</button>
        </div>
        <div class="tt-rp-calendars">
          <div v-for="mk in [viewStart, viewEnd]" :key="mk" class="tt-rp-cal">
            <div class="tt-rp-cal-head">
              <button
                type="button"
                class="tt-rp-nav"
                :disabled="mk === viewStart && !canShiftMonth(-1)"
                @click="shiftMonth(-1)"
              >‹</button>
              <span class="tt-rp-cal-title">{{ parseInt(mk.slice(0,4)) }}年{{ parseInt(mk.slice(5,7)) }}月</span>
              <button
                type="button"
                class="tt-rp-nav"
                :disabled="mk === viewEnd && !canShiftMonth(1)"
                @click="shiftMonth(1)"
              >›</button>
            </div>
            <div class="tt-rp-week">
              <span v-for="w in weekLabels" :key="w">{{ w }}</span>
            </div>
            <div class="tt-rp-cells">
              <button
                v-for="(c, ci) in monthCells(mk)"
                :key="ci"
                type="button"
                class="tt-rp-cell"
                :class="{ empty: !c.date, inrange: c.inRange, start: c.isStart, end: c.isEnd, today: c.today }"
                :disabled="!c.date"
                @click="c.date && clickDay(c.date)"
              >{{ c.d || "" }}</button>
            </div>
          </div>
        </div>
        <div class="tt-rp-custom-foot">
          <span v-if="pickedFrom && pickedTo" class="tt-rp-hint">{{ pickedFrom }} ~ {{ pickedTo }}（已选好，点击应用）</span>
          <span v-else class="tt-rp-hint">先点起始日，再点结束日</span>
          <div class="tt-rp-actions">
            <button type="button" class="tt-rp-apply" :disabled="!pickedFrom || !pickedTo" @click="applyCustomRange">应用</button>
            <button type="button" class="tt-rp-cancel" @click="close">取消</button>
          </div>
        </div>
      </template>
    </div>
  </div>
</template>
