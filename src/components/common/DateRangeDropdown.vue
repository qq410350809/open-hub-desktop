<script setup lang="ts">
import { computed, nextTick, onUnmounted, ref, watch } from "vue";
import { icons } from "../../icons";

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

const from = defineModel<string>("from", { default: "" });
const to = defineModel<string>("to", { default: "" });
const emit = defineEmits<{ apply: [] }>();

const open = ref(false);
const rootRef = ref<HTMLElement>();
const customMode = ref(false);

// —— 弹层定位（Teleport 到 body 后以 fixed 定位，避免被 overflow:hidden 的
//    页面容器或窗口边缘裁剪）——
const triggerRef = ref<HTMLElement>();
const popRef = ref<HTMLElement>();
const popStyle = ref<{ top: string; left: string }>({ top: "-9999px", left: "-9999px" });

function updatePopPosition() {
  const trigger = triggerRef.value;
  const pop = popRef.value;
  if (!trigger || !pop) return;
  const rect = trigger.getBoundingClientRect();
  const margin = 8;
  const width = pop.offsetWidth;
  const height = pop.offsetHeight;
  // 水平：优先与触发器右缘对齐（沿用原 right:0 视觉），越出视口时向内夹取
  let left = rect.right - width;
  if (left < margin) left = margin;
  if (left + width > window.innerWidth - margin) left = window.innerWidth - margin - width;
  // 垂直：优先向下展开；下方放不下且上方空间更大时向上翻
  let top = rect.bottom + 6;
  if (top + height > window.innerHeight - margin) {
    const above = rect.top - 6 - height;
    top = above >= margin ? above : Math.max(margin, window.innerHeight - margin - height);
  }
  popStyle.value = { top: `${top}px`, left: `${left}px` };
}

function onWindowReposition() {
  if (open.value) updatePopPosition();
}

function toLocalDate(value: Date): string {
  const year = value.getFullYear();
  const month = String(value.getMonth() + 1).padStart(2, "0");
  const day = String(value.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

/**
 * 快捷范围编码：
 *  -2 今日（仅今天）
 *  -3 昨日（仅昨天）
 *   0 本月（1 号至今）
 *  -1 全部（不限日期）
 *  >0 近 N 天（含今天，共 N 天，即今天往前 N-1 天）
 *  -4 本季度 / -5 今年 / -10 本周
 */
function setQuickRange(days: number) {
  if (days === -1) {
    from.value = "";
    to.value = "";
    emit("apply");
    return;
  }

  const today = new Date();
  const f = new Date(today);
  const t = new Date(today);

  if (days === -2) {
    // 今日
  } else if (days === -3) {
    f.setDate(f.getDate() - 1);
    t.setDate(t.getDate() - 1);
  } else if (days === 0) {
    f.setDate(1);
  } else if (days === -4) {
    f.setDate(1);
    f.setMonth(Math.floor(f.getMonth() / 3) * 3);
  } else if (days === -5) {
    f.setDate(1);
    f.setMonth(0);
  } else if (days === -10) {
    const offset = (f.getDay() + 6) % 7;
    f.setDate(f.getDate() - offset);
  } else {
    f.setDate(f.getDate() - (days - 1));
  }

  from.value = toLocalDate(f);
  to.value = toLocalDate(t);
  emit("apply");
}

function currentDays(): number | null {
  for (const g of groups) {
    for (const it of g.items) {
      if (it.custom) continue;
      if (isCurrentRange(it.days)) return it.days;
    }
  }
  return null;
}

function isCurrentRange(days: number) {
  if (days === -1) return !from.value && !to.value;

  const today = new Date();
  const f = new Date(today);
  const t = new Date(today);

  if (days === -2) {
    // 今日
  } else if (days === -3) {
    f.setDate(f.getDate() - 1);
    t.setDate(t.getDate() - 1);
  } else if (days === 0) {
    f.setDate(1);
  } else if (days === -4) {
    f.setDate(1);
    f.setMonth(Math.floor(f.getMonth() / 3) * 3);
  } else if (days === -5) {
    f.setDate(1);
    f.setMonth(0);
  } else if (days === -10) {
    const offset = (f.getDay() + 6) % 7;
    f.setDate(f.getDate() - offset);
  } else {
    f.setDate(f.getDate() - (days - 1));
  }

  return from.value === toLocalDate(f) && to.value === toLocalDate(t);
}

const hasCustomRange = computed(() => {
  return (!!from.value || !!to.value) && currentDays() == null;
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
  if (!from.value && !to.value) return "";
  // 单日区间只显示一个日期，避免触发器过宽挤压同行控件
  if (from.value && to.value && from.value === to.value) return from.value;
  return `${from.value || "…"} ~ ${to.value || "…"}`;
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
  const days = from.value && to.value
    ? Math.round((new Date(`${to.value}T00:00:00`).getTime() - new Date(`${from.value}T00:00:00`).getTime()) / 86_400_000)
    : 0;
  const base = from.value || to.value || toLocalToday();
  const newFrom = shiftDate(base, delta);
  const newTo = shiftDate(base, delta + days);
  from.value = newFrom;
  to.value = newTo;
  emit("apply");
}
function pick(item: RangeItem) {
  if (item.custom) {
    customMode.value = true;
    return;
  }
  setQuickRange(item.days);
  customMode.value = false;
  resetPicker();
  close();
}
function toLocalToday(): string {
  return toLocalDate(new Date());
}

// —— 点击外部关闭 ——
function onDocClick(e: Event) {
  if (!open.value) return;
  const target = e.target as Node | null;
  const inRoot = !!(rootRef.value && target && rootRef.value.contains(target));
  // 弹层 Teleport 到 body 后不在 rootRef 内，需一并排除
  const inPop = !!(popRef.value && target && popRef.value.contains(target));
  if (!inRoot && !inPop) close();
}
watch(open, (o) => {
  if (o) {
    window.addEventListener("pointerdown", onDocClick, { capture: true });
    window.addEventListener("scroll", onWindowReposition, { capture: true, passive: true });
    window.addEventListener("resize", onWindowReposition);
    nextTick(() => {
      updatePopPosition();
      requestAnimationFrame(updatePopPosition);
    });
    if (from.value) viewStart.value = toYM(from.value);
    else viewStart.value = toYM(new Date());
    pickedFrom.value = from.value;
    pickedTo.value = to.value;
    rangeAnchor = null;
  } else {
    window.removeEventListener("pointerdown", onDocClick, { capture: true });
    window.removeEventListener("scroll", onWindowReposition, { capture: true });
    window.removeEventListener("resize", onWindowReposition);
  }
});
onUnmounted(() => {
  window.removeEventListener("pointerdown", onDocClick, { capture: true });
  window.removeEventListener("scroll", onWindowReposition, { capture: true });
  window.removeEventListener("resize", onWindowReposition);
});
// 快捷面板 ↔ 自定义双日历切换会改变弹层尺寸，需重新计算定位
watch(customMode, () => {
  if (!open.value) return;
  nextTick(() => {
    updatePopPosition();
    requestAnimationFrame(updatePopPosition);
  });
});

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
  if (next > toYM(new Date())) return;
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
}
function resetPicker() {
  rangeAnchor = null;
  pickedFrom.value = "";
  pickedTo.value = "";
}
function applyCustomRange() {
  if (!pickedFrom.value || !pickedTo.value) return;
  from.value = pickedFrom.value;
  to.value = pickedTo.value;
  emit("apply");
  close();
}
</script>

<template>
  <div ref="rootRef" class="drd-dd" :class="{ open, 'is-custom': customMode }">
    <button
      v-if="activeRangeText"
      type="button"
      class="drd-range-arrow drd-range-arrow-prev"
      title="前一天"
      @click.stop.prevent="applyShift(-1)"
    >
      <span v-html="icons.chevron" class="drd-arrow-icon is-prev" />
    </button>
    <button ref="triggerRef" type="button" class="drd-range-trigger" :class="{ active: customMode }" @click="toggle">
      <span class="drd-range-label">{{ activeLabel }}</span>
      <span v-if="activeRangeText" class="drd-range-sub">{{ activeRangeText }}</span>
      <span class="drd-range-caret" v-html="icons.chevron" />
    </button>
    <button
      v-if="activeRangeText"
      type="button"
      class="drd-range-arrow drd-range-arrow-next"
      title="后一天"
      @click.stop.prevent="applyShift(1)"
    >
      <span v-html="icons.chevron" class="drd-arrow-icon is-next" />
    </button>

    <Teleport to="body">
      <div v-if="open" ref="popRef" class="drd-range-pop" :style="popStyle" @click.stop>
      <template v-if="!customMode">
        <div class="drd-rp-groups">
          <div v-for="g in groups" :key="g.title" class="drd-rp-group">
            <div class="drd-rp-group-title">{{ g.title }}</div>
            <div class="drd-rp-group-items">
              <button
                v-for="it in g.items"
                :key="it.label"
                type="button"
                class="drd-rp-item"
                :class="{ active: !it.custom && isCurrentRange(it.days) }"
                @click="pick(it)"
              >{{ it.label }}</button>
            </div>
          </div>
        </div>
      </template>

      <template v-else>
        <div class="drd-rp-custom-head">
          <button type="button" class="drd-rp-back" @click="customMode = false">‹ 返回</button>
          <div class="drd-rp-custom-range">
            <span class="drd-rp-val">{{ pickedFrom || "起" }}</span>
            <span class="drd-rp-sep">→</span>
            <span class="drd-rp-val">{{ pickedTo || "止" }}</span>
          </div>
          <button type="button" class="drd-rp-clear" @click="resetPicker">清空</button>
        </div>
        <div class="drd-rp-calendars">
          <div v-for="mk in [viewStart, viewEnd]" :key="mk" class="drd-rp-cal">
            <div class="drd-rp-cal-head">
              <button
                type="button"
                class="drd-rp-nav"
                :disabled="mk === viewStart && !canShiftMonth(-1)"
                @click="shiftMonth(-1)"
              >‹</button>
              <span class="drd-rp-cal-title">{{ parseInt(mk.slice(0,4)) }}年{{ parseInt(mk.slice(5,7)) }}月</span>
              <button
                type="button"
                class="drd-rp-nav"
                :disabled="mk === viewEnd && !canShiftMonth(1)"
                @click="shiftMonth(1)"
              >›</button>
            </div>
            <div class="drd-rp-week">
              <span v-for="w in weekLabels" :key="w">{{ w }}</span>
            </div>
            <div class="drd-rp-cells">
              <button
                v-for="(c, ci) in monthCells(mk)"
                :key="ci"
                type="button"
                class="drd-rp-cell"
                :class="{ empty: !c.date, inrange: c.inRange, start: c.isStart, end: c.isEnd, today: c.today }"
                :disabled="!c.date"
                @click="c.date && clickDay(c.date)"
              >{{ c.d || "" }}</button>
            </div>
          </div>
        </div>
        <div class="drd-rp-custom-foot">
          <span v-if="pickedFrom && pickedTo" class="drd-rp-hint">{{ pickedFrom }} ~ {{ pickedTo }}（已选好，点击应用）</span>
          <span v-else class="drd-rp-hint">先点起始日，再点结束日</span>
          <div class="drd-rp-actions">
            <button type="button" class="drd-rp-apply" :disabled="!pickedFrom || !pickedTo" @click="applyCustomRange">应用</button>
            <button type="button" class="drd-rp-cancel" @click="close">取消</button>
          </div>
        </div>
      </template>
      </div>
    </Teleport>
  </div>
</template>

<style scoped>
.drd-dd {
  position: relative;
  display: inline-flex;
  align-items: stretch;
  gap: 4px;
  flex-shrink: 0;
}

.drd-range-arrow {
  width: 28px;
  height: 34px;
  display: flex;
  align-items: center;
  justify-content: center;
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  background: var(--surface);
  color: var(--muted);
  cursor: pointer;
  transition: all 0.15s ease;
}

.drd-range-arrow:hover {
  background: var(--surface-hover);
  color: var(--text);
  border-color: var(--line-hover);
}

.drd-arrow-icon {
  display: inline-flex;
}
.drd-arrow-icon :deep(svg) {
  width: 13px;
  height: 13px;
}
.drd-arrow-icon.is-prev :deep(svg) {
  transform: rotate(90deg);
}
.drd-arrow-icon.is-next :deep(svg) {
  transform: rotate(-90deg);
}

.drd-range-trigger {
  height: 34px;
  padding: 0 12px;
  display: flex;
  align-items: center;
  gap: 8px;
  border: 1px solid var(--line);
  border-radius: var(--r-md, 8px);
  background: var(--surface);
  color: var(--text);
  font-size: 12.5px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.15s ease;
  white-space: nowrap;
  flex-shrink: 0;
}

.drd-range-trigger:hover {
  background: var(--surface-hover);
  border-color: var(--line-hover);
}

.drd-range-label {
  color: var(--brand);
  font-weight: 700;
}

.drd-range-sub {
  color: var(--muted);
  font-size: 11px;
  font-variant-numeric: tabular-nums;
}

.drd-range-caret {
  display: inline-flex;
  color: var(--muted);
  transition: transform 0.2s ease;
}
.drd-range-caret :deep(svg) {
  width: 12px;
  height: 12px;
}
.drd-dd.open .drd-range-caret {
  transform: rotate(180deg);
}

/* 弹出层：Teleport 到 body 后以 fixed 定位，坐标由脚本按触发器位置计算并夹取到视口内，
   避免被页面 overflow:hidden 容器或窗口边缘裁剪 */
.drd-range-pop {
  position: fixed;
  z-index: 1200;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--r-xl, 12px);
  box-shadow: 0 12px 32px rgba(0, 0, 0, 0.15);
  padding: 12px;
  min-width: 240px;
}

.drd-rp-groups {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.drd-rp-group-title {
  font-size: 10px;
  font-weight: 700;
  color: var(--muted);
  letter-spacing: 0.06em;
  text-transform: uppercase;
  margin-bottom: 4px;
}

.drd-rp-group-items {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 4px;
}

.drd-rp-item {
  height: 28px;
  padding: 0 8px;
  border-radius: var(--r-md, 6px);
  border: 1px solid transparent;
  background: var(--page-bg);
  color: var(--text);
  font-size: 12px;
  font-weight: 550;
  cursor: pointer;
  transition: all 0.12s ease;
  text-align: center;
}

.drd-rp-item:hover {
  background: var(--surface-hover);
  border-color: var(--line);
}

.drd-rp-item.active {
  background: var(--brand-soft);
  color: var(--brand-deep);
  border-color: var(--brand-line);
  font-weight: 700;
}

/* 自定义日历 */
.drd-rp-custom-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  margin-bottom: 10px;
  font-size: 12px;
}

.drd-rp-back,
.drd-rp-clear {
  border: none;
  background: transparent;
  color: var(--brand);
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
}

.drd-rp-custom-range {
  display: flex;
  align-items: center;
  gap: 6px;
  font-weight: 700;
  font-variant-numeric: tabular-nums;
  font-size: 11px;
}

.drd-rp-calendars {
  display: flex;
  gap: 12px;
}

.drd-rp-cal {
  width: 190px;
}

.drd-rp-cal-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 6px;
}

.drd-rp-cal-title {
  font-size: 12px;
  font-weight: 700;
}

.drd-rp-nav {
  border: none;
  background: transparent;
  color: var(--text);
  font-size: 14px;
  cursor: pointer;
  padding: 0 4px;
}

.drd-rp-week {
  display: grid;
  grid-template-columns: repeat(7, 1fr);
  text-align: center;
  font-size: 10px;
  color: var(--muted);
  margin-bottom: 4px;
}

.drd-rp-cells {
  display: grid;
  grid-template-columns: repeat(7, 1fr);
  gap: 2px;
}

.drd-rp-cell {
  height: 22px;
  border-radius: 4px;
  border: none;
  background: transparent;
  color: var(--text);
  font-size: 11px;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
}

.drd-rp-cell:hover:not(:disabled) {
  background: var(--surface-hover);
}

.drd-rp-cell.inrange {
  background: var(--brand-soft);
  color: var(--brand-deep);
}

.drd-rp-cell.start,
.drd-rp-cell.end {
  background: var(--brand);
  color: #fff;
  font-weight: 700;
}

.drd-rp-cell.today {
  border: 1px solid var(--brand);
}

.drd-rp-custom-foot {
  margin-top: 10px;
  padding-top: 8px;
  border-top: 1px solid var(--line);
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.drd-rp-hint {
  font-size: 10px;
  color: var(--muted);
}

.drd-rp-actions {
  display: flex;
  gap: 6px;
}

.drd-rp-apply {
  height: 26px;
  padding: 0 10px;
  border-radius: var(--r-md, 6px);
  border: none;
  background: var(--brand);
  color: #fff;
  font-size: 11px;
  font-weight: 700;
  cursor: pointer;
}

.drd-rp-cancel {
  height: 26px;
  padding: 0 8px;
  border-radius: var(--r-md, 6px);
  border: 1px solid var(--line);
  background: transparent;
  color: var(--muted);
  font-size: 11px;
  cursor: pointer;
}
</style>
